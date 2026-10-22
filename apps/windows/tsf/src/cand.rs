//! 候选窗：**什么时候显示、显示哪一页、翻页与点选之后怎么办**。
//!
//! ## 分工
//!
//! | 层 | 负责 |
//! |---|---|
//! | `heinrich-ui` | 一页候选怎么排、窗口放哪（纯算术，可单测） |
//! | `heinrich-win-ui` | HWND、分层窗口、DWrite 量字、D2D 画 |
//! | **本模块** | 候选集与页码这两份状态，以及「重画一次」这件事 |
//! | `text_service` | 什么时候喂候选、选中的词怎么上屏 |
//!
//! ## 窗口是懒建的
//!
//! DLL 会被注入到每个用到中文输入的宿主进程里，而很多人装了输入法却从不在某个
//! 进程里打中文。所以 `CandidateWindow::new`（D2D/DWrite 工厂 + 窗口类）推迟到
//! **第一次真的要显示候选**时才做，并且**一生只做一次**：这台机器上创建失败
//! （极老的系统、被安全软件挡住）就永久降级为「无候选窗也能打字」——M1.3 的行为，
//! 而不是每敲一个字失败一次、刷一屏日志。
//!
//! ## 显示失败一律静默降级
//!
//! 候选窗是**锦上添花**：它画不出来，字照样要能上。所以这里所有 API 都只记日志、
//! 不返回错误，调用方不需要处理「窗口挂了」这种分支。

use heinrich_ui::{place, Dir, Pager, Rect};
use heinrich_win_ui::{work_area_at, CandidateWindow, Candidates, Event, Theme};

/// 一页显示几条。
///
/// 5 条是五笔输入法的通行做法（数字键 1–9 的前五个），也是几何层文档里
/// 「一页超过 9 条时序号会变成两位数」的安全区。想改成 9 条只改这一个数。
pub const PAGE_SIZE: usize = 5;

/// 候选窗与光标之间留的空隙（逻辑像素，按 DPI 缩放）。
const CARET_GAP: f32 = 2.0;

pub struct Cands {
    seq: u32,
    /// 全部候选（不只是本页）。
    words: Vec<String>,
    /// 与 `words` 等长的码提示（§6.3）。空串 = 这条不画 chip。
    codes: Vec<String>,
    /// 折叠区第一条的全局序号（D48）。`== words.len()` 表示只有一段。
    split: usize,
    /// 页码、本页区间、两端箭头。
    pager: Pager,
    /// 本页内的**默认提交项**，恒为 0。
    ///
    /// 它**不是**一个可以移动的光标，请不要去找「移动高亮」的键——没有。
    /// 五笔输入法的传统交互是「第 1 项恒高亮 + 数字键直选 + `-`/`=` 翻页」，
    /// 高亮在这里的职责只有一个：告诉用户**此刻按空格会出哪个字**。
    /// 方向键已经被 D38 定成翻页（`←`/`→`），单行布局下也没有第二个方向可用。
    ///
    /// 因此原本计划的 `Ctrl+Shift+/`（取「当前高亮项」）与 `Ctrl+Shift+1`
    /// 完全等价，v1.23 撤掉了这个入口（D50 修订）。
    highlight: usize,
    /// 上次显示用的光标矩形。翻页/重画要用同一处，不能每次重问 TSF
    /// （翻页发生在窗口过程里，那时没有编辑会话可开）。
    caret: Option<Rect>,
    /// 候选窗**实际**落在哪（屏幕坐标）。键位图贴它，§6.7。
    /// `None` = 还没成功显示过。
    rect: Option<Rect>,
    /// 候选窗是否翻到了光标上方。键位图跟着走，否则会压在正文上。
    flipped: bool,
    window: Option<CandidateWindow>,
    /// 建窗时交给窗口的点击回调。只被取走一次。
    on_event: Option<Box<dyn FnMut(Event)>>,
    /// 建窗失败过。失败过就不再试，见模块文档。
    failed: bool,
}

