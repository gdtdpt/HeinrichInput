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
//!   `OnUpdate(TF_LBI_STATUS | TF_LBI_ICON)` 让系统重取图标。
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
use std::sync::atomic::{AtomicI32, AtomicIsize, Ordering};

use windows::core::{implement, w, IUnknown, Interface, Ref, Result, BOOL, BSTR, GUID};
use windows::Win32::Foundation::{COLORREF, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, GetDC, GetTextExtentPoint32W, ReleaseDC, RoundRect,
    SelectObject, SetBkMode, SetTextColor, TextOutW, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, FW_BOLD, HBITMAP, HGDIOBJ, OUT_DEFAULT_PRECIS, PS_NULL,
    TRANSPARENT,
};
use windows::Win32::UI::TextServices::{
    ITfLangBarItemButton, ITfLangBarItemButton_Impl, ITfLangBarItemSink, ITfLangBarItem_Impl,
    ITfMenu, ITfSource, ITfSource_Impl, TfLBIClick, GUID_LBI_INPUTMODE, TF_LANGBARITEMINFO,
    TF_LBI_CLK_LEFT, TF_LBI_CLK_RIGHT, TF_LBI_ICON, TF_LBI_STATUS, TF_LBI_STATUS_HIDDEN,
    TF_LBI_STYLE_BTN_BUTTON, TF_LBI_STYLE_BTN_MENU, TF_LBI_STYLE_SHOWNINTRAY, TF_LBMENUF_SEPARATOR,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetForegroundWindow, GetSystemMetrics,
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
}

impl LangBarButton {
    pub fn new(mode: Rc<Cell<Mode>>) -> Self {
        Self {
            mode,
            status: Cell::new(0),
            sink: RefCell::new(None),
        }
    }
}

// ---------------------------------------------------------------------------
// 图标缓存：进程级、永不销毁
// ---------------------------------------------------------------------------
//
// **句柄的生命周期不能跟着按钮对象走。** 语言栏拿到 `GetIcon` 的 HICON 之后
// 在什么时候绘制、绘制几次都不由我们决定；`Deactivate` 时按钮对象被丢掉、
// 若顺手 `DestroyIcon`，界面上的图标就变成空白——真机上表现为「Alt+Tab 切走
// 再切回，图标没了」与「切中英后图标不再出现」。小狼毫用
// `LoadImage(..., LR_SHARED)` 拿到同样效果：句柄活到进程退出。
//
// 缓存键带尺寸：DPI 变了（宿主进程 PerMonitorV2）就重画一个，旧的故意不销毁。

/// 一种模式的图标槽：上次画的尺寸 + 句柄。
struct IconSlot {
    size: AtomicI32,
    handle: AtomicIsize,
}

impl IconSlot {
    const fn new() -> Self {
        Self {
            size: AtomicI32::new(0),
            handle: AtomicIsize::new(0),
        }
    }

    /// 取（必要时先画）该模式的图标。`None` = 画不出来（`GetIcon` 返 E_FAIL）。
    fn get(&self, mode: Mode) -> Option<HICON> {
        let want = unsafe { GetSystemMetrics(SM_CXSMICON) };
        let have = self.size.load(Ordering::Acquire);
        let cached = self.handle.load(Ordering::Acquire);
        if cached != 0 && have == want {
            return Some(HICON(cached as *mut _));
        }
        let icon = draw_icon(mode, want)?;
        // 两个线程同时首次调用时会各画一个：后写的留下，另一个泄漏一次
        // （16×16 图标，几百字节）。换成加锁不值得——GetIcon 在 UI 线程上。
        self.size.store(want, Ordering::Release);
        self.handle.store(icon.0 as isize, Ordering::Release);
        Some(icon)
    }
}

static ICON_ZH: IconSlot = IconSlot::new();
static ICON_EN: IconSlot = IconSlot::new();

fn icon_for(mode: Mode) -> Result<HICON> {
    let slot = match mode {
        Mode::Chinese => &ICON_ZH,
        Mode::English => &ICON_EN,
    };
    let icon = slot.get(mode).ok_or_else(|| {
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
        self.notify();
    }

    /// 状态变了（模式切换、显隐），让语言栏管理器重取图标与状态。
    pub fn notify(&self) {
        if let Some(sink) = &*self.sink.borrow() {
            unsafe {
                let _ = sink.OnUpdate(TF_LBI_STATUS | TF_LBI_ICON);
            }
        }
    }

    /// 显隐（`Show` 的实现体）。`fshow = true` 表示系统要它显示。
    pub fn set_shown(&self, fshow: bool) {
        let old = self.status.get();
        let status = if fshow {
            old & !TF_LBI_STATUS_HIDDEN
        } else {
            old | TF_LBI_STATUS_HIDDEN
        };
        if status != old {
            self.status.set(status);
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
        unsafe {
            *pinfo = item_info();
        }
        Ok(())
    }

    fn GetStatus(&self) -> Result<u32> {
        Ok(self.status.get())
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
        icon_for(self.mode.get())
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
        Ok(SINK_COOKIE)
    }

    fn UnadviseSink(&self, dwcookie: u32) -> Result<()> {
        if dwcookie != SINK_COOKIE {
            return Err(connect_err(CONNECT_E_NOCONNECTION));
        }
        if self.sink.borrow_mut().take().is_none() {
            return Err(connect_err(CONNECT_E_NOCONNECTION));
        }
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

        // 再挂一个 → 限一次。
        let sink_obj2 = ComObject::new(RecordingSink::default());
        let sink_unk2: IUnknown = sink_obj2.to_interface();
        assert!(unsafe { src.AdviseSink(&ITfLangBarItemSink::IID, &sink_unk2) }.is_err());

        // 模式切换 → sink 收到 STATUS|ICON。
        obj.toggle_mode();
        assert_eq!(obj.mode.get(), Mode::English);
        assert_eq!(sink_obj.updates.get(), TF_LBI_STATUS | TF_LBI_ICON);

        // 错 cookie 摘不掉；对 cookie 摘掉后不再通知。
        assert!(unsafe { src.UnadviseSink(0) }.is_err());
        assert!(unsafe { src.UnadviseSink(SINK_COOKIE) }.is_ok());
        sink_obj.updates.set(0);
        obj.toggle_mode();
        assert_eq!(obj.mode.get(), Mode::Chinese);
        assert_eq!(sink_obj.updates.get(), 0, "摘除后不应再收到通知");
    }

    /// 图标：两种模式都能画出来、句柄有效，且第二次取到的是同一个句柄
    /// （进程级缓存）。**故意不销毁**——句柄可能还在语言栏手里。
    #[test]
    fn draws_icons_for_both_modes() {
        for mode in [Mode::Chinese, Mode::English] {
            let first = icon_for(mode).expect("GDI 画图标应成功");
            assert!(!first.is_invalid());
            let again = icon_for(mode).expect("缓存命中");
            assert_eq!(first.0, again.0, "同一模式的图标应复用同一个句柄");
        }
        let zh = icon_for(Mode::Chinese).unwrap();
        let en = icon_for(Mode::English).unwrap();
        assert_ne!(zh.0, en.0, "两种模式不能共用一张图");
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
