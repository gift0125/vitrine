//! 主视图：播放器三栏布局 + 播放引擎状态同步。
//!
//! - 左侧：播放列表面板（List 虚拟滚动 + 拖拽排序 + 删除）
//! - 右上：封面占位区（音频）/ 视频舞台（视频）
//! - 底部：控制栏（上一首/播放暂停/下一首 + 进度条 + 音量 + 全屏 + 主题切换）
//!
//! 双引擎：音频走 `AudioEngine`（rodio），视频走 `VideoEngine`（进程内 libmpv）。
//! 按文件扩展名自动切换；视频模式下 mpv 走**软件渲染 API**，把每一帧写成 BGRA
//! 交给我们，我们包成 GPUI 纹理画在舞台上（见 `video_surface`）。
//!
//! 这条链路取代了上一代的 `--wid` 叠加窗：画面就在主窗口自己的渲染树里，不再需要
//! 「窗口打洞 + 逐像素透明 + z 序自愈」，chrome 真的压在画面之上，画面像素还能顺手
//! 拿来做胶囊染色（L2）和模糊底（L3）。
//!
//! 100ms 定时器轮询位置并同步 SeekBar，播放结束自动切下一首。

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use player_core::{
    find_libmpv, fit_render_size, is_video_file, load_track_meta, scan_dir_for_tracks,
    AudioEngine, PlaybackState, SpawnOptions, TrackMeta, VideoEngine, VideoStatus,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::list::{List, ListEvent, ListState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Icon, InteractiveElementExt as _, Sizable as _, Theme,
    ThemeMode, WindowExt as _,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::persistence::{self, RepeatMode, Settings};
use crate::playlist::PlaylistDelegate;
use crate::theme::overlay;
use crate::ui_state::{ChromeLayout, Rect as UiRect, UiState};
use crate::video_surface::VideoSurface;
use crate::window_actions::{self, Hwnd};

/// 进度条量程未知时的兜底（元数据解析失败）。
const SEEK_FALLBACK_MAX: f32 = 600.0;
/// 轮询间隔（计划约定 100ms）。
const TICK_INTERVAL: Duration = Duration::from_millis(100);
/// 顶部标题带高度（系统标题栏已关闭，这一段是自绘拖拽区 + 窗口按钮）。
///
/// 设计稿分平台：mac 自绘 34，**Windows 主窗 40**（和设置窗标题带一致，也是
/// Win10/11 原生标题栏高度）；窗口按钮 46×40 正好是一块标准热区。
///
/// `theme::install` 也读它来给 toast 让位（通知层要从标题带下面开始），
/// 所以是 `pub(crate)`。
pub(crate) const TITLE_BAND: f32 = 40.0;
/// 底部悬浮胶囊的内缩。
const CAPSULE_INSET: f32 = 24.0;
/// 胶囊**固定宽度**（设计稿 760）。
///
/// 以前是 `left/right: 24` 跟着窗口拉伸，1280 宽的窗口上会撑成 1232——设计稿里
/// 它是画面下方一块窄条，两侧都留出画面。窄窗口用 `max_w_full` 退让，别让它在
/// 600 宽的小窗上溢出被裁掉。
const CAPSULE_WIDTH: f32 = 760.0;
/// 胶囊左右内边距（设计稿 20）。
const CAPSULE_PAD_X: f32 = 20.0;
/// 胶囊上下内边距（设计稿 8）。
///
/// 8×2 + 进度区 24 + 间距 8 + 走带 44 = 92，正好是设计稿的胶囊高度。
/// 旧版写 12，靠「进度行 16 + 间距 8」凑到 92，但那样时间行和轨道挤在同一行，
/// 和设计稿的两行进度区差一个数量级。
const CAPSULE_PAD_Y: f32 = 8.0;
/// 胶囊圆角（设计稿 14）。
const CAPSULE_RADIUS: f32 = 14.0;
/// 进度区总高（设计稿 24）= 时间行 16 + 间距 4 + 轨道 4。
const PROGRESS_HEIGHT: f32 = 24.0;
/// 进度区里的时间行高度（设计稿 16，两端对齐）。
const PROGRESS_TIME_ROW: f32 = 16.0;
/// 时间行与轨道之间的间距（设计稿 4）。
const PROGRESS_GAP: f32 = 4.0;
/// 进度区左右**光学修正**（设计稿 `side_padding: 3`）。
///
/// 轨道两端要和上面的图标字形对齐：图标在 20×20 盒子里图形只占 ~15（内缩
/// 2.5），轨道贴盒子边就会比图标两端各宽 3px，看着像"轨道戳出来一截"。
///
/// 这里写 2 而不是 3：胶囊自己还有 1px 描边，20 + 1 + 2 = 23 正好等于设计稿量到的
/// 「时间码左沿 − 胶囊左沿 = 23」，轨道也就是 760 − 2×1 − 2×20 − 2×2 = 714。
const PROGRESS_SIDE_PAD: f32 = 2.0;
/// 走带（播放控制）中点与相邻按钮的间距（设计稿实测 14）。
///
/// 设计稿的节奏：播放键两侧各留 4 的呼吸（44 的圆 + 4 = 52 的视觉盒），
/// 再往外每 14 一个 28 宽按钮 → 中心距 42（±10s↔上/下一首）、54（播放↔±10s）。
const TRANSPORT_GAP: f32 = 14.0;
/// 播放键直径（设计稿实测 44 正圆）。
const PLAY_BUTTON: f32 = 44.0;
/// 播放键两侧额外的呼吸宽度：设计稿里圆和 ±10 按钮之间比图标间宽 8。
const PLAY_BREATH: f32 = 4.0;
/// 走带行高度（设计稿 44，正好装下 44 的播放键）。
const CONTROLS_ROW: f32 = 44.0;
/// 时间码字号（设计稿 11，等宽）。
const TIME_SIZE: f32 = 11.0;
/// 音量条宽度（设计稿 96）。
const VOLUME_WIDTH: f32 = 96.0;
/// 视频信息卡宽度/高度（设计稿 240×180）。
const INFO_WIDTH: f32 = 240.0;
const INFO_HEIGHT: f32 = 180.0;
/// 信息卡与胶囊之间的间距（设计稿 16）。
const INFO_GAP: f32 = 16.0;
/// 底部为悬浮胶囊预留的高度。
///
/// 胶囊实高 92（见 `CAPSULE_PAD_Y`：8 + 24 + 8 + 44 + 8），加上 24 内缩再留
/// 2px 余量。留多了窗口下方会空出一条死气沉沉的黑带，留少了玻璃胶囊会贴到
/// 画面最下沿、下沿那圈亮边看不出厚度。
const BOTTOM_BAND: f32 = 118.0;
/// 多久没有新的拖拽经过就算「拖走了」：`drag_over` 在拖动过程中会持续触发，
/// 停更这么久只可能是鼠标带着文件离开了窗口。
const DROP_HINT_IDLE: Duration = Duration::from_millis(250);
/// 窗口按钮宽度：Windows 10/11 的标题栏按钮就是 46 宽、与标题栏等高。
const CAPTION_BUTTON_WIDTH: f32 = 46.0;
/// 视频模式下状态胶囊的存活时间。
///
/// 它替代了 toast，没有「手动关闭」的入口，只能靠时间退场；4s 够读完一行
/// 「已导入 3 个文件 · 正在播放「xxx」」，又不至于在画面顶上挂太久。
const STATUS_PILL_DURATION: Duration = Duration::from_secs(4);
/// 悬浮播放列表面板宽度（设计稿 320）。
const PLAYLIST_WIDTH: f32 = 320.0;
/// 播放列表面板到窗口右沿的距离（设计稿 Win `x: 936` → 1280 − 936 − 320 = 24）。
const PLAYLIST_INSET: f32 = 24.0;
/// 面板顶边与标题带之间的距离（设计稿 Win `y: 48`，即标题带 40 + 8）。
const PLAYLIST_TOP_GAP: f32 = 8.0;
/// 面板底边距窗口底的距离：设计稿面板高 633 = 800 − 48 − 119，而 119 正是
/// 「胶囊 92 + 内缩 24 + 3 余量」——所以这里锚底而不是写死 633，小窗口才不溢出。
const PLAYLIST_BOTTOM: f32 = 119.0;
/// 面板头部高度（设计稿 48，装「播放列表 / 章节」两个 tab 与关闭键）。
const PLAYLIST_HEADER: f32 = 48.0;
/// 面板底部工具条高度（循环 / 随机 / 排序 · 计数 · 添加 / 移除 / 清空）。
const PLAYLIST_FOOTER: f32 = 44.0;
/// 列表行高（设计稿 48，双行：标题 + 「规格 · 时长」）。
const PLAYLIST_ROW: f32 = 48.0;
/// 底部渐隐遮罩高度（设计稿 `texture_layers.bottom_vignette`：240）。
const VIGNETTE_HEIGHT: f32 = 240.0;
/// 播放中多久没有输入就开始淡出控制层。
///
/// 视频播放器（IINA / QuickTime / VLC）的通用手感是 2~3 秒；取中间值。暂停、
/// 拖动进度、开着面板时**不**淡出——那会儿用户正在读控件。
const CHROME_IDLE: Duration = Duration::from_millis(2500);
/// 淡出/淡入每 100ms tick 走一步：0.25 → 400ms 淡完，四步用肉眼已经看不出台阶。
///
/// 不用 GPUI 的 `with_animation`：那个动画只在有帧驱动时推进，而这个界面的帧
/// 是 tick 拉起来的（画面本身就是 mpv 在画，主窗口大部分时间是空的）。
const CHROME_FADE_STEP: f32 = 0.25;
/// 淡到这个值以下就当「已经隐藏」：chrome 整块让出、探针里 `rects` 为空。
const CHROME_HIDDEN_EPS: f32 = 0.01;
/// 画面色相换一次的门限（度）。
///
/// 逐帧套用会让胶囊玻璃跟着画面闪——同一镜头里每帧的均值都在几个度上抖。
/// 8° 差不多是「换了个配色」的量级，一个镜头之内基本一动不动。
const TINT_HYSTERESIS: f32 = 8.0;
/// 时间码字体（设计稿用等宽）：跟主题走，取平台原生的等宽族。
///
/// 写死 "Consolas" 在 macOS/Linux 上不存在，会退到非等宽字体，
/// 时间码一跳动整行宽度就跟着抖。
fn mono_font() -> &'static str {
    crate::theme::mono_font_family()
}
/// 关闭按钮的悬停底色（Windows 原生的 #C42B1C）。
const CLOSE_DANGER: u32 = 0xC42B1C;
/// 标题栏控制图标（10×10 网格、1 单位描边 = 1 物理逻辑像素）。
///
/// 不能用图标库里的 `WindowMinimize/Maximize/Close`：那些是 24 格、2px 描边的画法，
/// 缩到 10px 后描边被抗锯齿糊成一坨（最大化看着像个实心小方块）。这里按 Windows
/// 真实字形的尺寸重画，落点对齐到像素中心，1.5 倍屏下也干净。
const ICON_MINIMIZE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><path d="M0 5h10" stroke="currentColor" stroke-width="1"/></svg>"#;
const ICON_MAXIMIZE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect x="0.5" y="0.5" width="9" height="9" fill="none" stroke="currentColor" stroke-width="1"/></svg>"#;
const ICON_RESTORE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1"><rect x="2.5" y="0.5" width="7" height="7"/><rect x="0.5" y="2.5" width="7" height="7"/></svg>"#;
const ICON_CLOSE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><path d="M0.5 0.5l9 9M9.5 0.5l-9 9" stroke="currentColor" stroke-width="1"/></svg>"#;

/// ±10 秒图标的 path（24 格）。
///
/// 按设计稿手算的方向画：快退的缺口开在右上、箭头在顶部朝左，快进是精确镜像
/// （不是把同一个图标翻转，否则箭头的切线方向会反）。
const ICON_REWIND_10: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none"><path d="M12 3A9 9 0 1 0 21 12" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/><path d="M11.9 3.1L15.6 0.6 15.6 5.6Z" fill="currentColor"/></svg>"#;
const ICON_FORWARD_10: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none"><path d="M12 3A9 9 0 1 1 3 12" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/><path d="M12.1 3.1L8.4 0.6 8.4 5.6Z" fill="currentColor"/></svg>"#;

/// 走带与右组图标（**20 格**，图形尽量铺满 viewBox）。
///
/// 这里全部手写而不是取图标库：设计稿用的是**实心**跳过键、带内嵌小窗的画中画、
/// 五根声柱的音轨键、「CC」字幕键、左对齐三条线的列表键——`IconName` 里没有
/// 这几枚（`Captions` 是矩形加两条线、`ListMusic` 是音符列表、`SkipBack` 是描边
/// 三角），形状对不上就别硬套。
///
/// 尺寸约定：按钮盒 20 → `Button` 把图标渲染成 20×0.75 = 15，所以图形按
/// 「1..19」铺满，渲染后正好 ~15（设计稿字形 15~17）。描边 1.9 → 15/20 渲染后
/// ≈1.4，和设计稿的线条粗细一致。
const ICON_PREV: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20"><rect x="4.3" y="2.9" width="1.9" height="14.2" rx="0.7" fill="currentColor"/><path d="M15.7 2.9v14.2L8.1 10Z" fill="currentColor"/></svg>"#;
const ICON_NEXT: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20"><rect x="13.8" y="2.9" width="1.9" height="14.2" rx="0.7" fill="currentColor"/><path d="M4.3 2.9v14.2L11.9 10Z" fill="currentColor"/></svg>"#;
/// 字幕：圆角矩形 + 两个 C（用两段弧画，避免 SVG `<text>` 挑字体）。
const ICON_CC: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="none"><rect x="1" y="3.6" width="18" height="12.8" rx="3.4" stroke="currentColor" stroke-width="1.9"/><path d="M8.3 8.1a2.2 2.2 0 1 0 0 3.8M14.7 8.1a2.2 2.2 0 1 0 0 3.8" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>"#;
/// 音轨：五根声柱（中间最高）。
const ICON_AUDIO_BARS: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><path d="M2.2 8.2v3.6M6.1 5.6v8.8M10 3.2v13.6M13.9 5.6v8.8M17.8 7.2v5.6"/></svg>"#;
/// 画中画：屏幕框 + 右下内嵌实心小窗。
///
/// 暂时只有图形、没有按钮：真正的画中画要一个独立的置顶小窗（视频铺满整窗、
/// 腾掉标题带与胶囊），是下一轮的事；图形先留着，别让下一个人再画一遍。
#[allow(dead_code)]
const ICON_PIP: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="none"><rect x="1" y="3.6" width="18" height="12.8" rx="3.4" stroke="currentColor" stroke-width="1.9"/><rect x="10.6" y="8.8" width="6.2" height="5.2" rx="1.2" fill="currentColor"/></svg>"#;
/// 播放列表：三条左对齐横线（长度递减再回一点，和设计稿一致）。
const ICON_LIST_LINES: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><path d="M2 5.5h16M2 10h11M2 14.5h13.4"/></svg>"#;

/// 面板底部工具条图标（24 格、1.6 描边，16px 渲染后 ≈1.1 的细线）。
const ICON_REPEAT: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M17 2l4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="M7 22l-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/></svg>"#;
const ICON_SHUFFLE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M16 3h5v5"/><path d="M4 20L21 3"/><path d="M21 16v5h-5"/><path d="M15 15l6 6"/><path d="M4 4l5 5"/></svg>"#;
const ICON_SORT: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M7 4v16"/><path d="M4 7l3-3 3 3"/><path d="M17 20V4"/><path d="M14 17l3 3 3-3"/></svg>"#;
const ICON_MINUS: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"><path d="M5 12h14"/></svg>"#;

/// 播放 / 暂停：**33 格**（= 44 圆 × 0.75，`Button` 把图标渲染成盒子的 0.75）。
///
/// 图形尺寸按设计稿实测反推：白色三角 14 宽 × 18 高、双竖条各 4 宽 × 18 高。
/// 直接用 `IconName::Play` 不行——那是 24 格满画布的三角，缩到 33 会有 ~26 高，
/// 把 44 的圆塞满，和设计稿的"小三角 + 留白"完全两回事。
const ICON_PLAY_FILL: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 33 33"><path d="M12 9.8v12.7l10.5-6.35Z" fill="currentColor"/></svg>"#;
const ICON_PAUSE_FILL: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 33 33"><rect x="11.6" y="9.4" width="3.7" height="14.2" rx="1.1" fill="currentColor"/><rect x="18" y="9.4" width="3.7" height="14.2" rx="1.1" fill="currentColor"/></svg>"#;

/// 20 盒 / 24 盒 / 28 盒的通用图标按钮（走带与右组共用）。
///
/// 为什么不用 `IconName` 一把梭：设计稿的跳过键是**实心**三角、音轨键是五根声柱、
/// 字幕键是「CC」、列表键是三条左对齐横线，图标库里没有对应的画法（`SkipBack`
/// 是描边、`Captions` 是矩形加两横、`ListMusic` 带音符）。这几枚在
/// `ICON_*` 里手绘，`active` 只改颜色不改形状。
fn chrome_button(
    cx: &gpui::App,
    id: &'static str,
    icon: Icon,
    label: &'static str,
    size: f32,
    active: bool,
) -> Button {
    // `ghost` 变体的悬停底色是 `theme.accent`（浅底主题下是一块亮 Lavender），
    // 压在深玻璃上完全不对——设计稿要的是白 10%。`ghost` 的悬停色写死在
    // `ButtonVariant::hovered` 里，只有 `custom` 变体肯用调用方给的颜色。
    let custom = ButtonCustomVariant::new(cx)
        .color(gpui_kit::transparent_black())
        .foreground(if active {
            overlay::accent()
        } else {
            overlay::fg()
        })
        .hover(overlay::hover())
        .active(overlay::active());
    Button::new(id)
        .custom(custom)
        .with_size(px(size))
        .icon(icon)
        .rounded(px(6.))
        .tooltip(label)
        .accessibility_label(label)
}

/// 浅底面板上的图标按钮（页脚 / 面板头）。
///
/// `chrome_button` 用的是浮层前景 `overlay::fg()`（≈ #D5D9E2），那是给**深玻璃**
/// 胶囊用的。面板是主题浅底（Latte #EFF1F5），同一枚近白色图标放上去几乎看不见
/// ——页脚的循环/随机/排序就是这样糊成一团的。面板上的东西必须跟主题色走。
fn panel_button(
    id: &'static str,
    icon: Icon,
    label: &'static str,
    size: f32,
    active: bool,
    fg: gpui::Hsla,
    accent: gpui::Hsla,
) -> Button {
    Button::new(id)
        .ghost()
        .with_size(px(size))
        .icon(icon)
        .text_color(if active { accent } else { fg })
        .tooltip(label)
        .accessibility_label(label)
}

/// 窗口尺寸变化后多久落盘一次（tick 计数，50 ≈ 5s）。
const WINDOW_SAVE_TICKS: usize = 50;

/// 方向键的跳转步长（秒）：左右各 5 秒。
///
/// 走带上的 ±10 秒按钮是「粗调」，方向键留给「微调」——这和 mpv / IINA 的习惯一致，
/// 连按几次能落到比按钮更细的位置上。
const KEY_SEEK_STEP: f64 = 5.0;

/// 方向键的音量步长（百分点）：上下各 5%。
///
/// 100/5 = 20 档，从静音按到满音量最多 20 下，比 1% 那种按到手酸、比 10% 那种
/// 一按就跳过头都更合适。
const KEY_VOLUME_STEP: i32 = 5;

/// 订阅向量的固定下标。
const SUB_SEEK: usize = 1;

/// render 阶段捕获的视频区矩形（窗口逻辑坐标 x, y, w, h）。
/// 用 `Cell` 是因为 paint 回调只有 `&self` 上下文，且不需要触发重绘。
type VideoRectCell = Rc<Cell<Option<(f32, f32, f32, f32)>>>;

/// 视频播放运行时（mpv 引擎 + 界面手里的画面）。
#[derive(Default)]
struct VideoRuntime {
    engine: Option<VideoEngine>,
    /// 舞台上的画面、胶囊模糊底、染色色相（见 `video_surface`）
    surface: VideoSurface,
    /// mpv 的「有新帧」信号（容量 1，投递侧 `try_send` 合并）。
    ///
    /// 放在这里是因为**接信号要 window**（`cx.spawn_in`），而 `start_video` 手上
    /// 没有 window：引擎起播时只把接收端存下来，下一次 tick 再把泵拉起来。
    frames: Option<async_channel::Receiver<()>>,
}

/// 方向键的四个方向。
///
/// 单独抽出来是因为它要在两处用：`intercept_keystrokes` 里判「这个键要不要吃」、
/// 以及 `handle_arrow` 里决定动作。散着写就得比较四遍字符串，错一个字母是静默失效
/// （`Keystroke.key` 是小写命名键，写错只会匹配不上，不会报错）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ArrowKey {
    Left,
    Right,
    Up,
    Down,
}

