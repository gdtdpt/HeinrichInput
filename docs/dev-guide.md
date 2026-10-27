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
powershell -ExecutionPolicy Bypass -File tools\accept-m2.4.ps1  # 学习库验收（只读，见 §4.9）
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
  heinrich-proto     DLL↔server 帧：长度前缀 + 消息类型 + 编解码     （零依赖）
      ↑
  heinrich-server    常驻进程：core 宿主 + 命名管道服务端 + 空闲退出
  heinrich-ui        候选窗**几何**：布局 + 定位（不碰 Windows、不做字体量测）（零依赖）
      ↑
  heinrich-cli       REPL / check                                （仅 M0 验收用，不进产品）
  heinrich-gen       原始码表 → 方案数据 + 校验报告               （仅构建期用，不进产品）

apps/windows/        平台专属产物：调 Windows API，或产物是注入件 / 注册器
  tsf/               TSF DLL，只依赖 windows + heinrich-tip-id + heinrich-proto（D8）
  setup/             注册器，依赖 windows + heinrich-tip-id
  ui/                候选窗：HWND + 分层窗口 + D2D/DWrite（crate 名 heinrich-win-ui）
  cand-preview/      候选窗预览器：假候选直接弹窗 + --selftest     （验收工具，不进产品）
```

**这个两层划分是硬约定，不是目录洁癖**。判据只有两条：调某个平台的系统 API，或产物是 `.dll` / `.ime` 这类注入件、平台注册器 ⇒ 进 `apps/<platform>/`；其余一律进 `crates/`。新增 crate 前先问「它在另一个平台上编得过吗」，答否就是走错了层。

`heinrich-tip-id` 是 Windows 身份常量，但它零依赖、且 DLL 与注册器共引（D22），所以留在 `crates/`；将来 macOS 的身份常量按同一模式另立 crate，不与它合并。

**M5 加平台时只新增平级的 `apps/macos/`、`apps/linux/`，`crates/` 不动**。届时构建脚本必须用 `-p` 显式指定产物——`cargo build --workspace` 会去编别的平台的壳、必然失败。

**已实现**：`heinrich-proto` 与 `heinrich-server`（M1.3）、`heinrich-ui` 的几何部分（M2.1）、
`apps/windows/ui` 与 `apps/windows/cand-preview`（M2.2）。
`heinrich-ui` 的边界见 §4.6：它只算矩形，字形量测与绘制在 `apps/windows/ui`。

**边界纪律**：`heinrich-core` 里出现 `cfg(windows)`、`std::fs`、`SystemTime::now()` 就说明分层错了。时间一律由调用方作为 Unix 秒传入——这是排序行为可测的前提。

**`heinrich-win` 的依赖纪律（D8）**：只允许 `windows` + `heinrich-tip-id`。它被注入到每个宿主进程，加一个库等于让 chrome 的每个 tab 都多背一份。

## 4. TSF 壳（M1.0 – M1.3）

「薄壳」指的是**不含引擎、词库、SQLite、UI 绘制**，不是行数少。TSF 管线本身是重活：WindInput 的 C++ 壳约 1 MB 源码（`TextService.cpp` 385 KB、`KeyEventSink.cpp` 184 KB、`IPCClient.cpp` 114 KB、`LangBarItemButton.cpp` 60 KB），这些代码全部在 D8 的依赖纪律下活着。`heinrich-win` 到 M2 长到几千行是正常的，别拿行数当验收标准。

### 4.1 组件

| 文件 | 作用 |
|---|---|
| `crates/heinrich-tip-id/src/lib.rs` | CLSID / Profile GUID / 描述串。**发布后不可改**，值由 `uuid5` 生成 |
| `apps/windows/tsf/src/lib.rs` | `DllMain` / `DllGetClassObject` / `DllCanUnloadNow` / `IClassFactory` / `HeinrichTsfAbiVersion` |
| `apps/windows/tsf/src/keys.rs` | **纯逻辑**：`decide(mode, buffer_len, vk, mods) -> Action` + `ShiftTap` + `punct_text`。不碰 Windows API，可在任意平台单测 |
| `apps/windows/tsf/src/ipc.rs` | 命名管道客户端：重叠 I/O + 50 ms 硬上限、后台重连线程、候选缓存（D32） |
| `crates/heinrich-proto/src/lib.rs` | 帧协议：`u32 长度 + u16 类型 + 载荷`，7 条消息，手写编解码、零依赖 |
| `crates/heinrich-server/src/host.rs` | 平台中立的一半：收帧 → 查词库 → 回帧（可单测） |
| `crates/heinrich-server/src/pipe.rs` | 命名管道服务端：按会话单实例、一连接一线程、空闲退出 |
| `apps/windows/tsf/src/preedit.rs` | 组合串：`Show` / `Finish(Some)` / `Finish(None)` 三个动作的编辑会话，以及没有组合串时的直接插入降级 |
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

那条例外是踩坑 #15 换来的：M1.1 首版把「Shift 按下就置位」写在 `OnKeyDown` 里，
而 Notepad++ 这类宿主是**先问后做**的——问了说不要，它就不再调 `OnKeyDown`，于是
置位那一半根本不执行。状态机做成幂等的，所以「先问再调」的宿主重复回调也不会切两次。

> **别被 M1.0 的日志误导**：那一版 Notepad++ 只留下 `test-down`、没有 `down`，
> 是因为 `OnKeyDown` 当时恒返回 FALSE、一个键都不吃。**在 `OnTestKeyDown` 回答 TRUE
> 之后，宿主会接着调 `OnKeyDown`**，两条路径都会到齐（顺序是先问后做）。

违反这条的症状是「某些程序能用、某些完全没反应」，或「一个字进去两次」。原因见踩坑 #12 与 #13 前面的实测表。

**日志级别**：M1.1 起按键退回 `debug`，`info` 只留 Activate / Deactivate / 提交 / 无命中 / 模式切换。
要逐键排查用 `powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 debug on`（写一个
`%LOCALAPPDATA%\Heinrich\debug.on` 标志文件），**改完必须重启要观察的宿主程序**——
级别在每个进程里只解析一次。环境变量 `HEINRICH_TSF_LOG` 仍然可用，且优先级更高。

### 4.1.2 缓冲与提交规则

| 情形 | 行为 |
|---|---|
| `a`–`y` | 吃掉，追加缓冲（上限 4 码） |
| `z`，缓冲非空 | **吃掉但不动**（`Noop`）。放行会让宿主在组合串中间插 `z`、终止组合串，已打的编码按残留被删掉；真学习键待 M3（D42） |
| `z`，缓冲为空 / `Shift+z` | 放行。没有组合串可打断，且 `Shift+z` 是临时英文要输出大写 `Z` |
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
| 单击 Shift | 切换中英。**缓冲非空时把编码原样上屏成字母**再切（D41）。判定靠**采样物理状态**而不是靠 vkey 上下，松开事件缺失也能在下一个按键上补判（`ShiftTap`） |
| 方向键 / Home / End / PgUp / PgDn（0x21–0x28），组合态 | 吃掉并取消（D31）。代价是一次按键被吞，换来的是不留错位组合串 |
| 方向键类，缓冲为空 | 放行（正常移动光标） |

### 4.1.3 组合串（M1.2）

缓冲里那串编码**就是组合串的内容**——两者是同一份状态的两种表现，所以「缓冲一变
就同步组合串」是唯一不变式。三个动作都在 `preedit.rs`，全部走 `TF_ES_SYNC` 编辑会话：

| 动作 | 时机 | 会话里做什么 |
|---|---|---|
| `Show(text)` | 每次缓冲变化（加字母、退格） | 有组合串就改内容；没有就 `StartComposition` 后写入；`text` 为空则结束并抹掉 |
| `Finish(Some(t))` | 空格 / 回车提交、单击 Shift 切英文（D41） | 内容换成 `t`（符号 / 原样字母 / 编码字母）→ 光标收到末尾 → `EndComposition`，文字留下 |
| `Finish(None)` | Esc、方向键、失焦、切窗口 | 内容抹成空 → `EndComposition` |

三条硬约束（都来自 TSF 的语义，不是我们的选择）：

1. **`StartComposition` 要一个空 range。** 流程必须「清选区 → 起组合串 → 再往组合串的
   range 里写」。反过来组合串起点会落在文字末尾，覆盖不到 preedit，之后 `SetText` 改的
   内容看不见。
2. **`EndComposition` 不删文字**，它只把这段文字「转正」。取消必须先把内容写成空串。
3. **写文档只能在 `DoEditSession(ec)` 里**，且必须先 `SetSelection` 把光标收到 preedit
   末尾，否则看起来像「字往左边长」。

组合串也可能被**别人**终止（宿主换焦点、文档关闭、另一个 TIP 抢走）。这时 TSF 通过
`ITfCompositionSink::OnCompositionTerminated(ecwrite, comp)` 通知我们，我们趁 `ecwrite`
还有效把残留文字删掉——否则文档里会永久留一串 `vbg`。**自己 `EndComposition` 不会
触发它**，而且删之前会比对句柄身份，双保险。

### 4.1.4 IPC 与 server（M1.3）

13.6 万条词库装载约 300 ms，DLL 却会在**每个宿主进程**里加载——所以词库只在
`heinrich-server.exe` 里装一份，DLL 通过命名管道问它。

| 项 | 值 |
|---|---|
| 管道名 | `\\.\pipe\heinrich-<SessionId>`（按登录会话隔离） |
| 单实例互斥体 | `Local\HeinrichServer-<SessionId>` |
| 帧格式 | `u32 载荷长度（含类型）` + `u16 消息类型` + 载荷，小端 |
| 消息 | `Hello` / `Welcome` / `Query` / `Result` / `SetMode` / `Mode` / `Bye` |
| 谁拉起 server | DLL 在 `Activate` 时，同一目录下的 `heinrich-server.exe`（`CREATE_NO_WINDOW`） |
| 何时退出 | 无连接空闲 30 分钟（`--idle-exit`，0 = 不退） |

**50 ms 铁律**（D32）：按键回调里每次往返都有硬上限，超时就丢掉整条连接、把编码
原样上屏。连接/重连/拉起全在后台线程（`ipc::warm_up`），键路径只 `try_lock`。

降级行为一张表，排查时先对照这个：

| 情况 | 用户看到 |
|---|---|
| 正常 | 上屏汉字 |
| 那个码没有词条 | 上屏原样字母（D27） |
| server 未连上 / 超时 | 上屏原样字母，下一次按键后台重连 |
| 中英模式在英文 | 什么都不接管，宿主自己出字母 |

手工验证的四个动作：

```powershell
# 1) 看 server 在不在、管道有没有、日志尾部
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 server

# 2) 验零阻塞：让 server 每次查询前睡 5 秒，此时打字必须完全不卡（D18）
dist\heinrich-server.exe --debug-sleep 5000 --idle-exit 0
# 打完字记得结束它，否则后续几秒内的上屏都会变成原样字母

# 3) 验自动重连：杀掉后随便打个字，再跑一次 tsf.ps1 server 应看到新 pid
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 server stop

# 4) 无管道也能验「装得进、查得对」
dist\heinrich-server.exe --selftest
```

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
3. 看 `%LOCALAPPDATA%\Heinrich\logs\tsf-<今天日期>.log`（日志按天一个文件，D76；
   也可以 `tools\tsf.ps1 log` 直接看最新那天的尾部）：

```
2026-10-23 14:12:03.451 [11824] I ==== 亨利五笔 DLL：notepad++.exe pid=11824，ABI 7，协议 v6，日志 info ====
2026-10-23 14:12:03.455 [11824] I #1 Activate tid=0 pid=11824 —— 按键 sink 与焦点 sink 已挂
2026-10-23 14:12:05.902 [11824] I #1 down vk=0x56 'V' - 
2026-10-23 14:12:06.114 [11824] I #1 down vk=0x42 'B' - 
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

### 4.5 验 D18：怎么造一个「慢 server」

D18 的判据是「server 卡住时宿主一个键都不等」。要真机验它，先得有一个**连着但很慢**的 server：

```powershell
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 server slow     # 默认每帧 800ms
```

脚本会抢占同会话互斥体（正常实例每 200ms 会被 DLL 自动拉起，所以是竞态，输了自动重试最多 6 次）。**跑起来之后只需要切到宿主敲一个码，不用切回来按键**：脚本每 0.5s 读一次日志，判定完自己打印结论，并在结束时自动撤掉慢实例（中途 Ctrl+C 也会撤）。`-WaitSeconds` 默认 60 秒，`-SlowMs` 默认 800ms。

三个必须知道的点，否则会得出相反结论：

1. **`--debug-sleep` 对所有帧生效，包括 `Hello`。** 而 DLL 的握手预算是 `GREET_BUDGET = 3000ms` ⇒ `--debug-sleep 5000` 连握手都过不去，你测到的是「连不上」（瞬间返回），不是「连上了但慢」（50ms 上限才在这里生效）。要验上限就用 **800**。
2. **字母键根本不走 IPC。** `ipc::query` 只在上屏键（空格/回车）和标点键上调用 ⇒ 打字过程永远不卡，唯一可能卡的是**按下空格的那一下**。**M2.3 起这条会变**：候选窗要逐键查询（D37），届时「不卡」要靠 64 条缓存 + 超时只丢结果、不空窗；量逐键延迟是 M2.3 的第一件事。
3. **进程内候选缓存（64 条）会掩盖降级。** 判据不是「卡不卡」，而是**输出的字**：日志里出现 `server 不可用（ab），原样上屏` 才算降级生效；出现 `提交 ab → 某字` 说明这个码命中了缓存，换一个该宿主进程从没打过的码。

### 4.6 候选窗（M2）：同类产品怎么做的、我们怎么切

**先给结论：没有一家把候选呈现交给宿主的 TSF 候选 UI。** 三家都自绘，只是自绘的层不同：

| 产品 | 窗口 | 布局 | 定位 | 交互 |
|---|---|---|---|---|
| Weasel（小狼毫） | 自绘 `WeaselPanel`：`WS_POPUP｜WS_CLIPSIBLINGS｜WS_DISABLED` + `WS_EX_TOOLWINDOW｜TOPMOST｜NOACTIVATE｜LAYERED`，`_LayerUpdate` 走 `UpdateLayeredWindow`，`WM_DPICHANGED` 时重建资源 | 独立的 `Layout` 类层次（`HorizontalLayout` / `VerticalLayout` / `FullScreenLayout`），**只算矩形不画** | `MonitorFromRect(光标)` 取 `rcWork` → 越右夹右、越下翻上方（`y = inputPos.top - height`）、再不行贴边 | `WM_MOUSEACTIVATE` 拦激活、`WM_LBUTTONUP` 命中、`WM_MOUSEWHEEL` 翻页、`WM_MOUSEMOVE/LEAVE` 悬停 |
| fcitx5 Classic UI | 自绘，Cairo + Pango；X11 用 override-redirect 窗，Wayland 用 layer-shell | **Yoga 布局引擎**建节点树（`rootNode_` / `candidatesNode_` …），`InputWindow::paint` 只负责照画 | 按输入光标 + 逐屏 DPI 算（多屏各算各的） | 主题 9-patch、悬停、点选 |
| 青简 | 平台壳各写一份（macOS IMK / Windows TSF + 独立引擎进程） | core 不管界面 | core 不管界面 | core 不管界面 |

能抄的就三条：

1. **布局与绘制必须分家。** Weasel 的 `Layout` 类、fcitx5 的 Yoga 树，都是「先算矩形、再照画」。我们的落点是 `crates/heinrich-ui`（纯几何、27 单测）+ `apps/windows/ui`（HWND/DWrite）。
2. **定位永远夹进 `rcWork`，且越下就翻到光标上方。** Weasel 的 `_RepositionWindow` 就是全部逻辑，见 `crates/heinrich-ui/src/place.rs` 的文档注释。唯一和它不同的地方：窗口比工作区还高时我们**贴顶**，它贴底（贴底会连行首一起推出屏幕）。
3. **窗口不激活。** `WS_EX_NOACTIVATE` 只是不让它抢前台，**点它仍然会激活**——必须 `WM_MOUSEACTIVATE` 返回 `MA_NOACTIVATE`，再自己处理 `WM_LBUTTONUP`。这条不抄会得到一个「点候选就丢失组合串」的输入法。

我们**不抄**的：Weasel 的竖向/全屏布局（v1 只做单行 5 条）、fcitx5 的 9-patch 主题（v1 不做皮肤）、青简把候选窗算作壳的一部分（我们把它拆成几何 + 窗口两块，因为几何能单测）。

**翻页提示只画箭头，不画页码（D39）。** 这条是**反着主流做的**，理由写清楚免得后人来「补全」它：不少输入法在候选窗角上打 `2/3`，但那个数字的价值来自「知道还剩多少」，而我们只提供前后翻、不提供直接跳页 ⇒ 它对用户没有可操作的落点，却要在单行窗口里长期占一块横向空间。需要传递的只有一条信息：「这个方向还有」。所以用两端各一个箭头，**没有候选的那个方向连箭头都不画、也不占宽度**（`page_nav()` 给 `has_prev` / `has_next`），这样单页时候选窗能窄一截。要靠键盘翻页的用户看箭头也能发现 `←`/`→` 能用。

**分步与验收**：M2.1 几何（`crates/heinrich-ui`，`cargo test -p heinrich-ui`）→ M2.2 窗口 + `cand-preview.exe`（自截图验收）→ M2.3 接进 TSF（逐键查询、`GetTextExt` 定位、数字键与点选）→ M2.4 学习落库。四个阶段与验收项见 PROJECT.md。

**看一眼几何**（不需要窗口、不需要装输入法）：

```powershell
cargo run -q -p heinrich-ui --example geometry > dist\candidate-geometry.svg
```

### 4.7 候选窗的窗口层（M2.2）

`apps/windows/ui`（crate 名 `heinrich-win-ui`）只有三件事：**建窗口、量字、画**。几何一律来自
`crates/heinrich-ui`，这里不重算任何矩形——D36 的边界。

**窗口样式，四条缺一条都会出问题：**

| 样式 | 作用 | 少写会怎样 |
|---|---|---|
| `WS_POPUP｜WS_CLIPSIBLINGS` + `WS_EX_LAYERED` | 圆角外真透明 | 圆角变成白角，盖住底下的字 |
| `WS_EX_NOACTIVATE` | 显示时不抢前台 | 组合串被终止，打一半的编码消失 |
| `WM_MOUSEACTIVATE` → `MA_NOACTIVATE` | **点它也不抢前台** | 点候选 = 切焦点 = 组合串丢失 |
| `WS_EX_TOOLWINDOW` | 不进 Alt+Tab、不进任务栏 | 每敲一个字多一个幽灵窗口 |

第三条最容易漏：`WS_EX_NOACTIVATE` 只管「显示」那一步，鼠标点上来仍会走激活流程。
所以命中测试与点击都在本 crate 内完成，通过 `Event::Click(Hit)` 把**全局候选序号**交回调用方。

**绘制路线**：`ID2D1DCRenderTarget` 绑一块 `CreateDIBSection` 出来的 BGRA 内存位图，画完拿去
`UpdateLayeredWindow`（`ULW_ALPHA` + `AC_SRC_ALPHA`，位图必须是**预乘** alpha）。

- 不走 GDI：`TextOut` 画出来是 1 位 alpha，文字毛刺且背景发实，圆角也没法做。
- 不走 GDI+：`gdiplus.dll` 是 C++ 类接口，windows-rs 没封装，只能自己写 vtable。
- D2D 的 DC 渲染目标既给抗锯齿又能落在我们自己的位图上，是这条路的标准解法。

**三个坑，都踩过：**

1. **渲染目标的 DPI 固定 96。** `Metrics.font_size` 已经是物理像素，一旦把渲染目标的 DPI
   也设成 `96 × scale`，D2D 会把 DIP 再乘一次缩放，字号变成平方。比例只允许出现在一个地方。
2. **文字宽度必须与绘制同源。** 量字和画字用的是**同一批** `IDWriteTextFormat`（`text.rs` 缓存
   三个：词 / 序号 / 箭头）。换字体或字号只改一处，几何与字形就会错位，而且错得很微妙。
3. **DPI 感知不能由 DLL 设置。** `SetProcessDpiAwarenessContext` 是**进程级**的，DLL 跑在宿主
   里，改它等于替宿主做决定。DLL 只问 `GetDpiForMonitor(光标所在屏)`；只有独立 exe
   （`cand-preview`）才调 `heinrich_win_ui::enable_per_monitor_dpi()`。

**验收：自己截图，不用找人代拍**

```powershell
# 1) 行为自测（不抢前台、命中、窗口尺寸、点箭头不选词），不需要人看
cargo run --release -q -p heinrich-cand-preview -- --selftest

# 2) 真窗口看一眼：先弹出来，再从屏幕抓图
Start-Process .\dist\heinrich-cand-preview.exe -ArgumentList '--hold','8000','--dpi','1.5','--page','1'
python tools\shot-window.py HeinrichCandidateWindowV1 dist\cand-150-p2.png --margin 8
python tools\shot-window.py HeinrichCandidateWindowV1 dist\cand-100-zoom.png --zoom 4   # 放大看像素
```

`--dpi` 只是**指定缩放比**，不需要真有 150% 的显示器：位图按物理像素生成，`--dpi 1.0` 在
200% 屏上抓到的就是 100% 屏的一比一像素。

`tools/shot-window.py` 是从**屏幕**做 `BitBlt`，不是 `PrintWindow`：分层窗口的内容在 DWM
合成层里，`PrintWindow` 往往拿到空图。它自己会先 `SetProcessDpiAwarenessContext`——
**python.exe 默认 DPI 不感知**，不声明的话 200% 屏上抓到的图会先被虚拟化一半再放大，
字全是糊的，窗口坐标也会差一倍（这个坑很隐蔽：只抓到「正确的窗口的一角」，看起来像布局错了）。

### 4.8 候选窗接进 TSF（M2.3）

一次按键的完整链路（**顺序不能换**）：

```
OnKeyDown → keys::decide(Input{ mode, buffer_len, page_len }, vk, mods)
          → apply(action)
              Action::Buffer  → 缓冲 +1
                  ├─ caret_rect()          缓冲为空时先量一次「动手前」的光标，留作兜底
                  ├─ sync_preedit()        组合串先长（本地、快）
                  ├─ ipc::query(code)      ≤50 ms，超时就保留上一帧（D37）
                  ├─ caret_rect()          只读编辑会话 → GetTextExt（三级兜底）
                  └─ Cands::show(caret)    prepare（DWrite 量字）→ place → 分层窗口
              Action::Commit   → Cands::current() → Finish(Some(word))
              Action::Select(n)→ word_on_page(n) → Finish(Some(word))
              Action::Page*    → Cands::flip(dir)（到头则什么都不做）
```

**五条「不做就静默坏掉」的点**（D40）：

1. **`GetTextExt` 必须进编辑会话。** 它要一个读 cookie（`ec`），cookie 只在
   `DoEditSession` 里有效。所以我们为它单开一个 `TF_ES_READ | TF_ES_SYNC` 会话
   （`caret.rs`），而不是想在键回调里直接问。
