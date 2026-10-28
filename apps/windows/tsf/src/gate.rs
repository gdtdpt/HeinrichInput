//! 候选窗位置的闸门（D105）。
//!
//! ## 它要治的是什么
//!
//! D104 的复量只解决了「多量一次」，没解决「量到的值本身是错的」。真机实测过一次：
//! 输入框被清空后立刻打字，宿主（Chromium）连续 9 秒、5 段编码里给出的都是
//! **15 分钟前**那一处的坐标（`1636`，而真实光标在 `898`），复量两次数值完全一致。
//! 三条来源（选区 / 组合串 / GUI 插入符）也都指着同一个错值——因为源头是同一份
//! 缓存。于是「多量一次」不但没救回来，反而把错值确认了一遍。
//!
//! 结论：**位置的可信度不能只靠「量得晚」，得靠「和已知的历史对得上」**。
//!
//! ## 三条规矩
//!
//! | 规矩 | 做法 | 治什么 |
//! |---|---|---|
//! | 持久锚 | 只有通过闸门的值才写入 [`Gate::anchor`] | 错值进不来，因为锚一直是最后一处**已知正确**的位置 |
//! | 可达性 | 新值必须从锚「一个词的字宽内」到达（同行 `|Δx| ≤` [`JUMP_X`]），或者是换行 / 换编辑区 | 738px 那种回放旧坐标 |
//! | 逃生口 | 连续 [`STREAK_MAX`] 次给出**同一个**被拒值 → 承认它是真的 | 用户真的用我们看不见的方式移动了光标（Vim 式键位、程序跳转） |
//!
//! ## 为什么拒绝时不换位置，而要「慢跟随」
//!
//! 拒绝意味着「这个值不可信」，但**不意味着用户没在打字**。窗口原地不动的话，
//! 连打十个字候选窗就冻在起点了。所以拒绝时按 [`Gate::advance`] 往前推——
//! 推的是「自上次可信位置以来上屏的字宽之和」，一个汉字一个 `em`，ASCII 半个。
//! 误差是每键几像素，方向一定对，不会比「不动」更差。
//!
//! 推进只在**拒绝期间**累积：一旦有值通过闸门，`advance` 归零。所以正常打字时
//! 这条逻辑一个像素都不参与。
//!
//! ## 为什么闸门必须能「放行」
//!
//! 键盘上的移动键（←→↑↓ / Home / End / PgUp / PgDn / Esc / Ctrl+A）我们看得见，
//! 按下就把 [`Gate::mark_free`] 置位，下一次测量无条件采纳——那是用户明确告知
//! 「我把光标挪了」。鼠标点同一框内的别处我们看不见，用 `GetCursorPos` 兜底
//! （见 `Core::caret_is_clicked`）。两条加起来，剩下的盲区只有「程序自己移动光标」，
//! 而那正是逃生口管的事。

use heinrich_ui::Rect;

/// 同行横跳的容忍上限（逻辑像素）。
///
/// 五笔一个词最多五码，光标只会往右挪几个字宽；同行跳两百像素必然是量错了地方。
/// 与 D104 的 `SUSPECT_JUMP_X` 同一个数量级，但这一条**决定采纳与否**，
/// 不再是「要不要多量一次」。
const JUMP_X: f32 = 200.0;

/// 「换了行」的纵向界限。纵向一动就是换行或换框，横向怎么跳都合理。
const ROW_Y: f32 = 40.0;

/// 首键仲裁的差距门槛（以行高为单位）。写组合串前后两次测量差到两行高以上，
/// 就不是排版抖动，是其中一个值错了。
const PREWARM_GAP_EM: f32 = 2.0;

/// 连续多少次给出同一个被拒值，就承认它。见模块文档「逃生口」。
const STREAK_MAX: u32 = 3;

/// 一次已采纳的位置，连同它当时的编辑区矩形。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub rect: Rect,
    /// 采纳时的视图矩形。视图变了＝换了输入框 / 换了窗口，闸门直接放行。
    pub view: Option<Rect>,
}

