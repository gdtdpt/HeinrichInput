//! 把布局结果画成 SVG：**在没有窗口的情况下核对几何**。
//!
//! ```text
//! cargo run -q -p heinrich-ui --example geometry > candidate-geometry.svg
//! ```
//!
//! 画的是 `layout()` 吐出来的矩形，不是最终渲染——文字位置由 SVG 自己排，
//! 所以它验的是「框对不对、间距对不对、箭头会不会压住候选」，不验字形。

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
        "① 100% DPI（字号 16px）· 5 条候选 · 中间页：两端都有箭头",
        1.0,
        &page,
        Some(0),
        (true, true),
    );
    y += panel(
        &mut body,
        y,
        "② 150% DPI（字号 24px）· 同一份内容",
        1.5,
        &page,
        Some(0),
        (true, true),
    );
    // 3 条 + 只有左箭头：验「末页不画右箭头」「箭头不压住候选」
    y += panel(
        &mut body,
        y,
        "③ 只有 3 条候选（末页）：只有左箭头",
        1.0,
        &page[..3],
        Some(2),
        (true, false),
    );
    // 只有一页：两个箭头都不存在，也就不占宽度
    y += panel(
        &mut body,
        y,
        "④ 只有一页：两端都没有箭头，窗口相应变窄",
        1.0,
        &page[..3],
        Some(2),
        (false, false),
    );

    let width = 1100.0;
    println!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{y}" viewBox="0 0 {width} {y}" font-family="{FONT}">
<rect width="{width}" height="{y}" fill="#f5f5f5"/>
<text x="40" y="34" font-size="18" fill="{INK}">heinrich-ui 候选窗几何（矩形来自 layout()，虚线是鼠标命中区；箭头只表示「这个方向还有」）</text>
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
    nav: (bool, bool),
) -> f32 {
    let m = Metrics::new(16.0).scaled(scale);
    let scaled: Vec<CandidateText> = page
        .iter()
        .map(|c| CandidateText::new(c.text.clone(), c.width * scale))
        .collect();

    let l = layout(&LayoutInput {
        page: &scaled,
        page_start: 25,
        has_prev: nav.0,
        has_next: nav.1,
        highlight,
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

    // 两端箭头（虚线框同样是命中区：点箭头翻页，不选词）
    for arrow in [l.prev.as_ref(), l.next.as_ref()].into_iter().flatten() {
        out.push_str(&format!(
            r#"<rect x="{}" y="{}" width="{}" height="{}" fill="none" stroke="{HITBOX}" stroke-width="0.6" stroke-dasharray="3 3" opacity="0.5"/>"#,
            x0 + arrow.rect.x,
            y0 + arrow.rect.y,
            arrow.rect.w,
            arrow.rect.h
        ));
        out.push_str(&format!(
            r#"<text x="{}" y="{}" font-size="{}" fill="{MUTED}" text-anchor="middle">{}</text>"#,
            x0 + arrow.rect.x + arrow.rect.w / 2.0,
            y0 + arrow.rect.y + m.line_height * 0.76,
            m.font_size * 0.85,
            arrow.text
        ));
    }

    // 尺寸标注
    out.push_str(&format!(
        r#"<text x="{x0}" y="{:.0}" font-size="11" fill="{MUTED}">窗口 {} × {}　行高 {}　候选间距 {}　箭头 {}　圆角 {}</text>"#,
        y0 + l.size.h + 16.0,
        l.size.w,
        l.size.h,
        m.line_height,
        m.item_gap,
        m.arrow_width,
        m.corner_radius
    ));

    l.size.h + 26.0 + 34.0
}