2. **定位走三级兜底，别只问一种。** `caret.rs` 按顺序试：① **选区**——组合串的
   range 覆盖整串编码，宿主折行时它的外接矩形会横跨两行；而每次写完组合串我们都
   把选区收到末尾（`preedit::set_caret`），所以选区是一个折叠 range，**它就是插入符**；
   ② **组合串右下角**（连问两次，见下条）；③ `GetGUIThreadInfo` 给的 GUI 插入符。
   三级都空就返回 `None`，由调用方用「动手前」量到的那个位置顶上（首键才有）。
   每级只记日志、不返回错误——第一版在这里用 `?` 把错误抛出去，结果兜底路径成了
   死代码，Chrome 一个窗口都不显示。
3. **Chromium 会在组合串刚建立时回 `TF_E_NOLAYOUT (0x80040206)`。** 那时它还没给
   新组合串排版，`GetTextExt` 直接失败。记事本 / Notepad++ 不会。对策两条：**同一
   请求连问两次**（第二次它就现算排版并给出真实矩形，M2.3 实测通过率 100%），以及
   **首键前预取**——缓冲为空时先量一次，此时组合串还没建、排版干净，选区那条必然
   成功。所以 Chrome 的第一键靠预取、后续键靠连问两次。
4. **窗口里的点击不在任何 TSF 回调栈上。** 它发生在宿主的 `DispatchMessage` 里，
   拿不到 `&self`。所以 `TextService` 的全部状态搬进 `Rc<Core>`，回调只持
   `Weak<Core>`。**任何 `RefCell` 二次借用都会 panic，而 panic 跨出
   `extern "system"` 就是宿主进程 abort** —— 所以事件路径上只用瞬时借或
   `try_borrow`，借不到就丢掉这次事件（下一次按键会重画）。
5. **建窗失败要永久降级。** `CandidateWindow::new` 只在第一次真要显示时调用；
   失败一次就记一条错误、此后不再尝试，退化成 M1.3 的「无窗也能打字」。

**验收怎么跑**（真机，模拟不了）：

```powershell
# 1) 先确保装的是新 DLL（ABI 5）
.\dist\heinrich-setup.exe --status

# 2) 打开逐键 debug 日志（改完要重启宿主程序）
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 debug on

# 3) 在记事本 / Notepad++ / Chrome 里各打一遍 vbg，然后看日志
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 log
```

日志里该看到的（一条键一行）：

- `候选 5 条，共 1 页` → 查询命中、窗口内容已换
- `光标来自选区` / `光标来自组合串右下角（第 N 次）` / `光标来自 GUI 插入符`
  → 坐标是哪一级给的（Chrome 里常态是「组合串右下角（第 1 次）」）
- `沿用动手前量到的光标` → 现场三级全没量到，用了首键预取的位置（Chrome 首键即此）
- `三级兜底都没量到光标` + `本次不显示候选窗` → 真没坐标，这次不出窗（不该常出现）
- `候选窗 498×62 @ (x, y)` → 定位成功（**没有这行就是 `GetTextExt` 没给坐标**）
- `提交 vbg → 好（候选窗）` → 空格走的是窗口里那份候选
- `数字键选第 2 条 vbg → 㛅` / `点选第 2 条 vbg → 㛅` → 两条路径同一个词
- `翻到第 2 / 2 页` → 翻页生效；`已经在最后一页` → 末页按右

**自测用注入脚本**：不想手敲就用 `tools/smoke-ime.py`（纯 stdlib）把按键送进
**指定 pid** 的前台窗口，配合 `shot-window.py` 截图，全程不看屏幕也能验：

```powershell
$np = Start-Process notepad -PassThru
python tools\smoke-ime.py --pid $np.Id vbg "{SPACE}" vbg "2" vbg "{RIGHT}"
python tools\shot-window.py HeinrichCandidateWindowV1 dist\cand-live.png --margin 16
```

它抢不到前台就直接退出（绝不往未知窗口里打字），`{NAME}` 是功能键（表在脚本顶部）。
用 `SendInput` 而不是 `keybd_event`：`SendInput` 校验的 `cbSize` 是**整个 `INPUT`**
的大小，x64 上由 `MOUSEINPUT` 决定（40 字节），结构体里漏写 `mi`/`hi` 会恒返回
`ERROR_INVALID_PARAMETER (87)`——这个坑脚本注释里记着，别把它「精简」掉。

**M2.3 真机实测**（记事本 / Notepad++ / Chrome，脚本 `tools/smoke-ime.py` 注入按键）：

| 宿主 | 首键出窗 | 定位来源 | 提交 / 翻页 / 点选 / Esc |
|---|---|---|---|
| 记事本 | ✅ | 选区 | 全绿（13 项） |
| Notepad++ | ✅ | 选区 | 全绿 |
| Chrome（新 profile） | ✅（靠首键预取） | 组合串右下角（首键靠预取） | 提交 ✅ |

**已知边界**：宿主如果**不是** per-monitor DPI 感知（老程序），`GetTextExt` 给的坐标
是虚拟化的、而 `GetDpiForMonitor` 给的是真 DPI，副屏上会错位；记事本 / Notepad++ /
Chrome 都是 DPI 感知的，v1 不为老宿主做补偿。

### 4.9 学习数据落库（M2.4）

```
按键 → 上屏（空格 / 数字键 / 点选）
      └─ ipc::record(code, word)      入队 + 丢掉该码的本地缓存（微秒级，不等网络）
            │
      [后台线程 200 ms 一次]          flush_pending_records()
            └─ 单向 Frame::Record ──▶ server：store.record(word, at_unix)
                                          └─ 一个事务：n+1、last_used_at=MAX(旧,新)
```

**库在哪**：`%LOCALAPPDATA%\Heinrich\learn.db`（`--db <file>` 可改，`--memory-db` 不落盘）。
**不放 dist**：dist 可能装在只读目录，升级时整个被替换，用户攒的词序不能跟着一起没。

**表**：

| 表 | 列 | 说明 |
|---|---|---|
| `usage` | `key`（词）、`n`、`last_used_at`、`tier` | `tier`：0 学习 / 1 固顶 / 2 抑制，**显式映射**，不依赖枚举顺序 |
| `user_words` | `code`、`word`、`weight`、`created_at` | 自造词，M5 才写，先把 schema 定下来 |
| `meta` | `k`、`v` | `schema` 版本；比程序新就**拒绝启动学习库**（降级成内存），不硬写 |

**四条容易写错的**：

1. **只能有一个写者。** 库归 server（D1）。放进 DLL 就是每个宿主进程各写一份，
   SQLite 的锁会变成宿主的卡顿来源，而且 WAL 也救不了「两个进程抢同一个页」。
2. **`last_used_at` 取 `MAX(旧, 新)`。** server 被杀时 DLL 会积压记录、重连后成批
   补发；多个宿主各有一份队列，补发顺序不保证全局单调。直接覆盖会让晚到的旧记录
   把时间往回拨，`0.5^(Δt/H)` 的衰减就算反了。
3. **上报必须是单向帧。** 用 `exchange` 会一直等一条永远不会来的应答，把预算耗光
   并误判成连接坏了。DLL 侧用 `write_all` 只写不读。
4. **上屏之后要丢本地缓存。** `ipc::query` 有 64 条进程内缓存；不清掉的话，
   「学了半天不涨」——要等这条缓存被别的码挤掉才重新问 server。

**上报什么、不上报什么**：只有「用户真的选了一个词」才报——空格提交、数字键、
点选。三条路径**不报**：无命中、server 不可用（这两种吐的是编码字母）、
Shift 切模式吐字母。它们不是选择，报了只会把 `n` 灌成噪声。

**怎么验**：`tools\accept-m2.4.ps1`（只读打开库，不写用户的库）。

```powershell
# ① 打字之前记基线
powershell -ExecutionPolicy Bypass -File tools\accept-m2.4.ps1 -Snapshot

# ② 打成几个词（记事本即可），数清每个词按了几次

# ③ 对比 + 断言：哪些词的 n 涨了、涨了多少、结构有没有坏
powershell -ExecutionPolicy Bypass -File tools\accept-m2.4.ps1 -Check -Word 好 -Expect 3
```

它比的是**差值**而不是绝对值——「库里有 171 个词」只能说明以前用过，只有
「这一轮哪个词的 n 涨了」才能证明 DLL 真的把记录发过来了。不给参数时只打印现状
与前 15 名。结构断言查这些：`schema == 1`、三张表齐全、`usage` 非空、没有
`n < 1`、没有未来时间戳（时钟与 `MAX(旧,新)`）、`tier ∈ {0,1,2}`、没有空词条。

两个自己写脚本时容易踩的点（本脚本都踩过）：

1. **参数别叫 `-N`。** PowerShell 变量名不区分大小写，脚本里一个 `$n` 循环变量
   就会把 `$N` 覆盖掉——第一次自测时「期望 1」变成了「期望 4」，因为表格里最后一
   行的 n 正好是 4。现在参数叫 `-Expect`。
2. **`DateTimeOffset` 没有 `LocalTime` 属性**，只有 `LocalDateTime` / 方法
   `ToLocalTime()`。写成 `.LocalTime.ToString(...)` 会先得到 null、再对 null 调
   `ToString`，报「不能对 Null 值表达式调用方法」——而且报在调用脚本的那一行，
   看不出是脚本内部哪句。

**手工才能验的三件**（脚本验不了）：

1. **词序真的变了**：连打某个**次**候选六七次，再打同一个码，看它有没有升到第 1。
   第一位不会因为「打一次」就换人——`n = 1` 得 0 分（§5.1），与没用过一样。
   注意别拿「第 1 条」来验：连打第 1 条永远看不出变化。
2. **三条不报路径**：死码（如 `abc`）、Shift 吐字母、server 不可用时吐字母，
   这三种都不该让任何词的 `n` 上涨（它们不是「用户选了一个词」）。
3. **跨重启**：`tsf.ps1 server stop` → 再打同一个码 → 顺序应保持上次学到的样子。
4. **连打三天**：这一条没有任何脚本能替代，只能真用。

**升级注意**：`PROTO_VERSION` 变了以后，旧 server 还在跑会让新 DLL 握手直接吃
`Bye`，而 DLL 不停重连又把空闲计数刷新掉 ⇒ 旧 server 永不退出，表现为「一个候选
都不出」。所以 `heinrich-setup.exe install` 的第一步（`[0/5]`）就是结束**本会话**的
`heinrich-server.exe`，下一次按键由 DLL 惰性拉起新版本。

### 4.10 混输：拼音数据与裁决（M3.1 / M3.2）

```
按键 → 组合串（一串字母，不区分语言）
        │
   sync_preedit + ipc::query(buffer)
        │
   ┌────┴─────────────────────────────┐
   │ core::pinyin::query_with_limit    │
   │  带 ' → 纯拼音（五笔不查）          │
   │  五笔侧：lookup(scheme)            │ 精确命中 → 主段 = 五笔
   │  拼音侧：lex.match_indices        │ 五笔没命中 → 主段 = 拼音
   │  primary = 主段（D48：五笔优先）    │
   │  folded  = 补段，剔掉主段已有的词    │
   └──────────────────────────────────┘
```

**数据从哪来**：`rime/rime-pinyin-simp`（**Apache-2.0**）→ `tools/fetch-data.ps1` → `tools/gen.ps1 -Kind pinyin`
→ `data/pinyin/`（`lexicon.tsv` + `syllables.txt` + `scheme.toml`）。

| 产物 | 内容 |
|---|---|
| `lexicon.tsv` | `拼音<TAB>词<TAB>权重`，拼音是**去掉音节空格的连续小写串**（`haoping`），65,125 条 |
| `syllables.txt` | 415 个音节，供切分判定与合法性校验 |
| `scheme.toml` | 清单 + 词库哈希（装载时校验，与五笔同一套 `Manifest`） |

**四条容易写错的**：

1. **词条里不存音节空格。** 用户敲的是连续字母，要求他打 `hao ping` 违背所有拼音输入法的
   肌肉记忆。去掉空格后查询退化成一次前缀匹配：`hao` 命中「好」，`haop` 命中「好评」，
   正好对上 §6.1 的「完全匹配 / 前缀匹配」。代价是丢掉边界（`xian` = 先 / 西安），
   但那本来就是同一组候选——需要边界的地方（判断一段字母能不能切成音节）用
   `syllables.txt` 现场算可达性 DP，不靠词条自带的分隔。

2. **拼音词典归 `heinrich-scheme`，不归 `heinrich-core`。** core 的硬约束是「不得有文件 I/O、
   不得读时钟」，而装载要读三个文件。放 `heinrich-scheme` 后它与 `Scheme` 并列、
   共用同一套清单解析与哈希校验；core 只拿 `&PinyinLexicon` 做纯逻辑。

3. **排序必须复用 `rank_inputs`，但精确区与前缀区要分开排。** 复用是为了让拼音和五笔
   共享同一份学习数据（拼音下打惯的字，切回五笔也该靠前）。而**分开排**是因为混排的
   实测后果很难看：`hao` 会出「好、号、好吧、好不好、好啊、好不容易、浩、豪…」——
   词组的语料权重高于同音字，把用户真正想打的字挤到了后面。分开后是
   「好、号、浩、豪、耗、郝…」再跟前缀词组。

4. **判定不看质量分，只看「五笔有没有精确命中」（D48）。** D46 时代做过一套质量分
   （五笔精确 1.0 / 拼音可切分 1.0 / 拼音仅前缀 0.5，同分判拼音），真机证明它等价于
   「只要像拼音就判拼音」——3–4 个字母几乎总能切成一串合法音节。现在只剩一条判定：
   五笔精确命中就是五笔主段，否则拼音主段，带 `'` 则直接跳过五笔。
   `PinyinLexicon::can_split` 仍保留（`check-pinyin` 用它校验音节表），但**不再参与裁决**。

**当前只做单段。** 真正的多段输入（`haovb` = 拼音段 + 五笔段，组合出「好我」）需要
各段候选做笛卡尔积，而它的实际频率极低——用户要么在打五笔、要么在打拼音。单段判定
已覆盖纯拼音、纯五笔与 `hao` 这类歧义。多段留到 §6.1 的完整 DP。

**怎么验**（不需要 TSF 壳，CLI 就够）：

```powershell
# 交互：打 hao / haoping / vbg / nihao，看判定与折叠分界
cargo run -p heinrich-cli -- repl --data data/wubi86 --pinyin data/pinyin

# 数据校验（check.ps1 会跑这一步）
cargo run -p heinrich-cli -- check-pinyin --data data/pinyin
```

日志/输出对照：

- `判定 拼音 · 61 个候选` + `以下是五笔候选（折叠区）` → 判定与折叠都生效
- `判定 五笔` → 该串切不成音节（`vbg` 里的 `v` 不是音节起始）
- `` `zzz` 没有候选 `` → 两边都没命中，符合 D45（不出窗）

### 4.11 混输接进 TSF（M3.3）

```
server 握手 → Welcome { features }  ──┐
                                      ├→ ipc::FEATURES（原子）
DLL 键路径 ← keys::Input.pinyin ──────┘   ├─ z 进缓冲（还是按 D42 吃掉）
                                          └─ 码长上限 4 → 24
server 查询 → Result { entries, source, primary_len }
                 │                    │
                 └─ 主段在前、补段在后 └─ 分界 → Cands.split → Theme.secondary 淡一档
```

**判定顺序（D48，撤销了 D46 的「同分判拼音」）**：

```
串里有 '            → 纯拼音，五笔一条都不查
五笔精确命中        → 五笔主段 + 拼音补段（剔掉主段已有的词）
五笔没命中、拼音有   → 拼音主段
都没有              → 不出窗（D45）
```

原来的规则是「同分判拼音」，理由写的是「拼音是不会拆时的退路」。真机实测把前提推翻了：
五笔码多为 3–4 码，而 3–4 个字母几乎总能切成一串合法音节（`hao` / `ni` / `xian` 全切得开），
于是「同分」实际等于「只要长得像拼音就判拼音」。`hao` 因此出 61 条同音字，五笔的「虚」
（三级简码、唯一精确命中）被压到第 13 页。

想吃拼音的候选：它就在五笔候选后面（`ni` 的第 2 条是「你」）。想**独占**拼音：打 `ni'`。

**为什么 `z` 要按能力位分家（D47）。** D42 定「组合态内吃掉 `z`」的前提是「`z` 不是五笔字根键」，
这个前提在混输下不成立：拼音里 `zhuang`、`zao`、`zhe`、`zui` 都以 `z` 开头，吃掉它等于
**整片 z 声母的字打不出来**。反过来也不能无条件放开——纯五笔环境（dist 里没带 `data/pinyin`）
放开 `z` 就没有 D42 那个「不打断输入」的保护。所以判断权交给 server：它装了拼音词库就置
`feature::PINYIN`，DLL 读到才改行为。

**码长上限同理。** `MAX_CODE_LEN = 4` 的语义是「五笔全码定长」，而拼音词条最长 20 个字母
（`linchuangzhengzhuang` = 临床症状）。不放开的话打长词会在第 5 个字母时被「原样上屏」。
`keys::MAX_PINYIN_CODE_LEN = 24`，并在**编译期**断言 `≥ 20`——哪天有人为了省事改回去，
编译就会炸，而不是等真机上打到一半被截。

**三个容易踩的点**：

1. **`primary_len` 必须夹到 `limit`。** 主段可能有 30 条、协议只让带 9 条，那 9 条就全是主段的；
   若直接 `primary.len()`，DLL 会拿到一个超过结果长度的分界，画出来整屏都是主段色，
   而实际第 10 条以后根本没有。当前写法是 `primary.len().min(limit)`，**在截断之前**算。
2. **候选缓存要连 `source` / `primary_len` 一起存。** 只缓存 `Vec<String>` 的话，命中缓存那次
   就丢了分区信息，同一串输入第一次和第二次画出来不一样。
3. **`features` 存在原子变量里，别每次按键去握手。** 键路径不能等 IPC（D18），而握手在后台
   线程；连不上时保持上一次的值——宁可沿用「有拼音」，也别在一次掉线后把 `z` 突然锁死。

**怎么验**：

```powershell
# ① 数据与判定（不需要 TSF 壳）
cargo run -p heinrich-server -- --selftest --scheme-dir data/wubi86 --pinyin-dir data/pinyin
#   期望看到：混输已启用 / hao 五笔占主段 + 拼音跟随 / hao' 只出拼音 /
#            xian 判拼音 / 折叠分界不越界 / vbg 仍判五笔

# ② 确认降级路径（把拼音目录指到一个不存在的路径）
cargo run -p heinrich-server -- --selftest --scheme-dir data/wubi86 --pinyin-dir C:\no\such
#   期望看到：纯五笔（无拼音词库），且 hao 判五笔 —— 行为与 M2 一致

# ③ 真机：构建 → 装新 DLL → 在记事本里打 hao / nihao / vbg / zhuang
powershell -ExecutionPolicy Bypass -File tools\build.ps1
```

**翻页与字符键（D49）**：有候选窗时 `-` 往前、`=` 往后，与 `←/→/PgUp/PgDn` 等价
（笔记本上 PgUp/PgDn 常要按 Fn，方向键又离手位太远，那三组键在真机上等于没有翻页）。
组合态内（`buffer_len > 0`）所有**会产生字符**的键一律吞掉（`keys::is_text_key`）——
放行会在组合串中间插字符、终止组合串，`OnCompositionTerminated` 把编码当残留删掉，
真机表现是「符号先冒出来，然后正在打的字消失了」。功能键 F1–F12 / CapsLock 不受影响。

**Tab 是这条规则的一个例外（D53）**：它不插字符，但会让宿主切焦点，焦点一动组合串
照样被终止、编码照样被删——所以判据不能只看「会不会插字符」，还要看「会不会让组合串
消失」。`decide` 里 Tab 走单独一支（`VK_TAB if !empty => Noop`），空缓冲时仍是制表符，
`Ctrl+Tab` 在 `mods.ctrl` 那关就放行了。真机验法：打 `vbg`，按 Tab，候选窗与编码应当
**原样留着**；再按空格出「好」。

真机上怎么看日志（`%LOCALAPPDATA%\Heinrich\logs\`）：

| 现象 | 日志 | 含义 |
|---|---|---|
| 打 `hao` 第 1 条是「虚」，后面跟着淡色的「好 / 号 / 浩…」 | `已连上 server（… 混输可用）` + `hao → 32 条，主段五笔 1 条` | 正常（D48：五笔优先，补段淡一档） |
| 打 `hao'` 只出拼音 | `hao' → 32 条，主段拼音 32 条` | 正常（`'` 钉死成纯拼音） |
| 打 `hao` 第 1 条是「好」 | 同上但 `主段拼音` | 判定没反转过来 ⇒ 检查 DLL 与 server 是不是同一个版本 |
| 打 `zhuang` 打不出字（`z` 被吃掉） | `已连上 server（… 纯五笔）` | server 没装拼音词库（`features` 没置位）⇒ 检查 `dist\data\pinyin` 是否存在 |
| 打拼音出的是字母 | `server 不可用` / `候选查询无应答` | IPC 没连上（D27 的降级），不是混输的问题 |

### 4.12 码提示 chip（M3.4a）

```
五笔词库 ──┐
           ├─ attach_wubi_codes（装载期一次）→ PinyinEntry.wubi
拼音词库 ──┘                                        │
                                                     ▼
hint.mode（config.toml / --hint） ──► Host::query 逐条裁决 → Entry.code（空串 = 不提示）
                                                     │
                              PROTO_VERSION 4 ───────┘
                                                     ▼
                     ipc::Reply.codes → Cands.codes → Candidates.hints
                                                     ▼
                        CandidateText.with_hint → ItemBox.hint_rect → Theme.chip_bg + 0.72 号字
```

**最容易错的一条：拼音候选的 `Candidate.code` 装的是拼音串，不是五笔码。** 用 `hao` 查会拿到
`code == "hao"`，直接拿去画 chip 就是「好 hao」。码的另一条通路是 `PinyinEntry.wubi`，由
`PinyinLexicon::attach_wubi_codes` 在装载时用五笔词库反查填好（临时 `词 → 全码` 表，填完即弃）。

**取全码不取简码**：`好` 的二级简码是 `vb`、全码是 `vbg`，chip 给 `vbg`。简码只是把全码后几码
省掉，先教完整的才不会学出一套「只会打短码」的手感。

**配置怎么读**（`crates/heinrich-scheme/src/config.rs`）：宽容解析 —— 坏行跳过 + warning、
未知键忽略、`config_version` 比程序新只警告。理由写在模块头：`scheme.toml` 是机器写的，
错了一定是生成器 bug；`config.toml` 是人手改的，一个拼写错误不该让输入法起不来。

```powershell
# 看当前生效的模式（server 日志里也有一行「码提示：mixed」）
Get-Content $env:APPDATA\Heinrich\config.toml

# 改档（改完 ≤1 秒生效，不用重启 server）
#   hint.mode = "mixed" | "first_time" | "off"

# 不改文件、临时验一档（压过配置文件）
cargo run -p heinrich-server -- --selftest --scheme-dir data/wubi86 --pinyin-dir data/pinyin
```

**`first_time` 是近似**：判据是「这个词还没上过屏」（`store.usage(word).n == 0`），不区分当初
是拼音还是五笔打出来的。严格版要记录「每次上屏的输入来源」——`Record` 帧里其实带着 `code`，
但学习库没存那一列，属于 M3.5。

