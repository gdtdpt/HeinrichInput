//! emoji 网格选择器（v1.62 / D86）。
//!
//! # 为什么不是 ComboBox
//!
//! D85 的第一版把「从内置表选」做成了原生 ComboBox，两个问题一起暴露：一千多枚
//! emoji 排成一条竖列表只能滚着找，而 emoji 全是一行字形、扫一遍根本分不清谁是谁。
//! 真机反馈原话是「希望有一个类似这样的选择框来选择 emoji」——附的是系统那个按
//! 分类排的网格面板。所以这里做成分类页签 + 网格。
//!
//! 另外那个 ComboBox 本身就是坏的：创建时想追加 `WS_VSCROLL`（0x0020），可
//! ComboBox 的样式表里这一位是 **`CBS_OWNERDRAWVARIABLE`**——控件被切成自绘
//! 模式，条目都在、但没人画，于是下拉框里一片空白。
//!
//! # 为什么是 Direct2D
//!
//! GDI 画 `Segoe UI Emoji` 出来是**单色轮廓**（彩色表 COLR/CPAL 不在 GDI 的
//! 渲染路径上）。彩色只有一个办法：D2D 的 `DrawText` + `ENABLE_COLOR_FONT`，
//! 与候选窗同一个结论（见 `apps/windows/ui/src/render.rs`）。
//!
//! # 结构
//!
//! 几何与命中（[`Geometry`] / [`Catalog`]）是**纯函数**，能单测；窗口那层只负责
//! 「把纯函数的结果画出来 / 把鼠标位置喂给它」。模态循环用 `PeekMessage` +
//! `WaitMessage`，**不发 `PostQuitMessage`**——那会把设置窗口自己的消息循环一起
//! 结束掉。

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Brush, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, HDC, PAINTSTRUCT};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetWindowLongPtrW,
    GetWindowRect, LoadCursorW, PeekMessageW, RegisterClassW, SetForegroundWindow,
    SetWindowLongPtrW, ShowWindow, TranslateMessage, WaitMessage, CREATESTRUCTW, CS_DROPSHADOW,
    GWLP_USERDATA, IDC_ARROW, MSG, PM_REMOVE, SW_SHOW, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT,
    WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

/// 窗口类名。注册一次就够，重复注册会失败（错误可忽略）。
const CLASS_NAME: PCWSTR = w!("HeinrichEmojiPicker");

/// 逻辑尺寸（96 DPI 下的像素）。560×460 在 1366×768 的屏幕上也不越界。
const WIN_W: i32 = 560;
const WIN_H: i32 = 460;

/// 分类名（页签上的字）。顺序即页签顺序。
const CAT_NAMES: [&str; 9] = [
    "表情", "手势", "爱心", "动物", "食物", "活动", "出行", "物品", "符号",
];

/// 「其他」的编号：所有范围都没命中的落这里。它排最后，兜底。
const CAT_OTHER: usize = CAT_NAMES.len();

// ---------------------------------------------------------------- 纯逻辑

