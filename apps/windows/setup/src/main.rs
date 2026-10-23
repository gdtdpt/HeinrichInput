//! 亨利输入法 · TSF 注册器。
//!
//! ```text
//! heinrich-setup install     注册（会请求管理员权限）
//! heinrich-setup uninstall   反注册（会请求管理员权限）
//! heinrich-setup status      检查当前注册状态（不需要管理员）
//! ```
//!
//! ## 为什么单独做一个注册器，而不是 regsvr32
//!
//! 写 CLSID 只是三件事里的一件，另外两件（语言档、类别）走 CTF 的 API。
//! Weasel（`WeaselSetup`）、chewing（`tsfreg`）、WindInput（`wind-installer`）
//! 三家都单独做了注册器，没有一家让用户手敲 `regsvr32`。
//!
//! ## 为什么不用 requireAdministrator 清单
//!
//! 那样连 `status` 都会弹 UAC。这里改成「只在真要写 HKLM 时自己提权重启」，
//! 平时查状态完全静默。

#![windows_subsystem = "console"]

mod reg;
mod shortcut;
mod tsf;

use std::ffi::c_void;
use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Console::SetConsoleOutputCP;
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

mod proc;

fn main() {
    // 控制台默认是系统代码页（中文 Windows 上是 936），直接打 UTF-8 会成乱码。
    unsafe {
        let _ = SetConsoleOutputCP(65001);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args
        .first()
        .map(|s| s.trim_start_matches('-').to_ascii_lowercase())
        .unwrap_or_default();

    let code = match cmd.as_str() {
        "install" => install(),
        "uninstall" | "remove" => uninstall(),
        "status" | "check" => status(),
        "shortcut" | "lnk" => shortcut_only(),
        "" | "help" | "h" => {
            usage();
            0
        }
        other => {
            eprintln!("未知命令：{other}\n");
            usage();
            2
        }
    };
    std::process::exit(code);
}

fn usage() {
    println!("亨利五笔 · TSF 注册器\n");
    println!("  heinrich-setup install     注册输入法（会请求管理员权限）");
    println!("  heinrich-setup uninstall   反注册（会请求管理员权限）");
    println!("  heinrich-setup status      检查注册状态（不需要管理员）");
    println!("  heinrich-setup shortcut    只建当前用户的开始菜单快捷方式（不需要管理员）");
    println!(
        "\n注意：本程序必须与 {} 放在同一目录。",
        heinrich_tip_id::DLL_FILE_NAME
    );
}

// ---------------------------------------------------------------------------
// 权限
// ---------------------------------------------------------------------------

fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// 提权重启自己。成功发起 UAC 后本进程就可以退出了。
fn relaunch_elevated() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("无法定位自身路径：{e}"))?;
    let params = std::env::args()
        .skip(1)
        .map(|a| {
            if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    let file = reg::wide(&exe.to_string_lossy());
    let params = reg::wide(&params);
    let verb = reg::wide("runas");

    let r = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            if params.len() > 1 {
                PCWSTR(params.as_ptr())
            } else {
                PCWSTR::null()
            },
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW 的返回值 <= 32 表示失败，但它不是错误码，只是「句柄太小」。
    if (r.0 as isize) <= 32 {
        return Err(format!(
            "无法请求管理员权限（ShellExecuteW 返回 {}）。\
             用户账户控制被取消，或当前账户不是管理员。",
            r.0 as isize
        ));
    }
    Ok(())
}

/// 写 HKLM 之前必须提权。已提权返回 `Ok(true)`；否则发起提权并返回 `Ok(false)`。
fn ensure_elevated(action: &str) -> Result<bool, String> {
    if is_elevated() {
        return Ok(true);
    }
    println!("{action} 需要写入 HKLM，正在请求管理员权限…");
    relaunch_elevated()?;
    println!("已弹出用户账户控制窗口，请在窗口中点「是」，然后按新窗口的提示操作。");
    Ok(false)
}

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

fn dll_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("无法定位自身路径：{e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "无法定位程序所在目录".to_string())?;
    let dll = dir.join(heinrich_tip_id::DLL_FILE_NAME);
    if !dll.is_file() {
        return Err(format!(
            "找不到 {}\n    请把 heinrich-setup.exe 与 {} 放在同一目录。",
            dll.display(),
            heinrich_tip_id::DLL_FILE_NAME
        ));
    }
    Ok(dll)
}

fn log_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default();
    base.join("Heinrich").join("logs")
}

