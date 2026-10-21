//! 分页状态：候选总数 + 每页条数 + 当前第几页。
//!
//! ## 为什么它单独存在
//!
//! 「翻到第几页」是候选窗里唯一**有状态**的东西：布局是纯函数（给一页候选算矩形），
//! 定位也是纯函数，只有页码会在按键之间留存。而它最容易出错的地方全是边界——
//! 首页按左、末页按右、候选变少（退格）后页码越界、点选时把「本页第 2 条」当成
//! 「全部候选的第 2 条」。这些都能用纯算术验，所以放在这里而不是塞进 DLL。
//!
//! 页码越界的处理统一成两条：
//!
//! 1. 候选集一变就 [`Pager::reset`] 回第 1 页（五笔的习惯：重打编码从第一条看起）；
//! 2. 读页码时**夹紧**（`min(pages-1)`），所以哪怕调用方忘了 `reset`，也不会
//!    拿着一个空页去布局。

use crate::layout::{page_count, page_nav, page_slice, Dir};

/// 一页候选的状态。默认每页 5 条（单行窗口，数字键 1–9 选词）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pager {
    total: usize,
    page_size: usize,
    page: usize,
}

impl Pager {
    pub fn new(page_size: usize) -> Self {
        Self {
            total: 0,
            // 0 会让 `page_count` 恒为 0，等于永远没有候选；兜底成 5。
            page_size: if page_size == 0 { 5 } else { page_size },
            page: 0,
        }
    }

    /// 换成新的候选集，回到第 1 页。
    pub fn reset(&mut self, total: usize) {
        self.total = total;
        self.page = 0;
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    pub fn pages(&self) -> usize {
        page_count(self.total, self.page_size)
    }

    /// 当前页（已夹紧）。没有候选时是 0，此时 [`Pager::slice`] 返回空区间。
    pub fn page(&self) -> usize {
        let pages = self.pages();
        if pages == 0 {
            0
        } else {
            self.page.min(pages - 1)
        }
    }

    /// 本页覆盖的候选下标。
    pub fn slice(&self) -> core::ops::Range<usize> {
        page_slice(self.total, self.page_size, self.page())
    }

    /// 本页有几条候选（0 就是「不该显示窗口」）。
    pub fn len(&self) -> usize {
        self.slice().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 两端箭头该不该画。
    pub fn nav(&self) -> (bool, bool) {
        page_nav(self.total, self.page_size, self.page())
    }

    /// 本页第 `i` 条（0 起）在全部候选里的下标。超出本页返回 `None`。
    pub fn global(&self, i: usize) -> Option<usize> {
        let range = self.slice();
        (i < range.len()).then(|| range.start + i)
    }

    /// 翻一页。**真的翻动了才返回 `true`**——在首页按左、末页按右都返回 `false`，
    /// 此时按键仍然被吃掉（D38：宁可不动，也不让编码莫名其妙消失或光标跳走）。
    pub fn flip(&mut self, dir: Dir) -> bool {
        let current = self.page();
        let next = match dir {
            Dir::Prev => current.saturating_sub(1),
            Dir::Next => (current + 1).min(self.pages().saturating_sub(1)),
        };
        if next == current {
            return false;
        }
        self.page = next;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 换候选集回到第一页() {
        let mut p = Pager::new(5);
        p.reset(12);
        assert!(p.flip(Dir::Next));
        assert_eq!(p.page(), 1);
        p.reset(12);
        assert_eq!(p.page(), 0, "重打编码从第一条看起");
    }

    #[test]
    fn 翻页在两端都不动() {
        let mut p = Pager::new(5);
        p.reset(12); // 3 页
        assert!(!p.flip(Dir::Prev), "首页按左不动");
        assert!(p.flip(Dir::Next));
        assert!(p.flip(Dir::Next));
        assert_eq!(p.page(), 2);
        assert!(!p.flip(Dir::Next), "末页按右不动");
        assert_eq!(p.page(), 2);
    }

    #[test]
    fn 候选变少时页码夹紧() {
        let mut p = Pager::new(5);
        p.reset(12);
        p.flip(Dir::Next);
        p.flip(Dir::Next);
        assert_eq!(p.page(), 2);
        // 退格后只剩 3 条：页码从 2 夹回 0，而不是对着空页布局
        p.reset(3);
        assert_eq!(p.page(), 0);
        assert_eq!(p.len(), 3);
        // 即使调用方忘了 reset，读页码也不会越界
        let mut q = Pager::new(5);
        q.reset(20);
        q.flip(Dir::Next);
        q.total = 2;
        assert_eq!(q.page(), 0);
        assert_eq!(q.slice(), 0..2);
    }

    #[test]
    fn 本页序号换算成全局序号() {
        let mut p = Pager::new(5);
        p.reset(12);
        assert_eq!(p.global(0), Some(0));
        p.flip(Dir::Next);
        assert_eq!(p.global(0), Some(5));
        assert_eq!(p.global(4), Some(9));
        assert_eq!(p.global(5), None, "第 6 个数字键在本页没有对应候选");
        p.flip(Dir::Next);
        assert_eq!(p.len(), 2);
        assert_eq!(p.global(1), Some(11));
        assert_eq!(p.global(2), None);
    }

    #[test]
    fn 没有候选时是空页而且不显示箭头() {
        let p = Pager::new(5);
        assert!(p.is_empty());
        assert_eq!(p.pages(), 0);
        assert_eq!(p.slice(), 0..0);
        assert_eq!(p.nav(), (false, false));
        assert_eq!(p.global(0), None);
    }

    #[test]
    fn 每页条数为零时兜底成五() {
        let mut p = Pager::new(0);
        p.reset(6);
        assert_eq!(p.page_size(), 5);
        assert_eq!(p.nav(), (false, true));
    }

    #[test]
    fn 单页时两端都没有箭头() {
        let mut p = Pager::new(5);
        p.reset(4);
        assert_eq!(p.nav(), (false, false));
        assert!(!p.flip(Dir::Next));
    }
}
