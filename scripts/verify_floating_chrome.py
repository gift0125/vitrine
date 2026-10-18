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
9. **方向键接通播放器** —— 左右 ±5 秒（`seek`）、上下 ±5% 音量
   （`set_property volume`）。判据取 mpv 的 IPC trace：只看界面动没动不够，
   那可能只是 tick 的乐观更新；命令真的发出去才算接通。
10. **ⓘ 信息卡压在画面上** —— 点开之后左下角那块必须从"画面"变成"深玻璃"，
    而且那两个角点还在主窗口区域里（区域没把它加回去的话，卡片会被叠加窗盖住，
    用户看到的是"点了没反应"）；再点一次收起，那块要回到画面。
11. **字幕 / 音轨按钮接线通** —— 判据取 IPC trace 里的 `cycle sid/aid`：
    素材只有一条轨时 mpv 什么也不做，"点了没反应"和"切到了"肉眼分不出。

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
# 时长要够：整套流程要跑 50 秒上下（全屏/回窗口/拖窗每一步都要等窗口稳定），
# 素材播完之后叠加窗会被隐藏，后面的取样全变成主窗底色——"按住画面拖窗"就成了
# "点在一块 GPUI 画布上"，歌单/方向键也一起假失败。仓库里的样片只有 5 秒，
# 所以这里用一段现生成的 2 分钟长片（落在 target/，不进仓库）：
#   mpv av://lavfi:testsrc2=duration=120:size=1280x720:rate=24 \
#       --o=target/verify-long.mp4 --ovc=libx264 --oac=libmp3lame --of=mp4
media = root / "target" / "verify-long.mp4"
if not media.exists():
    # 注意：这里的 mpv.exe **只是用来合成测试素材**的工具，和播放内核无关
    # （应用现在进程内加载 libmpv-2.dll，不再起 mpv 子进程）。找不到就退到仓库里
    # 的样片，验收里"等画面稳定"的余量会变小。
    candidates = [
        os.environ.get("SMP_MPV_PATH"),
        r"C:\ProgramData\chocolatey\lib\mpvio.install\tools\mpv.exe",
        r"C:\Program Files\mpv\mpv.exe",
        str(root / "mpv.exe"),
        "mpv",
    ]
    mpv = next((c for c in candidates if c and (c == "mpv" or Path(c).exists())), "mpv")
    subprocess.run(
        [
            mpv,
            "av://lavfi:testsrc2=duration=120:size=1280x720:rate=24",
            f"--o={media}",
            "--ovc=libx264",
            "--oac=libmp3lame",
            "--of=mp4",
            "--really-quiet",
        ],
        check=False,
    )
if not media.exists():
    media = root / "test-media" / "gen-1080p.mp4"

user32 = ctypes.windll.user32
# Esc 退出全屏：应用侧是 App 级 keystroke 拦截，按 VK 码投递 WM_KEYDOWN 就能触发
# （GPUI 的 `parse_normal_key` 只认 wparam 里的虚拟键码，不需要真实键盘状态）。
VK_ESCAPE = 0x1B
# 方向键：左右 ±5 秒、上下 ±5% 音量，走同一个拦截（`PlayerView::intercept_keystrokes`）
VK_LEFT = 0x25
VK_UP = 0x26
VK_RIGHT = 0x27
VK_DOWN = 0x28
WM_KEYDOWN = 0x0100
WM_KEYUP = 0x0101
# 控制栏几何（逻辑像素，必须和 crates/app/src/player_view.rs 的常数一致）：
# 胶囊固定 760×92、距窗口下沿 24、内边距 20/8；进度区两行（时间行 16 + 4 + 轨道 4）；
# ⓘ 在胶囊内左侧，信息卡 240×180 浮在胶囊上方 16。写死这些数字是因为验收脚本按
# **像素**判定，拿不到应用的布局树；对不上就说明布局被改过，应该同步这里。
CAPSULE_WIDTH = 760.0
CAPSULE_HEIGHT = 92.0
CAPSULE_INSET = 24.0
CAPSULE_PAD_X = 20.0
CAPSULE_PAD_Y = 8.0
PROGRESS_HEIGHT = 24.0
CONTROLS_ROW = 44.0
# 走带行中心距胶囊上沿的距离：8（padding）+ 24（进度区）+ 8（间距）+ 22（半个走带行）
CONTROLS_Y_OFFSET = CAPSULE_PAD_Y + PROGRESS_HEIGHT + 8 + CONTROLS_ROW / 2
INFO_WIDTH = 240.0
INFO_HEIGHT = 180.0
INFO_GAP = 16.0
PLAYLIST_WIDTH = 320.0
PLAYLIST_INSET = 24.0