impl ArrowKey {
    fn parse(key: &str) -> Option<Self> {
        Some(match key {
            "left" => Self::Left,
            "right" => Self::Right,
            "up" => Self::Up,
            "down" => Self::Down,
            _ => return None,
        })
    }

    /// 左右是「有片子才有意义」的跳转；上下改的是全局音量，没有曲目时也该生效。
    fn needs_track(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
}

/// 切轨回执的等待上限：超过这么久观测值还没变，就认定「没有别的轨可切」。
///
/// mpv 处理 `cycle` 是毫秒级的，800ms 已经非常宽松；再长用户会觉得点了没反应。
const TRACK_ACK_TIMEOUT: Duration = Duration::from_millis(800);

/// 等回执的两种「切轨」动作。
#[derive(Clone, Copy, PartialEq, Eq)]
enum TrackKind {
    Subtitle,
    Audio,
}

impl TrackKind {
    fn label(self) -> &'static str {
        match self {
            Self::Subtitle => "字幕",
            Self::Audio => "音轨",
        }
    }
}

/// 相对跳转的落点：夹到 `[0, 时长]` 之间。
///
/// 越界后播放器自己会回弹，用户在进度条上看到的就是「按了右键、进度却往回退」。
pub(crate) fn seek_target(position: f64, delta: f64, max: f64) -> f64 {
    (position + delta).clamp(0.0, max)
}

/// 控制层（标题带 + 胶囊 + 底部遮罩）该不该显示。
///
/// 抽成纯函数是因为这条判据的每一根分支都对应一个用户场景，而真正的调用点
/// （`PlayerView::chrome_wanted`）背后挂着 mpv 引擎，测试里造不出来：
/// - 播放中 2.5 秒没输入 → 淡出（`CHROME_IDLE`）；
/// - 暂停 / 音频模式 / 正在拖进度 / 面板或信息卡开着 → 必须留着；
/// - 有任何输入（鼠标移动、按键、点按钮）→ 立刻淡回来。
pub(crate) fn chrome_visible_target(
    video_active: bool,
    playing: bool,
    busy: bool,
    idle: Duration,
) -> bool {
    if !video_active || busy || !playing {
        return true;
    }
    idle < CHROME_IDLE
}

/// 两个色相之间的最短距离（度，0~180）。///
/// 色相是环：350° 和 10° 只差 20°，不是 340°。胶囊染色的迟滞判据要用它，
/// 直接相减会让「红→品红」那一小段永远算成大幅变化，玻璃每帧都在闪。
pub(crate) fn hue_distance(a: f32, b: f32) -> f32 {
    let diff = (a - b).abs() % 360.0;
    diff.min(360.0 - diff)
}

/// 诊断：`SMP_RENDER_TRACE=1` 时把「舞台 / 原生 / 实际帧」的尺寸关系打出来。
///
/// 只在三者之一变化时打一行（逐帧打会把 session.log 淹掉）。这一行能一次回答
/// 三个常见病症：帧尺寸跟舞台一样 = 原生尺寸还没报上来（画面被拉伸）、
/// 帧尺寸小于舞台 = 被像素预算或原生尺寸限制（放大靠 GPU）、
/// 帧尺寸的比例和舞台不一样 = contain 生效了（该有黑边）。
fn trace_frame(
    stage_w: f32,
    stage_h: f32,
    native: Option<(u32, u32)>,
    frame: (usize, usize),
    scale: f32,
) {
    if std::env::var_os("SMP_RENDER_TRACE").is_none() {
        return;
    }
    use std::sync::Mutex;
    static LAST: Mutex<Option<(u32, u32, u32, u32)>> = Mutex::new(None);
    let key = (stage_w as u32, stage_h as u32, frame.0 as u32, frame.1 as u32);
    let mut slot = LAST.lock().unwrap();
    if *slot == Some(key) {
        return;
    }
    *slot = Some(key);
    let native = match native {
        Some((w, h)) => format!("{w}x{h}"),
        None => "none".to_string(),
    };
    let detail = format!(
        "frame {width}x{height} stage {sw:.0}x{sh:.0} native {native} scale {scale}",
        width = frame.0,
        height = frame.1,
        sw = stage_w,
        sh = stage_h,
    );
    eprintln!("[render] {detail}");
    persistence::log_session(&format!("render: {detail}"));
}

pub struct PlayerView {
    engine: Option<AudioEngine>,
    playlist: Entity<ListState<PlaylistDelegate>>,
    seek: Entity<SliderState>,
    volume: Entity<SliderState>,
    /// 当前曲目下标（对应 delegate.tracks）
    current: Option<usize>,
    /// 用户正在拖动进度条（期间不回写 slider 值）
    user_seeking: bool,
    volume_pct: i32,
    /// 上次落盘的窗口尺寸（宽度, 高度）
    last_saved_size: (f32, f32),
    /// 自窗口尺寸变化以来经过的 tick 数
    window_dirty_ticks: usize,
    settings: Settings,
    subscriptions: Vec<Subscription>,
    /// 当前进度条量程（切换曲目/时长更新时重建）
    seek_max: f32,
    /// 播放列表面板当前在「章节」页而不是「播放列表」页
    chapter_tab: bool,
    /// 当前视频的章节 `(起始秒, 标题)`：打开列表时按需读取，换片时清空
    chapters: Vec<(f64, String)>,

    // ---- 视频链路 ----
    video: VideoRuntime,
    /// 当前是否处于视频模式
    video_active: bool,
    /// 视频内核不可用时的提示文案
    video_error: Option<String>,
    /// 已经按哪一部片子的尺寸调过窗口（画面宽高）。
    ///
    /// 每部片子只调一次：用户手动拉伸过窗口之后，不能在下一个 tick 又弹回去。
    fit_done: Option<(u32, u32)>,
    /// 待确认的画面尺寸（宽、高、**已连续见到几次**）。
    ///
    /// mpv 的 `dwidth/dheight` 不止报一次：实测 1280×720 的片子起播时先报
    /// 960×540，隔一帧才报真值。按第一份数据调窗口会先跳到一个错的尺寸再跳回来，
    /// 所以同一个值连着出现两次才动手。
    fit_seen: Option<(u32, u32, u8)>,
    /// render 阶段由 canvas 捕获的视频区（窗口逻辑坐标 x, y, w, h）。
    ///
    /// 画面现在铺满整个窗口，这个值只给验收探针用（「舞台铺到哪」），不再驱动
    /// 叠加窗定位。
    video_rect: VideoRectCell,
    /// render 阶段由 canvas 捕获的控制胶囊矩形。
    ///
    /// 胶囊高度是内容撑开的（随字号/行距变），不能写死，只能量：验收脚本要按它
    /// 算点击坐标，单元测试要断言 760×92。
    capsule_rect: VideoRectCell,
    /// 等 mpv 回执的切轨动作：`(类型, 切换前的轨号, 发起时刻)`。    ///
    /// 见 `cycle_subtitles`：`cycle` 是单向命令、没有返回值，只能靠观测值变化确认。
    pending_track: Option<(TrackKind, u32, Instant)>,
    /// 是否展开「视频信息」卡（胶囊左端的 `ⓘ`）。
    ///
    /// 尺寸写死 240×180，但仍然量一次——字号/行距跟着主题变，实际高度未必是 180；
    /// 验收探针和单元测试都靠这个矩形。
    info_open: bool,
    info_rect: VideoRectCell,
    /// 待还掉的画面贴图（退出视频时立起来，下一次 tick 真正还）。
    surface_clear: bool,
    /// 主窗口当前是不是圆角状态（视频模式才圆，见 `sync_video_chrome`）。
    corners_rounded: bool,
    /// 上一次补算客户区（`refresh_client_area`）的时刻，用来限流
    client_kick: Option<Instant>,
    /// 上一次因为布局停在旧尺寸而补 `refresh()` 的时刻，用来限流
    layout_kick: Option<Instant>,
    /// 是否已经把窗口表面的底色改成透明（见 `Render` 里的说明）
    surface_transparent: bool,
    /// GPUI 主窗口句柄（拖动 / 最大化 / 客户区自愈都要它）
    main_hwnd: Hwnd,
    /// 窗口是否最大化。
    ///
    /// 不用 `window.is_maximized()` 直接读：那个值本身是对的（`IsZoomed`），
    /// 但**切换**不能走 `window.zoom_window()`（Windows 后端只最大化、不还原，
    /// 见 `window_actions`）。自己缓存一份，tick 里比对，状态变化才重绘。
    maximized: bool,
    /// 最近一次「点了最大化/还原」的时刻。
    ///
    /// `set_zoomed` 是异步的（同步调用会在 GPUI 的事件派发里重入 `WM_SIZE`，
    /// 界面不会跟着重新布局），所以紧接着读 `IsZoomed` 还是旧值。300ms 内不让
    /// tick 反向纠正，图标就不会在点下去的一瞬间闪一下。
    zoom_pending: Option<Instant>,
    /// 影院模式（隐藏歌单 + 系统全屏）
    cinema: bool,
    /// 悬浮播放列表面板是否展开（新版设计稿默认收起）
    playlist_open: bool,
    /// 控制层（标题带 + 胶囊 + 底部遮罩）当前的透明度：1 全显、0 全隐。
    ///
    /// 视频播放中一段时间没有输入就淡到 0，鼠标一动淡回来（见 [`CHROME_IDLE`]）。
    /// 只在 tick 里按 `CHROME_FADE_STEP` 推一步，所以淡出是 4 帧的阶梯，不是
    /// 真正的插值——换来的是**不需要额外帧驱动**，界面的帧本来就由 tick 拉起来。
    chrome_alpha: f32,
    /// 最近一次输入（鼠标移动/按键）的时刻，自动淡出的判据。
    activity: Instant,
    /// 鼠标移动的时间戳（`Cell`：`on_mouse_move` 每帧都来，不能每次都 notify）。
    ///
    /// 和 `drop_activity` 同一个套路：监听里只写时间戳，由 tick 消费并决定重绘。
    activity_moved: Rc<Cell<Option<Instant>>>,
    /// L2 画面染色：当前**已应用**的色相（0~360）。
    ///
    /// 逐帧取色是免费的（像素本来就在手上，见 `video_surface::FrameRef::band_hue`），
    /// 但每帧都换一次玻璃颜色会跟着画面闪。所以只在色相**跳超过 8°** 时才应用，
    /// 一个镜头之内基本是稳定的。
    tint_applied: Option<f32>,
    /// 拖动窗口的锚点：`(按下时光标 x, y, 窗口 x, y)`（都是屏幕物理像素）。
    ///
    /// 用 `Cell` 而不是普通字段：拖动循环跑在 `cx.spawn_in` 的任务里，每个 tick
    /// 读一次锚点、算一次目标位置。画面区以前归叠加窗，拖动由 wndproc 做；现在
    /// 画面在主窗口里，GPUI 的 `start_window_move()` 在 Windows 后端又没有实现
    /// （实测只有 apple / test 后端有），只能自己搬。
    drag_anchor: Rc<Cell<Option<(i32, i32, i32, i32)>>>,
    /// 验收探针的输出路径（`SMP_UI_STATE`），没设就完全不产生开销。
    ui_probe: Option<PathBuf>,
    /// 上一次写出去的探针 JSON（内容没变就不重写盘）。
    ui_published: Option<String>,

    // ---- 用户可见错误 ----
    /// 待弹出的错误提示（由 tick 统一推成 toast，保证有 window 可用）
    pending_errors: Vec<SharedString>,
    /// 待弹出的普通提示（例如导入结果）
    pending_infos: Vec<SharedString>,
    /// 启动即播放的曲目（命令行 `--play` / 位置参数）；在第一次 tick 消费
    pending_autoplay: Option<usize>,
    /// 最近一次「有外部文件在窗口上方」的时间戳（用于画「松开即可播放」提示）。
    ///
    /// 用 `Cell` 而不是普通字段：判定「拖的是外部文件」只能靠 `drag_over::<ExternalPaths>`
    /// 的样式回调，它拿不到 `Context`，改不了普通字段、也没法 `cx.notify()`。
    /// 所以这里只记时间戳，让 100ms 的 tick 去决定「显示/隐藏 + 重绘」——
    /// 拖走时不再有新时间戳，250ms 后自动熄灭，不依赖 `Exited` 事件是否送到。
    drop_activity: Rc<Cell<Option<Instant>>>,
    /// 上一帧是否画了拖放提示（tick 用它判断要不要重绘）
    drop_hint_shown: bool,
    /// 视频模式下的临时状态胶囊（文案, 出现时刻, 是否错误）。
    ///
    /// 视频模式下 gpui-component 的 toast 会和画面抢焦点位置（落在画面中央），
    /// 所以视频模式改成画在标题带上的小胶囊。
    status_pill: Option<(SharedString, Instant, bool)>,
    /// 上一帧是否画了状态胶囊（tick 用它判断要不要重绘）
    status_pill_shown: bool,
    /// 最近一条错误（便于测试断言，也给未来的状态栏复用）
    last_error: Option<SharedString>,
}

