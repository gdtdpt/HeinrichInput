//! 用户配置 `%APPDATA%\Heinrich\config.toml` 的**宽容**读取。
//!
//! ## 为什么不能复用 `manifest.rs` 的解析器
//!
//! `Manifest` 是给**机器写**的文件用的（方案清单由 `heinrich-gen` 生成）：语法只要
//! 有半点不合规就报错，因为它错了一定是生成器的 bug。用户配置正好相反——**它由
//! 人手改**，而且手改时最容易出的错是拼错键名、写错值、把值放在引号外。
//!
//! 所以这里的取舍是：**坏行不抛错，记一条 warning 跳过**。一个键写错不该让输入法
//! 起不来，那比「改了没生效」严重得多。缺键一律取默认，未知键忽略（§7）。
//!
//! ## 支持的语法（刻意只到这一层）
//!
//! ```toml
//! # 整行注释
//! config_version = 1
//! hint.mode = "mixed"      # 行尾注释也可以，引号内的 # 不算注释
//!
//! [hint]                   # 段头 = 后续键的前缀
//! mode = "off"             # 等价于 hint.mode
//! ```
//!
//! 不支持数组、内联表、多行串——目前没有任何选项需要它们。真需要时应当换一个
//! 真正的 TOML 实现，而不是把这里逐条补成半个 TOML。

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::Path;

/// 当前配置格式版本。比它新 ⇒ 记 warning 但照常读（缺的键取默认）。
pub const CONFIG_VERSION: u32 = 1;

/// 码提示的显示时机（§6.3）。
///
/// 语义是「拼音段候选旁边要不要显示它的五笔码」，与 `keys` / UI 无关——
/// 裁决在 server 侧完成，DLL 只画非空的码（见 `heinrich-proto` 的 `Entry::code`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HintMode {
    /// 拼音段的候选总是显示（默认）。
    #[default]
    Mixed,
    /// 只在这个词**还没上过屏**时显示一次。
    FirstTime,
    /// 一律不显示。
    Off,
}

impl HintMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            HintMode::Mixed => "mixed",
            HintMode::FirstTime => "first_time",
            HintMode::Off => "off",
        }
    }

    /// 解析配置值。大小写不敏感、允许 `-` 与 `_` 互通（`first-time` / `first_time`）。
    pub fn parse(raw: &str) -> Option<Self> {
        let norm = raw.trim().to_ascii_lowercase().replace('-', "_");
        match norm.as_str() {
            "mixed" => Some(HintMode::Mixed),
            "first_time" => Some(HintMode::FirstTime),
            "off" | "none" | "false" => Some(HintMode::Off),
            _ => None,
        }
    }
}

/// 解析出来的配置。**永远构造成功**——坏行只进 `warnings`。
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// 扁平化的键 → 值（`[hint] mode` 存成 `hint.mode`）。
    map: BTreeMap<String, String>,
    hint_mode: HintMode,
    /// `ranking.learning_gain`，缺省或非法为 `None`（由调用方取默认）。
    ranking_gain: Option<f64>,
    /// `ranking.half_life_days`，同上。
    ranking_half_life: Option<f64>,
    /// 磁盘上是否真有这个文件。`false` 表示全用默认值。
    found: bool,
    warnings: Vec<String>,
}

impl Config {
    /// 全默认（没有配置文件时用）。
    pub fn defaults() -> Self {
        Self::default()
    }

