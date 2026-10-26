//! 亨利输入法 · 常驻引擎宿主。
//!
//! ## 它解决什么问题
//!
//! `lexicon.tsv` 有 13.6 万条、2.5 MB，装载约 300 ms。TSF DLL 会被注入到**每一个**
//! 需要中文输入的进程（Chrome 的每个 tab、explorer、每个编辑器），在那里装载
//! 等于每个进程都卡 300 ms、每个进程都多背一份索引。
//!
//! 所以词库、排序、未来的候选窗和 SQLite 学习库都放这里，DLL 只做按键与协议搬运（D1）。
//!
//! ## 用法
//!
//! ```text
//! heinrich-server.exe                      # 常驻（正常形态，由 DLL 惰性拉起）
//! heinrich-server.exe --debug-sleep 5000   # 每次查询前睡 5 秒，用来验 D18 零阻塞
//! heinrich-server.exe --idle-exit 0        # 不自动退出，调完手工结束
//! heinrich-server.exe --no-pinyin          # 关掉混输，退回纯五笔
//! heinrich-server.exe --hint off           # 临时关掉码提示（压过配置文件）
//! heinrich-server.exe --selftest           # 装载词库 + 跑几条查询后退出（无管道）
//! ```
//!
//! 日志：`%LOCALAPPDATA%\Heinrich\logs\server.log`。

#[allow(unused_macros)]
#[macro_use]
mod log;

mod host;
mod store;

#[cfg(windows)]
mod pipe;

use std::path::PathBuf;
use std::time::Duration;

use heinrich_core::scheme::HintMode;
use host::Host;

struct Args {
    scheme_dir: Option<PathBuf>,
    /// 拼音数据目录。`None` = 纯五笔（`--no-pinyin`，或默认目录不存在）。
    pinyin_dir: Option<PathBuf>,
    /// 是否由用户显式指定过拼音目录。默认目录不存在时静默按纯五笔跑，
    /// 而显式指定的目录装坏了要**留一条错误日志**——两者都得知道。
    pinyin_explicit: bool,
    /// 学习库路径。`Some` 才是落盘；`--memory-db` 或 `--selftest` 用内存。
    db: Option<PathBuf>,
    /// 用户配置文件。`Some` 才会去读；`--no-config` 或 `--selftest` 为 `None`。
    config: Option<PathBuf>,
    /// 自定义词库。`Some` 才会去读；`--no-words` 或 `--selftest` 为 `None`。
    words: Option<PathBuf>,
    /// emoji 用户层。`Some` 才会去读；`--no-emoji` 或 `--selftest` 为 `None`。
    emoji: Option<PathBuf>,
    /// 命令行强制的码提示模式，压过配置文件（验三档时用）。
    hint: Option<HintMode>,
    debug_sleep: Duration,
    idle_exit: Duration,
    selftest: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = Args {
            scheme_dir: None,
            pinyin_dir: default_pinyin_dir(),
            pinyin_explicit: false,
            db: default_db_path(),
            config: default_config_path(),
            words: default_words_path(),
            emoji: default_emoji_path(),
            hint: None,
            debug_sleep: Duration::ZERO,
            idle_exit: Duration::from_secs(30 * 60),
            selftest: false,
        };
        let mut it = std::env::args().skip(1);
        while let Some(a) = it.next() {
            match a.as_str() {
                "--scheme-dir" => {
                    args.scheme_dir =
                        Some(PathBuf::from(it.next().ok_or("--scheme-dir 需要一个路径")?));
                }
                "--pinyin-dir" => {
                    args.pinyin_dir =
                        Some(PathBuf::from(it.next().ok_or("--pinyin-dir 需要一个路径")?));
                    args.pinyin_explicit = true;
                }
                "--no-pinyin" => {
                    args.pinyin_dir = None;
                    args.pinyin_explicit = true;
                }
                "--db" => {
                    args.db = Some(PathBuf::from(it.next().ok_or("--db 需要一个路径")?));
                }
                "--memory-db" => args.db = None,
                "--config" => {
                    args.config = Some(PathBuf::from(it.next().ok_or("--config 需要一个路径")?));
                }
                "--no-config" => args.config = None,
                "--words" => {
                    args.words = Some(PathBuf::from(it.next().ok_or("--words 需要一个路径")?));
                }
                "--no-words" => args.words = None,
                "--emoji" => {
                    args.emoji = Some(PathBuf::from(it.next().ok_or("--emoji 需要一个路径")?));
                }
                "--no-emoji" => args.emoji = None,
                "--hint" => {
                    let raw = it
                        .next()
                        .ok_or("--hint 需要一个值（mixed / first_time / off）")?;
                    args.hint = Some(HintMode::parse(&raw).ok_or_else(|| {
                        format!("--hint 只认 mixed / first_time / off，实际为 `{raw}`")
                    })?);
                }
                "--debug-sleep" => {
                    let ms: u64 = it
                        .next()
                        .ok_or("--debug-sleep 需要毫秒数")?
                        .parse()
                        .map_err(|_| "--debug-sleep 需要一个整数（毫秒）")?;
                    args.debug_sleep = Duration::from_millis(ms);
                }
                "--idle-exit" => {
                    let secs: u64 = it
                        .next()
                        .ok_or("--idle-exit 需要秒数")?
                        .parse()
                        .map_err(|_| "--idle-exit 需要一个整数（秒）")?;
                    args.idle_exit = Duration::from_secs(secs);
                }
                "--selftest" => args.selftest = true,
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => return Err(format!("不认识的参数：{other}")),
            }
        }
        Ok(args)
    }

    /// 真正交给 `Host::open` 的拼音目录。
    ///
    /// 默认目录不存在 ⇒ `None`，**不打日志**：源码树里跑 `cargo run` 时 dist 还没
    /// 组装出来，那不是错误。显式指定的目录原样交下去，装坏了由 `Host` 记错误并降级。
    fn resolved_pinyin_dir(&self) -> Option<PathBuf> {
        match &self.pinyin_dir {
            Some(d) if self.pinyin_explicit || d.is_dir() => Some(d.clone()),
            _ => None,
        }
    }
}

