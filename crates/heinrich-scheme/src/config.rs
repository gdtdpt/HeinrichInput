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

// ---------------------------------------------------------------- 写回

/// 定向改写配置文件的若干键，其余内容（注释、顺序、不认识的键）原样保留。
///
/// `edits` 里 `Some(值)` = 写成这个值，`None` = 删掉这一行。
///
/// **值必须是已经格式化好的 TOML 标量**（字符串要自己带引号：`"\"off\""`）。
/// 由调用方决定类型，这里不猜——把 `0.35` 写成字符串在 TOML 里是两回事。
///
/// ## 为什么不是「读出来改完整体重写」
///
/// 这个文件是给人手写的。整体重写会丢掉注释、打乱顺序、把用户自己加的段挤到
/// 一起；改一次「码提示」就把人家整份注释抹掉，等于逼人以后只敢用界面改。
///
/// ## 为什么保留原来的写法（点号键 / 段头两种）
///
/// `hint.mode = "off"` 与 `[hint]` + `mode = "off"` 在这个解析器里等价。改写时
/// 只在**原地**换掉值，不搬家——搬家的后果是同一个键在两处出现，人就不知道
/// 哪行算了。
pub fn edit_file(path: &Path, edits: &[(&str, Option<&str>)]) -> std::io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    // 删掉最后那条「因文件以换行结尾而多出来的空行」：不删的话每次改写都会多一行空行
    if text.ends_with('\n') && lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    for (key, value) in edits {
        edit_one(&mut lines, key, *value);
    }
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    write_atomic(path, &out)
}

/// 一行 `k = v` 的定位信息（只用来找位置，不做语义校验）。
struct Entry {
    idx: usize,
    section: String,
    local: String,
    full: String,
}

fn collect(lines: &[String]) -> (Vec<Entry>, Vec<(String, usize)>) {
    let mut section = String::new();
    let mut entries = Vec::new();
    let mut headers: Vec<(String, usize)> = Vec::new();
    for (idx, raw) in lines.iter().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            if let Some(rest) = rest.strip_suffix(']') {
                section = rest.trim_start_matches('[').trim().to_string();
                headers.push((section.clone(), idx));
            }
            continue;
        }
        let Some((k, _)) = line.split_once('=') else {
            continue;
        };
        let local = k.trim().to_string();
        if local.is_empty() {
            continue;
        }
        let full = if section.is_empty() {
            local.clone()
        } else {
            format!("{section}.{local}")
        };
        entries.push(Entry {
            idx,
            section: section.clone(),
            local,
            full,
        });
    }
    (entries, headers)
}

fn edit_one(lines: &mut Vec<String>, full: &str, value: Option<&str>) {
    let (section, local) = match full.rsplit_once('.') {
        Some((s, l)) => (s.to_string(), l.to_string()),
        None => (String::new(), full.to_string()),
    };
    let (entries, headers) = collect(lines);
    let hits: Vec<usize> = entries
        .iter()
        .filter(|e| e.full == full)
        .map(|e| e.idx)
        .collect();

    if !hits.is_empty() {
        // 重复写过的键只留第一条（解析器「以后写的为准」，不改的话用户会看到
        // 「我明明在界面上改了，文件里还留着旧值」）
        let first = hits[0];
        let mut out = Vec::with_capacity(lines.len());
        for (idx, line) in lines.iter().enumerate() {
            if !hits.contains(&idx) {
                out.push(line.clone());
                continue;
            }
            if idx != first {
                continue;
            }
            if let Some(v) = value {
                let entry = entries.iter().find(|e| e.idx == idx).expect("命中的行");
                let indent = &line[..line.len() - line.trim_start().len()];
                out.push(format!("{indent}{} = {v}", entry.local));
            }
        }
        *lines = out;
        return;
    }

    // 本来就没有这一行：删它等于什么都不做（正是「恢复默认」想要的）
    let Some(v) = value else {
        return;
    };

    if let Some((_, hidx)) = headers.iter().rev().find(|(name, _)| *name == section) {
        // 段已存在：接在该段最后一个键的后面；空段就紧跟段头
        let last = entries
            .iter()
            .rev()
            .find(|e| e.section == section && e.idx > *hidx)
            .map(|e| e.idx);
        let at = last.map(|i| i + 1).unwrap_or(hidx + 1);
        lines.insert(at, format!("{local} = {v}"));
        return;
    }
    if section.is_empty() {
        // 顶层键：放在第一个段头之前——写在文件末尾会落进最后一段，键名就变了
        let at = headers.first().map(|(_, i)| *i).unwrap_or(lines.len());
        lines.insert(at, format!("{local} = {v}"));
        return;
    }
    if lines.last().is_some_and(|l| !l.trim().is_empty()) {
        lines.push(String::new());
    }
    lines.push(format!("[{section}]"));
    lines.push(format!("{local} = {v}"));
}

