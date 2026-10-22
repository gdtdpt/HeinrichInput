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

use heinrich_core::scheme::{Config, HintMode, PinyinLexicon, Scheme};
use heinrich_core::{pinyin, MemoryStore, RankingConfig, Source, Tier, Usage, UsageStore};
use heinrich_proto::{
    feature, source as proto_source, Entry, Frame, Mode, MAX_ENTRIES, PROTO_VERSION,
};

use crate::store::SqliteStore;

/// 两次 `wal_checkpoint` 之间的最小间隔（§5.5）。
const CHECKPOINT_EVERY: Duration = Duration::from_secs(60);

/// 配置文件最短重查间隔。
///
/// 查询发生在**每个字母键**上（D37），所以不能每次都去 stat 配置文件。1 秒足够
/// 「改完配置切回输入框就生效」，同时把文件系统调用压到每秒最多一次。
const HINT_RECHECK: Duration = Duration::from_secs(1);

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
            checked_at: Instant::now(),
            stamp: None,
        };
        state.reload();
        state
    }

    /// 当前模式，顺便按节流检查配置文件有没有被改过。
    fn mode(&mut self) -> HintMode {
        if self.forced.is_some() || self.path.is_none() {
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
        Ok(Self {
            scheme,
            pinyin,
            store,
            cfg: RankingConfig::default(),
            mode: Mode::Chinese,
            last_checkpoint: Instant::now(),
            recorded: 0,
            hint: HintState::new(None, None),
        })
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
            // Welcome / Result / Mode / Bye 是**服务端发出去的**方向，
            // 收到它们只能说明对端把方向搞反了，不理。
            Frame::Welcome { .. }
            | Frame::Result { .. }
            | Frame::Mode { .. }
            | Frame::Bye { .. } => None,
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
        let hint = self.hint.mode();
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
                let show = pinyin_segment
                    && match hint {
                        HintMode::Mixed => true,
                        // 「第一次」按**这个词还没上过屏**判定，不区分当初是用拼音
                        // 还是五笔打出来的。严格版要记录每次上屏的输入来源（协议
                        // `Record` 里其实带着 `code`，但学习库没存），属于 M3.5 的
                        // 范围；这里的近似对用户更宽容：用过的词就是认识的。
                        HintMode::FirstTime => self.store.usage(&c.word).n == 0,
                        HintMode::Off => false,
                    };
                Entry {
                    word: c.word,
                    label: c.kind_label.to_string(),
                    code: if show { c.wubi } else { String::new() },
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
                assert_eq!(entry_count, 11);
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

    /// 上屏回报要真的进排序：用一次「蒿」之后它越过「呵」，但抢不走「号」的首位。
    ///
    /// 这就是 M2.4 验收项 ① 的自动化版本——「偶用一次抢不走首位」不是靠人盯三天，
    /// 而是靠 §5.2 的 K=1.5 门槛；这里验的就是那条规则真的接在 server 上了。
    #[test]
    fn record_reorders_but_keeps_the_throne() {
        let mut h = host();
        assert_eq!(words(&mut h, "vbgd"), ["号", "豪", "呵", "蒿"]);

        h.handle(Frame::Record {
            code: "vbgd".into(),
            word: "蒿".into(),
            at_unix: now(),
        });

        let after = words(&mut h, "vbgd");
        assert_eq!(after[0], "号", "偶用一次不许抢首位");
        assert_eq!(after, ["号", "豪", "蒿", "呵"], "蒿应上浮一位");
        assert_eq!(h.recorded(), 1);
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
    /// 连打三次「浩」应当越过同音的「号」，但仍被 1.5 倍门槛拦在「好」后面——
    /// 与五笔那条 `record_reorders_but_keeps_the_throne` 是同一个规则，
    /// 这里验的是它在**拼音候选**上同样成立（§5.2 不区分语言）。
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
        assert_eq!(after[0], "虚", "偶用三次不许抢主段首位");
        let hao = after.iter().position(|w| w == "浩").unwrap();
        let hao2 = after.iter().position(|w| w == "号").unwrap();
        assert!(hao < hao2, "「浩」应上浮越过「号」：{after:?}");
    }
}
