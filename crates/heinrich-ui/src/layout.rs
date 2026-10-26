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
    /// 候选右侧的码提示（§6.3 的 chip）。`None` / 空串 = 不画。
    ///
    /// 宽度必须由量测层给：chip 用的是**小一号**的字号，拿 `text` 的宽度换算
    /// 一定不准（`VB` 在大字号下比在小字号下宽得多）。
    pub hint: Option<String>,
    /// chip 的宽度（物理像素）。`hint` 为 `None` 时忽略。
    pub hint_width: f32,
    /// chip 的**词中位置**（D64）：挂在第 `hint_at` 个字符之后，`0` = 挂词尾。
    ///
    /// 多段组合（`hao'ktn` → 好吃）的 chip 必须贴着拼音段的字（`好[vbg]吃`），
    /// 挂词尾会被误读成整个词的五笔码。大于 0 时量测层必须同时给
    /// [`CandidateText::pre_width`] / [`CandidateText::post_width`]——
    /// 前后两截要分开画，整串宽度换算不出它们各自的边界。
    pub hint_at: usize,
    /// `hint_at > 0` 时 chip 前那截文字的宽度（量测层量好）。
    pub pre_width: f32,
    /// `hint_at > 0` 时 chip 后那截文字的宽度（量测层量好）。
    pub post_width: f32,
    /// 词组右侧的 emoji（§6.4 / D66）。`None` / 空串 = 不画。
    ///
    /// 宽度由量测层给：emoji 走的是彩色字形（Segoe UI Emoji 回退），它的
    /// 前进宽度**不等于**一个汉字宽，按字号估会在候选行里留下一条缝。
    pub emoji: Option<String>,
    /// emoji 的宽度（物理像素）。`emoji` 为 `None` 时忽略。
    pub emoji_width: f32,
}

impl CandidateText {
    pub fn new(text: impl Into<String>, width: f32) -> Self {
        Self {
            text: text.into(),
            width,
            hint: None,
            hint_width: 0.0,
            hint_at: 0,
            pre_width: 0.0,
            post_width: 0.0,
            emoji: None,
            emoji_width: 0.0,
        }
    }

    /// 带 emoji 的候选（§6.4）。emoji 排在词尾（有 chip 时排在 chip 之后）。
    pub fn with_emoji(mut self, emoji: impl Into<String>, width: f32) -> Self {
        let emoji = emoji.into();
        if emoji.is_empty() {
            return self;
        }
        self.emoji = Some(emoji);
        self.emoji_width = width.max(0.0);
        self
    }

    /// 带码提示的候选，chip 挂词尾。`hint_width` 由量测层用 chip 自己的字号量好。
    pub fn with_hint(mut self, hint: impl Into<String>, hint_width: f32) -> Self {
        let hint = hint.into();
        if hint.is_empty() {
            return self;
        }
        self.hint = Some(hint);
        self.hint_width = hint_width.max(0.0);
        self
    }

    /// 带码提示且 chip **嵌在词中**的候选（D64）。
    ///
    /// `at` 是 chip 前的字符数；`pre` / `post` 是前后两截文字各自的量测宽度。
    /// 三个宽度（整串、前截、后截）都由量测层给，几何层不做任何字体假设。
    pub fn with_hint_at(
        mut self,
        hint: impl Into<String>,
        hint_width: f32,
        at: usize,
        pre: f32,
        post: f32,
    ) -> Self {
        self = self.with_hint(hint, hint_width);
        if at > 0 {
            self.hint_at = at;
            self.pre_width = pre.max(0.0);
            self.post_width = post.max(0.0);
        }
        self
    }

    /// 这条候选是否要把词拆成前后两截画（chip 嵌词中）。
    fn is_split(&self) -> bool {
        self.hint.is_some() && self.hint_at > 0
    }

