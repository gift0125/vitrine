//! 软件渲染：把 mpv 的每一帧拿进**我们自己的**合成树。
//!
//! ## 为什么值得
//!
//! 在此之前画面是 mpv 自己画进一个自建顶层窗（`wid` 选项），主窗口只是「在画面
//! 上面挖几个洞」。那套做法能跑，但代价一条条堆着：
//!
//! | 两个窗（`wid` 叠加） | 一个窗（本模块） |
//! |---|---|
//! | 主窗区域 = 整窗 − 画面 + chrome（`SetWindowRgn` 逐帧算） | 不需要区域 |
//! | 叠加窗 z 序每帧核对（`ShowWindow` 会把它提到最前） | 不存在 |
//! | 画面区收不到 GPUI 事件（`WM_MOUSEMOVE` 要自己桥接） | 正常事件流 |
//! | 区域级 backdrop blur 拿不到 | 画面像素在我们手里，**自己模糊** |
//! | 圆角要两块窗一起设 | 一块窗 |
//!
//! 换来的代价是每帧一次 CPU 缩放 + 一次内存拷贝（`MPV_RENDER_API_TYPE_SW`），
//! 所以渲染尺寸按「视频原生尺寸 × 舞台尺寸」取小（见 [`fit_render_size`]），
//! 绝不为 4K 片源在一个小窗口里做 4K 的 CPU 缩放。
//!
//! ## 线程契约（照 `render.h` 抄）
//!
//! 同一个 `mpv_render_context` 的所有调用必须在**创建它的那个线程**上：
//! `mpv_render_context_create` 在 UI 线程调，`render` 也只能在 UI 线程调。
//! 唯一的例外是 update 回调——它由 mpv 的任意内部线程调起，里面**不能碰 mpv 的任何
//! API**，我们只往一个 channel 里丢个信号。
//!
//! ## 像素格式
//!
//! 软件渲染只接受 `rgb0` / `bgr0` / `0bgr` / `0rgb` 这几个 4 字节格式，**没有 alpha**
//! （第 4 个字节是「未定义的垃圾」，文档原话）。而 GPUI 的纹理是 BGRA、按 alpha 合成，
//! 所以每帧渲染完要把第 4 字节刷成 255；漏了这一步的表现是「画面上什么都没有」
//! （整帧 alpha 0），而且截图也是空的——很容易误判成渲染没跑起来。

use std::ffi::c_void;
use std::sync::Arc;

use crate::libmpv::{FnRenderUpdateFn, LibMpv};

/// 单帧像素数上限（≈2560×1440）。
///
/// 软件渲染是 CPU 缩放 + 全帧 memcpy，成本随像素数线性涨：1080p 每帧 ~8MB，
/// 4K 就是 33MB，30fps 下 1GB/s 的搬运量，主线程会被拖垮。4K 片源在 4K 窗里
/// 渲染成 2.5K 再让 GPU 拉上去，肉眼差别远小于「掉帧」。
pub const MAX_FRAME_PIXELS: usize = 2560 * 1440;

/// 取色/缩略图用的画面区域：底部中间 60%×38%。
///
/// 与设计稿 `L2_frame_tint.sample_region` 同一块——它正好是悬浮控制栏背后那一带，
/// 所以「玻璃的色温」和「玻璃挡住的画面」是同一个东西。
const BAND_X0: f32 = 0.20;
const BAND_X1: f32 = 0.80;
const BAND_Y0: f32 = 0.62;

/// 取均值时的目标采样点数：再多也对结果没有影响，但要把一次取色压在 1ms 量级。
const BAND_SAMPLES: f32 = 20_000.0;

/// 一块 BGRA 画面缓冲（行距 = 宽 × 4，**紧密排列**）。
///
/// 紧密排列是硬要求：GPUI 的图集上传按 `宽 × 4` 当源行距（`UpdateSubresource`），
/// 行距比它宽的话每一行都会错位（画面斜着裂开）。
pub struct FrameBuffer {
    data: Vec<u8>,
    width: usize,
    height: usize,
}

