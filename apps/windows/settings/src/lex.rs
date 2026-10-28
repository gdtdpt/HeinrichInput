//! 词库定位与装载。
//!
//! 设置窗口需要**推导**（拼音反查、码按规则推、权重取中位数），这是它唯一
//! 依赖词库的地方。装载是几百毫秒的一次性成本，可以接受；但装不上不能让窗口
//! 打不开——降级成「只显示文件里写了什么」，比弹个错误框然后退出有用得多。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use heinrich_scheme::user;
use heinrich_scheme::{PinyinLexicon, Scheme};

/// 装载好的一对词库（内置）+ 自定义词库里的词与码。
pub struct Lexicons {
    pub scheme: Scheme,
    pub pinyin: Option<PinyinLexicon>,
    /// 自定义词库（`words.tsv`）里的词。**只给 emoji 页的目标校验用**，不并进
    /// `scheme`：词组页的推导列要的是「内置词库怎么看这个词」，掺进用户词会让
    /// 一条词自己命中自己，推出来的码也分不清是推的还是自己写的。
    ///
    /// 为什么必须补这一份（D102）：运行时 server 会把自定义词合并进码表，那些词
    /// **真的会产生候选**，emoji 也就挂得上；而这里只装内置词库，于是
    /// 「先加词 → 再绑 emoji」被误判成「打不出来」，用户只能去手改
    /// `emoji.user.toml`。
    user_words: HashSet<String>,
    /// 同上，自定义词库解析出来的五笔码（按码绑的目标校验要用）。
    user_codes: HashSet<String>,
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
        Ok(Self {
            scheme,
            pinyin,
            user_words: HashSet::new(),
            user_codes: HashSet::new(),
        })
    }

    /// 叠加自定义词库：只记「这些词打得出来、这些码查得到」，不改 `scheme`。
    ///
    /// 解析走 [`user::resolve`]，与 server 合并前用的是同一条路（手填 > 反查 >
    /// 推导），所以这里认的词与运行时真能出候选的词是同一批。文件不存在 / 读不动
    /// 就是「没有自定义词」的正常状态——不报错，校验退回只认内置词库。
    pub fn attach_user_words(&mut self, path: &Path) {
        self.user_words.clear();
        self.user_codes.clear();
        let Ok(file) = user::load(path) else {
            return;
        };
        let (resolved, _) = user::resolve(&file.words, &self.scheme, self.pinyin.as_ref());
        for r in resolved {
            if let Some(code) = r.code {
                self.user_codes.insert(code);
            }
            self.user_words.insert(r.word);
        }
    }

    /// 这个词在候选里出现过吗（内置五笔 ∪ 内置拼音 ∪ 自定义词库）。
    ///
    /// emoji 页的「绑到」用它拦死条目：词不在词库里就永远不会有候选挂着它，
    /// 写进用户层只是一行死数据。**单字照常通过**（「好」在词库里）——单字口子
    /// 是 D85 明确放行的，这里拦的是「压根打不出来的词」。
    pub fn has_word(&self, word: &str) -> bool {
        self.scheme.entries.iter().any(|e| e.word == word)
            || self.pinyin.as_ref().is_some_and(|l| l.has_word(word))
            || self.user_words.contains(word)
    }

    /// 这个五笔码在码表里查得到（长度与字母都已由 `parse_target` 校验过）。
    /// 自定义词的码也算——它在运行时同样会产生候选。
    pub fn has_code(&self, code: &str) -> bool {
        match heinrich_scheme::Code::parse(code) {
            Some(c) => !self.scheme.exact_indices(&c).is_empty() || self.user_codes.contains(code),
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