    /// 按 `hint_at` 把词拆成（前截, 后截）。只在 [`Self::is_split`] 时有意义。
    fn split_parts(&self) -> (String, String) {
        let pre = self.text.chars().take(self.hint_at).collect();
        let post = self.text.chars().skip(self.hint_at).collect();
        (pre, post)
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
    /// 词与码提示 chip 之间的间隔。
    pub hint_gap: f32,
    /// chip 自己的水平内边距（左右各一份）。
    ///
    /// chip 是「一小块底 + 里面几个字母」，文字紧贴底边会显得挤；这个值同时决定
    /// 命中区与视觉区，所以只在这里算一次。
    pub hint_padding_x: f32,
    /// 词（或 chip）与 emoji 之间的间隔（§6.4）。与 `hint_gap` 同尺度：
    /// 两者都是「主文字后面的小标签」，间距不一致会看起来像两套排版。
    pub emoji_gap: f32,
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
            hint_gap: (font_size * 0.22).round(),
            hint_padding_x: (font_size * 0.22).round(),
            emoji_gap: (font_size * 0.22).round(),
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
            hint_gap: self.hint_gap * s,
            hint_padding_x: self.hint_padding_x * s,
            emoji_gap: self.emoji_gap * s,
            arrow_width: self.arrow_width * s,
            arrow_gap: self.arrow_gap * s,
            corner_radius: self.corner_radius * s,
            min_width: self.min_width * s,
            max_width: self.max_width * s,
        }
    }

    /// 单个候选的整项宽度（含高亮块内边距、序号与间隔）。
    pub fn item_width(&self, text_width: f32) -> f32 {
        self.item_width_with_hint(text_width, 0.0)
    }

    /// 带码提示时的整项宽度。`hint_width` 含 chip 自己的左右内边距。
    pub fn item_width_with_hint(&self, text_width: f32, hint_width: f32) -> f32 {
        self.item_width_full(text_width, hint_width, 0.0)
    }

    /// 整项宽度（§6.4 起多一枚 emoji）。三块追加件（chip / emoji）都不参与
    /// 压窄，所以宽度公式只有这一处，压窄只改 `text_width`。
    pub fn item_width_full(&self, text_width: f32, hint_width: f32, emoji_width: f32) -> f32 {
        let chip = if hint_width > 0.0 {
            self.hint_gap + hint_width + self.hint_padding_x * 2.0
        } else {
            0.0
        };
        let emoji = if emoji_width > 0.0 {
            self.emoji_gap + emoji_width
        } else {
            0.0
        };
        self.item_padding_x * 2.0
            + self.label_width
            + self.label_gap
            + text_width.max(0.0)
            + chip
            + emoji
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
/// 信息量，五个候选的一行窗口里还要腾地方给它更不值得。
///
/// 用 **`U+25C0` / `U+25B6` 实心三角**而不是更细的 `‹ ›` 或 `▸`：
/// 小三角（`U+25B8`）在中文字体里常常不存在，DirectWrite 会回退到别的字体，
/// 结果是一个既小又淡、跟字号对不上的符号（M2.2 真窗口里实测过）。实心三角是
/// CJK 字体的保留字符，不会回退，缩放时也不失真。
pub const ARROW_PREV: &str = "◀";
pub const ARROW_NEXT: &str = "▶";

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
    /// 折叠区第一条在**全部候选**里的序号（D48）。
    ///
    /// 混输下候选分两段：主段（当前判定语言，五笔优先）在前、另一门语言在后。
    /// 两段在视觉上必须能分开——否则用户看到「虚、好、号、浩」会以为列表坏了，
    /// 或者以为「好」也是五笔打出来的。`None` = 只有一段（纯五笔 / 纯拼音）。
    pub split: Option<usize>,
    pub metrics: Metrics,
}

