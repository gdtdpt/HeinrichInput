# TSF 落地预研（M1 前置）

> 日期：2026-10-20 ｜ 对应 PROJECT.md v0.8
> 目的：在写第一行 DLL 代码之前，把「注册什么、实现哪些接口、哪些地方会崩宿主、最小能验证到什么程度」定下来。
> 证据标记：✅ 已核对源码或官方文档 ｜ 🔶 推断（有理据但未逐行验证） ｜ ⚠️ 必须实测才能定

---

## 0. 结论摘要

1. **D1（server + 薄 DLL）成立，且已有一个「同品类、已发布」的产品这么做**：清风输入法 WindInput —— 五笔 86 + 五笔拼音混输，`wind_input`（Rust）是含「引擎 / 词库 / 候选 / UI 渲染 / IPC」的核心服务，`wind_tsf`（C++）只负责 TSF 接口与按键捕获。这比青简更有说服力：青简 2026-09 才首发，WindInput 有安装包、便携版、官网和版本历史。
2. **候选窗不由 DLL 绘制**也是主流：Weasel 的 `WeaselUI` 由 `WeaselServer` 加载（独立进程画），DLL 只转发。in-proc 自绘也有人做（windows-chewing-tsf 用 Direct2D 在 DLL 里画），但那正好是我们要回避的形态。
3. **最容易翻车的地方不是注册，是线程**。TSF 在宿主应用的 UI 线程上调用你的 COM 方法——你在那里做任何阻塞（等 IPC、等锁、Sleep），宿主窗口就跟着卡死。⇒ 新增 **D18：按键路径零阻塞**，这是 server 架构唯一真正的技术风险点，必须在 M1.0 就验证。
4. **安装必须提权、运行不需要**。写 `HKLM\SOFTWARE\Microsoft\CTF\TIP\...` 与 `HKLM\SOFTWARE\Classes\CLSID\...` 绕不过管理员。这不与「asInvoker 免管理员」的桌面工具约定冲突——那是运行期属性，装输入法是安装期动作。
5. 纯 Rust 写 TSF 壳**可行但非主流**：chewing-tsf 证明 `cdylib + windows crate + #[implement]` 能跑通；但最贴近我们的 WindInput 和官方样例移植 ime-rs 都选择「C++ 壳 + Rust 核心」。⇒ 保留 Rust-only 壳（D8），但把 chewing 当模板，且**不在 M1.0 阶段自创 COM 骨架**。
6. 青简的定位下调为**逻辑参照**，不作为实现参照：它一个月里暴露过的竞态（本地整句模型只认最新那次请求）对我们有警示价值，但它的 Windows 壳还没有经过装机量检验。

---

## 1. 参考实现分级

| 等级 | 项目 | 形态 | 为什么值得看 | 不能照抄的地方 |
|---|---|---|---|---|
| **A｜在产、有真实装机** | **WindInput 清风输入法**（Rust + C++/Swift，GPL-3.0-or-later，v0.127 起） | `wind_input` 核心服务 + `wind_tsf` C++ 薄层 + `wind-installer` / `wind-portable` | **同品类**：五笔 86 与五笔拼音混输是一等公民；架构与 D1 同构；连「便携版启动器」这种发布细节都开源 | 它的五笔数据来自极点移植版（权利链存疑），我们已定 `rime-wubi`；它用 C++ 写壳，我们是 Rust |
| **A｜在产、十年以上** | **Weasel 小狼毫**（C++，GPL） | `WeaselTSF.dll` + `WeaselServer.exe` + `WeaselIPC` + `WeaselUI` | TSF 的每一项该做什么，它都有一个文件对应；注册、语言栏、compartment、显示属性全齐 | 代码量巨大、风格老（MFC/ATL 混合），照抄会被拖进它的抽象 |
| **B｜可编译可跑的样例** | **windows-chewing-tsf**（纯 Rust） | `chewing_tip`（cdylib，`windows` + Direct2D/DirectWrite）+ `tsfreg`（独立注册器）+ `chewing-preferences` | **纯 Rust 写 TSF 的唯一完整证据**：COM 实现、注册器、候选窗渲染都在 | 引擎是注音（chewing），与五笔无关；UI 在 DLL 内画（我们不采用） |
| **B｜官方样例的 Rust 移植** | **ime-rs**（saschanaz） | C++ 壳 + 8 个 Rust crate（staticlib 链进 `SampleIME.dll`） | FFI 边界怎么切、UTF-16↔UTF-8 怎么传、候选 UI 怎么用 `ITfCandidateListUIElement` | 它是拼音 + 静态链接，没有 IPC；对我们只有「边界设计」的参考价值 |
| **C｜逻辑参照** | **青简 qingjian** | Server + 各平台壳 | 异步竞态的处理、配置回归测试的意识 | Windows 壳未经装机验证；无公开的发布版可对照行为 |
| **C｜语义字典** | Microsoft `Windows-classic-samples` 的 TSF 样例 + learn.microsoft.com/tsf | 官方最小实现 | 接口语义的**唯一权威**；遇到行为疑问先查它 | 样例停留在 Win7 时代，Win8+ 的 profile 注册、Immersive 支持要另看 |