impl PlayerView {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        initial_tracks: Vec<TrackMeta>,
        initial_current: Option<usize>,
        settings: Settings,
        autoplay: Option<usize>,
    ) -> Self {
        let small = cx.weak_entity();
        let playlist =
            cx.new(|cx| ListState::new(PlaylistDelegate::new(initial_tracks, small), window, cx));
        let seek = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(SEEK_FALLBACK_MAX)
                .default_value(0.0)
                .step(1.0)
        });
        let volume = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(100.0)
                .default_value(settings.volume * 100.0)
                .step(1.0)
        });

        let subscriptions = vec![
            // 点击/回车确认曲目 -> 播放
            cx.subscribe(&playlist, |this, _list, event: &ListEvent, cx| {
                if let ListEvent::Confirm(ix) = event {
                    this.play_track(ix.row, cx);
                }
            }),
            // 拖动进度条：Change 期间仅跟随显示，Release 才真正 seek（避免拖动爆音）
            cx.subscribe(
                &seek,
                |this, _slider, event: &SliderEvent, cx| match event {
                    SliderEvent::Change(_) => this.user_seeking = true,
                    SliderEvent::Release(value) => {
                        this.user_seeking = false;
                        this.seek_to(value.start() as f64);
                        cx.notify();
                    }
                },
            ),
            // 音量
            cx.subscribe(&volume, |this, _slider, event: &SliderEvent, cx| {
                if let SliderEvent::Change(value) = event {
                    this.set_volume_pct(value.start() as i32, cx);
                }
            }),
        ];

        let mut subscriptions = subscriptions;
        // 窗口移动/缩放：圆角属性与验收探针要跟着更新
        subscriptions.push(cx.observe_window_bounds(window, |this, window, _cx| {
            this.sync_video_chrome(window);
        }));
        // 退出应用时结束 mpv 子进程（避免孤儿进程）
        subscriptions.push(cx.on_app_quit(|this, _cx| {
            let detail = match this.current {
                Some(i) => format!(
                    "app_quit requested (video_active={}, current={i})",
                    this.video_active
                ),
                None => format!(
                    "app_quit requested (video_active={}, current=none)",
                    this.video_active
                ),
            };
            persistence::log_session(&detail);
            this.shutdown_video();
            async {}
        }));

        // 引擎：无音频设备时降级为只读 UI（控件仍可交互但无声）
        let mut startup_errors: Vec<SharedString> = Vec::new();
        let mut engine = match AudioEngine::new() {
            Ok(engine) => Some(engine),
            Err(err) => {
                eprintln!("警告：音频引擎初始化失败，播放功能不可用: {err}");
                startup_errors.push(format!("音频设备不可用：{err}").into());
                None
            }
        };
        if let Some(engine) = engine.as_mut() {
            engine.set_volume(settings.volume);
        }

        // 恢复上次播放位置高亮（不自动续播）
        if let Some(row) = initial_current {
            let count = playlist.read(cx).delegate().track_count();
            if row < count {
                playlist.update(cx, |state, cx| {
                    state.delegate_mut().set_confirmed(Some(row));
                    cx.notify();
                });
            }
        }

        // 主窗口句柄：拖动、最大化、客户区自愈都要它
        let main_hwnd = window
            .window_handle()
            .ok()
            .and_then(|handle| match handle.as_raw() {
                RawWindowHandle::Win32(win32) => Some(win32.hwnd.get() as Hwnd),
                _ => None,
            })
            .unwrap_or(0);

        let video_rect: VideoRectCell = Rc::new(Cell::new(None));
        let capsule_rect: VideoRectCell = Rc::new(Cell::new(None));
        let info_rect: VideoRectCell = Rc::new(Cell::new(None));
        let maximized = window.is_maximized();

        let mut this = Self {
            engine,
            playlist,
            seek,
            volume,
            current: None,
            user_seeking: false,
            volume_pct: (settings.volume * 100.0) as i32,
            last_saved_size: (settings.window_width, settings.window_height),
            window_dirty_ticks: 0,
            settings,
            subscriptions,
            seek_max: SEEK_FALLBACK_MAX,
            chapter_tab: false,
            chapters: Vec::new(),
            video: VideoRuntime::default(),
            video_active: false,
            video_error: None,
            fit_done: None,
            fit_seen: None,
            video_rect,
            capsule_rect,
            pending_track: None,
            info_open: false,
            info_rect,
            surface_clear: false,
            corners_rounded: false,
            client_kick: None,
            layout_kick: None,
            surface_transparent: false,
            main_hwnd,
            maximized,
            zoom_pending: None,
            cinema: false,
            playlist_open: false,
            chrome_alpha: 1.0,
            activity: Instant::now(),
            activity_moved: Rc::new(Cell::new(None)),
            tint_applied: None,
            drag_anchor: Rc::new(Cell::new(None)),
            ui_probe: std::env::var_os("SMP_UI_STATE").map(PathBuf::from),
            ui_published: None,
            last_error: startup_errors.first().cloned(),
            pending_errors: startup_errors,
            pending_infos: Vec::new(),
            pending_autoplay: autoplay,
            drop_activity: Rc::new(Cell::new(None)),
            drop_hint_shown: false,
            status_pill: None,
            status_pill_shown: false,
        };

        // 客户区铺满整窗：系统那圈缩放边框（左右各 7 逻辑像素）会让「画面铺外框、
        // 标题带只能铺客户区」对不上，看就是「顶栏没铺满宽度」（见 window_actions）。
        // 和 `fit_to_video` 一样必须放到前台任务里：`SetWindowPos` 会同步回调
        // `WM_SIZE` → 重新布局，而此刻 App 正被借用。
        if main_hwnd != 0 {
            let main = main_hwnd;
            cx.spawn_in(window, async move |this, cx| {
                window_actions::make_client_full_bleed(main);
                // 客户区刚变大了一圈，「外框-客户」那份边框厚度已经作废：把
                // 一次性的 fit 标记清掉，让下一个 tick 按真实几何重新定一次窗口尺寸
                // （否则第一支视频会短一条边框的高度，底下多出一条黑边）。
                let _ = this.update(cx, |view, cx| {
                    view.fit_done = None;
                    cx.notify();
                });
            })
            .detach();
        }

        // 快捷键（全屏的唯一键盘出口）。
        //
        // 为什么不用 `cx.bind_keys` + `on_action`：没有焦点元素时 GPUI 把按键派发给
        // dispatch tree 的**根**，而那一位是 gpui-component 的 `Root`（在我们视图之外），
        // 挂在自己根节点上的 `on_action` 永远收不到（实测 `dispatch_keystroke` 返回
        // false、影院状态不动）。App 级的 keystroke 拦截跑在派发**之前**，没有这个前提。
        let weak = cx.entity().downgrade();
        this.subscriptions
            .push(cx.intercept_keystrokes(move |event, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                // 按键也算"用户还在"：控制层淡掉之后按一下方向键，它得亮回来再动
                let _ = weak.update(cx, |view, _| view.activity = Instant::now());
                let handled = match event.keystroke.key.as_str() {
                    "escape" => {
                        // 只有真的在全屏时才吃这个键——否则会把将来的对话框/弹层的 Esc 挡掉
                        let in_cinema = weak.read_with(cx, |view, _| view.cinema).unwrap_or(false);
                        if in_cinema {
                            let _ = weak.update(cx, |view, cx| view.exit_cinema(window, cx));
                            true
                        } else {
                            false
                        }
                    }
                    "f11" => {
                        let video = weak
                            .read_with(cx, |view, _| view.video_active)
                            .unwrap_or(false);
                        if video {
                            let _ = weak.update(cx, |view, cx| view.toggle_cinema(window, cx));
                            true
                        } else {
                            false
                        }
                    }
                    // 方向键：左右 ±5 秒、上下 ±5% 音量。
                    //
                    // 为什么不在走带/音量的 Slider 上挂 on_key_down：gpui-component 0.7.3
                    // 的 `Slider` 没有 `FocusHandle`、也不参与键盘（上游硬限制），
                    // 焦点永远落不到它身上，键只能在这一层接。
                    key => match ArrowKey::parse(key) {
                        Some(arrow) => {
                            let ready = weak
                                .read_with(cx, |view, _| {
                                    !arrow.needs_track() || view.current.is_some()
                                })
                                .unwrap_or(false);
                            if ready {
                                let _ = weak
                                    .update(cx, |view, cx| view.handle_arrow(arrow, window, cx));
                            }
                            // 没有曲目时不吞左右键：将来把方向键留给列表导航还来得及。
                            ready
                        }
                        None => false,
                    },
                };
                if handled {
                    cx.stop_propagation();
                }
            }));

        // 100ms 轮询：进度同步 + 自动连播 + 控制层淡出 + 窗口尺寸节流落盘
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(TICK_INTERVAL).await;
            let Ok(()) = this.update_in(cx, |view, window, cx| view.tick(window, cx)) else {
                break;
            };
        })
        .detach();

        this
    }

    // ------------------------------------------------------------------
    // 引擎状态同步
    // ------------------------------------------------------------------

    /// 记录一条用户可见错误：留档到 `last_error`（测试与状态栏可读），
    /// 并排队等下一次 tick 统一弹 toast（tick 里有 window 可用）。
    fn report_error(&mut self, message: impl Into<SharedString>) {
        let message = message.into();
        // release 版是 GUI 子系统（双击不再有控制台），所以诊断信息不能只靠
        // eprintln!，必须同时落进 session.log，否则用户报障时手上什么都没有。
        eprintln!("{message}");
        crate::persistence::log_session(&format!("error: {message}"));
        self.last_error = Some(message.clone());
        self.pending_errors.push(message);
    }

    /// 记录一条普通提示（导入结果等），随下一次 tick 弹出。
    fn report_info(&mut self, message: impl Into<SharedString>) {
        self.pending_infos.push(message.into());
    }

    /// 把排队的提示弹出来。
    ///
    /// 两种呈现方式，取决于**底色**：音频模式用右上角 toast（gpui-component 的
    /// 通知层，位置固定、会自己排队）；视频模式整块内容区都是画面，toast 落在
    /// 画面中间会挡住内容，所以改成标题带里的小胶囊（见 `status_pill`）。
    fn flush_errors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let to_pill = self.video_active;
        for message in std::mem::take(&mut self.pending_errors) {
            if to_pill {
                self.status_pill = Some((message, Instant::now(), true));
            } else {
                window.push_notification(Notification::error(message), cx);
            }
        }
        for message in std::mem::take(&mut self.pending_infos) {
            if to_pill {
                self.status_pill = Some((message, Instant::now(), false));
            } else {
                window.push_notification(Notification::info(message), cx);
            }
        }
    }

    /// 视频模式下的状态胶囊是否还在有效期内。
    fn status_pill_live(&self) -> Option<&(SharedString, Instant, bool)> {
        if !self.video_active {
            return None;
        }
        self.status_pill
            .as_ref()
            .filter(|(_, shown_at, _)| shown_at.elapsed() < STATUS_PILL_DURATION)
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_errors(window, cx);

        // 退出视频时立起的标记：贴图要窗口才能还（见 `leave_video`）
        if std::mem::take(&mut self.surface_clear) {
            self.video.surface.clear(window);
            cx.notify();
        }

        // 命令行「打开方式」：等第一帧渲染完再起播，构造期间不发通知
        if let Some(index) = self.pending_autoplay.take() {
            self.play_track(index, cx);
        }

        // 拖放提示：`drag_over` 的样式回调拿不到 cx，只能留时间戳。
        // 250ms 内还有时间戳 = 仍在拖；没有 = 拖走了，提示自己熄灭。
        let dragging = self
            .drop_activity
            .get()
            .is_some_and(|stamp| stamp.elapsed() < DROP_HINT_IDLE);
        if dragging != self.drop_hint_shown {
            self.drop_hint_shown = dragging;
            cx.notify();
        }

        // 状态胶囊：出现和到点消失都只能由 tick 发现（没有别的事件会碰它），
        // 所以这里统一比对「这帧该不该画」，变了就重绘一次。
        let pill_live = self.status_pill_live().is_some();
        if pill_live != self.status_pill_shown {
            self.status_pill_shown = pill_live;
            cx.notify();
        }

        // 控制层淡出/淡入：必须在 `sync_video_chrome` 之前——探针要看到新的 `chrome_alpha`。
        self.tick_chrome_fade(cx);

        // 帧泵：引擎起播时只存下了接收端（`start_video` 手上没有 window），
        // 在这里把「有新帧」接成界面上的画面。
        self.ensure_frame_pump(window, cx);

        // L2 画面染色：色相变了才让胶囊重画（判据在 `apply_frame_tint`）。
        self.apply_frame_tint(cx);

        // 最大化状态：用户可能用系统的双击标题栏 / Aero Snap / 右键菜单改变它，
        // 这些都不经过我们的按钮，所以每帧比对一次，变了才重绘（图标要换成「还原」）。
        // 刚点过按钮的 300ms 内跳过——`set_zoomed` 是异步的，此刻还没生效。
        const ZOOM_SETTLE: Duration = Duration::from_millis(300);
        let pending = self
            .zoom_pending
            .is_some_and(|at| at.elapsed() < ZOOM_SETTLE);
        if !pending {
            self.zoom_pending = None;
            let maximized = window_actions::is_zoomed(self.main_hwnd);
            if maximized != self.maximized {
                self.maximized = maximized;
                cx.notify();
            }
        }

        // 影院模式只对视频成立：自动连播切到音频（或无视频可播）时必须退出全屏，
        // 否则影院按钮消失、用户被困在全屏里。
        if self.cinema && !self.video_active {
            self.set_cinema(false, window, cx);
        }

        let video_status = if self.video_active {
            self.video.engine.as_ref().map(|e| e.status())
        } else {
            None
        };

        if let Some(status) = video_status {
            self.tick_video(status, window, cx);
        } else if let Some(engine) = &mut self.engine {
            if engine.track_finished() {
                self.advance_after_end(cx);
            } else if !self.user_seeking {
                let pos = engine.position_secs();
                self.seek.update(cx, |state, cx| {
                    state.set_value(pos as f32, window, cx);
                });
            }
        }

        // 视频模式：圆角属性 + 验收探针
        self.sync_video_chrome(window);

        // 客户区自愈：进出全屏后系统不重算客户区（实测退全屏后窗口已是 1280×731、
        // 客户区还停在 3840×2010），界面会整块画在旧尺寸上。每帧比一次，不一致就补
        // 一次 `SWP_FRAMECHANGED`；两次补之间留 300ms，别把它变成死循环。
        // 和 z 序自愈（`ensure_below`）同一个套路：只查、不一致才动。
        const CLIENT_KICK_COOLDOWN: Duration = Duration::from_millis(300);
        if !window_actions::client_matches_window(self.main_hwnd) {
            let due = self
                .client_kick
                .is_none_or(|at| at.elapsed() >= CLIENT_KICK_COOLDOWN);
            if due {
                self.client_kick = Some(Instant::now());
                let main = self.main_hwnd;
                // 同样要放到前台任务里：`SetWindowPos` 会同步回调 `WM_SIZE` → 重新布局
                cx.spawn_in(window, async move |_this, _cx| {
                    window_actions::refresh_client_area(main);
                })
                .detach();
            }
        }

        // 布局自愈：进出全屏那一下 GPUI 的布局会停在旧尺寸（画面已经全屏，顶栏却
        // 只画在左上角一小块里、右上角的窗口按钮根本不存在）。
        //
        // 判据有两条，缺一不可：
        // - `viewport ≠ bounds`：布局用的和窗口说的是两回事（普通改尺寸走这条）；
        // - `bounds ≠ 真实客户区`：**进出全屏走的是这条**。GPUI 漏掉那一次尺寸通知
        //   之后 `logical_size` 根本没更新，`bounds()` 和 `viewport` 一起停在旧值，
        //   只比它俩永远发现不了。真实值只能问系统（`GetClientRect`）。
        //
        // 注意：**不能**在这里调 `window.bounds_changed(cx)`——它会把 `bounds_observers`
        // 全跑一遍，其中就有对本视图的 `update`，而此刻视图正在被 update，直接 panic
        // （`cannot update PlayerView while it is already being updated`）。
        // `refresh()` 只标脏、不碰 viewport，对这个病也治不好（实测重绘循环里
        // viewport 一直是旧值）。真正有效的办法是给窗口补一条 `WM_SIZE`：GPUI 的
        // 尺寸处理是**幂等**的（只有 lparam），补完布局就按新尺寸重排。
        const LAYOUT_KICK_COOLDOWN: Duration = Duration::from_millis(300);
        let viewport = window.viewport_size();
        let bounds = window.bounds().size;
        let scale = window.scale_factor();
        let physical = window_actions::client_size(self.main_hwnd);
        let viewport_stale = (viewport.width.as_f32() - bounds.width.as_f32()).abs() > 0.5
            || (viewport.height.as_f32() - bounds.height.as_f32()).abs() > 0.5;
        let bounds_stale = physical.0 > 0
            && ((bounds.width.as_f32() * scale - physical.0 as f32).abs() > 1.5
                || (bounds.height.as_f32() * scale - physical.1 as f32).abs() > 1.5);
        if viewport_stale || bounds_stale {
            let due = self
                .layout_kick
                .is_none_or(|at| at.elapsed() >= LAYOUT_KICK_COOLDOWN);
            if due {
                self.layout_kick = Some(Instant::now());
                persistence::log_session(&format!(
                    "layout stale: viewport={:.0}x{:.0} window={:.0}x{:.0} client={}x{} -> kick",
                    viewport.width.as_f32(),
                    viewport.height.as_f32(),
                    bounds.width.as_f32(),
                    bounds.height.as_f32(),
                    physical.0,
                    physical.1,
                ));
                let main = self.main_hwnd;
                cx.spawn_in(window, async move |_this, _cx| {
                    window_actions::kick_layout(main);
                })
                .detach();
            }
        }

        // 窗口尺寸节流落盘（变化后 ~5s 保存一次）
        // 影院模式下 bounds 是全屏尺寸、最大化时是最大化尺寸，都不能当窗口尺寸存
        // （否则下次启动会开成全屏大小 / 贴着屏幕边）。视频模式也不存：那份尺寸是
        // 按画面算出来的，存下来会让"音频模式"的默认窗口跟着某部片子的比例走。
        let bounds = window.bounds();
        let size = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
        if self.cinema || self.maximized || self.video_active || size == self.last_saved_size {
            self.window_dirty_ticks = 0;
        } else {
            self.window_dirty_ticks += 1;
            if self.window_dirty_ticks >= WINDOW_SAVE_TICKS {
                self.settings.window_width = size.0;
                self.settings.window_height = size.1;
                persistence::save_settings(&self.settings);
                self.last_saved_size = size;
                self.window_dirty_ticks = 0;
            }
        }
    }

    /// 视频模式下的状态同步（mpv 侧）。
    fn tick_video(&mut self, status: VideoStatus, window: &mut Window, cx: &mut Context<Self>) {
        // 加载/解码失败：给出可见提示并推进到下一首，避免卡在错误状态
        if let Some(reason) = self.video.engine.as_ref().and_then(|e| e.take_error()) {
            let track = self.current_track(cx);
            let title = track
                .as_ref()
                .map(|track| track.title.clone())
                .unwrap_or_else(|| "当前文件".to_string());
            // 区分「文件不在了」和「文件在但 mpv 解不了」——两种提示的处理方式完全不同
            let missing = track
                .as_ref()
                .is_some_and(|track| !Path::new(&track.path).exists());
            let message = if missing {
                format!("无法播放「{title}」：文件不存在或已被移动")
            } else {
                format!("无法播放「{title}」：mpv 无法加载该文件（{reason}）")
            };
            self.report_error(message);

            let count = self.playlist.read(cx).delegate().track_count();
            match self.current {
                Some(i) if i + 1 < count => self.play_track(i + 1, cx),
                _ => {
                    if let Some(engine) = &self.video.engine {
                        engine.stop();
                    }
                    self.video_active = false;
                    self.current = None;
                    self.surface_clear = true;
                    cx.notify();
                }
            }
            return;
        }

        // 切轨回执：`cycle` 是单向命令、没有返回值，"观测值没变"就等于没有别的轨。
        if let Some((kind, before, at)) = self.pending_track {
            let now = match kind {
                TrackKind::Subtitle => status.sub_track,
                TrackKind::Audio => status.audio_track,
            };
            if now != before {
                self.pending_track = None;
                self.report_info(if now == 0 {
                    format!("{}已关闭", kind.label())
                } else {
                    format!("{}轨 {now}", kind.label())
                });
            } else if at.elapsed() >= TRACK_ACK_TIMEOUT {
                self.pending_track = None;
                self.report_info(format!("没有其他{}轨可切", kind.label()));
            }
        }

        // 时长补齐：元数据缺失时以 mpv 报告的时长重建量程
        if status.duration > 0.5 && (status.duration as f32 - self.seek_max).abs() > 1.0 {
            self.rebuild_seek(status.duration as f32, cx);
        }
        // 视频入列时问不到时长（mpv 要起播才报），补进轨道元数据：页脚
        // 「N 个文件 · 总时长」和行内第二行都读它，不补就一直是空的。
        if status.duration > 0.5 {
            if let Some(row) = self.current {
                self.backfill_track_duration(row, status.duration, cx);
            }
        }

        // 窗体大小 = 内容大小：画面尺寸一报到就把窗口调成同样的尺寸/比例
        self.fit_to_video_source(status.width, status.height, window, cx);

        if status.finished {
            let count = self.playlist.read(cx).delegate().track_count();
            // 单曲循环：同一部片子重新起播（`start_video` 会重新 load，不是空操作）
            if self.settings.repeat == RepeatMode::One && count > 0 {
                if let Some(i) = self.current {
                    self.play_track(i, cx);
                    return;
                }
            }
            if count > 0 && (self.settings.repeat.is_on() || self.settings.shuffle) {
                self.skip(1, cx);
                return;
            }
            match self.current {
                Some(i) if i + 1 < count => self.play_track(i + 1, cx),
                _ => {
                    if let Some(engine) = &self.video.engine {
                        engine.stop();
                    }
                    self.video_active = false;
                    self.surface_clear = true;
                    cx.notify();
                }
            }
            return;
        }

        if !status.connected && !status.idle {
            self.video_error = Some("视频内核连接中断（mpv 已退出）".into());
            self.report_error("视频中断：mpv 进程已退出");
            self.video_active = false;
        }

        if !self.user_seeking {
            self.seek.update(cx, |state, cx| {
                state.set_value(status.position as f32, window, cx);
            });
        }
    }

    /// 把窗口调成画面的尺寸与比例（窗体大小 = 内容大小）。
    ///
    /// 每部片子只调一次：mpv 要解码出第一帧才知道 `dwidth/dheight`，而那个值
    /// 还会先报一份中间值——所以等它连着两帧不变才动手（见 `fit_seen`）；调完
    /// 之后用户自己拉伸窗口，不能再弹回去。
    ///
    /// 最大化 / 影院模式下不调——那两种状态的尺寸归系统管，按画面去改等于把用户
    /// 刚放大的窗口打回原形。等退出这两种状态，`fit_done` 还是 `None`，下一帧
    /// 自然补上。
    fn fit_to_video_source(
        &mut self,
        width: u32,
        height: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if width < 2 || height < 2 || self.main_hwnd == 0 {
            return;
        }
        if self.fit_done == Some((width, height)) {
            return;
        }
        // 先确认这个尺寸是**稳定**的：同一个值连着报两次才当真。mpv 起播时会先给
        // 一份中间值（1280×720 的片子报 960×540），按它调窗口会白跳一下。
        let repeats = match self.fit_seen {
            Some((w, h, seen)) if (w, h) == (width, height) => seen.saturating_add(1),
            _ => 0,
        };
        self.fit_seen = Some((width, height, repeats));
        if repeats == 0 {
            return;
        }
        if self.maximized || self.cinema || window.is_fullscreen() {
            return;
        }
        self.fit_done = Some((width, height));

        let main = self.main_hwnd;
        // 这条命令必须放到下一轮消息循环里执行：`SetWindowPos` 会**同步**回调
        // GPUI 的 `WM_SIZE` → 重新布局，而此刻 App 正被本轮更新借用，回调拿不到
        // 借用权，布局就停在旧尺寸上（`window_actions` 模块头记着同一个坑）。
        // 前台任务由平台投递到消息队列后运行，和 GPUI 自己的 `Window::resize`
        // 是同一套路。
        cx.spawn_in(window, async move |_this, _cx| {
            window_actions::fit_to_video(main, (width, height));
        })
        .detach();
    }

    /// 视频模式的窗口属性同步：圆角 + 验收探针。
    ///
    /// 上一代这里还要「让叠加窗跟上视频区、压在主窗正下方、把画面从主窗区域里减掉」，
    /// 因为画面是另一块窗口。现在画面就在自己的渲染树里，只剩下两件事：
    /// 视频时给整窗加圆角（画面贴到窗口边缘，方角会割裂），以及把界面状态写给验收脚本。
    fn sync_video_chrome(&mut self, window: &Window) {
        // 只在状态变化时调——改窗口属性会触发一次重绘，别每帧都打。
        if self.corners_rounded != self.video_active {
            self.corners_rounded = self.video_active;
            window_actions::set_rounded(self.main_hwnd, self.video_active);
        }
        self.publish_ui_state(window);
    }

    /// 把「界面认为自己处在什么状态」写到 `SMP_UI_STATE`。
    ///
    /// 没有区域可读之后，验收脚本要判断「胶囊在不在」「面板开没开」只能问界面自己。
    /// 内容不变就不写盘（`chrome_alpha` 只在淡入淡出的那 4 帧里变，其余时候是常数）。
    fn publish_ui_state(&mut self, window: &Window) {
        let Some(path) = self.ui_probe.clone() else {
            return;
        };
        let bounds = window.bounds();
        let state = UiState {
            video: self.video_active,
            chrome_alpha: self.chrome_alpha,
            panel_open: self.playlist_open,
            info_open: self.info_open,
            cinema: self.cinema,
            window: (
                bounds.size.width.as_f32(),
                bounds.size.height.as_f32(),
            ),
            stage: self.video_rect.get().map(|(x, y, w, h)| UiRect::new(x, y, w, h)),
            frame: self.video.surface.size(),
            layout: self.chrome_layout(window),
        };
        let json = state.to_json();
        if self.ui_published.as_deref() == Some(json.as_str()) {
            return;
        }
        state.publish(&path);
        self.ui_published = Some(json);
    }

    /// 视频模式下压在画面上的那几块 chrome（逻辑像素，已含抗锯齿留白）。
    ///
    /// 它已经从渲染管线上下来了（画面在自己手里，不需要给任何窗口「打洞」），
    /// 但仍然是**唯一一处**把「标题带 / 遮罩 / 胶囊 / 信息卡 / 歌单」的位置写成
    /// 数据的地方：单元测试靠它断言几何，验收脚本靠它算点击坐标。
    fn chrome_layout(&self, window: &Window) -> ChromeLayout {
        let bounds = window.bounds();
        let width = bounds.size.width.as_f32();
        let height = bounds.size.height.as_f32();

        // 抗锯齿留白：切得太贴边会把圆角那圈 1px 描边切掉半个像素
        const PAD: f32 = 1.5;

        let visible = self.chrome_alpha > CHROME_HIDDEN_EPS;
        let mut layout = ChromeLayout {
            visible,
            width,
            height,
            ..Default::default()
        };
        if !visible {
            return layout;
        }

        // 标题带整条：右端还有窗口按钮，中间截断会很奇怪
        layout.title_band = Some(UiRect::new(0., 0., width, TITLE_BAND));

        // 底部渐隐遮罩：整条压在画面上
        if self.video_active {
            layout.vignette = Some(UiRect::new(
                0.,
                (height - VIGNETTE_HEIGHT).max(0.),
                width,
                VIGNETTE_HEIGHT.min(height),
            ));
        }

        if let Some((x, y, w, h)) = self.capsule_rect.get() {
            layout.capsule = Some(
                UiRect::rounded(x, y, w, h, CAPSULE_RADIUS).inflate(PAD),
            );
        }

        if self.info_open && self.video_active {
            if let Some((x, y, w, h)) = self.info_rect.get() {
                layout.info_card = Some(UiRect::rounded(x, y, w, h, 12.).inflate(PAD));
            }
        }

        // 歌单：展开时浮在画面右侧
        //
        // 这里的四个数必须和 `render_playlist_panel` 里的定位**逐像素一致**
        // （top/right/bottom/width），否则脚本按这份数据算出来的点击会落在面板外。
        if self.playlist_open && !self.cinema {
            let panel_x = width - PLAYLIST_INSET - PLAYLIST_WIDTH;
            let panel_y = TITLE_BAND + PLAYLIST_TOP_GAP;
            let panel_h = (height - panel_y - PLAYLIST_BOTTOM).max(0.);
            layout.panel = Some(
                UiRect::rounded(panel_x, panel_y, PLAYLIST_WIDTH, panel_h, CAPSULE_RADIUS)
                    .inflate(PAD),
            );
        }
        layout
    }

    /// 把 mpv 报的时长补进轨道元数据（同时落盘）。
    ///
    /// 音频入列时就能问到时长，视频不能——mpv 要起播之后才报 `duration`，所以视频
    /// 轨的 `duration_secs` 一直是 `None`：页脚「N 个文件 · 总时长」和行内第二行
    /// 就永远空着（截图里只有「1 个文件」）。拿到就补一次，`set_duration` 自己会
    /// 挡住重复写，所以每帧调用也不会反复落盘。
    fn backfill_track_duration(&mut self, row: usize, secs: f64, cx: &mut Context<Self>) {
        let wrote = self.playlist.update(cx, |state, _cx| {
            state.delegate_mut().set_duration(row, secs)
        });
        if wrote {
            let tracks = self.playlist.read(cx).delegate().tracks().to_vec();
            persistence::save_playlist(&tracks, self.current);
        }
    }

    /// 重建进度条量程（SliderState 不支持运行时改 max）。
    fn rebuild_seek(&mut self, max: f32, cx: &mut Context<Self>) {
        let max = max.max(1.0);
        self.seek_max = max;
        let seek = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(max)
                .default_value(0.0)
                .step(1.0)
        });
        let sub = cx.subscribe(
            &seek,
            |this, _slider, event: &SliderEvent, cx| match event {
                SliderEvent::Change(_) => this.user_seeking = true,
                SliderEvent::Release(value) => {
                    this.user_seeking = false;
                    this.seek_to(value.start() as f64);
                    cx.notify();
                }
            },
        );
        if let Some(slot) = self.subscriptions.get_mut(SUB_SEEK) {
            *slot = sub;
        } else {
            self.subscriptions.push(sub);
        }
        self.seek = seek;
    }

    // ------------------------------------------------------------------
    // 播放控制
    // ------------------------------------------------------------------

    fn current_track(&self, cx: &Context<Self>) -> Option<TrackMeta> {
        self.current
            .and_then(|i| self.playlist.read(cx).delegate().track(i).cloned())
    }

    fn playback_state(&self) -> PlaybackState {
        if self.video_active {
            match &self.video.engine {
                Some(engine) => {
                    let status = engine.status();
                    if status.paused || status.idle || status.finished {
                        PlaybackState::Paused
                    } else {
                        PlaybackState::Playing
                    }
                }
                None => PlaybackState::Stopped,
            }
        } else {
            self.engine
                .as_ref()
                .map(|e| e.state())
                .unwrap_or(PlaybackState::Stopped)
        }
    }

    fn play_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let track = self.playlist.read(cx).delegate().track(index).cloned();
        let Some(track) = track else { return };
        let want_video = is_video_file(Path::new(&track.path));

        // 开播这一刻算一次输入：新片子的前 2.5 秒控制层必须是亮着的，
        // 否则刚打开视频就是一块"什么都没有"的画面（用户还没碰过任何输入）。
        self.activity = Instant::now();
        self.tint_applied = None;

        if want_video {
            // 音频引擎让位（mpv 负责音视频）
            if let Some(engine) = &mut self.engine {
                engine.stop();
            }
            self.current = Some(index);
            self.start_video(&track, cx);
        } else {
            self.leave_video();
            self.start_audio(index, &track, cx);
        }

        self.playlist.update(cx, |state, cx| {
            state.delegate_mut().set_confirmed(self.current);
            cx.notify();
        });
        self.persist_playlist(cx);
        cx.notify();
    }

    fn start_audio(&mut self, index: usize, track: &TrackMeta, cx: &mut Context<Self>) {
        if let Some(engine) = &mut self.engine {
            match engine.play(&track.path) {
                Ok(()) => {
                    self.current = Some(index);
                    let max = track.duration_secs.unwrap_or(SEEK_FALLBACK_MAX as f64) as f32;
                    self.rebuild_seek(max, cx);
                }
                Err(err) => {
                    self.report_error(format!("无法播放「{}」：{err}", track.title));
                    self.current = None;
                }
            }
        } else {
            // 无引擎（无音频设备）：仅更新高亮，便于 UI 验证，但要让用户知道没声音
            self.current = Some(index);
            self.report_error("音频设备不可用，无法播放音乐（视频不受影响）");
        }
    }

    /// 进入视频模式：确保 mpv 引擎与软件渲染就绪，然后加载文件。
    fn start_video(&mut self, track: &TrackMeta, cx: &mut Context<Self>) {
        self.video_error = None;
        self.video_active = true;
        // 换片了：等新片子的尺寸出来再调窗口（旧尺寸不能沿用）
        self.fit_done = None;
        self.fit_seen = None;

        // 复用进程；连接已断开则重建
        let need_spawn = match &self.video.engine {
            Some(engine) => !engine.is_connected(),
            None => true,
        };
        if need_spawn {
            // 进程内 libmpv：只找 DLL，不再起子进程
            let Some(libmpv) = find_libmpv(None) else {
                self.video_active = false;
                self.video_error =
                    Some("未找到 libmpv-2.dll（可用环境变量 SMP_LIBMPV_PATH 指定路径）".into());
                self.report_error(
                    "视频不可用：未找到 libmpv-2.dll（设置 SMP_LIBMPV_PATH 可指定路径）",
                );
                return;
            };
            // `wid: None` = 不要 `--wid` 叠加窗，改走软件渲染（见 `video::spawn`）
            match VideoEngine::spawn(SpawnOptions {
                libmpv,
                wid: None,
                volume: self.settings.volume,
                extra_args: Vec::new(),
            }) {
                Ok(engine) => self.video.engine = Some(engine),
                Err(err) => {
                    self.video_active = false;
                    self.video_error = Some(format!("libmpv 初始化失败：{err}"));
                    self.report_error(format!("视频不可用：libmpv 初始化失败（{err}）"));
                    return;
                }
            }
        }

        if let Some(engine) = &mut self.video.engine {
            // 软件渲染上下文：一个引擎开一次。换片复用引擎时不能重复创建
            // （`mpv_render_context_create` 会返回 `property unavailable`）。
            if !engine.has_render() {
                match engine.enable_render() {
                    Ok(()) => {}
                    Err(err) => {
                        self.video_error = Some(format!("视频渲染不可用：{err}"));
                        self.report_error(format!("视频不可用：软件渲染初始化失败（{err}）"));
                        self.video_active = false;
                        return;
                    }
                }
            }
            // 「有新帧」回调：mpv 在**自己的线程**上调，绝不能碰 mpv API，只往通道
            // 里丢一个信号；容量 1 + `try_send` 天然合并，解码快于出帧时也不会堆积。
            let (tx, rx) = async_channel::bounded::<()>(1);
            engine.set_frame_callback(Box::new(move || {
                let _ = tx.try_send(());
            }));
            self.video.frames = Some(rx);

            engine.set_volume(self.settings.volume);
            engine.load(&track.path);
        }
        let max = track.duration_secs.unwrap_or(SEEK_FALLBACK_MAX as f64) as f32;
        self.rebuild_seek(max, cx);
    }

    /// 退出视频模式（停止 mpv、把画面从舞台上撤下来）。
    ///
    /// 贴图不能在这里还：`Window::drop_image` 要窗口，而这个函数是从 `play_track`
    /// 那类没有 window 的路径上来的。所以只立一个标记，下一次 tick 统一还。
    fn leave_video(&mut self) {
        if let Some(engine) = &self.video.engine {
            engine.stop();
        }
        self.video_active = false;
        self.video_error = None;
        self.surface_clear = true;
        self.tint_applied = None;
    }

    fn shutdown_video(&mut self) {
        self.video_active = false;
        self.surface_clear = true;
        if let Some(engine) = &mut self.video.engine {
            engine.shutdown();
        }
        self.video.engine = None;
        self.video.frames = None;
    }

    /// 把「有新帧」信号接成界面上的画面。
    ///
    /// 接收端用一次就没了：换片时 `start_video` 会重新建一条通道并重设回调，
    /// 旧通道的发送端随回调被替换而释放，旧泵 `recv()` 拿到 `Err` 自己退出。
    fn ensure_frame_pump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(frames) = self.video.frames.take() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            while frames.recv().await.is_ok() {
                // 合并：解码可能比 UI 快，队列里堆的信号一次抽干，只画最新那一帧
                while frames.try_recv().is_ok() {}
                let Ok(()) = this.update_in(cx, |view, window, cx| view.pump_frame(window, cx))
                else {
                    break;
                };
            }
        })
        .detach();
    }

    /// 渲染并收下一帧。
    ///
    /// 渲染尺寸由 `fit_render_size` 定：按舞台尺寸做 contain、只缩不放、封顶
    /// 2560×1440。放大交给 GPU 的双线性插值——1080p 的片子在小窗口里缩小是无损的，
    /// 而 4K 放进 720p 窗口时按舞台尺寸渲染，解码器不用出 4K 帧。
    fn pump_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.video_active {
            return;
        }
        let native = self.video.engine.as_ref().and_then(|engine| {
            let status = engine.status();
            (status.width > 0 && status.height > 0).then_some((status.width, status.height))
        });
        let bounds = window.bounds().size;
        let (width, height) = fit_render_size(
            bounds.width.as_f32(),
            bounds.height.as_f32(),
            native,
            window.scale_factor(),
        );

        // 拆开借用：`render_frame` 拿 `&mut engine` 的同时 `surface` 要可变
        let VideoRuntime { engine, surface, .. } = &mut self.video;
        let Some(engine) = engine.as_mut() else {
            return;
        };
        let Some(frame) = engine.render_frame(width, height) else {
            return;
        };
        trace_frame(bounds.width.as_f32(), bounds.height.as_f32(), native, (frame.width, frame.height), window.scale_factor());
        surface.accept(frame, window);
        cx.notify();
    }

    /// L2 染色：画面底部那块的色相变了才让胶囊重画。
    ///
    /// 逐帧直接套用色相会让玻璃跟着画面闪（一个镜头里每帧的均值都在小范围抖），
    /// 所以设一个 8° 的迟滞：只有真的换了配色才更新。
    fn apply_frame_tint(&mut self, cx: &mut Context<Self>) {
        if !self.video_active {
            if self.tint_applied.take().is_some() {
                cx.notify();
            }
            return;
        }
        let Some(hue) = self.video.surface.tint() else {
            return;
        };
        let changed = self
            .tint_applied
            .is_none_or(|old| hue_distance(old, hue) > TINT_HYSTERESIS);
        if changed {
            self.tint_applied = Some(hue);
            cx.notify();
        }
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if self.video_active {
            let Some(engine) = &self.video.engine else {
                return;
            };
            let status = engine.status();
            if status.paused {
                engine.resume();
            } else if status.idle || status.finished {
                if let Some(index) = self.current {
                    self.play_track(index, cx);
                    return;
                }
            } else {
                engine.pause();
            }
            cx.notify();
            return;
        }

        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        match engine.state() {
            PlaybackState::Playing => {
                engine.pause();
                cx.notify();
            }
            PlaybackState::Paused => {
                engine.resume();
                cx.notify();
            }
            PlaybackState::Stopped => {
                let target = match self.current {
                    Some(i) => i,
                    None => {
                        if self.playlist.read(cx).delegate().track_count() == 0 {
                            return;
                        }
                        0
                    }
                };
                self.play_track(target, cx);
            }
        }
    }

    /// 上一首/下一首。
    ///
    /// 三种模式都在这里分叉（面板底部的「随机 / 循环」真正生效的地方就是这里）：
    /// - **随机**：从「除当前这一首」里抽一个，抽不到（只有一首）就不动；
    /// - **单曲循环**：手动按「下一首」仍然要换歌，否则按了没反应——单曲只影响
    ///   自动连播（`tick` 里播完那一下）；
    /// - **循环关闭**：到头就停住（不换、不回到第一首）。
    fn skip(&mut self, delta: i32, cx: &mut Context<Self>) {
        let count = self.playlist.read(cx).delegate().track_count();
        if count == 0 {
            return;
        }
        if self.settings.shuffle && count > 1 {
            let cur = self.current.unwrap_or(0) % count;
            // 用纳秒做种子：`rand` 不在依赖里，而这里只需要「别总是同一首」
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as usize)
                .unwrap_or(0);
            let next = (cur + 1 + now % (count - 1)) % count;
            self.play_track(next, cx);
            return;
        }
        let idx = match self.current {
            Some(i) => {
                let raw = i as i32 + delta;
                let last = count as i32 - 1;
                if raw < 0 || raw > last {
                    // 越界只可能是「循环关闭」，否则绕到另一端
                    if !self.settings.repeat.is_on() {
                        return;
                    }
                    raw.rem_euclid(count as i32) as usize
                } else {
                    raw as usize
                }
            }
            None => 0,
        };
        self.play_track(idx, cx);
    }

    /// 一曲播完之后做什么：单曲循环 / 列表循环 / 随机 / 停止。
    ///
    /// 和 `skip()` 分开是因为语义不同：手动按「下一首」时**单曲循环也要换歌**
    /// （否则按了没反应），只有自动连播才把「单曲」当回事。
    fn advance_after_end(&mut self, cx: &mut Context<Self>) {
        let count = self.playlist.read(cx).delegate().track_count();
        if count == 0 {
            return;
        }
        if self.settings.repeat == RepeatMode::One {
            if let Some(i) = self.current {
                self.play_track(i, cx);
            }
            return;
        }
        if self.settings.repeat.is_on() || self.settings.shuffle {
            self.skip(1, cx);
            return;
        }
        match self.current {
            Some(i) if i + 1 < count => self.play_track(i + 1, cx),
            _ => {
                if let Some(engine) = &mut self.engine {
                    engine.stop();
                }
                cx.notify();
            }
        }
    }

    /// 进度跳转：按当前模式路由到音频/视频引擎。
    fn seek_to(&mut self, secs: f64) {
        if self.video_active {
            if let Some(engine) = &self.video.engine {
                engine.seek(secs);
            }
            // 跳完画面全变了：把染色的迟滞重置，新镜头的色相立刻生效
            self.tint_applied = None;
        } else if let Some(engine) = &mut self.engine {
            engine.seek(secs);
        }
    }

    /// 相对跳转（±10 秒按钮 / 方向键）：夹到 [0, 时长] 之间，避免越界后播放器自己回弹。
    ///
    /// 播到末尾之后引擎已经 `Stopped`，`seek` 在那时是空操作——必须先重新起播，
    /// 否则用户越到结尾再按「后退」就是「按了没反应」（方向键会被连续按，这种死区
    /// 尤其刺眼）。
    fn seek_rel(&mut self, delta: f64, cx: &mut Context<Self>) {
        if self.current.is_none() {
            return;
        }
        let position = if self.video_active {
            self.video
                .engine
                .as_ref()
                .map(|e| e.status().position)
                .unwrap_or(0.0)
        } else {
            self.engine
                .as_ref()
                .map(|e| e.position_secs())
                .unwrap_or(0.0)
        };
        let target = seek_target(position, delta, self.seek_max as f64);

        // 音频播完（state = Stopped）：从目标位置重新起播。
        let stopped = !self.video_active
            && self
                .engine
                .as_ref()
                .is_some_and(|e| e.state() == PlaybackState::Stopped);
        if stopped {
            let track = self
                .current
                .and_then(|index| self.playlist.read(cx).delegate().track(index).cloned());
            // 视频播完时 tick 会 `stop()` + 退出视频模式，这一刻用音频引擎重起一个 mp4
            // 只会解码失败、弹一条假错误。视频的「播完再往回跳」不在这一版里做。
            if track
                .as_ref()
                .is_some_and(|track| is_video_file(Path::new(&track.path)))
            {
                return;
            }
            if let (Some(track), Some(engine)) = (track, &mut self.engine) {
                if let Err(err) = engine.play_at(&track.path, target) {
                    self.report_error(format!("无法播放「{}」：{err}", track.title));
                }
                cx.notify();
            }
            return;
        }

        self.seek_to(target);
    }

    /// 方向键：左右 ∓5 秒、上下 ±5% 音量。
    ///
    /// 音量走 `SliderState` 而不是只改 `volume_pct`：滑块的拇指位置是它自己那份状态，
    /// 只改镜像字段的话声音变了、界面上的圆点不动——看起来像没反应。
    /// `set_value` 不会发 `SliderEvent::Change`，所以引擎与持久化仍要自己调一次。
    fn handle_arrow(&mut self, arrow: ArrowKey, window: &mut Window, cx: &mut Context<Self>) {
        match arrow {
            ArrowKey::Left => self.seek_rel(-KEY_SEEK_STEP, cx),
            ArrowKey::Right => self.seek_rel(KEY_SEEK_STEP, cx),
            ArrowKey::Up | ArrowKey::Down => {
                let delta = if arrow == ArrowKey::Up {
                    KEY_VOLUME_STEP
                } else {
                    -KEY_VOLUME_STEP
                };
                let next = (self.volume_pct + delta).clamp(0, 100);
                // 已经顶到 0 / 100：不重复写盘（按住不放会以 30Hz 打 settings.json）
                if next == self.volume_pct {
                    return;
                }
                self.volume
                    .update(cx, |state, cx| state.set_value(next as f32, window, cx));
                self.set_volume_pct(next, cx);
                return;
            }
        }
        // 进度条下一帧由 tick 同步（`user_seeking` 为 false 时每 100ms 一次），
        // 这里只把视图标脏，让新位置尽快画出来。
        cx.notify();
    }

    /// 展开/收起悬浮播放列表（新版设计稿默认收起，省下整块画面）。
    fn toggle_playlist(&mut self, cx: &mut Context<Self>) {
        self.playlist_open = !self.playlist_open;
        // 诊断开关：`SMP_REGION_TRACE=1` 时把区域/点击事件落进 session.log。
        // 区域是「哪些像素归主窗口」的唯一真相，命中测试跟着它走，
        // 排查"点了没反应"只能看这条线索。
        if std::env::var_os("SMP_REGION_TRACE").is_some() {
            persistence::log_session(&format!(
                "toggle_playlist -> {} (video_active={})",
                self.playlist_open, self.video_active
            ));
        }
        cx.notify();
    }

    /// 展开/收起「视频信息」卡（胶囊左端的 `ⓘ`）。
    ///
    /// 只跟视频走：音频没有 mpv，卡上一半字段都是空的，不如不给这个入口
    /// （按钮在音频模式下也是 disabled）。
    fn toggle_info(&mut self, cx: &mut Context<Self>) {
        if !self.video_active {
            return;
        }
        self.info_open = !self.info_open;
        persistence::log_session(&format!("debug: info_open -> {}", self.info_open));
        cx.notify();
    }

    /// 切换面板的两个 tab（播放列表 / 章节）。
    ///
    /// 章节是**按需**从 mpv 读的：`chapter-list` 只在视频模式下有意义，音频模式下
    /// 切过去也只会看到「章节只在视频里才有」。切到章节页时刷新一次，避免换片后
    /// 还挂着上一部的章节。
    fn set_chapter_tab(&mut self, on: bool, cx: &mut Context<Self>) {
        self.chapter_tab = on;
        if on {
            self.refresh_chapters();
        }
        cx.notify();
    }

    /// 从当前视频内核读章节（没有内核/没章节就是空表）。
    fn refresh_chapters(&mut self) {
        self.chapters = self
            .video
            .engine
            .as_ref()
            .map(|e| e.chapters())
            .unwrap_or_default();
    }

    /// 点章节 → 跳到它的起点。
    fn seek_to_chapter(&mut self, start: f64, _window: &mut Window, cx: &mut Context<Self>) {
        // `seek_to` 已经按「视频 / 音频」路由到对应引擎；进度条位置由 tick 下一帧跟上
        self.seek_to(start);
        cx.notify();
    }

    /// 循环模式切下一档（关 → 列表 → 单曲 → 关）并落盘。
    fn cycle_repeat(&mut self, cx: &mut Context<Self>) {
        self.settings.repeat = self.settings.repeat.next();
        persistence::save_settings(&self.settings);
        cx.notify();
    }

    /// 随机播放开关。
    fn toggle_shuffle(&mut self, cx: &mut Context<Self>) {
        self.settings.shuffle = !self.settings.shuffle;
        persistence::save_settings(&self.settings);
        cx.notify();
    }

    /// 排序方式切下一档（加入顺序 → 标题 → 时长）并重排列表。
    ///
    /// 重排会把「当前播放项」挪到别的位置，所以必须拿 delegate 回报的新下标把
    /// `current` 修正过来——否则界面高亮的是 A、实际在放的是 B。
    fn cycle_sort(&mut self, cx: &mut Context<Self>) {
        self.settings.sort = self.settings.sort.next();
        let order = self.settings.sort;
        let new_row = self.playlist.update(cx, |state, cx| {
            let row = state.delegate_mut().sort_by(order);
            cx.notify();
            row
        });
        if let Some(row) = new_row {
            self.current = Some(row);
        }
        persistence::save_settings(&self.settings);
        self.persist_playlist(cx);
        cx.notify();
    }

    /// 底部「移除」删哪一条：优先列表里选中的那行，没选中就删当前播放项。
    ///
    /// 为什么不是「删当前播放」优先：用户点了一行（选中）再点移除，删的必须是
    /// 他刚点的那行；只看 `current` 的话会删掉正在播的那首，看起来像删错了。
    fn remove_selected(&mut self, cx: &mut Context<Self>) {
        let row = self
            .playlist
            .read(cx)
            .delegate()
            .selected_row()
            .or(self.current);
        if let Some(row) = row {
            self.remove_track(row, cx);
        }
    }

    fn can_remove_selected(&self, cx: &Context<Self>) -> bool {
        self.playlist.read(cx).delegate().selected_row().is_some() || self.current.is_some()
    }

    /// 循环切换字幕轨。
    ///
    /// mpv 的 `cycle` 没有返回值，而且**没有别的轨可切时它什么也不做**——用户看到的
    /// 就是"点了没反应"。这里记下切换前的轨号，下一轮 tick 比对观测值：变了报新轨，
    /// 800ms 还没变就明说"没有其他字幕轨"。
    fn cycle_subtitles(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = &self.video.engine else {
            return;
        };
        let before = engine.status().sub_track;
        engine.cycle_subtitles();
        self.pending_track = Some((TrackKind::Subtitle, before, Instant::now()));
        cx.notify();
    }

    /// 循环切换音轨（同上）。
    fn cycle_audio_track(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = &self.video.engine else {
            return;
        };
        let before = engine.status().audio_track;
        engine.cycle_audio_track();
        self.pending_track = Some((TrackKind::Audio, before, Instant::now()));
        cx.notify();
    }

    fn set_volume_pct(&mut self, pct: i32, cx: &mut Context<Self>) {
        self.volume_pct = pct.clamp(0, 100);
        let volume = self.volume_pct as f32 / 100.0;
        if let Some(engine) = &mut self.engine {
            engine.set_volume(volume);
        }
        if let Some(engine) = &self.video.engine {
            engine.set_volume(volume);
        }
        self.settings.volume = volume;
        persistence::save_settings(&self.settings);
        cx.notify();
    }

    /// 进入 / 退出影院（全屏播放）模式。
    ///
    /// 影院 = 全屏 + 收起歌单 + 控件浮在画面上。它是**唯一**的全屏入口：
    /// 双击画面、胶囊上的全屏按钮、标题带的退出按钮、`F11` 都走这里。
    ///
    /// 为什么要有这个封装：`cinema` 这个自己记的布尔值和 `window.is_fullscreen()`
    /// 是两套状态（后者在 GPUI 的执行器里异步翻），两个入口各写一遍必然对不上。
    fn set_cinema(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        // 影院只对视频成立：音频模式没有画面可铺，全屏只是把封面拉大，没有意义。
        let on = on && self.video_active;
        if self.cinema == on && window.is_fullscreen() == on {
            return;
        }
        self.cinema = on;
        // 全屏时画面占满整块屏、没有可拖的余地，把拖动锚点撤掉
        self.drag_anchor.set(None);
        if window.is_fullscreen() != on {
            window.toggle_fullscreen();
        }
        persistence::log_session(&format!(
            "cinema -> {on} (fullscreen={}, video_active={})",
            window.is_fullscreen(),
            self.video_active
        ));
        cx.notify();
    }

    fn toggle_cinema(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_cinema(!self.cinema, window, cx);
    }

    /// 退出全屏回到窗口（`Esc` / 标题带的退出按钮 / 全屏时的最大化按钮）。
    fn exit_cinema(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_cinema(false, window, cx);
    }

    /// 最大化 / 还原。
    ///
    /// 不走 `window.zoom_window()`：Windows 后端那句只有 `ShowWindowAsync(SW_MAXIMIZE)`，
    /// 没有还原分支，所以「最大化之后再点一次」什么都不会发生（用户就是这么发现的）。
    /// 见 `crate::window_actions`。
    ///
    /// 全屏时这个按钮的语义是**回到窗口**：GPUI 进全屏时把 `WS_MAXIMIZEBOX`
    /// / `WS_CAPTION` 一起摘掉了，此时再 `ShowWindowAsync(SW_MAXIMIZE)` 会把窗口
    /// 弄成「铺满工作区、但 GPUI 仍认为自己在全屏」的错乱状态——用户点完的感受
    /// 就是「回不到窗口」。所以这里先退全屏。
    fn toggle_maximize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cinema || window.is_fullscreen() {
            self.exit_cinema(window, cx);
            return;
        }
        let target = !self.maximized;
        window_actions::set_zoomed(self.main_hwnd, target);
        persistence::log_session(&format!(
            "toggle_maximize: hwnd={} target={target}",
            self.main_hwnd
        ));
        // 异步命令，此刻 IsZoomed 还是旧值：先乐观地更新，tick 里再校正
        // （用户用 Aero Snap / Win+↑ / 双击标题栏改状态时只能靠 tick 发现）。
        self.maximized = target;
        self.zoom_pending = Some(Instant::now());
        cx.notify();
    }

    // ------------------------------------------------------------------
    // 播放列表操作（由 delegate / 按钮回调）
    // ------------------------------------------------------------------

    fn append_tracks(&mut self, tracks: Vec<TrackMeta>, cx: &mut Context<Self>) {
        if tracks.is_empty() {
            return;
        }
        self.playlist.update(cx, |state, cx| {
            state.delegate_mut().append_tracks(tracks);
            cx.notify();
        });
        self.persist_playlist(cx);
        cx.notify();
    }

    /// 删除指定行（行尾 × 按钮回调）。
    pub fn remove_track(&mut self, row: usize, cx: &mut Context<Self>) {
        let count_before = self.playlist.read(cx).delegate().track_count();
        if row >= count_before {
            return;
        }
        self.playlist.update(cx, |state, cx| {
            state.delegate_mut().remove_at(row);
            cx.notify();
        });

        // 修正当前曲目下标
        match self.current {
            Some(i) if i == row => {
                if let Some(engine) = &mut self.engine {
                    engine.stop();
                }
                self.leave_video();
                self.current = None;
                self.playlist.update(cx, |state, cx| {
                    state.delegate_mut().set_confirmed(None);
                    cx.notify();
                });
            }
            Some(i) if i > row => {
                self.current = Some(i - 1);
                self.playlist.update(cx, |state, cx| {
                    state.delegate_mut().set_confirmed(self.current);
                    cx.notify();
                });
            }
            _ => {}
        }
        self.persist_playlist(cx);
        cx.notify();
    }

    /// 拖拽排序（on_drop 回调）。
    pub fn move_playlist_item(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let count = self.playlist.read(cx).delegate().track_count();
        if from >= count || to >= count || from == to {
            return;
        }
        self.playlist.update(cx, |state, cx| {
            state.delegate_mut().move_item(from, to);
            cx.notify();
        });
        // delegate 已重映射 confirmed；同步 view 侧 current
        self.current = self.playlist.read(cx).delegate().confirmed_row();
        self.persist_playlist(cx);
        cx.notify();
    }

    fn clear_playlist(&mut self, cx: &mut Context<Self>) {
        if let Some(engine) = &mut self.engine {
            engine.stop();
        }
        self.leave_video();
        self.current = None;
        self.playlist.update(cx, |state, cx| {
            state.delegate_mut().clear();
            cx.notify();
        });
        self.persist_playlist(cx);
        cx.notify();
    }

    fn persist_playlist(&self, cx: &Context<Self>) {
        let delegate = self.playlist.read(cx).delegate();
        persistence::save_playlist(delegate.tracks(), self.current);
    }

    // ------------------------------------------------------------------
    // 文件导入
    // ------------------------------------------------------------------

    fn add_files(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let files = rfd::AsyncFileDialog::new()
                .set_title("添加媒体文件")
                .add_filter(
                    "媒体文件",
                    &[
                        "mp3", "flac", "wav", "ogg", "oga", "m4a", "aac", "aiff", "mp4", "m4v",
                        "mkv", "webm", "mov", "avi", "flv", "wmv", "ts",
                    ],
                )
                .pick_files()
                .await;
            let Some(files) = files else { return };
            let paths: Vec<PathBuf> = files.into_iter().map(|f| f.path().to_path_buf()).collect();

            let tracks = cx
                .background_spawn(async move {
                    paths.iter().map(|p| load_track_meta(p)).collect::<Vec<_>>()
                })
                .await;

            let _ = this.update(cx, |view, cx| {
                if tracks.is_empty() {
                    view.report_info("未找到可播放的媒体文件");
                } else {
                    view.append_tracks(tracks, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn add_folder(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let folder = rfd::AsyncFileDialog::new()
                .set_title("添加文件夹（递归扫描）")
                .pick_folder()
                .await;
            let Some(folder) = folder else { return };
            let dir = folder.path().to_path_buf();

            let tracks = cx
                .background_spawn(async move { scan_dir_for_tracks(&dir) })
                .await;

            let _ = this.update(cx, |view, cx| {
                if tracks.is_empty() {
                    view.report_info("该文件夹内没有可播放的媒体文件");
                } else {
                    view.append_tracks(tracks, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------------
    // 从资源管理器拖文件进来
    // ------------------------------------------------------------------

    /// 外部文件被拖进窗口（GPUI 把平台的拖放事件转成内部拖拽，松开时命中根节点）。
    ///
    /// 目录递归扫描、文件读元数据都在后台线程做；回来后追加进列表并把**拖入的第一条**
    /// 直接播起来——「拖进来就能看」是这个交互的默认预期，不该再让用户点一下。
    fn import_dropped(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        // 拖放路径是外部世界给的，出问题时（相对路径、被剪短、编码）只有原样记下来才查得动
        persistence::log_session(&format!(
            "drop {} path(s): {}",
            paths.len(),
            paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(" | ")
        ));
        // 不自动展开右侧歌单：视频模式下展开会让画面整体让位，
        // 用户只是打开/拖进一个文件，却被顶开一块面板反而碍事。
        // 结果用一条 toast 说清楚（见 append_dropped_and_play），面板开合仍是用户自己决定。
        cx.notify();

        cx.spawn(async move |this, cx| {
            let tracks = cx
                .background_spawn(async move {
                    let mut out: Vec<TrackMeta> = Vec::new();
                    for path in paths {
                        if path.is_dir() {
                            out.extend(scan_dir_for_tracks(&path));
                        } else if path.is_file() {
                            out.push(load_track_meta(&path));
                        }
                    }
                    out
                })
                .await;

            let _ = this.update(cx, |view, cx| {
                if tracks.is_empty() {
                    view.report_info("拖入的内容里没有可播放的媒体文件");
                    cx.notify();
                } else {
                    view.append_dropped_and_play(tracks, cx);
                }
            });
        })
        .detach();
    }

    /// 追加拖入的曲目（按路径去重），并播放拖入的第一条。
    fn append_dropped_and_play(&mut self, tracks: Vec<TrackMeta>, cx: &mut Context<Self>) {
        let first_key = tracks.first().map(|track| path_key(&track.path));

        let mut seen: Vec<String> = self
            .playlist
            .read(cx)
            .delegate()
            .tracks()
            .iter()
            .map(|track| path_key(&track.path))
            .collect();
        let mut fresh = Vec::new();
        for track in tracks {
            let key = path_key(&track.path);
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            fresh.push(track);
        }
        if !fresh.is_empty() {
            self.append_tracks(fresh, cx);
        }

        // 拖的可能本来就是列表里的曲目：那就直接播那一条
        let target = first_key.and_then(|key| {
            let delegate = self.playlist.read(cx).delegate();
            (0..delegate.track_count()).find(|&row| {
                delegate
                    .track(row)
                    .map(|track| path_key(&track.path) == key)
                    .unwrap_or(false)
            })
        });
        match target {
            // 安静地播，不弹提示：拖进来的动作本身就是意图，
            // 画面换了 / 走带动了就是最好的反馈，再来一条「已导入 N 个文件」
            // 只是把用户已经知道的事复述一遍（2026-10-18 用户明确要求去掉）。
            Some(index) => self.play_track(index, cx),
            None => cx.notify(),
        }
    }

    /// 拖入文件时的提示层（盖住整个窗口）。
    ///
    /// 上一代这里要分两条路：视频模式下画面那块不归主窗口，居中卡片只会看到四周
    /// 暗边、中间什么都没有，所以只能把提示塞进标题带。画面进了自己的渲染树之后
    /// 这个约束消失了，两种模式用同一个居中卡片。
    fn render_drop_hint(&self) -> Div {
        let scrim = div()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .bg(gpui_kit::rgba(0x08090CC0));

        scrim.flex().items_center().justify_center().child(
            v_flex()
                .items_center()
                .gap(px(10.))
                .px(px(28.))
                .py(px(20.))
                .rounded(px(14.))
                .bg(overlay::glass())
                .border_1()
                .border_color(overlay::border())
                .child(
                    Icon::new(IconName::Music)
                        .size(px(30.))
                        .text_color(overlay::accent()),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(overlay::fg())
                        .child("松开即可导入并播放"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(overlay::dim())
                        .child("支持音视频文件，拖文件夹会递归扫描"),
                ),
        )
    }

    // ------------------------------------------------------------------
    // 渲染
    // ------------------------------------------------------------------
    //
    // 新版设计稿是 IINA 式沉浸布局（Ardot 主播放窗口 1280×800）：
    //   - 画面铺满整窗，控件以浮层形式压在画面上
    //   - 底部是内缩 24px 的悬浮胶囊（1232×100，圆角 14），深色玻璃
    //   - 播放列表是 320 宽的悬浮面板，**默认收起**，由控制栏的列表图标开合
    //   - 顶部只有一条 34px 的标题带（自绘），系统标题栏已关闭
    //
    // 架构限制（mpv 渲染在独立顶层窗口里）曾导致两处偏差：
    //   1. 视频画面只占「标题带以下、胶囊以上」，胶囊落在深色画布上；
    //   2. 视频模式下列表展开时，画面右侧让位。
    //
    // 第七轮起画面铺满整窗，chrome 用半透明玻璃真的压在画面上。第十轮起 mpv 走
    // 软件渲染 API，画面**就在主窗口的渲染树里**——不再有第二块窗口、不再有窗口
    // 区域打洞，玻璃的 L3 模糊也终于有像素可用（见 `video_surface`）。

    /// 标题带只压在两种底色上：音频模式是主题 `background`，视频模式是压在画面上的
    /// 深玻璃。前景色必须跟着**底色**走，不能跟着应用主题走——浅色主题 + 深玻璃的
    /// 组合下直接用 `muted_foreground` / `secondary_foreground` 会得到 3:1 左右的
    /// 对比度，曲名和窗口按钮看着像没渲染。
    fn chrome_fg(&self, cx: &Context<Self>) -> gpui_kit::Hsla {
        if self.video_active {
            overlay::fg()
        } else {
            cx.theme().foreground
        }
    }

    fn chrome_dim(&self, cx: &Context<Self>) -> gpui_kit::Hsla {
        if self.video_active {
            overlay::dim()
        } else {
            cx.theme().muted_foreground
        }
    }

    /// 标题带右侧的状态胶囊（视频模式下代替 toast）。
    ///
    /// 底色一定按浮层口径画：它只在 `video_active` 时出现，那一块 chrome 是压在画面上的深玻璃。
    fn render_status_pill(
        &self,
        message: &SharedString,
        is_error: bool,
    ) -> impl gpui_kit::IntoElement {
        let (icon, tint) = if is_error {
            (IconName::CircleX, overlay::danger())
        } else {
            (IconName::CircleCheck, overlay::accent())
        };

        h_flex()
            .id("status-pill")
            .test_support()
            .flex_shrink(1.)
            .min_w_0()
            .items_center()
            .gap(px(6.))
            .px(px(10.))
            .py(px(4.))
            .mr(px(8.))
            .rounded(px(999.))
            .bg(overlay::glass())
            .border_1()
            .border_color(overlay::border())
            .child(Icon::new(icon).size(px(12.)).text_color(tint))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(overlay::fg())
                    .child(message.clone()),
            )
    }

    /// 顶部标题带：当前曲名 + 自绘窗口按钮（系统标题栏已在 main 里关掉）。
    fn render_title_band(&self, cx: &Context<Self>) -> Div {
        // 自己缓存的最大化状态（切换不经过 GPUI，见 `toggle_maximize`）；
        // 最大化时 Windows 会把「最大化」换成「还原」图标。
        let maximized = self.maximized;
        let title = match self.current_track(cx) {
            Some(track) => match &track.artist {
                Some(artist) if !artist.is_empty() => format!("{} · {}", track.title, artist),
                _ => track.title.clone(),
            },
            None => "Chroma".to_string(),
        };

        h_flex()
            .w_full()
            .h(px(TITLE_BAND))
            .flex_shrink_0()
            .items_center()
            .pl(px(16.))
            // 视频模式下这一条压在画面上，必须自带一层玻璃，否则曲名和窗口按钮直接
            // 落在亮画面上（16:9 片源常见顶部黑边，看着「还好」，换成顶到边的画面
            // 就读不出来了）。72% 深玻璃 + 画面透出来，和胶囊同一套口径。
            .when(self.video_active, |el| el.bg(overlay::glass()))
            // 只有左侧这段是拖拽区：整条都设成 Drag 的话，右侧窗口按钮会一起
            // 被当成非客户区，点击事件就到不了 GPUI。
            .child(
                div()
                    .id("title-drag")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .window_control_area(WindowControlArea::Drag)
                    // 双击标题栏最大化/还原——同样不能走 `window.zoom_window()`
                    .on_double_click(cx.listener(|this, _e, window, cx| {
                        this.toggle_maximize(window, cx);
                    }))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(self.chrome_dim(cx))
                            .child(title),
                    ),
            )
            .when_some(self.status_pill_live(), |el, (message, _, is_error)| {
                el.child(self.render_status_pill(message, *is_error))
            })
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h_full()
                    .items_center()
                    .when(self.cinema, |el| {
                        // 影院模式下仍然留一个退出按钮，否则用户没法回到普通窗口
                        let fg = self.chrome_fg(cx);
                        el.child(
                            Button::new("exit-cinema")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Minimize)
                                .text_color(fg)
                                .tooltip("退出全屏")
                                .accessibility_label("退出全屏")
                                .mr(px(8.))
                                .on_click(cx.listener(|this, _e, window, cx| {
                                    this.toggle_cinema(window, cx);
                                })),
                        )
                    })
                    // Windows 的标题栏按钮是「46 宽 × 标题栏高」的矩形块，
                    // 悬停铺满整块底色；之前用的 xsmall 图标按钮只有 20×20，
                    // 在自绘标题栏里明显偏小（也不符合 Win 的点击热区）。
                    .child(caption_button(
                        "win-min",
                        ICON_MINIMIZE,
                        "最小化",
                        false,
                        self.video_active,
                        |_, window, _| window.minimize_window(),
                        cx,
                    ))
                    .child(caption_button(
                        "win-max",
                        if maximized {
                            ICON_RESTORE
                        } else {
                            ICON_MAXIMIZE
                        },
                        if maximized { "还原" } else { "最大化" },
                        false,
                        self.video_active,
                        cx.listener(|this, _e, window, cx| this.toggle_maximize(window, cx)),
                        cx,
                    ))
                    .child(caption_button(
                        "win-close",
                        ICON_CLOSE,
                        "关闭",
                        true,
                        self.video_active,
                        |_, window, _| window.remove_window(),
                        cx,
                    )),
            )
    }

    /// 舞台：视频/音频两种内容。
    ///
    /// 两种模式的占位方式不同：
    /// - **视频**：绝对定位铺满整窗，画面直接顶到窗口四边，chrome 压在上面；
    /// - **音频**：占「标题带以下、胶囊以上」的流式空间。
    ///
    /// `self_stretch` 不能少：`h_flex()` 默认 `items_center`，舞台的子元素又全是
    /// 绝对定位（内容高度 0），不撑满交叉轴的话舞台高度会塌成 0——画面被压成
    /// 零高就什么都看不见了。
    fn render_stage(&self, cx: &Context<Self>) -> impl gpui_kit::IntoElement {
        let stage = div().id("stage").test_support().relative();

        let stage = if self.video_active {
            // 画面铺满整窗。底下那一层深色画布在 `render_video_surface` 里铺
            // （`Contain` 留出来的边必须是黑的，不能透出主题底色）。
            stage.absolute().top(px(0.)).left(px(0.)).size_full()
        } else {
            stage
                .flex_1()
                .self_stretch()
                .min_w_0()
                .min_h_0()
                .bg(cx.theme().background)
        };

        if self.video_active {
            stage.child(self.render_video_surface(cx))
        } else {
            stage.child(self.render_audio_surface(cx))
        }
    }

    /// 音频：居中的封面卡片 + 曲目信息（浮层胶囊压在下缘）。
    fn render_audio_surface(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let track = self.current_track(cx);
        let empty = self.playlist.read(cx).delegate().track_count() == 0;

        let mut surface = v_flex()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(18.))
            .px(px(40.))
            // 沉到窗口视觉中心偏上一点：完全居中会和悬浮胶囊贴太近，
            // 完全让开胶囊（整块 BOTTOM_BAND）又显得整体偏上。
            .pb(px(BOTTOM_BAND * 0.5))
            .child(
                div()
                    .size(px(200.))
                    .flex_shrink_0()
                    .rounded(px(16.))
                    .bg(theme.secondary)
                    .border_1()
                    .border_color(theme.border)
                    .shadow_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(IconName::Music)
                            .size(px(56.))
                            .text_color(theme.muted_foreground),
                    ),
            );

        match &track {
            Some(track) => {
                surface = surface.child(
                    v_flex()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(20.))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(theme.foreground)
                                .child(track.title.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "{} · {}",
                                    track
                                        .artist
                                        .clone()
                                        .unwrap_or_else(|| "未知艺术家".to_string()),
                                    fmt_time(track.duration_secs.unwrap_or(0.0))
                                )),
                        ),
                );
            }
            None => {
                surface = surface.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.muted_foreground)
                        .child(if empty {
                            "播放列表为空，点控制栏的列表图标添加媒体"
                        } else {
                            "从播放列表选择一项开始播放"
                        }),
                );
            }
        }

        surface
    }

    /// 视频：把 mpv 交来的那一帧画在舞台上。
    ///
    /// 底色必须是深色画布：`object_fit: Contain` 在画面比例和窗口比例不一致时会
    /// 留边，留出来的那圈得是黑的（和设计稿的 `overlay::canvas()` 一致），不能透出
    /// 主题底色。
    ///
    /// 手势（拖动窗体 / 双击全屏）挂在这一层。以前画面归叠加窗，这两个手势只能在
    /// wndproc 里手写（见 `window_actions`）；现在事件正常到 GPUI，直接挂监听。
    fn render_video_surface(
        &self,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<Div> {
        // 不加 `test_support()`：它在 test-support 特性下会把元素包成 `Observed`，
        // 函数签名就没法写出具体类型了；这块也不需要按 id 查询（几何走 chrome_layout）。
        let base = div()
            .id("video-surface")
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .bg(overlay::canvas());

        if let Some(err) = &self.video_error {
            return base.flex().items_center().justify_center().child(
                v_flex()
                    .items_center()
                    .gap(px(10.))
                    .p(px(24.))
                    .child(
                        Icon::new(IconName::CircleAlert)
                            .size(px(32.))
                            .text_color(gpui_kit::rgb(0xF38BA8)),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(overlay::fg())
                            .child(err.clone()),
                    )
                    .child(div().text_size(px(11.)).text_color(overlay::dim()).child(
                        "提示：把 libmpv-2.dll 放到程序目录，或用 SMP_LIBMPV_PATH 指定路径",
                    )),
            );
        }

        // 画面。还没有帧（刚起播、或渲染失败）时只留黑底——不要画占位图，
        // 那会在"第一帧到之前"闪一块和设计稿无关的东西。
        let base = match self.video.surface.image() {
            Some(source) => base.child(
                gpui_kit::img(source)
                    .object_fit(gpui_kit::ObjectFit::Contain)
                    .size_full(),
            ),
            None => base,
        };

        // 每帧把视频区 bounds 写回 Cell（验收探针据此报告「画面铺到哪」）
        let slot = self.video_rect.clone();
        let base = base.child(
            canvas(
                move |bounds, _window, _cx| {
                    slot.set(Some((
                        bounds.origin.x.as_f32(),
                        bounds.origin.y.as_f32(),
                        bounds.size.width.as_f32(),
                        bounds.size.height.as_f32(),
                    )));
                },
                |_bounds, _out, _window, _cx| {},
            )
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full(),
        );

        self.picture_gestures(base, cx)
    }

    /// 画面上的手势：按住左键拖窗体、双击切全屏。
    ///
    /// 两个都不能用 GPUI 的现成设施：
    /// - `WindowControlArea::Drag` 会把这块变成非客户区（`HTCAPTION`），系统自己
    ///   接管拖动，同时也把双击变成"最大化"、鼠标事件也不再进 GPUI；
    /// - `Window::start_window_move()` 在 Windows 后端没有实现（只有 apple / test
    ///   后端有）。
    ///
    /// 所以自己搬：按下时记一份锚点（光标屏幕坐标 + 窗口外框坐标），拖动循环按
    /// 「光标走了多少，窗口就走多少」每 8ms 追一次。不用 `on_mouse_move` 驱动——
    /// 那个事件在快速拖动时可能被窗口重绘挤掉，位置会跟丢；轮询光标永远是最新的。
    fn picture_gestures(
        &self,
        element: gpui_kit::Stateful<Div>,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<Div> {
        element
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _e, window, cx| {
                    this.begin_picture_drag(window, cx);
                }),
            )
            .on_mouse_up(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _e, _window, _cx| {
                    this.drag_anchor.set(None);
                }),
            )
            .on_double_click(cx.listener(|this, _e, window, cx| {
                // 双击优先：先撤掉拖动，免得"双击的第二下"把窗口拖走一点点
                this.drag_anchor.set(None);
                this.toggle_cinema(window, cx);
            }))
    }

    /// 按下画面：记锚点并拉起拖动循环。
    fn begin_picture_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 全屏 / 最大化时窗口不该被拖走
        if !self.video_active || self.cinema || self.maximized || self.main_hwnd == 0 {
            return;
        }
        let (cursor_x, cursor_y) = window_actions::cursor_pos();
        let (window_x, window_y) = window_actions::window_origin(self.main_hwnd);
        let anchor = self.drag_anchor.clone();
        anchor.set(Some((cursor_x, cursor_y, window_x, window_y)));
        self.activity = Instant::now();

        let main = self.main_hwnd;
        cx.spawn_in(window, async move |_this, cx| {
            // 8ms ≈ 120Hz：比鼠标事件密，窗口才跟得上光标。
            // 这里不能挂 `on_mouse_move`：快速拖动时那个事件会被窗口重绘挤掉，
            // 位置一旦跟丢，后面每帧都会差同样的距离（越拖越偏）。
            let step = Duration::from_millis(8);
            while let Some((ax, ay, wx, wy)) = anchor.get() {
                let (x, y) = window_actions::cursor_pos();
                window_actions::move_window(main, wx + x - ax, wy + y - ay);
                cx.background_executor().timer(step).await;
            }
        })
        .detach();
    }

    /// 悬浮播放列表面板：320 宽、Latte 底、浮在画面上（默认可收起）。
    /// 悬浮播放列表面板：320 宽、贴右 24、顶边让开标题带。
    ///
    /// 设计稿 Win 口径：`x: 936, y: 48, w: 320, h: 633`。这里不写死高度——48 是
    /// 「标题带 40 + 8」、633 是「800 − 48 − 119」，而 119 正是胶囊占的那条带
    /// （92 + 24 + 3）。用 `top + bottom` 锚定，换窗口尺寸时面板自然跟着变长变短，
    /// 而不是在 720 高的窗口里戳出屏幕。
    ///
    /// 结构：头部 48（tab 切换 + 关闭）→ 分隔线 → 列表 → 分隔线 → 底部工具条 44。
    fn render_playlist_panel(&self, cx: &mut Context<Self>) -> impl gpui_kit::IntoElement {
        // 主题色先拷成局部量：下面要 `&mut cx` 建监听器，借着 `cx.theme()` 不放会冲突
        let (bg, border) = {
            let t = cx.theme();
            (t.background, t.border)
        };
        let count = self.playlist.read(cx).delegate().track_count();
        let chapter_tab = self.chapter_tab;

        v_flex()
            .id("playlist-panel")
            .test_support()
            .absolute()
            .top(px(TITLE_BAND + PLAYLIST_TOP_GAP))
            .right(px(PLAYLIST_INSET))
            .bottom(px(PLAYLIST_BOTTOM))
            .w(px(PLAYLIST_WIDTH))
            .rounded(px(CAPSULE_RADIUS))
            .bg(bg)
            .border_1()
            .border_color(border)
            .shadow_lg()
            .overflow_hidden()
            .child(self.render_playlist_header(cx))
            .child(div().w_full().h(px(1.)).flex_shrink_0().bg(border))
            // List 默认带 8px 左右内边距；设计稿行胶囊离面板边 12，所以这里给 12。
            //
            // `overflow_hidden` 不能省：面板本身裁了圆角，但中间这层是个 flex 容器，
            // 列表超出高度时会**画到页脚上面**（截图里最后一排和「循环/随机/排序」
            // 叠在一起）。`min_h_0` 只保证它能被压扁，不保证裁切。
            .child(
                div()
                    .id("playlist-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .when(chapter_tab, |el| el.child(self.render_chapter_list(cx)))
                    .when(!chapter_tab, |el| {
                        el.child(List::new(&self.playlist).px(px(12.)))
                    }),
            )
            .child(div().w_full().h(px(1.)).flex_shrink_0().bg(border))
            .child(self.render_playlist_footer(count, cx))
    }

    /// 面板头部：「播放列表 / 章节」两枚 tab + 关闭。
    ///
    /// tab 是**药丸**（选中态 Mauve 12% 底 + Mauve 字），不是下划线——设计稿里
    /// 它和胶囊、信息卡一样都是圆角块，下划线那套在深色玻璃上太重。
    fn render_playlist_header(&self, cx: &mut Context<Self>) -> Div {
        let muted_fg = cx.theme().muted_foreground;
        let active = self.chapter_tab;

        h_flex()
            .h(px(PLAYLIST_HEADER))
            .flex_shrink_0()
            .w_full()
            .items_center()
            .px(px(14.))
            .gap(px(4.))
            .child(self.render_panel_tab("tab-playlist", "播放列表", !active, cx))
            .child(self.render_panel_tab("tab-chapters", "章节", active, cx))
            .child(div().flex_1())
            .child(
                Button::new("playlist-close")
                    .ghost()
                    .with_size(px(24.))
                    .icon(IconName::Close)
                    .text_color(muted_fg)
                    .tooltip("关闭播放列表")
                    .accessibility_label("关闭播放列表")
                    .on_click(cx.listener(|this, _e, _w, cx| {
                        this.playlist_open = false;
                        // 收面板=回到「播放列表」页，下次展开不会突然停在空的章节页
                        this.chapter_tab = false;
                        cx.notify();
                    })),
            )
    }

    /// 单枚 tab（选中态：Mauve 12% 底 + Mauve 字；未选中：次要前景色）。
    fn render_panel_tab(
        &self,
        id: &'static str,
        label: &'static str,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (accent, muted, muted_fg) = {
            let t = cx.theme();
            (t.primary, t.muted, t.muted_foreground)
        };
        div()
            .id(id)
            .test_support()
            .h(px(24.))
            .px(px(10.))
            .rounded(px(8.))
            .flex()
            .items_center()
            .cursor_pointer()
            .when(selected, |el| el.bg(accent.opacity(0.12)))
            .when(!selected, |el| el.hover(move |s| s.bg(muted.opacity(0.6))))
            .text_size(px(13.))
            .when(selected, |el| {
                el.text_color(accent)
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
            })
            .when(!selected, |el| el.text_color(muted_fg))
            .on_click(cx.listener(move |this, _e, _w, cx| {
                this.set_chapter_tab(label == "章节", cx);
            }))
            .child(label)
    }

    /// 「章节」页：mpv 的 `chapter-list`（没有章节时给一句实话）。
    fn render_chapter_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let (muted, muted_fg, fg) = {
            let t = cx.theme();
            (t.muted, t.muted_foreground, t.foreground)
        };
        if self.chapters.is_empty() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .p(px(16.))
                .text_color(muted_fg)
                .child(Icon::new(IconName::ListMusic).size_10())
                .child(div().text_size(px(12.)).child(if self.video_active {
                    "这个片子没有章节信息"
                } else {
                    "章节只在视频里才有"
                }))
                .into_any_element();
        }

        let rows: Vec<_> = self
            .chapters
            .iter()
            .enumerate()
            .map(|(i, (start, title))| {
                let label = if title.is_empty() {
                    format!("章节 {}", i + 1)
                } else {
                    title.clone()
                };
                let start = *start;
                h_flex()
                    .id(SharedString::from(format!("chapter-{i}")))
                    .h(px(PLAYLIST_ROW))
                    .w_full()
                    .items_center()
                    .gap(px(10.))
                    .px(px(12.))
                    .rounded(px(10.))
                    .cursor_pointer()
                    .hover(move |s| s.bg(muted.opacity(0.6)))
                    .on_click(cx.listener(move |this, _e, window, cx| {
                        this.seek_to_chapter(start, window, cx);
                    }))
                    .child(
                        div()
                            .w(px(46.))
                            .flex_shrink_0()
                            .font_family(mono_font())
                            .text_size(px(11.))
                            .text_color(muted_fg)
                            .child(fmt_time(start)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(13.))
                            .text_color(fg)
                            .child(label),
                    )
                    .into_any_element()
            })
            .collect();

        v_flex()
            .size_full()
            .px(px(8.))
            .py(px(4.))
            .gap(px(0.))
            .children(rows)
            .into_any_element()
    }

    /// 面板底部工具条：循环 / 随机 / 排序 ·「N 个文件 · 总时长」· 添加 / 移除 / 清空。
    ///
    /// 三段都是 `flex_1`（等宽），中间那段才真的在面板正中——和胶囊一个道理，
    /// 只有 `justify_between` 会让中段被两侧宽度带偏。
    fn render_playlist_footer(&self, count: usize, cx: &mut Context<Self>) -> Div {
        let muted_fg = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let accent = cx.theme().primary;
        let repeat = self.settings.repeat;
        let shuffle = self.settings.shuffle;
        let sort = self.settings.sort;
        let total: f64 = self
            .playlist
            .read(cx)
            .delegate()
            .tracks()
            .iter()
            .filter_map(|t| t.duration_secs)
            .sum();
        let summary = if total > 0.5 {
            format!("{} 个文件 · {}", count, fmt_time(total))
        } else {
            format!("{count} 个文件")
        };
        let can_remove = self.can_remove_selected(cx);

        h_flex()
            .h(px(PLAYLIST_FOOTER))
            .flex_shrink_0()
            .w_full()
            .items_center()
            .px(px(12.))
            .gap(px(4.))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(2.))
                    .child(
                        panel_button(
                            "repeat-mode",
                            Icon::default().data(ICON_REPEAT),
                            repeat.label(),
                            24.,
                            repeat.is_on(),
                            fg,
                            accent,
                        )
                        .on_click(cx.listener(|this, _e, _w, cx| this.cycle_repeat(cx))),
                    )
                    .child(
                        panel_button(
                            "shuffle-mode",
                            Icon::default().data(ICON_SHUFFLE),
                            if shuffle {
                                "随机：开"
                            } else {
                                "随机：关"
                            },
                            24.,
                            shuffle,
                            fg,
                            accent,
                        )
                        .on_click(cx.listener(|this, _e, _w, cx| this.toggle_shuffle(cx))),
                    )
                    .child(
                        panel_button(
                            "sort-mode",
                            Icon::default().data(ICON_SORT),
                            sort.label(),
                            24.,
                            sort != crate::persistence::SortOrder::Added,
                            fg,
                            accent,
                        )
                        .on_click(cx.listener(|this, _e, _w, cx| this.cycle_sort(cx))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .justify_center()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(muted_fg)
                    .child(summary),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .justify_end()
                    .gap(px(2.))
                    .child(
                        panel_button(
                            "add-file",
                            Icon::new(IconName::Plus),
                            "添加文件",
                            24.,
                            false,
                            fg,
                            accent,
                        )
                        .on_click(cx.listener(|this, _e, _w, cx| this.add_files(cx))),
                    )
                    .child(
                        panel_button(
                            "add-folder",
                            Icon::new(IconName::FolderOpen),
                            "添加文件夹",
                            24.,
                            false,
                            fg,
                            accent,
                        )
                        .on_click(cx.listener(|this, _e, _w, cx| this.add_folder(cx))),
                    )
                    .child(
                        panel_button(
                            "remove-track",
                            Icon::default().data(ICON_MINUS),
                            "从播放列表移除",
                            24.,
                            false,
                            fg,
                            accent,
                        )
                        // 没有可删的行时压成次要色（`Button` 没有 `disabled` builder）
                        .text_color(if can_remove { fg } else { muted_fg })
                        .on_click(cx.listener(|this, _e, _w, cx| this.remove_selected(cx))),
                    )
                    .child(
                        panel_button(
                            "clear-playlist",
                            Icon::new(IconName::Delete),
                            "清空播放列表",
                            24.,
                            false,
                            fg,
                            accent,
                        )
                        .on_click(cx.listener(|this, _e, _w, cx| this.clear_playlist(cx))),
                    ),
            )
    }

    /// 底部 chrome：可选的「视频信息」卡 + 悬浮胶囊。
    ///
    /// 整列按 `bottom(CAPSULE_INSET)` 锚定，信息卡在胶囊上方 16——设计稿里那张卡
    /// 贴的是**画面左下角**（x24），高度 180、底边正好压在胶囊上沿之上 16。用
    /// 「一列自底向上排」而不是给卡片算 `bottom = 窗口高 - 胶囊高 - 24 - 16`：
    /// 胶囊高度是内容撑开的，写死算式会在换字号/换皮肤时错位。
    /// 画面底部的渐隐遮罩（设计稿 `texture_layers.L1.bottom_vignette`）。
    ///
    /// 240 高、从全透明压到 `rgba(11,12,16,.55)`。它不是装饰：玻璃浮在亮画面上
    /// 时（雪景、白墙、天空），11px 的时间码和 1px 图标会被糊掉，只能靠把玻璃
    /// 越加越浓自救（现在是 72%，设计稿要 60%）。底下先压一层渐变，玻璃就敢薄。
    ///
    /// 它压着画面最下面 240px，所以**手势要和画面一模一样**（按住拖动、双击全屏），
    /// 否则窗口底部三分之一就成了"点哪都没反应"的死区。
    ///
    /// 不给它 `WindowControlArea::Drag`：那会让压在它上面的胶囊按钮变成非客户区
    /// （标题带那条只敢给左半段设 Drag，就是同一个坑）。
    fn render_vignette(&self, cx: &mut Context<Self>) -> impl gpui_kit::IntoElement {
        let element = div()
            .id("vignette")
            .absolute()
            .left(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .h(px(VIGNETTE_HEIGHT))
            .bg(crate::theme::v_gradient(
                gpui_kit::rgba(0x0B0C1000),
                overlay::vignette(),
            ));
        self.picture_gestures(element, cx)
    }

    /// 控制层该不该显示（自动淡出的目标态）。
    ///
    /// 只有「视频 + 正在播放 + 用户没在碰控件」才淡出。暂停时必须留着：用户停下来
    /// 就是为了找控件；面板/信息卡/拖放提示/状态胶囊同理，那会儿他正在读东西。
    fn chrome_wanted(&self) -> bool {
        let busy = self.playlist_open
            || self.info_open
            || self.drop_hint_shown
            || self.user_seeking
            || self.status_pill_live().is_some();
        chrome_visible_target(
            self.video_active,
            self.playback_state().is_playing(),
            busy,
            self.activity.elapsed(),
        )
    }

    /// 每 tick 推一步透明度（见 [`CHROME_FADE_STEP`]）。
    fn tick_chrome_fade(&mut self, cx: &mut Context<Self>) {
        // 鼠标移动由根节点的 `on_mouse_move` 记（画面也在根节点下面，事件照常冒泡），
        // 这里只把时间戳搬进普通字段。
        if let Some(stamp) = self.activity_moved.take() {
            self.activity = stamp;
        }
        let target = if self.chrome_wanted() { 1.0 } else { 0.0 };
        if std::env::var_os("SMP_CHROME_TRACE").is_some() {
            use std::sync::Mutex;
            static LAST: Mutex<Option<Instant>> = Mutex::new(None);
            let mut slot = LAST.lock().unwrap();
            let due = slot
                .map(|at: Instant| at.elapsed() >= Duration::from_millis(500))
                .unwrap_or(true);
            if due {
                *slot = Some(Instant::now());
                eprintln!(
                    "[chrome] alpha={:.2} target={target:.2} video={} playing={} idle={:.1}s pill={} seeking={} drop={} panel={} info={}",
                    self.chrome_alpha,
                    self.video_active,
                    self.playback_state().is_playing(),
                    self.activity.elapsed().as_secs_f32(),
                    self.status_pill_live().is_some(),
                    self.user_seeking,
                    self.drop_hint_shown,
                    self.playlist_open,
                    self.info_open,
                );
            }
        }
        if (self.chrome_alpha - target).abs() < 0.001 {
            return;
        }
        self.chrome_alpha = if target > self.chrome_alpha {
            (self.chrome_alpha + CHROME_FADE_STEP).min(1.0)
        } else {
            (self.chrome_alpha - CHROME_FADE_STEP).max(0.0)
        };
        cx.notify();
    }

    fn render_bottom_chrome(&self, cx: &mut Context<Self>) -> Div {
        div()
            .absolute()
            .left(px(0.))
            .right(px(0.))
            .bottom(px(CAPSULE_INSET))
            .flex()
            .flex_col()
            .items_center()
            .when(self.info_open && self.video_active, |el| {
                el.child(
                    h_flex()
                        .w_full()
                        .px(px(CAPSULE_INSET))
                        .mb(px(INFO_GAP))
                        .items_end()
                        .child(self.render_info_card(cx)),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .px(px(CAPSULE_INSET))
                    .justify_center()
                    // 设计稿：播放列表展开时胶囊居中于**剩余画面**（x=88 / 中心 468），
                    // 不是继续整窗居中——否则胶囊会被面板压掉右半边。
                    .when(self.playlist_open, |el| {
                        el.pl(px(0.)).pr(px(PLAYLIST_WIDTH + PLAYLIST_INSET))
                    })
                    .child(self.render_capsule(cx)),
            )
    }

    /// 「视频信息」卡：240×180 深色玻璃，贴画面左下（设计稿 `ⓘ` 的弹出层）。
    ///
    /// 数据全部来自 mpv 的 `observe_property`（见 `VideoStatus` 的那些字段），
    /// 拿不到就显示 `—`：宁可空着，也不要编一个像样但不对的数。
    ///
    /// 它压在画面上 → 必须进 `chrome_layout`（探针与测试的几何真相），否则验收
    /// 脚本按坐标去点「ⓘ」就会落在卡片外面。
    fn render_info_card(&self, _cx: &Context<Self>) -> impl gpui_kit::IntoElement {
        let slot = self.info_rect.clone();
        let status = self
            .video
            .engine
            .as_ref()
            .map(|e| e.status())
            .unwrap_or_default();

        // 编码尺寸优先；个别容器（纯音频、未解码）只有显示尺寸，退回去用后者
        let (sw, sh) = if status.source_width > 0 && status.source_height > 0 {
            (status.source_width, status.source_height)
        } else {
            (status.width, status.height)
        };
        let resolution = if sw > 0 && sh > 0 {
            format!("{sw}×{sh}")
        } else {
            DASH.to_string()
        };

        let bitrate = status.video_bitrate + status.audio_bitrate;
        let has_audio =
            !status.audio_codec.is_empty() || status.audio_channels > 0 || status.audio_rate > 0;
        let audio = if !has_audio {
            // 一个音轨都没有时说「无音轨」，而不是留个 `—`：`—` 会被读成
            // "还没读到"，而这件事其实是确定的
            "无音轨".to_string()
        } else {
            let mut parts: Vec<String> = Vec::new();
            if !status.audio_codec.is_empty() {
                parts.push(short_codec(&status.audio_codec));
            }
            if status.audio_channels > 0 {
                parts.push(format!("{}ch", status.audio_channels));
            }
            if status.audio_rate > 0 {
                parts.push(format!("{} kHz", status.audio_rate / 1000));
            }
            parts.join(" · ")
        };

        let rows = [
            ("分辨率", resolution),
            ("编码", non_empty(&short_codec(&status.video_codec))),
            ("码率", fmt_bitrate(bitrate)),
            ("帧率", fmt_fps(status.fps)),
            (
                "色彩",
                match (status.primaries.is_empty(), status.gamma.is_empty()) {
                    (false, false) => format!("{} / {}", status.primaries, status.gamma),
                    (false, true) => status.primaries.clone(),
                    (true, false) => status.gamma.clone(),
                    (true, true) => DASH.to_string(),
                },
            ),
            ("音频", audio),
        ];

        div()
            .id("video-info-card")
            .test_support()
            .w(px(INFO_WIDTH))
            .h(px(INFO_HEIGHT))
            .rounded(px(12.))
            .bg(overlay::glass())
            .border_1()
            .border_color(overlay::border())
            .shadow_lg()
            .p(px(12.))
            .child(
                canvas(
                    move |bounds, _window, _cx| {
                        slot.set(Some((
                            bounds.origin.x.as_f32(),
                            bounds.origin.y.as_f32(),
                            bounds.size.width.as_f32(),
                            bounds.size.height.as_f32(),
                        )));
                    },
                    |_bounds, _out, _window, _cx| {},
                )
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .size_full(),
            )
            .child(
                v_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(overlay::fg())
                            .child("视频信息"),
                    )
                    .child(v_flex().gap(px(0.)).children(rows.into_iter().map(
                        |(label, value)| {
                            h_flex()
                                .h(px(22.))
                                .w_full()
                                .items_center()
                                .justify_between()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(11.))
                                        .text_color(overlay::label())
                                        .child(label),
                                )
                                .child(
                                    // 值必须能收窄 + 省略号：mpv 的编码名是
                                    // `h264 / AVC / MPEG-4 AVC / MPEG-4` 这种并列
                                    // 别名串，不截断会直接冲出卡片右边
                                    div()
                                        .min_w_0()
                                        .flex_shrink(1.)
                                        .truncate()
                                        .font_family(mono_font())
                                        .text_size(px(11.))
                                        .text_color(overlay::fg())
                                        .child(value),
                                )
                        },
                    ))),
            )
    }

    /// 悬浮胶囊：两行进度区 + 控制行，深色玻璃，**固定 760 宽**。
    ///
    /// 高度是算出来的：8 + 24（进度区：时间行 16 + 4 + 轨道 4）+ 8 + 44（走带）
    /// + 8 = 92，正好是设计稿的 92。所有尺寸都是常数，不随窗口拉伸——设计稿里它
    /// 是画面下方的一条窄带，两侧留出画面，不是铺满整宽的黑条。
    ///
    /// 三层的**先后顺序**是这张图的关键（后画的在上面）：
    /// ① L3 画面模糊（整块铺满）→ ② L1 深色玻璃（60~74% 半透明，模糊从它底下
    /// 透出来）→ ③ L2 染色（画面色相，α .26/.08）。所以玻璃底色不能写成
    /// `.bg()`：`bg` 在子节点**之前**画，会被模糊层整块盖住，胶囊就变成一块
    /// 亮色的磨砂板，浅色时间码在上面根本看不清。
    fn render_capsule(&self, cx: &mut Context<Self>) -> Div {
        // 外框负责定位/圆角，内层负责 padding —— 夹在中间的 canvas 量到的
        // 就是胶囊的真实外框（含 padding），算几何时不会差一圈。
        let slot = self.capsule_rect.clone();
        div()
            .w(px(CAPSULE_WIDTH))
            .max_w_full()
            .rounded(px(CAPSULE_RADIUS))
            // 圆角裁掉 1px 内描边和染色层探出去的那点角
            .overflow_hidden()
            .border_1()
            .border_color(overlay::edge())
            // 接触阴影 + 环境阴影：贴地那层负责"压住"，散开那层负责"浮起来"
            .shadow(vec![
                BoxShadow {
                    color: overlay::contact(),
                    offset: point(px(0.), px(2.)),
                    blur_radius: px(6.),
                    spread_radius: px(0.),
                    inset: false,
                },
                BoxShadow {
                    color: overlay::ambient(),
                    offset: point(px(0.), px(16.)),
                    blur_radius: px(40.),
                    spread_radius: px(0.),
                    inset: false,
                },
            ])
            // ① L3 画面模糊：48×20 的底部缩略图拉到整块胶囊，GPU 的双线性插值
            // 本身就是一次大半径模糊（见 `video_surface`）。没有画面时这一层不画，
            // 后面那层玻璃底色照样给出一块完整的深色。
            .children(self.video.surface.blur().map(|source| {
                gpui_kit::img(source)
                    .object_fit(gpui_kit::ObjectFit::Fill)
                    .absolute()
                    .top(px(0.))
                    .left(px(0.))
                    .size_full()
                    .into_any_element()
            }))
            // ② L1 定向光：竖向渐变（上偏蓝灰、下压深黑），不是一块平底色
            .child(
                div()
                    .absolute()
                    .top(px(0.))
                    .left(px(0.))
                    .size_full()
                    .bg(crate::theme::v_gradient(
                        overlay::capsule_top(),
                        overlay::capsule_bottom(),
                    )),
            )
            // ③ L2 画面染色（取不到画面时这一层干脆不画）
            .when_some(self.tint_applied, |el, hue| {
                el.child(div().absolute().top(px(0.)).left(px(0.)).size_full().bg(
                    crate::theme::v_gradient(overlay::tint(hue, 0.26), overlay::tint(hue, 0.08)),
                ))
            })
            // L1 的两道 1px 内描边：上沿吃光、下沿压暗，玻璃才有一层厚度
            .child(
                div()
                    .absolute()
                    .top(px(0.))
                    .left(px(0.))
                    .right(px(0.))
                    .h(px(1.))
                    .bg(overlay::highlight()),
            )
            .child(
                div()
                    .absolute()
                    .bottom(px(0.))
                    .left(px(0.))
                    .right(px(0.))
                    .h(px(1.))
                    .bg(overlay::shade()),
            )
            .child(
                canvas(
                    move |bounds, _window, _cx| {
                        slot.set(Some((
                            bounds.origin.x.as_f32(),
                            bounds.origin.y.as_f32(),
                            bounds.size.width.as_f32(),
                            bounds.size.height.as_f32(),
                        )));
                    },
                    |_bounds, _out, _window, _cx| {},
                )
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .size_full(),
            )
            .child(
                v_flex()
                    .px(px(CAPSULE_PAD_X))
                    .py(px(CAPSULE_PAD_Y))
                    .gap(px(8.))
                    .child(self.render_seek_row(cx))
                    .child(self.render_controls_row(cx)),
            )
    }

    /// 进度区：**时间行在上、轨道在下**（左当前 / 右剩余），整块 24 高。
    ///
    /// 设计稿是一行「时间–轨道–时间」的老布局的下一版：时间挪到轨道上方两端，
    /// 轨道因此能拿到整宽（714 = 760 − 2×20 − 2×side_pad − 4）。`side_pad` 是
    /// 光学修正：图标在 20 盒里只占 13.2（内缩 3.3），轨道贴盒子边会比图标两端
    /// 各宽出一截，所以往里收 2（spec 写 3，再扣掉胶囊那 1px 边框）。
    fn render_seek_row(&self, cx: &Context<Self>) -> Div {
        let track = self.current_track(cx);
        let duration = match &track {
            Some(t) if self.video_active => {
                let from_mpv = self
                    .video
                    .engine
                    .as_ref()
                    .map(|e| e.status().duration)
                    .unwrap_or(0.0);
                if from_mpv > 0.5 {
                    from_mpv
                } else {
                    t.duration_secs.unwrap_or(0.0)
                }
            }
            Some(t) => t.duration_secs.unwrap_or(0.0),
            None => 0.0,
        };
        let position = if self.user_seeking {
            self.seek.read(cx).value().start() as f64
        } else if self.video_active {
            self.video
                .engine
                .as_ref()
                .map(|e| e.status().position)
                .unwrap_or(0.0)
        } else {
            self.engine
                .as_ref()
                .map(|e| e.position_secs())
                .unwrap_or(0.0)
        };
        let remaining = (duration - position).max(0.0);

        // 设计稿的进度区是**两行**：上面一行两段时间码（两端对齐），下面一整条轨道。
        //
        //   `42:17                                       -1:26:27`
        //   `===============●───────────────────────────────────`
        //
        // 以前是「时间夹滑块」的单行（`42:17 | 轨道 596 | -1:26:27`）：轨道被时间码
        // 挤到中间 596，两边留出两条空带，视觉上既不像设计稿，也让「轨道两端就是
        // 作品的头尾」这条最基础的空间隐喻失效。
        v_flex()
            .w_full()
            .h(px(PROGRESS_HEIGHT))
            .gap(px(PROGRESS_GAP))
            .child(
                h_flex()
                    .w_full()
                    .h(px(PROGRESS_TIME_ROW))
                    .px(px(PROGRESS_SIDE_PAD))
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .id("time-current")
                            // 普通 div 不进命中测试树，`window.find(id)` 查不到 → 测试要用它
                            .test_support()
                            .font_family(mono_font())
                            .text_size(px(TIME_SIZE))
                            .text_color(overlay::fg())
                            // 等宽数字：时间跳动时整行宽度不能跟着抖
                            .child(fmt_time(position)),
                    )
                    .child(
                        div()
                            .id("time-remaining")
                            .test_support()
                            .font_family(mono_font())
                            .text_size(px(TIME_SIZE))
                            .text_color(overlay::dim())
                            .child(format!("-{}", fmt_time(remaining))),
                    ),
            )
            // 轨道带只有 4 高（设计稿），**点击热区**不能跟着变成 4：Slider 自己那层
            // 是 24 高，绝对定位铺在轨道带正中，命中测试走的是它，手感还是 24。
            //
            // Slider 自身没有 id / 无障碍信息（上游未实现），用带 id 的容器承载，
            // 既给读屏软件一个可命名的分组，也让 UI 测试能定位到进度条几何。
            .child(
                div().w_full().h(px(4.)).px(px(PROGRESS_SIDE_PAD)).child(
                    div()
                        .id("seek-bar")
                        .aria_label("播放进度")
                        .test_support()
                        .relative()
                        .w_full()
                        .h(px(4.))
                        // 窄窗口（< 808）时允许收窄，否则 760 的胶囊会溢出被裁掉
                        .child(
                            div()
                                .absolute()
                                .left(px(0.))
                                .right(px(0.))
                                .top(px(-10.))
                                .h(px(24.))
                                .flex()
                                .items_center()
                                .child(
                                    Slider::new(&self.seek)
                                        .bg(overlay::accent())
                                        // 圆点用白色（设计稿）；不给 text_color 时
                                        // 取的是主题的 slider_thumb，跟着皮肤漂
                                        .text_color(gpui_kit::rgb(0xFFFFFF))
                                        .w_full()
                                        .disabled(self.current.is_none()),
                                ),
                        ),
                ),
            )
    }

    /// 控制行：左音量 · 中走带（上一首 / -10s / 播放 / +10s / 下一首）· 右功能。
    ///
    /// 左、右两栏都是 `flex_1`（`flex: 1 1 0%`，等宽），中间那栏才落在窗口正中。
    /// 设计稿实测（1280 宽、胶囊 760 居中）：左组 158（ⓘ 20 + 14 + 喇叭 20 + 8 +
    /// 音量 96）、走带 220（28/28/52/28/28 + 4×14）、右组 224（6×24 + 5×16）——
    /// 两侧各剩 249，中间才真的居中。右组的**中心距 40** 是设计稿的节奏，别改成
    /// 「图标 20 + 间距 20」了：那样字形只有 15，比设计稿的 15~17 小一圈。
    fn render_controls_row(&self, cx: &mut Context<Self>) -> Div {
        let is_dark = cx.theme().mode.is_dark();
        let playing = self.playback_state().is_playing();
        let volume_icon = match self.volume_pct {
            0 => IconName::VolumeX,
            v if v < 50 => IconName::Volume1,
            _ => IconName::Volume2,
        };
        let is_video = self.video_active;
        let cinema = self.cinema;
        let playlist_open = self.playlist_open;
        let info_open = self.info_open;

        h_flex()
            .w_full()
            .h(px(CONTROLS_ROW))
            .items_center()
            // ---- 左：信息（ⓘ）+ 音量 ----
            //
            // 设计稿把 ⓘ 放在左侧控制组的**最左**、音量图标之前；右组已经塞了
            // 六个键（6×24 + 5×16 = 224），再挪进去就超宽了。
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(14.))
                    .when(is_video, |el| {
                        el.child(
                            Button::new("video-info")
                                .ghost()
                                .with_size(px(20.))
                                .icon(IconName::Info)
                                .text_color(if info_open {
                                    overlay::accent()
                                } else {
                                    overlay::fg()
                                })
                                .tooltip("视频信息")
                                .accessibility_label("视频信息")
                                .on_click(cx.listener(|this, _e, _w, cx| {
                                    this.toggle_info(cx);
                                })),
                        )
                    })
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                Icon::new(volume_icon)
                                    .size(px(20.))
                                    .text_color(overlay::fg()),
                            )
                            .child(
                                div()
                                    .id("volume-bar")
                                    .aria_label("音量")
                                    .w(px(VOLUME_WIDTH))
                                    .flex()
                                    .test_support()
                                    .child(
                                        Slider::new(&self.volume)
                                            .bg(overlay::accent())
                                            // 白圆点（设计稿）：不给 text_color 时取主题的
                                            // `slider_thumb`，换个皮肤连圆点颜色都跟着漂
                                            .text_color(gpui_kit::rgb(0xFFFFFF)),
                                    ),
                            ),
                    ),
            )
            // ---- 中：走带控制（不参与伸缩，恒在胶囊正中） ----
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(TRANSPORT_GAP))
                    .child(
                        chrome_button(
                            cx,
                            "prev",
                            Icon::default().data(ICON_PREV),
                            "上一首",
                            28.,
                            false,
                        )
                        .on_click(cx.listener(|this, _e, _w, cx| {
                            this.skip(-1, cx);
                        })),
                    )
                    .child(self.render_skip_button("seek-back-10", false, cx))
                    // 播放键：44 正圆 + 两侧各 4 的呼吸垫（设计稿圆和 ±10 之间比
                    // 图标之间宽 8）。设计稿那颗圆是**球面**的：
                    // `radial-gradient(120% 120% at 32% 18%, #A05BF5 → #8839EF 46% → #6E24D8)`
                    // 外加 1px 白色内高光。GPUI 没有径向渐变，用「竖向渐变 + 内阴影
                    // 高光 + 外发光」凑出同一个观感。
                    .child(
                        div().flex_shrink_0().p(px(PLAY_BREATH)).child(
                            Button::new("play-pause")
                                .primary()
                                .with_size(px(PLAY_BUTTON))
                                .rounded(px(PLAY_BUTTON / 2.))
                                .bg(crate::theme::v_gradient(
                                    overlay::play_top(),
                                    overlay::play_bottom(),
                                ))
                                .text_color(gpui_kit::rgb(0xFFFFFF))
                                .icon(Icon::default().data(if playing {
                                    ICON_PAUSE_FILL
                                } else {
                                    ICON_PLAY_FILL
                                }))
                                .shadow(vec![
                                    BoxShadow {
                                        color: overlay::play().opacity(0.35),
                                        offset: point(px(0.), px(4.)),
                                        blur_radius: px(18.),
                                        spread_radius: px(0.),
                                        inset: false,
                                    },
                                    // `inset 0 1px 0 rgba(255,255,255,.35)`
                                    BoxShadow {
                                        color: gpui_kit::rgba(0xFFFFFF59).into(),
                                        offset: point(px(0.), px(1.)),
                                        blur_radius: px(0.),
                                        spread_radius: px(0.),
                                        inset: true,
                                    },
                                ])
                                .tooltip(if playing { "暂停" } else { "播放" })
                                .accessibility_label(if playing { "暂停" } else { "播放" })
                                .on_click(cx.listener(|this, _e, _w, cx| {
                                    this.toggle_play(cx);
                                })),
                        ),
                    )
                    .child(self.render_skip_button("seek-fwd-10", true, cx))
                    .child(
                        chrome_button(
                            cx,
                            "next",
                            Icon::default().data(ICON_NEXT),
                            "下一首",
                            28.,
                            false,
                        )
                        .on_click(cx.listener(|this, _e, _w, cx| {
                            this.skip(1, cx);
                        })),
                    ),
            )
            // ---- 右：字幕 / 音轨 / 主题 / 列表 / 全屏（设计稿顺序，间距 16） ----
            //
            // 设计稿还有第 4 个「画中画」，见本轮改动说明：它要一个独立的置顶小窗
            // （视频要铺满整窗、腾掉标题带与胶囊），不是加个按钮的事，本轮先不挂
            // 死按钮上去。
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .justify_end()
                    .gap(px(16.))
                    .when(is_video, |el| {
                        el.child(
                            chrome_button(
                                cx,
                                "subtitles",
                                Icon::default().data(ICON_CC),
                                "切换字幕轨",
                                24.,
                                false,
                            )
                            .on_click(cx.listener(
                                |this, _e, _w, cx| {
                                    this.cycle_subtitles(cx);
                                },
                            )),
                        )
                        .child(
                            chrome_button(
                                cx,
                                "audio-track",
                                Icon::default().data(ICON_AUDIO_BARS),
                                "切换音轨",
                                24.,
                                false,
                            )
                            .on_click(cx.listener(
                                |this, _e, _w, cx| {
                                    this.cycle_audio_track(cx);
                                },
                            )),
                        )
                    })
                    .child(
                        chrome_button(
                            cx,
                            "theme-toggle",
                            Icon::new(if is_dark {
                                IconName::Sun
                            } else {
                                IconName::Moon
                            }),
                            if is_dark {
                                "切换到浅色主题"
                            } else {
                                "切换到深色主题"
                            },
                            24.,
                            false,
                        )
                        .on_click(cx.listener(|_this, _e, _w, cx| {
                            let next = if cx.theme().mode.is_dark() {
                                ThemeMode::Light
                            } else {
                                ThemeMode::Dark
                            };
                            Theme::change(next, None, cx);
                        })),
                    )
                    .child(
                        chrome_button(
                            cx,
                            "toggle-playlist",
                            Icon::default().data(ICON_LIST_LINES),
                            if playlist_open {
                                "收起播放列表"
                            } else {
                                "展开播放列表"
                            },
                            24.,
                            playlist_open,
                        )
                        .on_click(cx.listener(|this, _e, _w, cx| {
                            this.toggle_playlist(cx);
                        })),
                    )
                    .when(is_video, |el| {
                        el.child(
                            chrome_button(
                                cx,
                                "cinema",
                                Icon::new(if cinema {
                                    IconName::Minimize
                                } else {
                                    IconName::Maximize
                                }),
                                if cinema { "退出全屏" } else { "全屏" },
                                24.,
                                false,
                            )
                            .on_click(cx.listener(
                                |this, _e, window, cx| {
                                    this.toggle_cinema(window, cx);
                                },
                            )),
                        )
                    }),
            )
    }

    /// ±10 秒按钮：圆弧 + 箭头 + 数字。
    ///
    /// 图标自带 path 而不是用 `IconName::RotateCcw`：图标包（`default-icons.txt`）
    /// 只收了 `rotate-cw`，反向那个在包里根本没有，引用它会静默渲染成空白。
    /// 数字用独立文本层（设计稿同款做法）：SVG `<text>` 依赖字体环境，会掉字。
    fn render_skip_button(
        &self,
        id: &'static str,
        forward: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = if forward {
            "快进 10 秒"
        } else {
            "快退 10 秒"
        };
        let svg = if forward {
            ICON_FORWARD_10
        } else {
            ICON_REWIND_10
        };

        Button::new(id)
            .ghost()
            .with_size(px(28.))
            // 带子元素的 Button 会退回「普通按钮」分支，自己加 `px(size*0.2)`
            // 的左右内边距（28 → 35.2），走带的中心距就全歪了。这里显式压掉
            // padding 并钉死 28×28：走带总宽才是设计稿的 220。
            .p(px(0.))
            .w(px(28.))
            .h(px(28.))
            .text_color(overlay::fg())
            .tooltip(label)
            .accessibility_label(label)
            .on_click(cx.listener(move |this, _e, _w, cx| {
                this.seek_rel(if forward { 10.0 } else { -10.0 }, cx);
                cx.notify();
            }))
            .child(
                div()
                    .relative()
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    // 24 格视图缩到 22：弧线直径 ≈ 18，设计稿字形 20（±1 是描边）
                    .child(Icon::default().data(svg).size(px(22.)))
                    .child(
                        // 绝对定位必须给四边，否则它按"静态位置"排在图标后面，
                        // 数字会跑到圆弧右边去。
                        div()
                            .absolute()
                            .top(px(0.))
                            .left(px(0.))
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(7.))
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .text_color(overlay::fg())
                            .child("10"),
                    ),
            )
    }
}

