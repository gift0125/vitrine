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
use crate::video_overlay::{self, Hwnd, VideoOverlay};

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
    /// render 阶段由 canvas 捕获的视频区（窗口逻辑坐标 x, y, w, h）
    video_rect: VideoRectCell,
    /// GPUI 主窗口句柄（叠加窗口的 owner）
    main_hwnd: Hwnd,
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

        let this = Self {
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
            video_rect,
            main_hwnd,
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

        // 拖到视频区上的文件由叠加窗口代收（见 video_overlay::take_dropped）
        let dropped = video_overlay::take_dropped();
        if !dropped.is_empty() {
            self.import_dropped(dropped.into_iter().map(PathBuf::from).collect(), cx);
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

        // 影院模式只对视频成立：自动连播切到音频（或无视频可播）时必须退出全屏，
        // 否则影院按钮消失、用户被困在全屏里。
        if self.cinema && !self.video_active {
            self.cinema = false;
            if window.is_fullscreen() {
                window.toggle_fullscreen();
            }
            cx.notify();
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

        // 窗口尺寸节流落盘（变化后 ~5s 保存一次）
        // 影院模式下 bounds 是全屏尺寸，不能当窗口尺寸存（否则下次启动开成全屏大小）。
        let bounds = window.bounds();
        let size = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
        if self.cinema || size == self.last_saved_size {
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

    /// 让叠加窗口对齐 canvas 捕获的视频区。
    fn sync_overlay_rect(&mut self, window: &Window) {
        if !self.video_active {
            if let Some(overlay) = &mut self.video.overlay {
                overlay.hide();
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
        let (origin_x, origin_y) = video_overlay::client_origin(self.main_hwnd);
        let physical = (
            origin_x + (x * scale).round() as i32,
            origin_y + (y * scale).round() as i32,
            (w * scale).round() as i32,
            (h * scale).round() as i32,
        );
        if let Some(overlay) = &mut self.video.overlay {
            overlay.set_rect(physical.0, physical.1, physical.2, physical.3);
            if !overlay.is_visible() {
                overlay.show();
            }
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

    fn toggle_cinema(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cinema = !self.cinema;
        if window.is_fullscreen() != self.cinema {
            window.toggle_fullscreen();
        }
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
        let added = fresh.len();
        if added > 0 {
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
            Some(index) => {
                self.play_track(index, cx);
                let title = self
                    .current_track(cx)
                    .map(|track| track.title)
                    .unwrap_or_default();
                self.report_info(if added > 0 {
                    format!("已导入 {added} 个文件 · 正在播放「{title}」")
                } else {
                    format!("正在播放「{title}」")
                });
            }
            None => cx.notify(),
        }
    }

    /// 拖入文件时的提示层（盖住整个窗口）。
    ///
    /// 视频模式只能放在标题带里：画面那一块被 mpv 的叠加窗口盖着，居中卡片根本看不见，
    /// 只会看到四周暗下去、中间什么都没有。
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
    // 架构限制（mpv 渲染在独立顶层窗口里，会盖住 GPUI 控件）导致两处必要偏差：
    //   1. 视频画面只占「标题带以下、胶囊以上」，胶囊落在深色画布上而不是压在画面上；
    //   2. 视频模式下列表展开时，画面右侧让位，否则面板会被 mpv 盖住。
    //   音频模式没有这个限制，封面可以真正铺满整窗。

    /// 标题带只压在两种底色上：音频模式是主题 `background`，视频模式是深色画布。
    /// 前景色必须跟着**底色**走，不能跟着应用主题走——浅色主题 + 深色画布的组合下
    /// 直接用 `muted_foreground` / `secondary_foreground` 会得到 3:1 左右的对比度，
    /// 曲名和窗口按钮看着像没渲染。
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
    /// 底色一定按浮层口径画：它只在 `video_active` 时出现，那一块 chrome 是深色画布。
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
    fn render_title_band(&self, window: &Window, cx: &Context<Self>) -> Div {
        // 最大化时 Windows 会把"最大化"换成"还原"图标
        let maximized = window.is_maximized();
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
                    .on_double_click(|_, window, _| window.zoom_window())
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
                        |_, window, _| window.zoom_window(),
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

    /// 舞台：视频/音频两种内容，占满标题带与底部胶囊之间的区域。
    ///
    /// `self_stretch` 不能少：`h_flex()` 默认 `items_center`，舞台的子元素又全是
    /// 绝对定位（内容高度 0），不撑满交叉轴的话舞台高度会塌成 0——视频区 canvas
    /// 捕获到 `0x0`，mpv 叠加窗就永远定不到位（表现为黑屏但有声音）。
    fn render_stage(&self, cx: &Context<Self>) -> impl gpui_kit::IntoElement {
        let stage = div()
            .id("stage")
            .test_support()
            .flex_1()
            .self_stretch()
            .min_w_0()
            .min_h_0()
            .relative()
            .bg(if self.video_active {
                overlay::canvas()
            } else {
                cx.theme().background
            });

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

    /// 视频：黑色画布（mpv 渲染进叠加窗口，这里只提供位置与错误占位）。
    fn render_video_surface(&self, _cx: &Context<Self>) -> Div {
        let base = div()
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
    fn render_playlist_panel(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let count = self.playlist.read(cx).delegate().track_count();

        v_flex()
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
        v_flex()
            .absolute()
            .left(px(CAPSULE_INSET))
            .right(px(CAPSULE_INSET))
            .bottom(px(CAPSULE_INSET))
            .px(px(16.))
            .py(px(8.))
            .gap(px(4.))
            .rounded(px(12.))
            .bg(overlay::glass())
            .border_1()
            .border_color(overlay::border())
            .shadow_lg()
            .child(self.render_seek_row(cx))
            .child(self.render_controls_row(cx))
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
        }
    }

    /// 测试用：直接切「视频模式」开关。
    ///
    /// 真切换要起 mpv（`play_track` → `start_video`），测试环境里没有；
    /// 而「视频用胶囊、音频用 toast」只取决于 `video_active` 这一个布尔。
    pub(crate) fn test_set_video_mode(&mut self, on: bool) {
        self.video_active = on;
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
}

impl Render for PlayerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 拖放：GPUI 把系统的文件拖放转成内部拖拽（载荷类型是 ExternalPaths），
        // 松开时在鼠标下的元素上派发 MouseUp；把监听挂在根节点，整个窗口都是投放区。
        let drop_view = cx.weak_entity();
        let drag_stamp = self.drop_activity.clone();

        div()
            .relative()
            .size_full()
            .bg(if self.video_active {
                overlay::canvas()
            } else {
                cx.theme().background
            })
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
            .child(
                v_flex()
                    .size_full()
                    .child(self.render_title_band(window, cx))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .child(self.render_stage(cx))
                            // 视频模式下列表展开 → 画面右侧让位，
                            // 否则悬浮面板会被 mpv 叠加窗整个盖住。
                            .when(self.video_active && self.playlist_open, |el| {
                                el.child(div().w(px(PLAYLIST_WIDTH + 24.)).flex_shrink_0())
                            }),
                    )
                    // 底部预留悬浮胶囊的位置（视频模式下这块是画布）
                    .child(div().h(px(BOTTOM_BAND)).flex_shrink_0()),
            )
            .child(self.render_capsule(cx))
            .when(self.playlist_open && !self.cinema, |el| {
                el.child(self.render_playlist_panel(cx))
            })
            .when(self.drop_hint_shown, |el| el.child(self.render_drop_hint()))
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