/// 一枚 emoji 归哪一类。
///
/// 判据是**码位区间**而不是 CLDR 的正式分组：这里只要「同一类大致挨在一起、
/// 用户能在两三个页签里找到」，不需要与 Unicode 官方分类逐条对齐。区间重叠时
/// 以**先写的为准**，所以「手势」写在「物品」前面——`💪`（U+1F4AA）落在
/// 物品那段区间里，但它显然是手势。
fn cat_of(cp: u32) -> usize {
    match cp {
        // 表情与情绪：笑脸、猫脸、猴脸那一整片
        0x1F600..=0x1F64F
        | 0x1F910..=0x1F917
        | 0x1F920..=0x1F92F
        | 0x1F970..=0x1F976
        | 0x1F97A
        | 0x1F47F
        | 0x1F480 => 0,
        // 手势与身体
        0x1F440..=0x1F450
        | 0x1F590..=0x1F596
        | 0x1F918..=0x1F91F
        | 0x1F930..=0x1F93A
        | 0x1F9B5..=0x1F9BF
        | 0x261D
        | 0x26F9
        | 0x270A..=0x270D
        | 0x1F4AA => 1,
        // 心与情感符号
        0x2764
        | 0x1F493..=0x1F49F
        | 0x1F5A4
        | 0x1F90D..=0x1F90F
        | 0x1F9E1
        | 0x263A
        | 0x1F48B
        | 0x1F48C => 2,
        // 动物与自然
        0x1F330..=0x1F343 | 0x1F400..=0x1F43F | 0x1F980..=0x1F9AE | 0x1F344 => 3,
        // 食物与饮品
        0x1F345..=0x1F37F | 0x1F950..=0x1F96F | 0x1F32D..=0x1F32F | 0x2615 => 4,
        // 活动、庆祝、运动、乐器
        0x1F380..=0x1F3FA | 0x1F947..=0x1F94C | 0x26BD | 0x26BE => 5,
        // 出行与地点
        0x1F680..=0x1F6FF | 0x1F30D..=0x1F32C | 0x1F5FA..=0x1F5FF | 0x2708 => 6,
        // 物品与工具
        0x1F4A0..=0x1F4FF
        | 0x1F5B1..=0x1F5D3
        | 0x1F9F0..=0x1F9FF
        | 0x1FA70..=0x1FAFF
        | 0x1F9E0..=0x1F9EF => 7,
        // 符号与标记
        0x2190..=0x21FF
        | 0x2600..=0x26FF
        | 0x2700..=0x27BF
        | 0x2B00..=0x2BFF
        | 0x1F170..=0x1F19A
        | 0x1F500..=0x1F5FF
        | 0x1F1E6..=0x1F1FF
        | 0x00A9
        | 0x00AE
        | 0x2122
        | 0x3030 => 8,
        _ => CAT_OTHER,
    }
}

/// 按分类分好的候选。空分类**不出现**在页签上（点了也是空网格，没意义）。
pub struct Catalog {
    /// 每项：(分类名, 该类可见的 emoji)。顺序 = 页签顺序。
    cats: Vec<(&'static str, Vec<String>)>,
}

impl Catalog {
    /// `items` 是「本机能画出来的 emoji」全集（调用方已按字形过滤）。
    pub fn new(items: &[String]) -> Self {
        let mut buckets: Vec<Vec<String>> = vec![Vec::new(); CAT_NAMES.len() + 1];
        for e in items {
            let Some(cp) = e.chars().next().map(|c| c as u32) else {
                continue;
            };
            buckets[cat_of(cp).min(CAT_OTHER)].push(e.clone());
        }
        let mut cats = Vec::with_capacity(CAT_NAMES.len() + 1);
        for (i, mut v) in buckets.into_iter().enumerate() {
            if v.is_empty() {
                continue;
            }
            v.sort();
            v.dedup();
            let name = if i == CAT_OTHER {
                "其他"
            } else {
                CAT_NAMES[i]
            };
            cats.push((name, v));
        }
        Self { cats }
    }

    pub fn tabs(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.cats.iter().map(|(n, _)| *n)
    }

    pub fn len(&self) -> usize {
        self.cats.len()
    }

