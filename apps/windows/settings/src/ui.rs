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
use windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen, CreateRoundRectRgn,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC,
    GetTextExtentPoint32W, GetUpdateRect, InvalidateRect, LineTo, MoveToEx, ReleaseDC, RoundRect,
    ScreenToClient, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, ValidateRect, HBRUSH, HDC,
    HFONT, HGDIOBJ, PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OPENFILENAMEW};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Controls::{
    ShowScrollBar, DRAWITEMSTRUCT, LVCFMT_LEFT, LVCFMT_RIGHT, LVCOLUMNW, LVIR_BOUNDS, LVIS_FOCUSED,
    LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_DELETEITEM, LVM_ENSUREVISIBLE,
    LVM_GETITEMCOUNT, LVM_GETITEMRECT, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMW,
    LVNI_SELECTED, LVN_ITEMCHANGED, NMITEMACTIVATE, NMLVCUSTOMDRAW, NM_CLICK,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{
    DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetClientRect,
    GetMessageW, GetParent, GetSystemMetrics, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, KillTimer,
    LoadCursorW, LoadImageW, MessageBoxW, MoveWindow, PostQuitMessage, RealChildWindowFromPoint,
    RegisterClassW, SendMessageW, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowTextW,
    ShowWindow, ShowWindowAsync, TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, GWLP_ID, GWLP_USERDATA, GWL_STYLE, GW_CHILD, GW_HWNDNEXT, ICON_BIG, ICON_SMALL,
    IDC_ARROW, IDYES, IMAGE_ICON, LR_DEFAULTCOLOR, LR_SHARED, MB_ICONINFORMATION, MB_ICONWARNING,
    MB_OK, MB_YESNO, MSG, SB_HORZ, SM_CXICON, SM_CXSMICON, SM_CXVSCROLL, SM_CYICON, SM_CYSMICON,
    SW_HIDE, SW_RESTORE, SW_SHOW, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WINDOW_STYLE, WM_CHAR,
    WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_NOTIFY, WM_PAINT,
    WM_SETFONT, WM_SETICON, WM_SIZE, WM_TIMER, WNDCLASSW, WNDPROC, WS_CHILD, WS_CLIPCHILDREN,
    WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
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

// emoji 页（M4d / D85）。40x 列表与搜索，41x 按钮，42x 说明，43x 输入。
const ID_EMOJI_LIST: usize = 401;
const ID_E_EMOJI_SEARCH: usize = 402;
const ID_BE_SAVE: usize = 410;
const ID_BE_DISABLE: usize = 411;
const ID_BE_RESTORE: usize = 412;
const ID_BE_TOGGLE: usize = 413;
const ID_BE_OPEN: usize = 414;
const ID_NOTE_EMOJI: usize = 420;
const ID_E_EMOJI_NEW: usize = 431;
const ID_BE_BROWSE: usize = 432;
/// 「绑到」输入框（v1.65）：填词或码，从这里出发绑任何词——包括没绑过 emoji 的
/// 单字，它们在列表里根本不存在一行。
const ID_E_EMOJI_TARGET: usize = 433;
/// 「只看我绑的」过滤器（v1.65）。
const ID_BE_ONLY: usize = 434;

/// 页序号。**与页签顺序、`pages` 数组的下标三者必须一致**。
/// `--picker` 用的定时器 id（只在这个开发者开关下出现）。
const PICKER_TIMER: usize = 7;

const PAGE_GENERAL: usize = 0;
const PAGE_WORDS: usize = 1;
const PAGE_EMOJI: usize = 2;
const PAGE_LEARN: usize = 3;
const PAGE_COUNT: usize = 4;
const PAGE_TITLES: [&str; PAGE_COUNT] = ["通用", "词组", "emoji", "学习数据"];
/// 码提示的三段选择器文案与取值，顺序 = `HintMode` 的映射顺序。
const HINT_SEGMENTS: [&str; 3] = ["总是", "首次", "关闭"];

// ListView 的几条消息/样式常量。`windows` crate 把它们定义成裸 u32，这里直接引用
// 会让整段代码被 `as` 淹没，所以只挑用得到的几个，写清来历。
const LVS_REPORT: u32 = 0x0001;
const LVS_SINGLESEL: u32 = 0x0004;
const LVS_SHOWSELALWAYS: u32 = 0x0008;
const LVS_EX_FULLROWSELECT: usize = 0x0000_0020;
const LVS_EX_DOUBLEBUFFER: usize = 0x0001_0000;
const LVIF_TEXT: u32 = 0x0001;
const LVIF_PARAM: u32 = 0x0004;
const LVCF_WIDTH: u32 = 0x0002;
const LVCF_TEXT: u32 = 0x0004;
/// `LVCF_FMT`：列的对齐方式。不加这一位的列宽/文字照常生效，**但对齐默认左**，
/// 于是「上屏次数」「学习分」这种数字列会顶着列头左端排，一列数字参差不齐。
const LVCF_FMT: u32 = 0x0001;
/// ListView 背景与文字色（`LVM_FIRST = 0x1000`）。
/// 通用的 `NM_CUSTOMDRAW`（`NM_FIRST - 12` = -12）：列表要自绘行时
/// 先发这个通知问我们画不画。**不是** `LVN_FIRST - 12`，两者差 88。
const NM_CUSTOMDRAW: u32 = 0xFFFF_FFF4;
const LVM_SETBKCOLOR: u32 = 0x1001;
const LVM_SETTEXTCOLOR: u32 = 0x1024;
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
    /// emoji 页：搜索框外框、列表卡、编辑面板卡、面板内「绑到」与 emoji 两个
    /// 输入框的外框、「选中」信息行与两个标签。
    emoji_search_box: RECT,
    emoji_card: RECT,
    emoji_panel_card: RECT,
    emoji_target_box: RECT,
    emoji_new_box: RECT,
    emoji_sel_label: RECT,
    emoji_target_label: RECT,
    emoji_new_label: RECT,
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

    // emoji 页
    emoji_list: HWND,
    e_emoji_search: HWND,
    /// 「绑到」：词或码（v1.65）
    e_emoji_target: HWND,
    e_emoji_new: HWND,
    b_emoji_browse: HWND,
    b_emoji_save: HWND,
    b_emoji_disable: HWND,
    b_emoji_restore: HWND,
    b_emoji_toggle: HWND,
    b_emoji_only: HWND,
    b_emoji_open: HWND,
    n_emoji: HWND,

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
    /// `--picker`：窗口起来后自动弹一次 emoji 选择器（截图验收用；平时为 false）
    open_picker: bool,
    /// `learn.db`（学习数据页只读）
    db_path: PathBuf,
    /// 学习数据页当前列出的行（下标即 ListView 行号）
    learn_rows: Vec<crate::learn::Row>,

    // emoji 页（M4d / D85）
    /// 合并视图模型（内置只读 + 用户层读写）
    emoji: crate::emoji_page::EmojiPage,
    /// 当前（过滤后）展示的行，下标即 ListView 行号
    emoji_rows: Vec<crate::emoji_page::EmojiRow>,
    /// 编辑面板挂着哪一行
    emoji_sel: Option<usize>,
    /// 本机字形覆盖：列表标灰与「能不能保存」共用同一份判据
    emoji_support: heinrich_font::EmojiSupport,
    /// 面板行 1 的临时提示（红色）。`None` 时显示选中信息。
    ///
    /// 存在的理由（v1.64）：改绑被拒时只往窗口底部那行灰字写一句话，用户看不到，
    /// 反馈成「点了改绑没有任何反应」。提示要出现在他眼睛正盯着的地方——输入框上方。
    emoji_msg: Option<String>,
    /// 列表里画彩色 emoji 的 D2D 画笔（懒建：只有 emoji 页用得上）
    glyph: Option<crate::emoji_draw::ColorEmoji>,
    /// 列表过滤器（v1.65）：只看用户层动过的行（已改绑 / 已停用 / 新增）。
    ///
    /// 内置表两千多条，用户自己绑的往往只有几条——混在一起按词排序，
    /// 「我刚绑的那条在哪」就得靠搜索词回忆。开这个开关，列表只剩自己的改动。
    emoji_only_user: bool,
}

impl App {
    pub fn new(
        data: Option<PathBuf>,
        words: Option<PathBuf>,
        config: Option<PathBuf>,
        db: Option<PathBuf>,
        emoji: Option<PathBuf>,
    ) -> Result<Self, String> {
        let path = words
            .or_else(crate::lex::default_words_path)
            .ok_or("取不到 %APPDATA%，请用 --words 指定词库文件")?;
        let model =
            Model::load(path.clone()).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
        let (lex, lex_note) = match find_data_dir(data.clone()) {
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
        // emoji 页：内置表在数据目录（缺了就降级成「只有用户层」），用户层允许
        // 不存在（`--emoji` 是给自测用的，真路径默认 %APPDATA%）。
        let emoji_builtin = find_data_dir(data.clone()).map(|d| d.join("emoji.toml"));
        let emoji_user = emoji.or_else(crate::emoji_page::default_user_path);
        let emoji_model = match (emoji_builtin, emoji_user) {
            (Some(b), Some(u)) => crate::emoji_page::EmojiPage::load(b, u),
            (Some(_), None) => {
                return Err("取不到 %APPDATA%，请用 --emoji 指定 emoji 用户层路径".into())
            }
            (None, u) => {
                let u = u.ok_or("取不到 %APPDATA%，请用 --emoji 指定 emoji 用户层路径")?;
                crate::emoji_page::EmojiPage::load(PathBuf::from("emoji.toml"), u)
            }
        };
        let emoji_support = {
            let chars = emoji_model
                .builtin_choices()
                .iter()
                .chain(emoji_model.rows().iter().map(|r| &r.emoji))
                .flat_map(|s| s.chars())
                .collect::<Vec<_>>();
            heinrich_font::EmojiSupport::detect(chars)
        };
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
            emoji_list: HWND::default(),
            e_emoji_search: HWND::default(),
            e_emoji_target: HWND::default(),
            e_emoji_new: HWND::default(),
            b_emoji_browse: HWND::default(),
            b_emoji_save: HWND::default(),
            b_emoji_disable: HWND::default(),
            b_emoji_restore: HWND::default(),
            b_emoji_toggle: HWND::default(),
            b_emoji_only: HWND::default(),
            b_emoji_open: HWND::default(),
            n_emoji: HWND::default(),
            pages: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
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
            open_picker: false,
            db_path,
            learn_rows: Vec::new(),
            emoji: emoji_model,
            emoji_rows: Vec::new(),
            emoji_sel: None,
            emoji_support,
            emoji_msg: None,
            glyph: None,
            emoji_only_user: false,
        })
    }

    /// 启动时停在第几页（`--page`）。越界一律收到最后一页。
    pub fn set_initial_page(&mut self, page: usize) {
        self.initial_page = page.min(PAGE_COUNT - 1);
    }

    /// `--picker`：起来后自动弹一次 emoji 选择器。
    ///
    /// 是给截图验收用的：选择器是**模态**的，`PrintWindow` 只能截「已经开着」的
    /// 窗口，没这个开关就只能靠人手点，脚本没法复现。
    pub fn set_open_picker(&mut self, on: bool) {
        self.open_picker = on;
    }

    /// 逻辑像素 → 物理像素。窗口按 96 DPI 设计，乘上系统缩放即可。
    fn s(&self, v: i32) -> i32 {
        (v as f32 * self.dpi).round() as i32
    }

    fn create_children(&mut self) -> windows::core::Result<()> {
        self.create_words_page()?;
        self.create_general_page()?;
        self.create_learn_page()?;
        self.create_emoji_page()?;
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
        for (i, (title, width, fmt)) in [
            ("词", 140, LVCFMT_LEFT),
            ("拼音", 190, LVCFMT_LEFT),
            ("码", 90, LVCFMT_LEFT),
            // 权重是「数字 + *」：左对齐时一位数与两位数左端齐、右端不齐，
            // 一列看下来像随手打的。居中后整列有轴线。
            ("权重", 110, LVCFMT_LEFT),
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
            ("上次使用", 120, LVCFMT_LEFT),
            ("学习分", 90, LVCFMT_RIGHT),
            ("档位", 80, LVCFMT_LEFT),
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

    /// emoji 页：搜索 + 列表 + 编辑面板（新 emoji 框 + 从内置表选）+ 页级操作。
    ///
    /// 搜索框走 `EN_CHANGE` 实时过滤（不像学习页要按「搜」）：emoji 表 2400 条，
    /// 边打边缩是这条页最自然的使用方式。下拉框（ComboBox）是这一页唯一的原生
    /// 下拉——「从内置表选」的候选有一千多种，三段选择器那套自绘管不过来；
    /// D79 保留原生 Edit/ListView 的同一条理由在这里对 ComboBox 同样成立。
    fn create_emoji_page(&mut self) -> windows::core::Result<()> {
        let hwnd = self.hwnd;
        self.emoji_list = child(
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
            ID_EMOJI_LIST,
            self.font,
        )?;
        for (i, (title, width, fmt)) in [
            ("词 / 码", 200, LVCFMT_LEFT),
            ("绑定", 90, LVCFMT_LEFT),
            ("emoji", 130, LVCFMT_LEFT),
            ("状态", 120, LVCFMT_LEFT),
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
                    self.emoji_list,
                    LVM_INSERTCOLUMNW,
                    WPARAM(i),
                    LPARAM(&mut col as *mut _ as isize),
                );
            }
        }

        let edit_style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | 0x0080);
        self.e_emoji_search = child(
            hwnd,
            w!("Edit"),
            "",
            edit_style,
            ID_E_EMOJI_SEARCH,
            self.font,
        )?;
        self.e_emoji_new = child(hwnd, w!("Edit"), "", edit_style, ID_E_EMOJI_NEW, self.font)?;
        // 「绑到」框（v1.65）：Tab 序里排在 emoji 框前面——先想清楚绑给谁，
        // 再挑 emoji，与面板上从左到右的读法一致。
        self.e_emoji_target = child(
            hwnd,
            w!("Edit"),
            "",
            edit_style,
            ID_E_EMOJI_TARGET,
            self.font,
        )?;

        // 挑 emoji 走自绘网格选择器（D86 返工）：原生 ComboBox 一千多项只能滚，
        // 还看不出分类；而之前的 0x0020（本想写 WS_VSCROLL）恰好撞上
        // CBS_OWNERDRAWVARIABLE，下拉框变成自绘模式、里面一片空白。
        self.b_emoji_browse = button(hwnd, "浏览 emoji…", ID_BE_BROWSE, self.font)?;

        // 建子控件的顺序 = Tab 键顺序：保存绑定是主操作，第一个建
        self.b_emoji_save = child(
            hwnd,
            w!("Button"),
            "保存绑定",
            WINDOW_STYLE(WS_TABSTOP.0 | BS_OWNERDRAW),
            ID_BE_SAVE,
            self.font,
        )?;
        self.b_emoji_disable = button(hwnd, "停用", ID_BE_DISABLE, self.font)?;
        self.b_emoji_restore = button(hwnd, "恢复内置", ID_BE_RESTORE, self.font)?;
        self.b_emoji_toggle = button(hwnd, "", ID_BE_TOGGLE, self.font)?;
        self.b_emoji_only = button(hwnd, "只看我绑的", ID_BE_ONLY, self.font)?;
        self.b_emoji_open = button(hwnd, "打开文件位置", ID_BE_OPEN, self.font)?;
        self.n_emoji = child(
            hwnd,
            w!("Static"),
            "",
            WINDOW_STYLE(0),
            ID_NOTE_EMOJI,
            self.fonts.caption,
        )?;

        self.pages[PAGE_EMOJI] = vec![
            self.emoji_list,
            self.e_emoji_search,
            self.e_emoji_target,
            self.e_emoji_new,
            // 漏一个就是「它留在每一页上」：切页只对 `pages[n]` 里的控件做
            // `ShowWindow(SW_HIDE)`，没登记的就一直显示在最上层（真机看到
            // 通用页中间悬着一个「浏览 emoji…」按钮）。
            self.b_emoji_browse,
            self.b_emoji_save,
            self.b_emoji_disable,
            self.b_emoji_restore,
            self.b_emoji_toggle,
            self.b_emoji_only,
            self.b_emoji_open,
            self.n_emoji,
        ];
        Ok(())
    }

    /// 三个列表的底色/文字色与扩展样式统一在**各页都建完之后**上（`create_words_page`
    /// 跑的时候 `learn_list` 还不存在，那里碰不到它）。
    ///
    /// **不设 `LVM_SETTEXTBKCOLOR`**：那个消息把「文字背后的颜色」钉成卡片色，
    /// 系统画的选中高亮就被它盖住了——只剩行尾一小截空白变色，看起来像「点了
    /// 一行却什么都没选中」。真机反馈「不知道点哪里才能选中」就是这个。
    /// 底色 `LVM_SETBKCOLOR` 不影响高亮，留着让空行也是卡片色而不是白底。
    fn style_lists(&self) {
        let ex = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER;
        unsafe {
            for &h in &[self.list, self.learn_list, self.emoji_list] {
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
                // 整行选中：点「emoji」那一格也算选中这一行（默认只有第一列可点，
                // 而这一页用户想点的常常是右边那几格）。双击缓冲避免滚动闪白。
                send(
                    h,
                    LVM_SETEXTENDEDLISTVIEWSTYLE,
                    WPARAM(ex),
                    LPARAM(ex as isize),
                );
                // `LVS_SHOWSELALWAYS` 是**普通**样式，走 GWL_STYLE 而不是扩展样式。
                // 没有它，焦点一进「搜索」框，列表的选中就淡成看不见——用户转头
                // 回来又不确定自己选的是哪一行（真机反馈的另一半）。
                let style = GetWindowLongPtrW(h, GWL_STYLE) as u32;
                if style & LVS_SHOWSELALWAYS == 0 {
                    SetWindowLongPtrW(h, GWL_STYLE, (style | LVS_SHOWSELALWAYS) as isize);
                    let _ = InvalidateRect(Some(h), None, false);
                }
            }
        }
    }

    /// 切页：只动可见性，不重建控件（重建会让编辑中的内容、滚动位置全丢）。
    /// 页签本身是画出来的（见模块头），所以除了显隐只需要重画一次主窗口。
    fn show_page(&mut self, page: usize) {
        let page = page.min(PAGE_COUNT - 1);
        self.page = page;
        // 段选控件只在通用页存在：切走之后那个 hover 状态就没有意义了，
        // 留着会让下一次鼠标移动在别的页上白算一次「hover 变了」
        self.seg_hot = -1;
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
        self.layout_emoji(m, content_top, cw2, content_bottom);

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
        // 三个列表用卡片的圆角，其余是输入框的 4px
        for (h, rad) in [
            (self.list, self.s(8)),
            (self.learn_list, self.s(8)),
            (self.emoji_list, self.s(8)),
        ] {
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
            self.e_emoji_search,
            self.e_emoji_new,
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

    /// emoji 页：搜索行 + 列表卡 + 编辑面板卡（两行）+ 按钮行 + 说明行。
    ///
    /// 节奏与词组页一致（列表卡 → 面板卡 → 按钮行 → 说明，全部从底往上排），
    /// 多出来的只有搜索行——它和学习页同一摆位，两页切换时视线不用重新找。
    fn layout_emoji(&mut self, cx: i32, cy: i32, cw2: i32, bottom: i32) {
        let btn_h = self.s(30);
        let row = self.s(30);
        let note_h = self.s(22);
        let search_y = cy + self.s(56);
        // 面板卡两行：「选中」信息行 + 「新 emoji」输入行
        let panel_h = row * 2 + self.s(28);
        let note_y = bottom - note_h;
        let btn_y = note_y - self.s(8) - btn_h;
        let panel_y = btn_y - self.s(12) - panel_h;
        let list_y = search_y + self.s(38);
        let list_h = (panel_y - self.s(14) - list_y).max(self.s(120));

        self.geo.emoji_search_box = Self::rect(cx + self.s(40), search_y, self.s(320), btn_h);
        self.center_in_box(self.e_emoji_search, self.geo.emoji_search_box);

        let card = Self::rect(cx, list_y, cw2, list_h);
        self.geo.emoji_card = card;
        let panel = Self::rect(cx, panel_y, cw2, panel_h);
        self.geo.emoji_panel_card = panel;

        // 面板行 1：「选中」信息（纯自绘文字）；行 2：**同一行**两个输入
        // ——「绑到」框（词或码）+ emoji 框 + 浏览按钮。两个框并排是一行说完
        // 一件事：把哪枚 emoji 绑到哪个词/字上（v1.65）。
        let info = Self::inset(panel, self.s(16));
        self.geo.emoji_sel_label = Self::rect(
            info.left,
            panel.top + self.s(14),
            info.right - info.left,
            row,
        );
        let input_y = panel.top + self.s(14) + row;
        let browse_w = self.btn_w_from("浏览 emoji…", 116);
        let target_label_w = self.s(40);
        self.geo.emoji_target_label = Self::rect(info.left, input_y, target_label_w, row);
        let target_x = info.left + target_label_w + self.s(8);
        // 「绑到」按最长的合法目标给宽：8 个汉字的词（`parse_target` 的上限），
        // 4 位码绰绰有余，再宽就是空白。
        let target_w = self.s(180);
        self.geo.emoji_target_box = Self::rect(target_x, input_y, target_w, row);
        // 「emoji」标签按 5 个字母宽度算（原来是「新 emoji」，改成单个词避免
        // 两个标签各占一大截，把框挤没）
        let new_label_w = self.s(52);
        let new_label_x = target_x + target_w + self.s(20);
        self.geo.emoji_new_label = Self::rect(new_label_x, input_y, new_label_w, row);
        let new_x = new_label_x + new_label_w + self.s(8);
        // emoji 框按**内容**给宽（v1.66）：框里永远只有一枚字形，早先让它吃掉
        // 「浏览 emoji…」左边的全部剩余宽度（真机截图里约 520px），一行里挂个
        // 空箱子，看着像还没填。现在固定 96dp——够画一枚彩色 emoji 加左右内边距，
        // 「浏览 emoji…」紧随其后，右侧留白归卡片，不再塞给输入框。
        let new_w = self.s(96);
        self.geo.emoji_new_box = Self::rect(new_x, input_y, new_w, row);
        let browse_x = (self.geo.emoji_new_box.right + self.s(12)).min(info.right - browse_w);

        unsafe {
            // 搜索框右侧是「总开关」（破坏性弱、但要一眼看到状态），靠右放，
            // 与学习页「清除」同一摆位
            let toggle_w = self.btn_w_from("停用 emoji 候选", 140);
            let _ = MoveWindow(
                self.b_emoji_toggle,
                cx + cw2 - toggle_w,
                search_y,
                toggle_w,
                btn_h,
                true,
            );
            // 过滤器紧贴搜索框右边：搜「好」与「只看我绑的」是同一个动作的两半
            // （缩小范围），摆在一起才顺手。
            let only_w = self.btn_w_from("只看我绑的", 96);
            let only_x = (self.geo.emoji_search_box.right + self.s(8))
                .min(cx + cw2 - toggle_w - self.s(8) - only_w);
            let _ = MoveWindow(self.b_emoji_only, only_x, search_y, only_w, btn_h, true);
            move_rect(self.emoji_list, Self::inset(card, self.s(8)), true);
            self.center_in_box(self.e_emoji_target, self.geo.emoji_target_box);
            self.center_in_box(self.e_emoji_new, self.geo.emoji_new_box);
            let _ = MoveWindow(self.b_emoji_browse, browse_x, input_y, browse_w, row, true);
            let _ = MoveWindow(self.n_emoji, cx, note_y + self.s(2), cw2, note_h, true);
        }

        // 按钮行：保存绑定（主）→ 停用 → 恢复内置；「打开文件位置」靠右
        let gap = self.s(8);
        let mut bx = cx;
        for (h, text, min_w) in [
            (self.b_emoji_save, "保存绑定", 88),
            (self.b_emoji_disable, "停用", 72),
            (self.b_emoji_restore, "恢复内置", 96),
        ] {
            let w = self.btn_w_from(text, min_w);
            unsafe {
                let _ = MoveWindow(h, bx, btn_y, w, btn_h, true);
            }
            bx += w + gap;
        }
        let open_w = self.btn_w_from("打开文件位置", 104);
        unsafe {
            let _ = MoveWindow(
                self.b_emoji_open,
                cx + cw2 - open_w,
                btn_y,
                open_w,
                btn_h,
                true,
            );
        }
        // 权重：词/码、绑定、emoji、状态
        self.fit_columns(self.emoji_list, &[34, 14, 26, 26]);
    }

    /// [`Self::btn_w`] 的免借 DC 版：布局里连续量好几个按钮时少开几次 DC。
    fn btn_w_from(&self, text: &str, min: i32) -> i32 {
        let hdc = unsafe { GetDC(Some(self.hwnd)) };
        let w = self.btn_w(hdc, text, min);
        if !hdc.0.is_null() {
            unsafe { ReleaseDC(Some(self.hwnd), hdc) };
        }
        w
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

    /// 只失效页签栏那一条（hover 换高亮时用）。
    ///
    /// 页签是画出来的，没有子控件；hover 只影响这一条上的文字颜色和底块，
    /// 底下的页面内容一个字都不会变。整窗重绘能让它在几百微秒里「先灰一下」，
    /// 鼠标在页签上滑来滑去就一直在灰——真机反馈的「整个界面在闪」有一半是它。
    fn repaint_tab_bar(&self) {
        if self.hwnd.0.is_null() {
            return;
        }
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
            // 多给 4dp：分隔线画在 tab_h 上，正好压在边界
            let strip = RECT {
                left: 0,
                top: 0,
                right: rc.right,
                bottom: self.s(44) + self.s(4),
            };
            let _ = InvalidateRect(Some(self.hwnd), Some(&strip), false);
        }
    }

    /// 只失效段选控件那一块（含外扩 6dp 给描边与阴影）。
    fn repaint_seg(&self) {
        if self.hwnd.0.is_null() {
            return;
        }
        let pad = self.s(6);
        let r = RECT {
            left: self.geo.seg.left - pad,
            top: self.geo.seg.top - pad,
            right: self.geo.seg.right + pad,
            bottom: self.geo.seg.bottom + pad,
        };
        if r.right <= r.left || r.bottom <= r.top {
            return;
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), Some(&r), false);
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

// -------------------------------------------------------- emoji 页（M4d / D85）
impl App {
    /// 重填列表：合并视图 + 搜索过滤 + 「只看我绑的」。
    ///
    /// 两个过滤条件是**与**关系：先按开关砍掉内置未改动的行，再按搜索词筛。
    /// 用户层动过的行（已改绑 / 已停用 / 新增）就是 `row.user`——它比拿
    /// `state_cell()` 比字符串更直接，也不会漏掉将来新加的状态名。
    fn refresh_emoji(&mut self) {
        let needle = get_text(self.e_emoji_search).trim().to_lowercase();
        let only_user = self.emoji_only_user;
        let all = self.emoji.rows();
        self.emoji_rows = all
            .into_iter()
            .filter(|r| !only_user || r.user)
            .filter(|r| {
                needle.is_empty()
                    || r.key_cell().to_lowercase().contains(&needle)
                    || r.emoji.contains(&needle)
                    || r.state_cell().contains(&needle)
            })
            .collect();
        unsafe {
            send(self.emoji_list, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
        }
        for (i, r) in self.emoji_rows.iter().enumerate() {
            insert_emoji_row(self.emoji_list, i, r, &self.emoji_support);
        }
        self.update_emoji_note();
        self.update_emoji_buttons();
    }

    /// 刷新并按 key（词/码）找回选中项。改绑/停用/恢复之后行内容变了，
    /// 下标会挪，只能按内容找。
    fn refresh_emoji_keep_sel(&mut self, key: &str) {
        self.refresh_emoji();
        if let Some(i) = self.emoji_rows.iter().position(|r| r.key_cell() == key) {
            self.emoji_sel = Some(i);
            select_item(self.emoji_list, i);
            self.fill_emoji_panel(Some(i));
        } else {
            self.emoji_sel = None;
            self.fill_emoji_panel(None);
        }
        self.update_emoji_buttons();
        self.update_emoji_note();
    }

    fn selected_emoji(&self) -> Option<usize> {
        let r = unsafe {
            send(
                self.emoji_list,
                LVM_GETNEXTITEM,
                WPARAM(usize::MAX),
                LPARAM(LVNI_SELECTED as isize),
            )
        };
        (r.0 >= 0).then_some(r.0 as usize)
    }

    /// 当前面板挂着的行的 key（改绑/停用/恢复都作用于它）。
    fn emoji_key_of(&self, idx: usize) -> Option<crate::emoji_page::Key> {
        let row = self.emoji_rows.get(idx)?;
        match (&row.word, &row.code) {
            (Some(w), _) => Some(crate::emoji_page::Key::Word(w.clone())),
            (None, Some(c)) => Some(crate::emoji_page::Key::Code(c.clone())),
            (None, None) => None,
        }
    }

    /// 面板：「绑到」+ emoji 两个框都回填当前值 + 信息行 + 按钮态。
    fn fill_emoji_panel(&mut self, idx: Option<usize>) {
        let row = idx.and_then(|i| self.emoji_rows.get(i));
        let emoji = row.map(|r| r.emoji.clone()).unwrap_or_default();
        // 「绑到」框跟着选中的行走：点一行 = 把它填好，改 emoji 后按「保存绑定」
        // 就是老「改绑」那条路径；清空面板时框也清空，回到「自己写一个目标」。
        let target = row
            .map(|r| match (&r.word, &r.code) {
                (Some(w), _) => w.clone(),
                (None, Some(c)) => c.clone(),
                (None, None) => String::new(),
            })
            .unwrap_or_default();
        set_text(self.e_emoji_target, &target);
        set_text(self.e_emoji_new, &emoji);
        // 换选一行 = 上一次的提示已经过期，不能让它挂在新行头上
        self.emoji_msg = None;
        self.update_emoji_note();
        self.update_emoji_buttons();
        // 面板行 1 是**自绘**文字（原生 Edit 会自己刷，文字不会）：不重绘的话
        // 选了行之后那句话还停在上一句，看着像「点了没反应」
        self.repaint();
    }

    /// 点中列表里的一行（`NM_CLICK`）：**再点已选中的那行 = 还原面板**。
    ///
    /// 换行由 `LVN_ITEMCHANGED` 负责；这里只管「选中的还是它」那种点击——用户
    /// 清空了「绑到」或改了 emoji、想回到这一行的原值，最自然的手势就是再点它一次。
    /// 空占位（`iItem == -1`，点在列表空白处）不动面板，免得手一抖丢了正在写的值。
    fn on_emoji_row_click(&mut self, i_item: i32) {
        if i_item < 0 {
            return;
        }
        let idx = i_item as usize;
        if self.emoji_sel == Some(idx) {
            self.fill_emoji_panel(Some(idx));
        }
    }

    /// 两个输入框内容变化（`EN_CHANGE`）：按钮态要跟着变，旧提示要作废。
    ///
    /// **输入本身合不合格不在这里算**，而是绘制时现算（[`Self::emoji_panel_alert`]）。
    /// 理由是踩过的坑：`EN_CHANGE` 到达时窗口文本还不一定是新的（`SetWindowText`
    /// 那条路径上读到过旧值），存下来的结论会一直挂着——自测里「填回合法 emoji
    /// 提示还在」就是这么来的。绘制时读、绘制时算，永远和框里看到的一致。
    fn on_emoji_input_changed(&mut self) {
        self.update_emoji_buttons();
        self.emoji_msg = None;
        self.repaint();
    }

    /// 面板行 1 要不要报警、报什么（`None` = 正常，画选中信息）。
    ///
    /// 三条来源，顺序固定：**emoji 不合格**优先（那是刚才那次保存最可能的失败
    /// 原因），其次「绑到」看不懂，最后是上次保存被拒的原因（字形画不出 / 词库里
    /// 没有这个词）。
    fn emoji_panel_alert(&self) -> Option<String> {
        let emoji = get_text(self.e_emoji_new);
        // 空框不报错：用户可能正在从零敲一枚，先清空是正常动作
        if !emoji.trim().is_empty() {
            if let Err(e) = crate::emoji_page::check_input(&emoji) {
                return Some(e);
            }
        }
        let target = get_text(self.e_emoji_target);
        if !target.trim().is_empty() {
            if let Err(e) = crate::emoji_page::parse_target(&target) {
                return Some(e);
            }
        }
        self.emoji_msg.clone()
    }

    /// 「只看我绑的」：列表只剩用户层动过的行（已改绑 / 已停用 / 新增）。
    fn on_emoji_only(&mut self) {
        self.emoji_only_user = !self.emoji_only_user;
        self.sync_emoji_only_text();
        // 过滤后选中的那一行可能已经不在了：按 key 找回，找不回来就清面板
        let keep = self
            .emoji_sel
            .and_then(|i| self.emoji_rows.get(i))
            .map(|r| r.key_cell());
        self.refresh_emoji();
        match keep.and_then(|k| self.emoji_rows.iter().position(|r| r.key_cell() == k)) {
            Some(i) => {
                self.emoji_sel = Some(i);
                select_item(self.emoji_list, i);
                self.fill_emoji_panel(Some(i));
            }
            None => {
                self.emoji_sel = None;
                self.clear_emoji_selection();
                self.emoji_msg = None;
                self.update_emoji_buttons();
                self.repaint();
            }
        }
        self.set_status(if self.emoji_only_user {
            "只显示你自己绑过的行（已改绑 / 已停用 / 新增）"
        } else {
            "显示全部（内置 + 你的改动）"
        });
    }

    /// 过滤器按钮的文字跟着状态走：亮着的时候要说「怎么回到全部」。
    fn sync_emoji_only_text(&self) {
        set_text(
            self.b_emoji_only,
            if self.emoji_only_user {
                "看全部"
            } else {
                "只看我绑的"
            },
        );
    }

    /// 列表取消选中（过滤器把那一行藏起来时用）。
    fn clear_emoji_selection(&self) {
        let mut item = LVITEMW {
            state: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(0),
            stateMask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0),
            ..Default::default()
        };
        unsafe {
            send(
                self.emoji_list,
                LVM_SETITEMSTATE,
                WPARAM(usize::MAX),
                LPARAM(&mut item as *mut _ as isize),
            );
        }
    }

    /// 列表行自绘：选中行由我们整行画（浅蓝底 + 深色字）。
    ///
    /// 为什么要 `CDRF_SKIPDEFAULT` 自己画：开着视觉主题时，选中行的底色由
    /// 主题绘制，`clrTextBk` 被直接忽略（实测填什么都不变色，一直是系统蓝）；
    /// 而关掉主题虽然能让它生效，列头与滚动条会一起退回经典外观——那是一次
    /// 看得见的倒退。自己画整行两头都保住。
    ///
    /// 与列表**同进程**，所以 `LVM_GETITEMTEXTW` 可以直接传结构指针（跨进程
    /// 传指针会立刻崩，验收脚本踩过这个坑）。
    fn list_custom_draw(&mut self, lparam: LPARAM) -> LRESULT {
        const CDDS_PREPAINT: u32 = 0x0000_0001;
        const CDDS_ITEMPREPAINT: u32 = 0x0001_0001;
        const CDRF_DODEFAULT: isize = 0;
        const CDRF_NOTIFYITEMDRAW: isize = 0x0000_0020;
        const CDRF_SKIPDEFAULT: isize = 0x0000_0004;
        const LVM_GETCOLUMNWIDTH: u32 = 0x101D;
        const LVM_GETITEMTEXTW: u32 = 0x1073;
        const LVIF_TEXT_ONLY: u32 = 0x0001;
        // #E8F0FB（COLORREF 是 0x00BBGGRR）
        const SEL_BG: COLORREF = COLORREF(0x00FB_F0E8);
        /// emoji 表里 emoji 那一列的序号（`create_emoji_page` 的列序）
        const EMOJI_COL: usize = 2;

        let cd = unsafe { &mut *(lparam.0 as *mut NMLVCUSTOMDRAW) };
        let stage = cd.nmcd.dwDrawStage.0;
        if stage == CDDS_PREPAINT {
            return LRESULT(CDRF_NOTIFYITEMDRAW);
        }
        if stage != CDDS_ITEMPREPAINT {
            return LRESULT(CDRF_DODEFAULT);
        }
        // 判据用「列表自己说哪一行选中」而不是 `uItemState & CDIS_SELECTED`：
        // 那个字段在这个阶段的位与文档不一致（实测所有行都带 0x1，一涂
        // 就是整片蓝）。问列表要选中项，语义唯一。
        let list = cd.nmcd.hdr.hwndFrom;
        let sel = unsafe {
            send(
                list,
                LVM_GETNEXTITEM,
                WPARAM(usize::MAX),
                LPARAM(LVNI_SELECTED as isize),
            )
        }
        .0;
        let selected = sel >= 0 && cd.nmcd.dwItemSpec as isize == sel;
        // emoji 表**每一行**都自绘（v1.64）：它有一列要画彩色 emoji，而 GDI 只会
        // 画单色轮廓；顺带让选中行与普通行的内衬、字号、行高完全一致——只自绘
        // 选中行的时候，两套排版并排看得出错位（真机反馈「感觉匹配不上」）。
        // 另外两张表没有这个需求，继续走系统绘制。
        let is_emoji = list == self.emoji_list;
        if !is_emoji && !selected {
            return LRESULT(CDRF_DODEFAULT);
        }

        let hdc = cd.nmcd.hdc;
        let rc = cd.nmcd.rc;
        let row = if is_emoji {
            self.emoji_rows.get(cd.nmcd.dwItemSpec)
        } else {
            None
        };
        // 停用的行整行淡一档：状态列写着「已停用」，颜色再给一个不看字也能认的信号
        let enabled = row.is_none_or(|r| r.enabled);
        let bg = if selected { SEL_BG } else { self.colors.card };
        let text_color = if enabled {
            self.colors.text1
        } else {
            self.colors.text3
        };
        unsafe {
            let brush = CreateSolidBrush(bg);
            if !brush.0.is_null() {
                FillRect(hdc, &rc, brush);
                let _ = DeleteObject(brush.into());
            }
            // 用列表自己的字体，别让自绘的行和旁边几行字重/字宽不一致
            // WM_GETFONT = 0x0031（列表自己知道它用的是哪个字体）
            let font = send(list, 0x0031, WPARAM(0), LPARAM(0));
            let old = if font.0 != 0 {
                SelectObject(hdc, HGDIOBJ(font.0 as *mut core::ffi::c_void))
            } else {
                HGDIOBJ::default()
            };
            let _ = SetBkMode(hdc, TRANSPARENT);
            let _ = SetTextColor(hdc, text_color);

            let mut x = rc.left;
            for col in 0..4 {
                let w = send(list, LVM_GETCOLUMNWIDTH, WPARAM(col), LPARAM(0)).0 as i32;
                if w <= 0 {
                    break;
                }
                let mut buf = [0u16; 128];
                let mut item = LVITEMW {
                    mask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_FLAGS(LVIF_TEXT_ONLY),
                    iSubItem: col as i32,
                    pszText: PWSTR(buf.as_mut_ptr()),
                    cchTextMax: buf.len() as i32,
                    ..Default::default()
                };
                send(
                    list,
                    LVM_GETITEMTEXTW,
                    WPARAM(cd.nmcd.dwItemSpec),
                    LPARAM(&mut item as *mut _ as isize),
                );
                let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                let text = String::from_utf16_lossy(&buf[..n]);
                if !text.is_empty() {
                    // 列一律左对齐（三张表的列头也都是左对齐）：自绘行只画
                    // 一遍文字，居中列会让选中行那几格和列头错开一截
                    let mut cell = RECT {
                        left: x + 8,
                        top: rc.top,
                        right: x + w - 8,
                        bottom: rc.bottom,
                    };
                    if is_emoji && col == EMOJI_COL {
                        // 彩色字形只能走 D2D：边留给我们自己刷（DC 目标的
                        // Clear 不做 alpha 混合），右边用不到就铺到底
                        let done = self.emoji_glyph(hdc, cell, &text, bg);
                        if !done {
                            // D2D 失败时退回 GDI 的单色轮廓：难看，但至少不是空白
                            let mut wide = text.encode_utf16().collect::<Vec<u16>>();
                            DrawTextW(
                                hdc,
                                &mut wide,
                                &mut cell,
                                DRAW_TEXT_FORMAT(
                                    DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                                ),
                            );
                        }
                    } else {
                        let mut wide = text.encode_utf16().collect::<Vec<u16>>();
                        DrawTextW(
                            hdc,
                            &mut wide,
                            &mut cell,
                            DRAW_TEXT_FORMAT(
                                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            ),
                        );
                    }
                }
                x += w;
            }
            if !old.0.is_null() {
                SelectObject(hdc, old);
            }
        }
        LRESULT(CDRF_SKIPDEFAULT)
    }

    /// 用 D2D 在 `cell` 里画一枚彩色 emoji；画笔懒建（一次窗活一个）。
    ///
    /// 字号取行高的 0.78：`Segoe UI Emoji` 的字身比汉字矮，取和行高一样大会被
    /// 行切掉上下；0.78 是让它在行里既饱满又不碰边的一档（与候选窗同一条经验）。
    fn emoji_glyph(&mut self, hdc: HDC, cell: RECT, emoji: &str, bg: COLORREF) -> bool {
        if self.glyph.is_none() {
            self.glyph = crate::emoji_draw::ColorEmoji::new().ok();
        }
        let Some(g) = self.glyph.as_mut() else {
            return false;
        };
        let px = (cell.bottom - cell.top) as f32 * 0.78;
        g.draw(hdc, cell, emoji, px, crate::emoji_draw::from_colorref(bg))
    }

    /// 面板行 1 的信息文字（绘制取，也给说明行复用）。
    fn emoji_info_text(&self) -> String {
        match self.emoji_sel.and_then(|i| self.emoji_rows.get(i)) {
            Some(r) => {
                let mut s = format!(
                    "选中「{}」→ {}（{}）",
                    r.key_cell(),
                    r.emoji,
                    r.state_cell()
                );
                if !self.emoji_support.supports(&r.emoji) {
                    s.push_str(" · 本机字体画不出，候选窗里会是方块");
                }
                if r.bind_cell().contains("单字") {
                    s.push_str(" · 单字绑定会让该字所在的候选行变宽");
                }
                // 纯用户新增的行：把「怎么撤掉它」说出来，按钮就在下面一排
                if r.is_user_only() {
                    s.push_str(" · 撤掉它点「删除绑定」");
                }
                s
            }
            None => {
                "「绑到」里填词或字（如 好、好评）或码（如 vb）→ 选一枚 emoji → 点「保存绑定」；\
                 也可以点列表里的一行"
                    .to_string()
            }
        }
    }

    /// 说明行（列表底下的 Static）。
    ///
    /// **只放一行短文**：原来拼了两条完整路径，Static 不换行、直接画到窗口
    /// 底边之外，真机截图上那行只剩半截。路径有「打开文件位置」按钮兜着。
    fn update_emoji_note(&self) {
        let total = self.emoji.builtin.entries.len();
        let user_n = self.emoji.user.entries.len();
        let mut s = format!("内置 {total} 条 · 用户层改动 {user_n} 条");
        // 过滤开着时把「现在看到几行」说出来：否则用户会以为列表丢了东西
        if self.emoji_only_user || !get_text(self.e_emoji_search).trim().is_empty() {
            s.push_str(&format!(" · 当前显示 {} 行", self.emoji_rows.len()));
        }
        match (&self.emoji.builtin_err, &self.emoji.user_err) {
            (Some(e), _) => s.push_str(&format!(" · 内置表：{e}")),
            (_, Some(e)) => s.push_str(&format!(" · {e}")),
            _ => {}
        }
        set_text(self.n_emoji, &s);
    }

    fn update_emoji_buttons(&mut self) {
        let row = self.emoji_sel.and_then(|i| self.emoji_rows.get(i)).cloned();
        let has_target = !get_text(self.e_emoji_target).trim().is_empty();
        let has_emoji = !get_text(self.e_emoji_new).trim().is_empty();
        // 「保存绑定」只要两个框都有内容就能点：目标**不必**先出现在列表里
        // （v1.65）——列表按词列，没绑过 emoji 的词和单字根本不在其中，
        // 要求先选中等于把「给单字绑一枚」这件事整个堵死。
        enable_window(self.b_emoji_save, has_target && has_emoji);
        // 停用 / 启用同一个按钮（v1.67）：老版本只有「停用」，关掉之后按钮变灰，
        // 用户新增的绑定就再也点不回来（只能删了重建）。文字跟着行状态换。
        let disabled = row.as_ref().is_some_and(|r| !r.enabled);
        set_button_text(self.b_emoji_disable, if disabled { "启用" } else { "停用" });
        enable_window(self.b_emoji_disable, row.is_some());
        // 第三个按钮一职两用，名字跟着行类型走：
        //   内置有的行 → 「恢复内置」（删用户层条目 = 回到内置值）
        //   纯用户新增的行 → 「删除绑定」（删了就是解绑，行消失）
        // 对新增行还写「恢复内置」等于让人去猜「撤掉这枚 emoji 该点哪个」。
        let user_only = row.as_ref().is_some_and(|r| r.is_user_only());
        let label = if user_only {
            "删除绑定"
        } else {
            "恢复内置"
        };
        set_button_text(self.b_emoji_restore, label);
        // 「删除绑定」只对用户层动过的行有意义；没动过的内置条目没什么可删
        enable_window(self.b_emoji_restore, row.as_ref().is_some_and(|r| r.user));
    }

    /// 「浏览 emoji…」→ 网格选择器，选中就填进 emoji 框（仍要点「保存绑定」才落盘）。
    fn on_emoji_browse(&mut self) {
        let choices = self.emoji.choices_for_picker();
        // 打开时停在这个已选 emoji 所属的分类上：改绑多半是「同一类里换一枚」
        let current = get_text(self.e_emoji_new);
        let initial = (!current.trim().is_empty()).then_some(current.as_str());
        if let Some(e) =
            crate::emoji_picker::pick(self.hwnd, self.dpi, &choices, &self.emoji_support, initial)
        {
            set_text(self.e_emoji_new, &e);
        }
        self.update_emoji_buttons();
        self.repaint();
        set_focus(self.e_emoji_new);
    }

    /// 保存绑定（v1.65）：把「emoji」框里那一枚绑到「绑到」框写的词或码上。
    ///
    /// 顺序：目标看得懂 → 是一枚 emoji → 本机画得出 → 目标真的打得出来 → 落盘。
    /// 每一步被拒都写面板行 1（红字）并把焦点送回**出错的那个框**——用户下一眼
    /// 就该看到问题所在。
    fn on_emoji_save(&mut self) {
        let target_text = get_text(self.e_emoji_target);
        let emoji = get_text(self.e_emoji_new);

        let key = match crate::emoji_page::parse_target(&target_text) {
            Ok(k) => k,
            Err(e) => {
                self.reject_emoji(&e, self.e_emoji_target);
                return;
            }
        };
        // 先问「这是不是一枚 emoji」，再问「本机画不画得出来」。反过来的话，
        // 框里填了「好」会得到「本机字体画不出来」——那句提示答错了问题，
        // 用户读到的结论是「点了没反应」。
        let one = match crate::emoji_page::check_input(&emoji) {
            Ok(one) => one,
            Err(e) => {
                self.reject_emoji(&e, self.e_emoji_new);
                return;
            }
        };
        if heinrich_font::probe_one(&one) == Some(false) {
            let m = format!("{one} 本机字体画不出来（候选窗里会是方块），换一枚吧");
            self.reject_emoji(&m, self.e_emoji_new);
            return;
        }
        // 目标得真的打得出来：词不在词库里 / 码在码表里查不到，绑上去就是一行
        // 永远不会命中的死数据。没装词库（`--data` 缺失）时不拦，只在状态行说明。
        match (&key, &self.lex) {
            (crate::emoji_page::Key::Word(w), Some(lex)) if !lex.has_word(w) => {
                let m = format!("「{w}」不在词库里，打不出来——绑了也不会出现在候选里。要给单个字挂就写那个字（如 好）");
                self.reject_emoji(&m, self.e_emoji_target);
                return;
            }
            (crate::emoji_page::Key::Code(c), Some(lex)) if !lex.has_code(c) => {
                let m = format!("五笔码 {c} 在码表里查不到，换个码试试");
                self.reject_emoji(&m, self.e_emoji_target);
                return;
            }
            _ => {}
        }

        let label = key.display();
        match self.emoji.rebind(&key, &one) {
            Ok(()) => {
                self.emoji_msg = None;
                self.refresh_emoji_keep_sel(&key.label());
                // 新绑的词会出现成一行（状态「新增」）；过滤开着时它一定在里面
                let extra = if self.lex.is_none() {
                    "（没装词库，目标是否打得出来没校验）"
                } else {
                    ""
                };
                self.set_status(&format!("已把 {one} 绑到「{label}」· 1 秒内生效{extra}"));
            }
            Err(e) => self.reject_emoji(&e, self.e_emoji_target),
        }
    }

    /// 保存被拒：红字进面板行 1、底部状态行同一句，焦点回到该改的那个框。
    fn reject_emoji(&mut self, msg: &str, focus: HWND) {
        self.emoji_msg = Some(msg.to_string());
        self.set_status(msg);
        self.repaint();
        set_focus(focus);
    }

    /// 停用 / 启用（同一个按钮）：写一条带 `enabled` 的用户层条目。
    ///
    /// 老版本只做「停用」，且对用户新增的条目直接禁用按钮；现在两个方向都能走，
    /// 用户新增的绑定关掉后还能点「启用」回来，不必重新绑一次。
    fn on_emoji_toggle_row(&mut self) {
        let Some(idx) = self.emoji_sel else {
            return;
        };
        let Some(key) = self.emoji_key_of(idx) else {
            return;
        };
        let row = self.emoji_rows.get(idx).cloned();
        let label = row.as_ref().map(|r| r.key_cell()).unwrap_or_default();
        let turning_off = row.as_ref().is_some_and(|r| r.enabled);
        match self.emoji.set_disabled(&key, turning_off) {
            Ok(()) => {
                self.refresh_emoji_keep_sel(&label);
                self.set_status(&if turning_off {
                    format!("「{label}」已停用，后面不再出现 emoji 候选 · 1 秒内生效")
                } else {
                    format!("「{label}」已启用，emoji 候选回来了 · 1 秒内生效")
                });
            }
            Err(e) => self.set_status(&e),
        }
    }

    fn on_emoji_restore(&mut self) {
        let Some(idx) = self.emoji_sel else {
            return;
        };
        let Some(key) = self.emoji_key_of(idx) else {
            return;
        };
        let row = self.emoji_rows.get(idx).cloned();
        let label = row.as_ref().map(|r| r.key_cell()).unwrap_or_default();
        let user_only = row.as_ref().is_some_and(|r| r.is_user_only());
        match self.emoji.restore(&key) {
            Ok(()) => {
                self.refresh_emoji_keep_sel(&label);
                // 同一个动作两种说法：内置有的 = 回到内置值；纯新增 = 解绑
                let msg = if user_only {
                    format!("「{label}」的绑定已删除，这个字/词不再出现 emoji 候选 · 1 秒内生效")
                } else {
                    format!("「{label}」已恢复内置 · 1 秒内生效")
                };
                self.set_status(&msg);
            }
            Err(e) => self.set_status(&e),
        }
    }

    /// 总开关：开 = 删 `emoji.enabled` 行（默认就是开），关 = 写 `false`。
    fn on_emoji_toggle(&mut self) {
        let on = crate::emoji_page::emoji_enabled(&self.config_path);
        match crate::emoji_page::set_emoji_enabled(&self.config_path, !on) {
            Ok(()) => {
                self.sync_emoji_toggle_text();
                self.repaint();
                self.set_status(if on {
                    "已关闭 emoji 候选 · 1 秒内生效（候选行宽与没有这个功能时完全一样）"
                } else {
                    "已开启 emoji 候选 · 1 秒内生效"
                });
            }
            Err(e) => message(
                self.hwnd,
                "写配置失败",
                &format!("{}：{e}", self.config_path.display()),
            ),
        }
    }

    /// 开关按钮的文字跟着配置走（开窗、切换之后各同步一次）。
    fn sync_emoji_toggle_text(&self) {
        let on = crate::emoji_page::emoji_enabled(&self.config_path);
        set_text(
            self.b_emoji_toggle,
            if on {
                "停用 emoji 候选"
            } else {
                "启用 emoji 候选"
            },
        );
    }

    fn on_emoji_open(&mut self) {
        let path = self.emoji.user_path.clone();
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
}

/// emoji 列表的一行。列序：词/码、绑定、emoji、状态（缺字形的在状态里点名）。
fn insert_emoji_row(
    list: HWND,
    idx: usize,
    r: &crate::emoji_page::EmojiRow,
    support: &heinrich_font::EmojiSupport,
) {
    let state = if support.supports(&r.emoji) {
        r.state_cell().to_string()
    } else {
        format!("{}·本机缺字形", r.state_cell())
    };
    let cells = [r.key_cell(), r.bind_cell(), r.emoji.clone(), state];
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
                    app.sync_emoji_toggle_text();
                    app.sync_emoji_only_text();
                    app.refresh_emoji();
                    // 刻意**不**自动选中第一行：面板是给「新词」用的，一开窗就
                    // 躺着上次的词会让人以为已经选中了它，改完点保存就把那条词改了。
                    app.set_status("");
                    if app.open_picker {
                        // 等窗口画完第一帧再弹：否则选择器的居中位置按未布局的
                        // 父窗口算，会歪到屏幕角上
                        unsafe {
                            let _ = SetTimer(Some(hwnd), PICKER_TIMER, 250, None);
                        }
                    }
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
        WM_TIMER if wparam.0 == PICKER_TIMER => {
            unsafe {
                let _ = KillTimer(Some(hwnd), PICKER_TIMER);
            }
            if let Some(app) = unsafe { app_of(hwnd) } {
                app.open_picker = false;
                app.on_emoji_browse();
            }
            LRESULT(0)
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
                // 只有「真的有东西亮着」才需要重画。
                //
                // 这条通知**不只在鼠标离开窗口时来**：指针落进任何一个子控件
                // （Edit / 按钮）也会给父窗口发一次 WM_MOUSELEAVE——子控件的
                // 客户区不算父窗口的 hover 范围。于是「鼠标划过编辑面板」这个
                // 动作会连续发好几次：每进出一次输入框就整窗重绘一遍，屏幕上
                // 看到的就是闪。tab_hot / seg_hot 本来就是 -1 时，这里没有任何
                // 东西变过，重画纯属浪费。
                let was_tab = app.tab_hot != -1;
                let was_seg = app.seg_hot != -1;
                app.tab_hot = -1;
                app.seg_hot = -1;
                if was_tab {
                    app.repaint_tab_bar();
                }
                if was_seg {
                    app.repaint_seg();
                }
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
                    // emoji 页：搜索框实时过滤，两个输入框只管按钮态
                    (EN_CHANGE, ID_E_EMOJI_SEARCH) => app.refresh_emoji(),
                    (EN_CHANGE, ID_E_EMOJI_NEW | ID_E_EMOJI_TARGET) => app.on_emoji_input_changed(),
                    // CBN_SELCHANGE = 1（与 EN_ 段相邻的通知码）
                    (0, ID_BE_BROWSE) => app.on_emoji_browse(),
                    (0, ID_BE_ONLY) => app.on_emoji_only(),
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
                    (0, ID_BE_SAVE) => app.on_emoji_save(),
                    (0, ID_BE_DISABLE) => app.on_emoji_toggle_row(),
                    (0, ID_BE_RESTORE) => app.on_emoji_restore(),
                    (0, ID_BE_TOGGLE) => app.on_emoji_toggle(),
                    (0, ID_BE_OPEN) => app.on_emoji_open(),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            let hdr = unsafe { &*(lparam.0 as *const windows::Win32::UI::Controls::NMHDR) };
            if let Some(app) = unsafe { app_of(hwnd) } {
                if hdr.code == NM_CUSTOMDRAW {
                    // 三个列表共用一套：选中行浅蓝底，失焦也不淡
                    return app.list_custom_draw(lparam);
                }
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
                } else if hdr.code == LVN_ITEMCHANGED && hdr.hwndFrom == app.emoji_list {
                    // 三个列表同理：emoji 面板挂的是自己的选中行
                    if let Some(idx) = app.selected_emoji() {
                        if app.emoji_sel != Some(idx) {
                            app.emoji_sel = Some(idx);
                            app.fill_emoji_panel(Some(idx));
                        }
                    }
                } else if hdr.code == NM_CLICK && hdr.hwndFrom == app.emoji_list {
                    // 再点**已经选中**的那一行：把面板还原成该行的值（v1.67）。
                    // `LVN_ITEMCHANGED` 只在选中项真的换了才来，清空「绑到」之后
                    // 点原来那行不会有任何消息，面板就一直空着——真机上反馈为
                    // 「删除输入后重新点列表项也不恢复」。
                    let nm = unsafe { &*(lparam.0 as *const NMITEMACTIVATE) };
                    app.on_emoji_row_click(nm.iItem);
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
        // 段选控件只在通用页存在。`geo.seg` 在别的页上仍然是通用页那次的
        // 矩形（布局函数没清零），所以**必须**带页判断：否则在 emoji 页
        // 划过那块空白也会让 seg_hot 变化、白白重绘一次（实测整窗上下扫，
        // 编辑面板那一列 3 轮下来多出 7 次重绘）。
        let seg = if self.page == PAGE_GENERAL && pt_in(pt, self.geo.seg) {
            let seg_w = (self.geo.seg.right - self.geo.seg.left) / 3;
            ((pt.x - self.geo.seg.left) / seg_w.max(1)).clamp(0, 2)
        } else {
            -1
        };
        let tab_changed = tab != self.tab_hot;
        let seg_changed = seg != self.seg_hot;
        self.tab_hot = tab;
        self.seg_hot = seg;
        // 只失效真变了的那一小块：hover 换高亮跟整页内容无关，整窗重绘纯属浪费
        if tab_changed {
            self.repaint_tab_bar();
        }
        if seg_changed {
            self.repaint_seg();
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
    ///
    /// **双缓冲**（D92）：所有内容先画进等尺寸的内存 DC，最后一次 `BitBlt` 推上屏。
    /// 直接往窗口 DC 上画时，每一帧的中间态都真的出现在屏幕上——先铺底（整窗变
    /// 成底色灰）、再一块块画卡片、再画文字。重绘本来就不多（改状态、切页、焦点
    /// 变化），但那几次的「先灰一下」肉眼可见，用户会描述成「闪」。
    /// 内存 DC 没有裁剪区，画完按窗口 DC 的更新区（`WS_CLIPCHILDREN` 已经排除
    /// 子控件）整块贴回去，一帧只露一次。
    fn paint(&self) {
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        if hdc.is_invalid() {
            return;
        }
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let (w, h) = ((rc.right - rc.left).max(1), (rc.bottom - rc.top).max(1));
        // 建不出来就退回直画：宁可闪，也不能白屏
        let mem = unsafe { CreateCompatibleDC(Some(hdc)) };
        let bmp = unsafe { CreateCompatibleBitmap(hdc, w, h) };
        let use_mem = !mem.is_invalid() && !bmp.is_invalid();
        let old = if use_mem {
            unsafe { SelectObject(mem, bmp.into()) }
        } else {
            HGDIOBJ::default()
        };
        let target = if use_mem { mem } else { hdc };
        unsafe {
            // 有 WS_CLIPCHILDREN，子控件占的地方会被裁掉，不会把 Edit 白底刷成本底色
            let _ = FillRect(target, &rc, self.brush_bg);
        }
        self.draw_tab_bar(target);
        match self.page {
            PAGE_GENERAL => self.draw_general(target),
            PAGE_WORDS => self.draw_words(target),
            PAGE_EMOJI => self.draw_emoji(target),
            _ => self.draw_learn(target),
        }
        if use_mem {
            unsafe {
                let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
                let _ = SelectObject(mem, old);
                let _ = DeleteObject(bmp.into());
                let _ = DeleteDC(mem);
            }
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

    /// emoji 页：搜索框描边 + 列表卡 + 面板卡（选中信息 + 新 emoji 框描边）。
    fn draw_emoji(&self, hdc: HDC) {
        self.draw_page_head(hdc, "emoji", "候选改绑");
        // 搜索框 + 标签：与学习页同一摆位同一画法
        let border = if self.focus_id == ID_E_EMOJI_SEARCH {
            self.colors.accent
        } else {
            self.colors.input_border
        };
        round_rect_w(
            hdc,
            self.geo.emoji_search_box,
            self.colors.card,
            border,
            self.s(4),
            self.s(1),
        );
        draw_text_center(
            hdc,
            "搜索",
            Self::rect(
                self.geo.emoji_search_box.left - self.s(36),
                self.geo.emoji_search_box.top,
                self.s(32),
                self.geo.emoji_search_box.bottom - self.geo.emoji_search_box.top,
            ),
            self.fonts.caption,
            self.colors.text2,
        );
        round_rect_w(
            hdc,
            self.geo.emoji_card,
            self.colors.card,
            self.colors.card_border,
            self.s(8),
            self.s(1),
        );
        round_rect_w(
            hdc,
            self.geo.emoji_panel_card,
            self.colors.card,
            self.colors.card_border,
            self.s(8),
            self.s(1),
        );
        // 面板行 1：临时提示（改绑被拒 / 输入不合格）压过选中信息——它才是
        // 用户下一次动作要看的；错误红字，与「应用」页非法输入同一个颜色。
        let (msg, msg_color) = match self.emoji_panel_alert() {
            Some(m) => (m, self.colors.error),
            None => (self.emoji_info_text(), self.colors.text2),
        };
        draw_text(
            hdc,
            &msg,
            self.geo.emoji_sel_label,
            self.fonts.caption,
            msg_color,
        );
        // 面板行 2：两个输入框的标签 + 描边（无边框 Edit 的框画在卡片上）
        draw_text_align(
            hdc,
            "绑到",
            self.geo.emoji_target_label,
            self.fonts.caption,
            self.colors.text2,
            true,
        );
        let border = if self.focus_id == ID_E_EMOJI_TARGET {
            self.colors.accent
        } else {
            self.colors.input_border
        };
        round_rect_w(
            hdc,
            self.geo.emoji_target_box,
            self.colors.card,
            border,
            self.s(4),
            self.s(1),
        );
        draw_text_align(
            hdc,
            "emoji",
            self.geo.emoji_new_label,
            self.fonts.caption,
            self.colors.text2,
            true,
        );
        let border = if self.focus_id == ID_E_EMOJI_NEW {
            self.colors.accent
        } else {
            self.colors.input_border
        };
        round_rect_w(
            hdc,
            self.geo.emoji_new_box,
            self.colors.card,
            border,
            self.s(4),
            self.s(1),
        );
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
        ID_B_APPLY | ID_B_SAVE | ID_BE_SAVE => ButtonKind::Primary,
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

/// 按钮标题（自绘按钮从窗口文本取字，见 `draw_button`）。
///
/// 文字变了才写：调用点每次刷新都跑（选中行变化、输入变化），无谓的
/// `WM_SETTEXT` 会让自绘按钮连着重画。写之前还要在窗口文本没变时**别**重绘——
/// `SetWindowTextW` 对 owner-draw 按钮不保证触发 `WM_DRAWITEM`。
fn set_button_text(hwnd: HWND, s: &str) {
    if get_text(hwnd) == s {
        return;
    }
    set_text(hwnd, s);
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, true);
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
        // 标题带版本号：装着哪一个版本，一眼就能看出来。版本号只从 Cargo 取
        // （`version.workspace = true`），不在这里写第二份——重装后想确认
        // 「到底装上没」时，这个位置是最省事的一处（§10.1）。
        let title = wide(&format!("亨利五笔 · 设置 · v{}", env!("CARGO_PKG_VERSION")));
        let hwnd = CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
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
        .map_err(|e| format!("建窗失败：{e}"))?;

        // 窗口图标取 exe 里的 id=1 资源（`resources.rc`，build.rs 用 rc.exe 编进去）。
        //
        // **光嵌进 exe 不够**：Windows「设置 → 应用」读的是 `UninstallDisplayIcon`
        // 指向的 exe 资源，而标题栏 / 任务栏 / Alt+Tab 读的是**窗口图标**——后者默认
        // 来自窗口类的 `hIcon`，没设就是系统默认图标。两处来源不同，所以会出现
        // 「应用列表里有图标、标题栏左上角还是个方块」这种半对半错。
        //
        // 分大小两档发 `WM_SETICON`：`ICON_SMALL` 给标题栏，`ICON_BIG` 给任务栏与
        // Alt+Tab。尺寸取 `GetSystemMetrics`，程序是 PerMonitorV2，高 DPI 下才对得上。
        //
        // 资源缺失（构建机没有 rc.exe，build.rs 只警告不失败）时 `LoadImageW` 返回
        // 错误，这里跳过，退回系统默认图标——不影响任何功能。
        // `LR_SHARED`：句柄由系统缓存管理，不需要 `DestroyIcon`。
        let icon_res = PCWSTR(1 as *const u16); // MAKEINTRESOURCEW(1)
        for (which, cx, cy) in [
            (ICON_SMALL, SM_CXSMICON, SM_CYSMICON),
            (ICON_BIG, SM_CXICON, SM_CYICON),
        ] {
            if let Ok(icon) = LoadImageW(
                Some(HINSTANCE(hinstance.0)),
                icon_res,
                IMAGE_ICON,
                GetSystemMetrics(cx),
                GetSystemMetrics(cy),
                LR_DEFAULTCOLOR | LR_SHARED,
            ) {
                let _ = SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(which as usize)),
                    Some(LPARAM(icon.0 as isize)),
                );
            }
        }
        Ok(hwnd)
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
pub fn selftest_ui(_data: Option<PathBuf>) -> Result<(), String> {
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
    // emoji 页的两份映射也是临时造的（自测改用户的真 emoji 绑定同样不可接受）。
    // `data` 指到自测目录后 find_data_dir 就不会摸到仓库里的真数据，
    // 四页的断言都建立在已知夹具上。scheme.toml 不存在 → 词库降级，
    // 词组页/emoji 页不依赖它。
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir).map_err(|e| format!("造数据目录失败：{e}"))?;
    std::fs::write(
        data_dir.join("emoji.toml"),
        "[[emoji]]\nword = \"你好\"\nemoji = \"👋\"\n\n[[emoji]]\nword = \"好评\"\nemoji = \"👍\"\n",
    )
    .map_err(|e| format!("造 emoji 内置表失败：{e}"))?;
    let emoji_path = dir.join("emoji.user.toml");
    std::fs::write(
        &emoji_path,
        "[[emoji]]\nword = \"你好\"\nemoji = \"👏\"\n\n[[emoji]]\nword = \"好\"\nemoji = \"👍\"\n",
    )
    .map_err(|e| format!("造 emoji 用户层失败：{e}"))?;

    let mut app = Box::new(App::new(
        Some(data_dir.clone()),
        Some(path.clone()),
        Some(cfg_path.clone()),
        Some(db_path.clone()),
        Some(emoji_path.clone()),
    )?);
    let hwnd = create_window(&app)?;
    // 不抢焦点地显示一下：真实路径里窗口是可见的，z 序表现才一致
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    drain_messages();

    let result = (|| -> Result<(), String> {
        // 四页各查一次：z 序（Tab 在底）+ 命中（点得到）+ 页内控件归属。
        // 页是显隐切换的，隐藏页的控件命中测试必然失败，只能切过去再查。
        check_z_order(hwnd, PAGE_GENERAL)?;
        check_page_scope(&app, PAGE_GENERAL)?;
        check_general_flow(&app, &cfg_path)?;

        switch_page(&mut app, PAGE_WORDS);
        check_z_order(hwnd, PAGE_WORDS)?;
        check_page_scope(&app, PAGE_WORDS)?;
        check_panel_flow(&app, &path)?;

        switch_page(&mut app, PAGE_EMOJI);
        check_z_order(hwnd, PAGE_EMOJI)?;
        check_page_scope(&app, PAGE_EMOJI)?;
        check_emoji_page(&mut app, &emoji_path)?;

        switch_page(&mut app, PAGE_LEARN);
        check_z_order(hwnd, PAGE_LEARN)?;
        check_page_scope(&app, PAGE_LEARN)?;
        check_learn_page(&app)?;

        switch_page(&mut app, PAGE_GENERAL);
        check_z_order(hwnd, PAGE_GENERAL)?;
        check_page_scope(&app, PAGE_GENERAL)?;
        check_hover_repaint(hwnd)
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

/// 鼠标划过编辑面板**不该重绘**（D92，v1.68）。
///
/// 判据直接看窗口的脏区（`GetUpdateRect`）：没有 hot 状态时，`WM_MOUSELEAVE`
/// 不该在窗口上留下任何待画区域。这比截图比对可靠——闪的是一两帧的中间态，
/// 截图恰好拍到「画完了」的那一帧就什么都看不出来。
///
/// 另一半也要测：页签确实亮着时离开，必须把高亮撤掉，否则 hover 底色会留在
/// 屏幕上（这才是这条通知存在的意义）。
///
/// **不接 `&mut App`**：`SendMessage` 会重入窗口过程，那里会通过窗口用户数据再
/// 取一次同一个 `App`。调用方握着 `&mut App` 的同时发生这种重入是别名违规，
/// release 下编译器会把「判断用的读取」提前到调用之前——自测因此在 debug 通过、
/// release 报 `tab=-1 seg=-1` 却判失败（值打印出来是清的，判断用的不是）。
/// 只传句柄、状态一律经窗口用户数据读写，就没有跨 FFI 存活的引用了。
fn check_hover_repaint(hwnd: HWND) -> Result<(), String> {
    let state = || unsafe { app_of(hwnd).map(|a| (a.tab_hot, a.seg_hot)) }.unwrap_or((-1, -1));
    let set = |tab: i32, seg: i32| unsafe {
        if let Some(a) = app_of(hwnd) {
            a.tab_hot = tab;
            a.seg_hot = seg;
        }
    };
    let leave = || unsafe {
        let _ = SendMessageW(hwnd, WM_MOUSELEAVE, Some(WPARAM(0)), Some(LPARAM(0)));
    };
    unsafe {
        let _ = ValidateRect(Some(hwnd), None);
    };
    set(-1, -1);
    leave();
    if has_update_region(hwnd) {
        return Err(
            "鼠标移进输入框（父窗口收到 WM_MOUSELEAVE）时不该重绘整窗——那正是「划过面板会闪」的来源"
                .into(),
        );
    }
    // 页签 hover 着离开：既要清状态，也要重画一次把高亮抹掉
    set(0, -1);
    leave();
    let (tab, seg) = state();
    if tab != -1 || seg != -1 {
        return Err(format!(
            "离开窗口后页签/段选的 hover 状态没清掉（tab={tab} seg={seg}）"
        ));
    }
    if !has_update_region(hwnd) {
        return Err("页签正 hover 着离开时应当重绘一次，把 hover 底色撤掉".into());
    }
    // 重绘范围必须**只有页签那一条**：整窗重绘是这轮要消灭的东西
    let mut dirty = RECT::default();
    if unsafe { GetUpdateRect(hwnd, Some(&mut dirty), false) }.as_bool() {
        let mut client = RECT::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut client);
        }
        // 「只有页签那一条」= 从顶边开始、且没铺到客户区底部
        if dirty.top > 0 || dirty.bottom >= client.bottom {
            return Err(format!(
                "页签 hover 的重绘范围应当只有页签条（实际脏区 {}..{}，客户区高 {}）",
                dirty.top, dirty.bottom, client.bottom
            ));
        }
    }
    unsafe {
        let _ = ValidateRect(Some(hwnd), None);
    };
    Ok(())
}

/// 窗口有没有待画的脏区（`GetUpdateRect` 返回非空即真）。
fn has_update_region(hwnd: HWND) -> bool {
    unsafe { GetUpdateRect(hwnd, None, false).as_bool() }
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

/// 自测用：像用户点空白处那样取消列表选中（面板回到「自己写目标」态）。
fn clear_emoji_sel(app: &mut App) {
    app.emoji_sel = None;
    app.clear_emoji_selection();
    drain_messages();
    app.fill_emoji_panel(None);
    drain_messages();
}

/// emoji 页（M4d / D85）：列表条数、单字条目在列、改绑真落盘、
/// 「停用/恢复内置」按钮的可用性与选中态联动。
///
/// 落盘断言看**磁盘上的文件**（与 `check_panel_flow` 同一口径）：
/// 消息链通了但文件没写等于没做。
fn check_emoji_page(app: &mut App, emoji_path: &std::path::Path) -> Result<(), String> {
    let click = |b: HWND| unsafe {
        send(b, 0x00F5, WPARAM(0), LPARAM(0)); // BM_CLICK
    };
    // 夹具：内置 = {你好 👋, 好评 👍}；用户层 = {你好 → 👏, 好 → 👍}。
    // 合并视图按词排序（Unicode 码位）→ 你好（已改绑）/ 好（新增·单字）/
    // 好评（内置），共 3 行。
    let count = unsafe { send(app.emoji_list, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0)) }.0;
    if count != 3 {
        return Err(format!("emoji 页应列出 3 行合并视图，实际 {count} 条"));
    }
    if cell_text(app.emoji_list, 1, 0) != "好" || cell_text(app.emoji_list, 1, 1) != "按词·单字"
    {
        return Err(format!(
            "第 2 行应是单字「好」（D85 的口子要从列表里看出来），实际 {:?} / {:?}",
            cell_text(app.emoji_list, 1, 0),
            cell_text(app.emoji_list, 1, 1)
        ));
    }
    if cell_text(app.emoji_list, 0, 3) != "已改绑" || cell_text(app.emoji_list, 2, 3) != "内置"
    {
        return Err(format!(
            "状态列应为 已改绑 / 内置，实际 {:?} / {:?}",
            cell_text(app.emoji_list, 0, 3),
            cell_text(app.emoji_list, 2, 3)
        ));
    }

    // 搜索实时过滤：EN_CHANGE 走真实消息链
    set_text(app.e_emoji_search, "好评");
    drain_messages();
    let count = unsafe { send(app.emoji_list, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0)) }.0;
    if count != 1 || cell_text(app.emoji_list, 0, 0) != "好评" {
        return Err(format!("按「好评」过滤应剩 1 条，实际 {count} 条"));
    }
    set_text(app.e_emoji_search, "");
    drain_messages();

    // 「只看我绑的」过滤器（v1.65）：夹具里用户层动过的是 你好（已改绑）与 好
    // （新增），内置未改的只有 好评——开关一亮就该只剩 2 行。
    if !is_enabled(app.b_emoji_only) {
        return Err("「只看我绑的」应当一直可点".into());
    }
    click(app.b_emoji_only);
    drain_messages();
    let count = unsafe { send(app.emoji_list, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0)) }.0;
    if count != 2 {
        return Err(format!(
            "只看我绑的应当只剩 2 行（你好 / 好），实际 {count} 行"
        ));
    }
    for i in 0..count {
        if cell_text(app.emoji_list, i as usize, 3) == "内置" {
            return Err(format!(
                "只看我绑的里不该出现「内置」行，第 {i} 行是 {:?}",
                cell_text(app.emoji_list, i as usize, 0)
            ));
        }
    }
    if !app.emoji_only_user || !get_text(app.b_emoji_only).contains("看全部") {
        return Err(format!(
            "过滤器打开后按钮文字应变成「看全部」，实际 {:?}",
            get_text(app.b_emoji_only)
        ));
    }
    click(app.b_emoji_only);
    drain_messages();
    let count = unsafe { send(app.emoji_list, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0)) }.0;
    if count != 3 || app.emoji_only_user {
        return Err(format!("关掉过滤器后应回到 3 行，实际 {count} 行"));
    }

    // 输入不是 emoji 时的反馈（v1.64）：提示要写在面板行 1（红字），不能只在
    // 窗口底部那行灰字——真机上用户看着输入框，那句提示他没看见，反馈成
    // 「点了改绑没有任何反应」。
    set_text(app.e_emoji_target, "好评");
    set_text(app.e_emoji_new, "好");
    drain_messages();
    let msg = app.emoji_panel_alert().unwrap_or_default();
    if !msg.contains("不是 emoji") {
        return Err(format!(
            "框里填「好」时面板行 1 应当红字说明「不是 emoji」，实际 {msg:?}"
        ));
    }
    let before = std::fs::read_to_string(emoji_path).unwrap_or_default();
    click(app.b_emoji_save);
    drain_messages();
    let after = std::fs::read_to_string(emoji_path).unwrap_or_default();
    if before != after {
        return Err("填了非 emoji 时「保存绑定」不该写盘".into());
    }
    if app.emoji_panel_alert().is_none() {
        return Err("点「保存绑定」被拒后提示不该消失（那正是用户说的「没反应」）".into());
    }
    // 「绑到」看不懂时同样要拦在面板行 1，且不写盘（v1.65）
    set_text(app.e_emoji_target, "好a");
    set_text(app.e_emoji_new, "👍");
    drain_messages();
    if !app
        .emoji_panel_alert()
        .unwrap_or_default()
        .contains("看不懂")
    {
        return Err("「绑到」填「好a」（汉字混字母）时应当提示看不懂".into());
    }
    click(app.b_emoji_save);
    drain_messages();
    if std::fs::read_to_string(emoji_path).unwrap_or_default() != before {
        return Err("「绑到」看不懂时不该写盘".into());
    }
    set_text(app.e_emoji_target, "好评");
    set_text(app.e_emoji_new, "👍");
    drain_messages();
    if let Some(m) = app.emoji_panel_alert() {
        return Err(format!(
            "两个框都填对之后提示应当消失，实际还在：{m}（绑到 {:?} / emoji {:?}）",
            get_text(app.e_emoji_target),
            get_text(app.e_emoji_new)
        ));
    }

    // 改绑：点中「好评」（内置条目）→ 两个框都回填 → 换 emoji → 保存 → 文件里变新值
    select_item(app.emoji_list, 2);
    drain_messages();
    if get_text(app.e_emoji_new) != "👍" || get_text(app.e_emoji_target) != "好评" {
        return Err(format!(
            "点中行后两个框都应回填（绑到 {:?} / emoji {:?}）",
            get_text(app.e_emoji_target),
            get_text(app.e_emoji_new)
        ));
    }
    if is_enabled(app.b_emoji_restore) {
        return Err("没动过的内置条目「恢复内置」应当是灰的".into());
    }
    if !is_enabled(app.b_emoji_disable) {
        return Err("内置条目「停用」应当可点".into());
    }
    set_text(app.e_emoji_new, "🎉");
    drain_messages();
    if !is_enabled(app.b_emoji_save) {
        return Err("填了新 emoji 后「改绑」应当可点".into());
    }
    click(app.b_emoji_save);
    drain_messages();
    let text = std::fs::read_to_string(emoji_path).map_err(|e| format!("回读用户层失败：{e}"))?;
    if !text.contains("🎉") || !text.contains("word = \"好评\"") {
        return Err(format!("改绑后文件里应有「好评 → 🎉」：\n{text}"));
    }
    if !text.contains("word = \"好\"") {
        return Err(format!("改绑不能动别的条目（单字「好」丢了）：\n{text}"));
    }
    if emoji_path.with_extension("toml.tmp").exists() {
        return Err("保存后留下了 .tmp——原子落盘 broken".into());
    }
    if !is_enabled(app.b_emoji_restore) {
        return Err("改绑后「恢复内置」应当可点（删掉用户层条目即回内置）".into());
    }

    // 凭空新建绑定（v1.65）：**不点列表**，直接在「绑到」里写字，选一枚 emoji
    // 保存。这条流程正是当年把 emoji 绑到单字上的唯一通道——列表按词列，
    // 没绑过 emoji 的词不在里面，没有这条路就无处可点。
    clear_emoji_sel(app);
    set_text(app.e_emoji_target, "好运");
    set_text(app.e_emoji_new, "🍀");
    drain_messages();
    if get_text(app.e_emoji_target) != "好运" || app.emoji_sel.is_some() {
        return Err("准备新建绑定时不该还挂着旧的选中行".into());
    }
    click(app.b_emoji_save);
    drain_messages();
    let text = std::fs::read_to_string(emoji_path).map_err(|e| format!("回读用户层失败：{e}"))?;
    if !text.contains("word = \"好运\"") || !text.contains("🍀") {
        return Err(format!("新建绑定后文件里应有「好运 → 🍀」：\n{text}"));
    }
    let count = unsafe { send(app.emoji_list, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0)) }.0;
    if count != 4 {
        return Err(format!("新建绑定后列表应变成 4 行，实际 {count} 行"));
    }
    if app.emoji_sel.is_none() {
        return Err("保存后应当自动选中刚建的那一行（方便接着改）".into());
    }
    if get_text(app.e_emoji_target) != "好运" {
        return Err("保存后「绑到」框应停在刚保存的词上".into());
    }
    // 再存一次同一个词 = 覆盖，不会多出一行
    set_text(app.e_emoji_new, "🥳");
    drain_messages();
    click(app.b_emoji_save);
    drain_messages();
    let text = std::fs::read_to_string(emoji_path).map_err(|e| format!("回读用户层失败：{e}"))?;
    if text.matches("word = \"好运\"").count() != 1 || !text.contains("🥳") {
        return Err(format!("同词再保存应当是覆盖（只留一条 🥳）：\n{text}"));
    }
    let count = unsafe { send(app.emoji_list, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0)) }.0;
    if count != 4 {
        return Err(format!("覆盖保存不该新增行，实际 {count} 行"));
    }
    // —— 纯用户新增的行：停用 / 启用 / 删除绑定（v1.67）——
    // 真机反馈：给单字绑了一枚想取消，选中那行「停用」是灰的，也没有别的地方可点；
    // 而且清空「绑到」再点同一行，面板不会还原（选中项没变 → 没有 ITEMCHANGED）。
    let idx = app.emoji_sel.expect("刚才保存后应当选中「好运」这一行");
    if !app.emoji_rows[idx].is_user_only() {
        return Err("「好运」是用户新增的绑定，is_user_only 应当为真".into());
    }
    if !is_enabled(app.b_emoji_disable) {
        return Err("用户新增的行「停用」不该是灰的（那正是没法取消绑定的原因）".into());
    }
    if get_text(app.b_emoji_restore) != "删除绑定" {
        return Err(format!(
            "纯用户新增的行，第三个按钮应叫「删除绑定」，实际 {:?}",
            get_text(app.b_emoji_restore)
        ));
    }
    // 清空两个框，再点同一行 → 面板还原成该行的值。
    // 发**真的 `WM_NOTIFY(NM_CLICK)`**（不是直接调处理函数）：接线本身
    // （按 `hdr.hwndFrom` 认列表、从 `NMITEMACTIVATE` 取 `iItem`）也一并钉住。
    set_text(app.e_emoji_target, "");
    set_text(app.e_emoji_new, "");
    drain_messages();
    {
        let mut nm = NMITEMACTIVATE::default();
        nm.hdr.hwndFrom = app.emoji_list;
        nm.hdr.idFrom = ID_EMOJI_LIST;
        nm.hdr.code = NM_CLICK;
        nm.iItem = idx as i32;
        unsafe {
            let _ = SendMessageW(
                app.hwnd,
                WM_NOTIFY,
                Some(WPARAM(ID_EMOJI_LIST)),
                Some(LPARAM(&nm as *const NMITEMACTIVATE as isize)),
            );
        }
    }
    drain_messages();
    if get_text(app.e_emoji_target) != "好运" || get_text(app.e_emoji_new) != "🥳" {
        return Err(format!(
            "再点同一行应把面板还原成该行的值（实际 绑到 {:?} / emoji {:?}）",
            get_text(app.e_emoji_target),
            get_text(app.e_emoji_new)
        ));
    }

    // 停用：用户新增的条目也留得住——文件里 enabled = false 且 emoji 还在
    click(app.b_emoji_disable);
    drain_messages();
    let text = std::fs::read_to_string(emoji_path).map_err(|e| format!("回读用户层失败：{e}"))?;
    if !text.contains("word = \"好运\"")
        || !text.contains("🥳")
        || !text.contains("enabled = false")
    {
        return Err(format!(
            "用户新增的绑定停用后应是「条目还在 + enabled = false」：\n{text}"
        ));
    }
    if app.emoji_rows[app.emoji_sel.unwrap()].state_cell() != "已停用" {
        return Err("停用后那一行的状态列应当是「已停用」".into());
    }
    if get_text(app.b_emoji_disable) != "启用" {
        return Err(format!(
            "停用之后同一个按钮应当变成「启用」，实际 {:?}",
            get_text(app.b_emoji_disable)
        ));
    }
    // 启用：回来还是同一枚，状态回「新增」
    click(app.b_emoji_disable);
    drain_messages();
    let text = std::fs::read_to_string(emoji_path).map_err(|e| format!("回读用户层失败：{e}"))?;
    if text.contains("enabled = false") {
        return Err(format!("启用后文件里不该再有 enabled = false：\n{text}"));
    }
    if app.emoji_rows[app.emoji_sel.unwrap()].state_cell() != "新增" {
        return Err("启用后那一行的状态列应当回到「新增」".into());
    }

    // 删除绑定：条目整条消失，行也没了（内置没有这条，删了就是解绑）
    click(app.b_emoji_restore);
    drain_messages();
    let text = std::fs::read_to_string(emoji_path).map_err(|e| format!("回读用户层失败：{e}"))?;
    if text.contains("好运") {
        return Err(format!("删除绑定后用户层里不该再有「好运」：\n{text}"));
    }
    if app.emoji_rows.iter().any(|r| r.key_cell() == "好运") {
        return Err("删除绑定后列表里不该还有「好运」这一行".into());
    }
    if app.emoji_sel.is_some() {
        return Err("删掉选中行之后面板应当清空（不然按钮还指着一条不存在的绑定）".into());
    }

    // 回到「好评」继续测停用/恢复
    select_item(app.emoji_list, 2);
    drain_messages();

    // 停用：文件里出现 enabled = false，再「恢复内置」把它删掉
    click(app.b_emoji_disable);
    drain_messages();
    let text = std::fs::read_to_string(emoji_path).map_err(|e| format!("回读用户层失败：{e}"))?;
    if !text.contains("enabled = false") {
        return Err(format!("停用后文件里应有 enabled = false：\n{text}"));
    }
    click(app.b_emoji_restore);
    drain_messages();
    let text = std::fs::read_to_string(emoji_path).map_err(|e| format!("回读用户层失败：{e}"))?;
    if text.contains("好评") {
        return Err(format!("恢复内置后用户层里不该再有「好评」：\n{text}"));
    }
    if !text.contains("word = \"好\"") {
        return Err(format!("恢复内置不能动别的条目：\n{text}"));
    }

    // 总开关：关 = 写 emoji.enabled = false，开 = 删行（默认值不落盘）
    click(app.b_emoji_toggle);
    drain_messages();
    let cfg_text =
        std::fs::read_to_string(&app.config_path).map_err(|e| format!("回读配置失败：{e}"))?;
    if !cfg_text.contains("enabled = false") {
        return Err(format!(
            "关闭后配置里应有 [emoji] enabled = false：\n{cfg_text}"
        ));
    }
    click(app.b_emoji_toggle);
    drain_messages();
    let cfg_text =
        std::fs::read_to_string(&app.config_path).map_err(|e| format!("回读配置失败：{e}"))?;
    if cfg_text.contains("enabled") {
        return Err(format!(
            "开启 = 删行，配置里不该再有 emoji 的 enabled：\n{cfg_text}"
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

/// 切到 `page` 之后，屏幕上**能看到的子控件必须都属于这一页**。
///
/// 这是「控件忘了登记进 `pages[n]`」的唯一自动检法：`check_z_order` 只按 id 找本页
/// 那几个控件、量它们没被挡住——漏登记的控件待在别处，照样能全过。真机上表现为
/// 「每一页都悬着同一个按钮」（`b_emoji_browse` 就漏过一次）。
fn check_page_scope(app: &App, page: usize) -> Result<(), String> {
    let allowed = &app.pages[page];
    for h in children(app.hwnd) {
        if !unsafe { IsWindowVisible(h).as_bool() } {
            continue;
        }
        // 状态栏是**刻意**全页共用的（「刚刚发生了什么」的出口，切页不变），
        // 它不登记在任何一页里。
        if h == app.status || allowed.contains(&h) {
            continue;
        }
        return Err(format!(
            "第 {page} 页上有一个不属于本页的可见控件 id {}（漏登记进 pages[{page}]？）",
            id_of(h)
        ));
    }
    Ok(())
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
        PAGE_EMOJI => &[
            ID_EMOJI_LIST,
            ID_E_EMOJI_SEARCH,
            ID_E_EMOJI_TARGET,
            ID_E_EMOJI_NEW,
            ID_BE_BROWSE,
            ID_BE_SAVE,
            ID_BE_DISABLE,
            ID_BE_RESTORE,
            ID_BE_TOGGLE,
            ID_BE_ONLY,
            ID_BE_OPEN,
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
