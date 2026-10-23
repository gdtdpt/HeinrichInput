//! 从输入法里打开设置窗口。
//!
//! ## 谁调这个
//!
//! 系统「设置 → 时间和语言 → 语言 → 语言选项 → 键盘 → 亨利五笔 → 选项」。
//! 那个「选项」按钮是 Windows 自己的输入法管理界面，它按微软文档的约定
//! `CoCreateInstance(我们的 CLSID, IID_ITfFnConfigure)` 拿到接口，再调
//! [`ITfFnConfigure::Show`]（实现见 `text_service`，本模块是它的实现体）。
//!
//! ## 三个必须做对的地方
//!
//! 1. **DLL 自己的路径不能用 `GetModuleFileNameW(None)`**。传 NULL 拿到的是
//!    **宿主进程的 exe**（notepad++、chrome…），不是我们。必须用 `DllMain`
//!    存下来的模块句柄，否则「找设置程序」会去找宿主的同目录。
//! 2. **已经开着就别再开一个**。`heinrich-settings` 没有单实例锁（D68 之后
//!    它就是普通窗口程序），连点两次「选项」会开出两个窗口，两边各存各的，
//!    后保存的把先保存的盖掉。所以先 `FindWindowW` 找、找到就还原并前置。
//! 3. **拉起后立刻返回**。`Show` 的文档写「对话框关闭前不要返回」，那是给
//!    模态对话框的写法；我们的窗口是独立顶层窗口，阻塞会把 SystemSettings
//!    挂在那个调用上（用户会觉得整个设置页卡死）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HMODULE};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Threading::{
    CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SetForegroundWindow, ShowWindowAsync, SW_RESTORE,
};

/// 本 DLL 的模块句柄，`DllMain` 里存一次。
///
/// 只存一个值，不加载 DLL、不创建线程、不取锁——放在 loader lock 下是安全的。
static DLL_MODULE: AtomicIsize = AtomicIsize::new(0);

/// `DllMain` 调用：记下自己的模块句柄。
pub fn remember_module(hmodule: *mut std::ffi::c_void) {
    DLL_MODULE.store(hmodule as isize, Ordering::Release);
}

/// UTF-16 + NUL。Win32 的 `PCWSTR` 都要它。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 本 DLL 所在目录。取不到（还没经过 `DllMain`）时为 `None`。
///
/// `pub(crate)`：语言栏按钮的「打开词库文件夹」也用它定位 `data`。
pub(crate) fn module_dir() -> Option<PathBuf> {
    let raw = DLL_MODULE.load(Ordering::Acquire);
    if raw == 0 {
        return None;
    }
    // 先按 260 试一次；`GetModuleFileNameW` 在缓冲不够时返回缓冲长度且截断，
    // 所以拿到的长度 == 缓冲长度 - 1 时扩容重试（路径长到 260 以上确实存在，
    // 内网把项目放在深层目录时很容易撞上）。
    let mut buf = vec![0u16; 260];
    loop {
        let len = unsafe { GetModuleFileNameW(Some(HMODULE(raw as *mut _)), &mut buf) } as usize;
        if len == 0 {
            return None;
        }
        if len < buf.len() {
            buf.truncate(len);
            let path = PathBuf::from(String::from_utf16_lossy(&buf));
            return path.parent().map(Path::to_path_buf);
        }
        buf.resize(buf.len() * 2, 0);
        if buf.len() > 32 * 1024 {
            return None;
        }
    }
}

/// 设置程序应该在哪：与本 DLL 同目录。
///
/// 拆成纯函数是为了能单测——路径规则错了在真机上只表现为「点了没反应」，
/// 排查成本远高于写一个断言。
fn settings_exe_in(dir: &Path) -> PathBuf {
    dir.join(heinrich_tip_id::SETTINGS_EXE_NAME)
}

/// 找设置程序并检查它真的在。返回值同时给出工作目录。
fn locate_settings() -> Result<(PathBuf, PathBuf), String> {
    let dir = module_dir().ok_or_else(|| "取不到 DLL 自身目录".to_string())?;
    let exe = settings_exe_in(&dir);
    if !exe.is_file() {
        return Err(format!("找不到设置程序：{}", exe.display()));
    }
    Ok((exe, dir))
}

