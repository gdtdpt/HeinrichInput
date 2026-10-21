#!/usr/bin/env python3
"""按窗口类名从**屏幕**上抓一块图，写成 PNG。纯 stdlib，无需安装任何东西。

为什么不用 `PrintWindow`：候选窗是 `WS_EX_LAYERED` + `UpdateLayeredWindow` 画的，
内容在 DWM 的合成层里而不在窗口自己的绘制路径上，`PrintWindow` 拿到的往往是一张
空图（`win-ui-selfscreenshot` 里那套办法对普通窗口有效，对分层窗口不一定）。
从屏幕 DC 做 `BitBlt` 拿到的是**用户实际看到的样子**，反而更接近验收要看的东西。

用法：
    python tools/shot-window.py <窗口类名> <输出.png> [--margin 8]

窗口必须可见且没被别的窗口压住（预览器的候选窗是 TOPMOST，一般没问题）。
"""

import ctypes
import ctypes.wintypes as wt
import struct
import sys
import zlib

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)

# python.exe 默认是 DPI 不感知的：系统会把它的坐标与 BitBlt 一起按缩放比虚拟化，
# 200% 屏上抓到的图会变成「一半大小、再放大」，字全是糊的。必须先声明感知，
# 而且要在创建任何 DC/窗口之前。
try:
    user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))  # PerMonitorV2
except Exception:
    try:
        ctypes.WinDLL("shcore").SetProcessDpiAwareness(2)
    except Exception:
        pass

SRCCOPY = 0x00CC0020
CAPTUREBLT = 0x40000000
DIB_RGB_COLORS = 0


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wt.DWORD),
        ("biWidth", wt.LONG),
        ("biHeight", wt.LONG),
        ("biPlanes", wt.WORD),
        ("biBitCount", wt.WORD),
        ("biCompression", wt.DWORD),
        ("biSizeImage", wt.DWORD),
        ("biXPelsPerMeter", wt.LONG),
        ("biYPelsPerMeter", wt.LONG),
        ("biClrUsed", wt.DWORD),
        ("biClrImportant", wt.DWORD),
    ]


class BITMAPINFO(ctypes.Structure):
    _fields_ = [("bmiHeader", BITMAPINFOHEADER), ("bmiColors", wt.DWORD * 3)]


def find_window(class_name):
    hwnd = user32.FindWindowW(class_name, None)
    if not hwnd:
        raise SystemExit(f"没找到类名为 {class_name} 的窗口（程序起了吗？）")
    return hwnd


def window_rect(hwnd):
    r = wt.RECT()
    if not user32.GetWindowRect(hwnd, ctypes.byref(r)):
        raise SystemExit("GetWindowRect 失败")
    return r.left, r.top, r.right, r.bottom


def grab(x, y, w, h):
    screen = user32.GetDC(None)
    mem = gdi32.CreateCompatibleDC(screen)
    bmi = BITMAPINFO()
    bmi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    bmi.bmiHeader.biWidth = w
    # 负高度 = 自上而下，省得再翻一次行序
    bmi.bmiHeader.biHeight = -h
    bmi.bmiHeader.biPlanes = 1
    bmi.bmiHeader.biBitCount = 32
    bmi.bmiHeader.biCompression = 0
    bits = ctypes.c_void_p()
    bmp = gdi32.CreateDIBSection(
        mem, ctypes.byref(bmi), DIB_RGB_COLORS, ctypes.byref(bits), None, 0
    )
    old = gdi32.SelectObject(mem, bmp)
    # CAPTUREBLT：把分层窗口也算进 BitBlt，少了它可能抓到下一层的内容
    gdi32.BitBlt(mem, 0, 0, w, h, screen, x, y, SRCCOPY | CAPTUREBLT)
    buf = ctypes.string_at(bits, w * h * 4)
    gdi32.SelectObject(mem, old)
    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(mem)
    user32.ReleaseDC(None, screen)
    return buf


def write_png(path, w, h, bgra):
    """BGRA（自上而下）→ RGBA PNG。"""
    raw = bytearray()
    for row in range(h):
        raw.append(0)  # 行过滤器：none
        start = row * w * 4
        line = bgra[start : start + w * 4]
        px = bytearray(len(line))
        px[0::4] = line[2::4]  # R ← B
        px[1::4] = line[1::4]  # G
        px[2::4] = line[0::4]  # B ← R
        px[3::4] = b"\xff" * w  # 屏幕上抓到的像素本来就是合成后的，alpha 满
        raw += px

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)

    def chunk(tag, data):
        body = tag + data
        return (
            struct.pack(">I", len(data))
            + body
            + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)
        )

    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )
    with open(path, "wb") as f:
        f.write(png)


def zoom(bgra, w, h, factor):
    """最近邻放大：看 100% DPI 下字形的像素级细节时比原图清楚。"""
    if factor <= 1:
        return bgra, w, h
    out = bytearray()
    for row in range(h):
        line = bgra[row * w * 4 : (row + 1) * w * 4]
        wide = bytearray()
        for i in range(w):
            wide += line[i * 4 : i * 4 + 4] * factor
        for _ in range(factor):
            out += wide
    return bytes(out), w * factor, h * factor


def main(argv):
    if len(argv) < 3:
        raise SystemExit(__doc__)
    class_name, out = argv[1], argv[2]
    margin = 8
    if "--margin" in argv:
        margin = int(argv[argv.index("--margin") + 1])

    hwnd = find_window(class_name)
    left, top, right, bottom = window_rect(hwnd)
    x, y = left - margin, top - margin
    w, h = right - left + margin * 2, bottom - top + margin * 2
    factor = 1
    if "--zoom" in argv:
        factor = max(1, int(argv[argv.index("--zoom") + 1]))
    print(f"窗口 {left},{top} {right - left}×{bottom - top}；抓取 {x},{y} {w}×{h}")
    buf, w, h = zoom(grab(x, y, w, h), w, h, factor)
    write_png(out, w, h, buf)
    print(f"已写入 {out}（{w}×{h}）")


if __name__ == "__main__":
    main(sys.argv)
