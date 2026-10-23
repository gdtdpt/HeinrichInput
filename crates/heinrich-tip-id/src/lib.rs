//! 亨利输入法 · TSF 身份常量。
//!
//! 这些值**一旦发布就不能再改**：CLSID 与 Profile GUID 已经写进用户的注册表，
//! 改了等于在系统里留下一条永远指向空处的死条目。所以集中在这一处定义，
//! 由 DLL（`heinrich-win`）与注册器（`heinrich-setup`）共同引用，避免两边各写一份。
//!
//! 本 crate **零依赖**：只放 `u128` 与字符串，不引入 `windows`。
//! 需要 `GUID` 的调用方自己 `GUID::from_u128(heinrich_tip_id::TIP_CLSID)`。

/// TIP 的 CLSID。由 `uuid5(NAMESPACE_DNS, "heinrich.tip.clsid")` 生成，保证可复现。
pub const TIP_CLSID: u128 = 0x0EB9_6F60_FFEC_5F48_871F_EA75_A989_F546;

/// 简体中文（0x0804）语言档的 Profile GUID。
pub const PROFILE_ZH_CN: u128 = 0x06CF_8277_49D8_5317_9410_7EEA_A491_A936;

/// 语言档所属的 LANGID：中文（简体，中国）。
pub const LANGID_ZH_CN: u16 = 0x0804;

/// CLSID 的注册表书写形式（大括号内、无分隔花括号）。
pub const TIP_CLSID_TEXT: &str = "0EB96F60-FFEC-5F48-871F-EA75A989F546";

/// Profile GUID 的注册表书写形式。
pub const PROFILE_ZH_CN_TEXT: &str = "06CF8277-49D8-5317-9410-7EEAA491A936";

/// 语言栏与安装器里显示的输入法名。
pub const DISPLAY_NAME: &str = "亨利五笔";

/// DLL 文件名（与 `heinrich-setup.exe` 放在同一目录）。
///
/// 注意这里是**下划线**：Cargo 对 `cdylib` 的产物名会把 crate 名里的连字符
/// 换成下划线，`heinrich-win` → `heinrich_win.dll`。直接采用真实产物名，
/// 避免打包时改名、或维护一层名字映射（那才是真正容易出错的地方）。
pub const DLL_FILE_NAME: &str = "heinrich_win.dll";

/// 设置程序的可执行文件名（与 [`DLL_FILE_NAME`] 放在同一目录）。
///
/// DLL 被系统加载在别人的进程里，它要拉起设置窗口只能靠「自己所在目录 +
/// 这个文件名」，所以两边必须用同一份常量。
pub const SETTINGS_EXE_NAME: &str = "heinrich-settings.exe";

/// 设置窗口的窗口类名。
///
/// 一处定义、两处使用：设置程序用它 `RegisterClassW`，DLL 用 `FindWindowW`
/// 找「已经开着的那一个」。两边各写一份字符串是本项目最容易犯的错误——
/// 改了一边忘另一边，表现是「点选项开第二个窗口」，没有任何报错。
pub const SETTINGS_CLASS: &str = "HeinrichSettings";

/// 语言列表里的图标文件名（与 [`DLL_FILE_NAME`] 放在同一目录）。
///
/// Windows 的输入法列表读注册表 `...\LanguageProfile\...\IconFile` + `IconIndex`。
/// 我们原来把 IconFile 指到 DLL，但 DLL 里没有图标资源，列表里就是一块空白
/// （真机截图可见）。改成指向这个独立 .ico：由 `tools\make-icon.ps1` 生成、
/// `tools\build.ps1` 复制到 dist，`heinrich-setup install` 写进注册表。
pub const ICON_FILE_NAME: &str = "heinrich.ico";

/// `InstallLayoutOrTip` 需要的描述串：`<LANGID 十六进制>:{CLSID}{PROFILE GUID}`。
///
/// 格式见 <https://learn.microsoft.com/en-us/windows/win32/tsf/installlayoutortip>。
pub fn layout_or_tip_descriptor() -> String {
    format!("0x{LANGID_ZH_CN:04X}:{{{TIP_CLSID_TEXT}}}{{{PROFILE_ZH_CN_TEXT}}}")
}

/// 注册表路径片段：`Software\Classes\CLSID\{...}`。
pub fn clsid_key() -> String {
    format!("Software\\Classes\\CLSID\\{{{TIP_CLSID_TEXT}}}")
}

/// 注册表路径片段：`Software\\Microsoft\\CTF\\TIP\\{...}`。
pub fn ctf_tip_key() -> String {
    format!("Software\\Microsoft\\CTF\\TIP\\{{{TIP_CLSID_TEXT}}}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 描述串的格式错一个字符，`InstallLayoutOrTip` 就会静默不生效
    /// （返回 void，没有错误码可查），所以这里把形状钉死。
    #[test]
    fn descriptor_shape() {
        assert_eq!(
            layout_or_tip_descriptor(),
            "0x0804:{0EB96F60-FFEC-5F48-871F-EA75A989F546}{06CF8277-49D8-5317-9410-7EEAA491A936}"
        );
    }

    #[test]
    fn keys_match_guids() {
        assert!(clsid_key().contains(TIP_CLSID_TEXT));
        assert!(ctf_tip_key().contains(TIP_CLSID_TEXT));
    }

    /// 两个 GUID 必须是合法 UUID（版本位/变体位正确），否则 Windows 可能拒绝。
    #[test]
    fn guids_are_valid_uuid5() {
        for (raw, version_nibble) in [(TIP_CLSID, 5u128), (PROFILE_ZH_CN, 5u128)] {
            assert_eq!((raw >> 76) & 0xF, version_nibble, "版本位应为 5（uuid5）");
            assert_eq!((raw >> 62) & 0x3, 0b10, "变体位应为 RFC 4122");
        }
        assert_ne!(TIP_CLSID, PROFILE_ZH_CN);
    }
}
