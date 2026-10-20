# 亨利输入法 开发者指南（HeinrichInput）

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
cargo test -p heinrich-core                                  # 排序与查询语义
cargo test -p heinrich-core --test ranking                    # 集成测试（夹具在 tests/fixtures/mini）
cargo run --release -p heinrich-cli -- repl                   # 打字
cargo run --release -p heinrich-cli -- check --data data/wubi86
```

`tools/check.ps1` 全绿才算完成（`clippy -D warnings` 与 `fmt --check` 都是硬门槛）。

## 3. crate 结构与依赖方向

```
heinrich-tip-id   TSF 身份常量：CLSID / Profile GUID / LANGID / 显示名
                  / InstallLayoutOrTip 描述串                     （零依赖）
                     ↑                        ↑
heinrich-win         TSF DLL                   heinrich-setup      注册器
                     （只允许 windows 与 tip-id）  （windows 随意）
heinrich-scheme   编码表示 + 词库装载 + 索引 + 清单解析     （零依赖，无平台分支）
    ↑
heinrich-core     查询语义 + 排序 + 学习模型               （纯逻辑：不读时钟、不做文件 I/O）
    ↑
heinrich-cli      REPL / check                            （仅 M0 验收用，不进产品）
heinrich-gen      原始码表 → 方案数据 + 校验报告           （仅构建期用，不进产品）
```

后续（M1.3+）：`heinrich-proto`（DLL↔server 帧）、`heinrich-server`（常驻进程）、`heinrich-ui`（候选窗）。

**边界纪律**：`heinrich-core` 里出现 `cfg(windows)`、`std::fs`、`SystemTime::now()` 就说明分层错了。时间一律由调用方作为 Unix 秒传入——这是排序行为可测的前提。

**`heinrich-win` 的依赖纪律（D8）**：只允许 `windows` + `heinrich-tip-id`。它被注入到每个宿主进程，加一个库等于让 chrome 的每个 tab 都多背一份。

## 4. TSF 壳（M1.0）

### 4.1 三个组件

| 文件 | 作用 |
|---|---|
| `crates/heinrich-tip-id/src/lib.rs` | CLSID / Profile GUID / 描述串。**发布后不可改**，值由 `uuid5` 生成 |
| `crates/heinrich-win/src/lib.rs` | `DllMain` / `DllGetClassObject` / `DllCanUnloadNow` / `IClassFactory` |
| `crates/heinrich-win/src/text_service.rs` | `ITfTextInputProcessorEx` / `ITfKeyEventSink` / `ITfThreadMgrEventSink` |
| `crates/heinrich-win/src/log.rs` | 文件日志（多进程安全，见 D23） |
| `crates/heinrich-setup/src/main.rs` | `install` / `uninstall` / `status`，主动提权 |
| `crates/heinrich-setup/src/tsf.rs` | `RegisterProfile` / `RegisterCategory` / `InstallLayoutOrTip` / COM 探针 |

### 4.2 上手

```powershell
# 1) 构建 + 打包（dist/ 里 setup 与 DLL 必须同目录）
powershell -ExecutionPolicy Bypass -File tools\build.ps1

# 2) 注册（会弹 UAC）
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 install

# 3) 查状态（不需要管理员，随时可跑）
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 status

# 4) 看日志尾部
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 log

