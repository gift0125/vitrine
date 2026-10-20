//! L4 液态玻璃（默认档 `edge`）。
//!
//! 设计稿 `texture_layers.L4_liquid_glass` / `design/preview/index.html` 里那套
//! 位移映射，搬到 CPU 上：画面先按窗口做 **contain** 对位，再出两档模糊
//! （体部 σ=30、边圈 σ=7），玻璃边缘带沿圆角矩形的法线把边圈拉开最多 6.5px
//! （歌单 7px），中心不位移。暗底（胶囊 / 视频信息）在折射层上叠自适应压暗，
//! 浅底（播放列表）铺奶白，边圈不减薄。
//!
//! 只做 `edge`。`full`（Fresnel + 色散）留在 [`Tier::Full`] 里，给对照测试用，
//! 界面不切过去——设计稿把那一档标成加强档，默认不是它。
//!
//! 模糊用三次盒式模糊逼近高斯（半径跟 σ 走，单像素代价与半径无关），边圈缓冲
//! 长边封顶 1280、体部封顶 480：σ=30 的霜面本来就没有高频，没必要按窗口原尺寸算。
//!
//! 但**贴图本身按屏幕 1:1 出**（逻辑像素 × 缩放比，单块面 22 万像素封顶）。
//! 圆角是曲率，比直边更吃分辨率：半分辨率再放大时，14px 的折射带在弧上只剩
//! 几个采样点，圆角就成了阶梯加一圈灰黑毛边。边缘 alpha 的过渡带固定 1 个
//! **物理**像素，所以无论分辨率高低，圆角的柔软度都一样。
//!
//! ## 退场的光尘也在这张贴图里
//!
//! 控制栏退场时「散成尘」（设计稿 `chrome_auto_hide.motion`），粒子是这枚烘焙器里的
//! 第二道 pass，不是另一条渲染通道。理由两条：
//!
//! - **发光没有逐节点入口**。GPUI 的层叠是 source-over，想在画面上叠一层「光」得让
//!   每个粒子自己带混合模式；在 BGRA 缓冲里就是一次逐像素的加光。
//! - **两件事在时间上几乎不重叠**：胶囊本体 0.18s 就淡完，粒子接管到 0.64s；回场时
//!   粒子在聚、玻璃用缓存。所以不需要「玻璃贴图 + 粒子贴图」两层，一张分时复用。
//!
//! 带光尘的贴图往四周多留 [`DUST_PAD`]：粒子要飞出胶囊轮廓，贴图得跟着变大；胶囊
//! 本体那段淡出也烘进这张贴图的 alpha 里（[`DustPhase::fade`]），否则它挂在控制层的
//! 透明度下面，会跟着胶囊一起消失——而粒子恰恰要在那之后继续。

use std::time::{Duration, Instant};

/// 体部模糊的标准差（窗口逻辑像素）。必须和 L3 的 backdrop-blur 同一档。
const BLUR_BODY: f32 = 30.0;
/// 边圈模糊的标准差。斜边看过去是通透的，所以要比体部清。
const BLUR_RIM: f32 = 7.0;
/// 采样前的亮度（HTML `ctx.filter` 里的 `brightness(1.04)`）。
const BRIGHTNESS: f32 = 1.04;
/// 浅底体部奶白不透明度，边圈收到 [`LIGHT_VEIL_RIM`]（更实，不是更透）。
const LIGHT_VEIL: f32 = 0.87;
const LIGHT_VEIL_RIM: f32 = 0.93;
/// 奶白 `#EFF1F5`。
const MILK: [f32; 3] = [239.0, 241.0, 245.0];
/// 压暗色 `#0B0C10`。
const SCRIM_RGB: [f32; 3] = [11.0, 12.0, 16.0];
/// 画面留边（`overlay::canvas`，`#08090C`），BGRA。
const CANVAS_BGRA: [u8; 3] = [0x0C, 0x09, 0x08];

/// 每一块玻璃单独模糊自己背后那一截，长边封在这里。
/// 6.5px 的位移在这块缓冲里还有几个 texel，整窗不用跟着算。
const RIM_MAX_LONG: f32 = 420.0;
/// 体部缓冲长边上限。σ=30 的霜面没有高频。
const SOFT_MAX_LONG: f32 = 220.0;
/// 玻璃贴图的目标分辨率：和屏幕 **1:1**（逻辑像素 × 屏幕缩放比）。
///
/// 这里以前是 0.5（半分辨率再放大），圆角就是被它毁掉的：折射带 14px 宽在
/// 贴图里只剩 7 个像素，弧上这 7 个采样点再被拉成 21 个物理像素，于是圆角
/// 呈阶梯、边缘带在弧上糊成一坨。圆角是**曲率**，比直边更吃分辨率。
const PLATE_MAX_SCALE: f32 = 2.0;
/// 一块面的贴图像素上限。超了才按面积降分辨率（胶囊在 150% 缩放下是
/// 157k，正好在预算内；歌单 320×633 会超，见 [`plate_scale`]）。
const PLATE_BUDGET: f32 = 220_000.0;
/// 浅底（歌单）的奶白盖到 0.87 以上，折射只剩边上一圈，分辨率可以省。
const LIGHT_PLATE_RATIO: f32 = 0.72;
/// 覆盖区比玻璃面多出来的边（窗口像素）。要罩住边圈模糊核和朝外的那几像素位移。
const COVERAGE_MARGIN: f32 = 36.0;
/// 播放中玻璃底图的重烤间隔。
///
/// 贴图是屏幕 1:1 的，胶囊一块就 15.7 万像素、三块面一起 47 万（约 14ms CPU），
/// 每帧重烤会把预算吃满。玻璃里装的是模糊过的倒影，慢一两帧看不出来，所以
/// 只在**有画面在跑**时才节流；开合面板、改窗口这种布局变化照旧立刻重烤。
const BAKE_MIN_INTERVAL: Duration = Duration::from_millis(20);
/// 模糊用的源图长边上限。再大的帧先做面积平均，边圈 σ=7 用不上那些高频。
const SOURCE_MAX_LONG: f32 = 1920.0;

/// 退场光尘：散开 / 聚回各用多久（设计稿 `DUST.scatterMs` / `gatherMs`）。
pub const DUST_SCATTER: Duration = Duration::from_millis(640);
pub const DUST_GATHER: Duration = Duration::from_millis(520);
/// 粒子最远能跑出胶囊多远：外推 24 + 浮力 16 + 光点自身半径。贴图四周要留这么多，
/// 摆这张贴图的人也得知道（它比胶囊大一圈）。
pub const DUST_PAD: f32 = 48.0;
/// 带光尘时的贴图像素预算。玻璃 pass 走缓存，逐帧只有粒子在写，可以放宽。
const DUST_PLATE_BUDGET: f32 = 420_000.0;
/// 光尘期间玻璃底图的最长保鲜期。
///
/// 严格说玻璃必须每帧重烤才能跟上画面，但那正是 14ms/帧 的来源。退场时胶囊本体
/// 0.18s 就淡到 0，回场也只有 0.26s——这期间玻璃里装的是**模糊过的**倒影，定格
/// 一百来毫秒看不出来，而成本降到约 1/7。过了保鲜期照旧重烤。
const DUST_BASE_REFRESH: Duration = Duration::from_millis(120);
/// 粒子数（`DUST.count`）。
const DUST_COUNT: usize = 340;
/// 轮廓带宽度：只有贴着边这么深的一圈取点。
const DUST_BAND: f32 = 12.0;
/// 沿法线推开的距离上限。
const DUST_PUSH: f32 = 24.0;
/// 浮力：额外向上的位移。
const DUST_RISE: f32 = 16.0;
/// 基础 alpha（`DUST.alpha`）。
const DUST_ALPHA: f32 = 0.55;

/// 玻璃档位。界面固定 [`Tier::Edge`]。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// 边缘折射。默认档。
    Edge,
    /// 再加 Fresnel 高光和 R/B 色散。
    Full,
}

/// 底子往哪边靠。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    /// 胶囊 / 视频信息：自适应压暗。
    Dark,
    /// 播放列表：铺奶白，边圈更实。
    Light,
}

/// 退场光尘这一刻的相位。
///
/// 几何（位置、圆角）已经在 [`GlassJob`] 里了，这里只补「走到哪一步」——撒点和
/// 位移都要靠它。**它必须进烘焙 key**：只改相位时几何和帧都没变，`sync` 会判
/// 「没变」直接返回，粒子一帧都不会重画。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DustPhase {
    /// 0..=1 的进度
    pub u: f32,
    /// `true` = 散开（退场），`false` = 聚回（回场）
    pub scatter: bool,
    /// 胶囊本体这一刻的不透明度。淡出/淡入也烘进贴图 alpha——它不能挂在控制层的
    /// opacity 下面，否则粒子会跟着胶囊一起消失。
    pub fade: f32,
    /// 这一次退场/回场的编号：变了就重新撒点（两次退场不该是同一团气）。
    pub run: u32,
}

/// 一块玻璃面在窗口里的位置（逻辑像素）和这面自己的折射参数。
#[derive(Clone, Copy)]
pub struct GlassJob {
    pub id: u8,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    pub band: f32,
    pub maxd: f32,
    pub polarity: Polarity,
    /// 退场光尘。`None` = 这一块面不参与退场动效。
    pub dust: Option<DustPhase>,
}

impl GlassJob {
    /// 控制胶囊：半径 14，带 14，位移 6.5，暗底。
    pub fn capsule(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            id: 0,
            x,
            y,
            w,
            h,
            radius: 14.0,
            band: 14.0,
            maxd: 6.5,
            polarity: Polarity::Dark,
            dust: None,
        }
    }

    /// 视频信息：半径 12，其余跟胶囊一样。
    pub fn info(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            id: 1,
            x,
            y,
            w,
            h,
            radius: 12.0,
            band: 14.0,
            maxd: 6.5,
            polarity: Polarity::Dark,
            dust: None,
        }
    }

    /// 播放列表：带 18、位移 7，浅底。
    pub fn playlist(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            id: 2,
            x,
            y,
            w,
            h,
            radius: 14.0,
            band: 18.0,
            maxd: 7.0,
            polarity: Polarity::Light,
            dust: None,
        }
    }

    /// 让这块面参与退场光尘。
    pub fn with_dust(mut self, phase: DustPhase) -> Self {
        self.dust = Some(phase);
        self
    }

    /// 贴图四周要留的边：有光尘时粒子会飞出轮廓。
    fn pad(&self) -> f32 {
        if self.dust.is_some() {
            DUST_PAD
        } else {
            0.0
        }
    }
}

