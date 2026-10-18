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
use gpui_kit::{
    div, px, transparent_black, App, Context, FontWeight, SharedString, WeakEntity, Window,
};

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

        // 当前曲目的视觉线索按设计稿来：行底色（Mauve 8%）+ 标题 Mauve + 序号位换成
        // 均衡器图标。**不做左侧强调条**——设计稿的列表行是纯平面，没有装饰条。
        let accent = theme.primary;
        let title_color = if is_current { accent } else { theme.foreground };
        let row_bg = if is_current {
            theme.list_active
        } else {
            transparent_black()
        };
        let mono = theme.mono_font_family.clone();
        let secondary = theme.muted_foreground;

        let artist = track
            .artist
            .clone()
            .unwrap_or_else(|| "未知艺术家".to_string());
        let duration = fmt_duration(track.duration_secs);
        // 设计稿：当前项第二行是「正在播放 · 42:17」，其余是「4K HDR · 52:10」
        let meta = if is_current {
            format!("正在播放 · {duration}")
        } else {
            format!("{artist} · {duration}")
        };

        let drag = DragTrack {
            title: track.title.clone(),
        };
        let from_row = ix.row;
        let view_remove = self.view.clone();
        let remove_label = SharedString::from(format!("从播放列表移除 {}", track.title));

        Some(
            ListItem::new(ix)
                .h(px(48.))
                .px(px(8.))
                .py(px(0.))
                .rounded(px(8.))
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
                        .gap(px(12.))
                        // 序号列（JetBrains Mono 风格等宽数字）：当前项换成均衡器图标
                        .child(
                            div()
                                .w(px(22.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .when(is_current, |el| {
                                    el.child(
                                        Icon::new(IconName::Volume2)
                                            .size(px(14.))
                                            .text_color(accent),
                                    )
                                })
                                .when(!is_current, |el| {
                                    el.child(
                                        div()
                                            .font_family(mono.clone())
                                            .text_size(px(11.))
                                            .text_color(secondary)
                                            .child(format!("{:02}", ix.row + 1)),
                                    )
                                }),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .gap(px(3.))
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .text_color(title_color)
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .when(is_current, |el| el.font_weight(FontWeight::SEMIBOLD))
                                        .child(track.title.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(secondary)
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(meta),
                                ),
                        ),
                )
                // 行尾删除按钮（设计稿没有这个入口，压到最弱层级，别抢标题）
                .suffix(move |_, cx| {
                    let view = view_remove.clone();
                    Button::new(SharedString::from(format!("remove-{from_row}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .text_color(cx.theme().muted_foreground)
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
