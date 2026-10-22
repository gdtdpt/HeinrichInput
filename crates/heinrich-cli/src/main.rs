//! `heinrich-cli`：在没有 TSF 壳的情况下先让引擎「能打字」。
//!
//! 子命令：
//! - `repl`：逐行模拟键盘——输入字母组成缓冲区、数字选词，打印候选与打分依据。
//!   这是 M0b 的唯一验收手段：词序是否按 §5 浮动，只能靠连续打字看出来。
//! - `look`：查一次就退出（`look vbz` / `look "hao'vb"`），给脚本与回归用。
//! - `check`：装载方案并复核完整性（哈希、计数、前缀索引、一级简码键位）。

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use heinrich_core::{Engine, MemoryStore, Mixed, RankingConfig, Source, Tier};
use heinrich_scheme::{Code, PinyinLexicon, Roots, Scheme};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match argv.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => ("repl", &[][..]),
    };
    // 默认目录按子命令区分：`check-pinyin` 看的是拼音词库，其余看五笔词库。
    let default_data = if cmd == "check-pinyin" {
        PathBuf::from("data/pinyin")
    } else {
        PathBuf::from("data/wubi86")
    };
    let data = take_arg(rest, "--data").unwrap_or(default_data);
    let pinyin_dir = take_arg(rest, "--pinyin");

    match cmd {
        "repl" => match Scheme::load(&data) {
            Ok(scheme) => {
                let text = format!(
                    "亨利输入法 · {name} v{ver} · {} 条词条 · 数据来自 {src}（{lic}）",
                    scheme.entry_count(),
                    name = scheme.meta.name,
                    ver = scheme.meta.version,
                    src = scheme.meta.source,
                    lic = scheme.meta.license
                );
                println!("{text}");
                let mut eng = Engine::new(scheme, MemoryStore::new(), RankingConfig::default());
                if let Some(dir) = &pinyin_dir {
                    match PinyinLexicon::load(dir) {
                        Ok(lex) => {
                            println!(
                                "混输已启用 · 拼音 {} 条词条 / {} 个音节 · 数据来自 {}（Apache-2.0）",
                                lex.entry_count,
                                lex.syllable_count,
                                dir.display()
                            );
                            eng = eng.with_pinyin(lex);
                        }
                        Err(e) => {
                            eprintln!("拼音词库装载失败（{}）：{e}", dir.display());
                            return ExitCode::FAILURE;
                        }
                    }
                }
                repl(eng);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("装载失败：{e}");
                ExitCode::FAILURE
            }
        },
        "check" => match check(&data) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        "check-pinyin" => match check_pinyin(&data) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        "look" => {
            let args = positional(rest);
            let Some(code) = args.first().copied() else {
                eprintln!(
                    "用法：\n  heinrich-cli look <码> [--data <目录>] [--pinyin <目录>]\n\n  例：look vbz（通配）· look hao'vb（多段，注意加引号）"
                );
                return ExitCode::FAILURE;
            };
            match Scheme::load(&data) {
                Ok(scheme) => {
                    let mut eng = Engine::new(scheme, MemoryStore::new(), RankingConfig::default());
                    if let Some(dir) = &pinyin_dir {
                        match PinyinLexicon::load(dir) {
                            // 码提示的数据前提与 server 一致：装载后反查五笔全码
                            // （§6.3 / D64 的 chip 都吃这一列）。
                            Ok(mut lex) => {
                                lex.attach_wubi_codes(eng.scheme());
                                eng = eng.with_pinyin(lex);
                            }
                            Err(e) => {
                                eprintln!("拼音词库装载失败（{}）：{e}", dir.display());
                                return ExitCode::FAILURE;
                            }
                        }
                    }
                    let mixed = eng.mixed(code, now_unix());
                    print_candidates(code, mixed);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("装载失败：{e}");
                    ExitCode::FAILURE
                }
            }
        }
        "roots" => {
            let args = positional(rest);
            match args.first().copied() {
                Some("check") => match check_roots(&data) {
                    Ok(true) => ExitCode::SUCCESS,
                    Ok(false) => ExitCode::FAILURE,
                    Err(e) => {
                        eprintln!("{e}");
                        ExitCode::FAILURE
                    }
                },
                Some(code) => match show_roots(&data, code) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("{e}");
                        ExitCode::FAILURE
                    }
                },
                None => {
                    eprintln!(
                        "用法：\n  heinrich-cli roots <码>   逐位展开键位（如 `roots vbg`）\n  heinrich-cli roots check  校验字根表（25 键 / 130 条 / 助记 / 重复 / 一级简码）"
                    );
                    ExitCode::FAILURE
                }
            }
        }
        "--help" | "-h" | "help" => {
            println!(
                "用法：\n  heinrich-cli repl         [--data <目录>] [--pinyin <目录>]   交互打字（默认）\n  heinrich-cli look <码>    [--data <目录>] [--pinyin <目录>]   查一次候选（如 `look vbz`、`look hao'vb`）\n  heinrich-cli check        [--data <目录>]                     五笔方案完整性校验\n  heinrich-cli check-pinyin [--data <目录>]                     拼音词库校验\n  heinrich-cli roots <码>   [--data <目录>]                     逐位展开键位（如 `roots vbg`）\n  heinrich-cli roots check  [--data <目录>]                     字根表校验\n\n  --pinyin 给出拼音词库目录（如 data/pinyin）即启用混输判定；不给则纯五笔。"
            );
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("未知子命令 `{other}`，用 `heinrich-cli --help` 查看用法");
            ExitCode::FAILURE
        }
    }
}

