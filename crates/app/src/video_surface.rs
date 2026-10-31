//! 画面帧 → GPUI 纹理。
//!
//! mpv 的软件渲染把每一帧写成 BGRA，本模块负责把它（和从它派生的两个小东西）
//! 包成 `RenderImage` 交给界面：
//!
//! | 产物 | 用途 | 大小 |
//! |---|---|---|
//! | 整帧 | 舞台上的画面 | 原生尺寸（≤2560×1440） |
//! | 液态玻璃贴图 | 胶囊 / 视频信息 / 播放列表的折射底 | 屏幕 1:1，按面单独烤 |
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
//!
//! ## 玻璃在后台烤
//!
//! 三块面一起烤要十毫秒上下，放在 UI 线程上会和帧泵、输入抢同一个核。所以烘焙器
//! 整个借给后台（[`GlassTicket`]），烤完连同包好的贴图一起还回来（[`GlassDone`]）；
//! 在烤的这段时间界面照旧摆上一版贴图。帧也不在 UI 线程上拷：后台拿舞台那张图的
//! `Arc` 自己去降采样。
//!
//! 一次只派一单。在烤的时候来的新请求只记一笔「过期了」，烤完叫一次重绘，由那次
//! 重绘按**最新**状态再派——不排队，旧请求自然作废。

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{ImageSource, RenderImage, Window};
use player_core::FrameRef;

use crate::liquid_glass::{bake_key, BakeKey, GlassBaker, GlassJob, BAKE_MIN_INTERVAL};

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

/// 一块玻璃贴图。
struct Plate {
    image: Arc<RenderImage>,
    /// 带光尘留边：比面大一圈、圆角在 alpha 里裁，只能摆在根节点上，
    /// 不能塞回面自己的 div（那里会被 `overflow_hidden` 和 opacity 套住）。
    padded: bool,
}

/// 交给后台的一单烘焙：烘焙器本身 + 这一刻的全部输入。
pub struct GlassTicket {
    baker: GlassBaker,
    frame: Arc<RenderImage>,
    frame_tag: u64,
    frame_size: (usize, usize),
    win_w: f32,
    win_h: f32,
    jobs: Vec<GlassJob>,
    hue: Option<f32>,
    dpi: f32,
    /// 界面那边撤过贴图：烘焙器以为它们还在，得让它全部重出。
    force: bool,
    epoch: u64,
}

/// 后台烤完交回来的东西。
pub struct GlassDone {
    baker: GlassBaker,
    changed: bool,
    plates: Vec<(u8, Plate)>,
    present: Vec<u8>,
    epoch: u64,
}

impl GlassTicket {
    /// 在后台线程上跑：收帧（降采样）、烤、把有变化的贴图包成 `RenderImage`。
    pub fn run(self) -> GlassDone {
        let GlassTicket {
            mut baker,
            frame,
            frame_tag,
            frame_size: (frame_w, frame_h),
            win_w,
            win_h,
            jobs,
            hue,
            dpi,
            force,
            epoch,
        } = self;
        if !baker.has_frame(frame_tag) {
            if let Some(bytes) = frame.as_bytes(0) {
                baker.ingest_frame(frame_tag, frame_w, frame_h, bytes);
            }
        }
        if force {
            baker.invalidate();
        }
        let changed = baker.sync(win_w, win_h, &jobs, hue, dpi);
        let mut plates = Vec::new();
        let mut present = Vec::new();
        if changed {
            for (id, w, h, bytes, dirty) in baker.plates() {
                present.push(id);
                if !dirty {
                    continue;
                }
                let padded = jobs.iter().any(|job| job.id == id && job.dust.is_some());
                if let Some(image) = render_image(w, h, bytes) {
                    plates.push((id, Plate { image, padded }));
                }
            }
            baker.clear_dirty();
        }
        GlassDone {
            baker,
            changed,
            plates,
            present,
            epoch,
        }
    }
}

