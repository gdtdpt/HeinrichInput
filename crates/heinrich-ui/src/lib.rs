//! 亨利输入法 · 候选窗的**纯几何层**。
//!
//! 这个 crate 只回答两个问题，都用纯算术回答：
//!
//! 1. [`layout`]：这一页候选，每个字画在哪个矩形里，窗口多大；
//! 2. [`place`]：这个窗口该放在屏幕的哪个坐标。
//!
//! 剩下的（HWND、分层窗口、DirectWrite、DPI 消息、鼠标捕获）全在
//! `apps/windows/ui`——那部分只能靠肉眼和截图验收，所以尽量薄。
//!
//! ## 为什么这么切
//!
//! 候选窗最容易出错的地方不是绘制效果，而是**算错又看不出来**：页码压住了最后
//! 一条候选、翻页后窗口比屏幕高、右侧越界被系统裁掉、150% DPI 下光标和窗口错开
//! 二十像素。这些全是数值问题，放在「必须开窗口才能验」的层里就只能靠人眼扫。
//!
//! 所以这里刻意不做字体度量：文字宽度由调用方用 DirectWrite 量好传进来
//! （[`CandidateText::width`]），本 crate 于是能在任何平台上跑单测。
//!
//! ## 同类产品怎么做的
//!
//! | 产品 | 窗口 | 布局 | 定位 | 翻页 |
//! |---|---|---|---|---|
//! | Weasel（小狼毫） | 自绘 `WeaselPanel`：`WS_POPUP｜WS_DISABLED` + `WS_EX_TOOLWINDOW｜TOPMOST｜NOACTIVATE｜LAYERED`，`UpdateLayeredWindow`，`WM_DPICHANGED` 时重建 | 独立的 `Layout` 类层次（Horizontal / Vertical / FullScreen），只算矩形 | `MonitorFromRect(光标)` 取 `rcWork`，越右夹右、越下翻上方 | `WM_MOUSEWHEEL`、`WM_MOUSEMOVE` 悬停、点选命中 |
//! | fcitx5 Classic UI | 自绘（Cairo + Pango），X11 用 override-redirect 窗、Wayland 用 layer-shell | Yoga 布局引擎建节点树，`paint` 只负责照画 | 按输入光标位置 + 屏幕 DPI 逐屏计算 | 配置项 `verticalCandidateList` 等 |
//! | 青简 | 平台壳各写一份（macOS IMK / Windows TSF + 独立引擎进程），候选窗属于壳 | core 不管界面 | core 不管界面 | core 不管界面 |
//!
//! 三条共同点，也是本 crate 的形状来源：**都是自绘**（没有一家指望宿主的
//! `ITfCandidateListUIElement`）、**都把布局与绘制分开**、**都把定位夹进工作区**。
//! 第三条尤其一致——Weasel 的 `_RepositionWindow` 和 fcitx5 的逐屏 DPI 处理
//! 解决的是同一个问题，只是写法不同。

mod geom;
mod layout;
mod place;

pub use geom::{Rect, Size};
pub use layout::{
    layout, page_count, page_slice, CandidateText, ItemBox, Layout, LayoutInput, Metrics,
};
pub use place::{place, Placement};
