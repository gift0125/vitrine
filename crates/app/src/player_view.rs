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
use std::time::Duration;

use player_core::{
    find_mpv, is_video_file, load_track_meta, scan_dir_for_tracks, AudioEngine, PlaybackState,
    SpawnOptions, TrackMeta, VideoEngine, VideoStatus,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::list::{List, ListEvent, ListState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Icon, Sizable as _, Theme, ThemeMode, WindowExt as _,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::persistence::{self, Settings};
use crate::playlist::PlaylistDelegate;
use crate::video_overlay::{self, Hwnd, VideoOverlay};

/// 进度条量程未知时的兜底（元数据解析失败）。
const SEEK_FALLBACK_MAX: f32 = 600.0;
/// 轮询间隔（计划约定 100ms）。
const TICK_INTERVAL: Duration = Duration::from_millis(100);
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

    // ---- 用户可见错误 ----
    /// 待弹出的错误提示（由 tick 统一推成 toast，保证有 window 可用）
    pending_errors: Vec<SharedString>,
    /// 待弹出的普通提示（例如导入结果）
    pending_infos: Vec<SharedString>,
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
            cx.subscribe(&seek, |this, _slider, event: &SliderEvent, cx| match event {
                SliderEvent::Change(_) => this.user_seeking = true,
                SliderEvent::Release(value) => {
                    this.user_seeking = false;
                    this.seek_to(value.start() as f64);
                    cx.notify();
                }
            }),
            // 音量
            cx.subscribe(&volume, |this, _slider, event: &SliderEvent, cx| {
                if let SliderEvent::Change(value) = event {
                    this.set_volume_pct(value.start() as i32, cx);
                }
            }),
        ];

        let mut subscriptions = subscriptions;
        // 窗口移动/缩放时同步叠加窗口位置
        subscriptions.push(
            cx.observe_window_bounds(window, |this, window, _cx| {
                this.sync_overlay_rect(window);
            }),
        );
        // 退出应用时结束 mpv 子进程（避免孤儿进程）
        subscriptions.push(cx.on_app_quit(|this, _cx| {
            let detail = match this.current {
                Some(i) => format!("app_quit requested (video_active={}, current={i})", this.video_active),
                None => format!("app_quit requested (video_active={}, current=none)", this.video_active),
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
            last_error: startup_errors.first().cloned(),
            pending_errors: startup_errors,
            pending_infos: Vec::new(),
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
        eprintln!("{message}");
        self.last_error = Some(message.clone());
        self.pending_errors.push(message);
    }

    /// 记录一条普通提示（导入结果等），随下一次 tick 弹出。
    fn report_info(&mut self, message: impl Into<SharedString>) {
        self.pending_infos.push(message.into());
    }

    /// 把排队的提示推成右上角 toast（错误用 `Notification::error`，其余 info）。
    fn flush_errors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for message in std::mem::take(&mut self.pending_errors) {
            window.push_notification(Notification::error(message), cx);
        }
        for message in std::mem::take(&mut self.pending_infos) {
            window.push_notification(Notification::info(message), cx);
        }
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_errors(window, cx);

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
        let size = (
            bounds.size.width.as_f32(),
            bounds.size.height.as_f32(),
        );
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
    // 渲染
    // ------------------------------------------------------------------

    fn render_playlist_panel(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let count = self.playlist.read(cx).delegate().track_count();

        v_flex()
            .h_full()
            .w(px(280.))
            .flex_shrink_0()
            .border_r_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2p5()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        Icon::new(IconName::ListMusic)
                            .small()
                            .text_color(theme.primary),
                    )
                    .child(Label::new("播放列表").text_sm())
                    .child(
                        div().flex_1().child(
                            Label::new(format!("{count} 首"))
                                .text_xs()
                                .text_color(theme.muted_foreground),
                        ),
                    )
                    .child(
                        Button::new("add-file")
                            .ghost()
                            .small()
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
                            .small()
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
                            .small()
                            .icon(IconName::Delete)
                            .tooltip("清空播放列表")
                            .accessibility_label("清空播放列表")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.clear_playlist(cx);
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().child(List::new(&self.playlist)))
    }

    /// 音频：封面 + 曲目信息。
    fn render_cover(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let track = self.current_track(cx);
        let has_track = track.is_some();

        v_flex()
            .flex_1()
            .h_full()
            .items_center()
            .justify_center()
            .gap_5()
            .p_6()
            .child(
                div()
                    .size(px(220.))
                    .rounded(px(28.))
                    .bg(theme.secondary)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(IconName::Music)
                            .large()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .when_some(track, |el, track| {
                el.child(
                    v_flex()
                        .items_center()
                        .gap_1()
                        .child(div().text_xl().child(track.title.clone()))
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "{} · {}",
                                    track.artist.clone().unwrap_or_else(|| "未知艺术家".into()),
                                    fmt_time(track.duration_secs.unwrap_or(0.0))
                                )),
                        ),
                )
            })
            .when(!has_track, |el| {
                el.child(div().text_sm().text_color(theme.muted_foreground).child(
                    if self.playlist.read(cx).delegate().track_count() == 0 {
                        "播放列表为空，点击左上角 + 添加媒体"
                    } else {
                        "从左侧播放列表选择一项开始播放"
                    },
                ))
            })
    }

    /// 视频：舞台区（mpv 渲染进叠加窗口，这里只提供位置与占位背景）。
    fn render_video_stage(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let base = div()
            .flex_1()
            .h_full()
            .bg(gpui_kit::rgb(0x000000))
            .relative()
            .flex()
            .items_center()
            .justify_center();

        if let Some(err) = &self.video_error {
            return base.child(
                v_flex()
                    .items_center()
                    .gap_2()
                    .p_6()
                    .child(
                        Icon::new(IconName::CircleAlert)
                            .large()
                            .text_color(theme.danger),
                    )
                    .child(div().text_sm().text_color(gpui_kit::rgb(0xdddddd)).child(err.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(gpui_kit::rgb(0x888888))
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

    fn render_seek_row(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
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

        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(px(44.))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(fmt_time(position)),
            )
            // Slider 自身没有 id / 无障碍信息（上游未实现），用带 id 的容器承载，
            // 既给读屏软件一个可命名的分组，也让 UI 测试能定位到进度条几何。
            .child(
                div()
                    .id("seek-bar")
                    .aria_label("播放进度")
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .test_support()
                    .child(Slider::new(&self.seek).flex_1().disabled(self.current.is_none())),
            )
            .child(
                div()
                    .w(px(44.))
                    .text_xs()
                    .text_right()
                    .text_color(theme.muted_foreground)
                    .child(fmt_time(duration)),
            )
    }

    fn render_controls_row(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let is_dark = theme.mode.is_dark();
        let state = self.playback_state();
        let playing = state.is_playing();
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

        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            // 左：音量
            .child(
                h_flex()
                    .w(px(180.))
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(volume_icon)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .id("volume-bar")
                            .aria_label("音量")
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .test_support()
                            .child(Slider::new(&self.volume).flex_1()),
                    ),
            )
            // 中：上一首 / 播放暂停 / 下一首
            .child(
                h_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(
                        Button::new("prev")
                            .ghost()
                            .icon(IconName::SkipBack)
                            .tooltip("上一首")
                            .accessibility_label("上一首")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.skip(-1, cx);
                            })),
                    )
                    .child(
                        Button::new("play-pause")
                            .primary()
                            .large()
                            .icon(play_icon)
                            .tooltip(if playing { "暂停" } else { "播放" })
                            .accessibility_label(if playing { "暂停" } else { "播放" })
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.toggle_play(cx);
                            })),
                    )
                    .child(
                        Button::new("next")
                            .ghost()
                            .icon(IconName::SkipForward)
                            .tooltip("下一首")
                            .accessibility_label("下一首")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.skip(1, cx);
                            })),
                    ),
            )
            // 右：全屏（仅视频） + 主题切换
            .child(
                h_flex()
                    .w(px(96.))
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .when(is_video, |el| {
                        el.child(
                            Button::new("cinema")
                                .ghost()
                                .icon(if cinema {
                                    IconName::Minimize
                                } else {
                                    IconName::Maximize
                                })
                                .tooltip(if cinema { "退出全屏" } else { "全屏" })
                                .accessibility_label(if cinema { "退出全屏" } else { "进入全屏" })
                                .on_click(cx.listener(|this, _e, window, cx| {
                                    this.toggle_cinema(window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("theme-toggle")
                            .ghost()
                            .icon(if is_dark { IconName::Sun } else { IconName::Moon })
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
        }
    }
}

impl Render for PlayerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let video_active = self.video_active;
        let cinema = self.cinema;

        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            // 主区域：左歌单 + 右封面/视频舞台
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .when(!cinema, |el| el.child(self.render_playlist_panel(cx)))
                    .child(if video_active {
                        self.render_video_stage(cx)
                    } else {
                        self.render_cover(cx)
                    }),
            )
            // 底部控制栏
            .child(
                v_flex()
                    .border_t_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .px_4()
                    .py_3()
                    .gap_2()
                    .child(self.render_seek_row(cx))
                    .child(self.render_controls_row(cx)),
            )
    }
}

/// 秒数格式化为 `m:ss`。
fn fmt_time(secs: f64) -> String {
    let total = secs.max(0.0) as u32;
    format!("{}:{:02}", total / 60, total % 60)
}