fn take_arg(args: &[String], flag: &str) -> Option<PathBuf> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().map(PathBuf::from);
        }
    }
    None
}

/// 取出位置参数，跳过 `--flag <值>` 里的值与被忽略的游离标志。
fn positional(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut skip_value = false;
    for a in args {
        if skip_value {
            skip_value = false;
            continue;
        }
        if a.starts_with("--") {
            skip_value = true;
            continue;
        }
        out.push(a.as_str());
    }
    out
}

/// 终端里的显示宽度：ASCII 一列，其余两列。用于让键位表对齐。
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| if (c as u32) < 0x1100 { 1 } else { 2 })
        .sum()
}

fn pad_right(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(display_width(s))))
}

/// `roots <码>`：逐位展开成键位。纯文本，人看得懂，`check.ps1` 也能断言。
fn show_roots(dir: &Path, code: &str) -> Result<(), String> {
    let roots = Roots::load(dir).map_err(|e| e.to_string())?;
    if code.contains('z') {
        return Err("`z` 是学习键，没有字根（M3.5 才落地）".into());
    }
    let keys = roots
        .lookup(code)
        .ok_or_else(|| format!("`{code}` 里有不是 a–y 的字符"))?;
    println!("`{code}` 的键位：");
    for k in &keys {
        let shown: String = k.display_roots().collect();
        println!(
            "  {}  {}  一级简码 {}   {}",
            k.key,
            pad_right(&shown, 18),
            k.primary,
            k.mnemonic
        );
    }
    // §6.7 的固定说明：让它成为「可自查」，而不是「我替你判断」
    println!("\n  不足 4 码时末位可能是末笔识别码——取末笔笔画与字型。");
    Ok(())
}