impl Drop for PlayerView {
    fn drop(&mut self) {
        // 退出路径留痕：`on_app_quit` 只在显式 quit 时触发，窗口关闭这条路径
        // 不经过它（mpv 回收由 `VideoEngine::drop` 兜底）。要和
        // main 里的 "event loop returned" 对照，才能判断进程是怎么结束的。
        persistence::log_session(&format!(
            "player_view dropped (video_active={}, current={:?}, cinema={})",
            self.video_active, self.current, self.cinema
        ));
    }
}

/// UI 集成测试用的状态快照。
///
/// 只在测试构建存在：字段都是私有状态，`ui_tests` 是兄弟模块，无法直接读字段，
/// 用快照把「需要断言的事实」显式列出来，也避免为测试放开字段可见性。
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct ViewSnapshot {
    pub current: Option<usize>,
    pub seek_max: f32,
    pub last_error: Option<String>,
    pub playing: bool,
    /// 是否成功打开了音频输出设备（无设备时播放路径会降级）
    pub using_audio_device: bool,
    /// 当前画在标题带里的状态胶囊文案（视频模式专属）
    pub pill: Option<String>,
    /// 排队待弹的普通提示条数（>0 表示下一次 tick 会弹 toast/胶囊）
    pub pending_info: usize,
    /// 当前音量（百分点）——方向键上下要改它
    pub volume_pct: i32,
    /// 当前播放位置（秒）——方向键左右要改它
    pub position: f64,
}