fn print_help() {
    println!("亨利输入法 · 常驻引擎宿主");
    println!();
    println!("  --scheme-dir <dir>   方案目录（默认 <exe 目录>\\data\\wubi86）");
    println!("  --pinyin-dir <dir>   拼音数据目录（默认 <exe 目录>\\data\\pinyin）");
    println!("  --no-pinyin          关掉混输，退回纯五笔");
    println!("  --db <file>          学习库路径（默认 %LOCALAPPDATA%\\Heinrich\\learn.db）");
    println!("  --memory-db          不落盘，学习数据只存在内存里（调试用）");
    println!("  --config <file>      用户配置（默认 %APPDATA%\\Heinrich\\config.toml）");
    println!("  --no-config          不读配置文件，全部取默认值");
    println!("  --words <file>       自定义词库（默认 %APPDATA%\\Heinrich\\words.tsv）");
    println!("  --no-words           不读自定义词库");
    println!("  --emoji <file>       emoji 用户层（默认 %APPDATA%\\Heinrich\\emoji.user.toml）");
    println!("  --no-emoji           不读 emoji 用户层（内置表照旧）");
    println!("  --hint <mode>        码提示：mixed / first_time / off（压过配置文件）");
    println!("  --debug-sleep <ms>   每次查询前故意睡若干毫秒（验零阻塞）");
    println!("  --idle-exit <secs>   无连接多久后退出，0 = 不退出（默认 1800）");
    println!("  --selftest           只装载词库并跑几条查询，然后退出");
}

/// 学习库默认位置：`%LOCALAPPDATA%\Heinrich\learn.db`。
///
/// 和日志同一个目录，但**不放 dist**：dist 可能装在 Program Files 下（只读），
/// 而且升级时整个目录会被替换——用户攒了几个月的手感数据不能跟着一起没了。
fn default_db_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("Heinrich").join("learn.db"))
}

