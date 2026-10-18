"""视频模式的浮层验收：chrome 是否真的压在画面上、歌单是否悬浮、按住画面能否拖窗。

用法：python scripts/verify_floating_chrome.py

四步：
1. 起播一个 mp4，截「静止态」—— 标题带 + 胶囊必须出现在画面之上；
2. 点胶囊右侧的列表图标展开歌单，再截一张 —— 面板浮在右侧，画面宽度不变；
3. 用真实鼠标在画面中间按下并拖动 —— 主窗口应跟着移动（叠加窗由 tick 追上）；
4. 打印每一步的取样像素与窗口几何。
"""

import ctypes
import os
import subprocess
import time
from ctypes import wintypes
from pathlib import Path

from PIL import Image, ImageGrab

try:
    ctypes.windll.shcore.SetProcessDpiAwareness(2)
except Exception:  # noqa: BLE001
    ctypes.windll.user32.SetProcessDPIAware()

HWND_TOPMOST = -1
SWP_SHOWWINDOW = 0x0040
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004

root = Path(__file__).resolve().parent.parent
exe = Path(
    os.environ.get("SMP_EXE") or root / "target" / "debug" / "simple-media-player.exe"
)
if not exe.is_absolute():
    exe = root / exe

# 验收产物落到 target/（不进仓库）；docs 里只保留两张正式截图
out_dir = root / "target" / "verify-shots"
out_dir.mkdir(parents=True, exist_ok=True)
media = root / "test-media" / "gen-1080p.mp4"

user32 = ctypes.windll.user32
user32.SetWindowPos.argtypes = [
    wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int,
    ctypes.c_int, ctypes.c_int, ctypes.c_uint,
]
user32.GetClientRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user32.ClientToScreen.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.POINT)]
user32.GetDpiForWindow.argtypes = [wintypes.HWND]
user32.FindWindowW.restype = wintypes.HWND
user32.SetCursorPos.argtypes = [ctypes.c_int, ctypes.c_int]

# 数据目录隔离：不要碰用户真实的 playlist.json
data_dir = root / "target" / "verify-data"
data_dir.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, SMP_DATA_DIR=str(data_dir))

width, height = 1280, 800
log = root / "target" / "verify-stderr.log"
err = open(log, "wb")
proc = subprocess.Popen(
    [str(exe), "--play", str(media)],
    stdout=err,
    stderr=subprocess.STDOUT,
    env=env,
)
time.sleep(4.0)

hwnd = user32.FindWindowW(None, "Chroma")
if not hwnd:
    proc.kill()
    raise SystemExit("未找到标题为 Chroma 的窗口")

scale = user32.GetDpiForWindow(hwnd) / 96.0
phys_w, phys_h = round(width * scale), round(height * scale)
user32.SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, phys_w, phys_h, SWP_SHOWWINDOW)
user32.SetForegroundWindow(hwnd)
time.sleep(1.2)

overlay = user32.FindWindowW("SimpleMediaPlayerVideoOverlay", None)
print(f"overlay hwnd = {overlay}")


def client_rect():
    r = wintypes.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(r))
    return r.right, r.bottom


def client_origin():
    pt = wintypes.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(pt))
    return pt.x, pt.y


def window_rect():
    r = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(r))
    return r.left, r.top, r.right, r.bottom


# 逻辑尺寸取**实测客户区**：SetWindowPos 设的是外框，客户区实测 1898×1189 物理
# ÷1.5 = 1265×793，跟设的 1280×800 差几像素。按目标尺寸换算的话所有坐标整体
# 偏掉一点，点胶囊右侧那排 28px 的图标就会点到间隙里（踩过）。
cw0, ch0 = client_rect()
lw, lh = cw0 / scale, ch0 / scale
print(f"client = {cw0}x{ch0} 逻辑 = {lw:.1f}x{lh:.1f}  scale = {scale}")


def grab(path):
    cw, ch = client_rect()
    ox, oy = client_origin()
    img = ImageGrab.grab(bbox=(ox, oy, ox + cw, oy + ch))
    # 缩到实测逻辑尺寸：1 图素 = 1 逻辑像素
    want = (round(cw / scale), round(ch / scale))
    if img.size != want:
        img = img.resize(want, Image.LANCZOS)
    img.save(path)
    return img


def click(lx, ly):
    """真实光标点击。

    不能只 PostMessage：GPUI 的命中测试用的是光标位置，投递的 lparam 不总被采信
    （这条踩过坑，记在项目备忘里）。这里窗口位置稳定，直接用真实输入。
    """
    ox, oy = client_origin()
    user32.SetCursorPos(ox + round(lx * scale), oy + round(ly * scale))
    time.sleep(0.15)
    user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
    time.sleep(0.08)
    user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)
    time.sleep(0.45)
    # 光标挪到画面上，别把 hover 态留在截图里
    user32.SetCursorPos(ox + round(lw * 0.5 * scale), oy + round(lh * 0.3 * scale))
    time.sleep(0.2)