    pub fn items(&self, cat: usize) -> &[String] {
        self.cats.get(cat).map(|(_, v)| v.as_slice()).unwrap_or(&[])
    }
}

/// 网格几何（**物理像素**）。全部由 [`Geometry::new`] 一次算好，命中与绘制共用。
pub struct Geometry {
    pub w: i32,
    pub h: i32,
    /// DPI 缩放系数。
    ///
    /// **字体、圆角、描边这些不参与命中的量也必须乘它**：几何是物理像素，而 DWrite
    /// 的字号在 `dpi = 96` 的 DC 渲染目标上就是「1 DIP = 1 物理像素」。只缩放几何、
    /// 不缩放字号，结果就是 200% DPI 下格子 92px、emoji 仍只有 26px——看着像一颗芝麻
    /// 撒在白板上（真机截图反馈「文字也太小了吧」就是这个）。
    pub scale: f32,
    /// 网格单元边长。
    pub cell: i32,
    pub cols: i32,
    pub rows: i32,
    /// 网格左上角。
    pub grid_x: i32,
    pub grid_y: i32,
    /// 分类页签：起点 x、每个宽、步长。
    tab_x0: i32,
    tab_w: i32,
    tab_step: i32,
    tab_y: i32,
    tab_h: i32,
    /// 关闭按钮。
    pub close: RECT,
    /// 字号（物理像素）：标题 / 页签 / 格子里的 emoji / 底部提示。
    pub font_title: f32,
    pub font_tab: f32,
    pub font_emoji: f32,
    pub font_hint: f32,
}

/// 逻辑 dp → 物理像素。
fn sp(v: i32, scale: f32) -> i32 {
    (v as f32 * scale).round() as i32
}

impl Geometry {
    pub fn new(w: i32, h: i32, scale: f32) -> Self {
        let s = |v: i32| sp(v, scale);
        let (w, h) = (s(w), s(h));
        let cell = s(46);
        let tab_h = s(34);
        let tab_y = s(44);
        let title_h = s(88);
        let footer_h = s(30);
        let inset = s(14);
        let cols = ((w - inset * 2) / cell).max(1);
        let rows = ((h - title_h - footer_h) / cell).max(1);
        let grid_w = cols * cell;
        let grid_x = (w - grid_w) / 2;
        let tab_step = s(53);
        let tab_w = s(51);
        Self {
            w,
            h,
            scale,
            cell,
            cols,
            rows,
            grid_x,
            grid_y: title_h,
            tab_x0: s(12),
            tab_w,
            tab_step,
            tab_y,
            tab_h,
            close: RECT {
                left: w - s(38),
                top: s(8),
                right: w - s(12),
                bottom: s(34),
            },
            // emoji 直接按格子边长取比例：字号写死 26 的话，换一档 DPI 或改一次
            // 格子尺寸，两者就重新脱节。
            font_title: 16.0 * scale,
            font_tab: 14.0 * scale,
            font_emoji: cell as f32 * 0.68,
            font_hint: 13.0 * scale,
        }
    }

    /// 标题文字框（左内衬 20、顶 6、高 32——装得下 16dp 的粗体，不会切到字头）。
    pub fn title_rect(&self) -> RECT {
        RECT {
            left: sp(20, self.scale),
            top: sp(6, self.scale),
            right: self.w - sp(64, self.scale),
            bottom: sp(38, self.scale),
        }
    }

    /// 底部提示文字框。
    pub fn footer_rect(&self) -> RECT {
        RECT {
            left: sp(20, self.scale),
            top: self.h - sp(30, self.scale),
            right: self.w - sp(40, self.scale),
            bottom: self.h - sp(8, self.scale),
        }
    }

    /// 第 `i` 个分类页签的矩形。
    pub fn tab_rect(&self, i: usize) -> RECT {
        let left = self.tab_x0 + self.tab_step * i as i32;
        RECT {
            left,
            top: self.tab_y,
            right: left + self.tab_w,
            bottom: self.tab_y + self.tab_h,
        }
    }

    /// 滚到最底时的「首行号」。
    pub fn max_scroll(&self, count: usize) -> i32 {
        let total_rows = (count as i32 + self.cols - 1) / self.cols;
        (total_rows - self.rows).max(0)
    }

    /// 第 `idx` 枚格子的矩形（`scroll` 是首行行号）。
    pub fn cell_rect(&self, idx: usize, scroll: i32) -> RECT {
        let col = idx as i32 % self.cols;
        let row = idx as i32 / self.cols - scroll;
        let left = self.grid_x + col * self.cell;
        let top = self.grid_y + row * self.cell;
        RECT {
            left,
            top,
            right: left + self.cell,
            bottom: top + self.cell,
        }
    }

    /// 点在哪种一格？返回全集的格子序号（`scroll` 偏移已经算进去）。
    ///
    /// 超出网格、超出条目总数、或点在那半格留白上都返回 `None`。
    pub fn index_at(&self, x: i32, y: i32, count: usize, scroll: i32) -> Option<usize> {
        if x < self.grid_x || y < self.grid_y {
            return None;
        }
        let col = (x - self.grid_x) / self.cell;
        let row = (y - self.grid_y) / self.cell;
        if col >= self.cols || row >= self.rows {
            return None;
        }
        let idx = (scroll + row) * self.cols + col;
        if idx < 0 || idx as usize >= count {
            return None;
        }
        Some(idx as usize)
    }

