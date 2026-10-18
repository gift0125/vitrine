//! Win32 顶层窗口：mpv 以 `--wid` 挂载于此，**垫在 GPUI 主窗口下面**。
//!
//! ## 为什么需要它
//!
//! GPUI 的 Windows 后端使用 DirectComposition（窗口带 `WS_EX_NOREDIRECTIONBITMAP`），
//! 这种窗口没有重定向表面，**无法承载原生子窗口**。所以视频不能画在 GPUI 窗口内部，
//! 只能由一个「无边框、不抢焦点」的顶层弹窗承载，位置尺寸由主窗口跟着对齐。
//!
//! ## 为什么在主窗口下面（2026-10-18 第七轮）
//!
//! 之前它盖在主窗口**上面**，靠给自己的窗口区域开洞把标题带/胶囊/歌单露出来。
//! 那条路能让 chrome「浮」在画面上，但 chrome 背后只有主窗口的不透明表面，
//! 玻璃永远是实心黑板。现在反过来：**主窗口逐像素透明，画面垫在它下面**，
//! 于是同一句 `overlay::glass()`（`#0B0C10` @72%）就真的透出画面了。
//!
//! ## 但这里有个必须知道的坑：被完全盖住的窗口会停止出帧
//!
//! 实测（同一个 mpv、同一份文件）：
//!
//! - 视频窗垫在主窗口下面、主窗**整块**盖着它 → 画面一片空白，PrintWindow 取内容也是黑的；
//! - 把视频窗挪到没被盖住的地方 → 画面立刻正常；
//! - 在主窗口的窗口区域上挖个 300×300 的洞 → 画面正好出现在那个洞里。
//!
//! 结论：系统只认**窗口矩形/区域**，不认逐像素透明度。它认为视频窗 100% 被遮挡，
//! mpv 那边的呈现就停在那儿不出帧了（关掉 d3d11 flip 也没用）。所以主窗口必须把
//! 自己的**画面区从窗口区域里减掉**（见 [`set_region`]），只留下 chrome 那几块：
//! 画面露出来才会出帧，而 chrome 那几块是半透明的，于是「玻璃底下透出画面」成立。
//!
//! ## 输入因此要转一手
//!
//! 画面区不在主窗口的区域里了，点在上面命中的是视频窗。它只是个画布（不抢焦点、
//! 也没有 GPUI），所以替主窗口挡三件事：
//!
//! - 按住画面 → 自己实现窗口拖动（位移一比一，叠加窗同步粘上去）；
//! - 双击画面 → **全屏播放 / 退出全屏**（播放器的通用手势；[`OverlayEvent`]
//!   交给 GPUI 侧执行，见下）；
//! - 文件拖进来 → `WM_DROPFILES` 收进队列，主线程 tick 取走。
//!
//! 双击这里**不能**再等价于双击标题栏（最大化）：用户要的是「双击画面看全屏」，
//! 最大化有标题带右上角那个按钮。
//!
//! ## 坐标约定
//!
//! 布局是逻辑像素（相对客户区）。交给 Win32 的矩形要么是**物理屏幕像素**
//! （`set_rect`），要么是**物理像素、相对窗口矩形**（`set_region`），换算由调用方做。

use std::ffi::c_void;
use std::sync::Once;

/// Win32 窗口句柄。
pub type Hwnd = isize;

const WS_POPUP: u32 = 0x8000_0000;
const WS_CLIPCHILDREN: u32 = 0x0200_0000;
const WS_CLIPSIBLINGS: u32 = 0x0400_0000;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;

// 注意：**不要**给这块窗口加 `WS_EX_TRANSPARENT`。听着像「命中测试穿透」，
// 实测会让整块窗口不再被合成——画面直接消失（帧还在，就是看不见）。
// 点击本来就得它来接（见模块头「输入因此要转一手」）。

const SW_HIDE: i32 = 0;
const SW_SHOWNA: i32 = 8;

const WM_ERASEBKGND: u32 = 0x0014;
/// 文件拖放：画面区占窗口 80% 的面积，而 GPUI 的 `IDropTarget` 注册在**主窗口**上。
/// 画面区不在主窗口的区域里，OLE 找到的投放目标就是这块窗口，所以它自己接一份。
const WM_DROPFILES: u32 = 0x0233;
/// 按住画面拖动窗体：自己实现，不转交 `HTCAPTION` 的模态拖拽。
///
/// 曾经是 `ReleaseCapture()` + `SendMessageW(main, WM_NCLBUTTONDOWN, HTCAPTION, 0)`
/// 让系统接管（贴边吸附白拿），实测在这个窗口上不可靠：拖动只跟上头一两步就停住
/// （GPUI 的 wndproc 自己吃掉 `WM_MOUSEMOVE`，系统那套模态循环拿不到位移），
/// 紧接着的双击还会被当成"继续拖动"，窗口挪一段而不是最大化。
/// 所以这里自己记起点、自己 `SetWindowPos`：位移一比一、叠加窗同步粘住。
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
/// 双击画面：全屏播放 / 退出全屏。
///
/// 前提是窗口类带 `CS_DBLCLKS`——没有它系统只会连发两条 `WM_LBUTTONDOWN`，
/// `WM_LBUTTONDBLCLK` 永远不来（踩过）。
const WM_LBUTTONDBLCLK: u32 = 0x0203;
const WM_CAPTURECHANGED: u32 = 0x0215;

