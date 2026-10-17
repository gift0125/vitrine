//! 播放列表面板：List 组件 + 虚拟滚动。
//!
//! Stage 1 为静态演示数据（验证布局与滚动）；Stage 3 接入
//! 文件导入、拖拽排序与持久化。

use player_core::TrackMeta;

use gpui_kit::assets::IconName;
use gpui_kit::component::label::Label;
use gpui_kit::component::list::{ListDelegate, ListItem, ListState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Icon, IndexPath, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Window};

/// 播放列表数据源。
pub struct PlaylistDelegate {
    tracks: Vec<TrackMeta>,
    /// 当前键盘/悬停选中项
    selected: Option<IndexPath>,
    /// 当前播放项（双击/回车确认）
    confirmed: Option<IndexPath>,
}

impl PlaylistDelegate {
    pub fn new(tracks: Vec<TrackMeta>) -> Self {
        Self {
            tracks,
            selected: None,
            confirmed: None,
        }
    }

    /// 当前确认（播放）中的曲目下标（Stage 3 状态同步使用）。
    #[allow(dead_code)]
    pub fn confirmed_index(&self) -> Option<usize> {
        self.confirmed.map(|ix| ix.row)
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
        let track = self.tracks.get(ix.row)?;
        let is_current = self.confirmed == Some(ix);
        let theme = cx.theme();

        let icon_color = if is_current {
            theme.primary
        } else {
            theme.muted_foreground
        };

        let artist = track
            .artist
            .clone()
            .unwrap_or_else(|| "未知艺术家".to_string());

        Some(
            ListItem::new(ix)
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(Icon::new(IconName::Music).small().text_color(icon_color))
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .gap_0p5()
                                .child(
                                    Label::new(track.title.clone())
                                        .text_sm()
                                        .when(is_current, |l| l.text_color(theme.primary)),
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