/// `roots check`：四项结构校验 + 一项与码表的交叉校验。
fn check_roots(dir: &Path) -> Result<bool, String> {
    let roots = Roots::load(dir).map_err(|e| e.to_string())?;
    println!(
        "字根表：{} 键 / {} 条主字根（数据来自 {}）",
        roots.keys().len(),
        roots.total_roots(),
        dir.join("roots.toml").display()
    );
    let mut items = roots.validate();
    // 交叉项要码表；码表装不上就单独报，不让四项结构校验跟着一起失败
    match Scheme::load(dir) {
        Ok(scheme) => items.push(roots.check_primary(&scheme)),
        Err(e) => items.push(heinrich_scheme::CheckItem {
            name: "一级简码与码表一致",
            pass: false,
            detail: format!("码表装载失败：{e}"),
        }),
    }
    let mut ok = true;
    for item in &items {
        println!(
            "[{}] {}：{}",
            if item.pass { "通过" } else { "未通过" },
            item.name,
            item.detail
        );
        if !item.pass {
            ok = false;
        }
    }
    Ok(ok)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn repl(mut eng: Engine<MemoryStore>) {
    let mut buffer = String::new();
    let mut last: Vec<heinrich_core::Candidate> = Vec::new();
    println!("输入字母组成编码，数字选词；`:help` 看命令，`:q` 退出。\n");

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        print!("> {buffer:<6} ");
        let _ = io::stdout().flush();
        let Some(Ok(raw)) = lines.next() else { break };
        let line = raw.trim().to_string();
        if line.is_empty() {
            buffer.clear();
            continue;
        }

        if let Some(cmd) = line.strip_prefix(':') {
            match command(cmd, &mut eng, &mut buffer, &last) {
                Flow::Continue => {}
                Flow::Quit => break,
            }
            last = eng.candidates(&buffer, now_unix()).to_vec();
            continue;
        }

        // 纯数字 → 选词（1 起）
        if line.chars().all(|c| c.is_ascii_digit()) {
            let idx: usize = match line.parse::<usize>() {
                Ok(n) if n >= 1 => n - 1,
                _ => continue,
            };
            match eng.select(&buffer, idx, now_unix()) {
                Some(c) => {
                    println!("  上屏：{}（{}，{}）\n", c.word, c.code, c.kind_label);
                    buffer.clear();
                }
                None => println!("  没有第 {} 个候选\n", idx + 1),
            }
            last.clear();
            continue;
        }

        // 字母 / ' → 一行即一次完整组合（行式 REPL 无法逐键输入，故按「整体替换」）
        if line.chars().all(|c| c.is_ascii_lowercase() || c == '\'') {
            buffer.clear();
            eng.reset();
            // 有拼音词库时缓冲区的长度不受五笔码长约束——`haoping` 有 7 个字母。
            let mixed = eng.pinyin().is_some();
            for ch in line.chars() {
                if ch == '\'' {
                    if buffer.is_empty() {
                        println!("  （空缓冲区时 ' 输出标点「‘」，不进入编码）");
                        continue;
                    }
                    // `'` 是显式拼音标记（D48）：进缓冲、参与查询，core 会把
                    // 它当「这串按拼音解析」的开关。它占一个字符位但不占码长语义——
                    // 五笔码长校验只在没有拼音词库时跑，那时 `'` 也到不了这里。
                    buffer.push('\'');
                    continue;
                }
                buffer.push(ch);
                if !mixed && Code::parse(&buffer).is_none() && !wildcard_ok(&buffer) {
                    println!("  编码 `{buffer}` 含非法字符或超长（五笔码只含 a–y，最长 4）");
                    buffer.pop();
                }
            }
            last = eng.candidates(&buffer, now_unix()).to_vec();
            let m = eng.mixed(&buffer, now_unix());
            print_candidates(&buffer, m);
            continue;
        }

        println!("  无法识别的输入 `{line}`（`:help` 看用法）");
    }
}

enum Flow {
    Continue,
    Quit,
}