/// 一个画出来的候选项。
#[derive(Clone, Debug, PartialEq)]
pub struct ItemBox {
    /// 全局序号（从全部候选的第 0 条算起）。
    pub index: usize,
    /// 序号标签，本页内从 `1` 起。
    pub label: String,
    /// chip 前那截文字（D64 词中 chip 时只有前截；否则是整条词）。
    pub text: String,
    /// chip 后那截文字（D64 词中 chip 才有；空串 = 没有后截）。
    pub text2: String,
    /// 整项矩形（含高亮块内边距），鼠标命中就用它。
    pub rect: Rect,
    pub label_rect: Rect,
    /// [`Self::text`] 的矩形。被 `max_width` 压窄时这里也变窄，渲染端应在此矩形内截断。
    pub text_rect: Rect,
    /// [`Self::text2`] 的矩形。`text2` 为空时无效。
    pub text2_rect: Rect,
    /// 码提示 chip 的文字（§6.3）。`None` = 这条不提示。
    pub hint: Option<String>,
    /// chip 的矩形（**含**左右内边距，渲染端画底与写字都用它）。
    pub hint_rect: Rect,
    /// 词组 emoji（§6.4 / D66）。`None` = 这条不挂，或本次被压窄丢掉了。
    pub emoji: Option<String>,
    /// emoji 的矩形。`emoji` 为 `None` 时无效（宽高都是 0）。
    pub emoji_rect: Rect,
    /// 这条的文字宽度被压窄过。
    pub elided: bool,
    /// 属于折叠区（另一门语言的补段）。渲染端应当把它画淡一档。
    pub secondary: bool,
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
    // 词中 chip（D64）的候选：参与排版与压窄的「文字宽」是前后两截之和。
    let mut split_w: Vec<(f32, f32)> = input
        .page
        .iter()
        .map(|c| {
            if c.is_split() {
                (c.pre_width.max(0.0), c.post_width.max(0.0))
            } else {
                (0.0, 0.0)
            }
        })
        .collect();
    for (w, (pre, post)) in widths.iter_mut().zip(split_w.iter_mut()) {
        if *pre + *post > 0.0 {
            *w = *pre + *post;
        }
    }
    // chip 的宽度**不参与压窄**：码是三四字母的短串，压窄它只会让提示变得不可读，
    // 而它本来就窄。超宽时该让的是词。
    let hint_w: Vec<f32> = input
        .page
        .iter()
        .map(|c| {
            if c.hint.is_some() {
                c.hint_width.max(0.0)
            } else {
                0.0
            }
        })
        .collect();
    // emoji 的宽度单独一列：它**会被整行丢掉**（见下面的压窄顺序），所以要能单独清零。
    let mut emoji_w: Vec<f32> = input
        .page
        .iter()
        .map(|c| {
            if c.emoji.is_some() {
                c.emoji_width.max(0.0)
            } else {
                0.0
            }
        })
        .collect();

    // 箭头只在真有候选的那个方向占地方：第 1 页没有左箭头，窗口就不必为此变宽。
    let arrow = m.arrow_width + m.arrow_gap;
    let reserved =
        if input.has_prev { arrow } else { 0.0 } + if input.has_next { arrow } else { 0.0 };

    // 先按自然宽度算一遍总宽。
    let mut total = sum_width(&m, &widths, &hint_w, &emoji_w, reserved);

    // 压窄的**顺序**是硬约定（M4c 验收 ①）：先丢 emoji，再压词。
    // emoji 是装饰、词是内容——把「好评」压成「好…」去保住一枚 👍 是本末倒置。
    if m.max_width > 0.0 && total > m.max_width && emoji_w.iter().any(|w| *w > 0.0) {
        for w in emoji_w.iter_mut() {
            *w = 0.0;
        }
        total = sum_width(&m, &widths, &hint_w, &emoji_w, reserved);
    }