**覆盖率实测**（`dist/data/pinyin` 65,125 条 × 五笔全码）：**单字 16,472 条 100%**、多字词组
28,050/47,953 = 58.5%、合计 45,181/65,125 = 69.4%。查不到的是五笔词库里没有那个词（多为长
词组），这类候选**不显示 chip**——宁可不说，也不给错的。装载日志会打这一行：

```
拼音词库：65125 条（415 个音节），混输已启用；其中 45181 条能给出五笔全码（码提示可用）
```

**怎么验**：

```powershell
# ① 三档行为（不需要 TSF 壳）
cargo run -p heinrich-server -- --selftest --scheme-dir data/wubi86 --pinyin-dir data/pinyin
#   期望：mixed → 五笔段不带码 / 拼音段带码（「好」= vbg）/ 主段无码 + 折叠区有码
#         off   → 一律不带码
#         first_time → 首次带码、上屏一次后不带

# ② 排版（CHIP 的几何与命中，需要桌面会话）
cargo run -p heinrich-cand-preview -- --hint --split 2
#   --hint 只给 split 之后的条目加码，正好是「折叠区 = 拼音段」的真实形状
#   截图：python tools\shot-window.py HeinrichCandidateWindowV1 dist\cand-hint.png

# ③ 真机（见 §4.11 的构建流程）
```

**验 `hint.mode` 时的一个坑**：DLL 在宿主进程里缓存过该码的 `Reply`（M1.3 的 64 条缓存，
上屏时清），而配置热更新发生在 server。所以「改档 → 再打同一个码」可能命中旧缓存，
看到的是上一档的样子。**先上屏一次那个码（或换一个码）**再验，就对了。这个陈旧窗口只影响
开发期的验证手感：真实用户不会每分钟改一次配置，M4 的设置窗口会在改档时一并重启 server。

**装完新 DLL 要先重启宿主程序**：TSF 的 TIP 是进程内 COM DLL，加载了就拔不出来（我们自己的
`DllCanUnloadNow` 恒返回 `S_FALSE`）。已经打开的记事本 / Chrome / Electron 应用里跑的仍是旧
DLL，它会拿 `proto 3` 去连 `proto 4` 的 server，握手吃 `Bye` → 降级成「编码原样上屏」。
新开的窗口自然是新版。查谁还持有旧 DLL：

```powershell
Get-Process | ForEach-Object { try { if ($_.Modules.ModuleName -contains 'heinrich_win.dll') { "$($_.ProcessName)($($_.Id))" } } catch {} }
```

### 4.13 学习排序与「连打不跳位」（D57 / D58 / D59）

两件容易混为一谈的事，改动时别搞反：

| 层 | 在哪 | 管什么 |
|---|---|---|
| 分数层 | `rank.rs` 的 `learning_score` | server **算出来**的顺序对不对 |
| 展示层 | `ipc.rs` 的缓存 | 算出来的顺序**什么时候让你看见** |

**分数层**：`total = w + g × ln(1 + max(0, n−1))`，**没有任何门槛**。`n = 1` 得 0 分
（与没用过一样），所以「偶用一次不改顺序」是次数约束，不是分数门槛。

> 这一层被改过三次，别走回头路：D44 王座（`≥ 1.5 × 首位动态分`）→ D54 绝对分差
> （`> 前一条 + 0.35`）→ D58 废除门槛。前两版都错在**拿固定分数表达「按几次」**：
> 单次增量是 `g × ln((n+2)/(n+1))`，n=0 时 0.2426，n=26 时只剩 0.0127。同一个 0.35，
> 在起点相当于 1.4 次按键，在 n=26 处相当于 27 次 —— 结果是用得越多的词越难越过别人。
> 想「更稳」时**不要再加分数门槛**，调小 `g` 或调大半衰期。

想在 `config.toml` 里调：`ranking.learning_gain`（默认 0.35，0 = 完全不学习）、
`ranking.half_life_days`（默认 45）。`ranking.promote_k` / `ranking.promote_margin`
都会各收到一条废弃警告并被忽略。

**近同分抖动区（D59）**：排序是纯比较、不记「谁在位」，所以两个词的分差一旦掉到单次增量
以下，每次上屏都会跨线。扳回代价 ≈ `|Δ| ÷ 单次增量`（n = 28 时单次增量只有 0.0123）。
这是纯分数比较的固有性质，**只能推远、不能封顶**：`云` 35 次时「去」按 1 次扳不动、按 2 次
扳回；云 40 次要 6 次、45 次要 10 次、50 次要 14 次。

> 看到「按两次就翻」时先算这条式子，别当 bug 去加门槛 —— 要「绝不换位」只有在位者滞回
> （记住上次顺序给在位者留余量），也就是被否掉的 D56。验收标准定为「原首位再上屏
> **一次**不夺回」。完整实测曲线见 PROJECT.md §5.2。

**展示层**：`record()` 只把该码标脏；`cache_get()` 在「这条脏了 **且** 上次**提交**的
不是它」时才真正丢掉。所以连打同一个码，第 N 条永远是同一个词；**提交过别的码**
回来才看到新序。

> 判据必须是「提交过」而不是「查过」（D57）。D55 拿查询当信号，结果打 `fcu` 之后
> 第一个字母 `f` 就已经是「另一个码」，脏标记当场被清掉 —— 真机上等于什么都没保住。
> 而且一次组合串内部本来就不会重排，所以这一层只有跨组合串才有意义。

为什么两层都要：只做分数层挡不住误选（上位那一刻位置就变了），只做展示层则
换码后照样会被一次误点置顶。

**怎么验**（真机，先重启宿主）：

1. 打一个有多条的码（如 `fcu`），从第 2 条开始连按数字 —— 中途**不应**出现「按 2 出来的是别的词」
2. 连打六七次，然后打另一个词并上屏，再回来 —— 它应该已经升到第 1
3. 此时再打一次原首位 —— 只要领先量仍大于单次增量，它就不该抢回第 1（云 27 次、差 0.0247 >
   0.0122 ⇒ 不翻；差掉到 0.0065 时按一次就翻，那属于抖动区，见上）
4. 顺序与次数同向：同权重下 26 次的「云」必须排在 9 次的「去」之前。**跨权重时先验优先**——
   真实库里「去」的码表权重比「云」高 0.0638（折合 n≈30 处 5–6 次按键），所以云要到 35 次
   左右才稳定压过 28 次的「去」

### 4.14 码位键位图（M3.4b / D50 / D60 / D61）

一句话：`Ctrl+Alt+N` 把候选的码**逐位摊成键帽**，每枚写着「这个键上有哪些字根」。
它回答的是「v 键为什么是 v」，**不回答**「好 这个字怎么拆」——后者被
`docs/design/probe-report.md` 证伪过（R3）。

v1.29 之前每枚键帽底部还有一行「该键的一级简码」（`H`=上、`V`=发），真机一看就废：
它答的是「这个键单独一按出什么字」，摆在字根正下方只会被读成「这一位取的是上」。
去掉的不是一条信息，而是一条会被误读的信息（D61）。`roots.toml` 的 `primary` 字段
仍然保留——`roots check` 第 5 项靠它与码表交叉校验。

## 数据：`data/wubi86/roots.toml`

25 个 `[[key]]`，每键 `key` / `zone` / `mnemonic` / `primary` / `roots` / `variants`。

**口径**（唯一需要人来判断的地方）：按 86 版助记口诀逐键展开，把教材并列的常用偏旁
（氵 扌 灬 忄 纟 讠 钅 犭）算作主字根，**不把**只在部首补充区（U+2E80–2EFF）和扩展 B 区
出现的符号算进去——它们在常规字体里要么是豆腐块，要么与常用字撞脸（`𠭍` 长得就像「雯」）。
含义由助记词承担：`I` 的助记照样写着「水旁兴头小倒立」，人读得懂，机器不需要那个字形。
结果正好落在公开口径 **130** 条上。

五处校验里只有一处是「正确」而不是「自洽」：`primary` 必须等于码表里的单字母码
（`v`→发、`b`→了）。其余靠助记人工核对——`roots check` 只能说「没写重复、没漏键」。

```
cargo run -p heinrich-cli -- roots check     # 五项
cargo run -p heinrich-cli -- roots vbg       # 逐位展开
```

## 数据怎么到 DLL（D60）

DLL 只依赖 `windows` + 自有 crate，够不着 `heinrich-scheme` 里的 TOML 解析；而让 DLL
自己读文件（多一份解析器）或 `include_str!`（数据两份）都不划算。所以：

```
server: Host::roots_frame()  →  pipe.rs 每条连接第一次 Welcome 之后多发一帧
DLL   : ipc::exchange() 读到 Roots 就缓存、继续读真正的应答
```

两个必须记住的点：

1. **它排在 `Welcome` 后面**。客户端握手是「发 Hello、读一帧」，先发字根表会让 `open()`
   把它当成握手应答（日志里是「握手收到意外应答」）。
2. **客户端读应答要跳过它**。这是协议里第一条不是应答的帧；不跳的话下一次查询会读到
   它、再下一次读到上次的候选，整条管道永久错位一帧（而错位的样子是「查询返回了奇怪的
   东西」，看起来像引擎 bug）。

## 码从哪来（`keycap.rs`）

| 优先级 | 来源 | 场景 |
|---|---|---|
| 1 | 候选自己的 `code` | 拼音段的候选带五笔码：打 `hao` 选「好」→ 展开 `v b g` |
| 2 | 当前缓冲 | 五笔段的候选不带码（`mixed` 只给拼音段发码），而用户手上那串字母就是码 |
| 3 | 都没有 | 不弹 |

任何一位找不到（`z`、非字母、表里没这个键）就**整体不弹**：半张键位图比没有更容易
让人误读成「这个码就只有这几位」。

**纯五笔段按 `Ctrl+Alt+2` 与按 `Ctrl+Alt+1` 弹出同一张图，不是 bug**：那一页每条候选的
`code` 都是空串（server 只给拼音段发码提示），全部回落到同一个编码缓冲，于是每一位展开
的键帽完全相同；`KeycapPanel::show` 里的 `shown == code` 还会直接把重画也跳掉。要让
「第 N 条」有区别，前提是那条候选自己有一条码——拼音段有（`好 → vbg`），五笔段就是
用户手上那串，本来就只有一条（D61 ④）。

## 版式（三轮真机截图调出来的）

| 参数 | 值 | 为什么 |
|---|---|---|
| `roots_per_line` | **3** | 两列时 q/n 的八个字根要排四行，键帽变 80×256 的长条；四列又把「雨」「鼎」挤在一起 |
| `min_cap_width` | **2.2 倍字号** | 3.2 会把内容只有 80px 的键撑成 102px 的空框 |
| 说明行对齐 | **左对齐到内容区左缘** | 右对齐时第二行看起来像另一段话 |
| 字根块 | 在 `max_rows` 内**垂直居中** | 留白全压中间让人以为字根没列完 |
| 主字根 / 变体 | 实色 / 淡一档，分界来自 `main_root_count` | 按索引硬编码会在 G 键（4 主 + 2 变）之外全部着色错误 |
| 键帽底部 | **不画一级简码**（D61） | 它与「这个字用什么字根」无关，摆在字根下面必然被误读；删掉后键帽高度少一行 |

说明行还要**避头**：逐字量宽断行时标点不能落在行首，否则「——」会被拆到两行。

## 关闭路径（一条都不能漏）

上屏 / Esc / 失焦 / 继续打字 / 换码。前四条走 `Core::clear_popups()`（候选窗与键位图一起
收），「换码」在 `refresh_candidates` **入口**单独关一次——新码也查到候选时不会走 `clear`，
但那张图同样过期了。

`show_keycaps` 里还有一个借用纪律：**一次借用里取完 `rect` / `code` / `flipped` 就放开**，
因为 `keycap.show` 要建窗、要量字，期间若还持着 `cands` 的借用，任何一次重入都会 panic。

## 怎么验

1. `tools\check.ps1` 全绿（含「字根表校验」「server 自测 → 字根表三项」「键位图自测」）
2. `heinrich-cand-preview.exe --keycaps vbg` / `--keycaps qnk` / `--keycaps vbgu` 看版式
3. 真机（先重启宿主）：打 `hao` → `Ctrl+Alt+1` 弹「虚」的键帽（码 `hao`，h/a/o 键上的字根）；
   `vbgu` 平铺四枚；上屏 / Esc / 切窗口 / 再打一个字母四种方式都能关掉；没有候选时按
   `Ctrl+Alt+1` 放行给宿主。要验「不同候选不同键帽」，走**拼音段**：`'hao` 后按
   `Ctrl+Alt+1` / `Ctrl+Alt+2`，两条候选各有自己的码，键帽才会不同。顺带确认按
   `Ctrl+Shift+1` **不弹**（那是 Windows 切换输入法的键，见踩坑 34）

### 4.15 `z` 通配与段级多段（M3.5 / D62）

两块功能有一个共同点：**它们都不需要猜**。`z` 的位置是用户打的，段的边界是用户用 `'` 写的。

**`z` 通配（§6.8）**：`wildcard_stem` 只认「末位 `z` + 前缀 1–3 位全在 a–y」。去掉 `z` 后走
`Scheme::match_indices`（前缀二分），排序仍按 §5，然后**按字去重**——`vb` 与 `vbg` 都是「好」，
不去重首屏会被同一个字占满。拼音前缀结果放折叠区，免得「打 `niza` 停在 `niz`」时候选窗整片变五笔。

**多段（§6.5 / D64）**：`split_segments` 按 `'` 切，`'` 标记它前面那段是拼音；末尾 `'` 标记最后一段。
`MAX_SEGMENTS = 2`。每段取前 32 条（`SEGMENT_LIMIT`）做笛卡尔积，顺序是「段 1 主序、段 2 次序」；
组合结果**必须命中词典真词**（`Scheme::has_word` ∪ `PinyinLexicon::has_word`，各自 `OnceLock<HashSet>`
惰性建集）才保留——拼不出任何真词时整体不出窗（`Mixed::none`），不做字符拼接的假词组。
chip 恒挂段 1（`'` 语法决定了段 1 必是拼音段）：`Candidate.chip = ChipHint { code: 段 1 字的五笔全码,
chars_before: 1 }`，经协议 v6 的 `Entry.hint_at`（chip 前字符数，0 = 词尾）下发，候选窗把词拆成
`好[vbg]吃` 的前后两截夹 chip 排版。多段时 `Candidate.wubi` 留空。

**码提示的例外**：`Mixed.show_codes` 为真时，host 取 `Candidate.code` 而不是 `wubi`。
新增这个开关而不是复用 `wubi`：通配时 `code` 是那条词自己的五笔全码，多段时 `code` 是分段码——
两者都**不是**「拼音串反查出来的五笔码」，硬塞进 `wubi` 会让字段名彻底失去意义。
（D64 后多段不再走 `code` 下发 chip，改走 `Candidate.chip` + `hint_at`；`show_codes` 只覆盖通配。）
**chip 的裁决规则**：`chip` 存在就显示（不问 `hint.mode`）——多段拼音字是「教学时刻」，
用户故意混打，就该告诉他码。

**调参与验法**

| 位置 | 值 | 改了会怎样 |
|---|---|---|
| `wildcard_stem` 前缀上限 | 3 | 放到 4 等于问「第 5 码」，五笔没有第 5 码 |
| `MAX_SEGMENTS` | 2 | 3 段按 32³ 涨到 32768 条组合，翻不过来 |
| `SEGMENT_LIMIT` | 32 | 真词过滤会滤掉大部分组合，池子必须比一页大。**16 → 32 是实测调的**：`wu` 的同音候选里「悟」排第 17，`wu'ntgg`（悟性）因此出不了窗；32 留一倍余量，组合上限 256 → 1024 次 `has_word`，debug 构建实测 589 µs/例、release 再快一个量级，按键感知不到 |
| `MAX_EXTENDED_CODE_LEN` | 9 | 两段各 4 码 + `'`；只在含 `'` 或末位 `z` 时启用 |

验法（都不用真机）：

```
cargo run -q -p heinrich-cli -- look vbz --data data/wubi86
cargo run -q -p heinrich-cli -- look "hao'vb" --data data/wubi86 --pinyin data/pinyin
```

真机（先重启宿主——协议 v6，旧 DLL 会被 `Bye` 拒）：打 `vbz` 应看到「好 vbg / 好友 vbdc / 她 vbn …」
且每条都有码；打 `hao'ktn` 应**只出「好吃」**且 chip 在「好」后面（`好[vbg]吃`）；`hao'vb` 出「好好」；
`hao'ni'` 这类拼不出真词的组合**不出候选窗**。空格上屏应得到完整的词。

### 4.16 自定义词组（M3.6 / D65）

**为什么有这一节**：词表跟不上新词是长期状态，不是一次性的数据问题。抽 42 个现代常用词，
14 个（微信 / 扫码 / 点赞 / 朋友圈 / 笔记本电脑 / 表情包 / 健康码 / 弹幕 / 外卖小哥…）
在两份词库里都不存在。这条功能的目标就是「用户自己补，且不必会拆字」。

**数据流**

```
%APPDATA%\Heinrich\words.tsv        词 \t 拼音 \t 码 \t 权重（后三列可空）
        │  user::load（宽容：坏行跳过 + 警告，BOM/CRLF 都吃）
        │  user::resolve（手填 > 反查 > 推导）
        ▼
Resolved { word, pinyin?, code?, weight }
        │  split_for_merge
        ├─ Scheme::apply_user_words        （五笔侧：码 → 词，恒 4 码全文）
        └─ PinyinLexicon::apply_user_words （拼音侧：拼音 → 词 + 该词的五笔码）
```

**三列各自怎么补**（`user::resolve`）

| 列 | 手填缺失时 | 还拿不到 |
|---|---|---|
| 拼音 | 拼音词库整词反查（`pinyin_of`，取权重最高的读音）→ 逐字拼（`pinyin_of_text`） | 警告；这个词只在五笔里可用 |
| 码 | `Scheme::word_code`（86 词组规则） | 警告；这个词只在拼音里可用 |
| 权重 | `Scheme::default_weight`（同词长**非零**权重中位数） | — |

两条都拿不到的词整条丢弃并记警告——它哪条路都打不出来。

**取码规则不是猜的**：拿词库 61,194 条现成词组回推对照，一致 61,172 条（99.96%），
不一致的 22 条集中在「足字旁同码」（`蹒跚` 词库给 `kakm`、规则给 `khkh`）——那类人工
特例不影响「用户能打出来」这个目的。单字全码表来自 `Scheme::chars`（惰性建，
取字典序最小，与 `attach_wubi_codes` 同一约定）。

**合并是全量替换，不是叠加**：`apply_user_words` 先把 `entries` 截回 `base_len`
（装载时的基础条数），再追加这一份用户词。这样「删一行 → 重载 → 那个词真的消失」，
而且不需要任何撤销逻辑。截断安全的前提是用户词恒追加在末尾——**别在别处往
`entries` 中间插东西**。

**去重**：同一 `(码, 词)` 只留一条，冲突时删下标更大的那条（基础库在前 ⇒ 基础库胜）。
拼音侧遇到相同 `(拼音, 词)` 不新增，但如果基础条目的 `wubi` 为空就把它补上——
拼音词库本来没有五笔码，补上之后混打时那条候选才有 chip。

**热生效**：`WordsState` 与 `HintState` 同一套「节流 1 秒 + `(len, mtime)` stamp」，
挂在 `Host::query` 开头。**没有新增协议帧**：最初打算加 `Reload` / `Reloaded` 并升 v7，
实现时发现这条路径本来就在跑配置热更新，而「按键」是看到候选的前提——升协议的真实
代价是「旧 DLL 被 `Bye` 拒 → 重装 → 重启所有程序」，为一个看不见的差别不值（这段
取舍记在 `PROTO_VERSION` 的版本史里）。

**自测里不读用户目录**：`Host::open` 不碰 `%APPDATA%`，`--selftest` 也强制 `None`。
否则「同一份代码在两台机器上结论不同」，而那正是自测要排除的东西。

**怎么验**

```
# 加词（拼音与码全自动）
cargo run -q -p heinrich-cli -- word add 微信 --file out/t.tsv --data data/wubi86
# 查得到：tmwy 的第一条
cargo run -q -p heinrich-cli -- look tmwy --data data/wubi86 --pinyin data/pinyin --words out/t.tsv
# 拼音侧也通，且带码
cargo run -q -p heinrich-cli -- look weixin --data data/wubi86 --pinyin data/pinyin --words out/t.tsv
# 删词后必须消失（全量替换）
cargo run -q -p heinrich-cli -- word rm 微信 --file out/t.tsv --data data/wubi86
```

`check.ps1` 的「自定义词自测」把这条链跑一遍（临时文件，绝不碰真词库）。
真机（先换 server.exe、不必动 DLL——协议没变）：`word add 微信` → 切回输入框 →
打 `tmwy` 或 `weixin` 应看到「微信」；再 `word rm 微信`，1 秒后那个词消失。

**最后一道坎在 DLL，不在 server**（D67 / 踩坑 45）：每个宿主进程各有一份候选缓存，
它原来的失效条件只有「上屏后标脏」，于是**加词之前打过的码**加完再打还是旧结果，
而且因为命中缓存，server 日志里连 `Query` 都不会出现——看起来就像 server 没生效。
现在 `CACHE_TTL = 3 秒`：同一个码最坏等 3 秒就重问一次。改这一处之后**必须换 DLL**，
而 DLL 是进程内加载的，**已经开着的宿主（Notepad++ / Chrome）要重启才会用上新的**。

想知道「常驻 server 此刻到底返回什么」，用 `out/probe_pipe.py`（见踩坑 45），
别用 `heinrich-cli look`——它自己新起一个 `Host`，永远是「有」。

### 4.17 混输总验收语料（M3 总验收）

`crates/heinrich-core/tests/mixed_corpus.rs` + `tests/corpus/mixed.tsv`。这是**唯一**跑真实词库的
测试（其余全用手写迷你夹具），因为要验的是「判定规则在 13.6 万条真实数据上对不对」，
夹具尺寸下看不出池子、权重、多音字这些真实分布的边角。

**语料怎么来的**：`tools/gen-corpus.py`（一次性工具，不进构建）从 `data/` 两份词库分层取样——
纯五笔 50（其中 25 条特意选「码恰好也能切成一串拼音音节」的，验 D48 五笔优先）、
纯拼音 50、交替 50（段 1 取首字的真实读音、段 2 取次字的全码），另加 4 条实测确认过的负例。
改词库后重跑脚本即可刷新语料，`kind` 列的语义见文件头注释。

**断言四件事**：

1. `expect_source`：判定来源（`wubi` / `pinyin` / `empty`）。
2. `expect_word`：那个词必须在首屏（多段还要 chip 挂在第 1 个字之后）。
3. **首屏只放当前判定语言**：主段的每条候选都得能在对应语言的词库里查到。这条比逐个比对
   词表更狠——连「混进了另一语言的候选」都能抓到。多段例外，按两库并集查（D64）。
4. 负例与「拼不出真词」的严格模式：`Mixed::is_empty()`。