/// [`VideoSurface::plan_glass`] 的结论。
pub enum GlassPlan {
    /// 不用动：没变、在烤（烤完会叫重绘）、或一块面都没有。
    Idle,
    /// 节流中：过这么久再叫一次重绘，让最后那一帧补上。
    Later(Duration),
    /// 交给后台去烤。
    Bake(GlassTicket),
}

/// 界面当前持有的画面（没有视频时为「空」）。
pub struct VideoSurface {
    /// 舞台上的整帧（软件渲染）
    image: Option<Arc<RenderImage>>,
    /// 舞台上的整帧（mac 的 GPU 通路：IOSurface 上的 NV12，GPUI 零拷贝合成）
    #[cfg(target_os = "macos")]
    pixels: Option<player_core::PixelBuffer>,
    /// GPU 通路：舞台换过几帧 / 玻璃的源图取自第几帧。不等就该再回读一次。
    #[cfg(target_os = "macos")]
    stage_serial: u64,
    #[cfg(target_os = "macos")]
    glass_serial: u64,
    /// 已经被换下来、但可能还在上屏的帧
    retired: Vec<Arc<RenderImage>>,
    /// 画面底部那块的色相（给玻璃染色用）
    tint: Option<f32>,
    /// 当前帧的像素尺寸（0×0 = 还没有画面）
    size: (usize, usize),
    /// 液态玻璃的 CPU 烘焙器（两档模糊 + 边缘折射）。后台在烤时被借走（`None`）。
    baker: Option<GlassBaker>,
    /// 交给玻璃的那一帧（就是舞台那张图）和它的编号
    glass_frame: Option<Arc<RenderImage>>,
    frame_tag: u64,
    /// 最近一次派给后台的输入。相同就不再派。
    glass_key: Option<BakeKey>,
    glass_started: Option<Instant>,
    /// 最近一次 `plan_glass` 收到的输入（不管派没派）：烤回来的贴图只认这里还有的面。
    glass_wanted: Option<BakeKey>,
    /// 在烤的时候又来了新请求：烤完要再叫一次重绘。
    glass_stale: bool,
    /// 界面这边撤过贴图，下一单要让烘焙器全部重出。
    glass_force: bool,
    /// 节流时已经约了一次延后的重绘，别重复约。
    glass_wake_armed: bool,
    /// `clear` 时加一：之前派出去的那一单烤回来也不认了。
    glass_epoch: u64,
    /// 0 胶囊 / 1 视频信息 / 2 播放列表
    plates: [Option<Plate>; 3],
}

impl Default for VideoSurface {
    fn default() -> Self {
        Self {
            image: None,
            #[cfg(target_os = "macos")]
            pixels: None,
            #[cfg(target_os = "macos")]
            stage_serial: 0,
            #[cfg(target_os = "macos")]
            glass_serial: 0,
            retired: Vec::new(),
            tint: None,
            size: (0, 0),
            baker: Some(GlassBaker::default()),
            glass_frame: None,
            frame_tag: 0,
            glass_key: None,
            glass_started: None,
            glass_wanted: None,
            glass_stale: false,
            glass_force: false,
            glass_wake_armed: false,
            glass_epoch: 0,
            plates: [None, None, None],
        }
    }
}

impl VideoSurface {
    pub fn size(&self) -> (usize, usize) {
        self.size
    }

    pub fn tint(&self) -> Option<f32> {
        self.tint
    }

    /// 舞台用的画面（软件渲染）。
    pub fn image(&self) -> Option<ImageSource> {
        self.image.clone().map(ImageSource::Render)
    }

    /// 舞台用的画面（GPU 通路）。
    #[cfg(target_os = "macos")]
    pub fn pixels(&self) -> Option<player_core::PixelBuffer> {
        self.pixels.clone()
    }

