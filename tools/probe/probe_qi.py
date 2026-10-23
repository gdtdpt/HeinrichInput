"""对任意 CLSID 试 QI 一组 TSF 接口，用来看「系统设置页能拿到什么」。"""
import ctypes, sys, uuid

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

ole32 = ctypes.WinDLL("ole32")
ole32.CoInitializeEx.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
ole32.CoInitializeEx.restype = ctypes.c_long
ole32.CoCreateInstance.argtypes = [ctypes.POINTER(GUID), ctypes.c_void_p, ctypes.c_ulong,
                                   ctypes.POINTER(GUID), ctypes.POINTER(ctypes.c_void_p)]
ole32.CoCreateInstance.restype = ctypes.c_long
ole32.CoInitializeEx(None, 0x2)

IIDS = [
    ("ITfFnConfigure",   "88f567c6-1757-49f8-a1b2-89234c1eeff9"),
    ("ITfFunctionProvider","101d34ec-6d9f-4fcb-837f-bb04e59f5b2c"),
    ("ITfTextInputProcessorEx","6095c1bc-7f5f-4c02-8e5c-2f7a9e5e6b06"),
    ("ITfInputProcessorProfileMgr","71c6e74c-0f28-11d8-a82a-00065b84435c"),
    ("ITfLangBarItemButton","a3b8e9b4-3f1e-4b6b-9f7b-0d0e9a8a3c1b"),
    ("ITfFnSearchCandidateProvider","a1c9e1f1-4bd4-4d1d-a7f3-01e9c3ffc8ef"),
]

def try_clsid(label, clsid, iid_name, iid):
    c = GUID.make(clsid); i = GUID.make(iid); p = ctypes.c_void_p()
    hr = ole32.CoCreateInstance(ctypes.byref(c), None, 1, ctypes.byref(i), ctypes.byref(p)) & 0xFFFFFFFF
    if p.value:
        # Release
        vtbl = ctypes.cast(p, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))).contents
        rel = ctypes.WINFUNCTYPE(ctypes.c_ulong, ctypes.c_void_p)(vtbl[2])
        rel(p)
    return "%s %-28s %s" % (label, iid_name, "S_OK" if hr == 0 else "0x%08X" % hr)

targets = [
    ("微软五笔  ", "{6a498709-e00b-4c45-a018-8f9e4081ae40}"),
    ("微软拼音  ", "{81d4e9c9-1d3b-41bc-9e6c-4b40bf79e35e}"),
    ("微软注音  ", "{B115690A-EA02-48D5-A231-E3578D2FDF80}"),
    ("亨利五笔  ", "{0EB96F60-FFEC-5F48-871F-EA75A989F546}"),
]
for label, clsid in targets:
    print("---- %s %s ----" % (label, clsid))
    for name, iid in IIDS[:3]:
        print("  " + try_clsid(label, clsid, name, iid))
