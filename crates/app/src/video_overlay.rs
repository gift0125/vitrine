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
//! 视频区域内的 GPUI 内容（如浮层、圆角遮罩）会被原生窗口遮住——这是该方案固有的代价，
//! 换来的是 mpv 原生渲染性能（硬件解码 + GPU 输出，零像素搬运）。

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
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> Hwnd;
        fn GetLastError() -> u32;
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: Hwnd,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
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
            Some(hwnd)
        }
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
            SetWindowPos(
                hwnd,
                0,
                x,
                y,
                width.max(1),
                height.max(1),
                flags,
            );
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
    use super::Hwnd;

    pub fn create_overlay(_owner: Hwnd) -> Option<Hwnd> {
        None
    }
    pub fn destroy(_hwnd: Hwnd) {}
    pub fn show(_hwnd: Hwnd) {}
    pub fn hide(_hwnd: Hwnd) {}
    pub fn set_rect(_hwnd: Hwnd, _x: i32, _y: i32, _w: i32, _h: i32, _visible: bool) {}
    pub fn client_origin(_hwnd: Hwnd) -> (i32, i32) {
        (0, 0)
    }
}

/// 视频叠加窗口。
pub struct VideoOverlay {
    hwnd: Hwnd,
    visible: bool,
    rect: (i32, i32, i32, i32),
}

impl VideoOverlay {
    /// 创建所有者为主窗口的隐藏弹窗。
    pub fn create(owner: Hwnd) -> Option<Self> {
        let hwnd = win::create_overlay(owner)?;
        Some(Self {
            hwnd,
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

    /// 设置位置尺寸（物理屏幕像素）。位置未变化时不重复调用 SetWindowPos。
    pub fn set_rect(&mut self, x: i32, y: i32, width: i32, height: i32) {
        let rect = (x, y, width.max(1), height.max(1));
        if rect == self.rect {
            return;
        }
        self.rect = rect;
        win::set_rect(self.hwnd, rect.0, rect.1, rect.2, rect.3, self.visible);
    }

    pub fn show(&mut self) {
        if !self.visible {
            self.visible = true;
            win::set_rect(self.hwnd, self.rect.0, self.rect.1, self.rect.2, self.rect.3, true);
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
