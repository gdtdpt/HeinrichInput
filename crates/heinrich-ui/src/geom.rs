//! 几何基元。
//!
//! 刻意**不用** `windows::Win32::Foundation::RECT`：本 crate 要能在任意平台上单测，
//! 而 `RECT` 是 `i32`，会把「半像素的字宽」在源头就截掉。坐标一律用 `f32` 表达，
//! 取整只在边界处做一次（见 [`Rect::rounded`] 与 `place::place`）。

/// 宽高。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub const fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }

    pub fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

/// 左上角 + 宽高。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(self) -> f32 {
        self.y + self.h
    }

    pub fn size(self) -> Size {
        Size::new(self.w, self.h)
    }

    /// 四边等量内缩（负值即外扩）。
    ///
    /// 描边要压在边界上：不内缩半个线宽，1px 的线会被位图边缘裁掉一半，
    /// 看起来像缺了一条边。宽高夹到 0，避免负尺寸传进 D2D。
    pub fn inset(self, d: f32) -> Self {
        Self {
            x: self.x + d,
            y: self.y + d,
            w: (self.w - d * 2.0).max(0.0),
            h: (self.h - d * 2.0).max(0.0),
        }
    }

    /// 半开区间：右/下边不算在内。相邻的两个矩形不会同时命中同一个点，
    /// 否则点选落在缝隙上时会选中两个候选里的任一个。
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// 取整到整数像素。
    ///
    /// 分层窗口（`UpdateLayeredWindow`）的位置只能是整数像素，传小数会被系统
    /// 各自取整，导致窗口与位图错开半个像素——文字发虚。fcitx5 在 2026 年才把
    /// 这条修干净（"避免亚像素坐标的整数取整"），我们一开始就在边界处取整。
    pub fn rounded(self) -> Self {
        Self {
            x: self.x.round(),
            y: self.y.round(),
            w: self.w.round(),
            h: self.h.round(),
        }
    }
}
