//! 通用页的数据层：`config.toml` 三项的读、校验、写。
//!
//! 页面本身只负责把这三个值摆出来、把用户的输入交出去。所有「该怎么写文件」
//! 的判断都在这里，因为那是唯一能自测的部分。
//!
//! ## 写盘用**定向改写**，不是整份重写
//!
//! `config.toml` 是给人手写的（见 `heinrich-scheme::config` 的模块注释）。整份
//! 重写会把注释、顺序、我们不认识的键一起抹掉——改一次「码提示」就把人家整份
//! 注释冲掉，以后就不敢用界面了。所以只动这三个键所在的行。
//!
//! ## 「恢复默认」= 删掉这三行
//!
//! 不是写回当前默认数值：写死数值等于把一个会变的默认值钉在用户文件里，将来
//! 我们调默认值时，老用户永远停在旧值上，而且文件里看不出来「这是默认」。

use std::path::{Path, PathBuf};

use heinrich_scheme::{Config, HintMode, CONFIG_VERSION};

/// 学习增益 g 的下限/上限，取自 `RankingConfig::sanitized` 的实际区间。
pub const GAIN_RANGE: (f64, f64) = (0.0, 10.0);
/// 半衰期（天）的下限/上限，同上。
pub const HALF_RANGE: (f64, f64) = (7.0, 365.0);

const GAIN_DEFAULT: f64 = 0.35;
const HALF_DEFAULT: f64 = 45.0;

/// 三项当前的取值。
#[derive(Clone, Debug, PartialEq)]
pub struct General {
    pub hint: HintMode,
    pub gain: f64,
    pub half_life: f64,
    /// `config_version` 的只读文本（有值显示数字，没有就说清楚按 v1 处理）。
    pub version: String,
    /// 读配置时攒下的提醒（坏行、越界被夹），原样显示给用户。
    pub warnings: Vec<String>,
    /// 磁盘上有没有这个文件。
    pub found: bool,
}

impl General {
    pub fn load(path: &Path) -> Self {
        let cfg = Config::load(path);
        let mut warnings = cfg.warnings().to_vec();

        let gain = clamp(
            cfg.ranking_gain().unwrap_or(GAIN_DEFAULT),
            GAIN_RANGE,
            "ranking.learning_gain",
            &mut warnings,
        );
        let half_life = clamp(
            cfg.ranking_half_life_days().unwrap_or(HALF_DEFAULT),
            HALF_RANGE,
            "ranking.half_life_days",
            &mut warnings,
        );
        // 展示文案按「人话」写，不照抄键名：这行下面还有一句说明，写成
        // `config_version = 1` 会和真正的配置项混在一起，看着像文件内容漏到了界面上
        let version = match cfg.get("config_version") {
            Some(v) => format!("配置版本 {v}"),
            None => format!("配置版本未写入（按 v{CONFIG_VERSION} 处理）"),
        };
        Self {
            hint: cfg.hint_mode(),
            gain,
            half_life,
            version,
            warnings,
            found: cfg.found(),
        }
    }
}

/// 越界的值在界面上按边界显示，并说清「文件里写的是别的数」——
/// 静默改数字比报错更让人困惑（明明写了 500，界面显示 365）。
fn clamp(v: f64, (lo, hi): (f64, f64), key: &str, warnings: &mut Vec<String>) -> f64 {
    if v < lo || v > hi {
        let fixed = v.clamp(lo, hi);
        warnings.push(format!(
            "{key} = {v} 超出 {lo}–{hi}，按 {fixed} 处理；点「应用」会写回这个值"
        ));
        fixed
    } else {
        v
    }
}

/// 解析一个数字输入框。`Err` 是给人看的一句话，直接显示在说明行里。
pub fn parse_number(raw: &str, (lo, hi): (f64, f64), unit: &str) -> Result<f64, String> {
    let text = raw.trim().replace(['。', '，'], ".");
    if text.is_empty() {
        return Err("不能为空".to_string());
    }
    let v: f64 = text
        .parse()
        .map_err(|_| format!("「{raw}」不是数字，改成 {lo}–{hi} 之间的数"))?;
    if !v.is_finite() {
        return Err("不能是无穷大".to_string());
    }
    if v < lo || v > hi {
        return Err(format!("要在 {lo}–{hi}{unit} 之间（现在是 {v}）"));
    }
    Ok(v)
}