/// 用户配置默认位置：`%APPDATA%\Heinrich\config.toml`（§7）。
///
/// 用 `APPDATA`（漫游）而不是 `LOCALAPPDATA`：配置文件是用户偏好，跟着账户走才对；
/// 学习库与日志是本机产物，留在 `LOCALAPPDATA`。
fn default_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("Heinrich").join("config.toml"))
}

/// 自定义词库默认位置：`%APPDATA%\Heinrich\words.tsv`（D65）。
///
/// 与 `config.toml` 同目录、同样用漫游：自定义词是**用户偏好**，换台机器还该有；
/// 学习库与本机手感绑定，才留在 `LOCALAPPDATA`。也正因为不在 dist 里，
/// 升级、卸载重装都不会把用户攒的词冲掉。
fn default_words_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("Heinrich").join("words.tsv"))
}

/// emoji 用户层默认位置：`%APPDATA%\Heinrich\emoji.user.toml`（§6.4）。
///
/// 后缀 `.user.` 是给排障用的：一眼能看出它**不是**随版本发布的
/// `<方案目录>\emoji.toml`，升级替换的是后者，前者永远不动。
fn default_emoji_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("Heinrich").join("emoji.user.toml"))
}

/// 默认方案目录：与 exe 同级的 `data/wubi86`。
///
/// 故意**不读注册表**：setup 已经保证 dist 目录里的相对结构，读注册表只会多一处
/// 可能与实际安装位置不一致的状态。（DLL 侧找 server 也走「同目录」这条规则。）
fn default_scheme_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("data").join("wubi86"))
}

/// 默认拼音数据目录：与 exe 同级的 `data/pinyin`。
///
/// 拼音是可选的：这一份数据比五笔词库大（65k 条），缺了照样能打字——只是变成
/// 纯五笔。所以它不参与「找不到就退出」的判断，缺了就当没装。
fn default_pinyin_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("data").join("pinyin"))
}

/// 混输自测（§6.1）。**装没装拼音词库是两种合法形态**，各自断言自己的行为：
///
/// - 没装：`hao` 只能是五笔（`h`/`a`/`o` 都是字根键），不能因为「看起来像拼音」就回空。
/// - 装了：`hao` 必须判成拼音，`vbg` 必须仍是五笔。这两条一起才能证明判定不是
///   「一律偏拼音」或「一律偏五笔」——只测一边的话，实现退化成常量也能通过。
fn selftest_mixed(host: &mut Host) -> i32 {
    use heinrich_proto::source;

    let reply = host.query("hao", 9);
    let src = if reply.source == source::PINYIN {
        "拼音"
    } else {
        "五笔"
    };
    println!("  hao    → 判定为{src}，{} 条候选", reply.entries.len());

    if !host.has_pinyin() {
        // 纯五笔形态：`hao` 若恰好是某个五笔码就给候选，否则空——但都绝不能是拼音。
        let ok = reply.source == source::WUBI;
        println!("  混输   → 未启用拼音词库，按纯五笔跑  {}", tick(ok));
        return if ok { 0 } else { 1 };
    }

    let mut failed = 0;
    // D48：`hao` 两边都命中，五笔必须占主段（「虚」是三级简码），
    // 拼音候选紧接在后面——而不是反过来把 61 条同音字顶到前面去。
    let lead_ok = reply.source == source::WUBI
        && reply.primary_len >= 1
        && reply.entries.len() > reply.primary_len as usize;
    println!(
        "  混输   → hao 五笔占主段（{} 条）+ 拼音跟随  {}",
        reply.primary_len,
        tick(lead_ok)
    );
    if !lead_ok {
        failed += 1;
    }

    // `'` 钉死成拼音：同一串码的五笔候选全部让开
    let marked = host.query("hao'", 9);
    let mark_ok = marked.source == source::PINYIN
        && !marked.entries.is_empty()
        && !marked.entries.iter().any(|e| e.word == "虚");
    println!("  混输   → hao' 只出拼音  {}", tick(mark_ok));
    if !mark_ok {
        failed += 1;
    }

    // 五笔没有这个词条时拼音接管（`xian` 在五笔词库里没有 4 码词条）
    let py_only = host.query("xian", 9);
    let py_ok = py_only.source == source::PINYIN && !py_only.entries.is_empty();
    println!("  混输   → xian 判拼音且有候选  {}", tick(py_ok));
    if !py_ok {
        failed += 1;
    }
    // 首屏与折叠区的分界必须落在结果长度之内，否则 DLL 会把整屏都当折叠区。
    let bound_ok = reply.primary_len as usize <= reply.entries.len();
    println!(
        "  混输   → 折叠分界不越界（{}）  {}",
        reply.primary_len,
        tick(bound_ok)
    );
    if !bound_ok {
        failed += 1;
    }
    // 反过来：五笔码不能被判成拼音（`v` 不是任何音节的起始）
    let wubi_reply = host.query("vbg", 1);
    let wubi_ok = wubi_reply.source == source::WUBI;
    println!("  混输   → vbg 仍判五笔  {}", tick(wubi_ok));
    if !wubi_ok {
        failed += 1;
    }
    failed
}