    /// 命中的分类页签序号。
    pub fn tab_at(&self, x: i32, y: i32, n: usize) -> Option<usize> {
        if y < self.tab_y || y >= self.tab_y + self.tab_h {
            return None;
        }
        if x < self.tab_x0 {
            return None;
        }
        let i = ((x - self.tab_x0) / self.tab_step) as usize;
        if i < n && x < self.tab_rect(i).right {
            Some(i)
        } else {
            None
        }
    }

    pub fn hit_close(&self, x: i32, y: i32) -> bool {
        x >= self.close.left && x < self.close.right && y >= self.close.top && y < self.close.bottom
    }
}

// ---------------------------------------------------------------- 窗口层

/// 弹一次选择器。返回选中的 emoji；取消（Esc / 点 × / 点窗口外）返回 `None`。
pub fn pick(
    parent: HWND,
    dpi: f32,
    all: &[String],
    support: &heinrich_font::EmojiSupport,
    initial: Option<&str>,
) -> Option<String> {
    // 画不出来的不给选：选了也是候选窗里一枚方块（与 server 装载期同一判据）
    let usable: Vec<String> = all
        .iter()
        .filter(|e| support.supports(e))
        .cloned()
        .collect();
    let catalog = Catalog::new(&usable);
    if catalog.len() == 0 {
        return None;
    }
    let start_cat = initial
        .map(|e| cat_of(e.chars().next().map(|c| c as u32).unwrap_or(0)))
        .and_then(|want| {
            // `cat_of` 的编号与「去空后的页签序号」不是一回事，按名字反查
            let name = if want == CAT_OTHER {
                "其他"
            } else {
                CAT_NAMES[want]
            };
            catalog.tabs().position(|t| t == name)
        })
        .unwrap_or(0);

    let mut state = Box::new(State {
        geo: Geometry::new(WIN_W, WIN_H, dpi),
        catalog,
        cat: start_cat,
        scroll: 0,
        hover: None,
        result: None,
        done: false,
        device: None,
    });
    let sp: *mut State = &mut *state;

    unsafe {
        // 失败也无所谓：设置窗口主线程多半已经 STA 了。
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        register_class();
    }

    let (px, py) = parent_center(parent, unsafe { (*sp).geo.w }, unsafe { (*sp).geo.h });
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            CLASS_NAME,
            w!("选择 emoji"),
            WS_POPUP,
            px,
            py,
            (*sp).geo.w,
            (*sp).geo.h,
            Some(parent),
            None,
            None,
            Some(sp as *const core::ffi::c_void),
        )
    };
    let Ok(hwnd) = hwnd else {
        return None;
    };

    // 模态：父窗口禁掉，免得点到底下那页里去
    unsafe {
        let _ = EnableWindow(parent, false);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    let mut msg = MSG::default();
    loop {
        if unsafe { (*sp).done } {
            break;
        }
        let has = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() };
        if has {
            if msg.message == 0x0012 {
                // WM_QUIT：理论上不会到这儿（我们从不下发），真遇到就收工
                break;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        } else {
            unsafe {
                let _ = WaitMessage();
            }
        }
    }
    unsafe {
        let _ = DestroyWindow(hwnd);
        let _ = EnableWindow(parent, true);
        let _ = SetForegroundWindow(parent);
    }
    let out = unsafe { (*sp).result.clone() };
    drop(state);
    out
}

/// 让弹窗居中在父窗口上；算不出来就居中到主屏。
fn parent_center(parent: HWND, w: i32, h: i32) -> (i32, i32) {
    let mut pr = RECT::default();
    let ok = unsafe { GetWindowRect(parent, &mut pr) }.is_ok();
    if ok {
        let x = pr.left + ((pr.right - pr.left) - w) / 2;
        let y = pr.top + ((pr.bottom - pr.top) - h) / 2;
        // 别越过工作区上沿（任务栏 + 标题栏）
        return (x.max(0), y.max(0));
    }
    (200, 120)
}

