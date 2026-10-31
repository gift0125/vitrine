//! 主窗口的窗口动作：最大化 / 还原 / 按画面尺寸调整窗口 / 圆角。
//!
//! ## 为什么不用 `Window::zoom_window()`
//!
//! GPUI 的文档写着 "Toggle zoom on the window"，但 Windows 后端的实现只有一句
//! `ShowWindowAsync(SW_MAXIMIZE)`（`gpui-pre-windows 0.3.9` 的 `zoom()`），
//! **没有任何还原分支**——所以最大化之后再点那个按钮完全没反应，
//! 用户只能双击标题栏或拖边缘自己救回来。
//!
//! ## 为什么用 `ShowWindowAsync` 而不是 `ShowWindow`
//!
//! 被这个坑狠狠地教了一次：按钮的点击回调跑在 GPUI 的事件派发里，
//! `ShowWindow` 是**同步**的，它立刻把 `WM_SIZE` 重入给 GPUI 的 wndproc——
//! 而 GPUI 这一层并不因此重新布局，结果是窗口真的变成了 3840×2088，
//! 界面却还画在 1040×680 的位置上（右边一大片空、hitbox 全错位，
//! 于是「再点一次最大化/还原」永远点不到按钮）。
//!
//! `ShowWindowAsync` 把这条命令投回消息队列，等当前这次事件派发结束、
//! GPUI 回到自己的消息循环里再处理 `WM_SIZE`，布局就正常跟上了。
//! 代价：调用返回时窗口状态还没变，`IsZoomed` 读到的是旧值——
//! 所以调用方要「先乐观地假设切换成功」，再让 100ms 的 tick 去校正。

/// 窗口句柄（Win32 `HWND` 当作整数传递；非 Windows 上恒为 0）。
///
/// 放在这里而不是某个平台模块里：整个 app 只把句柄当不透明整数用，跨平台 stub
/// 也全是空实现，没必要为它引入一个平台类型。
pub type Hwnd = isize;

#[cfg_attr(not(windows), allow(dead_code))]
const SW_MAXIMIZE: i32 = 3;
#[cfg_attr(not(windows), allow(dead_code))]
const SW_RESTORE: i32 = 9;

/// 窗口当前是否最大化（`IsZoomed`）。
pub fn is_zoomed(hwnd: Hwnd) -> bool {
    imp::is_zoomed(hwnd)
}

/// 最大化（`zoomed = true`）或还原（`false`）。
///
/// 异步：命令投回消息队列，当前事件派发结束后才生效。`hwnd` 为 0 时什么都不做。
pub fn set_zoomed(hwnd: Hwnd, zoomed: bool) {
    imp::set_zoomed(hwnd, zoomed)
}

/// 给窗口加/去圆角（Windows 11 的 DWM 圆角，带抗锯齿）。
///
/// 视频播放时**两块窗口都要圆**：画面上下的两个角来自视频叠加窗，标题带的两个角
/// 来自主窗口——只圆一块会在角上露出一个方角。
///
/// 为什么不用 `SetWindowRgn` 自己切圆角：区域是 1 bit 的，边缘必然是锯齿；DWM 这条
/// 属性是系统按 DPI 算好半径再抗锯齿，也是 Windows 11 上所有应用的样子。
///
/// Windows 10 没有这条属性（`DwmSetWindowAttribute` 返回 `E_INVALIDARG`），调用会被
/// 静默忽略、窗口保持直角——这是预期行为，别为它记日志。
pub fn set_rounded(hwnd: Hwnd, rounded: bool) {
    imp::set_rounded(hwnd, rounded)
}

/// 让**客户区铺满整个窗口矩形**（左 / 右 / 下三边吃掉系统那圈缩放边框）。
///
/// ## 为什么必须这么做
///
/// 自绘标题栏的窗口仍然是 `WS_THICKFRAME`，系统会在外边留一圈给缩放用的**不可见边框**
/// （实测 1.5 倍屏下外框比客户区宽 22、高 11 物理像素）。于是：
///
/// - 视频叠加窗铺的是**外框**（铺客户区会从边框里透出桌面，见 `video_overlay`）；
/// - 标题带/胶囊只能画在**客户区**（GPUI 的画布就是客户区，画不到外面去）。
///
/// 结果就是窗口左右各露出 7 逻辑像素的画面——用户看到的是「顶栏没有铺满宽度」。
/// 客户区铺满整窗之后两者严丝合缝，标题带一直画到窗口边缘。
///
/// ## 怎么做到
///
/// 拦 `WM_NCCALCSIZE`：先让原来的处理（GPUI）算一遍——它负责「最大化时顶边往下让
/// 一条边框」这条规则（不让的话最大化后标题带上半截会被屏幕边缘切掉）——再把
/// 左 / 右 / 下三边改回窗口矩形。**不动** `WS_THICKFRAME`，所以四边拖拽缩放照旧。
pub fn make_client_full_bleed(hwnd: Hwnd) {
    imp::make_client_full_bleed(hwnd)
}