/// 闸门的判定结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decision {
    /// 候选窗该摆在哪儿。
    pub rect: Rect,
    /// 这个值是通过闸门的（`true`），还是拒绝后按锚 + 推进量算出来的（`false`）。
    pub accepted: bool,
    /// 一句话说明依据，进日志。
    pub why: &'static str,
}

/// 位置的闸门。一个宿主进程一份，和 `Core` 同生共死。
#[derive(Debug, Default)]
pub struct Gate {
    anchor: Option<Anchor>,
    /// 拒绝期间上屏的字宽之和（逻辑像素），向右累加。
    advance: f32,
    /// 连续拒绝次数与那个被拒的值。
    streak: u32,
    last_rejected: Option<Rect>,
    /// 下一次测量无条件采纳：移动键刚按下，或鼠标就在量到的位置旁边。
    free: bool,
}

impl Gate {
    /// 光标可能被用户挪走了（移动键 / 鼠标点击）——下一次测量不再过闸门。
    ///
    /// 只置位不立即改位置：位置永远由「一次真实测量」决定，
    /// 这条标志只是把判断权先交出去一次。
    pub fn mark_free(&mut self) {
        self.free = true;
    }

    /// 当前的锚，供日志与测试使用。
    pub fn anchor(&self) -> Option<Anchor> {
        self.anchor
    }

    /// 判一次位。
    ///
    /// `measured` 是这次量到的位置，`view` 是同一时刻的视图矩形，
    /// `prewarm` 是首键时「写组合串之前」量到的那个值（只有首键有）。
    pub fn decide(
        &mut self,
        measured: Rect,
        view: Option<Rect>,
        prewarm: Option<Rect>,
    ) -> Decision {
        self.judge(measured, view, prewarm, true)
    }

    /// 复量用的判位：同一条闸门，但**不计入逃生口的连击**。
    ///
    /// 复量发生在同一个键的下一轮消息里，它给出的值与按键那一刻本来就该一样。
    /// 把它算进「连续三次同一个值」，等于把一次事件记成两次——逃生口会提前一半
    /// 触发，那就等于把闸门关小了。
    pub fn decide_recheck(&mut self, measured: Rect, view: Option<Rect>) -> Decision {
        self.judge(measured, view, None, false)
    }

