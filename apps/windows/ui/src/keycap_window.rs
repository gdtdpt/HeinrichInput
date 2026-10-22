//! 码位键位图窗口（PROJECT.md §6.7 / D50）。
//!
//! 与候选窗同构：`WS_POPUP` + `WS_EX_LAYERED｜NOACTIVATE｜TOOLWINDOW｜TOPMOST`，
//! 显示走 `UpdateLayeredWindow`。它是**第二个**分层窗口实例，因此有自己的窗口类
//! 与自己的 `Renderer`（渲染目标的位图尺寸不同，共用一个实例会来回重建）。
//!
//! ## 与候选窗的三处差别
//!
//! | | 候选窗 | 键位图 |
//! |---|---|---|
//! | 鼠标 | 点候选 = 上屏，必须自己处理 | **完全穿透**（`WM_NCHITTEST` → `HTTRANSPARENT`），点它等于点在宿主文档上 |
//! | 内容 | 一页候选 | 一～四枚键帽 + 一行固定说明 |
//! | 定位 | 贴光标 | 贴**候选窗**（[`heinrich_ui::place_keycaps`]） |
//!
//! 第一条是刻意的：键位图是只读的参考信息，用户没有理由去点它。若让它吃掉点击，
//! 会出现「点在字上没反应」——比不弹它更让人困惑。

use std::sync::atomic::AtomicU16;
use std::sync::Once;

use heinrich_ui::{layout_keycaps, KeycapInput, KeycapLayout, KeycapMetrics, KeycapText};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, SetWindowLongPtrW, ShowWindow,
    UpdateLayeredWindow, CREATESTRUCTW, GWLP_USERDATA, HTTRANSPARENT, SW_HIDE, SW_SHOWNOACTIVATE,
    ULW_ALPHA, WM_ERASEBKGND, WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST, WS_CLIPSIBLINGS,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::render::Renderer;
use crate::Theme;

/// 一枚键帽的**数据**（不是矩形）。渲染所需的一切文字都在这里。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapData {
    pub key: char,
    /// 主字根在前、变体在后。
    pub roots: Vec<char>,
    /// `roots` 里前几枚是主字根，其余绘制时淡一档。
    pub main_root_count: usize,
}

/// 要显示的内容。
#[derive(Clone, Debug)]
pub struct Keycaps<'a> {
    pub caps: &'a [CapData],
    /// 底部固定说明（不针对具体候选，每次都一样）。空串 = 不画。
    pub footnote: &'a str,
    /// 候选窗所在位置，只为确定这块屏的 DPI。
    pub anchor: (i32, i32),
}

/// 量好字、算好矩形的结果。
#[derive(Clone, Debug)]
pub struct PreparedKeycaps {
    pub layout: KeycapLayout,
    pub metrics: KeycapMetrics,
    pub dpi_scale: f32,
    /// 说明行逐行文本，与 `layout.footnotes` 一一对应。
    pub footnote_lines: Vec<String>,
}

/// §6.7 的固定说明。放在数据之外，因为它不随候选变化——是「可自查」的提示，
/// 不是「我替你判断」的结论。
pub const KEYCAP_FOOTNOTE: &str = "不足 4 码时末位可能是末笔识别码——取末笔笔画与字型";

/// 键位图窗口。一个进程建一个、反复 `show` / `hide`。
pub struct KeycapWindow {
    hwnd: HWND,
    state: Box<State>,
}

struct State {
    renderer: Renderer,
    theme: Theme,
    visible: bool,
}

