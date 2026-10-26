//! 亨利输入法 · Windows 候选窗。
//!
//! 这个 crate 是 D36 里「绘制」的那一半：它**不自己算任何矩形**，只把
//! [`heinrich_ui::layout`] 吐出来的矩形画到屏幕上。
//!
//! ```no_run
//! # fn main() -> windows::core::Result<()> {
//! use heinrich_win_ui::{CandidateWindow, Candidates, Theme};
//! use heinrich_ui::place;
//!
//! let mut win = CandidateWindow::new(Theme::default())?;
//! let texts: Vec<String> = vec!["好".into(), "㛅".into()];
//! // 与 texts 等长：空串表示这一条不画码提示（§6.3）
//! let hints: Vec<String> = vec!["vbg".into(), String::new()];
//! // 与 texts 等长：chip 挂在第几个字符之后，0 = 词尾（D64）
//! let hint_ats: Vec<u8> = vec![0, 0];
//! // 与 texts 等长：true 表示这一条是 emoji 候选（v8/D83），要用彩色字形画
//! let is_emoji: Vec<bool> = vec![false, false];
//! let prepared = win.prepare(&Candidates {
//!     texts: &texts,
//!     hints: &hints,
//!     hint_ats: &hint_ats,
//!     is_emoji: &is_emoji,
//!     page: 0,
//!     page_size: 5,
//!     highlight: 0,
//!     split: texts.len(), // 只有一段；混输时传折叠区第一条的全局序号
//!     anchor: (400, 500), // 光标位置，只用来查这块屏的 DPI
//! })?;
//! let p = place(
//!     heinrich_ui::Rect::new(400.0, 500.0, 2.0, 20.0),
//!     prepared.layout.size,
//!     heinrich_ui::Rect::new(0.0, 0.0, 1920.0, 1040.0),
//!     2.0,
//! );
//! win.show(&prepared, (p.rect.x as i32, p.rect.y as i32))?;
//! # Ok(()) }
//! ```
//!
//! ## 为什么窗口是「分层 + 不激活」
//!
//! 三层约束叠起来，缺一条都会出问题：
//!
//! | 样式 | 作用 | 少写会怎样 |
//! |---|---|---|
//! | `WS_EX_LAYERED` + `UpdateLayeredWindow` | 圆角外**真透明**，不做色键抠图 | 圆角变成白角，盖住底下的字 |
//! | `WS_EX_NOACTIVATE` | 显示时不抢前台 | 组合串被终止，打一半的编码消失 |
//! | `WM_MOUSEACTIVATE` → `MA_NOACTIVATE` | **点它也不抢前台** | 点候选 = 切换焦点 = 组合串丢失 |
//! | `WS_EX_TOOLWINDOW` | 不进 Alt+Tab、不进任务栏 | 每敲一个字就多一个幽灵窗口 |
//!
//! 第三条是 Weasel 踩过的坑：`WS_EX_NOACTIVATE` 只管「显示时不激活」，鼠标点上去
//! 仍会走激活流程，必须由窗口过程自己拒绝。所以命中测试与点击处理都在本 crate 内
//! 完成，通过 [`Event::Click`] 把**全局候选序号**交回调用方。
//!
//! ## 线程与 COM
//!
//! 本 crate **不调用** `CoInitializeEx`：它要跑在 TSF 宿主进程里，那里的 COM 套间
//! 由宿主决定（TSF 要求 STA）。`cand-preview.exe` 自己初始化。窗口与所有 D2D/DWrite
//! 对象都绑定创建它的线程，跨线程使用会出错——TSF 侧只在按键线程用，符合这个前提。

mod keycap_window;
mod render;
mod text;

pub use keycap_window::{CapData, KeycapWindow, Keycaps, PreparedKeycaps, KEYCAP_FOOTNOTE};
pub use render::Theme;

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Once;