**数据缺失时跳过**（`data/` 是生成物，刚 clone 不存在是正常状态）；`check.ps1` 跑
`cargo test` 时它自动包含在内。测试末尾会打一行「154 例查询耗时 / 均 xx µs」，那是
调 `SEGMENT_LIMIT` 时的量化依据（`--nocapture` 才显示）。

**踩过的坑**（已收进 §12 的 40 / 41）：负例不能凭语感写，语料不能只取高频词。

### 4.18 设置窗口（M4a，`apps/windows/settings`）

**它有多轻**：只写 `words.tsv`。生效靠 server 的 mtime 节流（同一秒路径，见 §4.16），
所以这个 exe **不碰管道、不碰协议、不碰 SQLite**——关掉它，加的词照样在。这是 D65
选「热重载而不升协议」换来的第二次红利（第一次是 CLI 不用 IPC）。

**为什么不再自绘**：候选窗必须自绘（D35）是因为要画 chip 与 emoji，宿主控件做不到；
设置窗口没有这个需求，而编辑框里要**用亨利自己打中文**——自绘就得连光标、选区、
IME 组合串全实现，约为原生三倍工作量。判据是「有没有宿主控件做不到的绘制需求」，
不是「观感统一」。以后真想统一，只换绘制层，`layout`/动作逻辑不动。

**结构**

| 文件 | 职责 |
|---|---|
| `main.rs` | 参数（`--words` / `--data` / `--config` / `--db` / `--emoji` / `--page` / `--selftest`）、DPI 声明、`--selftest` 七项断言 |
| `emoji_page.rs` | emoji 页数据层（M4d / D85）：两层映射合并视图、`Key`（按词/按码）、改绑 / 停用 / 恢复内置、整份重写用户层（临时文件 + 改名）、`emoji.enabled` 开关（开 = 删行）、`choices_for_picker()`；不依赖 Win32，`--selftest` 直跑 |
| `emoji_picker.rs` | emoji 分类网格选择器（M4d 返工 / D86）：`cat_of` + `Catalog` + `Geometry` 是**纯函数**（8 条单测，不开窗口就能验命中），窗口层用 D2D DC 渲染目标画彩色字形、私有模态循环。见 §4.24 |
| `lex.rs` | 找 `data/wubi86`（exe 同级 → 上三级 → cwd）、装载 `Scheme` + `PinyinLexicon` |
| `model.rs` | **不依赖 Win32**：`Model`（文件 + 词条）、`Row`（原文 + 推导列）、`*_cell()` 的 `*` 约定 |
| `ui.rs` | 窗口类、Tab、ListView、编辑面板、按钮动作、`layout()` |

`model.rs` 与 Win32 无关是刻意的：自测直接跑它，窗口只负责「把 Row 画进 ListView、
把编辑框写回 Model」。这样「保存会不会留下 .tmp」「改词名会不会留下旧行」这类问题
不需要人点鼠标就能验。

**编辑面板只显示文件里的内容**（不显示推导结果）。否则用户点一次「保存」，
`weixin*`（推导）就变成 `weixin`（手填）——从此词库升级再也影响不到这个词。
这是「可编辑 = 文件内容 / 显示 = 最终效果」的分界，界面上靠 `*` 与脚注交代。

**面板常驻可编辑，没有「新增」按钮**（M4a 返工）。原设计是「点新增 → 清空面板 → 填词 →
保存」，问题是按「新增」列表毫无反应（它本来就只清面板），用户合理地以为词已经进去了。
现在：面板从打开窗口就能输入，点「保存」即加入/更新；「清空」只做字面那件事（清面板 +
取消选中）；「删除」要求有选中且面板里就是那个词。按钮亮灰跟着 `EN_CHANGE` 走（词框空时
「保存」是灰的），所以不存在「点亮了但点下去只得到一句抱怨」的假按钮。开窗也**不再自动
选中第一行**——面板是给新词用的，一开窗躺着某条已有的词，改完点保存就把它改掉了。

**保存之后必须回到「干净的新词态」**（F46，见踩坑 46）：清四个框、取消选中、焦点回词框。
理由不是美观——面板里留着上一个词、`editing` 又指着它时，下一次保存会被判成「改名」，
于是删掉刚加的那个词。**状态机里「刚保存完」这个中间态必须等价于「刚开始加词」**。

改名只认「从列表点进来的那一行」：`editing` 只在用户点列表时赋值，且词名与那一行不同
才走「删旧 + 加新」；没选中任何行时词名就是键，保存 = 新增或覆盖。

**回车 = 保存，Esc = 清空**：给四个编辑框挂子类化过程（`edit_proc`），收到 `WM_CHAR`
的 `0x0D` / `WM_KEYDOWN` 的 `0x1B` 就父窗口发一条 `WM_COMMAND(ID_B_SAVE)` /
`WM_COMMAND(ID_B_CLEAR)`——和按钮被点一下时发的是同一条消息，所以走的是同一个
`on_save`，不存在第二条保存路径。加词是「打一个词 → 回车 → 打下一个词」的节奏，
每个词都要把鼠标挪到按钮上，手感本身就会让人怀疑「是不是没加上」。

面板下方那行说明是动态的：没选中 → 讲怎么加词；选中某行 → 直接写「打字时输 `tmwy`」。
这条是踩坑 45 换来的。

**字段名与底色**（F47）：四个 `词/拼音/码/权重` 用 `SS_CENTER | SS_CENTERIMAGE`（水平 +
垂直居中），`WM_CTLCOLORSTATIC` 回**父窗口背景刷**（`COLOR_BTNFACE`）而不是白色。后者
差十几级灰度、看图容易漏，`--selftest-ui` 直接发一次真的 `WM_CTLCOLORSTATIC` 比对返回
画刷（见踩坑 47）。

**自己验**

```
cargo run -q -p heinrich-settings -- --selftest --data data/wubi86      # 数据层七项
cargo run -q -p heinrich-settings -- --selftest-ui --data data/wubi86   # z 序 + 命中 + 面板流程 + 字段名样式
# 带一组假词开窗口（不会碰 %APPDATA%）
cargo run -q -p heinrich-settings -- --words out/ui-words.tsv --data data/wubi86
```

`--selftest-ui` 是踩坑 44/45 换来的，两段断言：

1. **z 序 + 命中**：`RealChildWindowFromPoint` 在 10 个可交互控件的 5 个点上做命中测试，
   命中的必须是它自己。界面看着对、点哪儿都没反应这种问题，截图与 `WM_COMMAND` 脚本
   都照不出来。
2. **面板 → 保存 → 列表**：空面板三个按钮全灰 → 填词后「保存」「清空」亮 → `BM_CLICK`
   保存后**磁盘文件里真的有这个词**、面板回到新词态 → 再填一个词再保存，**两条都在**
   （F46 的回归）→ 点列表某行（真发 `LVM_SETITEMSTATE`、走 `LVN_ITEMCHANGED`）改词名，
   旧词消失、新词与其它词都在 → 「清空」回到新词态。断言落在文件上，不落在界面字符串上
   （界面骗人的事刚发生过）。

它建窗时用 `SW_SHOWNOACTIVATE` 显示一下（不抢焦点），验完即销毁，跑的是临时词库。

窗口的观感/对齐靠 `win-ui-selfscreenshot` 那套截图（`PrintWindow` 对普通窗口有效，
分层候选窗才需要 §4.15 的 `--save-bmp`）。**注意：后台任务结束后进程会被带走**，
截图必须在同一次调用里「启动 → 睡 2 秒 → 截」。

### 4.19 设置窗口怎么被打开（M4a7 / D69、D70、D71、D73）

**先记住结论：开始菜单是主入口；任务栏角标的语言栏按钮是第二入口（M4a8）；系统设置的
「选项」按钮指望不上。**

| 入口 | 触发 | 代码 | 可用性 |
|---|---|---|---|
| 任务栏角标「亨/英」图标 | 左键切中英；右键 → 设置 / 打开词库文件夹 | `tsf/src/langbar.rs` | **真机通过**（图标随切换变、左键、右键菜单、Alt+Tab 往返后仍在；与输入法列表图标可共存，见 D74；句柄纪律见踩坑 52） |
| 开始菜单「亨利五笔 设置」 | 用户点 .lnk | `setup/src/shortcut.rs` | **主路**，写两处位置 |
| 系统设置 → 语言选项 → 键盘 → 亨利五笔 → 选项 | `CoCreateInstance(IID_ITfFnConfigure)` 后调 `Show` | `tsf/src/text_service.rs` + `tsf/src/launch.rs` | 接口在，但 Win10 22H2 **不画按钮** |

**语言栏按钮（M4a8 / D71）**：小狼毫同款路径（`WeaselTSF/LanguageBar.cpp`）。按钮是独立
COM 对象（`LangBarButton`，`#[implement(ITfLangBarItemButton, ITfSource)]`），不引用
`TextService`（避免引用环），中英模式经 `Rc<Cell<Mode>>` 与 `Core.mode` 共享；
`Activate()` 时 QI `ITfLangBarItemMgr → AddItem`、`Deactivate()` 时 `RemoveItem`，
不需要 `ITfFunctionProvider`。guidItem 用 `GUID_LBI_INPUTMODE`（接入 Win10 任务栏角标
显示逻辑），样式 `BTN_BUTTON | BTN_MENU | SHOWNINTRAY`。

端到端测试 `langbar_item_survives_activate_and_deactivate`（`#[ignore]`，手动跑）钉住的
三个坑——测试走 `ITfInputProcessorProfileMgr::ActivateProfile` 真激活，**测的是 dist 里的
DLL**，跑之前必须 `tools\build.ps1`：

```powershell
tools\build.ps1
cargo test -p heinrich-win --lib langbar_item_survives -- --ignored --nocapture
```

1. **`ActivateProfile` 的 dwflags 必须带 `TF_IPP_FLAG_ACTIVE(1)`**——传 0 恒
   `E_INVALIDARG`，TIP 与普通键盘布局都一样，hkl 给不给都一样。这是本次调试最大的一笔
   浪费：先怀疑「langid 不是当前语言」（那是老接口 `ActivateLanguageProfile` 的限制）、
   再怀疑 hkl（`GetProfile` 查回的也是 0x0），最后才试 flag。
2. **hkl 传 0 即可**——`GetProfile` 查回我们 TIP 的 hkl 就是 0x0。
3. **恢复原 profile 要分类型**——`GetActiveProfile` 查回 clsid 为零说明原来是普通键盘布局
   （如美式键盘），要用 `TF_PROFILETYPE_KEYBOARDLAYOUT` + 同款 ACTIVE flag 恢复；按
   INPUTPROCESSOR 恢复零 clsid 也是 `E_INVALIDARG`。

另一个硬约束（试出来的）：按键 sink 与语言栏都只认 **TSF 亲自经 profile 激活**的 TIP——
测试进程里手动 `CoCreateInstance` TIP + `tip.Activate(tm, tid)`，`AdviseKeyEventSink` 直接
`E_INVALIDARG`；用 `ITfClientId::GetClientId` 拿 tid 也不行。所以端到端测试只能走
`ActivateProfile`。

**图标要改怎么办**：

```powershell
powershell -ExecutionPolicy Bypass -File tools\make-icon.ps1            # 生成 data\icon\heinrich.ico
powershell -ExecutionPolicy Bypass -File tools\make-icon.ps1 -Preview C:\tmp\p.png   # 顺便出 32×32 预览
tools\build.ps1                                                          # 复制进 dist
cd dist; .\heinrich-setup.exe install                                    # 把 IconFile 写进注册表
```

三处图标彼此独立，别指望改一处全变：**任务栏角标**由 `langbar.rs` 的 `draw_icon` 运行时
GDI 画（白「亨」/「英」）；**输入法列表**读注册表 `IconFile`（`dist\heinrich.ico`，
D72）；**设置窗口标题栏**是 Windows 默认图标（没有 exe 资源，M4e 与安装器一起做）。

改 `draw_icon` 时特别注意 `GetIcon` 的**句柄纪律**：每次调用都要交**新句柄**，最近 16 个
不销毁（`IconSlot::fresh`）。这条被写反过一次，四个症状一起出现（图标不刷新 / 点击失灵 /
Alt+Tab 消失 / 第二次切换起不动），详见踩坑 52 与单测
`every_get_icon_call_hands_out_a_fresh_handle`。

**为什么「选项」指望不上（D70）**：两次推断都被实测推翻——v1.43「取不到接口就不画按钮」
（微软五笔/拼音的 CLSID 连 `InprocServer32` 都没有、`CoCreateInstance` 直接
`REGDB_E_CLASSNOTREG`，却照样有按钮）；v1.44「只给微软自家画」（我们 `S_OK` 没按钮，
搜狗五笔也没有）。真正的判定条件没找到：页面渲染列表时 `SystemSettings.exe` 不加载任何
TIP DLL（模块表 + 日志为证），判据是否发生在点选行时未查明。**已决定不再追**。实现留着
（`ITfFnConfigure` 按 MS 文档是对的，遗留属性框可能用），`status` 里那项只表示「接口就绪」。

**TSF 侧**：`ITfFnConfigure` 挂在 `TextService` 上（不是另起一个 COM 类），所以
`DllGetClassObject → ClassFactory → QueryInterface` 这条既有链路一行没改。父接口
`ITfFunction` 只有一个 `GetDisplayName`（返回「亨利五笔 · 设置」）。

`Show` 的三条约定（都不在类型系统里，只能靠代码和注释守住）：

1. **立即返回**——不等窗口关闭。
2. **已有窗口就复用**——`FindWindowW(heinrich_tip_id::SETTINGS_CLASS)` 命中就
   `ShowWindowAsync(SW_RESTORE)` + `SetForegroundWindow`。settings 侧自己也有同样的
   检查（`ui::focus_existing`，见下），两道各自独立、都靠窗口类名。
3. **失败也返回 `S_OK`**，原因写 `tsf-<日期>.log`。

**设置窗口自己的单实例**：`main` 在 `--selftest-ui` 之后、`App::new` 之前调
`ui::focus_existing()`——`FindWindowW(SETTINGS_CLASS)` 命中就还原+前置、打印 pid、
退出码 0。**不用命名互斥体**：要防的是「两个窗口各持一份内存词表快照」，判据取
「有没有窗口」比「有没有进程」更贴近；进程活着但窗口已销毁时，新实例应该正常开窗。
验证（两次启动 + 数进程）：

```python
import subprocess, time
exe = r"dist\heinrich-settings.exe"
p1 = subprocess.Popen([exe]); time.sleep(1.8)
p2 = subprocess.Popen([exe], stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
out, _ = p2.communicate(timeout=10)
print(p2.returncode, out.decode("utf-8", "replace"))   # 0 已有设置窗口（pid=…），已切到前台
# 再用 tasklist 数 heinrich-settings.exe：应为 1
```

反向验证：把调用换成 `ui::focus_existing().filter(|_| false)`，重编译后两次启动会留下
2 个进程。**别用 Start-Process 起 GUI**（工具侧会拦，且 job object 会连带收掉进程树）；
`subprocess.Popen` 在同一条命令里启动、检查、`terminate` 才稳。

**自验**（不需要点设置页）：

```
python tools/probe/probe_fnconfigure.py            # 完整：接口 + 显示名 + Show + 复用
python tools/probe/probe_fnconfigure.py --no-show   # 只验接口存在性（反向验证用）
python tools/probe/tray_shot.py target\tray.png 4   # 截任务栏通知区并放大（判「那格是谁画的」）
```

`tray_shot.py` 是排查语言栏问题的关键工具：它把通知区从屏幕 DC 抓下来放大 4~6 倍，
一眼能分辨角标上是**我们自己 GDI 画的图标**还是**系统输入指示器画的 profile 图标**
（对照本机微软五笔：带细框的「五」＝系统画的）。比让人盯着 16×16 的图标猜可靠得多。

输出应形如：

```
CoCreateInstance(ITfFnConfigure) hr=0x00000000
GetDisplayName        hr=0x00000000  显示名='亨利五笔 · 设置'
Show #1               hr=0x00000000  耗时 10.9 ms（必须立刻返回，不能等窗口关闭）
OK: 窗口出现，pid=30500
Show #2               hr=0x00000000  pid=30500（同一进程，已复用）
```

**别用 `tasklist` 判断窗口在不在**：探针跑在 Bash/CI 的 job object 里，父进程一退出整棵
进程树会被收掉，「窗口」与「进程」在那种环境下不等价。用 `FindWindowW` 判窗口、用
`GetWindowThreadProcessId` 取 pid。

**安装器侧**：`create_settings_shortcut` 先写 `FOLDERID_CommonPrograms`（机器级安装 → 公共
开始菜单，避免 `runas` 到另一个管理员账户时把 .lnk 写进看不到的用户目录），失败就退
`FOLDERID_Programs`（`%APPDATA%\...\Start Menu\Programs`，不需要提权）。开发态用
`heinrich-setup shortcut` —— 只写用户级、不弹 UAC：

```powershell
cd dist
.\heinrich-setup.exe shortcut   # ✓ 已创建 %APPDATA%\...\Start Menu\Programs\亨利五笔 设置.lnk
.\heinrich-setup.exe status     # 「开始菜单入口」一行应为 ✓
```

两个取舍：

- 公共开始菜单**只有提权进程能写**，所以 `cargo test` 改用临时目录
  （`create_settings_shortcut_in`），验的是同一段 ShellLink 代码。
- 断言必须读到**目标路径**（`IShellLinkW::GetPath`），不能只 `IPersistFile::Load`。
  `Load` 只证明文件结构合法——指向随便一个文件也能过，那是假绿。反向验证：把期望值
  改成 `notepad.exe`，断言立刻失败并打出 lnk 里的真实目标。

### 4.20 日志：按天分文件与总量闸（D76，`crates/heinrich-log`）

DLL 的 `tsf-<日期>.log` 与 server 的 `server-<日期>.log` 共用一份实现，两侧 `log.rs` 只是薄壳
（级别枚举 + 宏名 + 启动 banner）。要改格式 / 轮转规则，改 `crates/heinrich-log/src/lib.rs`。

| 项 | 规则 |
|---|---|
| 路径 | `%LOCALAPPDATA%\Heinrich\logs\{tsf,server}-<yyyymmdd>.log` |
| 保留 | 7 天（`KEEP_DAYS`）。每进程**首次写日志**时扫目录删过期文件，不放在每行上 |
| 单日软阈值 | 8 MiB（`SOFT_CAP`）：此后只放行 `warn` / `error`，并写一行 `W` 说明 |
| 单日硬上限 | 32 MiB（`HARD_CAP`）：当天停写，第二天自然换新文件 |
| 去重 | **没有**。每触发一次落一行——「发生一次」与「发生一万次」必须能从日志区分 |
| banner | 每进程首行：宿主 exe + pid + ABI + 协议版本 + 当前级别 |

**为什么不做「写满就 rename」**：写者是几十个我们控制不了生命周期的外部进程
（chrome 的每个 tab、explorer、编辑器）。rename 需要所有写者先松手，
而改名那一刻正在写的进程会用旧句柄写进一个已不存在的路径——内容直接丢，
而且丢得无声无息。文件名带日期则「跨天」不需要任何动作：
本来就要 `open` 一个新文件，切分是这件事的副产品。

**为什么不去重**：这条是用户明确要求的。日志要表明真实情况，触发一次就要写一行；
「10 秒内同类只记一条」会让「偶发一次」和「每 5 秒一次的重试」在日志里长得一样，
而这恰恰是最需要区分的量（`E server 拒绝握手` 5 秒一条 = 每个受影响进程 5 秒重试一次，
能反推有几个宿主还加载着旧 DLL）。体积因此交给**总量闸**：单日 32 MiB 到顶就停。

**单测进程要换个文件名**：`cargo test` 编出的二进制叫 `heinrich_win-<hash>.exe`，
它真的会构造语言栏按钮、切模式、调 `Show(false)`。如果和真机宿主写同一个文件，
`tsf.ps1 log` 的第一屏有一半是单测的调试行，还会混进只属于测试的 `E`。
所以 `apps/windows/tsf/src/log.rs` 按 `cfg(test)` 换 stem 到 `tsf-test-<日期>.log`，
`tsf.ps1` 挑「当前日志」时同时排掉 `*-test-*`（否则测试文件总是最新，会被当成真机日志）。

**「没有 sink」要分两种情况**（`langbar.rs::update`）：构造期（`AddItem` 之前）
本来就没有 sink，那时刷图标是正常时序；挂上之后又被管理器摘掉才是真故障
（通知全部石沉大海）。两者的 `sink` 都是 `None`，靠 `advised` 标志分开——
未挂过降为 `debug`，挂过再丢保持 `error`。不让构造期报 `E` 的理由很实际：
一堆永远可以忽略的错误，会把 `error` 这个词贬值，真出事时反而没人看。

**连接日志里的 pid 从管道句柄问**：`GetNamedPipeServerProcessId(pipe)` 直接给出对端
server 的 pid，不必给 `Frame::Welcome` 加字段——加字段要升 `PROTO_VERSION`，
代价是机器上所有旧 DLL 一起失效。日志里写「pid 未知」尤其有害：上一行刚是
`已拉起 server（pid 4004）`，下一行「pid 未知」会让读者以为连的是另一个 server。

**读日志**：

```powershell
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 log        # 最新一天尾部 40 行
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 log size   # 每个文件多大
powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 log clean  # 删超 7 天 + 旧格式单文件
```

级别开关仍走 `tools\tsf.ps1 debug on|off|status`（标志文件 `%LOCALAPPDATA%\Heinrich\debug.on`）
或环境变量 `HEINRICH_TSF_LOG` / `HEINRICH_SERVER_LOG`。**排障完记得 `debug off`**——
`debug.on` 忘了关是「日志为什么这么大」的头号原因（这次就是 10-21 开的，一直开着）。

单测在 `crates/heinrich-log/src/lib.rs` 的 `tests`：级别排序与解析、`Spec` 能被
`const` 声明为 `static`、当天路径的形状（`<stem>-*.log`）。

### 4.21 设置窗口的三页（M4b / D78）

三页都是**主窗口的子控件，靠显隐切换**（页签是自绘的一段文字，本来就没有「页」这个对象）。
`App.pages: [Vec<HWND>; 3]` 是唯一真相，切页只 `ShowWindow(SW_SHOW/SW_HIDE)`——
重建控件会让编辑中的内容、列表滚动位置全丢。

| 页 | 控件 | 数据去向 |
|---|---|---|
| 通用（默认） | `hint.mode` 三段选择器 + `learning_gain` / `half_life_days` 两个 Edit + `应用` / `恢复默认` + 只读的路径与 `config_version` | `%APPDATA%\Heinrich\config.toml` |
| 词组 | M4a 原样 | `words.tsv` |
| 学习数据 | 搜索 Edit + `搜` + ListView（词 / 上屏次数 / 上次使用 / 学习分 / 档位）+ `清除选中词的学习分` | `%LOCALAPPDATA%\Heinrich\learn.db`（只读 + 清零） |

**写配置只用 `heinrich-scheme::edit_config_file`（定向改写）**，不整份重写：那个文件是给人
手写的，整份重写会冲掉注释、打乱顺序、把陌生键挪位置——改一次「码提示」就毁一次笔记，
以后没人敢用界面。实现是一个小的行级编辑器（`collect` 建「行号 → 段 + 键」索引，
`edit_one` 在原行原地换值 / 删行 / 缺位时插到同段的最后一个键之后 / 段不存在才新建段头）。
6 条单测钉住：保留注释、段内写法不搬家、缺失补位不产生重复段头、删行即默认、重复键收敛、
空文件也能写。

