//! TSF 的三步注册。
//!
//! 注册一个输入法需要**三件**彼此独立的事，缺一件都不出现在输入法列表里：
//!
//! 1. **COM 注册**：把 CLSID 指向 DLL，让 `CoCreateInstance` 能找到我们
//!    （写注册表，见 `main::install`）。
//! 2. **语言档注册**：告诉 CTF「这个 COM 组件是一个文本服务，隶属于简体中文」
//!    （`ITfInputProcessorProfileMgr::RegisterProfile`）。
//! 3. **类别注册**：声明它属于哪些能力类别（键盘输入法 / 沉浸式支持 / 托盘指示…）
//!    （`ITfCategoryMgr::RegisterCategory`）。
//!
//! 然后用 `InstallLayoutOrTip` 把它真正挂进语言的输入法列表——这一步没有返回值，
//! 也没法查错误码，写错就静默不生效。所以描述串的格式在
//! `heinrich-tip-id` 里用单测钉死了。

use windows::core::{IUnknown, Interface, Result, GUID};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, ITfCategoryMgr, ITfFnConfigure,
    ITfInputProcessorProfileMgr, ITfTextInputProcessor, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT, GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    GUID_TFCAT_TIP_KEYBOARD,
};

// `input.dll` 里的导出函数，windows crate 没有绑定，只能自己声明。
// 返回值是 void —— 这是它最坑的地方，失败了你不会知道。
windows::core::link!("input.dll" "system" fn InstallLayoutOrTip(psz: *const u16, dwflags: u32));

const ILOT_INSTALL: u32 = 0x0000_0000;
const ILOT_UNINSTALL: u32 = 0x0000_0001;

/// 我们声明的能力类别。
///
/// **只声明真的实现了的**。`GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER`（组合串的虚线）
/// 要等 M1.2 实现 `ITfDisplayAttributeProvider` 之后才能加——
/// 声明了却没实现，系统调用时会拿到 `E_NOINTERFACE`，表现为组合串样式丢失。
pub const CATEGORIES: [GUID; 4] = [
    // 声明「我是键盘输入法」。不加这一条根本不会出现在输入法列表里。
    GUID_TFCAT_TIP_KEYBOARD,
    // 支持中/英模式 compartment。不加则中英切换相关行为异常。
    GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
    // Win8+：支持 UWP / Store 应用里输入。
    GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    // Win8+：支持任务栏托盘输入指示器的状态显示。
    GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
];

pub fn tip_clsid() -> GUID {
    GUID::from_u128(heinrich_tip_id::TIP_CLSID)
}

pub fn profile_guid() -> GUID {
    GUID::from_u128(heinrich_tip_id::PROFILE_ZH_CN)
}

fn desc_units() -> Vec<u16> {
    heinrich_tip_id::DISPLAY_NAME.encode_utf16().collect()
}

/// 第 2 步：注册语言档。
///
/// `icon_path` 指向一个真实存在的文件（我们传 DLL 自己）。这里传空会被某些
/// Windows 版本拒绝，而传不存在的路径会让语言栏取图标失败——两者都别做。
pub fn register_profile(icon_path: &str) -> Result<()> {
    let icon: Vec<u16> = icon_path.encode_utf16().collect();
    let desc = desc_units();
    let clsid = tip_clsid();
    let profile = profile_guid();
    unsafe {
        let mgr: ITfInputProcessorProfileMgr =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?;
        mgr.RegisterProfile(
            &clsid,
            heinrich_tip_id::LANGID_ZH_CN,
            &profile,
            &desc,
            &icon,
            0,              // 图标序号
            HKL::default(), // 不探测 HKL：Weasel 对港/澳/新也是传 NULL
            0,              // 首选布局
            true,           // 装完即启用
            0,              // flags
        )
    }
}

pub fn unregister_profile() -> Result<()> {
    let clsid = tip_clsid();
    let profile = profile_guid();
    unsafe {
        let mgr: ITfInputProcessorProfileMgr =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?;
        mgr.UnregisterProfile(&clsid, heinrich_tip_id::LANGID_ZH_CN, &profile, 0)
    }
}