use heinrich_ui::{
    layout, page_nav, page_slice, CandidateText, Dir, Layout, LayoutInput, Metrics, Rect,
};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetDC, MonitorFromPoint, ReleaseDC, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, HDC,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, LoadCursorW, RegisterClassW,
    SetWindowLongPtrW, ShowWindow, UpdateLayeredWindow, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    GWLP_USERDATA, IDC_ARROW, MA_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WM_DPICHANGED,
    WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_NCCREATE, WM_NCDESTROY, WNDCLASSW, WNDPROC,
    WS_CLIPSIBLINGS, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// 候选集。分页信息在这里，具体的矩形由几何层算。
#[derive(Clone, Debug)]
pub struct Candidates<'a> {
    /// **全部**候选（不只是本页）。分页与「还有下一页」都按它算。
    pub texts: &'a [String],
    /// 与 `texts` 等长的码提示（§6.3）。空串 = 这条不画 chip。
    ///
    /// 裁决在 server（它知道候选来自哪一段、`hint.mode` 是什么），这一层只管画。
    /// 长度不一致时按 `texts` 对齐，缺的部分当作不提示——协议与 UI 各自守住自己的
    /// 边界，比「假设调用方一定传对」安全。
    pub hints: &'a [String],
    /// 与 `texts` 等长的 chip 词中位置（D64）：`hints[i]` 画在第 `hint_ats[i]`
    /// 个字符之后，`0` = 词尾。多段组合的 chip 贴着拼音段的字，例如
    /// `好[vbg]吃`。缺省按词尾处理。
    pub hint_ats: &'a [u8],
    /// 与 `texts` 等长：`true` = 这一条是 **emoji 候选**（v8/D83）。
    ///
    /// 这种候选的 `texts[i]` 就是那枚 emoji 本身，量宽要用彩色字形那条路
    /// （[`measure_emoji`](crate::Renderer)），渲染要开 `ENABLE_COLOR_FONT`。
    /// 长度不一致时按 `texts` 对齐，缺的一律当普通词。
    pub is_emoji: &'a [bool],
    pub page: usize,
    pub page_size: usize,
    /// 本页内的键盘选中项（0 起）。
    pub highlight: usize,
    /// 折叠区第一条的**全局**序号（D48）。等于 `texts.len()` 或更大 = 只有一段。
    ///
    /// 混输下候选分两段：主段（五笔优先）在前、补段（拼音）在后。两段必须
    /// 在视觉上分开，否则用户分不清哪条是五笔打出来的、哪条是拼音猜的。
    pub split: usize,
    /// 光标位置（屏幕坐标）。只为确定这块屏的 DPI——候选窗必须按它所在屏幕缩放，
    /// 不能按主屏。
    pub anchor: (i32, i32),
}

/// 量好字、算好矩形的结果，交给 [`CandidateWindow::show`]。
#[derive(Clone, Debug)]
pub struct Prepared {
    pub layout: Layout,
    pub metrics: Metrics,
    /// 本屏的 DPI 缩放（1.0 = 96 DPI）。
    pub dpi_scale: f32,
}

/// 命中了什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// 候选的**全局**序号（不是本页内的序号）。
    Candidate(usize),
    /// 翻页箭头。
    Arrow(Dir),
}

/// 窗口回调给调用方的事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// 左键点了候选或箭头。**只报事件，不改变显示**——翻页由调用方决定后重新
    /// [`CandidateWindow::show`]。
    Click(Hit),
    /// 窗口所在显示器的 DPI 变了（拖到另一块屏）。调用方应重新 `prepare` + `show`。
    DpiChanged(u32),
}

/// 候选窗。一个进程里可以只建一个、反复 `show` / `hide`。
pub struct CandidateWindow {
    hwnd: HWND,
    state: Box<State>,
}

struct State {
    renderer: render::Renderer,
    theme: Theme,
    /// 当前显示的内容。命中测试用它，所以 `show` 之后必须留着。
    layout: Option<Layout>,
    on_event: Option<Box<dyn FnMut(Event)>>,
    visible: bool,
}