> 采信顺序：**官方文档判语义 → Weasel 判工程做法 → WindInput 判产品形态 → chewing 判 Rust 写法 → 青简判坑**。不按这个顺序，容易被单个项目的偶然选择带偏。

---

## 2. 注册到底要写什么

TSF 里有**两套彼此独立的注册**，缺一套都不出现在输入法列表里。

### 2.1 COM 注册（让 DLL 能被 CoCreateInstance 找到）

```
HKLM\SOFTWARE\Classes\CLSID\{TIP_CLSID}
  (Default)              = 显示名
  InprocServer32
    (Default)            = heinrich-win.dll 的绝对路径
    ThreadingModel       = Apartment        ✅ Weasel Register.cpp 写这个值
```

32 位宿主会去 `HKLM\SOFTWARE\Classes\WOW6432Node\CLSID\...` 找，指向 32 位 DLL（见 §7 R13）。

### 2.2 TSF 注册（让 CTF 知道「这个 COM 组件是个文本服务」）

```
HKLM\SOFTWARE\Microsoft\CTF\TIP\{TIP_CLSID}
  Category
    Category\{CATID}\{TIP_CLSID}      # 正向：本 TIP 属于哪些类别
    Item\{TIP_CLSID}\{CATID}          # 反向索引
  LanguageProfile
    0x00000804\{PROFILE_GUID}         # 0x0804 = 中文（简体）
      Description          REG_SZ        # 语言栏显示名「亨利五笔」
      Display Description  REG_EXPAND_SZ # 本地化名（资源引用）
      IconFile             REG_EXPAND_SZ
      IconIndex            REG_DWORD
      Enable               REG_DWORD     # 1 = 启用
      ProfileFlags         REG_DWORD
```

**必须注册的类别** ✅（Weasel `RegisterCategories()` 的清单）：

| 类别 | 作用 | 不加会怎样 |
|---|---|---|
| `GUID_TFCAT_TIP_KEYBOARD` | 声明是键盘输入法 | 根本不进输入法列表 |
| `GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT` | 支持中/英模式 compartment | 中英切换相关行为异常 |
| `GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT`（Win8+） | 支持 Immersive/UWP 输入 | Store 应用里用不了 |
| `GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT`（Win8+） | 支持系统托盘输入指示器 | 托盘输入法图标状态不对 |

GUID 具体值不手抄，直接从 `msctf.idl` / `ctffunc.h` 取（Weasel 仓库里就自带一份 `ctffunc.h`，正是因为老 SDK 缺符号）。

### 2.3 用 API 注册，不要手写注册表

| 用途 | API | 备注 |
|---|---|---|
| COM 类 | `DllRegisterServer`（或安装器直接写） | 也可以调 `ITfInputProcessorProfiles::Register` |
| **Profile** | `ITfInputProcessorProfileMgr::RegisterProfile(clsid, langid, guidProfile, desc, descLen, iconFile, iconLen, iconIndex, hkl, flags, enabled, reserved)` | Win8+ 的正路 ✅；Win7 用 `AddLanguageProfile` |
| 类别 | `ITfCategoryMgr::RegisterCategory(clsid, catid, clsid)` | 正反索引它自己写 |

`hkl` 参数：Weasel 会去 `SYSTEM\CurrentControlSet\Control\Keyboard Layouts` 找一个已存在的 IME 的 HKL 传进去，找不到就传 `NULL` ✅（它对港/澳/新就是传 `NULL`）。⇒ **我们传 `NULL`，不做 HKL 探测**。

