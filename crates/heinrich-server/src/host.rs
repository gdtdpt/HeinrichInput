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

use heinrich_core::scheme::Scheme;
use heinrich_core::{lookup, LookupMode, MemoryStore, RankingConfig};
use heinrich_proto::{Entry, Frame, Mode, MAX_ENTRIES, PROTO_VERSION};

pub struct Host {
    scheme: Scheme,
    store: MemoryStore,
    cfg: RankingConfig,
    /// 中英模式。**这是权威副本**：DLL 每个宿主进程各有一份实例，
    /// 模式若存在实例里，A 应用切英文、B 应用仍是中文（M1.1 的既有缺陷）。
    mode: Mode,
}

impl Host {
    /// 从一个方案目录装载（`scheme.toml` + `lexicon.tsv`）。
    ///
    /// 136,239 条约 300 ms，**只该在一个进程里发生一次**——这是 server 存在的
    /// 全部理由：DLL 被注入到每个宿主进程，在那里装载等于每个 tab 卡 300 ms。
    pub fn load(dir: &Path) -> Result<Self, String> {
        let scheme =
            Scheme::load(dir).map_err(|e| format!("装载方案 {} 失败：{e}", dir.display()))?;
        Ok(Self {
            scheme,
            store: MemoryStore::new(),
            cfg: RankingConfig::default(),
            mode: Mode::Chinese,
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

    /// 处理一条请求，返回要回给对方的帧（`None` 表示不回）。
    pub fn handle(&mut self, frame: Frame) -> Option<Frame> {
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
            // Welcome / Result / Mode / Bye 是**服务端发出去的**方向，
            // 收到它们只能说明对端把方向搞反了，不理。
            Frame::Welcome { .. }
            | Frame::Result { .. }
            | Frame::Mode { .. }
            | Frame::Bye { .. } => None,
        }
    }

    /// 精确查询。`limit` 上限见 [`MAX_ENTRIES`]。
    pub fn query(&self, code: &str, limit: u8) -> Vec<Entry> {
        let limit = (limit as usize).clamp(1, MAX_ENTRIES);
        let now = unix_now();
        let (list, mode) = lookup(&self.scheme, code, &self.store, now, &self.cfg);
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
}