impl CandidateWindow {
    /// 建一个隐藏的候选窗。窗口从此刻起归本线程，直到 drop。
    pub fn new(theme: Theme) -> Result<Self> {
        register_class()?;
        let hinstance = unsafe { GetModuleHandleW(None)? };

        let state = Box::new(State {
            renderer: render::Renderer::new()?,
            theme,
            layout: None,
            on_event: None,
            visible: false,
        });

        let ex_style = WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                CLASS_NAME,
                w!("亨利候选窗"),
                WS_POPUP | WS_CLIPSIBLINGS,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(HINSTANCE(hinstance.0)),
                // 窗口过程在 `WM_NCCREATE` 里把它存进 GWLP_USERDATA。
                // 生命周期由 `CandidateWindow` 保证：drop 时先 DestroyWindow。
                Some(&*state as *const State as *const core::ffi::c_void),
            )
        }?;

        Ok(Self { hwnd, state })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// 当前渲染位图的设备上下文。给预览器落盘验收用：那块 DIB 就是
    /// `UpdateLayeredWindow` 要上屏的内容。没画过时为 `None`。
    pub fn hdc(&self) -> Option<HDC> {
        self.state.renderer.hdc()
    }

    pub fn theme(&self) -> Theme {
        self.state.theme
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.state.theme = theme;
    }

    /// 量文字 + 算布局，但不显示。
    ///
    /// 分两步（先 `prepare` 再 `show`）是因为**窗口放哪取决于它多大**：调用方要拿
    /// `prepared.layout.size` 去 [`heinrich_ui::place`] 算位置，才能避开屏幕边界。
    ///
    /// DPI 取自 `c.anchor` 所在的那块屏，不是主屏——双屏一高一低时按主屏缩放会
    /// 让候选窗在副屏上小一圈。
    pub fn prepare(&mut self, c: &Candidates<'_>) -> Result<Prepared> {
        let scale = dpi_at(c.anchor) as f32 / 96.0;
        self.prepare_scaled(c, scale)
    }

    /// 指定缩放比例的 [`prepare`](Self::prepare)。给预览器与 DPI 验收用：
    /// 没有 150% 的显示器时也能看 150% 的效果。
    pub fn prepare_scaled(&mut self, c: &Candidates<'_>, scale: f32) -> Result<Prepared> {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let metrics = Metrics::new(self.state.theme.font_size).scaled(scale);

        let mut measured = Vec::with_capacity(c.texts.len());
        for (i, text) in c.texts.iter().enumerate() {
            let emoji = c.is_emoji.get(i).copied().unwrap_or(false);
            // emoji 候选的文字**就是**那枚 emoji，量宽必须走彩色字形那条路：
            // 它的自然前进宽度与汉字不等，按主字号量出来的矩形会差一条缝。
            let w = if emoji {
                self.state
                    .renderer
                    .text_mut()
                    .measure_emoji(text, metrics.font_size)?
            } else {
                self.state
                    .renderer
                    .text_mut()
                    .measure(text, metrics.font_size)?
            };
            let mut item = CandidateText::new(text.clone(), w);
            if emoji {
                item = item.as_emoji();
            }
            // 只有 chip 真会被画出来才去量它：量字要建 `IDWriteTextLayout`，
            // 给每条候选白量一次是纯粹的浪费（五笔段一条都不带 chip）。
            if let Some(hint) = c.hints.get(i).filter(|h| !h.is_empty()) {
                let hw = self
                    .state
                    .renderer
                    .text_mut()
                    .measure_hint(hint, metrics.font_size)?;
                // 词中 chip（D64）：前后两截分开画，宽度必须各量各的——
                // 拿整串宽度减 chip 宽度去凑，字距误差会在 chip 两侧漏缝。
                let at = c.hint_ats.get(i).copied().unwrap_or(0) as usize;
                if at > 0 {
                    let pre: String = text.chars().take(at).collect();
                    let post: String = text.chars().skip(at).collect();
                    let pw = self
                        .state
                        .renderer
                        .text_mut()
                        .measure(&pre, metrics.font_size)?;
                    let qw = self
                        .state
                        .renderer
                        .text_mut()
                        .measure(&post, metrics.font_size)?;
                    item = item.with_hint_at(hint.clone(), hw, at, pw, qw);
                } else {
                    item = item.with_hint(hint.clone(), hw);
                }
            }
            measured.push(item);
        }

        let total = measured.len();
        let range = page_slice(total, c.page_size, c.page);
        let (has_prev, has_next) = page_nav(total, c.page_size, c.page);
        let page: Vec<CandidateText> = measured[range.clone()].to_vec();

        let layout = layout(&LayoutInput {
            page: &page,
            page_start: range.start,
            has_prev,
            has_next,
            highlight: Some(c.highlight),
            // 全局序号直接透传；越界（= 没有折叠区）时 layout 侧一次都不会命中。
            split: (c.split < total).then_some(c.split),
            metrics,
        });

        Ok(Prepared {
            layout,
            metrics,
            dpi_scale: scale,
        })
    }

    /// 画出来并显示在 `at`（屏幕坐标，左上角）。
    pub fn show(&mut self, prepared: &Prepared, at: (i32, i32)) -> Result<()> {
        // 设备丢失时重画一次就够了：`draw` 已经把 surface 扔了，第二次会重建。
        if !self.state.renderer.draw(prepared, &self.state.theme)? {
            self.state.renderer.draw(prepared, &self.state.theme)?;
        }

        let hdc = self
            .state
            .renderer
            .hdc()
            .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;

        let size = SIZE {
            cx: prepared.layout.size.w.ceil().max(1.0) as i32,
            cy: prepared.layout.size.h.ceil().max(1.0) as i32,
        };
        let dst = POINT { x: at.0, y: at.1 };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        let screen = unsafe { GetDC(None) };
        let result = unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                Some(screen),
                Some(&dst),
                Some(&size),
                Some(hdc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        };
        unsafe { ReleaseDC(None, screen) };
        result?;

        // SW_SHOWNOACTIVATE：显示但不激活。少了它就会把光标从宿主那里抢走。
        let _ = unsafe { ShowWindow(self.hwnd, SW_SHOWNOACTIVATE) };

        self.state.layout = Some(prepared.layout.clone());
        self.state.visible = true;
        Ok(())
    }

    pub fn hide(&mut self) {
        if self.state.visible {
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            self.state.visible = false;
            self.state.layout = None;
        }
    }

    pub fn is_visible(&self) -> bool {
        self.state.visible
    }

    /// 当前显示内容的布局（未显示时为 `None`）。
    pub fn layout(&self) -> Option<&Layout> {
        self.state.layout.as_ref()
    }

    /// 命中测试：客户区坐标 → 候选（全局序号）或箭头。
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        let layout = self.state.layout.as_ref()?;
        if let Some(dir) = layout.hit_arrow(x, y) {
            return Some(Hit::Arrow(dir));
        }
        layout.hit(x, y).map(Hit::Candidate)
    }

    pub fn set_on_event(&mut self, handler: impl FnMut(Event) + 'static) {
        self.state.on_event = Some(Box::new(handler));
    }
}

