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

use crate::video_overlay::Hwnd;

const SW_MAXIMIZE: i32 = 3;
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

/// 客户区的最小尺寸（逻辑像素）：和 `main.rs` 的 `window_min_size` 保持一致。
///
/// 系统会把窗口压不到这个尺寸以下（GPUI 把它交给 `WM_GETMINMAXINFO`）。而按画面
/// 比例算出来的尺寸可能比它还小（低分辨率视频），那时只能反过来把窗口放大——
/// 窗口缩不下去、画面又没跟着放大，就会留一圈黑边。
pub const MIN_CLIENT: (f32, f32) = (720.0, 480.0);

/// 窗口与屏幕边缘之间留的余量（逻辑像素）。
///
/// 不留余量的话窗口会正好贴满工作区，看起来像"被最大化"了，边框也不见了。
const SCREEN_MARGIN: f32 = 12.0;

/// 一次「按画面尺寸定窗口」用到的现场事实（全是物理像素）。
///
/// 单独抽出来是为了让策略能被单测覆盖：真机上的工作区尺寸、DPI、边框厚度
/// 都不可控，纯函数才测得出「4K 片子在 1080p 屏上怎么放」这种分支。
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitPlan {
    /// 目标外框左上角
    pub origin: (i32, i32),
    /// 目标外框尺寸
    pub outer: (i32, i32),
    /// 目标客户区尺寸（= 画面应该铺满的区域）
    pub client: (i32, i32),
}

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
    use super::{plan_fit, FitScene, Hwnd, MIN_CLIENT, SCREEN_MARGIN, SW_MAXIMIZE, SW_RESTORE};

    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;

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
    struct MonitorInfo {
        cb_size: u32,
        monitor: Rect,
        work: Rect,
        flags: u32,
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
