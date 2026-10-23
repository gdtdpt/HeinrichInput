//! 引擎宿主：**平台中立**的那一半 server。
//!
//! 本文件不碰任何 Windows API，只做「收到一条帧 → 回一条帧」。传输（命名管道）
//! 在 `pipe.rs`，这样 M5 换平台时这一半完全不用动，也能在任意平台上跑单测。
//!
//! ## 查询语义（M3.3 起）
//!
//! 走 `heinrich_core::pinyin::query_with_limit`：一段输入同时交给五笔与拼音，
//! 按段质量判定语言（D46），首屏只放判定语言，另一种折叠在后面。没有拼音词库时
//! 退化成纯五笔，行为与 M1/M2 完全一致。
//!
//! 五笔侧仍只认精确码长（D45）：前缀兜底一律丢弃，否则 `xxv` 这种「还没打完」
//! 会弹出一屏码前缀撞车的字。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use heinrich_core::scheme::user as user_words;
use heinrich_core::scheme::{Config, HintMode, PinyinLexicon, Roots, Scheme};
use heinrich_core::{pinyin, MemoryStore, RankingConfig, Source, Tier, Usage, UsageStore};
use heinrich_proto::{
    feature, source as proto_source, Entry, Frame, Mode, ProtoKeyRoots, MAX_ENTRIES, PROTO_VERSION,
};

use crate::store::SqliteStore;

/// 两次 `wal_checkpoint` 之间的最小间隔（§5.5）。
const CHECKPOINT_EVERY: Duration = Duration::from_secs(60);

/// 配置文件最短重查间隔。
///
/// 查询发生在**每个字母键**上（D37），所以不能每次都去 stat 配置文件。1 秒足够
/// 「改完配置切回输入框就生效」，同时把文件系统调用压到每秒最多一次。
const HINT_RECHECK: Duration = Duration::from_secs(1);

/// 自定义词库的最短重查间隔（D65）。与配置文件同一条理由：按键路径上不能每次 stat。
const WORDS_RECHECK: Duration = Duration::from_secs(1);

/// 学习数据的两种落地形态。
///
/// 上层（`Host` / `lookup`）只认 `UsageStore`，这里做一层枚举转发：SQLite 是常态，
/// 内存实现留给两种情况——单测（不想碰磁盘）、以及**学习库打不开时的降级**
/// （目录只读、库文件被别的版本占着、磁盘满了……）。降级后输入法照常工作，
/// 只是不记学习数据；这比「因为学习库坏了导致打不了字」正确得多。
pub enum Store {
    Sqlite(Box<SqliteStore>),
    Memory(MemoryStore),
}

impl Store {
    /// 一句话说明当前形态，启动日志用。
    pub fn describe(&self) -> String {
        match self {
            Store::Sqlite(s) => format!(
                "SQLite（{}，已记录 {} 个词）",
                s.path().display(),
                s.known_words()
            ),
            Store::Memory(_) => "内存（不落盘，重启即丢）".to_string(),
        }
    }

    fn checkpoint(&self) {
        if let Store::Sqlite(s) = self {
            s.checkpoint();
        }
    }
}

impl UsageStore for Store {
    fn usage(&self, word: &str) -> Usage {
        match self {
            Store::Sqlite(s) => s.usage(word),
            Store::Memory(m) => m.usage(word),
        }
    }

    fn tier(&self, word: &str) -> Tier {
        match self {
            Store::Sqlite(s) => s.tier(word),
            Store::Memory(m) => m.tier(word),
        }
    }

    fn record(&mut self, word: &str, at_unix: i64) {
        match self {
            Store::Sqlite(s) => s.record(word, at_unix),
            Store::Memory(m) => m.record(word, at_unix),
        }
    }

    fn set_tier(&mut self, word: &str, tier: Tier) {
        match self {
            Store::Sqlite(s) => s.set_tier(word, tier),
            Store::Memory(m) => m.set_tier(word, tier),
        }
    }
}

pub struct Host {
    scheme: Scheme,
    /// 拼音词库。`None` = 只有五笔（M1/M2 的行为，也是干净环境的降级形态）
    pinyin: Option<PinyinLexicon>,
    store: Store,
    cfg: RankingConfig,
    /// 中英模式。**这是权威副本**：DLL 每个宿主进程各有一份实例，
    /// 模式若存在实例里，A 应用切英文、B 应用仍是中文（M1.1 的既有缺陷）。
    mode: Mode,
    /// 上次 checkpoint 的时刻。放在 `handle` 里按时间触发，不额外开线程——
    /// checkpoint 要独占连接，单独开线程只会和查询抢锁。
    last_checkpoint: Instant,
    /// 本次运行累计收到的上屏回报条数（日志里看「学习有没有在动」）。
    recorded: u64,
    /// 码提示的生效模式，带配置文件热更新。
    hint: HintState,
    /// 字根表（§6.7 键位图）。`None` = 字根表没装成，DLL 那边就不接管
    /// `Ctrl+Shift+N`——键位图是锦上添花，不能因为它挡住打字。
    roots: Option<Roots>,
    /// 自定义词组（D65），带词库文件热更新。
    words: WordsState,
}

/// 自定义词库的热更新状态（D65）。
///
/// 与 [`HintState`] 同一套节流 + stamp 打法：查询路径上只 stat 一次，文件真变了
/// 才重读词库。重读是**全量替换**（[`Scheme::apply_user_words`] 自己会截回基础库），
/// 所以「删掉一行 → 1 秒后那个词真的消失」。
struct WordsState {
    /// `None` = 不读用户词库（单测、`--no-words`）
    path: Option<PathBuf>,
    /// 上次看到的 `(长度, 修改时间)`
    stamp: Option<(u64, Option<SystemTime>)>,
    /// 上次检查的时刻（节流）
    checked_at: Instant,
    /// 当前生效的自定义词条数（日志与 `Reloaded` 用）
    count: usize,
}