impl Drop for CandidateWindow {
    fn drop(&mut self) {
        // 先销毁窗口：窗口过程还持有 `state` 的裸指针，顺序反了就是 use-after-free。
        if !self.hwnd.0.is_null() {
            let _ = unsafe { DestroyWindow(self.hwnd) };
        }
    }
}

const CLASS_NAME: PCWSTR = w!("HeinrichCandidateWindowV1");

static REGISTER_ONCE: Once = Once::new();
static CLASS_ATOM: AtomicU16 = AtomicU16::new(0);

fn register_class() -> Result<()> {
    register_window_class(CLASS_NAME, Some(wndproc), &REGISTER_ONCE, &CLASS_ATOM)
}

/// 注册一个窗口类，把结果记进 `atom`。
///
/// 候选窗与键位图各一个类名、各一个窗口过程：两者的鼠标语义相反（一个必须吃掉
/// 点击以便选词，一个必须放行），共用一个过程只会让里面塞满 `if`。
pub(crate) fn register_window_class(
    name: PCWSTR,
    proc: WNDPROC,
    once: &Once,
    atom: &AtomicU16,
) -> Result<()> {
    once.call_once(|| {
        let hinstance = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: proc,
            hInstance: HINSTANCE(hinstance.0),
            hCursor: cursor,
            lpszClassName: name,
            ..Default::default()
        };
        let registered = unsafe { RegisterClassW(&wc) };
        atom.store(registered, Ordering::Release);
    });

    if atom.load(Ordering::Acquire) == 0 {
        return Err(windows::core::Error::from(
            windows::Win32::Foundation::E_FAIL,
        ));
    }
    Ok(())
}

