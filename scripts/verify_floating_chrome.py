"""视频模式的浮层验收：画面是否垫在主窗下面、chrome 是否真的半透明、交互桥是否通。

用法：python scripts/verify_floating_chrome.py

第七轮（视频窗挪到主窗下面）之后的判据：
0. **窗体大小 = 内容大小** —— 起播后窗口按画面像素 1:1 调整（`dwidth/dheight`
   → `window_actions::fit_to_video`），客户区比例等于画面比例，右缘不是黑边。
1. **画面区露出来了** —— 主窗口把自己的画面区从窗口区域里减掉了。没减的话，
   被整块盖住的视频窗会停止出帧，屏幕中心就不是彩条而是主窗口的底色；
   同时用 `GetWindowRgn` 直接确认「区域已设且不是整窗」。
2. **视频窗和主窗外框严丝合缝** —— 两者 `GetWindowRect` 必须逐像素相同。
   铺客户区而不是外框的话，边框那一圈会透出桌面。
3. **玻璃真的透着画面** —— 胶囊里取两个点（压在两个不同色条上）：72% 深玻璃
   不能把它们抹成同一个颜色。这是本轮的核心目标，也是「实心黑板」和
   「玻璃」的唯一客观区别。
4. **歌单是悬浮面板** —— 展开后画面宽度不变。
5. **双击画面能最大化、再双击能还原** —— 走的是叠加窗的 `WM_LBUTTONDBLCLK`
   → `WM_NCLBUTTONDBLCLK` 转交，等价于双击标题栏。
6. **按住画面能拖动窗体** —— 拖动过程中画面必须跟着走（叠加窗由 tick 追上），
   松手后外框依旧对齐。

每一步都打印取样像素与窗口几何，判据在最后统一汇总。
"""

import ctypes
import os
import shutil
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
user32.GetWindowRgn.argtypes = [wintypes.HWND, wintypes.HRGN]
user32.FindWindowW.restype = wintypes.HWND
user32.WindowFromPoint.restype = wintypes.HWND
user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.SetCursorPos.argtypes = [ctypes.c_int, ctypes.c_int]
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.AttachThreadInput.argtypes = [wintypes.DWORD, wintypes.DWORD, wintypes.BOOL]
user32.GetWindowLongW.argtypes = [wintypes.HWND, ctypes.c_int]
user32.AdjustWindowRectExForDpi.argtypes = [
    ctypes.POINTER(wintypes.RECT), wintypes.DWORD, wintypes.BOOL,
    wintypes.DWORD, wintypes.UINT,
]

# 数据目录隔离：不要碰用户真实的 playlist.json。
# 每轮**清空**：窗口尺寸会被持久化，上一轮拖动/最大化留下的尺寸会让这一轮的
# `SetWindowPos` 看起来"没生效"（实测读到过位于 301,158 的 1920×1200 外框）。
data_dir = root / "target" / "verify-data"
shutil.rmtree(data_dir, ignore_errors=True)
data_dir.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, SMP_DATA_DIR=str(data_dir))

# 上一轮没收干净的进程会让 `FindWindowW` 拿到一个已经死掉的句柄
subprocess.run(["taskkill", "/F", "/IM", "simple-media-player.exe"], capture_output=True)
subprocess.run(["taskkill", "/F", "/IM", "mpv.exe"], capture_output=True)
time.sleep(0.6)

width, height = 1200, 760
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
# 残留实例会让 `FindWindowW` 拿到**另一个进程**的窗口，后面每一条判定都会失真
owner = wintypes.DWORD()
user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
if owner.value != proc.pid:
    proc.kill()
    raise SystemExit(f"找到的窗口不属于本次启动的进程（{owner.value} ≠ {proc.pid}），先清掉残留实例")

scale = user32.GetDpiForWindow(hwnd) / 96.0

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


def window_rect(handle=None):
    r = wintypes.RECT()
    user32.GetWindowRect(handle or hwnd, ctypes.byref(r))
    return r.left, r.top, r.right, r.bottom


def frame_rect(handle=None):
    """外框矩形 (x, y, w, h)。"""
    left, top, right, bottom = window_rect(handle)
    return left, top, right - left, bottom - top


