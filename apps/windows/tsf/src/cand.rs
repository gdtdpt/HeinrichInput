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
    /// 折叠区第一条的全局序号（D48）。`== words.len()` 表示只有一段。
    split: usize,
    /// 页码、本页区间、两端箭头。
    pager: Pager,
    /// 本页内的高亮项。M2.3 里数字键是**直接提交**、鼠标点也是直接提交，
    /// 不存在「先移动高亮再空格」的交互，所以它恒为 0；留着是为了 M3 的
    /// `←/→` 移高亮与 emoji 标签。
    highlight: usize,
    /// 上次显示用的光标矩形。翻页/重画要用同一处，不能每次重问 TSF
    /// （翻页发生在窗口过程里，那时没有编辑会话可开）。
    caret: Option<Rect>,
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
            split: 0,
            pager: Pager::new(PAGE_SIZE),
            highlight: 0,
            caret: None,
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
    /// `split` 是折叠区第一条的全局序号（D48）：从它开始的候选属于另一门语言，
    /// 画的时候淡一档。传 `words.len()` 表示只有一段。
    pub fn update(&mut self, words: Vec<String>, split: usize) {
        self.pager.reset(words.len());
        self.highlight = 0;
        self.split = split.min(words.len());
        self.words = words;
        tsf_debug!(
            "#{} 候选 {} 条，共 {} 页",
            self.seq,
            self.words.len(),
            self.pager.pages()
        );
    }

    /// 清空候选并隐藏窗口。
    pub fn clear(&mut self) {
        self.words.clear();
        self.split = 0;
        self.pager.reset(0);
        self.highlight = 0;
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
                    Ok(()) => tsf_debug!(
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
                    ),
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