#[cfg(test)]
impl PlayerView {
    pub(crate) fn snapshot(&self) -> ViewSnapshot {
        ViewSnapshot {
            current: self.current,
            seek_max: self.seek_max,
            last_error: self.last_error.as_ref().map(|s| s.to_string()),
            playing: self.playback_state().is_playing(),
            using_audio_device: self.engine.is_some(),
            pill: self
                .status_pill_live()
                .map(|(message, _, _)| message.to_string()),
            pending_info: self.pending_infos.len(),
            volume_pct: self.volume_pct,
            position: if self.video_active {
                self.video
                    .engine
                    .as_ref()
                    .map(|e| e.status().position)
                    .unwrap_or(0.0)
            } else {
                self.engine
                    .as_ref()
                    .map(|e| e.position_secs())
                    .unwrap_or(0.0)
            },
        }
    }

    /// 测试用：直接切「视频模式」开关。
    ///
    /// 真切换要起 mpv（`play_track` → `start_video`），测试环境里没有；
    /// 而「视频用胶囊、音频用 toast」只取决于 `video_active` 这一个布尔。
    pub(crate) fn test_set_video_mode(&mut self, on: bool) {
        self.video_active = on;
    }

    /// 测试用：把控制层的透明度钉到某个值（淡出到 0 之后区域必须收缩）。
    pub(crate) fn test_set_chrome_alpha(&mut self, alpha: f32) {
        self.chrome_alpha = alpha;
    }