**交付形态**：`heinrich-setup.exe --install / --uninstall`（管理员清单），而不是 `regsvr32`。
理由：注册器需要做「写 CLSID → 注册 profile → 注册类别 → 校验」四步，还要能反注册干净；Weasel（`WeaselSetup`）、chewing（`tsfreg`）、WindInput（`wind-installer`）**三家都单独做了注册器**，没有一家让用户手敲 `regsvr32`。

---

## 3. 必须实现的接口链

### 3.1 最小可用集（M1.0–M1.2 必须）

| 接口 | 关键方法 | 为什么必须有 |
|---|---|---|
| `IClassFactory` | `CreateInstance` | `DllGetClassObject` 的返回物；`#[implement]` 或手写 |
| **`ITfTextInputProcessorEx`** | `ActivateEx` / `Activate` / `Deactivate` | 唯一入口。`Activate` 里拿 `ITfThreadMgr`、`ActivateEx(&tid)`、挂 sink |
| **`ITfKeyEventSink`** | `OnTestKeyDown` / `OnKeyDown` / `OnTestKeyUp` / `OnKeyUp` | 按键。**测试与分发必须成对返回同值**，否则宿主行为错乱 |
| **`ITfThreadMgrEventSink`** | `OnSetFocus` / `OnInitDocumentMgr` / `OnPushContext` / `OnPopContext` | 焦点换了要清缓冲；否则文字会跑到上一个窗口 |
| **`ITfCompositionSink`** | `OnCompositionTerminated` | 组合被外部终止（点别处、Esc、宿主取消）时必须收到，否则缓冲泄漏 |
| **`ITfEditSession`** | `DoEditSession` | **所有对 context 的写入都必须走它**（`RequestEditSession`），不能直接改 range |
| **`ITfDisplayAttributeProvider`** | `EnumDisplayAttributeInfo` / `GetDisplayAttributeInfo` | 组合串的虚线下划线；不给的话组合串和正文无法区分 |

### 3.2 可选集（v1 后段或 v1.5）

| 接口 | 用途 |
|---|---|
| `ITfLangBarItemButton` | 语言栏上的图标与菜单（Weasel 有 `LanguageBar.cpp`） |
| `ITfFunctionProvider` + `ITfFnConfigure` | 从输入法菜单直接开设置窗口 |
| `ITfCompartmentEventSink` | 监听中/英模式、全/半角切换 |
| `ITfTextEditSink` | 宿主删改正文时同步学习数据（v1 可不做） |
| `ITfThreadFocusSink` | 线程级焦点（比 ThreadMgrEventSink 更细） |

---

## 4. windows-rs 能做什么、必须自己写什么

| 事项 | 结论 | 细节 |
|---|---|---|
| 调 TSF API | ✅ 开 `Win32_UI_TextServices` feature 就有 | 接口、GUID、枚举全在 |
| **实现** COM 接口 | ✅ `#[implement]` 宏，需要 `implement` feature | `windows` 0.52+ 提供；新版拆成 `windows-core` + `windows-implement`。`#[implement(ITfTextInputProcessorEx, ITfKeyEventSink, ...)] struct Tsf { .. }`，然后为每一个实现 `Xxx_Impl` |
| 引用计数 / QueryInterface | ✅ 宏生成 | 多接口自动处理 vtable 指针调整 |
| `DllGetClassObject` / `DllCanUnloadNow` / 导出 | ⚠️ **手写** | `#[no_mangle] extern "system"`，加 `.def` 只导出这两个 + `DllMain` |
| `DllMain` | ⚠️ **手写，且只能做最少的** | 仅 `DisableThreadLibraryCalls`。任何加锁 / 加载 DLL / 建线程都可能 loader lock 死锁 |
| **panic 隔离** | ⚠️ **必须自己包** | 每个 COM 方法外面 `catch_unwind`，panic → `E_FAIL` 或 `S_FALSE`。Rust 的 unwind 跨 `extern "system"` 边界是 UB/abort，宿主应用会直接消失 |
| 线程/单元模型 | 🔶 由宿主决定 | TSF 通常在宿主 UI 线程（STA）调你。不要自己 `CoInitialize`，不要在回调里 `Send` 阻塞 |
| 候选窗 | ⚠️ 两种都要自己写 | 要么 in-proc 画（Direct2D + 分层窗口），要么交给 server 画（D1 选后者） |
| 日志 | ⚠️ 自己接 | DLL 里不能弹窗、不能打印到 stdout（没有控制台）。写文件（每次 open/write/close，多进程安全）或 `OutputDebugString` |