/// `logs` 目录里最新的 `tsf-<yyyymmdd>.log`。
///
/// 日志按天分文件，所以「用户名下的日志」不是一个固定路径。这里不自己算日期：
/// 按修改时间挑最新的那个，跨天、跨机器都不用改代码。
fn latest_tsf_log() -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(log_dir()).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("tsf-") || !name.ends_with(".log") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(t, _)| modified > *t) {
            best = Some((modified, entry.path()));
        }
    }
    best.map(|(_, p)| p)
}

// ---------------------------------------------------------------------------
// install
// ---------------------------------------------------------------------------

fn install() -> i32 {
    println!("== 亨利五笔 · 注册 ==\n");

    match ensure_elevated("注册") {
        Ok(true) => {}
        Ok(false) => return 0,
        Err(e) => {
            eprintln!("✗ {e}");
            return 1;
        }
    }

    let dll = match dll_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("✗ {e}");
            return 1;
        }
    };
    let dll_str = dll.to_string_lossy().to_string();
    // 语言列表图标：优先同目录的 heinrich.ico（tools\make-icon.ps1 生成）。
    // 没它就退回 DLL——DLL 里没有图标资源，列表里会是一块空白，但至少路径有效；
    // 这比直接失败好，老布局的 dist 也能继续装。
    let icon_str = dll
        .parent()
        .map(|d| d.join(heinrich_tip_id::ICON_FILE_NAME))
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| dll_str.clone());

    // [0/6] 先把旧的 server 收掉：它和新 DLL 的协议版本对不上，留着会让新版本
    // 每次握手都吃一个 `Bye`，而它自己又因为「总有人来连」永远退不出去。
    print!("[0/6] 结束旧的 server … ");
    let killed = proc::kill_server_in_this_session();
    if killed.is_empty() {
        println!("没有在跑的");
    } else {
        println!(
            "已结束 {} 个（pid {}）",
            killed.len(),
            killed
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    if let Err(e) = com_init() {
        eprintln!("✗ COM 初始化失败：{e}");
        return 1;
    }

    // [1] COM 注册：CLSID → DLL。
    //
    // 必须排在 RegisterProfile 之前：语言档一旦存在就会出现在列表里，
    // 此时 CLSID 还没指对，用户切过去只会得到「切换失败」。
    let cls_key = heinrich_tip_id::clsid_key();
    let inproc_key = format!("{cls_key}\\InprocServer32");

    print!("[1/6] COM 类名 … ");
    if let Err(e) = reg::set_string(
        HKEY_LOCAL_MACHINE,
        &cls_key,
        None,
        heinrich_tip_id::DISPLAY_NAME,
    ) {
        println!("失败\n✗ {e}");
        return 1;
    }
    println!("完成");

    print!("[2/6] DLL 路径 … ");
    let inproc = reg::set_string(HKEY_LOCAL_MACHINE, &inproc_key, None, &dll_str).and_then(|()| {
        // Apartment：TSF 在宿主 UI 线程（STA）上调用我们。写成 Both 会让系统
        // 在任意线程调过来，而那些线程没有输入队列，`GetKeyState` 之类全会失真。
        reg::set_string(
            HKEY_LOCAL_MACHINE,
            &inproc_key,
            Some("ThreadingModel"),
            "Apartment",
        )
    });
    if let Err(e) = inproc {
        println!("失败\n✗ {e}");
        return 1;
    }
    println!("完成");

    // [3] 语言档
    print!(
        "[3/6] 语言档（{} / 0x{:04X}）… ",
        heinrich_tip_id::DISPLAY_NAME,
        heinrich_tip_id::LANGID_ZH_CN
    );
    match tsf::register_profile(&icon_str) {
        Ok(()) => println!("完成"),
        Err(e) => {
            println!("失败");
            eprintln!("✗ RegisterProfile 失败：{e}");
            return 1;
        }
    }

    // [4] 类别
    print!("[4/6] 能力类别（{} 项）… ", tsf::CATEGORIES.len());
    match tsf::register_categories() {
        Ok(()) => println!("完成"),
        Err(e) => {
            println!("失败");
            eprintln!("✗ RegisterCategory 失败：{e}");
            return 1;
        }
    }

    // [5] 挂进输入法列表。这一步无返回值，只能事后查。
    print!("[5/6] 加入输入法列表 … ");
    tsf::set_layout(true);
    println!("已提交");

    // [6] 开始菜单快捷方式。
    //
    // 它不是注册的一部分：失败只警告，不返回非零。理由是这个入口是**便利**，
    // 真正的「正规入口」是系统设置页的「选项」按钮（`ITfFnConfigure`），
    // 那条路不依赖任何 .lnk。
    print!("[6/6] 开始菜单快捷方式 … ");
    let settings_exe = dll
        .parent()
        .map(|dir| dir.join(heinrich_tip_id::SETTINGS_EXE_NAME));
    match settings_exe {
        Some(exe) if exe.is_file() => match shortcut::create_settings_shortcut(&exe) {
            Ok(lnk) => println!("完成\n      {}", lnk.display()),
            Err(e) => println!("跳过（{e}）"),
        },
        Some(exe) => println!("跳过（找不到 {}）", exe.display()),
        None => println!("跳过（定位不到 DLL 目录）"),
    }

    println!("\n== 校验 ==");
    let mut all_ok = true;
    all_ok &= check("注册表 InprocServer32", || {
        reg::get_string(HKEY_LOCAL_MACHINE, &inproc_key, None)
            .map(|v| v.filter(|s| s.eq_ignore_ascii_case(&dll_str)))
    });
    all_ok &= check("DLL 可加载且导出 DllGetClassObject", || {
        tsf::probe_dll_exports(&dll_str).map(|()| Some("ok".to_string()))
    });
    all_ok &= check("CLSID 可实例化，ITfTextInputProcessor 可用", || {
        tsf::probe_create_instance()
            .map(|()| Some("ok".to_string()))
            .map_err(|e| e.to_string())
    });
    // 这一项决定系统设置页「选项」按钮存不存在：Windows 拿不到
    // `ITfFnConfigure` 就不画那个按钮，并且**不会报任何错**。
    all_ok &= check(
        "ITfFnConfigure 可用（设置的「选项」按钮）",
        || {
            tsf::probe_fn_configure()
                .map(|()| Some("ok".to_string()))
                .map_err(|e| e.to_string())
        },
    );

    println!();
    if all_ok {
        println!("✓ 注册完成。\n");
        print_next_steps();
        0
    } else {
        println!("✗ 有检查未通过，请把上面的输出发给我。");
        1
    }
}

fn print_next_steps() {
    println!("接下来手工做三件事（M1.0 的验收）：");
    println!();
    println!("  1) 打开  设置 → 时间和语言 → 语言和区域 → 中文(简体，中国)");
    println!(
        "     → 语言选项 → 添加键盘 → 选「{}」",
        heinrich_tip_id::DISPLAY_NAME
    );
    println!("     （装完不会自动出现在任务栏，Windows 要求用户确认一次）");
    println!();
    println!(
        "  2) 切到「{}」，在记事本里敲 vbgd 几个字母。",
        heinrich_tip_id::DISPLAY_NAME
    );
    println!("     ⚠ 这时**应当照常输出英文字母**——M1.0 还不接管按键，这是正确的中间态。");
    println!();
    println!("  3) 看日志，确认每个按键都进来了：");
    println!(
        "     {} （按天一个文件，如 tsf-20261023.log）",
        log_dir().display()
    );
    println!();
    println!("卸载：heinrich-setup uninstall");
}

// ---------------------------------------------------------------------------
// uninstall
// ---------------------------------------------------------------------------

fn uninstall() -> i32 {
    println!("== 亨利五笔 · 反注册 ==\n");

    match ensure_elevated("反注册") {
        Ok(true) => {}
        Ok(false) => return 0,
        Err(e) => {
            eprintln!("✗ {e}");
            return 1;
        }
    }

    if let Err(e) = com_init() {
        eprintln!("✗ COM 初始化失败：{e}");
        return 1;
    }

    // 顺序：先从输入法列表摘掉，再撤销语言档与类别，最后删 COM 键。
    // 反过来做会留下「列表里有个条目，点了报错」的中间状态。
    print!("[1/6] 从输入法列表移除 … ");
    tsf::set_layout(false);
    println!("已提交");
    println!("      注意：如果中文下不再有任何输入法，Windows 可能把默认输入语言改成英文，");
    println!("      这是系统行为（chewing-tsf #553），需要手工切回。");

    print!("[2/6] 撤销语言档 … ");
    match tsf::unregister_profile() {
        Ok(()) => println!("完成"),
        Err(e) => println!("跳过（{e}）"),
    }

    print!("[3/6] 撤销能力类别 … ");
    match tsf::unregister_categories() {
        Ok(()) => println!("完成"),
        Err(e) => println!("跳过（{e}）"),
    }

    print!("[4/6] 删除 CTF\\TIP 键 … ");
    match reg::delete_tree(HKEY_LOCAL_MACHINE, &heinrich_tip_id::ctf_tip_key()) {
        Ok(()) => println!("完成"),
        Err(e) => println!("失败（{e}）"),
    }

    print!("[5/6] 删除 CLSID 键 … ");
    let mut ok = true;
    for key in [
        heinrich_tip_id::clsid_key(),
        // 32 位视图。当前没有 32 位 DLL，但清掉可以保证不留死条目。
        format!(
            "Software\\Classes\\WOW6432Node\\CLSID\\{{{}}}",
            heinrich_tip_id::TIP_CLSID_TEXT
        ),
    ] {
        if let Err(e) = reg::delete_tree(HKEY_LOCAL_MACHINE, &key) {
            eprintln!("\n  ✗ {e}");
            ok = false;
        }
    }
    println!("{}", if ok { "完成" } else { "部分失败" });

    // [6] 快捷方式。放在最后：它不影响任何注册状态，删不掉也只是留下一个
    // 指向不存在文件的 .lnk（点开报错），不值得让整个卸载失败。
    print!("[6/6] 删除开始菜单快捷方式 … ");
    match shortcut::remove_settings_shortcut() {
        Ok(()) => println!("完成"),
        Err(e) => println!("跳过（{e}）"),
    }

    println!("\n✓ 反注册结束。已经打开的程序里可能还留着一份已加载的 DLL，重启它们才会彻底卸载。");
    if ok {
        0
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

fn status() -> i32 {
    println!("== 亨利五笔 · 状态 ==\n");
    println!(
        "  提权        : {}",
        if is_elevated() {
            "是"
        } else {
            "否（查状态不需要）"
        }
    );
    println!(
        "  版本        : {} {}",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_NAME")
    );
    println!("  CLSID       : {{{}}}", heinrich_tip_id::TIP_CLSID_TEXT);
    println!(
        "  Profile     : {{{}}} / LANGID 0x{:04X}",
        heinrich_tip_id::PROFILE_ZH_CN_TEXT,
        heinrich_tip_id::LANGID_ZH_CN
    );
    println!("  显示名      : {}", heinrich_tip_id::DISPLAY_NAME);
    println!(
        "  布局描述串  : {}",
        heinrich_tip_id::layout_or_tip_descriptor()
    );
    println!();

    let local_dll = dll_path().ok();
    let inproc_key = format!("{}\\InprocServer32", heinrich_tip_id::clsid_key());

    println!("-- 注册状态 --");
    let mut ok = true;
    ok &= check("CLSID 键存在", || {
        if reg::key_exists(HKEY_LOCAL_MACHINE, &heinrich_tip_id::clsid_key()) {
            Ok(Some("ok".into()))
        } else {
            Ok(None)
        }
    });
    ok &= check("InprocServer32 指向的文件存在", || match reg::get_string(
        HKEY_LOCAL_MACHINE,
        &inproc_key,
        None,
    )? {
        Some(path) => {
            if PathBuf::from(&path).is_file() {
                Ok(Some(path))
            } else {
                Ok(None)
            }
        }
        None => Ok(None),
    });
    ok &= check("ThreadingModel = Apartment", || {
        match reg::get_string(HKEY_LOCAL_MACHINE, &inproc_key, Some("ThreadingModel"))? {
            Some(v) if v.eq_ignore_ascii_case("Apartment") => Ok(Some(v)),
            Some(v) => Err(format!("实际是「{v}」")),
            None => Ok(None),
        }
    });

    let lp_key = format!(
        "{}\\LanguageProfile\\0x{:08X}\\{{{}}}",
        heinrich_tip_id::ctf_tip_key(),
        heinrich_tip_id::LANGID_ZH_CN,
        heinrich_tip_id::PROFILE_ZH_CN_TEXT
    );
    ok &= check(
        "语言档已注册（CTF\\TIP\\…\\LanguageProfile）",
        || {
            if reg::key_exists(HKEY_LOCAL_MACHINE, &lp_key) {
                Ok(Some("ok".into()))
            } else {
                Ok(None)
            }
        },
    );
    ok &= check("描述串里的名字", || {
        match reg::get_string(HKEY_LOCAL_MACHINE, &lp_key, Some("Description"))? {
            Some(v) => Ok(Some(v)),
            None => Ok(None),
        }
    });
    ok &= check("类别键已写入", || {
        let cat_key = format!("{}\\Category", heinrich_tip_id::ctf_tip_key());
        if reg::key_exists(HKEY_LOCAL_MACHINE, &cat_key) {
            Ok(Some("ok".into()))
        } else {
            Ok(None)
        }
    });

    println!();
    println!("-- DLL 与 COM --");
    // 先查 DLL 自身，再查注册表指引的 COM。顺序有讲究：
    // 两项都失败时，先排除「DLL 坏了」，剩下的才是「注册表写错了」。
    ok &= check("DLL 可加载且导出 DllGetClassObject", || match &local_dll {
        Some(p) => tsf::probe_dll_exports(&p.to_string_lossy()).map(|()| Some("ok".into())),
        None => Ok(None),
    });
    if com_init().is_ok() {
        ok &= check("CoCreateInstance + ITfTextInputProcessor", || {
            tsf::probe_create_instance()
                .map(|()| Some("ok".into()))
                .map_err(|e| e.to_string())
        });
        // 这一项不是「功能多了没有」，它决定系统设置页画不画「选项」按钮。
        ok &= check("CoCreateInstance + ITfFnConfigure", || {
            tsf::probe_fn_configure()
                .map(|()| Some("ok".into()))
                .map_err(|e| e.to_string())
        });
    } else {
        println!("  ✗ COM 初始化失败，跳过探针");
        ok = false;
    }

    println!();
    println!("-- 文件 --");
    let row = |mark: char, label: &str, detail: String| {
        let pad = " ".repeat(COL_WIDTH.saturating_sub(display_width(label)));
        println!("  {mark} {label}{pad} {detail}");
    };
    match &local_dll {
        Some(p) => row('✓', "本目录 DLL", p.display().to_string()),
        None => row(
            '✗',
            "本目录 DLL",
            format!("缺少 {}", heinrich_tip_id::DLL_FILE_NAME),
        ),
    }
    match latest_tsf_log() {
        Some(log) => {
            let len = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
            row('✓', "日志", format!("{} （{} 字节）", log.display(), len));
        }
        None => row(
            '–',
            "日志",
            format!(
                "尚未生成（{} 里按天命名，如 tsf-20261023.log）",
                log_dir().display()
            ),
        ),
    }
    // 两个「从哪儿进设置」的入口。都缺也能用（直接双击 exe），只是体验差。
    let settings = local_dll
        .as_ref()
        .and_then(|d| d.parent())
        .map(|dir| dir.join(heinrich_tip_id::SETTINGS_EXE_NAME));
    match settings {
        Some(p) if p.is_file() => row('✓', "设置程序", p.display().to_string()),
        Some(p) => row('–', "设置程序", format!("缺少 {}", p.display())),
        None => row('–', "设置程序", "定位不到 DLL 目录".into()),
    }
    match shortcut::shortcut_path() {
        Ok(p) if p.is_file() => row('✓', "开始菜单入口", p.display().to_string()),
        Ok(p) => row('–', "开始菜单入口", format!("未创建（{}）", p.display())),
        Err(e) => row('–', "开始菜单入口", e),
    }
    row(
        '–',
        "日志级别",
        format!(
            "{}（设 HEINRICH_TSF_LOG=debug 可看焦点等细节）",
            std::env::var("HEINRICH_TSF_LOG").unwrap_or_else(|_| "info（默认）".into())
        ),
    );

    println!();
    if ok {
        println!("✓ 注册看起来是完整的。");
        println!(
            "  若语言列表里仍然没有「{}」，是还没在系统设置里添加键盘。",
            heinrich_tip_id::DISPLAY_NAME
        );
        0
    } else {
        println!("✗ 注册不完整。运行 heinrich-setup install 重新注册（需要管理员）。");
        1
    }
}

// ---------------------------------------------------------------------------

fn com_init() -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| format!("CoInitializeEx：{e}"))
    }
}

