//! 播放列表面板：List 组件 + 虚拟滚动 + 拖拽排序。
//!
//! 数据源（tracks）的真身在 [`PlaylistDelegate`] 内；所有会改变列表的
//! 操作（删除/拖拽排序）通过 `WeakEntity<PlayerView>` 回写主视图，
//! 由主视图统一修正当前曲目下标并持久化。

use player_core::TrackMeta;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::list::{ListDelegate, ListItem, ListState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Icon, IndexPath, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, transparent_black, App, Context, FontWeight, SharedString, WeakEntity, Window};

use crate::player_view::PlayerView;

/// 拖拽排序时跟随光标的预览项。
#[derive(Clone)]
struct DragTrack {
    title: String,
}

impl Render for DragTrack {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme.primary)
            .text_color(theme.primary_foreground)
            .text_sm()
            .child(self.title.clone())
    }
}

/// 播放列表数据源。
pub struct PlaylistDelegate {
    tracks: Vec<TrackMeta>,
    /// 当前键盘/悬停选中项
    selected: Option<IndexPath>,
    /// 当前播放项
    confirmed: Option<IndexPath>,
    view: WeakEntity<PlayerView>,
}

impl PlaylistDelegate {
    pub fn new(tracks: Vec<TrackMeta>, view: WeakEntity<PlayerView>) -> Self {
        Self {
            tracks,
            selected: None,
            confirmed: None,
            view,
        }
    }

    pub fn tracks(&self) -> &[TrackMeta] {
        &self.tracks
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    pub fn track(&self, row: usize) -> Option<&TrackMeta> {
        self.tracks.get(row)
    }

    /// 追加曲目；返回新曲目起始下标。
    pub fn append_tracks(&mut self, tracks: Vec<TrackMeta>) -> usize {
        let start = self.tracks.len();
        self.tracks.extend(tracks);
        start
    }

    /// 移除指定曲目。选中/播放中索引由调用方（view）修正。
    pub fn remove_at(&mut self, row: usize) {
        if row < self.tracks.len() {
            self.tracks.remove(row);
        }
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
        self.selected = None;
        self.confirmed = None;
    }

    /// 拖拽排序：把 `from` 行移动到 `to` 行，选中/播放中索引同步重映射。
    pub fn move_item(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tracks.len() || to >= self.tracks.len() {
            return;
        }
        let track = self.tracks.remove(from);
        self.tracks.insert(to, track);
        self.selected = self
            .selected
            .map(|ix| IndexPath::new(remap_index(ix.row, from, to)));
        self.confirmed = self
            .confirmed
            .map(|ix| IndexPath::new(remap_index(ix.row, from, to)));
    }

    /// 同步当前播放项（主视图在播放/停止时调用）。
    pub fn set_confirmed(&mut self, row: Option<usize>) {
        self.confirmed = row.map(IndexPath::new);
    }

    /// 当前播放曲目下标。
    pub fn confirmed_row(&self) -> Option<usize> {
        self.confirmed.map(|ix| ix.row)
    }
}

/// 拖拽排序后旧下标 → 新下标映射。
fn remap_index(old: usize, from: usize, to: usize) -> usize {
    if old == from {
        to
    } else if from < old && old <= to {
        old - 1
    } else if to <= old && old < from {
        old + 1
    } else {
        old
    }
}

impl ListDelegate for PlaylistDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.tracks.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let track = self.tracks.get(ix.row)?.clone();
        let is_current = self.confirmed == Some(ix);
        let theme = cx.theme();
        let view = self.view.clone();

        // 当前曲目的四重视觉线索：行底色 + 左侧强调条 + 图标换 Volume2 + 标题半粗。
        //
        // 早期用 `theme.primary` 给标题着色，但默认主题里 primary 与正文色几乎重合
        // （light: neutral-900 vs neutral-950；dark: neutral-50 vs neutral-50），
        // 等于没有区分度。改用列表专门的语义色 list_active / list_active_border，
        // 后续换肤（Chroma）只需改主题 token，这里不用动。
        let accent = theme.list_active_border;
        let bar_color = if is_current {
            accent
        } else {
            transparent_black()
        };
        let row_bg = if is_current {
            theme.list_active
        } else {
            transparent_black()
        };
        let icon = if is_current {
            IconName::Volume2
        } else {
            IconName::Music
        };
        let icon_color = if is_current {
            accent
        } else {
            theme.muted_foreground
        };

        let artist = track
            .artist
            .clone()
            .unwrap_or_else(|| "未知艺术家".to_string());

        let drag = DragTrack {
            title: track.title.clone(),
        };
        let from_row = ix.row;
        let view_remove = self.view.clone();
        let remove_label = SharedString::from(format!("从播放列表移除 {}", track.title));

        Some(
            ListItem::new(ix)
                .bg(row_bg)
                .accessibility_label(SharedString::from(format!(
                    "{}{}",
                    track.title,
                    if is_current { "（正在播放）" } else { "" }
                )))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        // 左侧强调条：始终占位，非当前曲目为透明，避免行内容左右跳动
                        .child(div().w_0p5().h_4().rounded_full().bg(bar_color))
                        .child(Icon::new(icon).small().text_color(icon_color))
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .gap_0p5()
                                .child(
                                    Label::new(track.title.clone())
                                        .text_sm()
                                        .text_color(theme.foreground)
                                        .when(is_current, |l| {
                                            l.font_weight(FontWeight::SEMIBOLD)
                                        }),
                                )
                                .child(
                                    Label::new(artist)
                                        .text_xs()
                                        .text_color(theme.muted_foreground),
                                ),
                        )
                        .child(
                            Label::new(fmt_duration(track.duration_secs))
                                .text_xs()
                                .text_color(theme.muted_foreground),
                        ),
                )
                // 行尾删除按钮
                .suffix(move |_, _| {
                    let view = view_remove.clone();
                    Button::new(SharedString::from(format!("remove-{from_row}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("从播放列表移除")
                        .accessibility_label(remove_label.clone())
                        .on_click(move |_, _, cx| {
                            let _ = view.update(cx, |view, cx| {
                                view.remove_track(from_row, cx);
                            });
                        })
                })
                // 拖拽排序
                .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                .drag_over::<DragTrack>(|style, _, _, cx| style.bg(cx.theme().drop_target))
                .on_drop(move |drag: &DragTrack, _, cx| {
                    let _ = drag;
                    let _ = view.update(cx, |view, cx| {
                        view.move_playlist_item(from_row, ix.row, cx);
                    });
                })
                .selected(self.selected == Some(ix))
                .confirmed(is_current),
        )
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
        cx.notify();
    }

    fn confirm(
        &mut self,
        _secondary: bool,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.confirmed = self.selected;
        cx.notify();
    }

    fn cancel(&mut self, _window: &mut Window, cx: &mut Context<ListState<Self>>) {
        self.selected = None;
        cx.notify();
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .py_8()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(IconName::ListMusic).size_10())
            .child(Label::new("播放列表为空"))
            .child(
                Label::new("点击上方 + 添加音频文件，或 +📁 添加文件夹")
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
    }
}

/// 时长格式化：`Some(269.0)` -> "4:29"，`None` -> "--:--"。
fn fmt_duration(secs: Option<f64>) -> String {
    match secs {
        Some(s) => {
            let total = s.max(0.0) as u32;
            format!("{}:{:02}", total / 60, total % 60)
        }
        None => "--:--".to_string(),
    }
}