/// 客户区是不是已经铺满整个窗口矩形。
///
/// 系统在**进出全屏**那一下并不重算客户区（实测：窗口已经缩回 1280×731，
/// `GetClientRect` 还报 3840×2010、原点停在旧位置），整张界面就停在旧尺寸上。
/// 调用方（100ms tick）发现不一致时补一次 [`refresh_client_area`]。
pub fn client_matches_window(hwnd: Hwnd) -> bool {
    imp::client_matches_window(hwnd)
}

/// 让系统重新算一次客户区（`SWP_FRAMECHANGED`，不动位置和尺寸）。
///
/// **不要在 `Context::update` 里直接调**：它会同步回调 `WM_SIZE`（同一个坑，
/// 见 `fit_to_video` 的说明），要走前台任务。
pub fn refresh_client_area(hwnd: Hwnd) {
    imp::refresh_client_area(hwnd)
}

/// 当前**真实**客户区尺寸（物理像素）。
///
/// 用来和 GPUI 自己的 `window.bounds()` 对账：进出全屏那一下 GPUI 会漏掉尺寸
/// 通知，`bounds()` 也会跟着停在旧值（`logical_size` 只在处理 `WM_SIZE` 时更新），
/// 所以「viewport ≠ bounds」这条判据发现不了它——只能问系统要真实值。
pub fn client_size(hwnd: Hwnd) -> (i32, i32) {
    imp::client_size(hwnd)
}

/// 把「客户区已经铺满、布局却还停在旧尺寸」这个状态踢醒。
///
/// GPUI 进出全屏那一下会丢掉自己的一次尺寸通知（`WM_SIZE` 在 `SetWindowPos` 里同步
/// 派发，回调里 `App` 正被借用）。`window.bounds()`（= 平台侧记的 `logical_size`）
/// 已经是新尺寸，布局用的 `viewport_size` 却还是旧的：顶栏、胶囊、窗口按钮全都按
/// 旧尺寸画在左上角，点击也命中不到。
///
/// 这里做的就是把**当前真实客户区尺寸**再投递一条 `WM_SIZE`——GPUI 的尺寸处理只认
/// lparam，是幂等的，重放一次布局就按新尺寸重排。用 `PostMessage` 而不是
/// `SendMessage`：必须落在消息循环里，不能在别人的 `update` 栈里重入。
pub fn kick_layout(hwnd: Hwnd) {
    imp::kick_layout(hwnd)
}

/// 光标在屏幕上的位置（物理像素）。
///
/// 拖动窗体用它而不用 GPUI 的鼠标坐标：窗口跟着光标移动之后，光标在**窗口内**的
/// 相对位置几乎不变（每帧都是 0 位移），只有屏幕坐标才看得出「人在往哪拖」。
pub fn cursor_pos() -> (i32, i32) {
    imp::cursor_pos()
}

/// 窗口外框左上角（物理像素）。
pub fn window_origin(hwnd: Hwnd) -> (i32, i32) {
    imp::window_origin(hwnd)
}

/// 把窗口外框挪到指定位置（尺寸不变）。
///
/// 拖动过程中每帧调一次，所以只用 `SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE`：
/// 不碰 z 序、不抢焦点，也不触发重排（尺寸没变）。
pub fn move_window(hwnd: Hwnd, x: i32, y: i32) {
    imp::move_window(hwnd, x, y)
}

/// 客户区的最小尺寸（逻辑像素）：和 `main.rs` 的 `window_min_size` 保持一致。
///
/// 系统会把窗口压不到这个尺寸以下（GPUI 把它交给 `WM_GETMINMAXINFO`）。而按画面
/// 比例算出来的尺寸可能比它还小（低分辨率视频），那时只能反过来把窗口放大——
/// 窗口缩不下去、画面又没跟着放大，就会留一圈黑边。
#[cfg_attr(not(windows), allow(dead_code))]
pub const MIN_CLIENT: (f32, f32) = (720.0, 480.0);