impl Default for FrameBuffer {
    fn default() -> Self {
        Self {
            data: Vec::new(),
            width: 0,
            height: 0,
        }
    }
}

impl FrameBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// 保证缓冲区容得下 `width × height`；尺寸变了就重新分配。
    pub fn ensure(&mut self, width: usize, height: usize) {
        let width = width.max(1);
        let height = height.max(1);
        if self.width == width && self.height == height && self.data.len() >= width * height * 4 {
            return;
        }
        self.width = width;
        self.height = height;
        self.data = vec![0u8; width * height * 4];
        // 刚分配出来 alpha 全是 0：先刷一遍，免得第一帧渲染失败时画出「全透明」。
        for pixel in self.data.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }

    pub fn pixels_ptr(&mut self) -> *mut u8 {
        self.data.as_mut_ptr()
    }

    pub fn frame(&self) -> FrameRef<'_> {
        FrameRef {
            width: self.width,
            height: self.height,
            data: &self.data,
        }
    }

    /// 把 mpv 留下的第 4 字节刷成不透明（见模块注释「像素格式」）。
    pub fn set_opaque(&mut self) {
        for pixel in self.data.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }
}

/// 只读的一帧（BGRA）。
#[derive(Clone, Copy)]
pub struct FrameRef<'a> {
    pub width: usize,
    pub height: usize,
    /// 紧密排列的 BGRA
    pub data: &'a [u8],
}

impl<'a> FrameRef<'a> {
    /// 底部中间那块的平均色（`(r, g, b)`，0~255）。
    pub fn band_mean(&self) -> Option<(f32, f32, f32)> {
        band_mean(self.data, self.width, self.height, 4)
    }

    /// 同一块的色相（0~360）。设计稿只要色相，饱和度和明度在玻璃那层固定。
    pub fn band_hue(&self) -> Option<f32> {
        let (r, g, b) = self.band_mean()?;
        Some(rgb_hue(r, g, b))
    }

    /// 同一块的**模糊缩略图**（`tw × th`，BGRA、alpha 255）。
    ///
    /// 这是 L3 那层玻璃的来源：把 60%×38% 的画面压成几十像素的小图，再由 GPU
    /// 双线性拉满整块玻璃——等于做了一次「大半径 box blur」，代价是每帧几千字节。
    /// 它**不是**空间精确的背景模糊（缩略图整块铺在胶囊上，不对齐画面位置），
    /// 但玻璃本来就只有 48% 不透明度，看的是色温不是细节。
    pub fn band_thumbnail(&self, tw: usize, th: usize) -> Option<Vec<u8>> {
        let tw = tw.max(1);
        let th = th.max(1);
        if self.width < 4 || self.height < 4 || self.data.len() < self.width * self.height * 4 {
            return None;
        }
        let x0 = (self.width as f32 * BAND_X0) as usize;
        let x1 = ((self.width as f32 * BAND_X1) as usize).min(self.width);
        let y0 = (self.height as f32 * BAND_Y0) as usize;
        let y1 = self.height;
        let (bw, bh) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
        if bw == 0 || bh == 0 {
            return None;
        }

        let mut out = vec![0u8; tw * th * 4];
        for ty in 0..th {
            let sy0 = y0 + bh * ty / th;
            let sy1 = (y0 + bh * (ty + 1) / th).max(sy0 + 1);
            for tx in 0..tw {
                let sx0 = x0 + bw * tx / tw;
                let sx1 = (x0 + bw * (tx + 1) / tw).max(sx0 + 1);
                // 每块最多抽 ~256 个点：块再大也只是模糊，取全量白费 CPU。
                let area = (sx1 - sx0) * (sy1 - sy0);
                let step = (((area as f32) / 256.0).sqrt() as usize).max(1);
                let (mut sb, mut sg, mut sr, mut n) = (0u32, 0u32, 0u32, 0u32);
                let mut y = sy0;
                while y < sy1 {
                    let row = y * self.width;
                    let mut x = sx0;
                    while x < sx1 {
                        let base = (row + x) * 4;
                        sb += self.data[base] as u32;
                        sg += self.data[base + 1] as u32;
                        sr += self.data[base + 2] as u32;
                        n += 1;
                        x += step;
                    }
                    y += step;
                }
                let n = n.max(1);
                let base = (ty * tw + tx) * 4;
                out[base] = (sb / n) as u8;
                out[base + 1] = (sg / n) as u8;
                out[base + 2] = (sr / n) as u8;
                out[base + 3] = 255;
            }
        }
        Some(out)
    }
}

