//! M4c-0 渲染探针：`D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT` 在现有
//! DC 渲染目标（D2D 1.0）上能否画出**彩色** emoji。
//!
//! 与 `src/render.rs` 同一套管线：`CreateDIBSection` + `BindDC` + `DrawText`。
//! 画完把 32 位 BGRA 缓冲落成一张 BMP（无压缩、无依赖），供人工查看与
//! 脚本判定「是否出彩色」。运行：
//!
//! ```text
//! cargo run -p heinrich-win-ui --example emoji_probe -- <输出.bmp>
//! ```

use std::env;
use std::fs;
use std::path::PathBuf;
use std::ptr;

use windows::core::{w, Result};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_MULTI_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, BITMAPINFO,
    BITMAPINFOHEADER, DIB_RGB_COLORS,
};

const W: u32 = 320;
const H: u32 = 64;

fn main() -> Result<()> {
    let out = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("emoji-probe.bmp"));

    // 两组各画一行：开 / 关 ENABLE_COLOR_FONT。
    let colored = render_row(true)?;
    let plain = render_row(false)?;

    // 判定：彩色行的颜色种数应显著多于单色行（彩色 emoji 至少带黄/红/蓝）。
    let colored_colors = distinct_colors(&colored);
    let plain_colors = distinct_colors(&plain);
    println!(
        "distinct colors: color-font={} plain={}",
        colored_colors, plain_colors
    );
    if colored_colors <= plain_colors + 4 {
        println!("RESULT: FAIL — ENABLE_COLOR_FONT 未产出彩色字形");
    } else {
        println!("RESULT: PASS — 彩色字形可用");
    }

    // 落图：两行上下拼成一张。
    let mut pixels = vec![0u8; (W * H * 2 * 4) as usize];
    let row = (W * 4) as usize;
    pixels[..row * H as usize].copy_from_slice(&colored);
    pixels[row * H as usize..].copy_from_slice(&plain);
    write_bmp(&out, W, H * 2, &pixels);
    println!("written: {}", out.display());
    Ok(())
}

/// 画一行并取回像素：`white BG` + emoji + 中文字，options 由调用方决定。
fn render_row(color_font: bool) -> Result<Vec<u8>> {
    let factory: ID2D1Factory =
        unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None)? };
    let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };

    let screen = unsafe { GetDC(None) };
    let dc = unsafe { CreateCompatibleDC(Some(screen)) };
    unsafe { ReleaseDC(None, screen) };

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: W as i32,
            biHeight: -(H as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = ptr::null_mut();
    let bitmap = unsafe { CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)? };
    let old = unsafe { windows::Win32::Graphics::Gdi::SelectObject(dc, bitmap.into()) };

    let props = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    };
    let target = unsafe { factory.CreateDCRenderTarget(&props)? };
    unsafe {
        target.BindDC(
            dc,
            &RECT {
                left: 0,
                top: 0,
                right: W as i32,
                bottom: H as i32,
            },
        )?
    };

    let format = make_format(&dw, 28.0)?;
    let white = brush(&target, (1.0, 1.0, 1.0, 1.0))?;
    let black = brush(&target, (0.0, 0.0, 0.0, 1.0))?;

    unsafe {
        target.BeginDraw();
        target.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F_FULL,
                radiusX: 6.0,
                radiusY: 6.0,
            },
            &white,
        );
        // 👍 🎉 😊 + 中文参照
        let text: Vec<u16> = "👍🎉😊 彩色?".encode_utf16().collect();
        let rect = D2D_RECT_F_FULL;
        target.PushAxisAlignedClip(&rect, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        target.DrawText(
            &text,
            &format,
            &rect,
            &black,
            if color_font {
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT
            } else {
                D2D1_DRAW_TEXT_OPTIONS_NONE
            },
            DWRITE_MEASURING_MODE_NATURAL,
        );
        target.PopAxisAlignedClip();
        target.EndDraw(None, None)?;
    }

    let pixels =
        unsafe { std::slice::from_raw_parts(bits as *const u8, (W * H * 4) as usize) }.to_vec();

    unsafe {
        let _ = windows::Win32::Graphics::Gdi::SelectObject(dc, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
    }
    Ok(pixels)
}

/// 预乘 BGRA 里出现过的不同颜色数（忽略近黑与近白）。
fn distinct_colors(pixels: &[u8]) -> usize {
    let mut set = std::collections::HashSet::new();
    for px in pixels.chunks_exact(4) {
        let (b, g, r) = (px[0], px[1], px[2]);
        let near = |v: u8| !(24..=231).contains(&v);
        if near(b) && near(g) && near(r) {
            continue; // 背景与描边
        }
        set.insert((b / 16, g / 16, r / 16));
    }
    set.len()
}

fn make_format(factory: &IDWriteFactory, size: f32) -> Result<IDWriteTextFormat> {
    let format = unsafe {
        factory.CreateTextFormat(
            w!("Microsoft YaHei UI"),
            None::<&windows::Win32::Graphics::DirectWrite::IDWriteFontCollection>,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("zh-cn"),
        )?
    };
    unsafe {
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
    }
    Ok(format)
}

fn brush(target: &ID2D1DCRenderTarget, c: (f32, f32, f32, f32)) -> Result<ID2D1SolidColorBrush> {
    let color = D2D1_COLOR_F {
        r: c.0,
        g: c.1,
        b: c.2,
        a: c.3,
    };
    unsafe { target.CreateSolidColorBrush(&color, None) }
}

const D2D_RECT_F_FULL: windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F =
    windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
        left: 8.0,
        top: 4.0,
        right: (W - 8) as f32,
        bottom: (H - 4) as f32,
    };

/// 32 位 BGRA → 无压缩 24 位 BMP（自底向上行序），够看图即可。
fn write_bmp(path: &std::path::Path, w: u32, h: u32, bgra: &[u8]) {
    let row = (w * 3 + 3) & !3;
    let data_size = row * h;
    let header_size = 54;
    let mut out = Vec::with_capacity(header_size + data_size as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((header_size as u32) + data_size).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(header_size as u32).to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&data_size.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes()); // 72 dpi
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for y in (0..h).rev() {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let (b, g, r) = (bgra[i], bgra[i + 1], bgra[i + 2]);
            out.extend_from_slice(&[b, g, r]);
        }
        out.resize(out.len() + (row - w * 3) as usize, 0);
    }
    let _ = fs::write(path, out);
}
