"""探针：验证 mpv --wid 能否把视频渲染进一个 popup 窗口（owned by GPUI 窗口）。

不修改项目代码，只做可行性判定：
  1. 找到 Simple Media Player 主窗口
  2. 创建一个 WS_POPUP 叠加窗口（owner = 主窗口），定位在视频区位置
  3. mpv --wid=<该窗口> 播放 lavfi testsrc
  4. 截屏该区域，判断是否出现彩色视频帧
"""

import ctypes
import os
import subprocess
import sys
import time
from ctypes import wintypes

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32
user32.SetProcessDPIAware()

MPV = r"C:\ProgramData\chocolatey\lib\mpvio.install\tools\mpv.exe"

WNDPROC = ctypes.WINFUNCTYPE(
    ctypes.c_ssize_t, wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM
)


class WNDCLASSW(ctypes.Structure):
    _fields_ = [
        ("style", wintypes.UINT),
        ("lpfnWndProc", WNDPROC),
        ("cbClsExtra", ctypes.c_int),
        ("cbWndExtra", ctypes.c_int),
        ("hInstance", wintypes.HINSTANCE),
        ("hIcon", wintypes.HICON),
        ("hCursor", wintypes.HANDLE),
        ("hbrBackground", wintypes.HBRUSH),
        ("lpszMenuName", wintypes.LPCWSTR),
        ("lpszClassName", wintypes.LPCWSTR),
    ]


def proc(hwnd, msg, wparam, lparam):
    return user32.DefWindowProcW(hwnd, msg, wparam, lparam)


_proc_ref = WNDPROC(proc)


def main():
    app = user32.FindWindowW(None, "Simple Media Player")
    if not app:
        print("主窗口未找到，请先启动应用")
        return 1

    rect = wintypes.RECT()
    user32.GetWindowRect(app, ctypes.byref(rect))
    print(f"主窗口: {rect.left},{rect.top} {rect.right - rect.left}x{rect.bottom - rect.top}")

    # 视频区：主窗口内右侧面板（避开标题栏与底部控制栏）
    vx, vy = rect.left + 380, rect.top + 90
    vw, vh = 640, 360

    hinst = ctypes.windll.kernel32.GetModuleHandleW(None)
    cls = "SmpProbeOverlay"
    wc = WNDCLASSW()
    wc.style = 0
    wc.lpfnWndProc = _proc_ref
    wc.hInstance = hinst
    wc.lpszClassName = cls
    if not user32.RegisterClassW(ctypes.byref(wc)):
        err = ctypes.windll.kernel32.GetLastError()
        if err != 1410:  # class already exists
            print("RegisterClassW 失败:", err)
            return 1

    WS_POPUP = 0x80000000
    WS_VISIBLE = 0x10000000
    WS_EX_TOOLWINDOW = 0x00000080
    WS_EX_NOACTIVATE = 0x08000000

    hwnd = user32.CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        cls,
        "SmpProbeOverlay",
        WS_POPUP | WS_VISIBLE,
        vx,
        vy,
        vw,
        vh,
        app,  # owner
        None,
        hinst,
        None,
    )
    print("overlay hwnd:", hwnd)
    if not hwnd:
        print("CreateWindowExW 失败:", ctypes.windll.kernel32.GetLastError())
        return 1

    user32.ShowWindow(hwnd, 5)  # SW_SHOW
    time.sleep(0.3)

    cmd = [
        MPV,
        f"--wid={hwnd}",
        "--no-config",
        "--no-osc",
        "--osd-level=0",
        "--no-input-default-bindings",
        "--really-quiet",
        "--force-window=yes",
        "--hwdec=no",
        "av://lavfi:testsrc=size=640x360:rate=30",
    ]
    print("spawn mpv ...")
    p = subprocess.Popen(cmd)
    time.sleep(5)

    from PIL import Image, ImageGrab

    img = ImageGrab.grab(bbox=(vx, vy, vx + vw, vy + vh), all_screens=True)
    out = r"C:\Users\Kevin Zhang\workspace\simple-media-player\docs\screenshots\probe-mpv-wid.png"
    os.makedirs(os.path.dirname(out), exist_ok=True)
    img.save(out)

    # 判定：非灰阶像素比例（testsrc 有彩条）
    px = list(img.convert("RGB").resize((64, 36)).getdata())
    colorful = sum(1 for r, g, b in px if max(r, g, b) - min(r, g, b) > 40)
    print(f"彩色像素比例: {colorful / len(px):.2%}  -> ", "视频可见 ✅" if colorful / len(px) > 0.05 else "全黑/未渲染 ❌")
    print("截图:", out)

    p.terminate()
    try:
        p.wait(timeout=5)
    except Exception:
        p.kill()
    user32.DestroyWindow(hwnd)
    return 0


if __name__ == "__main__":
    sys.exit(main())
