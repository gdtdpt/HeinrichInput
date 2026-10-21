//! 光标矩形：候选窗贴哪儿。
//!
//! ## 为什么要开一次编辑会话
//!
//! `ITfContextView::GetTextExt` 要一个**读 cookie**（`ec`），而 cookie 只在编辑
//! 会话里有效。所以这里与组合串写入走同一条路：包一个 `ITfEditSession`，在
//! `DoEditSession(ec)` 里问一次位置。它是 `TF_ES_READ`（只读），纯粹取信息。
//!
//! Weasel 的 `_GetCompositionRect` 也是这么做的——TSF 没有「读一下光标在哪」的
//! 捷径，想拿坐标就得进会话。
//!
//! ## 三级兜底（M2.3 实测逼出来的）
//!
//! | 顺位 | 来源 | 谁需要它 |
//! |---|---|---|
//! | ① | 选区（折叠 range = 插入符） | 记事本、Notepad++ |
//! | ② | 组合串整体的右下角 | 选区拿不到时 |
//! | ③ | `GetGUIThreadInfo` 的插入符矩形 | **Chromium** |
//!
//! 为什么必须有第 ③ 级：Chromium 的 TSF 实现在组合串刚建立、还没走完一次排版
//! 时，会对 `GetTextExt` 直接返回 `TF_E_NOLAYOUT (0x80040206)`——「这个文档现在
//! 没有排版信息」。记事本与 Notepad++ 从不这样。第一版用 `?` 把错误抛出去，
//! 于是后两级**一行都跑不到**，表现为「Chrome 里一个候选窗都不显示，但打字正常」。
//!
//! 所以本模块的规矩是：**每一次尝试都只记日志、不报错**，一条路走不通就换下一条，
//! 三条都不行才放弃（放弃 = 这次不显示窗口，输入照常）。`GetGUIThreadInfo` 不依赖
//! TSF 的排版，代价是坐标来自**窗口**而不是文档；少数自绘编辑框不给 `hwndCaret`，
//! 那也就算了。
//!
//! ## 为什么优先问「选区」而不是「组合串整体」
//!
//! 组合串的 range 覆盖整串编码（`vbg` 三个字母），`GetTextExt` 返回它们合起来的
//! 外接矩形。一旦宿主把组合串折行，那个框会横跨两行，下沿跑到第二行去。
//! 我们每次写入组合串后都把选区收到组合串末尾（`preedit::set_caret`），所以**选区
//! 就是一个折叠 range，正好在光标处**——`GetTextExt` 对折叠 range 给的就是插入符矩形。

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;

use heinrich_ui::Rect;
use windows::core::{implement, Result};
use windows::Win32::Foundation::{FALSE, RECT};
use windows::Win32::UI::TextServices::{
    ITfContext, ITfContextView, ITfEditSession, ITfEditSession_Impl, ITfRange,
};

use crate::preedit::{selection_range, CompSlot};

/// 探测结果放这里。会话同步执行，所以调用方拿回来时已经填好了。
pub type CaretSlot = Rc<RefCell<Option<Rect>>>;

/// 一次「问光标在哪」的只读会话。
#[implement(ITfEditSession)]
pub struct CaretProbe {
    context: ITfContext,
    comp: CompSlot,
    /// 第 ③ 级兜底要 `GetGUIThreadInfo(tid)`，tid 就是 TSF 线程 id。
    tid: u32,
    slot: CaretSlot,
}

impl CaretProbe {
    pub fn new(context: ITfContext, comp: CompSlot, tid: u32) -> (Self, CaretSlot) {
        let slot: CaretSlot = Rc::new(RefCell::new(None));
        (
            Self {
                context,
                comp,
                tid,
                slot: Rc::clone(&slot),
            },
            slot,
        )
    }

    /// 转成 `ITfEditSession`（`RequestEditSession` 要的就是它）。
    pub fn into_session(probe: Self) -> ITfEditSession {
        probe.into()
    }

