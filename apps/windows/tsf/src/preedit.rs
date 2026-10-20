//! 组合串（preedit）：把正在输入的编码显示在文档里，并在提交时原地换成汉字。
//!
//! ## 为什么需要它
//!
//! M1.1 把编码只存在内存里：敲 `vbg` 屏幕上什么都不出现，按空格才蹦出一个「好」。
//! 用户没法确认自己敲了什么、敲到第几码，实际不可用。组合串就是让这段编码
//! **跟着光标显示在文档里**，提交时原地替换。
//!
//! ## 三个动作
//!
//! | 动作 | 会话里做什么 |
//! |---|---|
//! | `Show(text)` | 没有组合串就 `StartComposition` 后写入；有就改内容；`text` 为空则结束并抹掉 |
//! | `Finish(Some(text))` | 把组合串内容换成最终文字，再 `EndComposition`（文字留下） |
//! | `Finish(None)` | 抹掉组合串内容，再 `EndComposition`（取消） |
//!
//! ## 两个必须知道的细节
//!
//! 1. **`StartComposition` 要的是一个空 range。** 所以流程永远是「清空选区 → 起组合串
//!    → 再往组合串的 range 里写文字」。反过来（先写文字再起组合串）组合串起点会落在
//!    文字末尾，覆盖不到那串编码，后面 `SetText` 改的内容就看不见了。
//! 2. **`EndComposition` 不删文字**，它只把这段文字「转正」，留在文档里。
//!    所以「取消」必须先 `SetText` 成空串，否则那几个编码字母会永久留在文档里。
//!
//! 另外，写文档只能在 `DoEditSession(ec)` 里做——TSF 不允许输入法直接改文档，
//! 理由同 M1.1 的上屏路径。

use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;

use windows::core::{implement, Interface, Result};
use windows::Win32::Foundation::FALSE;
use windows::Win32::UI::TextServices::{
    ITfComposition, ITfCompositionSink, ITfContext, ITfContextComposition, ITfEditSession,
    ITfEditSession_Impl, ITfRange, TF_AE_END, TF_ANCHOR_END, TF_DEFAULT_SELECTION, TF_SELECTION,
    TF_SELECTIONSTYLE,
};

/// 当前组合串的句柄，由 `TextService` 与会话共享。
///
/// 用 `Rc<RefCell<_>>` 而不是把句柄交给会话再取回：会话可能被异步执行
/// （`RequestEditSession` 返回 `TS_S_ASYNC` 时），把状态留在共享槽里，
/// 无论会话什么时候跑，状态都在正确的地方。
pub type CompSlot = Rc<RefCell<Option<ITfComposition>>>;

/// 这次编辑会话要把组合串带到什么状态。
#[derive(Clone, Debug)]
pub enum Goal {
    /// 让组合串显示这段文字。空串表示「结束组合串并抹掉」。
    Show(String),
    /// 结束组合串。`Some` 把这段文字留在文档里，`None` 抹掉。
    Finish(Option<String>),
}

/// 一次组合串写入的编辑会话。
#[implement(ITfEditSession)]
pub struct PreeditSession {
    context: ITfContext,
    /// 组合串被别人终止时通知谁。`None` 表示不接收通知（降级路径）。
    sink: Option<ITfCompositionSink>,
    slot: CompSlot,
    goal: Goal,
}

impl PreeditSession {
    pub fn new(
        context: ITfContext,
        sink: Option<ITfCompositionSink>,
        slot: CompSlot,
        goal: Goal,
    ) -> Self {
        Self {
            context,
            sink,
            slot,
            goal,
        }
    }

    /// 真正的写入。调用时必须已持有 `ec`。
    ///
    /// # Safety
    ///
    /// 只能在 `DoEditSession` 里、用系统给的 `ec` 调用。
    unsafe fn apply(&self, ec: u32) -> Result<()> {
        match &self.goal {
            Goal::Show(text) => self.show(ec, text),
            Goal::Finish(final_text) => self.finish(ec, final_text.as_deref()),
        }
    }