**「恢复默认」= 删掉这三行**，不是写回当前默认数值。写死数值等于把会变的默认值钉在用户
文件里：将来调默认值，老用户永远停在旧值上，而且文件里看不出「这是默认」。

**数字框在界面层就校验**（`general::parse_number`，区间取 `RankingConfig::sanitized` 的
实际范围 7–365 / 0–10），非法值把「应用」灰掉并把原因写在该行右边。理由：写一个会被
`sanitized` 静默夹掉的数，用户只会看到「改了没生效」。

**学习页的「清除」是 `UPDATE usage SET n = 0, last_used_at = 0`，不是删行**：`usage` 同一
行里还存着 `tier`（0 学习 / 1 置顶 / 2 抑制），整行删掉会顺手把用户固顶的词降级——那是
另一个语义。清零后 server 下次查询就直接读到（它每次查询读库、不缓存），DLL 侧还有最坏
3 秒的候选缓存（D67），所以位次最多 3 秒回落。

**学习分复用 `heinrich-core::learning_score`**，g / H 从 `config.toml` 现场读——界面上显示
的分数与 server 排的序是同一个公式，改完 g 再回这页，分数立刻变。

**自测（`--selftest-ui`）扩到三页**：逐页 `check_z_order`（Tab 在 z 序底 + 命中测试），
通用页走一遍「非法值 → 按钮灰 → 合法 → 点应用 → 文件里有值且注释还在 → 点恢复默认 → 行没了」，
学习页断言行数 / 排序 / 档位 / 搜索 / 「选中才亮清除」。**注意**：隐藏页的控件做命中测试必然
失败（`RealChildWindowFromPoint` 不认隐藏窗口），所以要 `switch_page` 切过去再查。

**截图验收用 `--page 0|1|2`**：`PrintWindow` 只截当前可见的那一页，而三页是靠显隐切换的
子控件集，没有「单独截某一页」的办法。加了这个开关，`win-ui-selfscreenshot` 那套
（起进程 → 截图 → 看 → 改）就能逐页跑。

### 4.22 设置窗口的视觉层、自绘边界与重绘纪律（M4b-UI / D79、D80、D81）

**颜色与字体只有一个入口**：`src/theme.rs`。

| 项 | 内容 |
|---|---|
| `rgb(r, g, b)` | 全仓唯一的 `COLORREF` 构造入口。GDI 是 `0x00BBGGRR`，散写会把红蓝写反——反了不难看，只是「用户说强调色发绿」这种查半天的怪事 |
| `Colors::light(accent)` | 页面底 `#F3F3F3`、卡片白、卡片描边 `#E5E5E5`、分隔线 `#EDEDED`、正文 `#1A1A1A` / 次要 `#5D5D5D` / 弱 `#8A8A8A`、输入框描边 `#D6D6D6`、错误 `#C42B1C`、三段槽 `#ECECEC`，以及强调色的 hover / pressed 派生（`mix(accent, white, 36)` / `mix(accent, black, 36)`） |
| `system_accent()` | 读 `HKCU\SOFTWARE\Microsoft\Windows\DWM\ColorizationColor`（`0xAARRGGBB`，掩掉 alpha），失败回退 `#0067C0` |
| `Fonts::new(dpi)` | 标题 20/600、分区 13/600、正文 14/400、说明 12/400，全部 YaHei UI；`Drop` 里删句柄 |

**自绘只做壳，编辑框与列表仍是原生**：

| 自绘 | 原生（只套圆角 region + 画外部描边） |
|---|---|
| 页签文字 + 3px 指示条、卡片、行分隔线、按钮（`BS_OWNERDRAW` + `WM_DRAWITEM`）、三段选择器、输入框描边 | `Edit`（中文输入依赖系统 IME 组合窗）、`SysListView32`（列宽拖动、滚动、键盘导航） |

**几个必须记住的坑**：

- **页签不是控件**：`SysTabControl32` 一拿焦点就浮到 z 序顶（F44 的原病根），所以页签只是
  `WM_PAINT` 里的一段文字 + 命中矩形数组，hover 靠 `TrackMouseEvent(TME_LEAVE)`。
- **高 DPI 下描边要过 `s()`**：系统 DPI 2.0 时 1 物理像素 = 0.5 逻辑像素，`CreatePen(PS_SOLID, 1, …)`
  画出来的卡片框淡得像没有；统一用 `round_rect_w(..., self.s(1))`。
- **子窗口的方角会啃掉父窗口的圆角**：Edit / ListView 是不透明子窗口，父窗口在角上画的是弧，
  直角白块一盖就断。给它们 `SetWindowRgn(CreateRoundRectRgn(0, 0, w + 1, h + 1, …))`，
  并在布局里让列表内缩 `s(8)`——列头是直角、带自己的背景，缩 1 会把卡片圆角啃掉四角。
- **无边框 Edit 是白底**：白底压在白卡上等于隐形。描边必须画在父窗口上（`draw_words` 的
  `panel_boxes`），Edit 内缩 `s(1)` 让开那条线，焦点时描边转强调色。
- **`Ghost` 按钮也要擦底**：不填充就留着上一帧的内容（或控件默认灰底），看起来像「文字背后
  贴了块灰补丁」。`fill_rect(rectItem, colors.card)` 一句的事。
- **禁用按钮要留轮廓**：底色 `#F0F0F0` 与页面底 `#F3F3F3` 只差 3 级，不给描边时整排按钮凭空
  消失，用户看不出「这里本来有按钮、只是不能点」。

**文字摆位（D80，v1.56）——三条规矩，全是为「看起来歪」服务的**：

| 规矩 | 为什么 |
|---|---|
| 居中/右对齐前先 `text_width()`（`GetTextExtentPoint32W`）量宽，**左端点自己算**，不传 `DT_CENTER`/`DT_RIGHT` | `DT_END_ELLIPSIS` 会无条件在右侧预留省略号宽度（文字没超宽也预留），居中的文字因此整体左偏 7–8px。这个量没有文档、随字体变，只能自己算 |
| 页签矩形 = 实测文字宽 + 左右各 4dp，文字居中画、下划线**就是**这个矩形 | 「矩形 24dp 余量 + 文字靠左画」等于下划线比文字宽一倍还偏右；4dp 余量一半给 hover 底、一半喂给 GDI「笔画超出」的判定（刚好等宽时最后一个字会变「…」） |
| 第一个页签整体前移 4dp | 文字居中意味着左右各 4dp 空白；不前移，页签文字就比下面页标题右 4dp——两个「通用」一上一下，扫一眼就看出来 |

- **原生单行 Edit 不做垂直居中**：控件比一行字高时，文字贴上半边（32dp 框里数字偏高 7dp，
  量出来 13px）。治法是把 Edit 高度收到 `s(22)`、在框里居中、再下移 `s(3)` 补字体上沿——
  框的描边本来就画在父窗口上，控件不用铺满。
- **列表对齐跟着内容走**（`LVCF_FMT`）：数字列右对齐、短标记列居中、文字列左对齐。列宽用
  `fit_columns` 按权重分配；只让「最后一列吃掉剩余」的话，窗口一拉宽，`提示`/`档位` 这种
  短内容列会被撑成一块白板。
- **按钮宽度按实测字宽算**（`btn_w` = `text_width` + 32dp）。写死宽度时改一次文案就贴边
  （「打开文件位置」104 宽装六个 14dp 汉字，左右只剩 4dp，跟隔壁「撤销删除」的 10dp 一比就露馅）。
  内衬给 32 而不是 Win11 名义上的 24：`GetTextExtentPoint32W` 量的是字身，不含 ClearType
  右侧的光栅余量，实测每边会比标称少 3–4dp。
- **列表列宽铺满客户区，别再留 2dp。** 留余量会在表头右侧留一块白 + 一条竖直分隔线，
  看着像多出一列。要防水平滚动条就**按需**预留垂直条宽度（`SM_CXVSCROLL`，先用
  `LVM_GETITEMRECT` 判「行数 × 行高 > 客户区高」），最后 `ShowScrollBar(SB_HORZ, false)`
  把可能被挤出来的横条收掉。

**重绘与切页（D81，v1.57）**：

- **`paint()` 必须自己铺底，别指望 `WM_ERASEBKGND`。** `repaint()` 走的是
  `InvalidateRect(..., false)`（不请求擦除），`ShowWindow(SW_HIDE)` 露出来的区域同样不走擦除。
  指望擦除消息的结果是：切页时只画了新页那几张卡片，旧页的页标题、卡片描边、列表、
  页签下划线**全留在屏上**——用户看到的就是「页签只多了一条下划线、内容没换」和
  「切几次 tab 内容叠在一起」。现在 `paint()` 第一件事是 `FillRect(GetClientRect, brush_bg)`。
- **主窗口要带 `WS_CLIPCHILDREN`。** 这是「自绘父窗口 + 原生子控件」的必配：不设时整窗铺底
  会把 Edit / 列表的白底一起刷成页面底色，控件看着「消失」，鼠标划过才回来。
- **切页只动可见性。** `pages: [Vec<HWND>; 3]` 是唯一真相，`ShowWindow` 显隐 + 末尾一次整窗
  `InvalidateRect`；**不重建控件**（重建会丢编辑中的内容、列表滚动位置）。
- **验收必须真点。** 静态截单页永远看不出残留。用 `WM_LBUTTONDOWN`（或 DPI 感知的真鼠标点击）
  连切几次再截；判据是「切回第一页的截图与初始截图逐字节相同」。

**列表选中态（v1.62 / D86）**：三个坑叠在一起，缺一条就是「点了看不见」。

- `LVM_SETTEXTBKCOLOR` 会把每行**文字背板**钉成一个颜色（当初为了配卡片底加的），系统选中高亮
  整片被它盖掉 —— 删掉。文字颜色同理，别用 `LVM_SETTEXTCOLOR`。
- `LVS_EX_FULLROWSELECT` 缺了**只有第一列能点**；用户点第二列，消息到了、选中项却没变。
- `LVS_SHOWSELALWAYS` 是**普通**样式（`GWL_STYLE`），**不在** `SetExtendedListViewStyle` 的扩展位里
  —— 用 `GetWindowLongPtrW/SetWindowLongPtrW` 补。缺它时焦点一进搜索框，选中行就淡到几乎看不见，
  这是「我都不知道点选了什么」的主因。

三条修完，开着视觉主题时选中仍可能看不出来（`clrText*` 被主题直接忽略）。两条路：`SetWindowTheme(h, "", "")`
关主题让 `clrTextBk` 生效 —— 列头与滚动条会退回 Win95 凸起灰块（截图可见，明显倒退）；或者
`NM_CUSTOMDRAW` 返回 `CDRF_SKIPDEFAULT` 自己画整行。选后者，但要记住：

- 通知码是 `NM_FIRST - 12` = **`0xFFFFFFF4`**；写成 `LVN_FIRST - 12`（`0xFFFFFF90`）差 88，
  一条通知都收不到（表现为「代码在跑、画面没变」，极难查）。
- `CDDS_PREPAINT` 返回 `CDRF_NOTIFYITEMDRAW`，之后才是逐行的 `CDDS_ITEMPREPAINT`。
- 判断「这行选中没选中」**别读 `CDIS_SELECTED`**（0x0001）：这个阶段实测所有行都带这一位，
  照它涂会把整片行都染成选中色。改用 `LVM_GETNEXTITEM(-1, LVNI_SELECTED)` 问列表要。
- 返回 `CDRF_SKIPDEFAULT` 之后，列宽 / 对齐 / 省略全归自己：`LVM_GETCOLUMNWIDTH` 取宽、
  `LVM_GETITEMTEXTW` 取文字（缓冲按**字符**算，别按字节）、`SelectObject(WM_GETFONT)` 之后逐格
  `DrawTextW`。列头是系统画的，所以列的 `LVCFMT` 得统一改成左对齐，否则自绘行与列头错开半个身位。


**页内控件的归属（v1.63 / D87）**：`show_page` 只对 `pages[n]` 里的句柄做 `SW_HIDE`——**没登记的控件就一直在屏上**。
加控件时三件事必须一起做：创建 → 加进 `pages[n]` → 在对应的 `layout_*` 里摆位。漏第二件的症状不是
「少了个控件」，而是「每一页都悬着它」（`b_emoji_browse` 漏过一次，通用 / 词组 / 学习数据页的中间都挂着
一个「浏览 emoji…」）。

`--selftest-ui` 的 `check_page_scope` 钉住这一条：切到某页后枚举全部子窗口，**可见但不属于本页**即失败，
白名单只有状态栏（它刻意全页共用、切页不变）。`check_z_order` 抓不到这类问题——它只按 id 找本页那几件、
量它们没被挡住，漏登记的控件待在别处照样全过。

**comctl32 v6 清单**：`apps/windows/settings/app.manifest` + `build.rs`，用链接器的
`/MANIFEST:EMBED` + `/MANIFESTINPUT:<绝对路径>` + `/MANIFESTUAC:NO`，**不引 `embed-resource` /
`winres`**（那类 crate 要再拉一个 `rc.exe`，而本项目要求 `cargo build` 之外零工具）。
`/MANIFESTUAC:NO` 不能省：否则链接器会自己再生成一段 trustInfo，与清单里的
`requestedExecutionLevel` 撞成两份，直接报错。没有清单时 ListView 的列头是灰白凸起块、
滚动条是带箭头的 Win95 灰条，`SetWindowTheme` 也救不了——视觉样式没激活时它全是空操作。


### 4.23 emoji 候选（M4c / D83，协议 v8）

**一句话架构**：emoji 是**候选**，不是装饰。`heinrich-core` 的 `Candidate` 与排序一个字段都没动，
`Host::query` 把词候选建好之后，才在**有绑定的那条后面插一条** `kind = Emoji` 的条目；它的
`word` 就是那枚 emoji，选中即上屏。裁决在 server、DLL 只画——与 `code`（D51）同一条纪律。

> v1.58 第一版做的是「词尾装饰」（`Entry.emoji` 挂在词上、上屏只有词）。真机上跑起来才发现
> 那不是要的东西（原话见 D83 的理由），v1.59 改成独立候选，协议升到 v8 并把装饰字段删掉。

| 环节 | 文件 | 说明 |
|---|---|---|
| 渲染探针 | `apps/windows/ui/examples/emoji_probe.rs` | 判「D2D 1.0 能不能出彩色」。结论：能，**不需要 D2D 1.1 DeviceContext** |
| 覆盖探针 | `apps/windows/ui/examples/emoji_coverage.rs` | 判「本机 `Segoe UI Emoji` 有哪几个字形」。判据是 DWrite，不是 GDI（见下） |
| 生成器 | `crates/heinrich-gen/src/emoji.rs`（`--kind emoji`） | CLDR `zh.xml` → `data/wubi86/emoji.toml` + `out/m4c-emoji-report.md` |
| 映射层 | `crates/heinrich-scheme/src/emoji.rs` | `EmojiLayer` 解析 / `EmojiMap` 两层叠加 / `retain_supported` / `is_emoji` / `valid_emoji` |
| 字体层 | `crates/heinrich-font` | DWrite `IDWriteFont::HasCharacter` → `HashSet<char>`（普通值，可跨线程） |
| 合成 | `crates/heinrich-server/src/host.rs`：`EmojiState` + `insert_emoji` | 1 秒 mtime 热重载（与 `words.tsv` 同一套）+ 总开关 + 插入时顺移折叠分界 |
| 协议 | `crates/heinrich-proto`（v8） | `Entry.kind`（`0` 词 / `1` emoji 候选），**在 `hint_at` 之后**，encode/decode 必须同序 |
| 布局 | `crates/heinrich-ui/src/layout.rs` | 没有特殊待遇：`CandidateText::as_emoji()` → `ItemBox.is_emoji` |
| 绘制 | `apps/windows/ui/src/{text,render}.rs` | `measure_emoji`（实测前进宽度）+ `ENABLE_COLOR_FONT` |
| 透传 | `apps/windows/tsf/src/{ipc,cand,text_service}.rs` | `Reply.kinds` → `Cands.is_emoji` → `Candidates.is_emoji`；`resolve()` 对 emoji 跳过 `ipc::record` |
| CLI | `crates/heinrich-cli`（`emoji check/list/reset`） | 见下 |

**为什么不用 D2D 1.1**：探针拿同一个 `ID2D1DCRenderTarget` 画两行「👍🎉😊 彩色?」，一行带
`D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT`、一行不带，数「不同的颜色个数」：**105 vs 13**。
升 1.1 要换 `ID2D1DeviceContext` + 全链路改 `D2D1_BITMAP`，收益为零。判据不要用「看起来像不像彩色」，
用颜色计数——灰度抗锯齿也会产生几十种灰。

**本机字体门槛（D83 新增）**：`emoji.toml` 按 CLDR 全量生成，里面混着 Emoji 13/14/15/16 才有的码位，
而 Win10 的 `Segoe UI Emoji` 只到 Emoji 12。装不出来的 emoji 在候选窗和文档里都是**方块**（实测
`您好 → 🫂` = U+1FAC2）。处理方式：

- 装载期用 `heinrich_font::EmojiSupport::detect(码位集合)` 问一次本机字体，`EmojiMap::retain_supported`
  把装不出来的条目**连键一起丢掉**（只丢显示的话，用户选中它上屏仍是方块）；
- 探测一次就丢掉 COM 对象，只留 `HashSet<char>`——`IDWriteFont` 不是 `Send`，而 server 是按连接开线程的；
- 判据**只能是 DWrite**：GDI 的 `GetGlyphIndicesW` 按 UTF-16 **码元**查，非 BMP 字符（也就是全部彩色
  emoji）一律报缺，实测 2395 条只剩 228 条——假阴性，会让人误以为字体缺得离谱；
- 探测不到字体族时一律放行（`supports` 返回 `true`）：探测失败不该顺手把功能关掉。

**数据管线（CLDR `common/annotations/zh.xml`，292 KB，Unicode License）**：

```
<annotation cp="👍">赞 | 顶 | 大拇指</annotation>      ← 取这个
<annotation cp="👍" type="tts">竖大拇指</annotation>   ← 跳过（tts = 朗读名）
```

| 规则 | 为什么 |
|---|---|
| 只收**单码位** emoji（跳过 ZWJ / 旗帜 / 肤色修饰） | 多码位序列在候选行里撑两三个字宽 |
| 关键词 ≥2 字、全汉字、且**在拼音词库里** | 「赞 / 顶」这种单字关键词会把「赞」绑上 👍，而单字本来就不出候选 |
| 同一个词命中多个 emoji 时取 `(词库名次, 码位)` 最小 | 结果与 `HashMap` 遍历顺序无关 ⇒ 两次生成字节级相同 |
| 头部写 `entry_count` + 正文 `sha256`（只覆盖 `词<TAB>emoji<LF>`） | 装载期 `verify()` 自校验；手改过就报「与声明不符」，不静默 |

产物 **2395 条**（生成两次 `sha256` 完全相同），本机字形筛后 **2018 条**生效。重新生成：

```powershell
powershell -ExecutionPolicy Bypass -File tools\fetch-data.ps1 -Only cldr   # 缺 zh.xml 时
powershell -ExecutionPolicy Bypass -File tools\gen.ps1 -Kind emoji
```

**两层映射的语义（`EmojiMap::build(&[&builtin, &user])`）**：

| 情形 | 结果 |
|---|---|
| 用户层同 key（`word` 或 `code`）再次出现 | 覆盖内置 |
| 用户层 `enabled = false` | **删掉**同 key —— 不是「不覆盖」。跳过的话停用等于无声失败 |
| 同一个词同时有按词与按码两条 | 按词优先（词比码具体） |
| 文件内同一个 key 写两次 | 后写覆盖先写（改绑不必删旧行） |
| `word` 只有 1 个字 | 解析期就丢掉并给带行号的警告；`lookup` 里另有一道「词长 < 2 直接返回 None」的闸 |

**查表用的码与码提示无关**：拼音候选的 `code` 是拼音串，所以查表用装载期反查的 `wubi`；
五笔候选用 `code`。因此 `hint.mode = off` 时按码绑定的 emoji 照样生效（有单测钉死）。
实现上要注意顺序：`wubi` / `code` 在 `match` 里会被 move 进 `Entry`，`emoji_codes` 必须在
`match` **之前** clone 出来。

**合成（`insert_emoji`，纯函数，单测全靠它）**：

```
fn insert_emoji(words, codes, limit, primary_words, lookup) -> (Vec<Entry>, usize)
```

- 逐个词 push，紧接着 push 它的 emoji 候选（`word` = 那枚 emoji，`label` / `code` 空、`hint_at` 为 0）；
- **插入的条目也算一条 `limit`**：不封顶的话一次按键吐出两倍候选，窗口宽度先崩；
- **返回新的折叠分界**（`primary_len`）：它是全局序号，插一条后面全错一位。写错的形态特别隐蔽
  ——只是最后一条主段候选被画淡一档（预览器里截到过），看截图根本反应不过来；
- emoji 跟在主段词后面时**也算主段**，不能被画淡。

**布局与绘制**：

- 这一层**没有 emoji 专用几何**：`CandidateText::as_emoji()` 只是给 `ItemBox.is_emoji` 传个标记，
  宽度仍然由量测层给。v1.58 那套「emoji 单开一列 + 超宽先丢 emoji」已经删干净——
  独立候选之后，它自己就是一条宽度等于字形的普通条目。
- 量宽用 `measure_emoji`（走主字号 format 的 `GetMetrics`），**不能按「字号 × 1.0」估**：
  emoji 落到彩色字体后前进宽度与正文字体不同，估出来的值和实际差 1–2dp，一列五个候选就会看到
  间距忽宽忽窄。
- 绘制：`text_in_with(target, text, format, rect, color, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT)`。
  普通词仍走 `text_in`（不带选项），两条分支只差一个选项位。

**上屏**：走与词完全相同的 `SetText` 路径（`resolve()` 取当前高亮条目）。唯一的分支是
`current_is_emoji()` 为真时**跳过 `ipc::record`**——`👋` 不是「`wqvb` 的一个词」，记进学习库会把
词频与候选序一起带偏，下次这个码可能先弹 emoji。

**开关与热更新**：`config.toml` 里 `emoji.enabled = false`（也认 `on/off`、`1/0`；写坏按「开」处理并警告）。
`HintState` 每秒随 config 一起读，`EmojiState.refresh()` 按 mtime 重装用户层，整份重建映射
（不做原地改）。关掉时**一条 emoji 候选都不插**，布局与 v6 逐字段一致（单测钉住）。
日志里能看到 `emoji：N 条生效（内置 X + 用户层 Y）` 与 `emoji：M 条本机字体缺字形，已跳过`。

**CLI**：

