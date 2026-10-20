//! 单行候选列表的布局：**吃进已经量好的文字宽度，吐出每个字该画在哪**。
//!
//! 这里不做字体度量、不读 DPI、不保留任何状态——同一份输入永远得到同一份输出。
//! 文字宽度由平台层用 DirectWrite 量好后传进来，于是「圆角/间距/分页/命中测试」
//! 这些最容易算错又最不容易用肉眼发现的部分，可以在没有窗口的情况下单测。
//!
//! 这个切法是照着同类产品抄的：Weasel 把 `Layout` 做成独立的类层次
//! （`HorizontalLayout` / `VerticalLayout` / `FullScreenLayout`，只算矩形、不画）；
//! fcitx5 的 Classic UI 用 Yoga 建一棵布局节点树，`InputWindow::paint` 只管照着画。

use crate::geom::{Rect, Size};

/// 一条候选。`width` 是这串文字在目标字号下的量测宽度（物理像素）。
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateText {
    pub text: String,
    pub width: f32,
}

impl CandidateText {
    pub fn new(text: impl Into<String>, width: f32) -> Self {
        Self {
            text: text.into(),
            width,
        }
    }
}

/// 视觉参数。
///
/// 唯一的自由变量是字号，其余全部按比例推导——DPI 变化时只把字号乘上缩放比，
/// 整套间距就一致地跟着放大。Weasel 的 `UIStyle` 走的是同一条路：配置项只到
/// `font_point` / `margin` / `hilite_padding` 这一层，像素值由绘制时按 DPI 算。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// 主字号（**物理像素**，已乘 DPI 缩放）。
    pub font_size: f32,
    /// 单行高度。
    pub line_height: f32,
    /// 窗口内边距。
    pub padding_x: f32,
    pub padding_y: f32,
    /// 候选之间的间隔。
    pub item_gap: f32,
    /// 单个候选的高亮块内边距（左右）。
    pub item_padding_x: f32,
    /// 序号与词之间。
    pub label_gap: f32,
    /// 一个数字序号的宽度。
    pub label_width: f32,
    /// 翻页箭头的宽度。
    pub arrow_width: f32,
    /// 箭头与候选之间。
    pub arrow_gap: f32,
    pub corner_radius: f32,
    /// 窗口最小宽度（候选很少时不至于缩成一个方块）。
    pub min_width: f32,
    /// 窗口最大宽度。`0` 表示不限。
    ///
    /// 是**软上限**：宽度是「尽量不超过」。所有候选都压到地板宽度还放不下时
    /// （比如一个候选本身就比上限宽），窗口会撑到刚好放得下——硬裁文字需要知道
    /// 字符边界，那是量测层的事，不在几何层做。
    pub max_width: f32,
}

impl Metrics {
    /// 以字号为基准推导一整套间距。
    pub fn new(font_size: f32) -> Self {
        let font_size = font_size.max(8.0);
        Self {
            font_size,
            line_height: (font_size * 1.45).round(),
            padding_x: (font_size * 0.55).round(),
            padding_y: (font_size * 0.22).round(),
            item_gap: (font_size * 0.30).round(),
            item_padding_x: (font_size * 0.28).round(),
            label_gap: (font_size * 0.28).round(),
            label_width: (font_size * 0.62).round(),
            // 一个字号宽：字形在这里居中绘制，留一点余量比正好贴边安全
            // （量出来的字形宽度在 100%/150% DPI 下不会完全等于字号）。
            arrow_width: (font_size * 1.0).round(),
            arrow_gap: (font_size * 0.30).round(),
            corner_radius: (font_size * 0.25).round(),
            min_width: (font_size * 5.0).round(),
            max_width: 0.0,
        }
    }

