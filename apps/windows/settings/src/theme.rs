//! 设置窗口的视觉 token（Fluent / Windows 11 设置卡片风）。
//!
//! 参考的是**在产**实现而不是样例：Windows 11 系统设置的分组卡片、
//! Windows 社区工具包 `SettingsCard`（WinUI Gallery 现役控件），以及
//! Microsoft Learn「应用设置指南」给出的硬数字——正文 14/20、说明 12/16、
//! 卡片圆角 8px、控件圆角 4px、卡片内衬 16px。
//!
//! 深色模式暂缓（D78 验收时定的）：这里的结构是「一张色表 + 一组字体」，
//! 将来加深色只是补一张色表，不动任何绘制代码。
//!
//! 颜色一律存 `COLORREF`（0x00BBGGRR），从 RGB 值转过去只经 [`rgb`] 一处——
//! 直接写 `0x00F3F3F3` 这类字面量时灰度值碰巧 BGR 对称，一旦写彩色的就会
//! 红蓝颠倒，那种错肉眼很难一眼定位。

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, FONT_CHARSET, FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION,
    FONT_QUALITY, HFONT,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

/// 从 RGB（红绿蓝）造 `COLORREF`（内部是 BGR 序）。全仓只许这里出现字节序知识。
pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32)
}

/// `a`、`b` 两色按 `t`（0.0 = 全 a，1.0 = 全 b）线性混合。给强调色派生
/// hover（混白）与按下（混黑）两档用，不用 GDI+ 的 alpha 通道。
const fn mix(a: COLORREF, b: COLORREF, t: u32) -> COLORREF {
    let ar = a.0 & 0xFF;
    let ag = (a.0 >> 8) & 0xFF;
    let ab = (a.0 >> 16) & 0xFF;
    let br = b.0 & 0xFF;
    let bg = (b.0 >> 8) & 0xFF;
    let bb = (b.0 >> 16) & 0xFF;
    let r = (ar * (256 - t) + br * t) / 256;
    let g = (ag * (256 - t) + bg * t) / 256;
    let bl = (ab * (256 - t) + bb * t) / 256;
    COLORREF(bl << 16 | g << 8 | r)
}