    // 超过上限时按各自宽度**成比例**压窄：长候选让得多、短候选让得少，
    // 视觉上比「每条均摊」更不容易把短词挤成一条缝。
    let mut clipped = false;
    if m.max_width > 0.0 && total > m.max_width && n > 0 {
        let overflow = total - m.max_width;
        let sum: f32 = widths.iter().sum();
        if sum > 0.0 {
            // 地板留 0.75 个字宽：再窄下去就只剩标点了。
            let floor = m.font_size * 0.75;
            for (i, w) in widths.iter_mut().enumerate() {
                let share = overflow * (*w / sum);
                let next = (*w - share).max(floor);
                if next < *w {
                    clipped = true;
                    // 词中 chip 的候选按同一比例缩前后两截，chip 位置才不漂移。
                    let k = next / *w;
                    let (pre, post) = &mut split_w[i];
                    if *pre + *post > 0.0 {
                        *pre *= k;
                        *post *= k;
                    }
                    *w = next;
                }
            }
            total = sum_width(&m, &widths, &hint_w, &emoji_w, reserved);
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
        let item_w = m.item_width_full(widths[i], hint_w[i], emoji_w[i]);
        let rect = Rect::new(x, y, item_w, m.line_height);
        let label_rect = Rect::new(x + m.item_padding_x, y, m.label_width, m.line_height);

        // 一条候选从左到右只有四种东西：词（可能拆两截）、chip、emoji。
        // 用游标顺序摆放，谁缺席就谁不占位——`None` / 宽度为 0 都不留空当。
        let mut cx = label_rect.right() + m.label_gap;
        let has_hint = cand.hint.is_some() && hint_w[i] > 0.0;
        let (text, text_rect, text2, text2_rect, hint, hint_rect) = if cand.is_split() {
            // 词中 chip（D64）：前截文字、chip、后截文字依次排开。
            let (pre_str, post_str) = cand.split_parts();
            let (pre_w, post_w) = split_w[i];
            let pre_rect = Rect::new(cx, y, pre_w, m.line_height);
            cx = pre_rect.right();
            let hint_rect = if has_hint {
                let r = Rect::new(
                    cx + m.hint_gap,
                    y,
                    hint_w[i] + m.hint_padding_x * 2.0,
                    m.line_height,
                );
                cx = r.right() + m.hint_gap;
                r
            } else {
                Rect::new(cx, y, 0.0, 0.0)
            };
            let post_rect = Rect::new(cx, y, post_w, m.line_height);
            cx = post_rect.right();
            let hint = has_hint.then(|| cand.hint.clone()).flatten();
            (pre_str, pre_rect, post_str, post_rect, hint, hint_rect)
        } else {
            let text_rect = Rect::new(cx, y, widths[i], m.line_height);
            cx = text_rect.right();
            let hint_rect = if has_hint {
                let r = Rect::new(
                    cx + m.hint_gap,
                    y,
                    hint_w[i] + m.hint_padding_x * 2.0,
                    m.line_height,
                );
                cx = r.right();
                r
            } else {
                Rect::new(cx, y, 0.0, 0.0)
            };
            let hint = has_hint.then(|| cand.hint.clone()).flatten();
            (
                cand.text.clone(),
                text_rect,
                String::new(),
                Rect::new(0.0, 0.0, 0.0, 0.0),
                hint,
                hint_rect,
            )
        };

        // emoji 排在最后（词尾 / chip 之后）。被压窄丢掉时 `emoji_w` 是 0，
        // 这里自然一个矩形都不给——与「本来就没挂」在布局上完全等价（验收 ⑥）。
        let (emoji, emoji_rect) = match &cand.emoji {
            Some(e) if emoji_w[i] > 0.0 => (
                Some(e.clone()),
                Rect::new(cx + m.emoji_gap, y, emoji_w[i], m.line_height),
            ),
            _ => (None, Rect::new(cx, y, 0.0, 0.0)),
        };

        let index = input.page_start + i;
        items.push(ItemBox {
            index,
            // 序号是本页内的 1..=5。一页超过 9 条时标签会变成两位数，
            // 调用方应把 page_size 限制在 9 以内（数字键也就 1–9）。
            label: (i + 1).to_string(),
            text,
            text2,
            rect,
            label_rect,
            text_rect,
            text2_rect,
            hint,
            hint_rect,
            emoji,
            emoji_rect,
            elided: clipped && widths[i] < cand.width.max(0.0) - 0.01,
            secondary: input.split.is_some_and(|s| index >= s),
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

fn sum_width(m: &Metrics, widths: &[f32], hint_w: &[f32], emoji_w: &[f32], reserved: f32) -> f32 {
    let items: f32 = widths
        .iter()
        .enumerate()
        .map(|(i, w)| {
            m.item_width_full(
                *w,
                hint_w.get(i).copied().unwrap_or(0.0),
                emoji_w.get(i).copied().unwrap_or(0.0),
            )
        })
        .sum();
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
            split: None,
            metrics: Metrics::new(16.0),
        }
    }

    /// 折叠分界是**全局**序号（D48）：第 3 页的第一条不属于折叠区时，
    /// 不能因为「它是本页第 1 条」就判成主段。
    #[test]
    fn 折叠区按全局序号跨页分界() {
        let page = [cand("好", 16.0), cand("号", 16.0), cand("浩", 16.0)];
        let mut i = input(&page);
        i.page_start = 4;
        i.split = Some(5); // 全局第 5 条起是折叠区 ⇒ 本页第 2 条起
        let out = layout(&i);
        let flags: Vec<bool> = out.items.iter().map(|it| it.secondary).collect();
        assert_eq!(flags, [false, true, true]);
    }

    #[test]
    fn 没有折叠区时全部算主段() {
        let page = [cand("好", 16.0), cand("号", 16.0)];
        let out = layout(&input(&page));
        assert!(out.items.iter().all(|it| !it.secondary));
    }

    /// 分界落在整页之后（= 这一页全是主段）时，一条都不该淡。
    #[test]
    fn 分界在本页之后时不淡任何一条() {
        let page = [cand("好", 16.0), cand("号", 16.0)];
        let mut i = input(&page);
        i.split = Some(9);
        let out = layout(&i);
        assert!(out.items.iter().all(|it| !it.secondary));
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

    // ---- 码提示 chip（§6.3） ----

    /// chip 必须**长在词后面**，且整项宽度把它算进去——否则会盖到隔壁候选上。
    #[test]
    fn chip_排在词右侧并计入整项宽度() {
        let m = Metrics::new(16.0);
        let page = [cand("好", 16.0).with_hint("vbg", 24.0)];
        let out = layout(&input(&page));

        let it = &out.items[0];
        assert_eq!(it.hint.as_deref(), Some("vbg"));
        // 词右边界 + hint_gap 起步
        assert!((it.hint_rect.x - (it.text_rect.right() + m.hint_gap)).abs() < 0.01);
        // chip 宽度 = 文字宽 + 左右内边距
        assert!((it.hint_rect.w - (24.0 + m.hint_padding_x * 2.0)).abs() < 0.01);
        // 整项宽度与「不算 chip」的差正好是 chip 占的那一块
        assert!(
            (it.rect.w - (m.item_width(16.0) + m.hint_gap + 24.0 + m.hint_padding_x * 2.0)).abs()
                < 0.01
        );
        // 词与 chip 不重叠
        assert!(it.text_rect.right() <= it.hint_rect.x);
    }

    #[test]
    fn 没有码时不留空位() {
        let page = [cand("好", 16.0)];
        let mut i = input(&page);
        i.metrics = Metrics::new(16.0);
        let plain = layout(&i);

        let with_empty = [cand("好", 16.0).with_hint("", 30.0)];
        let mut j = input(&with_empty);
        j.metrics = Metrics::new(16.0);
        let empty = layout(&j);

        assert!(plain.items[0].hint.is_none());
        assert_eq!(plain.items[0].hint_rect.w, 0.0);
        assert!(empty.items[0].hint.is_none(), "空串等于不提示");
        assert_eq!(plain.size.w, empty.size.w, "空码不该撑宽窗口");
    }

    /// 压窄时让的是**词**，不是码：码本来就三四个字母，压窄等于把提示弄成不可读。
    #[test]
    fn 超宽压窄不碰_chip() {
        let page = [
            cand("一一一一", 100.0).with_hint("vbg", 24.0),
            cand("二二二二", 100.0).with_hint("kgn", 24.0),
        ];
        let mut i = input(&page);
        i.metrics = Metrics::new(16.0);
        i.metrics.max_width = 260.0;
        let out = layout(&i);

        assert!(out.clipped);
        let m = i.metrics;
        for it in &out.items {
            assert!(
                (it.hint_rect.w - (24.0 + m.hint_padding_x * 2.0)).abs() < 0.01,
                "chip 宽度不变，实际 {}",
                it.hint_rect.w
            );
            assert!(it.text_rect.w < 100.0, "被压窄的是词");
        }
        assert!(out.size.w <= 260.0 + 0.01);
    }

    /// 命中区仍然是**整项**：点在 chip 上也该选中这条，而不是点空。
    #[test]
    fn 点在_chip_上仍命中该候选() {
        let page = [cand("好", 16.0).with_hint("vbg", 24.0)];
        let out = layout(&input(&page));
        let it = &out.items[0];
        assert_eq!(out.hit(it.hint_rect.x + 1.0, it.hint_rect.y + 1.0), Some(0));
    }

    // ---- 词中 chip（D64） ----

    /// chip 嵌在词中间：前截、chip、后截依次排开，互不重叠，
    /// 整项宽度把三块都算进去。
    #[test]
    fn 词中_chip_把词拆成前后两截() {
        let m = Metrics::new(16.0);
        let page = [cand("好吃", 32.0).with_hint_at("vbg", 24.0, 1, 16.0, 16.0)];
        let out = layout(&input(&page));

        let it = &out.items[0];
        assert_eq!(it.text, "好", "前截是 chip 前的字");
        assert_eq!(it.text2, "吃", "后截是 chip 后的字");
        assert!((it.text_rect.w - 16.0).abs() < 0.01);
        assert!((it.hint_rect.w - (24.0 + m.hint_padding_x * 2.0)).abs() < 0.01);
        assert!((it.text2_rect.w - 16.0).abs() < 0.01);
        // 顺序与前距
        assert!((it.hint_rect.x - (it.text_rect.right() + m.hint_gap)).abs() < 0.01);
        assert!((it.text2_rect.x - (it.hint_rect.right() + m.hint_gap)).abs() < 0.01);
        // 整项宽度 = 无 chip 基线 + 前后两截 + chip 一块（几何上 chip 前后各有一个
        // hint_gap，但宽度公式与词尾 chip 同型：`item_width_with_hint` 只算一个 gap
        // ——「后截贴着 chip 走」与「chip 贴着词尾走」占的横向空间是一样的）。
        assert!(
            (it.rect.w
                - (m.item_width(0.0) + 16.0 + 16.0 + m.hint_gap + 24.0 + m.hint_padding_x * 2.0))
                .abs()
                < 0.01
        );
        // 命中测试落在后截上也算这条
        assert_eq!(
            out.hit(it.text2_rect.x + 1.0, it.text2_rect.y + 1.0),
            Some(0)
        );
    }

    /// `hint_at = 0` 即使给了前后宽度也按词尾 chip 画（不拆词）。
    #[test]
    fn hint_at_为_0_时不拆词() {
        let page = [cand("好吃", 32.0).with_hint_at("vbg", 24.0, 0, 16.0, 16.0)];
        let out = layout(&input(&page));
        let it = &out.items[0];
        assert_eq!(it.text, "好吃");
        assert!(it.text2.is_empty());
        assert!((it.text_rect.w - 32.0).abs() < 0.01, "整串一起画");
    }

    /// 压窄时词中 chip 的候选前后两截同比例缩，chip 不挪窝。
    #[test]
    fn 词中_chip_压窄时前后两截同比例缩() {
        let m = Metrics::new(16.0);
        let page = [
            cand("一一一一", 100.0),
            cand("好吃", 100.0).with_hint_at("vbg", 24.0, 1, 50.0, 50.0),
        ];
        let mut i = input(&page);
        i.metrics.max_width = 200.0;
        let out = layout(&i);
        assert!(out.clipped);
        let it = &out.items[1];
        assert!(
            it.text_rect.w < 50.0 && it.text2_rect.w < 50.0,
            "两截都被压"
        );
        assert!(
            (it.text_rect.w - it.text2_rect.w).abs() < 0.01,
            "两截等比例：{} vs {}",
            it.text_rect.w,
            it.text2_rect.w
        );
        assert!(
            (it.hint_rect.w - (24.0 + m.hint_padding_x * 2.0)).abs() < 0.01,
            "chip 不压"
        );
    }

    // ---- emoji（§6.4 / D66） ----

    /// emoji 排在词尾，计入整项宽度——否则会盖到隔壁候选上。
    #[test]
    fn emoji_排在词尾并计入整项宽度() {
        let m = Metrics::new(16.0);
        let page = [cand("好评", 32.0).with_emoji("👍", 18.0)];
        let out = layout(&input(&page));

        let it = &out.items[0];
        assert_eq!(it.emoji.as_deref(), Some("👍"));
        assert!((it.emoji_rect.x - (it.text_rect.right() + m.emoji_gap)).abs() < 0.01);
        assert!((it.emoji_rect.w - 18.0).abs() < 0.01);
        assert!(
            (it.rect.w - (m.item_width(32.0) + m.emoji_gap + 18.0)).abs() < 0.01,
            "整项宽 = v6 基线 + emoji 一块"
        );
        assert!(it.text_rect.right() <= it.emoji_rect.x);
        // 点在 emoji 上也算选中这条
        assert_eq!(
            out.hit(it.emoji_rect.x + 1.0, it.emoji_rect.y + 1.0),
            Some(0)
        );
    }

    /// emoji 在 chip 之后：`好评 vbyg 👍`，不能插在词与码之间。
    #[test]
    fn emoji_排在_chip_之后() {
        let m = Metrics::new(16.0);
        let page = [cand("好评", 32.0)
            .with_hint("vbyg", 24.0)
            .with_emoji("👍", 18.0)];
        let out = layout(&input(&page));
        let it = &out.items[0];
        assert!((it.hint_rect.x - (it.text_rect.right() + m.hint_gap)).abs() < 0.01);
        assert!(
            (it.emoji_rect.x - (it.hint_rect.right() + m.emoji_gap)).abs() < 0.01,
            "emoji 应排在 chip 右侧"
        );
    }

    /// 验收 ⑥：没有 emoji 时布局与 v6 **逐字段一致**——空串 / `None` / 宽度 0
    /// 三条路都必须完全等价，否则关掉 emoji 会悄悄改变行宽。
    #[test]
    fn 没有_emoji_时布局与_v6_完全一致() {
        let plain = [cand("好评", 32.0), cand("好评如潮", 64.0)];
        let base = layout(&input(&plain));
        for it in &base.items {
            assert!(it.emoji.is_none());
            assert_eq!(it.emoji_rect.w, 0.0);
        }

        let empty = [
            cand("好评", 32.0).with_emoji("", 18.0),
            cand("好评如潮", 64.0).with_emoji("", 24.0),
        ];
        assert_eq!(layout(&input(&empty)), base, "空串等于不挂");

        let zero = [
            cand("好评", 32.0).with_emoji("👍", 0.0),
            cand("好评如潮", 64.0).with_emoji("👍", 0.0),
        ];
        assert_eq!(layout(&input(&zero)), base, "宽度 0 等于量不出、等于不画");
    }

    /// 验收 ①：超宽时**先丢 emoji、不压词**。emoji 是装饰、词是内容。
    #[test]
    fn 超宽先丢_emoji_不压词() {
        let plain = [cand("好评", 32.0)];
        let with = [cand("好评", 32.0).with_emoji("👍", 18.0)];
        let natural0 = layout(&input(&plain));
        let natural1 = layout(&input(&with));
        assert!(natural1.size.w > natural0.size.w, "emoji 确实占了宽度");

        // 上限只比「带 emoji 的自然宽」少 1px：丢了 emoji 就够，不该动词。
        let mut i = input(&with);
        i.metrics.max_width = natural1.size.w - 1.0;
        let out = layout(&i);
        assert!(out.items[0].emoji.is_none(), "emoji 应被丢掉");
        assert!(
            (out.items[0].text_rect.w - 32.0).abs() < 0.01,
            "词一点都不能压，实际 {}",
            out.items[0].text_rect.w
        );
        assert!(!out.clipped, "没压词就不算 clipped（不出省略号）");
        assert!((out.size.w - natural0.size.w).abs() < 0.01);
    }

    /// 丢了 emoji 还超宽，才轮到压词——顺序不能反。
    #[test]
    fn 丢了_emoji_仍超宽才压词() {
        let page = [
            cand("一一一一", 100.0).with_emoji("👍", 18.0),
            cand("二二二二", 100.0).with_emoji("🎉", 18.0),
        ];
        let mut i = input(&page);
        i.metrics.max_width = 200.0;
        let out = layout(&i);
        assert!(
            out.items.iter().all(|it| it.emoji.is_none()),
            "emoji 必须全部先丢"
        );
        assert!(out.clipped, "还超宽才压词");
        assert!(out.items.iter().all(|it| it.text_rect.w < 100.0));
        assert!(out.size.w <= 200.0 + 0.01);
    }
}