/// 窗口类样式：不设这一位就收不到双击消息。
const CS_DBLCLKS: u32 = 0x0008;

const BLACK_BRUSH: i32 = 4;

/// `GetWindow` 的「z 序里下一个（更靠下的）窗口」。
const GW_HWNDNEXT: u32 = 2;

const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;
const SWP_SHOWWINDOW: u32 = 0x0040;

/// 主窗口区域里**要留下**的一块 chrome（**逻辑像素**，相对客户区左上角）。
///
/// 画面区要从主窗口的区域里减掉，而 chrome 是浮在画面上的——它们必须再加回区域，
/// 否则自己也被剪掉，看不见。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromeRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// 圆角半径；0 = 直角
    pub radius: f32,
}

impl ChromeRect {
    pub fn rect(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            radius: 0.0,
        }
    }

    pub fn rounded(x: f32, y: f32, width: f32, height: f32, radius: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            radius,
        }
    }
}

const CLASS_NAME: &str = "SimpleMediaPlayerVideoOverlay";

/// 画面上发生的、要交给 GPUI 侧执行的交互。
///
/// wndproc 是自由函数，既不在 GPUI 的 `App` 上下文里、也拿不到 `Window`（调不了
/// `toggle_fullscreen`），改不动视图状态。所以和拖放的路径一样：**先落队列**，
/// 由主线程的 100ms tick 取走执行。代价是延迟不超过一帧（≤100ms），
/// 用户感知不到——但代码里必须清楚「双击画面」是在下一次 tick 才生效的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayEvent {
    /// 双击画面：进入 / 退出全屏播放（影院模式）
    ToggleCinema,
}

#[cfg(windows)]
mod win {
    use super::*;

    #[repr(C)]
    struct WndClassW {
        style: u32,
        lpfn_wnd_proc: usize,
        cb_cls_extra: i32,
        cb_wnd_extra: i32,
        h_instance: Hwnd,
        h_icon: Hwnd,
        h_cursor: Hwnd,
        hbr_background: Hwnd,
        lpsz_menu_name: *const u16,
        lpsz_class_name: *const u16,
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct Point {
        pub x: i32,
        pub y: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn RegisterClassW(class: *const WndClassW) -> u16;
        fn CreateWindowExW(
            ex_style: u32,
            class_name: *const u16,
            window_name: *const u16,
            style: u32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: Hwnd,
            menu: Hwnd,
            instance: Hwnd,
            param: *mut c_void,
        ) -> Hwnd;
        fn DestroyWindow(hwnd: Hwnd) -> i32;
        fn ShowWindow(hwnd: Hwnd, cmd: i32) -> i32;
        fn SetWindowPos(
            hwnd: Hwnd,
            insert_after: Hwnd,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
        fn IsWindow(hwnd: Hwnd) -> i32;
        fn IsIconic(hwnd: Hwnd) -> i32;
        fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn ClientToScreen(hwnd: Hwnd, point: *mut Point) -> i32;
        fn GetClientRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        fn GetWindowRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        fn FillRect(hdc: Hwnd, rect: *const Rect, brush: Hwnd) -> i32;
        fn GetWindowLongPtrW(hwnd: Hwnd, index: i32) -> isize;
        fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: isize) -> isize;
        fn GetWindow(hwnd: Hwnd, cmd: u32) -> Hwnd;
        fn ReleaseCapture() -> i32;
        fn SetCapture(hwnd: Hwnd) -> Hwnd;
        fn SetForegroundWindow(hwnd: Hwnd) -> i32;
        fn IsZoomed(hwnd: Hwnd) -> i32;
        fn SetWindowRgn(hwnd: Hwnd, region: Hwnd, redraw: i32) -> i32;
        fn GetDoubleClickTime() -> u32;
        fn GetSystemMetrics(index: i32) -> i32;
    }

