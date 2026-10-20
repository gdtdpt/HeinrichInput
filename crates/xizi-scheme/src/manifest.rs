//! `scheme.toml` 的极简读取。
//!
//! 方案清单只由生成器写出、格式固定（一层 `key = value`），为其引入完整 TOML
//! 解析器不划算：解析成本、依赖与供应链风险都远大于收益。因此只支持标量与
//! 基础标点，遇到嵌套表、数组等语法直接报错，避免「静默忽略」埋坑。

use std::collections::BTreeMap;
use std::fmt;

/// 清单解析结果：键值均为字符串，数字按需转换。
#[derive(Debug, Default, Clone)]
pub struct Manifest {
    map: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    pub line: usize,
    pub reason: String,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "清单第 {} 行：{}", self.line, self.reason)
    }
}

impl std::error::Error for ManifestError {}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let mut map = BTreeMap::new();
        for (idx, raw) in text.lines().enumerate() {
            let line_no = idx + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                return Err(ManifestError {
                    line: line_no,
                    reason: "不支持表头（清单只允许一层键值）".into(),
                });
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(ManifestError {
                    line: line_no,
                    reason: "缺少 '='".into(),
                });
            };
            let key = key.trim();
            if key.is_empty() {
                return Err(ManifestError {
                    line: line_no,
                    reason: "键为空".into(),
                });
            }
            let value = value.trim();
            let value = strip_quotes(value).ok_or_else(|| ManifestError {
                line: line_no,
                reason: "引号未闭合".into(),
            })?;
            if map.insert(key.to_string(), value.to_string()).is_some() {
                return Err(ManifestError {
                    line: line_no,
                    reason: format!("键 `{key}` 重复"),
                });
            }
        }
        Ok(Self { map })
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    pub fn require(&self, key: &str) -> Result<&str, ManifestError> {
        self.get(key).ok_or_else(|| ManifestError {
            line: 0,
            reason: format!("缺少必需键 `{key}`"),
        })
    }

    pub fn require_usize(&self, key: &str) -> Result<usize, ManifestError> {
        let raw = self.require(key)?;
        raw.parse().map_err(|_| ManifestError {
            line: 0,
            reason: format!("`{key}` 需要非负整数，实际为 `{raw}`"),
        })
    }
}

fn strip_quotes(v: &str) -> Option<&str> {
    if let Some(inner) = v.strip_prefix('"') {
        return inner.strip_suffix('"');
    }
    if let Some(inner) = v.strip_prefix('\'') {
        return inner.strip_suffix('\'');
    }
    let ok = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
    ok.then_some(v)
}

#[cfg(test)]
mod tests {
    use super::Manifest;

    #[test]
    fn parses_flat_scalars() {
        let m = Manifest::parse(
            r#"
# 注释
id = "wubi86"
max_code_len = 4
name = '五笔 86'
"#,
        )
        .unwrap();
        assert_eq!(m.require("id").unwrap(), "wubi86");
        assert_eq!(m.require_usize("max_code_len").unwrap(), 4);
        assert_eq!(m.require("name").unwrap(), "五笔 86");
        assert!(m.get("absent").is_none());
    }

    #[test]
    fn rejects_tables_and_duplicates() {
        assert!(Manifest::parse("[meta]\nid = 1").is_err());
        assert!(Manifest::parse("id = 1\nid = 2").is_err());
        assert!(Manifest::parse("id = \"unclosed").is_err());
        assert!(Manifest::parse("no_equals_here").is_err());
    }
}