| 命令 | 行为 |
|---|---|
| `emoji check [--file <用户层>]` | 内置层可读/规模/哈希/条目数/坏行 + 一行「本机字体：2018 / 2395 条可显示」+ 合并后「按词绑定是否都在词库」「按码绑定是否都在码表」「重复键」。**默认不读 `%APPDATA%`**（回归要机器无关），要查自己的绑定就显式 `--file` |
| `emoji list [--file] [--limit n]` | 反过来：默认读用户层（这就是「我现在生效的是什么」）。`--limit 0` 看全部 |
| `emoji reset [--file]` | 备份成 `emoji.user.toml.bak-<本地时间戳>`，再写回带注释样例的空文件（删掉的话用户下次还得自己摸格式）。**文件不存在时先建父目录再写样例**，所以它同时是「生成一份用户层」的入口（D84）——用户层是可选覆盖层，装完不预生成 |

**怎么验**：

```powershell
cargo run --release -p heinrich-cli -- emoji check        # 里有「本机字体：2018 / 2395」
cargo run --release -p heinrich-cli -- emoji list --limit 10
cargo run --release -p heinrich-server -- --selftest      # 里有「emoji 是自己的候选」
cargo run --release -p heinrich-cand-preview -- --emoji --hint --selftest --save-bmp out\cand.bmp
cargo run --release -p heinrich-win-ui --example emoji_coverage -- data/wubi86/emoji.toml
```

`cand-preview --save-bmp` 落的 BMP 是 **top-down（高度字段为负）+ 24 位 BGR**：转 PNG 时必须
`h = abs(h)`、按 `off + y*rowsz` 正序读、并且 `BGR → RGB` 换序。按 bottom-up 假设翻一次，
会得到一张上下颠倒、左右也像镜像的图——那不是渲染错，是转换脚本错。

**踩过的坑**：

- `usize::to_le_bytes()` 是 **8 字节**。BMP 头字段是 `u32`，直接写会整体错位 4 字节（探针第一版
  写出来的图打不开）。写完 BMP 用 `xxd -l 64` 看一眼偏移 0x0A（数据起点）= 54、0x12（宽）、0x16（高）。
- **GDI 判字形是假阴性**：`GetGlyphIndicesW` 按 UTF-16 码元查，非 BMP 全报缺。用它筛 CLDR 表会
  得出「只剩 228 条可用」的错误结论。判据用 `IDWriteFont::HasCharacter`。
- **`windows` 0.62 的原始绑定**：`GetSystemFontCollection(&mut Option<IDWriteFontCollection>, false)`、
  `FindFamilyName(name, &mut index, &mut BOOL)` 都是原始指针风格，别按返回值直接接。
  `HasCharacter` 在 `IDWriteFont` 上（`Result<BOOL>`），不在 `IDWriteFontFace` 上。
- **插入 emoji 后折叠分界**：`primary_len` 是全局序号，插入会整体后移（见上）。
- **单字条目（D85 放行后）**：解析期与查表期的词长 gate 都已撤掉——内置表仍只挂词组
  （生成器只收 ≥2 字关键词），用户层显式给单字绑就照办（含按码绑定扫到同码单字）。
  代价（该字所在候选行变宽）由写条目的人承担：设置窗口在「绑定」列标 `按词·单字` 并
  在信息行提示，`emoji check` 打一行提醒（不算错）。
  ⇒ 用户报「`你好` 有 emoji、单个 `好` 没有」时先讲清这条默认规则，再问他要不要绑。

### 4.24 emoji 选择器：分类网格弹窗（M4d 返工 / D86，v1.62）

一句话：改绑时不再「在一千多条的下拉里翻」，而是弹一个**照系统 emoji 面板做的网格**——
10 个分类页签 + 11×7 格 + 点一枚即回填。

**为什么是弹窗，不是下拉**：

- 功能性：原生 ComboBox 创建时想追加 `WS_VSCROLL`（**0x0020**），而 ComboBox 样式表里这一位是
  `CBS_OWNERDRAWVARIABLE` —— 控件被置成自绘模式，条目明明在、但没人画，下拉里一片空白
  （用户原话「下拉框也不知道什么内容，没有显示」）。就算修好，也只是「有一千多行的下拉」。
- 认知：用户认的是**位置记忆**（表情在第一页签左上角），不是名字；怎么排序都得翻。

**分层**：`emoji_picker.rs` 切成两块，边界是「要不要窗口」。

| 层 | 内容 | 怎么验 |
|---|---|---|
| 纯函数 | `cat_of(cp)`（码位 → 分类）、`Catalog::new`（跳空分类、按码位排序去重）、`Geometry::new(w, h, scale)` + `cell_rect` / `index_at` / `tab_at` / `max_scroll` / `hit_close` | 8 条单测，不开窗口；「点第 (2,1) 格拿到哪枚」这类问题不需要人点鼠标 |
| 窗口 | 窗口类注册、D2D 画（圆角悬停块 / 页签下划线 / 滚动指示条）、滚轮与键盘、模态循环 | `--picker` 起窗 + 截图目视 |

**分类靠码位区间，不读 CLDR 的 `group`**：group 在生成器那一步就丢了，而码位区间本身足够稳。
**区间顺序即优先级**（先写的先命中），所以特例必须放前面——`0x1F63A..=0x1F63F`（猫脸）会被前面的
`0x1F600..` 一口气吃掉，写反了 `unreachable_pattern` 会直接报出来；末尾永远留一个「其他」兜底，
分类只增不减、不会因为漏一个区间就让某些 emoji 消失。

**几何一律物理像素**：`Geometry::new(w, h, scale)` 里每个数字都过 `scale`。逻辑单位混进去，
DPI 2.0 下格子会一大一小（本节这套数：`cell = 46`、`tab_y = 44`、`title_h = 88`、
`footer_h = 30`、`inset = 14`、11 列 × 7 行）。

**字号也走同一个 `scale`**（v1.63 修正）：`Geometry` 除了矩形还给出四个字号——标题 16dp、
页签 14dp、提示 13dp、emoji = `cell × 0.68`；圆角、页签下划线、滚动条、外框描边也一律过 `sp()`。
理由在渲染目标：D2D 的 **DC 渲染目标建于 `dpiX = dpiY = 96`**，DWrite 的字号 1 DIP = 1 物理像素。
只缩放几何、不缩放字号，结果就是「92px 的格子装 26px 的字」（真机反馈原话「文字也太小了吧」）。
emoji 取**格子边长的比例**而不是写死字号：改一次格子尺寸或换一档 DPI，两者不会重新脱节。
单测 `字号随dpi放大且填满格子` 断言「标题字号随 scale 翻倍」+「`font_emoji / cell` 落在 0.6–0.75」。

**彩色只能走 D2D**：`ID2D1DCRenderTarget::BindDC` → `Clear` → 逐枚 `DrawTextW`，format 上开
`D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT`。GDI 与 GDI+ 画 `Segoe UI Emoji` 都是**单色轮廓**
（候选窗在 D55 已踩过一次，同一个结论）。命名空间上 `D2D1_ROUNDED_RECT` 在 `Direct2D` 下，
不在 `Direct2D::Common` 下。

**模态靠私有消息泵**：`PeekMessageW` + `WaitMessage` 循环，关窗只置 `done` 标志、**绝不
`PostQuitMessage`** —— 那会结束设置窗口自己的循环，等于连带把主窗关掉。同时
`EnableWindow(parent, false)` 挡住背后的点击，返回前再打开。

**打开时机要延后**：`--picker` 不是建完窗就弹，而是 `SetTimer` 250ms 后弹。窗口刚
`CreateWindowExW` 时还没画过第一帧，此时算居中用到的窗口矩形是错的，会弹在左上角。

**命中测试两个易错点**：`index_at` 要**先减 `scroll` 再除 `cell`**（滚动偏移是像素，直接除会把
滚过的位置全算成第 0 行）；点到空白处返回 `None`，不要「就近吸附」到最后一格——那会让用户在
留白处点一下就莫名改绑。

**验收别在后台 shell 里起 GUI**：工具调用一结束进程就被回收，`tasklist` 里根本查不到。
一个进程里做完「起 → 等窗口 → 截图 → 真鼠标点击 → 再截图 → kill」，或者用
`win-ui-selfscreenshot` 那套（给程序加一个「直接开某个窗口」的开关 + `PrintWindow` 只截那一窗）。

> v1.65 起这一页的主操作从「给选中的行换 emoji」改成「把某一枚 emoji 绑到某个词/字」，
> 面板变成两个框并排——见 §4.26（D90）。下面这套「红字写面板行 1 / 绘制时现算」的
> 机制不变，只是多了一条「目标看不懂」的提示来源。

### 4.25 emoji 页改绑反馈与列表彩色自绘（D89，v1.64）

**改绑「点了没反应」的根因是校验顺序错了**：原实现先 `probe_one` 字形探测再校验输入——
框里填的是汉字「好」时，用户得到的是「这枚 emoji 本机字体画不出来」，答非所问，且提示
只写在窗口底部一行小灰字里，等于没有反馈。修正为两层：

- **输入校验前置**：`emoji_page::check_input(text) -> Result<String, String>`，空 / 不是 emoji /
  多枚三种各给一句人话（「「好」不是 emoji：一条绑定只挂一枚 emoji（如 👍），点「浏览 emoji…」选」）。
  校验不通过就不碰磁盘、不给 `probe_one`。
- **错误写在编辑面板行 1**（红色），不再走窗口底部的 status 行——那里离操作区太远，用户看不见。

**校验结论在绘制时现算，不在 `EN_CHANGE` 里存**。这条坑很隐蔽：`SetWindowText` 触发的
`EN_CHANGE` 到达时，控件文本可能还是**旧值**（消息排队与 `GetWindowText` 的时序），在处理器里
「读文本 → 算结论 → 存下来」会把过期结论一直挂着——自测「填回合法 emoji 后提示应当消失」
当场抓出来。现在 `EN_CHANGE` 只做三件事：更新按钮可用态、清掉保存时留下的 `emoji_msg`、
重绘；「现在该显示什么提示」由 paint 路径的 `emoji_panel_alert()` 现场对当前文本算。

**列表 emoji 列改 D2D 彩色**（`emoji_draw.rs` 的 `ColorEmoji`）。GDI 只能画 `Segoe UI Emoji`
的单色轮廓（坑 62），列表里单色、选择器里彩色，用户判定「匹配不上」。把选择器那套
`ID2D1DCRenderTarget` + `DrawText(ENABLE_COLOR_FONT)` 搬进 `NM_CUSTOMDRAW` 时又踩了三条：

1. **`BindDC(hdc, &rect)` 后，绘制坐标以绑定矩形左上角为原点**——不是 DC 原点。文字画在
   `(rect.left + …)` 会偏出一格。
2. **DC 渲染目标不做 alpha 混合**（`D2D1_ALPHA_MODE_IGNORE`），画 emoji 前必须先 `Clear(行底色)`，
   否则格子背景是黑/未定义色。
3. **`IDWriteTextFormat` 的字号创建时固定**（`SetFontSize` 只在 `IDWriteTextLayout` 上），字号变了
   要重建 format——按字号缓存（差 < 0.5 复用），列表行高固定时几乎不重建。

D2D 初始化失败时回落 GDI 单色轮廓（列表功能不因 D2D 挂掉）；emoji 表现在**全行**自绘
（停用行整行淡一档），不再只画选中行。

**验证约束**：桌面锁定时动态状态（红字提示、选中行）拍不到截图——`PrintWindow` 返回上一次
合成好的画面，屏幕 BitBlt 抓到的是锁屏壁纸。静态画面（列表彩色 emoji）能拍。动态断言用
`--selftest-ui` 走真消息链：`SetWindowText` 填「好」→ 真实 `WM_COMMAND(EN_CHANGE)` → 断言
`emoji_panel_alert()` 含「不是 emoji」→ 点改绑断言文件不变 → 填回「👍」→ 断言提示消失。

### 4.26 emoji 绑定：主键是「目标词/码」，不是选中的行（D90，v1.65）

**为什么改**：这一页原来是「先在列表点一个词 → 再给它换 emoji」。列表是**按词合并
视图**——没绑过 emoji 的词（尤其是单字）在里面根本没有一行，于是「给『好』挂 👍」
这条最自然的需求无路可走。用户的原话：「我不是要把词/码改绑不同的 emoji，而是要
将某个 emoji 改到不同的字和词上」。

**现在的形状**：编辑面板第二行两个输入框并排，读起来是一句话——把哪枚 emoji
绑到哪个词/字上：

```
[绑到: 好        ]  [emoji: 👍]  [浏览 emoji…]
[保存绑定] [停用] [恢复内置]              [打开文件位置]
```

- **`emoji_page::parse_target`**：全汉字 → `Key::Word`（≤8 字），全 a–y →
  `Key::Code`（≤4 位），掺了别的给人话。不做「按词 / 按码」开关——两个输入域
  几乎不重叠，多一个开关只换来一次停顿。注意 `z` **不是**码字母（只做通配），
  必须判成「看不懂」而不是一个查不到的码。
- **upsert 而不是「改绑」**：`rebind(&Key, emoji)` 本来就是「先按 key 删再 push」，
  所以「新增」与「覆盖」共用一条路径；列表点一行只是把两个框预填（快捷路径）。
- **存在性校验**（`lex::Lexicons::has_word` / `has_code`，与 CLI `emoji check`
  同一判据）：词不在词库就永远不会有候选挂着它，绑了是死数据，红字拦下；单字
  照常通过（「好」在词库）。`--data` 缺失（窗口降级）时跳过校验，状态行标注。
- **报错落点**：`reject_emoji(msg, focus)` 干三件事——写 `emoji_msg`（面板行 1
  红字）、写底部状态行、把焦点送回**出错的那个框**。顺序固定：目标看得懂 →
  单枚 emoji → 本机画得出 → 目标打得出来。
- **列表过滤器**：「只看我绑的」= `!only_user || r.user`（用 `row.user` 而不是
  比状态字符串，将来加状态名不会漏）。开着时按钮文字变「看全部」，说明行补
  「当前显示 N 行」——不然用户会以为列表丢了东西。切换时要按 key 找回选中行，
  那一行可能已经被过滤掉，找不回就清空面板（`clear_emoji_selection`）。

**自测覆盖**（`check_emoji_page`）：过滤器 2 行 ↔ 3 行、按钮文字、红字提示往返、
「绑到」看不懂不写盘、点行双框回填、**不点列表直接新建**「好运 → 🍀」（落盘 +
列表 4 行 + 自动选中 + 同词覆盖不增行）。新建那条是这轮的核心——它证明「不必先
在列表里找到目标」这件事真的通了。

**真机验法**：`heinrich-settings.exe --page 2 --emoji <临时文件>`（`--emoji` 是
路径参数，不是「打开 emoji 页」的开关；页号 emoji = 2、学习数据 = 3），填「好」+
👍 点保存 → 列表出现「好 / 按词·单字 / 👍 / 新增」，且临时文件里出现
`word = "好"` + `emoji = "👍"`。用临时文件是为了不动 `%APPDATA%` 里的真用户层。

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
2. **候选集不能做前缀展开，`Host::query` 连「空码兜底」也不做（D45）。** `v` 前缀下有 3,459 个候选——排序与翻页都会失去意义。五笔是定长编码，「还没打完」不等于「空码」：`xxv` 无精确词条但 `xxvk` 有「比如」，在此回退会在用户准备按第 4 键时弹出一屏「幽灵 / 幽婉」（只是码前缀撞了）。core 仍保留 `LookupMode::PrefixFallback`（CLI 与单测用），server 侧一律丢弃——**这两处不一致是有意的**，动 `Host::query` 之前先读 D45。
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
15. **状态更新不能只挂在 `OnKeyDown` 上——先问后做的宿主问了说不要就不调它。** M1.1 第一版把「Shift 按下就置位」写在 `OnKeyDown` 里，结果 Notepad++ 里单击 Shift 完全切换不了中英：它只回调 `OnTestKeyDown` / `OnTestKeyUp`，置位那一半从不执行，松手时的判定被整段跳过。日志里表现为**一条模式切换都没有**，而逐键日志看着一切正常。
    > **前提要记准**：`OnKeyDown` 不是「永远不调」，而是「`OnTestKeyDown` 回答 FALSE 时不调」。M1.0 的日志里 Notepad++ 只有 `test-down` 没有 `down`，是因为那一版一个键都不吃。回答 TRUE 之后两条路都会到齐（先问后做）。
    **推论**：凡是「同一个物理动作要产生副作用」的逻辑，四条回调都是入口，不能假手 `OnKey*` 代劳。
    **并且**：宿主不一定把修饰键的 **key-up** 送进来（M1.0 日志里 Notepad++ 对 Alt 只留下了 `test-down`，没有 `test-up`）。所以判定不能依赖「收到 up」，改成**每次回调采样一次 `GetKeyState` 与上次比对**：看到「上次按着、这次没按」就补判，最坏只是晚一拍（那个按键本身）。实现见 `keys::ShiftTap`，它是纯逻辑、有单测。
16. **`[CmdletBinding()]` 脚本的位置参数按声明顺序分配。** 给 `tsf.ps1` 加 `debug on` 的子参数时，新加的 `[string]$Switch` 声明在 `$DistDir` 之后，`on` 就被绑到了 `$DistDir` 上，`$Switch` 一直是空串——命令静默走了 `default` 分支，看起来像功能没实现。多参数脚本一律显式写 `[Parameter(Position = n)]`。
17. **组合串有三处语义与直觉相反（M1.2 起生效）：**
    - **`StartComposition` 要的是一个空 range。** 先写文字再起组合串，组合串起点落在文字末尾，覆盖不到 preedit，之后 `SetText` 改的内容在屏幕上根本看不见——症状是「能上屏、但打字过程中什么都没有」。正确顺序：清选区 → 起组合串 → 往组合串的 range 里写。
    - **`EndComposition` 不删文字。** 它只把这段文字转正，「取消」如果不先把内容写成空串，那几个编码字母会永久留在文档里。
    - **写完必须 `SetSelection` 把光标收到 preedit 末尾**，否则光标停在组合串起点，看起来像「字往左边长」。
    另外组合串可能被**别人**终止（换焦点 / 文档关闭 / 别的 TIP 抢走），要实现的不是自己再 end 一次，而是 `ITfCompositionSink::OnCompositionTerminated` 里用给的 `ecwrite` 把残留删掉；删之前比对句柄身份，避免把自己刚提交的字也删了。
18. **`#[implement]` 生成的 `as_interface_ref()` 是泛型方法。** 直接 `self.as_interface_ref().cast::<T>()` 会报 `type annotations needed`，得写成 `ComObjectInterface::<IUnknown>::as_interface_ref(self)`；要一份能长期持有的引用再加 `.to_owned()`（它会 AddRef）。相关：`ITfContextComposition::StartComposition` 的 sink 参数是 `Param<ITfCompositionSink>`，`&Option<T>` **不满足**，要传 `Option<&T>`（`self.sink.as_ref()`）。
19. **`windows-core` 的 `Error::code()` 是 `HRESULT`，不是 Win32 错误码。** 它被 `HRESULT_FROM_WIN32` 包过（`0x8007_0000 | code`），拿它直接跟 `ERROR_FILE_NOT_FOUND`（2）比永远不相等——表现是「管道还不存在」这种正常路径被当成真错误刷进日志，或者反过来的「该重连的时候不重连」。取低 16 位（`code.0 as u32 & 0xFFFF`）再比。另外 0.62 没有 `Error::message()` 和 `HRESULT_FROM_WIN32`，要看文案就 `format!("{e}")`。
20. **重叠 I/O 超时后必须放弃整条连接，而且 `OVERLAPPED` 要放堆上。** 超时那一刻内核可能仍然引用着那块 `OVERLAPPED`（`CancelIoEx` 只是发起取消，不保证同步完成），栈上的对象随函数返回就被复用了，等于让内核往别人的栈帧里写。做法：`Box<OVERLAPPED>`，丢弃 `mem::forget`（每次超时泄漏 40 字节，换「不写坏内存」）。同理，超时后管道里可能还剩半个响应，继续复用会读到错位的帧——直接关句柄、重连，比「猜着读」可靠得多。
21. **常驻进程会锁住自己的 exe。** `heinrich-server.exe` 在跑时 `Copy-Item` 覆盖 `dist` 里的同名文件必然失败，表现与 DLL 被宿主映射一模一样（`os error 32`）。`build.ps1` 在复制前先结束它——它是我们自己的进程、没有用户数据，DLL 下一次按键会自动拉起新版。
22. **同一个键在不同数据配置下归属会反转，判断权必须给「持有配置的那一侧」。** 典型是 `z`：纯五笔里它是万能学习键（D42 要吃掉，否则宿主会在组合串中间插字符、把已打的编码当残留删掉），装了拼音词库后它是普通字母（`zhuang` / `zao` / `zhe` 都以它开头，吃掉等于整片 z 声母打不出来）。DLL **不可能**自己判断——`data/pinyin` 在 dist 里是可选的，只有 server 知道自己装没装。所以做成握手能力位（`feature::PINYIN`），DLL 读到才改行为，读不到就退回老一套。同一批的还有码长上限：`MAX_CODE_LEN = 4` 是五笔的定长语义，混输下必须放到 24（最长词条 20 字母），并且用**编译期断言**钉住下界。
    > 协议侧的对称要求：加字段必须同时升 `PROTO_VERSION`。这次 `Result` 在条目数前插了 `source` + `primary_len`，旧 DLL 若被放行会把 `source` 当成候选条数读出一堆乱码；升版本后它在握手阶段就被 `Bye` 挡掉。

23. **命名管道的 `CreateNamedPipeW` / `ConnectNamedPipe` 在 `Win32::System::Pipes`，而 `CreateFileW` 在 `Win32::Storage::FileSystem`。** 而且 `CreateFileW` / `CreateEventW` 因为形参里有 `SECURITY_ATTRIBUTES`，**必须**同时开 `Win32_Security` feature，否则整个函数会被 cfg 掉，报错是「no `CreateFileW` in `Win32::Storage::FileSystem`」——看起来像 API 不存在，其实只是 feature 没开。

24. **删掉一个「看起来没用」的判断之前，先想它防的是什么。** 改 D48 时把 `wubi_candidates`
    里的 `LookupMode::Exact` 判断当成冗余删了（注释还写着「D45 之后没有调用方」），结果
    `lookup` 的前缀兜底复活：`abc` 打出「abce 节能 / abci 藂 / abcu 菆」一整屏毫无关系的字。
    **单元测试全绿**——因为夹具里的 `abc` 前缀恰好没有词条；抓住它的是 server 自测里那条
    `abc → 无命中`。教训有两层：① 兜底分支的注释写「已无人调用」时，去搜**真正的调用链**
    而不是相信注释；② 断言「空」比断言「有哪些」更能抓住兜底泄漏，因为泄漏的特征就是
    「本不该有，却有了」。
25. **同名同义的两个 `code` 字段，值可能完全不同。** `Candidate.code` 是「这条候选**是怎么被
    查出来的**」——五笔侧是码，拼音侧是**拼音串**（`hao` 命中「好」时 `code == "hao"`）；
    而码提示要的是「这个词用五笔**怎么打**」（`vbg`）。M3.4a 第一版直接把 `code` 当 chip，
    自测立刻报 `「好」的码「hao」`。两个问题只能两个字段（`Candidate.wubi`），并且词 →
    全码的反查必须在装载期做一次（按键路径上扫 13.6 万条是不可能的）。