fn command(
    cmd: &str,
    eng: &mut Engine<MemoryStore>,
    buffer: &mut String,
    last: &[heinrich_core::Candidate],
) -> Flow {
    let mut parts = cmd.split_whitespace();
    let head = parts.next().unwrap_or("");
    let arg = parts.next();
    match head {
        "q" | "quit" | "exit" => return Flow::Quit,
        "r" | "reset" => {
            buffer.clear();
            eng.reset();
            println!("  （组合已取消）");
        }
        "help" | "h" | "?" => println!(
            "  :q              退出\n  \
             :r              取消当前组合\n  \
             :dump <码>      精确查看某编码下的词条\n  \
             :explain        打印当前候选的打分构成\n  \
             :pin <n>        把第 n 个候选设为固顶\n  \
             :suppress <n>   抑制第 n 个候选（永不出现）\n  \
             :clear-tier <n> 恢复第 n 个候选为学习档\n  \
             :stats          最常用的词\n  \
             :gain <值>      设置学习增益 g（默认 0.35）\n  \
             :half <天>      设置半衰期 H（默认 45）"
        ),
        "dump" => match arg.and_then(Code::parse) {
            Some(code) => {
                let hits = eng.scheme().exact_indices(&code);
                if hits.is_empty() {
                    println!("  `{code}` 无精确词条");
                } else {
                    println!("  `{code}` → {} 条", hits.len());
                    for &i in hits {
                        let e = eng.scheme().entry(i);
                        println!("    {}  w={}  {}", e.word, e.weight as i64, e.kind.label());
                    }
                }
            }
            None => println!("  需要一个 a–y 的合法编码"),
        },
        "explain" => {
            if last.is_empty() {
                println!("  （当前无候选）");
            } else {
                println!("   #  词      码      档位     n   Δt(天)   学习分    综合分");
                for (i, c) in last.iter().enumerate() {
                    let days = if c.days_since.is_infinite() {
                        "—".to_string()
                    } else {
                        format!("{:.1}", c.days_since)
                    };
                    println!(
                        "  {:>2}  {:<4}  {:<5}  {:<6} {:>3}  {:>6}  {:>7.4}  {:>7.4}{}",
                        i + 1,
                        c.word,
                        c.code,
                        format!("{:?}", c.tier),
                        c.n,
                        days,
                        c.learning,
                        c.total,
                        if c.n == 1 {
                            "  ← 首次上屏，不计分"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
        "pin" | "suppress" | "clear-tier" => {
            let idx = arg
                .and_then(|a| a.parse::<usize>().ok())
                .filter(|n| *n >= 1);
            let Some(idx) = idx else {
                println!("  用法：`:{head} <序号>`，序号从 1 开始（先打几个字母）");
                return Flow::Continue;
            };
            let Some(c) = last.get(idx - 1) else {
                println!("  没有第 {idx} 个候选");
                return Flow::Continue;
            };
            let tier = match head {
                "pin" => Tier::Pinned,
                "suppress" => Tier::Suppressed,
                _ => Tier::Learning,
            };
            eng.set_tier(&c.word, tier);
            println!("  `{}` → {:?}（下一次组合生效）", c.word, tier);
        }
        "stats" => {
            let top = eng.store().top(10);
            if top.is_empty() {
                println!("  （还没有上屏记录）");
            } else {
                println!("   词      次数  ");
                for (w, u, t) in top {
                    println!("   {w:<4}  {:>4}  {:?}", u.n, t);
                }
            }
        }
        "gain" | "half" => {
            let Some(v) = arg.and_then(|a| a.parse::<f64>().ok()) else {
                println!("  需要一个数字");
                return Flow::Continue;
            };
            let mut cfg = eng.config();
            match head {
                "gain" => cfg.learning_gain = v,
                _ => cfg.half_life_days = v,
            }
            eng.set_config(cfg);
            let c = eng.config();
            println!("  g={} H={} 天", c.learning_gain, c.half_life_days);
        }
        other => println!("  未知命令 `:{other}`，用 `:help` 查看"),
    }
    Flow::Continue
}

const PRINT_LIMIT: usize = 12;

/// 纯五笔下 `Code::parse` 会拒掉 `z`（它不是字根键），但 CLI 要能验 `vbz`
/// 这类通配（M3.5）：末位 `z` 且前面是合法码就放行。真正的通配判定在 core 里
/// （`wildcard_stem`），这里只是别让 repl 把它当成打字错误。
fn wildcard_ok(buffer: &str) -> bool {
    buffer
        .strip_suffix('z')
        .is_some_and(|stem| !stem.is_empty() && Code::parse(stem).is_some())
}

fn print_candidates(buffer: &str, m: &Mixed) {
    if buffer.is_empty() {
        println!("  （空缓冲区）");
        return;
    }
    let total = m.primary.len() + m.folded.len();
    if total == 0 {
        println!("  `{buffer}` 没有候选（既不是五笔码，也切不成拼音音节）");
        return;
    }
    println!(
        "  `{buffer}` → 判定 {} · {} 个候选",
        m.source.label(),
        total
    );
    if m.show_codes {
        println!("   （每条都带码：通配下，码本身就是筛选依据）");
    }
    let mut shown = 0usize;
    for c in m.primary.iter().take(PRINT_LIMIT) {
        shown += 1;
        let marker = if c.n == 1 { " ←首次" } else { "" };
        // 词中 chip（D64）：多段候选把 chip 画进词里（好[vbg]吃），
        // 纯文本视图用方括号插值模拟。
        let word = match &c.chip {
            Some(chip) => {
                let head: String = c.word.chars().take(chip.chars_before).collect();
                let tail: String = c.word.chars().skip(chip.chars_before).collect();
                format!("{head}[{}]{}", chip.code, tail)
            }
            None => c.word.clone(),
        };
        println!(
            "   {}. {:<6} {:<12} {}{}",
            shown, word, c.code, c.kind_label, marker
        );
    }
    if !m.folded.is_empty() {
        let other = match m.source {
            Source::Pinyin => "五笔",
            Source::Wubi => "拼音",
        };
        println!("   ── 以下是{other}候选（折叠区，不占首屏）──");
        for c in m.folded.iter().take(PRINT_LIMIT.saturating_sub(shown)) {
            shown += 1;
            println!(
                "   {}. {:<6} {:<12} {}",
                shown, c.word, c.code, c.kind_label
            );
        }
    }
    if total > shown {
        println!("   … 另有 {} 个", total - shown);
    }
}

/// 拼音词库校验：`check.ps1` 用，不依赖 TSF 壳。
///
/// 只校验「数据本身能用」，不校验排序——排序的正确性由 `heinrich-core` 的单测覆盖，
/// 这里重复一遍只会让两处慢慢分叉。
fn check_pinyin(dir: &Path) -> Result<bool, String> {
    let lex = PinyinLexicon::load(dir).map_err(|e| format!("装载失败：{e}"))?;
    println!(
        "拼音词库：{} 条（{} 个音节）",
        lex.entry_count, lex.syllable_count
    );
    println!("来源：{}（Apache-2.0）", dir.display());

    let mut ok = true;
    let mut line = |name: &str, pass: bool, detail: String| {
        println!(
            "[{}] {name}：{detail}",
            if pass { "通过" } else { "未通过" }
        );
        if !pass {
            ok = false;
        }
    };

    line(
        "音节表规模",
        lex.syllable_count >= 400,
        format!("{} 个", lex.syllable_count),
    );

    // 精确命中：`hao` 的第一条必须是「好」
    let hao = lex.match_indices("hao", 5);
    let first = hao.first().map(|&(i, _)| lex.entry(i).word.clone());
    line(
        "精确命中（hao → 好）",
        first.as_deref() == Some("好"),
        format!("实得 {:?}", first.unwrap_or_default()),
    );

    // 前缀命中：`haop` 未打完，必须能命中「好评」这类更长的词
    let haop = lex.match_indices("haop", 20);
    let has = haop.iter().any(|&(i, _)| lex.entry(i).word == "好评");
    line(
        "未打完的前缀（haop → 好评）",
        has,
        format!("{} 条", haop.len()),
    );

    // 切分判定：五笔码不该被判成拼音，否则真机里纯五笔输入会全落到拼音侧
    let vbg_is_pinyin = lex.can_split("vbg");
    line(
        "vbg 不是拼音",
        !vbg_is_pinyin,
        if vbg_is_pinyin {
            "v 被当成了音节起始 —— 音节表有问题".to_string()
        } else {
            "切分失败，符合预期".to_string()
        },
    );

    // 多音节与歧义切分
    line(
        "zhuang 可切分",
        lex.can_split("zhuang"),
        "最长音节".to_string(),
    );
    line(
        "xian 可切分",
        lex.can_split("xian"),
        "xi+an 或 xian".to_string(),
    );

    if ok {
        println!("拼音词库校验通过");
    }
    Ok(ok)
}

fn check(dir: &Path) -> Result<bool, String> {
    let scheme = Scheme::load(dir).map_err(|e| format!("装载失败：{e}"))?;
    let st = &scheme.stats;
    println!("方案：{} v{}", scheme.meta.name, scheme.meta.version);
    println!("来源：{}（{}）", scheme.meta.source, scheme.meta.license);
    println!("词库哈希：{}", scheme.meta.lexicon_sha256);
    println!(
        "词条：{}（单字 {} / 词组 {}）",
        st.total, st.single_char, st.multi_char
    );
    println!(
        "码长：1码 {} / 2码 {} / 3码 {} / 4码 {}",
        st.by_code_len[1], st.by_code_len[2], st.by_code_len[3], st.by_code_len[4]
    );
    println!(
        "简码：一级 {} / 二级 {} / 三级 {}",
        st.short_codes[1], st.short_codes[2], st.short_codes[3]
    );
    println!(
        "重码：{} 个编码有多个词，单编码最多 {} 个",
        st.shared_codes, st.max_shared
    );

    let mut ok = true;
    let mut line = |name: &str, pass: bool, detail: String| {
        println!(
            "[{}] {name}：{detail}",
            if pass { "通过" } else { "未通过" }
        );
        if !pass {
            ok = false;
        }
    };

    line(
        "清单与词库一致",
        scheme.meta.entry_count == scheme.entry_count(),
        format!("{} 条", scheme.entry_count()),
    );
    line(
        "一级简码键位",
        st.level1_keys == 25 && st.short_codes[1] == 25,
        format!("{} 键 / {} 条", st.level1_keys, st.short_codes[1]),
    );

    // 前缀索引与精确索引必须自洽：任一编码都能被自身前缀命中最少一次
    let mut prefix_ok = true;
    let mut exact_ok = true;
    for e in scheme.entries.iter().take(2000) {
        if !scheme
            .match_indices(e.code.as_str())
            .iter()
            .any(|&i| scheme.entry(i).code == e.code)
        {
            prefix_ok = false;
            break;
        }
        if !scheme.exact_indices(&e.code).iter().any(|&i| {
            let x = scheme.entry(i);
            x.code == e.code && x.word == e.word
        }) {
            exact_ok = false;
            break;
        }
    }
    line("前缀索引自洽", prefix_ok, "抽样 2000 条".into());
    line("精确索引自洽", exact_ok, "抽样 2000 条".into());

    Ok(ok)
}
