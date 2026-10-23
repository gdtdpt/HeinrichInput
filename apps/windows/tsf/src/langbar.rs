//! 语言栏按钮——任务栏角标的「亨/英」图标，设置窗口的常驻入口（D71）。
//!
//! ## 它是怎么出现的
//!
//! 横向调研（2026-10-23）的结论：第三方输入法没有人依赖系统设置页的「选项」，
//! 入口要么是自绘状态栏（搜狗），要么是 TSF **语言栏按钮**（小狼毫）。本模块走
//! 后一条路：`Activate` 时把一个实现 `ITfLangBarItemButton` 的 COM 对象通过
//! `ITfLangBarItemMgr::AddItem` 交给系统；因为我们注册了
//! `GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT`，Win10 会把它画到任务栏角标区——
//! 输入法激活期间常驻可见。`Deactivate` 时 `RemoveItem` 摘掉。
//!
//! 接口分工（与 SDK 头一致）：
//!
//! - `ITfLangBarItem`（4 个方法）：`GetInfo`（样式/描述）、`GetStatus`、`Show`、
//!   `GetTooltipString`；
//! - `ITfLangBarItemButton`（再 5 个）：`OnClick`、`InitMenu`、`OnMenuSelect`、
//!   `GetIcon`、`GetText`；
//! - `ITfSource`（再 2 个）：**sink 的挂/摘走这里**——语言栏管理器拿到条目后按
//!   `ITfSource::AdviseSink(IID_ITfLangBarItemSink)` 订状态更新（小狼毫的
//!   `QueryInterface` 同样把这个接口挂在自己身上）。模式一变就
//!   `OnUpdate(TF_LBI_ICON)` 让系统重取图标；**只有显隐真的变了**才带
//!   `TF_LBI_STATUS`（带上会让系统整条重建托盘格，见 `notify_icon`）。
//!
//! ## 图标句柄（踩过的坑，别再「优化」回去）
//!
//! 每次 `GetIcon` 都现画一个新 HICON 交出去，只保留最近 `ICON_KEEP` 个不销毁。
//! Win10 的输入指示器按 **HICON 句柄**缓存位图：同一个句柄第二次交上去，它认为
//! 「这个图标我已经有了」——既不重画那一格，也不再往那格派点击。v1.48 曾为省
//! 内存改成「按模式复用句柄」，真机表现就是「第一次切换能变、第二次起图标不动、
//! 点击全部失灵、Alt+Tab 后消失」。反过来，交出去的句柄立刻销毁又会画面空白
//! （系统绘制时还拿着它），所以才有「保留最近 N 个」这个折中。
//!
//! ## 交互约定
//!
//! - **左键**：切换全局中英模式（与 Shift 单击同一份状态、同一台上报通道）。
//!   与 Shift 路径的差别：这里**不清缓冲、不撤候选窗**——点击托盘时焦点已经
//!   离开输入框，残留的组合串由失焦逻辑处理，这里也没有可用的 `ITfContext`。
//! - **右键**：弹出菜单（「设置…」/「打开词库文件夹」）。两条路都支持：
//!   `OnClick(RIGHT)` 自己 `TrackPopupMenuEx`（小狼毫同款），`InitMenu` /
//!   `OnMenuSelect` 兜老式语言栏与悬浮窗的 `ITfMenu` 路径。
//!
//! ## 状态共享
//!
//! 按钮只持有 `Rc<Cell<Mode>>`（与 `TextService` 的 `Core.mode` 同一份），
//! **不引用 TextService**——两个 COM 对象互持会造成引用环，而按钮需要的
//! 只有「当前模式」这一个数。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Mutex;