def region_box():
    """主窗口当前的窗口区域：返回 (类型, 外接矩形) 或 (0, None) 表示没设区域。

    `GetWindowRgn` 的返回码：0 = ERROR（区域是整窗/未设）、1 NULLREGION、
    2 SIMPLEREGION、3 COMPLEXREGION。整窗 − 画面 + chrome 一定是 COMPLEXREGION。
    """
    rgn = ctypes.windll.gdi32.CreateRectRgn(0, 0, 0, 0)
    kind = user32.GetWindowRgn(hwnd, rgn)
    if kind == 0:
        ctypes.windll.gdi32.DeleteObject(rgn)
        return 0, None
    r = wintypes.RECT()
    ctypes.windll.gdi32.GetRgnBox(rgn, ctypes.byref(r))
    ctypes.windll.gdi32.DeleteObject(rgn)
    return kind, (r.left, r.top, r.right - r.left, r.bottom - r.top)


# 抢前台：脚本所在进程不在前台时，`SetForegroundWindow` 会被系统拒绝（只有前台
# 进程才有权抢），而**第一次落在窗口上的点击会被吃掉用于激活**——表现为"点了没
# 反应，第二次才灵"。第七轮之后连"先点画面激活"这条也不成立了：画面点下去是叠加窗
# 的 `WM_NCLBUTTONDOWN`（HTCAPTION），那是鼠标捕获、不激活主窗口。
# 标准解法是临时把本线程挂到前台线程的输入队列上再抢。
def force_foreground():
    fg = user32.GetForegroundWindow()
    if fg == hwnd:
        return True
    cur = ctypes.windll.kernel32.GetCurrentThreadId()
    fg_tid = user32.GetWindowThreadProcessId(fg, None)
    user32.AttachThreadInput(cur, fg_tid, True)
    user32.ShowWindow(hwnd, 9)  # SW_RESTORE
    user32.BringWindowToTop(hwnd)
    user32.SetForegroundWindow(hwnd)
    user32.AttachThreadInput(cur, fg_tid, False)
    time.sleep(0.3)
    return user32.GetForegroundWindow() == hwnd


activated = force_foreground()
fg = user32.GetForegroundWindow()
buf = ctypes.create_unicode_buffer(128)
user32.GetClassNameW(fg, buf, 128)
print(f"前台窗口 = {fg}（主窗口 {hwnd}）→ {'OK' if activated else '仍未获得焦点'} 类='{buf.value}'")


def grab(path=None):
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
    if path:
        img.save(path)
    return img


def click(lx, ly):
    """真实光标点击。

    不能只 PostMessage：GPUI 的命中测试用的是光标位置，投递的 lparam 不总被采信
    （这条踩过坑，记在项目备忘里）。这里窗口位置稳定，直接用真实输入。
    """
    ox, oy = client_origin()
    sx, sy = ox + round(lx * scale), oy + round(ly * scale)
    # 打一下这一点到底属于哪个窗口：区域没减对时，点击会被 mpv 那一层吃掉，
    # 表现为「点了没反应」，光看截图看不出来。
    hit = user32.WindowFromPoint(wintypes.POINT(sx, sy))
    buf = ctypes.create_unicode_buffer(128)
    user32.GetClassNameW(hit, buf, 128)
    print(f"  点击 逻辑({lx:.0f},{ly:.0f}) → 屏幕({sx},{sy}) 窗口={hit} 类='{buf.value}'")
    # 先停在旁边再挪进去：`SetCursorPos` 到**同一个点**不产生 WM_MOUSEMOVE，
    # 光标已经在那儿时窗口就不知道鼠标进来了，随后的 LEFTDOWN 可能被当成
    # 「在窗外按下」而丢掉（实测：歌单"点了没反应"就是这么来的）。
    user32.SetCursorPos(sx - 40, sy)
    time.sleep(0.12)
    user32.SetCursorPos(sx, sy)
    time.sleep(0.2)
    user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
    time.sleep(0.08)
    user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)
    time.sleep(0.45)


def double_click(lx, ly):
    """在画面区双击（叠加窗自己处理 WM_LBUTTONDBLCLK → 最大化/还原）。"""
    ox, oy = client_origin()
    sx, sy = ox + round(lx * scale), oy + round(ly * scale)
    # 和 `click` 一样：先离开一点再进来，否则光标已经在那儿时窗口收不到 WM_MOUSEMOVE，
    # 第一下点击会被系统当成"在窗外按下"而丢掉。
    user32.SetCursorPos(sx - 40, sy)
    time.sleep(0.12)
    user32.SetCursorPos(sx, sy)
    time.sleep(0.2)
    for _ in range(2):
        user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
        time.sleep(0.03)
        user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)
        time.sleep(0.05)
    time.sleep(0.5)


