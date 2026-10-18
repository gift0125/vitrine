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
# 时长要够：整套流程要跑二十几秒，5 秒的样片会在中途播完 → 叠加窗被隐藏，
# 后面的"按住画面拖窗"就变成"点在一块 GPUI 画布上"，假失败。
# 仓库里的样片都只有 5 秒，所以这里优先用一段现生成的长片（不进仓库）：
#   mpv av://lavfi:testsrc2=duration=40:size=1280x720:rate=24 \
#       --o=target/verify-long.mp4 --ovc=libx264 --oac=libmp3lame --of=mp4
media = root / "target" / "verify-long.mp4"
if not media.exists():
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
user32.WindowFromPoint.restype = wintypes.HWND
user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
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

# 预热点击：应用刚从后台启动时，`SetForegroundWindow` 常常被系统拒绝
# （只有前台进程才能抢焦点），于是第一次真实点击只用来「激活窗口」，
# 事件到不了 GPUI（表现为"点了没反应"，第二次才灵）。
# 先对着画面点一下把激活做掉——叠加窗会把它当成 HTCAPTION，没移动就等于没发生。
user32.SetCursorPos(
    client_origin()[0] + round(lw * 0.5 * scale),
    client_origin()[1] + round(lh * 0.45 * scale),
)
time.sleep(0.2)
user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
time.sleep(0.06)
user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)
time.sleep(0.5)
fg = user32.GetForegroundWindow()
print(f"前台窗口 = {fg}（主窗口 {hwnd}）→ {'OK' if fg == hwnd else '仍未获得焦点'}")


def grab(path):
    cw, ch = client_rect()
    ox, oy = client_origin()
    if cw < 8 or ch < 8:
        # 客户区退化（窗口最小化 / 刚被搬走）时 ImageGrab 会给出空图并抛
        # "cannot write empty image"，这里退成一张纯黑占位图，让取样判据自然报错。
        print(f"  警告：客户区只有 {cw}x{ch}，截图退化")
        return Image.new("RGB", (width, height), (0, 0, 0))
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
    sx, sy = ox + round(lx * scale), oy + round(ly * scale)
    # 打一下这一点到底属于哪个窗口：叠加窗的洞没挖对时，点击会被 mpv 那一层吃掉，
    # 表现为「点了没反应」，光看截图看不出来。
    hit = user32.WindowFromPoint(wintypes.POINT(sx, sy))
    buf = ctypes.create_unicode_buffer(128)
    user32.GetClassNameW(hit, buf, 128)
    print(f"  点击 逻辑({lx:.0f},{ly}) → 屏幕({sx},{sy}) 窗口={hit} 类='{buf.value}'")
    user32.SetCursorPos(sx, sy)
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


def wait_zoomed(expected, timeout=2.5):
    """等窗口真的进入/退出最大化。

    不用固定 sleep：`ShowWindowAsync` 是排队的，最大化还有一段 DWM 动画，
    0.9s 里偶尔能读到「宽度还没跟上」的中间态（实测读到过 1898×2086）。
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        if bool(user32.IsZoomed(hwnd)) == expected:
            time.sleep(0.35)  # 等动画收尾
            return True
        time.sleep(0.1)
    return False


def wait_client_settled(timeout=2.0):
    """等客户区尺寸连续两次采样一致（避免读到动画中间态）。"""
    last = client_rect()
    deadline = time.time() + timeout
    while time.time() < deadline:
        time.sleep(0.1)
        now = client_rect()
        if now == last:
            return now
        last = now
    return last


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
time.sleep(0.6)
img2 = grab(out_dir / "floating-02-playlist.png")
panel_px = sample(img2, lw - 12 - 160, 120)
print(f"歌单面板内 (.., 120) = {panel_px}   ← 应为面板底色（浅）")
print(f"歌单展开后画面中心 = {sample(img2, lw / 2, lh / 2)}")

# ---- 3. 最大化 / 再点一次还原 -----------------------------------------
# 三个窗口按钮各 46 宽，从右往左是 close / max / min → 最大化键圆心在 lw-69。
# 用户报的 bug 就是这里：最大化之后再点没反应（GPUI 的 `zoom_window()` 在
# Windows 后端只有 SW_MAXIMIZE，没有还原分支），所以这一项必须走真实点击。
before_max = (cw0, ch0)
click(lw - 69, 17)
wait_zoomed(True)
cw1, ch1 = wait_client_settled()
lw, lh = cw1 / scale, ch1 / scale
img_max = grab(out_dir / "floating-03-maximized.png")
max_band = sample(img_max, 8, 17)
max_mid = sample(img_max, lw / 2, lh / 2)
print(f"最大化后 client = {cw1}x{ch1}；标题带 (8,17) = {max_band}；画面中心 = {max_mid}")

click(lw - 69, 17)
wait_zoomed(False)
cw2, ch2 = wait_client_settled()
print(f"再点一次后 client = {cw2}x{ch2}（原始 {before_max[0]}x{before_max[1]}）")
lw, lh = cw2 / scale, ch2 / scale

# ---- 4. 按住画面拖动窗体 ----------------------------------------------
before = window_rect()
ox, oy = client_origin()
start = (ox + round(lw * 0.5 * scale), oy + round(lh * 0.45 * scale))
# 光标本来就在附近时先挪开再回来：`SetCursorPos` 到同一个点不产生 WM_MOUSEMOVE，
# 叠加窗就不知道鼠标进来了，随后的 LEFTDOWN 可能被系统当成「在窗外按下」。
user32.SetCursorPos(start[0] - 40, start[1])
time.sleep(0.1)
user32.SetCursorPos(*start)
time.sleep(0.2)
hit = user32.WindowFromPoint(wintypes.POINT(*start))
buf = ctypes.create_unicode_buffer(128)
user32.GetClassNameW(hit, buf, 128)
print(f"拖拽起点 屏幕{start} 窗口={hit} 类='{buf.value}'")
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
if (cw2, ch2) != before_max:
    print(f"FAIL: 再点一次没还原：{cw2}x{ch2} vs 原始 {before_max[0]}x{before_max[1]}")
    ok = False
else:
    print(f"OK: 再点一次还原回 {cw2}x{ch2}")
if dx == 0 and dy == 0:
    print("FAIL: 按住画面没有拖动窗口")
    ok = False
else:
    print(f"OK: 按住画面拖动窗口，位移 {dx},{dy}")
print("ALL OK" if ok else "SOME CHECKS FAILED")