impl WordsState {
    fn new() -> Self {
        Self {
            path: None,
            stamp: None,
            checked_at: Instant::now(),
            count: 0,
        }
    }

    /// 文件有没有变过（`None` 路径恒为「没变」）。
    fn changed(&mut self) -> bool {
        let Some(path) = &self.path else {
            return false;
        };
        let meta = std::fs::metadata(path).ok();
        let stamp = meta
            .as_ref()
            .map(|m| (m.len(), m.modified().ok()))
            .unwrap_or((0, None));
        if self.stamp.as_ref() == Some(&stamp) {
            return false;
        }
        self.stamp = Some(stamp);
        true
    }
}

/// 码提示（§6.3）的生效模式。
///
/// 有三层来源，优先级从高到低：命令行 `--hint`（`forced`）、配置文件、默认 `mixed`。
/// 配置文件**热更新**：改完不必重启 server，下次查询（≤1 秒后）就生效。这样做不是
/// 为了炫技——反馈回来的第一句话一定是「我想看看 off 长什么样」，为此杀掉常驻
/// server 再等冷启动不值得。
struct HintState {
    /// `None` = 不读配置文件（单测、`--no-config`）。
    path: Option<PathBuf>,
    /// 命令行强制值，压过配置文件。
    forced: Option<HintMode>,
    mode: HintMode,
    /// 配置文件里的 `ranking.learning_gain` / `ranking.half_life_days`。
    /// `None` = 没写，沿用当前值（§5 / D58）。
    ranking_gain: Option<f64>,
    ranking_half_life: Option<f64>,
    /// 上次检查文件的时刻（节流，见 [`HINT_RECHECK`]）。
    checked_at: Instant,
    /// 上次看到的 `(长度, 修改时间)`。**只有它变了才重读内容**：stat 比 read 便宜得多，
    /// 而且「文件没变」是最常见的情况。
    stamp: Option<(u64, Option<SystemTime>)>,
}

impl HintState {
    fn new(path: Option<PathBuf>, forced: Option<HintMode>) -> Self {
        let mut state = Self {
            path,
            forced,
            mode: forced.unwrap_or_default(),
            ranking_gain: None,
            ranking_half_life: None,
            checked_at: Instant::now(),
            stamp: None,
        };
        state.reload();
        state
    }

    /// 当前模式，顺便按节流检查配置文件有没有被改过。
    fn mode(&mut self) -> HintMode {
        if self.path.is_none() {
            return self.mode;
        }
        if self.checked_at.elapsed() < HINT_RECHECK {
            return self.mode;
        }
        self.checked_at = Instant::now();
        self.reload();
        self.mode
    }

    fn reload(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let meta = std::fs::metadata(&path).ok();
        let stamp = meta
            .as_ref()
            .map(|m| (m.len(), m.modified().ok()))
            .unwrap_or((0, None));
        if self.stamp.as_ref() == Some(&stamp) {
            return;
        }
        self.stamp = Some(stamp);

        let cfg = Config::load(&path);
        for w in cfg.warnings() {
            srv_error!("配置 {}：{w}", path.display());
        }
        let mode = self.forced.unwrap_or_else(|| cfg.hint_mode());
        if mode != self.mode || !cfg.found() {
            srv_info!(
                "码提示：{}（{}）",
                mode.as_str(),
                if cfg.found() {
                    path.display().to_string()
                } else {
                    "未找到配置文件，用默认值".to_string()
                }
            );
        }
        self.mode = mode;

        // 排序参数同理热更新。`None`（没写这一行）不覆盖，继续用上次/默认值——
        // 否则用户删掉这行会以为「恢复默认」，实际是被静默改成 0（永不学习）。
        self.ranking_gain = cfg.ranking_gain();
        self.ranking_half_life = cfg.ranking_half_life_days();
    }
}

impl Host {
    /// 从一个方案目录装载（`scheme.toml` + `lexicon.tsv`）。
    ///
    /// 136,239 条约 300 ms，**只该在一个进程里发生一次**——这是 server 存在的
    /// 全部理由：DLL 被注入到每个宿主进程，在那里装载等于每个 tab 卡 300 ms。
    ///
    /// `db` 为 `None` 时用内存实现（单测 / `--selftest`）；给路径则落盘，
    /// **打不开就降级成内存**并记一条错误，绝不让学习库的问题挡住打字。
    #[cfg(test)]
    pub fn load(dir: &Path) -> Result<Self, String> {
        Self::open(dir, None, None)
    }

    /// `pinyin_dir` 给了路径就加载拼音词库并启用混输；**加载失败只记一条错误、
    /// 继续以纯五笔运行**——拼音数据装坏了不该让用户完全打不了字，这与学习库
    /// 打不开就降级成内存是同一条原则。
    pub fn open(dir: &Path, db: Option<&Path>, pinyin_dir: Option<&Path>) -> Result<Self, String> {
        let scheme =
            Scheme::load(dir).map_err(|e| format!("装载方案 {} 失败：{e}", dir.display()))?;
        let pinyin = match pinyin_dir {
            None => None,
            Some(pd) => match PinyinLexicon::load(pd) {
                Ok(mut lex) => {
                    // 码提示的数据前提：拼音词库里没有五笔码，装载时反查一次。
                    let coded = lex.attach_wubi_codes(&scheme);
                    srv_info!(
                        "拼音词库：{} 条（{} 个音节），混输已启用；其中 {} 条能给出五笔全码（码提示可用）",
                        lex.entry_count,
                        lex.syllable_count,
                        coded
                    );
                    Some(lex)
                }
                Err(e) => {
                    srv_error!("装载拼音词库 {} 失败：{e}；本次以纯五笔运行", pd.display());
                    None
                }
            },
        };
        let store = match db {
            None => Store::Memory(MemoryStore::new()),
            Some(path) => match SqliteStore::open(path) {
                Ok(s) => Store::Sqlite(Box::new(s)),
                Err(e) => {
                    srv_error!("{e}；本次运行不记学习数据");
                    Store::Memory(MemoryStore::new())
                }
            },
        };
        srv_info!("学习库：{}", store.describe());
        // 字根表与方案在同一个目录（`data/wubi86/roots.toml`）。装不上只降级：
        // 没有它只是键位图不弹，与「打不了字」是两件完全不同量级的事。
        let roots = match Roots::load(dir) {
            Ok(r) => {
                srv_info!(
                    "字根表：{} 键 / {} 条主字根",
                    r.keys().len(),
                    r.total_roots()
                );
                Some(r)
            }
            Err(e) => {
                srv_error!("{e}；本次不提供键位图（Ctrl+Shift+N 让给宿主）");
                None
            }
        };
        Ok(Self {
            scheme,
            pinyin,
            store,
            cfg: RankingConfig::default(),
            mode: Mode::Chinese,
            last_checkpoint: Instant::now(),
            recorded: 0,
            hint: HintState::new(None, None),
            roots,
            words: WordsState::new(),
        })
    }

