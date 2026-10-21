//! `heinrich-cli`：在没有 TSF 壳的情况下先让引擎「能打字」。
//!
//! 两个子命令：
//! - `repl`：逐行模拟键盘——输入字母组成缓冲区、数字选词，打印候选与打分依据。
//!   这是 M0b 的唯一验收手段：词序是否按 §5 浮动，只能靠连续打字看出来。
//! - `check`：装载方案并复核完整性（哈希、计数、前缀索引、一级简码键位）。

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use heinrich_core::{Engine, LookupMode, MemoryStore, RankingConfig, Tier};
use heinrich_scheme::{Code, Scheme};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match argv.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => ("repl", &[][..]),
    };
    let data = take_data(rest).unwrap_or_else(|| PathBuf::from("data/wubi86"));

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
                repl(Engine::new(
                    scheme,
                    MemoryStore::new(),
                    RankingConfig::default(),
                ));
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
        "--help" | "-h" | "help" => {
            println!(
                "用法：\n  heinrich-cli repl  [--data <目录>]   交互打字（默认）\n  heinrich-cli check [--data <目录>]   方案完整性校验"
            );
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("未知子命令 `{other}`，用 `heinrich-cli --help` 查看用法");
            ExitCode::FAILURE
        }
    }
}

fn take_data(args: &[String]) -> Option<PathBuf> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--data" {
            return it.next().map(PathBuf::from);
        }
    }
    None
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
            for ch in line.chars() {
                if ch == '\'' {
                    if buffer.is_empty() {
                        println!("  （空缓冲区时 ' 输出标点 ，不进入编码）");
                        continue;
                    }
                    println!("  「'」标记：本段强制按拼音处理（M3 生效，M0 暂无拼音引擎）");
                    continue;
                }
                buffer.push(ch);
                if Code::parse(&buffer).is_none() {
                    println!("  编码 `{buffer}` 含非法字符或超长（五笔码只含 a–y，最长 4）");
                    buffer.pop();
                }
            }
            last = eng.candidates(&buffer, now_unix()).to_vec();
            print_candidates(&buffer, &last, eng.lookup_mode());
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
             :k <值>         设置晋升阈值 K（默认 1.5）\n  \
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
                        if c.throne_held {
                            "  ← 首位受保护"
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
        "k" | "gain" | "half" => {
            let Some(v) = arg.and_then(|a| a.parse::<f64>().ok()) else {
                println!("  需要一个数字");
                return Flow::Continue;
            };
            let mut cfg = eng.config();
            match head {
                "k" => cfg.promote_k = v,
                "gain" => cfg.learning_gain = v,
                _ => cfg.half_life_days = v,
            }
            eng.set_config(cfg);
            let c = eng.config();
            println!(
                "  K={} g={} H={} 天",
                c.promote_k, c.learning_gain, c.half_life_days
            );
        }
        other => println!("  未知命令 `:{other}`，用 `:help` 查看"),
    }
    Flow::Continue
}

fn print_candidates(buffer: &str, cands: &[heinrich_core::Candidate], mode: Option<LookupMode>) {
    if buffer.is_empty() {
        println!("  （空缓冲区）");
        return;
    }
    if cands.is_empty() {
        println!("  `{buffer}` 没有候选");
        return;
    }
    let hint = match mode {
        Some(LookupMode::PrefixFallback) => {
            "（该码本身无词条，以下为前缀候选 · 仅 CLI 调试，真机不显示，见 D45）"
        }
        _ => "",
    };
    println!("  `{buffer}` → {} 个候选{hint}", cands.len());
    for (i, c) in cands.iter().take(12).enumerate() {
        let marker = if c.throne_held { " ←" } else { "" };
        println!(
            "   {}. {:<6} {:<5} {}{}",
            i + 1,
            c.word,
            c.code,
            c.kind_label,
            marker
        );
    }
    if cands.len() > 12 {
        println!("   … 另有 {} 个", cands.len() - 12);
    }
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
