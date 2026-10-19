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
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct JobKey {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
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
fn plate_scale(job: &GlassJob, dpi: f32) -> f32 {
    let dpi = if dpi.is_finite() && dpi > 0.5 {
        dpi.min(PLATE_MAX_SCALE)
    } else {
        1.0
    };
    let want = match job.polarity {
        Polarity::Dark => dpi,
        Polarity::Light => dpi * LIGHT_PLATE_RATIO,
    };
    let area = (job.w * job.h * want * want).max(1.0);
    if area <= PLATE_BUDGET {
        want
    } else {
        (PLATE_BUDGET / area).sqrt() * want
    }
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
        if let Some(prev) = self.key {
            let layout_same = prev.win_w == key.win_w
                && prev.win_h == key.win_h
                && prev.hue == key.hue
                && prev.dpi_q == key.dpi_q
                && prev.jobs == key.jobs;
            if layout_same
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
            let (x, y, w, h) = coverage_one(job, win_w, win_h);
            let (rim_max, soft_max) = backdrop_limits(job.polarity);
            self.rebuild_backdrop(win_w, win_h, Space { x, y, w, h }, rim_max, soft_max);
            let space = self.space;
            let scale = plate_scale(job, dpi);
            let mut plate = self.take_plate(job.id);
            render_plate(
                &self.soft,
                self.soft_w,
                self.soft_h,
                &self.rim,
                self.rim_w,
                self.rim_h,
                space,
                job,
                tint,
                has_tint,
                Tier::Edge,
                scale,
                dpi,
                &mut plate.bgra,
            );
            plate.w = (job.w * scale).round().max(1.0) as usize;
            plate.h = (job.h * scale).round().max(1.0) as usize;
            plate.dirty = true;
            next.push(plate);
        }
        self.plates = next;
        self.key = Some(key);
        self.last_bake = Some(started);
        self.last_ms = started.elapsed().as_secs_f32() * 1000.0;
        true
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
    out: &mut Vec<u8>,
) {
    let scale = if scale.is_finite() && scale > 0.05 {
        scale
    } else {
        1.0
    };
    let pw = (job.w * scale).round().max(1.0) as usize;
    let ph = (job.h * scale).round().max(1.0) as usize;
    out.resize(pw * ph * 4, 0);
    if soft_w == 0 || rim_w == 0 || space.w < 1.0 || space.h < 1.0 {
        return;
    }

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
    let dx = job.w / pw as f32;
    let dy = job.h / ph as f32;
    let k_soft_x = soft_w as f32 / space.w;
    let k_soft_y = soft_h as f32 / space.h;
    let k_rim_x = rim_w as f32 / space.w;
    let k_rim_y = rim_h as f32 / space.h;
    let half_w = (hw - radius).max(0.0);
    let half_h = (hh - radius).max(0.0);

    let mut col_qx = vec![0.0f32; pw];
    let mut col_bx = vec![0.0f32; pw];
    let mut col_sgx = vec![1.0f32; pw];
    let mut col_sx = vec![0.0f32; pw];
    let mut col_rx = vec![0.0f32; pw];
    for ox in 0..pw {
        let wx = job.x + (ox as f32 + 0.5) * dx;
        let rel = wx - cx;
        let qx = rel.abs() - half_w;
        col_qx[ox] = qx;
        col_bx[ox] = qx.max(0.0);
        col_sgx[ox] = if rel > 0.0 { 1.0 } else { -1.0 };
        col_sx[ox] = (wx - space.x) * k_soft_x;
        col_rx[ox] = (wx - space.x) * k_rim_x;
    }

    for oy in 0..ph {
        let wy = job.y + (oy as f32 + 0.5) * dy;
        let ty = (oy as f32 + 0.5) / ph as f32;
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

        for ox in 0..pw {
            let qx = col_qx[ox];
            let bx = col_bx[ox];
            let len = (bx * bx + by * by).sqrt();
            let d_raw = len + qx.max(qy).min(0.0) - radius;
            let o = (oy * pw + ox) * 4;
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
        let scale = plate_scale(&job, 1.0);
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
}