    /// 接上自定义词库：`path` 为 `None` 表示不读（单测与 `--selftest` 的默认）。
    ///
    /// 与 `configure_hint` 同一条理由——不把「读用户目录」写进 `open`，否则单测
    /// 会意外吃到开发机上的 `words.tsv`。
    pub fn configure_words(&mut self, path: Option<PathBuf>) {
        self.words.path = path;
        self.words.stamp = None;
        self.reload_user_words();
    }

    /// 重读自定义词库并合并（D65），返回生效条数。
    ///
    /// 文件不存在就是「一个自定义词都没有」的正常状态；读失败只记日志、保留上一份
    /// ——用户正在编辑时文件可能短暂不可读，那不该让已生效的词突然消失。
    pub fn reload_user_words(&mut self) -> usize {
        let Some(path) = self.words.path.clone() else {
            return self.words.count;
        };
        let file = match user_words::load(&path) {
            Ok(f) => f,
            Err(e) => {
                srv_error!("读取自定义词库 {} 失败：{e}；保留上一份", path.display());
                return self.words.count;
            }
        };
        for w in &file.warnings {
            srv_error!("自定义词库 {}：{w}", path.display());
        }
        let (resolved, warns) =
            user_words::resolve(&file.words, &self.scheme, self.pinyin.as_ref());
        for w in &warns {
            srv_error!("自定义词库 {}：{w}", path.display());
        }
        let (wubi, pin) = user_words::split_for_merge(&resolved);
        let n_wubi = self.scheme.apply_user_words(&wubi);
        let n_pin = match self.pinyin.as_mut() {
            Some(lex) => lex.apply_user_words(&pin),
            None => 0,
        };
        let count = n_wubi.max(n_pin);
        if count != self.words.count || self.words.count == 0 {
            srv_info!(
                "自定义词库：{} 条生效（五笔 {} / 拼音 {}）· {}",
                count,
                n_wubi,
                n_pin,
                path.display()
            );
        }
        self.words.count = count;
        count
    }

    /// 接上码提示配置：`path` 为 `None` 表示不读文件，`forced` 压过文件值。
    ///
    /// 单独一个方法而不是塞进 `open` 的参数表：单测与 `--selftest` 都不该碰
    /// 用户目录里的文件，默认「不读任何配置」才是最安全的起点。
    pub fn configure_hint(&mut self, path: Option<PathBuf>, forced: Option<HintMode>) {
        self.hint = HintState::new(path, forced);
        srv_info!(
            "码提示：{}（{}{}）",
            self.hint.mode.as_str(),
            if self.hint.path.is_some() {
                "配置文件 "
            } else {
                "不读配置文件"
            },
            self.hint
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );
    }

    /// 当前生效的码提示模式（读它会顺手做一次配置热检查）。
    pub fn hint_mode(&mut self) -> HintMode {
        self.hint.mode()
    }

    /// 把配置文件里的排序参数搬进 `cfg`（§5 / D58）。
    ///
    /// **`None` 不覆盖**：配置里没写这一行，就沿用 `open` 时的默认值。若把
    /// 「没有」当成 0 处理，用户删掉那行想恢复默认，实际会变成「永不学习」。
    ///
    /// 变化时打一行日志——这两个参数决定「词序多快变」，改了没反应时这一行
    /// 是唯一的抓手（DLL 侧还有缓存，见 D57）。
    fn sync_ranking(&mut self) {
        if let Some(g) = self.hint.ranking_gain {
            if (self.cfg.learning_gain - g).abs() > f64::EPSILON {
                self.cfg.learning_gain = g;
                srv_info!("学习增益 g = {g}（来自配置文件）");
            }
        }
        if let Some(h) = self.hint.ranking_half_life {
            if (self.cfg.half_life_days - h).abs() > f64::EPSILON {
                self.cfg.half_life_days = h;
                srv_info!("半衰期 H = {h} 天（来自配置文件）");
            }
        }
    }

    pub fn scheme_id(&self) -> &str {
        &self.scheme.meta.id
    }

