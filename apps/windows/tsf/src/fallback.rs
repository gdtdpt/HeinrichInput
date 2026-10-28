//! 应急简码表（D100）：**server 无应答时**，1–2 码仍能出字。
//!
//! ## 为什么内嵌在 DLL 里
//!
//! 这份表的价值恰恰在「连不上 server」的时刻——靠握手从 server 拿就本末倒置了。
//! 所以构建期由 `heinrich-gen` 从码表生成 `data/wubi86/fallback.tsv`（一级 25 条 +
//! 二级 616 条，每码取权重最高的单字），这里 `include_str!` 进 DLL，进程启动后
//! 首次用到时解析一次（641 行，微秒级），常驻内存 ~10 KB。
//!
//! ## 用在哪里、不用在哪里
//!
//! | 场景 | 行为 |
//! |---|---|
//! | `ipc::query` 返回 `None`（无应答 / 正在重连 / server 挂了） | 查这张表：命中就出字、弹候选窗 |
//! | 查到了但没这个词（`Some(空)`） | **不用**——那是「真无此字」，出简码字反而是错的 |
//! | server 正常 | 不用——候选序要含学习排序与混输，这张表没有 |
//!
//! 边界（都是刻意的，见 PROJECT.md D100）：
//!
//! - 只覆盖五笔 1–2 码单字：应急定位，三码以上与词组、拼音、emoji 一概不保；
//! - 顺序是码表权重序，**不含学习排序**——server 恢复后的下一个键自然接管；
//! - **不在组合中途热替换候选**（D55：同一码连打顺序必须稳定）——连接恢复不
//!   触发重画，`resolve()` 取「当前显示的那条」上屏，与 server 会给的完全一致。

use std::collections::HashMap;
use std::sync::OnceLock;

const RAW: &str = include_str!("../../../../data/wubi86/fallback.tsv");

fn table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut map = HashMap::with_capacity(641);
        for line in RAW.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((code, word)) = line.split_once('\t') else {
                continue;
            };
            let code = code.trim();
            let word = word.trim();
            if code.is_empty() || word.is_empty() {
                continue;
            }
            // 生成器已保证每码一条，entry 保守处理：先到先得。
            map.entry(code).or_insert(word);
        }
        map
    })
}

/// 查应急候选。码长超过 2 直接返回 `None`（三码以上的字应急表不保）。
pub fn lookup(code: &str) -> Option<&'static str> {
    if code.chars().count() > 2 {
        return None;
    }
    table().get(code).copied()
}

/// 表内条数（自测用；正式路径不查总数，表是静态的）。
#[cfg(test)]
pub fn len() -> usize {
    table().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_level_shortcodes_are_covered() {
        // 25 个一级简码全部命中（g → 一、r → 的、v → 发）
        assert_eq!(lookup("g"), Some("一"));
        assert_eq!(lookup("r"), Some("的"));
        assert_eq!(lookup("v"), Some("发"));
        for code in 'a'..='y' {
            assert!(lookup(&code.to_string()).is_some(), "一级简码缺 {code}");
        }
        assert!(lookup("z").is_none(), "z 不是字根键，没有简码");
    }

    #[test]
    fn second_level_and_length_limit() {
        assert!(lookup("aa").is_some(), "aa → 式");
        assert!(lookup("gg").is_some());
        // 三码及以上不保（应急边界）
        assert!(lookup("vbg").is_none());
        assert!(lookup("abcd").is_none());
        assert!(lookup("").is_none());
    }

    #[test]
    fn table_shape() {
        // 25 一级 + 616 二级 = 641；全部是单字
        assert_eq!(len(), 641);
    }
}