use windows::core::{implement, w, IUnknown, Interface, Ref, Result, BOOL, BSTR, GUID};
use windows::Win32::Foundation::{COLORREF, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, GetDC, GetTextExtentPoint32W, ReleaseDC, RoundRect,
    SelectObject, SetBkMode, SetTextColor, TextOutW, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, FW_BOLD, HBITMAP, HGDIOBJ, OUT_DEFAULT_PRECIS, PS_NULL,
    TRANSPARENT,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::TextServices::{
    ITfCompartment, ITfLangBarItemButton, ITfLangBarItemButton_Impl, ITfLangBarItemMgr,
    ITfLangBarItemSink, ITfLangBarItem_Impl, ITfMenu, ITfSource, ITfSource_Impl, TfLBIClick,
    GUID_LBI_INPUTMODE, TF_CONVERSIONMODE_NATIVE, TF_LANGBARITEMINFO, TF_LBI_CLK_LEFT,
    TF_LBI_CLK_RIGHT, TF_LBI_ICON, TF_LBI_STATUS, TF_LBI_STATUS_HIDDEN, TF_LBI_STYLE_BTN_BUTTON,
    TF_LBI_STYLE_BTN_MENU, TF_LBI_STYLE_SHOWNINTRAY, TF_LBMENUF_SEPARATOR,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyIcon, DestroyMenu, GetForegroundWindow, GetSystemMetrics,
    TrackPopupMenuEx, HICON, ICONINFO, MF_SEPARATOR, MF_STRING, SM_CXSMICON, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON,
};

use crate::ipc;
use crate::keys::Mode;
use crate::launch;

/// `ITfSource::AdviseSink` 固定回这个 cookie（小狼毫同款做法：一个条目只挂一个 sink）。
const SINK_COOKIE: u32 = 0x4242_4242;

/// 连接错误。这三枚在 `Win32_System_Ole` 模块里，为一组常量开一个 feature 不值，
/// 数值来自 olectl.h。
const CONNECT_E_ADVISELIMIT: i32 = 0x8004_0200u32 as i32;
const CONNECT_E_NOCONNECTION: i32 = 0x8004_0201u32 as i32;
const CONNECT_E_CANNOTCONNECT: i32 = 0x8004_0202u32 as i32;

/// 菜单项 id。`InitMenu`（ITfMenu 路径）与右键 `TrackPopupMenuEx` 共用一套。
const ID_SETTINGS: u32 = 1;
const ID_DATA_DIR: u32 = 2;

/// 按钮上显示的文字（`GetText`，也是信息结构里的描述）。
const ITEM_TEXT: &str = "亨利五笔";

#[implement(ITfLangBarItemButton, ITfSource)]
pub struct LangBarButton {
    /// 与 `Core.mode` 共享的中英模式。
    mode: Rc<Cell<Mode>>,
    /// 当前状态字（目前只有 HIDDEN 位会被置）。
    status: Cell<u32>,
    /// 语言栏管理器挂上来的更新 sink。
    sink: RefCell<Option<ITfLangBarItemSink>>,
    /// 管理器**曾经**挂上过 sink 吗。
    ///
    /// 只用来给「没有 sink」这条日志定级：构造期（`AddItem` 之前）本来就没有 sink，
    /// 那时刷图标是正常时序；而挂上之后又被摘掉才是真故障（通知石沉大海）。
    /// 两种情况 `sink` 都是 `None`，只有这个标志能把它们分开——否则日志里会出现
    /// 一堆永远可以忽略的 E，真出事时反而没人看。
    advised: Cell<bool>,
    /// 输入模式 compartment（`GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION`）。
    /// 切换模式时必须同步它：这是 Windows 用来判断「当前是中文还是英文」的
    /// 正式通道，不同步的话系统托盘指示器会按旧值重画，把我们的图标顶掉。
    compartment: RefCell<Option<ITfCompartment>>,
}

impl LangBarButton {
    pub fn new(mode: Rc<Cell<Mode>>) -> Self {
        Self {
            mode,
            status: Cell::new(0),
            sink: RefCell::new(None),
            advised: Cell::new(false),
            compartment: RefCell::new(None),
        }
    }

    /// 挂上输入模式 compartment（`Activate` 时从 thread mgr 取）。
    pub fn set_compartment(&self, compartment: ITfCompartment) {
        *self.compartment.borrow_mut() = Some(compartment);
    }
}

// ---------------------------------------------------------------------------
// 图标：每次 `GetIcon` 都交一个新句柄，最近若干个不销毁
// ---------------------------------------------------------------------------
//
// **Win10 的输入指示器按 HICON 句柄缓存位图。** 同一个句柄第二次交上去，它认为
// 「这个图标我已经有了」，于是不重画那一格。真机日志是干净的证据：
//
//   16:21:36.893  GetIcon(英文) → 0x589505e7   ← 新句柄，托盘跟着变
//   16:21:37.941  GetIcon(中文) → 0x3cdb02fb   ← 激活时用过的句柄，不动
//   16:21:39.120  GetIcon(英文) → 0x589505e7   ← 重复，不动
//
// v1.47 每次调用都新建句柄（顺带销毁旧的），中英切换一切正常；v1.48 为修「Alt+Tab
// 后图标空白」改成按模式缓存、句柄复用，正好踩中这个缓存——第一次切换能变、第二次
// 起就不动。所以正确做法是**每次都给新句柄**。
//
// 但句柄也不能立刻销毁：v1.47 在 `Deactivate`（对象 Drop）时就 `DestroyIcon`，
// 而系统仍拿着它绘制，界面直接变空白。折中是保留最近 `ICON_KEEP` 个不销毁
// （正在显示的一定是其中最新那个），更早的才释放。16×16 图标一个约 1~2KB。

/// 每种模式保留多少个历史句柄不销毁。
const ICON_KEEP: usize = 16;

/// 一种模式的图标槽：交出去过、还没释放的句柄（旧的在前）。
struct IconSlot {
    live: Mutex<Vec<isize>>,
}

impl IconSlot {
    const fn new() -> Self {
        Self {
            live: Mutex::new(Vec::new()),
        }
    }

    /// 画一个新图标交出去。`None` = 画不出来（`GetIcon` 要返 E_FAIL）。
    fn fresh(&self, mode: Mode) -> Option<HICON> {
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
        let icon = draw_icon(mode, size)?;
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.push(icon.0 as isize);
        while live.len() > ICON_KEEP {
            let old = live.remove(0);
            // 只释放「很久以前」的；最新的那些系统可能正在绘制，绝对不能碰。
            unsafe {
                let _ = DestroyIcon(HICON(old as *mut _));
            }
        }
        Some(icon)
    }
}

static ICON_ZH: IconSlot = IconSlot::new();
static ICON_EN: IconSlot = IconSlot::new();

/// 日志友好名（`Mode` 本身不带 Display）。
fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Chinese => "中文",
        Mode::English => "英文",
    }
}

/// 查「输入模式」那一格现在由谁占着——`GetItem(GUID_LBI_INPUTMODE)` 只认一条。
///
/// 真机上「图标不见了」有两种完全不同的成因：我们自己没画出来（`GetIcon` 有日志），
/// 或者那一格被别的条目（系统的输入模式条目、我们上一次的残留对象）顶掉了。
/// 只有这个查询能分辨，所以挂载时记一笔。
pub fn describe_guid_owner(mgr: &ITfLangBarItemMgr) -> String {
    unsafe {
        let Ok(item) = mgr.GetItem(&GUID_LBI_INPUTMODE) else {
            return "GetItem 失败（该 GUID 当前无条目）".to_string();
        };
        let mut info: TF_LANGBARITEMINFO = std::mem::zeroed();
        match item.GetInfo(&mut info) {
            Ok(()) => {
                let desc = String::from_utf16_lossy(
                    info.szDescription
                        .iter()
                        .take_while(|&&c| c != 0)
                        .copied()
                        .collect::<Vec<_>>()
                        .as_slice(),
                );
                let ours = info.clsidService == GUID::from_u128(heinrich_tip_id::TIP_CLSID);
                format!(
                    "clsid={:?} 描述={desc:?} {}",
                    info.clsidService,
                    if ours {
                        "（是我们的）"
                    } else {
                        "（不是我们的）"
                    }
                )
            }
            Err(e) => format!("GetInfo 失败：{e}"),
        }
    }
}