/// 相位也要量化进 key，否则粒子一帧都不会重画。
///
/// `u` 取 1/65534（散开 640ms 上约 0.01ms 一档）、`fade` 取 1/255：比肉眼能分辨的
/// 细得多，但足以让每一帧都判成「变了」。`run` 让两次退场重新撒点。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DustKey {
    u_q: u16,
    fade_q: u8,
    scatter: bool,
    run: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct JobKey {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    dust: Option<DustKey>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BakeKey {
    gen: u64,
    win_w: u32,
    win_h: u32,
    hue: u16,
    /// 缩放比（×64 定点）。换了显示器就得按新倍率重烤。
    dpi_q: u16,
    jobs: [Option<JobKey>; 3],
}

struct Plate {
    id: u8,
    w: usize,
    h: usize,
    bgra: Vec<u8>,
    dirty: bool,
}

/// 模糊缓冲在窗口里盖住的矩形。采样时把窗口坐标换进这块缓冲。
#[derive(Clone, Copy, Default)]
struct Space {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// 浅底几乎被奶白盖住，边圈不需要和暗底同一档采样密度。
fn backdrop_limits(polarity: Polarity) -> (f32, f32) {
    match polarity {
        Polarity::Dark => (RIM_MAX_LONG, SOFT_MAX_LONG),
        Polarity::Light => (200.0, 110.0),
    }
}

/// 这块面按多少倍**逻辑像素**烘焙。目标就是屏幕的物理像素（`dpi`），
/// 超出面积预算才降——降的时候圆角先糊，所以能撑就撑。
///
/// `pad` 是贴图四周的留边（光尘），面积按**带边**的算：这块贴图确实那么大。
fn plate_scale(job: &GlassJob, pad: f32, dpi: f32) -> f32 {
    let dpi = if dpi.is_finite() && dpi > 0.5 {
        dpi.min(PLATE_MAX_SCALE)
    } else {
        1.0
    };
    let want = match job.polarity {
        Polarity::Dark => dpi,
        Polarity::Light => dpi * LIGHT_PLATE_RATIO,
    };
    let (w, h) = (job.w + pad * 2.0, job.h + pad * 2.0);
    let area = (w * h * want * want).max(1.0);
    let budget = if pad > 0.0 {
        DUST_PLATE_BUDGET
    } else {
        PLATE_BUDGET
    };
    if area <= budget {
        want
    } else {
        (budget / area).sqrt() * want
    }
}

/// 一块面在贴图里占多少像素（含光尘留边）。
fn plate_pixels(job: &GlassJob, pad: f32, scale: f32) -> (usize, usize) {
    let w = (job.w + pad * 2.0) * scale;
    let h = (job.h + pad * 2.0) * scale;
    (
        w.round().max(1.0) as usize,
        h.round().max(1.0) as usize,
    )
}

fn coverage_one(job: &GlassJob, win_w: f32, win_h: f32) -> (f32, f32, f32, f32) {
    let x0 = (job.x - COVERAGE_MARGIN).clamp(0.0, win_w);
    let y0 = (job.y - COVERAGE_MARGIN).clamp(0.0, win_h);
    let x1 = (job.x + job.w + COVERAGE_MARGIN).clamp(0.0, win_w);
    let y1 = (job.y + job.h + COVERAGE_MARGIN).clamp(0.0, win_h);
    (x0, y0, (x1 - x0).max(1.0), (y1 - y0).max(1.0))
}

/// 复用缓冲的烘焙器。`ingest` 收一帧，`sync` 按窗口和玻璃面出图。
///
/// 底图（两档模糊）只跟帧和窗口尺寸走；开合一块面板不会重模糊整窗。
#[derive(Default)]
pub struct GlassBaker {
    source: Vec<u8>,
    source_w: usize,
    source_h: usize,
    generation: u64,
    soft: Vec<u8>,
    soft_w: usize,
    soft_h: usize,
    rim: Vec<u8>,
    rim_w: usize,
    rim_h: usize,
    /// 覆盖区的锐图，模糊之前留一份给体部降采样。
    sharp: Vec<u8>,
    scratch: Vec<u8>,
    /// 两档模糊缓冲覆盖的窗口矩形（不是整窗）。
    space: Space,
    plates: Vec<Plate>,
    key: Option<BakeKey>,
    last_bake: Option<Instant>,
    /// 上次 `sync` 里真正重算像素花的时间，给诊断用。
    last_ms: f32,
    /// 光尘期间缓存的「玻璃 pass」结果（带留边的整块贴图）。见 [`DUST_BASE_REFRESH`]。
    dust_base: Option<DustBase>,
    /// 这一次退场撒的点。相位每帧变、点不变，所以按 run 缓存。
    dust_field: Option<DustField>,
}

/// 缓存下来的玻璃 pass。`key` 里不含帧号：画面在动也要忍住，只按
/// [`DUST_BASE_REFRESH`] 的节奏重烤。
struct DustBase {
    key: BaseKey,
    /// 画这张图时收到的帧号（用来判断画面上有没有新东西）
    gen: u64,
    at: Instant,
    bgra: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BaseKey {
    id: u8,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    radius_q: u16,
    scale_q: u16,
    hue: u16,
    dpi_q: u16,
}

/// 一次退场（或回场）撒下的点。
struct DustField {
    run: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    radius_q: u16,
    motes: Vec<Mote>,
}

impl GlassBaker {
    pub fn has_source(&self) -> bool {
        self.source_w > 0 && self.source_h > 0
    }

    #[cfg(test)]
    pub fn last_ms(&self) -> f32 {
        self.last_ms
    }

    /// 收下一帧。长边超过 [`SOURCE_MAX_LONG`] 时做面积平均，避免 4K 帧每帧都进模糊。
    pub fn ingest(&mut self, width: usize, height: usize, bgra: &[u8]) {
        if width == 0 || height == 0 || bgra.len() < width * height * 4 {
            return;
        }
        let (dw, dh) = fit_long(width, height, SOURCE_MAX_LONG);
        if dw == width && dh == height {
            self.source.clear();
            self.source.extend_from_slice(&bgra[..width * height * 4]);
        } else {
            downsample_area(bgra, width, height, dw, dh, &mut self.source);
        }
        self.source_w = dw;
        self.source_h = dh;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn clear(&mut self) {
        self.source.clear();
        self.source_w = 0;
        self.source_h = 0;
        self.generation = self.generation.wrapping_add(1);
        self.plates.clear();
        self.key = None;
        self.dust_base = None;
        self.dust_field = None;
    }

    /// 按当前帧和这组玻璃面出图。什么都没变就返回 `false`（贴图不用重传）。
    ///
    /// `dpi` 是窗口的缩放比（每逻辑像素多少物理像素）：贴图按它烘焙才能和屏幕
    /// 1:1，圆角和 14px 折射带才不糊。
    pub fn sync(
        &mut self,
        win_w: f32,
        win_h: f32,
        jobs: &[GlassJob],
        hue: Option<f32>,
        dpi: f32,
    ) -> bool {
        if !self.has_source() || win_w < 8.0 || win_h < 8.0 {
            let had = !self.plates.is_empty();
            self.plates.clear();
            self.key = None;
            return had;
        }
        let key = bake_key(self.generation, win_w, win_h, hue, dpi, jobs);
        if self.key == Some(key) {
            return false;
        }
        // 只有「又来了一帧、布局没变」才节流。开合面板必须这一帧就跟上。
        // 光尘在跑时**不节流**：那 20ms 是防「整帧烘焙拖垮 UI」，而这段期间玻璃
        // 走缓存（见 `DUST_BASE_REFRESH`），逐帧的只有粒子 pass。
        if let Some(prev) = self.key {
            let layout_same = prev.win_w == key.win_w
                && prev.win_h == key.win_h
                && prev.hue == key.hue
                && prev.dpi_q == key.dpi_q
                && prev.jobs == key.jobs;
            let dusting = job_keys_have_dust(&key);
            if layout_same
                && !dusting
                && self
                    .last_bake
                    .is_some_and(|at| at.elapsed() < BAKE_MIN_INTERVAL)
            {
                return false;
            }
        }
        let started = Instant::now();
        let tint = hue.map(tint_rgb).unwrap_or([0.0, 0.0, 0.0]);
        let has_tint = hue.is_some();
        // 每块面单独模糊自己背后的那一截。三块面的并集几乎是整窗，合在一起糊
        // 会把没被玻璃挡住的画面也算进去。
        let mut next: Vec<Plate> = Vec::with_capacity(jobs.len());
        for job in jobs {
            if job.w < 8.0 || job.h < 8.0 || job.id > 2 {
                continue;
            }
            let pad = job.pad();
            let (x, y, w, h) = coverage_one(job, win_w, win_h);
            let (rim_max, soft_max) = backdrop_limits(job.polarity);
            let scale = plate_scale(job, pad, dpi);
            let mut plate = self.take_plate(job.id);
            match job.dust {
                // 光尘：玻璃 pass 走缓存，只有「过期了」才重烤；每帧拷贝 + 盖粒子。
                Some(phase) => {
                    self.ensure_dust_field(job, phase.run);
                    let base_key = base_key(job, scale, hue, dpi);
                    let fresh = self.dust_base.as_ref().is_some_and(|base| {
                        base.key == base_key
                            && (base.gen == self.generation
                                || base.at.elapsed() < DUST_BASE_REFRESH)
                    });
                    if !fresh {
                        self.rebuild_backdrop(
                            win_w,
                            win_h,
                            Space { x, y, w, h },
                            rim_max,
                            soft_max,
                        );
                        render_plate(
                            &self.soft,
                            self.soft_w,
                            self.soft_h,
                            &self.rim,
                            self.rim_w,
                            self.rim_h,
                            self.space,
                            job,
                            tint,
                            has_tint,
                            Tier::Edge,
                            scale,
                            dpi,
                            pad,
                            &mut plate.bgra,
                        );
                        self.dust_base = Some(DustBase {
                            key: base_key,
                            gen: self.generation,
                            at: Instant::now(),
                            bgra: plate.bgra.clone(),
                        });
                    } else if let Some(base) = self.dust_base.as_ref() {
                        plate.bgra.clear();
                        plate.bgra.extend_from_slice(&base.bgra);
                    }
                    let (pw, ph) = plate_pixels(job, pad, scale);
                    fade_alpha(&mut plate.bgra, phase.fade);
                    if let Some(field) = self.dust_field.as_ref() {
                        stamp_dust(&mut plate.bgra, pw, ph, scale, phase, &field.motes);
                    }
                    plate.w = pw;
                    plate.h = ph;
                }
                None => {
                    self.rebuild_backdrop(win_w, win_h, Space { x, y, w, h }, rim_max, soft_max);
                    render_plate(
                        &self.soft,
                        self.soft_w,
                        self.soft_h,
                        &self.rim,
                        self.rim_w,
                        self.rim_h,
                        self.space,
                        job,
                        tint,
                        has_tint,
                        Tier::Edge,
                        scale,
                        dpi,
                        0.0,
                        &mut plate.bgra,
                    );
                    let (pw, ph) = plate_pixels(job, 0.0, scale);
                    plate.w = pw;
                    plate.h = ph;
                }
            }
            plate.dirty = true;
            next.push(plate);
        }
        if !job_keys_have_dust(&key) {
            // 光尘散了就把缓存还回去：下一次退场可能是别的位置、别的 DPI。
            self.dust_base = None;
            self.dust_field = None;
        }
        self.plates = next;
        self.key = Some(key);
        self.last_bake = Some(started);
        self.last_ms = started.elapsed().as_secs_f32() * 1000.0;
        true
    }

    /// 取（必要时重撒）这一次退场的点集。
    fn ensure_dust_field(&mut self, job: &GlassJob, run: u32) {
        let want = DustField {
            run,
            x: job.x.round() as i32,
            y: job.y.round() as i32,
            w: job.w.round() as i32,
            h: job.h.round() as i32,
            radius_q: (job.radius * 16.0).round().min(65535.0) as u16,
            motes: Vec::new(),
        };
        if let Some(field) = self.dust_field.as_ref() {
            if field.run == want.run
                && field.x == want.x
                && field.y == want.y
                && field.w == want.w
                && field.h == want.h
                && field.radius_q == want.radius_q
            {
                return;
            }
        }
        let motes = seed_motes(job.w, job.h, job.radius, run as u64);
        self.dust_field = Some(DustField { motes, ..want });
    }

    pub fn plates(&self) -> impl Iterator<Item = (u8, usize, usize, &[u8], bool)> {
        self.plates
            .iter()
            .map(|p| (p.id, p.w, p.h, p.bgra.as_slice(), p.dirty))
    }

    pub fn clear_dirty(&mut self) {
        for plate in &mut self.plates {
            plate.dirty = false;
        }
    }

    fn take_plate(&mut self, id: u8) -> Plate {
        if let Some(pos) = self.plates.iter().position(|p| p.id == id) {
            return self.plates.swap_remove(pos);
        }
        Plate {
            id,
            w: 0,
            h: 0,
            bgra: Vec::new(),
            dirty: true,
        }
    }

    fn rebuild_backdrop(
        &mut self,
        win_w: f32,
        win_h: f32,
        space: Space,
        rim_max: f32,
        soft_max: f32,
    ) {
        self.space = space;
        let (rw, rh) = level_size(space.w, space.h, rim_max);
        let (sw, sh) = level_size(space.w, space.h, soft_max);
        // 只对位采样一次。体部从这张锐图降采样，边圈直接在它上面模糊。
        compose_region(
            &self.source,
            self.source_w,
            self.source_h,
            win_w,
            win_h,
            space,
            rw,
            rh,
            &mut self.sharp,
        );
        downsample_area(&self.sharp, rw, rh, sw, sh, &mut self.soft);
        let soft_sigma = BLUR_BODY * (sw as f32 / space.w.max(1.0));
        blur_gauss(&mut self.soft, sw, sh, soft_sigma, &mut self.scratch);
        self.rim.clone_from(&self.sharp);
        let rim_sigma = BLUR_RIM * (rw as f32 / space.w.max(1.0));
        blur_gauss(&mut self.rim, rw, rh, rim_sigma, &mut self.scratch);
        self.rim_w = rw;
        self.rim_h = rh;
        self.soft_w = sw;
        self.soft_h = sh;
    }
}

fn bake_key(
    gen: u64,
    win_w: f32,
    win_h: f32,
    hue: Option<f32>,
    dpi: f32,
    jobs: &[GlassJob],
) -> BakeKey {
    let mut slots = [None; 3];
    for job in jobs {
        if job.id > 2 || job.w < 8.0 || job.h < 8.0 {
            continue;
        }
        slots[job.id as usize] = Some(JobKey {
            x: job.x.round() as i32,
            y: job.y.round() as i32,
            w: job.w.round() as i32,
            h: job.h.round() as i32,
            // 相位不量化进 key 的话，几何/帧/色相都没变 → 判「没变」→ 粒子静止。
            dust: job.dust.map(|phase| DustKey {
                u_q: (phase.u.clamp(0.0, 1.0) * 65534.0).round() as u16,
                fade_q: (phase.fade.clamp(0.0, 1.0) * 255.0).round() as u8,
                scatter: phase.scatter,
                run: phase.run,
            }),
        });
    }
    let hue = match hue {
        Some(h) => (h.round() as i32).rem_euclid(360) as u16,
        None => 0xFFFF,
    };
    let dpi_q = if dpi.is_finite() && dpi > 0.0 {
        (dpi * 64.0).round().min(65535.0) as u16
    } else {
        0
    };
    BakeKey {
        gen,
        win_w: win_w.round().max(0.0) as u32,
        win_h: win_h.round().max(0.0) as u32,
        hue,
        dpi_q,
        jobs: slots,
    }
}

fn job_keys_have_dust(key: &BakeKey) -> bool {
    key.jobs.iter().flatten().any(|job| job.dust.is_some())
}

/// 玻璃 pass 缓存的指纹。**不含帧号**——画面在动也要忍住。
fn base_key(job: &GlassJob, scale: f32, hue: Option<f32>, dpi: f32) -> BaseKey {
    let hue = match hue {
        Some(h) => (h.round() as i32).rem_euclid(360) as u16,
        None => 0xFFFF,
    };
    BaseKey {
        id: job.id,
        x: job.x.round() as i32,
        y: job.y.round() as i32,
        w: job.w.round() as i32,
        h: job.h.round() as i32,
        radius_q: (job.radius * 16.0).round().min(65535.0) as u16,
        scale_q: (scale * 256.0).round().min(65535.0) as u16,
        hue,
        dpi_q: if dpi.is_finite() && dpi > 0.0 {
            (dpi * 64.0).round().min(65535.0) as u16
        } else {
            0
        },
    }
}

fn fit_long(width: usize, height: usize, max_long: f32) -> (usize, usize) {
    let long = width.max(height) as f32;
    if long <= max_long || long < 1.0 {
        return (width.max(1), height.max(1));
    }
    let scale = max_long / long;
    (
        (width as f32 * scale).round().max(1.0) as usize,
        (height as f32 * scale).round().max(1.0) as usize,
    )
}

fn level_size(win_w: f32, win_h: f32, max_long: f32) -> (usize, usize) {
    fit_long(
        win_w.round().max(1.0) as usize,
        win_h.round().max(1.0) as usize,
        max_long,
    )
}

fn downsample_area(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize, dst: &mut Vec<u8>) {
    dst.resize(dw * dh * 4, 0);
    for y in 0..dh {
        let y0 = y * sh / dh;
        let y1 = ((y + 1) * sh / dh).max(y0 + 1).min(sh);
        for x in 0..dw {
            let x0 = x * sw / dw;
            let x1 = ((x + 1) * sw / dw).max(x0 + 1).min(sw);
            let (mut b, mut g, mut r, mut n) = (0u32, 0u32, 0u32, 0u32);
            for sy in y0..y1 {
                let row = sy * sw;
                for sx in x0..x1 {
                    let i = (row + sx) * 4;
                    b += src[i] as u32;
                    g += src[i + 1] as u32;
                    r += src[i + 2] as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            let o = (y * dw + x) * 4;
            dst[o] = (b / n) as u8;
            dst[o + 1] = (g / n) as u8;
            dst[o + 2] = (r / n) as u8;
            dst[o + 3] = 255;
        }
    }
}

/// 把源画面按 contain 铺进窗口，只把 `region` 那一块写进 `out`。留边是画布色。
fn compose_region(
    src: &[u8],
    sw: usize,
    sh: usize,
    win_w: f32,
    win_h: f32,
    region: Space,
    ow: usize,
    oh: usize,
    out: &mut Vec<u8>,
) {
    out.resize(ow * oh * 4, 0);
    if sw == 0 || sh == 0 || win_w < 1.0 || win_h < 1.0 || region.w < 1.0 || region.h < 1.0 {
        return;
    }
    let fit = (win_w / sw as f32).min(win_h / sh as f32);
    let dw = sw as f32 * fit;
    let dh = sh as f32 * fit;
    let ox = (win_w - dw) * 0.5;
    let oy = (win_h - dh) * 0.5;
    for y in 0..oh {
        let wy = region.y + (y as f32 + 0.5) / oh as f32 * region.h;
        for x in 0..ow {
            let wx = region.x + (x as f32 + 0.5) / ow as f32 * region.w;
            let o = (y * ow + x) * 4;
            if wx < ox || wy < oy || wx >= ox + dw || wy >= oy + dh {
                out[o] = CANVAS_BGRA[0];
                out[o + 1] = CANVAS_BGRA[1];
                out[o + 2] = CANVAS_BGRA[2];
                out[o + 3] = 255;
                continue;
            }
            let u = (wx - ox) / dw * sw as f32;
            let v = (wy - oy) / dh * sh as f32;
            let [r, g, b] = sample(src, sw, sh, u, v);
            out[o] = b as u8;
            out[o + 1] = g as u8;
            out[o + 2] = r as u8;
            out[o + 3] = 255;
        }
    }
}

/// 三次盒式模糊逼近 σ，然后乘 [`BRIGHTNESS`]。半径再大也是每像素常数时间。
fn blur_gauss(pixels: &mut [u8], w: usize, h: usize, sigma: f32, scratch: &mut Vec<u8>) {
    if w == 0 || h == 0 {
        return;
    }
    scratch.resize(w * h * 4, 0);
    for width in boxes_for_gauss(sigma, 3) {
        let radius = ((width.max(1) - 1) / 2) as usize;
        if radius == 0 {
            continue;
        }
        box_h(pixels, scratch, w, h, radius);
        box_v(scratch, pixels, w, h, radius);
    }
    for px in pixels.chunks_exact_mut(4) {
        for c in 0..3 {
            px[c] = (px[c] as f32 * BRIGHTNESS).round().clamp(0.0, 255.0) as u8;
        }
        px[3] = 255;
    }
}

/// Ivan Kutskir 的三次盒宽度（奇数）。`sigma < 0.5` 时不模糊。
fn boxes_for_gauss(sigma: f32, n: i32) -> Vec<i32> {
    if !(sigma >= 0.5) || n <= 0 {
        return vec![1; n.max(0) as usize];
    }
    let n_f = n as f32;
    let w_ideal = (12.0 * sigma * sigma / n_f + 1.0).sqrt();
    let mut wl = w_ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    if wl < 1 {
        wl = 1;
    }
    let wu = wl + 2;
    let m_ideal =
        (12.0 * sigma * sigma - n_f * (wl * wl) as f32 - 4.0 * n_f * wl as f32 - 3.0 * n_f)
            / (-4.0 * wl as f32 - 4.0);
    let m = m_ideal.round() as i32;
    (0..n).map(|i| if i < m { wl } else { wu }).collect()
}

fn box_h(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let diam = (r * 2 + 1) as u32;
    for y in 0..h {
        let row = y * w;
        let mut sum = [0u32; 3];
        for i in 0..=r * 2 {
            let x = (i as isize - r as isize).clamp(0, w as isize - 1) as usize;
            let p = (row + x) * 4;
            sum[0] += src[p] as u32;
            sum[1] += src[p + 1] as u32;
            sum[2] += src[p + 2] as u32;
        }
        for x in 0..w {
            let o = (row + x) * 4;
            dst[o] = (sum[0] / diam) as u8;
            dst[o + 1] = (sum[1] / diam) as u8;
            dst[o + 2] = (sum[2] / diam) as u8;
            dst[o + 3] = 255;
            let add = (x + r + 1).min(w - 1);
            let sub = (x as isize - r as isize).max(0) as usize;
            let pa = (row + add) * 4;
            let ps = (row + sub) * 4;
            for c in 0..3 {
                sum[c] = sum[c] + src[pa + c] as u32 - src[ps + c] as u32;
            }
        }
    }
}

fn box_v(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let diam = (r * 2 + 1) as u32;
    for x in 0..w {
        let mut sum = [0u32; 3];
        for i in 0..=r * 2 {
            let y = (i as isize - r as isize).clamp(0, h as isize - 1) as usize;
            let p = (y * w + x) * 4;
            sum[0] += src[p] as u32;
            sum[1] += src[p + 1] as u32;
            sum[2] += src[p + 2] as u32;
        }
        for y in 0..h {
            let o = (y * w + x) * 4;
            dst[o] = (sum[0] / diam) as u8;
            dst[o + 1] = (sum[1] / diam) as u8;
            dst[o + 2] = (sum[2] / diam) as u8;
            dst[o + 3] = 255;
            let add = (y + r + 1).min(h - 1);
            let sub = (y as isize - r as isize).max(0) as usize;
            let pa = (add * w + x) * 4;
            let ps = (sub * w + x) * 4;
            for c in 0..3 {
                sum[c] = sum[c] + src[pa + c] as u32 - src[ps + c] as u32;
            }
        }
    }
}

/// 双线性采样。坐标是像素索引（0 = 第一个像素的左/上沿），返回 RGB 0..255。
#[inline(always)]
fn sample(data: &[u8], w: usize, h: usize, x: f32, y: f32) -> [f32; 3] {
    if w == 0 || h == 0 {
        return [0.0; 3];
    }
    let x = x.clamp(0.0, w as f32 - 1.001);
    let y = y.clamp(0.0, h as f32 - 1.001);
    let x0 = x as usize;
    let y0 = y as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let w00 = (1.0 - fx) * (1.0 - fy);
    let w10 = fx * (1.0 - fy);
    let w01 = (1.0 - fx) * fy;
    let w11 = fx * fy;
    let i00 = (y0 * w + x0) * 4;
    let i10 = (y0 * w + x1) * 4;
    let i01 = (y1 * w + x0) * 4;
    let i11 = (y1 * w + x1) * 4;
    // 内存是 BGRA，返回值用 RGB。三个通道写开，别指望迭代器在热路径里展开。
    [
        data[i00 + 2] as f32 * w00
            + data[i10 + 2] as f32 * w10
            + data[i01 + 2] as f32 * w01
            + data[i11 + 2] as f32 * w11,
        data[i00 + 1] as f32 * w00
            + data[i10 + 1] as f32 * w10
            + data[i01 + 1] as f32 * w01
            + data[i11 + 1] as f32 * w11,
        data[i00] as f32 * w00
            + data[i10] as f32 * w10
            + data[i01] as f32 * w01
            + data[i11] as f32 * w11,
    ]
}

fn map_xy(buf_w: usize, buf_h: usize, space: Space, wx: f32, wy: f32) -> (f32, f32) {
    let w = space.w.max(1.0);
    let h = space.h.max(1.0);
    (
        (wx - space.x) / w * buf_w as f32,
        (wy - space.y) / h * buf_h as f32,
    )
}

/// HSL（色相 0..360，饱和度/明度 0..1）→ RGB 0..255。和 HTML 原型的 `hue2rgb` 同一套。
pub fn hsl_rgb(h_deg: f32, s: f32, l: f32) -> [f32; 3] {
    let h = (h_deg / 360.0).rem_euclid(1.0);
    if s <= f32::EPSILON {
        let v = l * 255.0;
        return [v, v, v];
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue2 = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [
        hue2(h + 1.0 / 3.0) * 255.0,
        hue2(h) * 255.0,
        hue2(h - 1.0 / 3.0) * 255.0,
    ]
}

/// 设计稿 L2：只留色相，饱和度 46%、明度 36%。
fn tint_rgb(hue: f32) -> [f32; 3] {
    hsl_rgb(hue, 0.46, 0.36)
}

fn render_plate(
    soft: &[u8],
    soft_w: usize,
    soft_h: usize,
    rim: &[u8],
    rim_w: usize,
    rim_h: usize,
    space: Space,
    job: &GlassJob,
    tint: [f32; 3],
    has_tint: bool,
    tier: Tier,
    scale: f32,
    dpi: f32,
    // 贴图四周的留边（逻辑像素）。光尘要飞出去，玻璃本体仍然从 `pad` 处开始。
    pad: f32,
    out: &mut Vec<u8>,
) {
    let scale = if scale.is_finite() && scale > 0.05 {
        scale
    } else {
        1.0
    };
    let (pw, ph) = plate_pixels(job, pad, scale);
    // 先清干净：`resize` 不清旧内容（同一块 `Vec` 会被复用），而留边必须是透明的。
    out.clear();
    out.resize(pw * ph * 4, 0);
    if soft_w == 0 || rim_w == 0 || space.w < 1.0 || space.h < 1.0 {
        return;
    }
    // 胶囊本体在贴图里的位置与像素尺寸（留边之内）
    let ox0 = (pad * scale).round() as usize;
    let oy0 = (pad * scale).round() as usize;
    let pill_w = (job.w * scale).round().max(1.0) as usize;
    let pill_h = (job.h * scale).round().max(1.0) as usize;

    let (_mean_l, scrim, veil) = region_tone(soft, soft_w, soft_h, space, job);
    let light = job.polarity == Polarity::Light;
    let full = tier == Tier::Full;
    let hw = job.w * 0.5;
    let hh = job.h * 0.5;
    let cx = job.x + hw;
    let cy = job.y + hh;
    let radius = job.radius;
    let band = job.band.max(0.001);
    let maxd = job.maxd;
    // 圆角覆盖率：过渡带正好 1 个**物理**像素（= `1/dpi` 逻辑像素）。
    //
    // 贴图和屏幕 1:1 时就是 1 个贴图像素，是矢量级的干净边缘；分辨率被预算
    // 压下来时过渡带在贴图里更宽，双线性拉上去仍是同样的柔软度。之前用的是
    // 二值裁切（`d > 0` 清黑），半分辨率放大后就变成 2~3 物理像素的灰黑毛边
    // ——圆角"烧焦"就是这么来的。
    let aa = if dpi.is_finite() && dpi > 0.01 { dpi } else { 1.0 };

    // ---- 行/列预计算 ----
    //
    // 贴图现在是屏幕 1:1 的（一块胶囊 1140×138），逐像素的除法和 abs 撑不住。
    // SDF 的 `qx` 只跟 x 走、`qy` 只跟 y 走，`map_xy` 的除数也是常量 —— 全部
    // 提到循环外，内层只剩一次 sqrt、一次除法（求法线）和两次双线性采样。
    let dx = job.w / pill_w as f32;
    let dy = job.h / pill_h as f32;
    let k_soft_x = soft_w as f32 / space.w;
    let k_soft_y = soft_h as f32 / space.h;
    let k_rim_x = rim_w as f32 / space.w;
    let k_rim_y = rim_h as f32 / space.h;
    let half_w = (hw - radius).max(0.0);
    let half_h = (hh - radius).max(0.0);

    let mut col_qx = vec![0.0f32; pill_w];
    let mut col_bx = vec![0.0f32; pill_w];
    let mut col_sgx = vec![1.0f32; pill_w];
    let mut col_sx = vec![0.0f32; pill_w];
    let mut col_rx = vec![0.0f32; pill_w];
    for ox in 0..pill_w {
        let wx = job.x + (ox as f32 + 0.5) * dx;
        let rel = wx - cx;
        let qx = rel.abs() - half_w;
        col_qx[ox] = qx;
        col_bx[ox] = qx.max(0.0);
        col_sgx[ox] = if rel > 0.0 { 1.0 } else { -1.0 };
        col_sx[ox] = (wx - space.x) * k_soft_x;
        col_rx[ox] = (wx - space.x) * k_rim_x;
    }

    for oy in 0..pill_h {
        let wy = job.y + (oy as f32 + 0.5) * dy;
        let ty = (oy as f32 + 0.5) / pill_h as f32;
        let rel_y = wy - cy;
        let qy = rel_y.abs() - half_h;
        let by = qy.max(0.0);
        let sgy = if rel_y > 0.0 { 1.0 } else { -1.0 };
        let sy = (wy - space.y) * k_soft_y;
        let ry_base = (wy - space.y) * k_rim_y;
        // 暗底的两层压暗（0.45 平铺 + 渐变）合成一次仿射，逐行算一次。
        let flat_a = scrim * 0.45;
        let (gr, gg, gb, ga) = scrim_gradient(ty, tint, has_tint, scrim);
        let keep = (1.0 - flat_a) * (1.0 - ga);
        let add_r = SCRIM_RGB[0] * flat_a * (1.0 - ga) + gr * ga;
        let add_g = SCRIM_RGB[1] * flat_a * (1.0 - ga) + gg * ga;
        let add_b = SCRIM_RGB[2] * flat_a * (1.0 - ga) + gb * ga;

        for ox in 0..pill_w {
            let qx = col_qx[ox];
            let bx = col_bx[ox];
            let len = (bx * bx + by * by).sqrt();
            let d_raw = len + qx.max(qy).min(0.0) - radius;
            // 留边把整块贴图撑大了一圈，胶囊本体从 (ox0, oy0) 开始写。
            let o = ((oy + oy0) * pw + (ox + ox0)) * 4;
            let cover = (0.5 - d_raw * aa).clamp(0.0, 1.0);
            if cover <= 0.002 {
                out[o..o + 4].copy_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            // 颜色照旧按 `d = 0`（贴边）算：圆角外那半个像素仍然取边缘色，
            // 免得插值时把黑色拖进来。alpha 才是裁切。
            let t = ((-d_raw).max(0.0) / band).clamp(0.0, 1.0);
            let u = 1.0 - t;
            let u2 = u * u;
            let m = maxd * u2;

            let mut rgb = sample(soft, soft_w, soft_h, col_sx[ox], sy);
            let inv = if len > 1e-6 { 1.0 / len } else { 0.0 };
            let ny = sgy * by * inv;
            if u2 > 0.001 {
                let rx = col_rx[ox] + col_sgx[ox] * bx * inv * m * k_rim_x;
                let ry = ry_base + ny * m * k_rim_y;
                let rim_rgb = sample(rim, rim_w, rim_h, rx, ry);
                for c in 0..3 {
                    rgb[c] += (rim_rgb[c] - rgb[c]) * u2;
                }
                if full {
                    const DISPERSION: f32 = 0.07;
                    let far = sample(
                        rim,
                        rim_w,
                        rim_h,
                        col_rx[ox] + col_sgx[ox] * bx * inv * m * (1.0 + DISPERSION) * k_rim_x,
                        ry_base + ny * m * (1.0 + DISPERSION) * k_rim_y,
                    );
                    rgb[0] += (far[0] - rgb[0]) * u2;
                    let near = sample(
                        rim,
                        rim_w,
                        rim_h,
                        col_rx[ox] + col_sgx[ox] * bx * inv * m * (1.0 - DISPERSION) * k_rim_x,
                        ry_base + ny * m * (1.0 - DISPERSION) * k_rim_y,
                    );
                    rgb[2] += (near[2] - rgb[2]) * u2;
                }
            }

            if light {
                let v = veil - (veil - LIGHT_VEIL_RIM) * u2;
                let iv = 1.0 - v;
                rgb[0] = rgb[0] * iv + MILK[0] * v;
                rgb[1] = rgb[1] * iv + MILK[1] * v;
                rgb[2] = rgb[2] * iv + MILK[2] * v;
            } else {
                rgb[0] = rgb[0] * keep + add_r;
                rgb[1] = rgb[1] * keep + add_g;
                rgb[2] = rgb[2] * keep + add_b;
            }

            if full && u2 > 0.001 {
                const FRESNEL_SPAN: f32 = 0.45;
                const FRESNEL: f32 = 0.48;
                const FRESNEL_LIGHT: f32 = 0.22;
                const INNER_SHADE: f32 = 0.24;
                const INNER_SHADE_LIGHT: f32 = 0.10;
                let k = (1.0 - t / FRESNEL_SPAN).max(0.0);
                let hl = k
                    * k
                    * (if light { FRESNEL_LIGHT } else { FRESNEL })
                    * (0.30 + 0.70 * (-ny).max(0.0));
                for c in 0..3 {
                    rgb[c] += (255.0 - rgb[c]) * hl;
                }
                let shade = if light {
                    INNER_SHADE_LIGHT
                } else {
                    INNER_SHADE
                };
                let sh = 1.0 - (1.0 - t / 0.60).max(0.0).powi(3) * shade;
                for c in 0..3 {
                    rgb[c] *= sh;
                }
            }

            out[o] = rgb[2].round().clamp(0.0, 255.0) as u8;
            out[o + 1] = rgb[1].round().clamp(0.0, 255.0) as u8;
            out[o + 2] = rgb[0].round().clamp(0.0, 255.0) as u8;
            out[o + 3] = (cover * 255.0).round() as u8;
        }
    }
}

// ---------------- 退场光尘 ----------------

/// 一个光点。
#[derive(Clone, Copy)]
struct Mote {
    /// 起点（贴图局部坐标，含留边）
    x: f32,
    y: f32,
    /// 外法线（圆角矩形 SDF 的梯度，和折射层同一个场）
    nx: f32,
    ny: f32,
    /// 沿法线走多远
    dist: f32,
    /// 视觉半径（逻辑 px）
    r: f32,
    /// 峰值 alpha
    a: f32,
    /// 起步延迟（占整段的比例）：贴边先松、内侧后走
    delay: f32,
    /// 自己走完全程占整段的比例
    span: f32,
    /// 切向摆的相位
    ph: f32,
    /// 切向摆的幅度
    wob: f32,
}

/// 胶囊本体的淡出/淡入乘到贴图 alpha 上。
///
/// 这一步是「粒子接管」的前提：贴图挂在控制层的 opacity 下面会跟着胶囊一起消失，
/// 而粒子恰恰要在那之后继续散 0.4 秒。
fn fade_alpha(bgra: &mut [u8], fade: f32) {
    let f = fade.clamp(0.0, 1.0);
    if f >= 0.999 {
        return;
    }
    for px in bgra.chunks_exact_mut(4) {
        px[3] = (px[3] as f32 * f).round().clamp(0.0, 255.0) as u8;
    }
}

/// 把这一帧的光点盖到玻璃贴图上（第二道 pass）。
fn stamp_dust(
    out: &mut [u8],
    pw: usize,
    ph: usize,
    scale: f32,
    phase: DustPhase,
    motes: &[Mote],
) {
    let u = phase.u.clamp(0.0, 1.0);
    for m in motes {
        let pu = ((u - m.delay) / m.span).clamp(0.0, 1.0);
        if pu <= 0.0 || pu >= 1.0 {
            continue;
        }
        // 散：快脱开再减速（气被推开）；聚：距离从远端倒回 0，收尾变慢（被吸住）
        let e = if phase.scatter {
            1.0 - (1.0 - pu).powi(3)
        } else {
            (1.0 - pu).powi(2)
        };
        // 切向摆：让「气」有漂浮感，而不是一条条直线射出去
        let wob = (pu * 5.0 + m.ph).sin() * m.wob * e;
        let x = m.x + m.nx * m.dist * e - m.ny * wob;
        let y = m.y + m.ny * m.dist * e - DUST_RISE * e * e + m.nx * wob;
        let a = if phase.scatter {
            m.a * (1.0 - pu).powf(1.5)
        } else {
            m.a * (pu * std::f32::consts::PI).sin() * 0.9
        };
        if a <= 0.004 {
            continue;
        }
        let rad = m.r
            * if phase.scatter {
                1.0 - 0.3 * pu
            } else {
                0.7 + 0.3 * pu
            };
        // 贴图是软边，视觉核心约等于 s/2.8（和原型同一个口径）
        stamp(out, pw, ph, x * scale, y * scale, rad * 2.8 * scale, a);
    }
}

/// 撒点：只在胶囊的**轮廓带**里取。
///
/// 整块一起碎会像沙漏；真实材质是边先松、中心最后走——中心那块厚度最大，本来就
/// 最后散。所以权重按 `exp(d/(0.42·band))`：贴边 1、带内沿约 0.06。
fn seed_motes(w: f32, h: f32, radius: f32, rng_seed: u64) -> Vec<Mote> {
    let mut rng = Rng(rng_seed | 1);
    let mut out = Vec::with_capacity(DUST_COUNT);
    let mut guard = 0;
    while out.len() < DUST_COUNT && guard < DUST_COUNT * 120 {
        guard += 1;
        let x = DUST_PAD + rng.unit() * w;
        let y = DUST_PAD + rng.unit() * h;
        let d = sdf(x - DUST_PAD, y - DUST_PAD, w, h, radius);
        // 圆角外不要；太深也不要。只靠权重的话，340 颗里总会漏进几颗落在
        // 30px 深的胶囊中心——那不叫「从边沿碎开」。
        if d > 0.0 || d < -(DUST_BAND * 2.0) {
            continue;
        }
        let near = (d / (DUST_BAND * 0.42)).exp();
        if rng.unit() > near {
            continue;
        }
        let (nx, ny) = sdf_normal_local(x - DUST_PAD, y - DUST_PAD, w, h, radius);
        let k = rng.unit();
        let fine = k < 0.6;
        let fluff = k < 0.9;
        out.push(Mote {
            x,
            y,
            nx,
            ny,
            dist: DUST_PUSH * (0.45 + rng.unit() * 0.55) * if fine { 0.68 } else { 1.0 },
            r: if fine {
                0.5 + rng.unit() * 0.45
            } else if fluff {
                0.9 + rng.unit() * 0.7
            } else {
                0.4 + rng.unit() * 0.22
            },
            a: DUST_ALPHA
                * if fluff {
                    0.5 + rng.unit() * 0.45
                } else {
                    1.0
                },
            delay: 0.2 * (1.0 - near) + rng.unit() * 0.12,
            span: 0.62 + rng.unit() * 0.38,
            ph: rng.unit() * std::f32::consts::TAU,
            wob: 0.5 + rng.unit() * 1.6,
        });
    }
    out
}

/// 把一个软光点**加**进 BGRA 缓冲。
///
/// 粒子是光尘，要的是加光而不是覆盖：覆盖在亮画面上会变成脏点。单张贴图的混合
/// 只有 source-over，所以这里在预乘空间里做加法再除回去——天空（`a=0`）上得到
/// 白色 + 覆盖率，胶囊内部（`a=255`）上得到「原来那色往白推一点」。这比在 GPUI
/// 里给 340 个节点各配一次混合模式便宜得多，也省掉 340 次布局。
fn stamp(buf: &mut [u8], w: usize, h: usize, cx: f32, cy: f32, half: f32, a: f32) {
    if half <= 0.0 || w == 0 || h == 0 {
        return;
    }
    let x0 = (cx - half).floor().max(0.0) as usize;
    let y0 = (cy - half).floor().max(0.0) as usize;
    let x1 = ((cx + half).ceil() as isize).clamp(0, w as isize - 1) as usize;
    let y1 = ((cy + half).ceil() as isize).clamp(0, h as isize - 1) as usize;
    for py in y0..=y1 {
        let dy = (py as f32 + 0.5 - cy) / half;
        for px in x0..=x1 {
            let dx = (px as f32 + 0.5 - cx) / half;
            let d2 = dx * dx + dy * dy;
            if d2 >= 1.0 {
                continue;
            }
            let k = sprite_alpha(d2.sqrt()) * a;
            if k <= 0.002 {
                continue;
            }
            let o = (py * w + px) * 4;
            let a0 = buf[o + 3] as f32 / 255.0;
            let na = a0 + (1.0 - a0) * k;
            if na <= 1e-4 {
                continue;
            }
            for c in 0..3 {
                let v = (buf[o + c] as f32 * a0 + 255.0 * k) / na;
                buf[o + c] = v.round().clamp(0.0, 255.0) as u8;
            }
            buf[o + 3] = (na * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// 光点贴图的 alpha 剖面：原型四个色标 `0→1 / .32→.62 / .66→.16 / 1→0`。
///
/// 硬边（`arc` 填圆）会被读成噪点；而逐帧建渐变又会拖垮帧率，所以截面写死成
/// 分段线性——软、单调、没有一圈凸起的边。
#[inline]
fn sprite_alpha(d: f32) -> f32 {
    if d < 0.32 {
        1.0 - 0.38 * (d / 0.32)
    } else if d < 0.66 {
        0.62 - 0.46 * ((d - 0.32) / 0.34)
    } else {
        0.16 * (1.0 - (d - 0.66) / 0.34)
    }
}

/// 圆角矩形 SDF（`d < 0` 在内部）。
fn sdf(x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let qx = (x - w * 0.5).abs() - (w * 0.5 - r);
    let qy = (y - h * 0.5).abs() - (h * 0.5 - r);
    let ax = qx.max(0.0);
    let ay = qy.max(0.0);
    (ax * ax + ay * ay).sqrt() + qx.max(qy).min(0.0) - r
}

/// SDF 的梯度就是外法线：直边取轴向、圆角取径向。差分而不是解析式——只撒点时算
/// 340 次，不值得为它写四段分支。
fn sdf_normal_local(x: f32, y: f32, w: f32, h: f32, r: f32) -> (f32, f32) {
    const E: f32 = 0.6;
    let gx = sdf(x + E, y, w, h, r) - sdf(x - E, y, w, h, r);
    let gy = sdf(x, y + E, w, h, r) - sdf(x, y - E, w, h, r);
    let l = (gx * gx + gy * gy).sqrt();
    if l < 1e-6 {
        (0.0, -1.0)
    } else {
        (gx / l, gy / l)
    }
}

/// xorshift64*，只用来撒点。不引 `rand`：要的是「看起来随机」，不是统计质量。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 8) as f32 / 16_777_216.0
    }
}

/// 圆角矩形 SDF 和边缘法线。`d < 0` 在内部。直边法线取轴向，圆角取径向。
///
/// 贴着贴图跑的那份把 x/y 分量拆成行/列预计算了（见 [`render_plate`]），
/// 这份留作单点参考答案，测试用它核对拆分没算错。
#[cfg(test)]
fn sdf_normal(
    wx: f32,
    wy: f32,
    cx: f32,
    cy: f32,
    hw: f32,
    hh: f32,
    radius: f32,
) -> (f32, f32, f32, f32) {
    let qx = (wx - cx).abs() - (hw - radius);
    let qy = (wy - cy).abs() - (hh - radius);
    let bx = qx.max(0.0);
    let by = qy.max(0.0);
    let len = (bx * bx + by * by).sqrt();
    let d = len + qx.max(qy).min(0.0) - radius;
    let (nx, ny) = if len > 1e-6 {
        (
            if wx > cx { 1.0 } else { -1.0 } * bx / len,
            if wy > cy { 1.0 } else { -1.0 } * by / len,
        )
    } else {
        (0.0, 0.0)
    };
    (d, nx, ny, len)
}

fn region_tone(
    soft: &[u8],
    soft_w: usize,
    soft_h: usize,
    space: Space,
    job: &GlassJob,
) -> (f32, f32, f32) {
    let mut sum = 0.0;
    let mut cnt = 0.0;
    let mut y = 0.0;
    while y < job.h {
        let mut x = 0.0;
        while x < job.w {
            let (sx, sy) = map_xy(soft_w, soft_h, space, job.x + x, job.y + y);
            let rgb = sample(soft, soft_w, soft_h, sx, sy);
            sum += (0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]) / 255.0;
            cnt += 1.0;
            x += 4.0;
        }
        y += 4.0;
    }
    let mean_l = if cnt > 0.0 { sum / cnt } else { 0.0 };
    let scrim = (0.42 + 0.85 * (mean_l - 0.30).max(0.0)).min(0.72);
    let veil = (LIGHT_VEIL + 0.40 * (0.42 - mean_l).max(0.0)).clamp(0.72, 0.94);
    (mean_l, scrim, veil)
}

fn scrim_gradient(ty: f32, tint: [f32; 3], has_tint: bool, scrim: f32) -> (f32, f32, f32, f32) {
    let a_top = if has_tint { 0.18 } else { 0.0 };
    let a_mid = if has_tint { 0.07 } else { 0.0 };
    if ty <= 0.5 {
        let t = ty / 0.5;
        let a = a_top + (a_mid - a_top) * t;
        (tint[0], tint[1], tint[2], a)
    } else {
        let t = (ty - 0.5) / 0.5;
        let a0 = a_mid;
        let a1 = scrim;
        let pr = tint[0] * a0 + (SCRIM_RGB[0] * a1 - tint[0] * a0) * t;
        let pg = tint[1] * a0 + (SCRIM_RGB[1] * a1 - tint[1] * a0) * t;
        let pb = tint[2] * a0 + (SCRIM_RGB[2] * a1 - tint[2] * a0) * t;
        let a = a0 + (a1 - a0) * t;
        if a < 1e-4 {
            (0.0, 0.0, 0.0, 0.0)
        } else {
            (pr / a, pg / a, pb / a, a)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, rgb: [u8; 3]) -> Vec<u8> {
        let mut buf = vec![0u8; w * h * 4];
        for px in buf.chunks_exact_mut(4) {
            px[0] = rgb[2];
            px[1] = rgb[1];
            px[2] = rgb[0];
            px[3] = 255;
        }
        buf
    }

    fn rgb_at(buf: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
        let i = (y * w + x) * 4;
        [buf[i + 2], buf[i + 1], buf[i]]
    }

    #[test]
    fn straight_edge_normal_points_out_and_center_does_not_move() {
        // 80×40 的圆角矩形，半径 8。左缘中点应该朝左，中心位移为 0（法线长度 0）。
        let (d_edge, nx, ny, _) = sdf_normal(0.5, 20.0, 40.0, 20.0, 40.0, 20.0, 8.0);
        assert!(d_edge < 0.0 && d_edge > -2.0, "贴着左缘内侧，d={d_edge}");
        assert!((nx + 1.0).abs() < 1e-3, "左缘法线应朝左，nx={nx}");
        assert!(ny.abs() < 1e-3, "直边不该有纵向分量，ny={ny}");

        let (d_mid, nx, ny, len) = sdf_normal(40.0, 20.0, 40.0, 20.0, 40.0, 20.0, 8.0);
        assert!(d_mid < -8.0, "中心离边很远，d={d_mid}");
        assert_eq!(len, 0.0);
        assert_eq!((nx, ny), (0.0, 0.0));

        let (d_out, _, _, _) = sdf_normal(-2.0, -2.0, 40.0, 20.0, 40.0, 20.0, 8.0);
        assert!(d_out > 0.0, "圆角外应该是正的，d={d_out}");
    }

    #[test]
    fn edge_samples_rim_and_center_samples_body() {
        // 体部全黑、边圈全白。浅底会把两边都拉向奶白，但边缘仍应该更亮——
        // 它采的是白边圈，中心采的是黑体部。
        let w = 120;
        let h = 80;
        let soft = solid(w, h, [0, 0, 0]);
        let rim = solid(w, h, [255, 255, 255]);
        let job = GlassJob::capsule(20.0, 20.0, 80.0, 40.0);
        let mut out = Vec::new();
        render_plate(
            &soft,
            w,
            h,
            &rim,
            w,
            h,
            Space {
                x: 0.0,
                y: 0.0,
                w: w as f32,
                h: h as f32,
            },
            &job,
            [0.0; 3],
            false,
            Tier::Edge,
            1.0,
            1.0,
            0.0,
            &mut out,
        );
        let center = rgb_at(&out, 80, 40, 20);
        let left = rgb_at(&out, 80, 1, 20);
        let l = |c: [u8; 3]| c[0] as i32 + c[1] as i32 + c[2] as i32;
        assert!(
            l(left) > l(center) + 20,
            "左缘应比中心亮：edge={left:?} center={center:?}"
        );
        // 圆角裁在贴图里（带 1 物理像素的覆盖率），外层 `overflow_hidden`
        // 只是双保险：贴图四角必须已经透明，否则弧上的边缘带会被整块露出。
        assert_eq!(out[3], 0, "圆角外要透明，圆角才是圆的");
        assert_eq!(out[(20 * 80 + 40) * 4 + 3], 255, "中心不透明");
        assert_eq!(out[(20 * 80 + 5) * 4 + 3], 255, "贴着左缘内侧不透明");
    }

    #[test]
    fn outward_normal_pulls_the_outside_color_in() {
        // 边圈：左侧 24px 白，其余黑。体部全黑。
        // 玻璃左缘朝外采样会吃到白，右缘朝外仍是黑，所以左缘比右缘亮。
        let w = 160;
        let h = 80;
        let soft = solid(w, h, [0, 0, 0]);
        let mut rim = solid(w, h, [0, 0, 0]);
        for y in 0..h {
            for x in 0..24 {
                let i = (y * w + x) * 4;
                rim[i] = 255;
                rim[i + 1] = 255;
                rim[i + 2] = 255;
            }
        }
        let job = GlassJob {
            id: 0,
            x: 16.0,
            y: 20.0,
            w: 100.0,
            h: 40.0,
            radius: 10.0,
            band: 14.0,
            maxd: 6.5,
            polarity: Polarity::Light,
            dust: None,
        };
        let mut out = Vec::new();
        render_plate(
            &soft,
            w,
            h,
            &rim,
            w,
            h,
            Space {
                x: 0.0,
                y: 0.0,
                w: w as f32,
                h: h as f32,
            },
            &job,
            [0.0; 3],
            false,
            Tier::Edge,
            1.0,
            1.0,
            0.0,
            &mut out,
        );
        let left = rgb_at(&out, 100, 2, 20);
        let right = rgb_at(&out, 100, 97, 20);
        let l = |c: [u8; 3]| c[0] as i32 + c[1] as i32 + c[2] as i32;
        assert!(
            l(left) > l(right) + 15,
            "左缘应把外侧的白拉进来：L={left:?} R={right:?}"
        );
    }

    #[test]
    fn bright_backdrop_scrim_is_heavier_than_a_dark_one() {
        let job = GlassJob::capsule(0.0, 0.0, 40.0, 40.0);
        let dark = solid(40, 40, [10, 10, 10]);
        let bright = solid(40, 40, [240, 240, 240]);
        let space = Space {
            x: 0.0,
            y: 0.0,
            w: 40.0,
            h: 40.0,
        };
        let (_, scrim_dark, _) = region_tone(&dark, 40, 40, space, &job);
        let (_, scrim_bright, _) = region_tone(&bright, 40, 40, space, &job);
        assert!(scrim_dark < 0.43, "暗底几乎不压，scrim={scrim_dark}");
        assert!(scrim_bright > 0.70, "亮底压到顶，scrim={scrim_bright}");
    }

    #[test]
    fn light_veil_stays_milky_on_a_black_backdrop() {
        let w = 80;
        let h = 60;
        let soft = solid(w, h, [0, 0, 0]);
        let rim = soft.clone();
        let job = GlassJob::playlist(0.0, 0.0, w as f32, h as f32);
        let mut out = Vec::new();
        render_plate(
            &soft,
            w,
            h,
            &rim,
            w,
            h,
            Space {
                x: 0.0,
                y: 0.0,
                w: w as f32,
                h: h as f32,
            },
            &job,
            [0.0; 3],
            false,
            Tier::Edge,
            1.0,
            1.0,
            0.0,
            &mut out,
        );
        let center = rgb_at(&out, w, w / 2, h / 2);
        // 黑底上奶白不透明度顶到 0.94：0.94*239 ≈ 225，再加 1.04 的亮度是在模糊阶段，
        // 这里体部缓冲是测试直接填的，没有亮度增益。
        for c in center {
            assert!(
                c > 200 && c < 245,
                "浅底中心应是奶白而不是透出的黑，{center:?}"
            );
        }
    }

    #[test]
    fn blur_spreads_a_point_and_keeps_the_peak() {
        let w = 48;
        let h = 48;
        let mut buf = solid(w, h, [0, 0, 0]);
        let mid = (h / 2 * w + w / 2) * 4;
        buf[mid] = 255;
        buf[mid + 1] = 255;
        buf[mid + 2] = 255;
        let mut scratch = Vec::new();
        blur_gauss(&mut buf, w, h, 2.0, &mut scratch);
        let peak = rgb_at(&buf, w, w / 2, h / 2);
        let neighbor = rgb_at(&buf, w, w / 2 + 2, h / 2);
        let far = rgb_at(&buf, w, 1, 1);
        assert!(peak[0] > neighbor[0], "峰应留在原点");
        assert!(neighbor[0] > far[0] + 2, "邻近像素应被晕开");
    }

    #[test]
    fn unchanged_inputs_do_not_rebake() {
        let frame = solid(320, 180, [40, 80, 120]);
        let mut baker = GlassBaker::default();
        baker.ingest(320, 180, &frame);
        let jobs = [GlassJob::capsule(40.0, 280.0, 240.0, 60.0)];
        assert!(baker.sync(320.0, 360.0, &jobs, Some(200.0), 1.0));
        let first = baker.last_ms();
        assert!(first > 0.0);
        assert!(!baker.sync(320.0, 360.0, &jobs, Some(200.0), 1.0));
        baker.ingest(320, 180, &frame);
        // 「只有新帧」的重烤有 20ms 节流，布局变化不节流，所以这里要等一下。
        std::thread::sleep(BAKE_MIN_INTERVAL + Duration::from_millis(5));
        assert!(baker.sync(320.0, 360.0, &jobs, Some(200.0), 1.0));
    }

    #[test]
    fn capsule_on_a_720p_frame_stays_under_a_frame() {
        // 1280×720 的帧铺进 1280×800，只烤胶囊。这是播放中最常见的一帧。
        let w = 1280;
        let h = 720;
        let mut frame = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                frame[i] = (x % 64) as u8;
                frame[i + 1] = (y % 64) as u8;
                frame[i + 2] = 160;
                frame[i + 3] = 255;
            }
        }
        let mut baker = GlassBaker::default();
        baker.ingest(w, h, &frame);
        let jobs = [GlassJob::capsule(260.0, 684.0, 760.0, 92.0)];
        assert!(baker.sync(1280.0, 800.0, &jobs, Some(197.0), 1.5));
        let ms = baker.last_ms();
        eprintln!("liquid glass capsule bake {ms:.1} ms");
        assert!(ms < 40.0, "胶囊一帧的烘焙应留在交互预算里，实际 {ms:.1} ms");
        let plate = baker.plates().find(|(id, ..)| *id == 0).expect("应有胶囊");
        // 150% 缩放 → 贴图就是 1140×138 的物理像素（和屏幕 1:1）。
        assert!(plate.1 >= 600 && plate.1 <= 1140, "胶囊贴图宽 {}", plate.1);
        assert!(plate.2 >= 80 && plate.2 <= 138, "胶囊贴图高 {}", plate.2);

        // 三块面（胶囊 + 信息卡 + 歌单）同时开，是播放中最贵的一帧。
        //
        // 取 3 次里最小的那次：这是**墙钟**耗时，而 `cargo test` 默认并行跑，
        // 别的用例抢 CPU 时同一次烘焙能翻一倍。最小值才代表这段代码自己的开销。
        // 改一下胶囊 x 让布局变化强制重烤（只有新帧的路径有 20ms 节流，这里不睡）。
        let mut best = f32::MAX;
        for dx in [0.0, 1.0, 2.0] {
            let jobs = [
                GlassJob::capsule(88.0 + dx, 684.0, 760.0, 92.0),
                GlassJob::info(24.0, 488.0, 240.0, 180.0),
                GlassJob::playlist(936.0, 48.0, 320.0, 633.0),
            ];
            assert!(baker.sync(1280.0, 800.0, &jobs, Some(197.0), 1.5));
            best = best.min(baker.last_ms());
        }
        eprintln!("liquid glass three surfaces {best:.1} ms (best of 3)");
        assert!(best < 90.0, "三块面同时开也应能跟上播放，实际 {best:.1} ms");
    }

    #[test]
    fn preview_edge_bends_a_stripe() {
        // 画面右半是品红，紧贴胶囊左缘外侧有一条白缝。左缘沿法线朝外采样，
        // 应当把这条缝拉进来，所以比再往里 24px 的品红更亮。
        let w = 640;
        let h = 360;
        let mut frame = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let (r, g, b) = if (200..214).contains(&x) {
                    (255, 255, 255)
                } else if x < 214 {
                    (30, 90, 160)
                } else {
                    (190, 30, 140)
                };
                frame[i] = b;
                frame[i + 1] = g;
                frame[i + 2] = r;
                frame[i + 3] = 255;
            }
        }
        let mut baker = GlassBaker::default();
        baker.ingest(w, h, &frame);
        // 窗口 640×400，画面 contain 后上下各留 20。胶囊左缘落在白缝右边。
        let job = GlassJob::capsule(214.0, 250.0, 360.0, 80.0);
        assert!(baker.sync(640.0, 400.0, &[job], Some(320.0), 1.0));
        let (_, pw, ph, bytes, _) = baker.plates().next().expect("plate");
        let scale = plate_scale(&job, 0.0, 1.0);
        let edge = rgb_at(bytes, pw, 1, ph / 2);
        let inside = rgb_at(bytes, pw, (24.0 * scale) as usize + 2, ph / 2);
        let lum = |c: [u8; 3]| c[0] as i32 + c[1] as i32 + c[2] as i32;
        assert!(
            lum(edge) > lum(inside) + 15,
            "左缘应把外侧白缝拉进来：edge={edge:?} inside={inside:?}"
        );

        let mut rgba = Vec::with_capacity(bytes.len());
        for px in bytes.chunks_exact(4) {
            rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
        let img = image::RgbaImage::from_raw(pw as u32, ph as u32, rgba).expect("image");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        let _ = std::fs::create_dir_all(&dir);
        img.save(dir.join("liquid-glass-preview.png"))
            .expect("save");
    }

    // ---- 退场光尘 ----

    fn dust_baker() -> GlassBaker {
        // 640×400 的窗，胶囊 360×80 贴在下方；1.0 缩放，贴图大小一眼能算。
        let (w, h) = (640, 400);
        let mut frame = vec![0u8; w * h * 4];
        for (i, px) in frame.chunks_exact_mut(4).enumerate() {
            let x = i % w;
            px[0] = (x % 64) as u8;
            px[1] = 90;
            px[2] = 40;
            px[3] = 255;
        }
        let mut baker = GlassBaker::default();
        baker.ingest(w, h, &frame);
        baker
    }

    fn phase(u: f32) -> DustPhase {
        DustPhase {
            u,
            scatter: true,
            fade: 1.0,
            run: 7,
        }
    }

    #[test]
    fn dust_seeds_only_near_the_outline() {
        // 整块一起碎会像沙漏。340 颗都该贴着轮廓：中心那块厚度最大，本来就该
        // 最后走，不该一开始就撒在中间。
        let motes = seed_motes(760.0, 92.0, 14.0, 0x5EED);
        assert_eq!(motes.len(), DUST_COUNT, "撒点没凑够数量");
        for m in &motes {
            let depth = -sdf(m.x - DUST_PAD, m.y - DUST_PAD, 760.0, 92.0, 14.0);
            assert!(
                (0.0..=DUST_BAND * 2.0).contains(&depth),
                "粒子离边太远：depth={depth}"
            );
        }
        let inner = motes
            .iter()
            .filter(|m| -sdf(m.x - DUST_PAD, m.y - DUST_PAD, 760.0, 92.0, 14.0) <= DUST_BAND)
            .count();
        assert!(inner * 10 >= DUST_COUNT * 7, "贴边的粒子只占 {inner}/{DUST_COUNT}");
        // 两趟退场不该是同一团气
        let other = seed_motes(760.0, 92.0, 14.0, 0x5EEE);
        assert!((motes[0].x - other[0].x).abs() > 0.01);
    }

    #[test]
    fn phase_is_part_of_the_bake_key() {
        // 这是最容易漏的一条：几何、帧、色相全没变时，只改相位必须判成「变了」，
        // 否则 `sync` 直接返回 false，粒子一帧都不会重画。
        let job = GlassJob::capsule(100.0, 200.0, 360.0, 80.0);
        let a = bake_key(1, 640.0, 400.0, Some(200.0), 1.0, &[job.with_dust(phase(0.10))]);
        let b = bake_key(1, 640.0, 400.0, Some(200.0), 1.0, &[job.with_dust(phase(0.20))]);
        assert_ne!(a, b, "相位必须进 key");
        // 同一个相位要稳定，否则会白白重烤
        let c = bake_key(1, 640.0, 400.0, Some(200.0), 1.0, &[job.with_dust(phase(0.10))]);
        assert_eq!(a, c);

        let mut baker = dust_baker();
        let mut last = 0;
        for step in 0..4 {
            let u = 0.1 * step as f32 + 0.1;
            let job = job.with_dust(phase(u));
            assert!(
                baker.sync(640.0, 400.0, &[job], Some(200.0), 1.0),
                "第 {step} 帧应当重烤"
            );
            last += 1;
        }
        assert_eq!(last, 4);
        // 相位相同则不重烤
        let job = job.with_dust(phase(0.4));
        assert!(!baker.sync(640.0, 400.0, &[job], Some(200.0), 1.0));
    }

    #[test]
    fn dust_plate_is_padded_around_the_capsule() {
        // 粒子要飞出轮廓，贴图必须比胶囊大一圈——否则它们会被贴图边界切掉。
        let mut baker = dust_baker();
        let job = GlassJob::capsule(100.0, 200.0, 360.0, 80.0).with_dust(phase(0.42));
        assert!(baker.sync(640.0, 400.0, &[job], Some(200.0), 1.0));
        let (id, pw, ph, _, _) = baker.plates().next().expect("plate");
        assert_eq!(id, 0);
        assert_eq!(pw, 360 + (DUST_PAD as usize) * 2);
        assert_eq!(ph, 80 + (DUST_PAD as usize) * 2);
        // 没光尘时贴图就是胶囊本身
        let mut baker = dust_baker();
        let plain = GlassJob::capsule(100.0, 200.0, 360.0, 80.0);
        assert!(baker.sync(640.0, 400.0, &[plain], Some(200.0), 1.0));
        let (_, pw, ph, _, _) = baker.plates().next().expect("plate");
        assert_eq!((pw, ph), (360, 80));
    }

    #[test]
    fn dust_is_baked_into_the_same_plate() {
        // 粒子不走第二条通道：它们就写在玻璃那张贴图里，留边那圈原本是透明的。
        let mut baker = dust_baker();
        let job = GlassJob::capsule(100.0, 200.0, 360.0, 80.0).with_dust(phase(0.42));
        assert!(baker.sync(640.0, 400.0, &[job], Some(200.0), 1.0));
        let (_, pw, ph, bytes, _) = baker.plates().next().expect("plate");
        // 留边那一圈（胶囊上方 0..48 行）原本全透明，现在该有光点
        let lit = (0..(DUST_PAD as usize))
            .flat_map(|y| (0..pw).map(move |x| (x, y)))
            .filter(|&(x, y)| bytes[(y * pw + x) * 4 + 3] > 0)
            .count();
        assert!(lit > 40, "胶囊上方的留边里只有 {lit} 个亮像素");
        // 整张图不该被整块涂白
        assert!(lit < pw * (DUST_PAD as usize) / 2, "亮像素太多：{lit}");
        // 光尘是白的（加光的预乘还原），不该带色
        let (x, y) = (0..pw)
            .flat_map(|x| (0..(DUST_PAD as usize)).map(move |y| (x, y)))
            .find(|&(x, y)| bytes[(y * pw + x) * 4 + 3] > 90)
            .expect("应有实心光点");
        let i = (y * pw + x) * 4;
        assert!(
            bytes[i] > 240 && bytes[i + 1] > 240 && bytes[i + 2] > 240,
            "光点应是白的，实际 {:?}",
            &bytes[i..i + 3]
        );
        let _ = ph;
    }

    #[test]
    fn fade_is_baked_into_the_plate_alpha() {
        // 胶囊本体的淡出烘在贴图里：粒子要活到胶囊透明之后，不能跟着它一起消失。
        let mut baker = dust_baker();
        let mut faded = phase(0.30);
        faded.fade = 0.0;
        let job = GlassJob::capsule(100.0, 200.0, 360.0, 80.0).with_dust(faded);
        assert!(baker.sync(640.0, 400.0, &[job], Some(200.0), 1.0));
        let (_, pw, ph, bytes, _) = baker.plates().next().expect("plate");
        // 胶囊内部（避开留边）已经全透明，但留边里的光点还在
        let inside = ((DUST_PAD as usize + 20) * pw + (DUST_PAD as usize + 20)) * 4;
        assert_eq!(bytes[inside + 3], 0, "fade=0 时胶囊本体应当是透明的");
        let lit = (0..ph)
            .flat_map(|y| (0..pw).map(move |x| (x, y)))
            .filter(|&(x, y)| bytes[(y * pw + x) * 4 + 3] > 0)
            .count();
        assert!(lit > 20, "胶囊透明之后粒子还得在，实际 {lit} 个像素");
    }

    #[test]
    fn dust_frames_are_cheap_once_the_glass_is_cached() {
        // 光尘逐帧重烤，但**玻璃 pass 走缓存**：第一帧之后每帧只有拷贝 + 粒子。
        // 这条是那个决定的守门人——缓存一旦失效，退场会每帧多花十几毫秒。
        let (w, h) = (1280, 720);
        let mut frame = vec![0u8; w * h * 4];
        for (i, px) in frame.chunks_exact_mut(4).enumerate() {
            let x = i % w;
            px[0] = (x % 64) as u8;
            px[1] = 90;
            px[2] = 40;
            px[3] = 255;
        }
        let mut baker = GlassBaker::default();
        baker.ingest(w, h, &frame);
        let first = {
            let job = GlassJob::capsule(260.0, 604.0, 760.0, 92.0).with_dust(phase(0.05));
            assert!(baker.sync(1280.0, 800.0, &[job], Some(197.0), 1.5));
            baker.last_ms()
        };
        // 同一个帧号：缓存没过期，这一帧不该再跑玻璃 pass
        let cached = {
            let job = GlassJob::capsule(260.0, 604.0, 760.0, 92.0).with_dust(phase(0.20));
            assert!(baker.sync(1280.0, 800.0, &[job], Some(197.0), 1.5));
            baker.last_ms()
        };
        eprintln!("dust bake: first {first:.2} ms, cached {cached:.2} ms");
        assert!(cached < first, "缓存之后应当更快：{cached:.2} >= {first:.2}");
    }

    #[test]
    fn dust_preview_phases() {
        // 诊断用：把几个相位下的贴图铺到中灰上存成 PNG，肉眼核对粒子形态。
        // （和 `preview_edge_bends_a_stripe` 一样，只有跑测试才写 target/。）
        let (w, h) = (1280, 720);
        let mut frame = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let v = 20 + (x * 40 / w) as u8;
                frame[i] = v / 2;
                frame[i + 1] = v;
                frame[i + 2] = v + 30;
                frame[i + 3] = 255;
            }
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        let _ = std::fs::create_dir_all(&dir);
        for (n, u) in [0.12f32, 0.28, 0.45, 0.62].iter().enumerate() {
            let mut baker = GlassBaker::default();
            baker.ingest(w, h, &frame);
            let mut phase = self::tests::phase(*u);
            phase.fade = (1.0 - u * 2.5).max(0.0);
            let job = GlassJob::capsule(260.0, 604.0, 760.0, 92.0).with_dust(phase);
            assert!(baker.sync(1280.0, 800.0, &[job], Some(197.0), 1.5));
            let (_, pw, ph, bytes, _) = baker.plates().next().expect("plate");
            let mut rgba = Vec::with_capacity(bytes.len());
            for px in bytes.chunks_exact(4) {
                let a = px[3] as f32 / 255.0;
                let mix = |c: u8| (c as f32 * a + 96.0 * (1.0 - a)).round() as u8;
                rgba.extend_from_slice(&[mix(px[2]), mix(px[1]), mix(px[0]), 255]);
            }
            let img = image::RgbaImage::from_raw(pw as u32, ph as u32, rgba).expect("image");
            img.save(dir.join(format!("dust-plate-{n}.png"))).expect("save");
        }
    }

    #[test]
    fn sprite_profile_matches_the_four_stops() {
        assert!((sprite_alpha(0.0) - 1.0).abs() < 1e-4);
        assert!((sprite_alpha(0.32) - 0.62).abs() < 1e-4);
        assert!((sprite_alpha(0.66) - 0.16).abs() < 1e-4);
        assert!(sprite_alpha(1.0) <= 1e-4);
        // 单调递减：任何一个凸起都会让光点出现一圈硬边
        let mut prev = sprite_alpha(0.0);
        for i in 1..=100 {
            let v = sprite_alpha(i as f32 / 100.0);
            assert!(v <= prev + 1e-6, "剖面在 {}% 处回升了", i);
            prev = v;
        }
    }
}