fn tick(ok: bool) -> &'static str {
    if ok {
        "✓"
    } else {
        "✗"
    }
}

/// 自测用的一行结果打印 + 计数。
fn check(name: &str, ok: bool, detail: String) -> i32 {
    println!("  {name:<28} {detail}  {}", tick(ok));
    if ok {
        0
    } else {
        1
    }
}

/// 字根表自测（§6.7）。这一帧是 `Ctrl+Shift+N` 的唯一数据来源，
/// 它错了键位图就画不出来——而那种失败在真机上表现为「按了没反应」，最难查。
fn selftest_roots(host: &mut Host) -> i32 {
    let Some(heinrich_proto::Frame::Roots { keys }) = host.roots_frame() else {
        println!("  字根表 → 未装载  ✗");
        return 1;
    };
    let mut failed = 0;

    // ① 25 个字母键齐全、无重复
    let mut letters: Vec<char> = keys.iter().map(|k| k.key).collect();
    letters.sort_unstable();
    letters.dedup();
    failed += check(
        "字根表键位",
        keys.len() == 25 && letters.len() == 25,
        format!("{} 键，{} 个不同字母", keys.len(), letters.len()),
    );

    // ② 每个键都能画：一级简码是单字，字根非空，主字根个数不越界
    let bad: Vec<char> = keys
        .iter()
        .filter(|k| {
            k.roots.chars().count() == 0
                || k.main_len as usize > k.roots.chars().count()
                || k.primary == '\0'
        })
        .map(|k| k.key)
        .collect();
    failed += check(
        "字根表可绘制",
        bad.is_empty(),
        if bad.is_empty() {
            "每键都有字根、简码、合法主字根数".to_string()
        } else {
            format!("异常键：{}", bad.iter().collect::<String>())
        },
    );

    // ③ 一级简码与码表一致：`v` → 发、`b` → 了、`g` → 一（§6.7 的第五项校验
    //    在这里再走一遍真实路径，防止「roots.toml 改了、码表没跟上」）
    let mut mismatch = Vec::new();
    for want in [('v', '发'), ('b', '了'), ('g', '一'), ('k', '中')] {
        match keys.iter().find(|k| k.key == want.0) {
            Some(k) if k.primary == want.1 => {}
            Some(k) => mismatch.push(format!("{} 应为 {} 实为 {}", want.0, want.1, k.primary)),
            None => mismatch.push(format!("缺键 {}", want.0)),
        }
    }
    failed += check(
        "一级简码抽查",
        mismatch.is_empty(),
        if mismatch.is_empty() {
            "v/b/g/k 与码表一致".to_string()
        } else {
            mismatch.join("；")
        },
    );

    failed
}