def capsule_left():
    """胶囊左沿（逻辑 x）。

    歌单展开时控制栏**居中于剩余画面**，不是整窗居中：spec
    `playlist_panel.expanded_state_control_capsule.x = 88`，即可用区
    `1280 − 320 − 24 = 936` → `(936 − 760) / 2 = 88`。胶囊左沿跟着面板走，
    这是"展开态"唯一和静止态不同的地方。

    以前这里写死成整窗居中，于是"再点一次收起歌单"那一下点到胶囊外面去了：
    面板一直开着，后面每个按钮的坐标都偏 344（= 320 + 24），连着报出
    "字幕/音轨没反应""ⓘ 没反应"一串假失败——真正的原因只有这一个。
    """
    avail = lw - (PLAYLIST_WIDTH + PLAYLIST_INSET) if panel_open() else lw
    return (avail - CAPSULE_WIDTH) / 2


def controls_center_y():
    """走带行中心（逻辑 y）。胶囊贴窗口下沿 24，行中心距胶囊上沿 62（见常数注释）。"""
    return lh - (CAPSULE_INSET + CAPSULE_HEIGHT - CONTROLS_Y_OFFSET)


def right_group_button(index_from_right):
    """控制栏右组第 N 个按钮的中心（0 = 最右那枚，从右往左数）。

    右组从「胶囊右沿 − padding 20」开始往左排：按钮盒 24、间距 16 → 中心距 40，
    所以第 N 个的中心是 `右沿 − 12 − N×40`。
    视频模式下的顺序（右→左）：全屏 0、列表 1、主题 2、音轨 3、字幕 4。

    以前按「胶囊铺满整宽」写成 `lw - 123`：胶囊收窄之后那个点落到胶囊外的
    画面上，点击被叠加窗吃掉——表现为"点列表没反应"，实际是根本没点中。
    """
    group_right = capsule_left() + CAPSULE_WIDTH - CAPSULE_PAD_X
    return group_right - 12 - index_from_right * 40, controls_center_y()


def playlist_button():
    """「列表」按钮的中心（逻辑坐标）。视频模式下它在最右内侧第二位。"""
    return right_group_button(1)

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
# 方向键有没有真的落到播放器上，只能看 mpv 的 IPC trace：进度条/音量圆点就算不动，
# 也可能只是 tick 的乐观更新，反过来也一样（声音变了、界面没动）。
mpv_trace = root / "target" / "verify-mpv-trace.log"
mpv_trace.unlink(missing_ok=True)
env = dict(os.environ, SMP_DATA_DIR=str(data_dir), SMP_MPV_TRACE=str(mpv_trace))
# 视频内核是进程内 libmpv，DLL 必须能被 exe 找到。打包目录里自带一份；直接对
# target/debug 或 target/release 跑时可能还没拷，这里退到 target/mpv-dev（fetch-libmpv.ps1
# 的默认落地位置），免得"缺 DLL"被误判成"视频功能坏了"。
if not (exe.parent / "libmpv-2.dll").is_file():
    for guess in (root / "target" / "mpv-dev" / "libmpv-2.dll",
                  root / "target" / "release" / "libmpv-2.dll",
                  root / "target" / "debug" / "libmpv-2.dll"):
        if guess.is_file():
            env.setdefault("SMP_LIBMPV_PATH", str(guess))
            print(f"note: exe 目录没有 libmpv-2.dll，用 SMP_LIBMPV_PATH={guess}")
            break
    else:
        print("warn: 找不到 libmpv-2.dll，视频相关项会失败（跑 scripts/fetch-libmpv.ps1）")

# 上一轮没收干净的进程会让 `FindWindowW` 拿到一个已经死掉的句柄
subprocess.run(["taskkill", "/F", "/IM", "simple-media-player.exe"], capture_output=True)
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


