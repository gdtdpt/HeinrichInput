//! 绘制：把几何层算好的矩形画进一张 32 位预乘 alpha 的位图。
//!
//! 路线是 `Direct2D` 的 **DC 渲染目标**：把 `ID2D1DCRenderTarget` 绑到一块
//! `CreateDIBSection` 出来的 BGRA 内存位图上，画完拿这块位图去
//! `UpdateLayeredWindow`。这是 Windows 上做 IME 候选窗的常规组合（Weasel 用
//! GDI+ 走同一套位图/分层窗口接口），好处是：
//!
//! - **抗锯齿文字**：GDI 的 `TextOut` 会把文字画成 1 位 alpha，边缘毛刺且背景发实；
//! - **圆角与半透明**：位图的 alpha 通道归我们控制，形状外是真的透明；
//! - **白拿 DPI**：位图按物理像素生成，125%/150% 下字形自然清晰。
//!
//! 渲染目标固定 `dpiX = dpiY = 96`。这样 D2D 的「DIP」与我们几何层的「物理像素」
//! 一比一，`Metrics.font_size`（已经是物理像素）直接当字号用，中间不再有任何
//! 缩放系数——比例一旦出现在两个地方，就一定会有一处算错。

use std::mem::size_of;
use std::ptr;

use heinrich_ui::Rect;
use windows::core::Result;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_MULTI_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};

use crate::text::TextEngine;
use crate::Prepared;

/// 配色。候选窗是系统级 UI，v1 只做浅色（跟系统输入法一致）；深色主题留到 M4
/// 的设置一起做——现在没有配置入口，做了也没人能改。
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// 96 DPI 下的字号（逻辑像素）。实际字号 = 它 × DPI 缩放。
    pub font_size: f32,
    pub background: [f32; 4],
    pub border: [f32; 4],
    pub ink: [f32; 4],
    pub muted: [f32; 4],
    pub highlight: [f32; 4],
    pub border_width: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            background: rgba(0xFF, 0xFF, 0xFF, 0xFF),
            border: rgba(0xC9, 0xC9, 0xC9, 0xFF),
            ink: rgba(0x22, 0x22, 0x22, 0xFF),
            muted: rgba(0x8A, 0x8A, 0x8A, 0xFF),
            highlight: rgba(0xDB, 0xEA, 0xFE, 0xFF),
            border_width: 1.0,
        }
    }
}

const fn rgba(r: u8, g: u8, b: u8, a: u8) -> [f32; 4] {
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ]
}

pub struct Renderer {
    factory: ID2D1Factory,
    text: TextEngine,
    surface: Option<Surface>,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        let factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None)? };
        Ok(Self {
            factory,
            text: TextEngine::new()?,
            surface: None,
        })
    }

    pub fn text_mut(&mut self) -> &mut TextEngine {
        &mut self.text
    }

    /// 画一帧。返回 false 表示底层设备丢了（`D2DERR_RECREATE_TARGET`），
    /// 调用方应丢弃缓存后重画。
    pub fn draw(&mut self, prepared: &Prepared, theme: &Theme) -> Result<bool> {
        let layout = &prepared.layout;
        let m = prepared.metrics;
        let w = layout.size.w.ceil().max(1.0) as u32;
        let h = layout.size.h.ceil().max(1.0) as u32;

        if self.surface.as_ref().map(|s| (s.w, s.h)) != Some((w, h)) {
            self.surface = Some(Surface::new(&self.factory, w, h)?);
        }
        let surface = self.surface.as_ref().expect("刚刚建好");

        let formats = self.text.formats(m.font_size)?;
        let target = &surface.target;

        unsafe {
            target.BeginDraw();
            // 全透明起步：圆角外面是真的什么都没有，不会有一圈白框。
            let transparent = D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            };
            target.Clear(Some(&transparent));

            let bg = brush(target, theme.background)?;
            let border = brush(target, theme.border)?;
            let ink = brush(target, theme.ink)?;
            let muted = brush(target, theme.muted)?;
            let highlight = brush(target, theme.highlight)?;

            // 背景 + 描边。矩形内缩半个描边宽度，1px 的线才正好压在边界上、
            // 不会被位图边缘裁掉一半（否则看起来像缺了一条边）。
            let half = theme.border_width * 0.5;
            let outer = rounded(w, h, m.corner_radius, half);
            target.FillRoundedRectangle(&outer, &bg);
            if theme.border_width > 0.0 {
                target.DrawRoundedRectangle(
                    &outer,
                    &border,
                    theme.border_width,
                    None::<&windows::Win32::Graphics::Direct2D::ID2D1StrokeStyle>,
                );
            }

            // 高亮块（键盘选中项）
            if let Some(item) = layout.highlight.and_then(|i| layout.items.get(i)) {
                let r = D2D1_ROUNDED_RECT {
                    rect: to_d2d(item.rect),
                    radiusX: m.corner_radius * 0.8,
                    radiusY: m.corner_radius * 0.8,
                };
                target.FillRoundedRectangle(&r, &highlight);
            }

            for item in &layout.items {
                text_in(target, &item.label, &formats.label, item.label_rect, &muted)?;
                text_in(target, &item.text, &formats.text, item.text_rect, &ink)?;
            }
            for arrow in [layout.prev.as_ref(), layout.next.as_ref()]
                .into_iter()
                .flatten()
            {
                text_in(target, &arrow.text, &formats.arrow, arrow.rect, &muted)?;
            }

            match target.EndDraw(None, None) {
                Ok(()) => Ok(true),
                // 设备丢失：位图与目标都不可信了，扔了重建。
                Err(e) if e.code() == D2DERR_RECREATE_TARGET => {
                    self.surface = None;
                    Ok(false)
                }
                Err(e) => Err(e),
            }
        }
    }

    /// 供 `UpdateLayeredWindow` 使用：位图所在的设备上下文。
    pub fn hdc(&self) -> Option<HDC> {
        self.surface.as_ref().map(|s| s.dc)
    }
}