26. **给人改的配置不能复用给机器写的解析器。** `scheme.toml` 的 `Manifest` 遇到任何不合规
    语法都报错（对，因为那是生成器 bug）；`config.toml` 若照抄，用户少写一个引号就打不了字。
    正确做法是宽容读取：坏行跳过 + warning，只在日志里吼。反过来的错误同样存在——
    把宽容解析器用在数据文件上，会把「生成器写坏了」变成「静默少了几千条词」。
27. **决定对了，不代表写下来的理由对；错理由会被后来的人当成依据。** D49 把 Tab 与
    F1/CapsLock 归进「放行也不要紧的功能键」，理由是「它们不会往文档里插字符，不会打断
    组合串」。前半句对，后半句对 Tab **是错的**：Tab 让宿主切焦点，TSF 随即终止组合串，
    `OnCompositionTerminated` 把编码当残留删掉。这条错理由后来又支撑了 D50 的一个设计
    （「`Ctrl+Shift+/` 取当前高亮项」），直到真机试用才发现「高亮根本移不动、那个键与
    `Ctrl+Shift+1` 等价」。两条教训：① 决策记录里**「观察到的现象」与「推断出的后果」要分开
    写**，推断的那部分必须标明「未实测」；② 一个设计若依赖「某个状态可以被用户改变」，
    落地前先 grep 那个状态**有没有写入点**——`Cands.highlight` 全仓只有三处写入且全是
    `= 0`，一眼就能看出来。

28. **「保护第一名」用相对倍数会被第一名自己的使用量反噬；换成绝对分差只是换了个形状。**
    旧门槛是 `total(挑战者) ≥ 1.5 × total(第一名)`，而 `total` 含 `g·ln(1+n)` —— 第一名
    每被上屏一次，门槛就跟着涨一次。于是：挑战者刚够门槛上位 → 第一名打一次 → 门槛抬高
    → 挑战者被打回，还要重新攒十几分。
    第一反应是改成绝对分差 `total(B) > total(A) + δ`：正反馈没了，可真机上又出现新症状
    —— `云` 用了 26 次反被只用了 9 次的 `去` 压住，而且两个分差小于 δ 的词**每按一次
    就换一次位**（日志里 `fcu` 的返回顺序在 0.31–0.36 的分差上来回跨线）。
    根因在同一个地方：**「一次上屏加多少分」不是常数**。`g × ln((n+2)/(n+1))` 在 n=0 时
    是 0.2426，n=26 时只剩 0.0127 —— 任何固定的 δ 折合成「几次按键」都会随 n 涨几十倍，
    于是越常用的词越难越过别人。
    ⇒ 「少用一次不该改顺序」这类需求要**写成对 n 的约束**（`e = max(0, n−1)`），
    不要写成分数门槛。凡是「用一个常数去表达一个随 n 变化的量」的设计，都会在 n 大的
    那一端失效，而且失效的样子看起来像 bug。
29. **改了排序，还要问「用户什么时候看到」。** 连打同一个码时，server 每次查询都会
    重算顺序，而 DLL 上屏后就清缓存 ⇒ 用户手指还按着「2」，第 2 条已经换成了别的词，
    于是误选。真机日志里这一次误选让刚上位的词退回去多打 11 次。
    排序算法再稳也挡不住这个问题，因为它是**展示时机**问题：缓存要在「提交别的码」时
    才失效，而不是「上屏」时。两层别混为一谈（见 §4.13）。
30. **「查了别的码」不等于「换码了」。** D55 用「查询的码变了」当清缓存信号，真机上等于
    没生效：打完 `fcu` 再打下一个词，第一个字母 `f` 在缓存里就是「另一个码」，脏标记当场
    被清掉。而且一次组合串内部本来就不会重排 —— 所以「连打期间不跳」这件事只有跨组合串
    才有意义，恰好是那个判据没管住的那一半。**判据要选真正改变数据的那个动作**（提交），
    不是它的前奏（查询）。
31. **协议一旦有了「主动推送」，一问一答的前提就没了。** v5 的字根表是 server 在握手后
    自己发的一帧，它躺在管道里等客户端读。两处都踩得到：
    ① 发送顺序——客户端握手是「发 Hello、读一帧」，先发字根表会被 `open()` 当成握手应答；
    ② 读应答的循环——不跳过它，下一次查询读到的是它、再下一次读到上次的候选，**整条
    管道永久错位一帧**，症状是「查询返回了奇怪的东西」，看起来像引擎 bug。
    做法：推送排在应答**之后**发，客户端在等应答时只跳过这一类帧（其余帧照旧当应答），
    并给跳过次数设上限。
32. **键帽上「主字根 / 变体」的分界要跟数据走，不能按位置硬编码。** 示例里 G 键是
    4 主 + 2 变，照着写 `if i < 4` 会看着对；换 Q 键（8 主 + 1 变）就全错。分界由
    `main_root_count` 传下来，几何层只用它。
33. **Rust 原始字符串遇到 `"#` 会提前闭合。** 写 `r#"… fill="#fff" …"#` 时，`"#`
    正好是结束符，编译器报一串看不懂的错。内容是 HTML/SVG 就一律用 `r##"..."##`
    （或更高级的井号数量），别等报错才想这件事。
34. **`Ctrl+Shift+数字` 不是「没人用的组合」，它是 Windows 切换输入语言的热键。** 键位图
    v1.28 用它，真机第一次按就把输入法切走了——图弹出来了，但手一松已经不在亨利上。
    选组合键要先问三件事：系统占没占（`Ctrl+Shift` / `Alt+Shift` 是输入语言切换）、
    输入法自己占没占、宿主占没占。`Ctrl+Alt` 没有系统级绑定（AltGr 只存在于带 AltGr 的
    拉丁布局，中文环境的美式 / 中文布局没有这个键），顺带把「Excel 的 `Ctrl+Shift+1..5`
    是数字格式」这条顾虑也一起消掉了（D61）。
35. **`z` 既是五笔万能键又是拼音声母，通配只能认末位。** 拼音音节不以 `z` 结尾，但**连续拼音**
    的第二个音节以它开头（`niza`）。若允许 `z` 出现在任意位置，打到 `niz` 那一刻就会被判成通配，
    候选窗整片跳成五笔前缀。只认末位把冲突压到「用户还在往下打的那一瞬间」——闪一下，下一个字母
    就恢复。判据写死在 `wildcard_stem`，改它之前先想清楚连续拼音这条路径（D62）。
36. **五笔前缀查询的结果必须按字去重。** 同一个字常挂在多条词条上（`vb` 二级简码与 `vbg` 三级
    简码都是「好」，更长的码上还有三四条重码字）。通配问的是「有哪些**字**」，不去重的话首屏
    五条可能是同一个字的不同码——用户看不出「`vb` 下面还有谁」。去重后仍要保留各自的全码，
    否则 `vbz` 就退化成「一屏没有码的字」（D62）。

37. **「连不上」不等于「server 没跑」——先分类，再决定要不要拉起。** 实测踩过：一个还加载着
    旧协议 DLL 的宿主进程，每 200 ms 握手被 `Bye` 拒一次，但 server 写完 `Bye` 就断管道，客户端
    读到的是 `ERROR_NO_DATA`；旧代码把一切失败都当成「server 没跑」，于是每 5 s 拉起一个新
    server——新 server 绑不上已被占用的管道、立刻退出，无限空转。修复（D63）：`open()` 按
    **失败发生在哪一步**分类（没连上 / 被拒 / 断在半路），只有「没连上」才拉起；server 写完
    `Bye` 延迟 100 ms 再断管道，让客户端能读走拒绝原因。判据永远看**失败发生在哪一步**，
    不看错误码长相——同一个 `ERROR_NO_DATA` 既可能是「拒绝后断管」也可能是「真没 server」。

38. **沙箱 / 无头会话里 PrintWindow 和屏幕 BitBlt 抓不到分层窗口的内容**——截出来是纯黑。
    候选窗是 `UpdateLayeredWindow` 的分层窗口，DWM 合成阶段在无头会话里根本没跑，
    所以「抓屏幕」这条路全黑。绕法（D64）：不走屏幕，让程序**自己把要上屏的那张渲染 DIB
    落盘**（cand-preview `--save-bmp`）——渲染目标是一个 memory DC，`GetCurrentObject(hdc, OBJ_TYPE(7))`
    取当前选入的位图，再 `GetDIBits` 落成 24 位 BMP。

39. **`GetDIBits` 不能传裸的 `HBITMAP::default()`**——传空句柄它返回 0（失败）。
    必须先 `GetCurrentObject` 取到**当前选入该 DC 的**位图再喂给它。
    另外 `windows` crate 的 `OBJ_TYPE` 枚举没有 `OBJ_BITMAP` 关联常量，用 `OBJ_TYPE(7)`（7 = OBJ_BITMAP）。

40. **手写的「负例」必须实测过再用。** M3 总验收的第一版语料里，我凭印象把 `hao'ni'` 当成了
    「拼不出真词」的例子——实际「好呢」是词典真词，于是测试报「应不出窗，实际出了 1 条」。
    负例的价值全在「这个组合确实没有对应的词」，而这只能查词库确认，不能靠语感：
    语料里的 4 条负例最后都是拿 `look` 跑过、确认「段 1 出得了字、段 2 的码也出得了字、
    但没有任何组合是真词」才留下的。
41. **测试语料要分层取样，不要顺手挑前面几条。** 按权重降序取前 50 条，得到的全是「无 / 五 / 好」
    这类最常用词——它们恰好是排序与池子都最照顾的那批，测不出边角。「悟」排在 `wu` 的第 17 位
    就是这么漏掉的：只有把取样铺到全量（前 200 取 25、其余均匀取 25），才会撞上「池子差一条」。

42. **`WM_NCCREATE` 里只返回 1 不转 `DefWindowProcW`，窗口会没有标题。** 窗口标题是
    `DefWindowProcW` 在处理 `WM_NCCREATE` 时从 `CREATESTRUCT` 存下来的；我们为了存
    `GWLP_USERDATA` 自己接管了这条消息，返回 `LRESULT(1)` 就把它跳过了。现象很迷惑：
    窗口肉眼可见、能操作，但 `FindWindowW(L"亨利五笔 · 设置")` 找不到，截图脚本报
    `window not found`。写法：存完 userdata 后 `return DefWindowProcW(...)`。
43. **GUI 进程必须在建窗前声明 DPI 感知（`SetProcessDpiAwarenessContext`）。** 不声明
    时进程是「DPI 不感知」的：`GetDpiForSystem()` 返回 96、我们按 96 布局，然后 Windows
    把整张窗口当位图放大到 200%——窗口本身 900×600，截图出来是 1800×1200 且字全糊。
    迷惑点在于**布局坐标全部正确**，只是被整体缩放了一次，肉眼看截图会误判成「字号太小」。
44. **Tab 控件（`SysTabControl32`）必须压在 z 序最底层，否则它盖住整页控件、所有点击落空。**
    （**v1.55 起已按 D79 删掉 Tab 控件、页签改为自绘，本条成为历史**；留下的教训是
    「命中测试才是判据，截图看不出来」——`--selftest-ui` 的逐页 z 序检查仍在跑。）
    M4a 交付后第一次真机试用报的是「新增点了没反应、词也没真加上」。真相是：Tab 的矩形
    覆盖整个内容区（`(190,213)-(1053,728)`，把按钮、编辑框、列表全包进去），而它**自己被抬到了
    z 序最上层**——于是鼠标点哪个按钮，命中的都是 Tab，`WM_COMMAND` 一条都不会发出来。
    界面看起来完全正常（`PrintWindow` 截图一个字都不差），只发 `WM_COMMAND` 的脚本也全绿，
    **只有真的点鼠标才会发现**。
    - 它什么时候浮上来的：`SysTabControl32` 在 `WM_SETFOCUS` 里会 `SetWindowPos` 抬自己到顶部，
      而建窗显示时第一个 `WS_TABSTOP` 子控件正好是它。
    - 两道防线：`layout()` 末尾 `SetWindowPos(tab, HWND_BOTTOM, …, SWP_NOMOVE|SWP_NOSIZE|SWP_NOACTIVATE)`；
      再给 Tab 挂子类化过程，在 `WM_SETFOCUS` 里**先 `DefSubclassProc` 再压回去**（顺序反了会被它自己的抬升覆盖）。
    - 验收方式：`--selftest-ui` 用 `RealChildWindowFromPoint` 在 10 个可交互控件的 5 个点上做命中测试，
      命中的必须是它自己。**别用 `WindowFromPoint`**：那是在整个桌面上找窗口，会被别的窗口甚至
      锁屏界面（`LockScreenBackstopFrame`）抢走结果，锁屏时全部测不出来。
45. **「改了没生效」先对齐时间线，别急着改代码。** M4a 真机复验报的是「保存了「微信」，
    Notepad++ 里打 `tmwy` 还是那几个生僻字」。翻日志：`tmwy` 那次查询发生在 **10:19:12**，
    那一刻 `自定义词库：0 条生效`；`1 条生效` 是 **10:19:21**（保存之后）；再之后用户打的
    是 `wmty`——码序记错（微 `tm` + 信 `wy` = `tmwy`）。也就是说功能当场就生效了，
    截的是加词前那一拍。三条判据，以后照这个顺序查：
    - server 日志的 `自定义词库：N 条生效（五笔 x / 拼音 y）` 是「真的合并进去了」的唯一
      证据；同一次重连的 `Welcome.entry_count` 会跟着 +1（136239 → 136240）。
    - 想知道**常驻 server 此刻**返回什么，**别用 `heinrich-cli look`**：它自己新起一个
      `Host`、自己读一遍词库，所以它永远说「有」——看不出热重载到底有没有生效。
      要发真帧：`out/probe_pipe.py`（`Hello` + `Query`，纯 stdlib）。
    - 写这种探针时**必须给 `WriteFile`/`ReadFile` 设 `argtypes`**。不设的话 x64 下指针参数
      被按 32 位 int 截断，表现是「`WriteFile` 返回成功、对面永远收不到 Hello」——极容易
      误判成「server 卡死/管道坏了」。另外管道名带会话后缀（本机 `\\.\pipe\heinrich-14`），
      挨个试 0..39 比猜规则稳。
    - 产品侧的修法不是解释，而是**把码摆在眼前**：选中某行时脚注直接写「打字时输 `tmwy`」。
    - **真正的代码 bug 是另一条**（D67）：DLL 的候选缓存本来永不失效。用户加词前打过
      `tmwy`，那一份就被缓存住了；加完词再打，DLL 直接返回旧结果，server 连 `Query`
      都收不到——日志上表现为「server 明明 1 条生效，却什么查询都没有」。修法：缓存条目
      带 `Instant`，超过 `CACHE_TTL`（3 秒）就当没有。**改 DLL 之后已开的宿主必须重启**
      （Notepad++ / Chrome 都把 DLL 加载在进程里），这一点一定要在交付说明里写上。

46. **「保存」是一个状态转移，验收必须连做两次。** `--selftest-ui` 里原来的面板流程只验
    「填词 → 保存 → 文件里有它」，一次就过；可用户连加三个词，列表里只剩最后一个（F46）。
    病根是保存后**没把面板腾干净**：面板还留着上一个词、`editing` 还指着它，下一次保存
    就被判成「改名」——删旧词 + 加新词，看起来像是新数据把旧数据顶掉了。
    两个教训：
    - **凡是有状态的批量操作（保存/删除/发送），自测要连做两次**，断言「两次的结果都在」。
      单次通过只证明第一条路径对，证明不了状态机回到起点。这一条与「幂等」是两件事：
      这里要的恰恰是**第二次不幂等**（产生第二条记录）。
    - **界面上的「点一次」和「连着点三次」在代码里是同一条路径的不同状态进入点**。
      写 `on_save` 这类函数时先问一句：这个函数返回后，所有相关字段是不是等于「刚打开
      窗口」的样子？不等就是留了一个只在第二次暴露的坑。
    另外一条同类经验：验证时要 `BM_CLICK` / `SendMessage(WM_CHAR)` 打真消息，但**读控件
    文字别用 `GetWindowText`**——跨进程读 Edit 的内容会返回空串（`WM_SETTEXT` 返回 1、
    `GetWindowTextLength` 却是 0），很容易被误读成「没写进去」。判断以**落盘的文件**为准。

47. **`WM_CTLCOLORSTATIC` 回的画刷必须等于主窗口的背景刷。** 窗口类注册时
    `hbrBackground` 是 `COLOR_BTNFACE + 1`，而 `WM_CTLCOLORSTATIC` 里图省事回了
    `COLOR_WINDOW`（白）——结果每个字段名背后挂一块白方块，浅灰底上一眼可见（F47）。
    写 `SetBkMode(TRANSPARENT)` 只能让文字背景透出去，**画刷仍然会填满整个控件矩形**，
    两件事要一起改。
    两个附带坑：
    - `SS_CENTER` / `SS_CENTERIMAGE` 在 windows crate 的 `Win32::System::SystemServices`
      里，那个 feature 没开（`could not find SystemServices in System`）。为一对常量开一整个
      feature 不划算，照 `winuser.h` 写字面量 `0x1` / `0x200` 即可。
    - 这类**只差十几级灰度**的视觉问题，看图容易漏（尤其截图经过 PNG 压缩或缩放）。
      判据用像素：取 label 矩形四边众数当背景色，与窗口空白处比 `sum(|ΔRGB|)`；
      居中度用墨迹包围盒中点与控件中心的偏移（本次实测水平 +0.0 / +0.5 px、背景
      `(240,240,240)` 与窗口底一致）。自测里则直接发一次真的 `WM_CTLCOLORSTATIC`
      比对返回画刷——比读代码可靠，也比截图精确。

48. **`GetModuleFileNameW(None)` 给的是宿主 exe，不是你的 DLL。** 注入型 DLL 想找
    「自己所在目录」必须另外拿模块句柄：`GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, ...)`
    或者干脆在 `DllMain` 里把 `hmodule` 存进一个 `static`。我们用后者——只多一条原子写，
    不取锁、不分配，在 loader lock 下安全。写错的表现是「点选项去开了宿主目录下的
    `heinrich-settings.exe`」：如果宿主目录碰巧有同名文件，会开起别人的程序，而你从
    现象上完全看不出是自己找错了目录。

49. **「系统帮我们画 UI」的功能，推断链必须落到真实界面，而且每一步都要有原始证据。**
    系统设置页的「选项」按钮，我们连着推错两次：v1.43 把「接口存在」当成因果
    （实际微软五笔/拼音拿不到接口也有按钮）；v1.44 反推成「只给微软自家画」
    ——**那次的证据是把一张未展开的截图读成了「微软五笔那行也只有删除」**，
    图上没有的东西不能当证据。最后：我们 `ITfFnConfigure` 是 `S_OK` 没按钮、搜狗五笔
    也没有；`SystemSettings.exe` 渲染列表时不加载任何 TIP DLL（模块表 + 日志零调用）。
    判定条件至今未找到 → 已放弃（D70）。两条可复用的教训：
    - 引用截图/日志时**先回去看原始像素**，别凭当时的印象复述；「所以不用做了」这类
      结论尤其危险，它会顺着自己撑自己的证据走。
    - 「按钮不见」这类静默问题只能靠探针复现系统的调用
      （`tools/probe/probe_fnconfigure.py`），并配合进程模块表判断「系统到底加载没加载
      我们的 DLL」——这是比日志更早的分界线。

50. **同一段字符串写在 DLL 和 GUI 两边，就一定会漂。** 设置窗口的类名
    （`FindWindowW` 与 `RegisterClassW` 两边都要）与 exe 文件名（DLL 找它、安装器放它）
    都收进了 `heinrich-tip-id`（`SETTINGS_CLASS` / `SETTINGS_EXE_NAME`）。写两份的失败
    表现是「连点两次选项开出两个设置窗口」或「点了没反应」，两者都不报错。
    `w!` 宏只吃字面量，所以这类常量要改成运行时 `encode_utf16()` 再传 `PCWSTR`——
    换来的是单一事实源，值得。

51. **API 参数排查按「最便宜的可变项」排序，别按自己的假设排序。** `ActivateProfile`
    恒 `E_INVALIDARG` 时，我按假设先后试了「langid 限制」（其实是老接口
    `ActivateLanguageProfile` 的限制）、「hkl 必须非零」（`GetProfile` 查回的真实值就是
    0x0）——最后才试 dwflags，而答案就是它（必须带 `TF_IPP_FLAG_ACTIVE(1)`）。正确
    顺序应该是：先把每一个参数对照 MS 文档的 Remarks 逐个排除，**枚举常量类参数
    （flags）最先试**，因为取值空间小、改一行就能验证；「查回什么就传什么」的参数
    其次；跨接口的语义假设（A 接口的限制被记到 B 头上）最后。顺带的教训：恢复原
    profile 时 clsid 为零 = 原来是普通键盘布局，要用 `TF_PROFILETYPE_KEYBOARDLAYOUT`
    恢复——类型选错同样是 `E_INVALIDARG`，与 flag 问题叠加时更难分辨。

52. **`GetIcon` 既不能复用句柄、也不能立刻销毁句柄。** 两条都踩过，方向还相反：
    - **复用**（按模式缓存一个句柄反复交）：Win10 输入指示器按 **HICON 句柄**缓存位图，
      同一句柄第二次交上去它认为「这个图标我已经有了」——既不重画那一格，也不再往那格
      派点击。真机表现：第一次切换能变，第二次起图标不动、点击全部失灵（`OnClick` 一条
      都没有）、Alt+Tab 后消失。这四条其实是**同一个成因**。
    - **立刻销毁**（对象 `Drop` 里 `DestroyIcon`）：系统绘制时还拿着那个句柄，界面直接
      变空白。
    正确做法：**每次现画一个新 HICON 交出去，只保留最近 N 个不销毁**（我们取 16，约
    几十 KB），更早的才释放。单测 `every_get_icon_call_hands_out_a_fresh_handle` 钉的是
    「连续两次必须不同句柄」——这条极容易被「省内存」优化回去，注释里写了原因也照踩，
    所以一定要有测试。
    同类陷阱：任何「系统会异步使用」的句柄（HICON / HBITMAP / HMENU）都别在回调返回后
    立刻销毁，除非文档明确说「调用方负责」。

53. **输入法列表里的图标是注册表给的，不是 TSF 接口给的。** `ITfLangBarItemButton`
    只管语言栏（任务栏角标）；Win+空格 / 设置 → 语言 那一行的图标读的是
    `HKLM\SOFTWARE\Microsoft\CTF\TIP\{CLSID}\LanguageProfile\<langid>\<profile>`
    下的 `IconFile` + `IconIndex`（对照本机微软五笔：`ResourceDll.dll` + `IconIndex=1`）。
    `RegisterProfile` 的 `icon` 参数就是写这个值。我们把 `IconFile` 指到
    `heinrich_win.dll` 却从没给 DLL 加过图标资源 —— `IconIndex=0` 取不到东西，
    列表里就是一块空白，没有任何报错。修法是给一个**真的有图标的文件**（我们选了
    独立的 `heinrich.ico`，见 D72）；写完注册表不用等重启，重新打开列表即可。