def panel_open():
    """歌单面板是否展开。**只能**在 `wait_settled()` 之后问：

    区域还没按视频区算出来时整窗都在区域里，这个判断会假通过。
    取样点落在面板中线上——展开时那里是浅色面板底，收起时是画面。
    """
    return region_contains(lw - PLAYLIST_INSET - PLAYLIST_WIDTH / 2, 120)


def wake():
    """把控制层叫回来（播放中 2.5s 没有输入它就淡出了）。

    自动化脚本每两步之间常静置 1~3 秒，正好落进淡出窗口：这时主窗口的区域已经
    收缩成 1 像素，点击**直接落到下层叠加窗**上——轻则"点了没反应"，重则开始拖窗
    （窗口被挪走，后面所有坐标全错），或者被当成双击进了全屏。所以每次点击/取样
    之前先晃一下光标；0.45s 是淡入时间（4 个 100ms tick）。
    """
    ox, oy = client_origin()
    cx, cy = ox + round(lw * 0.5 * scale), oy + round(lh * 0.5 * scale)
    user32.SetCursorPos(cx - 30, cy)
    time.sleep(0.05)
    user32.SetCursorPos(cx, cy)
    time.sleep(0.45)


def wait_settled(timeout=3.0):
    """等一切跟上新的窗口几何：外框对齐 + 顶栏两端/胶囊都回到区域里。

    关键是那条 `False`：**画面中心必须是洞**。窗口刚被改过尺寸时，应用还没按
    新的视频区重算区域，`GetWindowRgn` 会短暂返回"整个窗口"——这时
    `region_contains(任意点)` 恒为 True，后面所有"面板/胶囊在不在区域里"的
    判断都会假通过（歌单明明没展开也判通过了，实测踩过）。
    """
    wake()
    wait_aligned(timeout)
    wait_region(lw * 0.5, lh * 0.45, False, timeout)
    wait_region(2, 17, True, timeout)
    wait_region(lw - 2, 17, True, timeout)
    wait_region(*playlist_button(), True, timeout)
    time.sleep(0.15)


def press_key(vk):
    """往主窗口投递一次真实按键（按下 + 抬起）。

    GPUI 在 `WM_KEYDOWN` 时就组 `Keystroke` 并跑 App 级拦截，所以 wparam 里的
    虚拟键码是唯一要紧的东西；`lparam` 按「重复次数 1 + 扫描码」随便填能过。
    """
    user32.PostMessageW(hwnd, WM_KEYDOWN, vk, 0x00010001)
    time.sleep(0.06)
    user32.PostMessageW(hwnd, WM_KEYUP, vk, 0xC0010001)


def trace_hits(needle):
    """mpv trace 里包含 `needle` 的行数（文件没生成时算 0）。"""
    if not mpv_trace.exists():
        return 0
    return sum(
        1
        for line in mpv_trace.read_text(encoding="utf-8", errors="replace").splitlines()
        if needle in line
    )


def wait_trace(needle, count, timeout=3.0):
    """等 trace 里 `needle` 的行数达到 `count`，返回实际条数。

    点击到"命令真的发出去"之间隔着：PostMessage → GPUI 命中测试 → 事件处理 →
    tick。进程内 libmpv 起播要初始化 hwdec（实测 3~4 秒首帧），这段时间 UI 线程
    的事件处理会变慢，固定 sleep 0.6s 取样会时不时漏掉刚发出去的命令。
    """
    deadline = time.time() + timeout
    hits = trace_hits(needle)
    while time.time() < deadline:
        hits = trace_hits(needle)
        if hits >= count:
            return hits
        time.sleep(0.1)
    return hits


def wait_darker(pt, base, timeout=2.5, gap=80):
    """等 `pt` 比 `base` 明显更暗（深玻璃卡片画上去了），返回最后一次采样。

    判据同判定段：玻璃卡三通道之和比画面低一大截，用亮度差比用具体颜色稳
    （换素材不会飘）。
    """
    deadline = time.time() + timeout
    px = sample(grab(), *pt)
    while time.time() < deadline:
        px = sample(grab(), *pt)
        if sum(px[:3]) + gap < sum(base[:3]):
            return px
        time.sleep(0.1)
    return px


def wait_region_clear(lx, ly, timeout=2.5):
    """等某个点**离开**主窗口区域（卡片收起后那块交还给画面）。"""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if not region_contains(lx, ly):
            return True
        time.sleep(0.1)
    return not region_contains(lx, ly)


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


