//! 码位键位图的几何（PROJECT.md §6.7 / D50）。
//!
//! 它回答的是「这几个字母在键盘上属于哪个键区、那个键上有哪些字根」，
//! **不是**「这个字是怎么拆的」——后者要笔顺与字型数据，R3 已证伪。
//!
//! 与 [`crate::layout`] 同一套分层：本模块只做算术，字体度量由调用方用
//! DirectWrite 量好传进来（[`KeycapText`] 里的各种 `width`）。于是这里的每一条
//! 断言都能在任意平台上跑单测。
//!
//! ## 形状为什么是「大键帽」而不是「键盘示意图」
//!
//! v1.21 定的是只画**用到的 3–4 枚**键帽，不画整块 25 键键盘：整块键盘要占满
//! 半个屏幕、挡住文档，为「看方位」付这个代价不值（§6.7 的「明确不做」表）。
//!
//! 每枚键帽内部自上而下是：字母 → 字根（每行三枚，自动折行）。
//! 字根包含变体（氵 扌 灬 忄 等），变体由绘制端画淡一档——几何不关心颜色，
//! 只按传入顺序排。
//!
//! ## 为什么不再画一级简码
//!
//! v1.28 之前每枚键帽底部还有一行「该键的一级简码」（`H` = 上、`A` = 工）。
//! 真机一看就废：它回答的是「这个键单独一按出什么字」，与「这个字由哪些字根拼成」
//! 毫无关系，放在字根正下方只会被读成「这个字在这一位取的是上」。用户的原话是
//! 「这行没什么意义」——去掉的不是一条信息，而是一条**会被误读的信息**。
//! 真正的逐字拆解要字根级拆解数据，R3 已证伪，所以这里也不打算换个说法糊上去。

use crate::geom::{Rect, Size};
use crate::place::Placement;

/// 一枚键帽要画的内容，宽度都已量好。
#[derive(Clone, Debug, PartialEq)]
pub struct KeycapText {
    /// 字母键，`a`–`y`。
    pub key: char,
    /// 字母的宽度。
    pub key_width: f32,
    /// 该键的全部字根（主字根在前、变体在后），顺序即绘制顺序。
    pub roots: Vec<char>,
    /// 与 `roots` 一一对应的宽度。
    pub root_widths: Vec<f32>,
    /// `roots` 里前几枚是**主字根**，其余是变体。绘制端据此把变体画淡一档。
    ///
    /// 默认等于 `roots.len()`（全部当主字根）——把变体当主字根只是不够淡，
    /// 反过来会把正经字根画淡，两害相权取前者。
    pub main_root_count: usize,
}

impl KeycapText {
    /// 从字符序列与宽度表构造；宽度表长度不足时按 0 补齐，多余部分忽略。
    pub fn new(key: char, key_width: f32, roots: Vec<char>, root_widths: Vec<f32>) -> Self {
        let mut widths = root_widths;
        widths.resize(roots.len(), 0.0);
        let main_root_count = roots.len();
        Self {
            key,
            key_width,
            roots,
            root_widths: widths,
            main_root_count,
        }
    }

    /// 声明前 `n` 枚是主字根，其余是变体（超出范围时夹紧）。
    pub fn with_main_roots(mut self, n: usize) -> Self {
        self.main_root_count = n.min(self.roots.len());
        self
    }
}

