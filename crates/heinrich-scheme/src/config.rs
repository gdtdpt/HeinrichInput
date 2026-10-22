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

        Self {
            map,
            hint_mode,
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
}