/// 临时文件 + 改名：server 每秒 stat 一次这个文件，不能让它读到半个写好的。
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
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

    // ------------------------------------------------------------ 写回

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("heinrich-cfg-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    /// 改写**只动目标那几行**：注释、顺序、别人加的键都得原样在。
    /// 这条是「界面改一次就把手写注释冲掉」的回归测试。
    #[test]
    fn 改写保留注释与顺序() {
        let path = tmp("keep-comments");
        std::fs::write(
            &path,
            "# 我自己的笔记\n\
             config_version = 1\n\
             \n\
             # 提示相关\n\
             hint.mode = \"mixed\"   # 行尾注释\n\
             mixed.prefer = \"pinyin\"\n",
        )
        .unwrap();

        edit_file(&path, &[("hint.mode", Some("\"off\""))]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# 我自己的笔记"), "整行注释被冲掉了：{text}");
        assert!(
            text.contains("mixed.prefer = \"pinyin\""),
            "别的键被删了：{text}"
        );
        assert!(text.contains("hint.mode = \"off\""), "目标行没改：{text}");
        // 行尾注释跟着旧值一起走：只换值会留下「off   # 先按 mixed」这种自相矛盾的行
        assert!(
            !text.contains("行尾注释"),
            "旧值后面的行尾注释应当一并换掉：{text}"
        );
        assert_eq!(Config::parse(&text).hint_mode(), HintMode::Off);
    }

    /// 段内写的键不能搬到顶层去——搬了以后 `[hint]` 段里那份还在，等于同一键两处，
    /// 人分不清哪行算。
    #[test]
    fn 段内写法原地改() {
        let path = tmp("in-section");
        std::fs::write(
            &path,
            "[hint]\nmode = \"mixed\"\n\n[ranking]\nhalf_life_days = 45\n",
        )
        .unwrap();
        edit_file(&path, &[("hint.mode", Some("\"first_time\""))]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[hint]\nmode = \"first_time\""), "{text}");
        assert!(!text.contains("hint.mode ="), "不该另起一行点号键：{text}");
        assert_eq!(Config::parse(&text).hint_mode(), HintMode::FirstTime);
    }

    /// 文件里根本没有这个键：段存在就补在段里，段不存在就在末尾新建一段。
    #[test]
    fn 缺失时补在正确的位置() {
        let path = tmp("missing");
        std::fs::write(&path, "[ranking]\nhalf_life_days = 30\n").unwrap();
        edit_file(
            &path,
            &[
                ("ranking.learning_gain", Some("0.5")),
                ("hint.mode", Some("\"off\"")),
            ],
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let cfg = Config::parse(&text);
        assert_eq!(cfg.ranking_gain(), Some(0.5), "{text}");
        assert_eq!(
            cfg.ranking_half_life_days(),
            Some(30.0),
            "原有值被弄坏了：{text}"
        );
        assert_eq!(cfg.hint_mode(), HintMode::Off, "{text}");
        // 段头不能重复（重复表头是非法的 TOML），每个键只出现一次
        assert_eq!(text.matches("[ranking]").count(), 1, "{text}");
        assert_eq!(text.matches("learning_gain").count(), 1, "{text}");
        assert_eq!(text.matches("half_life_days").count(), 1, "{text}");
    }

    /// 「恢复默认」= 删行，不是写当前默认值。写死数值会让将来调默认值时老用户不跟。
    #[test]
    fn 删除即为恢复默认() {
        let path = tmp("restore");
        std::fs::write(
            &path,
            "hint.mode = \"off\"\n# 保留我\n[ranking]\nlearning_gain = 0.9\nhalf_life_days = 90\n",
        )
        .unwrap();
        edit_file(
            &path,
            &[
                ("hint.mode", None),
                ("ranking.learning_gain", None),
                ("ranking.half_life_days", None),
            ],
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("hint.mode"), "{text}");
        assert!(!text.contains("learning_gain"), "{text}");
        assert!(!text.contains("half_life_days"), "{text}");
        assert!(text.contains("# 保留我"), "删键不该顺手删掉注释：{text}");
        let cfg = Config::parse(&text);
        assert_eq!(cfg.hint_mode(), HintMode::Mixed);
        assert_eq!(cfg.ranking_gain(), None, "删干净了才叫「没写」");
    }

    /// 同一个键被手写过两遍：改写后只留一条，否则界面上改了、文件里还留着旧值。
    #[test]
    fn 重复键收敛成一条() {
        let path = tmp("dup");
        std::fs::write(&path, "hint.mode = \"off\"\nhint.mode = \"mixed\"\n").unwrap();
        edit_file(&path, &[("hint.mode", Some("\"first_time\""))]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("hint.mode").count(), 1, "{text}");
        assert_eq!(Config::parse(&text).hint_mode(), HintMode::FirstTime);
    }

    /// 空文件（从没用过配置）也要能写：不能因为「没什么可改的」就失败。
    #[test]
    fn 空文件也能写() {
        let path = tmp("empty");
        edit_file(&path, &[("hint.mode", Some("\"off\""))]).unwrap();
        assert_eq!(
            Config::parse(&std::fs::read_to_string(&path).unwrap()).hint_mode(),
            HintMode::Off
        );
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "不该留下临时文件"
        );
    }
}