    pub fn entry_count(&self) -> usize {
        self.scheme.entry_count()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// 能力位，握手时告诉 DLL「该不该放开拼音那套行为」。
    pub fn features(&self) -> u8 {
        let mut f = 0u8;
        if self.pinyin.is_some() {
            f |= feature::PINYIN;
        }
        f
    }

    pub fn has_pinyin(&self) -> bool {
        self.pinyin.is_some()
    }

    /// 字根表这一帧（§6.7）。`None` = 字根表没装成。
    ///
    /// **不是应答**：由 `pipe.rs` 在每条连接第一次握手成功后主动发一次。
    /// 数据是装配期固定的，一条连接里不会变，没必要每次 `Hello` 都重发。
    pub fn roots_frame(&self) -> Option<Frame> {
        let roots = self.roots.as_ref()?;
        Some(Frame::Roots {
            keys: roots
                .keys()
                .iter()
                .map(|k| {
                    let mut all: String = k.roots.iter().collect();
                    all.extend(k.variants.iter());
                    ProtoKeyRoots {
                        key: k.key,
                        primary: k.primary,
                        // 主字根个数是**字符数**，不是字节数：`main_len` 由 DLL
                        // 拿去切前几位画实色，按字节切会把多字节字切开。
                        main_len: k.roots.len().min(u8::MAX as usize) as u8,
                        roots: all,
                    }
                })
                .collect(),
        })
    }

    /// 本次运行收到的上屏回报条数。
    pub fn recorded(&self) -> u64 {
        self.recorded
    }

    /// 处理一条请求，返回要回给对方的帧（`None` 表示不回）。
    pub fn handle(&mut self, frame: Frame) -> Option<Frame> {
        self.maybe_checkpoint();
        match frame {
            Frame::Hello { proto, scheme_id } => {
                if proto != PROTO_VERSION {
                    return Some(Frame::Bye {
                        reason: format!("协议版本不匹配：客户端 {proto} / 服务端 {PROTO_VERSION}"),
                    });
                }
                if scheme_id != self.scheme_id() {
                    return Some(Frame::Bye {
                        reason: format!(
                            "方案不匹配：客户端 {scheme_id} / 服务端 {}",
                            self.scheme_id()
                        ),
                    });
                }
                Some(Frame::Welcome {
                    proto: PROTO_VERSION,
                    scheme_id: self.scheme_id().to_string(),
                    entry_count: self.entry_count() as u32,
                    mode: self.mode,
                    features: self.features(),
                })
            }
            Frame::Query { code, limit } => {
                let reply = self.query(&code, limit);
                Some(Frame::Result {
                    code,
                    entries: reply.entries,
                    source: reply.source,
                    primary_len: reply.primary_len,
                })
            }
            Frame::SetMode { mode } => {
                self.mode = mode;
                Some(Frame::Mode { mode })
            }
            // 上屏回报：单向，不回帧。DLL 那边不等应答，回一条反而会让它读错位。
            Frame::Record {
                code,
                word,
                at_unix,
            } => {
                self.store.record(&word, at_unix);
                self.recorded += 1;
                srv_debug!("上屏 {code} → {word}（第 {} 条）", self.recorded);
                None
            }
            // Welcome / Result / Mode / Bye / Roots 是**服务端发出去的**方向，
            // 收到它们只能说明对端把方向搞反了，不理。
            Frame::Welcome { .. }
            | Frame::Result { .. }
            | Frame::Mode { .. }
            | Frame::Bye { .. }
            | Frame::Roots { .. } => None,
        }
    }

    /// 到点就把 WAL 合并一次。这是在 `handle` 里顺手做的，代价是一次时间比较。
    fn maybe_checkpoint(&mut self) {
        if self.last_checkpoint.elapsed() >= CHECKPOINT_EVERY {
            self.last_checkpoint = Instant::now();
            self.store.checkpoint();
            srv_debug!("wal_checkpoint 完成，累计上屏回报 {} 条", self.recorded);
        }
    }

    /// 退出前调一次，把 WAL 落干净。
    pub fn shutdown(&mut self) {
        self.store.checkpoint();
        srv_info!(
            "退出：本次运行收到 {} 条上屏回报，学习库 {}",
            self.recorded,
            self.store.describe()
        );
    }

    /// 混输查询。`limit` 上限见 [`MAX_ENTRIES`]。
    ///
    /// 返回的不只是候选：还有**判定语言**与**首屏条数**——DLL 要据此把折叠区
    /// （另一种语言的候选）排到首屏之后，而它自己无法判定（判定要看两份词库）。
    ///
    /// `&mut self` 是因为要顺手做一次码提示配置的热检查；这次检查被 [`HINT_RECHECK`]
    /// 节流，绝大多数调用只花一次 `Instant::now()`。
    pub fn query(&mut self, code: &str, limit: u8) -> QueryReply {
        let limit = (limit as usize).clamp(1, MAX_ENTRIES);
        let now = unix_now();
        // 先做一次配置热检查：`hint.mode()` 内部会 reload（含换位门槛），
        // 所以必须在排序之前调用，否则这一拍仍在用上一份配置。
        let hint = self.hint.mode();
        self.sync_ranking();
        // 自定义词库的热检查（D65）。必须排在查询之前，否则这一拍仍在用旧词表。
        if self.words.checked_at.elapsed() >= WORDS_RECHECK {
            self.words.checked_at = Instant::now();
            if self.words.changed() {
                self.reload_user_words();
            }
        }
        let mixed = pinyin::query_with_limit(
            &self.scheme,
            self.pinyin.as_ref(),
            code,
            &self.store,
            now,
            &self.cfg,
            limit,
        );
        let source = mixed.source;
        let proto_src = match source {
            Source::Wubi => proto_source::WUBI,
            Source::Pinyin => proto_source::PINYIN,
        };
        // 折叠分界要在截断**之前**算：首屏可能有 30 条、而协议只让带 9 条，
        // 那 9 条就全是首屏的。若先截断再取 len，DLL 会以为折叠区从第 9 条开始，
        // 把纯五笔的结果标成「拼音在前」。
        let primary_total = mixed.primary.len();
        let primary_len = primary_total.min(limit) as u16;

        // 码提示的裁决全在 server：DLL 只画非空的 `code`（§6.3）。
        // 规则是「提示跟随这一段是怎么出来的」——拼音段给码、五笔段不给。
        //
        // 两个例外：
        //
        // - **通配**（`vbz`，`show_codes`，M3.5/D62）：必须显示码，而且显示的是
        //   `Candidate.code` 而不是 `wubi`——通配时 `code` 就是那条词自己的五笔
        //   全码（用户要的正是「这些字怎么打」）。这种模式下 `wubi` 是空串，
        //   沿用老路径会画出空 chip。
        // - **多段**（D64）：提示是**词中 chip**（`Candidate.chip`），挂在拼音段
        //   字后面、内容是该字的五笔全码。chip 存在就显示，不问 `hint.mode`——
        //   多段的拼音字本身就是「用户不会拆」的那半个词，这正是教学时刻。
        let show_codes = mixed.show_codes;
        let primary_is_pinyin = source == Source::Pinyin;
        let entries: Vec<Entry> = mixed
            .into_all()
            .into_iter()
            .take(limit)
            .enumerate()
            .map(|(i, c)| {
                let in_primary = i < primary_total;
                let pinyin_segment = if in_primary {
                    primary_is_pinyin
                } else {
                    !primary_is_pinyin
                };
                let show = show_codes
                    || pinyin_segment
                        && match hint {
                            HintMode::Mixed => true,
                            // 「第一次」按**这个词还没上过屏**判定，不区分当初是用拼音
                            // 还是五笔打出来的。这里的近似对用户更宽容：用过的词就是认识的。
                            HintMode::FirstTime => self.store.usage(&c.word).n == 0,
                            HintMode::Off => false,
                        };
                // 词中 chip（D64）压过一切常规路径：位置与内容都由 core 算好。
                let (code, hint_at) = match c.chip {
                    Some(chip) => (chip.code, chip.chars_before.min(u8::MAX as usize) as u8),
                    // 通配（show_codes）的码在 `code` 里（它自己的五笔全码）；
                    // 常规拼音段的码在 `wubi` 里（装载期反查的全码）。
                    None if show_codes => (c.code, 0),
                    None if show => (c.wubi, 0),
                    None => (String::new(), 0),
                };
                Entry {
                    word: c.word,
                    label: c.kind_label.to_string(),
                    code,
                    hint_at,
                }
            })
            .collect();
        QueryReply {
            entries,
            source: proto_src,
            primary_len,
        }
    }
}

/// 一次查询的答复。
///
/// 拆成结构体而不是元组，是因为 `primary_len` 与 `source` 语义相关却容易被写反：
/// 拼音候选的 `primary_len` 是拼音条的条数，五笔候选的则是五笔条的条数。
pub struct QueryReply {
    pub entries: Vec<Entry>,
    /// 首屏候选的语言，见 `heinrich_proto::source`
    pub source: u8,
    /// 前多少条属于判定语言（首屏）；其余是折叠区
    pub primary_len: u16,
}

/// 当前 Unix 秒。
///
/// `heinrich-core` 明确不读时钟（时间一律由调用方传入，保证排序在测试里可复现），
/// 所以读时钟的责任落在宿这边。
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 `heinrich-core` 共用同一份迷你夹具，避免第二份数据漂移。
    fn fixture() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../heinrich-core/tests/fixtures/mini")
    }

    fn host() -> Host {
        Host::load(&fixture()).expect("夹具应可装载")
    }

    fn hello() -> Frame {
        Frame::Hello {
            proto: PROTO_VERSION,
            scheme_id: "mini".into(),
        }
    }

    #[test]
    fn hello_gets_welcome_with_mode() {
        let mut h = host();
        let reply = h.handle(hello()).expect("应回应");
        match reply {
            Frame::Welcome {
                proto,
                scheme_id,
                entry_count,
                mode,
                features,
            } => {
                assert_eq!(proto, PROTO_VERSION);
                assert_eq!(scheme_id, "mini");
                assert_eq!(entry_count, 12);
                assert_eq!(mode, Mode::Chinese);
                assert_eq!(features, 0, "没装拼音词库时不该声称有拼音能力");
            }
            other => panic!("期望 Welcome，得到 {other:?}"),
        }
    }

    #[test]
    fn wrong_proto_is_rejected() {
        let mut h = host();
        let reply = h.handle(Frame::Hello {
            proto: PROTO_VERSION + 1,
            scheme_id: "mini".into(),
        });
        assert!(matches!(reply, Some(Frame::Bye { .. })));
    }

    #[test]
    fn exact_hit_returns_candidates() {
        let mut h = host();
        let reply = h
            .handle(Frame::Query {
                code: "vbg".into(),
                limit: 9,
            })
            .expect("应回应");
        match reply {
            Frame::Result {
                code,
                entries,
                source,
                primary_len,
            } => {
                assert_eq!(code, "vbg");
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].word, "好");
                assert_eq!(source, proto_source::WUBI);
                assert_eq!(primary_len, 1, "唯一那条属于首屏");
            }
            other => panic!("期望 Result，得到 {other:?}"),
        }
    }

    /// 无命中的码必须回空列表，而不是前缀兜底——否则「打错了」会蹦出无关的字。
    #[test]
    fn unknown_code_returns_empty() {
        let mut h = host();
        match h.handle(Frame::Query {
            code: "zz".into(),
            limit: 9,
        }) {
            Some(Frame::Result { entries, .. }) => assert!(entries.is_empty()),
            other => panic!("期望空 Result，得到 {other:?}"),
        }
    }

    /// `v` 有精确命中（一级简码「发」），不能被前缀展开污染成几千个候选。
    #[test]
    fn one_letter_code_is_exact() {
        let mut h = host();
        match h.handle(Frame::Query {
            code: "v".into(),
            limit: 9,
        }) {
            Some(Frame::Result { entries, .. }) => {
                assert_eq!(entries.len(), 1, "一级简码只有一条");
                assert_eq!(entries[0].word, "发");
            }
            other => panic!("期望 Result，得到 {other:?}"),
        }
    }

    #[test]
    fn limit_is_honoured() {
        let mut h = host();
        match h.handle(Frame::Query {
            code: "vbgd".into(),
            limit: 1,
        }) {
            Some(Frame::Result { entries, .. }) => assert_eq!(entries.len(), 1),
            other => panic!("期望 Result，得到 {other:?}"),
        }
    }

    #[test]
    fn set_mode_is_remembered() {
        let mut h = host();
        let reply = h.handle(Frame::SetMode {
            mode: Mode::English,
        });
        assert_eq!(
            reply,
            Some(Frame::Mode {
                mode: Mode::English
            })
        );
        assert_eq!(h.mode(), Mode::English);
        // 重新握手要能问到最新模式
        match h.handle(hello()) {
            Some(Frame::Welcome { mode, .. }) => assert_eq!(mode, Mode::English),
            other => panic!("期望 Welcome，得到 {other:?}"),
        }
    }

    /// 上屏时间必须**贴近现在**：`query` 用的是真实时钟，`Δt` 一大，
    /// 学习分就被半衰期衰减到 0（`0.5^(Δt/45天)`），测出来的就是「没排序」。
    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    fn words(h: &mut Host, code: &str) -> Vec<String> {
        h.query(code, 9)
            .entries
            .into_iter()
            .map(|e| e.word)
            .collect()
    }

    /// 上屏回报要真的进排序：**第一次不计分**（D58），第二次起才动。
    ///
    /// 这是 M2.4 验收项 ① 的自动化版本。三个阶段的含义各不相同：
    /// ① 一次不改顺序（防误选）；② 两次就开始动（学了要看得见）；
    /// ③ 用到十次能越过码表首位（「越用越顺手」不是空话）。
    #[test]
    fn record_reorders_from_the_second_use() {
        let mut h = host();
        assert_eq!(words(&mut h, "vbgd"), ["号", "豪", "呵", "蒿"]);

        h.handle(Frame::Record {
            code: "vbgd".into(),
            word: "蒿".into(),
            at_unix: now(),
        });
        assert_eq!(
            words(&mut h, "vbgd"),
            ["号", "豪", "呵", "蒿"],
            "第一次上屏不计分，顺序不动"
        );
        assert_eq!(h.recorded(), 1);

        // 第二次起开始加分：越过静态权重更低的「呵」。
        h.handle(Frame::Record {
            code: "vbgd".into(),
            word: "蒿".into(),
            at_unix: now(),
        });
        assert_eq!(words(&mut h, "vbgd"), ["号", "豪", "蒿", "呵"]);
        assert_eq!(h.recorded(), 2);

        // 打到十次：越过码表首位。旧设计的 1.5 倍门槛与 D54 的 δ 都做不到这一点
        // ——蒿的静态权重只有号的 45%，攒分攒不到那个份上。
        for _ in 0..8 {
            h.handle(Frame::Record {
                code: "vbgd".into(),
                word: "蒿".into(),
                at_unix: now(),
            });
        }
        assert_eq!(words(&mut h, "vbgd")[0], "蒿", "常用就该排前面");
        assert_eq!(h.recorded(), 10);
    }

    /// 上屏回报是**单向**帧：不该回任何东西（回了会让 DLL 读到错位的帧）。
    #[test]
    fn record_gets_no_reply() {
        let mut h = host();
        let reply = h.handle(Frame::Record {
            code: "vbg".into(),
            word: "好".into(),
            at_unix: 1,
        });
        assert!(reply.is_none());
    }

    /// 学习数据落盘：换一个 `Host` 打开同一个库，词序应该已经是学过的样子。
    #[test]
    fn learning_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("heinrich-host-db-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("learn.db");

        {
            let mut h = Host::open(&fixture(), Some(&db), None).expect("应能打开");
            for _ in 0..20 {
                h.handle(Frame::Record {
                    code: "vbgd".into(),
                    word: "蒿".into(),
                    at_unix: now(),
                });
            }
            h.shutdown();
        }

        let mut h2 = Host::open(&fixture(), Some(&db), None).expect("应能重开");
        assert_eq!(
            words(&mut h2, "vbgd")[0],
            "蒿",
            "连用 20 次后重开应还排在第一"
        );
        assert_eq!(h2.recorded(), 0, "新进程的计数从零开始");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 学习库打不开时必须降级成内存实现，而不是让 server 起不来。
    #[test]
    fn broken_db_falls_back_to_memory() {
        // 拿一个目录当库文件路径：open 一定失败。
        let dir = std::env::temp_dir().join(format!("heinrich-bad-db-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut h = Host::open(&fixture(), Some(&dir), None).expect("降级后仍应能启动");
        assert!(words(&mut h, "vbg").contains(&"好".to_string()));
        h.handle(Frame::Record {
            code: "vbg".into(),
            word: "好".into(),
            at_unix: 1,
        });
        assert_eq!(h.recorded(), 1, "降级后仍照常接收回报，只是不落盘");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- M3.3：混输接进 server ----

    /// 拼音夹具跟 `heinrich-scheme` 走（`PinyinLexicon` 在那里），只此一份。
    fn pinyin_fixture() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../heinrich-scheme/tests/fixtures/pinyin-mini")
    }

    fn mixed_host() -> Host {
        Host::open(&fixture(), None, Some(&pinyin_fixture())).expect("混输宿主应可装载")
    }

    fn features_of(h: &mut Host) -> u8 {
        match h.handle(hello()) {
            Some(Frame::Welcome { features, .. }) => features,
            other => panic!("期望 Welcome，得到 {other:?}"),
        }
    }

    /// 装了拼音词库，握手要声明能力位——DLL 全靠它决定要不要放开 `z` 和 4 码上限。
    #[test]
    fn welcome_declares_pinyin_feature() {
        assert_eq!(
            features_of(&mut mixed_host()) & feature::PINYIN,
            feature::PINYIN
        );
        assert_eq!(features_of(&mut host()) & feature::PINYIN, 0);
    }

    /// 装了拼音后 `vbg` 仍是五笔：`v` 不是任何音节的起始，判不出拼音。
    #[test]
    fn wubi_code_still_wubi_with_pinyin_loaded() {
        let mut h = mixed_host();
        match h.handle(Frame::Query {
            code: "vbg".into(),
            limit: 9,
        }) {
            Some(Frame::Result {
                entries,
                source,
                primary_len,
                ..
            }) => {
                assert_eq!(source, proto_source::WUBI);
                assert_eq!(entries[0].word, "好");
                assert_eq!(primary_len as usize, entries.len(), "五笔没有折叠区");
            }
            other => panic!("期望 Result，得到 {other:?}"),
        }
    }

    /// `hao` 既是「字母串」又恰好全是字根键：判定为拼音，首屏全是拼音候选，
    /// `primary_len` 指向首屏末尾（折叠区从它之后开始）。
    /// D48：`hao` 两边都命中时五笔占主段，`primary_len` 就是五笔那几条。
    #[test]
    fn wubi_leads_and_pinyin_follows() {
        let mut h = mixed_host();
        match h.handle(Frame::Query {
            code: "hao".into(),
            limit: 9,
        }) {
            Some(Frame::Result {
                entries,
                source,
                primary_len,
                ..
            }) => {
                assert_eq!(source, proto_source::WUBI);
                assert_eq!(entries[0].word, "虚");
                // 夹具里 hao 的两条五笔词（虚、好）
                assert_eq!(primary_len, 2);
                assert_eq!(entries[1].word, "好");
                // 拼音紧接其后：精确区（号、浩）+ 前缀区（好评、好吃）
                assert_eq!(
                    entries[2..]
                        .iter()
                        .map(|e| e.word.as_str())
                        .collect::<Vec<_>>(),
                    ["号", "浩", "好评", "好吃"]
                );
            }
            other => panic!("期望 Result，得到 {other:?}"),
        }
    }

    /// 五笔没有这个词条时拼音接管主段，`primary_len` 覆盖全部候选。
    #[test]
    fn pinyin_leads_when_wubi_misses() {
        let mut h = mixed_host();
        match h.handle(Frame::Query {
            code: "xian".into(),
            limit: 9,
        }) {
            Some(Frame::Result {
                entries,
                source,
                primary_len,
                ..
            }) => {
                assert_eq!(source, proto_source::PINYIN);
                assert_eq!(entries[0].word, "先");
                assert_eq!(primary_len as usize, entries.len());
            }
            other => panic!("期望 Result，得到 {other:?}"),
        }
    }

    /// `'` 是显式拼音标记：同一串码的五笔候选全部让开。
    #[test]
    fn apostrophe_skips_wubi_side() {
        let mut h = mixed_host();
        match h.handle(Frame::Query {
            code: "hao'".into(),
            limit: 9,
        }) {
            Some(Frame::Result {
                entries,
                source,
                primary_len,
                ..
            }) => {
                assert_eq!(source, proto_source::PINYIN);
                assert_eq!(entries[0].word, "好");
                assert!(
                    !entries.iter().any(|e| e.word == "虚"),
                    "带标记时五笔侧一条都不该出"
                );
                assert_eq!(primary_len as usize, entries.len());
            }
            other => panic!("期望 Result，得到 {other:?}"),
        }
    }

    /// `primary_len` 是**截断后**的主段条数：协议只让带 1 条时，主段也只算 1 条。
    #[test]
    fn primary_len_is_clamped_by_limit() {
        let mut h = mixed_host();
        match h.handle(Frame::Query {
            code: "hao".into(),
            limit: 1,
        }) {
            Some(Frame::Result {
                entries,
                primary_len,
                ..
            }) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(primary_len, 1);
            }
            other => panic!("期望 Result，得到 {other:?}"),
        }
    }

    /// 拼音数据装坏了（路径不存在 / 文件缺）只降级成纯五笔，不能挡住打字。
    #[test]
    fn broken_pinyin_dir_degrades_to_wubi() {
        let missing = std::env::temp_dir().join("heinrich-no-such-pinyin-dir");
        let mut h = Host::open(&fixture(), None, Some(&missing)).expect("应能启动");
        assert!(!h.has_pinyin());
        assert_eq!(features_of(&mut h) & feature::PINYIN, 0);
        assert_eq!(words(&mut h, "vbg"), ["好"]);
    }

    /// 拼音的上屏回报与五笔共用一份学习数据：学过的词在拼音候选里也上浮。
    ///
    /// 连打三次「浩」越过同音的「号」；主段「虚」不动，因为它在五笔那一段里，
    /// 两条区各自独立排序（§6.2）。学习分不区分语言，也不区分这一段是五笔还是
    /// 拼音查出来的——与 `record_reorders_from_the_second_use` 是同一套规则。
    #[test]
    fn learning_is_shared_between_wubi_and_pinyin() {
        let mut h = mixed_host();
        let before = words(&mut h, "hao");
        assert_eq!(
            before,
            ["虚", "好", "号", "浩", "好评", "好吃"],
            "基线：五笔主段在前，拼音按语料权重跟在后面"
        );

        for _ in 0..3 {
            h.handle(Frame::Record {
                code: "hao".into(),
                word: "浩".into(),
                at_unix: now(),
            });
        }

        let after = words(&mut h, "hao");
        assert_eq!(after[0], "虚", "主段是五笔区，与拼音区各自排序");
        let hao = after.iter().position(|w| w == "浩").unwrap();
        let hao2 = after.iter().position(|w| w == "号").unwrap();
        assert!(hao < hao2, "「浩」应上浮越过「号」：{after:?}");
    }

    /// 通配（`vbz`）：每一条候选都带上**它自己的五笔码**（M3.5 / D62）。
    ///
    /// 这是「提示跟随这一段是怎么出来的」的唯一例外：五笔段平常不给码，但通配问的
    /// 就是「这些字怎么打」，不给码等于看了一屏无关的字。
    #[test]
    fn wildcard_entries_carry_their_own_code() {
        let mut h = mixed_host();
        let (entries, primary_len) = match h.handle(Frame::Query {
            code: "vbz".into(),
            limit: 9,
        }) {
            Some(Frame::Result {
                entries,
                primary_len,
                ..
            }) => (entries, primary_len),
            other => panic!("期望 Result，得到 {other:?}"),
        };
        assert!(!entries.is_empty(), "vb 下面必须有字");
        assert_eq!(primary_len as usize, entries.len(), "通配没有折叠区");
        for e in &entries {
            assert!(
                e.code.starts_with("vb"),
                "「{}」的码应是它自己的全码：{:?}",
                e.word,
                e.code
            );
        }
        // 同一个字不重复出现（`vb` 与 `vbg` 都是「好」）
        let mut ws: Vec<&str> = entries.iter().map(|e| e.word.as_str()).collect();
        ws.sort_unstable();
        let n = ws.len();
        ws.dedup();
        assert_eq!(ws.len(), n, "通配结果按字去重");
    }

    /// 多段（`hao'vb`）：只出词典真词（D64），chip 挂拼音段字后面——
    /// `hint_at = 1` 说明 chip 画在第 1 个字符之后，内容是该字的五笔全码。
    /// 首屏全部算主段（没有折叠区）。
    #[test]
    fn multi_segment_keeps_words_and_chips_mid_word() {
        let mut h = mixed_host();
        let (entries, source, primary_len) = match h.handle(Frame::Query {
            code: "hao'vb".into(),
            limit: 9,
        }) {
            Some(Frame::Result {
                entries,
                source,
                primary_len,
                ..
            }) => (entries, source, primary_len),
            other => panic!("期望 Result，得到 {other:?}"),
        };
        assert_eq!(source, proto_source::PINYIN, "段 1 是拼音");
        assert_eq!(primary_len as usize, entries.len(), "多段没有折叠区");
        assert_eq!(entries.len(), 1, "夹具里只有「好好」是词典真词");
        assert_eq!(entries[0].word, "好好");
        assert_eq!(entries[0].hint_at, 1, "chip 挂在「好」后面");
        // 夹具里「好」有两条全码（hao、vbg），取字典序最小
        assert_eq!(entries[0].code, "hao");
    }

    /// 多段拼不出真词就不出窗（D64 严格模式）：「好你」不是词，`hao'ni'` 给空。
    #[test]
    fn multi_segment_without_real_words_is_empty() {
        let mut h = mixed_host();
        match h.handle(Frame::Query {
            code: "hao'ni'".into(),
            limit: 9,
        }) {
            Some(Frame::Result { entries, .. }) => assert!(entries.is_empty()),
            other => panic!("期望空 Result，得到 {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // 自定义词库（D65）
    // -----------------------------------------------------------------------

    /// 写一份临时词库文件。名字带 pid，避免并发测试互相覆盖。
    fn temp_words(tag: &str, body: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("heinrich-words-{}-{tag}.tsv", std::process::id()));
        std::fs::write(&p, body).expect("写临时词库失败");
        p
    }

    fn words_of(reply: Option<Frame>) -> Vec<String> {
        match reply {
            Some(Frame::Result { entries, .. }) => entries.into_iter().map(|e| e.word).collect(),
            other => panic!("期望 Result，得到 {other:?}"),
        }
    }

    #[test]
    fn user_words_are_queryable() {
        let path = temp_words("add", "好吃\t\tvbkt\n");
        let mut h = host();
        h.configure_words(Some(path.clone()));
        let words = words_of(h.handle(Frame::Query {
            code: "vbkt".into(),
            limit: 9,
        }));
        assert!(words.contains(&"好吃".to_string()), "{words:?}");
        // 词库条数跟着涨：Welcome 里的 entry_count 是「实际能查到多少条」
        match h.handle(hello()) {
            Some(Frame::Welcome { entry_count, .. }) => assert_eq!(entry_count, 13),
            other => panic!("期望 Welcome，得到 {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    /// 用户词的权重照常参与码表序：权重排在谁中间，出现的位置就在谁中间。
    ///
    /// 权重超过词库最大值时会被归一化钳到 1.0——那时它与原有最高权重的词并列，
    /// 由「同分按词排序」定先后。这是「权重与正常词库一致」的必然结果：
    /// 不给用户词任何特权位，想让它更靠前就多打几次，让学习分把它抬上去。
    #[test]
    fn user_word_weight_drives_order() {
        // 夹具里 vbgd 上依次是 号(900) / 豪(600) / 呵(50) / 蒿(20)
        let path = temp_words("order", "好\t\tvbgd\t850\n");
        let mut h = host();
        h.configure_words(Some(path.clone()));
        let words = words_of(h.handle(Frame::Query {
            code: "vbgd".into(),
            limit: 9,
        }));
        assert_eq!(words, ["号", "好", "豪", "呵", "蒿"], "{words:?}");
        let _ = std::fs::remove_file(&path);
    }

    /// 重载是全量替换：文件删光了词就该真的消失，条数回落到 0。
    #[test]
    fn reload_drops_removed_words() {
        let path = temp_words("reload", "好吃\t\tvbkt\n");
        let mut h = host();
        h.configure_words(Some(path.clone()));
        assert!(words_of(h.handle(Frame::Query {
            code: "vbkt".into(),
            limit: 9,
        }))
        .contains(&"好吃".to_string()));

        std::fs::write(&path, "# 清空\n").unwrap();
        assert_eq!(h.reload_user_words(), 0);
        assert!(
            words_of(h.handle(Frame::Query {
                code: "vbkt".into(),
                limit: 9,
            }))
            .is_empty(),
            "重载后旧词应消失"
        );
        let _ = std::fs::remove_file(&path);
    }
}