impl Cands {
    pub fn new(seq: u32) -> Self {
        Self {
            seq,
            words: Vec::new(),
            codes: Vec::new(),
            split: 0,
            pager: Pager::new(PAGE_SIZE),
            highlight: 0,
            caret: None,
            rect: None,
            flipped: false,
            window: None,
            on_event: None,
            failed: false,
        }
    }

    /// 装好点击回调。必须在第一次 `show` 之前调用（`Core::new` 里做）。
    pub fn set_on_event(&mut self, handler: impl FnMut(Event) + 'static) {
        self.on_event = Some(Box::new(handler));
    }

    /// 换一批候选，回到第 1 页，并**不**自动显示（光标位置得由调用方给）。
    ///
    /// `codes` 与 `words` 等长，空串表示这条不画码提示——规则由 server 裁决
    /// （它知道这一段是拼音还是五笔、`hint.mode` 是什么），这里不重复判断。
    ///
    /// `split` 是折叠区第一条的全局序号（D48）：从它开始的候选属于另一门语言，
    /// 画的时候淡一档。传 `words.len()` 表示只有一段。
    pub fn update(&mut self, words: Vec<String>, mut codes: Vec<String>, split: usize) {
        // 长度对齐：协议与 UI 之间多一道防线。少了补空串（不画），多了直接丢掉——
        // 越界的提示会画到隔壁项上，比不画难查得多。
        codes.resize(words.len(), String::new());
        codes.truncate(words.len());
        self.pager.reset(words.len());
        self.highlight = 0;
        self.split = split.min(words.len());
        self.words = words;
        self.codes = codes;
        tsf_debug!(
            "#{} 候选 {} 条（{} 条带码提示），共 {} 页",
            self.seq,
            self.words.len(),
            self.codes.iter().filter(|c| !c.is_empty()).count(),
            self.pager.pages()
        );
    }

    /// 清空候选并隐藏窗口。
    pub fn clear(&mut self) {
        self.words.clear();
        self.codes.clear();
        self.split = 0;
        self.pager.reset(0);
        self.highlight = 0;
        self.rect = None;
        self.flipped = false;
        self.hide();
    }

    /// 本页有几条候选。**0 表示此刻没有候选窗**（`keys::decide` 靠它决定
    /// 数字键与方向键归谁）。
    pub fn page_len(&self) -> usize {
        self.pager.len()
    }

    /// 本页第 `i` 条（0 起）的文字。
    pub fn word_on_page(&self, i: usize) -> Option<String> {
        let global = self.pager.global(i)?;
        self.words.get(global).cloned()
    }

    /// 本页第 `i` 条（0 起）的**五笔码**（§6.3 的码提示数据，键位图要用它）。
    ///
    /// `Some("")` = 这一条没有码（五笔段的候选就是这样，查询串本身就是码）；
    /// `None` = 本页没有这一条。
    pub fn code_on_page(&self, i: usize) -> Option<String> {
        let global = self.pager.global(i)?;
        Some(self.codes.get(global).cloned().unwrap_or_default())
    }

    /// 候选窗**当前**矩形（屏幕坐标）。`None` = 没有窗口。
    ///
    /// 键位图贴着它排（§6.7）——贴**光标**会挤在候选窗与正文之间，正好挡住
    /// 用户正在看的那一行。
    pub fn rect(&self) -> Option<Rect> {
        self.rect
    }

    /// 候选窗是否翻到了光标上方。
    pub fn flipped(&self) -> bool {
        self.flipped
    }

    /// 全部候选里第 `global` 条的文字。点选提交走这条——窗口给的就是全局序号。
    pub fn word_at(&self, global: usize) -> Option<String> {
        self.words.get(global).cloned()
    }

    /// 当前高亮那条。空格 / 回车提交的就是它——与数字键、点选**同一份数据**，
    /// 所以「按 2 上屏的字」和「点第 2 条上屏的字」不可能不一致。
    pub fn current(&self) -> Option<String> {
        let global = self.pager.global(self.highlight)?;
        self.words.get(global).cloned()
    }

