//! 把一枚**彩色** emoji 画进任意 HDC（列表行里用）。
//!
//! ## 为什么不能直接用 GDI
//!
//! `TextOutW` / `DrawTextW` 走的是 GDI 的字形选择：`Segoe UI Emoji` 里的彩色
//! 字形由 COLR/CPAL 表描述，GDI 只能取到它的单色轮廓（那一道空心轮廓就是这样
//! 画出来的）。候选窗与选择器都是同一个结论（见 `apps/windows/ui/src/render.rs`
//! 与 `emoji_picker.rs`）：**彩色只有 D2D 的 `DrawText` +
//! `D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT` 这一条路**。
//!
//! ## 为什么能画进列表
//!
//! `ID2D1DCRenderTarget` 可以把目标绑到任意 HDC 的任意矩形上（`BindDC`），
//! ListView 在 `NM_CUSTOMDRAW` 里给的就是它自己的（双缓冲）HDC。于是同一行里
//! 汉字走 GDI、emoji 走 D2D，两者落在同一张位图上——这是让列表里那一列从
//! 「单色轮廓」变成「与候选窗同一枚」的唯一办法。
//!
//! **两个必须记住的点**：
//!
//! - 绑定之后绘制坐标以**绑定矩形的左上角**为原点，不是 DC 的原点；
//! - DC 渲染目标不做 alpha 混合（`D2D1_ALPHA_MODE_IGNORE`），背景得自己刷成
//!   行底色，否则那一格会留着上一次画的内容。

use windows::core::HSTRING;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::HDC;

/// 一枚 emoji 的画笔：一个 DC 渲染目标 + 一份「按需换字号」的文字格式。
///
/// 目标只建一次、反复 `BindDC`——一行建一个目标既慢又浪费（一屏十几行）。
/// `IDWriteTextFormat` 的字号在创建时固定（`SetFontSize` 只存在于
/// `IDWriteTextLayout`），所以字号变了就重建那一份：实测行高恒定，重建只在
/// 第一次发生。
pub struct ColorEmoji {
    target: ID2D1DCRenderTarget,
    write: IDWriteFactory,
    format: Option<(f32, IDWriteTextFormat)>,
}

impl ColorEmoji {
    pub fn new() -> windows::core::Result<Self> {
        let factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            // DC 目标的 DPI 固定 96：绑上来的 HDC 是像素坐标，写别的值等于再乘一次
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let target: ID2D1DCRenderTarget = unsafe { factory.CreateDCRenderTarget(&props)? };
        let write: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        Ok(Self {
            target,
            write,
            format: None,
        })
    }

    fn format(&mut self, px: f32) -> windows::core::Result<IDWriteTextFormat> {
        if let Some((cur, fmt)) = &self.format {
            if (*cur - px).abs() < 0.5 {
                return Ok(fmt.clone());
            }
        }
        let fmt = unsafe {
            self.write.CreateTextFormat(
                windows::core::w!("Segoe UI Emoji"),
                None::<&IDWriteFontCollection>,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                px,
                windows::core::w!(""),
            )?
        };
        unsafe {
            fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        }
        self.format = Some((px, fmt.clone()));
        Ok(fmt)
    }

    /// 在 `hdc` 的 `rect` 里画一枚 emoji，`bg` 是这一格的底色（先刷满）。
    ///
    /// 返回 `false` 表示这格没画成（目标丢了 / 绑定失败 / 空串）——调用方自己
    /// 决定退回什么，本层不猜。
    pub fn draw(&mut self, hdc: HDC, rect: RECT, emoji: &str, px: f32, bg: D2D1_COLOR_F) -> bool {
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        if w <= 0 || h <= 0 || emoji.is_empty() {
            return false;
        }
        let Ok(format) = self.format(px.clamp(8.0, 96.0)) else {
            return false;
        };
        if unsafe { self.target.BindDC(hdc, &rect) }.is_err() {
            return false;
        }
        let layout = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: w as f32,
            bottom: h as f32,
        };
        unsafe {
            self.target.BeginDraw();
            self.target.Clear(Some(&bg));
            let brush: ID2D1SolidColorBrush = match self.target.CreateSolidColorBrush(&ink(), None)
            {
                Ok(b) => b,
                Err(_) => {
                    let _ = self.target.EndDraw(None, None);
                    return false;
                }
            };
            self.target.DrawText(
                &HSTRING::from(emoji),
                &format,
                &layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            self.target.EndDraw(None, None).is_ok()
        }
    }
}

/// 文字色。彩色字形自己带颜色，这一位只影响非彩色字形（少数单色 emoji）。
fn ink() -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: 0.13,
        g: 0.13,
        b: 0.13,
        a: 1.0,
    }
}

/// `COLORREF`（0x00BBGGRR）→ D2D 颜色。
pub fn from_colorref(c: COLORREF) -> D2D1_COLOR_F {
    let v = c.0;
    D2D1_COLOR_F {
        r: ((v & 0xFF) as f32) / 255.0,
        g: (((v >> 8) & 0xFF) as f32) / 255.0,
        b: (((v >> 16) & 0xFF) as f32) / 255.0,
        a: 1.0,
    }
}