# 5) 反注册
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 uninstall
```

`tools\tsf.ps1` 只是 `dist\heinrich-setup.exe` 的外壳（自动请求管理员、装完顺手打日志），
也可以直接跑 exe：`dist\heinrich-setup.exe install|uninstall|status`。

`status` 的检查项对应注册的三个层次，**哪一项红就说明卡在哪一层**：

| 检查项 | 红了说明 |
|---|---|
| CLSID 键 / InprocServer32 / ThreadingModel | 第一层 COM 注册没写进去（权限？DLL 路径不对？） |
| 语言档（`CTF\TIP\…\LanguageProfile`） | `RegisterProfile` 失败（描述串过长？GUID 非法？） |
| 类别键 | `RegisterCategory` 失败 |
| **DLL 可加载且导出 `DllGetClassObject`** | DLL **自身**有问题（缺依赖 / 架构不对 / 导出被裁）。这一项**不经过注册表** |
| `CoCreateInstance + ITfTextInputProcessor` | **整条链**：注册表 → DLL 加载 → `DllGetClassObject` → `QueryInterface` |

最后一项是最有价值的：它真的 `CoCreateInstance` 一次。注册表全对但 DLL 缺依赖、或 `DllGetClassObject` 认错 CLSID，表现都是「列表里有名字、一切换就废」，只有探针能提前发现。

注意倒数两项的顺序是刻意的：**先查 DLL 自身，再查注册表**。两项都红时先修 DLL；只红最后一项，那才是注册表写错了。

### 4.3 M1.0 的验收（人工）

1. `设置 → 时间和语言 → 语言和区域 → 中文(简体，中国) → 语言选项 → 添加键盘 → 亨利五笔`
   —— 装完**不会自动**出现，Windows 要求用户确认一次。
2. 切到「亨利五笔」，在记事本里敲 `vbgd`。
   **应当照常输出英文字母**——M1.0 还不消费按键，这是正确的中间态。
3. 看 `%LOCALAPPDATA%\Heinrich\logs\tsf.log`：

```
2026-10-20 14:12:03.451 [11824] I #1 Activate tid=0 pid=11824 —— 按键 sink 与焦点 sink 已挂
2026-10-20 14:12:05.902 [11824] I #1 down vk=0x56 'V' - 
2026-10-20 14:12:06.114 [11824] I #1 down vk=0x42 'B' - 
```

有 `Activate` 行 = 激活成功；有 `down` 行 = 按键进来了。两者都出现即 M1.0 通过。

### 4.4 排查

| 现象 | 先看这里 |
|---|---|
| 语言列表里没有「亨利五笔」 | `status`；再看 `设置` 里是否还没「添加键盘」 |
| 有名字但切换后没反应、日志也没文件 | DLL 没被加载。多半是 `InprocServer32` 路径不对，或 32 位宿主（R13） |
| 日志里有 `Activate` 但没有 `down` | 按键 sink 没挂上/被抢。确认 `AdviseKeyEventSink` 的返回值 |
| 日志完全没有 | `HEINRICH_TSF_LOG=off`？`%LOCALAPPDATA%` 不可写？ |
| 宿主应用崩溃 | 先查是否 panic 跨了 FFI（D21）——日志里会有 `panic 被拦截于 …` |

日志级别：`HEINRICH_TSF_LOG=debug` 可看焦点事件等细节；`off` 关闭。

## 5. 数据管线

```
data/raw/wubi86.dict.yaml     Rime 词典：`...` 之后是 词 \t 码 \t 权重
        │  heinrich-gen
        ├─ 丢弃含 z 的编码（z 是万能键，677 条符号类）
        ├─ 分类：单字取「最长编码 = 全码」，更短的是简码；词组恒 4 码
        ├─ 排序：码升序 → 权重降序 → 词升序（保证字节级可复现）
        ▼
data/wubi86/lexicon.tsv       码 \t 词 \t 权重 \t 简码级别(0=全码)
data/wubi86/scheme.toml       清单 + lexicon.tsv 的 SHA-256
        │  heinrich-scheme::Scheme::load（校验哈希与条目数）
        ▼