/// 写入三项。只动这三行，其余原样保留。
pub fn save(path: &Path, hint: HintMode, gain: f64, half_life: f64) -> std::io::Result<()> {
    // 字符串在 TOML 里要带引号，数字不能带——类型由调用方定，这里只负责格式
    let hint_v = format!("\"{}\"", hint.as_str());
    let gain_v = fmt_number(gain);
    let half_v = fmt_number(half_life);
    heinrich_scheme::edit_config_file(
        path,
        &[
            ("hint.mode", Some(hint_v.as_str())),
            ("ranking.learning_gain", Some(gain_v.as_str())),
            ("ranking.half_life_days", Some(half_v.as_str())),
        ],
    )
}

/// 「恢复默认」= 把三行一起删掉（删完文件可能只剩注释，那是正常的）。
pub fn restore_defaults(path: &Path) -> std::io::Result<()> {
    heinrich_scheme::edit_config_file(
        path,
        &[
            ("hint.mode", None),
            ("ranking.learning_gain", None),
            ("ranking.half_life_days", None),
        ],
    )
}

/// `0.35` 不该写成 `0.3500`，`45` 也不该写成 `45.0000`。
pub fn fmt_number(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// 默认配置位置：`%APPDATA%\Heinrich\config.toml`（与 server 的 `--config` 默认一致）。
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("Heinrich").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 数字格式不带尾零() {
        assert_eq!(fmt_number(0.35), "0.35");
        assert_eq!(fmt_number(45.0), "45");
        assert_eq!(fmt_number(7.5), "7.5");
        assert_eq!(fmt_number(0.0), "0");
    }

    #[test]
    fn 输入校验给出人话() {
        assert!(parse_number("0.5", GAIN_RANGE, "").is_ok());
        assert_eq!(parse_number("  ", GAIN_RANGE, "").unwrap_err(), "不能为空");
        assert!(parse_number("大", GAIN_RANGE, "")
            .unwrap_err()
            .contains("不是数字"));
        assert!(parse_number("99", GAIN_RANGE, "")
            .unwrap_err()
            .contains("0–10"));
        // 中文标点误输入是很常见的，顺手兜住
        assert_eq!(parse_number("0。35", GAIN_RANGE, "").unwrap(), 0.35);
    }

    /// 写 → 读 一轮：值的类型要正确（字符串带引号、数字不带），
    /// 这样 server 那边的 `Config::load` 才能读回同样的东西。
    #[test]
    fn 写回的值能被读回() {
        let dir = std::env::temp_dir().join(format!("heinrich-general-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "# 手写注释\nconfig_version = 1\n").unwrap();

        save(&path, HintMode::FirstTime, 0.8, 30.0).unwrap();
        let cfg = Config::load(&path);
        assert_eq!(cfg.hint_mode(), HintMode::FirstTime);
        assert_eq!(cfg.ranking_gain(), Some(0.8));
        assert_eq!(cfg.ranking_half_life_days(), Some(30.0));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# 手写注释"), "注释被冲掉了：{text}");

        // 再改一次：不能因为「行已存在」而多出一行
        save(&path, HintMode::Mixed, 0.35, 45.0).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("learning_gain").count(), 1, "{text}");

        restore_defaults(&path).unwrap();
        let cfg = Config::load(&path);
        assert_eq!(cfg.ranking_gain(), None, "删行才算恢复默认");
        assert_eq!(cfg.hint_mode(), HintMode::Mixed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 文件不存在时页面也要能显示默认值，而不是报错。
    #[test]
    fn 缺文件时显示默认() {
        let g = General::load(Path::new("这个文件一定不存在-heinrich"));
        assert!(!g.found);
        assert_eq!(g.gain, GAIN_DEFAULT);
        assert_eq!(g.half_life, HALF_DEFAULT);
        assert_eq!(g.hint, HintMode::Mixed);
        assert!(g.version.contains("未写入"));
    }

    /// 第一次用：目录都没有，点「应用」也要能建出来（不能报「保存失败」）。
    #[test]
    fn 目录不存在时也能写() {
        let dir = std::env::temp_dir().join(format!("heinrich-general-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("Heinrich").join("config.toml");
        save(&path, HintMode::Off, 0.5, 60.0).unwrap();
        let cfg = Config::load(&path);
        assert_eq!(cfg.hint_mode(), HintMode::Off);
        assert_eq!(cfg.ranking_half_life_days(), Some(60.0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
