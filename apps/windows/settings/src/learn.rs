//! 学习数据页的数据层：只读 `learn.db`，外加「清除某个词的学习分」。
//!
//! ## 为什么设置程序直接读库，而不是问 server
//!
//! 问 server 要么升协议（emoji 引擎 M4c 正在用 v7，会撞车），要么新开一条只读
//! 命令；而且 server 没在跑时页面就打不开了。库本来就是 WAL + `busy_timeout`，
//! 多一个读者是它设计内的用法。
//!
//! ## 为什么「清除」是 UPDATE 清零，不是 DELETE 整行
//!
//! `usage` 这一行里除了次数和时间，还存着**档位**（`tier`：学习 / 置顶 / 抑制）。
//! 整行删掉等于顺手把用户「置顶」过的词降回普通词——那是另一个语义，混在一起
//! 会让人删一次学习分就把自己固定的词弄丢。所以只把 `n` 与 `last_used_at` 清零，
//! 档位原样留着。
//!
//! 清零后立刻生效：server 每次查询都直接读库（`usage()` 是一条 SQL），不缓存；
//! DLL 侧还有最坏 3 秒的候选缓存，所以位次回落最多等 3 秒。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use heinrich_core::{learning_score, RankingConfig};
use rusqlite::{params, Connection};

/// 一次最多列多少条。用过的词一般几百条，1000 是兜底——再多就该搜索了。
pub const LIMIT: usize = 1000;

/// 表格里的一行。
#[derive(Clone, Debug)]
pub struct Row {
    pub word: String,
    /// 上屏次数（含第一次，第一次不计分——D58）。
    pub n: u32,
    /// 上次上屏的 unix 秒。
    pub last_used: i64,
    /// 0 = 学习，1 = 置顶，2 = 抑制。
    pub tier: i64,
    /// 当前参数下算出来的学习分（越大越往前提）。
    pub score: f64,
}

impl Row {
    pub fn tier_label(&self) -> &'static str {
        match self.tier {
            1 => "置顶",
            2 => "抑制",
            _ => "",
        }
    }

    /// 「多久以前」比绝对时间更能说明「这个分还剩多少」。
    pub fn ago(&self, now: i64) -> String {
        if self.last_used <= 0 {
            return "—".to_string();
        }
        let secs = (now - self.last_used).max(0);
        match secs {
            0..=59 => "刚刚".to_string(),
            60..=3599 => format!("{} 分钟前", secs / 60),
            3600..=86_399 => format!("{} 小时前", secs / 3600),
            _ => format!("{} 天前", secs / 86_400),
        }
    }
}

/// 一次读取的结果。
#[derive(Debug)]
pub struct Report {
    /// 按学习分降序、已截到 [`LIMIT`]。
    pub rows: Vec<Row>,
    /// 库里一共有多少条用过的词（不受搜索与截断影响）。
    pub total: usize,
}

/// 默认学习库：`%LOCALAPPDATA%\Heinrich\learn.db`（与 server 的 `--db` 默认一致）。
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("Heinrich").join("learn.db"))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 当前 unix 秒（界面算「多久以前」用同一把尺子，免得测试与显示各算一次）。
pub fn now_unix() -> i64 {
    now()
}

/// 打开库。**库不存在就报一句人话**，不去创建——「学习记录还没生成」和
/// 「记录是空的」是两件事，前者要告诉用户先打几个字。
fn open(db: &Path) -> Result<Connection, String> {
    if !db.is_file() {
        return Err(format!(
            "还没有学习记录（{}）。切到「亨利五笔」打几个字上屏，再回来看",
            db.display()
        ));
    }
    let conn = Connection::open(db).map_err(|e| format!("打开 {} 失败：{e}", db.display()))?;
    // 与 server 抢锁时最多等 2 秒——它的写都是一条语句的小事务，等不到就是真出事
    conn.busy_timeout(Duration::from_secs(2))
        .map_err(|e| format!("设置 busy_timeout 失败：{e}"))?;
    Ok(conn)
}