    /// 判定顺序就是「证据强度」的顺序，改动它之前先读一遍每一步的注释：
    ///
    /// | 顺位 | 证据 | 结论 |
    /// |---|---|---|
    /// | ① | 移动键 / 鼠标刚动过 | 采纳测量值 |
    /// | ② | 视图矩形变了 | 采纳测量值（换了框，旧锚不再适用） |
    /// | ③ | 写组合串前后两次测量差得远 | **采纳写前那个** |
    /// | ④ | 还没有锚 | 采纳测量值 |
    /// | ⑤ | 纵向变了 | 采纳测量值（换行） |
    /// | ⑥ | 同行且在一个词的字宽内 | 采纳测量值（正常跟随） |
    /// | ⑦ | 连续三次同一个被拒值 | 采纳测量值（逃生口） |
    /// | ⑧ | 其余 | 拒绝，位置留在「锚 + 推进量」 |
    ///
    /// ② 排在 ③ 之前的讲究：换框时旧锚本来就作废，写前的值同样可能是上一个框
    /// 的缓存；而同一个框内「写前对、写后错」正是 D105 的现场。两种情形靠
    /// 「视图矩形有没有变」区分，这是唯一分得开它们的证据。
    fn judge(
        &mut self,
        measured: Rect,
        view: Option<Rect>,
        prewarm: Option<Rect>,
        count_streak: bool,
    ) -> Decision {
        // ① 用户明确告知挪了光标：无条件采纳，并从这里重建锚。
        if self.free {
            self.free = false;
            return self.accept(measured, view, "移动键/鼠标后");
        }

        // ② 换了编辑区：换输入框、换窗口、窗口缩放。横向怎么跳都合理。
        //
        //    必须排在首键仲裁之前：换框时**旧锚已经作废**，而写前的那个值同样
        //    可能来自上一个框（刚点进来，宿主还没换缓存）。这一条先放行，
        //    就等于宣布「锚重新开始」，仲裁那一步自然不再有发言权。
        if let Some(anchor) = self.anchor {
            if view.is_some() && anchor.view.is_some() && view != anchor.view {
                return self.accept(measured, view, "换了编辑区");
            }
        }

        // ③ 首键仲裁：写组合串之前量的那个值是在宿主排版干净的时候拿的；
        //    写完之后再量，宿主可能已经在搬旧缓存了。两者差到两行高以上，
        //    信前者——D105 那次 738px 的事件，正是靠这一条挡住的。
        //
        //    为什么这一步直接采纳、不再过闸门：这次的错误形态恰恰是
        //    「错值与旧锚一致、对值与旧锚不一致」（输入框被清空，光标回到行首，
        //    锚还停在清空前那一行末尾）。拿锚去校验会正好反过来。
        if let Some(pre) = prewarm {
            let gap = PREWARM_GAP_EM * measured.h.max(1.0);
            if (pre.x - measured.x).abs() > gap || (pre.y - measured.y).abs() > gap {
                tsf_debug!(
                    "# 首键仲裁：写前 {pre:?} 写后 {measured:?}，采用写前（Δx {:.0}）",
                    (pre.x - measured.x).abs()
                );
                return self.accept(pre, view, "首键仲裁");
            }
        }

        // ④ 还没有锚：第一次测量就是锚。
        let Some(anchor) = self.anchor else {
            return self.accept(measured, view, "首个锚点");
        };

        // ⑤ 换了行：纵向一动，横向没有「可达」可言。
        if (measured.y - anchor.rect.y).abs() > ROW_Y {
            return self.accept(measured, view, "换了行");
        }

        // ⑥ 可达性：同行、且在一个词的字宽内。
        if (measured.x - anchor.rect.x).abs() <= JUMP_X {
            return self.accept(measured, view, "正常跟随");
        }

        // ⑦ 逃生口：连续几次给同一个值，那不像是抖出来的旧缓存，而是真的。
        //    `streak` 只在「被拒的值一模一样」时累加——D105 那次连续 5 个值
        //    互不相同（1330/1432/1440/1448/1490），一路被拒到底，正确。
        if count_streak {
            if self.last_rejected == Some(measured) {
                self.streak += 1;
            } else {
                self.last_rejected = Some(measured);
                self.streak = 1;
            }
            if self.streak >= STREAK_MAX {
                tsf_debug!(
                    "# 位置闸门：同一个值连续被拒 {} 次，承认它是真的 {measured:?}",
                    self.streak
                );
                return self.accept(measured, view, "连续被拒后放行");
            }
        }

        // ⑧ 拒绝：位置保持在「锚 + 已上屏字数估算」处，不跟这个值走。
        Decision {
            rect: Rect::new(
                anchor.rect.x + self.advance,
                anchor.rect.y,
                measured.w,
                measured.h,
            ),
            accepted: false,
            why: "拒绝：同行横跳太远",
        }
    }

    /// 上屏了一段文字。拒绝期间按字宽把锚往前推，让窗口「慢跟随」而不是冻住。
    ///
    /// 正常路径下这个累加量永远不会被用到——下次测量一通过闸门就归零了。
    /// 字宽取锚的行高（`rect.h` 就是插入符高度，与字号同量级）。
    pub fn note_committed(&mut self, text: &str) {
        let Some(anchor) = self.anchor else { return };
        let em = if anchor.rect.h > 0.0 {
            anchor.rect.h
        } else {
            16.0
        };
        for ch in text.chars() {
            // 全角（汉字、全角标点）一个 em，半角半个。emoji 之类按全角算。
            self.advance += if ch.is_ascii() { em * 0.5 } else { em };
        }
    }