/// 底部中间那块的均值。`bpp` 是每像素字节数（BGRA = 4）。
fn band_mean(data: &[u8], width: usize, height: usize, bpp: usize) -> Option<(f32, f32, f32)> {
    if width < 4 || height < 4 || data.len() < width * height * bpp {
        return None;
    }
    let x0 = (width as f32 * BAND_X0) as usize;
    let x1 = ((width as f32 * BAND_X1) as usize).min(width);
    let y0 = (height as f32 * BAND_Y0) as usize;
    let y1 = height;
    let (bw, bh) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    if bw == 0 || bh == 0 {
        return None;
    }
    let step = (((bw * bh) as f32 / BAND_SAMPLES).sqrt() as usize).max(1);

    let (mut sb, mut sg, mut sr, mut n) = (0u64, 0u64, 0u64, 0u64);
    let mut y = y0;
    while y < y1 {
        let row = y * width;
        let mut x = x0;
        while x < x1 {
            let base = (row + x) * bpp;
            sb += data[base] as u64;
            sg += data[base + 1] as u64;
            sr += data[base + 2] as u64;
            n += 1;
            x += step;
        }
        y += step;
    }
    if n == 0 {
        return None;
    }
    let n = n as f32;
    Some((sr as f32 / n, sg as f32 / n, sb as f32 / n))
}

/// RGB → 色相（0~360 度，饱和度/明度丢掉不要）。
///
/// 设计稿只要色相：直接把画面均值混进玻璃等于把它染得更黑（暗场景几乎全黑，
/// 混完看不出色温），固定 S/L 才能让每一部片子的玻璃都落在同一个观感上。
pub fn rgb_hue(r: f32, g: f32, b: f32) -> f32 {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    if delta <= f32::EPSILON {
        return 0.0; // 灰阶没有色相
    }
    let h = if max == r {
        ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    h * 60.0
}

/// 每帧该渲染多大（设备像素）。
///
/// 三条约束，按优先级：
/// 1. **只缩不放**：舞台比视频原生尺寸大时按原生尺寸渲染，放大交给 GPU。软件渲染
///    的每一点像素都是 CPU 时间，而 GPU 拉一张已经够大的图几乎免费——720p 视频在
///    4K 窗里按 4K 渲染，等于为了让画质提升一点点去放弃帧率。
/// 2. **保持视频宽高比**：只算「装得进舞台的那个盒子」，mpv 就不会自己加黑边，
///    画面可以按 `object_fit: contain` 铺——窗口拉伸时只换 letterbox，不必等一帧。
/// 3. **像素预算**：[`MAX_FRAME_PIXELS`]，4K 片源 + 4K 窗也只渲染 2.5K。
pub fn fit_render_size(
    stage_width: f32,
    stage_height: f32,
    native: Option<(u32, u32)>,
    scale: f32,
) -> (usize, usize) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let mut width = (stage_width * scale).round().max(16.0);
    let mut height = (stage_height * scale).round().max(16.0);

    if let Some((nw, nh)) = native {
        let (nw, nh) = (nw as f32, nh as f32);
        if nw >= 16.0 && nh >= 16.0 {
            // 装进舞台（contain）并且**不超过原生尺寸**——两个上限取小就是答案。
            let fit = (width / nw).min(height / nh).min(1.0);
            width = nw * fit;
            height = nh * fit;
        }
    }

    // 像素预算
    let pixels = width * height;
    if pixels > MAX_FRAME_PIXELS as f32 {
        let k = (MAX_FRAME_PIXELS as f32 / pixels).sqrt();
        width *= k;
        height *= k;
    }

    (width.round().max(16.0) as usize, height.round().max(16.0) as usize)
}

