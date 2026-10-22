//! 把键位图的几何画成 SVG：**在没有窗口的情况下核对布局**。
//!
//! ```text
//! cargo run -q -p heinrich-ui --example keycaps > out/keycaps.svg
//! ```
//!
//! 与 `geometry.rs` 同样只验矩形、不验字形。三块场景覆盖三种极端：
//! ① `vbg`（普通码，3 枚键帽）② `q`+`n`（各 8 个字根，最高的情形）
//! ③ 150% DPI + 两行说明。

use heinrich_ui::{layout_keycaps, KeycapInput, KeycapMetrics, KeycapText};

const FONT: &str = "'Microsoft YaHei','Noto Sans CJK SC',sans-serif";
const INK: &str = "#1f2328";
const MUTED: &str = "#9aa0a6";
const BG: &str = "#ffffff";
const CAP_BG: &str = "#f6f7f9";
const BORDER: &str = "#c9ced6";
const HIT: &str = "#e11d48";

/// 粗略量宽：中日韩字符按一个字宽，ASCII 按 0.6 个字宽。
/// 示例只关心「框对不对」，不追求与 DirectWrite 完全一致。
fn width_of(s: &str, size: f32) -> f32 {
    s.chars()
        .map(|c| {
            if (c as u32) < 0x1100 {
                size * 0.6
            } else {
                size
            }
        })
        .sum()
}

fn cap(key: char, roots: &[char], m: KeycapMetrics) -> KeycapText {
    let key_width = width_of(&key.to_string(), m.font_size);
    let widths: Vec<f32> = roots
        .iter()
        .map(|r| width_of(&r.to_string(), m.root_size))
        .collect();
    KeycapText::new(key, key_width, roots.to_vec(), widths)
}

fn main() {
    let mut body = String::new();
    let mut y = 60.0;

    // ① vbg：真实数据（roots.toml）
    let m = KeycapMetrics::new(16.0);
    let caps = vec![
        cap('v', &['女', '刀', '九', '臼', '彐', '巛'], m),
        cap('b', &['子', '耳', '了', '也', '凵', '卩', '阝'], m),
        cap('g', &['王', '一', '五', '戋', '龶', '丰'], m).with_main_roots(4),
    ];
    let note = "不足 4 码时末位可能是末笔识别码——取末笔笔画与字型";
    let note_w = width_of(note, m.footnote_size);
    y = panel(
        &mut body,
        y,
        "① `vbg` · 16px · 带说明行",
        &caps,
        &[note_w],
        m,
        note,
    );

    // ② q 与 n：各 8 个字根，高度与折行的最坏情况
    let caps = vec![
        cap('q', &['金', '勺', '鱼', '犭', '儿', '夕', '乂', '氏'], m),
        cap('n', &['已', '己', '巳', '尸', '心', '羽', '乙', '忄'], m),
        cap('k', &['口', '川'], m),
    ];
    y = panel(
        &mut body,
        y,
        "② `q` / `n`（各 8 个字根）+ `k`（最矮）",
        &caps,
        &[],
        m,
        "",
    );

    // ③ 150% DPI：整套参数乘 1.5，框必须等比
    let m15 = KeycapMetrics::new(16.0).scaled(1.5);
    let caps = vec![
        cap('v', &['女', '刀', '九', '臼', '彐', '巛'], m15),
        cap('b', &['子', '耳', '了', '也', '凵', '卩', '阝'], m15),
        cap('g', &['王', '一', '五', '戋', '龶', '丰'], m15).with_main_roots(4),
    ];
    let note_w = width_of(note, m15.footnote_size);
    y = panel(
        &mut body,
        y,
        "③ 同一内容 · 150% DPI",
        &caps,
        &[note_w],
        m15,
        note,
    );

    // ④ 词组 vbgu：平铺四枚，不标归属
    let caps = vec![
        cap('v', &['女', '刀', '九', '臼', '彐', '巛'], m),
        cap('b', &['子', '耳', '了', '也', '凵', '卩', '阝'], m),
        cap('g', &['王', '一', '五', '戋', '龶', '丰'], m).with_main_roots(4),
        cap('u', &['立', '辛', '丬', '六', '门', '疒', '丷'], m),
    ];
    y = panel(
        &mut body,
        y,
        "④ `vbgu`（词组）· 平铺四枚",
        &caps,
        &[],
        m,
        "",
    );

    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">
<rect width="{w}" height="{h}" fill="{BG}"/>
{body}
</svg>"##,
        w = 760,
        h = y + 20.0,
        body = body
    );
    print!("{svg}");
}

#[allow(clippy::too_many_arguments)]
fn panel(
    body: &mut String,
    y: f32,
    title: &str,
    caps: &[KeycapText],
    footnote_widths: &[f32],
    m: KeycapMetrics,
    footnote_text: &str,
) -> f32 {
    let l = layout_keycaps(&KeycapInput {
        caps,
        footnote_widths,
        metrics: m,
    });
    let ox = 24.0;
    let oy = y + 28.0;

    body.push_str(&format!(
        r##"<text x="{ox}" y="{ty}" font-family="{FONT}" font-size="15" fill="{INK}" font-weight="600">{title}</text>"##,
        ty = y + 14.0
    ));

    // 窗口底
    body.push_str(&format!(
        r##"<rect x="{ox}" y="{oy}" width="{w}" height="{h}" rx="{r}" fill="#ffffff" stroke="{BORDER}" stroke-dasharray="3 3"/>"##,
        w = l.size.w,
        h = l.size.h,
        r = m.corner_radius
    ));

    for c in &l.caps {
        let r = c.rect;
        body.push_str(&format!(
            r##"<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" fill="{CAP_BG}" stroke="{BORDER}"/>"##,
            x = ox + r.x,
            y = oy + r.y,
            w = r.w,
            h = r.h,
            rx = m.corner_radius
        ));
        // 字母
        body.push_str(&format!(
            r##"<text x="{x}" y="{y}" font-family="{FONT}" font-size="{fs}" fill="{INK}" font-weight="700" dominant-baseline="text-before-edge">{t}</text>"##,
            x = ox + c.key_rect.x,
            y = oy + c.key_rect.y + 2.0,
            fs = m.font_size,
            t = c.key
        ));
        // 字根：前两个（主字根）实色，其余淡色——与绘制端的「变体淡一档」同义
        for (i, r) in c.root_rects.iter().enumerate() {
            let color = if i < c.main_root_count { INK } else { MUTED };
            body.push_str(&format!(
                r##"<text x="{x}" y="{y}" font-family="{FONT}" font-size="{fs}" fill="{c}" dominant-baseline="text-before-edge">{t}</text>"##,
                x = ox + r.x,
                y = oy + r.y + 1.0,
                fs = m.root_size,
                c = color,
                t = c.roots[i]
            ));
        }
        // 命中矩形（红）：核对「框」与「字」是否对齐
        body.push_str(&format!(
            r##"<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="none" stroke="{HIT}" stroke-width="0.6" opacity="0.5"/>"##,
            x = ox + r.x,
            y = oy + r.y,
            w = r.w,
            h = r.h
        ));
    }

    for (i, r) in l.footnotes.iter().enumerate() {
        let _ = i;
        body.push_str(&format!(
            r##"<text x="{x}" y="{y}" font-family="{FONT}" font-size="{fs}" fill="{MUTED}" dominant-baseline="text-before-edge">{t}</text>"##,
            x = ox + r.x,
            y = oy + r.y,
            fs = m.footnote_size,
            t = footnote_text
        ));
    }

    oy + l.size.h + 36.0
}
