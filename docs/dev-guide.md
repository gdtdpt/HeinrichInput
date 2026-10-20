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
crates/              平台中立：换平台不该改这里，两个 target 都要编得过
  heinrich-tip-id    TSF 身份常量：CLSID / Profile GUID / LANGID / 显示名
                     / InstallLayoutOrTip 描述串                   （零依赖）
  heinrich-scheme    编码表示 + 词库装载 + 索引 + 清单解析         （零依赖，无平台分支）
      ↑
  heinrich-core      查询语义 + 排序 + 学习模型                   （纯逻辑：不读时钟、不做文件 I/O）
      ↑
  heinrich-cli       REPL / check                                （仅 M0 验收用，不进产品）
  heinrich-gen       原始码表 → 方案数据 + 校验报告               （仅构建期用，不进产品）

apps/windows/        平台专属产物：调 Windows API，或产物是注入件 / 注册器
  tsf/               TSF DLL，只依赖 windows + heinrich-tip-id     （D8）
  setup/             注册器，依赖 windows + heinrich-tip-id
```

**这个两层划分是硬约定，不是目录洁癖**。判据只有两条：调某个平台的系统 API，或产物是 `.dll` / `.ime` 这类注入件、平台注册器 ⇒ 进 `apps/<platform>/`；其余一律进 `crates/`。新增 crate 前先问「它在另一个平台上编得过吗」，答否就是走错了层。

`heinrich-tip-id` 是 Windows 身份常量，但它零依赖、且 DLL 与注册器共引（D22），所以留在 `crates/`；将来 macOS 的身份常量按同一模式另立 crate，不与它合并。

**M5 加平台时只新增平级的 `apps/macos/`、`apps/linux/`，`crates/` 不动**。届时构建脚本必须用 `-p` 显式指定产物——`cargo build --workspace` 会去编别的平台的壳、必然失败。

后续（M1.3+）：`heinrich-proto`（DLL↔server 帧）、`heinrich-server`（常驻进程）、`heinrich-ui`（候选窗）。

**边界纪律**：`heinrich-core` 里出现 `cfg(windows)`、`std::fs`、`SystemTime::now()` 就说明分层错了。时间一律由调用方作为 Unix 秒传入——这是排序行为可测的前提。

**`heinrich-win` 的依赖纪律（D8）**：只允许 `windows` + `heinrich-tip-id`。它被注入到每个宿主进程，加一个库等于让 chrome 的每个 tab 都多背一份。

## 4. TSF 壳（M1.0 / M1.1）

「薄壳」指的是**不含引擎、词库、SQLite、UI 绘制**，不是行数少。TSF 管线本身是重活：WindInput 的 C++ 壳约 1 MB 源码（`TextService.cpp` 385 KB、`KeyEventSink.cpp` 184 KB、`IPCClient.cpp` 114 KB、`LangBarItemButton.cpp` 60 KB），这些代码全部在 D8 的依赖纪律下活着。`heinrich-win` 到 M2 长到几千行是正常的，别拿行数当验收标准。

### 4.1 组件

| 文件 | 作用 |
|---|---|
| `crates/heinrich-tip-id/src/lib.rs` | CLSID / Profile GUID / 描述串。**发布后不可改**，值由 `uuid5` 生成 |
| `apps/windows/tsf/src/lib.rs` | `DllMain` / `DllGetClassObject` / `DllCanUnloadNow` / `IClassFactory` / `HeinrichTsfAbiVersion` |
| `apps/windows/tsf/src/keys.rs` | **纯逻辑**：`decide(mode, buffer_len, vk, mods) -> Action` + `ShiftTap`。不碰 Windows API，可在任意平台单测 |
| `apps/windows/tsf/src/tinydict.rs` | 20 条硬编码小码表。**M1.3 接 server 后整个删除** |
| `apps/windows/tsf/src/commit.rs` | `ITfEditSession`：`GetSelection` → `SetText` → `Collapse` → `SetSelection` |
| `apps/windows/tsf/src/text_service.rs` | TSF 接口实现，只做搬运；`OnTestKey*` 只回答、`OnKey*` 才执行 |
| `apps/windows/tsf/src/log.rs` | 文件日志（多进程安全，见 D23） |
| `apps/windows/setup/src/main.rs` | `install` / `uninstall` / `status`，主动提权 |
| `apps/windows/setup/src/tsf.rs` | `RegisterProfile` / `RegisterCategory` / `InstallLayoutOrTip` / COM 探针 |

### 4.1.1 按键处理的固定形状（D26）

```
OnTestKeyDown ─┐
               ├→ keys::decide() → Action ─┬→ Pass  → 返回 FALSE（宿主处理）