fn icon_for(mode: Mode) -> Result<HICON> {
    let slot = match mode {
        Mode::Chinese => &ICON_ZH,
        Mode::English => &ICON_EN,
    };
    let icon = slot.fresh(mode).ok_or_else(|| {
        windows::core::Error::from_hresult(windows::core::HRESULT(
            0x8000_4005u32 as i32, // E_FAIL
        ))
    })?;
    Ok(icon)
}

// ---------------------------------------------------------------------------
// `LangBarButton_Impl` 上的操作。`TextService` 与 COM 回调都从这儿进。
// ---------------------------------------------------------------------------

impl LangBarButton_Impl {
    /// 左键：切换全局中英模式并刷新图标。
    pub fn toggle_mode(&self) {
        let next = match self.mode.get() {
            Mode::Chinese => Mode::English,
            Mode::English => Mode::Chinese,
        };
        self.mode.set(next);
        // 与 Shift 单击同一条上报通道（全局模式，D34）。不 flush 缓冲：托盘点击
        // 时焦点不在输入框，见模块文档。
        ipc::set_mode(next == Mode::Chinese);
        tsf_info!(
            "语言栏按钮：切换为{}模式",
            if next == Mode::Chinese {
                "中文"
            } else {
                "英文"
            }
        );
        self.notify_icon();
    }

    /// **只报图标变化**（模式切换走这条）。
    ///
    /// `OnUpdate(dwflags)` 的 `dwflags` 是「哪些部分变了」，不是「重画整条」的开关。
    /// 早先一律带 `TF_LBI_STATUS`，日志里看得很清楚：每切一次模式，系统就来一遍
    /// `GetStatus → GetInfo（重建这一条）→ GetIcon`——状态没变却按整条重建处理，
    /// 托盘格里那条被拆了重搭。真机上「点第二下没反应 / 切回某个窗口图标就没了」
    /// 就出现在这一类重建的缝里。状态（显隐）真变时走 `notify()`。
    ///
    /// **顺序不能反**：先把中英模式写进系统 compartment，再 `OnUpdate`。
    /// 系统收到通知后会去读输入模式状态；如果此时 compartment 还是旧值，
    /// 它按旧模式画一遍，我们刚刷的新图标立刻被顶掉。
    pub fn notify_icon(&self) {
        self.sync_conversion();
        self.update(TF_LBI_ICON);
    }

    /// 状态与图标都可能变了（`Show` 引起显隐变化时走这条）。
    pub fn notify(&self) {
        self.sync_conversion();
        self.update(TF_LBI_STATUS | TF_LBI_ICON);
    }

    fn update(&self, flags: u32) {
        // 这两行是排障的关键分界：**有 sink 且 OnUpdate 成功**说明请求已发出，
        // 图标没变就是系统侧没重画；**没有 sink** 说明管理器把订阅摘了（我们这边
        // 的通知全部石沉大海）——两种情况修法完全不同，日志必须能分开。
        let sink = self.sink.borrow().clone();
        let Some(sink) = sink else {
            if self.advised.get() {
                tsf_error!("语言栏：sink 曾被挂上又被摘掉，{flags:#x} 通知发不出去");
            } else {
                tsf_debug!("语言栏：sink 尚未挂上，{flags:#x} 跳过（构造期，正常）");
            }
            return;
        };
        unsafe {
            match sink.OnUpdate(flags) {
                Ok(()) => tsf_debug!("语言栏：已通知 OnUpdate({flags:#x})"),
                Err(e) => tsf_error!("语言栏：通知管理器失败：{e}"),
            }
        }
    }

    /// 把当前模式写进 `GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION` 的
    /// `TF_CONVERSIONMODE_NATIVE` 位（中文 = 置位，英文 = 清位）。
    ///
    /// 这是小狼毫 `_UpdateLanguageBar` 里的同一动作。读-改-写是为了保留其他位
    /// （全角等将来可能用到的），读失败就按 0 起算。
    fn sync_conversion(&self) {
        // 先克隆出来再动，避免 `SetValue` 触发的系统回调重入本函数时借用冲突。
        let Some(compartment) = self.compartment.borrow().clone() else {
            return;
        };
        let native = self.mode.get() == Mode::Chinese;
        unsafe {
            let current = compartment
                .GetValue()
                .ok()
                .and_then(|v| i32::try_from(&v).ok())
                .unwrap_or(0);
            let bit = TF_CONVERSIONMODE_NATIVE as i32;
            let want = if native {
                current | bit
            } else {
                current & !bit
            };
            if want == current {
                return;
            }
            if let Err(e) = compartment.SetValue(0, &VARIANT::from(want)) {
                tsf_error!("语言栏：写输入模式 compartment 失败：{e}");
            }
        }
    }