    /// 按倍数缩放整套参数（DPI 变化时用）。
    pub fn scaled(self, scale: f32) -> Self {
        let s = if scale > 0.0 { scale } else { 1.0 };
        Self {
            font_size: self.font_size * s,
            line_height: self.line_height * s,
            padding_x: self.padding_x * s,
            padding_y: self.padding_y * s,
            item_gap: self.item_gap * s,
            item_padding_x: self.item_padding_x * s,
            label_gap: self.label_gap * s,
            label_width: self.label_width * s,
            arrow_width: self.arrow_width * s,
            arrow_gap: self.arrow_gap * s,
            corner_radius: self.corner_radius * s,
            min_width: self.min_width * s,
            max_width: self.max_width * s,
        }
    }

    /// 单个候选的整项宽度（含高亮块内边距、序号与间隔）。
    pub fn item_width(&self, text_width: f32) -> f32 {
        self.item_padding_x * 2.0 + self.label_width + self.label_gap + text_width.max(0.0)
    }
}

/// 翻页方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// 往前一页（更靠前的候选）。
    Prev,
    /// 往后一页（更多候选）。
    Next,
}

/// 翻页箭头文字的默认字形。
///
/// 只说明「这个方向还有候选」，不说明「第几页」——用户不能跳页，页码对他没有
/// 信息量，五个候选的一行窗口里还要腾地方给它更不值得。真正的观感（字形大小、
/// 是否换成 `‹ ›`、要不要淡一点）留到 M2.2 真窗口里对着 100%/150% DPI 再定，
/// 这里是常量，改一处就全变。
pub const ARROW_PREV: &str = "◂";
pub const ARROW_NEXT: &str = "▸";

/// 一个翻页箭头。它**不是候选**：不参与 `hit()`，点它只翻页不选词。
#[derive(Clone, Debug, PartialEq)]
pub struct PageArrow {
    pub dir: Dir,
    pub text: String,
    pub rect: Rect,
}

/// 一次布局的输入。`page` 是**已经切好页**的候选。
#[derive(Clone, Debug)]
pub struct LayoutInput<'a> {
    pub page: &'a [CandidateText],
    /// 本页第一条在全部候选里的序号。点选时要回传全局序号（`vbg` 的第 7 个候选
    /// 不是本页的第 2 个），所以在布局里就带上。
    pub page_start: usize,
    /// 还有更靠前的候选（第 1 页时为 `false`）。`false` 时左端箭头不存在、
    /// 也不占宽度。
    pub has_prev: bool,
    /// 还有更靠后的候选（最后一页时为 `false`）。
    pub has_next: bool,
    /// 本页内的高亮项（键盘选中项），不参与几何。
    pub highlight: Option<usize>,
    pub metrics: Metrics,
}

/// 一个画出来的候选项。
#[derive(Clone, Debug, PartialEq)]
pub struct ItemBox {
    /// 全局序号（从全部候选的第 0 条算起）。
    pub index: usize,
    /// 序号标签，本页内从 `1` 起。
    pub label: String,
    pub text: String,
    /// 整项矩形（含高亮块内边距），鼠标命中就用它。
    pub rect: Rect,
    pub label_rect: Rect,
    /// 文字矩形。被 `max_width` 压窄时这里也变窄，渲染端应在此矩形内截断。
    pub text_rect: Rect,
    /// 这条的文字宽度被压窄过。
    pub elided: bool,
}

/// 布局结果。
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub size: Size,
    pub items: Vec<ItemBox>,
    /// 左端翻页箭头。第 1 页时不存在，也不占宽度。
    pub prev: Option<PageArrow>,
    /// 右端翻页箭头。最后一页时不存在，也不占宽度。
    pub next: Option<PageArrow>,
    pub highlight: Option<usize>,
    /// 有候选因为 `max_width` 被压窄。渲染端据此决定要不要在末尾画省略号。
    pub clipped: bool,
}