/// 只建「当前用户」开始菜单快捷方式，**不提权**。
///
/// `install` 那步建的是公共开始菜单（正统、所有用户可见），代价是要 UAC；
/// 开发态下 DLL 直接注册 dist 目录、从没跑过 `install`，这时想有个「Win 搜得到」
/// 的入口就只能写用户级。系统设置页那个「选项」按钮指望不上——它只给微软自家
/// 输入法画（见 `shortcut.rs` 顶部注释）。
fn shortcut_only() -> i32 {
    let exe = std::env::current_exe().map_err(|e| format!("定位自身路径失败：{e}"));
    let settings = exe.ok().and_then(|p| {
        p.parent()
            .map(|d| d.join(heinrich_tip_id::SETTINGS_EXE_NAME))
    });
    let settings = match settings {
        Some(p) if p.is_file() => p,
        Some(p) => {
            eprintln!("✗ 找不到设置程序 {}", p.display());
            return 1;
        }
        None => {
            eprintln!("✗ 定位不到安装目录");
            return 1;
        }
    };
    if let Err(e) = com_init() {
        eprintln!("✗ COM 初始化失败：{e}");
        return 1;
    }
    match shortcut::create_for_current_user(&settings) {
        Ok(lnk) => {
            println!("✓ 已创建 {}", lnk.display());
            println!("  按 Win 键输入「亨利」即可搜到（已打开的搜索要先关掉再开）。");
            0
        }
        Err(e) => {
            eprintln!("✗ {e}");
            1
        }
    }
}

