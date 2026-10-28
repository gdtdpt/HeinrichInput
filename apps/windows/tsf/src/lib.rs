//! 亨利输入法 · Windows TSF 前端（DLL）。
//!
//! ## 这是什么
//!
//! 一个**极薄**的 in-proc COM 服务器。它被 Windows 加载进每一个需要中文输入的
//! 宿主进程（记事本、Chrome 的每个 tab、explorer、VS Code…），负责：
//!
//! 1. 向 TSF 注册自己是一个文本服务；
//! 2. 接管（或放行）按键；
//! 3. 把按键转给常驻的 `heinrich-server`（M1.3 起）；
//! 4. 把候选窗画在自己进程里（M2.3 起）。
//!
//! 码表、排序**都不在这里**——这是 D1 的核心取舍：DLL 被注入到别人的进程里，
//! 任何 panic、任何依赖冲突、任何阻塞都会直接拖垮宿主应用。
//!
//! ## 依赖纪律（D8）
//!
//! 只允许 `windows`、`heinrich-tip-id`、`heinrich-proto`、`heinrich-ui`、
//! `heinrich-win-ui`——后两个是自有的候选窗 crate，不引入任何第三方运行时。
//! 想加别的库之前先问一句：它值得让每个宿主进程都多背一份吗。
//!
//! ## 现在做到哪（M3.4b）
//!
//! 缓冲 `a`–`y` + 中文标点，编码以**组合串**显示在文档里；每个字母键都向 server
//! 问一次候选（D37），候选窗贴在光标下方显示，空格 / 回车 / 数字键 1–9 / 点选
//! 都能上屏，`←/→` 与 PgUp/PgDn 翻页（D38），Esc 取消；单击 Shift 切中英。
//! 学习排序（M2.4，server 侧）、拼音混输与码提示（M3.3 / M3.4a）已接上；
//! `Ctrl+Shift+1..9` 弹码位键位图（M3.4b）。配色与无障碍、emoji 编辑仍在 M4。
//!
//! 模块分工：
//!
//! - [`keys`]：**纯逻辑**，不碰 Windows API。「吃不吃这个键」只在这里回答一次。
//! - [`ipc`]：命名管道客户端。键路径上**永远不超过 50 ms**，重连全在后台线程。
//! - [`preedit`]：组合串（显示 / 提交 / 取消）的编辑会话。
//! - [`caret`]：问光标矩形（`GetTextExt`）的只读会话。
//! - [`cand`]：候选窗的状态与生命周期（候选集、页码、懒建窗口）。
//! - [`keycap`]：码位键位图（`Ctrl+Shift+N`）——把一条候选的码摊成键帽。
//! - [`text_service`]：TSF 接口实现，只做搬运。

#[allow(unused_macros)]
#[macro_use]
mod log;

mod cand;
mod caret;
mod gate;
mod ipc;
mod keycap;
mod keys;
mod langbar;
mod launch;
mod notify;
mod preedit;
mod text_service;

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};

use windows::core::{
    implement, ComObjectInner, ComObjectInterface, IUnknown, Interface, BOOL, GUID, HRESULT,
};
use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, E_POINTER, HMODULE, S_FALSE, TRUE};
use windows::Win32::System::Com::{CoLockObjectExternal, IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::{DisableThreadLibraryCalls, GetModuleFileNameW};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

use crate::text_service::TextService;

/// 本 DLL 期望的 CLSID。注册器写进注册表的就是这个值。
fn tip_clsid() -> GUID {
    GUID::from_u128(heinrich_tip_id::TIP_CLSID)
}

/// 「谁在问我们要对象」——宿主 exe 名 + pid，进程内解析一次。
///
/// 排查「系统设置页的选项按钮为什么不出现」时这是唯一的线索：日志里出现
/// `SystemSettings.exe` 才说明页面真的走到了 COM 这一步；一行都没有，说明它
/// 在更早的地方就把我们筛掉了，补再多接口也没用。
fn who() -> &'static str {
    static WHO: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    WHO.get_or_init(|| {
        let mut buf = [0u16; 260];
        let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
        let full = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
        let exe = full.rsplit('\\').next().unwrap_or(&full);
        format!("{exe} pid={}", std::process::id())
    })
}

fn guid_text(g: &GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7]
    )
}