    /// 显隐（`Show` 的实现体）。
    ///
    /// **`Show(false)` 不能当成「置 HIDDEN」来执行。** Win10 的托盘会发
    /// `Show(false)`（日志实证：`15:44:05`、`15:44:49` 各一次），但**从不补发
    /// `Show(true)`**——上位把隐藏位置上就等于永久隐身，真机上表现为「按几下
    /// Shift / Alt+Tab 之后图标没了，再也回不来」。
    ///
    /// 所以：`Show(true)` 照旧清隐藏位（幂等），`Show(false)` 只记日志不改状态，
    /// 图标保持常显。这条是有意与文档「按 fShow 更新状态」相反的选择——对方只
    /// 说了一半的话（hide 了不说 show），照办就是单向陷阱。
    pub fn set_shown(&self, fshow: bool) {
        if !fshow {
            tsf_info!("语言栏：Show(false) → 忽略（保持常显）");
            return;
        }
        let old = self.status.get();
        let status = old & !TF_LBI_STATUS_HIDDEN;
        if status != old {
            self.status.set(status);
            tsf_info!("语言栏：Show(true) → status={status:#x}");
            self.notify();
        }
    }

    /// 菜单/菜单选择的分发。
    fn run_action(&self, id: u32) {
        match id {
            ID_SETTINGS => {
                if let Err(e) = launch::open_settings() {
                    tsf_error!("语言栏菜单：打开设置失败：{e}");
                }
            }
            ID_DATA_DIR => self.open_data_dir(),
            _ => {}
        }
    }

    /// 用资源管理器打开 DLL 同目录的 `data`（词库所在）。打不开只记日志。
    fn open_data_dir(&self) {
        let Some(dir) = launch::module_dir().map(|d| d.join("data")) else {
            tsf_error!("语言栏菜单：取不到 DLL 目录，打不开词库文件夹");
            return;
        };
        if !dir.is_dir() {
            tsf_debug!("语言栏菜单：{} 不存在，不打开词库文件夹", dir.display());
            return;
        }
        let mut cmdline: Vec<u16> = format!("explorer.exe \"{}\"", dir.display())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let si = windows::Win32::System::Threading::STARTUPINFOW {
            cb: std::mem::size_of::<windows::Win32::System::Threading::STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut pi = windows::Win32::System::Threading::PROCESS_INFORMATION::default();
        unsafe {
            if let Err(e) = windows::Win32::System::Threading::CreateProcessW(
                None,
                Some(windows::core::PWSTR(cmdline.as_mut_ptr())),
                None,
                None,
                false,
                windows::Win32::System::Threading::PROCESS_CREATION_FLAGS(0),
                None,
                None,
                &si,
                &mut pi,
            ) {
                tsf_error!("语言栏菜单：打开 {} 失败：{e}", dir.display());
                return;
            }
            let _ = windows::Win32::Foundation::CloseHandle(pi.hProcess);
            let _ = windows::Win32::Foundation::CloseHandle(pi.hThread);
        }
        tsf_info!("语言栏菜单：已打开 {}", dir.display());
    }

    /// 右键菜单。`pt` 是屏幕坐标（语言栏管理器传进来的点击点）。
    fn show_menu(&self, pt: &POINT) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                tsf_error!("语言栏菜单：CreatePopupMenu 失败");
                return;
            };
            let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS as usize, w!("设置…"));
            if launch::module_dir()
                .map(|d| d.join("data").is_dir())
                .unwrap_or(false)
            {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
                let _ = AppendMenuW(menu, MF_STRING, ID_DATA_DIR as usize, w!("打开词库文件夹"));
            }
            // 菜单挂前台窗口名下；TPM_RETURNCMD 让选择结果直接作返回值回来，
            // 不走 WM_COMMAND。
            let hwnd = GetForegroundWindow();
            let cmd = TrackPopupMenuEx(
                menu,
                (TPM_NONOTIFY | TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
                pt.x,
                pt.y,
                hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
            if cmd.as_bool() {
                self.run_action(cmd.0 as u32);
            }
        }
    }
}

/// `GetInfo` 的实现体。拆成纯函数好测：CLSID/GUID/样式写错的表现全是
/// 「任务栏上那个图标长得不对或者干脆不出现」，真机上没法排查。
fn item_info() -> TF_LANGBARITEMINFO {
    let mut desc = [0u16; 32];
    for (i, ch) in ITEM_TEXT.encode_utf16().take(32).enumerate() {
        desc[i] = ch;
    }
    TF_LANGBARITEMINFO {
        clsidService: GUID::from_u128(heinrich_tip_id::TIP_CLSID),
        // 借用系统认得的「输入模式」条目 GUID：Win10 把它接到任务栏角标的
        // 显示逻辑上（小狼毫同款）。
        guidItem: GUID_LBI_INPUTMODE,
        dwStyle: TF_LBI_STYLE_BTN_BUTTON | TF_LBI_STYLE_BTN_MENU | TF_LBI_STYLE_SHOWNINTRAY,
        ulSort: 0,
        szDescription: desc,
    }
}