    /// GPU 通路收下一帧：只换舞台，不碰像素（玻璃的源图另取，见 [`Self::accept_glass_source`]）。
    #[cfg(target_os = "macos")]
    pub fn accept_pixels(&mut self, buffer: player_core::PixelBuffer, window: &mut Window) {
        self.size = player_core::pixel_buffer_size(&buffer);
        self.pixels = Some(buffer);
        self.stage_serial = self.stage_serial.wrapping_add(1);
        if let Some(old) = self.image.take() {
            self.retired.push(old);
            self.drain_retired(window);
        }
    }

    /// 玻璃的源图落后于舞台了吗（GPU 通路才会出现：源图是按需回读的）。
    #[cfg(target_os = "macos")]
    pub fn glass_source_stale(&self) -> bool {
        self.glass_serial != self.stage_serial || self.glass_frame.is_none()
    }

    /// GPU 通路：回读来的缩小图当玻璃的源（色相也从它算）。它不上舞台，不进图集。
    #[cfg(target_os = "macos")]
    pub fn accept_glass_source(&mut self, frame: FrameRef<'_>) {
        let Some(image) = render_image(frame.width, frame.height, frame.data) else {
            return;
        };
        self.tint = frame.band_hue();
        self.glass_frame = Some(image);
        self.frame_tag = self.frame_tag.wrapping_add(1);
        self.glass_serial = self.stage_serial;
    }

    /// 液态玻璃贴图（和面一样大、整块不透明，圆角由 `img().rounded()` 裁）。
    /// `0` 胶囊，`1` 视频信息，`2` 播放列表。还没烤出来、或手上是光尘那张时是 `None`。
    pub fn glass(&self, id: u8) -> Option<ImageSource> {
        self.plate(id, false)
    }

    /// 带光尘留边的那张（比面大 `DUST_PAD` 一圈，圆角裁在 alpha 里）。
    pub fn glass_padded(&self, id: u8) -> Option<ImageSource> {
        self.plate(id, true)
    }

    fn plate(&self, id: u8, padded: bool) -> Option<ImageSource> {
        self.plates
            .get(id as usize)
            .and_then(|slot| slot.as_ref())
            .filter(|plate| plate.padded == padded)
            .map(|plate| ImageSource::Render(plate.image.clone()))
    }

    /// 收下一帧：建纹理、算色相，并记下它给液态玻璃用。
    ///
    /// 玻璃贴图不在这里烤——那时候还不知道胶囊有没有被歌单挤到旁边。`plan_glass`
    /// 在布局时按真实位置派单。
    pub fn accept(&mut self, frame: FrameRef<'_>, window: &mut Window) {
        // 纹理必须在窗口线程上建（`drop_image` 同理），所以这一整套都在 UI 线程。
        let Some(image) = render_image(frame.width, frame.height, frame.data) else {
            return;
        };
        self.tint = frame.band_hue();
        self.size = (frame.width, frame.height);
        self.glass_frame = Some(image.clone());
        self.frame_tag = self.frame_tag.wrapping_add(1);
        // 软件渲染的舞台图本身就是玻璃的源，两边永远同步
        #[cfg(target_os = "macos")]
        {
            self.stage_serial = self.stage_serial.wrapping_add(1);
            self.glass_serial = self.stage_serial;
        }

        if let Some(old) = self.image.replace(image) {
            self.retired.push(old);
        }
        self.drain_retired(window);
    }

