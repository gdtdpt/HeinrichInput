//! 设置窗口：主窗口 + 三页（通用 / 词组 / 学习数据），Win11 设置卡片风。
//!
//! 词组页写 `%APPDATA%\Heinrich\words.tsv`，通用页写同目录的 `config.toml`，
//! 学习数据页只读（外加清零）`%LOCALAPPDATA%\Heinrich\learn.db`。三者都**不经过
//! 任何进程间通信**：server 每秒 stat 一次前两个、每次查询直接读第三个（D65），
//! 所以这个 exe 可以随时开、随时关，改完不用重启任何东西。
//!
//! **为什么编辑框仍是原生控件**：编辑框要能**用亨利自己打中文**（单字都在词库里），
//! 自绘就得连光标、选区、IME 组合串一起实现。自绘的是壳（卡片、页签、按钮、
//! 三段选择器、输入框描边），输入仍交给系统。
//!
//! **页签没有控件**：`SysTabControl32` 一拿到焦点就把自己抬到 z 序最上层，而它的
//! 矩形正好覆盖整个内容区——一旦抬上来就把所有按钮和编辑框盖住，鼠标点哪儿都被
//! 它吃掉（F44）。上一版靠子类化「等它抬完再压回去」；这一版干脆不建这个窗口，
//! 页签、指示条、hover 全部画在主窗口自己的 `WM_PAINT` 里，命中测试就是几个矩形，
//! 没有可浮上来的东西，F44 这类病根连发生的土壤都没了。
//!
//! 多页的代价：三页的控件都是主窗口的子窗口，靠显隐切换。所以每条 WM_COMMAND /
//! WM_NOTIFY 都要看清是谁发的——两个 ListView 同时存在，只按消息类型分派会把
//! 学习数据的行当成词组去改。
//!
//! 几何的唯一真相是 [`App::geo`]：`layout()` 算一遍存下来，`paint()` 只读不算。
//! 画卡片和摆控件用同一组矩形，「卡片里有一行、控件却在行外」这种错位从结构上
//! 就不可能出现。

use std::path::PathBuf;

