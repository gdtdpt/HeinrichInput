//! 收掉本会话里正在跑的 `heinrich-server.exe`。
//!
//! ## 为什么安装时要干这件事
//!
//! server 是**常驻**的（无连接 30 分钟才自己退），而它和 DLL 之间有协议版本
//! （`PROTO_VERSION`）与 ABI 约定。升级 dist 之后：
//!
//! - 新的 `heinrich_win.dll` 会连上**旧的** server；
//! - 旧 server 只认识旧协议，握手直接回 `Bye`：`协议版本不匹配`；
//! - DLL 于是每 200 ms 重连一次，而每次重连都会把「空闲计数」刷新掉 ⇒
//!   **旧 server 永远不会自己退出**，用户看到的是「输入法一个候选都不出」。
//!
//! 所以安装时顺手把它收掉：下一次按键会由 DLL 拉起新的 server（D1 的惰性拉起）。
//!
//! ## 只收本会话的
//!
//! 管道名与单实例互斥体都带会话 id（`\\.\pipe\heinrich-<sid>`），多用户同时登录时
//! 各会话有自己的 server。这里按 `ProcessIdToSessionId` 过滤，不去动别人的。

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, TerminateProcess, PROCESS_TERMINATE,
};

/// 要收的镜像名（小写比较）。
const TARGET: &str = "heinrich-server.exe";

/// 结束本会话里所有 `heinrich-server.exe`，返回收掉的 pid 列表。
///
/// 失败一律忽略：收不掉最多是「升级后要等旧 server 自己退」，
/// 不该让安装流程为此中断。
pub fn kill_server_in_this_session() -> Vec<u32> {
    let mut killed = Vec::new();
    let mut my_sid = 0u32;
    unsafe {
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut my_sid);
    }

    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return killed;
    };

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    if unsafe { Process32FirstW(snap, &mut entry) }.is_err() {
        let _ = unsafe { CloseHandle(snap) };
        return killed;
    }
    loop {
        let name = wide_to_string(&entry.szExeFile);
        if name.eq_ignore_ascii_case(TARGET) {
            let pid = entry.th32ProcessID;
            let mut sid = 0u32;
            let same_session =
                unsafe { ProcessIdToSessionId(pid, &mut sid) }.is_ok() && sid == my_sid;
            // 自己的进程不可能叫这个名字，但仍排除一下，避免误伤。
            let not_self = pid != 0 && pid != unsafe { GetCurrentProcessId() };
            if same_session && not_self && terminate(pid) {
                killed.push(pid);
            }
        }
        if unsafe { Process32NextW(snap, &mut entry) }.is_err() {
            break;
        }
    }
    let _ = unsafe { CloseHandle(snap) };
    killed
}

fn terminate(pid: u32) -> bool {
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) }) else {
        return false;
    };
    let ok = unsafe { TerminateProcess(handle, 0) }.is_ok();
    let _ = unsafe { CloseHandle(handle) };
    ok
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}