impl Layout {
    /// 命中测试：返回**全局**候选序号。落在缝隙、箭头、窗口内边距上返回 `None`。
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.items
            .iter()
            .find(|it| it.rect.contains(x, y))
            .map(|it| it.index)
    }

    /// 命中测试：落在哪个翻页箭头上。**与 `hit()` 分开**，因为点箭头只翻页、
    /// 不选词——两条动作不能混在一个返回值里。
    pub fn hit_arrow(&self, x: f32, y: f32) -> Option<Dir> {
        self.prev
            .as_ref()
            .filter(|a| a.rect.contains(x, y))
            .map(|a| a.dir)
            .or_else(|| {
                self.next
                    .as_ref()
                    .filter(|a| a.rect.contains(x, y))
                    .map(|a| a.dir)
            })
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// 全量候选要分几页。`page_size == 0` 或没有候选时为 `0`。
pub fn page_count(total: usize, page_size: usize) -> usize {
    if page_size == 0 || total == 0 {
        0
    } else {
        total.div_ceil(page_size)
    }
}

/// 第 `page` 页覆盖的候选下标区间（已按页数夹紧，越界的页号给最后一页）。
pub fn page_slice(total: usize, page_size: usize, page: usize) -> core::ops::Range<usize> {
    let pages = page_count(total, page_size);
    if pages == 0 {
        return 0..0;
    }
    let page = page.min(pages - 1);
    let start = page * page_size;
    let end = (start + page_size).min(total);
    start..end
}

/// 第 `page` 页两端的翻页箭头该不该出现：`(has_prev, has_next)`。
///
/// 和 [`page_slice`] 用同一套夹紧规则，所以越界页号得到的答案与它一致。
pub fn page_nav(total: usize, page_size: usize, page: usize) -> (bool, bool) {
    let pages = page_count(total, page_size);
    if pages == 0 {
        return (false, false);
    }
    let page = page.min(pages - 1);
    (page > 0, page + 1 < pages)
}

/// 算一页的布局。
pub fn layout(input: &LayoutInput<'_>) -> Layout {
    let m = input.metrics;
    let n = input.page.len();

    let mut widths: Vec<f32> = input.page.iter().map(|c| c.width.max(0.0)).collect();

    // 箭头只在真有候选的那个方向占地方：第 1 页没有左箭头，窗口就不必为此变宽。
    let arrow = m.arrow_width + m.arrow_gap;
    let reserved =
        if input.has_prev { arrow } else { 0.0 } + if input.has_next { arrow } else { 0.0 };

    // 先按自然宽度算一遍总宽。
    let mut total = sum_width(&m, &widths, reserved);

    // 超过上限时按各自宽度**成比例**压窄：长候选让得多、短候选让得少，
    // 视觉上比「每条均摊」更不容易把短词挤成一条缝。
    let mut clipped = false;
    if m.max_width > 0.0 && total > m.max_width && n > 0 {
        let overflow = total - m.max_width;
        let sum: f32 = widths.iter().sum();
        if sum > 0.0 {
            // 地板留 0.75 个字宽：再窄下去就只剩标点了。
            let floor = m.font_size * 0.75;
            for w in &mut widths {
                let share = overflow * (*w / sum);
                let next = (*w - share).max(floor);
                if next < *w {
                    clipped = true;
                    *w = next;
                }
            }
            total = sum_width(&m, &widths, reserved);
        }
    }

    let width = total.max(m.min_width);
    let height = m.line_height + m.padding_y * 2.0;

    let mut items = Vec::with_capacity(n);
    let mut x = m.padding_x;
    let y = m.padding_y;

    let prev = input.has_prev.then(|| PageArrow {
        dir: Dir::Prev,
        text: ARROW_PREV.to_string(),
        rect: Rect::new(m.padding_x, y, m.arrow_width, m.line_height),
    });
    if prev.is_some() {
        x = m.padding_x + m.arrow_width + m.arrow_gap;
    }

    for (i, cand) in input.page.iter().enumerate() {
        let item_w = m.item_width(widths[i]);
        let rect = Rect::new(x, y, item_w, m.line_height);
        let label_rect = Rect::new(x + m.item_padding_x, y, m.label_width, m.line_height);
        let text_w = widths[i];
        let text_rect = Rect::new(label_rect.right() + m.label_gap, y, text_w, m.line_height);
        items.push(ItemBox {
            index: input.page_start + i,
            // 序号是本页内的 1..=5。一页超过 9 条时标签会变成两位数，
            // 调用方应把 page_size 限制在 9 以内（数字键也就 1–9）。
            label: (i + 1).to_string(),
            text: cand.text.clone(),
            rect,
            label_rect,
            text_rect,
            elided: clipped && text_w < cand.width.max(0.0) - 0.01,
        });
        x = rect.right() + m.item_gap;
    }

    // 右箭头贴窗口右端。候选满了之后它本来就紧跟在最后一条后面；只有
    // 「有下一页 + 本页没满」这种不会发生的情况（有下一页 ⇒ 本页是满页）
    // 才会看到它和候选之间空出一块。
    let next = input.has_next.then(|| PageArrow {
        dir: Dir::Next,
        text: ARROW_NEXT.to_string(),
        rect: Rect::new(
            width - m.padding_x - m.arrow_width,
            y,
            m.arrow_width,
            m.line_height,
        ),
    });

    Layout {
        size: Size::new(width, height),
        items,
        prev,
        next,
        highlight: input.highlight,
        clipped,
    }
}

fn sum_width(m: &Metrics, widths: &[f32], reserved: f32) -> f32 {
    let items: f32 = widths.iter().map(|w| m.item_width(*w)).sum();
    let gaps = if widths.is_empty() {
        0.0
    } else {
        m.item_gap * (widths.len() as f32 - 1.0)
    };
    m.padding_x * 2.0 + items + gaps + reserved
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(text: &str, w: f32) -> CandidateText {
        CandidateText::new(text, w)
    }

    fn input<'a>(page: &'a [CandidateText]) -> LayoutInput<'a> {
        LayoutInput {
            page,
            page_start: 0,
            has_prev: false,
            has_next: false,
            highlight: Some(0),
            metrics: Metrics::new(16.0),
        }
    }

    #[test]
    fn 单条候选把内边距和序号都算进去() {
        let page = [cand("好", 16.0)];
        let m = Metrics::new(16.0);
        let out = layout(&input(&page));

        let it = &out.items[0];
        assert_eq!(it.rect.x, m.padding_x);
        assert_eq!(it.rect.y, m.padding_y);
        assert_eq!(it.rect.h, m.line_height);
        assert_eq!(it.label_rect.right() + m.label_gap, it.text_rect.x);
        assert_eq!(it.text_rect.w, 16.0);
        assert_eq!(it.rect.w, m.item_width(16.0));
        // 自然宽度小于 min_width 时窗口会被兜底撑宽，所以只断言「不压住右边」
        assert!(out.size.w >= it.rect.right() + m.padding_x);
        assert_eq!(out.size.h, m.line_height + m.padding_y * 2.0);
        assert!(!out.clipped);
        assert_eq!(out.items[0].label, "1");
    }

    #[test]
    fn 多条候选右排且间隔等于_item_gap() {
        let page = [cand("好", 16.0), cand("㛅", 16.0), cand("崽", 16.0)];
        let m = Metrics::new(16.0);
        let out = layout(&input(&page));

        for pair in out.items.windows(2) {
            let gap = pair[1].rect.x - pair[0].rect.right();
            assert!((gap - m.item_gap).abs() < 0.01, "间隔应为 {}", m.item_gap);
        }
        // 没有重叠
        for pair in out.items.windows(2) {
            assert!(pair[0].rect.right() <= pair[1].rect.x);
        }
        assert!(out.size.w - out.items.last().unwrap().text_rect.right() >= m.padding_x);
    }

    #[test]
    fn 序号是本页内的一到五_全局序号从_page_start_起() {
        let page = [
            cand("a", 10.0),
            cand("b", 10.0),
            cand("c", 10.0),
            cand("d", 10.0),
            cand("e", 10.0),
        ];
        let mut input = input(&page);
        input.page_start = 5;
        input.has_prev = true;
        let out = layout(&input);

        assert_eq!(
            out.items
                .iter()
                .map(|i| i.label.as_str())
                .collect::<Vec<_>>(),
            vec!["1", "2", "3", "4", "5"]
        );
        assert_eq!(
            out.items.iter().map(|i| i.index).collect::<Vec<_>>(),
            vec![5, 6, 7, 8, 9]
        );
    }

    #[test]
    fn 右箭头贴右端且不压住最后一个候选() {
        let page = [cand("好", 16.0), cand("㛅", 16.0)];
        let mut input = input(&page);
        input.has_next = true;
        let m = Metrics::new(16.0);
        let out = layout(&input);

        let arrow = out.next.as_ref().unwrap();
        assert_eq!(arrow.dir, Dir::Next);
        assert!((arrow.rect.right() - (out.size.w - m.padding_x)).abs() < 0.01);
        assert!(arrow.rect.x >= out.items.last().unwrap().rect.right());
        assert!(out.prev.is_none());
    }

    #[test]
    fn 左箭头排在最前且与第一个候选隔_arrow_gap() {
        let page = [cand("好", 16.0), cand("㛅", 16.0)];
        let mut input = input(&page);
        input.has_prev = true;
        let m = Metrics::new(16.0);
        let out = layout(&input);

        let arrow = out.prev.as_ref().unwrap();
        assert_eq!(arrow.dir, Dir::Prev);
        assert_eq!(arrow.rect.x, m.padding_x);
        assert_eq!(arrow.rect.w, m.arrow_width);
        assert!((out.items[0].rect.x - (arrow.rect.right() + m.arrow_gap)).abs() < 0.01);
        assert!(out.next.is_none());
    }

    #[test]
    fn 只有一页时两端都没有箭头() {
        let page = [cand("好", 16.0)];
        let out = layout(&input(&page));
        assert!(out.prev.is_none());
        assert!(out.next.is_none());
    }

    #[test]
    fn 箭头只在需要时占宽度() {
        let page = [cand("好", 16.0), cand("㛅", 16.0)];
        let plain = layout(&input(&page));

        let mut with_next = input(&page);
        with_next.has_next = true;
        let one_side = layout(&with_next);

        let mut both = input(&page);
        both.has_prev = true;
        both.has_next = true;
        let two_sides = layout(&both);

        let m = Metrics::new(16.0);
        let step = m.arrow_width + m.arrow_gap;
        assert!((one_side.size.w - plain.size.w - step).abs() < 0.01);
        assert!((two_sides.size.w - plain.size.w - step * 2.0).abs() < 0.01);
        // 候选本身不被压窄，只是整体右移
        assert!((two_sides.items[0].rect.x - plain.items[0].rect.x - step).abs() < 0.01);
        assert_eq!(two_sides.items[0].text_rect.w, plain.items[0].text_rect.w);
    }

    #[test]
    fn 点箭头只翻页_不选词() {
        let page = [cand("好", 16.0), cand("㛅", 16.0)];
        let mut input = input(&page);
        input.has_prev = true;
        input.has_next = true;
        let out = layout(&input);

        for arrow in [out.prev.as_ref().unwrap(), out.next.as_ref().unwrap()] {
            let (x, y) = (arrow.rect.x + 1.0, arrow.rect.y + 1.0);
            assert_eq!(out.hit(x, y), None, "箭头不该被当成候选");
            assert_eq!(out.hit_arrow(x, y), Some(arrow.dir));
        }
        // 候选身上不会误判成箭头
        let it = out.items[0].rect;
        assert_eq!(out.hit_arrow(it.x + 1.0, it.y + 1.0), None);
    }

    #[test]
    fn 超宽时按比例压窄并标记_clipped() {
        let page = [
            cand("一一一一", 100.0),
            cand("二", 50.0),
            cand("三三三三", 100.0),
        ];
        let mut input = input(&page);
        input.metrics.max_width = 260.0;
        let out = layout(&input);

        assert!(out.clipped);
        assert!(out.size.w <= 260.0 + 0.01, "实际 {}", out.size.w);
        // 长候选让得比短的更多
        let a = out.items[0].text_rect.w;
        let b = out.items[1].text_rect.w;
        assert!(100.0 - a > 50.0 - b);
        assert!(out.items.iter().all(|i| i.elided));
    }

    #[test]
    fn 不设上限时按自然宽度排() {
        let page = [cand("一一一一", 100.0), cand("二二二二", 100.0)];
        let out = layout(&input(&page));
        assert!(!out.clipped);
        assert!(out.size.w > 200.0);
    }

    #[test]
    fn 候选很少时由_min_width_兜底() {
        let page = [cand("我", 16.0)];
        let m = Metrics::new(16.0);
        let out = layout(&input(&page));
        assert_eq!(out.size.w, m.min_width);
    }

    #[test]
    fn 命中测试用整项矩形_缝隙不命中() {
        let page = [cand("好", 16.0), cand("㛅", 16.0)];
        let m = Metrics::new(16.0);
        let out = layout(&input(&page));

        let first = out.items[0].rect;
        // 高亮块内边距里也算命中（否则点数字左边会点空）
        assert_eq!(out.hit(first.x + 1.0, first.y + 1.0), Some(0));
        assert_eq!(out.hit(first.right() - 0.5, first.y + 1.0), Some(0));
        // 两条之间的缝隙
        let gap_x = first.right() + m.item_gap * 0.5;
        assert_eq!(out.hit(gap_x, first.y + 1.0), None);
        // 窗口外
        assert_eq!(out.hit(first.x - 5.0, first.y), None);
        assert_eq!(out.hit(first.x, first.y + out.size.h + 1.0), None);
    }

    #[test]
    fn 命中返回全局序号() {
        let page = [cand("a", 10.0), cand("b", 10.0)];
        let mut input = input(&page);
        input.page_start = 5;
        let out = layout(&input);
        let second = out.items[1].rect;
        assert_eq!(out.hit(second.x + 1.0, second.y + 1.0), Some(6));
    }

    #[test]
    fn 空页只有一个内边距大小的框() {
        let page: [CandidateText; 0] = [];
        let m = Metrics::new(16.0);
        let out = layout(&input(&page));
        assert!(out.is_empty());
        assert_eq!(out.size.h, m.line_height + m.padding_y * 2.0);
        assert_eq!(out.size.w, m.min_width);
    }

    #[test]
    fn 分页数量() {
        assert_eq!(page_count(0, 5), 0);
        assert_eq!(page_count(1, 5), 1);
        assert_eq!(page_count(5, 5), 1);
        assert_eq!(page_count(6, 5), 2);
        assert_eq!(page_count(45, 5), 9);
        assert_eq!(page_count(45, 0), 0);
    }

    #[test]
    fn 分页区间会夹紧越界页号() {
        assert_eq!(page_slice(12, 5, 0), 0..5);
        assert_eq!(page_slice(12, 5, 2), 10..12);
        assert_eq!(page_slice(12, 5, 99), 10..12);
        assert_eq!(page_slice(0, 5, 0), 0..0);
    }

    #[test]
    fn 翻页箭头只在真有多页时出现() {
        // 只有一页：两端都没有
        assert_eq!(page_nav(3, 5, 0), (false, false));
        // 两页：首页只有右、末页只有左
        assert_eq!(page_nav(12, 5, 0), (false, true));
        assert_eq!(page_nav(12, 5, 2), (true, false));
        // 中间页两端都有
        assert_eq!(page_nav(20, 5, 1), (true, true));
        // 越界页号按最后一页算，与 page_slice 一致
        assert_eq!(page_nav(12, 5, 99), (true, false));
        assert_eq!(page_nav(0, 5, 0), (false, false));
        assert_eq!(page_nav(12, 0, 0), (false, false));
    }

    #[test]
    fn dpi_缩放整套间距等比变化() {
        let base = Metrics::new(16.0);
        let double = base.scaled(2.0);
        assert_eq!(double.line_height, base.line_height * 2.0);
        assert_eq!(double.padding_x, base.padding_x * 2.0);
        // 字号与文字宽度一起翻倍时，整项宽度正好翻倍
        assert_eq!(double.item_width(32.0), base.item_width(16.0) * 2.0);

        let page = [cand("好", 32.0)];
        let mut input = input(&page);
        input.metrics = double;
        let out = layout(&input);
        assert!((out.size.h - (base.line_height + base.padding_y * 2.0) * 2.0).abs() < 0.01);
    }
}
