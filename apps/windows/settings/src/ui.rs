//! 设置窗口：主窗口 + Tab + 词组页（ListView + 编辑面板）。
//!
//! 只写 `%APPDATA%\Heinrich\words.tsv`，写完全靠 server 的 mtime 热重载生效——
//! 不需要任何进程间通信（D65 的第二个红利）。所以这个 exe 可以随时开、随时关，
//! 关掉之后加的词照样在。
//!
//! 为什么用原生控件而不是复用 `heinrich-win-ui` 的自绘：编辑框要能**用亨利自己
//! 打中文**（单字都在词库里），自绘就得连光标、选区、IME 组合串一起实现；而这张
//! 表的重点是把功能跑通，观感可以以后再统一（M4 之后仍可只换绘制层）。

use std::path::PathBuf;

use heinrich_scheme::user::UserWord;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, GetSysColorBrush, SetBkMode, HFONT, TRANSPARENT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OPENFILENAMEW};
use windows::Win32::UI::Controls::{
    LVCOLUMNW, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_DELETEITEM,
    LVM_ENSUREVISIBLE, LVM_GETITEMCOUNT, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMW, LVNI_SELECTED, LVN_ITEMCHANGED,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, LoadCursorW, MessageBoxW, MoveWindow,
    PostQuitMessage, RegisterClassW, SendMessageW, SetWindowLongPtrW, SetWindowTextW, ShowWindow,
    TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA,
    IDC_ARROW, MB_ICONINFORMATION, MB_OK, MSG, SW_SHOW, SW_SHOWNORMAL, WINDOW_STYLE, WM_CLOSE,
    WM_COMMAND, WM_CREATE, WM_CTLCOLORSTATIC, WM_DESTROY, WM_NCCREATE, WM_NCDESTROY, WM_NOTIFY,
    WM_SETFONT, WM_SIZE, WNDCLASSW, WNDPROC, WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP,
    WS_VISIBLE,
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
const ID_B_ADD: usize = 130;
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
    b_add: HWND,
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
    /// 编辑面板当前挂着哪一行（`None` = 不在编辑）
    editing: Option<usize>,
    /// 「新增」状态：面板是空的，保存时插入新词而不是改已有词
    adding: bool,
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
            b_add: HWND::default(),
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
            adding: false,
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

        self.b_add = child(hwnd, w!("Button"), "新增", WS_TABSTOP, ID_B_ADD, self.font)?;
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

        self.note = child(
            hwnd,
            w!("Static"),
            "编辑框只显示文件里写的内容，留空 = 按规则自动推导（列表里带 *）；保存后 1 秒内生效，不用重启输入法",
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
        place(self.b_add, self.s(72), row, by, &mut bx);
        place(self.b_save, self.s(72), row, by, &mut bx);
        place(self.b_del, self.s(72), row, by, &mut bx);
        place(self.b_import, self.s(96), row, by, &mut bx);
        place(self.b_open, self.s(104), row, by, &mut bx);
        place(self.b_undo, self.s(88), row, by, &mut bx);

        let ny = by + row + self.s(4);
        unsafe {
            let _ = MoveWindow(self.note, cx, ny, cw2, self.s(20), true);
            let _ = MoveWindow(self.status, m, ch - status_h, cw - m * 2, self.s(20), true);
            let _ = MoveWindow(self.e_word, cx + label_w + gap, py, self.s(130), row, true);
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
    }

    fn select(&mut self, idx: usize) {
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
                self.list,
                LVM_SETITEMSTATE,
                WPARAM(idx),
                LPARAM(&mut item as *mut _ as isize),
            );
            send(self.list, LVM_ENSUREVISIBLE, WPARAM(idx), LPARAM(0));
        }
        self.editing = Some(idx);
        self.adding = false;
        self.fill_panel(Some(idx));
        self.update_buttons();
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
        enable_window(self.b_save, true);
        enable_window(self.b_del, has_sel);
        enable_window(self.b_undo, self.undo.is_some());
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

    fn on_add(&mut self) {
        self.adding = true;
        self.editing = None;
        self.fill_panel(None);
        set_focus(self.e_word);
        self.set_status("输入词后点「保存」；拼音与码留空就自动推导");
    }

    fn on_save(&mut self) {
        let mut w = self.panel_word();
        if w.word.trim().is_empty() {
            message(self.hwnd, "提示", "「词」不能为空");
            set_focus(self.e_word);
            return;
        }
        w.word = w.word.trim().to_string();
        // 改了词名 = 删旧词 + 加新词，否则旧词会留在文件里再也删不掉
        if let Some(idx) = self.editing {
            if let Some(old) = self.rows.get(idx).map(|r| r.raw.word.clone()) {
                if old != w.word {
                    self.model.remove(&old);
                }
            }
        }
        let (idx, is_new) = self.model.upsert(w.clone());
        if !self.save() {
            return;
        }
        self.adding = false;
        self.editing = Some(idx);
        self.refresh();
        self.select(idx);
        let r = self.rows.get(idx);
        let (pin, code) = r
            .map(|r| (r.pinyin_cell(), r.code_cell()))
            .unwrap_or_default();
        self.set_status(&format!(
            "{}「{}」· 拼音 {} · 码 {} · 1 秒内生效",
            if is_new { "已加入" } else { "已更新" },
            w.word,
            pin,
            code
        ));
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
                    if app.editing.is_none() {
                        app.editing = (!app.rows.is_empty()).then_some(0);
                        if app.editing.is_some() {
                            app.select(0);
                        }
                    }
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
                if code == 0 {
                    match id {
                        ID_B_ADD => app.on_add(),
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
                            app.adding = false;
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

/// 建窗、跑消息循环。返回进程退出码。
pub fn run(app: Box<App>) -> Result<i32, String> {
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
        let hwnd = CreateWindowExW(
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
            Some(&*app as *const App as *const core::ffi::c_void),
        )
        .map_err(|e| format!("建窗失败：{e}"))?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(0)
}

// 保留未使用的导入时给出明确意图：这些常量与函数是给 M4b 的页留的入口。
#[allow(dead_code)]
fn _reserved() {
    let _ = (LVM_GETITEMCOUNT, LVM_DELETEITEM, COLORREF(0), MIN_W, MIN_H);
    let _: Option<WNDPROC> = None;
}