/// 画一枚模式图标：深灰圆角底 + 白字「亨 / 英」。
///
/// 没有资源文件可载（DLL 不引 `.rc`），运行时用 GDI 画是最省事的路。
/// `size` 取 `SM_CXSMICON`（系统小图标尺寸，已含 DPI 缩放）。
fn draw_icon(mode: Mode, size: i32) -> Option<HICON> {
    if size <= 0 {
        return None;
    }
    unsafe {
        let hdc_screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(hdc_screen));
        let color = CreateCompatibleBitmap(hdc_screen, size, size);
        let old_bmp: HGDIOBJ = SelectObject(mem, color.into());
        // 深灰底：与 heinrich.ico（语言列表图标）同一套观感，托盘和列表看起来是一个东西。
        let brush = CreateSolidBrush(COLORREF(0x0030_3030)); // BGR：深灰
        let old_brush: HGDIOBJ = SelectObject(mem, brush.into());
        let pen = CreatePen(PS_NULL, 0, COLORREF(0));
        let old_pen: HGDIOBJ = SelectObject(mem, pen.into());
        let r = (size * 22 / 100).max(1);
        let _ = RoundRect(mem, 0, 0, size, size, r, r);

        let font = CreateFontW(
            -(size * 68 / 100),
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32,
            w!("Microsoft YaHei"),
        );
        let old_font: HGDIOBJ = SelectObject(mem, font.into());
        let _ = SetBkMode(mem, TRANSPARENT);
        let _ = SetTextColor(mem, COLORREF(0x00FF_FFFF)); // 白字
        let text: Vec<u16> = if mode == Mode::Chinese { "亨" } else { "英" }
            .encode_utf16()
            .collect();
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(mem, &text, &mut sz);
        let _ = TextOutW(mem, (size - sz.cx) / 2, (size - sz.cy) / 2, &text);

        // 掩码全 0：色位图整体不透明。
        let mask_bits = vec![0u8; (size as usize * size as usize).div_ceil(8)];
        let mask = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr().cast()));
        let info = ICONINFO {
            fIcon: BOOL::from(true),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = windows::Win32::UI::WindowsAndMessaging::CreateIconIndirect(&info).ok();

        // CreateIconIndirect 会复制位图，原件必须清掉。
        SelectObject(mem, old_bmp);
        SelectObject(mem, old_brush);
        SelectObject(mem, old_pen);
        SelectObject(mem, old_font);
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(brush.into());
        let _ = DeleteObject(pen.into());
        let _ = DeleteObject(font.into());
        let _ = DeleteDC(mem);
        let _ = ReleaseDC(None, hdc_screen);
        icon
    }
}

// ---------------------------------------------------------------------------
// COM 接口实现
// ---------------------------------------------------------------------------

// 基类接口的 4 个方法与按钮的 5 个方法分两个 impl 块：windows-rs 把
// `ITfLangBarItemButton_Impl` 声明成 `ITfLangBarItem_Impl` 的子 trait，
// 两者都必须实现。

impl ITfLangBarItem_Impl for LangBarButton_Impl {
    fn GetInfo(&self, pinfo: *mut TF_LANGBARITEMINFO) -> Result<()> {
        tsf_debug!("语言栏：GetInfo（系统在重建这一条）");
        unsafe {
            *pinfo = item_info();
        }
        Ok(())
    }

    fn GetStatus(&self) -> Result<u32> {
        let status = self.status.get();
        tsf_debug!("语言栏：GetStatus → {status:#x}");
        Ok(status)
    }

    fn Show(&self, fshow: BOOL) -> Result<()> {
        self.set_shown(fshow.as_bool());
        Ok(())
    }

    fn GetTooltipString(&self) -> Result<BSTR> {
        Ok(BSTR::from("亨利五笔 · 左键切换中英，右键打开菜单"))
    }
}

impl ITfLangBarItemButton_Impl for LangBarButton_Impl {
    fn OnClick(&self, click: TfLBIClick, pt: &POINT, _prcarea: *const RECT) -> Result<()> {
        // 记下每一次点击：真机上「点一下没反应」与「系统重复发点击」在日志里
        // 长得完全一样（两次切换），只有这个计数能分辨。
        tsf_info!(
            "语言栏：OnClick click={} 当前={}",
            match click {
                TF_LBI_CLK_LEFT => "左",
                TF_LBI_CLK_RIGHT => "右",
                _ => "其它",
            },
            mode_name(self.mode.get())
        );
        if click == TF_LBI_CLK_LEFT {
            self.toggle_mode();
        } else if click == TF_LBI_CLK_RIGHT {
            self.show_menu(pt);
        }
        Ok(())
    }

    fn InitMenu(&self, pmenu: Ref<ITfMenu>) -> Result<()> {
        let Ok(menu) = pmenu.ok() else {
            return Ok(());
        };
        let settings: Vec<u16> = "设置…".encode_utf16().collect();
        let data: Vec<u16> = "打开词库文件夹".encode_utf16().collect();
        unsafe {
            menu.AddMenuItem(
                ID_SETTINGS,
                0,
                HBITMAP::default(),
                HBITMAP::default(),
                &settings,
                std::ptr::null_mut(),
            )?;
            menu.AddMenuItem(
                0,
                TF_LBMENUF_SEPARATOR,
                HBITMAP::default(),
                HBITMAP::default(),
                &[],
                std::ptr::null_mut(),
            )?;
            menu.AddMenuItem(
                ID_DATA_DIR,
                0,
                HBITMAP::default(),
                HBITMAP::default(),
                &data,
                std::ptr::null_mut(),
            )?;
        }
        Ok(())
    }

    fn OnMenuSelect(&self, wid: u32) -> Result<()> {
        self.run_action(wid);
        Ok(())
    }

    fn GetIcon(&self) -> Result<HICON> {
        let mode = self.mode.get();
        let icon = icon_for(mode);
        match &icon {
            Ok(h) => tsf_debug!("语言栏：GetIcon({}) → {:#x}", mode_name(mode), h.0 as isize),
            Err(e) => tsf_error!("语言栏：GetIcon({}) 失败：{e}", mode_name(mode)),
        }
        icon
    }

    fn GetText(&self) -> Result<BSTR> {
        Ok(BSTR::from(ITEM_TEXT))
    }
}

/// 连接类错误统一从这里出（HRESULT 值来自 olectl.h）。
fn connect_err(code: i32) -> windows::core::Error {
    windows::core::Error::from_hresult(windows::core::HRESULT(code))
}