/// 一个 `mpv_render_context`（软件后端）。
///
/// 生命周期：`create` → 反复 `set_update_callback` / `render` → `Drop`（自动 free）。
/// `mpv_render_context_free` 必须在 `mpv_terminate_destroy` **之前**调，所以
/// `VideoEngine` 关停时先 drop 它再关 mpv。
pub struct RenderContext {
    lib: Arc<LibMpv>,
    ptr: *mut c_void,
    /// update 回调的用户数据（`Box<Box<dyn Fn>>`，见 [`Self::set_update_callback`]）。
    callback: Option<*mut CallbackSlot>,
    /// 创建线程：`render` 只能在这个线程上调（`render.h` 的约定）。
    owner: std::thread::ThreadId,
}

/// 回调的用户数据壳子：`*mut c_void` 传不了胖指针，所以双装箱。
struct CallbackSlot(Box<dyn Fn() + Send + Sync>);

/// mpv 的 update 回调（**任意线程**调用，里面不能碰 mpv API）。
unsafe extern "C" fn update_trampoline(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` 只可能来自 `set_update_callback` 里 `Box::into_raw` 的那个壳子，
    // 且它在被替换/Drop 之前一直有效。
    let slot = &*(ctx as *const CallbackSlot);
    (slot.0)();
}

impl RenderContext {
    /// 创建软件渲染上下文。失败时返回 mpv 的错误码（调用方用 `error_string` 翻译）。
    pub fn create(lib: Arc<LibMpv>, mpv: *mut c_void) -> Result<Self, i32> {
        if !lib.has_render_api() {
            return Err(-1);
        }
        let (ptr, rc) = lib.render_create_sw(mpv);
        if ptr.is_null() {
            return Err(rc);
        }
        Ok(Self {
            lib,
            ptr,
            callback: None,
            owner: std::thread::current().id(),
        })
    }

    pub fn is_owner_thread(&self) -> bool {
        std::thread::current().id() == self.owner
    }

    /// 换一个「有新帧了」的回调。
    ///
    /// 回调只该做一件事：把信号丢进队列，然后立刻返回。它跑在 mpv 的内部线程上，
    /// 调 mpv API（包括 `mpv_render_context_render`）会死锁。
    pub fn set_update_callback(&mut self, callback: Option<Box<dyn Fn() + Send + Sync>>) {
        // 先摘掉旧回调，再释放它的壳子——顺序反了就是「mpv 手里握着一个已释放的指针」。
        self.lib
            .render_set_update_callback(self.ptr, None, std::ptr::null_mut());
        if let Some(old) = self.callback.take() {
            // SAFETY: 指针来自 `Box::into_raw`，并且刚刚已经从这个上下文摘掉。
            unsafe { drop(Box::from_raw(old)) };
        }
        if let Some(callback) = callback {
            let raw = Box::into_raw(Box::new(CallbackSlot(callback)));
            self.callback = Some(raw);
            let tramp: FnRenderUpdateFn = update_trampoline;
            self.lib
                .render_set_update_callback(self.ptr, Some(tramp), raw as *mut c_void);
        }
    }

    /// 自上次以来有没有新帧。可选，但便宜，用来跳过「其实没有新帧」的空转渲染。
    pub fn has_new_frame(&self) -> bool {
        self.lib.render_update(self.ptr) & crate::libmpv::MPV_RENDER_UPDATE_FRAME != 0
    }

    /// 渲染当前帧到 `buffer`（尺寸由 `buffer` 决定，改了尺寸会自动重新配置缩放器）。
    pub fn render(&self, buffer: &mut FrameBuffer) -> Result<(), String> {
        debug_assert!(
            self.is_owner_thread(),
            "mpv_render_context 只能在创建它的线程上渲染"
        );
        let (width, height) = (buffer.width(), buffer.height());
        if width == 0 || height == 0 {
            return Err("渲染尺寸为 0".into());
        }
        let rc = self.lib.render_frame(
            self.ptr,
            width,
            height,
            width * 4,
            buffer.pixels_ptr(),
        );
        if rc < 0 {
            return Err(self.lib.error_string(rc));
        }
        buffer.set_opaque();
        Ok(())
    }

    /// 报一次「这一帧已经上屏」，让 mpv 的显示时刻估算更准。
    pub fn report_swap(&self) {
        self.lib.render_report_swap(self.ptr);
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        // 顺序：摘回调 → 释放壳子 → free 上下文。反了就可能在 free 之后被回调。
        self.lib
            .render_set_update_callback(self.ptr, None, std::ptr::null_mut());
        if let Some(raw) = self.callback.take() {
            // SAFETY: 见 `set_update_callback`。
            unsafe { drop(Box::from_raw(raw)) };
        }
        self.lib.render_free(self.ptr);
        self.ptr = std::ptr::null_mut();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 色相：纯红 0°、纯绿 120°、纯蓝 240°，灰阶没有色相。
    #[test]
    fn hue_matches_the_color_wheel() {
        assert!((rgb_hue(255.0, 0.0, 0.0) - 0.0).abs() < 0.5);
        assert!((rgb_hue(0.0, 255.0, 0.0) - 120.0).abs() < 0.5);
        assert!((rgb_hue(0.0, 0.0, 255.0) - 240.0).abs() < 0.5);
        assert_eq!(rgb_hue(90.0, 90.0, 90.0), 0.0, "灰阶没有色相");
        // 设计稿举例的偏蓝底 `rgb(46,62,71)`：色相落在 190~205 之间（他们写 197°）
        let hue = rgb_hue(46.0, 62.0, 71.0);
        assert!(
            (190.0..=205.0).contains(&hue),
            "偏蓝底应落在蓝青区间，实际 {hue}"
        );
    }

    /// 底部那块均值：只统计 y ≥ 62% 的区域，通道顺序是 B,G,R。
    #[test]
    fn band_mean_only_looks_at_the_bottom_band() {
        // 64x64 的 BGRA：上半纯红、下半纯蓝
        let (w, h) = (64usize, 64usize);
        let mut data = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let base = (y * w + x) * 4;
                if y < h / 2 {
                    data[base + 2] = 255; // R
                } else {
                    data[base] = 255; // B
                }
                data[base + 3] = 255;
            }
        }
        let frame = FrameRef {
            width: w,
            height: h,
            data: &data,
        };
        let (r, g, b) = frame.band_mean().expect("应有均值");
        assert!(b > 200.0 && r < 40.0, "只看下半部分：r={r} b={b}");
        assert!(g < 40.0);
        // 色相落在蓝色区间
        let hue = frame.band_hue().expect("应有色相");
        assert!((200.0..=260.0).contains(&hue), "蓝色色相，实际 {hue}");
    }

    /// 尺寸为 0 / 数据不够时给 None，不 panic（首帧之前就是这种情况）。
    #[test]
    fn degenerate_frames_do_not_panic() {
        let empty = FrameRef {
            width: 0,
            height: 0,
            data: &[],
        };
        assert!(empty.band_mean().is_none());
        assert!(empty.band_hue().is_none());
        assert!(empty.band_thumbnail(4, 4).is_none());
    }

    /// 缩略图：尺寸对、不透明、颜色和源区域一致（不做白平衡也不串通道）。
    #[test]
    fn band_thumbnail_keeps_the_band_color_and_is_opaque() {
        let (w, h) = (120usize, 100usize);
        let mut data = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let base = (y * w + x) * 4;
                // 刻意让上下半区颜色不同：缩略图只该看到下半区（y ≥ 62%）
                let in_band = y >= 62;
                data[base] = if in_band { 0 } else { 255 }; // B
                data[base + 1] = if in_band { 128 } else { 0 }; // G
                data[base + 2] = if in_band { 255 } else { 0 }; // R
                data[base + 3] = 255;
            }
        }
        let frame = FrameRef {
            width: w,
            height: h,
            data: &data,
        };
        let thumb = frame.band_thumbnail(8, 4).expect("应有缩略图");
        assert_eq!(thumb.len(), 8 * 4 * 4);
        for pixel in thumb.chunks_exact(4) {
            assert_eq!(pixel[3], 255, "缩略图必须不透明");
            assert!(pixel[2] > 240, "取的是下半区的红：{:?}", pixel);
            assert!((pixel[1] as i32 - 128).abs() <= 2, "绿通道：{:?}", pixel);
            assert!(pixel[0] < 10, "蓝通道：{:?}", pixel);
        }
    }

    /// 舞台和原生尺寸的组合：只缩不放、保持宽高比、有像素预算。
    #[test]
    fn fit_render_size_respects_native_and_aspect() {
        // 720p 视频放进 1280×720 的窗口：正好 1:1
        assert_eq!(
            fit_render_size(1280.0, 720.0, Some((1280, 720)), 1.0),
            (1280, 720)
        );
        // 窗口比视频大：不放大，放大交给 GPU
        assert_eq!(
            fit_render_size(2560.0, 1440.0, Some((1280, 720)), 1.0),
            (1280, 720)
        );
        // 4:3 视频（原生就比窗口小）放进 16:9 窗口：仍按原生渲染，靠布局留黑边
        assert_eq!(
            fit_render_size(1280.0, 720.0, Some((640, 480)), 1.0),
            (640, 480)
        );
        // 4K 视频缩进 720p 窗口：按窗口的 16:9 盒子，等比缩小
        assert_eq!(
            fit_render_size(1280.0, 720.0, Some((3840, 2160)), 1.0),
            (1280, 720)
        );
        // 高 DPI：舞台 1280×720 在 150% 下就是 1920×1080 的设备像素
        assert_eq!(
            fit_render_size(1280.0, 720.0, Some((1920, 1080)), 1.5),
            (1920, 1080)
        );
        // 还不知道原生尺寸时按舞台给
        assert_eq!(fit_render_size(800.0, 600.0, None, 1.0), (800, 600));
        // 4K 片源 + 4K 窗：撞像素预算，比例不变
        let (w, h) = fit_render_size(3840.0, 2160.0, Some((3840, 2160)), 1.0);
        assert!(w * h <= MAX_FRAME_PIXELS + 4096, "{w}x{h} 超预算");
        assert!((w as f32 / h as f32 - 16.0 / 9.0).abs() < 0.01, "比例要保住");
    }

    /// 缓冲区换尺寸要重分配，同尺寸不重分配（每帧都分配会拖慢）。
    #[test]
    fn frame_buffer_reallocates_only_on_size_change() {
        let mut buffer = FrameBuffer::new();
        buffer.ensure(4, 2);
        assert_eq!((buffer.width(), buffer.height()), (4, 2));
        assert_eq!(buffer.frame().data.len(), 4 * 2 * 4);
        let capacity = buffer.frame().data.as_ptr();
        buffer.ensure(4, 2);
        assert_eq!(buffer.frame().data.as_ptr(), capacity, "同尺寸不该重分配");
        buffer.ensure(2, 2);
        assert_eq!((buffer.width(), buffer.height()), (2, 2));
        // 刚分配时 alpha 也不该是 0（第一帧渲染失败时不能画出全透明）
        assert!(buffer.frame().data.chunks_exact(4).all(|p| p[3] == 255));
    }
}
