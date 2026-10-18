"""启动应用 → 置顶定位 → 截客户区。用于对照设计稿做视觉验收。

用法：python scripts/shot.py <输出png> [--dark] [--w 1040] [--h 680] [--no-resize]
                              [--click "x,y"]|--real-click "x,y"]...
                              [--wait 2.0] [--keep]
                              [--margin 16] [-- <透传给应用的参数>]

--no-resize 不碰窗口尺寸，只拍当前客户区（缩回逻辑像素）。视频模式下窗口是应用
按画面尺寸自己长出来的，外部改尺寸会和 `fit_to_video` 打架（它在首帧之后才跑）。

--click 可以给多次，按顺序投递点击（例如先展开播放列表、再点某一集）。
`--` 之后的参数原样传给应用（例如 `-- --play D:\\movie.mp4` 直接起播）。

`--click` 走 `PostMessage`（不移动真实光标，截图里不会有悬停态），但 GPUI 的命中
测试读的是**光标位置**，投递的消息偶发不生效（表现为"点了没反应"，而同一坐标在
验收脚本里用真实输入点就正常）。要点的东西一旦有状态（展开面板、弹信息卡），
用 `--real-click`：走真实光标 + `mouse_event`，点完把光标停到标题带左上角，
免得截图里留下悬停高亮。

--w/--h 是**逻辑**尺寸（与设计稿 1280×800 同一坐标系）；脚本按屏幕缩放比换算成
物理像素，截完再缩回逻辑尺寸，这样高 DPI 下也是 1:1 的清晰图。

--margin 在客户区外留一圈（逻辑像素）：圆角、DWM 阴影这些都在客户区之外，只截客户区
看不到。留了 margin 之后窗口会被放到 (margin, margin)，免得 bbox 越过屏幕边缘被裁。
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
SWP_NOSIZE = 0x0001

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
# 截图时在客户区外留一圈（逻辑像素）：圆角、DWM 阴影这些都在客户区**之外**，
# 只截客户区就看不到它们。
margin = int(args[args.index("--margin") + 1]) if "--margin" in args else 0
clicks = []
real_clicks = []
for i, a in enumerate(args):
    if a == "--click":
        clicks.append([int(v) for v in args[i + 1].split(",")])
    elif a == "--real-click":
        real_clicks.append([int(v) for v in args[i + 1].split(",")])
# `--` 之后的参数透传给应用（例如 --play 某文件）
passthrough = args[args.index("--") + 1:] if "--" in args else []

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

cmd = [str(exe)] + (["--dark"] if dark else []) + passthrough
log = root / "target" / "shot-stderr.log"
err = open(log, "wb")
# 配置目录隔离：`--play <文件>` 会把它**写进播放列表并保存**，直接跑就会改掉用户
# 真实的 `%APPDATA%\simple-media-player\playlist.json`（踩过：截图里混进了上一次
# 验收留下的十来个样片）。`SMP_DATA_DIR` 可以整体顶掉这个目录。
shot_data = root / "target" / "shot-data"
shot_data.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, SMP_DATA_DIR=str(shot_data))
proc = subprocess.Popen(cmd, stdout=err, stderr=subprocess.STDOUT, env=env)
time.sleep(3.2)

hwnd = user32.FindWindowW(None, "Chroma")
if not hwnd:
    proc.kill()
    raise SystemExit("未找到标题为 Chroma 的窗口")

scale = user32.GetDpiForWindow(hwnd) / 96.0
phys_w, phys_h = round(width * scale), round(height * scale)
margin_px = round(margin * scale)
# `--no-resize`：不碰窗口尺寸，只拍当前客户区（按 DPI 缩回逻辑像素）。
# 视频模式下窗口是应用自己按画面尺寸长出来的，外部改尺寸会和它打架
# （fit_to_video 在首帧之后才跑，晚于这里的 SetWindowPos）。
no_resize = "--no-resize" in args

# SetWindowPos 设的是**窗口**尺寸，客户区可能比它小一圈。这里不用
# `AdjustWindowRectExForDpi` 反推——那是按「标准外框」算的，而本应用子类化了
# `WM_NCCALCSIZE`（客户区铺满窗口矩形），照它反推会**多减一圈边框**：
# 请求 1920×1080 只得到 1898×1024 的客户区，截图再拉回去就把画面拉扁了。
# 直接量一次「当前窗口矩形 − 当前客户区」，这个差值在任何 DPI 下都成立。
wr0 = wintypes.RECT()
user32.GetWindowRect(hwnd, ctypes.byref(wr0))
cr0 = wintypes.RECT()
user32.GetClientRect(hwnd, ctypes.byref(cr0))
frame_w = (wr0.right - wr0.left) - cr0.right
frame_h = (wr0.bottom - wr0.top) - cr0.bottom
outer_w, outer_h = phys_w + frame_w, phys_h + frame_h

if no_resize:
    # 只置顶 + 归位，尺寸交给应用（0 + SWP_NOSIZE）
    user32.SetWindowPos(
        hwnd, HWND_TOPMOST, margin_px, margin_px, 0, 0, SWP_NOSIZE | SWP_SHOWWINDOW
    )
else:
    user32.SetWindowPos(
        hwnd, HWND_TOPMOST, margin_px, margin_px, outer_w, outer_h, SWP_SHOWWINDOW
    )
user32.SetForegroundWindow(hwnd)


def client_size():
    cr = wintypes.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(cr))
    return cr.right, cr.bottom


def origin():
    """客户区左上角的屏幕坐标（真实光标点击要用）。"""
    pt = wintypes.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(pt))
    return pt.x, pt.y


def wait_settled():
    """等客户区尺寸连续两次采样一致——SetWindowPos 之后 WM_SIZE 还没处理完就量，
    会拿到上一次的尺寸，按它算修正量会把窗口越改越大。"""
    last = client_size()
    for _ in range(40):
        time.sleep(0.05)
        now = client_size()
        if now == last:
            return now
        last = now
    return last


# 万一客户区还差一两像素（DWM 边距、最小尺寸夹取），再修一次外框。`--no-resize`
# 时跳过这一切，按应用自己给的客户区截。
if not no_resize:
    for _ in range(4):
        cw, ch = wait_settled()
        dx, dy = phys_w - cw, phys_h - ch
        if abs(dx) <= 1 and abs(dy) <= 1:
            break
        outer_w += dx
        outer_h += dy
        user32.SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, outer_w, outer_h, SWP_SHOWWINDOW)


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
for click in clicks:
    cx_, cy_ = round(click[0] * scale), round(click[1] * scale)
    lparam = (cy_ << 16) | (cx_ & 0xFFFF)
    user32.PostMessageW(hwnd, 0x0200, 0, lparam)  # WM_MOUSEMOVE
    user32.PostMessageW(hwnd, 0x0201, 1, lparam)  # WM_LBUTTONDOWN
    time.sleep(0.08)
    user32.PostMessageW(hwnd, 0x0202, 0, lparam)  # WM_LBUTTONUP
    time.sleep(0.35)

# 真实光标点击：先停在旁边再挪进去（同一个点不产生 WM_MOUSEMOVE，随后的
# LEFTDOWN 可能被当成"在窗外按下"而丢掉）。全部点完把光标停到标题带左上角，
# 那里没有悬停高亮。
for click in real_clicks:
    ox, oy = origin()
    sx, sy = ox + round(click[0] * scale), oy + round(click[1] * scale)
    user32.SetCursorPos(sx - 40, sy)
    time.sleep(0.12)
    user32.SetCursorPos(sx, sy)
    time.sleep(0.2)
    user32.mouse_event(0x0002, 0, 0, 0, 0)  # LEFTDOWN
    time.sleep(0.08)
    user32.mouse_event(0x0004, 0, 0, 0, 0)  # LEFTUP
    time.sleep(0.5)
if real_clicks:
    ox, oy = origin()
    user32.SetCursorPos(ox + round(8 * scale), oy + round(8 * scale))
    time.sleep(0.25)
time.sleep(wait)

rect = wintypes.RECT()
user32.GetClientRect(hwnd, ctypes.byref(rect))
pt = wintypes.POINT(0, 0)
user32.ClientToScreen(hwnd, ctypes.byref(pt))
box = (
    pt.x - margin_px,
    pt.y - margin_px,
    pt.x + rect.right + margin_px,
    pt.y + rect.bottom + margin_px,
)
img = ImageGrab.grab(bbox=box)
if no_resize:
    # 按**物理像素**原样输出：视频模式 1:1 时客户区正好等于画面像素数
    # （720p 片子 = 1280×720 物理），缩回逻辑像素反而会把画面压扁。
    want = img.size
else:
    want = (width + margin * 2, height + margin * 2)
if img.size != want:
    img = img.resize(want, Image.LANCZOS)
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
