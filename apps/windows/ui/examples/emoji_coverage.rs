//! M4c 补充探针：本机 emoji 字体对 `emoji.toml` 的**字形覆盖**。
//!
//! 起因：候选窗里「您好 🫂」显示成方块。`🫂` 是 U+1FAC2（Unicode 13.0），
//! 老 Segoe UI Emoji 没有这个字形，DirectWrite 找不到就画 .notdef。
//!
//! 判据必须是 `IDWriteFontFace::HasCharacter`：GDI 的 `GetGlyphIndicesW`
//! 按 UTF-16 码元查，非 BMP 字符（含全部彩色 emoji）一律报缺，是假阴性。
//!
//! ```text
//! cargo run -p heinrich-win-ui --example emoji_coverage -- data/wubi86/emoji.toml
//! ```

use std::env;
use std::fs;

use windows::core::HSTRING;
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
};

fn main() -> windows::core::Result<()> {
    let path = env::args()
        .nth(1)
        .unwrap_or_else(|| "data/wubi86/emoji.toml".to_string());
    let text = fs::read_to_string(&path).expect("读不到 emoji.toml");

    let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
    let mut collection: Option<IDWriteFontCollection> = None;
    unsafe { factory.GetSystemFontCollection(&mut collection, false)? };
    let collection = collection.expect("系统字体集为空");
    let name = HSTRING::from("Segoe UI Emoji");
    let mut index = 0u32;
    let mut exists = windows::core::BOOL(0);
    unsafe { collection.FindFamilyName(&name, &mut index, &mut exists)? };
    if !exists.as_bool() {
        println!("本机没有 {name:?} 这个字体族，无从判断");
        return Ok(());
    }
    let family = unsafe { collection.GetFontFamily(index)? };
    let font = unsafe {
        family.GetFirstMatchingFont(
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
        )?
    };

    let mut total = 0usize;
    let mut missing: Vec<(String, String)> = Vec::new();
    let mut word = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("word = ") {
            word = rest.trim_matches('"').to_string();
        } else if let Some(rest) = line.strip_prefix("emoji = ") {
            let emoji = rest.trim_matches('"').to_string();
            total += 1;
            // 逐个码位都要有字形：基字符 + FE0F 缺一个就画不全。
            let ok = emoji.chars().all(|c| {
                unsafe { font.HasCharacter(c as u32) }
                    .map(|found| found.as_bool())
                    .unwrap_or(false)
            });
            if !ok {
                missing.push((word.clone(), emoji));
            }
        }
    }

    println!(
        "条目 {total}  缺字形 {}  可用 {}",
        missing.len(),
        total - missing.len()
    );
    let mut blocks: std::collections::BTreeMap<u32, usize> = Default::default();
    for (_, e) in &missing {
        if let Some(c) = e.chars().next() {
            *blocks.entry((c as u32 >> 8) << 8).or_default() += 1;
        }
    }
    for (b, n) in blocks {
        println!("  缺 U+{b:04X}-U+{b:04X} 共 {n}");
    }
    for (w, e) in missing.iter().take(12) {
        let cp: Vec<String> = e.chars().map(|c| format!("U+{:04X}", c as u32)).collect();
        println!("  {w} {e}  {}", cp.join(" "));
    }
    Ok(())
}
