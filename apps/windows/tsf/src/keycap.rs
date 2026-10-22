//! 码位键位图：把一条候选的**五笔码**展开成键帽（PROJECT.md §6.7 / D50）。
//!
//! ## 码从哪来
//!
//! 三条来源，优先级从高到低：
//!
//! 1. 候选自己的 `code`（`Frame::Result::Entry::code`）。拼音段查出来的候选带这个
//!    ——用户打 `hao` 选「好」，键位图要显示 `v b g`，不是 `h a o`。
//! 2. 当前编码缓冲。五笔段的候选不带 `code`（server 只在拼音段给码提示，§6.3），
//!    这时用户打的就是五笔码本身，直接展开它。
//! 3. 都没有 → 不显示。宁可不弹，也不要弹一排按不到的字根。
//!
//! ## 为什么是「贴候选窗」而不是「贴光标」
//!
//! 键位图回答的是「候选窗里这几条为什么长这样」，它和候选窗是一组信息。贴光标会
//! 挤在候选窗与正文之间，正好压住用户正在看的那一行；贴候选窗下沿则是往下加一层，
//! 视线不用回跳。
//!
//! ## 失败一律静默
//!
//! 与候选窗同一条原则：键位图是锦上添花。建窗失败、布局失败、DPI 读不到——都只
//! 记日志，用户按 `Ctrl+Shift+N` 的代价最多是「什么也没发生」。

use heinrich_proto::ProtoKeyRoots;
use heinrich_ui::{place_keycaps, Rect};
use heinrich_win_ui::{work_area_at, CapData, KeycapWindow, Keycaps, Theme, KEYCAP_FOOTNOTE};

use crate::ipc;

/// 键位图与候选窗之间的空隙（逻辑像素，按 DPI 缩放）。
const CAND_GAP: f32 = 2.0;

/// 把一串码逐位展开成键帽数据。纯函数，可单测。
///
/// 任何一位找不到（`z`、非字母、表里没这个键）就整体返回 `None`：
/// 半张键位图比没有更容易让人误读成「这个码就只有这几位」。
pub fn expand(code: &str, roots: &[ProtoKeyRoots]) -> Option<Vec<CapData>> {
    if code.is_empty() {
        return None;
    }
    let mut caps = Vec::with_capacity(code.chars().count());
    for ch in code.chars() {
        let k = roots.iter().find(|r| r.key == ch)?;
        caps.push(CapData {
            key: k.key,
            roots: k.roots.chars().collect(),
            // `main_len` 是**字符数**：主字根之后的变体在键帽上淡一档。
            main_root_count: (k.main_len as usize).min(k.roots.chars().count()),
        });
    }
    Some(caps)
}

/// 键位图窗口的持有者。一个进程一份，懒建、反复显示。
pub struct KeycapPanel {
    seq: u32,
    window: Option<KeycapWindow>,
    /// 建窗失败过就不再试（与候选窗同一条降级策略）。
    failed: bool,
    /// 当前显示的码。同码重复按 `Ctrl+Shift+N` 不重画——重画会闪一下。
    shown: Option<String>,
}

impl KeycapPanel {
    pub fn new(seq: u32) -> Self {
        Self {
            seq,
            window: None,
            failed: false,
            shown: None,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.window.as_ref().is_some_and(|w| w.is_visible())
    }

    /// 显示 `code` 的键位图。
    ///
    /// `anchor` 是**候选窗**矩形，`prefer_above` 表示候选窗当时翻到了光标上方
    /// （键位图跟着往上走，否则会压在正文上）。
    pub fn show(&mut self, code: &str, anchor: Rect, prefer_above: bool) {
        if self.shown.as_deref() == Some(code) && self.is_visible() {
            return;
        }
        let Some(roots) = ipc::key_roots() else {
            tsf_debug!("#{} 还没收到字根表，键位图不弹", self.seq);
            return;
        };
        let Some(caps) = expand(code, &roots) else {
            tsf_debug!(
                "#{} 码 {code:?} 无法逐位展开（含 z 或非字母），不弹",
                self.seq
            );
            return;
        };
        if !self.ensure_window() {
            return;
        }
        let Some(win) = self.window.as_mut() else {
            return;
        };

        let at = (anchor.x as i32, anchor.bottom() as i32);
        let prepared = match win.prepare(&Keycaps {
            caps: &caps,
            footnote: KEYCAP_FOOTNOTE,
            anchor: at,
        }) {
            Ok(p) => p,
            Err(e) => {
                tsf_error!("#{} 键位图布局失败：{e}", self.seq);
                return;
            }
        };

        let gap = CAND_GAP * prepared.dpi_scale;
        let placed = place_keycaps(
            anchor,
            prepared.layout.size,
            work_area_at(at),
            gap,
            prefer_above,
        );
        match win.show(&prepared, (placed.rect.x as i32, placed.rect.y as i32)) {
            Ok(()) => {
                self.shown = Some(code.to_string());
                tsf_debug!(
                    "#{} 键位图 {code} {}×{} @ ({}, {}){}",
                    self.seq,
                    prepared.layout.size.w,
                    prepared.layout.size.h,
                    placed.rect.x as i32,
                    placed.rect.y as i32,
                    if placed.flipped { "（上方）" } else { "" }
                );
            }
            Err(e) => tsf_error!("#{} 键位图显示失败：{e}", self.seq),
        }
    }

    pub fn hide(&mut self) {
        if let Some(w) = self.window.as_mut() {
            w.hide();
        }
        self.shown = None;
    }

    fn ensure_window(&mut self) -> bool {
        if self.window.is_some() {
            return true;
        }
        if self.failed {
            return false;
        }
        match KeycapWindow::new(Theme::default()) {
            Ok(w) => {
                tsf_debug!("#{} 键位图窗口已创建", self.seq);
                self.window = Some(w);
                true
            }
            Err(e) => {
                self.failed = true;
                tsf_error!("#{} 键位图窗口创建失败：{e}（此后不再尝试）", self.seq);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Vec<ProtoKeyRoots> {
        vec![
            ProtoKeyRoots {
                key: 'v',
                primary: '发',
                roots: "女刀九臼山彐曰".into(),
                main_len: 6,
            },
            ProtoKeyRoots {
                key: 'b',
                primary: '了',
                roots: "子耳了也凵阝".into(),
                main_len: 4,
            },
            ProtoKeyRoots {
                key: 'g',
                primary: '一',
                roots: "王一五戋龶丰".into(),
                main_len: 4,
            },
        ]
    }

    #[test]
    fn 逐位展开成键帽() {
        let caps = expand("vbg", &roots()).unwrap();
        assert_eq!(caps.len(), 3);
        assert_eq!(caps[0].key, 'v');
        assert_eq!(
            caps[0].roots,
            vec!['女', '刀', '九', '臼', '山', '彐', '曰']
        );
        // 主字根 6 枚，第 7 枚（曰）以外都算主字根？——`main_len = 6` 表示前 6 枚
        assert_eq!(caps[0].main_root_count, 6);
        // 变体在后面，界内界外分得开
        assert_eq!(caps[2].main_root_count, 4);
        assert_eq!(caps[2].roots.len(), 6);
    }

    #[test]
    fn 有_z_或非字母就整体不弹() {
        assert!(expand("vbz", &roots()).is_none());
        assert!(expand("v1", &roots()).is_none());
        assert!(expand("", &roots()).is_none());
    }

    #[test]
    fn main_len_越界时夹到字根总数() {
        let mut r = roots();
        r[0].main_len = 99;
        let caps = expand("v", &r).unwrap();
        assert_eq!(caps[0].main_root_count, 7);
    }
}
