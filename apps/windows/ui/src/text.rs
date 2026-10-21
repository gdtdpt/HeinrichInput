//! DirectWrite：字体选取与文字度量。
//!
//! 几何层刻意**不量字**（它连字体都不认识），宽度必须由这里量好喂进去。所以这个
//! 模块是「同一份文字，几何层认为是多宽」的唯一来源——绘制时用的是同一批
//! `IDWriteTextFormat`，量出来的宽度与画出来的字形才对得上。

use windows::core::{w, Result};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP,
};

/// 界面字体。Win10 1709 自带；缺了 DirectWrite 会自己回退，不会失败。
const FONT_FAMILY: windows::core::PCWSTR = w!("Microsoft YaHei UI");

/// 序号与箭头相对词的字号比例。
const LABEL_SCALE: f32 = 0.80;
const ARROW_SCALE: f32 = 0.85;

/// 一套字号下的三个 `IDWriteTextFormat`（词 / 序号 / 箭头）。
///
/// 分开建是因为对齐方式不同：词左对齐，序号与箭头居中。三个都开
/// `PARAGRAPH_ALIGNMENT_CENTER`，这样 `text_rect` 一定高 = `line_height`，
/// 文字自动在行内垂直居中，绘制端不必再算基线偏移。
pub struct Formats {
    font_size: f32,
    pub text: IDWriteTextFormat,
    pub label: IDWriteTextFormat,
    pub arrow: IDWriteTextFormat,
}

pub struct TextEngine {
    factory: IDWriteFactory,
    /// 按字号缓存。DPI 不变时（绝大多数情况）一次都不重建。
    cached: Option<Formats>,
}

impl TextEngine {
    pub fn new() -> Result<Self> {
        let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        Ok(Self {
            factory,
            cached: None,
        })
    }

    /// 取（必要时重建）某个字号下的三个 format。
    pub fn formats(&mut self, font_size: f32) -> Result<&Formats> {
        let stale = match &self.cached {
            Some(f) => (f.font_size - font_size).abs() > 0.01,
            None => true,
        };
        if stale {
            self.cached = Some(Formats {
                font_size,
                text: make_format(&self.factory, font_size, false)?,
                label: make_format(&self.factory, font_size * LABEL_SCALE, true)?,
                arrow: make_format(&self.factory, font_size * ARROW_SCALE, true)?,
            });
        }
        Ok(self.cached.as_ref().expect("刚刚填过"))
    }

    /// 量一段文字在 `font_size` 下的自然宽度（物理像素）。
    ///
    /// 用 `widthIncludingTrailingWhitespace` 与 `width` 取大者：前者把行尾空格算进去
    /// （候选词里不会有），后者是字形实际占据的宽度。取大是为了「宁宽勿窄」——
    /// 少量空隙比把最后一个字压掉强。
    pub fn measure(&mut self, text: &str, font_size: f32) -> Result<f32> {
        let format = self.formats(font_size)?.text.clone();
        let utf16: Vec<u16> = text.encode_utf16().collect();
        // 上限给足：不换行，只要单行宽度。
        let layout = unsafe {
            self.factory
                .CreateTextLayout(&utf16, &format, 100_000.0, 100_000.0)?
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)? };
        Ok(metrics
            .widthIncludingTrailingWhitespace
            .max(metrics.width)
            .max(0.0))
    }
}

fn make_format(factory: &IDWriteFactory, size: f32, centered: bool) -> Result<IDWriteTextFormat> {
    let format = unsafe {
        factory.CreateTextFormat(
            FONT_FAMILY,
            // None = 用系统字体集合
            None::<&IDWriteFontCollection>,
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
        if centered {
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        }
    }
    Ok(format)
}