/// 从 `GWLP_USERDATA` 取回窗口状态。
///
/// 裸指针能成立的前提：`CandidateWindow` 活着的时候窗口一定活着，且窗口销毁发生在
/// `Box<State>` 释放之前（见 `Drop`）。窗口消息只在本线程派发，不存在并发访问。
unsafe fn state_of(hwnd: HWND) -> Option<&'static mut State> {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
    unsafe { ptr.as_mut() }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
            LRESULT(1)
        }
        // 关键：鼠标点上来也**不激活**。`WS_EX_NOACTIVATE` 只覆盖「显示」那一步。
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // 分层窗口不会走常规擦除路径，返回 1 免得系统自己填背景（会闪一下白）。
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as u16 as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
            if let Some(state) = unsafe { state_of(hwnd) } {
                if let Some(hit) = state.layout.as_ref().and_then(|l| {
                    l.hit_arrow(x, y)
                        .map(Hit::Arrow)
                        .or_else(|| l.hit(x, y).map(Hit::Candidate))
                }) {
                    if let Some(cb) = state.on_event.as_mut() {
                        cb(Event::Click(hit));
                    }
                }
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = (wparam.0 & 0xFFFF) as u32;
            if let Some(state) = unsafe { state_of(hwnd) } {
                if let Some(cb) = state.on_event.as_mut() {
                    cb(Event::DpiChanged(dpi));
                }
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 光标所在显示器的有效 DPI（96 = 100%）。
pub(crate) fn dpi_at(point: (i32, i32)) -> u32 {
    unsafe {
        let monitor = MonitorFromPoint(
            POINT {
                x: point.0,
                y: point.1,
            },
            MONITOR_DEFAULTTONEAREST,
        );
        let mut dx = 96u32;
        let mut dy = 96u32;
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_ok() && dx > 0 {
            dx
        } else {
            96
        }
    }
}

/// 让本进程按「每显示器 DPI 感知 v2」工作。
///
/// **只给独立 exe 用，注入型 DLL 绝不能调**：DPI 感知是**进程级**属性，DLL 跑在
/// 宿主进程里，改了等于替宿主做决定——轻则让宿主自己的界面错位，重则让
/// `chrome` 这类对 DPI 变化敏感的宿主整窗重排。TSF 宿主通常已经是 v2，
/// 我们只管问 `GetDpiForMonitor` 就行。
pub fn enable_per_monitor_dpi() -> Result<()> {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
}

/// 光标所在显示器的工作区（不含任务栏）。给调用方算 [`heinrich_ui::place`] 用。
pub fn work_area_at(point: (i32, i32)) -> Rect {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
    unsafe {
        let monitor = MonitorFromPoint(
            POINT {
                x: point.0,
                y: point.1,
            },
            MONITOR_DEFAULTTONEAREST,
        );
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let r = info.rcWork;
            Rect::new(
                r.left as f32,
                r.top as f32,
                (r.right - r.left) as f32,
                (r.bottom - r.top) as f32,
            )
        } else {
            Rect::new(0.0, 0.0, 1920.0, 1040.0)
        }
    }
}
