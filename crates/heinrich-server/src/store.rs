//! 学习库：`usage` / `user_words` 的 SQLite 实现（§5.5）。
//!
//! ## 为什么放在 server、而不是 `heinrich-core`
//!
//! `heinrich-core` 的定位是「纯逻辑、无 OS 依赖、**无文件 I/O**」——它的 `UsageStore`
//! 是一张接口，`MemoryStore` 是给单测和 CLI 用的实现。真正的落盘放在 server：
//!
//! 1. server 是**进程内唯一**的写者（D1）。DLL 被注入到几十个宿主进程里，谁都能
//!    写同一份库的话，SQLite 的锁会变成宿主的卡顿来源。
//! 2. 写库这件事可以失败、可以慢，但不能拖住按键路径——它在 server 线程里，而
//!    按键路径只往上丢一帧就返回（D18）。
//!
//! ## 为什么不是「追加写文件」
//!
//! 历史教训：多写者的追加日志在崩溃点附近会互相覆盖，而且没有事务边界——
//! 一次上屏要么整条记上，要么完全没记，不能记半条。SQLite 的 WAL 给的就是这个。
//!
//! ## 这里唯一一个反直觉的写法
//!
//! `last_used_at` 取 `MAX(旧值, 新值)` 而不是直接覆盖：server 被强杀时 DLL 会把
//! 上屏记录积压在进程里（见 `tsf/ipc.rs` 的 `PENDING_RECORDS`），重连后成批补发；
//! 多个宿主进程各有一份队列，补发的顺序不保证全局单调。直接覆盖会让一条「晚到的
//! 旧记录」把时间往回拨，学习分的衰减就跟着算反。

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use heinrich_core::{Tier, Usage, UsageStore};

/// 当前 schema 版本。改动表结构时递增，并在 [`SqliteStore::open`] 里决定怎么迁移。
const SCHEMA_VERSION: i64 = 1;

/// 学习库。**同一时刻只该有一个实例**（server 进程内）。
pub struct SqliteStore {
    conn: Connection,
    path: PathBuf,
}

impl SqliteStore {
    /// 打开（必要时创建）一个学习库。
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("创建学习库目录 {} 失败：{e}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| format!("打开学习库 {} 失败：{e}", path.display()))?;
        Self::prepare(conn, path.to_path_buf())
    }