    /// 按窗口里这几块玻璃面的位置决定要不要重烤。
    ///
    /// 贴图按 `dpi`（`window.scale_factor()`）烘到物理像素（1:1），圆角和 14px 折射
    /// 带才不会在半分辨率放大后糊成阶梯。
    pub fn plan_glass(
        &mut self,
        win_w: f32,
        win_h: f32,
        jobs: &[GlassJob],
        hue: Option<f32>,
        dpi: f32,
        window: &mut Window,
    ) -> GlassPlan {
        let key = bake_key(self.frame_tag, win_w, win_h, hue, dpi, jobs);
        self.glass_wanted = Some(key);
        // 面撤了，贴图立刻撤：不必等后台，光尘那张也不该多挂一帧。
        for (index, slot) in self.plates.iter_mut().enumerate() {
            if !key.has_job(index as u8) {
                if let Some(old) = slot.take() {
                    self.retired.push(old.image);
                    self.glass_force = true;
                }
            }
        }
        self.drain_retired(window);

        let Some(frame) = self.glass_frame.clone() else {
            return GlassPlan::Idle;
        };
        if key.is_empty() {
            // 控制层藏起来的大部分播放时间都在这里：不烤、也不收帧。
            self.glass_key = None;
            return GlassPlan::Idle;
        }
        if self.glass_key == Some(key) {
            return GlassPlan::Idle;
        }
        let Some(baker) = self.baker.take() else {
            self.glass_stale = true;
            return GlassPlan::Idle;
        };
        // 只有「又来了一帧、布局没变」才节流。开合面板必须这一帧就跟上；光尘在跑时
        // 玻璃 pass 走缓存（见 `liquid_glass::DUST_BASE_REFRESH`），逐帧的只有粒子。
        if let (Some(prev), Some(at)) = (self.glass_key, self.glass_started) {
            let waited = at.elapsed();
            if prev.same_layout(&key) && !key.has_dust() && waited < BAKE_MIN_INTERVAL {
                self.baker = Some(baker);
                if std::mem::replace(&mut self.glass_wake_armed, true) {
                    return GlassPlan::Idle;
                }
                return GlassPlan::Later(BAKE_MIN_INTERVAL - waited);
            }
        }
        self.glass_key = Some(key);
        self.glass_started = Some(Instant::now());
        self.glass_stale = false;
        GlassPlan::Bake(GlassTicket {
            baker,
            frame,
            frame_tag: self.frame_tag,
            frame_size: self.size,
            win_w,
            win_h,
            jobs: jobs.to_vec(),
            hue,
            dpi,
            force: std::mem::take(&mut self.glass_force),
            epoch: self.glass_epoch,
        })
    }

    /// 节流约的那次重绘到了。
    pub fn glass_woke(&mut self) {
        self.glass_wake_armed = false;
    }

    /// 收回后台烤好的一单。返回要不要重绘。
    pub fn finish_glass(&mut self, done: GlassDone, window: &mut Window) -> bool {
        let GlassDone {
            mut baker,
            changed,
            plates,
            present,
            epoch,
        } = done;
        if epoch != self.glass_epoch {
            // 派单之后退出了视频：贴图还没上过屏，直接丢；烘焙器还回来留着下次用。
            baker.clear();
            self.baker = Some(baker);
            return false;
        }
        self.baker = Some(baker);
        let mut repaint = std::mem::take(&mut self.glass_stale);
        if changed {
            let wanted = self.glass_wanted;
            let live = |id: u8| wanted.is_some_and(|key| key.has_job(id));
            for (id, plate) in plates {
                // 派单之后撤掉的面：烤回来也不挂
                if !live(id) {
                    continue;
                }
                if let Some(old) = self.plates[id as usize].replace(plate) {
                    self.retired.push(old.image);
                }
            }
            for (index, slot) in self.plates.iter_mut().enumerate() {
                if !present.contains(&(index as u8)) {
                    if let Some(old) = slot.take() {
                        self.retired.push(old.image);
                    }
                }
            }
            repaint = true;
        }
        self.drain_retired(window);
        repaint
    }

    /// 退出视频 / 换片：把还挂着的贴图全部还给图集。
    pub fn clear(&mut self, window: &mut Window) {
        self.image = None;
        #[cfg(target_os = "macos")]
        {
            self.pixels = None;
        }
        self.tint = None;
        self.size = (0, 0);
        self.glass_frame = None;
        self.glass_key = None;
        self.glass_wanted = None;
        self.glass_stale = false;
        self.glass_force = false;
        self.glass_epoch = self.glass_epoch.wrapping_add(1);
        if let Some(baker) = self.baker.as_mut() {
            baker.clear();
        }
        for slot in &mut self.plates {
            if let Some(old) = slot.take() {
                self.retired.push(old.image);
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