CHROME_MISSES = []


def click(lx, ly):
    """真实光标点击（只用于主窗口上的 chrome：标题栏按钮 / 胶囊 / 面板）。

    不能只 PostMessage：GPUI 的命中测试用的是光标位置，投递的 lparam 不总被采信
    （这条踩过坑，记在项目备忘里）。这里窗口位置稳定，直接用真实输入。
    """
    # 控制层可能已经淡出（见 wake）：先叫回来，等区域恢复再谈坐标
    wake()
    ox, oy = client_origin()
    sx, sy = ox + round(lx * scale), oy + round(ly * scale)
    # 打一下这一点到底属于哪个窗口：区域没减对时，点击会被 mpv 那一层吃掉，
    # 表现为「点了没反应」，光看截图看不出来。
    hit = user32.WindowFromPoint(wintypes.POINT(sx, sy))
    buf = ctypes.create_unicode_buffer(128)
    user32.GetClassNameW(hit, buf, 128)
    print(f"  点击 逻辑({lx:.0f},{ly:.0f}) → 屏幕({sx},{sy}) 窗口={hit} 类='{buf.value}'")
    if buf.value != "Zed::Window":
        # 记下来在判定段汇总：坐标偏出胶囊时点击会落到下层视频叠加窗上，
        # 界面毫无反应，而截图看不出差别（这一轮就吃了"坐标偏 344"的亏）。
        CHROME_MISSES.append((round(lx), round(ly), buf.value))
        print(f"  ⚠ 该点不在主窗（落到 {buf.value}），按钮必然点不中——坐标和布局对不上")
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
expect = EXPECTED_PIXELS.get(media.name)


def wait_client_size(target, timeout=15.0, tol=2):
    """等客户区变成目标尺寸。

    起播后窗口要按画面调一次尺寸，而这一步依赖"解码出首帧后报出的 dwidth/dheight"；
    进程内 libmpv 还要先初始化 hwdec（d3d11va），实测要 3~4 秒才报出真实尺寸。
    只等"尺寸稳定"会在起播前就返回那个旧的默认尺寸，看起来像"没调窗口"。
    """
    deadline = time.time() + timeout
    last = client_rect()
    while time.time() < deadline:
        last = client_rect()
        if abs(last[0] - target[0]) <= tol and abs(last[1] - target[1]) <= tol:
            return last
        time.sleep(0.1)
    print(f"  等待超时：{timeout:.0f}s 内没等到 {target[0]}x{target[1]}（当前 {last[0]}x{last[1]}）")
    return wait_client_settled()


fit_client = wait_client_size(expect) if expect else wait_client_settled()
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


def toggle_playlist(want_open, tries=5):
    """把歌单切到目标状态，最多试 `tries` 次。

    单次真实点击偶尔会被系统吞掉（`mouse_event` 的 down/up 与 GPUI 的
    激活/命中是两套时序），这不是应用的问题；这里按**结果**重试。
    每次先看当前状态再决定要不要点——盲目连点会在两次点击都成功时互相抵消。
    按钮位置也必须每次重算：展开态胶囊是左移的（`capsule_left()`）。
    """
    for _ in range(tries):
        if panel_open() == want_open:
            return True
        click(*playlist_button())
        deadline = time.time() + 1.0
        while time.time() < deadline:
            if panel_open() == want_open:
                return True
            time.sleep(0.1)
    return panel_open() == want_open


def refresh_geometry():
    """重新读一次客户区，更新 lw/lh（最大化/还原之后必须重读，否则取样越界）。"""
    global lw, lh
    cw, ch = wait_client_settled()
    lw, lh = cw / scale, ch / scale
    return cw, ch


# ---- 3. 展开歌单：面板浮在右侧，画面不被挤窄 ---------------------------
opened = toggle_playlist(True)
img2 = grab(out_dir / "floating-02-playlist.png")
panel_mid = lw - PLAYLIST_INSET - PLAYLIST_WIDTH / 2
panel_px = sample(img2, panel_mid, 120)
print(f"歌单面板内 (.., 120) = {panel_px}   ← 应为面板底色（浅）")
print(f"歌单展开后画面中心 = {sample(img2, lw / 2, lh / 2)}")
# 面板浮在画面上，但它**不在**原来的窗口区域里：区域必须把它加回去（否则被剪掉）。
panel_inside = region_contains(panel_mid, 120)
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

