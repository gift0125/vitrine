//! 主视图：播放器三栏布局（Stage 1 纯静态骨架）。
//!
//! - 左侧：播放列表面板（List 虚拟滚动）
//! - 右上：封面占位区（当前曲目信息）
//! - 底部：控制栏（上一首/播放暂停/下一首 + 进度条 + 音量 + 主题切换）
//!
//! Stage 2 将把控制逻辑接入 player-core 的 AudioEngine。

use player_core::TrackMeta;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::list::{List, ListEvent, ListState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Icon, Sizable as _, Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::playlist::PlaylistDelegate;

/// 进度条量程（秒）。Stage 2 起按当前曲目时长动态设置。
const SEEK_MAX: f32 = 300.0;

pub struct PlayerView {
    playlist: Entity<ListState<PlaylistDelegate>>,
    seek: Entity<SliderState>,
    volume: Entity<SliderState>,
    /// 演示曲目（Stage 2 起来自 AudioEngine / 播放列表导入）
    tracks: Vec<TrackMeta>,
    /// 当前曲目下标
    current: Option<usize>,
    playing: bool,
    position_secs: f32,
    volume_pct: i32,
    #[allow(dead_code)]
    subscriptions: Vec<Subscription>,
}

impl PlayerView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tracks = demo_tracks();

        let playlist =
            cx.new(|cx| ListState::new(PlaylistDelegate::new(tracks.clone()), window, cx));
        let seek = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(SEEK_MAX)
                .default_value(0.0)
                .step(1.0)
        });
        let volume = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(100.0)
                .default_value(70.0)
                .step(1.0)
        });

        let subscriptions = vec![
            // 双击/回车确认曲目 -> 播放
            cx.subscribe(&playlist, |this, _list, event: &ListEvent, cx| {
                if let ListEvent::Confirm(ix) = event {
                    this.current = Some(ix.row);
                    this.playing = true;
                    this.position_secs = 0.0;
                    cx.notify();
                }
            }),
            // 拖动进度条
            cx.subscribe(
                &seek,
                |this, _slider, event: &SliderEvent, cx| match event {
                    SliderEvent::Change(value) | SliderEvent::Release(value) => {
                        this.position_secs = value.start();
                        cx.notify();
                    }
                },
            ),
            // 音量
            cx.subscribe(&volume, |this, _slider, event: &SliderEvent, cx| {
                if let SliderEvent::Change(value) = event {
                    this.volume_pct = value.start() as i32;
                    cx.notify();
                }
            }),
        ];

        Self {
            playlist,
            seek,
            volume,
            tracks,
            current: None,
            playing: false,
            position_secs: 0.0,
            volume_pct: 70,
            subscriptions,
        }
    }

    fn current_track(&self) -> Option<&TrackMeta> {
        self.current.and_then(|i| self.tracks.get(i))
    }

    /// 上一首/下一首（循环跳转）
    fn skip(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.tracks.is_empty() {
            return;
        }
        let len = self.tracks.len() as i32;
        let idx = match self.current {
            Some(i) => (i as i32 + delta).rem_euclid(len) as usize,
            None => 0,
        };
        self.current = Some(idx);
        self.playing = true;
        self.position_secs = 0.0;
        cx.notify();
    }

    fn render_playlist_panel(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
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
                            Label::new(format!("{} 首", self.tracks.len()))
                                .text_xs()
                                .text_color(theme.muted_foreground),
                        ),
                    )
                    .child(
                        Button::new("add-track")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .tooltip("添加文件（Stage 3 接入）"),
                    ),
            )
            .child(div().flex_1().min_h_0().child(List::new(&self.playlist)))
    }

    fn render_cover(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let track = self.current_track();

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
            .when(track.is_none(), |el| {
                el.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("从左侧播放列表选择一首曲目开始"),
                )
            })
    }

    fn render_seek_row(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let duration = self
            .current_track()
            .and_then(|t| t.duration_secs)
            .unwrap_or(0.0);

        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(px(44.))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(fmt_time(self.position_secs as f64)),
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
        let play_icon = if self.playing {
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
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.skip(-1, cx);
                            })),
                    )
                    .child(
                        Button::new("play-pause")
                            .primary()
                            .large()
                            .icon(play_icon)
                            .tooltip(if self.playing { "暂停" } else { "播放" })
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.playing = !this.playing;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("next")
                            .ghost()
                            .icon(IconName::SkipForward)
                            .tooltip("下一首")
                            .on_click(cx.listener(|this, _event, _window, cx| {
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
                        .on_click(cx.listener(|_this, _event, _window, cx| {
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

/// Stage 1 演示数据（Stage 2 起由真实文件导入替代）。
fn demo_tracks() -> Vec<TrackMeta> {
    vec![
        TrackMeta {
            title: "晴天".into(),
            artist: Some("周杰伦".into()),
            duration_secs: Some(269.0),
            path: String::new(),
        },
        TrackMeta {
            title: "稻香".into(),
            artist: Some("周杰伦".into()),
            duration_secs: Some(223.0),
            path: String::new(),
        },
        TrackMeta {
            title: "泡沫".into(),
            artist: Some("邓紫棋".into()),
            duration_secs: Some(253.0),
            path: String::new(),
        },
        TrackMeta {
            title: "光年之外".into(),
            artist: Some("邓紫棋".into()),
            duration_secs: Some(235.0),
            path: String::new(),
        },
        TrackMeta {
            title: "起风了".into(),
            artist: Some("买辣椒也用券".into()),
            duration_secs: Some(325.0),
            path: String::new(),
        },
        TrackMeta {
            title: "Lemon".into(),
            artist: Some("米津玄師".into()),
            duration_secs: Some(254.0),
            path: String::new(),
        },
    ]
}

/// 秒数格式化为 `m:ss`。
fn fmt_time(secs: f64) -> String {
    let total = secs.max(0.0) as u32;
    format!("{}:{:02}", total / 60, total % 60)
}
