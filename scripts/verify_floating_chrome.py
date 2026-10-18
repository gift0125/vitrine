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
5. **双击画面 = 全屏播放**（不是最大化）—— 叠加窗自己判双击（`WM_LBUTTONDBLCLK`
   对合成输入不可靠），把 `ToggleCinema` 塞进队列，由 tick 调
   `PlayerView::set_cinema`；判定看**外框是否等于显示器矩形**，比 `IsZoomed` 更
   贴近用户看到的样子。
6. **全屏里有三个出口都回窗口** —— `Esc`（`App::intercept_keystrokes`，不是
   keymap + `on_action`）、右上角「最大化」按钮、再双击画面。出口之后客户区必须
   重新等于外框（GPUI 退出全屏时不重算客户区，靠 tick 里的自愈）。
7. **顶栏铺满整个宽度** —— 窗口有 7 逻辑像素的隐形外框，叠加窗按外框铺、GPUI 只
   画客户区，于是顶栏左右各缺一条，露出的是**没被压暗的画面**。判据：窗口最左 /
   最右那一列，标题带中线必须比它下面的画面明显暗。
8. **按住画面能拖动窗体** —— 拖动过程中画面必须跟着走（叠加窗由 tick 追上），
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
# Esc 退出全屏：应用侧是 App 级 keystroke 拦截，按 VK 码投递 WM_KEYDOWN 就能触发
# （GPUI 的 `parse_normal_key` 只认 wparam 里的虚拟键码，不需要真实键盘状态）。
VK_ESCAPE = 0x1B
WM_KEYDOWN = 0x0100
WM_KEYUP = 0x0101
# 双击：叠加窗认这一组消息（真实双击就是它们，见 `double_click`）
WM_LBUTTONDOWN = 0x0201
WM_LBUTTONUP = 0x0202
WM_LBUTTONDBLCLK = 0x0203
MK_LBUTTON = 0x0001
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
user32.PostMessageW.argtypes = [wintypes.HWND, ctypes.c_uint, wintypes.WPARAM, wintypes.LPARAM]
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
            ctypes.windll.gdi32.PtInRegion(rgn, ox + round(lx * scale), oy + round(ly * scale))
        )
    ctypes.windll.gdi32.DeleteObject(rgn)
    return inside