    /// 读文件。不存在 ⇒ 全默认且不记 warning（第一次装完本来就没有）。
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut cfg = Self::parse(&text);
                cfg.found = true;
                cfg
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Self::defaults(),
            Err(e) => {
                let mut cfg = Self::defaults();
                cfg.warnings.push(format!(
                    "读不了配置文件 {}：{e}；本次使用默认值",
                    path.display()
                ));
                cfg
            }
        }
    }

    pub fn parse(text: &str) -> Self {
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        let mut warnings = Vec::new();
        let mut section = String::new();

        for (idx, raw) in text.lines().enumerate() {
            let line_no = idx + 1;
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') {
                match line.strip_suffix(']') {
                    Some(rest) => section = rest.trim_start_matches('[').trim().to_string(),
                    None => warnings.push(format!("第 {line_no} 行：段头没闭合，已跳过")),
                }
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                warnings.push(format!("第 {line_no} 行：缺少 '='，已跳过"));
                continue;
            };
            let key = key.trim();
            if key.is_empty() {
                warnings.push(format!("第 {line_no} 行：键为空，已跳过"));
                continue;
            }
            let full = if section.is_empty() {
                key.to_string()
            } else {
                format!("{section}.{key}")
            };
            let value = unquote(value.trim());
            if map.insert(full.clone(), value).is_some() {
                warnings.push(format!("第 {line_no} 行：`{full}` 重复，以后写的为准"));
            }
        }

        // `config_version` 只用于提示「这个文件比程序新」，不做拒绝——拒绝会让
        // 用户从新版本回退到旧版本时直接失去配置。
        if let Some(raw) = map.get("config_version") {
            match raw.parse::<u32>() {
                Ok(v) if v > CONFIG_VERSION => warnings.push(format!(
                    "配置版本 {v} 高于本程序的 {CONFIG_VERSION}，不认识的选项会被忽略"
                )),
                Ok(_) => {}
                Err(_) => warnings.push(format!("config_version 不是整数：`{raw}`")),
            }
        }

        let hint_mode = match map.get("hint.mode") {
            None => HintMode::default(),
            Some(raw) => HintMode::parse(raw).unwrap_or_else(|| {
                warnings.push(format!(
                    "hint.mode 的值 `{raw}` 不认识（可用 mixed / first_time / off），按 mixed 处理"
                ));
                HintMode::default()
            }),
        };

        // 排序参数（§5）。这里只管「是不是有限数、够不够下界」，上界交给
        // `RankingConfig::sanitized` 夹——两处都夹一次会分成两套规则。
        let ranking_gain = read_number(&map, "ranking.learning_gain", 0.0, &mut warnings);
        let ranking_half_life = read_number(&map, "ranking.half_life_days", 1.0, &mut warnings);

        // 已退役的排序键。它们都曾经真的生效过，用户照旧文档写了会以为还在起作用，
        // 所以必须逐个说清楚「这行被忽略」——静默吞掉比报错更难查。
        for (key, why) in [
            ("ranking.promote_k", "王座式动态门槛，D54 已废除"),
            (
                "ranking.promote_margin",
                "绝对分差门槛，D58 已废除——「一次上屏不改顺序」改由「第一次上屏不计分」保证",
            ),
        ] {
            if map.contains_key(key) {
                warnings.push(format!("{key} 已废弃（{why}），这行被忽略"));
            }
        }

        Self {
            map,
            hint_mode,
            ranking_gain,
            ranking_half_life,
            found: false,
            warnings,
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    pub fn hint_mode(&self) -> HintMode {
        self.hint_mode
    }

    /// 配置文件里写的学习增益 `g`；没写或写坏为 `None`。
    pub fn ranking_gain(&self) -> Option<f64> {
        self.ranking_gain
    }

    /// 配置文件里写的半衰期（天）；没写或写坏为 `None`。
    pub fn ranking_half_life_days(&self) -> Option<f64> {
        self.ranking_half_life
    }

    pub fn found(&self) -> bool {
        self.found
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

/// 去掉行尾注释：`#` 之后丢弃，但引号内的 `#` 是内容。
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' | '\'' if quote.is_none() => quote = Some(ch),
            c if Some(c) == quote => quote = None,
            '#' if quote.is_none() => return &line[..i],
            _ => {}
        }
    }
    line
}

/// 去掉一层引号。没有引号就原样返回（`hint.mode = off` 这种写法也接受）。
fn unquote(v: &str) -> String {
    if let Some(inner) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return inner.to_string();
    }
    if let Some(inner) = v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return inner.to_string();
    }
    v.to_string()
}