/// 跑一项检查并打印结果。`Ok(Some(detail))` = 通过，`Ok(None)` = 未就绪，`Err` = 出错。
fn check(name: &str, f: impl FnOnce() -> Result<Option<String>, String>) -> bool {
    // 不能用 `{:<44}`：它按**字符数**补空格，而中日韩字符占两列，
    // 结果是一列列参差不齐。按显示宽度补。
    let pad = " ".repeat(COL_WIDTH.saturating_sub(display_width(name)));
    match f() {
        Ok(Some(detail)) => {
            println!("  ✓ {name}{pad} {detail}");
            true
        }
        Ok(None) => {
            println!("  ✗ {name}{pad} 未找到");
            false
        }
        Err(e) => {
            println!("  ✗ {name}{pad} {e}");
            false
        }
    }
}

const COL_WIDTH: usize = 46;

/// 终端显示宽度：全角字符算 2 列。
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let u = c as u32;
            let wide = matches!(u,
                0x1100..=0x115F      // 谚文字母
                | 0x2E80..=0x303E    // 部首、标点
                | 0x3041..=0x33FF    // 假名、注音、CJK 兼容
                | 0x3400..=0x4DBF    // 扩展 A
                | 0x4E00..=0x9FFF    // 基本汉字
                | 0xA000..=0xA4CF    // 彝文
                | 0xAC00..=0xD7A3    // 谚文音节
                | 0xF900..=0xFAFF    // 兼容汉字
                | 0xFE30..=0xFE6F    // 兼容形式
                | 0xFF00..=0xFF60    // 全角
                | 0xFFE0..=0xFFE6
                | 0x20000..=0x3FFFD  // 扩展 B 及以后
            );
            if wide {
                2
            } else {
                1
            }
        })
        .sum()
}
