//! 开始菜单里的「亨利五笔 设置」快捷方式。
//!
//! ## 为什么需要它
//!
//! 系统设置页那个「选项」按钮（`ITfFnConfigure`）**只给微软自家输入法画**——
//! 本机实测：微软五笔拿得到 `ITfFnConfigure`，设置页里它那一行照样只有「删除」；
//! 装上第三方输入法后，连微软拼音的「选项」都会变空白。所以对第三方 TIP 来说，
//! 那条路走不通，开始菜单才是真正的入口：按 Win、输入「亨利」，第一条就是。
//!
//! ## 两个位置
//!
//! - **公共**开始菜单（`FOLDERID_CommonPrograms`，`%ProgramData%\...`）：机器级安装
//!   的正统位置，所有用户可见；但**只有提权进程写得进去**。
//! - **当前用户**开始菜单（`FOLDERID_Programs`，`%APPDATA%\...`）：不需要提权。
//!   开发态（DLL 直接注册 dist 目录、从没跑过 `install`）想有个入口，只能靠它。
//!
//! `create_settings_shortcut` 先试公共、失败退用户级——提权时走正统位置，非提权
//! 时也不至于什么也没有。卸载时两处都删。
//!
//! 图标直接取设置程序自己的（`SetIconLocation(exe, 0)`）：exe 没有资源图标时
//! 系统会退回通用程序图标，也不难看——总比给 exe 塞一个只有快捷方式用得上的
//! 图标资源好。

use std::path::{Path, PathBuf};

use windows::core::{Interface, GUID, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER,
};
use windows::Win32::UI::Shell::{
    FOLDERID_CommonPrograms, FOLDERID_Programs, IShellLinkW, SHGetKnownFolderPath, ShellLink,
    KF_FLAG_DEFAULT,
};

/// 快捷方式文件名。中文名是为了让开始菜单搜索能命中「亨利」。
pub const SHORTCUT_NAME: &str = "亨利五笔 设置.lnk";

/// 解析一个已知文件夹。
fn known_dir(id: &GUID) -> Result<PathBuf, String> {
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None)
            .map_err(|e| format!("取已知文件夹目录失败：{e}"))?;
        let text = raw.to_string().map_err(|e| format!("路径解码失败：{e}"))?;
        // SHGetKnownFolderPath 的返回串是 CoTaskMemAlloc 出来的，必须还回去。
        CoTaskMemFree(Some(raw.0 as *const _));
        Ok(PathBuf::from(text))
    }
}

/// 公共开始菜单目录（写它需要提权）。
fn common_programs_dir() -> Result<PathBuf, String> {
    known_dir(&FOLDERID_CommonPrograms)
}

/// 当前用户开始菜单目录（不需要提权）。
fn user_programs_dir() -> Result<PathBuf, String> {
    known_dir(&FOLDERID_Programs)
}

/// 快捷方式的完整路径。
///
/// 已经有公共开始菜单那份就返回它，否则返回用户级位置（`install` 在非提权下
/// 会退到那里）。两处都没有时返回的是「下次会建在哪儿」。
pub fn shortcut_path() -> Result<PathBuf, String> {
    let common = common_programs_dir()?.join(SHORTCUT_NAME);
    if common.is_file() {
        return Ok(common);
    }
    Ok(user_programs_dir()?.join(SHORTCUT_NAME))
}

/// 建（或覆盖）快捷方式：先公共开始菜单，失败退用户级。返回实际写入的路径。
///
/// 调用前 COM 必须已初始化（`install` 在 `com_init()` 之后调）。
pub fn create_settings_shortcut(exe: &Path) -> Result<PathBuf, String> {
    if let Ok(dir) = common_programs_dir() {
        if let Ok(lnk) = create_settings_shortcut_in(&dir, exe) {
            return Ok(lnk);
        }
    }
    // 非提权进程写公共开始菜单是 `E_ACCESSDENIED`，不是错误——退到用户级。
    create_settings_shortcut_in(&user_programs_dir()?, exe)
}

/// 只写当前用户开始菜单（不需要提权）。
///
/// `shortcut` 子命令用这条：开发态下 DLL 直接注册 dist 目录、从没跑过 `install`，
/// 想给用户一个「Win 搜得到」的入口，这是唯一不弹 UAC 的路。
pub fn create_for_current_user(exe: &Path) -> Result<PathBuf, String> {
    create_settings_shortcut_in(&user_programs_dir()?, exe)
}