/// 已经开着设置窗口就还原 + 前置，返回 `true`。
///
/// 判定「前置成功」的规矩见 dev-guide 的 Win32 踩坑：**前台身份与屏幕可见性
/// 是两个量**，最小化窗口被设成前台时 `GetForegroundWindow` 也认。所以这里
/// 先 `ShowWindowAsync(SW_RESTORE)` 还原（同步版碰上无响应的目标会把调用方
/// 一起卡住），再 `SetForegroundWindow`；调用方不必再校验，因为它的目标只是
/// 「用户能看见那个窗口」，还原本身已经达到。
fn focus_existing() -> bool {
    let class = wide(heinrich_tip_id::SETTINGS_CLASS);
    let hwnd = match unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) } {
        Ok(h) if !h.0.is_null() => h,
        _ => return false,
    };
    unsafe {
        let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
    }
    true
}

/// 打开设置窗口：优先复用已开的那个，否则拉起一个新进程。
///
/// 返回 `Err` 只用于**真的没法开**（DLL 目录取不到、exe 不在、CreateProcess
/// 失败）。调用方（`ITfFnConfigure::Show`）一律返回 `S_OK`：让系统的「选项」
/// 按钮弹一个错误框没有意义，用户该看到的是设置窗口；失败原因写进 tsf.log。
pub fn open_settings() -> Result<(), String> {
    if focus_existing() {
        tsf_debug!("设置窗口已开着，已还原并前置");
        return Ok(());
    }

    let (exe, dir) = locate_settings()?;
    let mut exe_units = wide(&exe.to_string_lossy());
    // `lpCommandLine` 必须可写（CreateProcessW 会就地改它），所以单独复制一份。
    let mut cmdline = exe_units.clone();
    let dir_units = wide(&dir.to_string_lossy());

    // `cb` 必须填：`CreateProcessW` 靠它认结构体版本，填 0 会直接失败。
    let si = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();

    unsafe {
        CreateProcessW(
            PCWSTR(exe_units.as_mut_ptr()),
            Some(PWSTR(cmdline.as_mut_ptr())),
            None,
            None,
            false,
            // 不给 CREATE_NEW_CONSOLE 之类的标志：设置程序自己是 GUI 子系统，
            // 继承宿主的环境（含 %APPDATA%）反而正是我们要的。
            PROCESS_CREATION_FLAGS(0),
            None,
            PCWSTR(dir_units.as_ptr()),
            &si,
            &mut pi,
        )
        .map_err(|e| format!("拉起 {} 失败：{e}", exe.display()))?;
        // 句柄不用就关掉，否则每次点「选项」泄漏两个。
        let _ = CloseHandle(pi.hProcess);
        let _ = CloseHandle(pi.hThread);
    }
    tsf_info!("已拉起设置程序（pid {}）", pi.dwProcessId);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 设置程序与 DLL 必须同目录——这是打包布局的约定，写错的表现是
    /// 「设置页点选项没反应」，真机上极难定位。
    #[test]
    fn settings_exe_sits_beside_dll() {
        let p = settings_exe_in(Path::new(r"C:\Heinrich\dist"));
        assert_eq!(
            p,
            PathBuf::from(r"C:\Heinrich\dist").join("heinrich-settings.exe")
        );
        assert_eq!(p.file_name().unwrap(), heinrich_tip_id::SETTINGS_EXE_NAME);
    }

    /// 没存过模块句柄时不能 panic、不能返回奇怪路径。
    #[test]
    fn module_dir_is_none_before_dllmain() {
        // 注意：测试进程里 DLL_MODULE 恒为 0（DllMain 不由测试运行时调用）。
        assert!(module_dir().is_none());
        assert!(locate_settings().is_err());
    }

    /// 类名两边必须一模一样：DLL 的 `FindWindowW` 与设置程序的 `RegisterClassW`。
    #[test]
    fn settings_class_is_the_one_window_registers() {
        assert_eq!(heinrich_tip_id::SETTINGS_CLASS, "HeinrichSettings");
    }
}
