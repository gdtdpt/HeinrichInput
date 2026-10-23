//! 词组页的数据模型：读写 `words.tsv` + 逐条推导要显示的列。
//!
//! 刻意不依赖任何 Win32：`--selftest` 直接跑这一层，窗口只负责把
//! [`Row`] 画进 ListView、把编辑框的内容写回 [`Model`]。

use std::io;
use std::path::PathBuf;

use heinrich_scheme::user::{self, UserWord};

use crate::lex::Lexicons;

/// 一行要显示的东西：文件里写的原文 + 推导出来的三列。
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub raw: UserWord,
    /// 实际生效的拼音（手填优先，否则反查/逐字拼）
    pub pinyin: Option<String>,
    /// 实际生效的码（手填优先，否则按 86 规则推）
    pub code: Option<String>,
    pub weight: f64,
    /// 这条词的毛病（推不出码 / 查不到拼音…），显示在状态栏
    pub note: Option<String>,
}

impl Row {
    /// 这一列是推出来的还是用户写的（`*` 后缀，与 CLI 的 `word list` 同一约定）。
    pub fn pinyin_cell(&self) -> String {
        match (&self.pinyin, self.raw.pinyin.is_some()) {
            (Some(p), true) => p.clone(),
            (Some(p), false) => format!("{p}*"),
            (None, _) => "—".into(),
        }
    }

    pub fn code_cell(&self) -> String {
        match (&self.code, self.raw.code.is_some()) {
            (Some(c), true) => c.clone(),
            (Some(c), false) => format!("{c}*"),
            (None, _) => "—".into(),
        }
    }

    pub fn weight_cell(&self) -> String {
        match self.raw.weight {
            Some(w) => format!("{w:.0}"),
            None => format!("{:.0}*", self.weight),
        }
    }
}

/// 词库文件 + 里面的词条。
pub struct Model {
    pub path: PathBuf,
    pub words: Vec<UserWord>,
    /// 读文件时的坏行警告（只在状态栏提示，不影响其它词）
    pub warnings: Vec<String>,
}

impl Model {
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let file = user::load(&path)?;
        Ok(Self {
            path,
            words: file.words,
            warnings: file.warnings,
        })
    }

    /// 逐条推导。词库没装成（`None`）时只回显文件里写了什么——这是降级路径，
    /// 不是错误路径：至少用户还能看见、还能删。
    pub fn rows(&self, lex: Option<&Lexicons>) -> Vec<Row> {
        self.words
            .iter()
            .map(|w| {
                let Some(lex) = lex else {
                    return Row {
                        raw: w.clone(),
                        pinyin: w.pinyin.clone(),
                        code: w.code.clone(),
                        weight: w.weight.unwrap_or(0.0),
                        note: None,
                    };
                };
                let (resolved, warns) =
                    user::resolve(std::slice::from_ref(w), &lex.scheme, lex.pinyin.as_ref());
                match resolved.into_iter().next() {
                    Some(r) => Row {
                        raw: w.clone(),
                        pinyin: r.pinyin,
                        code: r.code,
                        weight: r.weight,
                        note: warns.first().cloned(),
                    },
                    // 推不出来、也没有手填：这条词现在打不出来，但仍然要显示出来，
                    // 否则用户会以为自己加的词被吞了
                    None => Row {
                        raw: w.clone(),
                        pinyin: w.pinyin.clone(),
                        code: w.code.clone(),
                        weight: w.weight.unwrap_or(0.0),
                        note: warns.first().cloned(),
                    },
                }
            })
            .collect()
    }

    /// 落盘。`user::save` 已经是「临时文件 + 改名」，server 那每秒一次的 stat
    /// 不会读到半个文件。
    pub fn save(&self) -> io::Result<()> {
        user::save(&self.path, &self.words)
    }

    /// 新增或按词覆盖，返回下标与新加与否。
    pub fn upsert(&mut self, w: UserWord) -> (usize, bool) {
        let is_new = user::upsert(&mut self.words, w.clone());
        let idx = self
            .words
            .iter()
            .position(|x| x.word == w.word)
            .unwrap_or(0);
        (idx, is_new)
    }

    /// 删除，返回原位置与被删的那条（给「撤销删除」用）。
    pub fn remove(&mut self, word: &str) -> Option<(usize, UserWord)> {
        let idx = self.words.iter().position(|w| w.word == word)?;
        Some((idx, self.words.remove(idx)))
    }

    pub fn insert_at(&mut self, idx: usize, w: UserWord) {
        let idx = idx.min(self.words.len());
        self.words.insert(idx, w);
    }
}