fn register_class() {
    unsafe {
        let wc = WNDCLASSW {
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(wndproc),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // 已注册会失败，忽略
        let _ = RegisterClassW(&wc);
    }
}

struct Device {
    factory: ID2D1Factory,
    title: IDWriteTextFormat,
    tab: IDWriteTextFormat,
    emoji: IDWriteTextFormat,
    hint: IDWriteTextFormat,
}

struct State {
    geo: Geometry,
    catalog: Catalog,
    cat: usize,
    scroll: i32,
    hover: Option<usize>,
    result: Option<String>,
    done: bool,
    device: Option<Device>,
}

impl State {
    fn items(&self) -> &[String] {
        self.catalog.items(self.cat)
    }

    fn clamp_scroll(&mut self) {
        let max = self.geo.max_scroll(self.items().len());
        self.scroll = self.scroll.clamp(0, max);
    }

    fn close(&mut self, value: Option<String>) {
        self.result = value;
        self.done = true;
    }
}

impl Device {
    fn new(geo: &Geometry) -> windows::core::Result<Self> {
        let factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        Ok(Self {
            title: make_format(&dwrite, w!("Microsoft YaHei UI"), geo.font_title, false)?,
            tab: make_format(&dwrite, w!("Microsoft YaHei UI"), geo.font_tab, true)?,
            emoji: make_format(&dwrite, w!("Segoe UI Emoji"), geo.font_emoji, true)?,
            hint: make_format(&dwrite, w!("Microsoft YaHei UI"), geo.font_hint, false)?,
            factory,
        })
    }
}

fn make_format(
    f: &IDWriteFactory,
    family: PCWSTR,
    size: f32,
    center: bool,
) -> windows::core::Result<IDWriteTextFormat> {
    let fmt = unsafe {
        f.CreateTextFormat(
            family,
            None::<&IDWriteFontCollection>,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("zh-cn"),
        )?
    };
    unsafe {
        fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        fmt.SetTextAlignment(if center {
            DWRITE_TEXT_ALIGNMENT_CENTER
        } else {
            DWRITE_TEXT_ALIGNMENT_LEADING
        })?;
    }
    Ok(fmt)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = lp.0 as *const CREATESTRUCTW;
            let st = (*cs).lpCreateParams as *mut State;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, st as isize);
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        let Some(st) = st.as_mut() else {
            return DefWindowProcW(hwnd, msg, wp, lp);
        };
        match msg {
            WM_PAINT => {
                paint(hwnd, st);
                LRESULT(0)
            }
            // 背景全由 D2D 铺，别让系统先刷一遍白（会闪）
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEMOVE => {
                let (x, y) = lp_xy(lp);
                let idx = st.geo.index_at(x, y, st.items().len(), st.scroll);
                if idx != st.hover {
                    st.hover = idx;
                    let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = lp_xy(lp);
                if st.geo.hit_close(x, y) {
                    st.close(None);
                    return LRESULT(0);
                }
                if let Some(tab) = st.geo.tab_at(x, y, st.catalog.len()) {
                    if tab != st.cat {
                        st.cat = tab;
                        st.scroll = 0;
                        st.hover = None;
                        let _ =
                            windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
                    }
                    return LRESULT(0);
                }
                if let Some(i) = st.geo.index_at(x, y, st.items().len(), st.scroll) {
                    let picked = st.items().get(i).cloned();
                    if let Some(e) = picked {
                        st.close(Some(e));
                    }
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
                let step = if delta > 0 { -1 } else { 1 };
                st.scroll += step;
                st.clamp_scroll();
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_KEYDOWN if wp.0 == 0x1B => {
                st.close(None);
                LRESULT(0)
            }
            WM_CLOSE => {
                st.close(None);
                LRESULT(0)
            }
            WM_DESTROY => {
                // 注意：**不** PostQuitMessage——主程序的循环在别的窗口上
                LRESULT(0)
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn lp_xy(lp: LPARAM) -> (i32, i32) {
    let x = (lp.0 & 0xFFFF) as i16 as i32;
    let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
    (x, y)
}

fn paint(hwnd: HWND, st: &mut State) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    if hdc.0.is_null() {
        return;
    }
    if let Err(e) = draw(hdc, st) {
        // 画不出来就留空窗口；把错误告诉用户会打断「我在挑 emoji」这件事
        let _ = e;
    }
    unsafe {
        let _ = EndPaint(hwnd, &ps);
    }
}

fn draw(hdc: HDC, state: &mut State) -> windows::core::Result<()> {
    // 设备对象懒建（一个弹窗活一次）；建完就把 `&mut` 收回去，后面全是只读
    if state.device.is_none() {
        state.device = Device::new(&state.geo).ok();
    }
    let st: &State = state;
    let geo_w = st.geo.w;
    let geo_h = st.geo.h;
    let cat_i = st.cat;
    let scroll = st.scroll;
    let hover = st.hover;
    let tabs: Vec<&'static str> = st.catalog.tabs().collect();
    let items: Vec<String> = st.items().to_vec();
    let Some(dev) = st.device.as_ref() else {
        return Ok(());
    };

    // DC 渲染目标：绑定到窗口 DC，DIP 比例固定 96（几何已是物理像素）
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_IGNORE,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    };
    let target: ID2D1DCRenderTarget = unsafe { dev.factory.CreateDCRenderTarget(&props)? };
    let full = RECT {
        left: 0,
        top: 0,
        right: geo_w,
        bottom: geo_h,
    };
    unsafe { target.BindDC(hdc, &full)? };

    unsafe {
        target.BeginDraw();
        target.Clear(Some(&d2d_color(0xFF, 0xFF, 0xFF, 0xFF)));

        let ink = dev.title_brush(&target, 0x22, 0x22, 0x22)?;
        let muted = dev.title_brush(&target, 0x77, 0x77, 0x77)?;
        let accent = dev.title_brush(&target, 0x00, 0x67, 0xC0)?;
        let hover_bg = dev.title_brush(&target, 0xEE, 0xF1, 0xF4)?;
        let border = dev.title_brush(&target, 0xD8, 0xDC, 0xE0)?;

        // 标题 + 关闭
        let t = st.geo.title_rect();
        text(
            &target,
            &dev.title,
            "选择 emoji",
            (t.left, t.top, t.right - t.left, t.bottom - t.top),
            &ink,
            false,
        );
        text(
            &target,
            &dev.tab,
            "×",
            (
                st.geo.close.left,
                st.geo.close.top,
                st.geo.close.right - st.geo.close.left,
                st.geo.close.bottom - st.geo.close.top,
            ),
            &muted,
            false,
        );

        // 分类页签
        let sx = st.geo.scale;
        let px = |v: i32| sp(v, sx);
        for (i, name) in tabs.iter().enumerate() {
            let r = st.geo.tab_rect(i);
            let color = if i == cat_i { &accent } else { &muted };
            text(
                &target,
                &dev.tab,
                name,
                (r.left, r.top, r.right - r.left, r.bottom - r.top),
                color,
                false,
            );
            if i == cat_i {
                // 选中下划线：与文字同宽（居中时左右各收 8dp），粗 2dp
                let y = r.bottom - px(2);
                target.FillRectangle(
                    &d2d_rect(r.left + px(8), y, r.right - px(8), y + px(2)),
                    &accent as &ID2D1Brush,
                );
            }
        }

        // 网格
        for (i, e) in items.iter().enumerate() {
            let row = i as i32 / st.geo.cols - scroll;
            if row < 0 || row >= st.geo.rows {
                continue;
            }
            let r = st.geo.cell_rect(i, scroll);
            if hover == Some(i) {
                target.FillRoundedRectangle(
                    &windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                        rect: d2d_rect(
                            r.left + px(2),
                            r.top + px(2),
                            r.right - px(2),
                            r.bottom - px(2),
                        ),
                        radiusX: 6.0 * sx,
                        radiusY: 6.0 * sx,
                    },
                    &hover_bg as &ID2D1Brush,
                );
            }
            text(
                &target,
                &dev.emoji,
                e,
                (r.left, r.top, r.right - r.left, r.bottom - r.top),
                &ink,
                true,
            );
        }

        // 空分类（理论上不会出现，Catalog 已经滤掉空的）
        if items.is_empty() {
            text(
                &target,
                &dev.hint,
                "这类没有可用的 emoji",
                (px(20), geo_h / 2 - px(14), geo_w - px(40), px(28)),
                &muted,
                false,
            );
        }

        // 滚动指示条
        let max = st.geo.max_scroll(items.len());
        if max > 0 {
            let track_top = st.geo.grid_y;
            let track_h = st.geo.rows * st.geo.cell;
            let total_rows = (items.len() as i32 + st.geo.cols - 1) / st.geo.cols;
            let thumb_h = (track_h * st.geo.rows / total_rows.max(1)).max(px(24));
            let y = track_top + (track_h - thumb_h) * scroll / max;
            let x = geo_w - px(12);
            target.FillRoundedRectangle(
                &windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                    rect: d2d_rect(x, y, x + px(6), y + thumb_h),
                    radiusX: 3.0 * sx,
                    radiusY: 3.0 * sx,
                },
                &border as &ID2D1Brush,
            );
        }

        // 底部提示
        let f = st.geo.footer_rect();
        text(
            &target,
            &dev.hint,
            "点击一枚填入「新 emoji」框；Esc 取消",
            (f.left, f.top, f.right - f.left, f.bottom - f.top),
            &muted,
            false,
        );

        // 最后画外框：压在内容上，绝不会被格子盖住
        target.DrawRectangle(&d2d_rect(0, 0, geo_w, geo_h), &border, sx, None);

        target.EndDraw(None, None)?;
    }
    Ok(())
}

impl Device {
    fn title_brush(
        &self,
        target: &ID2D1DCRenderTarget,
        r: u8,
        g: u8,
        b: u8,
    ) -> windows::core::Result<ID2D1SolidColorBrush> {
        unsafe { target.CreateSolidColorBrush(&d2d_color(r, g, b, 0xFF), None) }
    }
}

/// 画一行字。`rect` 是物理像素，`(x, y, w, h)` 的简写形式，省得每处都写四个字段。
fn text(
    target: &ID2D1DCRenderTarget,
    format: &IDWriteTextFormat,
    s: &str,
    rect: (i32, i32, i32, i32),
    brush: &ID2D1SolidColorBrush,
    color_font: bool,
) {
    let layout = d2d_rect(rect.0, rect.1, rect.0 + rect.2, rect.1 + rect.3);
    let utf16 = windows::core::HSTRING::from(s);
    let options = if color_font {
        D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT
    } else {
        D2D1_DRAW_TEXT_OPTIONS_NONE
    };
    unsafe {
        target.DrawText(
            &utf16,
            format,
            &layout,
            brush,
            options,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

fn d2d_color(r: u8, g: u8, b: u8, a: u8) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    }
}

fn d2d_rect(l: i32, t: i32, r: i32, b: i32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: l as f32,
        top: t as f32,
        right: r as f32,
        bottom: b as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn 分类按码位落位() {
        assert_eq!(cat_of('😀' as u32), 0, "笑脸 → 表情");
        assert_eq!(cat_of('👍' as u32), 1, "竖大拇指 → 手势");
        assert_eq!(cat_of('❤' as u32), 2, "心 → 爱心");
        assert_eq!(cat_of('🐶' as u32), 3, "狗 → 动物");
        assert_eq!(cat_of('🍔' as u32), 4, "汉堡 → 食物");
        assert_eq!(cat_of('🎉' as u32), 5, "礼花 → 活动");
        assert_eq!(cat_of('🚗' as u32), 6, "汽车 → 出行");
        assert_eq!(cat_of('💡' as u32), 7, "灯泡 → 物品");
        assert_eq!(cat_of('✅' as u32), 8, "对勾 → 符号");
        assert_eq!(cat_of(0x10FF), CAT_OTHER, "没人认领的进兜底");
    }

    /// `💪` 落在物品那段区间（1F4A0–1F4FF）里，但它先被手势命中——顺序即优先级。
    #[test]
    fn 重叠区间以先写的为准() {
        assert_eq!(cat_of('💪' as u32), 1, "肌肉是手势，不是物品");
        assert_eq!(cat_of('💩' as u32), 7, "便便留在物品类");
    }

    #[test]
    fn 分类表跳过空类并按码位排序() {
        let cat = Catalog::new(&[e("🍔"), e("😀"), e("🐶")]);
        let names: Vec<&str> = cat.tabs().collect();
        assert_eq!(names, vec!["表情", "动物", "食物"], "空分类不该占页签");
        assert_eq!(cat.items(0), &[e("😀")]);
        assert_eq!(cat.items(1), &[e("🐶")]);
    }

    #[test]
    fn 命中格子要算上滚动偏移() {
        // 40 个格子宽的窗口：cell=46 → cols 由窗口宽度算出来，这里直接用构造结果
        let g = Geometry::new(WIN_W, WIN_H, 1.0);
        assert!(g.cols >= 10, "560 宽至少放得下 10 列");
        let (x, y) = (g.grid_x + 3, g.grid_y + 3);
        assert_eq!(g.index_at(x, y, 100, 0), Some(0));
        assert_eq!(g.index_at(x, y, 100, 2), Some(2 * g.cols as usize));

        // 网格左边外、上边外、超出列数、超出条目总数
        assert_eq!(g.index_at(g.grid_x - 1, y, 100, 0), None);
        assert_eq!(g.index_at(x, g.grid_y - 1, 100, 0), None);
        let far_right = g.grid_x + g.cols * g.cell + 2;
        assert_eq!(g.index_at(far_right, y, 100, 0), None);
        assert_eq!(g.index_at(x, y, 1, 0), Some(0));
        let second = (g.grid_x + g.cell + 3, g.grid_y + 3);
        assert_eq!(
            g.index_at(second.0, second.1, 1, 0),
            None,
            "只有一条时第 2 格是空的"
        );
    }

    #[test]
    fn 滚动上限按整行算() {
        let g = Geometry::new(WIN_W, WIN_H, 1.0);
        let per = (g.cols * g.rows) as usize;
        assert_eq!(g.max_scroll(per), 0, "一屏装得下就不用滚");
        assert_eq!(g.max_scroll(per + 1), 1, "多一行 → 滚 1 行");
        assert_eq!(g.max_scroll(per * 2), g.rows, "两屏 → 正好往下滚一屏");
    }

    #[test]
    fn 页签命中不越界() {
        let g = Geometry::new(WIN_W, WIN_H, 1.0);
        let r = g.tab_rect(2);
        assert_eq!(g.tab_at(r.left + 2, r.top + 2, 9), Some(2));
        assert_eq!(
            g.tab_at(r.left + 2, r.top + 2, 2),
            None,
            "只有 2 个页签时第 3 个不算"
        );
        assert_eq!(g.tab_at(r.left + 2, g.tab_y - 2, 9), None, "页签行之外");
    }

    #[test]
    fn 关窗按钮命中() {
        let g = Geometry::new(WIN_W, WIN_H, 1.0);
        assert!(g.hit_close(g.close.left + 1, g.close.top + 1));
        assert!(!g.hit_close(g.close.left - 2, g.close.top + 1));
    }

    /// DPI 缩放后所有几何都得跟着放大，否则 150% 下格子会挤在一起。
    #[test]
    fn 几何随dpi放大() {
        let a = Geometry::new(WIN_W, WIN_H, 1.0);
        let b = Geometry::new(WIN_W, WIN_H, 1.5);
        assert_eq!(b.w, (WIN_W as f32 * 1.5) as i32);
        assert_eq!(b.cell, 69);
        assert_eq!((b.cols, b.rows), (a.cols, a.rows), "行列数不该随 DPI 变");
    }

    /// **字号也得随 DPI 放大**。几何是物理像素，而 DWrite 的字号在 dpi=96 的渲染目标上
    /// 就是物理像素——只放大几何不放大字号，200% 下就是「92px 的格子装 26px 的字」
    /// （真机反馈「文字也太小了吧」）。
    #[test]
    fn 字号随dpi放大且填满格子() {
        let a = Geometry::new(WIN_W, WIN_H, 1.0);
        let b = Geometry::new(WIN_W, WIN_H, 2.0);
        assert!(
            (b.font_title / a.font_title - 2.0).abs() < 0.02,
            "标题字号应随 DPI 翻倍：{} → {}",
            a.font_title,
            b.font_title
        );
        // emoji 与格子同比例：占格约 2/3，既不留大片白边，也不会顶到线
        for g in [&a, &b] {
            let ratio = g.font_emoji / g.cell as f32;
            assert!((0.6..=0.75).contains(&ratio), "emoji 占格比例 {ratio}");
        }
    }
}
