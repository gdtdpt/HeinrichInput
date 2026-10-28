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
//!
//! ## 三级兜底也会撒谎（D104）
//!
//! 三级兜底解决的是「答不出来」，没解决「答了个错值」：坐标由宿主算，宿主在
//! 组合串刚建立、排版未落定时回来的可能是**上一处输入框的缓存值**——纵向贴着
//! 光标、横向偏出几百像素，看起来像窗口自己飞走了。
//!
//! 本模块的两条对策：① 每条来源都过一遍视口校验（[`view_screen_rect`] /
//! [`plausible`]），越出编辑区的一律作废；② 把来源（[`Source`]）交回调用方，
//! 由它在**下一轮消息里再量一次**做对照（`Core::on_recheck_caret`）。视口校验
//! 拦不住「偏了但还在窗口里」这种，复量能——因为那时宿主的排版已经落定。

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

/// 光标矩形的来源（D104）。
///
/// 调用方要它做两件事：日志里说清「这次是谁答的」，以及决定要不要安排一次
/// 「晚一轮消息的复量」——②③ 本来就比 ① 不可信，值不值得再量一次由来源说了算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// ① 选区。我们把选区收在组合串末尾，折叠 range 就是插入符。
    Selection,
    /// ② 组合串整体的右下角。
    Composition,
    /// ③ `GetGUIThreadInfo` 的插入符矩形。
    GuiCaret,
}

/// 探测结果放这里。会话同步执行，所以调用方拿回来时已经填好了。
pub type CaretSlot = Rc<RefCell<Option<(Rect, Source)>>>;

/// 视口校验的容差（逻辑像素）。宿主的视图矩形本身可能不含边框内衬，
/// 插入符又贴着边写，留一点余量免得把正常位置判成越界。
const VIEW_SLACK: f32 = 8.0;

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

    unsafe fn probe(&self, ec: u32) -> Result<Option<(Rect, Source)>> {
        let view: ITfContextView = self.context.GetActiveView()?;
        // 视口矩形：三条来源都要过这一关（D104）。拿不到就不校验——
        // 宁可按老规矩来，也不能因为少一个矩形就一个窗口都不显示。
        let view_rect = view_screen_rect(&view);

        // ① 选区
        let selection = unsafe { selection_range(&self.context, ec) }.ok().flatten();
        let from_selection = text_ext(&view, ec, selection.as_ref())
            .filter(|r| r.h > 0.0)
            .filter(|r| plausible(*r, view_rect));

        // ② 组合串整体 → 右下角
        //
        // ① 拿到时只问一次（这一问只为日志里的三方对照）；① 没拿到才按老规矩
        // 连问两次——Chromium 对新建立的组合串第一次问会给 `TF_E_NOLAYOUT`，
        // 紧接着再问一次它就现算排版并给出真实矩形（M2.3 实测）。第二次仍然失败
        // 才算真拿不到。
        let comp_range = self.comp.borrow().as_ref().and_then(|c| c.GetRange().ok());
        let tries = if from_selection.is_some() { 1 } else { 2 };
        let mut from_comp = None;
        for _ in 0..tries {
            if let Some(rect) = text_ext(&view, ec, comp_range.as_ref()) {
                let caret = Rect::new(rect.right(), rect.y, 0.0, rect.h);
                if plausible(caret, view_rect) {
                    from_comp = Some(caret);
                    break;
                }
            }
        }

        // ③ GUI 插入符
        let from_gui = gui_caret(self.tid).filter(|r| plausible(*r, view_rect));

        // 三方对照打在一行里：定位一旦偏了，一眼能看出是哪一路在撒谎、
        // 以及「到底偏得有多离谱」。（真正偏的值往往还在视图之内——
        // 那正是 D104 采用「下一轮消息复量」而不是「视口校验」的原因。）
        tsf_debug!(
            "# 光标三方对照：选区={from_selection:?} 组合串={from_comp:?} GUI={from_gui:?} 视图={view_rect:?}"
        );

        let picked = if let Some(rect) = from_selection {
            Some((rect, Source::Selection))
        } else if let Some(rect) = from_comp {
            Some((rect, Source::Composition))
        } else {
            from_gui.map(|rect| (rect, Source::GuiCaret))
        };

        match picked {
            Some((rect, source)) => tsf_debug!("# 光标采用 {source:?} {rect:?}"),
            // 返回 `None` 不代表放弃：调用方手里通常还捏着「动手前」量到的那一个
            // 位置（见 `TextService::show_candidates`）。这里只如实说明本层没量到。
            None => tsf_debug!("# 三级兜底都没量到光标"),
        }
        Ok(picked)
    }
}

/// 光标矩形是否落在视图矩形里（外扩 [`VIEW_SLACK`]）。
///
/// 拿不到视图矩形时一律放行：这一关是**加保险**，不是新门槛。
fn plausible(rect: Rect, view: Option<Rect>) -> bool {
    let Some(view) = view else { return true };
    rect.right() >= view.x - VIEW_SLACK
        && rect.x <= view.right() + VIEW_SLACK
        && rect.bottom() >= view.y - VIEW_SLACK
        && rect.y <= view.bottom() + VIEW_SLACK
}

/// 视图（编辑区）在屏幕上的矩形。
fn view_screen_rect(view: &ITfContextView) -> Option<Rect> {
    let rc = unsafe { view.GetScreenExt() }.ok()?;
    if rc.right <= rc.left || rc.bottom <= rc.top {
        return None;
    }
    Some(Rect::new(
        rc.left as f32,
        rc.top as f32,
        (rc.right - rc.left) as f32,
        (rc.bottom - rc.top) as f32,
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 一块 800×600 的编辑区，左上角在屏幕 (100, 50)。
    fn view() -> Rect {
        Rect::new(100.0, 50.0, 800.0, 600.0)
    }

    #[test]
    fn 视图里的光标放行() {
        assert!(plausible(Rect::new(300.0, 200.0, 0.0, 20.0), Some(view())));
    }

    #[test]
    fn 没有视图矩形时一律放行() {
        // 这一关是加保险，不是新门槛：拿不到视图就不该把光标判死。
        let far = Rect::new(5000.0, 5000.0, 0.0, 20.0);
        assert!(plausible(far, None));
    }

    #[test]
    fn 明显在图外的不认() {
        // 右侧偏出 300px：这正是「候选窗飞很远」那种值的形态
        assert!(!plausible(
            Rect::new(1200.0, 200.0, 0.0, 20.0),
            Some(view())
        ));
        assert!(!plausible(Rect::new(300.0, 900.0, 0.0, 20.0), Some(view())));
    }

    #[test]
    fn 贴着边沿的仍在容差内() {
        // 右边界 900、容差 8：x = 908 还认，909 就不认了
        assert!(plausible(Rect::new(908.0, 200.0, 0.0, 20.0), Some(view())));
        assert!(!plausible(Rect::new(909.0, 200.0, 0.0, 20.0), Some(view())));
    }

    #[test]
    fn 宽度为零的插入符按左端点判() {
        // 光标矩形宽 0：右端点等于左端点，不能因此判它越界
        let caret = Rect::new(100.0, 50.0, 0.0, 20.0);
        assert!(plausible(caret, Some(view())));
    }
}
