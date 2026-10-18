//! Win32 顶层叠加窗口：mpv 以 `--wid` 挂载于此，覆盖在 GPUI 视频区之上。
//!
//! ## 为什么需要它
//!
//! GPUI 的 Windows 后端使用 DirectComposition（窗口带 `WS_EX_NOREDIRECTIONBITMAP`），
//! 这种窗口没有重定向表面，**无法承载原生子窗口**。因此视频不能直接画在 GPUI 窗口内部，
//! 只能由一个「无边框、不抢焦点、由主窗口拥有（owned）」的顶层弹窗盖在视频区位置上方。
//!
//! ## 坐标约定
//!
//! GPUI 的布局坐标是逻辑像素（相对窗口客户区）。这里统一用
//! `set_rect(x, y, w, h)` 接收**物理屏幕像素**，由调用方完成
//! `客户区原点（ClientToScreen） + 逻辑坐标 × scale_factor` 的换算。
//!
//! ## 已知约束
//!
//! 视频区域内的 GPUI 内容会被这层原生窗口遮住——这是该方案固有的代价，
//! 换来的是 mpv 原生渲染性能（硬件解码 + GPU 输出，零像素搬运）。
//!
//! 但「遮住」不等于只能让位：给叠加窗口设**窗口区域**（`SetWindowRgn`）就能把
//! chrome 的几块从画面里挖掉，露出下面的 GPUI 主窗口——标题带、控制胶囊、歌单面板
//! 因此可以压在画面上，而不是把画面挤小。见 [`Hole`]。

use std::ffi::c_void;
use std::sync::Once;

/// Win32 窗口句柄。
pub type Hwnd = isize;

const WS_POPUP: u32 = 0x8000_0000;
const WS_CLIPCHILDREN: u32 = 0x0200_0000;
const WS_CLIPSIBLINGS: u32 = 0x0400_0000;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;

const SW_HIDE: i32 = 0;
const SW_SHOWNA: i32 = 8;

/// 文件拖放：这块窗口盖住了整个视频区，也就是窗口 80% 的面积。
/// GPUI 的 `IDropTarget` 注册在**主窗口**上，而 OLE 的拖放目标查找是按
/// `WindowFromPoint` 从光标下的窗口开始的（只沿父链向上找），走到叠加窗口就断了，
/// 拖进去只会看到「禁止」光标。所以叠加窗口自己接一份。
const WM_DROPFILES: u32 = 0x0233;
/// 按下画面：转成「按住标题栏」，交给系统做窗口拖动。
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_NCLBUTTONDOWN: u32 = 0x00A1;
const HTCAPTION: usize = 2;
const GW_OWNER: u32 = 4;

/// 区域合并方式：从整窗里减掉一块。
const RGN_DIFF: i32 = 4;

/// 从叠加窗口里扣掉的一块矩形（**逻辑像素**，相对窗口左上角）。
///
/// 视频模式下画面铺满整窗，而标题带/胶囊/歌单是画在 GPUI 主窗口里的——
/// mpv 的叠加窗口浮在主窗口之上，不把这几块从它的可见区域里挖掉，
/// chrome 就永远被画面盖着（这正是「顶栏没有浮在视频上方」的成因）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hole {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// 圆角半径；0 = 直角
    pub radius: f32,
}