**Rust-only 壳的现实评估**：chewing-tsf 证明可行；但 `#[implement]` 在多接口 + `ITfEditSession` 这种「框架回调你、你在里头再回调框架」的嵌套场景里，借用检查会打得比较难看（回调里拿 `&self` 但实际需要 `&mut` 状态 → 内部状态一律放进 `Mutex`/`RefCell` 且要小心重入）。这是 Rust 写 TSF 的主要税，不是不可能，是**要提前设计状态容器**。

---

## 5. 三种壳形态对照（对 D1 的再确认）

| 形态 | 代表 | 优点 | 缺点 | 我们的取舍 |
|---|---|---|---|---|
| **A. 全逻辑 in-proc** | windows-chewing-tsf、ime-rs、极点五笔 | 无 IPC、延迟最低、实现最简单 | 引擎一崩宿主跟着崩；学习数据多进程写；UI 在宿主进程里受 DPI/焦点/权限牵连 | ❌ |
| **B. 薄 DLL + 服务进程** | **Weasel**、**WindInput**、搜狗/QQ、微软拼音 | 崩溃隔离；引擎与 UI 集中一处；学习数据单写者 | 多一个进程；IPC 延迟与阻塞风险；首次按键冷启动 | ✅ **D1** |
| **C. C++ 壳 + Rust 核心静态链** | ime-rs、WindInput 的 `wind_tsf` | 跳过 Rust 写 COM 的税；核心仍是 Rust | 两套构建工具链；FFI 边界要手工维护 | 🔶 备选：若 §4 的「税」超出预期，退到这条 |

**结论**：坚持 B。A 的失败模式（宿主被拖死）恰好是你最不能接受的；C 留作应急退路，不在 M1 首选。

---

## 6. 关键约束：按键路径零阻塞（新增 D18）

TSF 在**宿主 UI 线程**上同步调用 `OnTestKeyDown` / `OnKeyDown`。这里做一次命名管道往返，正常情况下是几十微秒，但以下情形会变成「宿主卡死」：

- server 正在启动（冷启动装 13.6 万条码表）
- server 被安全软件挂起、或用户手动杀掉后正在重启
- 管道被占满 / 对端不读

青简那条「本地整句模型只认最新那次请求的结果」的 commit，本质就是这套结构在异步化之后必然遇到的排序竞态。

### 6.1 机制

```
OnKeyDown
  ├─ 更新本地缓冲区（纯内存，微秒级）
  ├─ 命中本地候选缓存？ ──是──▶ 立刻刷新候选窗（若窗在 server，则发「画」通知，不等回执）
  │                        └─否──▶ 发异步查询请求，立刻返回「消费」
  └─ 一律不等待 IPC 回复

上屏（数字键选词）
  └─ 文本来自本地缓存，直接 RequestEditSession 写入，不等 server
```

- **候选缓存**：DLL 保存「最近 N 个缓冲前缀 → 候选列表」。五笔是定长编码，同码组的候选集稳定，缓存命中率极高。
- **超时降级**：请求 100ms 无应答，候选窗显示占位而不是转圈；**连续 3 次无应答**则本次会话降级为「不消费按键」（用户会看到英文直接打出来，而不是打字没反应）。
- **server 不可用时的行为必须明确定义**，这是验收项：**宁可不接管，不可半接管**。

### 6.2 必须在 M1.0 就验证

给 server 加一个 `--debug-sleep 5000` 开关，让它在握手后故意睡 5 秒，然后在记事本里连续打字：
**宿主必须完全无卡顿**（改用 `heinrich-cli` 或任务管理器观察），候选窗显示占位。这一条不过，D1 就不成立。

---

## 7. 已知坑清单

按「会不会让你以为代码写错了」排序。