impl KeycapWindow {
    pub fn new(theme: Theme) -> Result<Self> {
        crate::register_window_class(
            KEYCAP_CLASS,
            Some(keycap_wndproc),
            &REGISTER_ONCE,
            &CLASS_ATOM,
        )?;
        let hinstance = unsafe { GetModuleHandleW(None)? };

        let state = Box::new(State {
            renderer: Renderer::new()?,
            theme,
            visible: false,
        });

        let ex_style = WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                KEYCAP_CLASS,
                w!("亨利键位图"),
                WS_POPUP | WS_CLIPSIBLINGS,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(HINSTANCE(hinstance.0)),
                Some(&*state as *const State as *const core::ffi::c_void),
            )
        }?;

        Ok(Self { hwnd, state })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn is_visible(&self) -> bool {
        self.state.visible
    }

    /// 量字 + 算布局，但不显示。
    pub fn prepare(&mut self, k: &Keycaps<'_>) -> Result<PreparedKeycaps> {
        let scale = crate::dpi_at(k.anchor) as f32 / 96.0;
        self.prepare_scaled(k, scale)
    }

    /// 指定缩放比例的 [`prepare`](Self::prepare)。给预览器与 DPI 验收用。
    pub fn prepare_scaled(&mut self, k: &Keycaps<'_>, scale: f32) -> Result<PreparedKeycaps> {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let m = KeycapMetrics::new(self.state.theme.font_size).scaled(scale);

        // format 先取再 clone：`keycap_formats` 要 `&mut`，量字要 `&`。
        let (f_key, f_root, f_footnote) = {
            let f = self.state.renderer.text_mut().keycap_formats(
                m.font_size,
                m.root_size,
                m.footnote_size,
            )?;
            (f.key.clone(), f.root.clone(), f.footnote.clone())
        };

        let mut caps = Vec::with_capacity(k.caps.len());
        for c in k.caps {
            let key_width = self.measure(&c.key.to_string(), &f_key)?;
            let mut widths = Vec::with_capacity(c.roots.len());
            for r in &c.roots {
                widths.push(self.measure(&r.to_string(), &f_root)?);
            }
            caps.push(
                KeycapText::new(c.key, key_width, c.roots.clone(), widths)
                    .with_main_roots(c.main_root_count),
            );
        }

        // 第一遍不带说明：拿到「键帽块有多宽」，说明行才能按这个宽度断。
        // 顺序不能反：说明行会撑宽窗口，而键帽块宽度与说明行无关。
        let probe = layout_keycaps(&KeycapInput {
            caps: &caps,
            footnote_widths: &[],
            metrics: m,
        });
        let content_w = (probe.size.w - m.padding_x * 2.0).max(m.min_cap_width);

        let (footnote_lines, footnote_widths) =
            self.wrap_footnote(k.footnote, content_w, &f_footnote)?;

        let layout = layout_keycaps(&KeycapInput {
            caps: &caps,
            footnote_widths: &footnote_widths,
            metrics: m,
        });

        Ok(PreparedKeycaps {
            layout,
            metrics: m,
            dpi_scale: scale,
            footnote_lines,
        })
    }

    /// 画出来并显示在 `at`（屏幕坐标，左上角）。
    pub fn show(&mut self, prepared: &PreparedKeycaps, at: (i32, i32)) -> Result<()> {
        if !self
            .state
            .renderer
            .draw_keycaps(prepared, &self.state.theme)?
        {
            self.state
                .renderer
                .draw_keycaps(prepared, &self.state.theme)?;
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

        let _ = unsafe { ShowWindow(self.hwnd, SW_SHOWNOACTIVATE) };
        self.state.visible = true;
        Ok(())
    }

    pub fn hide(&mut self) {
        if self.state.visible {
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            self.state.visible = false;
        }
    }

    fn measure(&self, text: &str, format: &IDWriteTextFormat) -> Result<f32> {
        self.state.renderer.text().measure_in(text, format)
    }

    /// 把说明断成若干行，每行宽度不超过 `max_w`。
    ///
    /// 逐字量再累加：几何层不做字体度量，也就没法按字符边界换行，只能在这里按
    /// **实测宽度**断。断完每行再整行量一次——逐字累加与整行排版有细微差别
    /// （字距、kerning），用累加值当行宽会让文字紧贴或超出容器一两像素。
    fn wrap_footnote(
        &self,
        text: &str,
        max_w: f32,
        format: &IDWriteTextFormat,
    ) -> Result<(Vec<String>, Vec<f32>)> {
        if text.trim().is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let max_w = max_w.max(1.0);
        let mut lines: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut cur_w = 0.0_f32;

        for ch in text.chars() {
            let w = self.measure(&ch.to_string(), format)?;
            // 避头：标点不放在行首。不处理会把「——」拆成「…识别码—」+「—取末笔…」，
            // 第一行看起来像被截断了。只做避头不做避尾：避尾要回溯已排好的行，
            // 而多挤一个字到上行最多让那行宽出半个字，窗口跟着宽一点，没有副作用。
            if !cur.is_empty() && cur_w + w > max_w && !is_forbidden_leading(ch) {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0.0;
            }
            cur.push(ch);
            cur_w += w;
        }
        if !cur.is_empty() {
            lines.push(cur);
        }

        let mut widths = Vec::with_capacity(lines.len());
        for line in &lines {
            widths.push(self.measure(line, format)?);
        }
        Ok((lines, widths))
    }
}

impl Drop for KeycapWindow {
    fn drop(&mut self) {
        // 与候选窗同一顺序要求：窗口过程持有 `state` 的裸指针，先销毁窗口。
        if !self.hwnd.0.is_null() {
            let _ = unsafe { DestroyWindow(self.hwnd) };
        }
    }
}

const KEYCAP_CLASS: PCWSTR = w!("HeinrichKeycapWindowV1");

/// 不能出现在行首的标点（避头，见 [`KeycapWindow::wrap_footnote`]）。
fn is_forbidden_leading(ch: char) -> bool {
    matches!(
        ch,
        '，' | '。'
            | '、'
            | '；'
            | '：'
            | '？'
            | '！'
            | '—'
            | '…'
            | '·'
            | '）'
            | '」'
            | '』'
            | '】'
            | '》'
            | '〉'
            | '%'
            | ','
            | '.'
            | ';'
            | ':'
            | '?'
            | '!'
    )
}

static REGISTER_ONCE: Once = Once::new();
static CLASS_ATOM: AtomicU16 = AtomicU16::new(0);

unsafe extern "system" fn keycap_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
            LRESULT(1)
        }
        // 点击**穿透**：键位图是只读参考，点它应当等于点在宿主文档上。
        // 少了这条，它会吃掉鼠标事件（而且它是 TOPMOST，连宿主的右键菜单都弹不出来）。
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_NCDESTROY => {
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