OnKeyDown ─────┘                            └→ 其余  → 返回 TRUE（我处理）
                                              └ OnKeyDown 里才改状态 / 上屏
```

- **`decide` 是唯一决策点**，两条路径共用，答案必须一致。
- **`OnTestKey*` 不碰编码缓冲**。它是问句，同一个键可能被问多次。
- **`OnKey*` 才执行**：改缓冲、上屏。
- **唯一的例外是 Shift 采样状态机**（`sync_shift` → `keys::ShiftTap`）：**四条回调都要调**。

那条例外是踩坑 #15 换来的：走 test 路径的宿主（Notepad++）**永远不调 `OnKeyDown`**，
把「Shift 按下就置位」写在那里，等于这一半逻辑根本不存在。状态机做成幂等的，
所以「先问再调」的宿主重复回调也不会切两次。

违反这条的症状是「某些程序能用、某些完全没反应」，或「一个字进去两次」。原因见踩坑 #12 与 #13 前面的实测表。

**日志级别**：M1.1 起按键退回 `debug`，`info` 只留 Activate / Deactivate / 提交 / 无命中 / 模式切换。
要逐键排查用 `powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 debug on`（写一个
`%LOCALAPPDATA%\Heinrich\debug.on` 标志文件），**改完必须重启要观察的宿主程序**——
级别在每个进程里只解析一次。环境变量 `HEINRICH_TSF_LOG` 仍然可用，且优先级更高。

### 4.1.2 缓冲与提交规则

| 情形 | 行为 |
|---|---|
| `a`–`y` | 吃掉，追加缓冲（上限 4 码） |
| `z` | **放行**。86 版 `z` 是万能学习键，M1.1 不实现，吃掉不出字等于丢键 |
| 空格 / 回车，缓冲非空 | 吃掉并提交 |
| 空格 / 回车，缓冲为空 | 放行（正常空格 / 换行） |
| `Esc`，缓冲非空 | 吃掉并丢弃缓冲 |
| `Esc`，缓冲为空 | 放行（很多程序用它关窗口） |
| `Backspace`，缓冲非空 | 吃掉并退一格 |
| `Backspace`，缓冲为空 | 放行（否则文档里已有的字删不掉） |
| 缓冲命中码表 | 上屏对应字/词 |
| **缓冲没命中** | **原样上屏缓冲的字母**（D27） |
| **已满 4 码还来字母** | 原样上屏 + 清空，该字母开始新组合（D27） |
| Ctrl / Alt 组合 | 一律放行 |
| Shift + 字母 | 放行，宿主输出大写 = 临时英文（D28） |
| 单击 Shift | 切换中英。判定靠**采样物理状态**而不是靠 vkey 上下，松开事件缺失也能在下一个按键上补判（`ShiftTap`） |

### 4.2 上手

> **先 `cd` 到仓库根目录再执行。** `powershell -File` 的路径参数是按**当前目录**解析的，
> 在别处跑 `-File tools\tsf.ps1` 会直接报
> `The argument 'tools\tsf.ps1' to the -File parameter does not exist.`
> 不想 `cd` 就用绝对路径：`-File C:\Users\<你>\Develop\Heinrich\tools\tsf.ps1`。

```powershell
# 0) 先进入仓库根目录
cd C:\Users\richard\Develop\Heinrich

# 1) 构建 + 打包（dist/ 里 setup 与 DLL 必须同目录）
powershell -ExecutionPolicy Bypass -File tools\build.ps1

# 2) 注册（会弹 UAC）
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 install

# 3) 查状态（不需要管理员，随时可跑）
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 status

