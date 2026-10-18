"""M2 视频链路运行验证（不修改项目代码，只操作窗口 + 截屏）。

用法：
  python scripts/verify_m2_video.py shot <out.png>      截主窗口（含叠加窗口区域）
  python scripts/verify_m2_video.py click <x> <y>       点击（坐标 = 截图里的像素坐标）
  python scripts/verify_m2_video.py dblclick <x> <y>    双击（用于确认播放列表曲目）
  python scripts/verify_m2_video.py overlays            列出叠加窗口及其屏幕矩形
  python scripts/verify_m2_video.py color <png> <x> <y> <w> <h>   统计区域内彩色像素比例

坐标约定：x/y 直接量截图（`shot` 产出的 PNG）里的像素位置，脚本自动换算为客户区物理坐标。
"""

import ctypes
import json
import os
import sys
import time
from ctypes import wintypes

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32
user32.SetProcessDPIAware()

MAIN_TITLE = "Simple Media Player"
OVERLAY_CLASS = "SimpleMediaPlayerVideoOverlay"

WM_MOUSEMOVE = 0x0200
WM_LBUTTONDOWN = 0x0201
WM_LBUTTONUP = 0x0202
MK_LBUTTON = 0x0001
SW_RESTORE = 9


def find_main():
    hwnd = user32.FindWindowW(None, MAIN_TITLE)
    if not hwnd:
        print("主窗口未找到，请先启动应用", file=sys.stderr)
        sys.exit(2)
    return hwnd


def enum_overlays():
    found = []

    @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    def cb(hwnd, _):
        buf = ctypes.create_unicode_buffer(256)
        user32.GetClassNameW(hwnd, buf, 256)
        if buf.value == OVERLAY_CLASS:
            r = wintypes.RECT()
            user32.GetWindowRect(hwnd, ctypes.byref(r))
            found.append(
                {
                    "hwnd": hwnd,
                    "visible": bool(user32.IsWindowVisible(hwnd)),
                    "rect": [r.left, r.top, r.right - r.left, r.bottom - r.top],
                }
            )
        return True

    user32.EnumWindows(cb, 0)
    return found


def client_to_screen(hwnd, x, y):
    pt = wintypes.POINT(int(x), int(y))
    user32.ClientToScreen(hwnd, ctypes.byref(pt))
    return pt.x, pt.y


def client_offset(hwnd):
    """窗口截图坐标 → 客户区坐标的偏移（边框 + 标题栏，物理像素）。"""
    wr = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(wr))
    pt = wintypes.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(pt))
    return pt.x - wr.left, pt.y - wr.top


def cmd_click(x, y, double=False):
    hwnd = find_main()
    place_window(hwnd)
    user32.ShowWindow(hwnd, SW_RESTORE)
    user32.SetForegroundWindow(hwnd)
    time.sleep(0.3)
    hwnd = find_main()
    ox, oy = client_offset(hwnd)
    cx, cy = int(x) - ox, int(y) - oy  # 截图像素坐标 → 客户区坐标
    lp = ((cy & 0xFFFF) << 16) | (cx & 0xFFFF)
    for _ in range(2 if double else 1):
        user32.PostMessageW(hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lp)
        time.sleep(0.03)
        user32.PostMessageW(hwnd, WM_LBUTTONUP, 0, lp)
        time.sleep(0.06)
    sx, sy = client_to_screen(hwnd, cx, cy)
    print(f"{'dblclick' if double else 'click'} shot=({x},{y}) client=({cx},{cy}) screen=({sx},{sy})")


def cmd_drag(x1, y1, x2, y2, steps=8):
    hwnd = find_main()
    place_window(hwnd)
    time.sleep(0.3)
    ox, oy = client_offset(hwnd)

    def lp_of(x, y):
        return (((int(y) - oy) & 0xFFFF) << 16) | ((int(x) - ox) & 0xFFFF)

    user32.PostMessageW(hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lp_of(x1, y1))
    for i in range(1, steps + 1):
        x = x1 + (x2 - x1) * i / steps
        y = y1 + (y2 - y1) * i / steps
        user32.PostMessageW(hwnd, WM_MOUSEMOVE, MK_LBUTTON, lp_of(x, y))
        time.sleep(0.02)
    user32.PostMessageW(hwnd, WM_LBUTTONUP, 0, lp_of(x2, y2))
    print(f"drag ({x1},{y1}) -> ({x2},{y2})")


