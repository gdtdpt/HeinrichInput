# 截「任务栏通知区」并放大，用来肉眼判断那一格是谁在画。
# 用法：python tray_shot.py out.png [放大倍数]
# 纯 stdlib（ctypes + zlib 手写 PNG），零依赖。
import ctypes
import struct
import sys
import zlib
from ctypes import wintypes

u32 = ctypes.windll.user32
g32 = ctypes.windll.gdi32
u32.SetProcessDPIAware()

OUT = sys.argv[1] if len(sys.argv) > 1 else "tray.png"
SCALE = int(sys.argv[2]) if len(sys.argv) > 2 else 4

u32.FindWindowExW.restype = ctypes.c_void_p
u32.FindWindowW.restype = ctypes.c_void_p

tray = u32.FindWindowW("Shell_TrayWnd", None)
notify = u32.FindWindowExW(ctypes.c_void_p(tray), None, "TrayNotifyWnd", None)
print("Shell_TrayWnd=0x%X TrayNotifyWnd=0x%X" % (tray or 0, notify or 0))
target = notify or tray
if not target:
    sys.exit("任务栏没找到")

rect = wintypes.RECT()
u32.GetWindowRect(ctypes.c_void_p(target), ctypes.byref(rect))
w, h = rect.right - rect.left, rect.bottom - rect.top
print("rect=(%d,%d)-(%d,%d) size=%dx%d" % (rect.left, rect.top, rect.right, rect.bottom, w, h))

# 从屏幕 DC 直接抓（DWM 合成的任务栏 PrintWindow 常是空白）
screen = u32.GetDC(None)
mdc = g32.CreateCompatibleDC(screen)
bmp = g32.CreateCompatibleBitmap(screen, w, h)
g32.SelectObject(mdc, bmp)
SRCCOPY = 0x00CC0020
g32.BitBlt(mdc, 0, 0, w, h, screen, rect.left, rect.top, SRCCOPY)


class BMIH(ctypes.Structure):
    _fields_ = [("biSize", wintypes.DWORD), ("biWidth", wintypes.LONG), ("biHeight", wintypes.LONG),
                ("biPlanes", wintypes.WORD), ("biBitCount", wintypes.WORD), ("biCompression", wintypes.DWORD),
                ("biSizeImage", wintypes.DWORD), ("biXPelsPerMeter", wintypes.LONG),
                ("biYPelsPerMeter", wintypes.LONG), ("biClrUsed", wintypes.DWORD),
                ("biClrImportant", wintypes.DWORD)]


bi = BMIH()
bi.biSize = ctypes.sizeof(BMIH)
bi.biWidth = w
bi.biHeight = -h  # 负数 = 自上而下
bi.biPlanes = 1
bi.biBitCount = 32
bi.biCompression = 0
buf = ctypes.create_string_buffer(w * h * 4)
got = g32.GetDIBits(mdc, bmp, 0, h, buf, ctypes.byref(bi), 0)
print("GetDIBits lines=%d" % got)

g32.DeleteObject(bmp)
g32.DeleteDC(mdc)
u32.ReleaseDC(None, screen)

# BGRA -> RGB，并按 SCALE 最近邻放大
data = buf.raw
W, H = w * SCALE, h * SCALE
raw = bytearray()
for y in range(H):
    sy = y // SCALE
    row = data[sy * w * 4:(sy + 1) * w * 4]
    line = bytearray(3 * W)
    for x in range(W):
        sx = x // SCALE
        i = sx * 4
        line[x * 3] = row[i + 2]
        line[x * 3 + 1] = row[i + 1]
        line[x * 3 + 2] = row[i]
    raw.append(0)
    raw += line


def chunk(tag, payload):
    return (struct.pack(">I", len(payload)) + tag + payload
            + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF))


png = (b"\x89PNG\r\n\x1a\n"
       + chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, 8, 2, 0, 0, 0))
       + chunk(b"IDAT", zlib.compress(bytes(raw), 6))
       + chunk(b"IEND", b""))
open(OUT, "wb").write(png)
print("written %s (%dx%d, %d bytes)" % (OUT, W, H, len(png)))
