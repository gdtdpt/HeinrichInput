"""验证 ITfFnConfigure 全链路：CoCreateInstance -> GetDisplayName -> Show。

用法：python out/probe_fnconfigure_show.py [--no-show]

为什么要这个脚本：系统设置页的「选项」按钮走的就是这条路。能在探针进程里
复现它，就不必每改一版都请人去点设置页——真机确认留给最后一步。
"""

import ctypes
import subprocess
import sys
import time
import uuid

CLSID = "{0EB96F60-FFEC-5F48-871F-EA75A989F546}"
IID_FN_CONFIGURE = "{88f567c6-1757-49f8-a1b2-89234c1eeff9}"
LANGID_ZH_CN = 0x0804
PROFILE = "{06CF8277-49D8-5317-9410-7EEAA491A936}"


class GUID(ctypes.Structure):
    _fields_ = [("Data1", ctypes.c_ulong), ("Data2", ctypes.c_ushort),
                ("Data3", ctypes.c_ushort), ("Data4", ctypes.c_ubyte * 8)]

    @classmethod
    def make(cls, s):
        b = uuid.UUID(s).bytes_le
        g = cls()
        g.Data1 = int.from_bytes(b[0:4], "little")
        g.Data2 = int.from_bytes(b[4:6], "little")
        g.Data3 = int.from_bytes(b[6:8], "little")
        for i in range(8):
            g.Data4[i] = b[8 + i]
        return g


class Vtbl(ctypes.Structure):
    _fields_ = [("QueryInterface", ctypes.c_void_p), ("AddRef", ctypes.c_void_p),
                ("Release", ctypes.c_void_p),
                ("GetDisplayName", ctypes.c_void_p),   # ITfFunction
                ("Show", ctypes.c_void_p)]             # ITfFnConfigure


class Obj(ctypes.Structure):
    _fields_ = [("vtbl", ctypes.POINTER(Vtbl))]


ole32 = ctypes.WinDLL("ole32")
ole32.CoInitializeEx.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
ole32.CoCreateInstance.argtypes = [ctypes.POINTER(GUID), ctypes.c_void_p, ctypes.c_ulong,
                                   ctypes.POINTER(GUID), ctypes.POINTER(ctypes.c_void_p)]
ole32.CoCreateInstance.restype = ctypes.c_long


user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.FindWindowW.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p]
user32.FindWindowW.restype = ctypes.c_void_p
user32.GetWindowThreadProcessId.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]

WINDOW_CLASS = "HeinrichSettings"


def settings_hwnd():
    """设置窗口是否存在。

    不看进程名：探针跑在 Bash 工具的 job 里，父进程一退出整棵进程树会被收掉，
    `tasklist` 的结论会变成「窗口没了」，而我们要验的是窗口本身。
    """
    hwnd = user32.FindWindowW(WINDOW_CLASS, None)
    return hwnd or 0


def settings_pid():
    hwnd = settings_hwnd()
    if not hwnd:
        return 0
    pid = ctypes.c_ulong()
    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    return pid.value


def wait_window(timeout=5.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if settings_hwnd():
            return True
        time.sleep(0.1)
    return False


def main():
    do_show = "--no-show" not in sys.argv
    ole32.CoInitializeEx(None, 0x2)

    clsid = GUID.make(CLSID)
    iid = GUID.make(IID_FN_CONFIGURE)
    ptr = ctypes.c_void_p()
    hr = ole32.CoCreateInstance(ctypes.byref(clsid), None, 1, ctypes.byref(iid), ctypes.byref(ptr))
    print("CoCreateInstance(ITfFnConfigure) hr=0x%08X" % (hr & 0xFFFFFFFF))
    if hr != 0:
        print("FAIL: 系统拿不到这个接口，设置页就不会画「选项」按钮")
        return 1

    obj = ctypes.cast(ptr, ctypes.POINTER(Obj)).contents

    # ITfFunction::GetDisplayName(BSTR*)
    name_fn = ctypes.WINFUNCTYPE(ctypes.c_long, ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p))(
        obj.vtbl.contents.GetDisplayName)
    bstr = ctypes.c_void_p()
    hr = name_fn(ptr, ctypes.byref(bstr))
    text = ctypes.wstring_at(bstr.value) if bstr.value else "<null>"
    print("GetDisplayName        hr=0x%08X  显示名=%r" % (hr & 0xFFFFFFFF, text))
    if bstr.value:
        ctypes.WinDLL("oleaut32").SysFreeString(ctypes.c_void_p(bstr.value))

    if not do_show:
        return 0

    print("调用 Show 前窗口是否存在：%s" % ("是" if settings_hwnd() else "否"))

    # ITfFnConfigure::Show(HWND, LANGID, REFGUID)
    show_fn = ctypes.WINFUNCTYPE(ctypes.c_long, ctypes.c_void_p, ctypes.c_void_p,
                                 ctypes.c_ushort, ctypes.POINTER(GUID))(obj.vtbl.contents.Show)
    profile = GUID.make(PROFILE)
    t0 = time.time()
    hr = show_fn(ptr, None, LANGID_ZH_CN, ctypes.byref(profile))
    cost = (time.time() - t0) * 1000
    print("Show #1               hr=0x%08X  耗时 %.1f ms（必须立刻返回，不能等窗口关闭）"
          % (hr & 0xFFFFFFFF, cost))
    if hr != 0:
        return 1
    if not wait_window():
        print("FAIL: 5 秒内没看到窗口（类名 %s）" % WINDOW_CLASS)
        return 1
    pid1 = settings_pid()
    print("OK: 窗口出现，pid=%d" % pid1)

    # 第二次 Show 必须复用同一个窗口：连点两次「选项」不该开出两个设置页，
    # 那样两边各存各的，后保存的会把先保存的盖掉。
    hr = show_fn(ptr, None, LANGID_ZH_CN, ctypes.byref(profile))
    time.sleep(0.6)
    pid2 = settings_pid()
    print("Show #2               hr=0x%08X  pid=%d（%s）"
          % (hr & 0xFFFFFFFF, pid2, "同一进程，已复用" if pid2 == pid1 else "开了第二个！"))
    return 0 if pid2 == pid1 and pid2 != 0 else 1


if __name__ == "__main__":
    sys.exit(main())