SWP_NOMOVE = 0x0002
SWP_NOSIZE = 0x0001
SWP_NOZORDER = 0x0004
SWP_NOACTIVATE = 0x0010
HWND_TOPMOST = -1
HWND_NOTOPMOST = -2


def place_window(hwnd, x=0, y=0):
    """把窗口移到固定位置（不改尺寸、不激活），让截图像素坐标与窗口坐标一一对应。"""
    user32.SetWindowPos(hwnd, 0, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)


user32.SetWindowPos.argtypes = [
    wintypes.HWND,
    wintypes.HWND,
    ctypes.c_int,
    ctypes.c_int,
    ctypes.c_int,
    ctypes.c_int,
    wintypes.UINT,
]
user32.SetWindowPos.restype = wintypes.BOOL


def set_topmost(hwnd, on):
    ok = user32.SetWindowPos(
        hwnd,
        wintypes.HWND(HWND_TOPMOST if on else HWND_NOTOPMOST),
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
    )
    if not ok:
        print(f"SetWindowPos(topmost={on}) 失败 hwnd={hwnd} err={ctypes.windll.kernel32.GetLastError()}")


def cmd_shot(out, place=True):
    from PIL import ImageGrab

    hwnd = find_main()
    if place:
        place_window(hwnd)
    # 主窗口与叠加窗口一起置顶，避免被其他应用遮挡（截完立即恢复）
    user32.ShowWindow(hwnd, SW_RESTORE)
    set_topmost(hwnd, True)
    for ov in enum_overlays():
        set_topmost(ov["hwnd"], True)
    time.sleep(0.6)

    r = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(r))
    bbox = (r.left, r.top, r.right, r.bottom)
    img = ImageGrab.grab(bbox=bbox, all_screens=True)

    for ov in enum_overlays():
        set_topmost(ov["hwnd"], False)
    set_topmost(hwnd, False)
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    img.save(out)

    info = {
        "window": {"rect": list(bbox), "size": [img.width, img.height]},
        "overlays": enum_overlays(),
        "saved": os.path.abspath(out),
    }
    print(json.dumps(info, ensure_ascii=False, indent=2))


def cmd_overlays():
    print(json.dumps(enum_overlays(), ensure_ascii=False, indent=2))


def cmd_color(path, x, y, w, h):
    from PIL import Image

    img = Image.open(path).convert("RGB").crop((x, y, x + w, y + h))
    px = list(img.resize((64, 36)).getdata())
    colorful = sum(1 for r, g, b in px if max(r, g, b) - min(r, g, b) > 40)
    dark = sum(1 for r, g, b in px if max(r, g, b) < 24)
    n = len(px)
    print(
        f"区域=({x},{y},{w},{h}) 彩色={colorful / n:.2%} 近黑={dark / n:.2%} "
        f"均值={tuple(round(sum(c[i] for c in px) / n) for i in range(3))}"
    )


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    cmd = sys.argv[1]
    if cmd == "shot":
        cmd_shot(sys.argv[2] if len(sys.argv) > 2 else "docs/screenshots/m2.png")
    elif cmd == "shotn":  # 不重定位窗口（全屏等场景）
        cmd_shot(sys.argv[2], place=False)
    elif cmd == "click":
        cmd_click(int(sys.argv[2]), int(sys.argv[3]))
    elif cmd == "dblclick":
        cmd_click(int(sys.argv[2]), int(sys.argv[3]), double=True)
    elif cmd == "drag":
        cmd_drag(*map(int, sys.argv[2:6]))
    elif cmd == "overlays":
        cmd_overlays()
    elif cmd == "color":
        cmd_color(sys.argv[2], *map(int, sys.argv[3:7]))
    else:
        print(__doc__)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
