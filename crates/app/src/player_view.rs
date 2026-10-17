//! 主视图：播放器三栏布局 + 播放引擎状态同步。
//!
//! - 左侧：播放列表面板（List 虚拟滚动 + 拖拽排序 + 删除）
//! - 右上：封面占位区（当前曲目信息）
//! - 底部：控制栏（上一首/播放暂停/下一首 + 进度条 + 音量 + 主题切换）
//!
//! 引擎（player-core AudioEngine）由本视图持有；100ms 定时器轮询位置并
//! 同步 SeekBar，检测到曲目播完自动切下一首。

use std::path::PathBuf;
use std::time::Duration;

use player_core::{load_track_meta, scan_dir_for_tracks, AudioEngine, TrackMeta};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::list::{List, ListEvent, ListState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Icon, Sizable as _, Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::persistence::{self, Settings};
use crate::playlist::PlaylistDelegate;

/// 进度条量程未知时的兜底（元数据解析失败）。
const SEEK_FALLBACK_MAX: f32 = 600.0;
/// 轮询间隔（计划约定 100ms）。
const TICK_INTERVAL: Duration = Duration::from_millis(100);
/// 窗口尺寸变化后多久落盘一次（tick 计数，50 ≈ 5s）。
const WINDOW_SAVE_TICKS: usize = 50;

/// 订阅向量的固定下标。
const SUB_SEEK: usize = 1;

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
}

impl PlayerView {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        initial_tracks: Vec<TrackMeta>,
        initial_current: Option<usize>,
        settings: Settings,
    ) -> Self {
        let weak_self = cx.weak_entity();
        let playlist = cx
            .new(|cx| ListState::new(PlaylistDelegate::new(initial_tracks, weak_self), window, cx));
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
                        let secs = value.start() as f64;
                        if let Some(engine) = &mut this.engine {
                            engine.seek(secs);
                        }
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

        // 引擎：无音频设备时降级为只读 UI（控件仍可交互但无声）
        let mut engine = match AudioEngine::new() {
            Ok(engine) => Some(engine),
            Err(err) => {
                eprintln!("警告：音频引擎初始化失败，播放功能不可用: {err}");
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
        };

        // 100ms 轮询：进度同步 + 自动连播 + 窗口尺寸节流落盘
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

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(engine) = &mut self.engine {
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

        // 窗口尺寸节流落盘（变化后 ~5s 保存一次）
        let bounds = window.bounds();
        let size = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
        if size != self.last_saved_size {
            self.window_dirty_ticks += 1;
            if self.window_dirty_ticks >= WINDOW_SAVE_TICKS {
                self.settings.window_width = size.0;
                self.settings.window_height = size.1;
                persistence::save_settings(&self.settings);
                self.last_saved_size = size;
                self.window_dirty_ticks = 0;
            }
        } else {
            self.window_dirty_ticks = 0;
        }
    }

    // ------------------------------------------------------------------
    // 播放控制
    // ------------------------------------------------------------------

    fn play_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let track = self.playlist.read(cx).delegate().track(index).cloned();
        let Some(track) = track else { return };

        if let Some(engine) = &mut self.engine {
            match engine.play(&track.path) {
                Ok(()) => {
                    self.current = Some(index);
                    // 重建进度条量程（SliderState 不支持运行时改 max）
                    let max = track.duration_secs.unwrap_or(SEEK_FALLBACK_MAX as f64) as f32;
                    let seek = cx.new(|_| {
                        SliderState::new()
                            .min(0.0)
                            .max(max.max(1.0))
                            .default_value(0.0)
                            .step(1.0)
                    });
                    let sub =
                        cx.subscribe(
                            &seek,
                            |this, _slider, event: &SliderEvent, cx| match event {
                                SliderEvent::Change(_) => this.user_seeking = true,
                                SliderEvent::Release(value) => {
                                    this.user_seeking = false;
                                    let secs = value.start() as f64;
                                    if let Some(engine) = &mut this.engine {
                                        engine.seek(secs);
                                    }
                                    cx.notify();
                                }
                            },
                        );
                    self.subscriptions[SUB_SEEK] = sub;
                    self.seek = seek;
                }
                Err(err) => {
                    eprintln!("播放失败 {}: {err}", track.path);
                    self.current = None;
                }
            }
        } else {
            // 无引擎（无音频设备）：仅更新高亮，便于 UI 验证
            self.current = Some(index);
        }

        self.playlist.update(cx, |state, cx| {
            state.delegate_mut().set_confirmed(self.current);
            cx.notify();
        });
        self.persist_playlist(cx);
        cx.notify();
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        match engine.state() {
            player_core::PlaybackState::Playing => {
                engine.pause();
                cx.notify();
            }
            player_core::PlaybackState::Paused => {
                engine.resume();
                cx.notify();
            }
            player_core::PlaybackState::Stopped => {
                // 重新播放当前曲目，或从第一首开始
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

    fn set_volume_pct(&mut self, pct: i32, cx: &mut Context<Self>) {
        self.volume_pct = pct.clamp(0, 100);
        if let Some(engine) = &mut self.engine {
            engine.set_volume(self.volume_pct as f32 / 100.0);
        }
        self.settings.volume = self.volume_pct as f32 / 100.0;
        persistence::save_settings(&self.settings);
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
                // 删除的是正在播放的曲目：停止播放
                if let Some(engine) = &mut self.engine {
                    engine.stop();
                }
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
                .set_title("添加音频文件")
                .add_filter(
                    "音频文件",
                    &[
                        "mp3", "flac", "wav", "ogg", "oga", "m4a", "mp4", "aac", "aiff",
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
                view.append_tracks(tracks, cx);
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
                view.append_tracks(tracks, cx);
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
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.clear_playlist(cx);
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().child(List::new(&self.playlist)))
    }

    fn render_cover(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let track = self
            .current
            .and_then(|i| self.playlist.read(cx).delegate().track(i).cloned());
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
                        "播放列表为空，点击左上角 + 添加音频"
                    } else {
                        "从左侧播放列表选择一首曲目开始"
                    },
                ))
            })
    }

    fn render_seek_row(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let duration = self
            .current
            .and_then(|i| self.playlist.read(cx).delegate().track(i))
            .and_then(|t| t.duration_secs)
            .unwrap_or(0.0);
        let position = if self.user_seeking {
            self.seek.read(cx).value().start() as f64
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
            .child(
                Slider::new(&self.seek)
                    .flex_1()
                    .disabled(self.current.is_none()),
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
        let state = self
            .engine
            .as_ref()
            .map(|e| e.state())
            .unwrap_or(player_core::PlaybackState::Stopped);
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
                    .child(Slider::new(&self.volume).flex_1()),
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
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.toggle_play(cx);
                            })),
                    )
                    .child(
                        Button::new("next")
                            .ghost()
                            .icon(IconName::SkipForward)
                            .tooltip("下一首")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.skip(1, cx);
                            })),
                    ),
            )
            // 右：主题切换
            .child(
                h_flex().w(px(60.)).justify_end().child(
                    Button::new("theme-toggle")
                        .ghost()
                        .icon(if is_dark {
                            IconName::Sun
                        } else {
                            IconName::Moon
                        })
                        .tooltip("明暗主题切换")
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

impl Render for PlayerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            // 主区域：左歌单 + 右封面
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_playlist_panel(cx))
                    .child(self.render_cover(cx)),
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
