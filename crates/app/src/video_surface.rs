//! 画面帧 → GPUI 纹理。
//!
//! mpv 的软件渲染把每一帧写成 BGRA，本模块负责把它（和从它派生的两个小东西）
//! 包成 `RenderImage` 交给界面：
//!
//! | 产物 | 用途 | 大小 |
//! |---|---|---|
//! | 整帧 | 舞台上的画面 | 原生尺寸（≤2560×1440） |
//! | 底部那块 48×20 的缩略图 | 胶囊玻璃的模糊底 | 每帧 ~3.8KB |
//! | 色相 | 玻璃染色 | 一个 f32 |
//!
//! ## 为什么要「退休队列」
//!
//! GPUI 的图集按 `(image_id, frame_index)` 缓存，**每帧都是一张新图**（新 `ImageId`）。
//! 不主动 `drop_image` 的话，图集里的 tile 只增不减，播几分钟就会把纹理撑爆
//! （Windows 单个图集上限 16384²，超了就是 `failed to allocate`）；但立刻删也不行——
//! 上一帧的场景可能还没上屏，删早了那一帧会缺贴图。折中是**留最后两三帧**再删。
//!
//! ## 每帧一次分配
//!
//! `RenderImage` 拿走 `Frame` 的所有权，所以每帧都会新分配一块像素内存（1080p ≈ 8MB）。
//! 这个量级在 30fps 下是 240MB/s 的分配/释放，堆分配器（Windows 的 LFH）扛得住；
//! 想再省只能改成「复用同一个 `ImageId` 的贴图」，那要上游开放 `RenderImage` 的
//! 数据字段（0.3.9 里是 `pub(crate)`），不在本轮范围。

use std::sync::Arc;

use gpui_kit::{ImageSource, RenderImage, Window};
use player_core::FrameRef;

/// 胶囊玻璃用的缩略图尺寸。
///
/// 48×20 拉到 760×92 是十几倍的放大，GPU 的双线性插值本身就等价于一个半径很大的
/// 模糊——所以这层「模糊」的代价只有每帧一次 3.8KB 的采样。
const BLUR_WIDTH: usize = 48;
const BLUR_HEIGHT: usize = 20;

/// 退休队列长度：留两帧，保证「上一帧还在上屏」时贴图仍在。
const RETIRED_KEEP: usize = 2;

/// 一块 BGRA 像素 → `RenderImage`。
///
/// `image` crate 的 `Rgba` 在这里只是「四个通道的容器」，GPUI 按 **BGRA** 解释内存
/// （它自己的 SVG 光栅化也是先 RGBA 再逐像素换成 BGRA，见 `swap_rgba_pa_to_bgra`），
/// 所以 mpv 的 `bgr0`/`bgra` 直接塞进来即可，不要额外换通道。
fn render_image(width: usize, height: usize, bgra: &[u8]) -> Option<Arc<RenderImage>> {
    if width == 0 || height == 0 || bgra.len() < width * height * 4 {
        return None;
    }
    let buffer = image::RgbaImage::from_raw(width as u32, height as u32, bgra.to_vec())?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// 界面当前持有的画面（没有视频时为「空」）。
#[derive(Default)]
pub struct VideoSurface {
    /// 舞台上的整帧
    image: Option<Arc<RenderImage>>,
    /// 胶囊玻璃的模糊底
    blur: Option<Arc<RenderImage>>,
    /// 已经被换下来、但可能还在上屏的帧
    retired: Vec<Arc<RenderImage>>,
    /// 画面底部那块的色相（给玻璃染色用）
    tint: Option<f32>,
    /// 当前帧的像素尺寸（0×0 = 还没有画面）
    size: (usize, usize),
}

impl VideoSurface {
    pub fn size(&self) -> (usize, usize) {
        self.size
    }

    pub fn tint(&self) -> Option<f32> {
        self.tint
    }

    /// 舞台用的画面。
    pub fn image(&self) -> Option<ImageSource> {
        self.image.clone().map(ImageSource::Render)
    }

    /// 胶囊玻璃用的模糊底。
    pub fn blur(&self) -> Option<ImageSource> {
        self.blur.clone().map(ImageSource::Render)
    }

    /// 收下一帧：建纹理、算色相与缩略图，并把该退休的旧图从图集里删掉。
    pub fn accept(&mut self, frame: FrameRef<'_>, window: &mut Window) {
        // 纹理必须在窗口线程上建（`drop_image` 同理），所以这一整套都在 UI 线程。
        let Some(image) = render_image(frame.width, frame.height, frame.data) else {
            return;
        };
        let blur_bytes = frame.band_thumbnail(BLUR_WIDTH, BLUR_HEIGHT);
        let blur = blur_bytes
            .as_deref()
            .and_then(|bytes| render_image(BLUR_WIDTH, BLUR_HEIGHT, bytes));

        self.tint = frame.band_hue();
        self.size = (frame.width, frame.height);

        if let Some(old) = self.image.replace(image) {
            self.retired.push(old);
        }
        if let Some(new_blur) = blur {
            if let Some(old) = self.blur.replace(new_blur) {
                self.retired.push(old);
            }
        }
        self.drain_retired(window);
    }

    /// 退出视频 / 换片：把还挂着的贴图全部还给图集。
    pub fn clear(&mut self, window: &mut Window) {
        self.image = None;
        self.blur = None;
        self.tint = None;
        self.size = (0, 0);
        for image in self.retired.drain(..) {
            let _ = window.drop_image(image);
        }
    }

    fn drain_retired(&mut self, window: &mut Window) {
        while self.retired.len() > RETIRED_KEEP {
            let image = self.retired.remove(0);
            let _ = window.drop_image(image);
        }
    }
}