    /// 内存库。单测用，不落盘。
    #[cfg(test)]
    pub fn open_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("打开内存库失败：{e}"))?;
        Self::prepare(conn, PathBuf::from(":memory:"))
    }

    fn prepare(conn: Connection, path: PathBuf) -> Result<Self, String> {
        // WAL：写不阻塞读，崩溃后靠 wal 文件恢复到最后一次提交。`synchronous=NORMAL`
        // 是 WAL 下的推荐档——每次提交仍落盘，只是不做额外的 fsync，掉电最多丢最后
        // 一两次学习记录，而输入法**绝不能**为了学习数据去牺牲按键延迟。
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| format!("设置 WAL 失败：{e}"))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| format!("设置 synchronous 失败：{e}"))?;
        // server 是唯一写者，但用户可能同时开着 sqlite3 命令行在看数据。
        conn.busy_timeout(Duration::from_secs(2))
            .map_err(|e| format!("设置 busy_timeout 失败：{e}"))?;

        let me = Self { conn, path };
        me.init_schema()?;
        Ok(me)
    }

    fn init_schema(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS usage (
                    key          TEXT PRIMARY KEY,
                    n            INTEGER NOT NULL DEFAULT 0,
                    last_used_at INTEGER NOT NULL DEFAULT 0,
                    tier         INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS user_words (
                    code       TEXT NOT NULL,
                    word       TEXT NOT NULL,
                    weight     REAL NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (code, word)
                );
                CREATE TABLE IF NOT EXISTS meta (
                    k TEXT PRIMARY KEY,
                    v TEXT NOT NULL
                );
                "#,
            )
            .map_err(|e| format!("建表失败：{e}"))?;

        let version: Option<String> = self
            .conn
            .query_row("SELECT v FROM meta WHERE k = 'schema'", [], |r| r.get(0))
            .optional()
            .map_err(|e| format!("读 schema 版本失败：{e}"))?;
        match version {
            None => {
                self.conn
                    .execute(
                        "INSERT INTO meta (k, v) VALUES ('schema', ?1)",
                        params![SCHEMA_VERSION.to_string()],
                    )
                    .map_err(|e| format!("写 schema 版本失败：{e}"))?;
            }
            Some(v) => {
                let found: i64 = v.parse().unwrap_or(0);
                if found > SCHEMA_VERSION {
                    // 新版本写过的库，老 server 看不懂。宁可拒绝启动学习库
                    // （上层会降级成内存实现），也不要把用户的记录写坏。
                    return Err(format!(
                        "学习库 schema v{found} 比本程序支持的 v{SCHEMA_VERSION} 新，\
                         可能是降级安装，已跳过学习记录"
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 已记录的词数（自测与日志用）。
    pub fn known_words(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM usage WHERE n > 0", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0) as usize
    }

    /// 把 WAL 合并回主库。空闲时与退出时各做一次（§5.5）。
    pub fn checkpoint(&self) {
        if let Err(e) = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
            srv_warn!("checkpoint 失败：{e}");
        }
    }
}

/// `tier` 列 → 枚举。**显式映射**，不依赖枚举的声明顺序——哪天在 `Tier` 中间插一档，
/// 落库的数字不能跟着漂。
///
/// 数字 1 历史上是 `Tier::Pinned`（D95 已删除该档），保留其含义不再复用；
/// 反向映射里 1 兜底读作 `Learning`，历史数据（若有）无感迁移。
fn tier_to_int(tier: Tier) -> i64 {
    match tier {
        Tier::Learning => 0,
        Tier::Suppressed => 2,
    }
}

fn int_to_tier(v: i64) -> Tier {
    match v {
        2 => Tier::Suppressed,
        _ => Tier::Learning,
    }
}

impl UsageStore for SqliteStore {
    fn usage(&self, word: &str) -> Usage {
        let got = self
            .conn
            .query_row(
                "SELECT n, last_used_at FROM usage WHERE key = ?1",
                params![word],
                |r| {
                    Ok(Usage {
                        n: r.get::<_, i64>(0)? as u32,
                        last_used: r.get::<_, i64>(1)?,
                    })
                },
            )
            .optional();
        match got {
            Ok(Some(u)) => u,
            Ok(None) => Usage::default(),
            Err(e) => {
                // 读失败按「没用过」处理：宁可按码表序给候选，也不要因为学习库
                // 坏了让整次查询失败。
                srv_warn!("读学习记录失败（{word}）：{e}");
                Usage::default()
            }
        }
    }

    fn tier(&self, word: &str) -> Tier {
        let got = self
            .conn
            .query_row(
                "SELECT tier FROM usage WHERE key = ?1",
                params![word],
                |r| r.get::<_, i64>(0),
            )
            .optional();
        match got {
            Ok(Some(v)) => int_to_tier(v),
            Ok(None) => Tier::default(),
            Err(e) => {
                srv_warn!("读档位失败（{word}）：{e}");
                Tier::default()
            }
        }
    }

    fn record(&mut self, word: &str, at_unix: i64) {
        // 一次上屏 = 一个事务（§5.5）。自增与时间戳必须在同一事务里，
        // 否则崩溃可能在两者之间留下「次数加了、时间没更新」的半条记录。
        let result = self.conn.transaction().and_then(|tx| {
            tx.execute(
                "INSERT INTO usage (key, n, last_used_at, tier) VALUES (?1, 1, ?2, 0)
                 ON CONFLICT(key) DO UPDATE SET
                     n = n + 1,
                     last_used_at = MAX(last_used_at, excluded.last_used_at)",
                params![word, at_unix],
            )?;
            tx.commit()
        });
        if let Err(e) = result {
            srv_error!("记录上屏失败（{word}）：{e}");
        }
    }

    fn set_tier(&mut self, word: &str, tier: Tier) {
        let v = tier_to_int(tier);
        if let Err(e) = self.conn.execute(
            "INSERT INTO usage (key, n, last_used_at, tier) VALUES (?1, 0, 0, ?2)
             ON CONFLICT(key) DO UPDATE SET tier = excluded.tier",
            params![word, v],
        ) {
            srv_error!("设置档位失败（{word}）：{e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_accumulates_in_one_row() {
        let mut s = SqliteStore::open_memory().expect("内存库应能打开");
        assert_eq!(s.usage("好"), Usage::default());
        s.record("好", 100);
        s.record("好", 250);
        assert_eq!(
            s.usage("好"),
            Usage {
                n: 2,
                last_used: 250
            }
        );
        assert_eq!(s.known_words(), 1);
    }

    /// 补发的旧记录不能把「上次使用时间」往回拨——那会让衰减算反。
    #[test]
    fn late_record_does_not_rewind_last_used() {
        let mut s = SqliteStore::open_memory().expect("内存库应能打开");
        s.record("好", 1000);
        s.record("好", 500); // 晚到的旧记录
        let u = s.usage("好");
        assert_eq!(u.n, 2, "次数照加");
        assert_eq!(u.last_used, 1000, "时间取最大值，不回拨");
    }

    #[test]
    fn tier_survives_without_usage() {
        let mut s = SqliteStore::open_memory().expect("内存库应能打开");
        s.set_tier("号", Tier::Suppressed);
        assert_eq!(s.tier("号"), Tier::Suppressed);
        assert_eq!(s.usage("号").n, 0);
        s.record("号", 10);
        assert_eq!(s.tier("号"), Tier::Suppressed, "上屏不改档位");
    }

    /// 重开同一个文件必须看得见之前记的东西（这是「落库」的定义）。
    #[test]
    fn data_survives_reopen() {
        let dir = std::env::temp_dir().join(format!("heinrich-store-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("learn.db");
        {
            let mut s = SqliteStore::open(&path).expect("应能建库");
            s.record("好", 42);
            s.checkpoint();
        }
        {
            let s = SqliteStore::open(&path).expect("应能重开");
            assert_eq!(s.usage("好").n, 1);
            assert_eq!(s.usage("好").last_used, 42);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// schema 比程序新时必须拒绝，而不是硬写进去把库弄坏。
    #[test]
    fn newer_schema_is_refused() {
        let dir =
            std::env::temp_dir().join(format!("heinrich-store-schema-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("learn.db");
        {
            let s = SqliteStore::open(&path).expect("应能建库");
            s.conn
                .execute("UPDATE meta SET v = '99' WHERE k = 'schema'", [])
                .expect("改版本应成功");
        }
        assert!(SqliteStore::open(&path).is_err(), "更新的 schema 应被拒绝");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