    /// 按光标矩形显示/重画。候选为空时等价于 [`Self::hide`]。
    pub fn show(&mut self, caret: Rect) {
        self.caret = Some(caret);
        self.redraw();
    }

    pub fn hide(&mut self) {
        if let Some(win) = self.window.as_mut() {
            win.hide();
        }
    }

    /// 翻一页并重画。**真的翻动了才返回 `true`**。
    ///
    /// 翻不动（首页按左、末页按右）时窗口连重画都不做：内容没变，重画只会闪一下。
    pub fn flip(&mut self, dir: Dir) -> bool {
        if !self.pager.flip(dir) {
            tsf_debug!("#{} 已经在{}，忽略翻页", self.seq, edge_name(dir));
            return false;
        }
        tsf_debug!(
            "#{} 翻到第 {} / {} 页（本页 {} 条）",
            self.seq,
            self.pager.page() + 1,
            self.pager.pages(),
            self.pager.len()
        );
        self.redraw();
        true
    }

    /// 重画一次。
    fn redraw(&mut self) {
        let Some(caret) = self.caret else {
            return;
        };
        if self.pager.is_empty() {
            // 空候选不该留一个空壳窗在屏幕上（例如查到 0 条）。
            self.hide();
            return;
        }
        if !self.ensure_window() {
            return;
        }
        let Some(mut win) = self.window.take() else {
            return;
        };

        let anchor = (caret.x as i32, caret.bottom() as i32);
        let prepared = win.prepare(&Candidates {
            texts: &self.words,
            hints: &self.codes,
            page: self.pager.page(),
            page_size: self.pager.page_size(),
            highlight: self.highlight,
            split: self.split,
            anchor,
        });

        match prepared {
            Ok(prepared) => {
                // 空隙也跟着 DPI 走，否则 200% 下窗口会像贴在字上。
                let gap = CARET_GAP * prepared.dpi_scale;
                let placed = place(caret, prepared.layout.size, work_area_at(anchor), gap);
                match win.show(&prepared, (placed.rect.x as i32, placed.rect.y as i32)) {
                    Ok(()) => {
                        // 记下窗口实际落在哪：键位图要贴着它往下排（§6.7），
                        // 而那时早已不在这次 `redraw` 的栈上了。
                        self.rect = Some(placed.rect);
                        self.flipped = placed.flipped;
                        tsf_debug!(
                            "#{} 候选窗 {}×{} @ ({}, {}){}",
                            self.seq,
                            prepared.layout.size.w,
                            prepared.layout.size.h,
                            placed.rect.x as i32,
                            placed.rect.y as i32,
                            if placed.flipped {
                                "（翻到光标上方）"
                            } else {
                                ""
                            }
                        );
                    }
                    Err(e) => tsf_error!("#{} 候选窗显示失败：{e}", self.seq),
                }
            }
            Err(e) => tsf_error!("#{} 候选布局失败：{e}", self.seq),
        }

        self.window = Some(win);
    }

    /// 保证窗口存在。建窗失败后永久放弃（见模块文档）。
    fn ensure_window(&mut self) -> bool {
        if self.window.is_some() {
            return true;
        }
        if self.failed {
            return false;
        }
        match CandidateWindow::new(Theme::default()) {
            Ok(mut win) => {
                if let Some(handler) = self.on_event.take() {
                    win.set_on_event(handler);
                }
                tsf_debug!("#{} 候选窗已创建", self.seq);
                self.window = Some(win);
                true
            }
            Err(e) => {
                self.failed = true;
                tsf_error!(
                    "#{} 候选窗创建失败：{e}（此后不再尝试，输入照常）",
                    self.seq
                );
                false
            }
        }
    }
}

fn edge_name(dir: Dir) -> &'static str {
    match dir {
        Dir::Prev => "第一页",
        Dir::Next => "最后一页",
    }
}