# 4) 看日志尾部
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 log

# 5) 打开 / 关闭逐键 debug 日志（改完要重启宿主程序）
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 debug on
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 debug off

# 6) 反注册
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
6. **`panic = "abort"` 对注入型 DLL 是致命的。** workspace 原来为省体积设了 `abort`，但 `heinrich-win` 跑在别的进程里——abort 等于「终止宿主」。已移除（D21）。注意：即使 `unwind`，panic 跨出 `extern "system"` 仍是 UB，每个 COM 方法必须自己 `catch_unwind`（见 `apps/windows/tsf/src/text_service.rs` 的 `guarded()`）。对照：WindInput 敢设 `abort`，是因为它的 Windows 壳是 C++、Rust 代码全在独立 service 进程里；我们的壳本身就是 Rust，跑在宿主进程内，所以这条约束是我们独有的。
7. **`#[implement]` 要求 `windows-core` 作为直接依赖。** 宏展开的代码按 `windows_core::` 绝对路径引用它；只依赖 `windows` 会报 `could not find windows_core in the list of imported crates`。
8. **`as_interface_ref()` 只在宏生成的 `_Impl` 上，不在你的原始结构体上。** 所以「挂 sink」这类需要把自己交给系统的代码，必须写在 `impl Xxx_Impl` 里（`_Impl` 对原结构体有 Deref，字段照样能访问）。
9. **`RegisterProfile` 的签名与 MSDN 不一致。** windows-rs 0.62 是 `(rclsid, langid, guidprofile, pchdesc: &[u16], pchiconfile: &[u16], uiconindex, hklsubstitute, dwpreferredlayout, benabledbydefault, dwflags)`——描述串和图标是**切片**，长度由运行时补，不要自己传 `cchDesc`。
10. **`InstallLayoutOrTip` 返回值是 `void`。** 写错描述串不会报错，只会静默不生效。所以描述串格式在 `heinrich-tip-id` 的单测里钉死了。
11. **控制台输出中文要 `SetConsoleOutputCP(65001)`。** 中文 Windows 默认代码页 936，直接打 UTF-8 全是乱码。另外表格对齐不能用 `{:<44}`——它数字符不数显示宽度，CJK 占两列会歪。
12. **DLL 吃掉的键，必须保证最终一定有字出去（M1.1 起生效）。** TSF 在 `OnTestKeyDown` 问你「这个键要不要」时**早于**任何 IPC 往返：说「要」之后，事后再想让 server 把键放回宿主已经来不及，在严格的宿主里那个键直接消失。WindInput 把这条写成硬约定——**壳的吃键集必须 ⊆ 引擎的出字集**。所以「吃不吃」的判据必须 DLL 本地就能算出（本地候选缓存 + 本地「哪些键归我管」的判定表），不能依赖 server 回话。历史症状是「有些应用打不出字、有些只出半角」，极难归因。这是 D18「按键路径零阻塞」更硬的一面：不只是不能等，是**判断依据必须已经在本进程里**。
    > **实机证据（M1.0 日志）**：同一份 DLL，Notepad++（Scintilla）只回调 `OnTestKeyDown` / `OnTestKeyUp`，WorkBuddy（Electron/Chromium）只回调 `OnKeyDown` / `OnKeyUp`。两种键路由风格——前者「先问要不要，不要就自己处理、不再回调」，后者直接调 `ITfKeystrokeMgr::KeyDown`。⇒ **吃键判定必须在两条路径上都返回 TRUE**，只写一条会表现为「在某些程序里能用、在另一些里完全没反应」。
13. **已注册的 DLL 会被宿主映射，`Copy-Item` 覆盖必然失败（`os error 32`）。** 而且 COM 的 DLL 要等**宿主进程退出**才真正卸载：切换输入法、切走窗口都不够。开发期最坑的是宿主里包含你自己的终端——本项目在 WorkBuddy 的内置终端里测过一次输入法之后，WorkBuddy 进程就一直持有 `heinrich_win.dll`，`build.ps1` 从此写不进 `dist`。
    **解法**：Windows 允许**重命名**正在映射的镜像文件，只是不允许删除或覆盖。所以先 `Move-Item` 到 `dist\.stale\heinrich_win.<时间戳>.dll`，再拷新的——运行中的宿主继续映射旧内容（不受影响），新启动的宿主拿到新版。`build.ps1` 已经把这条做成失败提示；`.stale` 目录可以随时整体删掉。
    查持有者：`Get-Process | Where-Object { $_.Modules.FileName -like '*heinrich_win.dll' }`。
