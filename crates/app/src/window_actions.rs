//! 主窗口的窗口动作：最大化 / 还原。
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

#[cfg(windows)]
mod imp {
    use super::{Hwnd, SW_MAXIMIZE, SW_RESTORE};

    #[link(name = "user32")]
    extern "system" {
        fn IsZoomed(hwnd: Hwnd) -> i32;
        fn ShowWindowAsync(hwnd: Hwnd, cmd: i32) -> i32;
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
}

#[cfg(not(windows))]
mod imp {
    use super::Hwnd;

    pub fn is_zoomed(_hwnd: Hwnd) -> bool {
        false
    }

    pub fn set_zoomed(_hwnd: Hwnd, _zoomed: bool) {}
}