/// 第 3 步：注册能力类别。`RegisterCategory` 会自己写正反两张索引表。
pub fn register_categories() -> Result<()> {
    let clsid = tip_clsid();
    unsafe {
        let mgr: ITfCategoryMgr =
            CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
        for cat in CATEGORIES {
            mgr.RegisterCategory(&clsid, &cat, &clsid)?;
        }
    }
    Ok(())
}

pub fn unregister_categories() -> Result<()> {
    let clsid = tip_clsid();
    unsafe {
        let mgr: ITfCategoryMgr =
            CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
        for cat in CATEGORIES {
            mgr.UnregisterCategory(&clsid, &cat, &clsid)?;
        }
    }
    Ok(())
}

/// 把输入法挂进语言的输入法列表 / 摘出来。
pub fn set_layout(install: bool) {
    let desc = heinrich_tip_id::layout_or_tip_descriptor();
    let units = crate::reg::wide(&desc);
    unsafe {
        InstallLayoutOrTip(
            units.as_ptr(),
            if install {
                ILOT_INSTALL
            } else {
                ILOT_UNINSTALL
            },
        );
    }
}

/// 端到端探针：真的 `CoCreateInstance` 一次，再查 `ITfTextInputProcessor`。
///
/// 这一步能一次性验证「注册表 → DLL → 导出 → QueryInterface」整条链。
/// 只查注册表不够：注册表全对但 DLL 缺依赖、或 `DllGetClassObject` 认错 CLSID，
/// 表现都是「列表里有名字但一切换就废」。
pub fn probe_create_instance() -> Result<()> {
    let clsid = tip_clsid();
    unsafe {
        let unknown: IUnknown = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)?;
        let _tsf: ITfTextInputProcessor = unknown.cast()?;
    }
    Ok(())
}

/// 端到端探针：系统设置页的「选项」按钮能不能出现。
///
/// Windows 的输入法管理界面按微软文档的做法
/// `CoCreateInstance(CLSID, IID_ITfFnConfigure)` 拿这个接口——**拿得到才画按钮，
/// 拿不到就静默不画**（v1.42 的截图里「删除」左边那块空白就是这么来的）。
/// 所以这一项不是「锦上添花的功能检查」，它直接决定系统 UI 长什么样。
///
/// 注意与 `probe_create_instance` 的区别：那个只证明「是个能激活的文本服务」，
/// 这个证明「是一个可配置的文本服务」。两者都过，才等于「注册完整」。
pub fn probe_fn_configure() -> Result<()> {
    let clsid = tip_clsid();
    unsafe {
        let _configure: ITfFnConfigure = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)?;
    }
    Ok(())
}

/// 直接加载 DLL 并检查导出符号。
///
/// 与 `probe_create_instance` 分工不同：这一项**不经过注册表**，
/// 所以能把两种故障分开——
///
/// - 本项失败 ⇒ DLL 本身有问题（缺依赖 / 架构不匹配 / 导出被裁掉）；
/// - 本项通过但 COM 探针失败 ⇒ 注册表写错了。
///
/// 加载本身是安全的：我们的 `DllMain` 只调 `DisableThreadLibraryCalls`。
pub fn probe_dll_exports(dll_path: &str) -> std::result::Result<(), String> {
    use windows::core::{s, PCWSTR};
    use windows::Win32::Foundation::FreeLibrary;
    use windows::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH,
    };

    let path = crate::reg::wide(dll_path);
    unsafe {
        let module = LoadLibraryExW(PCWSTR(path.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH)
            .map_err(|e| format!("LoadLibraryExW 失败：{e}"))?;

        let mut missing = Vec::new();
        for name in ["DllGetClassObject", "DllCanUnloadNow"] {
            // `s!` 在编译期生成以 NUL 结尾的 PCSTR，不需要运行时分配。
            let proc = match name {
                "DllGetClassObject" => GetProcAddress(module, s!("DllGetClassObject")),
                _ => GetProcAddress(module, s!("DllCanUnloadNow")),
            };
            if proc.is_none() {
                missing.push(name);
            }
        }
        let _ = FreeLibrary(module);

        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!("缺少导出：{}", missing.join("、")))
        }
    }
}
