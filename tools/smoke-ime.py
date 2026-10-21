#!/usr/bin/env python3
"""把一串按键打进指定进程的前台窗口 —— 给注入型输入法（TSF）做冒烟用。

为什么需要它：TSF 输入法只在**别人的输入框**里工作，验收时要么人工敲、要么
没法复现。这个脚本用 `SendInput` 把按键送进一个**指定的** pid 的顶层窗口
（送之前会确认前台确实是它，抢不到就退出，绝不往未知窗口里打字），
配合 `tools/shot-window.py` 就能自己看候选窗的位置、箭头与命中效果。

只用标准库（ctypes），不需要装任何东西。

用法（PowerShell / cmd 均可）：

    # 1) 开一个记事本，拿到 pid
    $np = Start-Process notepad -PassThru

    # 2) 打 vbg、空格、vbg、2、vbg、右方向键（每个参数是一段，段间有间隔）
    python tools\\smoke-ime.py --pid $np.Id vbg "{SPACE}" vbg "2" vbg "{RIGHT}"

    # 3) 抓候选窗（类名固定为 HeinrichCandidateWindowV1）
    python tools\\shot-window.py HeinrichCandidateWindowV1 dist\\cand-live.png --margin 16

参数里 `{NAME}` 是功能键，其余字符按字面敲（只支持 a-z 0-9 与空格：
中文输入法的验收无非这几种，其余字符请用 `{NAME}`）。
"""

import argparse
import ctypes
import sys
import time
from ctypes import wintypes

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

INPUT_KEYBOARD = 1
KEYEVENTF_KEYUP = 0x0002
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
SW_RESTORE = 9

VK = {
    "SPACE": 0x20,
    "ENTER": 0x0D,
    "ESC": 0x1B,
    "BACKSPACE": 0x08,
    "TAB": 0x09,
    "LEFT": 0x25,
    "UP": 0x26,
    "RIGHT": 0x27,
    "DOWN": 0x28,
    "PGUP": 0x21,
    "PGDN": 0x22,
    "HOME": 0x24,
    "END": 0x23,
    "DELETE": 0x2E,
    "SHIFT": 0x10,
    "CTRL": 0x11,
    "ALT": 0x12,
    # 单发没意义，专门给 `press_combo` 用（例如 WIN+SPACE 切输入法）。
    "WIN": 0x5B,
}


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", wintypes.WORD),
        ("wScan", wintypes.WORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.c_size_t),
    ]


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [
        ("dx", wintypes.LONG),
        ("dy", wintypes.LONG),
        ("mouseData", wintypes.DWORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.c_size_t),
    ]


class HARDWAREINPUT(ctypes.Structure):
    _fields_ = [
        ("uMsg", wintypes.DWORD),
        ("wParamL", wintypes.WORD),
        ("wParamH", wintypes.WORD),
    ]


class _INPUTUNION(ctypes.Union):
    # 三个成员都要写出来：`SendInput` 校验的 `cbSize` 是**整个 INPUT 结构**的大小，
    # x64 上由 MOUSEINPUT（32 字节）决定 ⇒ 40。只写 KEYBDINPUT 会得到 32，
    # 于是每次调用都以 `ERROR_INVALID_PARAMETER (87)` 失败。
    _fields_ = [("ki", KEYBDINPUT), ("mi", MOUSEINPUT), ("hi", HARDWAREINPUT)]


class INPUT(ctypes.Structure):
    _fields_ = [("type", wintypes.DWORD), ("u", _INPUTUNION)]


user32.SendInput.argtypes = (wintypes.UINT, ctypes.POINTER(INPUT), ctypes.c_int)
user32.SendInput.restype = wintypes.UINT


def window_of_pid(pid: int):
    """找到该 pid 的第一个可见顶层窗口。"""
    found = []

    @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    def cb(hwnd, _lparam):
        owner = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value == pid and user32.IsWindowVisible(hwnd):
            # 跳过空标题的中间层窗口（记事本也有一个）。
            length = user32.GetWindowTextLengthW(hwnd)
            if length > 0:
                found.append(hwnd)
                return False
        return True

    user32.EnumWindows(cb, 0)
    return found[0] if found else None


def foreground_pid() -> int:
    hwnd = user32.GetForegroundWindow()
    pid = wintypes.DWORD()
    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    return pid.value