/// 窗口与屏幕边缘之间留的余量（逻辑像素）。
///
/// 不留余量的话窗口会正好贴满工作区，看起来像"被最大化"了，边框也不见了。
#[cfg_attr(not(windows), allow(dead_code))]
const SCREEN_MARGIN: f32 = 12.0;

/// 一次「按画面尺寸定窗口」用到的现场事实（全是物理像素）。
///
/// 单独抽出来是为了让策略能被单测覆盖：真机上的工作区尺寸、DPI、边框厚度
/// 都不可控，纯函数才测得出「4K 片子在 1080p 屏上怎么放」这种分支。
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitScene {
    /// 画面显示尺寸（像素，已含像素宽高比校正）
    pub video: (u32, u32),
    /// 当前外框 (x, y, w, h)
    pub outer: (i32, i32, i32, i32),
    /// 当前客户区 (w, h)
    pub client: (i32, i32),
    /// 所在显示器的工作区 (x, y, w, h)，已扣掉任务栏
    pub work: (i32, i32, i32, i32),
    /// 允许的最小客户区（物理像素）
    pub min_client: (f32, f32),
    /// 与屏幕边缘的余量（物理像素）
    pub margin: i32,
}

/// 「按画面尺寸定窗口」的结果（物理像素）。
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitPlan {
    /// 目标外框左上角
    pub origin: (i32, i32),
    /// 目标外框尺寸
    pub outer: (i32, i32),
    /// 目标客户区尺寸（= 画面应该铺满的区域）
    pub client: (i32, i32),
}

#[cfg_attr(not(windows), allow(dead_code))]
impl FitPlan {
    /// 和当前窗口完全一致（不必再调 `SetWindowPos`）。
    pub fn same_as(&self, scene: &FitScene) -> bool {
        self.origin == (scene.outer.0, scene.outer.1)
            && self.outer == (scene.outer.2, scene.outer.3)
    }
}

/// 算出「窗口尺寸 = 画面尺寸」的目标几何；画面尺寸不可用时返回 `None`。
///
/// 规则按优先级：
/// 1. **1:1 像素**——画面在屏幕上一点不缩放（窗体大小就是内容大小）；
/// 2. 屏幕放不下时**等比缩小**到工作区里（留 `margin`）；
/// 3. 缩放后的客户区小于窗口最小尺寸时**反过来放大**窗口（否则画面周围会留黑边）；
/// 4. 位置以当前窗口中心为锚点，并夹回工作区内（不改居中的观感，又不会跑出屏幕）。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn plan_fit(scene: FitScene) -> Option<FitPlan> {
    let (vw, vh) = (scene.video.0 as f32, scene.video.1 as f32);
    if vw < 2.0 || vh < 2.0 {
        return None;
    }

    // 外框比客户区大的那一圈（边框）。自绘标题栏把非客户区压到只剩边框，
    // 具体几像素随 DPI 与系统版本变，只能实测。
    let border_w = (scene.outer.2 - scene.client.0).max(0);
    let border_h = (scene.outer.3 - scene.client.1).max(0);
    let work = scene.work;

    let max_w = (work.2 - border_w - scene.margin * 2).max(1) as f32;
    let max_h = (work.3 - border_h - scene.margin * 2).max(1) as f32;

    let kmax = (max_w / vw).min(max_h / vh);
    let kmin = (scene.min_client.0 / vw).max(scene.min_client.1 / vh);
    // 先保证不超屏（kmax），再保证不小于最小窗口（kmin）；只有屏幕真的装不下
    // 最小窗口时才放弃后者——那时候怎么排都会溢出，优先让画面完整可见。
    let mut k = kmax.min(1.0);
    if kmin <= kmax {
        k = k.max(kmin);
    }

    let client_w = (vw * k).round().max(1.0) as i32;
    let client_h = (vh * k).round().max(1.0) as i32;
    let outer_w = client_w + border_w;
    let outer_h = client_h + border_h;

    // 以当前窗口中心为锚点缩放（不是在左上角原地长大），再夹回工作区。
    let anchor_x = scene.outer.0 + scene.outer.2 / 2;
    let anchor_y = scene.outer.1 + scene.outer.3 / 2;
    let max_x = (work.0 + work.2 - outer_w).max(work.0);
    let max_y = (work.1 + work.3 - outer_h).max(work.1);
    let x = (anchor_x - outer_w / 2).clamp(work.0, max_x);
    let y = (anchor_y - outer_h / 2).clamp(work.1, max_y);

    Some(FitPlan {
        origin: (x, y),
        outer: (outer_w, outer_h),
        client: (client_w, client_h),
    })
}