/// emoji 自测（§6.4 / D66）：词组戴、单字不戴。
///
/// 方案目录里没有 `emoji.toml` 不是失败——那是「这份数据没装」的正常降级，
/// 与拼音词库缺失同一个口径：跳过并说清楚，不假装通过。
fn selftest_emoji(host: &mut Host) -> i32 {
    let count = host.emoji_count();
    if count == 0 {
        println!("  emoji    → 方案目录里没有 emoji.toml，跳过");
        return 0;
    }
    let mut failed = 0;
    println!("  emoji    → 生效 {count} 条");

    // ① 词组：`wqvb` → 你好，内置表里是 👋
    let word = host.query("wqvb", 3);
    let got = word
        .entries
        .iter()
        .find(|e| e.word == "你好")
        .map(|e| e.emoji.as_str());
    failed += check(
        "词组带 emoji",
        got == Some("👋"),
        format!("wqvb 你好 → {:?}", got.unwrap_or("（无此词）")),
    );

    // ② 单字：`vbg` → 好，一个字都不该挂（§6.4：一行好几个单字，挂上会加宽行）
    let single = host.query("vbg", 9);
    let dirty: Vec<&str> = single
        .entries
        .iter()
        .filter(|e| !e.emoji.is_empty())
        .map(|e| e.word.as_str())
        .collect();
    failed += check(
        "单字不挂 emoji",
        dirty.is_empty(),
        if dirty.is_empty() {
            format!("{} 条单字候选全部无 emoji", single.entries.len())
        } else {
            format!("不该挂的：{}", dirty.join(" / "))
        },
    );
    failed
}

/// 码提示自测（§6.3）。三档必须**各有不同的可观测行为**，否则「配置项存在但不生效」
/// 这种状态也会被当成通过。
///
/// 全部用 `hao` 这一族输入来验：它同时有两种形态——`hao'` 是纯拼音（带 `'`）、
/// `vbg` 是纯五笔，两边的码提示必须相反。
fn selftest_hint(host: &mut Host) -> i32 {
    let mut failed = 0;
    println!(
        "  码提示   → 启动模式 {}（下面逐个覆盖）",
        host.hint_mode().as_str()
    );

    // ① 默认（mixed）：纯拼音全带码，纯五笔一条都不带
    host.configure_hint(None, None);
    let wubi = host.query("vbg", 9);
    let none_coded = wubi.entries.iter().all(|e| e.code.is_empty());
    failed += check(
        "mixed → 五笔段不带码",
        none_coded,
        format!("{} 条，无一带码", wubi.entries.len()),
    );

    // 没有拼音词库时，「拼音段」根本不存在，下面几条无从验起——直接说清楚，
    // 不假装通过。纯五笔形态下「不带码」已经由上面那条覆盖。
    if !host.has_pinyin() {
        println!("  码提示   → 未装拼音词库，跳过拼音段的三档断言");
        return failed;
    }

    let py = host.query("hao'", 9);
    let all_coded = !py.entries.is_empty() && py.entries.iter().all(|e| !e.code.is_empty());
    let hao = py
        .entries
        .iter()
        .find(|e| e.word == "好")
        .map(|e| e.code.clone())
        .unwrap_or_default();
    failed += check(
        "mixed → 拼音段带码",
        all_coded && hao == "vbg",
        format!("{} 条全带码，「好」的码「{hao}」", py.entries.len()),
    );

    // 混输的两段：主段是五笔（不带码）、折叠区是拼音（带码）。这条是「按段裁决」
    // 唯一的直接证据——只看 `hao'` 的话，实现改成「整屏都带码」也能通过。
    let mixed = host.query("hao", 9);
    let split = mixed.primary_len as usize;
    let head_ok = mixed.entries[..split.min(mixed.entries.len())]
        .iter()
        .all(|e| e.code.is_empty());
    let tail_ok = mixed.entries[split.min(mixed.entries.len())..]
        .iter()
        .all(|e| !e.code.is_empty());
    let tail_any = mixed.entries.len() > split;
    failed += check(
        "mixed → 主段无码 / 折叠区有码",
        head_ok && tail_ok && tail_any,
        format!("主段 {split} 条、折叠区 {} 条", mixed.entries.len() - split),
    );

    // ② off：一条都不带
    host.configure_hint(None, Some(HintMode::Off));
    let off = host.query("hao'", 9);
    let off_ok = !off.entries.is_empty() && off.entries.iter().all(|e| e.code.is_empty());
    failed += check(
        "off → 一律不带码",
        off_ok,
        format!("{} 条，无一带码", off.entries.len()),
    );

    // ③ first_time：没上过屏的词带码；上过一次之后不再带
    host.configure_hint(None, Some(HintMode::FirstTime));
    let first = host.query("hao'", 9);
    let first_coded = first
        .entries
        .iter()
        .find(|e| e.word == "好")
        .is_some_and(|e| !e.code.is_empty());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    host.handle(heinrich_proto::Frame::Record {
        code: "hao".into(),
        word: "好".into(),
        at_unix: now,
    });
    let second = host.query("hao'", 9);
    let used_quiet = second
        .entries
        .iter()
        .find(|e| e.word == "好")
        .is_some_and(|e| e.code.is_empty());
    let others_coded = second
        .entries
        .iter()
        .filter(|e| e.word != "好")
        .all(|e| !e.code.is_empty());
    failed += check(
        "first_time → 只在没上过屏时带码",
        first_coded && used_quiet && others_coded,
        format!("首次带码={first_coded}，上屏后不带={used_quiet}"),
    );

    // 收尾恢复默认：后面的学习用例仍在用同一个 host，不该被这里的强制值影响。
    host.configure_hint(None, None);
    failed
}