/// 键帽图的视觉参数。唯一自由变量同样是字号，其余按比例推导。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeycapMetrics {
    /// 字母字号。
    pub font_size: f32,
    pub key_line_height: f32,
    /// 字根字号（比字母小一档）。
    pub root_size: f32,
    pub root_line_height: f32,
    /// 键帽内的左右内边距。
    pub padding_x: f32,
    /// 键帽内的上下内边距。
    pub padding_y: f32,
    /// 相邻两枚键帽之间。
    pub cap_gap: f32,
    /// 键帽内部「字母 / 字根块 / 简码」之间。
    pub inner_gap: f32,
    /// 同一行两个相邻字根之间。
    pub root_gap: f32,
    pub corner_radius: f32,
    /// 每行排几个字根。
    ///
    /// 三个是**在 200% DPI 真机上比出来的**：两个会把键帽拉成 80×256 的长条
    /// （八画的 q/n 要四行，加简码就超过二倍高），四个又让「雨」「鼎」这类宽字
    /// 挤在一起。三个落在 106×222（约 1:2），读起来才像一枚键帽而不是一列清单。
    pub roots_per_line: usize,
    pub footnote_size: f32,
    pub footnote_line_height: f32,
    /// 键帽块与说明行之间。
    pub footnote_gap: f32,
    /// 键帽最小宽度。只有一个短字根时（`K` = 口川）不至于缩成一条。
    pub min_cap_width: f32,
}

impl KeycapMetrics {
    pub fn new(font_size: f32) -> Self {
        let font_size = font_size.max(8.0);
        let root_size = (font_size * 0.68).round().max(8.0);
        Self {
            font_size,
            key_line_height: (font_size * 1.45).round(),
            root_size,
            root_line_height: (root_size * 1.5).round(),
            padding_x: (font_size * 0.50).round(),
            padding_y: (font_size * 0.35).round(),
            cap_gap: (font_size * 0.35).round(),
            inner_gap: (font_size * 0.30).round(),
            root_gap: (font_size * 0.12).round(),
            corner_radius: (font_size * 0.30).round(),
            roots_per_line: 3,
            footnote_size: (font_size * 0.62).round().max(8.0),
            footnote_line_height: (font_size * 0.80).round(),
            footnote_gap: (font_size * 0.40).round(),
            // 2.2 个字宽：最窄的键（`K` = 口川，两个窄字根）也放得下两列，
            // 再大就会让「内容只有 80px、框却有 102px」的空框效应变得显眼。
            min_cap_width: (font_size * 2.2).round(),
        }
    }

    pub fn scaled(self, scale: f32) -> Self {
        let s = if scale > 0.0 { scale } else { 1.0 };
        Self {
            font_size: self.font_size * s,
            key_line_height: self.key_line_height * s,
            root_size: self.root_size * s,
            root_line_height: self.root_line_height * s,
            padding_x: self.padding_x * s,
            padding_y: self.padding_y * s,
            cap_gap: self.cap_gap * s,
            inner_gap: self.inner_gap * s,
            root_gap: self.root_gap * s,
            corner_radius: self.corner_radius * s,
            roots_per_line: self.roots_per_line,
            footnote_size: self.footnote_size * s,
            footnote_line_height: self.footnote_line_height * s,
            footnote_gap: self.footnote_gap * s,
            min_cap_width: self.min_cap_width * s,
        }
    }
}

/// 一次布局的输入。
#[derive(Clone, Debug)]
pub struct KeycapInput<'a> {
    /// 要画的键帽，从左到右。
    pub caps: &'a [KeycapText],
    /// 底部固定说明的行宽。**调用方负责断行与量宽**——几何层不做字体度量，
    /// 于是也没法按字符边界换行。空数组 = 不画说明。
    pub footnote_widths: &'a [f32],
    pub metrics: KeycapMetrics,
}

/// 一枚画出来的键帽。
#[derive(Clone, Debug, PartialEq)]
pub struct KeycapBox {
    pub key: char,
    pub roots: Vec<char>,
    /// 前几枚是主字根，其余是变体（绘制端据此把变体画淡一档）。
    pub main_root_count: usize,
    /// 键帽矩形（画圆角底与边）。
    pub rect: Rect,
    pub key_rect: Rect,
    /// 字根矩形，与 `roots` 一一对应。
    pub root_rects: Vec<Rect>,
}