def focus(pid: int, hwnd) -> bool:
    """把目标窗口拉到前台，并确认**前台真的是它**。

    只信 `SetForegroundWindow` 的返回值是不够的：Windows 会因为它「不是你
    的进程」而静默失败。所以轮询 `GetForegroundWindow` 的 pid，最多等 1.5 s。
    """
    if user32.IsIconic(hwnd):
        user32.ShowWindow(hwnd, SW_RESTORE)
    user32.SetForegroundWindow(hwnd)
    for _ in range(15):
        if foreground_pid() == pid:
            return True
        time.sleep(0.1)
    return False


def _send(vk: int, up: bool) -> None:
    scan = user32.MapVirtualKeyW(vk, 0)
    flags = KEYEVENTF_KEYUP if up else 0
    inp = INPUT(INPUT_KEYBOARD, _INPUTUNION(ki=KEYBDINPUT(vk, scan, flags, 0, 0)))
    if user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT)) != 1:
        raise OSError(ctypes.get_last_error(), "SendInput 失败")


def press(vk: int) -> None:
    _send(vk, False)
    time.sleep(0.02)
    _send(vk, True)


def press_combo(mods, vk: int) -> None:
    """按住一组修饰键再敲一下 `vk`（如 `Win+Space` 切输入法）。松开顺序反过来。"""
    for m in mods:
        _send(m, False)
        time.sleep(0.02)
    time.sleep(0.05)
    press(vk)
    for m in reversed(mods):
        _send(m, True)
        time.sleep(0.02)


def click_at(x: float, y: float) -> None:
    """在屏幕坐标 (x, y) 上点一下左键。

    候选窗是 `WS_EX_NOACTIVATE`，点它不会抢前台——这正是要点验的地方：
    点了之后输入框还得有焦点，组合串还得在。
    """
    if not user32.SetCursorPos(int(x), int(y)):
        raise OSError(ctypes.get_last_error(), "SetCursorPos 失败")
    time.sleep(0.05)
    for flags in (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP):
        inp = INPUT(0, _INPUTUNION(mi=MOUSEINPUT(0, 0, 0, flags, 0, 0)))
        if user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT)) != 1:
            raise OSError(ctypes.get_last_error(), "SendInput(mouse) 失败")
        time.sleep(0.04)


def vk_of(token: str) -> int:
    if token.startswith("{") and token.endswith("}"):
        name = token[1:-1].upper()
        if name not in VK:
            raise SystemExit(f"不认识的功能键 {token}（可用：{', '.join(sorted(VK))}）")
        return VK[name]
    if len(token) == 1:
        ch = token.lower()
        if "a" <= ch <= "z":
            return ord(ch.upper())
        if "0" <= ch <= "9":
            return ord(ch)
        if ch == " ":
            return VK["SPACE"]
    raise SystemExit(f"不支持的按键 {token!r}：只支持 a-z 0-9 空格与 {{NAME}}")


def main() -> int:
    ap = argparse.ArgumentParser(description="把按键打进指定进程（TSF 冒烟用）")
    ap.add_argument("--pid", type=int, required=True, help="目标进程 pid")
    ap.add_argument(
        "--gap",
        type=float,
        default=0.12,
        help="每段之间的间隔（秒），默认 0.12。慢机器可给 0.3",
    )
    ap.add_argument(
        "segments",
        nargs="+",
        help='按键段，每段整体发出。例：vbg "{SPACE}" vbg "2"',
    )
    args = ap.parse_args()

    hwnd = window_of_pid(args.pid)
    if not hwnd:
        print(f"❌ pid {args.pid} 没有可见的顶层窗口", file=sys.stderr)
        return 2
    if not focus(args.pid, hwnd):
        print(
            f"❌ 抢不到前台（当前前台 pid={foreground_pid()}，目标 {args.pid}）。"
            "先手动点一下目标窗口再跑。",
            file=sys.stderr,
        )
        return 3

    print(f"目标 pid={args.pid} hwnd={hwnd:#x}")
    for seg in args.segments:
        keys = seg if not seg.startswith("{") else seg
        # 一段里可能连写多个字符（vbg），逐个按。功能键整体当一个。
        if seg.startswith("{") and seg.endswith("}"):
            press(vk_of(seg))
            print(f"  {seg}")
        else:
            for ch in keys:
                press(vk_of(ch))
            print(f"  {keys!r}")
        time.sleep(args.gap)
    print("已发送完毕")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