| # | 坑 | 表现 | 对策 | 证据 |
|---|---|---|---|---|
| 1 | **DLL 被注入到所有进程** | 任务管理器里 chrome 每 tab 一个进程、explorer、VS 全加载了你的 DLL；调试时断点到处命中 | 全局状态懒初始化；不在 `DllMain` 做任何事；日志带 PID | ✅ |
| 2 | **`OnTestKeyDown` 与 `OnKeyDown` 返回值必须一致** | 键被吃掉但没输入、或输入了两次 | 两个方法共用同一个判定函数 | ✅ |
| 3 | **阻塞宿主 UI 线程** | 宿主窗口整体假死，用户以为输入法坏了 | D18 | ✅ |
| 4 | 组合串写入不走 `ITfEditSession` | 偶发无效、或宿主文本错乱 | 所有写操作 `RequestEditSession(TF_ES_SYNC \| TF_ES_READWRITE)` | ✅ |
| 5 | 焦点切换不清缓冲 | 在 A 窗口打的字跑到 B 窗口 | `OnSetFocus` / `OnPopContext` 清缓冲 + 结束组合 | ✅ |
| 6 | `Deactivate` 漏撤 sink | 卸载输入法后宿主崩溃、或残留钩子 | 撤销时逐个 `UnadviseSink`，并置空指针 | ✅ |
| 7 | 老 IMM32 应用不认 TSF | 某些老国产软件、部分游戏里完全无反应 | 明确不支持；文档写清（`ImmDisableIME` 类应用） | 🔶 |
| 8 | 32 位宿主需要 32 位 DLL | 32 位程序里切不到你 | 双架构构建 + WOW6432Node 注册（R13） | 🔶 |
| 9 | 提权应用内的 IPC | 提权窗口里输入法无候选或打字丢失 | 待实测；必要时 server 也提供提权副本 | ⚠️ |
| 10 | Store/UWP/AppContainer | 沙箱应用里用不了 | 已注册 `IMMERSIVESUPPORT` 也未必够；实测 | ⚠️ |
| 11 | 安全桌面不加载第三方 TIP | UAC 弹窗、锁屏里按不出中文 | 系统设计如此，不是 bug | ✅ |
| 12 | 卸载不干净 | 语言列表里留死条目，点了没反应 | 反注册 profile + 删 CTF\TIP + 删 CLSID 键 | ✅ |
| 13 | 未签名 DLL 被 SmartScreen 拦 | 用户装完提示「未知发布者」 | 签名；WindInput README 自己就写「新版本发布初期仍可能被拦」 | ✅ |
| 14 | 候选窗在 server 进程里画，位置算错 | 候选窗跑到屏幕角落 | 由 DLL 用 `ITfContextView::GetScreenExt` / `GetTextExt` 算出屏幕坐标再传给 server | 🔶 |
| 15 | 多显示器 / 高 DPI 混合 | 候选窗大小或位置偏 | 按候选窗所在显示器的 DPI 缩放；`GetDpiForWindow` | 🔶 |

---

## 8. 最小 demo 范围（把 M1 拆成可验收的四步）

每一步都必须是**能在记事本里亲手看到结果**的，避免「写了两千行还没见过字」。

| 步骤 | 内容 | 验收（人工，记事本为主） | 预估 |
|---|---|---|---|
| **M1.0 键进得来** | 只有 `DllGetClassObject` + `ITfTextInputProcessorEx` + `ITfKeyEventSink`；`OnKeyDown` 只写日志、**不消费** | ① 语言列表里出现「亨利五笔」② 切过去后记事本里打字**照常出英文**（因为我们还不接管）③ 日志能看到每个按键的 vkey | 小 |
| **M1.1 字上得去** | 缓冲 a–y，空格/回车把缓冲**原样上屏**为组合串；先查一个硬编码的 20 条小码表，命中就上屏汉字 | ① 记事本敲 `vbg` 出「好」② Esc 取消不留残留 ③ 切到别的窗口再回来不出乱码 | 中 |
| **M1.2 组合串与显示属性** | composition + 虚线下划线 + 光标跟随；候选来自本地小表 | ① 组合串有虚线 ② 候选窗跟着光标走 ③ 点右键菜单（宿主改变选择）后组合自动终止 | 中 |
| **M1.3 接 server** | 走命名管道查真码表；候选缓存；`--debug-sleep` 验证零阻塞 | ① 13.6 万条码表下打字无延迟感 ② server 被杀后 5 秒内自动重连 ③ server 睡着时宿主不卡 | 大 |
| M2 | 自绘候选窗（heinrich-ui）+ 学习排序接入 + 词组 emoji | 见 PROJECT.md 里程碑表 | — |

**M1.0 的价值**：它把「注册对不对」与「接口写对没」两件事分开了。注册错了在 M1.0 就暴露（列表里没名字），接口错了也能从日志看出来，不用等到能打字时才发现问题在别处。