impl ITfSource_Impl for LangBarButton_Impl {
    fn AdviseSink(&self, riid: *const GUID, punk: Ref<IUnknown>) -> Result<u32> {
        unsafe {
            if *riid != ITfLangBarItemSink::IID {
                return Err(connect_err(CONNECT_E_CANNOTCONNECT));
            }
        }
        let Ok(unk) = punk.ok() else {
            return Err(connect_err(CONNECT_E_CANNOTCONNECT));
        };
        let sink: ITfLangBarItemSink = unk.cast()?;
        let mut slot = self.sink.borrow_mut();
        if slot.is_some() {
            return Err(connect_err(CONNECT_E_ADVISELIMIT));
        }
        *slot = Some(sink);
        self.advised.set(true);
        Ok(SINK_COOKIE)
    }

    fn UnadviseSink(&self, dwcookie: u32) -> Result<()> {
        if dwcookie != SINK_COOKIE {
            return Err(connect_err(CONNECT_E_NOCONNECTION));
        }
        if self.sink.borrow_mut().take().is_none() {
            return Err(connect_err(CONNECT_E_NOCONNECTION));
        }
        self.advised.set(false);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 自测
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::{ComObject, Interface};
    use windows::Win32::UI::TextServices::{
        ITfInputProcessorProfileMgr, ITfLangBarItem, ITfLangBarItemMgr, TF_INPUTPROCESSORPROFILE,
        TF_PROFILETYPE_INPUTPROCESSOR,
    };

    /// 信息结构里装的是我们的 CLSID + 系统的输入模式 GUID + 按钮样式。
    /// 任何一项写错，任务栏上都不出现图标且无任何报错。
    #[test]
    fn item_info_carries_our_identity() {
        let info = item_info();
        assert_eq!(
            info.clsidService,
            GUID::from_u128(heinrich_tip_id::TIP_CLSID)
        );
        assert_eq!(info.guidItem, GUID_LBI_INPUTMODE);
        assert_eq!(
            info.dwStyle,
            TF_LBI_STYLE_BTN_BUTTON | TF_LBI_STYLE_BTN_MENU | TF_LBI_STYLE_SHOWNINTRAY
        );
        let desc = String::from_utf16_lossy(
            info.szDescription
                .iter()
                .take_while(|&&c| c != 0)
                .copied()
                .collect::<Vec<_>>()
                .as_slice(),
        );
        assert_eq!(desc, ITEM_TEXT);
    }

    /// sink 的挂/摘协议：错的 iid 拒收、重复挂限一次、cookie 校验、摘了就不再通知。
    #[test]
    fn sink_advise_unadvise_protocol() {
        let obj = ComObject::new(LangBarButton::new(Rc::new(Cell::new(Mode::Chinese))));
        let src: ITfSource = obj.to_interface();

        let sink_obj = ComObject::new(RecordingSink::default());
        let sink_unk: IUnknown = sink_obj.to_interface();

        // 错的 iid → CONNECT_E_CANNOTCONNECT。
        let bogus = GUID::from_u128(0x1234_5678_9abc_def0_1122_3344_5566_7788);
        assert!(
            unsafe { src.AdviseSink(&bogus, &sink_unk) }.is_err(),
            "错误的 iid 必须被拒绝"
        );

        // 正确挂上：cookie 固定。
        let cookie = unsafe {
            src.AdviseSink(&ITfLangBarItemSink::IID, &sink_unk)
                .expect("挂 sink")
        };
        assert_eq!(cookie, SINK_COOKIE);
        assert!(
            obj.advised.get(),
            "挂上之后必须记住，否则丢 sink 会被当成构造期"
        );

        // 再挂一个 → 限一次。
        let sink_obj2 = ComObject::new(RecordingSink::default());
        let sink_unk2: IUnknown = sink_obj2.to_interface();
        assert!(unsafe { src.AdviseSink(&ITfLangBarItemSink::IID, &sink_unk2) }.is_err());

        // 模式切换 → sink 只收到 ICON。**不能带 STATUS**：状态位没变，
        // 带上去系统会整条重建托盘格（日志里表现为 GetStatus→GetInfo→GetIcon）。
        obj.toggle_mode();
        assert_eq!(obj.mode.get(), Mode::English);
        assert_eq!(sink_obj.updates.get(), TF_LBI_ICON);

        // `Show(false)` 是单向陷阱：Win10 会发 hide、从不补 show，照办就永久隐身。
        // 所以它必须**不改变状态、不发通知**。
        sink_obj.updates.set(0);
        obj.set_shown(false);
        assert_eq!(sink_obj.updates.get(), 0, "Show(false) 不该改状态");
        assert_eq!(obj.status.get() & TF_LBI_STATUS_HIDDEN, 0, "不该置隐藏位");

        // `Show(true)` 只清隐藏位（本就干净 → 什么都不发）。
        obj.set_shown(true);
        assert_eq!(sink_obj.updates.get(), 0);

        // 错 cookie 摘不掉；对 cookie 摘掉后不再通知。
        assert!(unsafe { src.UnadviseSink(0) }.is_err());
        assert!(unsafe { src.UnadviseSink(SINK_COOKIE) }.is_ok());
        assert!(!obj.advised.get(), "摘除后要复位，重挂是另一次生命周期");
        sink_obj.updates.set(0);
        obj.toggle_mode();
        assert_eq!(obj.mode.get(), Mode::Chinese);
        assert_eq!(sink_obj.updates.get(), 0, "摘除后不应再收到通知");
    }

    /// 图标：两种模式都能画出来、句柄有效，且第二次取到的是同一个句柄
    /// 图标：两种模式都画得出来，且**每次调用都必须是新句柄**。
    ///
    /// 这是这条路上最容易「优化错」的地方：Win10 输入指示器按句柄缓存位图，句柄
    /// 一复用它就认为图标没变、不再重画那一格（真机表现：第一次切换能变，第二次
    /// 起就不动）。所以这里断言的是「不复用」，不是「缓存命中」。
    #[test]
    fn every_get_icon_call_hands_out_a_fresh_handle() {
        let zh1 = icon_for(Mode::Chinese).expect("GDI 画图标应成功");
        let zh2 = icon_for(Mode::Chinese).expect("GDI 画图标应成功");
        let en1 = icon_for(Mode::English).expect("GDI 画图标应成功");
        assert!(!zh1.is_invalid() && !zh2.is_invalid() && !en1.is_invalid());
        assert_ne!(zh1.0, zh2.0, "同一模式连续两次也必须给新句柄");
        assert_ne!(zh1.0, en1.0, "两种模式不能共用一张图");

        // 池子有上限：老的会被释放，不会随切换次数无限涨。
        for _ in 0..(ICON_KEEP * 3) {
            let _ = icon_for(Mode::Chinese);
        }
        let live = ICON_ZH.live.lock().unwrap().len();
        assert!(
            live <= ICON_KEEP,
            "存活句柄应收敛到 {ICON_KEEP}，实际 {live}"
        );
    }

    /// 菜单 id 与动作的对应关系钉住：设置=1、词库文件夹=2。
    #[test]
    fn menu_ids_are_stable() {
        assert_eq!(ID_SETTINGS, 1);
        assert_eq!(ID_DATA_DIR, 2);
        assert_ne!(ID_SETTINGS, ID_DATA_DIR);
    }

    // -----------------------------------------------------------------------
    // 端到端：走 TSF 的正规激活路径（ActivateProfile）→ 枚举语言栏条目 →
    // 恢复原 profile → 断言条目被摘掉。
    //
    // 两个绕不过去的事实：
    // - 按键 sink 与语言栏都只认 **TSF 亲自激活**的 TIP——测试进程里手动
    //   CoCreateInstance + Activate 会被 `AdviseKeyEventSink` 以 E_INVALIDARG
    //   拒收（TSF 只给「经 profile 激活」的 client 发牌）。
    // - TSF 按注册表实例化 TIP——所以这里测的是 **dist 里的 DLL**。
    //   跑之前必须先 `tools\build.ps1`（或手动拷贝 target 的 DLL 进 dist），
    //   否则测的是旧版。
    //
    // ActivateProfile 的三个坑（逐个试出来的）：
    // - dwflags 必须带 TF_IPP_FLAG_ACTIVE(1)，传 0 恒 E_INVALIDARG——
    //   无论 TIP 还是普通键盘布局、hkl 给不给都一样。
    // - hkl 可以传 0（GetProfile 查回来的也是 0x0），不影响成败。
    // - 恢复原 profile：clsid 为零说明原来是普通键盘布局，要用
    //   TF_PROFILETYPE_KEYBOARDLAYOUT 类型 + 同款 ACTIVE 标志恢复；
    //   按 INPUTPROCESSOR 恢复零 clsid 也是 E_INVALIDARG。
    //
    // 激活会短暂把本测试线程的输入法切到亨利五笔，结束前恢复原 profile，
    // 不影响全局默认输入法。
    //
    // 手动执行：tools\build.ps1 之后再
    //   cargo test -p heinrich-win -- --ignored --nocapture langbar
    // -----------------------------------------------------------------------

    /// 枚举当前语言栏条目，找 `clsidService == 我们的 CLSID` 的那个。
    unsafe fn find_our_item(
        mgr: &ITfLangBarItemMgr,
    ) -> windows::core::Result<Option<(TF_LANGBARITEMINFO, ITfLangBarItem)>> {
        use windows::Win32::UI::TextServices::ITfLangBarItem;

        let enum_items = mgr.EnumItems()?;
        let mut slot: [Option<ITfLangBarItem>; 1] = [None];
        let mut fetched = 0u32;
        while enum_items.Next(&mut slot, &mut fetched).is_ok() && fetched == 1 {
            let item = slot[0].take().expect("fetched==1 时槽位必有值");
            let mut info = TF_LANGBARITEMINFO::default();
            item.GetInfo(&mut info)?;
            if info.clsidService == GUID::from_u128(heinrich_tip_id::TIP_CLSID) {
                return Ok(Some((info, item)));
            }
        }
        Ok(None)
    }

    /// 恢复激活的原 profile。两类 profile 的 dwflags 都要带
    /// TF_IPP_FLAG_ACTIVE(1)，传 0 恒 E_INVALIDARG；clsid 为零说明原来是
    /// 普通键盘布局，要用 KEYBOARDLAYOUT 类型恢复。
    unsafe fn profiles_restore(mgr: &ITfInputProcessorProfileMgr, prev: &TF_INPUTPROCESSORPROFILE) {
        use windows::Win32::UI::TextServices::TF_PROFILETYPE_KEYBOARDLAYOUT;
        let is_tip = prev.clsid != GUID::zeroed();
        let r = mgr.ActivateProfile(
            if is_tip {
                TF_PROFILETYPE_INPUTPROCESSOR
            } else {
                TF_PROFILETYPE_KEYBOARDLAYOUT
            },
            prev.langid,
            &prev.clsid,
            &prev.guidProfile,
            prev.hkl,
            1, // TF_IPP_FLAG_ACTIVE
        );
        if let Err(e) = r {
            println!("恢复原 profile 失败（测试线程销毁后自动失效）：{e}");
        }
    }

    #[test]
    #[ignore = "端到端：走 TSF profile 激活路径（会短暂切换本线程输入法），手动执行"]
    fn langbar_item_survives_activate_and_deactivate() {
        use windows::Win32::System::Com::{
            CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
        };
        use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
        use windows::Win32::UI::TextServices::{
            CLSID_TF_InputProcessorProfiles, CLSID_TF_ThreadMgr, ITfInputProcessorProfileMgr,
            ITfLangBarItemButton, ITfLangBarItemMgr, GUID_TFCAT_TIP_KEYBOARD,
            TF_INPUTPROCESSORPROFILE, TF_PROFILETYPE_INPUTPROCESSOR,
        };

        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            // 已经初始化过（别的测试）也算成功：S_FALSE = 1。
            assert!(hr.is_ok() || hr.0 == 1, "CoInitializeEx hr={:#x}", hr.0);

            let tm: windows::Win32::UI::TextServices::ITfThreadMgr =
                windows::Win32::System::Com::CoCreateInstance(
                    &CLSID_TF_ThreadMgr,
                    None,
                    CLSCTX_INPROC_SERVER,
                )
                .expect("创建 ITfThreadMgr");
            tm.Activate().expect("ThreadMgr::Activate");

            // 记住当前激活的键盘 profile（测完恢复）。
            let profile_mgr: ITfInputProcessorProfileMgr =
                windows::Win32::System::Com::CoCreateInstance(
                    &CLSID_TF_InputProcessorProfiles,
                    None,
                    CLSCTX_INPROC_SERVER,
                )
                .expect("创建 ITfInputProcessorProfileMgr");
            let mut prev = TF_INPUTPROCESSORPROFILE::default();
            let have_prev = profile_mgr
                .GetActiveProfile(&GUID_TFCAT_TIP_KEYBOARD, &mut prev)
                .is_ok();
            println!(
                "原 profile：clsid={:?} langid={:#06x}",
                prev.clsid, prev.langid
            );

            // 激活亨利五笔：TSF 会自己 CoCreateInstance + ActivateEx。
            // dwflags 必须带 TF_IPP_FLAG_ACTIVE(1)——传 0 恒 E_INVALIDARG；
            // hkl 传 0 即可（GetProfile 查回来的也是 0x0）。
            let mut ours = TF_INPUTPROCESSORPROFILE::default();
            profile_mgr
                .GetProfile(
                    TF_PROFILETYPE_INPUTPROCESSOR,
                    heinrich_tip_id::LANGID_ZH_CN,
                    &GUID::from_u128(heinrich_tip_id::TIP_CLSID),
                    &GUID::from_u128(heinrich_tip_id::PROFILE_ZH_CN),
                    HKL::default(),
                    &mut ours,
                )
                .expect("GetProfile(亨利五笔)");
            println!("我们的 profile：hkl={:#x}", ours.hkl.0 as usize);
            // dwflags：TF_IPP_FLAG_ACTIVE(1)。传 0 一直 E_INVALIDARG；
            // TSF 官方样例（InputProcessorProfiles）激活时带 ACTIVE 标志。
            profile_mgr
                .ActivateProfile(
                    TF_PROFILETYPE_INPUTPROCESSOR,
                    heinrich_tip_id::LANGID_ZH_CN,
                    &GUID::from_u128(heinrich_tip_id::TIP_CLSID),
                    &GUID::from_u128(heinrich_tip_id::PROFILE_ZH_CN),
                    ours.hkl,
                    1, // TF_IPP_FLAG_ACTIVE
                )
                .expect("ActivateProfile(亨利五笔)");

            let mgr: ITfLangBarItemMgr = tm.cast().expect("ThreadMgr QI ITfLangBarItemMgr");

            // 激活后：条目在，信息正确，图标与文字都能取到。
            let (info, item) = find_our_item(&mgr)
                .expect("枚举")
                .expect("ActivateProfile 后应能枚举到语言栏条目");
            assert_eq!(info.guidItem, GUID_LBI_INPUTMODE);
            let desc = String::from_utf16_lossy(
                info.szDescription
                    .iter()
                    .take_while(|&&c| c != 0)
                    .copied()
                    .collect::<Vec<_>>()
                    .as_slice(),
            );
            assert_eq!(desc, ITEM_TEXT);

            let button: ITfLangBarItemButton = item.cast().expect("条目 QI ITfLangBarItemButton");
            let text = button.GetText().expect("GetText");
            assert_eq!(text.to_string(), ITEM_TEXT);
            let icon = button.GetIcon().expect("GetIcon");
            assert!(!icon.is_invalid(), "图标句柄有效");
            println!("条目：描述={desc} 文字={text} 图标有效");

            // 恢复原 profile：TSF 随之 Deactivate 我们的 TIP → RemoveItem。
            // 普通键盘布局（clsid 为零）也照恢复——profiles_restore 内部区分类型。
            if have_prev {
                profiles_restore(&profile_mgr, &prev);
            }
            let gone = find_our_item(&mgr).expect("再次枚举");
            assert!(gone.is_none(), "Deactivate 后语言栏条目应已摘除");

            let _ = tm.Deactivate();
            println!("端到端：挂载 → 枚举 → 图标/文字 → 摘除，全部通过");
        }
    }
}

/// 记录 OnUpdate 收到过的标志，验证通知链路。
#[derive(Default)]
#[implement(ITfLangBarItemSink)]
struct RecordingSink {
    updates: Cell<u32>,
}

impl windows::Win32::UI::TextServices::ITfLangBarItemSink_Impl for RecordingSink_Impl {
    fn OnUpdate(&self, dwflags: u32) -> Result<()> {
        self.updates.set(dwflags);
        Ok(())
    }
}
