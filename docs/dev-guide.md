# 析字 开发者指南

> 面向改代码的人 / agent。使用者请看 `README.md`；规格与决策请看 `PROJECT.md`（单一事实源）。

## 1. 环境

| 项 | 要求 |
|---|---|
| Rust | stable ≥ 1.83（实测 1.92） |
| 平台 | Windows（M0 的代码本身跨平台，只有后续 TSF 壳限定 Windows） |
| 第三方依赖 | **M0 的四个 crate 全部零依赖**（只用 std）。新增依赖前先问：能不能用 std 写完 |

## 2. 命令

```powershell
powershell -ExecutionPolicy Bypass -File tools\fetch-data.ps1   # 拉原始数据 → data/raw/
powershell -ExecutionPolicy Bypass -File tools\gen.ps1          # 装配 → data/wubi86/ + out/m0a-report.md
powershell -ExecutionPolicy Bypass -File tools\check.ps1        # fmt + clippy + test + 数据校验
powershell -ExecutionPolicy Bypass -File tools\build.ps1        # release → dist/
```

单跑某一项：

```powershell
cargo test -p xizi-core                                  # 排序与查询语义
cargo test -p xizi-core --test ranking                    # 集成测试（夹具在 tests/fixtures/mini）
cargo run --release -p xizi-cli -- repl                   # 打字
cargo run --release -p xizi-cli -- check --data data/wubi86
```

`tools/check.ps1` 全绿才算完成（`clippy -D warnings` 与 `fmt --check` 都是硬门槛）。

## 3. crate 结构与依赖方向

```
xizi-scheme   编码表示 + 词库装载 + 索引 + 清单解析     （零依赖，无平台分支）
    ↑
xizi-core     查询语义 + 排序 + 学习模型               （纯逻辑：不读时钟、不做文件 I/O）
    ↑
xizi-cli      REPL / check                            （仅 M0 验收用，不进产品）
xizi-gen      原始码表 → 方案数据 + 校验报告           （仅构建期用，不进产品）
```

后续（M1+）：`xizi-proto`（DLL↔server 帧）、`xizi-server`（常驻进程）、`xizi-win`（TSF DLL，仅允许 `windows-rs` + `xizi-proto`）、`xizi-ui`（候选窗）。

**边界纪律**：`xizi-core` 里出现 `cfg(windows)`、`std::fs`、`SystemTime::now()` 就说明分层错了。时间一律由调用方作为 Unix 秒传入——这是排序行为可测的前提。

## 4. 数据管线

```
data/raw/wubi86.dict.yaml     Rime 词典：`...` 之后是 词 \t 码 \t 权重
        │  xizi-gen
        ├─ 丢弃含 z 的编码（z 是万能键，677 条符号类）
        ├─ 分类：单字取「最长编码 = 全码」，更短的是简码；词组恒 4 码
        ├─ 排序：码升序 → 权重降序 → 词升序（保证字节级可复现）
        ▼
data/wubi86/lexicon.tsv       码 \t 词 \t 权重 \t 简码级别(0=全码)
data/wubi86/scheme.toml       清单 + lexicon.tsv 的 SHA-256
        │  xizi-scheme::Scheme::load（校验哈希与条目数）
        ▼
内存索引：entries[] + sorted[]（按打包编码排序，前缀查询 = 一次二分）
```

**改动数据格式时同步改三处**：`xizi-gen` 写出、`xizi-scheme` 读入、`out/m0a-report.md` 的复现段落。

### 编码打包（`xizi-scheme/src/code.rs`）

字母 `a..y` 映射为 1..25，每字母占 5 bit（位置 20/15/10/5），打包成 `u32`：

- 同前缀的码在 `u32` 上是**连续区间** ⇒ 前缀查询用两次 `partition_point`；
- 字母值不用 0 ⇒ 尾部零填充可无歧义地区分长度；
- 短码打包值天然小于同前缀长码 ⇒ 区间内「同长度段」在头部，`exact_indices` 再切一刀即可。

## 5. 已经踩过的坑（别再踩）

1. **单字全码不一定是 4 码。** 字根不足四个时补末笔识别码，补到 3 码也是全码（`好 = VBG`）。实测 3 码全码 3,427 字。所以「码短于 4 就是简码」是错的判据，正确判据是「同一字多条编码中最长者为全码」。
2. **候选集不能做前缀展开。** `v` 前缀下有 3,459 个候选——排序与翻页都会失去意义。五笔是定长编码，前缀只用于「空码兜底」。
3. **一级简码与全码没有前缀关系。** 一级简码是键位映射（`Q=我`、`V=发`），`我 = TRNT`。校验「简码是否全码前缀」时必须跳过 1 级，否则误报 4 条。
4. **Windows 上带中文的文本经常带 UTF-8 BOM。** 两个读取入口（`xizi-scheme::read_text`、`xizi-gen::read_text`）都会剥掉，且哈希只覆盖剥离后的内容——否则同一个文件会算出两个哈希。
5. **生成物里不许写时间戳**，也不许依赖 `HashMap` 遍历顺序。两次 `tools\gen.ps1` 必须得到同一个 SHA-256（当前 `1b5c432c…`）。

## 6. 排序参数（`xizi-core/src/rank.rs`）

```
total = w + g × ln(1+n) × 0.5^(Δt/H)
  w : ln(1+权重)/ln(1+最大权重)，n=0 时 total=w ⇒ 首屏 = 码表序
  g : 0.35        单次使用约挪一位；g=1 时一次误选会跨 4–6 位
  H : 45 天       半衰期
  K : 1.5         越过首位的门槛（首位 = 基线序第一，不随挑战者浮动）
```

改参数先改 `tests/ranking.rs` 的期望值——那 10 个用例就是这套规则的规格说明。

## 7. 进度

| 里程碑 | 状态 |
|---|---|
| M0a 数据装配 | ✅ 完成（覆盖 100%，2–3 级简码例外 0.40%，可复现） |
| M0b 引擎 | ✅ 完成（26 单测，clippy/fmt 干净） |
| M1 TSF 壳 | 下一步：注册 Text Service、接管按键、用系统默认候选窗上屏 |

M1 的最大未知数是 TSF 兼容性，冒烟矩阵：记事本 / VS Code / Chrome / Edge / 资源管理器 / cmd。
