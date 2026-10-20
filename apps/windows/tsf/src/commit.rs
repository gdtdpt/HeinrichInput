//! 上屏：把一段文本写进宿主的文档。
//!
//! ## 为什么必须走 `ITfEditSession`
//!
//! TSF 不允许输入法直接改文档。所有写操作都要包在一个「编辑会话」里，
//! 由 `ITfContext::RequestEditSession` 提交，在 `DoEditSession(ec)` 里拿到
//! `ec`（edit cookie）后才能 `GetSelection` / `SetText`。
//!
//! ## 为什么用 `TF_ES_SYNC`
//!
//! 同步会话看着像「阻塞」，但它等的是**宿主自己的 UI 线程**——TSF 就是在那个
//! 线程上回调我们的，所以这里不存在跨进程往返。D18 约束的是 IPC，不是这个。
//!
//! 用异步（`TF_ES_ASYNCDONTCARE`）反而有害：连续敲两下空格，两次上屏的先后
//! 顺序就没有保证了。
//!
//! ## `TF_SELECTION` 里的 `ManuallyDrop`
//!
//! windows-rs 把 `TF_SELECTION.range` 包成 `ManuallyDrop`，因为那是 API 交出来的
//! 引用、所有权在我们手上。**拿完必须自己放**，否则每敲一次键泄漏一个引用计数。

use std::mem::ManuallyDrop;
use std::panic::{catch_unwind, AssertUnwindSafe};

use windows::core::{implement, Result};
use windows::Win32::Foundation::FALSE;
use windows::Win32::UI::TextServices::{
    ITfContext, ITfEditSession, ITfEditSession_Impl, ITfRange, TF_AE_END, TF_ANCHOR_END,
    TF_DEFAULT_SELECTION, TF_SELECTION, TF_SELECTIONSTYLE,
};

/// 一次「把 `text` 写到当前选区」的编辑会话。
#[implement(ITfEditSession)]
pub struct CommitSession {
    context: ITfContext,
    /// UTF-16，**不含结尾 NUL**——`ITfRange::SetText` 收的是切片，长度由调用方给。
    text: Vec<u16>,
}

impl CommitSession {
    pub fn new(context: ITfContext, text: &str) -> Self {
        Self {
            context,
            text: text.encode_utf16().collect(),
        }
    }

    /// 真正的写入。调用时必须已经持有 `ec`。
    ///
    /// # Safety
    ///
    /// 只能在 `DoEditSession` 里、用系统给的 `ec` 调用。
    unsafe fn write(&self, ec: u32) -> Result<()> {
        let mut sel = [TF_SELECTION::default()];
        let mut fetched = 0u32;
        self.context
            .GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)?;
        if fetched == 0 {
            return Ok(());
        }

        // 取走引用，数组里那份立刻放掉，避免泄漏。
        let taken: Option<ITfRange> = ManuallyDrop::take(&mut sel[0].range);
        let Some(range) = taken else {
            return Ok(());
        };

        // 选区若是非空的（用户先选中了几个字再打字），SetText 会整体替换掉——
        // 这正是「打字覆盖选区」的预期行为。
        range.SetText(ec, 0, &self.text)?;
        // 上屏后光标跟到文本末尾，否则下一个字会插到刚写的字前面。
        range.Collapse(ec, TF_ANCHOR_END)?;

        let mut out = [TF_SELECTION {
            range: ManuallyDrop::new(Some(range)),
            style: TF_SELECTIONSTYLE {
                ase: TF_AE_END,
                fInterimChar: FALSE,
            },
        }];
        let r = self.context.SetSelection(ec, &out);
        ManuallyDrop::drop(&mut out[0].range);
        r
    }
}

impl ITfEditSession_Impl for CommitSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        // panic 跨出 `extern "system"` 是未定义行为，这里必须自己兜住。
        // 与 `text_service.rs` 的 `guarded()` 是同一个理由（D21）。
        match catch_unwind(AssertUnwindSafe(|| unsafe { self.write(ec) })) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                tsf_error!("上屏失败：{e}");
                Err(e)
            }
            Err(_) => {
                tsf_error!("上屏时 panic 被拦截");
                Ok(())
            }
        }
    }
}