---

## 9. 待实测清单（M1.0 期间并行做）

| # | 命题 | 验证方法 | 若为否的后果 |
|---|---|---|---|
| T1 | HKCU 注册**不可用**，必须 HKLM | 写 HKCU 版本，看语言列表是否出现 | 若 HKCU 可用 ⇒ 免管理员安装可行，体验大幅改善 |
| T2 | 提权应用内 DLL 能加载、能上屏 | 以管理员开记事本测试 | 若不能 ⇒ 老五笔用户常在提权工具里打字，属硬伤，需 elevation 方案 |
| T3 | 32 位宿主是否需要独立 DLL | 用 32 位程序（如老版 WinRAR）测试 | 若需要 ⇒ R13 变成本期必须 |
| T4 | 中文输入法在 Chromium/Electron 里的组合串位置 | Chrome / VS Code / Slack 各试 | 若异常 ⇒ 候选窗定位要按进程特判 |
| T5 | `RegisterProfile` 传 `NULL` HKL 是否通用于 0x0804 | 装完看语言列表 | 若不通 ⇒ 抄 Weasel 的 HKL 探测 |

---

## 10. 参考源码索引（要抄思路时直接翻这几个文件）

**Weasel（A 级，工程做法）**

| 文件 | 看什么 |
|---|---|
| `WeaselTSF/Register.cpp` | 两套注册的完整写法；Win7/Win8+ 两条 API 路径；HKL 探测 |
| `WeaselTSF/KeyEventSink.cpp` | `OnTestKeyDown` / `OnKeyDown` 的消费语义与修饰键处理 |
| `WeaselTSF/Composition.cpp` + `EditSession.cpp` | 组合串的读写为什么必须走 edit session |
| `WeaselTSF/Compartment.cpp` | 中/英模式（conversion mode）compartment |
| `WeaselTSF/DisplayAttribute*.cpp` | 虚线下划线：`ITfDisplayAttributeProvider` 三件套 |
| `WeaselTSF/Server.cpp` + `WeaselTSF.def` | in-proc COM server 骨架 + 导出清单 |
| `WeaselTSF/ThreadMgrEventSink.cpp` | 焦点事件的正确接法 |
| `WeaselIPC/` + `WeaselIPCServer/` | 管道协议、重连、超时 |

**chewing-tsf（B 级，Rust 写法）**

| 文件 | 看什么 |
|---|---|
| `chewing_tip/` | `cdylib` + `windows`；`#[implement]` 写 TSF COM；Direct2D 候选窗 |
| `tsfreg/` | 独立注册器：注册 → 校验 → 反注册 |

**ime-rs（B 级，FFI 边界）**

| 文件 | 看什么 |
|---|---|
| `cpp/` | 壳层如何把 `ITfContext` / `ITfRange` 的调用收拢成少数几个 FFI 函数 |
| `rust/ruststringrange` | UTF-16 ↔ UTF-8 边界的处理 |

**WindInput（A 级，产品形态）**

| 位置 | 看什么 |
|---|---|
| `wind_tsf/` | C++ 薄层如何只做「抓键 + 转发」，把候选与 UI 全交给核心服务 |
| `wind_input/` | 五笔混输的引擎组织、方案文件格式、候选管理 |
| `wind-portable` | 便携版如何做「就地运行」，与安装版共享注册逻辑 |

---

## 11. 建议的 M1.0 动作顺序

1. 建 `apps/windows/tsf`（`crate-type = ["cdylib"]`）+ `apps/windows/setup`（注册器）。目录分层的判据见 PROJECT.md D24。
2. 先抄 chewing 的骨架：`DllGetClassObject` + `IClassFactory` + `ITfTextInputProcessorEx::Activate/Deactivate`，`OnKeyDown` 只写日志。
3. 写 `heinrich-setup --install`：COM 键 + `RegisterProfile` + 类别，然后打开「设置 → 时间和语言 → 语言」确认列表里有「亨利五笔」。
4. **在记事本里切过去，确认打字照常出英文**（说明我们还没接管，这是正确的中间态）。
5. 打开日志文件，确认每个按键都进来了 —— M1.0 完成。
6. 再动 `OnKeyDown` 的消费逻辑（M1.1）。**不要**在 M1.0 通过之前写任何组合串与 UI 代码。
