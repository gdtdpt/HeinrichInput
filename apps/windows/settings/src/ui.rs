//! 设置窗口：主窗口 + Tab（通用 / 词组 / 学习数据）。
//!
//! 词组页写 `%APPDATA%\Heinrich\words.tsv`，通用页写同目录的 `config.toml`，
//! 学习数据页只读（外加清零）`%LOCALAPPDATA%\Heinrich\learn.db`。三者都**不经过
//! 任何进程间通信**：server 每秒 stat 一次前两个、每次查询直接读第三个（D65），
//! 所以这个 exe 可以随时开、随时关，改完不用重启任何东西。
//!
//! 为什么用原生控件而不是复用 `heinrich-win-ui` 的自绘：编辑框要能**用亨利自己
//! 打中文**（单字都在词库里），自绘就得连光标、选区、IME 组合串一起实现；而这张
//! 表的重点是把功能跑通，观感可以以后再统一（M4 之后仍可只换绘制层）。
//!
//! **Tab 必须永远压在 z 序最底层**（[`lower_tab`]）：`SysTabControl32` 拿到焦点
//! 时会把自己抬到最上层，而它的矩形正好覆盖整个内容区——一旦抬上来，它就把下面
//! 所有按钮和编辑框整片盖住，鼠标点哪儿都被它吃掉。界面看起来完全正常，只是所有
//! 点击都石沉大海（F44：验收时「新增/保存点了没反应、词也没真加上」就是这个）。
//!
//! 多页的代价：三页的控件都是主窗口的子窗口，靠显隐切换（原生 Tab 没有「页」这个
//! 对象）。所以每条 WM_COMMAND / WM_NOTIFY 都要看清是谁发的——两个 ListView 同时
//! 存在，只按消息类型分派会把学习数据的行当成词组去改。

use std::path::PathBuf;