impl Hole {
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

const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;
const SWP_SHOWWINDOW: u32 = 0x0040;

const CLASS_NAME: &str = "SimpleMediaPlayerVideoOverlay";

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
        fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn ClientToScreen(hwnd: Hwnd, point: *mut Point) -> i32;
        fn GetWindow(hwnd: Hwnd, cmd: u32) -> Hwnd;
        fn SendMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn ReleaseCapture() -> i32;
        fn SetWindowRgn(hwnd: Hwnd, region: Hwnd, redraw: i32) -> i32;
    }

    // 区域（RGN）相关都在 gdi32 里，`SetWindowRgn` 在 user32。
    #[link(name = "gdi32")]
    extern "system" {
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

    /// 叠加窗口收到的拖放路径，等主线程的 100ms tick 来取走。
    ///
    /// `WM_DROPFILES` 只在 wndproc 里能拿到，拿完必须立刻 `DragFinish`；
    /// 而拿到的这一刻我们不在 GPUI 的 `App` 上下文里，不能直接改视图状态，
    /// 所以先落在队列里，由 tick 统一消费（和通知的「入队 + flush」同一套路）。
    static DROPPED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

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
            // 画面这块属于叠加窗口，按下去默认什么都不会发生（它还是个不抢焦点的
            // 工具窗）。这里转成「按住标题栏」交给系统，于是按住画面就能拖动整个窗体：
            // 主窗口是它的 owner，跟着一起走，画面靠 tick 的 sync_overlay_rect 追上。
            let owner = unsafe { GetWindow(hwnd, GW_OWNER) };
            if owner != 0 {
                unsafe {
                    ReleaseCapture();
                    SendMessageW(owner, WM_NCLBUTTONDOWN, HTCAPTION, 0);
                }
                return 0;
            }
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    static REGISTER: Once = Once::new();
    static mut CLASS_READY: bool = false;

    fn ensure_class() -> bool {
        REGISTER.call_once(|| {
            let class_name = wide(CLASS_NAME);
            let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
            let class = WndClassW {
                style: 0,
                lpfn_wnd_proc: wnd_proc as *const () as usize,
                cb_cls_extra: 0,
                cb_wnd_extra: 0,
                h_instance: instance,
                h_icon: 0,
                h_cursor: 0,
                hbr_background: 0, // 不擦背景，避免闪白
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

    pub fn create_overlay(owner: Hwnd) -> Option<Hwnd> {
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
                owner,
                0,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null_mut(),
            )
        };
        if hwnd == 0 {
            None
        } else {
            // 让 Explorer 把拖到这块窗口上的文件转成 WM_DROPFILES
            unsafe { DragAcceptFiles(hwnd, 1) };
            Some(hwnd)
        }
    }

    /// 取走并清空拖放队列（主线程 tick 调用）。
    pub fn take_dropped() -> Vec<String> {
        DROPPED
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

    /// 把 chrome 占的区域从窗口可见范围里挖掉（`holes` 为空 = 恢复整窗矩形）。
    ///
    /// 窗口区域既裁剪**渲染**也裁剪**命中测试**：挖掉的那几块会露出下面的 GPUI
    /// 主窗口（标题带/胶囊/歌单就画在那儿），点在洞里也会直接给主窗口。
    /// 传 `scale` 是因为区域只能用物理像素，而布局是逻辑像素。
    pub fn set_holes(hwnd: Hwnd, width: i32, height: i32, holes: &[Hole], scale: f32) {
        if hwnd == 0 {
            return;
        }
        if holes.is_empty() {
            // NULL 区域 = 整窗，交给系统接管（原来的区域会被系统释放）
            unsafe { SetWindowRgn(hwnd, 0, 1) };
            return;
        }
        let px = |value: f32| (value * scale).round() as i32;
        unsafe {
            let region = CreateRectRgn(0, 0, width.max(1), height.max(1));
            if region == 0 {
                return;
            }
            for hole in holes {
                let (left, top) = (px(hole.x), px(hole.y));
                let (right, bottom) = (px(hole.x + hole.width), px(hole.y + hole.height));
                let cut = if hole.radius > 0.0 {
                    // CreateRoundRectRgn 收的是椭圆的**宽高**，不是半径
                    let d = px(hole.radius) * 2;
                    CreateRoundRectRgn(left, top, right, bottom, d, d)
                } else {
                    CreateRectRgn(left, top, right, bottom)
                };
                if cut != 0 {
                    CombineRgn(region, region, cut, RGN_DIFF);
                    // CombineRgn 是复制语义，源区域用完要自己释放
                    DeleteObject(cut);
                }
            }
            // SetWindowRgn 成功后 region 归系统所有，**不要**再 DeleteObject
            SetWindowRgn(hwnd, region, 1);
        }
    }

    /// 主窗口客户区左上角在屏幕上的物理坐标。
    pub fn client_origin(hwnd: Hwnd) -> (i32, i32) {
        if hwnd == 0 {
            return (0, 0);
        }
        let mut point = Point::default();
        let ok = unsafe { ClientToScreen(hwnd, &mut point) };
        if ok == 0 {
            (0, 0)
        } else {
            (point.x, point.y)
        }
    }
}

#[cfg(not(windows))]
mod win {
    use super::{Hole, Hwnd};

    pub fn create_overlay(_owner: Hwnd) -> Option<Hwnd> {
        None
    }
    pub fn destroy(_hwnd: Hwnd) {}
    pub fn show(_hwnd: Hwnd) {}
    pub fn hide(_hwnd: Hwnd) {}
    pub fn set_rect(_hwnd: Hwnd, _x: i32, _y: i32, _w: i32, _h: i32, _visible: bool) {}
    pub fn set_holes(_hwnd: Hwnd, _w: i32, _h: i32, _holes: &[Hole], _scale: f32) {}
    pub fn client_origin(_hwnd: Hwnd) -> (i32, i32) {
        (0, 0)
    }
    pub fn take_dropped() -> Vec<String> {
        Vec::new()
    }
}

/// 视频叠加窗口。
pub struct VideoOverlay {
    hwnd: Hwnd,
    visible: bool,
    rect: (i32, i32, i32, i32),
    /// 当前挖掉的 chrome 区域（逻辑像素），窗口尺寸变化后要按新尺寸重新应用
    holes: Vec<Hole>,
    /// 逻辑 → 物理的换算比（区域只能用物理像素）
    scale: f32,
}

impl VideoOverlay {
    /// 创建所有者为主窗口的隐藏弹窗。
    pub fn create(owner: Hwnd) -> Option<Self> {
        let hwnd = win::create_overlay(owner)?;
        Some(Self {
            hwnd,
            visible: false,
            rect: (0, 0, 0, 0),
            holes: Vec::new(),
            scale: 1.0,
        })
    }

    pub fn hwnd(&self) -> Hwnd {
        self.hwnd
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// 设置位置尺寸（物理屏幕像素）。位置未变化时不重复调用 SetWindowPos。
    pub fn set_rect(&mut self, x: i32, y: i32, width: i32, height: i32) {
        let rect = (x, y, width.max(1), height.max(1));
        if rect == self.rect {
            return;
        }
        self.rect = rect;
        win::set_rect(self.hwnd, rect.0, rect.1, rect.2, rect.3, self.visible);
        // 区域不会跟着 SetWindowPos 自动缩放，改尺寸后必须重新算一遍
        if !self.holes.is_empty() {
            win::set_holes(self.hwnd, rect.2, rect.3, &self.holes, self.scale);
        }
    }

    /// 设置要从画面里挖掉的 chrome 区域（逻辑像素）。内容没变就什么都不做。
    pub fn set_holes(&mut self, holes: Vec<Hole>, scale: f32) {
        self.scale = scale;
        if holes == self.holes {
            return;
        }
        self.holes = holes;
        win::set_holes(self.hwnd, self.rect.2, self.rect.3, &self.holes, self.scale);
    }

    pub fn show(&mut self) {
        if !self.visible {
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
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            self.visible = false;
            win::hide(self.hwnd);
        }
    }
}

impl Drop for VideoOverlay {
    fn drop(&mut self) {
        win::destroy(self.hwnd);
    }
}

/// 主窗口客户区原点（物理屏幕坐标）。
pub fn client_origin(hwnd: Hwnd) -> (i32, i32) {
    win::client_origin(hwnd)
}

/// 取走叠加窗口收到的拖放路径。
pub fn take_dropped() -> Vec<String> {
    win::take_dropped()
}