/// 读一个数值键：没写 ⇒ `None`（调用方取默认）；写坏 ⇒ `None` + 一条 warning。
///
/// 写坏时**不能**退回 0 之类的「看起来合理」的值：`learning_gain = 0` 意味着
/// 彻底不学习，用户把一个字写错就再也不会学任何东西，而且完全没有提示。
fn read_number(
    map: &BTreeMap<String, String>,
    key: &str,
    min: f64,
    warnings: &mut Vec<String>,
) -> Option<f64> {
    let raw = map.get(key)?;
    match raw.parse::<f64>() {
        Ok(v) if v.is_finite() && v >= min => Some(v),
        Ok(_) => {
            warnings.push(format!(
                "{key} 的值 `{raw}` 不合理（要 ≥ {min} 的数），已忽略"
            ));
            None
        }
        Err(_) => {
            warnings.push(format!("{key} 不是数字：`{raw}`，已忽略"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 缺文件时全用默认() {
        let cfg = Config::load(Path::new("这个文件一定不存在-heinrich"));
        assert!(!cfg.found());
        assert_eq!(cfg.hint_mode(), HintMode::Mixed);
        assert!(cfg.warnings().is_empty(), "文件不存在不该报 warning");
    }

    #[test]
    fn 点号键与段头两种写法等价() {
        let a = Config::parse("hint.mode = \"off\"");
        let b = Config::parse("[hint]\nmode = \"first_time\"");
        assert_eq!(a.hint_mode(), HintMode::Off);
        assert_eq!(b.hint_mode(), HintMode::FirstTime);
        assert_eq!(b.get("hint.mode"), Some("first_time"));
    }

    #[test]
    fn 未知键被忽略而不报错() {
        let cfg = Config::parse("mixed.prefer = \"pinyin\"\n未来的键 = 42");
        assert!(cfg.warnings().is_empty());
        assert_eq!(cfg.hint_mode(), HintMode::Mixed);
    }

    #[test]
    fn 坏值回落到默认并留警告() {
        let cfg = Config::parse("hint.mode = \"sometimes\"");
        assert_eq!(cfg.hint_mode(), HintMode::Mixed);
        assert_eq!(cfg.warnings().len(), 1);
        assert!(cfg.warnings()[0].contains("sometimes"));
    }

    /// 一个键写坏不该让整个文件失效——其余键仍要生效。
    #[test]
    fn 坏行只跳这一行() {
        let cfg = Config::parse(
            "这不是键值\n\
             hint.mode = \"off\"\n\
             [hint\n",
        );
        assert_eq!(cfg.hint_mode(), HintMode::Off);
        // 两行坏语法各记一条：缺 '=' 的那行，以及段头没闭合的那行。
        assert_eq!(cfg.warnings().len(), 2, "{:?}", cfg.warnings());
    }

    #[test]
    fn 行尾注释与引号内的井号() {
        let cfg = Config::parse("hint.mode = \"off\"   # 关掉提示");
        assert_eq!(cfg.hint_mode(), HintMode::Off);

        let quoted = Config::parse("note = \"a # b\"");
        assert_eq!(quoted.get("note"), Some("a # b"));
    }

    #[test]
    fn 大小写与连字符都接受() {
        assert_eq!(HintMode::parse("First-Time"), Some(HintMode::FirstTime));
        assert_eq!(HintMode::parse("OFF"), Some(HintMode::Off));
        assert_eq!(HintMode::parse("mixed "), Some(HintMode::Mixed));
    }

    #[test]
    fn 未来版本的配置照样读() {
        let cfg = Config::parse("config_version = 9\nhint.mode = \"off\"");
        assert_eq!(cfg.hint_mode(), HintMode::Off, "不认识的版本不该让配置失效");
        assert!(cfg.warnings().iter().any(|w| w.contains("配置版本 9")));
    }

    #[test]
    fn 重复键以后写的为准() {
        let cfg = Config::parse("hint.mode = \"off\"\nhint.mode = \"mixed\"");
        assert_eq!(cfg.hint_mode(), HintMode::Mixed);
        assert_eq!(cfg.warnings().len(), 1);
    }

    #[test]
    fn 排序参数两种写法都认() {
        assert_eq!(
            Config::parse("ranking.learning_gain = 0.5").ranking_gain(),
            Some(0.5)
        );
        assert_eq!(
            Config::parse("[ranking]\nhalf_life_days = 30").ranking_half_life_days(),
            Some(30.0)
        );
        let none = Config::parse("hint.mode = \"off\"");
        assert_eq!(none.ranking_gain(), None);
        assert_eq!(none.ranking_half_life_days(), None, "没写就是取默认");
    }

    /// 参数写坏不该让输入法起不来，也**不该悄悄变成 0**（gain = 0 等于永不学习）。
    #[test]
    fn 排序参数写坏时取默认而不是零() {
        let not_num = Config::parse("ranking.learning_gain = 大一点");
        assert_eq!(not_num.ranking_gain(), None);
        assert!(
            not_num.warnings()[0].contains("不是数字"),
            "{:?}",
            not_num.warnings()
        );

        let negative = Config::parse("ranking.half_life_days = -1");
        assert_eq!(negative.ranking_half_life_days(), None);
        assert!(negative.warnings()[0].contains("不合理"));

        // 0 对 gain 合法（就是关掉学习），对半衰期不合法（除零）。
        assert_eq!(
            Config::parse("ranking.learning_gain = 0").ranking_gain(),
            Some(0.0)
        );
        assert_eq!(
            Config::parse("ranking.half_life_days = 0").ranking_half_life_days(),
            None
        );
    }

    /// 两个已退役的门槛键都必须显式警告：用户按旧文档写了 0.35，不警告就以为生效了。
    #[test]
    fn 退役的门槛键会提示() {
        for key in ["ranking.promote_k", "ranking.promote_margin"] {
            let cfg = Config::parse(&format!("{key} = 2.0"));
            assert!(
                cfg.warnings().iter().any(|w| w.contains("已废弃")),
                "{key} 应当报废弃：{:?}",
                cfg.warnings()
            );
        }
    }
}