# ---- 6. 方向键：左右 ±5 秒、上下 ±5% 音量 -------------------------------
#
# 判据落在 mpv 的 IPC trace 上：界面上的进度条/音量圆点就算没动，也可能只是
# tick 的乐观更新；反过来"声音变了界面没动"同样是 bug。只有 `seek` /
# `set_property volume` 这条命令真的发出去了才算接通。
seeks_before = trace_hits('"seek"')
vols_before = trace_hits('"volume"')
press_key(VK_RIGHT)
seeks_after = wait_trace('"seek"', seeks_before + 1)
press_key(VK_UP)
vols_after = wait_trace('"volume"', vols_before + 1)
seek_keys_ok = seeks_after > seeks_before
volume_keys_ok = vols_after > vols_before
print(
    f"方向键 → seek 命令 {seeks_before}→{seeks_after} 条，"
    f"volume 命令 {vols_before}→{vols_after} 条"
)

# ---- 6.5 字幕 / 音轨按钮接通 mpv ---------------------------------------
#
# mpv 的 `cycle sid/aid` 没有返回值，素材里只有一条轨时它**什么也不做**——
# 界面上"点了没反应"和"切到了"长得一样。所以判据同样落在 IPC trace 上：
# 两条 cycle 命令真的发出去了，说明按钮到引擎这段是通的（应用会给 800ms 的
# 超时提示"没有其他字幕轨可切"，那是取值的表现，不是接线问题）。
def click_until_cycle(point, want, tries=3):
    """点到 trace 里出现第 `want` 条 cycle 为止（合成长按偶发被吞，按结果重试）。

    判据仍是"两条命令真的发出去了"，重试只解决输入层丢事件——应用侧从来没错。
    每次重试前等 0.7s：两个按钮只隔 40 逻辑像素，落在 `SM_CXDOUBLECLK`(36) 与
    `GetDoubleClickTime`(500ms) 里会被判成双击，第二次点击就不当单击处理。
    """
    for _ in range(tries):
        hits = trace_hits('"cycle"')
        if hits >= want:
            return hits
        click(*point)
        hits = wait_trace('"cycle"', want, timeout=1.5)
        if hits >= want:
            return hits
        time.sleep(0.7)
    return trace_hits('"cycle"')


cycles_before = trace_hits('"cycle"')
click_until_cycle(right_group_button(3), cycles_before + 1)  # 音轨
time.sleep(0.8)
cycles_after = click_until_cycle(right_group_button(4), cycles_before + 2)  # 字幕
cycle_ok = cycles_after >= cycles_before + 2
print(f"字幕/音轨 → cycle 命令 {cycles_before}→{cycles_after} 条")

# ---- 7. ⓘ 视频信息卡：贴左下、压在画面上、再点收起 ----------------------
#
# 卡片是「画在画面上」的东西，所以踩的坑和胶囊一样：不进窗口区域就被叠加窗整块
# 盖住。判据用像素而非 `try_find`：区域没减对时节点是在的，只是用户看不见。
cw4, ch4 = client_rect()
cw4, ch4 = cw4 / scale, ch4 / scale
# ⓘ 是左侧组第一枚：胶囊左沿 + padding 20 + 半个 20 盒。胶囊左沿不是 `x = 24`
# 也不是整窗居中——歌单展开时它会左移（这里面板是收起的，但仍然走同一个函数，
# 免得以后又写死一份）。
info_pt = (capsule_left() + CAPSULE_PAD_X + 10, controls_center_y())
# 胶囊底边 24 + 胶囊高 92 + 间距 16 → 卡片底边，再往上 180 是顶边
CARD_TOP = ch4 - (CAPSULE_INSET + CAPSULE_HEIGHT + INFO_GAP + INFO_HEIGHT)
CARD_LEFT = CAPSULE_INSET
card_probe = (CARD_LEFT + INFO_WIDTH / 2, CARD_TOP + INFO_HEIGHT / 2)  # 卡片正中
wait_region(*info_pt, True)
info_before = sample(grab(out_dir / "verify-info-0-before.png"), *card_probe)