/// 系统强调色。读注册表 `HKCU\...\DWM\ColorizationColor`（0xAARRGGBB），
/// 掩掉 alpha。读不到（策略锁死、精简系统）回退 Win11 默认蓝。
///
/// 不用 WinRT 的 `UISettings`：那要引入整个 `Win32_UI_ViewManagement` 面，
/// 注册表这一个 DWORD 在 Win10/11 上都有、改强调色时会同步更新。
pub fn system_accent() -> COLORREF {
    let fallback = rgb(0x00, 0x67, 0xC0);
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let key: PCWSTR = w!("SOFTWARE\\Microsoft\\Windows\\DWM");
    let name: PCWSTR = w!("ColorizationColor");
    let ok = unsafe {
        RegGetValueW(
            HKEY(HKEY_CURRENT_USER.0),
            key,
            name,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    if ok.is_err() || size as usize != std::mem::size_of::<u32>() {
        return fallback;
    }
    // 高 8 位是 alpha（用户主题里可能带透明度），落盘规则不用它
    let c = value & 0x00FF_FFFF;
    COLORREF((c & 0xFF) << 16 | (c & 0xFF00) | ((c >> 16) & 0xFF))
}

/// 一张浅色色表。强调色之外的值全部抄 Win11 浅色主题；换深色 = 换这张表。
pub struct Colors {
    /// 窗口底（页面背景，不是卡片）
    pub bg: COLORREF,
    /// 卡片底
    pub card: COLORREF,
    /// 卡片 1px 描边
    pub card_border: COLORREF,
    /// 卡片内行分隔线（比描边更淡，且左端缩进 16px）
    pub separator: COLORREF,
    /// 主文字（标题、正文）
    pub text1: COLORREF,
    /// 次级文字（描述、说明）
    pub text2: COLORREF,
    /// 弱文字（默认值提示、路径）
    pub text3: COLORREF,
    /// 输入框描边（非焦点）
    pub input_border: COLORREF,
    /// 强调色（焦点描边、主按钮、选中页指示条）
    pub accent: COLORREF,
    /// 强调色上的文字（白或黑，按强调色亮度定）
    pub accent_text: COLORREF,
    /// 错误文字 / 非法输入
    pub error: COLORREF,
    /// 三段选择器的底槽（比 bg 深一档）
    pub seg_track: COLORREF,
    /// 次按钮 hover 底
    pub btn_hover: COLORREF,
    /// 禁用底
    pub btn_disabled_bg: COLORREF,
    /// 禁用字
    pub btn_disabled_text: COLORREF,
    /// 主按钮 hover / 按下
    pub accent_hover: COLORREF,
    pub accent_down: COLORREF,
}

impl Colors {
    pub fn light(accent: COLORREF) -> Self {
        // 亮度（感知加权）超过 150 时强调色太浅，上面的字改用黑——
        // 用户在系统里选黄/青绿当强调色时，白字会看不清。
        let r = accent.0 & 0xFF;
        let g = (accent.0 >> 8) & 0xFF;
        let b = (accent.0 >> 16) & 0xFF;
        let lum = (r * 299 + g * 587 + b * 114) / 1000;
        let white = rgb(0xFF, 0xFF, 0xFF);
        let black = rgb(0x1A, 0x1A, 0x1A);
        Self {
            bg: rgb(0xF3, 0xF3, 0xF3),
            card: white,
            card_border: rgb(0xE5, 0xE5, 0xE5),
            separator: rgb(0xED, 0xED, 0xED),
            text1: rgb(0x1A, 0x1A, 0x1A),
            text2: rgb(0x5D, 0x5D, 0x5D),
            text3: rgb(0x8A, 0x8A, 0x8A),
            input_border: rgb(0xD6, 0xD6, 0xD6),
            accent,
            accent_text: if lum > 150 { black } else { white },
            error: rgb(0xC4, 0x2B, 0x1C),
            seg_track: rgb(0xEC, 0xEC, 0xEC),
            btn_hover: rgb(0xF6, 0xF6, 0xF6),
            btn_disabled_bg: rgb(0xF0, 0xF0, 0xF0),
            btn_disabled_text: rgb(0xA6, 0xA6, 0xA6),
            accent_hover: mix(accent, white, 36),
            // 按下态往纯黑里混：text1 那个黑带 0x1A 底，红通道为 0 的强调色
            // 混它反而会变亮
            accent_down: mix(accent, COLORREF(0), 36),
        }
    }
}

/// 四档字号。微软规范（正文 14/20、说明 12/16、分区标题 BodyStrong、页面标题 20/28）
/// 落到 GDI 就是四个 HFONT；YaHei UI 没有可变的字重轴，标题用 600 近似（渲染出来是粗体）。
pub struct Fonts {
    /// 页面标题 20px / 600
    pub title: HFONT,
    /// 分组标题 13px / 600
    pub section: HFONT,
    /// 正文与设置行标题 14px / 400
    pub body: HFONT,
    /// 说明文字 12px / 400
    pub caption: HFONT,
}

impl Fonts {
    pub fn new(dpi: f32) -> Self {
        let make = |px: f32, weight: i32| -> HFONT {
            unsafe {
                CreateFontW(
                    -(px * dpi) as i32,
                    0,
                    0,
                    0,
                    weight,
                    0,
                    0,
                    0,
                    FONT_CHARSET(1), // DEFAULT_CHARSET：中文按系统取
                    FONT_OUTPUT_PRECISION(0),
                    FONT_CLIP_PRECISION(0),
                    FONT_QUALITY(5), // CLEARTYPE_QUALITY
                    0,
                    w!("Microsoft YaHei UI"),
                )
            }
        };
        Self {
            title: make(20.0, 600),
            section: make(13.0, 600),
            body: make(14.0, 400),
            caption: make(12.0, 400),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.title.into());
            let _ = DeleteObject(self.section.into());
            let _ = DeleteObject(self.body.into());
            let _ = DeleteObject(self.caption.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 颜色字节序不颠倒() {
        // rgb(r,g,b) 的低 8 位必须是 r：COLORREF 是 0x00BBGGRR
        let c = rgb(0x12, 0x34, 0x56);
        assert_eq!(c.0 & 0xFF, 0x12);
        assert_eq!((c.0 >> 8) & 0xFF, 0x34);
        assert_eq!((c.0 >> 16) & 0xFF, 0x56);
    }

    #[test]
    fn 深强调色用白字浅强调色用黑字() {
        let dark = Colors::light(rgb(0x00, 0x42, 0x7F));
        assert_eq!(dark.accent_text.0, 0x00FF_FFFF); // 白
        let light = Colors::light(rgb(0xF7, 0xC9, 0x48));
        assert_eq!(light.accent_text.0 & 0xFF, 0x1A); // 黑（字面量是 BGR，r 在低位）
    }

    #[test]
    fn 强调色派生色不越界() {
        let c = Colors::light(rgb(0x00, 0x67, 0xC0));
        let ch = |x: COLORREF, sh: u32| ((x.0 >> sh) & 0xFF) as i32;
        // hover 比原色亮、down 比原色暗（逐通道），且都落在 0–255
        for sh in [0u32, 8, 16] {
            assert!(ch(c.accent_hover, sh) >= ch(c.accent, sh));
            assert!(ch(c.accent_down, sh) <= ch(c.accent, sh));
        }
    }
}
