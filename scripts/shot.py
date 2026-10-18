"""启动应用 → 置顶定位 → 截客户区。用于对照设计稿做视觉验收。

用法：python scripts/shot.py <输出png> [--dark] [--w 1040] [--h 680]
                              [--click "x,y"] [--wait 2.0] [--keep]

--w/--h 是**逻辑**尺寸（与设计稿 1280×800 同一坐标系）；脚本按屏幕缩放比换算成
物理像素，截完再缩回逻辑尺寸，这样高 DPI 下也是 1:1 的清晰图。
"""

import ctypes
import os
import subprocess
import sys
import time
from ctypes import wintypes
from pathlib import Path

from PIL import Image, ImageGrab

# 必须在任何窗口/截图 API 之前声明 DPI 感知，否则 GetClientRect 给的是
# 虚拟化坐标（96dpi），截出来的图只有窗口左上角一块。
try:
    ctypes.windll.shcore.SetProcessDpiAwareness(2)  # PER_MONITOR_AWARE_V2
except Exception:  # noqa: BLE001 - 老系统没有 shcore
    ctypes.windll.user32.SetProcessDPIAware()

HWND_TOPMOST = -1
SWP_SHOWWINDOW = 0x0040

root = Path(__file__).resolve().parent.parent
# 默认拍 debug 构建；验发布包时用 SMP_EXE=target/release/simple-media-player.exe
exe = Path(os.environ.get("SMP_EXE") or root / "target" / "debug" / "simple-media-player.exe")
if not exe.is_absolute():
    exe = root / exe

args = sys.argv[1:]
out = Path(args[0]) if args and not args[0].startswith("--") else root / "docs/screenshots/chroma-light.png"
dark = "--dark" in args
keep = "--keep" in args
width = int(args[args.index("--w") + 1]) if "--w" in args else 1040
height = int(args[args.index("--h") + 1]) if "--h" in args else 680
wait = float(args[args.index("--wait") + 1]) if "--wait" in args else 1.5
click = None
if "--click" in args:
    click = [int(v) for v in args[args.index("--click") + 1].split(",")]

user32 = ctypes.windll.user32
user32.SetWindowPos.argtypes = [
    wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int,
    ctypes.c_int, ctypes.c_int, ctypes.c_uint,
]
user32.GetClientRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user32.ClientToScreen.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.POINT)]
user32.GetDpiForWindow.argtypes = [wintypes.HWND]
user32.GetWindowLongW.argtypes = [wintypes.HWND, ctypes.c_int]
user32.GetWindowLongW.restype = ctypes.c_long
try:
    user32.AdjustWindowRectExForDpi.argtypes = [
        ctypes.POINTER(wintypes.RECT), ctypes.c_uint32, wintypes.BOOL,
        ctypes.c_uint32, ctypes.c_uint32,
    ]
    HAS_ADJUST_DPI = True
except AttributeError:  # Win10 1607 之前没有这个 API
    HAS_ADJUST_DPI = False

cmd = [str(exe)] + (["--dark"] if dark else [])
log = root / "target" / "shot-stderr.log"
err = open(log, "wb")
proc = subprocess.Popen(cmd, stdout=err, stderr=subprocess.STDOUT)
time.sleep(3.2)

hwnd = user32.FindWindowW(None, "Chroma")
if not hwnd:
    proc.kill()
    raise SystemExit("未找到标题为 Chroma 的窗口")

scale = user32.GetDpiForWindow(hwnd) / 96.0
phys_w, phys_h = round(width * scale), round(height * scale)

# SetWindowPos 设的是**窗口**尺寸，客户区会少掉边框和标题栏（实测 1560 的外框
# 只给出 1538 的客户区），截完再拉伸就会让 1px 的边距对不上。这里先按目标客户区
# 反推外框尺寸，保证截图是严格的 1:1 逻辑像素。
outer_w, outer_h = phys_w, phys_h
if HAS_ADJUST_DPI:
    style = user32.GetWindowLongW(hwnd, -16)  # GWL_STYLE
    exstyle = user32.GetWindowLongW(hwnd, -20)  # GWL_EXSTYLE
    frame = wintypes.RECT(0, 0, phys_w, phys_h)
    if user32.AdjustWindowRectExForDpi(
        ctypes.byref(frame), style, False, exstyle, round(scale * 96)
    ):
        outer_w = frame.right - frame.left
        outer_h = frame.bottom - frame.top

user32.SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, outer_w, outer_h, SWP_SHOWWINDOW)
user32.SetForegroundWindow(hwnd)


def window_rect():
    r = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(r))
    return (r.left, r.top, r.right, r.bottom)


# 窗口移动/改尺寸后 DWM 还会补一段动画，抢在这之前截图会截到窗口还没挪到位
# 的桌面（表现为图左边多一条黑边）。等到外框位置连续两次采样一致再动手。
last = window_rect()
for _ in range(40):
    time.sleep(0.1)
    now = window_rect()
    if now == last and now[0] >= 0 and now[1] >= 0:
        break
    last = now

# 需要交互（点播放列表某一行）时，直接给窗口投递鼠标消息，
# 不移动真实光标——避免截图里混进悬停态。
if click:
    cx_, cy_ = round(click[0] * scale), round(click[1] * scale)
    lparam = (cy_ << 16) | (cx_ & 0xFFFF)
    user32.PostMessageW(hwnd, 0x0200, 0, lparam)  # WM_MOUSEMOVE
    user32.PostMessageW(hwnd, 0x0201, 1, lparam)  # WM_LBUTTONDOWN
    time.sleep(0.08)
    user32.PostMessageW(hwnd, 0x0202, 0, lparam)  # WM_LBUTTONUP
    time.sleep(wait)
else:
    time.sleep(wait)

rect = wintypes.RECT()
user32.GetClientRect(hwnd, ctypes.byref(rect))
pt = wintypes.POINT(0, 0)
user32.ClientToScreen(hwnd, ctypes.byref(pt))
box = (pt.x, pt.y, pt.x + rect.right, pt.y + rect.bottom)
img = ImageGrab.grab(bbox=box)
if img.size != (width, height):
    img = img.resize((width, height), Image.LANCZOS)
out.parent.mkdir(parents=True, exist_ok=True)
img.save(out)
print(f"saved {out} {img.size} scale={scale} client={rect.right}x{rect.bottom}")

if not keep:
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
err.close()
text = log.read_text(encoding="utf-8", errors="replace").strip()
if text:
    print("--- stderr ---")
    print(text[-3000:])