54. **GUI 程序的 PE 子系统不是自动的。** cargo 默认把 bin 打成 console 子系统——从
    别的程序（输入法、开始菜单）拉起来时，背后会多一个黑框命令行窗口，看着像
    「程序没退干净」。`#![windows_subsystem = "windows"]` 要自己写。取舍是 stdout
    没了，所以用 `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`：
    release 是 GUI，debug 留控制台给 `--selftest`。验证方法（不用点开窗口）：读 PE
    头 `e_lfanew + 92` 的 `Subsystem` 字段，2 = GUI、3 = console。

55. **同一个子系统里的「两个症状」很可能是一个成因，别分别开药。** 「Alt+Tab 后图标消失」
    和「第二次切换起图标不动」我当成两件事：前者判成句柄生命周期、后者判成注册表里的
    `IconFile`，于是 v1.48 同时改了两处，结果一个没修好、还让点击全部失灵。真机 A/B
    （把 `IconFile` 指回没有图标资源的 DLL，症状不变）才把后者排除掉。
    判据其实一直在手上：**用户说的「上一版是好的」**。那一版每次 `GetIcon` 都新建句柄，
    是天然的对照组，一看就能定位到「唯一不同的是句柄复用」。以后遇到「某功能一半好
    一半坏」，先问「上次好的那版在这处代码上有什么不同」，比按症状逐个猜成因快得多；
    另外，**一次只改一个变量**，否则 A/B 会变成猜谜。
    终局补充（D74）：句柄问题修好后，`IconFile` 指 `heinrich.ico` 与指 DLL **两态皆正常**
    ——「profile 有了有效图标，系统就把那格抢走」这条推断从头到尾是错的，它只是从
    「v1.48 改了 IconFile、症状也在 v1.48 出现」这个**时间相邻**推出来的假因果。
    时间相邻只能产生候选，**要定论必须让其它变量都不动、只翻这一个**。

56. **日志的「真实」和「能读」是一对矛盾，别用去重去解决体积。** 4 天 31.4 万行 /
    32 MiB 里，86.8% 是两条消息：`E server 拒绝握手：协议版本不匹配`（159,187 条）
    和 `D 握手失败：管道的另一端上无任何进程`（113,953 条），而 `W` 级一条没有。
    第一反应是「同类事件 10 秒内只记一条」，但这样一来「偶发一次」和「每 5 秒重试一次」
    在日志里就长得一样了——**而次数本身正是信息**：5 秒一条能反推「有几个宿主还加载着
    旧 DLL」，合并掉就没法反推了。所以改成「一次不落地写 + 单日总量闸」。
    另外三件事值得记住：
    ① **rename 轮转在注入型 DLL 上不能用**。写者不受控，改名那一刻的写入会打进
       已不存在的路径（见 §4.20）。
    ② **先查 `debug status`**。这次 `debug.on` 从 10-21 一直开着，默认级别根本不是
       `info` 而是 `debug`——「为什么日志这么大」的第一嫌疑不是刷屏，是开关忘了关。
    ③ **刷屏本身可能是真故障**。15.9 万条版本不匹配说明旧 DLL 与新 server 长期共存；
       日志只负责让它可见，修它要重启宿主 / 重装。banner 就是为「下次一眼看出哪个 pid
       是哪版」加的。

57. **跨进程 `SendMessage` 带指针等于带垃圾。** `WM_NOTIFY` 的 `lParam` 是结构体指针，
    系统只对少数消息做跨进程封送（`WM_COPYDATA` 之类），`WM_NOTIFY` 不在其中：脚本往
    另一个进程的窗口发它，对方读到的是**自己地址空间里的随机内容**，于是「发过了、
    脚本也打印成功、界面一动不动」。踩法是写了个切页截图的小脚本，`cursel=1` 打印得
    漂漂亮亮，三张截图的哈希却几乎一样。正解是给程序加一个开关（`--page`），别想着从
    外面驱动它——这也是「UI 验收要能程序化打开目标窗口」那条经验的另一半。
    同一个坑的第二个入口：**ctypes 不声明 `restype` 时 `HWND` 会被当成 C `int` 截成
    32 位**，`SendMessage` 静默打到一个不存在的句柄上，同样一声不响。

58. **多页窗口里「同一类控件有两个」时，消息必须按 `hwndFrom` / `id` 分派。**
    三页共存意味着主窗口下同时有两个 ListView、五个 Edit。`WM_NOTIFY` 只判
    `hdr.code == LVN_ITEMCHANGED` 会让学习页的选中去驱动词组页的「保存」按钮；
    `WM_COMMAND` 里 `code == EN_CHANGE` 一律调 `update_buttons()` 会拿两个数字框的
    内容去判「词填了没」，结果按钮永远亮着。分派写细一点，比事后再加一堆 `if` 便宜。
    同理，**z 序 / 命中自测要按页做**：隐藏页的控件命中测试必然失败，不切页就断言
    只会得到一个假失败（`PrintWindow` 也只看得到当前可见的那一页）。

59. **自绘父窗口的「擦背景」不能外包给系统。** 症状是切页后旧页内容留在屏上、与新页叠在一起，
    看起来像「页签点了没反应」或「切几次全糊了」。根因在 `InvalidateRect(hwnd, None, false)`
    的第三个参数：`false` 表示「不要擦除」，而 `paint()` 又假设背景已经由 `WM_ERASEBKGND` 铺过
    ——两边都不擦，旧像素就永远留着。**规矩：`WM_PAINT` 里第一件事自己 `FillRect` 铺底**，
    不依赖擦除消息；另外主窗口必须带 `WS_CLIPCHILDREN`，否则铺底会连子控件的白底一起刷掉。
    这类问题**静态截一张图看不出来**（每页单独看都正常），必须「真消息点击 → 再截图」才算验过。

60. **给控件追加样式前先查那张样式表 —— 同一位在不同控件里是别的东西。** 想给 ComboBox 加滚动条，
    写了 `WS_VSCROLL`（**0x0020**），而 ComboBox 的样式表里这一位是 `CBS_OWNERDRAWVARIABLE`：
    控件被静默置成自绘模式，条目明明已经在里面、却没人画，下拉展开后**一片空白**。
    症状最像「数据没填进去」，查起来会先去怀疑数据源。规矩：`SetWindowLongPtrW(GWL_STYLE, ...)`
    追加样式时，只加**目标控件类文档里列出的**那几位；形状类（宽高/位置）与滚动条交给布局解决。
    排查手法：把 `GetWindowLongPtrW` 拿到的样式打成十六进制，对着常量表逐位核。

61. **`NM_CUSTOMDRAW` 是 `NM_FIRST - 12` = `0xFFFFFFF4`。** 写成 `LVN_FIRST - 12`（`0xFFFFFF90`）
    差 88，`WM_NOTIFY` 里一条都匹配不上——表现为「代码在跑、画面没变」，而且没有任何报错。
    另外两件同源的事：① `CDIS_SELECTED`（0x0001）在 `CDDS_ITEMPREPAINT` 阶段实测**每行都带**，
    照它涂会把整片行染成选中色，判「选中没选中」要问列表（`LVM_GETNEXTITEM(-1, LVNI_SELECTED)`）；
    ② 自定义绘制接管行以后，列的 `LVCFMT` 必须与自绘文字的对齐方式一致（列头仍是系统画的），
    否则同一列上下错开半个身位，看起来像「字体不对」。

62. **彩色 emoji 只有 D2D 能画。** GDI 的 `DrawTextW` 与 GDI+ 都只出**单色轮廓**，`Segoe UI Emoji`
    在它们眼里是一套普通字形。正确路径是 `ID2D1DCRenderTarget`（DC 渲染目标，不需要换 D2D 1.1）
    + `D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT`。这个结论在本项目出现过两次（候选窗 D55、
    emoji 选择器 D86），不要再从 GDI 试起。判据**用颜色计数**（同两行文字数不同颜色值，
    105 vs 13），不要用眼睛看「像不像彩色」——灰度抗锯齿也能骗过眼睛。

63. **「几何放大了、字没放大」是自绘窗口的默认结局。** `ID2D1DCRenderTarget` 用
    `dpiX/dpiY = 96` 建，DWrite 的字号就 1:1 等于物理像素；而布局通常已经乘过 DPI。
    两套坐标系混着用，200% 下必然出现「格子 92px、字还是 26px」。
    **判据**：字号与它所在矩形的比例必须与 DPI 无关（本项目写成单测：`font_emoji / cell`
    落在 0.6–0.75、标题字号随 scale 翻倍），别靠截图眼看。
    同一类的第二个入口是**控件归属**：页内控件漏登记进显隐列表，它就会在每一页上一直显示。
    给自测加一条「当前页可见的子控件 ⊆ 本页集合」（白名单只留给刻意共用的状态栏），
    比事后逐页截图便宜得多——它第一次跑就抓出了自己白名单漏写的状态栏。

64. **「空缓冲放行」这类键判定，前提必须跟着词库配置走。** `z` 键的「空缓冲放行」（D42）被
    D62 原样保留进代码；装上拼音词库（D47 能力位）后前提就塌了——拼音里 `zhe / zhang /
    zhenbang` 全以 `z` 开头，空缓冲的 `z` 被放行上屏，组合串只剩 `henbang`，用户打
    `zhenbang` 出「很棒」。判据写成 `empty && !input.pinyin → Pass`（D88）。**排查路径**值得记：
    CLI `look` 正确 → 排除 core / 词库 → server 日志发现「组合串被外部终止，已删除残留
    "henbang"」→ 指向 DLL 按键判定。**凡是「同一物理键在两种配置下语义不同」的分支，
    判据必须挂在能力位上**，与 D47 码长上限同一条纪律。

65. **D2D 画进 ListView 自绘行（`NM_CUSTOMDRAW`）的三个独立坑。** ① `BindDC(hdc, &rect)` 之后
    绘制坐标以**绑定矩形左上角**为原点，不是 DC 原点；② DC 渲染目标是
    `D2D1_ALPHA_MODE_IGNORE`，不做 alpha 混合，每格先 `Clear(行底色)` 否则背景是未定义色；
    ③ `IDWriteTextFormat` 字号创建后不可变（`SetFontSize` 只在 `IDWriteTextLayout` 上），
    字号变了要重建——按字号缓存（差 < 0.5 复用）。

66. **`EN_CHANGE` 处理器里读文本可能是旧值，校验结论别在那里存。** `SetWindowText` 触发的
    `EN_CHANGE` 到达时 `GetWindowText` 可能还读到旧内容，「读文本 → 算结论 → 存下来」会把
    过期结论一直挂着（表现：填回合法值后错误提示不消失）。**模式**：`EN_CHANGE` 只更新
    按钮态 + 清暂存错误 + 重绘；「此刻该显示什么」由 paint 路径对当前文本**现算**
    （本项目 `emoji_panel_alert()`）。自测断言「填坏 → 有提示 → 填好 → 提示消失」的往返
    才能抓住这类坑，只断言单向抓不住。

## 7. 排序参数（`heinrich-core/src/rank.rs`）```
total = w + g × ln(1 + max(0, n−1)) × 0.5^(Δt/H)
  w : ln(1+权重)/ln(1+最大权重)，n ≤ 1 时 total = w ⇒ 首屏 = 码表序
  g : 0.35        单次使用约挪一位；g=1 时一次误选会跨 4–6 位
                  （`ranking.learning_gain`，0 = 完全不学习）
  H : 45 天       半衰期（`ranking.half_life_days`）
  —— 没有门槛参数。D44 的 K=1.5、D54 的 δ=0.35 都已退役，写了会被警告并忽略。
```

**第一次上屏不计分**（D58）：`n = 1` 时 `total = w`，与没用过完全一样。这不是近似处理，
而是「偶用一次不改顺序」的定义本身——它不随 n 变严，也不需要记「谁在位」。

这一段被改过三次（王座 → 绝对分差 → 废除门槛），两次都栽在同一件事上：**拿固定分数
表达「按几次」**。单次增量从 0.2426 衰减到 0.0127，任何常数门槛折合按键数都会随 n 膨胀。
想调「学得快还是稳」，调 `g` 与 `H`，不要再引入门槛。

`:explain` 里 `n = 1` 的候选会标「← 首次上屏，不计分」，用来回答「我选过它了为什么没动」。

## 8. 进度

| 里程碑 | 状态 |
|---|---|
| M0a 数据装配 | ✅ 完成（覆盖 100%，2–3 级简码例外 0.40%，可复现） |
| M0b 引擎 | ✅ 完成（26 单测，clippy/fmt 干净） |
| M1 TSF 壳 | 预研完成（`docs/design/tsf-preflight.md`），按 M1.0–M1.3 四步推进 |
| M1.0 键进得来 | ✅ 已验收（注册 / 激活 / 按键只记录不消费 / 状态检查；`Get-WinUserLanguageList` 里已出现该 TIP） |
| M1.1 字上得去 | ✅ 已验收（缓冲 + 小码表 + `ITfEditSession` 上屏 + 单击 Shift 切中英；`vbg`→好、`q`→我、`vbgf`→好玩、Esc 取消、无命中原样上屏） |
| M1.2 组合串 | ✅ 已验收（`ITfComposition` preedit 逐键生长 / 退格收缩 + 空格原地替换 + Esc 与方向键取消 + 外部终止删残留；显示属性虚线与光标跟随推迟到 M2，因为它们只服务候选窗。实际观感是宿主默认组合串渲染的浅底色块） |
| M1.3 接 server | ✅ 已验收（`heinrich-proto` + `heinrich-server` 命名管道 + 50 ms 硬上限 + 后台重连 + 中文标点 + 全局中英模式）。降级路径用 `tsf.ps1 server slow` 验，见 §4.5 |
| M2.1 候选窗几何 | ✅ 已验收（`crates/heinrich-ui`：单行布局 + 两端翻页箭头 + 分页 + 超宽压窄 + 命中 `hit()` / `hit_arrow()` + 定位夹工作区，27 单测；`examples/geometry.rs` 出几何 SVG） |
| M2.2 候选窗窗口 | ✅ 已验收（`apps/windows/ui`：分层窗口 + `MA_NOACTIVATE` + D2D DC 渲染目标 + DWrite 量字；`apps/windows/cand-preview` 带 `--selftest`；`tools/shot-window.py` 自截图。见 §4.7） |
| M2.3 接进 TSF | ✅ 自测通过（记事本 / Notepad++ / Chrome）（逐键查询 D37 + `GetTextExt` 三级兜底 + 数字键/点选 + 失焦隐藏 + 方向键翻页 D38；`cand.rs` / `caret.rs`，状态搬进 `Rc<Core>`，见 §4.8） |
| M2.4 学习落库 | ✅ 已真机验收（2026-10-22 三轮日志：连打不跳位 / 换码后升位 / 原首位上屏一次不夺回；交叉点 n=23 与解析解一致）。真实库与日志已核（177 词 / 累计 400+ 次）；按实测三次修正：D54 绝对分差 → **D58 废除门槛、第一次上屏不计分** → **D59 抖动区写成规格**；连打缓存判据 D55 → **D57 提交别的码才失效**（`store.rs` 的 SQLite + WAL + 每 60 秒 checkpoint、单向 `Frame::Record`、DLL 有界队列补发、安装器先收旧 server。见 §4.9 / §4.13） |
| M3.1 拼音数据 | ✅ 完成（`data/pinyin/`，`rime/rime-pinyin-simp` Apache-2.0，65,125 条 / 415 音节；常用字 100% 覆盖。见 §4.10） |
| M3.2 混输裁决 | ✅ 已验收（`PinyinLexicon` + `core::pinyin::query` 单段判定与折叠 + `rank_inputs` 共用排序；CLI `--pinyin` / `check-pinyin`。判定已在 v1.20 按 D48 反转成五笔优先，见 §4.10） |
| M3.3 接进 TSF | ✅ 已验收（真机验过 `hao` / 带 `'` 的纯拼音 / `-` `=` 翻页 / 组合态符号不进文档）（协议 v3：`features` / `source` / `primary_len`；`Host::query` 返回 `QueryReply`；DLL 侧 `ipc::Reply` + 能力位驱动 24 码上限（`z` 的归属在 v1.31 由 D62 改为「一律进缓冲」；**v1.64 由 D88 再修**：空缓冲放行必须带能力位判据 `empty && !input.pinyin`，装了拼音时空缓冲的 `z` 也进缓冲——否则 `zhenbang` 的首字母 `z` 上屏、组合串只剩 `henbang`）；`dist\data\pinyin` 由 `build.ps1` 一并拷。**v1.20 追加**：D48 五笔优先 + `'` 强制拼音 + 补段淡一档（`Candidates.split` → `Theme.secondary`）；D49 `-`/`=` 翻页 + 组合态内吞字符键。见 §4.11） |
| M3.4a 码提示 chip | ✅ 已验收（§6.3 / D51：协议 v4 的 `Entry.code`；`PinyinEntry.wubi` 装载期反查（单字 100% / 词组 58.5%）；`hint.mode` 三档 + `config.toml` 热更新；几何层 `CandidateText.hint` / `ItemBox.hint_rect`，绘制端 `Theme.chip_bg`；`cand-preview --hint`。随验收一并落地 D52（撤销 `Ctrl+Shift+/`）与 D53（Tab 组合态内吞掉）。见 §4.12） |
| M3.4b 码位键位图 | ✅ **2026-10-22 真机验收通过**（D50 / D60 / D61 / §6.7 / §4.14：`roots.toml` 25 键 130 字根 + `heinrich-cli roots` 五项校验 + 协议 v5 下发 + `KeycapWindow` 点击穿透 + `Ctrl+Alt+1..9` 入口（仅中文模式且候选窗存在时接管；v1.29 由 `Ctrl+Shift` 改过来，且键帽不再画一级简码）。`Ctrl+Shift+/` 入口已按 D52 撤销） |
| M3.5 `z` 通配 + 段级多段 | ✅ **2026-10-23 真机验收通过**（D62 / D64 / §6.5 / §6.8 / §4.15：`wildcard_stem` + `split_segments`；`z` 从「吃掉不动」改为进缓冲；`MAX_EXTENDED_CODE_LEN = 9`；多段只出词典真词 + 词中 chip（协议 v6 `hint_at`，D64）；多段不记学习分；`heinrich-cli look` 与 `check.ps1` 通配/多段自测；`cand-preview --save-bmp` 无头视觉验收。验收中发现词表缺新词，转 D65 预研——见 §8 后继） |
| M3.6 自定义词组 | ✅ **已验收（以自测为准，不做真机手测，D75）**（D65 / §6.9 / §4.16：`%APPDATA%\Heinrich\words.tsv` 四列；`Scheme::word_code` 词组取码（与词库 6.1 万条对照一致率 99.96%）；`apply_user_words` 双库全量替换合并；权重取同词长中位数、不置顶；server 走 mtime 节流做 1 秒热重载（**协议仍是 v6**）；CLI `word add/list/rm/import` + `look --words`；`check.ps1` 加「自定义词自测」）。热生效链路已由 M4a 真机经同一份文件覆盖 |
| M3 总验收 | ✅ **2026-10-23 自动化通过（154 例）**（§4.17：`tests/corpus/mixed.tsv` 分层取样语料 + `tests/mixed_corpus.rs` 四项断言；顺带把 `SEGMENT_LIMIT` 16→32——「悟」在 `wu` 排第 17 让 `wu'ntgg` 出不了窗） |
| M4a 设置窗口 + 词组页 | ✅ **已真机功能验收（2026-10-23，D75）**（§4.18 / §6.10：`apps/windows/settings`，Win32 原生控件；只写 `words.tsv` 靠 mtime 热重载，无 IPC；`--selftest` 七项 + `check.ps1` 步骤）。六项功能通过；UI 细节 F46/F47/F48/DPI 属前序已修项，本轮未重走 |
| M4b 通用设置页 + 学习数据页 | ✅ **已实现（2026-10-23，D78，待真机验收）**（§4.21：`hint.mode` + `learning_gain` / `half_life_days`，`edit_config_file` 定向改写；`learn.db` 只读列表 + 搜索 + 清单词学习分；`--page 0\|1\|2` 供截图） |
| M4b-UI 设置窗口视觉重做 | ✅ **已实现（2026-10-23/26，D79 + D80 + D81，待真机验收）**（§4.22：`theme.rs` token 表 + 系统强调色；文字页签 / 卡片 / 三档按钮 / 三段选择器全部自绘，Edit 与 ListView 保持原生；`app.manifest` + `build.rs` 请求 comctl32 v6；文字摆位改为「实测字宽 + 自算左端点」，页签下划线即文字宽、输入框文字真垂直居中、列表列按内容对齐） |
| M4c emoji 引擎 | ✅ **已实现（2026-10-26，D83，待真机验收）**（§4.23：探针证明 D2D 1.0 + `ENABLE_COLOR_FONT` 即出彩色；`heinrich-gen --kind emoji` 从 CLDR 生成 2395 条 + 正文哈希；`heinrich-scheme::emoji` 两层映射（按词 > 按码、`enabled=false` 删 key）；`insert_emoji` 在 `Host::query` 之后插独立候选（算进 `limit`、折叠分界顺移）；协议 v8 `Entry.kind`；`heinrich-font` 按本机字形丢条目（2395 → 2018）；布局只留 `is_emoji` 标记；CLI `emoji check/list/reset`；`cand-preview --emoji` 12 项） |
| M4d emoji 改绑页 + `config_version` | ✅ **已实现（2026-10-26，D85 / D86，v1.62 返工，待真机验收）**（设置窗口第四页「emoji」：合并视图列表 + 实时搜索 + 编辑面板改绑/停用/恢复内置 + **分类网格选择器弹窗**（v1.62 取代原 ComboBox，见 §4.24）+ 总开关；列表**选中整行自绘**（v1.62，`FULLROWSELECT` + `SHOWSELALWAYS` + `NM_CUSTOMDRAW`）；写侧整份重写用户层；单字口子——scheme 两处词长 gate 撤掉，用户层显式单字照办；`heinrich-font::probe_one` 现场判字形；`config_version` 兼容复核（未来版本只警告不拒绝、三份用户文件互不干扰）；`--selftest-ui` 四页全绿、`--picker` 可单独弹选择器验图；v1.63 补：选择器字号随 DPI 缩放（D87）、`check_page_scope` 钉住页内控件归属；v1.64 再补（D88/D89）：改绑输入校验前置（`check_input` 三种人话，红色错误写在面板行 1、绘制时现算）、列表 emoji 列改 D2D 彩色自绘（§4.25）） |
| M4e 安装包 / 卸载 + 全量回归 | ⬜ 未开始 |
| M5 | ⏸ **整体后移（D66，v1.37）**：其他平台 Shell / 98 数据 / 主题 / 拆解研究 / 外链查询 |

**动手前必读**：`docs/design/tsf-preflight.md`。里面写了注册要写哪些键、必须实现哪些接口、15 条已知坑，以及「不要在 M1.0 通过之前写组合串与 UI 代码」的理由。
另外**不要照抄青简**——它 2026-09 才首发、Windows 壳未经装机验证。参考顺序固定为：官方文档判语义 → Weasel 判工程做法 → WindInput 判产品形态 → chewing-tsf 判 Rust 写法 → 青简判坑。
M1 的冒烟矩阵：记事本 / VS Code / Chrome / Edge / 资源管理器 / cmd，另加提权记事本与一个 32 位程序（见预研 §9 待实测 T2/T3）。