def sample(img, lx, ly):
    return img.getpixel((round(lx), round(ly)))


# ---- 1. 静止态：标题带 / 胶囊是否真的在画面之上 -------------------------
img = grab(out_dir / "floating-01-video.png")

mid = sample(img, lw / 2, lh / 2)
print(f"画面中心像素 = {mid}")
band = sample(img, 8, 17)
print(f"标题带左端 (8,17) = {band}   ← 应为画布深色（挖出来了）")
capsule = sample(img, 40, lh - 60)
print(f"胶囊左下 (40,{lh - 60:.0f}) = {capsule}   ← 应为玻璃深色")
below_band = sample(img, lw / 2, 40)
print(f"标题带下方 (.., 40) = {below_band}   ← 应为画面彩条")

# ---- 2. 展开歌单：面板浮在右侧，画面不被挤窄 ---------------------------
# 胶囊内缩 24 + 内 padding 16 = 内容右沿 lw-40；右侧三键 28 宽、gap 6，
# 从右往左是 theme-toggle / cinema / toggle-playlist → 列表键圆心在 lw-122。
# 走带行高 44，胶囊底 lh-24，下 padding 8 → 行中心 lh-54。
click(lw - 122, lh - 54)
time.sleep(0.5)
img2 = grab(out_dir / "floating-02-playlist.png")
panel_px = sample(img2, lw - 12 - 160, 120)
print(f"歌单面板内 (.., 120) = {panel_px}   ← 应为面板底色（浅）")
print(f"歌单展开后画面中心 = {sample(img2, lw / 2, lh / 2)}")

# ---- 3. 最大化：区域不会跟着 SetWindowPos 自动缩放，必须重算 -------------
# 三个窗口按钮各 46 宽，从右往左是 close / max / min → 最大化键圆心在 lw-69。
click(lw - 69, 17)
time.sleep(0.9)
cw1, ch1 = client_rect()
lw, lh = cw1 / scale, ch1 / scale
img_max = grab(out_dir / "floating-03-maximized.png")
max_band = sample(img_max, 8, 17)
max_mid = sample(img_max, lw / 2, lh / 2)
print(f"最大化后 client = {cw1}x{ch1}；标题带 (8,17) = {max_band}；画面中心 = {max_mid}")

# 还原用 ShowWindow(SW_RESTORE) 而不是再点一次按钮：点按钮会走 GPUI 那层，
# 时序不稳（上一步就出现过没还原成功、拖动时被系统当成"拖最大化窗口"还原）。
user32.ShowWindow(hwnd, 9)  # SW_RESTORE
time.sleep(0.4)
user32.SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, phys_w, phys_h, SWP_SHOWWINDOW)
time.sleep(0.8)
cw0, ch0 = client_rect()
lw, lh = cw0 / scale, ch0 / scale
print(f"还原后 client = {cw0}x{ch0}")

# ---- 4. 按住画面拖动窗体 ----------------------------------------------
before = window_rect()
ox, oy = client_origin()
start = (ox + round(lw * 0.5 * scale), oy + round(lh * 0.45 * scale))
user32.SetCursorPos(*start)
time.sleep(0.15)
user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
time.sleep(0.15)
for step in range(1, 9):
    user32.SetCursorPos(start[0] + step * round(12 * scale), start[1] + step * round(6 * scale))
    time.sleep(0.05)
time.sleep(0.2)
user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)
time.sleep(0.6)
after = window_rect()
dx, dy = after[0] - before[0], after[1] - before[1]
print(f"拖动前 {before[:2]} → 拖动后 {after[:2]}（位移 {dx},{dy}）")

grab(root / "target" / "floating-03-dragged.png")

proc.terminate()
try:
    proc.wait(timeout=5)
except subprocess.TimeoutExpired:
    proc.kill()
err.close()

print("--- 判定 ---")
ok = True
if sum(panel_px[:3]) < 600:
    print(f"FAIL: 歌单面板没展开（取样仍是画面 {panel_px}）")
    ok = False
if sum(max_band[:3]) > 200:
    print(f"FAIL: 最大化后标题带没挖出来（取样 {max_band}，区域没重算）")
    ok = False
else:
    print(f"OK: 最大化后标题带仍是挖出来的深色 {max_band}")
if dx == 0 and dy == 0:
    print("FAIL: 按住画面没有拖动窗口")
    ok = False
else:
    print(f"OK: 按住画面拖动窗口，位移 {dx},{dy}")
print("ALL OK" if ok else "SOME CHECKS FAILED")
