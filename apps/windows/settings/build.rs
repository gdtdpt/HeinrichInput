//! 把 `app.manifest` 与图标资源合并进可执行文件。
//!
//! 清单只用链接器自带的能力（MSVC 的 `/MANIFEST:EMBED` + `/MANIFESTINPUT:`），
//! 不引 `embed-resource` / `winres` 之类的构建依赖。
//!
//! 图标绕不开 rc.exe：PE 的图标必须有 RT_ICON + RT_GROUP_ICON 资源，链接器
//! 不接受裸 .ico。这里直接调 Windows SDK 里的 rc.exe 编出 `.res`，再把它当
//! 一个输入文件交给链接器（`link.exe` 原生接受 `.res`）。整个过程零 crate 依赖，
//! 只是多一个「构建机要有 SDK」的前提——有 MSVC 工具链就一定有。
//!
//! 为什么非嵌不可（v1.72，D94）：安装包在注册表写的 `UninstallDisplayIcon`
//! 指向 `heinrich-settings.exe`，Windows「设置 → 应用」读的是这个 exe 的图标；
//! 无图标资源的 exe 在那里显示成一块默认色块。同一个图标也喂给开始菜单快捷
//! 方式、任务栏和 Alt+Tab。D44 当年「不给 DLL 嵌图标」说的是输入法列表
//! （那里有注册表 `IconFile` 这条替代路），exe 自身没有替代品。
//!
//! 为什么不是「随 exe 放一个同名 .manifest」：产物要能单文件拷贝即用，
//! 多一个文件就是多一种「拷漏了所以界面变回 Win95」的现场。
//!
//! `/MANIFESTUAC:NO` 是必须的：否则链接器会自己再生成一段 trustInfo，
//! 与清单里的 `requestedExecutionLevel` 撞成两份（链接器会直接报错）。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=app.manifest");

    // GNU 链的 ld 不认 /MANIFEST*，也没有 rc.exe；本项目只出 MSVC 产物。
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("app.manifest");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );
    println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:NO");

    embed_icon();
}

/// 编 `resources.rc` → `$OUT_DIR/app.res`，再把这个 .res 作为链接器输入。
fn embed_icon() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rerun-if-changed=resources.rc");
    println!(
        "cargo:rerun-if-changed={}",
        crate_dir.join("../../../data/icon/heinrich.ico").display()
    );

    // 找不到 rc.exe 不阻断构建：内网脱机机可能只装了链接器一半的工具链。
    // 代价是 exe 退回默认图标，check.ps1 会断言资源段来兜住这种静默退化。
    let Some(rc) = find_rc() else {
        println!(
            "cargo:warning=未找到 rc.exe（Windows SDK），跳过图标资源。\
             设 RC 环境变量可指定路径，例如 \
             set RC=C:\\Program Files (x86)\\Windows Kits\\10\\bin\\10.0.26100.0\\x64\\rc.exe"
        );
        return;
    };

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR 缺失"));
    let res = out_dir.join("app.res");
    let output = Command::new(&rc)
        .current_dir(&crate_dir)
        // /nologo：别在 cargo 输出里刷版本横幅。
        .arg("/nologo")
        .arg(format!("/fo{}", res.display()))
        .arg("resources.rc")
        .output()
        .unwrap_or_else(|e| panic!("无法运行 rc.exe（{}）：{e}", rc.display()));

    if !output.status.success() {
        panic!(
            "rc.exe 编译 resources.rc 失败（{}）：\n{}",
            rc.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // link.exe 直接吃 .res；路径带空格时要自己加引号，否则会被拆成两个参数。
    let arg = res.display().to_string();
    let arg = if arg.contains(' ') {
        format!("\"{arg}\"")
    } else {
        arg
    };
    println!("cargo:rustc-link-arg-bins={arg}");
}

/// 依次尝试：`RC` 环境变量 → Windows SDK 里版本最高的 `x64\rc.exe` → `PATH`。
///
/// 不从 `VSINSTALLDIR` 推：VS 自带的那份 rc.exe 也在 SDK 目录下，按 SDK 找
/// 能同时覆盖「只装 Build Tools」和「装了完整 VS」两种情况。
fn find_rc() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RC") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }

    for base in ["ProgramFiles(x86)", "ProgramFiles"] {
        let Some(base) = std::env::var_os(base) else {
            continue;
        };
        let bin = PathBuf::from(base)
            .join("Windows Kits")
            .join("10")
            .join("bin");
        if let Some(p) = highest_version_rc(&bin) {
            return Some(p);
        }
    }

    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join("rc.exe"))
        .find(|p| p.is_file())
}

/// `bin\10.0.26100.0\x64\rc.exe` 之类；按版本号数值比较取最大，不按字符串。
fn highest_version_rc(bin: &Path) -> Option<PathBuf> {
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for entry in std::fs::read_dir(bin).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let version: Vec<u64> = match name.split('.').map(str::parse::<u64>).collect() {
            Ok(v) => v,
            Err(_) => continue,
        };
        if version.len() < 3 {
            continue;
        }
        let rc = entry.path().join("x64").join("rc.exe");
        if !rc.is_file() {
            continue;
        }
        if best.as_ref().is_none_or(|(cur, _)| *cur < version) {
            best = Some((version, rc));
        }
    }
    best.map(|(_, p)| p)
}
