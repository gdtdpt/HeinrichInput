//! 候选窗的位置：给定「光标矩形」「窗口大小」「显示器工作区」，算出窗口该放哪。
//!
//! 规则照抄同类产品的实测行为，只有两条：
//!
//! 1. **默认贴在光标下沿**；下方放不下就**翻到光标上方**；
//! 2. **任何情况下不越出工作区**（工作区 = `MonitorFromRect(光标)` 的 `rcWork`，
//!    不含任务栏）。
//!
//! 这两条就是 Weasel `WeaselPanel::_RepositionWindow` 的全部逻辑：
//!
//! ```text
//! rcWorkArea.right -= width;  rcWorkArea.bottom -= height;
//! int x = m_inputPos.left;    int y = m_inputPos.bottom;
//! if (x > rcWorkArea.right)  x = rcWorkArea.right;
//! if (x < rcWorkArea.left)   x = rcWorkArea.left;
//! if (y > rcWorkArea.bottom) y = m_inputPos.top - height;   // 翻到上方
//! if (y > rcWorkArea.bottom) y = rcWorkArea.bottom;         // 还是放不下 → 贴底
//! ```
//!
//! 差别只有一处：它「贴底」，我们「贴顶」。窗口比工作区还高时（多屏 + 150% DPI
//! 会出现），贴底会把窗口的**上沿连同行首一起推到屏幕外**，贴顶至少能看到第一条
//! 候选。工作区能装下窗口时两种写法完全等价。

use crate::geom::{Rect, Size};

/// 定位结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// 窗口矩形（左上角是屏幕坐标，已取整）。
    pub rect: Rect,
    /// 是否翻到了光标上方。
    pub flipped: bool,
}

/// 算窗口位置。
///
/// - `anchor`：光标矩形（TSF 的 `GetTextExt` 给的就是这个）
/// - `size`：窗口大小（来自 `layout`）
/// - `work`：光标所在显示器的**工作区**
/// - `gap`：窗口与光标之间留的空隙
pub fn place(anchor: Rect, size: Size, work: Rect, gap: f32) -> Placement {
    let gap = gap.max(0.0);
    let w = size.w.max(0.0);
    let h = size.h.max(0.0);

    let below = anchor.bottom() + gap;
    let above = anchor.y - gap - h;

    let (mut y, flipped) = if below + h <= work.bottom() {
        (below, false)
    } else if above >= work.y {
        (above, true)
    } else {
        // 两侧都放不下：留在下方，靠下面的夹紧贴顶。
        (below, false)
    };

    // 夹紧到工作区。窗口比工作区还大时，`max_*` 会取工作区的上/左边界，
    // 于是窗口对齐到左上角，而不是跑出去。
    let max_x = (work.right() - w).max(work.x);
    let max_y = (work.bottom() - h).max(work.y);
    let mut x = anchor.x;

    x = x.min(max_x).max(work.x);
    y = y.min(max_y).max(work.y);

    Placement {
        rect: Rect::new(x, y, w, h).rounded(),
        flipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1920×1080、任务栏占底部 40px 的工作区。
    fn work() -> Rect {
        Rect::new(0.0, 0.0, 1920.0, 1040.0)
    }

    /// 光标在正文中间：有足够空间放下面。
    fn caret_middle() -> Rect {
        Rect::new(400.0, 500.0, 2.0, 20.0)
    }

    fn win() -> Size {
        Size::new(300.0, 40.0)
    }

    #[test]
    fn 默认贴在光标下沿() {
        let p = place(caret_middle(), win(), work(), 2.0);
        assert!(!p.flipped);
        assert_eq!(p.rect.x, 400.0);
        assert_eq!(p.rect.y, 522.0);
        assert_eq!(p.rect.size(), win());
    }

    #[test]
    fn 下方放不下就翻到上方() {
        let caret = Rect::new(400.0, 1010.0, 2.0, 20.0);
        let p = place(caret, win(), work(), 2.0);
        assert!(p.flipped);
        // 光标上沿 1010 - 空隙 2 - 窗高 40 = 968
        assert_eq!(p.rect.y, 968.0);
        assert!(p.rect.bottom() <= caret.y);
    }

    #[test]
    fn 两侧都放不下时贴顶不越界() {
        // 窗口比工作区还高：上方 -64、下方 64 都放不进 1040 高的区域
        let caret = Rect::new(10.0, 40.0, 2.0, 20.0);
        let tall = Size::new(200.0, 2000.0);
        let p = place(caret, tall, work(), 4.0);
        assert_eq!(p.rect.y, 0.0);
        assert_eq!(p.rect.h, 2000.0);
    }

    #[test]
    fn 右边越界时右对齐到工作区() {
        let caret = Rect::new(1900.0, 500.0, 2.0, 20.0);
        let p = place(caret, win(), work(), 2.0);
        assert_eq!(p.rect.right(), 1920.0);
        assert_eq!(p.rect.x, 1620.0);
    }

    #[test]
    fn 左边越界时左对齐到工作区() {
        // 副屏在主屏左侧时坐标是负的，这里用 work 左边界 0 模拟
        let caret = Rect::new(-30.0, 500.0, 2.0, 20.0);
        let p = place(caret, win(), work(), 2.0);
        assert_eq!(p.rect.x, 0.0);
    }

    #[test]
    fn 窗口比工作区还宽时左对齐() {
        let wide = Size::new(3000.0, 40.0);
        let p = place(caret_middle(), wide, work(), 2.0);
        assert_eq!(p.rect.x, 0.0);
    }

    #[test]
    fn 副屏坐标为负时同样不越界() {
        let work = Rect::new(-1920.0, 0.0, 1920.0, 1040.0);
        let caret = Rect::new(-300.0, 500.0, 2.0, 20.0);
        let p = place(caret, win(), work, 2.0);
        assert_eq!(p.rect.x, -300.0);
        assert!(p.rect.x >= work.x && p.rect.right() <= work.right());

        // 光标在该屏最右侧（x = -10），窗宽 300 放不下 → 右对齐到 0
        let caret = Rect::new(-10.0, 500.0, 2.0, 20.0);
        let p = place(caret, win(), work, 2.0);
        assert_eq!(p.rect.right(), 0.0);
        assert_eq!(p.rect.x, -300.0);
    }

    #[test]
    fn 输出整数像素坐标() {
        let caret = Rect::new(400.4, 500.7, 2.5, 19.3);
        let p = place(caret, Size::new(300.5, 40.4), work(), 1.5);
        for v in [p.rect.x, p.rect.y, p.rect.w, p.rect.h] {
            assert_eq!(v.fract(), 0.0, "应为整数：{v}");
        }
    }

    #[test]
    fn 负的间距当零处理() {
        let p = place(caret_middle(), win(), work(), -10.0);
        assert_eq!(p.rect.y, 520.0);
    }
}