def sample(img, lx, ly):
    return img.getpixel((round(lx), round(ly)))


def colorful(px):
    """彩条判定：testsrc2 的高饱和色条至少有一个通道远超其它两个。

    主窗口底色是 #EFF1F5（接近中性灰），黑边是 (0,0,0)，都过不了这条。
    """
    return max(px) - min(px) >= 60


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


def check_aligned(tag):
    """视频窗必须和主窗外框逐像素重合。"""
    main = frame_rect()
    ovl = frame_rect(overlay)
    delta = tuple(o - m for o, m in zip(ovl, main))
    print(f"  [{tag}] 主窗外框 = {main}  视频窗外框 = {ovl}  差 = {delta}")
    return delta == (0, 0, 0, 0)


def wait_aligned(timeout=2.0):
    """等视频窗追上主窗口的新几何。

    窗口刚被改过尺寸时，叠加窗要等下一帧（canvas 重新上报视频区）+ 100ms tick
    才跟上来；这期间截图会拍到"画面还停在旧位置、右边露出桌面/别的窗口"，
    看起来像"没铺满"，其实是取样太早。
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        if frame_rect(overlay) == frame_rect():
            return True
        time.sleep(0.1)
    return frame_rect(overlay) == frame_rect()


# ---- 0. 窗体大小 = 内容大小 ---------------------------------------------
# 起播后窗口会按画面调一次尺寸：mpv 报 `dwidth/dheight` → `window_actions::fit_to_video`。
# 判据用**像素级**的：客户区应等于画面像素（1:1，不做缩放），比例应等于画面比例。
# 样片都是 16:9，这里按文件名给出期望像素；换样片时改这张表。
EXPECTED_PIXELS = {"verify-long.mp4": (1280, 720), "gen-1080p.mp4": (1920, 1080)}
fit_client = wait_client_settled()
fit_w, fit_h = fit_client[0] / scale, fit_client[1] / scale
print(
    f"起播后客户区 = {fit_client[0]}x{fit_client[1]} 物理"
    f" → 逻辑 {fit_w:.1f}x{fit_h:.1f}"
)
expect = EXPECTED_PIXELS.get(media.name)
# 截图前先等叠加窗追上新的窗口几何（否则取到的是"画面还停在旧位置"的中间态）
fit_aligned = wait_aligned()
if expect:
    fit_ok = abs(fit_client[0] - expect[0]) <= 2 and abs(fit_client[1] - expect[1]) <= 2
    print(f"  期望画面像素 {expect[0]}x{expect[1]}（窗口 = 内容，1:1）→ {'符合' if fit_ok else '不符'}")
else:
    ratio = fit_client[0] / fit_client[1]
    fit_ok = abs(ratio - 16 / 9) < 0.02
    print(f"  客户区比例 = {ratio:.3f}（期望 16:9）→ {'符合' if fit_ok else '不符'}")
# 画面右缘：窗口比例正好等于画面比例时那里是画面本体；黑边（letterbox）或比例
# 不对时会是黑的。取样点避开左侧竖条里的胶囊（y 取客户区正中）。
#
# 要等几帧：改完尺寸后 mpv 得重新配置 VO，叠加窗的几何已经到了、画面却可能还是
# 上一帧的一部分（表现为"上面一条是画面、下面露出桌面"）。这是**瞬态**，
# 等不到就必须如实报失败，所以只在超时前重试（给 3 秒，慢机器上 VO 重配更久）。
fit_edge = (0, 0, 0)
deadline = time.time() + 3.0
while time.time() < deadline:
    fit_aligned = fit_aligned or wait_aligned(0.3)
    img_fit = grab()
    fit_edge = sample(img_fit, fit_w - 4, fit_h / 2)
    if colorful(fit_edge):
        break
    time.sleep(0.15)
img_fit.save(out_dir / "floating-00-fit.png")
print(f"  画面右缘像素 = {fit_edge}   ← 应为彩条（有黑边就不是）")
print(f"  视频窗是否已跟上新几何 = {fit_aligned}")

# 后面几步（点击 / 拖动 / 双击）需要稳定的几何，先按脚本自己的尺寸把窗口摆好：
# 按**目标客户区**反推外框。`AdjustWindowRectExForDpi` 在这类自绘标题栏
# （`WM_NCCALCSIZE` 返回 0）的窗口上会多算一个标题栏高度，所以先拿它当起点，
# 再按实测客户区差值补两次——比任何公式都准。
style = user32.GetWindowLongW(hwnd, -16)  # GWL_STYLE
target = (round(width * scale), round(height * scale))
box = wintypes.RECT(0, 0, target[0], target[1])
user32.AdjustWindowRectExForDpi(
    ctypes.byref(box), style, False, 0, user32.GetDpiForWindow(hwnd)
)
user32.SetWindowPos(
    hwnd, HWND_TOPMOST, 0, 0, box.right - box.left, box.bottom - box.top, SWP_SHOWWINDOW
)
time.sleep(0.6)
for _ in range(3):
    r = wintypes.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(r))
    dw, dh = target[0] - r.right, target[1] - r.bottom
    if abs(dw) <= 1 and abs(dh) <= 1:
        break
    o = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(o))
    user32.SetWindowPos(
        hwnd, HWND_TOPMOST, 0, 0,
        (o.right - o.left) + dw, (o.bottom - o.top) + dh, SWP_SHOWWINDOW,
    )
    time.sleep(0.5)
set_w, set_h = wait_client_settled()
lw, lh = set_w / scale, set_h / scale
print(f"脚本摆到 {width}x{height} 后：物理 {set_w}x{set_h} 逻辑 {lw:.1f}x{lh:.1f}")

# ---- 1. 画面区是否真的让出来了 -----------------------------------------
img = grab(out_dir / "floating-01-video.png")
mid = sample(img, lw / 2, lh / 2)
kind, box = region_box()
print(f"画面中心像素 = {mid}   ← 应为彩条（区域减法生效才会出帧）")
print(f"主窗口区域 = 类型 {kind}（2=SIMPLE / 3=COMPLEX），外接框 {box}")

aligned = check_aligned("静止")

# 标题带：玻璃压在画面的顶部黑边上，值应接近 #0B0C10（画不画玻璃几乎看不出差别，
# 因为底下本来就是黑边）；关键是它不能是主窗口的浅色底。
band = sample(img, 8, 17)
print(f"标题带左端 (8,17) = {band}   ← 应为深色（玻璃 / 黑边）")

# ---- 2. 玻璃真的透出画面 -----------------------------------------------
# 两个点都在胶囊里、都压在画面上，但下面分别是两个色条（左红、右蓝/紫）。
# 实心黑板（旧架构）会把它们抹成同一个颜色；72% 玻璃会留住色相。
# 走带行高 44，胶囊底 lh-24，下 padding 8 → 行中心 lh-54。
caps_a = sample(img, 40, lh - 54)
caps_b = sample(img, lw * 0.55, lh - 54)
spread = abs(caps_a[0] - caps_b[0]) + abs(caps_a[1] - caps_b[1]) + abs(caps_a[2] - caps_b[2])
print(f"胶囊内左侧 (40,{lh - 54:.0f}) = {caps_a}")
print(f"胶囊内右侧 ({lw * 0.55:.0f},{lh - 54:.0f}) = {caps_b}   ← 两处色差 {spread}（>25 才算透出画面）")

def region_contains(lx, ly):
    """这一点是否还在主窗口的窗口区域里（= 点击/绘制都归主窗口）。"""
    rgn = ctypes.windll.gdi32.CreateRectRgn(0, 0, 0, 0)
    inside = False
    if user32.GetWindowRgn(hwnd, rgn):
        wr = wintypes.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(wr))
        pt = wintypes.POINT(0, 0)
        user32.ClientToScreen(hwnd, ctypes.byref(pt))
        ox, oy = pt.x - wr.left, pt.y - wr.top
        inside = bool(
            ctypes.windll.gdi32.PtInRegion(
                rgn, ox + round(lx * scale), oy + round(ly * scale)
            )
        )
    ctypes.windll.gdi32.DeleteObject(rgn)
    return inside


def toggle_playlist(want_open, tries=4):
    """把歌单切到目标状态，最多试 `tries` 次。

    单次真实点击偶尔会被系统吞掉（`mouse_event` 的 down/up 与 GPUI 的
    激活/命中是两套时序），这不是应用的问题；这里按**结果**重试，
    最后一次仍不对就如实报失败。
    """
    for _ in range(tries):
        click(lw - 123, lh - 55)
        time.sleep(0.6)
        if region_contains(lw - 172, 120) == want_open:
            return True
    return False


def refresh_geometry():
    """重新读一次客户区，更新 lw/lh（最大化/还原之后必须重读，否则取样越界）。"""
    global lw, lh
    cw, ch = wait_client_settled()
    lw, lh = cw / scale, ch / scale
    return cw, ch


# ---- 3. 展开歌单：面板浮在右侧，画面不被挤窄 ---------------------------
# 坐标不是算出来的，是 UI 测试实测的：`toggle-playlist` 的中心距窗口右沿 123、
# 距下沿 55（三键各 28 宽、间距 6，胶囊内缩 24 + padding 16）。算出来的值差 1~2px
# 看着没问题，但按钮之间只有 6px 间隙，偏一点就落进空档，点击"点了没反应"。
opened = toggle_playlist(True)
img2 = grab(out_dir / "floating-02-playlist.png")
panel_px = sample(img2, lw - 12 - 160, 120)
print(f"歌单面板内 (.., 120) = {panel_px}   ← 应为面板底色（浅）")
print(f"歌单展开后画面中心 = {sample(img2, lw / 2, lh / 2)}")
# 面板浮在画面上，但它**不在**原来的窗口区域里：区域必须把它加回去（否则被剪掉）。
panel_inside = region_contains(lw - 172, 120)
print(f"歌单面板区在窗口区域里 = {panel_inside}   ← 必须是 True，否则面板被剪掉")
closed = toggle_playlist(False)  # 收起来，别挡住后面的取样
print(f"再点一次收起歌单 = {closed}")
refresh_geometry()

# ---- 4. 按住画面拖动窗体 ----------------------------------------------
before = window_rect()
ox, oy = client_origin()
start = (ox + round(lw * 0.5 * scale), oy + round(lh * 0.45 * scale))
# 光标本来就在附近时先挪开再回来：见 `click` 里的说明。
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
time.sleep(0.25)
# 拖动**途中**取样：叠加窗跟不上就会在这里露黑边（画面还停在旧位置）
img_drag = grab(root / "target" / "floating-03-dragging.png")
drag_mid = sample(img_drag, lw * 0.5, lh * 0.45)
user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)
time.sleep(0.6)
after = window_rect()
dx, dy = after[0] - before[0], after[1] - before[1]
print(f"拖动前 {before[:2]} → 拖动后 {after[:2]}（位移 {dx},{dy}）")
print(f"拖动途中画面中心 = {drag_mid}   ← 应为彩条（叠加窗跟上了）")
drag_aligned = check_aligned("拖动后")
refresh_geometry()

# ---- 5. 双击画面最大化 / 再双击还原 ------------------------------------
def wait_maximized(bigger, base, timeout=4.0):
    """等窗口真的变大/回原尺寸。

    不拿 `IsZoomed` 当唯一判据：合成输入下偶尔会读到「尺寸已经是工作区、
    `IsZoomed` 还是假」的中间态（光标事件和 `ShowWindowAsync` 都在排队）。
    用户能感知的是**窗口尺寸变了**，所以主判据是尺寸，`IsZoomed` 只做参考打印。
    """
    deadline = time.time() + timeout
    hit = False
    while time.time() < deadline:
        cw, ch = client_rect()
        if (bigger and cw > base[0] + 50) or (not bigger and abs(cw - base[0]) <= 2):
            hit = True
            break
        time.sleep(0.1)
    time.sleep(0.45)  # 等 DWM 动画收尾
    return hit, bool(user32.IsZoomed(hwnd))


# 这两步放在最后：会改窗口尺寸/位置，前面的取样都按旧几何算好的。
# 走的是叠加窗自己的 `WM_LBUTTONDBLCLK` → `window_actions::set_zoomed`。
# 和歌单一样按**结果**重试：合成鼠标事件偶发被系统吞掉，那不是应用的问题，
# 但重试仍不对就必须报失败。
base_client = client_rect()
zoomed, zoom_style = False, False
for attempt in range(1, 3):
    double_click(lw * 0.5, lh * 0.45)
    zoomed, zoom_style = wait_maximized(True, base_client)
    if zoomed:
        break
    print(f"  第 {attempt} 次双击没让窗口变大，重试")
cw1, ch1 = refresh_geometry()
img_max = grab(out_dir / "floating-04-maximized.png")
max_mid = sample(img_max, lw / 2, lh / 2)
max_aligned = check_aligned("最大化")
print(
    f"双击画面 → 变大 {zoomed}（IsZoomed={zoom_style}）；"
    f"client = {cw1}x{ch1}（原始 {base_client[0]}x{base_client[1]}）；画面中心 = {max_mid}"
)

restored, restore_style = False, False
for attempt in range(1, 3):
    double_click(lw * 0.5, lh * 0.45)
    restored, restore_style = wait_maximized(False, base_client)
    if restored:
        break
    print(f"  第 {attempt} 次双击没还原，重试")
cw2, ch2 = refresh_geometry()
print(
    f"再双击画面 → 回原尺寸 {restored}（IsZoomed={restore_style}）；"
    f"client = {cw2}x{ch2}（原始 {base_client[0]}x{base_client[1]}）"
)
restore_aligned = check_aligned("还原后")

grab(root / "target" / "floating-05-restored.png")

proc.terminate()
try:
    proc.wait(timeout=5)
except subprocess.TimeoutExpired:
    proc.kill()
err.close()

print("--- 判定 ---")
ok = True
if not fit_ok:
    print(f"FAIL: 起播后窗口尺寸没跟画面对齐（客户区 {fit_client}，期望 {expect or '16:9'}）")
    ok = False
elif not fit_aligned:
    print("FAIL: 调整尺寸后视频窗没跟上主窗口几何（画面会停在旧位置）")
    ok = False
elif not colorful(fit_edge):
    print(f"FAIL: 画面右缘是 {fit_edge}，比例对不上/有黑边（画面没铺满客户区）")
    ok = False
else:
    print(f"OK: 起播即把窗口调成画面尺寸 {fit_client}（1:1），画面铺满不留黑边")
if not colorful(mid):
    print(f"FAIL: 画面中心不是彩条（{mid}），画面区没从窗口区域里减掉")
    ok = False
else:
    print(f"OK: 画面区露出来了，屏幕中心是彩条 {mid}")
if kind not in (2, 3):
    print("FAIL: 主窗口没有设窗口区域（视频窗会被判成完全遮挡而停止出帧）")
    ok = False
else:
    print(f"OK: 主窗口区域已设（类型 {kind}），外接框仍然是整窗 {box}")
if not aligned:
    print("FAIL: 视频窗和主窗外框没对齐（铺客户区会让边框透出桌面）")
    ok = False
else:
    print("OK: 静止时视频窗与主窗外框严丝合缝")
if spread < 25:
    print(f"FAIL: 胶囊里的两处几乎同色（色差 {spread}），玻璃没有透出画面")
    ok = False
else:
    print(f"OK: 胶囊玻璃透出画面，两个色条处色差 {spread}")
if sum(panel_px[:3]) < 600:
    print(f"FAIL: 歌单面板没展开（取样仍是画面 {panel_px}）")
    ok = False
elif not panel_inside:
    print("FAIL: 面板展开后那块区域没加回主窗口区域，面板会被剪掉")
    ok = False
else:
    print(f"OK: 歌单面板浮在画面右侧 {panel_px}，且区域已把它加回去")
if not zoomed:
    print(f"FAIL: 双击画面没让窗口变大（client 仍是 {cw1}x{ch1}）")
    ok = False
elif not colorful(max_mid):
    print(f"FAIL: 最大化后画面中心不是彩条（{max_mid}），区域没跟着重算")
    ok = False
elif not max_aligned:
    print("FAIL: 最大化后视频窗和主窗外框没对齐")
    ok = False
else:
    print(f"OK: 双击画面最大化到 {cw1}x{ch1}，画面仍铺满 {max_mid}（IsZoomed={zoom_style}）")
if not restored:
    print(f"FAIL: 再双击没回原尺寸（仍是 {cw2}x{ch2}，原始 {base_client[0]}x{base_client[1]}）")
    ok = False
elif not restore_aligned:
    print("FAIL: 还原后视频窗和主窗外框没对齐")
    ok = False
else:
    print(f"OK: 再双击还原回 {cw2}x{ch2}（IsZoomed={restore_style}）")
if dx == 0 and dy == 0:
    print("FAIL: 按住画面没有拖动窗口")
    ok = False
else:
    print(f"OK: 按住画面拖动窗口，位移 {dx},{dy}")
if not colorful(drag_mid):
    print(f"FAIL: 拖动途中画面没跟上（中心取到 {drag_mid}）")
    ok = False
else:
    print(f"OK: 拖动途中画面跟着走 {drag_mid}")
if not drag_aligned:
    print("FAIL: 拖动后视频窗和主窗外框没对齐")
    ok = False
else:
    print("OK: 拖动后外框依旧对齐")
print("ALL OK" if ok else "SOME CHECKS FAILED")