fn main() {
    let args = match Args::parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误：{e}");
            print_help();
            std::process::exit(2);
        }
    };

    let dir = match args.scheme_dir.clone().or_else(default_scheme_dir) {
        Some(d) => d,
        None => {
            eprintln!("无法确定方案目录，请用 --scheme-dir 指定");
            std::process::exit(2);
        }
    };

    // 自测**不碰真实学习库**：`check.ps1` 每次都跑，跑一次就在用户的库里
    // 记上几十条假数据，等于悄悄改了他的词序。
    let db = if args.selftest { None } else { args.db.clone() };
    let pinyin_dir = args.resolved_pinyin_dir();
    let mut host = match Host::open(&dir, db.as_deref(), pinyin_dir.as_deref()) {
        Ok(h) => h,
        Err(e) => {
            srv_error!("{e}");
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // 自测同样不碰用户配置：`--selftest` 里三种模式由代码逐个切换，读真文件
    // 会让「同一份代码在两台机器上结论不同」——那正是自测要排除的东西。
    let config_path = if args.selftest {
        None
    } else {
        args.config.clone()
    };
    host.configure_hint(config_path, args.hint);

    // 自定义词库同理：自测不吃开发机上的 words.tsv，否则同一份代码在两台机器上
    // 结论不同。真机上若文件不存在，`reload_user_words` 就是一次空跑。
    let words_path = if args.selftest {
        None
    } else {
        args.words.clone()
    };
    host.configure_words(words_path);

    // emoji 用户层同理：自测不吃开发机上的绑定。
    let emoji_path = if args.selftest {
        None
    } else {
        args.emoji.clone()
    };
    host.configure_emoji(emoji_path);

    if args.selftest {
        let code = selftest(&mut host);
        std::process::exit(code);
    }

    #[cfg(windows)]
    {
        let cfg = pipe::Config {
            idle_exit: args.idle_exit,
            debug_sleep: args.debug_sleep,
        };
        std::process::exit(pipe::run(host, cfg));
    }

    #[cfg(not(windows))]
    {
        eprintln!("本程序当前只有 Windows 形态（命名管道传输）");
        std::process::exit(2);
    }
}

/// 无界面自测：装载成功 + 几条查询的答案对得上 + 学习数据真的参与了排序。
///
/// 与 DLL 的自测一样，这是给 `check.ps1` 用的——不需要真的开一个管道客户端。
/// 学习部分故意只用**内存库**：自测要可重复，不能读也不能写用户的真实库。
fn selftest(host: &mut Host) -> i32 {
    println!(
        "方案 {} · {} 条词条 · {}",
        host.scheme_id(),
        host.entry_count(),
        if host.has_pinyin() {
            "混输已启用"
        } else {
            "纯五笔（无拼音词库）"
        }
    );
    let mut failed = 0;
    let cases = [("vbg", "好"), ("q", "我"), ("vbgf", "好玩")];
    for (code, want) in cases {
        let got = host.query(code, 1);
        let first = got
            .entries
            .first()
            .map(|e| e.word.as_str())
            .unwrap_or("（无）");
        let ok = first == want;
        println!("  {code:<6} → {first}  {}", if ok { "✓" } else { "✗" });
        if !ok {
            failed += 1;
        }
    }
    // 无命中必须是空列表，不能是前缀兜底的结果
    let empty = host.query("abc", 9).entries.is_empty();
    println!("  abc    → 无命中  {}", if empty { "✓" } else { "✗" });
    if !empty {
        failed += 1;
    }

    // ---- 混输（§6.1）：判定语言标在结果上，且装没装拼音要走不同的路 ----
    failed += selftest_mixed(host);

    // ---- 码提示（§6.3）：提示跟随「这一段是怎么出来的」，三档各验一次 ----
    failed += selftest_hint(host);

    // ---- 字根表（§6.7）：键位图的唯一数据来源 ----
    failed += selftest_roots(host);

    // ---- emoji（§6.4 / D66）：词组戴、单字不戴 ----
    failed += selftest_emoji(host);

    // ---- 学习（§5）：第一次不计分，用够次数就该翻过来 ----
    // 记基线：上面的码提示用例也会回报一次上屏（first_time 需要它），
    // 那条记录不该算进「学习用例收了几条」。
    let recorded_before = host.recorded();
    let base: Vec<String> = host
        .query("vbg", 9)
        .entries
        .into_iter()
        .map(|e| e.word)
        .collect();
    if base.len() < 2 {
        println!("  学习   → 跳过（vbg 候选不足 2 条）");
        return if failed == 0 { 0 } else { 1 };
    }
    let front = base[0].clone();
    let challenger = base[base.len() - 1].clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    host.handle(heinrich_proto::Frame::Record {
        code: "vbg".into(),
        word: challenger.clone(),
        at_unix: now,
    });
    let after: Vec<String> = host
        .query("vbg", 9)
        .entries
        .into_iter()
        .map(|e| e.word)
        .collect();
    let held = after.first() == Some(&front);
    println!(
        "  学习   → 用一次「{challenger}」后首位仍是「{front}」  {}",
        if held { "✓" } else { "✗" }
    );
    if !held {
        failed += 1;
    }
    for _ in 0..19 {
        host.handle(heinrich_proto::Frame::Record {
            code: "vbg".into(),
            word: challenger.clone(),
            at_unix: now,
        });
    }
    let counted = host.recorded() - recorded_before == 20;
    println!(
        "  学习   → 收到 {} 条上屏回报（期望 20）  {}",
        host.recorded() - recorded_before,
        if counted { "✓" } else { "✗" }
    );
    if !counted {
        failed += 1;
    }
    // 连用 20 次就该翻过来（D58）。这一条最容易被误判成 bug：
    // 「㛅」的 `w` 只有 0.3714，比「好」低 0.59，但用满 20 次能加
    // `0.35 × ln 20 ≈ 1.05` 分，足够越过码表序。「用得多的词排前面」正是
    // 用户要的那个行为，所以这里断言的是**翻转**而不是保持。
    // 反方向（第一次不改顺序）在上一段已经验过了。
    let later: Vec<String> = host
        .query("vbg", 9)
        .entries
        .into_iter()
        .map(|e| e.word)
        .collect();
    let flipped = later.first() == Some(&challenger);
    println!(
        "  学习   → 连用 20 次后「{challenger}」升到首位  {}",
        if flipped { "✓" } else { "✗" }
    );
    if !flipped {
        failed += 1;
    }

    if failed == 0 {
        println!("自测通过");
        0
    } else {
        println!("自测失败 {failed} 项");
        1
    }
}
