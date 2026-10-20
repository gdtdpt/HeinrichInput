//! 把布局结果画成 SVG：**在没有窗口的情况下核对几何**。
//!
//! ```text
//! cargo run -q -p heinrich-ui --example geometry > candidate-geometry.svg
//! ```
//!
//! 画的是 `layout()` 吐出来的矩形，不是最终渲染——文字位置由 SVG 自己排，
//! 所以它验的是「框对不对、间距对不对、页码会不会压住候选」，不验字形。

use heinrich_ui::{layout, CandidateText, LayoutInput, Metrics};

const FONT: &str = "'Microsoft YaHei','Noto Sans CJK SC',sans-serif";
const INK: &str = "#222";
const MUTED: &str = "#8a8a8a";
const BG: &str = "#ffffff";
const BORDER: &str = "#c9c9c9";
const HILITE: &str = "#dbeafe";
const HITBOX: &str = "#e11d48";

fn main() {
    // `vbg` 在真词库里只有 2 个候选，这里凑 5 条来验满页布局。
    let texts = ["好", "㛅", "崽", "孬", "妳"];
    let page: Vec<CandidateText> = texts.iter().map(|t| CandidateText::new(*t, 16.0)).collect();

    let mut body = String::new();
    let mut y = 70.0;

    y += panel(
        &mut body,
        y,
        "① 100% DPI（字号 16px）· 5 条候选 · 第 1/3 页",
        1.0,
        &page,
        Some(0),
        true,
    );
    y += panel(
        &mut body,
        y,
        "② 150% DPI（字号 24px）· 同一份内容",
        1.5,
        &page,
        Some(0),
        true,
    );
    // 3 条 + 页码：验「窄窗口时页码贴右端且不压住候选」
    y += panel(
        &mut body,
        y,
        "③ 只有 3 条候选时，min_width 与页码的位置",
        1.0,
        &page[..3],
        Some(2),
        true,
    );

    let width = 1100.0;
    println!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{y}" viewBox="0 0 {width} {y}" font-family="{FONT}">
<rect width="{width}" height="{y}" fill="#f5f5f5"/>
<text x="40" y="34" font-size="18" fill="{INK}">heinrich-ui 候选窗几何（矩形来自 layout()，虚线是鼠标命中区）</text>
{body}</svg>"##
    );
}

/// 画一块面板，返回它占的高度。
fn panel(
    out: &mut String,
    y0: f32,
    title: &str,
    scale: f32,
    page: &[CandidateText],
    highlight: Option<usize>,
    with_page_label: bool,
) -> f32 {
    let m = Metrics::new(16.0).scaled(scale);
    let label_width = m.font_size * 1.7;
    let page_label = with_page_label.then_some(("1/3", label_width));
    let scaled: Vec<CandidateText> = page
        .iter()
        .map(|c| CandidateText::new(c.text.clone(), c.width * scale))
        .collect();

    let l = layout(&LayoutInput {
        page: &scaled,
        page_start: 25,
        page_index: 0,
        page_count: 3,
        highlight,
        page_label,
        metrics: m,
    });

    let x0 = 40.0;
    let y0 = y0 + 26.0;

    out.push_str(&format!(
        r#"<text x="{x0}" y="{:.0}" font-size="14" fill="{INK}">{title}</text>"#,
        y0 - 10.0
    ));

    // 窗口
    out.push_str(&format!(
        r#"<rect x="{x0}" y="{y0}" width="{}" height="{}" rx="{}" fill="{BG}" stroke="{BORDER}"/>"#,
        l.size.w, l.size.h, m.corner_radius
    ));

    // 高亮块
    if let Some(h) = highlight.and_then(|h| l.items.get(h)) {
        out.push_str(&format!(
            r#"<rect x="{}" y="{}" width="{}" height="{}" rx="{}" fill="{HILITE}"/>"#,
            x0 + h.rect.x - 1.0,
            y0 + h.rect.y,
            h.rect.w + 2.0,
            h.rect.h,
            m.corner_radius * 0.8
        ));
    }

    for it in &l.items {
        // 命中区（M2.3 点选就用这个矩形）
        out.push_str(&format!(
            r#"<rect x="{}" y="{}" width="{}" height="{}" fill="none" stroke="{HITBOX}" stroke-width="0.6" stroke-dasharray="3 3" opacity="0.5"/>"#,
            x0 + it.rect.x,
            y0 + it.rect.y,
            it.rect.w,
            it.rect.h
        ));
        // 序号
        out.push_str(&format!(
            r#"<text x="{}" y="{}" font-size="{}" fill="{MUTED}" text-anchor="middle">{}</text>"#,
            x0 + it.label_rect.x + it.label_rect.w / 2.0,
            y0 + it.text_rect.y + m.line_height * 0.76,
            m.font_size * 0.8,
            it.label
        ));
        // 词
        out.push_str(&format!(
            r#"<text x="{}" y="{}" font-size="{}" fill="{INK}">{}</text>"#,
            x0 + it.text_rect.x,
            y0 + it.text_rect.y + m.line_height * 0.78,
            m.font_size,
            it.text
        ));
    }

    if let Some(pl) = &l.page_label {
        out.push_str(&format!(
            r#"<text x="{}" y="{}" font-size="{}" fill="{MUTED}" text-anchor="end">{}</text>"#,
            x0 + pl.rect.right(),
            y0 + pl.rect.y + m.line_height * 0.76,
            m.font_size * 0.75,
            pl.text
        ));
    }

    // 尺寸标注
    out.push_str(&format!(
        r#"<text x="{x0}" y="{:.0}" font-size="11" fill="{MUTED}">窗口 {} × {}　行高 {}　候选间距 {}　圆角 {}</text>"#,
        y0 + l.size.h + 16.0,
        l.size.w,
        l.size.h,
        m.line_height,
        m.item_gap,
        m.corner_radius
    ));

    l.size.h + 26.0 + 34.0
}