    unsafe fn probe(&self, ec: u32) -> Result<Option<Rect>> {
        let view: ITfContextView = self.context.GetActiveView()?;

        // ① 选区
        let selection = unsafe { selection_range(&self.context, ec) }.ok().flatten();
        if let Some(rect) = text_ext(&view, ec, selection.as_ref()).filter(|r| r.h > 0.0) {
            tsf_debug!("# 光标来自选区 {rect:?}");
            return Ok(Some(rect));
        }

        // ② 组合串整体 → 右下角
        //
        // 连问两次：Chromium 对新建立的组合串第一次问会给 `TF_E_NOLAYOUT`，
        // 紧接着再问一次它就现算排版并给出真实矩形（M2.3 实测）。第二次仍然失败
        // 才算真拿不到。
        let comp_range = self.comp.borrow().as_ref().and_then(|c| c.GetRange().ok());
        for attempt in 0..2 {
            if let Some(rect) = text_ext(&view, ec, comp_range.as_ref()) {
                let caret = Rect::new(rect.right(), rect.y, 0.0, rect.h);
                tsf_debug!("# 光标来自组合串右下角 {caret:?}（第 {} 次）", attempt + 1);
                return Ok(Some(caret));
            }
        }

        // ③ GUI 插入符
        if let Some(rect) = gui_caret(self.tid) {
            tsf_debug!("# 光标来自 GUI 插入符 {rect:?}");
            return Ok(Some(rect));
        }

        // 返回 `None` 不代表放弃：调用方手里通常还捏着「动手前」量到的那一个
        // 位置（见 `TextService::show_candidates`）。这里只如实说明本层没量到。
        tsf_debug!("# 三级兜底都没量到光标");
        Ok(None)
    }
}

impl ITfEditSession_Impl for CaretProbe_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        // panic 跨出 `extern "system"` 是未定义行为，必须自己兜住（D21）。
        match catch_unwind(AssertUnwindSafe(|| unsafe { self.probe(ec) })) {
            Ok(Ok(rect)) => {
                *self.slot.borrow_mut() = rect;
                Ok(())
            }
            Ok(Err(e)) => {
                // 拿不到 context 的视图不是错误：不显示候选窗而已（键入照常）。
                tsf_debug!("# 光标位置查询失败：{e}");
                Ok(())
            }
            Err(_) => {
                tsf_error!("光标位置查询时 panic 被拦截");
                Ok(())
            }
        }
    }
}

/// 问一个 range 的屏幕矩形。**失败只记日志、返回 `None`**（见模块文档）。
///
/// `ITfContextView::GetTextExt` 直接吃 `ITfRange`（windows-rs 的绑定没有
/// `ITfRange::GetStart/GetEnd`，也不需要——锚点由宿主自己取）。
unsafe fn text_ext(view: &ITfContextView, ec: u32, range: Option<&ITfRange>) -> Option<Rect> {
    let range = range?;
    let mut rc = RECT::default();
    let mut clipped = FALSE;
    match unsafe { view.GetTextExt(ec, range, &mut rc, &mut clipped) } {
        Ok(()) => {}
        Err(e) => {
            tsf_debug!("# GetTextExt 失败：{e}");
            return None;
        }
    }
    if rc.right <= rc.left && rc.bottom <= rc.top {
        return None;
    }
    Some(Rect::new(
        rc.left as f32,
        rc.top as f32,
        (rc.right - rc.left) as f32,
        (rc.bottom - rc.top) as f32,
    ))
}

/// GUI 线程的插入符矩形（屏幕坐标）。
///
/// `GUITHREADINFO.rcCaret` 是**客户区**坐标，要 `ClientToScreen` 换算。
fn gui_caret(tid: u32) -> Option<Rect> {
    use windows::Win32::Foundation::POINT;
    // `ClientToScreen` 在 Gdi 里（不在 WindowsAndMessaging）——按名字找会找错模块。
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::{GetGUIThreadInfo, GUITHREADINFO};

    unsafe {
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(tid, &mut info).ok()?;
        if info.hwndCaret.0.is_null() {
            return None;
        }
        let h = (info.rcCaret.bottom - info.rcCaret.top).max(1);
        let mut pt = POINT {
            x: info.rcCaret.left,
            y: info.rcCaret.bottom,
        };
        if !ClientToScreen(info.hwndCaret, &mut pt).as_bool() {
            return None;
        }
        // 宽 0：`place()` 只用 x 与 bottom，宽度对定位没有意义。
        Some(Rect::new(pt.x as f32, (pt.y - h) as f32, 0.0, h as f32))
    }
}