    /// 测试用：直接切「最大化」状态。
    ///
    /// 真实切换要 `ShowWindowAsync`（无头窗口没有原生句柄），这里只验
    /// 「状态 → 按钮文案/图标」这一层——用户报的 bug 里，图标卡在「最大化」
    /// 也是症状之一，所以按钮 label 必须跟着状态走。
    pub(crate) fn test_set_maximized(&mut self, on: bool) {
        self.maximized = on;
    }

    /// 测试用：视频模式下压在画面上的那几块 chrome 矩形。
    ///
    /// 这几个矩形是「chrome 铺在哪」的唯一真相，单元测试靠它断言几何与淡出行为：
    /// 胶囊 760×92、面板右沿对齐、淡出后整块为空。
    pub(crate) fn test_chrome_rects(&self, window: &Window) -> Vec<UiRect> {
        self.chrome_layout(window).chrome_rects()
    }

    /// 测试用：模拟一次用户可见提示，并立刻走 tick 里的分发逻辑。
    pub(crate) fn test_notify(
        &mut self,
        message: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.report_info(message.to_string());
        self.flush_errors(window, cx);
    }

    /// 测试用：视频信息卡是否展开。
    pub(crate) fn test_info_open(&self) -> bool {
        self.info_open
    }

    /// 测试用：面板是否停在「章节」页。
    pub(crate) fn test_chapter_tab(&self) -> bool {
        self.chapter_tab
    }