use heinrich_core::RankingConfig;
use heinrich_scheme::user::UserWord;
use heinrich_scheme::HintMode;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, GetDC, GetSysColorBrush, ReleaseDC, ScreenToClient, SetBkMode, HFONT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OPENFILENAMEW};
use windows::Win32::UI::Controls::{
    LVCOLUMNW, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_DELETEITEM,
    LVM_ENSUREVISIBLE, LVM_GETITEMCOUNT, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMW, LVNI_SELECTED, LVN_ITEMCHANGED,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Shell::{
    DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetClientRect,
    GetMessageW, GetParent, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, LoadCursorW, MessageBoxW, MoveWindow,
    PostQuitMessage, RealChildWindowFromPoint, RegisterClassW, SendMessageW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, ShowWindowAsync, TranslateMessage,
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_ID, GWLP_USERDATA, GWL_STYLE,
    GW_CHILD, GW_HWNDNEXT, HWND_BOTTOM, IDC_ARROW, IDYES, MB_ICONINFORMATION, MB_ICONWARNING,
    MB_OK, MB_YESNO, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_RESTORE, SW_SHOW,
    SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WINDOW_STYLE, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_KEYDOWN, WM_NCCREATE, WM_NCDESTROY, WM_NOTIFY, WM_SETFOCUS,
    WM_SETFONT, WM_SIZE, WNDCLASSW, WNDPROC, WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP,
    WS_VISIBLE,
};

use crate::lex::{find_data_dir, Lexicons};
use crate::model::{Model, Row};

// 窗口类名不在这里写死：见 `create_window` 里对 `heinrich_tip_id::SETTINGS_CLASS` 的说明。

// Static 的两个样式位（`SS_CENTER` / `SS_CENTERIMAGE`）。它们在 windows crate 的
// `Win32_System_SystemServices` 里，那个 feature 没开、为一对常量开一整个模块不值，
// 直接照 winuser.h 写字面量。
const SS_CENTER: u32 = 0x0000_0001;
const SS_CENTERIMAGE: u32 = 0x0000_0200;

const ID_TAB: usize = 100;
const ID_LIST: usize = 101;
const ID_E_WORD: usize = 110;
const ID_E_PINYIN: usize = 111;
const ID_E_CODE: usize = 112;
const ID_E_WEIGHT: usize = 113;
const ID_L_WORD: usize = 120;
const ID_L_PINYIN: usize = 121;
const ID_L_CODE: usize = 122;
const ID_L_WEIGHT: usize = 123;
const ID_B_CLEAR: usize = 130;
const ID_B_SAVE: usize = 131;
const ID_B_DEL: usize = 132;
const ID_B_IMPORT: usize = 133;
const ID_B_OPEN: usize = 134;
const ID_B_UNDO: usize = 135;
const ID_NOTE: usize = 140;
const ID_STATUS: usize = 141;

// 通用页
const ID_L_HINT: usize = 200;
const ID_C_HINT: usize = 201;
const ID_N_HINTDESC: usize = 202;
const ID_L_GAIN: usize = 203;
const ID_E_GAIN: usize = 204;
const ID_N_GAINDESC: usize = 205;
const ID_L_HALF: usize = 206;
const ID_E_HALF: usize = 207;
const ID_N_HALFDESC: usize = 208;
const ID_B_APPLY: usize = 209;
const ID_B_DEFAULT: usize = 210;
const ID_N_CONFIG: usize = 211;
const ID_N_TIPS: usize = 212;

// 学习数据页
const ID_L_SEARCH: usize = 300;
const ID_E_SEARCH: usize = 301;
const ID_B_SEARCH: usize = 302;
const ID_LEARN_LIST: usize = 303;
const ID_B_FORGET: usize = 304;
const ID_N_LEARN: usize = 305;

/// 页序号。**与 Tab 里的顺序、`pages` 数组的下标三者必须一致**。
const PAGE_GENERAL: usize = 0;
const PAGE_WORDS: usize = 1;
const PAGE_LEARN: usize = 2;
const PAGE_COUNT: usize = 3;
const PAGE_TITLES: [&str; PAGE_COUNT] = ["通用", "词组", "学习数据"];

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
/// `TCN_SELCHANGE = TCN_FIRST - 1`，`TCN_FIRST = -550`。
const TCN_SELCHANGE: u32 = 0xFFFF_FDD9;
/// `CBN_SELCHANGE = 1`：下拉框换了选项，走的是 `WM_COMMAND` 的通知码。
const CBN_SELCHANGE: u32 = 1;
/// `CBS_DROPDOWNLIST`：只能选、不能输入——这三个值是封闭集合，让人敲字只会敲出
/// 一个解析不了的值。
const CBS_DROPDOWNLIST: u32 = 0x0003;
const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_SETCURSEL: u32 = 0x014E;
/// `TCM_SETCURSEL` / `TCM_GETCURSEL`：切页是程序驱动的（启动时选第一页）。
const TCM_GETCURSEL: u32 = 0x130B;
const TCM_SETCURSEL: u32 = 0x130C;

/// 窗口内容的最小尺寸（96 DPI 下的逻辑像素），窄于这个就压缩不动了。
const MIN_W: i32 = 720;
const MIN_H: i32 = 460;

pub struct App {
    hwnd: HWND,
    dpi: f32,
    font: HFONT,
    tab: HWND,
    list: HWND,
    e_word: HWND,
    e_pinyin: HWND,
    e_code: HWND,
    e_weight: HWND,
    l_word: HWND,
    l_pinyin: HWND,
    l_code: HWND,
    l_weight: HWND,
    b_clear: HWND,
    b_save: HWND,
    b_del: HWND,
    b_import: HWND,
    b_open: HWND,
    b_undo: HWND,
    note: HWND,
    status: HWND,

    // 通用页
    c_hint: HWND,
    l_hint: HWND,
    n_hint_desc: HWND,
    l_gain: HWND,
    e_gain: HWND,
    n_gain_desc: HWND,
    l_half: HWND,
    e_half: HWND,
    n_half_desc: HWND,
    b_apply: HWND,
    b_default: HWND,
    n_config: HWND,
    n_tips: HWND,

    // 学习数据页
    l_search: HWND,
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
        let font = unsafe {
            CreateFontW(
                -(14.0 * dpi) as i32,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                windows::Win32::Graphics::Gdi::FONT_CHARSET(1), // DEFAULT_CHARSET
                windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
                windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
                windows::Win32::Graphics::Gdi::FONT_QUALITY(5), // CLEARTYPE_QUALITY
                0,
                w!("Microsoft YaHei UI"),
            )
        };
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
            font,
            tab: HWND::default(),
            list: HWND::default(),
            e_word: HWND::default(),
            e_pinyin: HWND::default(),
            e_code: HWND::default(),
            e_weight: HWND::default(),
            l_word: HWND::default(),
            l_pinyin: HWND::default(),
            l_code: HWND::default(),
            l_weight: HWND::default(),
            b_clear: HWND::default(),
            b_save: HWND::default(),
            b_del: HWND::default(),
            b_import: HWND::default(),
            b_open: HWND::default(),
            b_undo: HWND::default(),
            note: HWND::default(),
            status: HWND::default(),
            c_hint: HWND::default(),
            l_hint: HWND::default(),
            n_hint_desc: HWND::default(),
            l_gain: HWND::default(),
            e_gain: HWND::default(),
            n_gain_desc: HWND::default(),
            l_half: HWND::default(),
            e_half: HWND::default(),
            n_half_desc: HWND::default(),
            b_apply: HWND::default(),
            b_default: HWND::default(),
            n_config: HWND::default(),
            n_tips: HWND::default(),
            l_search: HWND::default(),
            e_search: HWND::default(),
            b_search: HWND::default(),
            learn_list: HWND::default(),
            b_forget: HWND::default(),
            n_learn: HWND::default(),
            pages: [Vec::new(), Vec::new(), Vec::new()],
            page: PAGE_GENERAL,
            initial_page: PAGE_GENERAL,
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
        let hwnd = self.hwnd;
        self.tab = child(
            hwnd,
            w!("SysTabControl32"),
            "",
            WS_TABSTOP,
            ID_TAB,
            self.font,
        )?;
        // 见模块头的 F44：Tab 一拿到焦点就自己浮到最上层，把内容区全盖住。
        // 这里挂一个子类化过程，等它自己抬完再把它压回去（顺序不能反）。
        unsafe {
            let _ = SetWindowSubclass(self.tab, Some(tab_proc), 1, 0);
        }
        // 页签顺序 = `PAGE_*` 常量的顺序 = `pages` 的下标，三者错一个就会「点通用
        // 出来的是词组」。启动默认选第一个（通用）——见 `show_page` 的调用点。
        for (i, title) in PAGE_TITLES.iter().enumerate() {
            let mut item = vec![0u16; 32];
            put_utf16(&mut item, title);
            let mut tc = windows::Win32::UI::Controls::TCITEMW {
                mask: windows::Win32::UI::Controls::TCITEMHEADERA_MASK(0x0001), // TCIF_TEXT
                pszText: PWSTR(item.as_mut_ptr()),
                ..Default::default()
            };
            unsafe {
                send(
                    self.tab,
                    windows::Win32::UI::Controls::TCM_INSERTITEMW,
                    WPARAM(i),
                    LPARAM(&mut tc as *mut _ as isize),
                );
            }
        }

        self.create_words_page()?;
        self.create_general_page()?;
        self.create_learn_page()?;
        Ok(())
    }

    /// 词组页：列表 + 编辑面板 + 文件级操作。
    fn create_words_page(&mut self) -> windows::core::Result<()> {
        let hwnd = self.hwnd;
        self.list = child(
            hwnd,
            w!("SysListView32"),
            "",
            WINDOW_STYLE(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | WS_BORDER.0
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
        for (i, (title, width)) in [
            ("词", 140),
            ("拼音", 190),
            ("码", 90),
            ("权重", 110),
            ("提示", 250),
        ]
        .into_iter()
        .enumerate()
        {
            let mut text = vec![0u16; 32];
            put_utf16(&mut text, title);
            let mut col = LVCOLUMNW {
                mask: windows::Win32::UI::Controls::LVCOLUMNW_MASK(LVCF_WIDTH | LVCF_TEXT),
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

        // 字段名居中（`SS_CENTER` + `SS_CENTERIMAGE`：水平、垂直都居中）。
        // 默认的 Static 是左上对齐，一行高 26px 的框里字贴着上沿、又紧靠左边框，
        // 和右边同样高的编辑框摆在一起看着像错位。
        let label_style = WINDOW_STYLE(SS_CENTER | SS_CENTERIMAGE);
        self.l_word = child(hwnd, w!("Static"), "词", label_style, ID_L_WORD, self.font)?;
        self.l_pinyin = child(
            hwnd,
            w!("Static"),
            "拼音",
            label_style,
            ID_L_PINYIN,
            self.font,
        )?;
        self.l_code = child(hwnd, w!("Static"), "码", label_style, ID_L_CODE, self.font)?;
        self.l_weight = child(
            hwnd,
            w!("Static"),
            "权重",
            label_style,
            ID_L_WEIGHT,
            self.font,
        )?;

        let edit_style =
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_TABSTOP.0 | 0x0080); // ES_AUTOHSCROLL
        self.e_word = child(hwnd, w!("Edit"), "", edit_style, ID_E_WORD, self.font)?;
        self.e_pinyin = child(hwnd, w!("Edit"), "", edit_style, ID_E_PINYIN, self.font)?;
        self.e_code = child(hwnd, w!("Edit"), "", edit_style, ID_E_CODE, self.font)?;
        self.e_weight = child(hwnd, w!("Edit"), "", edit_style, ID_E_WEIGHT, self.font)?;

        // 「新增」被去掉了：编辑面板一直可输入，点「保存」就是加入/更新，
        // 多一步「先点新增」只会让人以为词已经进列表了。
        //
        // 建子控件的顺序 = 视觉顺序 = Tab 键走过去的顺序，所以「保存」必须第一个建。
        // 「清空」是次要动作（只清面板、不写文件），排到行尾。
        self.b_save = child(hwnd, w!("Button"), "保存", WS_TABSTOP, ID_B_SAVE, self.font)?;
        self.b_del = child(hwnd, w!("Button"), "删除", WS_TABSTOP, ID_B_DEL, self.font)?;
        self.b_import = child(
            hwnd,
            w!("Button"),
            "批量导入…",
            WS_TABSTOP,
            ID_B_IMPORT,
            self.font,
        )?;
        self.b_open = child(
            hwnd,
            w!("Button"),
            "打开文件位置",
            WS_TABSTOP,
            ID_B_OPEN,
            self.font,
        )?;
        self.b_undo = child(
            hwnd,
            w!("Button"),
            "撤销删除",
            WS_TABSTOP,
            ID_B_UNDO,
            self.font,
        )?;
        self.b_clear = child(
            hwnd,
            w!("Button"),
            "清空",
            WS_TABSTOP,
            ID_B_CLEAR,
            self.font,
        )?;
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
            self.font,
        )?;
        // 状态栏三页共用：它是「刚刚发生了什么」的唯一出口，切页时不跟着变。
        self.status = child(
            hwnd,
            w!("Static"),
            "",
            WINDOW_STYLE(0),
            ID_STATUS,
            self.font,
        )?;

        self.pages[PAGE_WORDS] = vec![
            self.list,
            self.l_word,
            self.l_pinyin,
            self.l_code,
            self.l_weight,
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

    /// 通用页：码提示时机 + 排序两参数 + 应用/恢复默认。
    ///
    /// 这一页**没有**「即时生效」：三个框都可能处于半输入状态（`0.` 还没打完），
    /// 边打边写盘会把中间态写进配置。所以统一走「应用」，Enter 也等于应用。
    fn create_general_page(&mut self) -> windows::core::Result<()> {
        let hwnd = self.hwnd;
        let label_style = WINDOW_STYLE(SS_CENTER | SS_CENTERIMAGE);
        let plain = WINDOW_STYLE(0);
        let edit_style =
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_TABSTOP.0 | 0x0080); // ES_AUTOHSCROLL

        self.l_hint = child(
            hwnd,
            w!("Static"),
            "码提示时机",
            label_style,
            ID_L_HINT,
            self.font,
        )?;
        self.c_hint = child(
            hwnd,
            w!("ComboBox"),
            "",
            WINDOW_STYLE(WS_TABSTOP.0 | CBS_DROPDOWNLIST),
            ID_C_HINT,
            self.font,
        )?;
        for label in [
            "总是显示（mixed）",
            "只在第一次（first_time）",
            "不显示（off）",
        ] {
            let text = wide(label);
            unsafe {
                send(
                    self.c_hint,
                    CB_ADDSTRING,
                    WPARAM(0),
                    LPARAM(text.as_ptr() as isize),
                );
            }
        }
        self.n_hint_desc = child(hwnd, w!("Static"), "", plain, ID_N_HINTDESC, self.font)?;

        self.l_gain = child(
            hwnd,
            w!("Static"),
            "学习增益 g",
            label_style,
            ID_L_GAIN,
            self.font,
        )?;
        self.e_gain = child(hwnd, w!("Edit"), "", edit_style, ID_E_GAIN, self.font)?;
        self.n_gain_desc = child(hwnd, w!("Static"), "", plain, ID_N_GAINDESC, self.font)?;

        self.l_half = child(
            hwnd,
            w!("Static"),
            "半衰期 H（天）",
            label_style,
            ID_L_HALF,
            self.font,
        )?;
        self.e_half = child(hwnd, w!("Edit"), "", edit_style, ID_E_HALF, self.font)?;
        self.n_half_desc = child(hwnd, w!("Static"), "", plain, ID_N_HALFDESC, self.font)?;

        self.b_apply = child(
            hwnd,
            w!("Button"),
            "应用",
            WS_TABSTOP,
            ID_B_APPLY,
            self.font,
        )?;
        self.b_default = child(
            hwnd,
            w!("Button"),
            "恢复默认",
            WS_TABSTOP,
            ID_B_DEFAULT,
            self.font,
        )?;
        self.n_config = child(hwnd, w!("Static"), "", plain, ID_N_CONFIG, self.font)?;
        self.n_tips = child(
            hwnd,
            w!("Static"),
            "这三项写在 config.toml 里，点「应用」后 1 秒内生效，不用重启任何东西。\n\
             「恢复默认」是把这三行删掉，不是写死数值——将来默认值调整时，你的文件会跟着变。",
            plain,
            ID_N_TIPS,
            self.font,
        )?;

        // 两个数字框里按回车 = 点「应用」；不打这个补丁，回车只会「叮」一声
        unsafe {
            for &h in &[self.e_gain, self.e_half] {
                let _ = SetWindowSubclass(h, Some(enter_proc), 2, ID_B_APPLY);
            }
        }

        self.pages[PAGE_GENERAL] = vec![
            self.l_hint,
            self.c_hint,
            self.n_hint_desc,
            self.l_gain,
            self.e_gain,
            self.n_gain_desc,
            self.l_half,
            self.e_half,
            self.n_half_desc,
            self.b_apply,
            self.b_default,
            self.n_config,
            self.n_tips,
        ];
        Ok(())
    }

    /// 学习数据页：搜索 + 只读列表 + 「清除选中词的学习分」。
    fn create_learn_page(&mut self) -> windows::core::Result<()> {
        let hwnd = self.hwnd;
        let label_style = WINDOW_STYLE(SS_CENTER | SS_CENTERIMAGE);
        let edit_style =
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_TABSTOP.0 | 0x0080);

        self.l_search = child(
            hwnd,
            w!("Static"),
            "搜索",
            label_style,
            ID_L_SEARCH,
            self.font,
        )?;
        self.e_search = child(hwnd, w!("Edit"), "", edit_style, ID_E_SEARCH, self.font)?;
        self.b_search = child(hwnd, w!("Button"), "搜", WS_TABSTOP, ID_B_SEARCH, self.font)?;
        self.b_forget = child(
            hwnd,
            w!("Button"),
            "清除选中词的学习分",
            WS_TABSTOP,
            ID_B_FORGET,
            self.font,
        )?;
        self.learn_list = child(
            hwnd,
            w!("SysListView32"),
            "",
            WINDOW_STYLE(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | WS_BORDER.0
                    | WS_TABSTOP.0
                    | LVS_REPORT
                    | LVS_SINGLESEL
                    | LVS_SHOWSELALWAYS,
            ),
            ID_LEARN_LIST,
            self.font,
        )?;
        unsafe {
            send(
                self.learn_list,
                LVM_SETEXTENDEDLISTVIEWSTYLE,
                WPARAM(LVS_EX_FULLROWSELECT),
                LPARAM(LVS_EX_FULLROWSELECT as isize),
            );
        }
        for (i, (title, width)) in [
            ("词", 160),
            ("上屏次数", 90),
            ("上次使用", 120),
            ("学习分", 90),
            ("档位", 80),
        ]
        .into_iter()
        .enumerate()
        {
            let mut text = vec![0u16; 32];
            put_utf16(&mut text, title);
            let mut col = LVCOLUMNW {
                mask: windows::Win32::UI::Controls::LVCOLUMNW_MASK(LVCF_WIDTH | LVCF_TEXT),
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
            ID_N_LEARN,
            self.font,
        )?;

        unsafe {
            let _ = SetWindowSubclass(self.e_search, Some(enter_proc), 2, ID_B_SEARCH);
        }

        self.pages[PAGE_LEARN] = vec![
            self.l_search,
            self.e_search,
            self.b_search,
            self.b_forget,
            self.learn_list,
            self.n_learn,
        ];
        Ok(())
    }

    /// 切页：只动可见性，不重建控件（重建会让编辑中的内容、滚动位置全丢）。
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
        // Tab 只在内容区外面画页签，页控件都在它上面；但切页后它会重画自己，
        // 顺手再压一次底，免得某些情况下浮上来盖住新页（F44 的另一条路径）
        lower_tab(self.tab);
    }

    fn layout(&self) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let (cw, ch) = (rc.right - rc.left, rc.bottom - rc.top);
        let m = self.s(12);
        let status_h = self.s(26);
        let tab_h = ch - m * 2 - status_h;
        unsafe {
            let _ = MoveWindow(self.tab, m, m, cw - m * 2, tab_h, true);
        }
        // Tab 的内容区：扣掉边框与页签高度
        let cx = m + self.s(10);
        let cy = m + self.s(30);
        let cw2 = cw - m * 2 - self.s(20);
        let ch2 = tab_h - self.s(38);

        self.layout_words(cx, cy, cw2, ch2);
        self.layout_general(cx, cy, cw2, ch2);
        self.layout_learn(cx, cy, cw2, ch2);

        unsafe {
            let _ = MoveWindow(self.status, m, ch - status_h, cw - m * 2, self.s(20), true);
        }
        // 排在最后：Tab 是背景板，必须待在所有页控件下面（F44）
        lower_tab(self.tab);
    }

    fn layout_words(&self, cx: i32, cy: i32, cw2: i32, ch2: i32) {
        let row = self.s(26);
        let gap = self.s(8);
        let panel_h = row * 2 + gap * 2 + self.s(20);
        let list_h = (ch2 - panel_h - gap).max(self.s(80));

        unsafe {
            let _ = MoveWindow(self.list, cx, cy, cw2, list_h, true);
        }

        let py = cy + list_h + gap;
        let mut x = cx;
        let label_w = self.s(44);
        let place = |h: HWND, w: i32, hh: i32, y: i32, x: &mut i32| {
            unsafe {
                let _ = MoveWindow(h, *x, y, w, hh, true);
            }
            *x += w + gap;
        };
        place(self.l_word, label_w, row, py, &mut x);
        place(self.e_word, self.s(130), row, py, &mut x);
        place(self.l_pinyin, label_w + self.s(4), row, py, &mut x);
        place(self.e_pinyin, self.s(150), row, py, &mut x);
        place(self.l_code, label_w, row, py, &mut x);
        place(self.e_code, self.s(80), row, py, &mut x);
        place(self.l_weight, label_w + self.s(4), row, py, &mut x);
        place(self.e_weight, self.s(100), row, py, &mut x);

        let by = py + row + gap;
        let mut bx = cx;
        // 主操作在前：保存 / 删除；中间空一格，再是文件级的三个；「清空」靠右单独放，
        // 它是「放弃这次输入」，不该跟保存挤在一起（F46 的界面意见）。
        place(self.b_save, self.s(72), row, by, &mut bx);
        place(self.b_del, self.s(72), row, by, &mut bx);
        bx += self.s(16);
        place(self.b_import, self.s(96), row, by, &mut bx);
        place(self.b_open, self.s(104), row, by, &mut bx);
        place(self.b_undo, self.s(88), row, by, &mut bx);
        unsafe {
            let _ = MoveWindow(
                self.b_clear,
                cx + cw2 - self.s(64),
                by,
                self.s(64),
                row,
                true,
            );
        }

        let ny = by + row + self.s(4);
        unsafe {
            let _ = MoveWindow(self.note, cx, ny, cw2, self.s(20), true);
        }
    }

    /// 通用页：三行「标签 + 输入 + 一句说明」，下面一行按钮，最底下一行只读的配置信息。
    ///
    /// 说明文字跟在**各自那一行右边**而不是统一堆在底部：改 g 的时候眼睛在那一行，
    /// 说明跑到底部就得来回跳。
    fn layout_general(&self, cx: i32, cy: i32, cw2: i32, ch2: i32) {
        let row = self.s(26);
        let gap = self.s(10);
        let label_w = self.s(110);
        let field_w = self.s(150);
        let mut y = cy + self.s(4);

        let line = |label: HWND, field: HWND, field_h: i32, desc: HWND, y: &mut i32| {
            unsafe {
                let _ = MoveWindow(label, cx, *y, label_w, row, true);
                let _ = MoveWindow(field, cx + label_w + gap, *y, field_w, field_h, true);
                let _ = MoveWindow(
                    desc,
                    cx + label_w + gap + field_w + self.s(16),
                    *y,
                    cw2 - label_w - gap - field_w - self.s(20),
                    row,
                    true,
                );
            }
            *y += row + self.s(18);
        };
        // 下拉框的高度是「展开后」的高度，收起来只有一行高
        line(
            self.l_hint,
            self.c_hint,
            self.s(160),
            self.n_hint_desc,
            &mut y,
        );
        line(self.l_gain, self.e_gain, row, self.n_gain_desc, &mut y);
        line(self.l_half, self.e_half, row, self.n_half_desc, &mut y);

        y += self.s(6);
        unsafe {
            let _ = MoveWindow(self.b_apply, cx, y, self.s(80), row, true);
            let _ = MoveWindow(self.b_default, cx + self.s(88), y, self.s(96), row, true);
        }
        // 两行说明：高度按两行给足，Static 不会自己长高（AutoSize 在这里没有）
        unsafe {
            let _ = MoveWindow(self.n_tips, cx, y + row + self.s(16), cw2, self.s(44), true);
        }
        // 配置文件的路径贴在页面底部：它只在排障时有用，平时不该抢注意力
        unsafe {
            let _ = MoveWindow(
                self.n_config,
                cx,
                cy + ch2 - self.s(24),
                cw2,
                self.s(20),
                true,
            );
        }
    }

    fn layout_learn(&self, cx: i32, cy: i32, cw2: i32, ch2: i32) {
        let row = self.s(26);
        let gap = self.s(8);
        let label_w = self.s(44);
        let mut x = cx;
        let place = |h: HWND, w: i32, hh: i32, y: i32, x: &mut i32| {
            unsafe {
                let _ = MoveWindow(h, *x, y, w, hh, true);
            }
            *x += w + gap;
        };
        place(self.l_search, label_w, row, cy, &mut x);
        place(self.e_search, self.s(220), row, cy, &mut x);
        place(self.b_search, self.s(56), row, cy, &mut x);
        unsafe {
            // 「清除」是破坏性动作，靠右单独放，别挨着搜索框（F46 的同一条理由）
            let _ = MoveWindow(
                self.b_forget,
                cx + cw2 - self.s(160),
                cy,
                self.s(160),
                row,
                true,
            );
        }

        let list_y = cy + row + gap;
        let list_h = (ch2 - row - gap - self.s(28)).max(self.s(80));
        unsafe {
            let _ = MoveWindow(self.learn_list, cx, list_y, cw2, list_h, true);
            let _ = MoveWindow(
                self.n_learn,
                cx,
                list_y + list_h + self.s(4),
                cw2,
                self.s(20),
                true,
            );
        }
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
    fn refresh_general(&mut self) {
        let g = crate::general::General::load(&self.config_path);
        unsafe {
            send(
                self.c_hint,
                CB_SETCURSEL,
                WPARAM(hint_index(g.hint)),
                LPARAM(0),
            );
        }
        set_text(self.e_gain, &crate::general::fmt_number(g.gain));
        set_text(self.e_half, &crate::general::fmt_number(g.half_life));
        self.update_hint_desc(g.hint);
        self.update_general_fields();

        let mut info = format!("{} · {}", self.config_path.display(), g.version);
        if !g.found {
            info.push_str(" · 文件还不存在，点「应用」时会建");
        }
        for w in g.warnings.iter().take(1) {
            info.push_str(&format!(" · {w}"));
        }
        set_text(self.n_config, &info);
    }

    fn selected_hint(&self) -> HintMode {
        match unsafe { send(self.c_hint, CB_GETCURSEL, WPARAM(0), LPARAM(0)) }.0 {
            1 => HintMode::FirstTime,
            2 => HintMode::Off,
            _ => HintMode::Mixed,
        }
    }

    /// 下拉框右边那句说明。选完立刻变——选完还要滚到别处看说明，等于没说明。
    fn update_hint_desc(&self, mode: HintMode) {
        let text = match mode {
            HintMode::Mixed => "拼音候选旁边总是显示这个词的五笔码",
            HintMode::FirstTime => "只在这个词还没上过屏时显示一次（学会就不打扰）",
            HintMode::Off => "一律不显示五笔码",
        };
        set_text(self.n_hint_desc, text);
    }

    /// 两个数字框的即时校验：合法就亮「应用」，非法就把原因写在该行右边。
    ///
    /// 界面层拦住非法值，是为了不让人写进一个会被 `sanitized` 静默夹掉的数——
    /// 那种「写了 500、跑起来是 365、界面上还显示 500」最费时间。
    fn update_general_fields(&self) {
        let gain =
            crate::general::parse_number(&get_text(self.e_gain), crate::general::GAIN_RANGE, "");
        let half =
            crate::general::parse_number(&get_text(self.e_half), crate::general::HALF_RANGE, " 天");
        let gain_text = match &gain {
            Ok(_) => "越大越容易把常用的词往前提；0 = 不学习；默认 0.35".to_string(),
            Err(e) => format!("输入有误：{e}"),
        };
        let half_text = match &half {
            Ok(_) => "多少天不用，分数减半；默认 45".to_string(),
            Err(e) => format!("输入有误：{e}"),
        };
        set_text(self.n_gain_desc, &gain_text);
        set_text(self.n_half_desc, &half_text);
        enable_window(self.b_apply, gain.is_ok() && half.is_ok());
    }

    fn on_apply(&mut self) {
        let gain = match crate::general::parse_number(
            &get_text(self.e_gain),
            crate::general::GAIN_RANGE,
            "",
        ) {
            Ok(v) => v,
            Err(e) => {
                self.set_status(&format!("学习增益没法用：{e}"));
                return;
            }
        };
        let half = match crate::general::parse_number(
            &get_text(self.e_half),
            crate::general::HALF_RANGE,
            " 天",
        ) {
            Ok(v) => v,
            Err(e) => {
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
                    app.layout();
                    // 默认停在第一页（通用）。先显页再刷数据：刷的时候要读文件的，
                    // 读失败的信息得写在已经可见的控件上。
                    let page = app.initial_page;
                    app.show_page(page);
                    unsafe {
                        send(app.tab, TCM_SETCURSEL, WPARAM(page), LPARAM(0));
                    }
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
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xffff;
            let code = ((wparam.0 >> 16) & 0xffff) as u32;
            if let Some(app) = unsafe { app_of(hwnd) } {
                // EN_CHANGE = 0x0300：编辑框内容变了，按钮该亮该灰要跟着走。
                // 不做这一步，「保存」就只能一直亮着（空词点下去只得到一句抱怨）。
                // 三个编辑框的判据各不相同，按 id 分派——一律走 `update_buttons`
                // 会拿两个数字框的内容去判「词填了没」，结果永远是亮的。
                match (code, id) {
                    (0x0300, ID_E_GAIN | ID_E_HALF) => app.update_general_fields(),
                    (0x0300, ID_E_WORD | ID_E_PINYIN | ID_E_CODE | ID_E_WEIGHT) => {
                        app.update_buttons()
                    }
                    (CBN_SELCHANGE, ID_C_HINT) => app.update_hint_desc(app.selected_hint()),
                    (0, ID_B_CLEAR) => app.on_clear(),
                    (0, ID_B_SAVE) => app.on_save(),
                    (0, ID_B_DEL) => app.on_delete(),
                    (0, ID_B_UNDO) => app.on_undo(),
                    (0, ID_B_IMPORT) => app.on_import(),
                    (0, ID_B_OPEN) => app.on_open(),
                    (0, ID_B_APPLY) => app.on_apply(),
                    (0, ID_B_DEFAULT) => app.on_restore_defaults(),
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
                } else if hdr.code == TCN_SELCHANGE && hdr.hwndFrom == app.tab {
                    let page = send(app.tab, TCM_GETCURSEL, WPARAM(0), LPARAM(0)).0;
                    app.show_page(page.max(0) as usize);
                }
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            // 所有 Static 都用**父窗口自己的背景色**。主窗口类注册时的 hbrBackground
            // 是 `COLOR_BTNFACE + 1`（系统灰），这里必须回同一个颜色：原来回的是
            // `COLOR_WINDOW`（白），于是每个字段名背后挂一块白方块，在浅灰底上一眼
            // 就看出来（F47）。文字本身用透明背景，字缝里透出的也是这块灰。
            let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut core::ffi::c_void);
            unsafe {
                SetBkMode(hdc, TRANSPARENT);
            }
            let brush = unsafe {
                GetSysColorBrush(
                    windows::Win32::Graphics::Gdi::SYS_COLOR_INDEX(15), /* COLOR_BTNFACE */
                )
            };
            LRESULT(brush.0 as isize)
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
                    let _ = RemoveWindowSubclass(app.tab, Some(tab_proc), 1);
                    for &h in &[app.e_word, app.e_pinyin, app.e_code, app.e_weight] {
                        let _ = RemoveWindowSubclass(h, Some(edit_proc), 1);
                    }
                    for &h in &[app.e_gain, app.e_half, app.e_search] {
                        let _ = RemoveWindowSubclass(h, Some(enter_proc), 2);
                    }
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

unsafe fn app_of(hwnd: HWND) -> Option<&'static mut App> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;
    unsafe { ptr.as_mut() }
}

// ---------------------------------------------------------------- 小工具

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

/// 编辑框里的回车 = 保存，Esc = 清空。
///
/// 单行 Edit 收到回车只会「叮」一声（`WM_CHAR` 走默认处理），改成转发一条
/// `WM_COMMAND(ID_B_SAVE)` 给父窗口即可——就是按钮被点一下时发的那条消息，
/// 所以走的还是同一个 `on_save`，不存在第二条保存路径。
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

/// 通用页 / 学习页的编辑框里按回车 = 点它对应的那个按钮。
///
/// 目标按钮的 id 走子类化的 `data` 参数，所以一个过程能服务「应用」和「搜」
/// 两个不同的按钮——不必为每个框写一遍同样的代码（也就不会漏掉某一个）。
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

/// 把 Tab 压到 z 序最底层（F44）。
///
/// 不用 `WS_EX_TRANSPARENT` 之类的花样：Tab 只是背景板，压到底之后它照常画页签、
/// 照常响应 `TCN_SELCHANGE`，被页控件盖住的部分不画——这正是原生 Tab 的常规用法。
fn lower_tab(tab: HWND) {
    if tab.0.is_null() {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            tab,
            Some(HWND_BOTTOM),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// Tab 的子类化过程：`SysTabControl32` 在 `WM_SETFOCUS` 里会 `SetWindowPos` 把
/// 自己抬到最上层，所以必须**等它处理完**（先 `DefSubclassProc`）再压回去。
unsafe extern "system" fn tab_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    let result = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
    if msg == WM_SETFOCUS {
        lower_tab(hwnd);
    }
    result
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

/// 字段名（`词/拼音/码/权重`）的两条视觉约定：
///
/// 1. 样式里带 `SS_CENTER | SS_CENTERIMAGE`（水平+垂直居中）——默认的 Static 是
///    左上对齐，跟同高的编辑框摆一起看着像错位；
/// 2. 主窗口对 `WM_CTLCOLORSTATIC` 回的画刷必须是**父窗口背景色**（`COLOR_BTNFACE`），
///    不是 `COLOR_WINDOW`。回白的后果是每个字段名背后挂一块白方块，浅灰底上一眼可见
///    （F47）。
///
/// 第 2 条直接发一次真的 `WM_CTLCOLORSTATIC` 看返回值——这比读代码可靠，也比截图
/// 精确：色差只有 15 级，肉眼复查容易漏。
fn check_labels(hwnd: HWND, app: &App) -> Result<(), String> {
    let want = unsafe {
        GetSysColorBrush(
            windows::Win32::Graphics::Gdi::SYS_COLOR_INDEX(15), /* COLOR_BTNFACE */
        )
    };
    let dc = unsafe { GetDC(None) };
    let result = (|| -> Result<(), String> {
        for &(h, name) in &[
            (app.l_word, "词"),
            (app.l_pinyin, "拼音"),
            (app.l_code, "码"),
            (app.l_weight, "权重"),
        ] {
            let style = unsafe { GetWindowLongPtrW(h, GWL_STYLE) } as u32;
            if style & SS_CENTER == 0 || style & SS_CENTERIMAGE == 0 {
                return Err(format!(
                    "字段名「{name}」没有居中样式（SS_CENTER | SS_CENTERIMAGE）"
                ));
            }
            let got = unsafe {
                send(
                    hwnd,
                    WM_CTLCOLORSTATIC,
                    WPARAM(dc.0 as usize),
                    LPARAM(h.0 as isize),
                )
            };
            if got.0 as isize != want.0 as isize {
                return Err(format!(
                    "字段名「{name}」的背景画刷不是父窗口背景色——白方块会露出来（F47）"
                ));
            }
        }
        Ok(())
    })();
    unsafe {
        ReleaseDC(None, dc);
    }
    result
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
            WS_OVERLAPPEDWINDOW,
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

    let app = Box::new(App::new(
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

        switch_page(&app, PAGE_WORDS);
        check_z_order(hwnd, PAGE_WORDS)?;
        check_labels(hwnd, &app)?;
        check_panel_flow(&app, &path)?;

        switch_page(&app, PAGE_LEARN);
        check_z_order(hwnd, PAGE_LEARN)?;
        check_learn_page(&app)?;

        switch_page(&app, PAGE_GENERAL);
        // 模拟 Tab 拿到焦点（点中它、或 Tab 键走过去）：它自己会抬到最上层，
        // 我们的子类化过程必须把它压回去
        unsafe {
            send(app.tab, WM_SETFOCUS, WPARAM(0), LPARAM(0));
        }
        check_z_order(hwnd, PAGE_GENERAL).map_err(|e| format!("Tab 拿到焦点后：{e}"))
    })();

    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// 自测用：像用户点页签那样切页（走 `TCM_SETCURSEL` + 真正的 `TCN_SELCHANGE`）。
fn switch_page(app: &App, page: usize) {
    unsafe {
        send(app.tab, TCM_SETCURSEL, WPARAM(page), LPARAM(0));
        // 直接调 `show_page` 会绕开「消息 → 分派」这一段，而恰恰是那一段里
        // 容易指错页（下标与页签顺序不一致）。
        let mut hdr = windows::Win32::UI::Controls::NMHDR {
            hwndFrom: app.tab,
            idFrom: ID_TAB,
            code: TCN_SELCHANGE,
        };
        send(
            app.hwnd,
            WM_NOTIFY,
            WPARAM(ID_TAB),
            LPARAM(&mut hdr as *mut _ as isize),
        );
    }
    drain_messages();
}

/// 通用页：合法/非法输入的门、写盘后注释还在、恢复默认真的删行。
fn check_general_flow(app: &App, cfg: &std::path::Path) -> Result<(), String> {
    // 1. 打开时按文件里的值显示：hint.mode = "mixed" → 第 0 项
    let sel = unsafe { send(app.c_hint, CB_GETCURSEL, WPARAM(0), LPARAM(0)) }.0;
    if sel != 0 {
        return Err(format!("下拉框应选中 mixed（0），实际是 {sel}"));
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
    if !get_text(app.n_gain_desc).contains("输入有误") {
        return Err(format!(
            "非法输入没给出原因，实际是 {:?}",
            get_text(app.n_gain_desc)
        ));
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
    let last = *kids.last().unwrap();
    if id_of(last) != ID_TAB as isize {
        return Err(format!(
            "z 序最底层应是 Tab（id {ID_TAB}），实际是 id {}——Tab 一浮上来就会盖住所有按钮",
            id_of(last)
        ));
    }

    let clickable: &[usize] = match page {
        PAGE_GENERAL => &[ID_C_HINT, ID_E_GAIN, ID_E_HALF, ID_B_APPLY, ID_B_DEFAULT],
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
