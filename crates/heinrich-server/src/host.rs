//! 引擎宿主：**平台中立**的那一半 server。
//!
//! 本文件不碰任何 Windows API，只做「收到一条帧 → 回一条帧」。传输（命名管道）
//! 在 `pipe.rs`，这样 M5 换平台时这一半完全不用动，也能在任意平台上跑单测。
//!
//! ## 为什么查询只认「精确命中」
//!
//! `heinrich_core::lookup` 在该码长没有候选时会回退到前缀展开（对 `v` 这种
//! 一级简码是必要的兜底）。但那是给**候选窗**用的：M1.3 没有候选窗，一旦把
//! 前缀候选的首位拿来上屏，`abc` 这种「打错了」的码会突然蹦出一个无关的字。
//! 所以这里显式只在 `LookupMode::Exact` 时返回候选，其余一律当「无命中」，
//! 由 DLL 原样上屏（D27）。

use std::path::Path;
use std::time::{Duration, Instant};

use heinrich_core::scheme::Scheme;
use heinrich_core::{lookup, LookupMode, MemoryStore, RankingConfig, Tier, Usage, UsageStore};
use heinrich_proto::{Entry, Frame, Mode, MAX_ENTRIES, PROTO_VERSION};

use crate::store::SqliteStore;

/// 两次 `wal_checkpoint` 之间的最小间隔（§5.5）。
const CHECKPOINT_EVERY: Duration = Duration::from_secs(60);

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
        Self::open(dir, None)
    }

    pub fn open(dir: &Path, db: Option<&Path>) -> Result<Self, String> {
        let scheme =
            Scheme::load(dir).map_err(|e| format!("装载方案 {} 失败：{e}", dir.display()))?;
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
            store,
            cfg: RankingConfig::default(),
            mode: Mode::Chinese,
            last_checkpoint: Instant::now(),
            recorded: 0,
        })
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
                })
            }
            Frame::Query { code, limit } => {
                let entries = self.query(&code, limit);
                Some(Frame::Result { code, entries })
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

    /// 精确查询。`limit` 上限见 [`MAX_ENTRIES`]。
    pub fn query(&self, code: &str, limit: u8) -> Vec<Entry> {
        let limit = (limit as usize).clamp(1, MAX_ENTRIES);
        let now = unix_now();
        let (list, mode) = lookup(&self.scheme, code, &self.store, now, &self.cfg);
        // 只认精确码长（D45）：前缀兜底一律丢弃。**这不是漏写的分支**——
        // `xxv` 没有精确词条但 `xxvk` 有「比如」，若在此回退，用户正准备按第 4 键时
        // 会弹出一屏「幽灵 / 幽婉」（它们只是码前缀撞了，与「比」毫无语义关系）。
        // 五笔是定长编码，「还没打完」不等于「空码」。想查某个前缀下有哪些词，
        // 走 `z` 学习键（M3）。core 仍保留 `LookupMode::PrefixFallback` 供 CLI 与单测用。
        if mode != LookupMode::Exact {
            return Vec::new();
        }
        list.iter()
            .take(limit)
            .map(|c| Entry {
                word: c.word.clone(),
                label: c.kind_label.to_string(),
            })
            .collect()
    }
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
            } => {
                assert_eq!(proto, PROTO_VERSION);
                assert_eq!(scheme_id, "mini");
                assert_eq!(entry_count, 8);
                assert_eq!(mode, Mode::Chinese);
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
            Frame::Result { code, entries } => {
                assert_eq!(code, "vbg");
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].word, "好");
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
        h.query(code, 9).into_iter().map(|e| e.word).collect()
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
            let mut h = Host::open(&fixture(), Some(&db)).expect("应能打开");
            for _ in 0..20 {
                h.handle(Frame::Record {
                    code: "vbgd".into(),
                    word: "蒿".into(),
                    at_unix: now(),
                });
            }
            h.shutdown();
        }

        let mut h2 = Host::open(&fixture(), Some(&db)).expect("应能重开");
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
        let mut h = Host::open(&fixture(), Some(&dir)).expect("降级后仍应能启动");
        assert!(words(&mut h, "vbg").contains(&"好".to_string()));
        h.handle(Frame::Record {
            code: "vbg".into(),
            word: "好".into(),
            at_unix: 1,
        });
        assert_eq!(h.recorded(), 1, "降级后仍照常接收回报，只是不落盘");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