    #[link(name = "gdi32")]
    extern "system" {
        fn GetStockObject(index: i32) -> Hwnd;
        fn CreateRectRgn(left: i32, top: i32, right: i32, bottom: i32) -> Hwnd;
        fn CreateRoundRectRgn(
            left: i32,
            top: i32,
            right: i32,
            bottom: i32,
            ellipse_w: i32,
            ellipse_h: i32,
        ) -> Hwnd;
        fn CombineRgn(dest: Hwnd, src1: Hwnd, src2: Hwnd, mode: i32) -> i32;
        fn GetRgnBox(region: Hwnd, rect: *mut Rect) -> i32;
        fn DeleteObject(object: Hwnd) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> Hwnd;
        fn GetLastError() -> u32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn DragAcceptFiles(hwnd: Hwnd, accept: i32);
        fn DragQueryFileW(hdrop: Hwnd, index: u32, file: *mut u16, len: u32) -> u32;
        fn DragFinish(hdrop: Hwnd);
    }

    const GWLP_USERDATA: i32 = -21;
    /// 双击判据里的位移阈值（`GetSystemMetrics` 的索引）。
    const SM_CXDOUBLECLK: i32 = 36;
    const SM_CYDOUBLECLK: i32 = 37;
    /// 区域合并方式：加上一块 / 减掉一块。
    const RGN_OR: i32 = 2;
    const RGN_DIFF: i32 = 4;

    /// 视频窗收到的拖放路径，等主线程的 100ms tick 来取走。
    ///
    /// `WM_DROPFILES` 只在 wndproc 里能拿到，而这一刻我们不在 GPUI 的 `App`
    /// 上下文里，改不了视图状态，所以先落队列再统一消费。
    static DROPPED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    /// 画面上点出来的交互（双击 → 全屏），同样等 tick 取走。
    static EVENTS: std::sync::Mutex<Vec<OverlayEvent>> = std::sync::Mutex::new(Vec::new());

    /// 画面上最后一次鼠标移动的时刻（控制层自动淡出用）。
    ///
    /// 主窗口的区域把画面挖掉了（见 `set_region`），所以画面上的鼠标移动只有
    /// 叠加窗知道。控制层淡掉之后整块区域都让给叠加窗，用户"晃一下鼠标叫回
    /// 控制栏"这件事就完全靠这里上报。
    static ACTIVITY: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

    fn stamp_activity() {
        if let Ok(mut slot) = ACTIVITY.lock() {
            *slot = Some(std::time::Instant::now());
        }
    }

