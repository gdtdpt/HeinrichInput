//! 设置窗口：主窗口 + Tab + 词组页（ListView + 编辑面板）。
//!
//! 只写 `%APPDATA%\Heinrich\words.tsv`，写完全靠 server 的 mtime 热重载生效——
//! 不需要任何进程间通信（D65 的第二个红利）。所以这个 exe 可以随时开、随时关，
//! 关掉之后加的词照样在。
//!
//! 为什么用原生控件而不是复用 `heinrich-win-ui` 的自绘：编辑框要能**用亨利自己
//! 打中文**（单字都在词库里），自绘就得连光标、选区、IME 组合串一起实现；而这张
//! 表的重点是把功能跑通，观感可以以后再统一（M4 之后仍可只换绘制层）。
//!
//! **Tab 必须永远压在 z 序最底层**（[`lower_tab`]）：`SysTabControl32` 拿到焦点
//! 时会把自己抬到最上层，而它的矩形正好覆盖整个内容区——一旦抬上来，它就把下面
//! 所有按钮和编辑框整片盖住，鼠标点哪儿都被它吃掉。界面看起来完全正常，只是所有
//! 点击都石沉大海（F44：验收时「新增/保存点了没反应、词也没真加上」就是这个）。

use std::path::PathBuf;

use heinrich_scheme::user::UserWord;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, GetSysColorBrush, ScreenToClient, SetBkMode, HFONT, TRANSPARENT,
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
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetParent, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    LoadCursorW, MessageBoxW, MoveWindow, PostQuitMessage, RealChildWindowFromPoint,
    RegisterClassW, SendMessageW, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_ID, GWLP_USERDATA,
    GW_CHILD, GW_HWNDNEXT, HWND_BOTTOM, IDC_ARROW, MB_ICONINFORMATION, MB_OK, MSG, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SW_SHOW, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WINDOW_STYLE, WM_CHAR,
    WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORSTATIC, WM_DESTROY, WM_KEYDOWN, WM_NCCREATE,
    WM_NCDESTROY, WM_NOTIFY, WM_SETFOCUS, WM_SETFONT, WM_SIZE, WNDCLASSW, WNDPROC, WS_BORDER,
    WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
};

use crate::lex::{find_data_dir, Lexicons};
use crate::model::{Model, Row};

const CLASS_NAME: PCWSTR = w!("HeinrichSettings");

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

    model: Model,
    lex: Option<Lexicons>,
    lex_note: Option<String>,
    rows: Vec<Row>,
    /// 编辑面板当前挂着哪一行（`None` = 面板是空的，保存时按新词处理）
    editing: Option<usize>,
    /// 上一次删除的 (原位置, 词条)，给「撤销删除」用
    undo: Option<(usize, UserWord)>,
}

impl App {
    pub fn new(data: Option<PathBuf>, words: Option<PathBuf>) -> Result<Self, String> {
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
            model,
            lex,
            lex_note,
            rows: Vec::new(),
            editing: None,
            undo: None,
        })
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
        // 页码与 `refresh` 里的顺序一致；以后加页只在这里追加一行
        let mut item = vec![0u16; 32];
        put_utf16(&mut item, "词组");
        let mut tc = windows::Win32::UI::Controls::TCITEMW {
            mask: windows::Win32::UI::Controls::TCITEMHEADERA_MASK(0x0001), // TCIF_TEXT
            pszText: PWSTR(item.as_mut_ptr()),
            ..Default::default()
        };
        unsafe {
            send(
                self.tab,
                windows::Win32::UI::Controls::TCM_INSERTITEMW,
                WPARAM(0),
                LPARAM(&mut tc as *mut _ as isize),
            );
        }

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

        self.l_word = child(
            hwnd,
            w!("Static"),
            "词",
            WINDOW_STYLE(0),
            ID_L_WORD,
            self.font,
        )?;
        self.l_pinyin = child(
            hwnd,
            w!("Static"),
            "拼音",
            WINDOW_STYLE(0),
            ID_L_PINYIN,
            self.font,
        )?;
        self.l_code = child(
            hwnd,
            w!("Static"),
            "码",
            WINDOW_STYLE(0),
            ID_L_CODE,
            self.font,
        )?;
        self.l_weight = child(
            hwnd,
            w!("Static"),
            "权重",
            WINDOW_STYLE(0),
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
        self.status = child(
            hwnd,
            w!("Static"),
            "",
            WINDOW_STYLE(0),
            ID_STATUS,
            self.font,
        )?;
        Ok(())
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
            let _ = MoveWindow(self.status, m, ch - status_h, cw - m * 2, self.s(20), true);
        }
        // 排在最后：Tab 是背景板，必须待在所有页控件下面（F44）
        lower_tab(self.tab);
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
                    app.refresh();
                    // 刻意**不**自动选中第一行：面板是给「新词」用的，一开窗就
                    // 躺着上次的词会让人以为已经选中了它，改完点保存就把那条词改了。
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
                if code == 0x0300 {
                    app.update_buttons();
                } else if code == 0 {
                    match id {
                        ID_B_CLEAR => app.on_clear(),
                        ID_B_SAVE => app.on_save(),
                        ID_B_DEL => app.on_delete(),
                        ID_B_UNDO => app.on_undo(),
                        ID_B_IMPORT => app.on_import(),
                        ID_B_OPEN => app.on_open(),
                        _ => {}
                    }
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
                } else if hdr.code == TCN_SELCHANGE && hdr.hwndFrom == app.tab {
                    // 只有一页，暂时没有可切的东西（M4b 在这里按页显示/隐藏）
                }
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            // 让 Static 背景透明，否则文字背后是一块灰底
            let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut core::ffi::c_void);
            unsafe {
                SetBkMode(hdc, TRANSPARENT);
            }
            let brush = unsafe {
                GetSysColorBrush(
                    windows::Win32::Graphics::Gdi::SYS_COLOR_INDEX(5), /* COLOR_WINDOW */
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
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: HINSTANCE(hinstance.0),
            hCursor: cursor,
            lpszClassName: CLASS_NAME,
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
            CLASS_NAME,
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

    let app = Box::new(App::new(data, Some(path.clone()))?);
    let hwnd = create_window(&app)?;
    // 不抢焦点地显示一下：真实路径里窗口是可见的，z 序表现才一致
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    drain_messages();

    let result = (|| -> Result<(), String> {
        check_z_order(hwnd)?;
        check_panel_flow(&app, &path)?;
        // 模拟 Tab 拿到焦点（点中它、或 Tab 键走过去）：它自己会抬到最上层，
        // 我们的子类化过程必须把它压回去
        unsafe {
            send(app.tab, WM_SETFOCUS, WPARAM(0), LPARAM(0));
        }
        check_z_order(hwnd).map_err(|e| format!("Tab 拿到焦点后：{e}"))
    })();

    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    let _ = std::fs::remove_dir_all(&dir);
    result
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
/// 为什么不用 `WindowFromPoint`：那是在**整个桌面**上找窗口，会被别的窗口、锁屏
/// 界面干扰；`RealChildWindowFromPoint` 只在父窗口内部按 z 序/可见性找子窗口，
/// 与桌面状态无关，可以放进自动化里跑。
fn check_z_order(hwnd: HWND) -> Result<(), String> {
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

    const CLICKABLE: [usize; 10] = [
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
    ];
    for id in CLICKABLE {
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
                    "控件 id {id} 在相对位置 ({fx}, {fy}) 处命中的是 id {}（点了不会有反应）",
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