/// 真正的实现，目录由调用方给。
///
/// 拆出来是为了能测：公共开始菜单**只有提权进程能写**（非提权下
/// `Save` 返回 `E_ACCESSDENIED`），拿它当测试目标等于让 `cargo test`
/// 必须开着管理员窗口。测试里换成临时目录，验的是同一段 ShellLink 代码。
fn create_settings_shortcut_in(dir: &Path, exe: &Path) -> Result<PathBuf, String> {
    let lnk = dir.join(SHORTCUT_NAME);
    let exe_units = crate::reg::wide(&exe.to_string_lossy());
    let lnk_units = crate::reg::wide(&lnk.to_string_lossy());
    let desc_units = crate::reg::wide("亨利五笔 · 设置");
    // 工作目录设成 exe 所在目录：设置程序自己按 exe 路径找 `data/wubi86`，
    // 但它也会看一眼 cwd——把 cwd 摆正，从任何地方启动结果都一样。
    let dir_units = exe
        .parent()
        .map(|p| crate::reg::wide(&p.to_string_lossy()))
        .unwrap_or_else(|| crate::reg::wide(""));

    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("创建 ShellLink 失败：{e}"))?;
        link.SetPath(PCWSTR(exe_units.as_ptr()))
            .map_err(|e| format!("设置快捷方式目标失败：{e}"))?;
        if dir_units.len() > 1 {
            let _ = link.SetWorkingDirectory(PCWSTR(dir_units.as_ptr()));
        }
        let _ = link.SetDescription(PCWSTR(desc_units.as_ptr()));
        let _ = link.SetIconLocation(PCWSTR(exe_units.as_ptr()), 0);

        let file: IPersistFile = link
            .cast()
            .map_err(|e| format!("取 IPersistFile 失败：{e}"))?;
        file.Save(PCWSTR(lnk_units.as_ptr()), true)
            .map_err(|e| format!("写入 {} 失败：{e}", lnk.display()))?;
    }
    Ok(lnk)
}

/// 删快捷方式：公共与用户级两处都清。本来就不存在算成功——卸载要能重复跑。
pub fn remove_settings_shortcut() -> Result<(), String> {
    let mut first_err = None;
    for dir in [common_programs_dir(), user_programs_dir()]
        .into_iter()
        .flatten()
    {
        let lnk = dir.join(SHORTCUT_NAME);
        match std::fs::remove_file(&lnk) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                let msg = format!("删除 {} 失败：{e}", lnk.display());
                first_err.get_or_insert(msg);
            }
        }
    }
    match first_err {
        Some(msg) => Err(msg),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名字里必须有「亨利」，否则开始菜单搜索命中不了（这是它存在的唯一理由）。
    #[test]
    fn shortcut_name_is_searchable() {
        assert!(SHORTCUT_NAME.contains("亨利"));
        assert!(SHORTCUT_NAME.ends_with(".lnk"));
    }

    /// 公共开始菜单真的存在（不存在的话这台机器上写快捷方式必然失败，
    /// 而失败又只在安装日志里表现成一行警告）。
    #[test]
    fn common_programs_dir_exists() {
        let dir = common_programs_dir().expect("取公共开始菜单目录");
        assert!(dir.is_dir(), "{} 不是一个目录", dir.display());
    }

    /// 用户级开始菜单也必须解析得出来——它是非提权时的唯一退路。
    #[test]
    fn user_programs_dir_exists() {
        let dir = user_programs_dir().expect("取用户开始菜单目录");
        assert!(dir.is_dir(), "{} 不是一个目录", dir.display());
        // 退路必须在 %APPDATA% 下，否则说明拿错文件夹了。
        let appdata = std::env::var("APPDATA").expect("APPDATA");
        assert!(
            dir.starts_with(&appdata),
            "{} 不在 %APPDATA% 下",
            dir.display()
        );
    }

    /// 真的建一个 .lnk，再读回来看它是不是合法的快捷方式。
    ///
    /// 目标目录用临时目录而不是开始菜单：写公共那份只有提权进程能做，
    /// 拿它当测试目标等于要求 `cargo test` 必须开着管理员窗口。
    #[test]
    fn writes_a_valid_shortcut_file() {
        use windows::Win32::System::Com::STGM_READ;

        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            )
            .ok();
        }
        let dir = std::env::temp_dir().join("heinrich-shortcut-selftest");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");

        // 目标不必真的存在：`SetPath` 只存字符串，不校验文件。
        let exe = Path::new(r"C:\Heinrich\dist\heinrich-settings.exe");
        let lnk = create_settings_shortcut_in(&dir, exe).expect("建快捷方式");
        assert!(lnk.is_file(), "快捷方式没落盘");
        assert!(
            std::fs::metadata(&lnk).unwrap().len() > 0,
            "快捷方式是空文件"
        );

        // 用同一个 COM 类读回来：Load 能成功就说明文件是合法 .lnk（不是随便
        // 写了几百字节）。这一步是「写对了」与「写了个坏文件」的分界。
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .expect("再建一个 ShellLink");
            let file: IPersistFile = link.cast().expect("QI IPersistFile");
            let units = crate::reg::wide(&lnk.to_string_lossy());
            file.Load(PCWSTR(units.as_ptr()), STGM_READ)
                .expect("读回刚写的快捷方式");

            // Load 只说文件结构合法，「点开是不是那个 exe」得看 GetPath。
            // 只验元数据不验目标，`.lnk` 指向随便一个东西也能过——那是假绿。
            let mut buf = [0u16; 512];
            link.GetPath(&mut buf, std::ptr::null_mut(), 0)
                .expect("读快捷方式目标");
            let got = String::from_utf16_lossy(&buf);
            let got = got.trim_end_matches('\0');
            assert_eq!(got, exe.to_string_lossy(), "快捷方式指向的不是设置程序");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