    /// 测试用：循环 / 随机 / 排序三档的当前值（面板底部那一排按钮）。
    pub(crate) fn test_play_modes(&self) -> (RepeatMode, bool, persistence::SortOrder) {
        (
            self.settings.repeat,
            self.settings.shuffle,
            self.settings.sort,
        )
    }

    /// 测试用：列表里每一行的标题（排序后就靠它验顺序）。
    pub(crate) fn test_titles(&self, cx: &App) -> Vec<String> {
        self.playlist
            .read(cx)
            .delegate()
            .tracks()
            .iter()
            .map(|t| t.title.clone())
            .collect()
    }

    /// 测试用：走 tick 的同一条路径补时长，返回是否真的写进去了。
    ///
    /// 无头环境起不了 mpv，视频时长只能这样喂进来。
    pub(crate) fn test_backfill_duration(
        &mut self,
        row: usize,
        secs: f64,
        cx: &mut Context<Self>,
    ) -> bool {
        let before = self
            .playlist
            .read(cx)
            .delegate()
            .track(row)
            .and_then(|t| t.duration_secs);
        self.backfill_track_duration(row, secs, cx);
        self.playlist
            .read(cx)
            .delegate()
            .track(row)
            .and_then(|t| t.duration_secs)
            != before
    }

    /// 测试用：影院状态（`Esc` / 双击画面 / 最大化按钮都要能改它）。
    pub(crate) fn test_cinema(&self) -> bool {
        self.cinema
    }