const D2DERR_RECREATE_TARGET: windows::core::HRESULT = windows::core::HRESULT(0x8899000Eu32 as i32);

fn rounded(w: u32, h: u32, radius: f32, inset: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: inset,
            top: inset,
            right: w as f32 - inset,
            bottom: h as f32 - inset,
        },
        radiusX: radius,
        radiusY: radius,
    }
}

fn to_d2d(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

fn brush(target: &ID2D1DCRenderTarget, rgba: [f32; 4]) -> Result<ID2D1SolidColorBrush> {
    let color = D2D1_COLOR_F {
        r: rgba[0],
        g: rgba[1],
        b: rgba[2],
        a: rgba[3],
    };
    unsafe { target.CreateSolidColorBrush(&color, None) }
}

/// 在一段矩形里画一行字，**超出就裁掉**。
///
/// 裁是必须的：`layout()` 在超宽时会把候选按比例压窄并标记 `elided`，而 D2D 的
/// `DrawText` 不会自动截断——不裁就会画到隔壁候选身上。省略号（`…`）要
/// `IDWriteTextLayout` + trimming sign，留到 M2.3 一起做；目前顶多是被压窄的
/// 长词末尾被切掉一点，观感可接受。
fn text_in(
    target: &ID2D1DCRenderTarget,
    text: &str,
    format: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat,
    rect: Rect,
    color: &ID2D1SolidColorBrush,
) -> Result<()> {
    if text.is_empty() || rect.w <= 0.0 {
        return Ok(());
    }
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let d2d = to_d2d(rect);
    unsafe {
        target.PushAxisAlignedClip(&d2d, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        target.DrawText(
            &utf16,
            format,
            &d2d,
            color,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
        target.PopAxisAlignedClip();
    }
    Ok(())
}

/// 一块内存位图 + 绑在它上面的 D2D 渲染目标。
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    /// `SelectObject` 之前的老对象，销毁前要换回去，否则位图泄漏。
    old: HGDIOBJ,
    target: ID2D1DCRenderTarget,
    w: u32,
    h: u32,
}

impl Surface {
    fn new(factory: &ID2D1Factory, w: u32, h: u32) -> Result<Self> {
        let screen = unsafe { GetDC(None) };
        let dc = unsafe { CreateCompatibleDC(Some(screen)) };
        unsafe { ReleaseDC(None, screen) };

        // 负高度 = 自上而下的行序，与 D2D 的坐标方向一致；正高度会画出上下颠倒的图。
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB：不压缩，alpha 通道归我们自己填
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = ptr::null_mut();
        let bitmap =
            unsafe { CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)? };
        let old = unsafe { SelectObject(dc, bitmap.into()) };

        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                // `UpdateLayeredWindow` + `AC_SRC_ALPHA` 要求预乘 alpha。
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let target = unsafe { factory.CreateDCRenderTarget(&props)? };
        let rect = RECT {
            left: 0,
            top: 0,
            right: w as i32,
            bottom: h as i32,
        };
        unsafe { target.BindDC(dc, &rect)? };

        Ok(Self {
            dc,
            bitmap,
            old,
            target,
            w,
            h,
        })
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}