    fn accept(&mut self, rect: Rect, view: Option<Rect>, why: &'static str) -> Decision {
        self.anchor = Some(Anchor { rect, view });
        self.advance = 0.0;
        self.streak = 0;
        self.last_rejected = None;
        Decision {
            rect,
            accepted: true,
            why,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一块编辑区，左上角 (898, 1490)，1480×128——真机那次的实际值。
    fn view() -> Option<Rect> {
        Some(Rect::new(898.0, 1490.0, 1480.0, 128.0))
    }

    /// 一个行高 24 的插入符。
    fn caret(x: f32) -> Rect {
        Rect::new(x, 1546.0, 0.0, 24.0)
    }

    #[test]
    fn 第一个值直接成为锚() {
        let mut g = Gate::default();
        let d = g.decide(caret(900.0), view(), None);
        assert!(d.accepted);
        assert_eq!(d.rect.x, 900.0);
        assert_eq!(g.anchor().unwrap().rect.x, 900.0);
    }

    #[test]
    fn 正常跟随不拦() {
        let mut g = Gate::default();
        g.decide(caret(900.0), view(), None);
        let d = g.decide(caret(915.0), view(), None);
        assert!(d.accepted, "{}", d.why);
        assert_eq!(d.rect.x, 915.0);
    }

    /// D105 的现场形态：锚在第一处，宿主给了 738px 开外的旧坐标。
    #[test]
    fn 同行横跳七百像素被拒() {
        let mut g = Gate::default();
        g.decide(caret(898.0), view(), None);
        let d = g.decide(caret(1636.0), view(), None);
        assert!(!d.accepted, "{}", d.why);
        assert_eq!(d.rect.x, 898.0, "拒绝时窗口留在锚上");
    }

    #[test]
    fn 换行放行() {
        let mut g = Gate::default();
        g.decide(caret(898.0), view(), None);
        let d = g.decide(Rect::new(1900.0, 1600.0, 0.0, 24.0), view(), None);
        assert!(d.accepted, "{}", d.why);
    }

    #[test]
    fn 换了编辑区放行() {
        let mut g = Gate::default();
        g.decide(caret(898.0), view(), None);
        let other = Some(Rect::new(100.0, 300.0, 500.0, 40.0));
        let d = g.decide(Rect::new(150.0, 320.0, 0.0, 24.0), other, None);
        assert!(d.accepted, "{}", d.why);
    }

    #[test]
    fn 移动键后无条件放行() {
        let mut g = Gate::default();
        g.decide(caret(898.0), view(), None);
        g.mark_free();
        let d = g.decide(caret(1636.0), view(), None);
        assert!(d.accepted, "{}", d.why);
        // 只放行一次：紧接着的横跳又该被拦。
        let d = g.decide(caret(898.0), view(), None);
        assert!(!d.accepted, "{}", d.why);
    }

    /// 首键仲裁：写组合串之前量到 898，写完量到 1636（宿主搬了旧缓存）。
    /// 差 738px 远超两行高，要用写前那个。
    #[test]
    fn 首键仲裁用写前的值() {
        let mut g = Gate::default();
        let d = g.decide(caret(1636.0), view(), Some(caret(898.0)));
        assert!(d.accepted, "{}", d.why);
        assert_eq!(d.rect.x, 898.0);
    }

    /// 即使锚正好等于那个错值，首键仲裁也要盖过它——输入框被清空、光标回到行首，
    /// 锚却还停在清空前的行尾，这是 D105 的真实形态。
    #[test]
    fn 首键仲裁盖过与锚一致的错值() {
        let mut g = Gate::default();
        g.decide(caret(1636.0), view(), None); // 白天长句打到这里，锚是 1636
        let d = g.decide(caret(1636.0), view(), Some(caret(898.0)));
        assert!(d.accepted, "{}", d.why);
        assert_eq!(d.rect.x, 898.0);
        assert_eq!(g.anchor().unwrap().rect.x, 898.0);
    }

    #[test]
    fn 首键两次测量接近时不仲裁() {
        let mut g = Gate::default();
        let d = g.decide(caret(902.0), view(), Some(caret(898.0)));
        assert!(d.accepted);
        assert_eq!(d.rect.x, 902.0, "差值在容差内，用写后的新鲜值");
    }

    /// 换框时旧锚作废，写前的值也可能来自上一个框——顺序上 ③ 必须压过 ④。
    #[test]
    fn 换了编辑区时不采信写前的旧值() {
        let mut g = Gate::default();
        g.decide(caret(898.0), view(), None);
        let other = Some(Rect::new(100.0, 300.0, 500.0, 40.0));
        let d = g.decide(
            Rect::new(150.0, 320.0, 0.0, 24.0),
            other,
            Some(caret(898.0)),
        );
        assert!(d.accepted, "{}", d.why);
        assert_eq!(d.rect.x, 150.0, "换框后用的是新框里的测量值");
    }

    /// 逃生口：宿主连续给同一个被拒值，第三次承认它——用户可能是用我们看不见的
    /// 方式（Vim 键位、程序跳转）挪了光标。
    #[test]
    fn 连续同一个被拒值三次后放行() {
        let mut g = Gate::default();
        g.decide(caret(898.0), view(), None);
        assert!(!g.decide(caret(1636.0), view(), None).accepted);
        assert!(!g.decide(caret(1636.0), view(), None).accepted);
        let d = g.decide(caret(1636.0), view(), None);
        assert!(d.accepted, "{}", d.why);
        assert_eq!(d.rect.x, 1636.0);
    }

    /// D105 现场的形态：连续几个值互不相同（1330/1432/1440…），
    /// 一辈子也凑不满三次相同，所以一路被拒——正确。
    #[test]
    fn 连续不同的被拒值不算逃生口() {
        let mut g = Gate::default();
        g.decide(caret(898.0), view(), None);
        for x in [1330.0, 1432.0, 1440.0, 1448.0, 1490.0] {
            let d = g.decide(caret(x), view(), None);
            assert!(!d.accepted, "x={x} 不该放行");
            assert_eq!(d.rect.x, 898.0);
        }
    }

    #[test]
    fn 拒绝期间按上屏字宽慢跟随() {
        let mut g = Gate::default();
        g.decide(caret(898.0), view(), None);
        g.decide(caret(1636.0), view(), None); // 被拒
        g.note_committed("感觉"); // 两个全角，行高 24
        let d = g.decide(caret(1636.0), view(), None); // 仍被拒
        assert!(!d.accepted);
        assert_eq!(d.rect.x, 898.0 + 48.0, "两个汉字往前推 2em");
    }

    #[test]
    fn 放行后推进量归零() {
        let mut g = Gate::default();
        g.decide(caret(898.0), view(), None);
        g.decide(caret(1636.0), view(), None);
        g.note_committed("感觉");
        g.mark_free();
        // 放行一次：锚挪到 950，推进量清零。
        let d = g.decide(caret(950.0), view(), None);
        assert!(d.accepted);
        assert_eq!(d.rect.x, 950.0);
        // 再被拒时只按新的上屏量推，旧的 48px 不再重复计入。
        g.decide(caret(1636.0), view(), None);
        g.note_committed("g"); // 半角一个字母
        let d = g.decide(caret(1636.0), view(), None);
        assert_eq!(d.rect.x, 950.0 + 12.0);
    }

    /// 复量给出同一个值，不能算成「第二次连击」——否则逃生口会提前一半触发，
    /// 等于把闸门关小了。
    #[test]
    fn 复量不计入逃生口的连击() {
        let mut g = Gate::default();
        g.decide(caret(898.0), view(), None);
        // 按键判一次：拒绝，streak = 1
        assert!(!g.decide(caret(1636.0), view(), None).accepted);
        // 同一段编码的复量来三轮：都不该放行
        for _ in 0..5 {
            let d = g.decide_recheck(caret(1636.0), view());
            assert!(!d.accepted, "复量不该放行：{}", d.why);
        }
        // 按键再判两次才凑满三次
        assert!(!g.decide(caret(1636.0), view(), None).accepted);
        assert!(g.decide(caret(1636.0), view(), None).accepted);
    }

    #[test]
    fn 拿不到视图时按跳变判() {
        // 视图矩形缺一不可：没有它就只能靠跳变判据，不能当成「换了编辑区」。
        let mut g = Gate::default();
        g.decide(caret(898.0), None, None);
        let d = g.decide(caret(1636.0), None, None);
        assert!(!d.accepted, "{}", d.why);
    }
}
