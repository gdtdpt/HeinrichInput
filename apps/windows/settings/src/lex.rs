//! 词库定位与装载。
//!
//! 设置窗口需要**推导**（拼音反查、码按规则推、权重取中位数），这是它唯一
//! 依赖词库的地方。装载是几百毫秒的一次性成本，可以接受；但装不上不能让窗口
//! 打不开——降级成「只显示文件里写了什么」，比弹个错误框然后退出有用得多。

use std::path::{Path, PathBuf};

use heinrich_scheme::{PinyinLexicon, Scheme};

/// 装载好的一对词库。
pub struct Lexicons {
    pub scheme: Scheme,
    pub pinyin: Option<PinyinLexicon>,
}

impl Lexicons {
    /// 按 `--data` 给的目录装载五笔，同级 `pinyin/` 存在就一起装。
    pub fn load(data: &Path) -> Result<Self, String> {
        let scheme =
            Scheme::load(data).map_err(|e| format!("装载五笔方案 {} 失败：{e}", data.display()))?;
        let pinyin = data
            .parent()
            .map(|p| p.join("pinyin"))
            .filter(|p| p.is_dir())
            .and_then(|p| match PinyinLexicon::load(&p) {
                Ok(mut lex) => {
                    // 与 server / CLI 同一约定：拼音候选的 chip 要吃五笔码这一列。
                    // 设置窗口只用来显示推导结果，但保持同一条路径能少一类「两边不一样」。
                    lex.attach_wubi_codes(&scheme);
                    Some(lex)
                }
                Err(_) => None,
            });
        Ok(Self { scheme, pinyin })
    }

    /// 这个词在候选里出现过吗（五笔词库 ∪ 拼音词库，与 CLI `emoji check` 同一判据）。
    ///
    /// emoji 页的「绑到」用它拦死条目：词不在词库里就永远不会有候选挂着它，
    /// 写进用户层只是一行死数据。**单字照常通过**（「好」在词库里）——单字口子
    /// 是 D85 明确放行的，这里拦的是「压根打不出来的词」。
    pub fn has_word(&self, word: &str) -> bool {
        self.scheme.entries.iter().any(|e| e.word == word)
            || self.pinyin.as_ref().is_some_and(|l| l.has_word(word))
    }

    /// 这个五笔码在码表里查得到（长度与字母都已由 `parse_target` 校验过）。
    pub fn has_code(&self, code: &str) -> bool {
        match heinrich_scheme::Code::parse(code) {
            Some(c) => !self.scheme.exact_indices(&c).is_empty(),
            None => false,
        }
    }
}

/// 找 `data/wubi86`。
///
/// 三个候选位置，按「越靠近使用者越优先」排：
/// 1. `--data` 显式指定（测试与自测用）；
/// 2. 可执行文件旁边的 `data/wubi86`（dist 的布局：`heinrich-settings.exe` 与 `data/` 并排）；
/// 3. 往上三级的 `data/wubi86`（开发时跑 `target/debug/heinrich-settings.exe`）。
pub fn find_data_dir(explicit: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(d) = explicit {
        return Some(d);
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.to_path_buf());
            // target/debug/x.exe → ../../../data
            if let Some(up) = dir.ancestors().nth(3) {
                roots.push(up.to_path_buf());
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    for root in roots {
        let candidate = root.join("data").join("wubi86");
        if candidate.join("scheme.toml").is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 默认用户词库：`%APPDATA%\Heinrich\words.tsv`（与 server 的 `--words` 默认值一致）。
pub fn default_words_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("Heinrich").join("words.tsv"))
}
