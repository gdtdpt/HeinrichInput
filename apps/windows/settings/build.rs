//! 把 `app.manifest` 合并进可执行文件。
//!
//! 只用链接器自带的能力（MSVC 的 `/MANIFEST:EMBED` + `/MANIFESTINPUT:`），
//! 不引 `embed-resource` / `winres` 之类要再拉一个 rc.exe 的构建依赖：
//! `cargo build` 之外不需要任何额外工具，内网脱机机上也编得过。
//!
//! 为什么不是「随 exe 放一个同名 .manifest」：产物要能单文件拷贝即用，
//! 多一个文件就是多一种「拷漏了所以界面变回 Win95」的现场。
//!
//! `/MANIFESTUAC:NO` 是必须的：否则链接器会自己再生成一段 trustInfo，
//! 与清单里的 `requestedExecutionLevel` 撞成两份（链接器会直接报错）。

fn main() {
    println!("cargo:rerun-if-changed=app.manifest");

    // GNU 链的 ld 不认 /MANIFEST*；本项目只出 MSVC 产物，其余直接跳过。
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("app.manifest");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );
    println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:NO");
}