/// 布局结果。
#[derive(Clone, Debug, PartialEq)]
pub struct KeycapLayout {
    pub size: Size,
    pub caps: Vec<KeycapBox>,
    /// 说明行矩形，顺序即绘制顺序。
    pub footnotes: Vec<Rect>,
}

impl KeycapLayout {
    pub fn is_empty(&self) -> bool {
        self.caps.is_empty()
    }
}

/// 算键位图布局。
pub fn layout_keycaps(input: &KeycapInput<'_>) -> KeycapLayout {
    let m = input.metrics;
    let per = m.roots_per_line.max(1);
    let n = input.caps.len();
    if n == 0 {
        return KeycapLayout {
            size: Size::default(),
            caps: Vec::new(),
            footnotes: Vec::new(),
        };
    }

    // 每枚键帽的内容宽：字母 / 每行字根两者取最大
    let cap_width = input
        .caps
        .iter()
        .map(|c| {
            let mut inner = c.key_width.max(0.0);
            for chunk in c.root_widths.chunks(per) {
                let row: f32 =
                    chunk.iter().sum::<f32>() + m.root_gap * chunk.len().saturating_sub(1) as f32;
                inner = inner.max(row);
            }
            (inner + m.padding_x * 2.0).max(m.min_cap_width)
        })
        .fold(0.0, f32::max);

    // 高度按字根最多的那枚算，全部键帽等高（像键盘，也免得逐枚错位）
    let max_rows = input
        .caps
        .iter()
        .map(|c| c.root_widths.len().div_ceil(per))
        .max()
        .unwrap_or(0);
    let cap_h =
        m.padding_y * 2.0 + m.key_line_height + m.inner_gap + max_rows as f32 * m.root_line_height;

    let caps_w = n as f32 * cap_width + (n - 1) as f32 * m.cap_gap;
    let footnote_h = if input.footnote_widths.is_empty() {
        0.0
    } else {
        m.footnote_gap + input.footnote_widths.len() as f32 * m.footnote_line_height
    };
    let footnote_w = input.footnote_widths.iter().copied().fold(0.0, f32::max);

    let width = (caps_w.max(footnote_w) + m.padding_x * 2.0).max(m.min_cap_width);
    let height = m.padding_y * 2.0 + cap_h + footnote_h;

    // 键帽块在窗口内水平居中：说明行比键帽宽时（很常见，固定说明有三十来字），
    // 若键帽仍贴左缘，右边会空出一大块，看起来像布局没算完。
    let content_w = width - m.padding_x * 2.0;
    let left = m.padding_x + (content_w - caps_w).max(0.0) / 2.0;
    let mut caps = Vec::with_capacity(n);
    for (i, c) in input.caps.iter().enumerate() {
        let x = left + i as f32 * (cap_width + m.cap_gap);
        let rect = Rect::new(x, m.padding_y, cap_width, cap_h);

        let key_rect = Rect::new(
            x + (cap_width - c.key_width.max(0.0)) / 2.0,
            rect.y + m.padding_y,
            c.key_width.max(0.0),
            m.key_line_height,
        );

        let mut root_rects = Vec::with_capacity(c.roots.len());
        let own_rows = c.root_widths.len().div_ceil(per);
        // 字根块在自己的行数范围内**垂直居中**：键帽等高（`max_rows` 决定），但
        // `K` 这种只有两个字的键会空出三行。留白全压在中间最刺眼——它会让人以为
        // 「这个键的字根还没列完」。分到上下两侧就只是正常的边上留白。
        let slack = (max_rows.saturating_sub(own_rows)) as f32 * m.root_line_height;
        for (row, chunk) in c.root_widths.chunks(per).enumerate() {
            let row_w: f32 =
                chunk.iter().sum::<f32>() + m.root_gap * chunk.len().saturating_sub(1) as f32;
            let mut cx = x + (cap_width - row_w) / 2.0;
            let cy = rect.y
                + m.padding_y
                + m.key_line_height
                + m.inner_gap
                + slack * 0.5
                + row as f32 * m.root_line_height;
            for w in chunk {
                root_rects.push(Rect::new(cx, cy, *w, m.root_line_height));
                cx += w + m.root_gap;
            }
        }

        caps.push(KeycapBox {
            key: c.key,
            roots: c.roots.clone(),
            main_root_count: c.main_root_count,
            rect,
            key_rect,
            root_rects,
        });
    }

    let mut footnotes = Vec::with_capacity(input.footnote_widths.len());
    let mut fy = m.padding_y + cap_h + m.footnote_gap;
    for w in input.footnote_widths {
        // 左对齐到内容区左缘：说明是连贯的一两句，右对齐会让每行左缘参差，
        // 读起来像两段互不相干的话。
        footnotes.push(Rect::new(m.padding_x, fy, *w, m.footnote_line_height));
        fy += m.footnote_line_height;
    }

    KeycapLayout {
        size: Size::new(width, height),
        caps,
        footnotes,
    }
}