def wait_region(lx, ly, want, timeout=3.0):
    """等窗口区域在某个逻辑点上变成期望状态。

    改尺寸之后要过两帧才一致：先有新的布局（canvas 报新的 chrome 矩形），
    再由 tick 重建区域。中间那几帧里，胶囊所在的位置**还不在**主窗口区域里，
    点击会落到下面的视频窗上（表现为"点了没反应"，看起来像应用的 bug）。
    验收脚本必须等它稳定，否则测的是过渡态。
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        if region_contains(lx, ly) == want:
            return True
        time.sleep(0.1)
    return region_contains(lx, ly) == want


def wait_settled(timeout=3.0):
    """等一切跟上新的窗口几何：外框对齐 + 顶栏两端/胶囊都回到区域里。"""
    wait_aligned(timeout)
    wait_region(2, 17, True, timeout)
    wait_region(lw - 2, 17, True, timeout)
    wait_region(lw - 123, lh - 55, True, timeout)
    time.sleep(0.15)


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
    """在画面区双击（叠加窗收 `WM_LBUTTONDBLCLK` → 进/出全屏播放）。

    这里**不用**模拟光标：真实双击走的是
    `WM_LBUTTONDOWN → UP → WM_LBUTTONDBLCLK → UP` 这一组消息，而叠加窗正是照这个
    路径处理的，直接把同一组消息投给它即可。用 `SetCursorPos` + `mouse_event` 反而
    不稳：两次按下之间只要漏进一条 `WM_MOUSEMOVE`，叠加窗就会当成「按住画面拖窗体」
    把窗口挪走（实测窗口被挪了 12,-16，双击也随之失效）。
    """
    ox, oy = client_origin()
    wl, wt, _, _ = frame_rect(overlay)
    cx = ox + round(lx * scale) - wl
    cy = oy + round(ly * scale) - wt
    lparam = ((cy & 0xFFFF) << 16) | (cx & 0xFFFF)
    for message in (WM_LBUTTONDOWN, WM_LBUTTONUP, WM_LBUTTONDBLCLK, WM_LBUTTONUP):
        user32.PostMessageW(overlay, message, MK_LBUTTON, lparam)
        time.sleep(0.02)
    time.sleep(0.6)


def sample(img, lx, ly):
    """取一个像素；越界时返回黑色而不是抛异常。

    拖动途中截图偶尔会拿到被屏幕边缘裁掉一小块的图（`ImageGrab` 对越界的 bbox
    是裁剪语义），那时按原坐标取样会 IndexError 把整个脚本打断——判定层需要的
    是"这个点没取到彩条"这种结论，不是 traceback。
    """
    x, y = round(lx), round(ly)
    if x < 0 or y < 0 or x >= img.width or y >= img.height:
        return (0, 0, 0)
    return img.getpixel((x, y))


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

# ---- 0.5 圆角 ----------------------------------------------------------
# 视频模式两块窗口都设了 `DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND`。
# 像素判据不稳（角外是桌面，颜色不可控），这里查**属性值**证明请求发出去了，
# 再打一行角上的实测像素供人眼复核。2 = ROUND / 1 = DONOTROUND。
DWMWA_WINDOW_CORNER_PREFERENCE = 33


def corner_pref(handle):
    value = ctypes.c_uint(0)
    ctypes.windll.dwmapi.DwmGetWindowAttribute(
        wintypes.HWND(handle),
        ctypes.c_uint(DWMWA_WINDOW_CORNER_PREFERENCE),
        ctypes.byref(value),
        ctypes.sizeof(value),
    )
    return value.value


corner_main = corner_pref(hwnd)
corner_overlay = corner_pref(overlay)
rounded_ok = corner_main == 2 and corner_overlay == 2
wx, wy, _, _ = frame_rect()
corner_px = ImageGrab.grab(bbox=(wx, wy, wx + 4, wy + 4)).getpixel((1, 1))
inner_px = ImageGrab.grab(bbox=(wx + 8, wy + 8, wx + 12, wy + 12)).getpixel((1, 1))
print(
    f"  圆角：主窗={corner_main} 视频窗={corner_overlay}（2=ROUND）"
    f"  角上像素={corner_px} 往里 8px={inner_px}"
)

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
wait_settled()

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
wait_settled()

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
user32.SetForegroundWindow(hwnd)
time.sleep(0.15)
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

# ---- 5. 双击画面 → 全屏播放；Esc / 最大化按钮 → 回窗口 -----------------
#
# 双击画面是**播放器手势**（进/出全屏），不是最大化：叠加窗只把「发生了一次双击」
# 排进队列，由 100ms tick 切影院模式（窗口真的全屏 + 收起歌单）。用户报过三连：
# 双击没进全屏、全屏后回不来、Esc 不管用。
#
# 这几步放在最后：会改窗口尺寸/位置，前面的取样都按旧几何算好的。
# 先把窗口摆回**确定的位置**：上一步的拖动会让它停在任意地方（实测出现过
# 拖到屏幕左上角外侧、坐标变负数），那时按客户区原点算出来的双击点会落到别的
# 窗口上，判定就成了假失败。
def monitor_rect():
    class MONITORINFO(ctypes.Structure):
        _fields_ = [
            ("cbSize", wintypes.DWORD),
            ("rcMonitor", wintypes.RECT),
            ("rcWork", wintypes.RECT),
            ("dwFlags", wintypes.DWORD),
        ]

    mon = user32.MonitorFromWindow(hwnd, 2)  # MONITOR_DEFAULTTONEAREST
    info = MONITORINFO()
    info.cbSize = ctypes.sizeof(MONITORINFO)
    user32.GetMonitorInfoW(mon, ctypes.byref(info))
    r = info.rcMonitor
    return (r.left, r.top, r.right - r.left, r.bottom - r.top)


def is_fullscreen():
    """全屏 = 外框正好铺满所在显示器（比 IsZoomed 更贴近用户看到的样子）。"""
    return frame_rect() == monitor_rect()


def wait_fullscreen(want, timeout=4.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if is_fullscreen() == want:
            break
        time.sleep(0.1)
    time.sleep(0.45)  # 等 DWM 动画 + tick 收尾
    return is_fullscreen() == want


def band_edges(img):
    """顶栏有没有铺到窗口最左 / 最右一列。

    判据：标题带中线那一列必须**比它下面（画面本体）明显暗**——玻璃把画面压暗了。
    顶栏只铺到客户区时，最左那一列露出的是**没压暗的画面**，两者几乎同色。
    """
    y_band, y_below = 17, 52
    left = sample(img, 2, y_band)
    left_below = sample(img, 2, y_below)
    right = sample(img, img.width - 3, y_band)
    right_below = sample(img, img.width - 3, y_below)

    def dimmed(band, below):
        # 玻璃压在画面上 → 带内明显比带下的画面暗。用**三通道之和**而不是均值：
        # testsrc2 最左那根色条本来就接近黑（60,0,0），除以 3 之后差值只有 19，
        # 卡在阈值边缘会误判（顶栏其实已经铺满了）。
        sb, sd = sum(band), sum(below)
        return sb < 200 and sd - sb > 12

    ok = dimmed(left, left_below) and dimmed(right, right_below)
    return ok, (left, left_below, right, right_below)


_, _, outer_w, outer_h = frame_rect()
user32.SetWindowPos(hwnd, HWND_TOPMOST, 60, 60, outer_w, outer_h, SWP_SHOWWINDOW)
time.sleep(0.6)
refresh_geometry()
wait_settled()

base_client = client_rect()
refresh_geometry()
img_band = grab(out_dir / "floating-04-band-windowed.png")
win_band_ok, win_band_px = band_edges(img_band)
print(f"窗口态顶栏两端 = {win_band_px}   ← 左边带内/带下、右边带内/带下")

fs_entered = False
for attempt in range(1, 3):
    double_click(lw * 0.5, lh * 0.45)
    fs_entered = wait_fullscreen(True)
    if fs_entered:
        break
    print(f"  第 {attempt} 次双击没进全屏，重试")
refresh_geometry()
wait_settled()
cw1, ch1 = client_rect()
img_fs = grab(out_dir / "floating-05-fullscreen.png")
fs_mid = sample(img_fs, lw / 2, lh / 2)
fs_band_ok, fs_band_px = band_edges(img_fs)
fs_aligned = check_aligned("全屏")
print(
    f"双击画面 → 全屏 {fs_entered}；client = {cw1}x{ch1}（窗口态 {base_client[0]}x{base_client[1]}）；"
    f"画面中心 = {fs_mid}"
)
print(f"全屏顶栏两端 = {fs_band_px}")

esc_ok = False
for attempt in range(1, 3):
    user32.PostMessageW(hwnd, WM_KEYDOWN, VK_ESCAPE, 0x00010001)
    time.sleep(0.06)
    user32.PostMessageW(hwnd, WM_KEYUP, VK_ESCAPE, 0xC0010001)
    esc_ok = wait_fullscreen(False)
    if esc_ok:
        break
    print(f"  第 {attempt} 次 Esc 没回窗口，重试")
refresh_geometry()
wait_settled()
cw2, ch2 = client_rect()
esc_aligned = check_aligned("Esc 之后")
print(f"Esc → 回窗口 {esc_ok}；client = {cw2}x{ch2}（窗口态 {base_client[0]}x{base_client[1]}）")

# 全屏态下点「最大化」按钮：全屏时它的语义是「回到窗口」（GPUI 全屏时已经把
# WS_MAXIMIZEBOX/WS_CAPTION 摘掉了，此时再 SW_MAXIMIZE 只会卡在错乱态）。
fs_again = False
for attempt in range(1, 3):
    double_click(lw * 0.5, lh * 0.45)
    fs_again = wait_fullscreen(True)
    if fs_again:
        break
    print(f"  第 {attempt} 次双击没进全屏（第二次），重试")
refresh_geometry()
wait_settled()
max_exits = False
for attempt in range(1, 3):
    click(lw - 46 - 23, 17)
    max_exits = wait_fullscreen(False)
    if max_exits:
        break
    print(f"  第 {attempt} 次点「最大化」没回窗口，重试")
refresh_geometry()
wait_settled()
cw3, ch3 = client_rect()
max_aligned = check_aligned("全屏里点最大化之后")
print(f"全屏态点「最大化」→ 回窗口 {max_exits}；client = {cw3}x{ch3}")

grab(root / "target" / "floating-06-restored.png")

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
if not rounded_ok:
    print(
        f"FAIL: 视频模式没设圆角（主窗={corner_main} 视频窗={corner_overlay}，期望都是 2）"
    )
    ok = False
else:
    print(f"OK: 视频模式两块窗口都设了圆角（角上像素 {corner_px}，不是画面本体）")
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
if not win_band_ok:
    print(f"FAIL: 窗口态顶栏没铺满宽度（两端取样 {win_band_px}）")
    ok = False
else:
    print(f"OK: 窗口态顶栏铺到窗口最左/最右一列 {win_band_px}")
if not fs_entered:
    print(f"FAIL: 双击画面没进全屏播放（client 仍是 {cw1}x{ch1}，外框 {frame_rect()}）")
    ok = False
elif not colorful(fs_mid):
    print(f"FAIL: 全屏后画面中心不是彩条（{fs_mid}），叠加窗没跟着铺满")
    ok = False
elif not fs_aligned:
    print("FAIL: 全屏后视频窗和主窗外框没对齐")
    ok = False
elif not fs_band_ok:
    print(f"FAIL: 全屏态顶栏没铺满宽度（两端取样 {fs_band_px}）")
    ok = False
else:
    print(f"OK: 双击画面进全屏 {cw1}x{ch1}，画面铺满 {fs_mid}，顶栏也铺满")
if not esc_ok:
    print(f"FAIL: 全屏里按 Esc 没回窗口（仍是全屏，client={cw2}x{ch2}）")
    ok = False
elif (cw2, ch2) != (base_client[0], base_client[1]):
    # 客户区要回到「窗口态」的尺寸：GPUI 退出全屏时不重算客户区，卡在显示器
    # 尺寸就说明 tick 里的自愈（client_matches_window/refresh_client_area）没生效。
    print(f"FAIL: Esc 之后客户区没回到窗口态（{cw2}x{ch2}，期望 {base_client}）")
    ok = False
elif not esc_aligned:
    print("FAIL: Esc 之后视频窗和主窗外框没对齐")
    ok = False
else:
    print(f"OK: Esc 从全屏回窗口 {cw2}x{ch2}（窗口态 {base_client[0]}x{base_client[1]}）")
if not fs_again:
    print("FAIL: 第二次双击没进全屏，最大化按钮那一步没验到")
    ok = False
elif not max_exits:
    print(f"FAIL: 全屏态点「最大化」没回窗口（client={cw3}x{ch3}）")
    ok = False
elif not max_aligned:
    print("FAIL: 全屏里点「最大化」之后视频窗和主窗外框没对齐")
    ok = False
else:
    print(f"OK: 全屏态点「最大化」回窗口 {cw3}x{ch3}")
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
