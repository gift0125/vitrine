//! 主视图：播放器三栏布局 + 播放引擎状态同步。
//!
//! - 左侧：播放列表面板（List 虚拟滚动 + 拖拽排序 + 删除）
//! - 右上：封面占位区（音频）/ 视频舞台（视频）
//! - 底部：控制栏（上一首/播放暂停/下一首 + 进度条 + 音量 + 全屏 + 主题切换）
//!
//! 双引擎：音频走 `AudioEngine`（rodio），视频走 `VideoEngine`（mpv 子进程）。
//! 按文件扩展名自动切换；视频模式下 mpv 渲染进 `video_overlay` 创建的顶层叠加窗口，
//! 其位置由 render 阶段的 `canvas` 回调捕获的视频区 bounds 驱动。
//!
//! 100ms 定时器轮询位置并同步 SeekBar，播放结束自动切下一首。

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use player_core::{
    find_mpv, is_video_file, load_track_meta, scan_dir_for_tracks, AudioEngine, PlaybackState,
    SpawnOptions, TrackMeta, VideoEngine, VideoStatus,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::list::{List, ListEvent, ListState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Icon, InteractiveElementExt as _, Sizable as _, Theme,
    ThemeMode, WindowExt as _,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::persistence::{self, Settings};
use crate::playlist::PlaylistDelegate;
use crate::theme::overlay;
use crate::video_overlay::{self, ChromeRect, Hwnd, VideoOverlay};
use crate::window_actions;

/// 进度条量程未知时的兜底（元数据解析失败）。
const SEEK_FALLBACK_MAX: f32 = 600.0;
/// 轮询间隔（计划约定 100ms）。
const TICK_INTERVAL: Duration = Duration::from_millis(100);
/// 顶部标题带高度（系统标题栏已关闭，这一段是自绘拖拽区 + 窗口按钮）。
///
/// `theme::install` 也读它来给 toast 让位，所以是 `pub(crate)`。
pub(crate) const TITLE_BAND: f32 = 34.0;
/// 底部悬浮胶囊的内缩。
const CAPSULE_INSET: f32 = 24.0;
/// 底部为悬浮胶囊预留的高度。
///
/// 胶囊实高约 90（上下 padding 8 + 进度 10 + 时间行 14 + 走带 44 + 间距 8），
/// 加上 24 内缩再留 2px 余量：留多了窗口下方会空出一条死气沉沉的黑带，
/// 留少了 mpv 叠加窗会啃掉胶囊上沿。
const BOTTOM_BAND: f32 = 116.0;
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

/// 窗口尺寸变化后多久落盘一次（tick 计数，50 ≈ 5s）。
const WINDOW_SAVE_TICKS: usize = 50;

/// 订阅向量的固定下标。
const SUB_SEEK: usize = 1;

/// render 阶段捕获的视频区矩形（窗口逻辑坐标 x, y, w, h）。
/// 用 `Cell` 是因为 paint 回调只有 `&self` 上下文，且不需要触发重绘。
type VideoRectCell = Rc<Cell<Option<(f32, f32, f32, f32)>>>;

/// 视频播放运行时（叠加窗口 + mpv 进程）。
#[derive(Default)]
struct VideoRuntime {
    overlay: Option<VideoOverlay>,
    engine: Option<VideoEngine>,
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
    /// render 阶段由 canvas 捕获的视频区（窗口逻辑坐标 x, y, w, h）
    video_rect: VideoRectCell,
    /// render 阶段由 canvas 捕获的控制胶囊矩形。
    ///
    /// 视频模式要把画面区从主窗口的窗口区域里减掉（否则被整块盖住的视频窗会停止
    /// 出帧，见 `video_overlay` 模块头），而胶囊是浮在画面上的，得再加回区域里。
    /// 胶囊高度是内容撑开的（随字号/行距变），不能写死，只能量。
    capsule_rect: VideoRectCell,
    /// 当前已应用的窗口区域（用内容做键，避免每帧都打 SetWindowRgn——
    /// 那会让整窗重绘，画面跟着闪）。第二项是 scale 的位模式（跨屏换 DPI 要重算）。
    chrome_region: Option<(Vec<ChromeRect>, u32)>,
    /// 主窗口当前是不是圆角状态（视频模式才圆，见 `sync_overlay_rect`）。
    corners_rounded: bool,
    /// 上一次补算客户区（`refresh_client_area`）的时刻，用来限流
    client_kick: Option<Instant>,
    /// 上一次因为布局停在旧尺寸而补 `refresh()` 的时刻，用来限流
    layout_kick: Option<Instant>,
    /// 是否已经把窗口表面的底色改成透明（见 `Render` 里的说明）
    surface_transparent: bool,
    /// GPUI 主窗口句柄（叠加窗口的 z 序基准）
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
    /// 视频模式下 mpv 叠加窗盖住了整个内容区，gpui-component 的 toast 落在里面
    /// 等于不可见，所以改成画在标题带上的小胶囊（那条带子在叠加窗之上）。
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
        // 窗口移动/缩放时同步叠加窗口位置
        subscriptions.push(cx.observe_window_bounds(window, |this, window, _cx| {
            this.sync_overlay_rect(window);
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

        // 主窗口句柄（叠加窗口的 owner）；拿不到则视频功能降级
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
            video: VideoRuntime::default(),
            video_active: false,
            video_error: None,
            fit_done: None,
            fit_seen: None,
            video_rect,
            capsule_rect,
            chrome_region: None,
            corners_rounded: false,
            client_kick: None,
            layout_kick: None,
            surface_transparent: false,
            main_hwnd,
            maximized,
            zoom_pending: None,
            cinema: false,
            playlist_open: false,
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
                    _ => false,
                };
                if handled {
                    cx.stop_propagation();
                }
            }));

        // 100ms 轮询：进度同步 + 自动连播 + 叠加窗口跟随 + 窗口尺寸节流落盘
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
    /// 通知层，位置固定、会自己排队）；视频模式整块内容区被 mpv 叠加窗盖着，
    /// in-app toast 等于不可见，所以改成标题带里的小胶囊（见 `status_pill`）。
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

        // 命令行「打开方式」：等第一帧渲染完再起播，构造期间不发通知
        if let Some(index) = self.pending_autoplay.take() {
            self.play_track(index, cx);
        }

        // 拖到画面上的文件由视频窗代收（画面区不在主窗口的区域里，见 video_overlay）
        let dropped = video_overlay::take_dropped();
        if !dropped.is_empty() {
            self.import_dropped(dropped.into_iter().map(PathBuf::from).collect(), cx);
        }

        // 双击画面 → 全屏播放。视频窗只能把"发生了一次双击"记进队列（它不在 GPUI 的
        // 上下文里），真正的切换在这里做。延迟 ≤1 帧。
        for event in video_overlay::take_events() {
            match event {
                video_overlay::OverlayEvent::ToggleCinema => self.toggle_cinema(window, cx),
            }
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
                // 自动连播：到列表末尾则停止
                let count = self.playlist.read(cx).delegate().track_count();
                match self.current {
                    Some(i) if i + 1 < count => self.play_track(i + 1, cx),
                    _ => {
                        engine.stop();
                        cx.notify();
                    }
                }
            } else if !self.user_seeking {
                let pos = engine.position_secs();
                self.seek.update(cx, |state, cx| {
                    state.set_value(pos as f32, window, cx);
                });
            }
        }

        // 叠加窗口跟随窗口移动/缩放
        self.sync_overlay_rect(window);

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
        // 只画在左上角 853×34 那一小块里、右上角的窗口按钮根本不存在）。判据用
        // `viewport_size`（布局真正用的那个）和 `window.bounds()` 比。
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
        let viewport_stale = (viewport.width.as_f32() - bounds.width.as_f32()).abs() > 0.5
            || (viewport.height.as_f32() - bounds.height.as_f32()).abs() > 0.5;
        if viewport_stale {
            let due = self
                .layout_kick
                .is_none_or(|at| at.elapsed() >= LAYOUT_KICK_COOLDOWN);
            if due {
                self.layout_kick = Some(Instant::now());
                persistence::log_session(&format!(
                    "layout stale: viewport={:.0}x{:.0} window={:.0}x{:.0} -> kick",
                    viewport.width.as_f32(),
                    viewport.height.as_f32(),
                    bounds.width.as_f32(),
                    bounds.height.as_f32(),
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
                    if let Some(overlay) = &mut self.video.overlay {
                        overlay.hide();
                    }
                    cx.notify();
                }
            }
            return;
        }

        // 时长补齐：元数据缺失时以 mpv 报告的时长重建量程
        if status.duration > 0.5 && (status.duration as f32 - self.seek_max).abs() > 1.0 {
            self.rebuild_seek(status.duration as f32, cx);
        }

        // 窗体大小 = 内容大小：画面尺寸一报到就把窗口调成同样的尺寸/比例
        self.fit_to_video_source(status.width, status.height, window, cx);

        if status.finished {
            let count = self.playlist.read(cx).delegate().track_count();
            match self.current {
                Some(i) if i + 1 < count => self.play_track(i + 1, cx),
                _ => {
                    if let Some(engine) = &self.video.engine {
                        engine.stop();
                    }
                    self.video_active = false;
                    if let Some(overlay) = &mut self.video.overlay {
                        overlay.hide();
                    }
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

    /// 让叠加窗口对齐 canvas 捕获的视频区，并压在主窗口正下方。
    ///
    /// 视频窗在**下面**（见 `video_overlay` 模块头），所以这里只管位置和 z 序：
    /// 内容区不再需要开洞，主窗口的 chrome 半透明，画面自己会从玻璃底下透出来。
    fn sync_overlay_rect(&mut self, window: &Window) {
        // 圆角：视频播放时整窗变成圆角（画面上下两角来自视频叠加窗，标题带两角来自
        // 主窗口，见 `window_actions::set_rounded`）；回到音频模式恢复直角。
        // 只在状态变化时调——改变窗口属性会触发一次重绘，别每帧都打。
        if self.corners_rounded != self.video_active {
            self.corners_rounded = self.video_active;
            window_actions::set_rounded(self.main_hwnd, self.video_active);
        }
        if !self.video_active {
            if let Some(overlay) = &mut self.video.overlay {
                overlay.hide();
            }
            // 退出视频模式：主窗口恢复整窗矩形（否则界面还是被剪成 chrome 那几块）
            if self.chrome_region.take().is_some() {
                video_overlay::clear_region(self.main_hwnd);
            }
            return;
        }
        let Some((x, y, w, h)) = self.video_rect.get() else {
            return;
        };
        if w < 8.0 || h < 8.0 {
            return;
        }
        let scale = window.scale_factor();
        // 铺满**外框**：客户区外那一圈边框留在主窗口的区域里（要不没法拖动缩放），
        // 而那一圈我们不画东西，只铺到客户区就会从边框透出桌面。
        let (wx, wy, ww, wh) = video_overlay::window_rect(self.main_hwnd);
        let physical = (wx, wy, ww, wh);
        if let Some(overlay) = &mut self.video.overlay {
            // 最小化时它得跟着消失（没有 owner 关系，系统不再代劳）
            if overlay.follow_minimize() {
                return;
            }
            overlay.set_rect(physical.0, physical.1, physical.2, physical.3);
            if !overlay.is_visible() {
                overlay.show();
            }
        }
        // 主窗口把画面区让出去：系统只认窗口区域，不减掉这块，被完全盖住的视频窗
        // 就会被判成「不可见」而停止出帧（画面一片空白，连 PrintWindow 都是黑的）。
        //
        // 去重键**不含窗口位置**：区域坐标是相对窗口矩形的，纯移动不需要重设；
        // 带上位置的话拖动/贴边过程中每一步都会 `SetWindowRgn`（整窗重绘 + 画面抖）。
        // 尺寸变化已经由 chrome 自己的坐标/宽高体现，scale 单独记一笔（跨屏换 DPI）。
        let chrome = self.chrome_rects(window);
        let key = (chrome, scale.to_bits());
        if self.chrome_region.as_ref() != Some(&key) {
            if std::env::var_os("SMP_REGION_TRACE").is_some() {
                persistence::log_session(&format!(
                    "region <- stage=({x:.1},{y:.1},{w:.1},{h:.1}) chrome={:?} scale={scale}",
                    key.0
                ));
            }
            video_overlay::set_region(self.main_hwnd, (x, y, w, h), &key.0, scale);
            self.chrome_region = Some(key);
        }
    }

    /// 视频模式下主窗口区域里要**留下**的 chrome（逻辑像素）。
    ///
    /// 画面铺满整窗，所以除了这几块以外全让出去——让出去的部分露出下面的视频窗，
    /// 这几块留在主窗口里、用半透明玻璃画在画面之上。
    fn chrome_rects(&self, window: &Window) -> Vec<ChromeRect> {
        let bounds = window.bounds();
        let width = bounds.size.width.as_f32();
        let height = bounds.size.height.as_f32();

        // 抗锯齿留白：区域切得太贴边会把胶囊的白描边切掉半个像素
        const PAD: f32 = 1.5;
        const CAPSULE_RADIUS: f32 = 12.0;

        let mut chrome = Vec::with_capacity(3);
        // 标题带整条留下：右端还有窗口按钮，中间截断会很奇怪
        chrome.push(ChromeRect::rect(0., 0., width, TITLE_BAND));

        if let Some((x, y, w, h)) = self.capsule_rect.get() {
            chrome.push(ChromeRect::rounded(
                x - PAD,
                y - PAD,
                w + PAD * 2.,
                h + PAD * 2.,
                CAPSULE_RADIUS + PAD,
            ));
        }

        // 歌单：展开时浮在画面右侧
        if self.playlist_open && !self.cinema {
            let panel_x = width - 12. - PLAYLIST_WIDTH;
            let panel_y = TITLE_BAND + 6.;
            let panel_h = (height - panel_y - BOTTOM_BAND).max(0.);
            chrome.push(ChromeRect::rounded(
                panel_x - PAD,
                panel_y - PAD,
                PLAYLIST_WIDTH + PAD * 2.,
                panel_h + PAD * 2.,
                12. + PAD,
            ));
        }
        chrome
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

    /// 进入视频模式：确保叠加窗口与 mpv 进程就绪，然后加载文件。
    fn start_video(&mut self, track: &TrackMeta, cx: &mut Context<Self>) {
        self.video_error = None;
        self.video_active = true;
        // 换片了：等新片子的尺寸出来再调窗口（旧尺寸不能沿用）
        self.fit_done = None;
        self.fit_seen = None;

        if self.main_hwnd == 0 {
            self.video_active = false;
            self.video_error = Some("无法获取主窗口句柄，视频渲染不可用".into());
            self.report_error("视频不可用：无法获取主窗口句柄");
            return;
        }

        if self.video.overlay.is_none() {
            self.video.overlay = VideoOverlay::create(self.main_hwnd);
        }
        let Some(overlay) = &self.video.overlay else {
            self.video_active = false;
            self.video_error = Some("无法创建视频叠加窗口".into());
            self.report_error("视频不可用：无法创建叠加窗口");
            return;
        };
        let wid = overlay.hwnd();

        // 复用进程；连接已断开则重建
        let need_spawn = match &self.video.engine {
            Some(engine) => !engine.is_connected(),
            None => true,
        };
        if need_spawn {
            let Some(mpv) = find_mpv() else {
                self.video_active = false;
                self.video_error =
                    Some("未找到 mpv.exe（可用环境变量 SMP_MPV_PATH 指定路径）".into());
                self.report_error("视频不可用：未找到 mpv.exe（设置 SMP_MPV_PATH 可指定路径）");
                return;
            };
            match VideoEngine::spawn(SpawnOptions {
                mpv_path: mpv,
                wid: Some(wid),
                volume: self.settings.volume,
                extra_args: Vec::new(),
            }) {
                Ok(engine) => self.video.engine = Some(engine),
                Err(err) => {
                    self.video_active = false;
                    self.video_error = Some(format!("mpv 启动失败：{err}"));
                    self.report_error(format!("视频不可用：mpv 启动失败（{err}）"));
                    return;
                }
            }
        }

        if let Some(engine) = &self.video.engine {
            engine.set_volume(self.settings.volume);
            engine.load(&track.path);
        }
        let max = track.duration_secs.unwrap_or(SEEK_FALLBACK_MAX as f64) as f32;
        self.rebuild_seek(max, cx);
    }

    /// 退出视频模式（停止 mpv、隐藏叠加窗口）。
    fn leave_video(&mut self) {
        if let Some(engine) = &self.video.engine {
            engine.stop();
        }
        if let Some(overlay) = &mut self.video.overlay {
            overlay.hide();
        }
        self.video_active = false;
        self.video_error = None;
    }

    fn shutdown_video(&mut self) {
        self.video_active = false;
        if let Some(overlay) = &mut self.video.overlay {
            overlay.hide();
        }
        if let Some(engine) = &mut self.video.engine {
            engine.shutdown();
        }
        self.video.engine = None;
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

    /// 上一首/下一首（手动触发：列表末尾循环回开头）。
    fn skip(&mut self, delta: i32, cx: &mut Context<Self>) {
        let count = self.playlist.read(cx).delegate().track_count();
        if count == 0 {
            return;
        }
        let idx = match self.current {
            Some(i) => (i as i32 + delta).rem_euclid(count as i32) as usize,
            None => 0,
        };
        self.play_track(idx, cx);
    }

    /// 进度跳转：按当前模式路由到音频/视频引擎。
    fn seek_to(&mut self, secs: f64) {
        if self.video_active {
            if let Some(engine) = &self.video.engine {
                engine.seek(secs);
            }
        } else if let Some(engine) = &mut self.engine {
            engine.seek(secs);
        }
    }

    /// 相对跳转（±10 秒按钮）：夹到 [0, 时长] 之间，避免越界后播放器自己回弹。
    fn seek_rel(&mut self, delta: f64) {
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
        let target = (position + delta).clamp(0.0, self.seek_max as f64);
        self.seek_to(target);
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
        // 视频窗要知道这件事：全屏时不能再按住画面拖窗体
        video_overlay::set_cinema(on);
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
    /// 视频模式只能放在标题带里：画面那一块已经从主窗口的窗口区域里减掉了（见
    /// `chrome_rects`），主窗口在那块画什么都不会显示——居中卡片只会看到四周
    /// 暗下去的边框、中间什么都没有。
    fn render_drop_hint(&self) -> Div {
        let scrim = div()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .bg(gpui_kit::rgba(0x08090CC0));

        if self.video_active {
            return scrim
                .flex()
                .items_center()
                .justify_center()
                .h(px(TITLE_BAND))
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(14.))
                        .py(px(5.))
                        .rounded(px(999.))
                        .bg(overlay::glass())
                        .border_1()
                        .border_color(overlay::border())
                        .child(
                            Icon::new(IconName::Music)
                                .size(px(13.))
                                .text_color(overlay::accent()),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(overlay::fg())
                                .child("松开即可导入并播放"),
                        ),
                );
        }

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
    // 第七轮起这两个偏差都没了：mpv 窗口改到主窗口**下面**，画面铺满整窗，chrome
    // 用半透明玻璃真的压在画面上（主窗口把画面区从自己的窗口区域里减掉，见
    // `video_overlay::set_region`）。唯一剩下的是**没有区域级 backdrop blur**：
    // Windows 的 acrylic 只能整窗，所以玻璃只有 72% 深色这一半效果。

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
    /// 绝对定位（内容高度 0），不撑满交叉轴的话舞台高度会塌成 0——视频区 canvas
    /// 捕获到 `0x0`，mpv 叠加窗就永远定不到位（表现为黑屏但有声音）。
    fn render_stage(&self, cx: &Context<Self>) -> impl gpui_kit::IntoElement {
        let stage = div().id("stage").test_support().relative();

        let stage = if self.video_active {
            // 画面铺满整窗。这里**不铺任何底色**：视频窗在主窗口下面，主窗口这块
            // 保持 alpha 0，画面才透得上来（铺一层深色就又把画面盖住了）。
            //
            // 注意这块区域最终会被 `chrome_rects` 从主窗口的窗口区域里减掉——
            // 它压根不属于主窗口，所以按住画面拖动、拖放文件都发生在视频窗上
            // （`video_overlay` 把两者转回主窗口）。
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

    /// 视频：一块**透明**的占位（真正的像素来自下面那个 mpv 叠加窗）。
    ///
    /// 这里曾经铺的是 `overlay::canvas()`（不透明深色），因为那时叠加窗在**上面**、
    /// 根本透不过来。现在视频窗在主窗口下面，这一层必须留空，否则又把画面盖住了。
    /// 只有出错时才铺回去——那时没有画面可透，浮一块透明的错误提示反而看不清。
    fn render_video_surface(&self, _cx: &Context<Self>) -> Div {
        let base = div().absolute().top(px(0.)).left(px(0.)).size_full();
        let base = if self.video_error.is_some() {
            base.bg(overlay::canvas())
        } else {
            base
        };

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
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(overlay::dim())
                            .child("提示：安装 mpv 或设置 SMP_MPV_PATH 指向 mpv.exe"),
                    ),
            );
        }

        // 每帧把视频区 bounds 写回 Cell，tick 据此定位叠加窗口
        let slot = self.video_rect.clone();
        let stage = canvas(
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
        .size_full();

        base.child(stage)
    }

    /// 悬浮播放列表面板：320 宽、Latte 底、浮在画面上（默认可收起）。
    fn render_playlist_panel(&self, cx: &Context<Self>) -> impl gpui_kit::IntoElement {
        let theme = cx.theme();
        let count = self.playlist.read(cx).delegate().track_count();

        v_flex()
            .id("playlist-panel")
            .test_support()
            .absolute()
            .top(px(TITLE_BAND + 6.))
            .right(px(12.))
            .bottom(px(BOTTOM_BAND))
            .w(px(PLAYLIST_WIDTH))
            .rounded(px(12.))
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .overflow_hidden()
            .px(px(16.))
            .pt(px(14.))
            .pb(px(10.))
            .gap(px(8.))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("播放列表"),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .pr(px(4.))
                            .text_size(px(12.))
                            .text_color(theme.muted_foreground)
                            .child(format!("{count} 个文件")),
                    )
                    // 设计稿没有这三个入口，但功能上必须有；收成 xsmall ghost 图标，
                    // 颜色交给 muted 层级，不抢「播放列表」标题的视觉权重。
                    .child(
                        Button::new("add-file")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .tooltip("添加文件")
                            .accessibility_label("添加文件")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.add_files(cx);
                            })),
                    )
                    .child(
                        Button::new("add-folder")
                            .ghost()
                            .xsmall()
                            .icon(IconName::FolderOpen)
                            .tooltip("添加文件夹")
                            .accessibility_label("添加文件夹")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.add_folder(cx);
                            })),
                    )
                    .child(
                        Button::new("clear-playlist")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Delete)
                            .tooltip("清空播放列表")
                            .accessibility_label("清空播放列表")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.clear_playlist(cx);
                            })),
                    ),
            )
            // 1px Surface0 分隔线（设计稿唯一的分隔方式）
            .child(div().w_full().h(px(1.)).flex_shrink_0().bg(theme.border))
            // List 默认带 8px 左右内边距（会把条目挤得比标题缩进一格），压成 0
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(List::new(&self.playlist).px(px(0.))),
            )
    }

    /// 悬浮胶囊：进度轨道 + 时间行 + 控制行，深色玻璃。
    ///
    /// 尺寸是按「占窗口多少」调的，不是按像素堆的：三条横排控件加上上下 padding
    /// 曾经撑到 107 高（窗口 680 时占 15.7%），远看像一块压在下沿的黑板。
    /// 现在压到 ~90（占 13%），间距也从 12/6 收到 8/4，视觉重量才和画面匹配。
    fn render_capsule(&self, cx: &mut Context<Self>) -> Div {
        // 外框负责定位/底色/圆角，内层负责 padding —— 夹在中间的 canvas 量到的
        // 就是胶囊的真实外框（含 padding），算区域时不会差一圈。
        let slot = self.capsule_rect.clone();
        div()
            .absolute()
            .left(px(CAPSULE_INSET))
            .right(px(CAPSULE_INSET))
            .bottom(px(CAPSULE_INSET))
            .rounded(px(12.))
            .bg(overlay::glass())
            .border_1()
            .border_color(overlay::border())
            .shadow_lg()
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
                    .px(px(16.))
                    .py(px(8.))
                    .gap(px(4.))
                    .child(self.render_seek_row(cx))
                    .child(self.render_controls_row(cx)),
            )
    }

    /// 进度：轨道在上、时间行在下（左当前 / 右剩余）。
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

        v_flex()
            .w_full()
            .gap(px(2.))
            // Slider 自身没有 id / 无障碍信息（上游未实现），用带 id 的容器承载，
            // 既给读屏软件一个可命名的分组，也让 UI 测试能定位到进度条几何。
            .child(
                div()
                    .id("seek-bar")
                    .aria_label("播放进度")
                    .w_full()
                    .h(px(10.))
                    .flex()
                    .items_center()
                    .test_support()
                    .child(
                        Slider::new(&self.seek)
                            .bg(overlay::accent())
                            .flex_1()
                            .disabled(self.current.is_none()),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .font_family(mono_font())
                            .text_size(px(10.))
                            .text_color(overlay::fg())
                            .child(fmt_time(position)),
                    )
                    .child(
                        div()
                            .font_family(mono_font())
                            .text_size(px(10.))
                            .text_color(overlay::dim())
                            .child(format!("剩余 {}", fmt_time(remaining))),
                    ),
            )
    }

    /// 控制行：左音量 · 中走带（上一首 / -10s / 播放 / +10s / 下一首）· 右功能。
    ///
    /// 左、右两栏都是 `flex_1`（`flex: 1 1 0%`，等宽），中间那栏才落在窗口正中。
    fn render_controls_row(&self, cx: &mut Context<Self>) -> Div {
        let is_dark = cx.theme().mode.is_dark();
        let playing = self.playback_state().is_playing();
        let play_icon = if playing {
            IconName::Pause
        } else {
            IconName::Play
        };
        let volume_icon = match self.volume_pct {
            0 => IconName::VolumeX,
            v if v < 50 => IconName::Volume1,
            _ => IconName::Volume2,
        };
        let is_video = self.video_active;
        let cinema = self.cinema;
        let playlist_open = self.playlist_open;

        h_flex()
            .w_full()
            .h(px(44.))
            .items_center()
            .gap(px(12.))
            // ---- 左：音量 ----
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        Icon::new(volume_icon)
                            .size(px(16.))
                            .text_color(overlay::fg()),
                    )
                    .child(
                        div()
                            .id("volume-bar")
                            .aria_label("音量")
                            .w(px(64.))
                            .flex()
                            .test_support()
                            .child(Slider::new(&self.volume).bg(overlay::accent())),
                    ),
            )
            // ---- 中：走带控制（不参与伸缩，恒在窗口正中） ----
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(20.))
                    .child(
                        Button::new("prev")
                            .ghost()
                            .with_size(px(28.))
                            .icon(IconName::SkipBack)
                            .text_color(overlay::fg())
                            .tooltip("上一首")
                            .accessibility_label("上一首")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.skip(-1, cx);
                            })),
                    )
                    .child(self.render_skip_button("seek-back-10", false, cx))
                    // 设计稿：44 正圆、品牌 Mauve 实心、白色图标
                    .child(
                        Button::new("play-pause")
                            .primary()
                            .with_size(px(44.))
                            .rounded(px(22.))
                            .bg(overlay::play())
                            .text_color(gpui_kit::rgb(0xFFFFFF))
                            .icon(play_icon)
                            .tooltip(if playing { "暂停" } else { "播放" })
                            .accessibility_label(if playing { "暂停" } else { "播放" })
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.toggle_play(cx);
                            })),
                    )
                    .child(self.render_skip_button("seek-fwd-10", true, cx))
                    .child(
                        Button::new("next")
                            .ghost()
                            .with_size(px(28.))
                            .icon(IconName::SkipForward)
                            .text_color(overlay::fg())
                            .tooltip("下一首")
                            .accessibility_label("下一首")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.skip(1, cx);
                            })),
                    ),
            )
            // ---- 右：列表 / 全屏 / 换肤 ----
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .justify_end()
                    .gap(px(6.))
                    .child(
                        Button::new("toggle-playlist")
                            .ghost()
                            .with_size(px(28.))
                            .icon(IconName::ListMusic)
                            .text_color(if playlist_open {
                                overlay::accent()
                            } else {
                                overlay::fg()
                            })
                            .tooltip(if playlist_open {
                                "收起播放列表"
                            } else {
                                "展开播放列表"
                            })
                            .accessibility_label(if playlist_open {
                                "收起播放列表"
                            } else {
                                "展开播放列表"
                            })
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.toggle_playlist(cx);
                            })),
                    )
                    .when(is_video, |el| {
                        el.child(
                            Button::new("cinema")
                                .ghost()
                                .with_size(px(28.))
                                .icon(if cinema {
                                    IconName::Minimize
                                } else {
                                    IconName::Maximize
                                })
                                .text_color(overlay::fg())
                                .tooltip(if cinema { "退出全屏" } else { "全屏" })
                                .accessibility_label(if cinema {
                                    "退出全屏"
                                } else {
                                    "进入全屏"
                                })
                                .on_click(cx.listener(|this, _e, window, cx| {
                                    this.toggle_cinema(window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("theme-toggle")
                            .ghost()
                            .with_size(px(28.))
                            .icon(if is_dark {
                                IconName::Sun
                            } else {
                                IconName::Moon
                            })
                            .text_color(overlay::fg())
                            .tooltip("明暗主题切换")
                            .accessibility_label(if is_dark {
                                "切换到浅色主题"
                            } else {
                                "切换到深色主题"
                            })
                            .on_click(cx.listener(|_this, _e, _w, cx| {
                                let next = if cx.theme().mode.is_dark() {
                                    ThemeMode::Light
                                } else {
                                    ThemeMode::Dark
                                };
                                Theme::change(next, None, cx);
                            })),
                    ),
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
            .text_color(overlay::fg())
            .tooltip(label)
            .accessibility_label(label)
            .on_click(cx.listener(move |this, _e, _w, cx| {
                this.seek_rel(if forward { 10.0 } else { -10.0 });
                cx.notify();
            }))
            .child(
                div()
                    .relative()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::default().data(svg).size(px(18.)))
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
        }
    }

    /// 测试用：直接切「视频模式」开关。
    ///
    /// 真切换要起 mpv（`play_track` → `start_video`），测试环境里没有；
    /// 而「视频用胶囊、音频用 toast」只取决于 `video_active` 这一个布尔。
    pub(crate) fn test_set_video_mode(&mut self, on: bool) {
        self.video_active = on;
    }

    /// 测试用：直接切「最大化」状态。
    ///
    /// 真实切换要 `ShowWindowAsync`（无头窗口没有原生句柄），这里只验
    /// 「状态 → 按钮文案/图标」这一层——用户报的 bug 里，图标卡在「最大化」
    /// 也是症状之一，所以按钮 label 必须跟着状态走。
    pub(crate) fn test_set_maximized(&mut self, on: bool) {
        self.maximized = on;
    }

    /// 测试用：视频模式下主窗口区域里会留下哪些 chrome 矩形。
    ///
    /// 这几个矩形就是「哪些像素还归主窗口」的全部真相——画面区被减掉之后，
    /// 没被列进来的 chrome 会被系统直接剪掉，而**剪掉的部分看着像"没画"**，
    /// 所以必须有测试盯着它和实际布局对齐，不能靠肉眼看截图。
    pub(crate) fn test_chrome_rects(
        &self,
        window: &Window,
    ) -> Vec<crate::video_overlay::ChromeRect> {
        self.chrome_rects(window)
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

    /// 测试用：影院状态（`Esc` / 双击画面 / 最大化按钮都要能改它）。
    pub(crate) fn test_cinema(&self) -> bool {
        self.cinema
    }

    /// 测试用：自己记的最大化状态（真值在系统那边，见 `toggle_maximize`）。
    pub(crate) fn test_maximized(&self) -> bool {
        self.maximized
    }

    /// 测试用：走「双击画面」那条路——视频窗把事件排进队列，tick 取走执行。
    ///
    /// 队列本身是 Win32 侧的静态量（真窗口才有），无头测试直接调同一条处理路径，
    /// 覆盖的是「事件 → 切换影院」这段逻辑。
    pub(crate) fn test_on_overlay_event(
        &mut self,
        event: crate::video_overlay::OverlayEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            crate::video_overlay::OverlayEvent::ToggleCinema => self.toggle_cinema(window, cx),
        }
    }

    /// 测试用：点「最大化」按钮（全屏时它的语义是回到窗口）。
    pub(crate) fn test_toggle_maximize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_maximize(window, cx);
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

        // 视频模式整窗留空（alpha 0）：窗口表面已经透明，
        // 这块空出来的地方露出的就是下面那个 mpv 叠加窗。
        // 音频模式仍然自己铺一层不透明底色，和以前完全一样。
        let background = if self.video_active {
            gpui_kit::transparent_black()
        } else {
            cx.theme().background
        };

        div()
            .relative()
            .size_full()
            .bg(background)
            .text_color(cx.theme().foreground)
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
            // 视频模式：舞台铺满**整个窗口**，标题带/胶囊/歌单压在画面上（见 chrome_holes）。
            // 必须排在最前面——GPUI 按子节点顺序绘制，后画的才在上面。
            .when(self.video_active, |el| el.child(self.render_stage(cx)))
            .child(
                v_flex()
                    .size_full()
                    .child(self.render_title_band(cx))
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
            .child(self.render_capsule(cx))
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

/// 秒数格式化为 `m:ss`。
fn fmt_time(secs: f64) -> String {
    let total = secs.max(0.0) as u32;
    format!("{}:{:02}", total / 60, total % 60)
}
