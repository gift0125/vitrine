//! 画面帧 → GPUI 纹理。
//!
//! mpv 的软件渲染把每一帧写成 BGRA，本模块负责把它（和从它派生的两个小东西）
//! 包成 `RenderImage` 交给界面：
//!
//! | 产物 | 用途 | 大小 |
//! |---|---|---|
//! | 整帧 | 舞台上的画面 | 原生尺寸（≤2560×1440） |
//! | 液态玻璃贴图 | 胶囊 / 视频信息 / 播放列表的折射底 | 半分辨率，按面单独烤 |
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

use crate::liquid_glass::{GlassBaker, GlassJob};

/// 退休队列长度。
///
/// 每一帧可能同时换掉「画面 + 最多三块玻璃」。留两轮，上一轮还在上屏时贴图仍在。
const RETIRED_KEEP: usize = 8;

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
    /// 已经被换下来、但可能还在上屏的帧
    retired: Vec<Arc<RenderImage>>,
    /// 画面底部那块的色相（给玻璃染色用）
    tint: Option<f32>,
    /// 当前帧的像素尺寸（0×0 = 还没有画面）
    size: (usize, usize),
    /// 液态玻璃的 CPU 烘焙器（两档模糊 + 边缘折射）
    glass: GlassBaker,
    /// 0 胶囊 / 1 视频信息 / 2 播放列表
    plates: [Option<Arc<RenderImage>>; 3],
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

    /// 液态玻璃贴图。`0` 胶囊，`1` 视频信息，`2` 播放列表。还没烤出来时是 `None`。
    pub fn glass(&self, id: u8) -> Option<ImageSource> {
        self.plates
            .get(id as usize)
            .and_then(|slot| slot.clone())
            .map(ImageSource::Render)
    }

    /// 收下一帧：建纹理、算色相，并把像素交给液态玻璃烘焙器。
    ///
    /// 玻璃贴图不在这里烤——那时候还不知道胶囊有没有被歌单挤到旁边。`sync_glass`
    /// 在布局时按真实位置烤。
    pub fn accept(&mut self, frame: FrameRef<'_>, window: &mut Window) {
        // 纹理必须在窗口线程上建（`drop_image` 同理），所以这一整套都在 UI 线程。
        let Some(image) = render_image(frame.width, frame.height, frame.data) else {
            return;
        };
        self.glass.ingest(frame.width, frame.height, frame.data);
        self.tint = frame.band_hue();
        self.size = (frame.width, frame.height);

        if let Some(old) = self.image.replace(image) {
            self.retired.push(old);
        }
        self.drain_retired(window);
    }

    /// 按窗口里这几块玻璃面的位置重烤贴图。位置和帧都没变就什么也不做。
    pub fn sync_glass(
        &mut self,
        win_w: f32,
        win_h: f32,
        jobs: &[GlassJob],
        hue: Option<f32>,
        window: &mut Window,
    ) {
        if !self.glass.sync(win_w, win_h, jobs, hue) {
            return;
        }
        let dirty: Vec<(u8, usize, usize, Vec<u8>)> = self
            .glass
            .plates()
            .filter(|(_, _, _, _, dirty)| *dirty)
            .map(|(id, w, h, bytes, _)| (id, w, h, bytes.to_vec()))
            .collect();
        let present: Vec<u8> = self.glass.plates().map(|(id, ..)| id).collect();
        self.glass.clear_dirty();
        for (id, w, h, bytes) in dirty {
            let Some(image) = render_image(w, h, &bytes) else {
                continue;
            };
            let slot = &mut self.plates[id as usize];
            if let Some(old) = slot.replace(image) {
                self.retired.push(old);
            }
        }
        for (index, slot) in self.plates.iter_mut().enumerate() {
            if !present.contains(&(index as u8)) {
                if let Some(old) = slot.take() {
                    self.retired.push(old);
                }
            }
        }
        self.drain_retired(window);
    }

    /// 退出视频 / 换片：把还挂着的贴图全部还给图集。
    pub fn clear(&mut self, window: &mut Window) {
        self.image = None;
        self.tint = None;
        self.size = (0, 0);
        self.glass.clear();
        for slot in &mut self.plates {
            if let Some(old) = slot.take() {
                self.retired.push(old);
            }
        }
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
