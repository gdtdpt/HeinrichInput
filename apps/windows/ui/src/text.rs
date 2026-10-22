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
/// 码提示 chip 相对词的字号比例。
///
/// 0.72 是「一眼能读、又不跟词抢注意力」的折中：再大 chip 就和候选字同权，
/// 用户会先看到 `VBG` 而不是「好」；再小在 100% DPI 下笔画就糊了。
const HINT_SCALE: f32 = 0.72;

/// 一套字号下的四个 `IDWriteTextFormat`（词 / 序号 / 箭头 / 码提示）。
///
/// 分开建是因为对齐方式不同：词左对齐，序号与箭头居中。四个都开
/// `PARAGRAPH_ALIGNMENT_CENTER`，这样 `text_rect` 一定高 = `line_height`，
/// 文字自动在行内垂直居中，绘制端不必再算基线偏移。
pub struct Formats {
    font_size: f32,
    pub text: IDWriteTextFormat,
    pub label: IDWriteTextFormat,
    pub arrow: IDWriteTextFormat,
    /// 码提示 chip 里的字。居中：chip 的宽度是量出来的，居中才不会一边挤一边空。
    pub hint: IDWriteTextFormat,
}

/// 键位图（§6.7）的四个字号。
///
/// 与 [`Formats`] 分开，是因为键位图要**四个不同字号同时出现**（字母 / 字根 /
/// 简码 / 说明），而候选窗只要一个主字号加三个比例变体。硬塞进 `Formats` 会让
/// 候选窗每次 `formats()` 都多建三个用不上的 format。
pub struct KeycapFormats {
    base: f32,
    /// 字母。
    pub key: IDWriteTextFormat,
    /// 字根（含变体，颜色不同但字号相同）。
    pub root: IDWriteTextFormat,
    /// 一级简码。
    pub primary: IDWriteTextFormat,
    /// 底部固定说明。左对齐：它是一整句话，居中会让两行的左缘参差。
    pub footnote: IDWriteTextFormat,
}

pub struct TextEngine {
    factory: IDWriteFactory,
    /// 按字号缓存。DPI 不变时（绝大多数情况）一次都不重建。
    cached: Option<Formats>,
    /// 键位图的一套，同样按基数缓存。
    keycap: Option<KeycapFormats>,
}

impl TextEngine {
    pub fn new() -> Result<Self> {
        let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        Ok(Self {
            factory,
            cached: None,
            keycap: None,
        })
    }

    /// 取（必要时重建）某个字号下的几个 format。
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
                hint: make_format(&self.factory, font_size * HINT_SCALE, true)?,
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
        self.measure_with(text, font_size, false)
    }

    /// 量码提示用**小字号**的宽度。`hint = true` 时用 chip 的 format——
    /// 与主字号混用会把 chip 量宽一圈（`VB` 在 16px 下比 11.5px 下宽得多）。
    pub fn measure_hint(&mut self, text: &str, font_size: f32) -> Result<f32> {
        self.measure_with(text, font_size, true)
    }

    fn measure_with(&mut self, text: &str, font_size: f32, hint: bool) -> Result<f32> {
        let formats = self.formats(font_size)?;
        let format = if hint {
            formats.hint.clone()
        } else {
            formats.text.clone()
        };
        self.measure_in(text, &format)
    }

    /// 按**指定 format** 量宽度。键位图的四种字号各用各的，不能共用一个 `font_size`
    /// 参数（`formats()` 按字号只缓存一套，来回切会反复重建）。
    ///
    /// 取 `&self`：`factory` 与 `CreateTextLayout` 都不需要可变状态，这样调用方可以先
    /// `keycap_formats()` 取 format、clone 出来，再调本方法——不会撞上借用冲突。
    pub fn measure_in(&self, text: &str, format: &IDWriteTextFormat) -> Result<f32> {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        // 上限给足：不换行，只要单行宽度。
        let layout = unsafe {
            self.factory
                .CreateTextLayout(&utf16, format, 100_000.0, 100_000.0)?
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)? };
        Ok(metrics
            .widthIncludingTrailingWhitespace
            .max(metrics.width)
            .max(0.0))
    }

    /// 取（必要时重建）键位图的那一套 format。`base` 是字母字号，其余三个由调用方
    /// 按 `KeycapMetrics` 的比例算好后传入——本模块不替几何层决定字号比例。
    pub fn keycap_formats(
        &mut self,
        base: f32,
        root: f32,
        primary: f32,
        footnote: f32,
    ) -> Result<&KeycapFormats> {
        let stale = match &self.keycap {
            Some(f) => (f.base - base).abs() > 0.01,
            None => true,
        };
        if stale {
            self.keycap = Some(KeycapFormats {
                base,
                key: make_format(&self.factory, base, true)?,
                root: make_format(&self.factory, root, true)?,
                primary: make_format(&self.factory, primary, true)?,
                footnote: make_format(&self.factory, footnote, false)?,
            });
        }
        Ok(self.keycap.as_ref().expect("刚刚填过"))
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
