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
| 方向键 / Home / End / PgUp / PgDn（0x21–0x28），组合态 | 吃掉并取消（D31）。代价是一次按键被吞，换来的是不留错位组合串 |
| 方向键类，缓冲为空 | 放行（正常移动光标） |

### 4.1.3 组合串（M1.2）

缓冲里那串编码**就是组合串的内容**——两者是同一份状态的两种表现，所以「缓冲一变
就同步组合串」是唯一不变式。三个动作都在 `preedit.rs`，全部走 `TF_ES_SYNC` 编辑会话：

| 动作 | 时机 | 会话里做什么 |
|---|---|---|
| `Show(text)` | 每次缓冲变化（加字母、退格） | 有组合串就改内容；没有就 `StartComposition` 后写入；`text` 为空则结束并抹掉 |
| `Finish(Some(t))` | 空格 / 回车提交 | 内容换成 `t`（符号或原样字母）→ 光标收到末尾 → `EndComposition`，文字留下 |
| `Finish(None)` | Esc、方向键、失焦、切窗口、单击 Shift | 内容抹成空 → `EndComposition` |

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
22. **命名管道的 `CreateNamedPipeW` / `ConnectNamedPipe` 在 `Win32::System::Pipes`，而 `CreateFileW` 在 `Win32::Storage::FileSystem`。** 而且 `CreateFileW` / `CreateEventW` 因为形参里有 `SECURITY_ATTRIBUTES`，**必须**同时开 `Win32_Security` feature，否则整个函数会被 cfg 掉，报错是「no `CreateFileW` in `Win32::Storage::FileSystem`」——看起来像 API 不存在，其实只是 feature 没开。

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
| M1.2 组合串 | ✅ 已验收（`ITfComposition` preedit 逐键生长 / 退格收缩 + 空格原地替换 + Esc 与方向键取消 + 外部终止删残留；显示属性虚线与光标跟随推迟到 M2，因为它们只服务候选窗。实际观感是宿主默认组合串渲染的浅底色块） |
| M1.3 接 server | ✅ 已验收（`heinrich-proto` + `heinrich-server` 命名管道 + 50 ms 硬上限 + 后台重连 + 中文标点 + 全局中英模式）。降级路径用 `tsf.ps1 server slow` 验，见 §4.5 |
| M2.1 候选窗几何 | ✅ 已验收（`crates/heinrich-ui`：单行布局 + 两端翻页箭头 + 分页 + 超宽压窄 + 命中 `hit()` / `hit_arrow()` + 定位夹工作区，27 单测；`examples/geometry.rs` 出几何 SVG） |
| M2.2 候选窗窗口 | 🟡 代码完成，待真机验收（`apps/windows/ui`：分层窗口 + `MA_NOACTIVATE` + D2D DC 渲染目标 + DWrite 量字；`apps/windows/cand-preview` 带 `--selftest`；`tools/shot-window.py` 自截图。见 §4.7） |
| M2.3 接进 TSF | 逐键查询 + `ITfContextView::GetTextExt` 定位 + 数字键/点选 + 失焦隐藏 + 方向键翻页（D37/D38） |
| M2.4 学习落库 | SQLite `usage` 表 + 上屏回报 + 完整 §5 打分 |

**动手前必读**：`docs/design/tsf-preflight.md`。里面写了注册要写哪些键、必须实现哪些接口、15 条已知坑，以及「不要在 M1.0 通过之前写组合串与 UI 代码」的理由。
另外**不要照抄青简**——它 2026-09 才首发、Windows 壳未经装机验证。参考顺序固定为：官方文档判语义 → Weasel 判工程做法 → WindInput 判产品形态 → chewing-tsf 判 Rust 写法 → 青简判坑。
M1 的冒烟矩阵：记事本 / VS Code / Chrome / Edge / 资源管理器 / cmd，另加提权记事本与一个 32 位程序（见预研 §9 待实测 T2/T3）。