    /// 让组合串的内容等于 `text`。
    unsafe fn show(&self, ec: u32, text: &str) -> Result<()> {
        let mut slot = self.slot.borrow_mut();

        // 已有组合串：改内容。空串顺手结束掉。
        if let Some(comp) = slot.as_ref() {
            let range = comp.GetRange()?;
            write_text(ec, &range, text)?;
            if text.is_empty() {
                comp.EndComposition(ec)?;
                *slot = None;
            } else {
                // 光标跟到 preedit 末尾。不设的话光标会停在组合串起点，
                // 看起来像「字往左边长」。
                set_caret(&self.context, ec, &range)?;
            }
            return Ok(());
        }

        if text.is_empty() {
            return Ok(());
        }

        // 起一个新组合串。
        let Some(start) = selection_range(&self.context, ec)? else {
            return Ok(());
        };
        // 先把选区删掉：用户选中几个字再打字时，语义就是「替换」。
        start.SetText(ec, 0, &[])?;
        start.Collapse(ec, TF_ANCHOR_END)?;

        let ctx_comp: ITfContextComposition = self.context.cast()?;
        // `StartComposition` 的 sink 允许为 null（`Option<&T>` 是它的 Param 实现之一），
        // 没有 sink 只是收不到「组合串被外部终止」的通知，功能本身不受影响。
        let comp = ctx_comp.StartComposition(ec, &start, self.sink.as_ref())?;
        // 组合串的 range 现在是一个位于光标处的空 range，往里写就是 preedit 内容。
        let range = comp.GetRange()?;
        write_text(ec, &range, text)?;
        set_caret(&self.context, ec, &range)?;
        *slot = Some(comp);
        Ok(())
    }

    /// 结束组合串。`final_text` 为 `None` 时抹掉内容（取消）。
    unsafe fn finish(&self, ec: u32, final_text: Option<&str>) -> Result<()> {
        // 先摘出句柄再 EndComposition：`OnCompositionTerminated` 靠「槽里还有没有
        // 这个对象」判断这次终止是不是我们自己发起的。
        let taken = self.slot.borrow_mut().take();

        let Some(comp) = taken else {
            // 没有组合串（例如 StartComposition 曾经失败过，或宿主不支持）：
            // 有文字就直接插到光标处，否则什么也不做。
            if let Some(text) = final_text {
                insert_at_selection(&self.context, ec, text)?;
            }
            return Ok(());
        };

        let range = comp.GetRange()?;
        match final_text {
            Some(text) => {
                write_text(ec, &range, text)?;
                set_caret(&self.context, ec, &range)?;
            }
            None => {
                range.SetText(ec, 0, &[])?;
            }
        }
        comp.EndComposition(ec)?;
        Ok(())
    }
}

impl ITfEditSession_Impl for PreeditSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        // panic 跨出 `extern "system"` 是未定义行为，必须自己兜住（D21）。
        match catch_unwind(AssertUnwindSafe(|| unsafe { self.apply(ec) })) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                tsf_error!("组合串写入失败：{e}");
                Err(e)
            }
            Err(_) => {
                tsf_error!("组合串写入时 panic 被拦截");
                Ok(())
            }
        }
    }
}

/// 改写 range 的内容并把 range 收到文字末尾。
unsafe fn write_text(ec: u32, range: &ITfRange, text: &str) -> Result<()> {
    let buf: Vec<u16> = text.encode_utf16().collect();
    // `SetText` 会把 range 调整成正好覆盖新文字，所以组合串会跟着变长变短。
    range.SetText(ec, 0, &buf)?;
    // 收拢到末尾：range 的两端重合时，光标位置才有意义（下面 SetSelection 用）。
    range.Collapse(ec, TF_ANCHOR_END)?;
    Ok(())
}

/// 取当前选区的 range。没有选区（极少见）返回 `None`。
unsafe fn selection_range(context: &ITfContext, ec: u32) -> Result<Option<ITfRange>> {
    let mut sel = [TF_SELECTION::default()];
    let mut fetched = 0u32;
    context.GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)?;
    if fetched == 0 {
        return Ok(None);
    }
    // windows-rs 把 `range` 包成 `ManuallyDrop`：引用是 API 交出来的，所有权在我们手上，
    // 必须自己取走，否则每敲一次键泄漏一个引用计数。
    Ok(ManuallyDrop::take(&mut sel[0].range))
}

/// 把光标放到 `range` 的位置。
unsafe fn set_caret(context: &ITfContext, ec: u32, range: &ITfRange) -> Result<()> {
    let mut out = [TF_SELECTION {
        range: ManuallyDrop::new(Some(range.clone())),
        style: TF_SELECTIONSTYLE {
            ase: TF_AE_END,
            fInterimChar: FALSE,
        },
    }];
    let r = context.SetSelection(ec, &out);
    ManuallyDrop::drop(&mut out[0].range);
    r
}

/// 没有组合串时的降级路径：直接在光标处写入文字。
///
/// 两种情况会走到这里：`StartComposition` 失败（宿主不支持组合串），或者句柄在
/// 中途被外部终止了。此时功能降级成 M1.1 的行为——不显示 preedit，但字照样上得去。
unsafe fn insert_at_selection(context: &ITfContext, ec: u32, text: &str) -> Result<()> {
    let Some(range) = selection_range(context, ec)? else {
        return Ok(());
    };
    // 选区非空时 `SetText` 会整体替换，这正是「打字覆盖选区」。
    write_text(ec, &range, text)?;
    set_caret(context, ec, &range)
}