/// 把窗口调成画面的尺寸与比例，并挪回屏幕里；返回是否真的动了窗口。
///
/// **不要在 `Context::update` 里直接调用**：`SetWindowPos` 会同步回调 GPUI 的
/// `WM_SIZE` → 重新布局，而那一刻 App 正被借用，回调拿不到借用权，界面会留在
/// 旧尺寸上（`main.rs` 里 `ShowWindow` 踩过同一个坑）。调用方要把这条命令
/// 放进一个前台任务里，让它在消息循环的下一轮跑。
pub fn fit_to_video(hwnd: Hwnd, video: (u32, u32)) -> bool {
    imp::fit_to_video(hwnd, video)
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicIsize, Ordering};

    use super::{plan_fit, FitScene, Hwnd, MIN_CLIENT, SCREEN_MARGIN, SW_MAXIMIZE, SW_RESTORE};

    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_FRAMECHANGED: u32 = 0x0020;
    const GWLP_WNDPROC: i32 = -4;
    /// `GWL_STYLE`（`GetWindowLongW` 的索引）与「可缩放边框」样式位。
    const GWL_STYLE: i32 = -16;
    const WS_THICKFRAME: u32 = 0x0004_0000;
    const WM_NCCALCSIZE: u32 = 0x0083;
    const WM_NCHITTEST: u32 = 0x0084;
    const WM_GETMINMAXINFO: u32 = 0x0024;
    const WM_SIZE: u32 = 0x0005;
    /// `WM_SIZE` 的 `wParam`：窗口既没最大化也没最小化（补发通知时用它）。
    const SIZE_RESTORED: usize = 0;
    /// `WM_NCHITTEST` 的返回码：客户区 / 左 / 右 / 下 / 左下 / 右下。
    const HTCLIENT: isize = 1;
    const HTLEFT: isize = 10;
    const HTRIGHT: isize = 11;
    const HTBOTTOM: isize = 15;
    const HTBOTTOMLEFT: isize = 16;
    const HTBOTTOMRIGHT: isize = 17;
    /// 缩放热区宽度（逻辑像素）。
    ///
    /// 客户区铺满之后，那圈边框变成了我们自己的客户区，`DefWindowProc` 不再往里
    /// 认「这是边框」——四边就拉不动了。所以自己补一条热区，宽度取 Windows 的
    /// 常规手感（8）稍窄一点，避免吃掉标题带左端那 16px 的拖拽区。
    const RESIZE_GRIP: f32 = 6.0;

    /// `WM_NCCALCSIZE`（`wParam = TRUE`）的入参：前三个矩形是「建议客户区 / 客户区 /
    /// 窗口矩形」，后面跟一个 `WINDOWPOS` 指针。只用到第一项。
    #[repr(C)]
    struct NcCalcSizeParams {
        rgrc: [Rect; 3],
        window_pos: *mut std::ffi::c_void,
    }

    /// `DWMWA_WINDOW_CORNER_PREFERENCE` 与它的取值（Win11 SDK 的头文件值）。
    ///
    /// 旧 SDK 里没有这两个名字，所以写死数字而不是引 `windows` crate。
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_DONOTROUND: u32 = 1;
    const DWMWCP_ROUND: u32 = 2;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct MonitorInfo {
        cb_size: u32,
        monitor: Rect,
        work: Rect,
        flags: u32,
    }

    /// `MINMAXINFO`：只关心 `ptMinTrackSize`（前三个 `POINT` 是保留位 / 最大化尺寸 /
    /// 最大化位置，必须照样留出空间，否则读到的偏移是错的）。
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct MinMaxInfo {
        reserved: Point,
        max_size: Point,
        max_position: Point,
        min_track: Point,
        max_track: Point,
    }

    #[link(name = "user32")]
    extern "system" {
        fn IsZoomed(hwnd: Hwnd) -> i32;
        fn ShowWindowAsync(hwnd: Hwnd, cmd: i32) -> i32;
        fn GetWindowRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        fn GetClientRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        fn MonitorFromWindow(hwnd: Hwnd, flags: u32) -> Hwnd;
        fn GetMonitorInfoW(monitor: Hwnd, info: *mut MonitorInfo) -> i32;
        fn GetDpiForWindow(hwnd: Hwnd) -> u32;
        fn SetWindowPos(
            hwnd: Hwnd,
            insert_after: Hwnd,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
        fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: isize) -> isize;
        fn PostMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> i32;
        fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn ScreenToClient(hwnd: Hwnd, point: *mut Point) -> i32;
        fn IsIconic(hwnd: Hwnd) -> i32;
        fn GetWindowLongW(hwnd: Hwnd, index: i32) -> i32;
        fn CallWindowProcW(
            previous: isize,
            hwnd: Hwnd,
            msg: u32,
            wparam: usize,
            lparam: isize,
        ) -> isize;
        fn GetCursorPos(point: *mut Point) -> i32;
    }

    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: Hwnd,
            attribute: u32,
            value: *const std::ffi::c_void,
            size: u32,
        ) -> i32;
    }

    pub fn is_zoomed(hwnd: Hwnd) -> bool {
        hwnd != 0 && unsafe { IsZoomed(hwnd) } != 0
    }

    /// 原来那个窗口过程（GPUI 的），`full_bleed_proc` 逐条转发给它。
    static PREVIOUS_WNDPROC: AtomicIsize = AtomicIsize::new(0);

    /// 铺满之前实测的**外框 − 客户区**（宽、高，物理像素），如 (22, 11)。
    ///
    /// 客户区铺满之后这圈"边框"已经不存在了，但 GPUI 在窗口**创建时**就把这个厚度
    /// 记进了 `border_offset`，并拿它算 `ptMinTrackSize`（`最小客户区 + 厚度`）。
    /// 1280×720 的片子正好等于最小高度 480 逻辑像素 × DPI，于是被顶成 720+11=731
    /// ——「窗口 = 内容」就差这 11 像素。见 `full_bleed_proc` 里的减除。
    static FRAME_INSETS: [std::sync::atomic::AtomicI32; 2] = [
        std::sync::atomic::AtomicI32::new(0),
        std::sync::atomic::AtomicI32::new(0),
    ];

    /// 拦 `WM_NCCALCSIZE`：把客户区的左 / 右 / 下三边扩到窗口矩形。
    ///
    /// 顶边**不碰**：GPUI 在最大化时会把 `top` 往下让一条边框的厚度，正是为了让
    /// 标题带不被屏幕边缘切掉——这里照抄它的结果即可。
    ///
    /// 窗口矩形取系统在入参里给的那份（`wParam != 0` 时是 `rgrc[1]`，`wParam == 0`
    /// 时就是 `lparam` 指向的那个 `RECT` 的初值），**不能**调 `GetWindowRect`：
    /// 这一刻窗口矩形正在变（尤其是进出全屏那一下），读到的还是旧值，客户区就会
    /// 停在旧尺寸上（实测表现为「窗口已经缩回 1280×731，客户区还报 3840×2010」）。
    ///
    /// `wParam == 0`（单纯改尺寸）这一路也必须改：GPUI 只在 `wParam != 0` 时自己
    /// 处理标题栏，改尺寸那一下会退回 `DefWindowProc`，客户区仍是「窗口减边框」，
    /// 于是每改一次尺寸都要靠 tick 里的自愈再补一次 `SetWindowPos`（画面会抖一下）。
    unsafe extern "system" fn full_bleed_proc(
        hwnd: Hwnd,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        let previous = PREVIOUS_WNDPROC.load(Ordering::Relaxed);
        if previous == 0 {
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        // 先抄下窗口矩形：`CallWindowProcW` 会**就地把这块内存改成客户区**。
        let window_rect = if msg == WM_NCCALCSIZE && lparam != 0 {
            Some(unsafe {
                if wparam != 0 {
                    (*(lparam as *const NcCalcSizeParams)).rgrc[1]
                } else {
                    *(lparam as *const Rect)
                }
            })
        } else {
            None
        };
        // 先让 GPUI 算（它管顶边与最大化补偿），再改三边
        let result = unsafe { CallWindowProcW(previous, hwnd, msg, wparam, lparam) };
        if let Some(window) = window_rect {
            unsafe {
                let client = if wparam != 0 {
                    &mut (*(lparam as *mut NcCalcSizeParams)).rgrc[0]
                } else {
                    &mut *(lparam as *mut Rect)
                };
                client.left = window.left;
                client.right = window.right;
                client.bottom = window.bottom;
            }
        } else if msg == WM_NCHITTEST && result == HTCLIENT {
            // GPUI 认了「这是客户区」，但那块地方本来属于系统边框 → 补回缩放热区
            if let Some(edge) = unsafe { frame_edge(hwnd, lparam) } {
                return edge;
            }
        } else if msg == WM_GETMINMAXINFO && lparam != 0 {
            // 最小尺寸里那圈幻影边框（见 `FRAME_INSETS`）要减掉，否则窗口比画面高一条
            unsafe {
                let info = &mut *(lparam as *mut MinMaxInfo);
                let insets = [
                    FRAME_INSETS[0].load(Ordering::Relaxed),
                    FRAME_INSETS[1].load(Ordering::Relaxed),
                ];
                info.min_track.x = (info.min_track.x - insets[0]).max(1);
                info.min_track.y = (info.min_track.y - insets[1]).max(1);
            }
        }
        result
    }

    /// 光标在左 / 右 / 下边框（含两个下角）附近时给出对应的 `HT*` 码。
    unsafe fn frame_edge(hwnd: Hwnd, lparam: isize) -> Option<isize> {
        if hwnd == 0 || unsafe { IsZoomed(hwnd) } != 0 || unsafe { IsIconic(hwnd) } != 0 {
            return None; // 最大化/最小化时没有边框可拉
        }
        // 全屏时 GPUI 会把 `WS_THICKFRAME` 摘掉：那会儿整块屏幕都是内容，不该能拉边
        let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
        if style & WS_THICKFRAME == 0 {
            return None;
        }
        let bits = lparam as u32;
        let mut point = Point {
            x: (bits & 0xFFFF) as u16 as i16 as i32,
            y: ((bits >> 16) & 0xFFFF) as u16 as i16 as i32,
        };
        let mut rect = Rect::default();
        unsafe {
            ScreenToClient(hwnd, &mut point);
            if GetClientRect(hwnd, &mut rect) == 0 {
                return None;
            }
        }
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let grip = (RESIZE_GRIP * scale).round().max(1.0) as i32;
        let (width, height) = (rect.right, rect.bottom);
        let left = point.x < grip;
        let right = point.x >= width - grip;
        let bottom = point.y >= height - grip;
        Some(match (left, right, bottom) {
            (true, _, true) => HTBOTTOMLEFT,
            (_, true, true) => HTBOTTOMRIGHT,
            (true, _, _) => HTLEFT,
            (_, true, _) => HTRIGHT,
            (_, _, true) => HTBOTTOM,
            _ => return None,
        })
    }

    pub fn make_client_full_bleed(hwnd: Hwnd) {
        if hwnd == 0 || PREVIOUS_WNDPROC.load(Ordering::Relaxed) != 0 {
            return;
        }
        // 趁客户区还没铺满，量一下这圈边框有多厚（GPUI 就是这么量的，见 FRAME_INSETS）
        let mut window = Rect::default();
        let mut client = Rect::default();
        if unsafe { GetWindowRect(hwnd, &mut window) != 0 && GetClientRect(hwnd, &mut client) != 0 }
        {
            FRAME_INSETS[0].store(
                (window.right - window.left) - client.right,
                Ordering::Relaxed,
            );
            FRAME_INSETS[1].store(
                (window.bottom - window.top) - client.bottom,
                Ordering::Relaxed,
            );
        }
        let previous =
            unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, full_bleed_proc as *const () as isize) };
        if previous == 0 {
            return;
        }
        PREVIOUS_WNDPROC.store(previous, Ordering::Relaxed);
        // 换过窗口过程不会自己重算客户区，得主动触发一次
        unsafe {
            SetWindowPos(
                hwnd,
                0,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
        crate::persistence::log_session(&format!(
            "client full bleed: WM_NCCALCSIZE hooked (frame insets {}x{})",
            FRAME_INSETS[0].load(Ordering::Relaxed),
            FRAME_INSETS[1].load(Ordering::Relaxed),
        ));
    }

    pub fn client_size(hwnd: Hwnd) -> (i32, i32) {
        if hwnd == 0 {
            return (0, 0);
        }
        let mut client = Rect::default();
        if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
            return (0, 0);
        }
        (client.right, client.bottom)
    }

    pub fn client_matches_window(hwnd: Hwnd) -> bool {
        if hwnd == 0 {
            return true;
        }
        let mut window = Rect::default();
        let mut client = Rect::default();
        let ok = unsafe {
            GetWindowRect(hwnd, &mut window) != 0 && GetClientRect(hwnd, &mut client) != 0
        };
        if !ok {
            return true;
        }
        (window.right - window.left) == client.right
            && (window.bottom - window.top) == client.bottom
    }

    pub fn refresh_client_area(hwnd: Hwnd) {
        if hwnd == 0 {
            return;
        }
        unsafe {
            SetWindowPos(
                hwnd,
                0,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }

    pub fn kick_layout(hwnd: Hwnd) {
        if hwnd == 0 {
            return;
        }
        let mut client = Rect::default();
        if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
            return;
        }
        let width = client.right.max(1) as u32 & 0xFFFF;
        let height = client.bottom.max(1) as u32 & 0xFFFF;
        unsafe {
            PostMessageW(
                hwnd,
                WM_SIZE,
                SIZE_RESTORED,
                ((height << 16) | width) as isize,
            )
        };
    }

    pub fn cursor_pos() -> (i32, i32) {
        let mut point = Point::default();
        if unsafe { GetCursorPos(&mut point) } == 0 {
            return (0, 0);
        }
        (point.x, point.y)
    }

    pub fn window_origin(hwnd: Hwnd) -> (i32, i32) {
        if hwnd == 0 {
            return (0, 0);
        }
        let mut rect = Rect::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
            return (0, 0);
        }
        (rect.left, rect.top)
    }

    pub fn move_window(hwnd: Hwnd, x: i32, y: i32) {
        if hwnd == 0 {
            return;
        }
        // 尺寸参数传 0 + `SWP_NOSIZE`：只移动不改尺寸，系统就不会发 `WM_SIZE`，
        // 也就不会在拖动每帧里触发一次重排（GPUI 的尺寸处理只认 `WM_SIZE`）。
        // `SWP_NOZORDER` 保住 z 序（`SetWindowPos` 默认会把窗口提到最前），
        // `SWP_NOACTIVATE` 不抢焦点——拖动时窗口本来就该是当前活动的。
        unsafe {
            SetWindowPos(
                hwnd,
                0,
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    pub fn set_zoomed(hwnd: Hwnd, zoomed: bool) {
        if hwnd == 0 {
            return;
        }
        let cmd = if zoomed { SW_MAXIMIZE } else { SW_RESTORE };
        unsafe {
            ShowWindowAsync(hwnd, cmd);
        }
    }

    pub fn set_rounded(hwnd: Hwnd, rounded: bool) {
        if hwnd == 0 {
            return;
        }
        let value: u32 = if rounded {
            DWMWCP_ROUND
        } else {
            DWMWCP_DONOTROUND
        };
        unsafe {
            // 失败只可能是「系统不认识这条属性」（Win10），窗口保持直角即可
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&value as *const u32).cast(),
                std::mem::size_of::<u32>() as u32,
            );
        }
    }

    pub fn fit_to_video(hwnd: Hwnd, video: (u32, u32)) -> bool {
        if hwnd == 0 {
            return false;
        }
        let mut outer = Rect::default();
        let mut client = Rect::default();
        let ok = unsafe {
            GetWindowRect(hwnd, &mut outer) != 0 && GetClientRect(hwnd, &mut client) != 0
        };
        if !ok {
            return false;
        }

        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut info = MonitorInfo {
            cb_size: std::mem::size_of::<MonitorInfo>() as u32,
            ..Default::default()
        };
        if monitor == 0 || unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return false;
        }

        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };

        let scene = FitScene {
            video,
            outer: (
                outer.left,
                outer.top,
                outer.right - outer.left,
                outer.bottom - outer.top,
            ),
            client: (client.right - client.left, client.bottom - client.top),
            work: (
                info.work.left,
                info.work.top,
                info.work.right - info.work.left,
                info.work.bottom - info.work.top,
            ),
            min_client: (MIN_CLIENT.0 * scale, MIN_CLIENT.1 * scale),
            margin: (SCREEN_MARGIN * scale).round() as i32,
        };
        let Some(plan) = plan_fit(scene) else {
            return false;
        };
        let (x, y) = plan.origin;
        let (w, h) = plan.outer;
        let unchanged = plan.same_as(&scene);
        // 每部片子只记一行：出问题时能看出「按什么算的、算成了什么」，又不刷屏。
        crate::persistence::log_session(&format!(
            "fit_to_video: video={}x{} client={}x{} -> outer={w}x{h}@({x},{y}){}",
            video.0,
            video.1,
            scene.client.0,
            scene.client.1,
            if unchanged { " unchanged" } else { "" },
        ));
        if unchanged {
            return false; // 已经是这个尺寸了，别再打一次 SetWindowPos（整窗重绘一次）
        }

        unsafe { SetWindowPos(hwnd, 0, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE) != 0 }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Hwnd;

    pub fn is_zoomed(_hwnd: Hwnd) -> bool {
        false
    }

    pub fn set_zoomed(_hwnd: Hwnd, _zoomed: bool) {}

    pub fn set_rounded(_hwnd: Hwnd, _rounded: bool) {}

    pub fn make_client_full_bleed(_hwnd: Hwnd) {}

    pub fn client_matches_window(_hwnd: Hwnd) -> bool {
        true
    }

    pub fn client_size(_hwnd: Hwnd) -> (i32, i32) {
        (0, 0)
    }

    pub fn refresh_client_area(_hwnd: Hwnd) {}

    pub fn kick_layout(_hwnd: Hwnd) {}

    /// macOS / Linux 上拖动直接用 `Window::start_window_move()`（那边后端实现的是
    /// 系统标准拖动），这三个函数只为 Windows 的 `--wid` 迁移后「鼠标按住画面拖窗口」
    /// 提供同一套签名，非 Windows 下没人调用。
    pub fn cursor_pos() -> (i32, i32) {
        (0, 0)
    }

    pub fn window_origin(_hwnd: Hwnd) -> (i32, i32) {
        (0, 0)
    }

    pub fn move_window(_hwnd: Hwnd, _x: i32, _y: i32) {}

    pub fn fit_to_video(_hwnd: Hwnd, _video: (u32, u32)) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{plan_fit, FitScene};

    /// 一台 4K 屏、1.0 缩放的现场：外框比客户区大 16×40（边框 + 阴影），
    /// 窗口在屏幕中间偏左上。
    fn scene_4k(video: (u32, u32)) -> FitScene {
        FitScene {
            video,
            outer: (800, 400, 1280, 800),
            client: (1264, 760),
            work: (0, 0, 3840, 2088),
            min_client: (720.0, 480.0),
            margin: 12,
        }
    }

    /// 画得下就 1:1：窗口客户区正好等于画面像素，画面不该被缩放。
    #[test]
    fn fits_video_at_one_to_one_when_it_fits() {
        let scene = scene_4k((1920, 1080));
        let plan = plan_fit(scene).expect("应给出方案");
        assert_eq!(plan.client, (1920, 1080), "客户区应等于画面尺寸");
        assert_eq!(plan.outer, (1936, 1120), "外框 = 客户区 + 边框");
        // 以原窗口中心为锚点：(800+640, 400+400) = (1440, 800)
        assert_eq!(plan.origin, (1440 - 968, 800 - 560));
        assert!(!plan.same_as(&scene), "尺寸变了就不能算一致");
    }

    /// 4K 片子放在 1080p 屏上：等比缩到工作区里，比例不能变形。
    #[test]
    fn shrinks_oversized_video_into_work_area() {
        let mut scene = scene_4k((3840, 2160));
        scene.work = (0, 0, 1920, 1040);
        let plan = plan_fit(scene).expect("应给出方案");

        let ratio = plan.client.0 as f32 / plan.client.1 as f32;
        assert!(
            (ratio - 3840.0 / 2160.0).abs() < 0.01,
            "比例被拉变形了：{}",
            ratio
        );
        assert!(
            plan.outer.0 <= 1920 && plan.outer.1 <= 1040,
            "放不进工作区：{:?}",
            plan.outer
        );
        // 高度先顶到上限（16:9 在 16:9 的屏幕上高度先不够用）
        assert_eq!(plan.client.1, 1040 - 40 - 24, "应贴到工作区允许的最大高度");
    }

    /// 320×240 的小片子：窗口有最小尺寸，缩不下去就只能放大窗口，
    /// 否则画面居中缩在一小块、四周全是黑边。
    #[test]
    fn upscales_tiny_video_to_min_window() {
        let mut scene = scene_4k((320, 240));
        scene.min_client = (1080.0, 720.0); // 1.5 倍缩放下的 720×480
        let plan = plan_fit(scene).expect("应给出方案");
        assert_eq!(plan.client, (1080, 810), "应按 4:3 放大到最小宽度");
        assert!(
            plan.client.0 >= 1080 && plan.client.1 >= 720,
            "不小于窗口最小尺寸：{:?}",
            plan.client
        );
    }

    /// 画面尺寸还没拿到（音频 / 未解码）时什么都不能做。
    #[test]
    fn ignores_unknown_video_size() {
        assert!(plan_fit(scene_4k((0, 0))).is_none());
        assert!(plan_fit(scene_4k((1920, 0))).is_none());
    }

    /// 已经吻合时不要再动窗口（每动一次都是整窗重绘 + 画面闪一下）。
    #[test]
    fn already_fitted_is_reported_unchanged() {
        let scene = FitScene {
            video: (1920, 1080),
            outer: (0, 0, 1920, 1080),
            client: (1920, 1080),
            work: (0, 0, 3840, 2088),
            min_client: (720.0, 480.0),
            margin: 12,
        };
        let plan = plan_fit(scene).expect("应给出方案");
        assert!(plan.same_as(&scene));
    }
}