14. **`[CmdletBinding()]` 的脚本里，`param` 默认值不能用 `$PSScriptRoot`。** 用 `powershell -File tools\x.ps1` 调用时，参数绑定早于脚本作用域建立，`$PSScriptRoot` 还是空串，`Join-Path` 直接抛
    `无法将参数绑定到参数"Path"，因为该参数为空字符串`，脚本在绑参阶段就死了、一行都没执行。
    **更阴的是它只在 `-File` 下发作**：`& .\tools\x.ps1` 或 `. .\tools\x.ps1` 都正常。所以本地调试全绿、照着文档调用却报错。
    **写法**：`param` 里只声明 `[string]$DistDir`（不给默认值），函数体里再 `if (-not $DistDir) { $DistDir = Join-Path $PSScriptRoot '..\dist' }`。本项目 `tools\` 下 5 个脚本已全部按此改。
15. **状态更新不能只挂在 `OnKeyDown` 上——走 test 路径的宿主永远不调它。** M1.1 第一版把「Shift 按下就置位」写在 `OnKeyDown` 里，结果 Notepad++ 里单击 Shift 完全切换不了中英：它只回调 `OnTestKeyDown` / `OnTestKeyUp`，置位那一半从不执行，松手时的判定被整段跳过。日志里表现为**一条模式切换都没有**，而逐键日志看着一切正常。
    **推论**：凡是「同一个物理动作要产生副作用」的逻辑，四条回调都是入口，不能假手 `OnKey*` 代劳。
    **并且**：宿主不一定把修饰键的 **key-up** 送进来（M1.0 日志里 Notepad++ 对 Alt 只留下了 `test-down`，没有 `test-up`）。所以判定不能依赖「收到 up」，改成**每次回调采样一次 `GetKeyState` 与上次比对**：看到「上次按着、这次没按」就补判，最坏只是晚一拍（那个按键本身）。实现见 `keys::ShiftTap`，它是纯逻辑、有单测。
16. **`[CmdletBinding()]` 脚本的位置参数按声明顺序分配。** 给 `tsf.ps1` 加 `debug on` 的子参数时，新加的 `[string]$Switch` 声明在 `$DistDir` 之后，`on` 就被绑到了 `$DistDir` 上，`$Switch` 一直是空串——命令静默走了 `default` 分支，看起来像功能没实现。多参数脚本一律显式写 `[Parameter(Position = n)]`。

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
| M1.0 键进得来 | ✅ 已验收（注册 / 激活 / 按键只记录不消费 / 状态检查；`Get-WinUserLanguageList` 里已出现该 TIP） |
| M1.1 字上得去 | ✅ 已验收（缓冲 + 小码表 + `ITfEditSession` 上屏 + 单击 Shift 切中英；`vbg`→好、`q`→我、`vbgf`→好玩、Esc 取消、无命中原样上屏） |
| M1.2 组合串 | composition + 显示属性虚线 + 光标跟随 |
| M1.3 接 server | 命名管道 + 本地候选缓存 + `--debug-sleep` 零阻塞验证 |

**动手前必读**：`docs/design/tsf-preflight.md`。里面写了注册要写哪些键、必须实现哪些接口、15 条已知坑，以及「不要在 M1.0 通过之前写组合串与 UI 代码」的理由。
另外**不要照抄青简**——它 2026-09 才首发、Windows 壳未经装机验证。参考顺序固定为：官方文档判语义 → Weasel 判工程做法 → WindInput 判产品形态 → chewing-tsf 判 Rust 写法 → 青简判坑。
M1 的冒烟矩阵：记事本 / VS Code / Chrome / Edge / 资源管理器 / cmd，另加提权记事本与一个 32 位程序（见预研 §9 待实测 T2/T3）。