use heinrich_core::RankingConfig;
use heinrich_scheme::user::UserWord;
use heinrich_scheme::HintMode;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
    FillRect, GetDC, GetTextExtentPoint32W, InvalidateRect, LineTo, MoveToEx, ReleaseDC, RoundRect,
    ScreenToClient, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, HBRUSH, HDC, HFONT,
    PAINTSTRUCT, PS_SOLID, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OPENFILENAMEW};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Controls::{
    ShowScrollBar, DRAWITEMSTRUCT, LVCFMT_CENTER, LVCFMT_LEFT, LVCFMT_RIGHT, LVCOLUMNW,
    LVIR_BOUNDS, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_DELETEITEM,
    LVM_ENSUREVISIBLE, LVM_GETITEMCOUNT, LVM_GETITEMRECT, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW,
    LVM_INSERTITEMW, LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE,
    LVM_SETITEMW, LVNI_SELECTED, LVN_ITEMCHANGED,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{
    DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetClientRect,
    GetMessageW, GetParent, GetSystemMetrics, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, LoadCursorW, MessageBoxW,
    MoveWindow, PostQuitMessage, RealChildWindowFromPoint, RegisterClassW, SendMessageW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW, ShowWindow, ShowWindowAsync,
    TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_ID, GWLP_USERDATA,
    GW_CHILD, GW_HWNDNEXT, IDC_ARROW, IDYES, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_YESNO,
    MSG, SB_HORZ, SM_CXVSCROLL, SW_HIDE, SW_RESTORE, SW_SHOW, SW_SHOWNOACTIVATE, SW_SHOWNORMAL,
    WINDOW_STYLE, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORSTATIC, WM_DESTROY,
    WM_DRAWITEM, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCCREATE,
    WM_NCDESTROY, WM_NOTIFY, WM_PAINT, WM_SETFONT, WM_SIZE, WNDCLASSW, WNDPROC, WS_CHILD,
    WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
};

use crate::lex::{find_data_dir, Lexicons};
use crate::model::{Model, Row};
use crate::theme::{self, Colors, Fonts};

// 窗口类名不在这里写死：见 `create_window` 里对 `heinrich_tip_id::SETTINGS_CLASS` 的说明。

const ID_LIST: usize = 101;
const ID_E_WORD: usize = 110;
const ID_E_PINYIN: usize = 111;
const ID_E_CODE: usize = 112;
const ID_E_WEIGHT: usize = 113;
const ID_B_CLEAR: usize = 130;
const ID_B_SAVE: usize = 131;
const ID_B_DEL: usize = 132;
const ID_B_IMPORT: usize = 133;
const ID_B_OPEN: usize = 134;
const ID_B_UNDO: usize = 135;
const ID_NOTE: usize = 140;
const ID_STATUS: usize = 141;

// 通用页
const ID_E_GAIN: usize = 204;
const ID_E_HALF: usize = 207;
const ID_B_APPLY: usize = 209;
const ID_B_DEFAULT: usize = 210;
const ID_B_OPEN_CFG: usize = 213;

// 学习数据页
const ID_E_SEARCH: usize = 301;
const ID_B_SEARCH: usize = 302;
const ID_LEARN_LIST: usize = 303;
const ID_B_FORGET: usize = 304;

/// 页序号。**与页签顺序、`pages` 数组的下标三者必须一致**。
const PAGE_GENERAL: usize = 0;
const PAGE_WORDS: usize = 1;
const PAGE_LEARN: usize = 2;
const PAGE_COUNT: usize = 3;
const PAGE_TITLES: [&str; PAGE_COUNT] = ["通用", "词组", "学习数据"];
/// 码提示的三段选择器文案与取值，顺序 = `HintMode` 的映射顺序。
const HINT_SEGMENTS: [&str; 3] = ["总是", "首次", "关闭"];

// ListView 的几条消息/样式常量。`windows` crate 把它们定义成裸 u32，这里直接引用
// 会让整段代码被 `as` 淹没，所以只挑用得到的几个，写清来历。
const LVS_REPORT: u32 = 0x0001;
const LVS_SINGLESEL: u32 = 0x0004;
const LVS_SHOWSELALWAYS: u32 = 0x0008;
const LVS_EX_FULLROWSELECT: usize = 0x0000_0020;
const LVIF_TEXT: u32 = 0x0001;
const LVIF_PARAM: u32 = 0x0004;
const LVCF_WIDTH: u32 = 0x0002;
const LVCF_TEXT: u32 = 0x0004;
/// `LVCF_FMT`：列的对齐方式。不加这一位的列宽/文字照常生效，**但对齐默认左**，
/// 于是「上屏次数」「学习分」这种数字列会顶着列头左端排，一列数字参差不齐。
const LVCF_FMT: u32 = 0x0001;
/// ListView 背景与文字色（`LVM_FIRST = 0x1000`）。
const LVM_SETBKCOLOR: u32 = 0x1001;
const LVM_SETTEXTCOLOR: u32 = 0x1024;
const LVM_SETTEXTBKCOLOR: u32 = 0x1026;
/// `WM_COMMAND` 的通知码。EN_* 在 winuser.h 是 0x01xx 段。
const EN_CHANGE: u32 = 0x0300;
const EN_SETFOCUS: u32 = 0x0100;
const EN_KILLFOCUS: u32 = 0x0200;
/// `BS_OWNERDRAW`：按钮自己画（见 `draw_button`）。
const BS_OWNERDRAW: u32 = 0x000B;
/// `WM_DRAWITEM` 的 itemState 位。
const ODS_SELECTED: u32 = 0x0001;
const ODS_DISABLED: u32 = 0x0004;
const ODS_HOTLIGHT: u32 = 0x0040;
/// `DrawTextW` 的排版位：单行 + 垂直居中 + 超宽省略号 + 不处理 `&` 前缀
/// （路径里有 `&` 的概率不为零，把它当普通字符）。
const DT_SINGLELINE: u32 = 0x0020;
const DT_VCENTER: u32 = 0x0004;
const DT_LEFT: u32 = 0x0000;
const DT_CENTER: u32 = 0x0001;
const DT_RIGHT: u32 = 0x0002;
const DT_END_ELLIPSIS: u32 = 0x8000;
/// 中间省略（路径用）：保留头尾，中间换成「…」
const DT_PATH_ELLIPSIS: u32 = 0x4000;
const DT_NOPREFIX: u32 = 0x0800;

/// 窗口内容的最小尺寸（96 DPI 下的逻辑像素），窄于这个就压缩不动了。
const MIN_W: i32 = 720;
const MIN_H: i32 = 460;

/// 页面的几何快照：`layout()` 算、`paint()` 读的唯一真相。
///
/// 只存「画的时候要原样用」的矩形。控件自己的矩形不在这里——控件用 `MoveWindow`
/// 摆好之后系统自己知道，重复存一份只会早晚失同步。
#[derive(Default)]
struct Geo {
    /// 页签矩形（横排）。命中测试与绘制共用。
    tabs: [RECT; PAGE_COUNT],
    /// 通用页卡片与其中四行（码提示 / g / H / 配置文件）。
    gen_card: RECT,
    gen_rows: [RECT; 4],
    /// 三段选择器（在 gen_rows[0] 内）。
    seg: RECT,
    /// 数字框外框（Edit 比它内缩 1px，描边画在框上）。
    gain_box: RECT,
    half_box: RECT,
    /// 数字框左侧的默认值提示（右对齐到框左沿 - 8）。
    gain_hint: RECT,
    half_hint: RECT,
    /// 通用页按钮行基线 y（按钮矩形由布局直接摆，这里给「短语」定位）。
    gen_btn_row: RECT,
    /// 词组页：列表卡、编辑面板卡、面板内四个输入框外框。
    words_card: RECT,
    panel_card: RECT,
    panel_boxes: [RECT; 4],
    panel_labels: [RECT; 4],
    /// 学习页：搜索框外框、列表卡。
    learn_search_box: RECT,
    learn_card: RECT,
}

pub struct App {
    hwnd: HWND,
    dpi: f32,
    /// 原生控件共用的正文字体（14px）。标题/说明走 [`Self::fonts`]。
    font: HFONT,
    list: HWND,
    e_word: HWND,
    e_pinyin: HWND,
    e_code: HWND,
    e_weight: HWND,
    b_clear: HWND,
    b_save: HWND,
    b_del: HWND,
    b_import: HWND,
    b_open: HWND,
    b_undo: HWND,
    note: HWND,
    status: HWND,

    // 通用页（只剩输入框与按钮；标题、描述、路径全部自绘）
    e_gain: HWND,
    e_half: HWND,
    b_apply: HWND,
    b_default: HWND,
    b_open_cfg: HWND,

    // 学习数据页
    e_search: HWND,
    b_search: HWND,
    learn_list: HWND,
    b_forget: HWND,
    n_learn: HWND,

    /// 每页的控件，按下标显隐。**建完就固定**，切页只动可见性。
    pages: [Vec<HWND>; PAGE_COUNT],
    /// 当前页（启动时是通用页）。
    page: usize,
    /// 启动时停在哪一页。只给 `--page` 用（截图验收要能直接打开某一页）。
    initial_page: usize,

    // ---- 视觉 ----
    colors: Colors,
    fonts: Fonts,
    geo: Geo,
    /// hover 中的页签（-1 = 无）。切页与 hover 都靠重画整窗，窗口小，代价可忽略。
    tab_hot: i32,
    /// 三段选择器 hover 中的段（-1 = 无）。
    seg_hot: i32,
    /// 码提示的当前段（0 总是 / 1 首次 / 2 关闭），与 `HINT_SEGMENTS` 对齐。
    hint_sel: usize,
    /// 焦点所在的输入框 id（画强调色描边用；0 = 无）。
    focus_id: usize,
    /// 两个数字框的即时校验结果。`Err` 时描述行画红字——说明 Static 已经不存在，
    /// 这是 paint 从这里直接取的。
    gain_state: Result<f64, String>,
    half_state: Result<f64, String>,
    /// 配置行第一行：配置文件路径（长路径按 `DT_PATH_ELLIPSIS` 中间省略，保住文件名）。
    cfg_path: String,
    /// 配置行第二行：路径之外的信息（版本 / 文件是否存在 / 第一条警告）。
    ///
    /// 拆成两行是因为上面三行都是「标题 + 说明」，只有这行把路径和说明挤在同一行里，
    /// 一行小字从左铺到右、中间还夹着「·」，看着像一段没排完版的日志。
    cfg_desc: String,
    /// 缓存的刷子：WM_ERASEBKGND / WM_CTLCOLOR* 每次消息都要一个，现场创建太浪费。
    brush_bg: HBRUSH,
    brush_card: HBRUSH,

    model: Model,
    lex: Option<Lexicons>,
    lex_note: Option<String>,
    rows: Vec<Row>,
    /// 编辑面板当前挂着哪一行（`None` = 面板是空的，保存时按新词处理）
    editing: Option<usize>,
    /// 上一次删除的 (原位置, 词条)，给「撤销删除」用
    undo: Option<(usize, UserWord)>,

    /// `config.toml`（通用页读写）
    config_path: PathBuf,
    /// `learn.db`（学习数据页只读）
    db_path: PathBuf,
    /// 学习数据页当前列出的行（下标即 ListView 行号）
    learn_rows: Vec<crate::learn::Row>,
}

impl App {
    pub fn new(
        data: Option<PathBuf>,
        words: Option<PathBuf>,
        config: Option<PathBuf>,
        db: Option<PathBuf>,
    ) -> Result<Self, String> {
        let path = words
            .or_else(crate::lex::default_words_path)
            .ok_or("取不到 %APPDATA%，请用 --words 指定词库文件")?;
        let model =
            Model::load(path.clone()).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
        let (lex, lex_note) = match find_data_dir(data) {
            Some(dir) => match Lexicons::load(&dir) {
                Ok(l) => (Some(l), None),
                Err(e) => (None, Some(e)),
            },
            None => (
                None,
                Some("找不到 data/wubi86：拼音与码只会显示手填的值，推导不可用".to_string()),
            ),
        };
        let dpi = unsafe { GetDpiForSystem() } as f32 / 96.0;
        let fonts = Fonts::new(dpi);
        let colors = Colors::light(theme::system_accent());
        let make_brush = |c: COLORREF| unsafe { CreateSolidBrush(c) };
        let config_path = config
            .or_else(crate::general::default_path)
            .ok_or("取不到 %APPDATA%，请用 --config 指定配置文件")?;
        // 学习库路径**允许取不到**：页面降级成一句「找不到学习库」，
        // 但词组页与通用页不该因此打不开。
        let db_path = db
            .or_else(crate::learn::default_path)
            .unwrap_or_else(|| PathBuf::from("learn.db"));
        Ok(Self {
            hwnd: HWND::default(),
            dpi,
            font: fonts.body,
            list: HWND::default(),
            e_word: HWND::default(),
            e_pinyin: HWND::default(),
            e_code: HWND::default(),
            e_weight: HWND::default(),
            b_clear: HWND::default(),
            b_save: HWND::default(),
            b_del: HWND::default(),
            b_import: HWND::default(),
            b_open: HWND::default(),
            b_undo: HWND::default(),
            note: HWND::default(),
            status: HWND::default(),
            e_gain: HWND::default(),
            e_half: HWND::default(),
            b_apply: HWND::default(),
            b_default: HWND::default(),
            b_open_cfg: HWND::default(),
            e_search: HWND::default(),
            b_search: HWND::default(),
            learn_list: HWND::default(),
            b_forget: HWND::default(),
            n_learn: HWND::default(),
            pages: [Vec::new(), Vec::new(), Vec::new()],
            page: PAGE_GENERAL,
            initial_page: PAGE_GENERAL,
            tab_hot: -1,
            seg_hot: -1,
            hint_sel: 0,
            focus_id: 0,
            gain_state: Ok(crate::general::GAIN_RANGE.0),
            half_state: Ok(crate::general::HALF_RANGE.0),
            cfg_path: String::new(),
            cfg_desc: String::new(),
            brush_bg: make_brush(colors.bg),
            brush_card: make_brush(colors.card),
            colors,
            fonts,
            geo: Geo::default(),
            model,
            lex,
            lex_note,
            rows: Vec::new(),
            editing: None,
            undo: None,
            config_path,
            db_path,
            learn_rows: Vec::new(),
        })
    }

    /// 启动时停在第几页（`--page`）。越界一律收到最后一页。
    pub fn set_initial_page(&mut self, page: usize) {
        self.initial_page = page.min(PAGE_COUNT - 1);
    }

    /// 逻辑像素 → 物理像素。窗口按 96 DPI 设计，乘上系统缩放即可。
    fn s(&self, v: i32) -> i32 {
        (v as f32 * self.dpi).round() as i32
    }

    fn create_children(&mut self) -> windows::core::Result<()> {
        self.create_words_page()?;
        self.create_general_page()?;
        self.create_learn_page()?;
        Ok(())
    }

    /// 词组页：列表 + 编辑面板 + 文件级操作。
    fn create_words_page(&mut self) -> windows::core::Result<()> {
        let hwnd = self.hwnd;
        // 列表不带 WS_BORDER：它的「边框」就是包着它的卡片描边（见 `paint_words`），
        // 双层框是老界面的主要土气来源之一。底色/文字色在两个列表都建完之后统一上。
        self.list = child(
            hwnd,
            w!("SysListView32"),
            "",
            WINDOW_STYLE(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | WS_TABSTOP.0
                    | LVS_REPORT
                    | LVS_SINGLESEL
                    | LVS_SHOWSELALWAYS,
            ),
            ID_LIST,
            self.font,
        )?;
        unsafe {
            send(
                self.list,
                LVM_SETEXTENDEDLISTVIEWSTYLE,
                WPARAM(LVS_EX_FULLROWSELECT),
                LPARAM(LVS_EX_FULLROWSELECT as isize),
            );
        }
        for (i, (title, width, fmt)) in [
            ("词", 140, LVCFMT_LEFT),
            ("拼音", 190, LVCFMT_LEFT),
            ("码", 90, LVCFMT_LEFT),
            // 权重是「数字 + *」：左对齐时一位数与两位数左端齐、右端不齐，
            // 一列看下来像随手打的。居中后整列有轴线。
            ("权重", 110, LVCFMT_CENTER),
            ("提示", 250, LVCFMT_LEFT),
        ]
        .into_iter()
        .enumerate()
        {
            let mut text = vec![0u16; 32];
            put_utf16(&mut text, title);
            let mut col = LVCOLUMNW {
                mask: windows::Win32::UI::Controls::LVCOLUMNW_MASK(
                    LVCF_FMT | LVCF_WIDTH | LVCF_TEXT,
                ),
                fmt,
                cx: self.s(width),
                pszText: PWSTR(text.as_mut_ptr()),
                ..Default::default()
            };
            unsafe {
                send(
                    self.list,
                    LVM_INSERTCOLUMNW,
                    WPARAM(i),
                    LPARAM(&mut col as *mut _ as isize),
                );
            }
        }

        let edit_style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | 0x0080); // ES_AUTOHSCROLL，无边框
        self.e_word = child(hwnd, w!("Edit"), "", edit_style, ID_E_WORD, self.font)?;
        self.e_pinyin = child(hwnd, w!("Edit"), "", edit_style, ID_E_PINYIN, self.font)?;
        self.e_code = child(hwnd, w!("Edit"), "", edit_style, ID_E_CODE, self.font)?;
        self.e_weight = child(hwnd, w!("Edit"), "", edit_style, ID_E_WEIGHT, self.font)?;

        // 「新增」被去掉了：编辑面板一直可输入，点「保存」就是加入/更新，
        // 多一步「先点新增」只会让人以为已经进列表了。
        //
        // 建子控件的顺序 = 视觉顺序 = Tab 键走过去的顺序，所以「保存」必须第一个建。
        // 「清空」是次要动作（只清面板、不写文件），排到行尾。
        // 所有按钮都 owner-draw：主次、hover、按下在 `draw_button` 里分派。
        self.b_save = child(
            hwnd,
            w!("Button"),
            "保存",
            WINDOW_STYLE(WS_TABSTOP.0 | BS_OWNERDRAW),
            ID_B_SAVE,
            self.font,
        )?;
        self.b_del = button(hwnd, "删除", ID_B_DEL, self.font)?;
        self.b_import = button(hwnd, "批量导入…", ID_B_IMPORT, self.font)?;
        self.b_open = button(hwnd, "打开文件位置", ID_B_OPEN, self.font)?;
        self.b_undo = button(hwnd, "撤销删除", ID_B_UNDO, self.font)?;
        self.b_clear = button(hwnd, "清空", ID_B_CLEAR, self.font)?;
        // 回车 = 保存、Esc = 清空：加词是「打一个词 → 回车 → 打下一个词」的节奏，
        // 每个词都要把鼠标挪到「保存」上，手感就像没加上一样。
        unsafe {
            for &h in &[self.e_word, self.e_pinyin, self.e_code, self.e_weight] {
                let _ = SetWindowSubclass(h, Some(edit_proc), 1, 0);
            }
        }

        self.note = child(
            hwnd,
            w!("Static"),
            "在上面四个框里填词，点「保存」或按回车就加入；点列表任一行可以改它。拼音/码留空按规则自动推导",
            WINDOW_STYLE(0),
            ID_NOTE,
            self.fonts.caption,
        )?;
        // 状态栏三页共用：它是「刚刚发生了什么」的唯一出口，切页时不跟着变。
        self.status = child(
            hwnd,
            w!("Static"),
            "",
            WINDOW_STYLE(0),
            ID_STATUS,
            self.fonts.caption,
        )?;

        self.pages[PAGE_WORDS] = vec![
            self.list,
            self.e_word,
            self.e_pinyin,
            self.e_code,
            self.e_weight,
            self.b_save,
            self.b_del,
            self.b_import,
            self.b_open,
            self.b_undo,
            self.b_clear,
            self.note,
        ];
        Ok(())
    }

    /// 通用页：码提示三段选择 + 排序两参数 + 应用/恢复默认。
    ///
    /// 这一页**没有**「即时生效」：三个框都可能处于半输入状态（`0.` 还没打完），
    /// 边打边写盘会把中间态写进配置。所以统一走「应用」，Enter 也等于应用。
    fn create_general_page(&mut self) -> windows::core::Result<()> {
        let hwnd = self.hwnd;
        let edit_style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | 0x0080);
        self.e_gain = child(hwnd, w!("Edit"), "", edit_style, ID_E_GAIN, self.font)?;
        self.e_half = child(hwnd, w!("Edit"), "", edit_style, ID_E_HALF, self.font)?;
        self.b_apply = child(
            hwnd,
            w!("Button"),
            "应用",
            WINDOW_STYLE(WS_TABSTOP.0 | BS_OWNERDRAW),
            ID_B_APPLY,
            self.font,
        )?;
        self.b_default = button(hwnd, "恢复默认", ID_B_DEFAULT, self.font)?;
        // 配置行右侧的「打开位置」：无边框文字按钮（accent 色），见 draw_button 的 ghost
        self.b_open_cfg = child(
            hwnd,
            w!("Button"),
            "打开位置",
            WINDOW_STYLE(BS_OWNERDRAW),
            ID_B_OPEN_CFG,
            self.fonts.caption,
        )?;

        // 两个数字框里按回车 = 点「应用」；不打这个补丁，回车只会「叮」一声
        unsafe {
            for &h in &[self.e_gain, self.e_half] {
                let _ = SetWindowSubclass(h, Some(enter_proc), 2, ID_B_APPLY);
            }
        }

        self.pages[PAGE_GENERAL] = vec![
            self.e_gain,
            self.e_half,
            self.b_apply,
            self.b_default,
            self.b_open_cfg,
        ];
        Ok(())
    }

    /// 学习数据页：搜索 + 只读列表 + 「清除选中词的学习分」。
    fn create_learn_page(&mut self) -> windows::core::Result<()> {
        let hwnd = self.hwnd;
        let edit_style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | 0x0080);
        self.e_search = child(hwnd, w!("Edit"), "", edit_style, ID_E_SEARCH, self.font)?;
        self.b_search = child(
            hwnd,
            w!("Button"),
            "搜",
            WINDOW_STYLE(WS_TABSTOP.0 | BS_OWNERDRAW),
            ID_B_SEARCH,
            self.font,
        )?;
        self.b_forget = button(hwnd, "清除选中词的学习分", ID_B_FORGET, self.font)?;
        self.learn_list = child(
            hwnd,
            w!("SysListView32"),
            "",
            WINDOW_STYLE(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | WS_TABSTOP.0
                    | LVS_REPORT
                    | LVS_SINGLESEL
                    | LVS_SHOWSELALWAYS,
            ),
            ID_LEARN_LIST,
            self.font,
        )?;
        // 数字列右对齐、文字列左对齐、短标记居中：**列头跟着内容走**，
        // 这是表格排版的老规矩。全都左对齐时，一列 3 位小数看着像长短不一的
        // 标签，扫不下去（99% 的人第一眼就是看「学习分」这一列）。
        for (i, (title, width, fmt)) in [
            ("词", 110, LVCFMT_LEFT),
            ("上屏次数", 90, LVCFMT_RIGHT),
            ("上次使用", 120, LVCFMT_CENTER),
            ("学习分", 90, LVCFMT_RIGHT),
            ("档位", 80, LVCFMT_CENTER),
        ]
        .into_iter()
        .enumerate()
        {
            let mut text = vec![0u16; 32];
            put_utf16(&mut text, title);
            let mut col = LVCOLUMNW {
                mask: windows::Win32::UI::Controls::LVCOLUMNW_MASK(
                    LVCF_FMT | LVCF_WIDTH | LVCF_TEXT,
                ),
                fmt,
                cx: self.s(width),
                pszText: PWSTR(text.as_mut_ptr()),
                ..Default::default()
            };
            unsafe {
                send(
                    self.learn_list,
                    LVM_INSERTCOLUMNW,
                    WPARAM(i),
                    LPARAM(&mut col as *mut _ as isize),
                );
            }
        }
        self.n_learn = child(
            hwnd,
            w!("Static"),
            "",
            WINDOW_STYLE(0),
            ID_STATUS + 1, // 不参与命中分派，id 只要不撞别的控件即可
            self.fonts.caption,
        )?;

        unsafe {
            send(
                self.learn_list,
                LVM_SETEXTENDEDLISTVIEWSTYLE,
                WPARAM(LVS_EX_FULLROWSELECT),
                LPARAM(LVS_EX_FULLROWSELECT as isize),
            );
            let _ = SetWindowSubclass(self.e_search, Some(enter_proc), 2, ID_B_SEARCH);
        }

        self.pages[PAGE_LEARN] = vec![
            self.e_search,
            self.b_search,
            self.b_forget,
            self.learn_list,
            self.n_learn,
        ];
        Ok(())
    }

    /// 两个列表的底色/文字色统一在**两页都建完之后**上色（`create_words_page`
    /// 跑的时候 `learn_list` 还不存在，那里碰不到它）。
    fn style_lists(&self) {
        unsafe {
            for &h in &[self.list, self.learn_list] {
                send(
                    h,
                    LVM_SETBKCOLOR,
                    WPARAM(0),
                    LPARAM(self.colors.card.0 as isize),
                );
                send(
                    h,
                    LVM_SETTEXTCOLOR,
                    WPARAM(0),
                    LPARAM(self.colors.text1.0 as isize),
                );
                send(
                    h,
                    LVM_SETTEXTBKCOLOR,
                    WPARAM(0),
                    LPARAM(self.colors.card.0 as isize),
                );
            }
        }
    }

    /// 切页：只动可见性，不重建控件（重建会让编辑中的内容、滚动位置全丢）。
    /// 页签本身是画出来的（见模块头），所以除了显隐只需要重画一次主窗口。
    fn show_page(&mut self, page: usize) {
        let page = page.min(PAGE_COUNT - 1);
        self.page = page;
        for (i, list) in self.pages.iter().enumerate() {
            for &h in list {
                unsafe {
                    let _ = ShowWindow(h, if i == page { SW_SHOW } else { SW_HIDE });
                }
            }
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// 一个 RECT 的便捷构造（全是逻辑像素，调用处负责 `s()` 换算）。
    fn rect(x: i32, y: i32, w: i32, h: i32) -> RECT {
        RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        }
    }

    fn inset(r: RECT, d: i32) -> RECT {
        RECT {
            left: r.left + d,
            top: r.top + d,
            right: r.right - d,
            bottom: r.bottom - d,
        }
    }

    /// 把一个单行 Edit 摆到容器框的正中。
    ///
    /// **原生单行 Edit 不做垂直居中**：控件比一行字高时，文字按「顶边 + 字体
    /// 上沿」定位，于是 32dp 的框里数字会偏高 7dp（截图量出来就是 13px）。想让
    /// 文字在框里居中，只能把控件高度收到一行字左右、再整体居中摆放——框本身
    /// 由父窗口画描边，所以控件不用铺满，露出来的白底就是卡片色，看不出接缝。
    fn center_in_box(&self, edit: HWND, box_r: RECT) {
        // 22dp ≈ 14dp 字的两行内边距：Edit 自己还要吃掉几个像素的上下留白
        let h = self.s(22).min(box_r.bottom - box_r.top);
        // 再往下 3dp：Edit 的绘制起点是「顶边 + 字体上沿」，字身比行高矮，
        // 光把控件居中会让文字偏高 2–3dp（截图量出来 5–6px），补这一下才真居中
        let top = box_r.top + (box_r.bottom - box_r.top - h) / 2 + self.s(3);
        move_rect(
            edit,
            RECT {
                left: box_r.left + self.s(1),
                top,
                right: box_r.right - self.s(1),
                bottom: top + h,
            },
            true,
        );
    }

    /// 按钮宽度：**实测**文字宽度 + 左右各 12dp 内衬，`min` 是短按钮的下限
    /// （逻辑像素）。
    ///
    /// 别按字数估算：「打开文件位置」六个字估出来 84dp，实测 90dp，差出的 6dp
    /// 全被内衬吃掉，左右只剩 5dp——跟旁边「撤销删除」的 10dp 一比就露馅。
    /// 内衬给到 16dp（而不是 Win11 名义上的 12）：`GetTextExtentPoint32W` 量的是
    /// 字身宽，不含 ClearType 的右侧光栅余量，实测每边会比标称少 3–4dp。
    fn btn_w(&self, hdc: HDC, text: &str, min: i32) -> i32 {
        let need = text_width(hdc, self.fonts.body, text) + self.s(32);
        self.s(min).max(need)
    }

    /// 整窗布局。Win11 设置的节奏：页签 44 → 页标题 → 分组标题 → 卡片 → 按钮行。
    /// 所有数字都是 96 DPI 逻辑像素，落屏前过 `s()`。
    fn layout(&mut self) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let (cw, ch) = (rc.right - rc.left, rc.bottom - rc.top);
        let m = self.s(20); // 页面左右留白
        let tab_h = self.s(44);
        let status_h = self.s(26);

        // 页签：`geo.tabs[i]` 就是**文字本身**的矩形（宽度实测），不是「点击区域」。
        // 命中测试再横向外扩（`tab_at`），这样画出来的下划线正好与文字同宽、居中。
        // 上一版按「每字 15dp + 24dp 余量」估宽、文字却按左端画，结果下划线比
        // 文字长一倍、还整体偏右——一眼就能看出「没对齐」的地方就是这里。
        //
        // 起点往前挪 4dp：矩形左右各留 4dp 给 hover 底和省略号余量，文字是**居中**
        // 画的，所以第一个页签的文字要落在 -4dp 起步的位置上，才与下面页标题的
        // 左边缘（`m`）严格对齐。两个「通用」一上一下差 4dp，扫一眼就能看出来。
        let mut tx = m - self.s(4);
        let hdc = unsafe { GetDC(Some(self.hwnd)) };
        for (i, title) in PAGE_TITLES.iter().enumerate() {
            // 左右各留 4dp：`DT_END_ELLIPSIS` 的判定比「文字宽 ≤ 矩形宽」严一点
            // （GDI 会算上笔画的右侧超出部分），刚好等宽时会把最后一个字换成「…」。
            // 文字居中画，多出来的余量左右平分，不影响下划线居中。
            let w = text_width(hdc, self.fonts.body_strong, title).max(self.s(20)) + self.s(8);
            self.geo.tabs[i] = Self::rect(tx, 0, w, tab_h);
            // 间距扣掉两侧内衬：文字之间实际隔 20dp
            tx += w + self.s(12);
        }
        if !hdc.0.is_null() {
            unsafe { ReleaseDC(Some(self.hwnd), hdc) };
        }

        let content_top = tab_h + self.s(16);
        let content_bottom = ch - status_h - self.s(8);
        let cw2 = cw - m * 2;
        // 通用页顶部还挂着页标题与分组标题，先给 layout_general 用
        let y_after_title = content_top + self.s(30) + self.s(26);

        self.layout_general(m, y_after_title, cw2, content_bottom);
        self.layout_words(m, content_top, cw2, content_bottom);
        self.layout_learn(m, content_top, cw2, content_bottom);

        self.round_edits();

        // 状态栏：全宽一行小字，贴底
        unsafe {
            let _ = MoveWindow(self.status, m, ch - status_h, cw2, self.s(20), true);
        }
    }

    /// 给无边框 Edit 也套一个 4px 圆角 region。
    ///
    /// 不做这一步，Edit 的方形白底会压住父窗口画的圆角描边：留 1px 内衬只够直边，
    /// 四个角是 4px 的弧，被方角白块一切，描边在角上就断了（截图里看到「只有右/下
    /// 两条边」正是这个）。region 让 Edit 自己的轮廓也是圆角，内外两圈才贴得住。
    fn round_edits(&self) {
        let r = self.s(4);
        // 两个列表用卡片的圆角，其余是输入框的 4px
        for (h, rad) in [(self.list, self.s(8)), (self.learn_list, self.s(8))] {
            round_control(h, rad);
        }
        for &h in &[
            self.e_gain,
            self.e_half,
            self.e_search,
            self.e_word,
            self.e_pinyin,
            self.e_code,
            self.e_weight,
        ] {
            let mut rc = RECT::default();
            unsafe {
                let _ = GetWindowRect(h, &mut rc);
            }
            let (w, ht) = (rc.right - rc.left, rc.bottom - rc.top);
            if w > 0 && ht > 0 {
                unsafe {
                    let rgn = CreateRoundRectRgn(0, 0, w + 1, ht + 1, r * 2, r * 2);
                    if !rgn.is_invalid() {
                        let _ = SetWindowRgn(h, Some(rgn), true);
                    }
                }
            }
        }
    }

    /// 通用页：页标题 + 分组标题 + 卡片（四行）+ 按钮行。
    fn layout_general(&mut self, cx: i32, cy: i32, cw2: i32, bottom: i32) {
        let row_h = self.s(62);
        // 配置行与上面三行同高：它也是「标题 + 说明」两行，只给 52 时第二行会贴到
        // 卡片下边缘，视觉上像被截断
        let cfg_h = row_h;
        let card_h = row_h * 3 + cfg_h;
        let card = Self::rect(cx, cy, cw2, card_h);
        self.geo.gen_card = card;

        // 四行：前三行各 row_h，最后是配置信息行
        for i in 0..3 {
            self.geo.gen_rows[i] = Self::rect(card.left, card.top + row_h * i as i32, cw2, row_h);
        }
        self.geo.gen_rows[3] = Self::rect(card.left, card.top + row_h * 3, cw2, cfg_h);

        // 右缘对齐的控件区
        let right = card.right - self.s(16);
        let ctl_h = self.s(32);

        // 行 0：三段选择器（3 × 88）
        let seg_w = self.s(88 * 3);
        self.geo.seg = Self::rect(right - seg_w, card.top + self.s(15), seg_w, ctl_h);

        // 行 1/2：数字框（84 宽）+ 左侧的默认值提示
        let box_w = self.s(84);
        let hint_left = card.left + self.s(180);
        let hint_w = right - box_w - self.s(8) - hint_left;
        let mut y = card.top + row_h + self.s(15);
        self.geo.gain_box = Self::rect(right - box_w, y, box_w, ctl_h);
        self.geo.gain_hint = Self::rect(hint_left, y, hint_w, ctl_h);
        y += row_h;
        self.geo.half_box = Self::rect(right - box_w, y, box_w, ctl_h);
        self.geo.half_hint = Self::rect(hint_left, y, hint_w, ctl_h);

        // 按钮行：卡片下 14px，右对齐「恢复默认」「应用」
        let btn_y = card.bottom + self.s(14);
        let btn_h = self.s(30);
        self.geo.gen_btn_row = Self::rect(cx, btn_y, cw2, btn_h);
        unsafe {
            let _ = MoveWindow(
                self.b_default,
                right - self.s(96) - self.s(72) - self.s(8),
                btn_y,
                self.s(96),
                btn_h,
                true,
            );
            let _ = MoveWindow(
                self.b_apply,
                right - self.s(72),
                btn_y,
                self.s(72),
                btn_h,
                true,
            );
            // 配置行右侧的链接
            let cfg = self.geo.gen_rows[3];
            let _ = MoveWindow(
                self.b_open_cfg,
                right - self.s(80),
                cfg.top + (cfg.bottom - cfg.top - btn_h) / 2,
                self.s(80),
                btn_h,
                true,
            );
        }

        // 数字框：Edit 内缩 1px，描边画在框上（`paint_general`）
        // 两个数字框：Edit 只占一行字高、在框里垂直居中（见 `center_in_box`）
        self.center_in_box(self.e_gain, self.geo.gain_box);
        self.center_in_box(self.e_half, self.geo.half_box);

        // 窗口太矮时卡片底部至少要在状态栏之上（MIN_H 已保证，这里只兜底）
        let _ = bottom;
    }

    /// 词组页：列表卡 + 编辑面板卡 + 按钮行 + 说明行。
    fn layout_words(&mut self, cx: i32, cy: i32, cw2: i32, bottom: i32) {
        let btn_h = self.s(30);
        let row = self.s(30);
        let note_h = self.s(22);
        // 面板卡：一行输入 + 上下内衬
        let panel_h = row + self.s(28);
        // 从底往上排：说明 → 按钮行 → 面板卡 → 列表卡
        let note_y = bottom - note_h;
        let btn_y = note_y - self.s(8) - btn_h;
        let panel_y = btn_y - self.s(12) - panel_h;
        let list_y = cy + self.s(56); // 页标题 + 分组标题
        let list_h = (panel_y - self.s(14) - list_y).max(self.s(120));

        let card = Self::rect(cx, list_y, cw2, list_h);
        self.geo.words_card = card;
        unsafe {
            // 列表内缩 1px：描边画在卡片上，别跟列表自带边框叠
            // 缩 8 的理由同学习页：列头是直角，缩 1 会把卡片圆角啃掉。
            move_rect(self.list, Self::inset(card, self.s(8)), true);
            // 说明行是原生 Static：它自己会往里缩 1px，所以控件左缘要给 `cx`
            // 而不是 `cx + 4`——否则文字比上面的卡片左缘右 4dp，扫一眼就是歪的
            let _ = MoveWindow(self.note, cx, note_y + self.s(2), cw2, note_h, true);
        }

        let panel = Self::rect(cx, panel_y, cw2, panel_h);
        self.geo.panel_card = panel;
        // 一行四组：标签 + 输入框。三个「恒定」同时成立才算排齐：
        //   ① 标签到自己的框恒 8dp（标签右对齐，所以文字右端永远贴着框）
        //   ② 上一组的框到下一组的标签恒 20dp（组间距）
        //   ③ **四个框等宽**——这条最要紧：框是这条视觉带上最重的元素，
        //      宽窄不一时眼睛先看到的是「四个框长短不一」，而不是间距。
        //
        // 做法是先扣掉「标签宽 + 8dp + 20dp」这些不可动的部分，剩下的宽度除以 4
        // 得到框宽。标签宽按各自字数算（「码」14dp、「拼音」28dp），组宽因此不同，
        // 但框一样宽、两处间距一样大。上一版把「组等宽」当目标，结果四个框宽度
        // 差出一整个字，看着像没画完。
        let label_ws: [i32; 4] = [self.s(14), self.s(14 * 2), self.s(14), self.s(14 * 2)];
        let label_to_box = self.s(8);
        let group_gap = self.s(20);
        let row_left = panel.left + self.s(16);
        let row_right = panel.right - self.s(16);
        // 4 个「标签→框」+ 3 个组间距是固定开销（组间距只有 3 个，4 个就会把
        // 最后一组压窄 20dp，四个框又不一样宽了）
        let fixed: i32 = label_ws.iter().sum::<i32>() + label_to_box * 4 + group_gap * 3;
        let box_w = ((row_right - row_left - fixed) / 4).max(self.s(60));
        let mid = panel.top + self.s(14);
        let mut x = row_left;
        for (i, &label_w) in label_ws.iter().enumerate() {
            self.geo.panel_labels[i] = Self::rect(x, mid, label_w, row);
            let box_x = x + label_w + label_to_box;
            // 最后一组吃掉取整余数，右侧才不会飘出半像素的缝
            let w = if i == 3 { row_right - box_x } else { box_w };
            self.geo.panel_boxes[i] = Self::rect(box_x, mid, w, row);
            x += label_w + label_to_box + w + group_gap;
        }
        let boxes = self.geo.panel_boxes;
        for (&box_r, &h) in
            boxes
                .iter()
                .zip(&[self.e_word, self.e_pinyin, self.e_code, self.e_weight])
        {
            self.center_in_box(h, box_r);
        }

        // 按钮行：主操作靠左，文件级操作跟后，「清空」靠右
        //
        // 宽度按**文字宽度 + 左右各 12dp 内衬**算，`min` 只是给短按钮一个下限。
        // 原来全写死：改一次文案就贴边——「打开文件位置」104 宽装 6 个 14dp 汉字，
        // 左右各剩 4dp，字像被框子夹住了。
        let gap = self.s(8);
        let hdc = unsafe { GetDC(Some(self.hwnd)) };
        let mut bx = cx;
        for (h, text, min_w) in [
            (self.b_save, "保存", 72),
            (self.b_del, "删除", 72),
            (self.b_import, "批量导入…", 96),
            (self.b_open, "打开文件位置", 104),
            (self.b_undo, "撤销删除", 88),
        ] {
            let w = self.btn_w(hdc, text, min_w);
            unsafe {
                let _ = MoveWindow(h, bx, btn_y, w, btn_h, true);
            }
            bx += w + gap;
        }
        let clear_w = self.btn_w(hdc, "清空", 64);
        if !hdc.0.is_null() {
            unsafe { ReleaseDC(Some(self.hwnd), hdc) };
        }
        unsafe {
            let _ = MoveWindow(
                self.b_clear,
                cx + cw2 - clear_w,
                btn_y,
                clear_w,
                btn_h,
                true,
            );
        }
        // 权重：词、拼音、码、权重、提示（提示最宽，它放的是「拼音/码从哪来」）
        self.fit_columns(self.list, &[16, 22, 12, 12, 38]);
    }

    /// 按权重分配列宽（总宽 = 列表客户区）。
    ///
    /// 原来只有「最后一列吃掉剩余宽度」：窗口一拉宽，词、拼音、码都不动，
    /// 剩下的全砸给最后一列——于是「提示」「档位」这种短内容被撑成一块
    /// 白板，列头孤零零飘在中间。按权重一起长，才是表格该有的样子。
    fn fit_columns(&self, list: HWND, weights: &[i32]) {
        let sum: i32 = weights.iter().sum();
        if sum <= 0 {
            return;
        }
        unsafe {
            let mut r = RECT::default();
            if GetClientRect(list, &mut r).is_err() {
                return;
            }
            // 列宽铺满到客户区右缘。留 2dp 防水平滚动条的老办法会在表头右侧
            // 留一块白 + 一条竖直分隔线，看着像多出来一列——所以干脆铺满，
            // 真挤出水平滚动条就把它藏掉：这些列表的列宽是按比例自己分的，
            // 永远不需要横向滚动。
            //
            // 有垂直滚动条时先把它占的宽度扣掉（用系统实际宽度，不用估的 18dp，
            // 估大了照样在表头右端留一条白缝）。
            let total = r.right - r.left - self.vscroll_reserve(list);
            let min = self.s(40);
            let mut used = 0;
            for (i, w) in weights.iter().enumerate() {
                let last = i + 1 == weights.len();
                let cw = if last {
                    (total - used).max(min)
                } else {
                    (total * w / sum).max(min)
                };
                used += cw;
                send(list, LVM_SETCOLUMNWIDTH, WPARAM(i), LPARAM(cw as isize));
            }
            // 铺满之后列边框可能把水平滚动条顶出来；藏掉它，别让它占掉列表底边
            let _ = ShowScrollBar(list, SB_HORZ, false);
        }
    }

    /// 这个列表会不会出垂直滚动条（行数 × 行高 > 客户区高）→ 要预留多宽。
    ///
    /// 宽度取 `SM_CXVSCROLL`（系统实际值，物理像素），不要用估的 18dp：估大一点
    /// 就会在表头右端留出一条白缝加一根分隔线。
    ///
    /// 行数为 0 或量不到行高时按「不会出」处理。
    fn vscroll_reserve(&self, list: HWND) -> i32 {
        unsafe {
            let n = send(list, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0)).0 as i32;
            if n <= 0 {
                return 0;
            }
            // LVM_GETITEMRECT 用 `rc.left` 传入「要哪一部分」：LVIR_BOUNDS = 整行
            let mut ir = RECT {
                left: LVIR_BOUNDS as i32,
                ..Default::default()
            };
            let ok = send(
                list,
                LVM_GETITEMRECT,
                WPARAM(0),
                LPARAM(&mut ir as *mut _ as isize),
            )
            .0 != 0;
            let mut cr = RECT::default();
            if !ok || GetClientRect(list, &mut cr).is_err() {
                return 0;
            }
            let row_h = (ir.bottom - ir.top).max(1);
            let client_h = (cr.bottom - cr.top).max(1);
            if n * row_h > client_h {
                GetSystemMetrics(SM_CXVSCROLL)
            } else {
                0
            }
        }
    }

    /// 学习数据页：搜索行 + 列表卡 + 说明行。
    fn layout_learn(&mut self, cx: i32, cy: i32, cw2: i32, bottom: i32) {
        let btn_h = self.s(30);
        let note_h = self.s(22);
        let search_y = cy + self.s(56);
        let note_y = bottom - note_h;
        let card_y = search_y + self.s(38);
        let card_h = (note_y - self.s(8) - card_y).max(self.s(120));
        let card = Self::rect(cx, card_y, cw2, card_h);
        self.geo.learn_card = card;
        self.geo.learn_search_box = Self::rect(cx + self.s(40), search_y, self.s(320), btn_h);

        // 搜索框里的字同样要垂直居中；内衬必须过 `s()`，写死的 1 在 DPI 2.0 下
        // 只有 0.5dp，Edit 白底会盖住描边的内半边
        self.center_in_box(self.e_search, self.geo.learn_search_box);

        unsafe {
            let _ = MoveWindow(
                self.b_search,
                self.geo.learn_search_box.right + self.s(8),
                search_y,
                self.s(48),
                btn_h,
                true,
            );
            // 「清除」是破坏性动作，靠右单独放，别挨着搜索框（F46 的同一条理由）
            let _ = MoveWindow(
                self.b_forget,
                cx + cw2 - self.s(176),
                search_y,
                self.s(176),
                btn_h,
                true,
            );
            // 列表内缩 8：卡片描边与列表之间留出呼吸位。缩 1 时列头（直角、带自己的
            // 背景）正好压在卡片圆角上，四个角全被削成方的，看着像「卡片没画圆」。
            move_rect(self.learn_list, Self::inset(card, self.s(8)), true);
            // 同词组页：Static 自己缩 1px，控件左缘对齐 `cx` 才与卡片左缘齐
            let _ = MoveWindow(self.n_learn, cx, note_y + self.s(2), cw2, note_h, true);
        }
        // 权重：词、上屏次数、上次使用、学习分、档位。数字列给足宽度，
        // 右对齐的数字才有「小数点成列」的效果
        self.fit_columns(self.learn_list, &[20, 18, 20, 18, 24]);
    }

    /// 重算推导列并重填 ListView（保留当前选中项）。
    fn refresh(&mut self) {
        let keep = self
            .editing
            .and_then(|i| self.rows.get(i).map(|r| r.raw.word.clone()));
        self.rows = self.model.rows(self.lex.as_ref());
        unsafe {
            send(self.list, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
        }
        for (i, r) in self.rows.iter().enumerate() {
            insert_row(self.list, i, r);
        }
        // 选中项按词名找回：刷新后下标可能变（排序不保证，但删除会挪位）
        if let Some(word) = keep {
            if let Some(idx) = self.rows.iter().position(|r| r.raw.word == word) {
                self.select(idx);
            } else {
                self.editing = None;
                self.fill_panel(None);
            }
        }
        self.update_buttons();
        self.set_status_counts();
        self.update_note();
    }

    /// 取消列表选中（「清空」回到新词态时要连高亮一起去掉）。
    fn clear_selection(&self) {
        let mut item = LVITEMW {
            state: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(0),
            stateMask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0),
            ..Default::default()
        };
        unsafe {
            send(
                self.list,
                LVM_SETITEMSTATE,
                WPARAM(usize::MAX), // -1 = 所有行
                LPARAM(&mut item as *mut _ as isize),
            );
        }
    }

    fn select(&mut self, idx: usize) {
        select_item(self.list, idx);
        self.editing = Some(idx);
        self.fill_panel(Some(idx));
        self.update_buttons();
        self.update_note();
    }

    fn selected(&self) -> Option<usize> {
        let r = unsafe {
            send(
                self.list,
                LVM_GETNEXTITEM,
                WPARAM(usize::MAX),
                LPARAM(LVNI_SELECTED as isize),
            )
        };
        (r.0 >= 0).then_some(r.0 as usize)
    }

    fn fill_panel(&self, idx: Option<usize>) {
        let (w, p, c, wt) = match idx.and_then(|i| self.rows.get(i)) {
            Some(r) => (
                r.raw.word.clone(),
                r.raw.pinyin.clone().unwrap_or_default(),
                r.raw.code.clone().unwrap_or_default(),
                r.raw.weight.map(|x| format!("{x:.0}")).unwrap_or_default(),
            ),
            None => (String::new(), String::new(), String::new(), String::new()),
        };
        set_text(self.e_word, &w);
        set_text(self.e_pinyin, &p);
        set_text(self.e_code, &c);
        set_text(self.e_weight, &wt);
    }

    fn panel_word(&self) -> UserWord {
        let pinyin = get_text(self.e_pinyin);
        let code = get_text(self.e_code);
        let weight = get_text(self.e_weight);
        UserWord {
            word: get_text(self.e_word),
            pinyin: (!pinyin.trim().is_empty()).then(|| pinyin.trim().to_string()),
            code: (!code.trim().is_empty()).then(|| code.trim().to_string()),
            weight: weight.trim().parse::<f64>().ok(),
        }
    }

    fn update_buttons(&self) {
        let has_sel = self.editing.is_some();
        let has_word = !get_text(self.e_word).trim().is_empty();
        let dirty = has_word
            || !get_text(self.e_pinyin).trim().is_empty()
            || !get_text(self.e_code).trim().is_empty()
            || !get_text(self.e_weight).trim().is_empty();
        // 「保存」只在真的填了词时才亮：空面板上点亮它就是个假按钮，
        // 点下去只能弹一句「词不能为空」，和原来那个「新增」一样没意义。
        enable_window(self.b_save, has_word);
        enable_window(self.b_del, has_sel && has_word);
        enable_window(self.b_clear, dirty || has_sel);
        enable_window(self.b_undo, self.undo.is_some());
    }

    /// 面板下方那行说明。没选中时讲「怎么加词」，选中某行时直接告出**该打什么**。
    ///
    /// 后半句是这次反馈逼出来的：用户加了词、回去却打不出来，最常见的原因不是没生效，
    /// 而是码序记错（微信 = 微 tm + 信 wy = `tmwy`，不是 `wmty`）。把码摆在眼前，
    /// 比让用户自己去列表里找要省事。
    fn update_note(&self) {
        let text = match self.editing.and_then(|i| self.rows.get(i)) {
            Some(r) => {
                let how = match (&r.code, &r.pinyin) {
                    (Some(c), _) => format!("打字时输 {c}"),
                    (None, Some(p)) => format!("打字时输 {p}"),
                    (None, None) => "这条推不出码：在「码」框里手填一个".to_string(),
                };
                format!(
                    "「{}」· 码 {} · 拼音 {} · {}　（* = 按规则自动推导，想固定就在框里手填）",
                    r.raw.word,
                    r.code_cell(),
                    r.pinyin_cell(),
                    how
                )
            }
            None => {
                "在上面四个框里填词，点「保存」就加入；点列表任一行可以改它。拼音/码留空自动推导"
                    .to_string()
            }
        };
        set_text(self.note, &text);
    }

    fn set_status(&self, text: &str) {
        set_text(self.status, text);
    }

    fn set_status_counts(&self) {
        let bad = self.rows.iter().filter(|r| r.note.is_some()).count();
        let mut s = format!("{} 条 · {}", self.rows.len(), self.model.path.display());
        if bad > 0 {
            s.push_str(&format!(" · {bad} 条有提示"));
        }
        if let Some(w) = self.model.warnings.first() {
            s.push_str(&format!(" · 读文件时 {w}"));
        }
        if let Some(n) = &self.lex_note {
            s.push_str(&format!(" · {n}"));
        }
        self.set_status(&s);
    }

    fn save(&mut self) -> bool {
        match self.model.save() {
            Ok(()) => true,
            Err(e) => {
                message(
                    self.hwnd,
                    "保存失败",
                    &format!("{}：{e}", self.model.path.display()),
                );
                false
            }
        }
    }

    /// 「清空」= 回到新词态：清掉四个框、取消列表高亮。它不写文件，
    /// 只是把面板腾出来——保存才是唯一落盘动作。
    fn on_clear(&mut self) {
        self.editing = None;
        self.clear_selection();
        self.fill_panel(None);
        set_focus(self.e_word);
        self.update_buttons();
        self.update_note();
        self.set_status("已清空编辑区；填好词点「保存」即加入");
    }

    fn on_save(&mut self) {
        let mut w = self.panel_word();
        if w.word.trim().is_empty() {
            self.set_status("「词」不能为空——先在上面填一个词再点保存");
            set_focus(self.e_word);
            return;
        }
        w.word = w.word.trim().to_string();
        // 只有**从列表里点进来的那一行**被改了词名才算「改名」（删旧 + 加新）。
        //
        // 判据不能是「上次保存后留下的 editing」：那样连着加第二个词时面板还挂着
        // 上一个词，于是被当成改名、旧词被删掉——连加三次列表里只留一条（F46）。
        // 保存完把面板腾空，就再也不可能误判。
        let renamed = self
            .editing
            .and_then(|i| self.rows.get(i))
            .map(|r| r.raw.word.clone())
            .filter(|old| *old != w.word);
        if let Some(old) = &renamed {
            self.model.remove(old);
        }
        let (_, is_new) = self.model.upsert(w.clone());
        if !self.save() {
            return;
        }
        // 回到新词态：面板空、无选中、焦点在词框。下一次点保存（或回车）就是再加一条。
        self.editing = None;
        self.fill_panel(None);
        self.refresh();
        set_focus(self.e_word);
        // `refresh` 会清空并重填列表，滚动位置跟着回到顶部——词一多，刚存的这条就看不见了。
        // 只滚过去、**不选中**：选中会填面板，又变回「下一次保存被当成改名」的老问题。
        let at = self.rows.iter().position(|r| r.raw.word == w.word);
        if let Some(i) = at {
            unsafe {
                send(self.list, LVM_ENSUREVISIBLE, WPARAM(i), LPARAM(0));
            }
        }
        let how = match at.and_then(|i| self.rows.get(i)) {
            Some(r) => match (&r.code, &r.pinyin) {
                (Some(c), _) => format!("打字时输 {c}"),
                (None, Some(p)) => format!("打字时输 {p}"),
                (None, None) => "还没推出码，去「码」框里手填一个".to_string(),
            },
            None => String::new(),
        };
        let verb = match (&renamed, is_new) {
            (Some(old), _) => format!("已改名「{old}」→「{}」", w.word),
            (None, true) => format!("已加入「{}」", w.word),
            (None, false) => format!("已更新「{}」", w.word),
        };
        self.set_status(&format!("{verb} · {how} · 最坏 3 秒内可打"));
    }

    fn on_delete(&mut self) {
        let Some(idx) = self.editing else {
            return;
        };
        let Some(row) = self.rows.get(idx) else {
            return;
        };
        let word = row.raw.word.clone();
        if let Some((at, w)) = self.model.remove(&word) {
            self.undo = Some((at, w));
            if self.save() {
                self.editing = None;
                self.refresh();
                self.set_status(&format!("已删除「{word}」· 点「撤销删除」可以反悔"));
            }
        }
    }

    fn on_undo(&mut self) {
        let Some((at, w)) = self.undo.take() else {
            return;
        };
        let word = w.word.clone();
        self.model.insert_at(at, w);
        if self.save() {
            self.refresh();
            if let Some(idx) = self.rows.iter().position(|r| r.raw.word == word) {
                self.select(idx);
            }
            self.set_status(&format!("已恢复「{word}」"));
        }
        self.update_buttons();
    }

    fn on_import(&mut self) {
        let Some(path) = pick_file(self.hwnd) else {
            return;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                message(self.hwnd, "导入失败", &format!("{}：{e}", path.display()));
                return;
            }
        };
        let incoming = heinrich_scheme::user::parse(&text);
        if incoming.words.is_empty() {
            message(self.hwnd, "导入失败", "文件里没有能读的词");
            return;
        }
        let mut added = 0usize;
        let total = incoming.words.len();
        for w in incoming.words {
            if self.model.upsert(w).1 {
                added += 1;
            }
        }
        if self.save() {
            self.refresh();
            let mut s = format!(
                "已导入 {total} 条（新增 {added} / 覆盖 {}），共 {} 条",
                total - added,
                self.rows.len()
            );
            if let Some(w) = incoming.warnings.first() {
                s.push_str(&format!(" · {w}"));
            }
            self.set_status(&s);
        }
    }

    fn on_open(&mut self) {
        let path = self.model.path.clone();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let target = wide(&format!("/select,\"{}\"", path.display()));
        unsafe {
            ShellExecuteW(
                Some(self.hwnd),
                w!("open"),
                w!("explorer.exe"),
                PCWSTR(target.as_ptr()),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
    }

    // ------------------------------------------------------------ 通用页

    /// 从磁盘重读三项并刷进控件。打开窗口、点「应用」、点「恢复默认」之后各一次。
    ///
    /// **刷新一定从文件读**，不从界面读：界面只是文件的投影，照界面回填会把
    /// 「文件里本来写的是 500、被界面夹成 365」这种差异永久抹平。
    /// 从磁盘重读三项并刷进控件。打开窗口、点「应用」、点「恢复默认」之后各一次。
    ///
    /// **刷新一定从文件读**，不从界面读：界面只是文件的投影，照界面回填会把
    /// 「文件里本来写的是 500、被界面夹成 365」这种差异永久抹平。
    fn refresh_general(&mut self) {
        let g = crate::general::General::load(&self.config_path);
        self.hint_sel = hint_index(g.hint);
        set_text(self.e_gain, &crate::general::fmt_number(g.gain));
        set_text(self.e_half, &crate::general::fmt_number(g.half_life));
        self.update_general_fields();

        self.cfg_path = self.config_path.display().to_string();
        // `g.version` 本身就是「config_version = 1」或「config_version 未写入（按 v1
        // 处理）」，前面再加「配置版本」会读成「配置版本 config_version = 1」，重复
        let mut desc = g.version.clone();
        if !g.found {
            desc.push_str(" · 文件还不存在，点「应用」时会创建");
        }
        for w in g.warnings.iter().take(1) {
            desc.push_str(&format!(" · {w}"));
        }
        self.cfg_desc = desc;
        self.repaint();
    }

    fn selected_hint(&self) -> HintMode {
        match self.hint_sel {
            1 => HintMode::FirstTime,
            2 => HintMode::Off,
            _ => HintMode::Mixed,
        }
    }

    /// 码提示每段的描述，绘制时直接取（不再有 Static 存它）。
    fn hint_desc(sel: usize) -> &'static str {
        match sel {
            1 => "只在这个词还没上过屏时显示一次（学会就不打扰）",
            2 => "一律不显示五笔码",
            _ => "拼音候选旁边总是显示这个词的五笔码",
        }
    }

    /// 点了三段选择器：只改内存里的段位，落盘仍等「应用」。
    fn select_hint(&mut self, seg: usize) {
        if self.hint_sel != seg {
            self.hint_sel = seg;
            self.repaint();
        }
    }

    /// 两个数字框的即时校验：合法就亮「应用」，非法就把原因存下来给绘制画红字。
    ///
    /// 界面层拦住非法值，是为了不让人写进一个会被 `sanitized` 静默夹掉的数——
    /// 那种「写了 500、跑起来是 365、界面上还显示 500」最费时间。
    fn update_general_fields(&mut self) {
        self.gain_state =
            crate::general::parse_number(&get_text(self.e_gain), crate::general::GAIN_RANGE, "");
        self.half_state =
            crate::general::parse_number(&get_text(self.e_half), crate::general::HALF_RANGE, " 天");
        enable_window(
            self.b_apply,
            self.gain_state.is_ok() && self.half_state.is_ok(),
        );
        self.repaint();
    }

    fn on_apply(&mut self) {
        let gain = match self.gain_state {
            Ok(v) => v,
            Err(ref e) => {
                self.set_status(&format!("学习增益没法用：{e}"));
                return;
            }
        };
        let half = match self.half_state {
            Ok(v) => v,
            Err(ref e) => {
                self.set_status(&format!("半衰期没法用：{e}"));
                return;
            }
        };
        let hint = self.selected_hint();
        match crate::general::save(&self.config_path, hint, gain, half) {
            Ok(()) => {
                self.refresh_general();
                self.set_status("已写入配置文件，1 秒内生效（不用重启任何东西）");
            }
            Err(e) => message(
                self.hwnd,
                "保存失败",
                &format!("{}：{e}", self.config_path.display()),
            ),
        }
    }

    fn on_restore_defaults(&mut self) {
        match crate::general::restore_defaults(&self.config_path) {
            Ok(()) => {
                self.refresh_general();
                self.set_status(
                    "已恢复默认：三行从配置文件里删掉了（不写死数值，将来默认值变了也会跟着走）",
                );
            }
            Err(e) => message(
                self.hwnd,
                "恢复默认失败",
                &format!("{}：{e}", self.config_path.display()),
            ),
        }
    }

    /// 打开配置文件所在目录（资源管理器，选中文件本身）。
    fn on_open_cfg(&mut self) {
        let path = self.config_path.clone();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let target = wide(&format!("/select,\"{}\"", path.display()));
        unsafe {
            ShellExecuteW(
                Some(self.hwnd),
                w!("open"),
                w!("explorer.exe"),
                PCWSTR(target.as_ptr()),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
    }

    /// 重画整窗。静态文案全部由 `paint` 从 `self` 现取，所以状态一变就整窗重画——
    /// 窗口小（< 1000×700），一次全画在 GDI 下微秒级，不值得做脏矩形。
    fn repaint(&self) {
        if self.hwnd.0.is_null() {
            return;
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    // -------------------------------------------------------- 学习数据页

    /// 学习分用当前生效的 g / H 现场算，所以先按配置合成一份 `RankingConfig`。
    fn ranking_config(&self) -> RankingConfig {
        let g = crate::general::General::load(&self.config_path);
        RankingConfig {
            learning_gain: g.gain,
            half_life_days: g.half_life,
        }
        .sanitized()
    }

    fn refresh_learn(&mut self) {
        let cfg = self.ranking_config();
        let needle = get_text(self.e_search);
        match crate::learn::load(&self.db_path, &cfg, &needle) {
            Ok(rep) => {
                self.learn_rows = rep.rows;
                unsafe {
                    send(self.learn_list, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
                }
                for (i, r) in self.learn_rows.iter().enumerate() {
                    insert_learn_row(self.learn_list, i, r);
                }
                let shown = self.learn_rows.len();
                let mut note = if rep.total > shown {
                    format!(
                        "显示 {shown} 条（共 {} 条用过的词，按学习分取前 {shown}）",
                        rep.total
                    )
                } else {
                    format!("{shown} 条用过的词")
                };
                if shown == 0 && !needle.trim().is_empty() {
                    note.push_str(" · 没有匹配的词");
                }
                note.push_str(" · 学习分 = ln(次数) × 衰减；第一次上屏不计分");
                set_text(self.n_learn, &note);
            }
            Err(e) => {
                self.learn_rows.clear();
                unsafe {
                    send(self.learn_list, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
                }
                set_text(self.n_learn, &e);
            }
        }
        self.update_learn_buttons();
    }

    fn selected_learn(&self) -> Option<usize> {
        let r = unsafe {
            send(
                self.learn_list,
                LVM_GETNEXTITEM,
                WPARAM(usize::MAX),
                LPARAM(LVNI_SELECTED as isize),
            )
        };
        (r.0 >= 0).then_some(r.0 as usize)
    }

    fn update_learn_buttons(&self) {
        let has = self
            .selected_learn()
            .and_then(|i| self.learn_rows.get(i))
            .is_some();
        enable_window(self.b_forget, has);
    }

    fn on_forget(&mut self) {
        let Some(idx) = self.selected_learn() else {
            return;
        };
        let Some(row) = self.learn_rows.get(idx) else {
            return;
        };
        let word = row.word.clone();
        let detail = format!(
            "「{word}」上屏 {} 次，上次 {}。\n\n清零后它的位次会回到按码表排的位置，\
             最多 3 秒生效。次数与时间找不回来；置顶/抑制档位会保留。",
            row.n,
            row.ago(crate::learn::now_unix()),
        );
        if !confirm(self.hwnd, "清除这个词的学习分？", &detail) {
            return;
        }
        match crate::learn::clear(&self.db_path, &word) {
            Ok(0) => self.set_status(&format!("「{word}」本来就没有学习记录")),
            Ok(_) => {
                self.refresh_learn();
                self.set_status(&format!("已清除「{word}」的学习分，位次最多 3 秒后回落"));
            }
            Err(e) => message(
                self.hwnd,
                "清除失败",
                &format!("{e}\n\n可能是 server 正在写库，稍后再试"),
            ),
        }
    }

    fn on_search(&mut self) {
        self.refresh_learn();
        let needle = get_text(self.e_search);
        if needle.trim().is_empty() {
            self.set_status("已列出全部用过的词");
        } else {
            self.set_status(&format!("按「{}」过滤", needle.trim()));
        }
    }
}

fn insert_row(list: HWND, idx: usize, r: &Row) {
    let mut text = wide(&r.raw.word);
    let mut item = LVITEMW {
        mask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_FLAGS(LVIF_TEXT | LVIF_PARAM),
        iItem: idx as i32,
        iSubItem: 0,
        pszText: PWSTR(text.as_mut_ptr()),
        lParam: LPARAM(idx as isize),
        ..Default::default()
    };
    unsafe {
        send(
            list,
            LVM_INSERTITEMW,
            WPARAM(0),
            LPARAM(&mut item as *mut _ as isize),
        );
    }
    for (k, cell) in [
        r.pinyin_cell(),
        r.code_cell(),
        r.weight_cell(),
        r.note.clone().unwrap_or_default(),
    ]
    .into_iter()
    .enumerate()
    {
        let mut text = wide(&cell);
        let mut sub = LVITEMW {
            mask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_FLAGS(LVIF_TEXT),
            iItem: idx as i32,
            iSubItem: (k + 1) as i32,
            pszText: PWSTR(text.as_mut_ptr()),
            ..Default::default()
        };
        unsafe {
            send(
                list,
                LVM_SETITEMW,
                WPARAM(0),
                LPARAM(&mut sub as *mut _ as isize),
            );
        }
    }
}

/// 下拉框里 mixed / first_time / off 的位置，与 [`App::update_hint_desc`] 的顺序一致。
fn hint_index(mode: HintMode) -> usize {
    match mode {
        HintMode::Mixed => 0,
        HintMode::FirstTime => 1,
        HintMode::Off => 2,
    }
}

/// 学习数据页的一行。列序：词 / 次数 / 上次使用 / 学习分 / 档位。
fn insert_learn_row(list: HWND, idx: usize, r: &crate::learn::Row) {
    let now = crate::learn::now_unix();
    let cells = [
        r.word.clone(),
        r.n.to_string(),
        r.ago(now),
        format!("{:.3}", r.score),
        r.tier_label().to_string(),
    ];
    for (k, cell) in cells.iter().enumerate() {
        let mut text = wide(cell);
        let mut item = LVITEMW {
            mask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_FLAGS(
                LVIF_TEXT | if k == 0 { LVIF_PARAM } else { 0 },
            ),
            iItem: idx as i32,
            iSubItem: k as i32,
            pszText: PWSTR(text.as_mut_ptr()),
            lParam: LPARAM(idx as isize),
            ..Default::default()
        };
        let msg = if k == 0 {
            LVM_INSERTITEMW
        } else {
            LVM_SETITEMW
        };
        unsafe {
            send(list, msg, WPARAM(0), LPARAM(&mut item as *mut _ as isize));
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
            // **必须转给 DefWindowProcW**：窗口标题就是它在 WM_NCCREATE 里从
            // CREATESTRUCT 存下来的。只返回 1 会让窗口建出来没有标题（踩过：
            // FindWindowW 找不到窗口，截图脚本报 window not found）。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_CREATE => {
            if let Some(app) = unsafe { app_of(hwnd) } {
                app.hwnd = hwnd;
                if app.create_children().is_ok() {
                    app.style_lists();
                    app.layout();
                    // 默认停在第一页（通用）。先显页再刷数据：刷的时候要读文件的，
                    // 读失败的信息得写在已经可见的控件上。
                    let page = app.initial_page;
                    app.show_page(page);
                    app.refresh_general();
                    app.refresh_learn();
                    app.refresh();
                    // 刻意**不**自动选中第一行：面板是给「新词」用的，一开窗就
                    // 躺着上次的词会让人以为已经选中了它，改完点保存就把那条词改了。
                    app.set_status("");
                }
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(app) = unsafe { app_of(hwnd) } {
                app.layout();
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            // 背景就是色表的 bg。返回 1 = 「我擦过了」，别再走 DefWindowProc
            // 的白墙刷（那是全窗口闪烁的来源之一）。
            if let Some(app) = unsafe { app_of(hwnd) } {
                let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
                let mut rc = RECT::default();
                unsafe {
                    let _ = GetClientRect(hwnd, &mut rc);
                    let _ = FillRect(hdc, &rc, app.brush_bg);
                }
                LRESULT(1)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_PAINT => {
            if let Some(app) = unsafe { app_of(hwnd) } {
                app.paint();
            }
            LRESULT(0)
        }
        WM_DRAWITEM => {
            // 所有按钮都是 BS_OWNERDRAW，这里统一入口
            let dis = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            if let Some(app) = unsafe { app_of(hwnd) } {
                app.draw_button(dis);
            }
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            if let Some(app) = unsafe { app_of(hwnd) } {
                let pt = POINT {
                    x: (lparam.0 & 0xFFFF) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
                };
                app.on_click(pt);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(app) = unsafe { app_of(hwnd) } {
                let pt = POINT {
                    x: (lparam.0 & 0xFFFF) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
                };
                app.on_mouse_move(pt);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if let Some(app) = unsafe { app_of(hwnd) } {
                app.tab_hot = -1;
                app.seg_hot = -1;
                app.repaint();
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xffff;
            let code = ((wparam.0 >> 16) & 0xffff) as u32;
            if let Some(app) = unsafe { app_of(hwnd) } {
                match (code, id) {
                    // 焦点跟到哪个输入框，那个框的描边就用强调色（Win11 行为）
                    (EN_SETFOCUS, _) => {
                        app.focus_id = id;
                        app.repaint();
                    }
                    (EN_KILLFOCUS, _) => {
                        if app.focus_id == id {
                            app.focus_id = 0;
                            app.repaint();
                        }
                    }
                    // EN_CHANGE = 编辑框内容变了。三个编辑框的判据各不相同，按 id
                    // 分派——一律走同一个函数会拿数字框的内容去判「词填了没」。
                    (EN_CHANGE, ID_E_GAIN | ID_E_HALF) => app.update_general_fields(),
                    (EN_CHANGE, ID_E_WORD | ID_E_PINYIN | ID_E_CODE | ID_E_WEIGHT) => {
                        app.update_buttons()
                    }
                    (0, ID_B_CLEAR) => app.on_clear(),
                    (0, ID_B_SAVE) => app.on_save(),
                    (0, ID_B_DEL) => app.on_delete(),
                    (0, ID_B_UNDO) => app.on_undo(),
                    (0, ID_B_IMPORT) => app.on_import(),
                    (0, ID_B_OPEN) => app.on_open(),
                    (0, ID_B_APPLY) => app.on_apply(),
                    (0, ID_B_DEFAULT) => app.on_restore_defaults(),
                    (0, ID_B_OPEN_CFG) => app.on_open_cfg(),
                    (0, ID_B_SEARCH) => app.on_search(),
                    (0, ID_B_FORGET) => app.on_forget(),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            let hdr = unsafe { &*(lparam.0 as *const windows::Win32::UI::Controls::NMHDR) };
            if let Some(app) = unsafe { app_of(hwnd) } {
                if hdr.code == LVN_ITEMCHANGED && hdr.hwndFrom == app.list {
                    // 不读 NMLISTVIEW 的字段，直接问「现在选的是谁」——少一处
                    // 结构体布局的假设，行为一样
                    if let Some(idx) = app.selected() {
                        if app.editing != Some(idx) {
                            app.editing = Some(idx);
                            app.fill_panel(Some(idx));
                            app.update_buttons();
                        }
                    }
                } else if hdr.code == LVN_ITEMCHANGED && hdr.hwndFrom == app.learn_list {
                    // 两个 ListView 同时存在，「有没有选中」要各问各的：
                    // 拿词语列表的选中项去判「能不能清除学习分」会指错行
                    app.update_learn_buttons();
                }
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            // 说明性质的 Static（note / status / n_learn）全部：透明底 + 次级文字色 +
            // 页面底色刷。回页面底色而不是白色，字缝里透出的才是底色而不是白方块。
            let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
            if let Some(app) = unsafe { app_of(hwnd) } {
                unsafe {
                    SetBkMode(hdc, TRANSPARENT);
                    SetTextColor(hdc, app.colors.text2);
                }
                LRESULT(app.brush_bg.0 as isize)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(app) = unsafe { app_of(hwnd) } {
                unsafe {
                    for &h in &[app.e_word, app.e_pinyin, app.e_code, app.e_weight] {
                        let _ = RemoveWindowSubclass(h, Some(edit_proc), 1);
                    }
                    for &h in &[app.e_gain, app.e_half, app.e_search] {
                        let _ = RemoveWindowSubclass(h, Some(enter_proc), 2);
                    }
                    let _ = DeleteObject(app.brush_bg.into());
                    let _ = DeleteObject(app.brush_card.into());
                }
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_NCDESTROY => {
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 页签 / 三段选择器 / 「打开位置」链接的命中与切换。
impl App {
    /// 页签命中。`geo.tabs` 存的是**文字**矩形（用于画下划线），点击区域要往外扩：
    /// 没人会精确点「通用」那两个字。两侧各 10dp，页签间距 28dp，相邻区域之间
    /// 还留 8dp 空隙，不会误点。
    fn tab_at(&self, pt: POINT) -> Option<usize> {
        if pt.y < 0 || pt.y >= self.s(44) {
            return None;
        }
        let pad = self.s(10);
        self.geo
            .tabs
            .iter()
            .position(|r| pt.x >= r.left - pad && pt.x < r.right + pad)
    }

    fn on_click(&mut self, pt: POINT) {
        // 页签
        if let Some(i) = self.tab_at(pt) {
            if self.page != i {
                self.switch_to(i);
            }
            return;
        }
        // 三段选择器（只在通用页响应）
        if self.page == PAGE_GENERAL && pt_in(pt, self.geo.seg) {
            let seg_w = (self.geo.seg.right - self.geo.seg.left) / 3;
            let seg = ((pt.x - self.geo.seg.left) / seg_w.max(1)).clamp(0, 2) as usize;
            self.select_hint(seg);
        }
    }

    fn on_mouse_move(&mut self, pt: POINT) {
        let tab = self.tab_at(pt).map(|i| i as i32).unwrap_or(-1);
        let seg = if pt_in(pt, self.geo.seg) {
            let seg_w = (self.geo.seg.right - self.geo.seg.left) / 3;
            ((pt.x - self.geo.seg.left) / seg_w.max(1)).clamp(0, 2)
        } else {
            -1
        };
        if tab != self.tab_hot || seg != self.seg_hot {
            self.tab_hot = tab;
            self.seg_hot = seg;
            self.repaint();
        }
        // hover 离开通知只订阅一次；WM_MOUSELEAVE 后再订阅即可持续生效
        unsafe {
            let _ = TrackMouseEvent(&mut TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            });
        }
    }

    /// 自测与「点页签」共用的切页入口。
    fn switch_to(&mut self, page: usize) {
        self.show_page(page);
    }
}

/// secondary 档按钮的便捷构造（主按钮/ghost 按钮样式各异，就地写）。
fn button(parent: HWND, text: &str, id: usize, font: HFONT) -> windows::core::Result<HWND> {
    child(
        parent,
        w!("Button"),
        text,
        WINDOW_STYLE(WS_TABSTOP.0 | BS_OWNERDRAW),
        id,
        font,
    )
}

/// `MoveWindow` 的 RECT 版。
fn move_rect(hwnd: HWND, r: RECT, repaint: bool) {
    unsafe {
        let _ = MoveWindow(
            hwnd,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            repaint,
        );
    }
}

/// 把一个子窗口裁成圆角矩形（坐标相对窗口自身）。
fn round_control(hwnd: HWND, radius: i32) {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rc);
    }
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    if w <= 0 || h <= 0 {
        return;
    }
    unsafe {
        let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, radius * 2, radius * 2);
        if !rgn.is_invalid() {
            let _ = SetWindowRgn(hwnd, Some(rgn), true);
        }
    }
}

fn pt_in(pt: POINT, r: RECT) -> bool {
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

// ---------------------------------------------------------------- 绘制

/// 绘制辅助（都是 96 DPI 语义下的薄封装，颜色来自 `theme::Colors`）。
impl App {
    /// 整窗绘制入口：先铺底，再画页签与当前页。
    ///
    /// **底必须在这里铺，不能指望 `WM_ERASEBKGND`**。`repaint()` 用的是
    /// `InvalidateRect(..., false)`（不请求擦除），`ShowWindow(SW_HIDE)` 暴露出来的
    /// 那块区域也不会走擦除——于是切页时新页只画自己那几张卡片，旧页的页标题、
    /// 卡片描边、列表、页签下划线全留在屏上，两层直接叠着。用户看到的就是
    /// 「tab 只添了下划线、内容没换」和「切几次内容糊在一起」。
    ///
    /// 窗口小（900×600），一次全铺在 GDI 下是微秒级，不值得做脏矩形。
    fn paint(&self) {
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        if hdc.is_invalid() {
            return;
        }
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rc);
            // 有 WS_CLIPCHILDREN，子控件占的地方会被裁掉，不会把 Edit 白底刷成本底色
            let _ = FillRect(hdc, &rc, self.brush_bg);
        }
        self.draw_tab_bar(hdc);
        match self.page {
            PAGE_GENERAL => self.draw_general(hdc),
            PAGE_WORDS => self.draw_words(hdc),
            _ => self.draw_learn(hdc),
        }
        unsafe {
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    /// 页签栏：文字 + 选中项底下的 3px 强调色条 + 一条全宽分隔线。
    fn draw_tab_bar(&self, hdc: HDC) {
        let tab_h = self.s(44);
        unsafe {
            // 分隔线横贯整个客户区。之前按「最后一个页签 + 200px」估算，
            // 窗口一拉宽线就在半路断掉，看起来像画到一半没墨了。
            let mut rc = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rc);
            let pen = CreatePen(PS_SOLID, self.s(1), self.colors.card_border);
            let old = SelectObject(hdc, pen.into());
            let _ = MoveToEx(hdc, 0, tab_h, None);
            let _ = LineTo(hdc, rc.right, tab_h);
            SelectObject(hdc, old);
            let _ = DeleteObject(pen.into());
        }
        for (i, title) in PAGE_TITLES.iter().enumerate() {
            let r = self.geo.tabs[i];
            let selected = i == self.page;
            let hot = i as i32 == self.tab_hot;
            let color = if selected || hot {
                self.colors.text1
            } else {
                self.colors.text2
            };
            // 选中的重一档（WinUI 的导航项就是这样）：浅色底上 #1A1A1A 与
            // #5D5D5D 的差别在小字号下不够分明，加粗才看得出「当前在哪一页」。
            let font = if selected {
                self.fonts.body_strong
            } else {
                self.fonts.body
            };
            // hover 反馈：非选中页签在文字外加一层淡底（WinUI 的导航项就这样）。
            // 只改文字颜色太弱——鼠标划过时几乎看不出「这里能点」。
            if hot && !selected {
                let pill = Self::rect(
                    r.left - self.s(10),
                    r.top + self.s(7),
                    (r.right - r.left) + self.s(20),
                    (r.bottom - r.top) - self.s(7) - self.s(10),
                );
                round_rect_w(
                    hdc,
                    pill,
                    self.colors.btn_hover,
                    self.colors.btn_hover,
                    self.s(4),
                    1,
                );
            }
            draw_text_center(hdc, title, r, font, color);
            if selected {
                // 指示条 = 文字宽（`geo.tabs` 本身就是实测的文字矩形），
                // 所以它必然与文字左右齐平、视觉居中，不会再拖出半个身位。
                let bar = Self::rect(r.left, tab_h - self.s(3), r.right - r.left, self.s(3));
                fill_rect(hdc, bar, self.colors.accent);
            }
        }
    }

    /// 页标题 + 分组标题（三页共用的小件）。
    fn draw_page_head(&self, hdc: HDC, title: &str, section: &str) {
        let m = self.s(20);
        let tab_h = self.s(44);
        draw_text(
            hdc,
            title,
            Self::rect(m, tab_h + self.s(10), self.s(400), self.s(30)),
            self.fonts.title,
            self.colors.text1,
        );
        draw_text(
            hdc,
            section,
            Self::rect(m, tab_h + self.s(48), self.s(400), self.s(18)),
            self.fonts.section,
            self.colors.text2,
        );
    }

    /// 通用页：卡片 + 四行 + 三段选择器 + 数字框描边 + 按钮行短语。
    fn draw_general(&self, hdc: HDC) {
        self.draw_page_head(hdc, "通用", "候选与提示");
        let card = self.geo.gen_card;
        round_rect_w(
            hdc,
            card,
            self.colors.card,
            self.colors.card_border,
            self.s(8),
            self.s(1),
        );

        // 行间分隔线（左端缩进 16，不顶到圆角）
        for i in 0..3 {
            let r = self.geo.gen_rows[i];
            let h = self.s(1);
            fill_rect(
                hdc,
                Self::rect(
                    card.left + self.s(16),
                    r.bottom - h,
                    card.right - card.left - self.s(32),
                    h,
                ),
                self.colors.separator,
            );
        }

        // 行 0：标题 + 描述（描述随选中段变）
        let (title, desc) = ("码提示时机", Self::hint_desc(self.hint_sel));
        draw_row_text(hdc, title, desc, self.geo.gen_rows[0], self, false);

        // 行 1/2：标题 + 描述（校验失败时红色）
        let rows = [
            (
                "学习增益 g",
                &self.gain_state,
                "越大越容易把常用的词往前提；0 表示不学习",
            ),
            ("半衰期 H", &self.half_state, "多少天不用，学习分减半"),
        ];
        for (i, (title, state, ok_desc)) in rows.iter().enumerate() {
            let desc = match state {
                Ok(_) => (*ok_desc).to_string(),
                Err(e) => format!("输入有误：{e}"),
            };
            let bad = state.is_err();
            draw_row_text(hdc, title, &desc, self.geo.gen_rows[i + 1], self, bad);
        }

        // 三段选择器：底槽 + 白色选中块 + 文字
        let seg = self.geo.seg;
        round_rect_w(
            hdc,
            seg,
            self.colors.seg_track,
            self.colors.seg_track,
            self.s(6),
            self.s(1),
        );
        let seg_w = (seg.right - seg.left) / 3;
        let sel = Self::rect(
            seg.left + seg_w * self.hint_sel as i32 + self.s(2),
            seg.top + self.s(2),
            seg_w - self.s(4),
            seg.bottom - seg.top - self.s(4),
        );
        round_rect_w(
            hdc,
            sel,
            self.colors.card,
            self.colors.separator,
            self.s(4),
            self.s(1),
        );
        for (i, name) in HINT_SEGMENTS.iter().enumerate() {
            let r = Self::rect(
                seg.left + seg_w * i as i32,
                seg.top,
                seg_w,
                seg.bottom - seg.top,
            );
            let color = if i == self.hint_sel {
                self.colors.text1
            } else if i as i32 == self.seg_hot {
                self.colors.text2
            } else {
                self.colors.text3
            };
            // 每段文字在**自己那一格**里居中。左对齐时「总是」贴着白块左缘、
            // 「首次」贴着分隔线，三段各自偏左，整排看着像没排进去。
            draw_text_center(hdc, name, r, self.fonts.caption, color);
        }

        // 行 1/2 的数字框：1px 描边（焦点时强调色）+ 左侧默认值提示
        for (id, box_r, hint_r, hint) in [
            (
                ID_E_GAIN,
                self.geo.gain_box,
                self.geo.gain_hint,
                "默认 0.35",
            ),
            (ID_E_HALF, self.geo.half_box, self.geo.half_hint, "7 – 365"),
        ] {
            let border = if self.focus_id == id {
                self.colors.accent
            } else {
                self.colors.input_border
            };
            round_rect_w(hdc, box_r, self.colors.card, border, self.s(4), self.s(1));
            draw_text_align(
                hdc,
                hint,
                hint_r,
                self.fonts.caption,
                self.colors.text3,
                true,
            );
        }

        // 行 3：配置文件路径 + 说明（两行，与上面三行同一节奏）
        //
        // 原来把「路径 · config_version · 警告」拼成一句塞在一行里：路径本身就有
        // 五六十个字符，后半截的版本和警告被挤到右边缘，还跟「打开位置」抢地方。
        // 现在路径独占第一行（中间省略，保住 `config.toml`），其余信息进第二行。
        let cfg = self.geo.gen_rows[3];
        let cfg_x = card.left + self.s(16);
        // 右边界让给「打开位置」按钮：80 宽 + 右侧内衬 16 + 12 的间隔
        let cfg_right = card.right - self.s(16 + 80 + 12);
        draw_text_path(
            hdc,
            &self.cfg_path,
            Self::rect(cfg_x, cfg.top + self.s(11), cfg_right - cfg_x, self.s(18)),
            self.fonts.caption,
            self.colors.text2,
        );
        draw_text(
            hdc,
            &self.cfg_desc,
            Self::rect(cfg_x, cfg.top + self.s(33), cfg_right - cfg_x, self.s(18)),
            self.fonts.caption,
            self.colors.text3,
        );

        // 按钮行左边的短语
        let tip = "改完点「应用」，1 秒内生效，无需重启";
        draw_text(
            hdc,
            tip,
            Self::rect(
                self.geo.gen_btn_row.left,
                self.geo.gen_btn_row.top,
                self.geo.gen_btn_row.right - self.geo.gen_btn_row.left - self.s(220),
                self.geo.gen_btn_row.bottom - self.geo.gen_btn_row.top,
            ),
            self.fonts.caption,
            self.colors.text2,
        );
    }

    /// 词组页：列表卡 + 面板卡（label 自绘）。
    fn draw_words(&self, hdc: HDC) {
        self.draw_page_head(hdc, "词组", "自定义词");
        round_rect_w(
            hdc,
            self.geo.words_card,
            self.colors.card,
            self.colors.card_border,
            self.s(8),
            self.s(1),
        );
        round_rect_w(
            hdc,
            self.geo.panel_card,
            self.colors.card,
            self.colors.card_border,
            self.s(8),
            self.s(1),
        );
        for (i, name) in ["词", "拼音", "码", "权重"].iter().enumerate() {
            draw_text_align(
                hdc,
                name,
                self.geo.panel_labels[i],
                self.fonts.caption,
                self.colors.text2,
                true,
            );
        }
        // 四个输入框是**无边框原生 Edit**，描边只能画在卡片上：Edit 本身是白底，
        // 白底压在白卡上等于没长眼睛，不画框就成了一片空白（截图里「输入框不见了」）。
        // 描边画在 box 外沿，Edit 已经内缩 s(1) 让开这条线。
        for (i, id) in [ID_E_WORD, ID_E_PINYIN, ID_E_CODE, ID_E_WEIGHT]
            .into_iter()
            .enumerate()
        {
            let border = if self.focus_id == id {
                self.colors.accent
            } else {
                self.colors.input_border
            };
            round_rect_w(
                hdc,
                self.geo.panel_boxes[i],
                self.colors.card,
                border,
                self.s(4),
                self.s(1),
            );
        }
    }

    /// 学习数据页：搜索框描边 + 列表卡。
    fn draw_learn(&self, hdc: HDC) {
        self.draw_page_head(hdc, "学习数据", "按学习分排序");
        let border = if self.focus_id == ID_E_SEARCH {
            self.colors.accent
        } else {
            self.colors.input_border
        };
        round_rect_w(
            hdc,
            self.geo.learn_search_box,
            self.colors.card,
            border,
            self.s(4),
            self.s(1),
        );
        // 「搜索」是个标签，不是按钮，但它只两个字，左对齐会在 32dp 的矩形里
        // 靠左半个字宽——跟右边输入框之间显得忽远忽近。居中放，与框的距离就恒定。
        draw_text_center(
            hdc,
            "搜索",
            Self::rect(
                self.geo.learn_search_box.left - self.s(36),
                self.geo.learn_search_box.top,
                self.s(32),
                self.geo.learn_search_box.bottom - self.geo.learn_search_box.top,
            ),
            self.fonts.caption,
            self.colors.text2,
        );
        round_rect_w(
            hdc,
            self.geo.learn_card,
            self.colors.card,
            self.colors.card_border,
            self.s(8),
            self.s(1),
        );
    }

    /// owner-draw 按钮统一入口。样式分三档：
    ///
    /// | 档 | id | 观感 |
    /// |---|---|---|
    /// | primary | 应用、保存 | 实心强调色，hover 提亮、按下压暗 |
    /// | secondary | 其余 | 白底细描边，hover 浅灰底 |
    /// | ghost | 打开位置 | 无底无框的强调色文字（链接感） |
    fn draw_button(&self, dis: &DRAWITEMSTRUCT) {
        let id = dis.CtlID as usize;
        let (fill, border, text_color) = if dis.itemState.0 & ODS_DISABLED != 0 {
            // 灰按钮**也要留住轮廓**：底色 #F0F0F0 与页面底 #F3F3F3 只差 3 级，
            // 不给描边时整排按钮会凭空消失，用户看不出「这里本来有按钮、只是不能点」。
            match button_kind(id) {
                ButtonKind::Primary => (
                    self.colors.btn_disabled_bg,
                    self.colors.card_border,
                    self.colors.btn_disabled_text,
                ),
                ButtonKind::Secondary => (
                    self.colors.btn_disabled_bg,
                    self.colors.card_border,
                    self.colors.btn_disabled_text,
                ),
                ButtonKind::Ghost => (
                    self.colors.bg,
                    self.colors.bg,
                    self.colors.btn_disabled_text,
                ),
            }
        } else if dis.itemState.0 & ODS_SELECTED != 0 {
            match button_kind(id) {
                ButtonKind::Primary => (
                    self.colors.accent_down,
                    self.colors.accent_down,
                    self.colors.accent_text,
                ),
                ButtonKind::Secondary => {
                    (self.colors.seg_track, self.colors.text3, self.colors.text1)
                }
                ButtonKind::Ghost => (self.colors.card, self.colors.card, self.colors.accent),
            }
        } else if dis.itemState.0 & ODS_HOTLIGHT != 0 {
            match button_kind(id) {
                ButtonKind::Primary => (
                    self.colors.accent_hover,
                    self.colors.accent_hover,
                    self.colors.accent_text,
                ),
                ButtonKind::Secondary => {
                    (self.colors.btn_hover, self.colors.text3, self.colors.text1)
                }
                ButtonKind::Ghost => (self.colors.card, self.colors.card, self.colors.accent),
            }
        } else {
            match button_kind(id) {
                ButtonKind::Primary => (
                    self.colors.accent,
                    self.colors.accent,
                    self.colors.accent_text,
                ),
                ButtonKind::Secondary => (
                    self.colors.card,
                    self.colors.input_border,
                    self.colors.text1,
                ),
                ButtonKind::Ghost => (self.colors.card, self.colors.card, self.colors.accent),
            }
        };
        let radius = match button_kind(id) {
            ButtonKind::Ghost => 0,
            _ => self.s(4),
        };
        if radius > 0 {
            round_rect_w(dis.hDC, dis.rcItem, fill, border, radius, self.s(1));
        } else {
            // Ghost 也要**先把底擦掉**再写字。不擦的话按钮矩形里留着上一帧的
            // 内容（或控件默认灰底），看起来像「文字背后贴了一块灰补丁」。
            fill_rect(dis.hDC, dis.rcItem, fill);
        }
        // 文字：用按钮自己的标题（SetWindowTextW 改过也能跟）
        let label = get_text(dis.hwndItem);
        let font = if button_kind(id) == ButtonKind::Ghost {
            self.fonts.caption
        } else {
            self.fonts.body
        };
        draw_text_center(dis.hDC, &label, dis.rcItem, font, text_color);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
}

fn button_kind(id: usize) -> ButtonKind {
    match id {
        ID_B_APPLY | ID_B_SAVE => ButtonKind::Primary,
        ID_B_OPEN_CFG => ButtonKind::Ghost,
        _ => ButtonKind::Secondary,
    }
}

/// 纯色矩形填充。
fn fill_rect(hdc: HDC, r: RECT, c: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(c);
        let _ = FillRect(hdc, &r, brush);
        let _ = DeleteObject(brush.into());
    }
}

/// 圆角矩形：填充 + 1px 描边。GDI 的圆角是硬边，8px 下肉眼可接受；
/// 引 GDI+ 只为抗锯齿一个收益，不值当（单文件、零多余依赖是本程序的底线）。
/// 带线宽的版本：高 DPI 下调用方传 `s(1)`，否则描边只有半个逻辑像素
/// （系统 DPI 2.0 时 1 物理像素 = 0.5 逻辑像素，卡片框会淡得像没有）。
fn round_rect_w(hdc: HDC, r: RECT, fill: COLORREF, border: COLORREF, radius: i32, pen_w: i32) {
    unsafe {
        let brush = CreateSolidBrush(fill);
        let pen = CreatePen(PS_SOLID, pen_w, border);
        let (ob, op) = (
            SelectObject(hdc, brush.into()),
            SelectObject(hdc, pen.into()),
        );
        // RoundRect 的椭圆宽高是直径语义
        let _ = RoundRect(
            hdc,
            r.left,
            r.top,
            r.right,
            r.bottom,
            radius * 2,
            radius * 2,
        );
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        let _ = DeleteObject(brush.into());
        let _ = DeleteObject(pen.into());
    }
}

/// 量一段文字在指定字体下的像素宽度。
///
/// 页签矩形、下划线长度都得靠它。**不能按「每个字 15dp」估**：中文 14dp 实际
/// 14px 宽，英文数字只有 7–8px，估出来的矩形必然一头紧一头松；而文字是按矩形
/// 左端画的，于是右边空出一大块，下划线跟着矩形走，看着就像「文字在条子外面」。
fn text_width(hdc: HDC, font: HFONT, s: &str) -> i32 {
    if hdc.0.is_null() || s.is_empty() {
        return 0;
    }
    let wide: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        let old = SelectObject(hdc, font.into());
        let mut size = SIZE::default();
        let ok = GetTextExtentPoint32W(hdc, &wide, &mut size).as_bool();
        SelectObject(hdc, old);
        if ok {
            size.cx
        } else {
            0
        }
    }
}

/// 单行文字的唯一实现：`align` 是 `DT_LEFT` / `DT_CENTER` / `DT_RIGHT` 之一，
/// `ellipsis` 是超宽时的省略方式（`DT_END_ELLIPSIS` 尾巴省略 / `DT_PATH_ELLIPSIS`
/// 中间省略，给路径用）。
///
/// **水平位置自己算，不用 `DrawTextW` 的对齐位**。两个坑叠在一起：
/// ① `DT_END_ELLIPSIS` 会在右侧预留省略号的宽度（哪怕文字远没超宽），居中/
///    右对齐因此整体偏左 7–8px；② 这个偏移量没有任何文档，随字体和字号变。
/// 而我们已经用 `text_width` 量出真实宽度了，直接算左端点最稳——量多少放多少，
/// 居中就是几何意义上的居中（页签下划线、按钮文字、三段选择器都靠这一步对齐）。
fn draw_text_flags(
    hdc: HDC,
    s: &str,
    r: RECT,
    font: HFONT,
    color: COLORREF,
    align: u32,
    ellipsis: u32,
) {
    let mut wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    let avail = r.right - r.left;
    let w = text_width(hdc, font, s);
    let extra = if w > avail { ellipsis } else { 0 };
    let left = match align {
        DT_CENTER => r.left + (avail - w).max(0) / 2,
        DT_RIGHT => (r.right - w).max(r.left),
        _ => r.left,
    };
    unsafe {
        let old = SelectObject(hdc, font.into());
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, color);
        let mut rc = RECT {
            left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        };
        let _ = DrawTextW(
            hdc,
            &mut wide,
            &mut rc,
            windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | extra,
            ),
        );
        SelectObject(hdc, old);
    }
}

/// 单行文字，可选右对齐（数字框左侧那两句「默认 0.35」用它贴框）。
fn draw_text_align(hdc: HDC, s: &str, r: RECT, font: HFONT, color: COLORREF, right: bool) {
    draw_text_flags(
        hdc,
        s,
        r,
        font,
        color,
        if right { DT_RIGHT } else { DT_LEFT },
        DT_END_ELLIPSIS,
    );
}

/// 单行文字（左对齐，垂直居中，超宽省略）。
fn draw_text(hdc: HDC, s: &str, r: RECT, font: HFONT, color: COLORREF) {
    draw_text_flags(hdc, s, r, font, color, DT_LEFT, DT_END_ELLIPSIS);
}

/// 按钮文字：水平居中。左对齐放在按钮里，一个字宽的「搜」会贴着左边框，
/// 看着像没排进去。
fn draw_text_center(hdc: HDC, s: &str, r: RECT, font: HFONT, color: COLORREF) {
    draw_text_flags(hdc, s, r, font, color, DT_CENTER, DT_END_ELLIPSIS);
}

/// 路径专用：放不下时**从中间省略**，保住尾部的文件名。
///
/// 用尾巴省略会把 `config.toml` 整个换成「…」——路径长了以后，这行最关键的信息
/// 恰好第一个没了。`DT_PATH_ELLIPSIS` 保留盘符和文件名，中间打省略号。
fn draw_text_path(hdc: HDC, s: &str, r: RECT, font: HFONT, color: COLORREF) {
    draw_text_flags(hdc, s, r, font, color, DT_LEFT, DT_PATH_ELLIPSIS);
}

/// 通用页一行：标题（左上）+ 描述（标题下方，可红）。
fn draw_row_text(hdc: HDC, title: &str, desc: &str, row: RECT, app: &App, bad: bool) {
    let x = row.left + app.s(16);
    let title_r = RECT {
        left: x,
        top: row.top + app.s(10),
        right: row.right - app.s(16),
        bottom: row.top + app.s(32),
    };
    let desc_r = RECT {
        left: x,
        top: row.top + app.s(33),
        right: row.right - app.s(16),
        bottom: row.top + app.s(52),
    };
    draw_text(hdc, title, title_r, app.fonts.body, app.colors.text1);
    draw_text(
        hdc,
        desc,
        desc_r,
        app.fonts.caption,
        if bad {
            app.colors.error
        } else {
            app.colors.text2
        },
    );
}

unsafe fn app_of(hwnd: HWND) -> Option<&'static mut App> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;
    unsafe { ptr.as_mut() }
}

/// 在列表里选中第 `idx` 行（不管界面状态，也不填面板）。
///
/// 走的是 ListView 自己的 `LVM_SETITEMSTATE`，所以会照常给父窗口发
/// `LVN_ITEMCHANGED`——自测因此可以走**和真实点击同一条消息链**。
fn select_item(list: HWND, idx: usize) {
    let mut item = LVITEMW {
        state: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(
            LVIS_SELECTED.0 | LVIS_FOCUSED.0,
        ),
        stateMask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(
            LVIS_SELECTED.0 | LVIS_FOCUSED.0,
        ),
        ..Default::default()
    };
    unsafe {
        send(
            list,
            LVM_SETITEMSTATE,
            WPARAM(idx),
            LPARAM(&mut item as *mut _ as isize),
        );
        send(list, LVM_ENSUREVISIBLE, WPARAM(idx), LPARAM(0));
    }
}

unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_CHAR if wparam.0 == 0x0d => {
            let parent = unsafe { GetParent(hwnd) }.unwrap_or_default();
            send(parent, WM_COMMAND, WPARAM(ID_B_SAVE), LPARAM(0));
            return LRESULT(0);
        }
        WM_KEYDOWN if wparam.0 == 0x1b => {
            let parent = unsafe { GetParent(hwnd) }.unwrap_or_default();
            send(parent, WM_COMMAND, WPARAM(ID_B_CLEAR), LPARAM(0));
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn enter_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    if msg == WM_CHAR && wparam.0 == 0x0d {
        let parent = unsafe { GetParent(hwnd) }.unwrap_or_default();
        send(parent, WM_COMMAND, WPARAM(data), LPARAM(0));
        return LRESULT(0);
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// 发一条窗口消息。
///
/// `windows` 0.62 把 `SendMessageW` 的两参数改成了 `Option<WPARAM>`/`Option<LPARAM>`，
/// 每个调用点都要 `Some(...)` 会让代码被括号埋掉，这里统一一次。
///
/// 标 `unsafe` 而不是包一层安全函数：调用点本来就都在 Win32 的 `unsafe` 区间里，
/// 再包一层只会让编译器报「多余的 unsafe」，反而要到处去删括号。
unsafe fn send(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { SendMessageW(hwnd, msg, Some(wparam), Some(lparam)) }
}

fn enable_window(hwnd: HWND, enable: bool) {
    unsafe {
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(hwnd, enable);
    }
}

fn is_enabled(hwnd: HWND) -> bool {
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(hwnd).as_bool() }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn put_utf16(buf: &mut [u16], s: &str) {
    let it = s.encode_utf16();
    for (i, c) in it.enumerate() {
        if i + 1 >= buf.len() {
            break;
        }
        buf[i] = c;
    }
    // 末尾留一个 0：`buf` 已用 0 初始化
}

fn child(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE,
    id: usize,
    font: HFONT,
) -> windows::core::Result<HWND> {
    let text = wide(text);
    let hwnd = unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            class,
            PCWSTR(text.as_ptr()),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                id as isize as *mut core::ffi::c_void,
            )),
            None,
            None,
        )
    }?;
    unsafe {
        send(hwnd, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
    }
    Ok(hwnd)
}

fn set_text(hwnd: HWND, s: &str) {
    let t = wide(s);
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(t.as_ptr()));
    }
}

fn get_text(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn set_focus(hwnd: HWND) {
    unsafe {
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(hwnd));
    }
}

fn message(owner: HWND, title: &str, text: &str) {
    let t = wide(title);
    let m = wide(text);
    unsafe {
        MessageBoxW(
            Some(owner),
            PCWSTR(m.as_ptr()),
            PCWSTR(t.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

/// 是/否确认。用在**不能撤销**的地方（清学习分），其余一律直接做——多一次点击
/// 也会训练出「不看内容就按是」的习惯。
fn confirm(owner: HWND, title: &str, text: &str) -> bool {
    let t = wide(title);
    let m = wide(text);
    let r = unsafe {
        MessageBoxW(
            Some(owner),
            PCWSTR(m.as_ptr()),
            PCWSTR(t.as_ptr()),
            MB_YESNO | MB_ICONWARNING,
        )
    };
    r == IDYES
}

fn pick_file(owner: HWND) -> Option<PathBuf> {
    let filter: Vec<u16> = "词表 (*.tsv;*.txt)\0*.tsv;*.txt\0所有文件 (*.*)\0*.*\0\0"
        .encode_utf16()
        .collect();
    let mut buf = vec![0u16; 512];
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: 1,
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrTitle: w!("导入自定义词组"),
        ..Default::default()
    };
    let ok = unsafe { GetOpenFileNameW(&mut ofn) };
    if !ok.as_bool() {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..end])))
}

/// 面板 → 保存 → 列表这条链的自测（去掉了「新增」按钮之后的约定）：
///
/// 1. 空面板时「保存」「清空」「删除」都是灰的——不给假按钮；
/// 2. 填了词，「保存」「清空」亮，「删除」仍灰（还没选中任何行）；
/// 3. 点「保存」：文件里真出现这个词，**面板回到新词态**（否则下一次保存会被
///    当成改名——F46 就是这么连加三个只剩一条的）；
/// 4. 再填一个词再保存：文件里必须**两条都在**；
/// 5. 从列表里点一行、改词名、保存：旧词消失、新词出现、**其它词不受影响**。
///
/// 全部走真实控件消息（`WM_SETTEXT` / `BM_CLICK` 由按钮自己发 `BN_CLICKED` /
/// `LVM_SETITEMSTATE` 由列表自己发 `LVN_ITEMCHANGED`），断言落点是**磁盘上的文件**，
/// 不是界面上的字符串——界面骗人的事刚发生过（F44）。
fn check_panel_flow(app: &App, path: &std::path::Path) -> Result<(), String> {
    let click = |b: HWND| unsafe {
        send(b, 0x00F5, WPARAM(0), LPARAM(0)); // BM_CLICK
    };
    let state = |what: &str| -> Result<(), String> {
        let got = (
            is_enabled(app.b_save),
            is_enabled(app.b_clear),
            is_enabled(app.b_del),
        );
        let want = match what {
            "空面板" | "保存后" | "清空后" => (false, false, false),
            "填了词" => (true, true, false),
            _ => unreachable!(),
        };
        if got != want {
            return Err(format!(
                "{what}：保存={} 清空={} 删除={}，期望 {want:?}",
                got.0, got.1, got.2
            ));
        }
        Ok(())
    };
    // 落盘的内容按「词」取出来比对：注释行、空行、坏行都不算
    let words = || -> Result<Vec<String>, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("回读词库失败：{e}"))?;
        Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.split('\t').next().map(str::to_string))
            .collect())
    };

    state("空面板")?;

    set_text(app.e_word, "微信");
    state("填了词")?;
    click(app.b_save);
    if !words()?.iter().any(|w| w == "微信") {
        return Err(format!("点保存后文件里没有「微信」：{:?}", words()?));
    }
    if app.editing.is_some() || !get_text(app.e_word).is_empty() {
        return Err("保存后面板没回到新词态——下一个词会被当成改名，把这条顶掉（F46）".into());
    }
    state("保存后")?;

    // 连加第二个词：两条都得在。这一步就是 F46 的回归测试。
    set_text(app.e_word, "虎纠");
    click(app.b_save);
    let got = words()?;
    if !(got.iter().any(|w| w == "微信") && got.iter().any(|w| w == "虎纠")) {
        return Err(format!("连加两个词后应当两条都在，实际是 {got:?}"));
    }

    // 改名：从列表点一行（真发 LVN_ITEMCHANGED），把「微信」改成「威信」
    let Some(idx) = app.rows.iter().position(|r| r.raw.word == "微信") else {
        return Err("列表里找不到刚加的「微信」".into());
    };
    select_item(app.list, idx);
    drain_messages();
    if get_text(app.e_word) != "微信" {
        return Err(format!(
            "点中列表行后面板没跟着填，实际是 {:?}",
            get_text(app.e_word)
        ));
    }
    set_text(app.e_word, "威信");
    click(app.b_save);
    let got = words()?;
    if got.iter().any(|w| w == "微信") {
        return Err(format!("改名后旧词「微信」还在文件里：{got:?}"));
    }
    if !(got.iter().any(|w| w == "威信") && got.iter().any(|w| w == "虎纠")) {
        return Err(format!("改名后新词或其它词丢了：{got:?}"));
    }

    set_text(app.e_word, "多打的草稿");
    click(app.b_clear);
    if !get_text(app.e_word).is_empty() || app.editing.is_some() {
        return Err("点清空后面板/选中状态没回到新词态".into());
    }
    state("清空后")?;
    Ok(())
}

/// 建窗（不显示、不进消息循环）。
fn create_window(app: &App) -> Result<HWND, String> {
    unsafe {
        let hinstance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        // 类名从 `heinrich-tip-id` 取，不在这里写第二份：DLL 的
        // `ITfFnConfigure::Show` 用同一个常量 `FindWindowW` 找「已经开着的那个」，
        // 写两份的失败表现是「连点两次选项开出两个窗口」，没有任何报错。
        let class: Vec<u16> = heinrich_tip_id::SETTINGS_CLASS
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: HINSTANCE(hinstance.0),
            hCursor: cursor,
            lpszClassName: PCWSTR(class.as_ptr()),
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(6usize as *mut core::ffi::c_void),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err("注册窗口类失败".into());
        }
        let dpi = app.dpi;
        let w = (900.0 * dpi) as i32;
        let h = (600.0 * dpi) as i32;
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            w!("亨利五笔 · 设置"),
            // WS_CLIPCHILDREN：父窗口整窗铺底时会**跳过子控件占的矩形**。
            // 不设的话切页重绘会把 Edit / 列表白底刷成页面底色，控件看着「消失」，
            // 鼠标划过去才回来（子控件只在需要时自己重画）。
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            w,
            h,
            None,
            None,
            Some(HINSTANCE(hinstance.0)),
            Some(app as *const App as *const core::ffi::c_void),
        )
        .map_err(|e| format!("建窗失败：{e}"))
    }
}

/// 建窗、跑消息循环。返回进程退出码。
/// 已经有一个设置窗口就还原 + 前置，返回它的 pid；没有则 `None`。
///
/// **为什么需要**：这个 exe 没有单实例锁，而现在有两个入口指向它（开始菜单快捷方式、
/// 系统设置的「选项」）。点两次就开出两个窗口，各自持一份**内存里的**词表快照：
/// 在 A 里加的词，B 的列表里没有；在 B 里再保存一次，A 刚写的改动就按 B 的旧快照
/// 被算成「已删除」。所以第二个实例只负责把第一个叫到前台，自己立刻退出。
///
/// 还原用**异步** `ShowWindowAsync`：同步 `ShowWindow` 碰上无响应的目标会把调用方
/// 一起卡住（目标可能正卡在写 `words.tsv` 上）。
pub fn focus_existing() -> Option<u32> {
    unsafe {
        let class: Vec<u16> = heinrich_tip_id::SETTINGS_CLASS
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let hwnd = match FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) {
            Ok(h) if !h.0.is_null() => h,
            _ => return None,
        };
        // 最小化时 `SetForegroundWindow` 只改前台身份、屏幕上看不见；
        // 判「用户能看见」必须是「还原 + 前置」这一对。
        let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        Some(pid)
    }
}

pub fn run(app: Box<App>) -> Result<i32, String> {
    let hwnd = create_window(&app)?;
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(0)
}

/// 窗口层的自测（`--selftest-ui`）：不靠肉眼、不靠截图，直接断言 **z 序与几何**。
///
/// 这两样正是 F44 的病根——Tab 盖住页控件时，界面看起来一切正常，所有点击却都
/// 落空；只发 `WM_COMMAND` 的脚本测不出它，只有查 z 序才测得出。
pub fn selftest_ui(data: Option<PathBuf>) -> Result<(), String> {
    let dir = std::env::temp_dir().join("heinrich-settings-uitest");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建临时目录失败：{e}"))?;
    let path = dir.join("words.tsv");
    std::fs::write(&path, "好吃\t\t\n").map_err(|e| format!("造词库失败：{e}"))?;

    // 通用页与学习页也要有自己的临时文件：自测改用户的真配置是不可接受的副作用
    let cfg_path = dir.join("config.toml");
    std::fs::write(
        &cfg_path,
        "# 自测造的文件\nconfig_version = 1\nhint.mode = \"mixed\"\n[ranking]\nhalf_life_days = 45\n",
    )
    .map_err(|e| format!("造配置失败：{e}"))?;
    let db_path = dir.join("learn.db");
    {
        use rusqlite::{params, Connection};
        let conn = Connection::open(&db_path).map_err(|e| format!("造学习库失败：{e}"))?;
        conn.execute_batch(
            "CREATE TABLE usage (key TEXT PRIMARY KEY, n INTEGER NOT NULL DEFAULT 0,
                                 last_used_at INTEGER NOT NULL DEFAULT 0,
                                 tier INTEGER NOT NULL DEFAULT 0);",
        )
        .map_err(|e| format!("建表失败：{e}"))?;
        let now = crate::learn::now_unix();
        conn.execute(
            "INSERT INTO usage VALUES ('好吃', 4, ?1, 0), ('的', 7, ?1, 1), ('嗯', 1, ?1, 2)",
            params![now],
        )
        .map_err(|e| format!("写学习记录失败：{e}"))?;
    }

    let mut app = Box::new(App::new(
        data,
        Some(path.clone()),
        Some(cfg_path.clone()),
        Some(db_path.clone()),
    )?);
    let hwnd = create_window(&app)?;
    // 不抢焦点地显示一下：真实路径里窗口是可见的，z 序表现才一致
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    drain_messages();

    let result = (|| -> Result<(), String> {
        // 三页各查一次：z 序（Tab 在底）+ 命中（点得到）。页是显隐切换的，
        // 隐藏页的控件命中测试必然失败，只能切过去再查。
        check_z_order(hwnd, PAGE_GENERAL)?;
        check_general_flow(&app, &cfg_path)?;

        switch_page(&mut app, PAGE_WORDS);
        check_z_order(hwnd, PAGE_WORDS)?;
        check_panel_flow(&app, &path)?;

        switch_page(&mut app, PAGE_LEARN);
        check_z_order(hwnd, PAGE_LEARN)?;
        check_learn_page(&app)?;

        switch_page(&mut app, PAGE_GENERAL);
        check_z_order(hwnd, PAGE_GENERAL)
    })();

    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// 自测用：像用户点页签那样切页。页签现在是画出来的，没有「发消息给 Tab
/// 控件」这回事；但「切页 = 显隐 + 重画」这半段仍然走真实路径（`show_page`）。
fn switch_page(app: &mut App, page: usize) {
    app.switch_to(page);
    drain_messages();
}

/// 通用页：合法/非法输入的门、写盘后注释还在、恢复默认真的删行。
fn check_general_flow(app: &App, cfg: &std::path::Path) -> Result<(), String> {
    // 1. 打开时按文件里的值显示：hint.mode = "mixed" → 第 0 段
    if app.hint_sel != 0 {
        return Err(format!(
            "三段选择器应选中 mixed（0），实际是 {}",
            app.hint_sel
        ));
    }
    if get_text(app.e_half) != "45" {
        return Err(format!(
            "半衰期应显示文件里的 45，实际是 {:?}",
            get_text(app.e_half)
        ));
    }

    // 2. 非法输入：按钮必须灰掉，且原因写在该行右边
    set_text(app.e_gain, "99");
    drain_messages();
    if is_enabled(app.b_apply) {
        return Err("学习增益输 99（超出 0–10）时「应用」应当灰掉".into());
    }
    if !matches!(&app.gain_state, Err(e) if e.contains("0") || e.contains("之间") || e.contains("10"))
    {
        return Err(format!("非法输入没存下原因，实际是 {:?}", app.gain_state));
    }

    // 3. 合法了才允许写：写盘后注释与顺序都得在，值要能被 Config::parse 读回
    set_text(app.e_gain, "0.6");
    set_text(app.e_half, "30");
    drain_messages();
    if !is_enabled(app.b_apply) {
        return Err("合法输入时「应用」应当可点".into());
    }
    unsafe {
        send(app.b_apply, 0x00F5, WPARAM(0), LPARAM(0)); // BM_CLICK
    }
    drain_messages();
    let text = std::fs::read_to_string(cfg).map_err(|e| format!("回读配置失败：{e}"))?;
    if !text.contains("# 自测造的文件") {
        return Err(format!("写盘把注释冲掉了：\n{text}"));
    }
    let parsed = heinrich_scheme::Config::parse(&text);
    if parsed.ranking_gain() != Some(0.6) || parsed.ranking_half_life_days() != Some(30.0) {
        return Err(format!(
            "写进去的值读不回来：g={:?} H={:?}\n{text}",
            parsed.ranking_gain(),
            parsed.ranking_half_life_days()
        ));
    }
    if text.matches("half_life_days").count() != 1 {
        return Err(format!("同一个键写了两次：\n{text}"));
    }

    // 4. 恢复默认 = 删掉这三行（不是写回数值）
    unsafe {
        send(app.b_default, 0x00F5, WPARAM(0), LPARAM(0));
    }
    drain_messages();
    let text = std::fs::read_to_string(cfg).map_err(|e| format!("回读配置失败：{e}"))?;
    for key in ["hint.mode", "learning_gain", "half_life_days"] {
        if text.contains(key) {
            return Err(format!("恢复默认后 {key} 还在文件里：\n{text}"));
        }
    }
    if get_text(app.e_gain) != "0.35" || get_text(app.e_half) != "45" {
        return Err(format!(
            "恢复默认后界面应显示内置默认值，实际 g={:?} H={:?}",
            get_text(app.e_gain),
            get_text(app.e_half)
        ));
    }
    Ok(())
}

/// 学习页：行数、排序、档位、按词搜索、「选中才亮清除」。
fn check_learn_page(app: &App) -> Result<(), String> {
    let count = unsafe { send(app.learn_list, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0)) }.0;
    if count != 3 {
        return Err(format!("学习页应列出 3 条用过的词，实际 {count} 条"));
    }
    // 分最高的在最前：n=7（置顶）> n=4 > n=1（0 分）
    let first = cell_text(app.learn_list, 0, 0);
    if first != "的" {
        return Err(format!("首行应是次数最多的「的」，实际是 {first:?}"));
    }
    if cell_text(app.learn_list, 0, 4) != "置顶" {
        return Err("「的」的档位列应显示「置顶」".into());
    }
    if is_enabled(app.b_forget) {
        return Err("没选中任何行时「清除」应当是灰的".into());
    }

    // 搜索：过滤到 1 条
    set_text(app.e_search, "好");
    unsafe {
        send(app.b_search, 0x00F5, WPARAM(0), LPARAM(0));
    }
    drain_messages();
    let count = unsafe { send(app.learn_list, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0)) }.0;
    if count != 1 || cell_text(app.learn_list, 0, 0) != "好吃" {
        return Err(format!("按「好」搜索应只剩 1 条「好吃」，实际 {count} 条"));
    }
    set_text(app.e_search, "");
    unsafe {
        send(app.b_search, 0x00F5, WPARAM(0), LPARAM(0));
    }
    drain_messages();

    // 选中一行 → 「清除」才亮（这里不点它：它会弹确认框，自测没人点「是」）
    select_item(app.learn_list, 0);
    drain_messages();
    if !is_enabled(app.b_forget) {
        return Err("选中一行后「清除学习分」应当可点".into());
    }
    Ok(())
}

/// 读 ListView 某一格的文字（自测断言用；`LVM_GETITEMTEXTW` 要自己备缓冲）。
fn cell_text(list: HWND, row: usize, col: usize) -> String {
    let mut buf = vec![0u16; 256];
    let mut item = LVITEMW {
        iSubItem: col as i32,
        pszText: PWSTR(buf.as_mut_ptr()),
        cchTextMax: buf.len() as i32,
        ..Default::default()
    };
    const LVM_GETITEMTEXTW: u32 = 0x1073;
    let n = unsafe {
        send(
            list,
            LVM_GETITEMTEXTW,
            WPARAM(row),
            LPARAM(&mut item as *mut _ as isize),
        )
    };
    String::from_utf16_lossy(&buf[..n.0.max(0) as usize])
}

/// 跑掉已经排队的消息（建窗期间的布局/刷新都在 `WM_CREATE` 里同步跑完，这里再兜一遍）。
fn drain_messages() {
    let mut msg = MSG::default();
    while unsafe {
        windows::Win32::UI::WindowsAndMessaging::PeekMessageW(
            &mut msg,
            None,
            0,
            0,
            windows::Win32::UI::WindowsAndMessaging::PM_REMOVE,
        )
    }
    .as_bool()
    {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 取子窗口（**z 序从上到下**，第一个是最上层）。
fn children(hwnd: HWND) -> Vec<HWND> {
    let mut out = Vec::new();
    let mut cur = unsafe { GetWindow(hwnd, GW_CHILD) }.unwrap_or_default();
    while !cur.0.is_null() {
        out.push(cur);
        cur = unsafe { GetWindow(cur, GW_HWNDNEXT) }.unwrap_or_default();
    }
    out
}

fn id_of(hwnd: HWND) -> isize {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_ID) }
}

fn rect_of(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    r
}

/// 两道检查，都针对 F44 那个「点了没反应」：
/// 1. **结构**：Tab 必须是 z 序最底层（它一浮上来就会盖住整个内容区）；
/// 2. **命中**：拿系统自己的 `RealChildWindowFromPoint` 在可交互控件的几个点上
///    做命中测试，命中的必须是它自己——这一条才真正等价于「鼠标点得到」。
///
/// 只查 `page` 这一页的控件：其余页是 `SW_HIDE` 的，隐藏窗口本来就点不到，
/// 拿它们做命中测试只会得到一个假的失败。
///
/// 为什么不用 `WindowFromPoint`：那是在**整个桌面**上找窗口，会被别的窗口、锁屏
/// 界面干扰；`RealChildWindowFromPoint` 只在父窗口内部按 z 序/可见性找子窗口，
/// 与桌面状态无关，可以放进自动化里跑。
fn check_z_order(hwnd: HWND, page: usize) -> Result<(), String> {
    let kids = children(hwnd);
    if kids.is_empty() {
        return Err("窗口里一个子控件都没有".into());
    }

    let clickable: &[usize] = match page {
        PAGE_GENERAL => &[ID_E_GAIN, ID_E_HALF, ID_B_APPLY, ID_B_DEFAULT],
        PAGE_WORDS => &[
            ID_LIST,
            ID_E_WORD,
            ID_E_PINYIN,
            ID_E_CODE,
            ID_E_WEIGHT,
            ID_B_CLEAR,
            ID_B_SAVE,
            ID_B_DEL,
            ID_B_IMPORT,
            ID_B_UNDO,
        ],
        _ => &[ID_E_SEARCH, ID_B_SEARCH, ID_LEARN_LIST, ID_B_FORGET],
    };
    for &id in clickable {
        let Some(h) = kids.iter().copied().find(|&h| id_of(h) == id as isize) else {
            return Err(format!("找不到控件 id {id}"));
        };
        let r = rect_of(h);
        if r.right - r.left <= 0 || r.bottom - r.top <= 0 {
            return Err(format!("控件 id {id} 尺寸是 0（没被布局）"));
        }
        // 中心 + 四个四分点：中心挡住、边缘露出来的情况也要抓
        for (fx, fy) in [(0.5, 0.5), (0.2, 0.5), (0.8, 0.5), (0.5, 0.2), (0.5, 0.8)] {
            let mut pt = POINT {
                x: r.left + ((r.right - r.left) as f32 * fx) as i32,
                y: r.top + ((r.bottom - r.top) as f32 * fy) as i32,
            };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut pt);
            }
            let hit = unsafe { RealChildWindowFromPoint(hwnd, pt) };
            if hit != h {
                return Err(format!(
                    "第 {page} 页的控件 id {id} 在相对位置 ({fx}, {fy}) 处命中的是 id {}（点了不会有反应）",
                    id_of(hit)
                ));
            }
        }
    }
    Ok(())
}

// 保留未使用的导入时给出明确意图：这些常量与函数是给 M4b 的页留的入口。
#[allow(dead_code)]
fn _reserved() {
    let _ = (LVM_GETITEMCOUNT, LVM_DELETEITEM, COLORREF(0), MIN_W, MIN_H);
    let _: Option<WNDPROC> = None;
}