/// DLL 入口。
///
/// **这里只做两件事**：`DisableThreadLibraryCalls`——告诉系统「不需要给我
/// THREAD_ATTACH/DETACH 通知」，省掉每个线程创建时的无谓回调；把模块句柄存进
/// 一个 `static`——`ITfFnConfigure::Show` 要找到与 DLL 同目录的设置程序，而
/// `GetModuleFileNameW(None)` 拿到的是**宿主**的 exe（见 `launch` 的模块文档）。
///
/// 其余一概不做。`DllMain` 在 loader lock 下运行，此时加载 DLL、创建线程、
/// 等待其它锁都可能直接死锁整个进程。日志、COM 初始化、读码表都留到 `Activate`
/// 或首次调用时惰性完成。存一个整数是本函数里唯一新加的写操作，它不取锁、
/// 不分配、不调用任何 API。
#[no_mangle]
pub extern "system" fn DllMain(
    hmodule: *mut c_void,
    ul_reason_for_call: u32,
    _lp_reserved: *mut c_void,
) -> BOOL {
    if ul_reason_for_call == DLL_PROCESS_ATTACH {
        launch::remember_module(hmodule);
        unsafe {
            let _ = DisableThreadLibraryCalls(HMODULE(hmodule));
        }
    }
    TRUE
}

/// COM 类工厂入口。`CoCreateInstance(我们的 CLSID)` 最终走到这里。
///
/// 标成 `unsafe` 是为了把契约写清楚：这三个指针由 COM 运行时给出，
/// 有效性由调用方（系统）保证，本函数无从校验。Rust 侧没有调用者，
/// 这个标记对 ABI 没有影响——它只是不让 clippy 把「裸指针解引用」当作疏漏。
///
/// # Safety
///
/// 由 COM 运行时调用，必须保证 `rclsid` / `riid` 可读且 `ppv` 可写。
/// Rust 代码不要直接调用本函数，走 `CoCreateInstance`。
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    match catch_unwind(AssertUnwindSafe(|| -> HRESULT {
        if rclsid.is_null() || riid.is_null() || ppv.is_null() {
            return E_POINTER;
        }
        // 只认自己的 CLSID。宿主/系统可能拿别的 CLSID 问过来（例如枚举时复用同一个
        // DLL 路径），照单全收会让别人以为我们能提供那个类。
        if *rclsid != tip_clsid() {
            if !riid.is_null() {
                tsf_info!(
                    "DllGetClassObject 被问的是别的 CLSID：rclsid={} riid={}（{}）",
                    guid_text(&*rclsid),
                    guid_text(&*riid),
                    who()
                );
            }
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        tsf_info!(
            "DllGetClassObject rclsid={} riid={}（{}）",
            guid_text(&*rclsid),
            guid_text(&*riid),
            who()
        );
        let factory: IUnknown = ClassFactory.into_object().into_interface();
        factory.query(riid, ppv)
    })) {
        Ok(hr) => hr,
        Err(_) => {
            // 连日志都不敢保证安全（可能正是日志路径不可用），直接给失败码。
            windows::Win32::Foundation::E_FAIL
        }
    }
}

/// 能否卸载。
///
/// **永远返回 `S_FALSE`（不卸载）**，这是刻意的：TSF 会在任意时刻持有我们的
/// 接口指针，卸载时机由系统决定而我们无法精确知道「它是不是还攥着某张 vtable」。
/// 输入法 DLL 常驻一个进程的成本是一次性的几十 KB，而卸载早了就是宿主崩溃。
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

#[implement(IClassFactory)]
struct ClassFactory;

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: windows::core::Ref<'_, IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        // 聚合（punkOuter 非空）我们不支持：没有需要被聚合的理由。
        if !punkouter.is_null() {
            return Err(CLASS_E_CLASSNOTAVAILABLE.into());
        }
        if ppv.is_null() {
            return Err(E_POINTER.into());
        }
        if !riid.is_null() {
            tsf_info!(
                "CreateInstance riid={}（{}）",
                guid_text(unsafe { &*riid }),
                who()
            );
        }
        let service: IUnknown = TextService::new().into_object().into_interface();
        unsafe { service.query(riid, ppv).ok()? };
        Ok(())
    }

    fn LockServer(&self, flock: BOOL) -> windows::core::Result<()> {
        unsafe { CoLockObjectExternal(self.as_interface_ref(), flock.as_bool(), true) }
    }
}

/// 供 `heinrich-setup --status` 校验「DLL 导出了该导出的东西」。
///
/// 故意做成一个平凡的导出函数：注册器可以在不带 COM 注册的情况下
/// `LoadLibrary` 本 DLL 再取这个函数，用来区分「DLL 本身有问题」与
/// 「注册表写错了」这两种故障。
#[no_mangle]
pub extern "system" fn HeinrichTsfAbiVersion() -> u32 {
    // 1 = M1.0（观测按键）；2 = M1.1（缓冲 + 上屏 + 中英切换）；
    // 3 = M1.2（组合串）；4 = M1.3（接 server + 中文标点 + 全局中英模式）；
    // 5 = M2.3（候选窗：逐键查询、光标定位、数字键与点选、翻页）；
    // 6 = M4a7（ITfFnConfigure：系统设置里的「选项」能打开设置窗口）；
    // 7 = M4a8（语言栏按钮：任务栏角标「亨/英」，左键切中英、右键菜单开设置）。
    // 改动导出的语义时递增。
    7
}