内存索引：entries[] + sorted[]（按打包编码排序，前缀查询 = 一次二分）
```

**改动数据格式时同步改三处**：`heinrich-gen` 写出、`heinrich-scheme` 读入、`out/m0a-report.md` 的复现段落。

### 5.1 编码打包（`heinrich-scheme/src/code.rs`）

字母 `a..y` 映射为 1..25，每字母占 5 bit（位置 20/15/10/5），打包成 `u32`：

- 同前缀的码在 `u32` 上是**连续区间** ⇒ 前缀查询用两次 `partition_point`；
- 字母值不用 0 ⇒ 尾部零填充可无歧义地区分长度；
- 短码打包值天然小于同前缀长码 ⇒ 区间内「同长度段」在头部，`exact_indices` 再切一刀即可。

## 6. 已经踩过的坑（别再踩）

1. **单字全码不一定是 4 码。** 字根不足四个时补末笔识别码，补到 3 码也是全码（`好 = VBG`）。实测 3 码全码 3,427 字。所以「码短于 4 就是简码」是错的判据，正确判据是「同一字多条编码中最长者为全码」。
2. **候选集不能做前缀展开。** `v` 前缀下有 3,459 个候选——排序与翻页都会失去意义。五笔是定长编码，前缀只用于「空码兜底」。
3. **一级简码与全码没有前缀关系。** 一级简码是键位映射（`Q=我`、`V=发`），`我 = TRNT`。校验「简码是否全码前缀」时必须跳过 1 级，否则误报 4 条。
4. **Windows 上带中文的文本经常带 UTF-8 BOM。** 两个读取入口（`heinrich-scheme::read_text`、`heinrich-gen::read_text`）都会剥掉，且哈希只覆盖剥离后的内容——否则同一个文件会算出两个哈希。
5. **生成物里不许写时间戳**，也不许依赖 `HashMap` 遍历顺序。两次 `tools\gen.ps1` 必须得到同一个 SHA-256（当前 `95b85711…`）。
6. **`panic = "abort"` 对注入型 DLL 是致命的。** workspace 原来为省体积设了 `abort`，但 `heinrich-win` 跑在别的进程里——abort 等于「终止宿主」。已移除（D21）。注意：即使 `unwind`，panic 跨出 `extern "system"` 仍是 UB，每个 COM 方法必须自己 `catch_unwind`（见 `text_service.rs` 的 `guarded()`）。
7. **`#[implement]` 要求 `windows-core` 作为直接依赖。** 宏展开的代码按 `windows_core::` 绝对路径引用它；只依赖 `windows` 会报 `could not find windows_core in the list of imported crates`。
8. **`as_interface_ref()` 只在宏生成的 `_Impl` 上，不在你的原始结构体上。** 所以「挂 sink」这类需要把自己交给系统的代码，必须写在 `impl Xxx_Impl` 里（`_Impl` 对原结构体有 Deref，字段照样能访问）。
9. **`RegisterProfile` 的签名与 MSDN 不一致。** windows-rs 0.62 是 `(rclsid, langid, guidprofile, pchdesc: &[u16], pchiconfile: &[u16], uiconindex, hklsubstitute, dwpreferredlayout, benabledbydefault, dwflags)`——描述串和图标是**切片**，长度由运行时补，不要自己传 `cchDesc`。
10. **`InstallLayoutOrTip` 返回值是 `void`。** 写错描述串不会报错，只会静默不生效。所以描述串格式在 `heinrich-tip-id` 的单测里钉死了。
11. **控制台输出中文要 `SetConsoleOutputCP(65001)`。** 中文 Windows 默认代码页 936，直接打 UTF-8 全是乱码。另外表格对齐不能用 `{:<44}`——它数字符不数显示宽度，CJK 占两列会歪。

## 7. 排序参数（`heinrich-core/src/rank.rs`）

```
total = w + g × ln(1+n) × 0.5^(Δt/H)
  w : ln(1+权重)/ln(1+最大权重)，n=0 时 total=w ⇒ 首屏 = 码表序
  g : 0.35        单次使用约挪一位；g=1 时一次误选会跨 4–6 位
  H : 45 天       半衰期
  K : 1.5         越过首位的门槛（首位 = 基线序第一，不随挑战者浮动）
```

改参数先改 `tests/ranking.rs` 的期望值——那 10 个用例就是这套规则的规格说明。

## 8. 进度

| 里程碑 | 状态 |
|---|---|
| M0a 数据装配 | ✅ 完成（覆盖 100%，2–3 级简码例外 0.40%，可复现） |
| M0b 引擎 | ✅ 完成（26 单测，clippy/fmt 干净） |
| M1 TSF 壳 | 预研完成（`docs/design/tsf-preflight.md`），按 M1.0–M1.3 四步推进 |
| M1.0 键进得来 | ✅ 代码完成（注册 / 激活 / 按键只记录不消费 / 状态检查），**待真机注册验收** |
| M1.1 字上得去 | 缓冲 + 小码表 + `ITfEditSession` 上屏 |
| M1.2 组合串 | composition + 显示属性虚线 + 光标跟随 |
| M1.3 接 server | 命名管道 + 本地候选缓存 + `--debug-sleep` 零阻塞验证 |

**动手前必读**：`docs/design/tsf-preflight.md`。里面写了注册要写哪些键、必须实现哪些接口、15 条已知坑，以及「不要在 M1.0 通过之前写组合串与 UI 代码」的理由。
另外**不要照抄青简**——它 2026-09 才首发、Windows 壳未经装机验证。参考顺序固定为：官方文档判语义 → Weasel 判工程做法 → WindInput 判产品形态 → chewing-tsf 判 Rust 写法 → 青简判坑。
M1 的冒烟矩阵：记事本 / VS Code / Chrome / Edge / 资源管理器 / cmd，另加提权记事本与一个 32 位程序（见预研 §9 待实测 T2/T3）。