/// 读全表，按学习分排序。`needle` 非空时按「词里含这段字」过滤。
pub fn load(db: &Path, cfg: &RankingConfig, needle: &str) -> Result<Report, String> {
    let conn = open(db)?;
    let mut stmt = conn
        .prepare("SELECT key, n, last_used_at, tier FROM usage WHERE n > 0")
        .map_err(|e| format!("读学习库失败：{e}"))?;
    let mapped = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| format!("读学习库失败：{e}"))?;

    let at = now();
    let needle = needle.trim().to_lowercase();
    let mut rows = Vec::new();
    for item in mapped {
        let (word, n, last_used, tier) = item.map_err(|e| format!("读一行失败：{e}"))?;
        if !needle.is_empty() && !word.to_lowercase().contains(&needle) {
            continue;
        }
        let days = if last_used <= 0 {
            f64::INFINITY
        } else {
            (at - last_used).max(0) as f64 / 86_400.0
        };
        rows.push(Row {
            word,
            n: n.max(0) as u32,
            last_used,
            tier,
            score: learning_score(n.max(0) as u32, days, cfg),
        });
    }
    // 分高的在前；同分按次数、再按词，保证每次打开的顺序是同一个
    rows.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.n.cmp(&a.n))
            .then_with(|| a.word.cmp(&b.word))
    });
    let total = rows.len();
    rows.truncate(LIMIT);
    Ok(Report { rows, total })
}

/// 清除一个词的学习分：次数与时间归零，**档位保留**。返回动了几行（0 = 本来就没有）。
pub fn clear(db: &Path, word: &str) -> Result<usize, String> {
    let conn = open(db)?;
    conn.execute(
        "UPDATE usage SET n = 0, last_used_at = 0 WHERE key = ?1",
        params![word],
    )
    .map_err(|e| format!("清除失败（{word}）：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个只有 `usage` 表的小库，和 server 建的那张列名一致。
    fn make_db(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE usage (key TEXT PRIMARY KEY, n INTEGER NOT NULL DEFAULT 0,
                                 last_used_at INTEGER NOT NULL DEFAULT 0,
                                 tier INTEGER NOT NULL DEFAULT 0);",
        )
        .unwrap();
    }

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("heinrich-learn-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("learn.db")
    }

    #[test]
    fn 按学习分排序且档位看得见() {
        let db = tmp("sort");
        make_db(&db);
        let conn = Connection::open(&db).unwrap();
        let now = now();
        conn.execute(
            "INSERT INTO usage VALUES ('好', 5, ?1, 0), ('的', 2, ?1, 1), ('嗯', 1, ?1, 2)",
            params![now],
        )
        .unwrap();

        let r = load(&db, &RankingConfig::default(), "").unwrap();
        assert_eq!(r.total, 3);
        // n=1 是 0 分（第一次上屏不计分），n=5 最高
        assert_eq!(r.rows[0].word, "好");
        assert_eq!(
            r.rows[0].score,
            learning_score(5, 0.0, &RankingConfig::default())
        );
        let pinned = r.rows.iter().find(|x| x.word == "的").unwrap();
        assert_eq!(pinned.tier_label(), "置顶");
        assert!(pinned.score > 0.0);
        let once = r.rows.iter().find(|x| x.word == "嗯").unwrap();
        assert_eq!(once.score, 0.0, "只上屏一次就是 0 分（D58）");
    }

    #[test]
    fn 搜索按词过滤() {
        let db = tmp("search");
        make_db(&db);
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO usage VALUES ('好吃', 3, ?1, 0), ('微信', 2, ?1, 0)",
            params![now()],
        )
        .unwrap();
        let r = load(&db, &RankingConfig::default(), "好").unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.rows[0].word, "好吃");
    }

    /// 清零必须留住 `tier`：把置顶词删成普通词是不可接受的副作用。
    #[test]
    fn 清除只清学习分不动档位() {
        let db = tmp("clear");
        make_db(&db);
        let conn = Connection::open(&db).unwrap();
        conn.execute("INSERT INTO usage VALUES ('的', 9, ?1, 1)", params![now()])
            .unwrap();
        assert_eq!(clear(&db, "的").unwrap(), 1);

        let r = load(&db, &RankingConfig::default(), "").unwrap();
        assert_eq!(r.total, 0, "清零后 n=0，不该再出现在列表里");
        let (n, tier): (i64, i64) = conn
            .query_row("SELECT n, tier FROM usage WHERE key = '的'", [], |x| {
                Ok((x.get(0)?, x.get(1)?))
            })
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(tier, 1, "档位必须保留");
    }

    #[test]
    fn 库不存在时给人话而不是建库() {
        let db = tmp("absent");
        let msg = load(&db, &RankingConfig::default(), "").unwrap_err();
        assert!(msg.contains("还没有学习记录"), "{msg}");
        assert!(!db.exists(), "只读操作不该顺手建一个空库");
    }
}