/// 键位图的落点：**贴候选窗**，不是贴光标。
///
/// `anchor` 是候选窗矩形。默认贴在它下方；下方放不下才翻到上方。若候选窗本身
/// 已经翻到了光标上方（`prefer_above`），键位图也往上走——否则它会压在光标和
/// 正文之间，正好挡住用户在看的那一行。
pub fn place_keycaps(
    anchor: Rect,
    size: Size,
    work: Rect,
    gap: f32,
    prefer_above: bool,
) -> Placement {
    let gap = gap.max(0.0);
    let w = size.w.max(0.0);
    let h = size.h.max(0.0);
    let below = anchor.bottom() + gap;
    let above = anchor.y - gap - h;

    let try_below = below + h <= work.bottom();
    let try_above = above >= work.y;

    let (mut y, flipped) = if prefer_above && try_above {
        (above, true)
    } else if try_below {
        (below, false)
    } else if try_above {
        (above, true)
    } else {
        // 两侧都放不下：留在下方，靠下面的夹紧贴顶（与 `place` 同一取舍）
        (below, false)
    };

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

    fn cap(key: char, roots: &[char]) -> KeycapText {
        KeycapText::new(
            key,
            9.0,
            roots.to_vec(),
            roots.iter().map(|_| 12.0).collect(),
        )
    }

    fn sample() -> Vec<KeycapText> {
        vec![
            cap('v', &['女', '刀', '九', '臼', '彐', '巛']),
            cap('b', &['子', '耳', '了', '也', '凵', '卩', '阝']),
            cap('g', &['王', '一', '五', '戋']),
        ]
    }

    fn input_with(caps: &[KeycapText]) -> KeycapInput<'_> {
        KeycapInput {
            caps,
            footnote_widths: &[],
            metrics: KeycapMetrics::new(14.0),
        }
    }

    #[test]
    fn 三枚键帽横排不重叠且都在窗口内() {
        let caps = sample();
        let l = layout_keycaps(&input_with(&caps));
        assert_eq!(l.caps.len(), 3);
        for w in l.caps.windows(2) {
            assert!(w[0].rect.right() <= w[1].rect.x, "键帽之间不该重叠");
        }
        for c in &l.caps {
            assert!(c.rect.x >= 0.0);
            assert!(c.rect.right() <= l.size.w + 0.01);
            assert!(c.rect.bottom() <= l.size.h + 0.01);
        }
    }

    #[test]
    fn 全部键帽等高且宽度一致() {
        let caps = sample();
        let l = layout_keycaps(&input_with(&caps));
        let h = l.caps[0].rect.h;
        let w = l.caps[0].rect.w;
        for c in &l.caps {
            assert!((c.rect.h - h).abs() < 0.01);
            assert!((c.rect.w - w).abs() < 0.01);
        }
    }

    #[test]
    fn 字根按每行三个折行() {
        let caps = sample();
        let l = layout_keycaps(&input_with(&caps));
        let v = &l.caps[0];
        assert_eq!(v.root_rects.len(), 6);
        // 6 个 → 2 行：前三个同一行（y 相同），第四个换行
        let same_row = (v.root_rects[0].y - v.root_rects[2].y).abs() < 0.01;
        assert!(same_row, "前三个应在同一行");
        assert!(v.root_rects[3].y > v.root_rects[2].y, "第四个应换行");
    }

    #[test]
    fn 字根不越出键帽() {
        let caps = sample();
        let l = layout_keycaps(&input_with(&caps));
        for c in &l.caps {
            for r in &c.root_rects {
                assert!(r.bottom() <= c.rect.bottom() + 0.01);
                // 字母在全部字根之上
                assert!(c.key_rect.bottom() <= r.y + 0.01);
            }
        }
    }

    #[test]
    fn 说明行在键帽下方并按行堆叠() {
        let caps = sample();
        let input = KeycapInput {
            caps: &caps,
            footnote_widths: &[180.0, 120.0],
            metrics: KeycapMetrics::new(14.0),
        };
        let l = layout_keycaps(&input);
        assert_eq!(l.footnotes.len(), 2);
        let cap_bottom = l.caps[0].rect.bottom();
        assert!(l.footnotes[0].y > cap_bottom);
        assert!(l.footnotes[1].y > l.footnotes[0].y);
        assert!(l.footnotes[1].bottom() <= l.size.h + 0.01);
    }

    #[test]
    fn 说明行更宽时窗口跟着变宽() {
        let caps = sample();
        let narrow = layout_keycaps(&KeycapInput {
            caps: &caps,
            footnote_widths: &[10.0],
            metrics: KeycapMetrics::new(14.0),
        });
        let wide = layout_keycaps(&KeycapInput {
            caps: &caps,
            footnote_widths: &[900.0],
            metrics: KeycapMetrics::new(14.0),
        });
        assert!(wide.size.w > narrow.size.w);
        assert!(wide.size.w >= 900.0);
    }

    #[test]
    fn 只有一个字根时用最小宽度() {
        let caps = vec![cap('k', &['口', '川'])];
        let l = layout_keycaps(&input_with(&caps));
        let m = KeycapMetrics::new(14.0);
        assert!(l.caps[0].rect.w >= m.min_cap_width);
    }

    #[test]
    fn 主字根与变体的分界随数据走() {
        let caps = vec![KeycapText::new(
            'g',
            9.0,
            vec!['王', '一', '五', '戋', '龶', '丰'],
            vec![12.0; 6],
        )
        .with_main_roots(4)];
        let l = layout_keycaps(&input_with(&caps));
        assert_eq!(l.caps[0].main_root_count, 4);
        assert_eq!(l.caps[0].roots.len(), 6);

        // 不声明时默认全部当主字根（宁可不淡，也不把正经字根画淡）
        let plain = cap('v', &['女', '刀']);
        assert_eq!(plain.main_root_count, 2);
        // 超出范围夹紧
        assert_eq!(plain.clone().with_main_roots(9).main_root_count, 2);
    }

    #[test]
    fn 说明行更宽时键帽块居中() {
        let caps = sample();
        let l = layout_keycaps(&KeycapInput {
            caps: &caps,
            footnote_widths: &[600.0],
            metrics: KeycapMetrics::new(14.0),
        });
        let first = l.caps.first().unwrap().rect;
        let last = l.caps.last().unwrap().rect;
        let left_gap = first.x;
        let right_gap = l.size.w - last.right();
        assert!(
            (left_gap - right_gap).abs() < 0.5,
            "左右留白应当相等：{left_gap} vs {right_gap}"
        );
    }

    #[test]
    fn 空输入不panic() {
        let l = layout_keycaps(&KeycapInput {
            caps: &[],
            footnote_widths: &[],
            metrics: KeycapMetrics::new(14.0),
        });
        assert!(l.is_empty());
        assert_eq!(l.size, Size::default());
    }

    #[test]
    fn dpi缩放按比例() {
        // 真实场景里字宽也由 DWrite 按物理像素量，150% 下就是 1.5 倍。所以缩放
        // 测试必须把字宽一起放大，否则量的是「框放大、字没放大」这个假想情形。
        let caps = sample();
        let scaled: Vec<KeycapText> = caps
            .iter()
            .map(|c| KeycapText {
                key_width: c.key_width * 1.5,
                root_widths: c.root_widths.iter().map(|w| w * 1.5).collect(),
                ..c.clone()
            })
            .collect();

        let a = layout_keycaps(&input_with(&caps));
        let b = layout_keycaps(&KeycapInput {
            caps: &scaled,
            footnote_widths: &[],
            metrics: KeycapMetrics::new(14.0).scaled(1.5),
        });
        assert!(
            (b.size.w / a.size.w - 1.5).abs() < 0.05,
            "宽 {} → {}",
            a.size.w,
            b.size.w
        );
        assert!(
            (b.size.h / a.size.h - 1.5).abs() < 0.05,
            "高 {} → {}",
            a.size.h,
            b.size.h
        );
    }

    fn work() -> Rect {
        Rect::new(0.0, 0.0, 1920.0, 1040.0)
    }

    fn cand() -> Rect {
        Rect::new(400.0, 500.0, 300.0, 36.0)
    }

    #[test]
    fn 键位图贴候选窗下沿并左对齐() {
        let p = place_keycaps(cand(), Size::new(320.0, 140.0), work(), 4.0, false);
        assert!(!p.flipped);
        assert_eq!(p.rect.x, 400.0);
        assert_eq!(p.rect.y, 540.0);
    }

    #[test]
    fn 下方放不下翻到候选窗上方() {
        let near_bottom = Rect::new(400.0, 950.0, 300.0, 36.0);
        let p = place_keycaps(near_bottom, Size::new(320.0, 140.0), work(), 4.0, false);
        assert!(p.flipped);
        assert_eq!(p.rect.y, 950.0 - 4.0 - 140.0);
        assert!(p.rect.bottom() <= near_bottom.y);
    }

    #[test]
    fn 候选窗已在上方时键位图继续往上() {
        // 即便下方放得下，prefer_above 也要求往上，避免压住光标那一行
        let above_cand = Rect::new(400.0, 300.0, 300.0, 36.0);
        let p = place_keycaps(above_cand, Size::new(320.0, 140.0), work(), 4.0, true);
        assert!(p.flipped);
        assert_eq!(p.rect.y, 300.0 - 4.0 - 140.0);
    }

    #[test]
    fn 上方也放不下时退回下方() {
        let top_cand = Rect::new(400.0, 20.0, 300.0, 36.0);
        let p = place_keycaps(top_cand, Size::new(320.0, 140.0), work(), 4.0, true);
        assert!(!p.flipped);
        assert_eq!(p.rect.y, 20.0 + 36.0 + 4.0);
    }

    #[test]
    fn 右边越界时右对齐到工作区() {
        let cand = Rect::new(1800.0, 500.0, 300.0, 36.0);
        let p = place_keycaps(cand, Size::new(320.0, 140.0), work(), 4.0, false);
        assert_eq!(p.rect.right(), 1920.0);
    }

    #[test]
    fn 输出整数像素() {
        let p = place_keycaps(
            Rect::new(400.4, 500.7, 300.2, 36.4),
            Size::new(320.6, 140.3),
            work(),
            3.7,
            false,
        );
        for v in [p.rect.x, p.rect.y, p.rect.w, p.rect.h] {
            assert_eq!(v.fract(), 0.0, "应为整数：{v}");
        }
    }
}