def info_toggle(want_open, tries=3):
    """点 ⓘ 直到卡片真的到了目标状态，返回 (是否到位, 最后一次采样)。

    和歌单一样按**结果**重试：单次真实点击偶尔会被系统吞掉（这一轮就漏了第一次，
    卡片在第二次点击才出来），判据不变——三次都点不动才算应用的问题。
    """
    px = info_before
    for attempt in range(1, tries + 1):
        click(*info_pt)
        if want_open:
            px = wait_darker(card_probe, info_before)
            if sum(px[:3]) + 80 < sum(info_before[:3]):
                return True, px
        else:
            cleared = wait_region_clear(CARD_LEFT + 10, CARD_TOP + 10)
            px = sample(grab(), *card_probe)
            if cleared:
                return True, px
        print(f"  第 {attempt} 次点 ⓘ 没{'展开' if want_open else '收起'}卡片，重试")
    return False, px


# 轮询到卡片画上去为止（固定 sleep 会拍到"点击还没被处理"的那一帧）
info_opened, info_open_px = info_toggle(True)
grab(out_dir / "verify-info-1-open.png")
# 两个角点都要归主窗口：左上角的文字区 + 右下角的空白玻璃
info_in_region = region_contains(CARD_LEFT + 10, CARD_TOP + 10) and region_contains(
    CARD_LEFT + INFO_WIDTH - 10, CARD_TOP + INFO_HEIGHT - 10
)
# 收起同理：等那块真的从区域里退出去，别按 0.45s 猜
info_closed_ok, info_closed_px = info_toggle(False)
info_hole_gone = info_closed_ok
print(
    f"信息卡 {card_probe} 采样：之前 {info_before} → 展开 {info_open_px}"
    f"（到位={info_opened}）→ 收起 {info_closed_px}（到位={info_closed_ok}）；"
    f"区域 展开={info_in_region} 收起={info_hole_gone}"
)

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
elif not closed:
    print(
        "FAIL: 再点一次「列表」没收起歌单——展开态胶囊居中于剩余画面"
        "（`capsule_left()`），坐标按整窗居中算就会点到胶囊外面"
    )
    ok = False
else:
    print(f"OK: 歌单面板浮在画面右侧 {panel_px}，区域已把它加回去，再点收回")
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
if not seek_keys_ok:
    print("FAIL: 右键没把 seek 发给 mpv（方向键没接通）")
    ok = False
else:
    print("OK: 左右键真的把 seek 发给了 mpv")
if not volume_keys_ok:
    print("FAIL: 上键没把 volume 发给 mpv（方向键没接通）")
    ok = False
else:
    print("OK: 上下键真的把 volume 发给了 mpv")
if not cycle_ok:
    print(f"FAIL: 字幕/音轨按钮没把 cycle 发给 mpv（{cycles_before}→{cycles_after}）")
    ok = False
else:
    print("OK: 字幕/音轨按钮各把一条 cycle 发给了 mpv")
# 深玻璃 ≈ (27,29,20)，彩条那一块和 ≈ 300+：用亮度差而不是颜色值，换素材也不飘
info_dark = sum(info_open_px[:3]) + 80 < sum(info_before[:3])
if not info_dark or not info_opened:
    print(f"FAIL: 点 ⓘ 之后那块还是画面（{info_before} → {info_open_px}，卡片没画出来）")
    ok = False
elif not info_in_region:
    print("FAIL: 信息卡两个角点不在主窗口区域里（会被叠加窗盖住）")
    ok = False
elif not info_hole_gone:
    print(f"FAIL: 再点一次 ⓘ 没收回信息卡（那块仍在窗口区域里，{info_closed_px}）")
    ok = False
else:
    print(f"OK: ⓘ 信息卡压住画面（{info_before} → {info_open_px}），再点收回 {info_closed_px}")
if CHROME_MISSES:
    # 坐标和布局对不上时点击会静默落到叠加窗上，症状是"点了没反应"——先报这个，
    # 否则上面那些 FAIL 会被误当成功能坏掉。
    print(f"FAIL: {len(CHROME_MISSES)} 次点击没落在主窗上（首次 {CHROME_MISSES[0]}）")
    ok = False
print("ALL OK" if ok else "SOME CHECKS FAILED")
