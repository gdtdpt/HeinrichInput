//! M1.1 的 20 条硬编码小码表 —— **临时件，M1.3 接入 server 后整个文件删除**。
//!
//! ## 为什么要有它
//!
//! 这一里程碑只解决「字上得去」，还没有 server、没有真码表。但「vbg → 好」这条
//! 链路必须端到端跑通一次，否则 TSF 侧的上屏代码到底对不对根本无从验证。
//!
//! ## 为什么放在 TSF crate 里，而不放 `heinrich-core`
//!
//! D8 规定注入型 DLL 只允许依赖 `windows` + `heinrich-tip-id`。为了 20 条假数据
//! 去破这条规矩不划算——那会让每个宿主进程都多背一份 `heinrich-core` 和它的码表装载逻辑。
//!
//! ## 条目是怎么选的
//!
//! 全部取自 `data/wubi86/lexicon.tsv` 的**真实编码**，不手编假码。否则「能出字」
//! 只证明了查表逻辑对，证明不了码表侧对。覆盖面刻意覆盖四种码长：
//!
//! | 类型 | 条目 |
//! |---|---|
//! | 一级简码（1 码，键位映射） | `v` `w` `q` `r` `t` `k` |
//! | 二级简码（2 码） | `vb` `bg` `bm` `aa` |
//! | 三级全码（3 码） | `vbg` |
//! | 四码词组 | `wqvb` `trwu` `wftc` `khlg` `wwna` `ddpe` `jfuj` `aawt` `vbgf` |
//!
//! 注意 `vbg` 与 `vb` 都指向「好」（全码与二级简码并存），这是五笔的正常形态，
//! 也顺带验证了 D16 的精确码长匹配：缓冲是 `vbg` 时按 `vbg` 查，不会被 `vb` 抢走。

/// 精确匹配编码，返回第一个命中项。
///
/// D16：**只做精确码长匹配**，不做前缀展开。`v` 不该查出 `vbgf`，否则每敲一个键
/// 候选就翻几倍（M0a 实测：单 `v` 前缀展开有 3,459 条）。
pub fn lookup(code: &str) -> Option<&'static str> {
    Some(match code {
        // 一级简码
        "v" => "发",
        "w" => "人",
        "q" => "我",
        "r" => "的",
        "t" => "和",
        "k" => "中",
        // 二级简码
        "vb" => "好",
        "bg" => "卫",
        "bm" => "出",
        "aa" => "式",
        // 三级全码
        "vbg" => "好",
        // 四码词组
        "wqvb" => "你好",
        "trwu" => "我们",
        "wftc" => "什么",
        "khlg" => "中国",
        "wwna" => "人民",
        "ddpe" => "大家",
        "jfuj" => "时间",
        "aawt" => "工作",
        "vbgf" => "好玩",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 表里每一条都必须查得回来，且没有一个编码长于 4 码。
    #[test]
    fn every_entry_is_reachable_and_within_four_codes() {
        for code in [
            "v", "w", "q", "r", "t", "k", "vb", "bg", "bm", "aa", "vbg", "wqvb", "trwu", "wftc",
            "khlg", "wwna", "ddpe", "jfuj", "aawt", "vbgf",
        ] {
            assert!(lookup(code).is_some(), "{code} 查不到");
            assert!(
                code.len() <= crate::keys::MAX_CODE_LEN,
                "{code} 超过 {} 码",
                crate::keys::MAX_CODE_LEN
            );
            assert!(
                code.bytes().all(|b| b.is_ascii_lowercase() && b <= b'y'),
                "{code} 含五笔字根键之外的字符"
            );
        }
    }

    /// 里程碑的验收项就是这条。
    #[test]
    fn vbg_is_hao() {
        assert_eq!(lookup("vbg"), Some("好"));
    }

    /// 精确码长匹配：无效码不能"差不多"命中。
    #[test]
    fn no_prefix_or_fuzzy_match() {
        assert_eq!(lookup("vbgx"), None);
        assert_eq!(lookup("vbgg"), None);
        assert_eq!(lookup("zzz"), None);
        assert_eq!(lookup(""), None);
    }
}