    /// 主窗口现在是不是影院（全屏）模式。
    ///
    /// wndproc 看不到 GPUI 的 `state.fullscreen`，但它要判断两件事：
    /// 双击该「进全屏」还是「退全屏」（其实都是 toggle，不用判断）、
    /// **以及此刻还能不能拖动窗体**——全屏时窗口已经铺满屏幕，拖它只会把画面
    /// 拖出屏幕、留下一块空白（用户观感像"窗口坏了"）。所以由 GPUI 侧在状态
    /// 变化时同步过来，tick 里读这个开关。
    static CINEMA: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    pub fn set_cinema(on: bool) {
        CINEMA.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    fn is_cinema() -> bool {
        CINEMA.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn push_event(event: OverlayEvent) {
        if let Ok(mut queue) = EVENTS.lock() {
            queue.push(event);
        }
    }

    /// 一次「按住画面拖动」的起点。
    ///
    /// wndproc 是自由函数，没有 self；叠加窗又只有一个，用静态量足够。
    /// 存的是**按下那一刻**的光标与主窗外框位置，移动时算总位移——
    /// 用累计位移而不是逐步增量，丢一两个 `WM_MOUSEMOVE` 也不会漂。
    #[derive(Clone, Copy)]
    struct Drag {
        /// 捕获鼠标的窗口（= 叠加窗自己，用来区分是不是这一轮的拖动）
        overlay: Hwnd,
        main: Hwnd,
        cursor: (i32, i32),
        origin: (i32, i32),
    }

    static DRAG: std::sync::Mutex<Option<Drag>> = std::sync::Mutex::new(None);

    fn drag_of(hwnd: Hwnd) -> Option<Drag> {
        DRAG.lock()
            .ok()
            .and_then(|slot| *slot)
            .filter(|drag| drag.overlay == hwnd)
    }

    fn end_drag(hwnd: Hwnd) {
        if let Ok(mut slot) = DRAG.lock() {
            if slot.as_ref().is_some_and(|drag| drag.overlay == hwnd) {
                *slot = None;
            }
        }
    }

    /// 上一次左键按下（时刻, 屏幕坐标）：自己判「双击的第二下」用。
    static LAST_PRESS: std::sync::Mutex<Option<(std::time::Instant, i32, i32)>> =
        std::sync::Mutex::new(None);

    /// 这一下是不是双击的第二下。
    ///
    /// 不指望 `WM_LBUTTONDBLCLK`：这块窗口是 `WS_EX_NOACTIVATE`，系统对"没被激活的
    /// 窗口"并不保证派发双击——实测（合成输入下稳定复现）只来两条
    /// `WM_LBUTTONDOWN`，于是"双击画面最大化"变成了"点了两下没反应"。
    /// 这里按系统同一套判据补一遍：时间取 `GetDoubleClickTime`，位移取
    /// `SM_CXDOUBLECLK`/`SM_CYDOUBLECLK`，和真双击的语义一致。
    fn take_double_press(x: i32, y: i32) -> bool {
        let now = std::time::Instant::now();
        let Ok(mut slot) = LAST_PRESS.lock() else {
            return false;
        };
        let double = slot.is_some_and(|(at, px, py)| {
            let window =
                std::time::Duration::from_millis(u64::from(unsafe { GetDoubleClickTime() }));
            let (slop_x, slop_y) = unsafe {
                (
                    GetSystemMetrics(SM_CXDOUBLECLK),
                    GetSystemMetrics(SM_CYDOUBLECLK),
                )
            };
            now.duration_since(at) <= window && (x - px).abs() <= slop_x && (y - py).abs() <= slop_y
        });
        // 成立就清空：连点三下算"一次双击 + 一次单击"，不能算两轮双击
        *slot = if double { None } else { Some((now, x, y)) };
        double
    }

    fn collect_dropped(hdrop: Hwnd) {
        const ALL: u32 = 0xFFFF_FFFF;
        let count = unsafe { DragQueryFileW(hdrop, ALL, std::ptr::null_mut(), 0) };
        let mut paths = Vec::new();
        for index in 0..count {
            let len = unsafe { DragQueryFileW(hdrop, index, std::ptr::null_mut(), 0) };
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let copied =
                unsafe { DragQueryFileW(hdrop, index, buf.as_mut_ptr(), buf.len() as u32) };
            if copied == 0 {
                continue;
            }
            paths.push(String::from_utf16_lossy(&buf[..copied as usize]));
        }
        unsafe { DragFinish(hdrop) };
        if paths.is_empty() {
            return;
        }
        if let Ok(mut queue) = DROPPED.lock() {
            queue.extend(paths);
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: Hwnd,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        if msg == WM_DROPFILES {
            collect_dropped(wparam as Hwnd);
            return 0;
        }
        if msg == WM_LBUTTONDOWN {
            // 画面这块不在主窗口的区域里，按下来的是我们：自己开一轮拖动。
            let main = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
            // 坐标取**消息里的**，不取 `GetCursorPos()`：合成输入（验收脚本 PostMessage）
            // 下真实光标还停在别处，用它判断双击会永远判不出来——而且消息里的坐标
            // 就是系统判双击用的那一份，语义最正。
            let screen = screen_point(hwnd, lparam);
            trace(&format!(
                "lbuttondown main={main} iconic={} zoomed={} at=({},{})",
                unsafe { IsIconic(main) },
                unsafe { IsZoomed(main) },
                screen.0,
                screen.1
            ));
            if main != 0 && unsafe { IsIconic(main) } == 0 {
                // 先激活主窗口。这块窗口是 `WS_EX_NOACTIVATE`（点了不抢焦点才不闪），
                // 所以系统不会替我们把主窗提到前台——不显式做一次，用户点画面
                // 既不会激活窗口、第一次双击还会被系统当成「激活」吃掉。
                activate(main);
                // 双击自己判（`WM_LBUTTONDBLCLK` 在合成输入下不会来），见 take_double_press
                if take_double_press(screen.0, screen.1) {
                    // 第一下可能已经开了拖动（正常会有 UP 收尾，这里兜一层）
                    end_drag(hwnd);
                    trace("double press -> toggle cinema");
                    push_event(OverlayEvent::ToggleCinema);
                    return 0;
                }
                // 全屏时不拖：窗口已经铺满屏幕，拖它只会把画面拖出去留一块空白
                if !is_cinema() && unsafe { IsZoomed(main) } == 0 {
                    let mut rect = Rect::default();
                    unsafe {
                        GetWindowRect(main, &mut rect);
                    }
                    if let Ok(mut slot) = DRAG.lock() {
                        *slot = Some(Drag {
                            overlay: hwnd,
                            main,
                            cursor: screen,
                            origin: (rect.left, rect.top),
                        });
                    }
                    unsafe {
                        SetCapture(hwnd);
                    }
                }
                return 0;
            }
            // 最小化状态下不拖也不双击：那是"还原窗口"的活儿，交给系统/任务栏。
        }
        if msg == WM_MOUSEMOVE {
            // 先记一笔"用户还在动鼠标"：控制层淡出之后，这是唯一能把它叫回来的信号
            stamp_activity();
            if let Some(drag) = drag_of(hwnd) {
                let cursor = screen_point(hwnd, lparam);
                {
                    let x = drag.origin.0 + (cursor.0 - drag.cursor.0);
                    let y = drag.origin.1 + (cursor.1 - drag.cursor.1);
                    trace(&format!("drag -> main@({x},{y})"));
                    unsafe {
                        SetWindowPos(
                            drag.main,
                            0,
                            x,
                            y,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                    // 叠加窗立刻贴上去：不等 GPUI 的 100ms tick，
                    // 否则拖动过程中画面会明显落后于窗口边框。
                    let (mx, my, mw, mh) = outer_rect(drag.main);
                    unsafe {
                        SetWindowPos(hwnd, 0, mx, my, mw, mh, SWP_NOZORDER | SWP_NOACTIVATE);
                    }
                }
                return 0;
            }
        }
        if msg == WM_LBUTTONUP || msg == WM_CAPTURECHANGED {
            if drag_of(hwnd).is_some() {
                end_drag(hwnd);
                unsafe {
                    ReleaseCapture();
                }
            }
            return 0;
        }
        if msg == WM_LBUTTONDBLCLK {
            // 双击画面 = 全屏播放 / 退出全屏（播放器手势）。
            //
            // 这里**不再**转交 `WM_NCLBUTTONDBLCLK`（最大化）：那条路在 GPUI 窗口上会把
            // `WS_MAXIMIZE` 位弄丢；而且用户要的是"双击看全屏"，本来就不该最大化。
            let main = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
            if main != 0 {
                activate(main);
                end_drag(hwnd);
                // 系统已经认了这一轮双击，把自判的按下记录清掉，
                // 否则紧跟着的单击会被自判逻辑当成"第二下"。
                if let Ok(mut slot) = LAST_PRESS.lock() {
                    *slot = None;
                }
                trace("dblclk -> toggle cinema");
                push_event(OverlayEvent::ToggleCinema);
            }
            return 0;
        }
        if msg == WM_ERASEBKGND {
            // mpv 第一帧到达之前这块窗口是空白的。它在透明主窗口**下面**，
            // 不铺黑就会透出桌面（切换视频时会闪一下桌面）。
            let mut rect = Rect::default();
            unsafe {
                GetClientRect(hwnd, &mut rect);
                FillRect(wparam as Hwnd, &rect, GetStockObject(BLACK_BRUSH));
            }
            return 1;
        }
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 鼠标消息里的客户区坐标 → 屏幕坐标。
    ///
    /// `lparam` 的低 16 位是 x、高 16 位是 y，都是**有符号**的 16 位（负坐标要还原成
    /// 负数，否则窗口跑到多显示器负区域时拖动会跳）。
    fn screen_point(hwnd: Hwnd, lparam: isize) -> (i32, i32) {
        let bits = lparam as u32;
        let mut point = Point {
            x: (bits & 0xFFFF) as u16 as i16 as i32,
            y: ((bits >> 16) & 0xFFFF) as u16 as i16 as i32,
        };
        unsafe {
            ClientToScreen(hwnd, &mut point);
        }
        (point.x, point.y)
    }

    /// 把主窗口提到前台。
    ///
    /// 这一下鼠标事件是投给本进程的，系统因此允许我们调 `SetForegroundWindow`
    /// （前台权限的判定规则之一），不会像纯后台进程那样被静默忽略。
    fn activate(main: Hwnd) {
        if main != 0 {
            unsafe {
                SetForegroundWindow(main);
            }
        }
    }

    /// 输入桥的诊断留痕（`SMP_OVERLAY_TRACE` 打开）。
    ///
    /// 合成鼠标事件与窗口消息是两套时序，出了问题只有把"收到什么消息、据此做了什么"
    /// 落盘才说得清（双击被吞、按住画面变最大化这类）。
    fn trace(message: &str) {
        if std::env::var_os("SMP_OVERLAY_TRACE").is_some() {
            crate::persistence::log_session(&format!("overlay: {message}"));
        }
    }

    /// 外框矩形（物理屏幕像素）：x, y, w, h。
    fn outer_rect(hwnd: Hwnd) -> (i32, i32, i32, i32) {
        let mut rect = Rect::default();
        unsafe { GetWindowRect(hwnd, &mut rect) };
        (
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
        )
    }

    static REGISTER: Once = Once::new();
    static mut CLASS_READY: bool = false;

    fn ensure_class() -> bool {
        REGISTER.call_once(|| {
            let class_name = wide(CLASS_NAME);
            let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
            let class = WndClassW {
                style: CS_DBLCLKS,
                lpfn_wnd_proc: wnd_proc as *const () as usize,
                cb_cls_extra: 0,
                cb_wnd_extra: 0,
                h_instance: instance,
                h_icon: 0,
                h_cursor: 0,
                hbr_background: 0, // 走 WM_ERASEBKGND 自己铺黑
                lpsz_menu_name: std::ptr::null(),
                lpsz_class_name: class_name.as_ptr(),
            };
            let registered = unsafe { RegisterClassW(&class) };
            let already = unsafe { GetLastError() } == 1410; // ERROR_CLASS_ALREADY_EXISTS
            unsafe {
                CLASS_READY = registered != 0 || already;
            }
        });
        unsafe { CLASS_READY }
    }

    /// 建一个**没有 owner** 的隐藏弹窗（`hWndParent = 0`）。
    pub fn create_overlay(main: Hwnd) -> Option<Hwnd> {
        if !ensure_class() {
            return None;
        }
        let class_name = wide(CLASS_NAME);
        let title = wide("Simple Media Player Video");
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                0,
                0,
                0,
                0,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null_mut(),
            )
        };
        if hwnd == 0 {
            return None;
        }
        unsafe {
            // 拖动画面时要知道该把「按住标题栏」转交给谁
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, main);
            // 让 Explorer 把拖到画面上的文件转成 WM_DROPFILES
            DragAcceptFiles(hwnd, 1);
        }
        // 画面也要圆角：窗口最上面两个角是主窗口的标题带、最下面两个角就是这块画面，
        // 只圆主窗口会留下两个方角（半径必须一致，所以两边都用 DWM 的圆角）。
        crate::window_actions::set_rounded(hwnd, true);
        Some(hwnd)
    }

    /// 取走并清空拖放队列（主线程 tick 调用）。
    pub fn take_dropped() -> Vec<String> {
        DROPPED
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default()
    }

    /// 取走并清空画面交互队列（主线程 tick 调用）。
    pub fn take_events() -> Vec<OverlayEvent> {
        EVENTS
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default()
    }

    pub fn destroy(hwnd: Hwnd) {
        if hwnd != 0 && unsafe { IsWindow(hwnd) } != 0 {
            unsafe {
                DestroyWindow(hwnd);
            }
        }
    }

    pub fn show(hwnd: Hwnd) {
        unsafe {
            ShowWindow(hwnd, SW_SHOWNA);
        }
    }

    pub fn hide(hwnd: Hwnd) {
        unsafe {
            ShowWindow(hwnd, SW_HIDE);
        }
    }

    pub fn set_rect(hwnd: Hwnd, x: i32, y: i32, width: i32, height: i32, visible: bool) {
        if hwnd == 0 {
            return;
        }
        let mut flags = SWP_NOZORDER | SWP_NOACTIVATE;
        if visible {
            flags |= SWP_SHOWWINDOW;
        }
        unsafe {
            SetWindowPos(hwnd, 0, x, y, width.max(1), height.max(1), flags);
        }
    }

    /// 把它插到主窗口**正下方**。
    ///
    /// `SetWindowPos` 的 `hWndInsertAfter` 是「排在谁后面（下面）」，
    /// 所以直接传主窗口句柄。主窗口自己的 z 序不动。
    pub fn keep_below(hwnd: Hwnd, main: Hwnd) {
        if hwnd == 0 || main == 0 {
            return;
        }
        unsafe {
            SetWindowPos(
                hwnd,
                main,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    /// 视频窗现在是不是就压在 `main` 正下方（`GW_HWNDNEXT` 就是它）。
    ///
    /// 只查不改：z 序会被别的进程（`BringWindowToTop`、新窗口插进来）打乱，
    /// 查一次比每 100ms 都打一次 `SetWindowPos` 便宜得多。
    pub fn is_below(hwnd: Hwnd, main: Hwnd) -> bool {
        hwnd != 0 && main != 0 && unsafe { GetWindow(main, GW_HWNDNEXT) } == hwnd
    }

    /// z 序不对才纠正（每帧调这个，正确时只是一次 `GetWindow`）。
    pub fn ensure_below(hwnd: Hwnd, main: Hwnd) {
        if !is_below(hwnd, main) {
            keep_below(hwnd, main);
        }
    }

    pub fn is_minimized(hwnd: Hwnd) -> bool {
        hwnd != 0 && unsafe { IsIconic(hwnd) } != 0
    }

    /// 客户区左上角相对**窗口矩形**的物理偏移（含边框与不可见边框）。
    fn client_offset(hwnd: Hwnd) -> (i32, i32) {
        let mut window = Rect::default();
        let mut point = Point::default();
        unsafe {
            GetWindowRect(hwnd, &mut window);
            ClientToScreen(hwnd, &mut point);
        }
        (point.x - window.left, point.y - window.top)
    }

    fn window_size(hwnd: Hwnd) -> (i32, i32) {
        let mut window = Rect::default();
        unsafe { GetWindowRect(hwnd, &mut window) };
        (window.right - window.left, window.bottom - window.top)
    }

    /// 给**主窗口**设区域：整窗 − 画面区 ＋ chrome。
    ///
    /// 为什么必须减掉画面区：系统只认窗口矩形/区域，不认逐像素透明度。主窗口整块
    /// 盖着视频窗时，mpv 的呈现会被当成「完全被遮挡」而停止出帧（见模块头）。
    /// 减掉之后画面露出来；而画面区之外的部分（含非客户区的边框）仍留在区域里，
    /// 所以四边拖动缩放、系统边框都是正常的。
    pub fn set_region(hwnd: Hwnd, stage: (f32, f32, f32, f32), chrome: &[ChromeRect], scale: f32) {
        if hwnd == 0 {
            return;
        }
        let (width, height) = window_size(hwnd);
        let (ox, oy) = client_offset(hwnd);
        let px = |value: f32| (value * scale).round() as i32;
        // 逻辑（客户区）→ 区域（窗口矩形、物理像素）
        let place = |x: f32, y: f32, w: f32, h: f32| {
            (ox + px(x), oy + px(y), ox + px(x + w), oy + px(y + h))
        };
        unsafe {
            let region = CreateRectRgn(0, 0, width.max(1), height.max(1));
            if region == 0 {
                return;
            }
            let (left, top, right, bottom) = place(stage.0, stage.1, stage.2, stage.3);
            let cut = CreateRectRgn(left, top, right, bottom);
            if cut != 0 {
                CombineRgn(region, region, cut, RGN_DIFF);
                // CombineRgn 是复制语义，源区域用完自己释放
                DeleteObject(cut);
            }
            for item in chrome {
                let (left, top, right, bottom) = place(item.x, item.y, item.width, item.height);
                let add = if item.radius > 0.0 {
                    // CreateRoundRectRgn 收的是椭圆的**宽高**，不是半径
                    let d = px(item.radius) * 2;
                    CreateRoundRectRgn(left, top, right, bottom, d, d)
                } else {
                    CreateRectRgn(left, top, right, bottom)
                };
                if add != 0 {
                    CombineRgn(region, region, add, RGN_OR);
                    DeleteObject(add);
                }
            }
            // 空区域（控制层淡掉、整窗都让给画面时就是这种）不能真的交上去：
            // `SetWindowRgn` 收到空区域等于让窗口"不存在"，DWM 会把它从合成树里
            // 摘掉，回程（淡入）时窗口句柄、焦点、z 序都要重新长回来。留 1 像素
            // 的窗口在左上角——物理上看不见，但窗口还是活的。
            let mut box_rect = Rect::default();
            if GetRgnBox(region, &mut box_rect) <= 1 {
                let keep = CreateRectRgn(ox, oy, ox + 1, oy + 1);
                if keep != 0 {
                    CombineRgn(region, region, keep, RGN_OR);
                    DeleteObject(keep);
                }
            }
            // 成功后 region 归系统所有，**不要** DeleteObject
            SetWindowRgn(hwnd, region, 1);
        }
    }

    /// 恢复整窗矩形（NULL 区域交给系统接管，原区域会被释放）。
    pub fn clear_region(hwnd: Hwnd) {
        if hwnd != 0 {
            unsafe { SetWindowRgn(hwnd, 0, 1) };
        }
    }

    /// 窗口**外框**在屏幕上的物理矩形（含边框）。
    pub fn window_rect(hwnd: Hwnd) -> (i32, i32, i32, i32) {
        if hwnd == 0 {
            return (0, 0, 0, 0);
        }
        let mut rect = Rect::default();
        unsafe { GetWindowRect(hwnd, &mut rect) };
        (
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
        )
    }

    /// 画面上的鼠标移动：给控制层的自动淡出报个「用户还在」。
    ///
    /// 画面区不在主窗口的区域里（见 `set_region`），所以主窗口收不到这些移动：
    /// 控制层淡掉之后鼠标划过视频，GPUI 那边一点消息都没有，只有这里知道。
    /// 取走即清空——tick 每 100ms 消费一次，多个移动只留最后一个时间戳。
    pub fn activity_stamp() -> Option<std::time::Instant> {
        ACTIVITY.lock().ok().and_then(|mut slot| slot.take())
    }
}

#[cfg(not(windows))]
mod win {
    use super::{ChromeRect, Hwnd, OverlayEvent};

    pub fn create_overlay(_main: Hwnd) -> Option<Hwnd> {
        None
    }
    pub fn take_dropped() -> Vec<String> {
        Vec::new()
    }
    pub fn take_events() -> Vec<OverlayEvent> {
        Vec::new()
    }
    pub fn set_cinema(_on: bool) {}
    pub fn destroy(_hwnd: Hwnd) {}
    pub fn show(_hwnd: Hwnd) {}
    pub fn hide(_hwnd: Hwnd) {}
    pub fn set_rect(_hwnd: Hwnd, _x: i32, _y: i32, _w: i32, _h: i32, _visible: bool) {}
    pub fn keep_below(_hwnd: Hwnd, _main: Hwnd) {}
    pub fn is_below(_hwnd: Hwnd, _main: Hwnd) -> bool {
        true
    }
    pub fn ensure_below(_hwnd: Hwnd, _main: Hwnd) {}
    pub fn is_minimized(_hwnd: Hwnd) -> bool {
        false
    }
    pub fn set_region(_hwnd: Hwnd, _stage: (f32, f32, f32, f32), _chrome: &[ChromeRect], _s: f32) {}
    pub fn clear_region(_hwnd: Hwnd) {}
    pub fn activity_stamp() -> Option<std::time::Instant> {
        None
    }
    pub fn window_rect(_hwnd: Hwnd) -> (i32, i32, i32, i32) {
        (0, 0, 0, 0)
    }
}

/// 视频叠加窗口（mpv 的画布，垫在主窗口下面）。
pub struct VideoOverlay {
    hwnd: Hwnd,
    /// GPUI 主窗口：z 序、最小化、拖动转交都要跟它对齐
    main: Hwnd,
    visible: bool,
    rect: (i32, i32, i32, i32),
}

impl VideoOverlay {
    /// 创建无 owner 的隐藏弹窗。
    pub fn create(main: Hwnd) -> Option<Self> {
        let hwnd = win::create_overlay(main)?;
        Some(Self {
            hwnd,
            main,
            visible: false,
            rect: (0, 0, 0, 0),
        })
    }

    pub fn hwnd(&self) -> Hwnd {
        self.hwnd
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// 设置位置尺寸（物理屏幕像素）。位置未变化时不再移动窗口。
    ///
    /// 但 z 序**每次都要查**：位置没变不代表压在被主窗口下面——别的进程把主窗口
    /// 提到最前、或我们自己 `ShowWindow` 过，都可能把它翻上来盖住 chrome。
    pub fn set_rect(&mut self, x: i32, y: i32, width: i32, height: i32) {
        let rect = (x, y, width.max(1), height.max(1));
        if rect != self.rect {
            self.rect = rect;
            win::set_rect(self.hwnd, rect.0, rect.1, rect.2, rect.3, self.visible);
        }
        win::ensure_below(self.hwnd, self.main);
    }

    pub fn show(&mut self) {
        if self.visible {
            return;
        }
        self.visible = true;
        win::set_rect(
            self.hwnd,
            self.rect.0,
            self.rect.1,
            self.rect.2,
            self.rect.3,
            true,
        );
        win::show(self.hwnd);
        // `ShowWindow` 会把它提到最前，必须立刻压回主窗口下面
        win::keep_below(self.hwnd, self.main);
    }

    pub fn hide(&mut self) {
        if self.visible {
            self.visible = false;
            win::hide(self.hwnd);
        }
    }

    /// 主窗口最小化时把它一起藏起来（没有 owner 关系，系统不再代劳）。
    ///
    /// 返回 `true` 表示「刚刚因为最小化被藏起来」。
    pub fn follow_minimize(&mut self) -> bool {
        if self.visible && win::is_minimized(self.main) {
            self.hide();
            return true;
        }
        false
    }
}

impl Drop for VideoOverlay {
    fn drop(&mut self) {
        win::destroy(self.hwnd);
    }
}

/// 主窗口外框矩形（物理屏幕坐标：x, y, w, h）。
///
/// 视频窗要铺满**外框**而不是客户区：客户区之外那一圈是窗口边框，它留在主窗口的
/// 区域里（否则四边拖动缩放失效），而那一圈我们不画任何东西——铺到客户区就会
/// 从边框里透出桌面（实测左边会露出别的窗口的文字）。
pub fn window_rect(hwnd: Hwnd) -> (i32, i32, i32, i32) {
    win::window_rect(hwnd)
}

/// 取走视频窗收到的拖放路径（主线程 tick 调用）。
pub fn take_dropped() -> Vec<String> {
    win::take_dropped()
}

/// 取走画面上点出来的交互（双击 → 全屏），主线程 tick 调用。
pub fn take_events() -> Vec<OverlayEvent> {
    win::take_events()
}

/// 告诉视频窗「主窗口现在是不是全屏」——全屏时它不再拖窗体。
pub fn set_cinema(on: bool) {
    win::set_cinema(on)
}

/// 视频模式下给主窗口设区域：整窗 − 画面区 ＋ chrome。
pub fn set_region(main: Hwnd, stage: (f32, f32, f32, f32), chrome: &[ChromeRect], scale: f32) {
    win::set_region(main, stage, chrome, scale);
}

/// 退出视频模式：主窗口恢复整窗矩形。
pub fn clear_region(main: Hwnd) {
    win::clear_region(main);
}

/// 画面上最后一次鼠标移动（控制层自动淡出用），取走即清空。
pub fn activity_stamp() -> Option<std::time::Instant> {
    win::activity_stamp()
}