    /// 测试用：自己记的最大化状态（真值在系统那边，见 `toggle_maximize`）。
    pub(crate) fn test_maximized(&self) -> bool {
        self.maximized
    }

    /// 测试用：走「双击画面」那条路（GPUI 的 `on_double_click` 最终也调它）。
    pub(crate) fn test_toggle_cinema(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_cinema(window, cx);
    }

    /// 测试用：点「最大化」按钮（全屏时它的语义是回到窗口）。
    pub(crate) fn test_toggle_maximize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_maximize(window, cx);
    }

    /// 测试用：音量滑块自己那份状态——拇指位置靠它，光改 `volume_pct` 界面不会动。
    ///
    /// 返回实体而不是值：调用方在 `view.read(cx)` 里拿不到第二份 `cx` 去 `read` 它。
    pub(crate) fn test_volume_slider(&self) -> Entity<SliderState> {
        self.volume.clone()
    }

    /// 测试用：把音频引擎按回停止态，模拟「播完了」。
    ///
    /// 真实流程里是 tick 发现 `track_finished()` 才 `stop()`，无头测试没有那个定时器。
    pub(crate) fn test_stop_engine(&mut self) {
        if let Some(engine) = &mut self.engine {
            engine.stop();
        }
    }
}

impl Render for PlayerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 拖放：GPUI 把系统的文件拖放转成内部拖拽（载荷类型是 ExternalPaths），
        // 松开时在鼠标下的元素上派发 MouseUp；把监听挂在根节点，整个窗口都是投放区。
        let drop_view = cx.weak_entity();
        let drag_stamp = self.drop_activity.clone();

        // 窗口表面那层底色不是我们铺的：gpui-component 的 `RootPlugin::style` 会给
        // 每个窗口的表面 refine 一层 `.bg(theme.tokens.background)`，它在 `Root` 里、
        // 不在我们的 view 下面——不改掉它，根节点再透明都会被盖住（实测截出来
        // 还是 #EFF1F5）。`Root` 实现了 `Styled`，实例样式 refine 在插件之后，
        // 所以逐窗覆盖一次即可。
        //
        // 必须在 render 里做：`PlayerView::new` 那会儿 `Root` 还没挂上窗口
        // （取不到，守卫会跳过）。无头测试里的窗口没有 `Root`，这里也就跳过。
        if !self.surface_transparent {
            if let Some(root) = window.root::<gpui_kit::base::Root>().flatten() {
                root.update(cx, |root, _cx| {
                    use gpui_kit::Refineable as _;
                    root.style().refine(
                        &gpui_kit::StyleRefinement::default().bg(gpui_kit::transparent_black()),
                    );
                });
                self.surface_transparent = true;
                cx.notify();
            }
        }

        // 视频模式整窗交给舞台（画面 + 黑边），根节点仍然是透明的；音频模式自己铺
        // 一层不透明底色，和以前完全一样。
        let background = if self.video_active {
            gpui_kit::transparent_black()
        } else {
            cx.theme().background
        };
        // 控制层透明度：视频播放中一段时间没输入就淡掉（见 `chrome_wanted`）。
        // 淡到 0 只影响绘制，元素还在树上——这是有意的：面板/信息卡本来就不该在
        // 淡出时被卸载（它们的展开态要留住），而透明元素的命中测试同样会被跳过。
        let chrome_alpha = self.chrome_alpha;
        let move_stamp = self.activity_moved.clone();

        div()
            .relative()
            .size_full()
            .bg(background)
            .text_color(cx.theme().foreground)
            // 任何鼠标移动都算"用户还在"：视频模式下它会把淡出的控制层叫回来。
            // 只写时间戳不 notify——每帧通知会让整窗跟着鼠标重绘。
            .on_mouse_move(move |_, _, _| move_stamp.set(Some(Instant::now())))
            // 外部文件拖到窗口上：只记时间戳，提示层由 tick 控制（见 drop_activity）
            .drag_over::<ExternalPaths>(move |style, _, _, _| {
                drag_stamp.set(Some(Instant::now()));
                style
            })
            .on_drop::<ExternalPaths>(move |paths, _window, cx| {
                let dropped: Vec<PathBuf> = paths.paths().to_vec();
                let _ = drop_view.update(cx, |view, cx| {
                    view.drop_activity.set(None);
                    view.import_dropped(dropped, cx);
                });
            })
            // 视频模式：舞台铺满**整个窗口**，标题带/胶囊/歌单压在画面上。
            // 必须排在最前面——GPUI 按子节点顺序绘制，后画的才在上面。
            .when(self.video_active, |el| {
                el.child(self.render_stage(cx))
                    // 底部渐隐遮罩：压在画面之上、胶囊之下。包一层只为拿 `.opacity`
                    // （遮罩自己是 `Stateful`，`IntoElement` 上没有透明度）。
                    .child(
                        div()
                            .absolute()
                            .top(px(0.))
                            .left(px(0.))
                            .size_full()
                            .opacity(chrome_alpha)
                            .child(self.render_vignette(cx)),
                    )
            })
            .child(
                v_flex()
                    .size_full()
                    .child(self.render_title_band(cx).opacity(chrome_alpha))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            // 音频模式舞台在流里（占标题带与胶囊之间）；视频模式已经在
                            // 上面单独铺满了，这里留空，chrome 才能浮在画面上。
                            // 歌单展开也不再挤画面——它本来就是悬浮面板。
                            .when(!self.video_active, |el| el.child(self.render_stage(cx))),
                    )
                    // 底部预留悬浮胶囊的位置（视频模式下这块是画布）
                    .child(div().h(px(BOTTOM_BAND)).flex_shrink_0()),
            )
            .child(self.render_bottom_chrome(cx).opacity(chrome_alpha))
            .when(self.playlist_open && !self.cinema, |el| {
                el.child(self.render_playlist_panel(cx))
            })
            .when(self.drop_hint_shown, |el| el.child(self.render_drop_hint()))
            .into_any_element()
    }
}

/// 路径比较用的归一化键：`\` 和 `/` 视为同一个字符。
///
/// 三处来源写法不统一——`load_track_meta` 用平台原生分隔符、命令行参数被我们改成 `/`、
/// 用户拖进来的又是原生分隔符，不归一化就会把同一个文件当成两条。
fn path_key(path: &str) -> String {
    path.replace('\\', "/")
}

/// 标题栏上的窗口按钮。
///
/// 尺寸按 Windows 10/11 的窗口按钮来：**46 宽、与标题栏等高**（不是跟着图标缩放的
/// 小方块），悬停铺满整块底色，关闭键悬停用系统红 `#C42B1C` + 白色图标。
/// 水平/垂直居中的 10×10 字形。`data()` 直接吃 SVG 源码：
/// `IconName::WindowMinimize` 这类内置图标是 24 网格 + 2px 描边，缩到 10px 后
/// 描边只剩半个像素，栅格化出来是一团糊；手写 10 网格 + 1px 描边才是 Windows 的样子。
fn caption_button(
    id: &'static str,
    glyph: &'static [u8],
    label: &'static str,
    close: bool,
    on_dark: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
    cx: &Context<PlayerView>,
) -> impl IntoElement {
    let theme = cx.theme();
    let (rest_fg, hover_bg, hover_fg): (gpui_kit::Hsla, gpui_kit::Hsla, gpui_kit::Hsla) = if close {
        (
            if on_dark {
                overlay::fg()
            } else {
                theme.secondary_foreground
            },
            gpui_kit::rgb(CLOSE_DANGER).into(),
            gpui_kit::rgb(0xFFFFFF).into(),
        )
    } else if on_dark {
        (
            overlay::fg(),
            gpui_kit::rgba(0xFFFFFF1F).into(),
            gpui_kit::rgb(0xFFFFFF).into(),
        )
    } else {
        (
            theme.secondary_foreground,
            theme.secondary_hover,
            theme.foreground,
        )
    };

    div()
        .id(id)
        // 注册进命中测试树：测试才能 `window.find("win-max").label()` 断言文案
        .test_support()
        .w(px(CAPTION_BUTTON_WIDTH))
        // 高度写死成标题带高度，**不能用 `h_full()`**：外面那圈 `h_flex()` 没有显式高度
        // （高度由内容撑开），`height: 100%` 在不定高父级上会退化成 auto，
        // 按钮就只剩图标那 10px 高——悬停底色成了一条横线（用户就是这么发现的）。
        .h(px(TITLE_BAND))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .text_color(rest_fg)
        .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
        .active(move |style| style.bg(hover_bg))
        .aria_label(label)
        .on_click(on_click)
        .child(Icon::default().data(glyph).size(px(10.)))
}

/// 信息卡里的空值占位。
const DASH: &str = "—";

/// 空串/nil 一律显示占位符：卡上留白会被当成"渲染坏了"。
fn non_empty(value: &str) -> String {
    if value.is_empty() {
        DASH.to_string()
    } else {
        value.to_string()
    }
}

/// 从 mpv 的编码串里取第一段。
///
/// mpv 的 `video-codec` / `audio-codec-name` 报的是**并列别名**：
/// `h264 (High) / AVC / MPEG-4 AVC / MPEG-4`、`aac / AAC (LC)`。全塞进卡片会
/// 冲出 240 宽的正文，而这一段里的人名按重要性递减——第一个就够用户认了。
pub(crate) fn short_codec(codec: &str) -> String {
    codec
        .split('/')
        .next()
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// 码率：`4.4 Mbps` / `192 kbps`（>= 1 Mbps 才用 Mbps，否则数字全是 0.x）。
fn fmt_bitrate(bps: f64) -> String {
    if bps <= 0.0 {
        DASH.to_string()
    } else if bps >= 1_000_000.0 {
        format!("{:.1} Mbps", bps / 1_000_000.0)
    } else {
        format!("{:.0} kbps", bps / 1000.0)
    }
}

/// 帧率：`23.976 fps`。整数帧率（30/60）不补小数位。
fn fmt_fps(fps: f64) -> String {
    if fps <= 0.0 {
        return DASH.to_string();
    }
    if (fps - fps.round()).abs() < 0.01 {
        format!("{:.0} fps", fps)
    } else {
        format!("{fps:.3} fps")
    }
}

/// 秒数格式化为 `m:ss`（过 1 小时写 `h:mm:ss`）。
fn fmt_time(secs: f64) -> String {
    let total = secs.max(0.0) as u32;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    // 只有真过了 1 小时才写小时位：`0:42:17` 比 `42:17` 多占 3 个字宽，
    // 而设计稿里两段时间是夹着轨道的，多一位就会把轨道挤歪
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
