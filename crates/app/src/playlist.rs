//! 播放列表面板：List 组件 + 虚拟滚动 + 拖拽排序。
//!
//! 数据源（tracks）的真身在 [`PlaylistDelegate`] 内；所有会改变列表的
//! 操作（删除/拖拽排序）通过 `WeakEntity<PlayerView>` 回写主视图，
//! 由主视图统一修正当前曲目下标并持久化。

use player_core::TrackMeta;

use crate::persistence::SortOrder;
use gpui_kit::assets::IconName;
use gpui_kit::component::label::Label;
use gpui_kit::component::list::{ListDelegate, ListItem, ListState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Icon, IndexPath};
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
    /// 曲目路径的**加入顺序**（第一次排序时拍快照），用于「排序：加入顺序」还原
    added: Vec<String>,
    view: WeakEntity<PlayerView>,
}

impl PlaylistDelegate {
    pub fn new(tracks: Vec<TrackMeta>, view: WeakEntity<PlayerView>) -> Self {
        Self {
            tracks,
            selected: None,
            confirmed: None,
            added: Vec::new(),
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

    /// 补一条曲目的时长（秒）。返回是否真的写进去了。
    ///
    /// 音频的时长在入列时就能从解码器问出来，视频不能——mpv 要等起播之后才报
    /// `duration`。所以视频的 `duration_secs` 入列时是 `None`，页脚那行
    /// 「N 个文件 · 总时长」和行内第二行都会空着；由 tick 在拿到 mpv 的时长后补进来。
    /// 只在原来是 `None`（或明显不对）时写，避免覆盖已知值、也避免每帧都改。
    pub fn set_duration(&mut self, row: usize, secs: f64) -> bool {
        let Some(track) = self.tracks.get_mut(row) else {
            return false;
        };
        if secs <= 0.5 {
            return false;
        }
        if track.duration_secs.is_some_and(|d| (d - secs).abs() < 1.0) {
            return false;
        }
        track.duration_secs = Some(secs);
        true
    }

    /// 追加曲目；返回新曲目起始下标。
    pub fn append_tracks(&mut self, tracks: Vec<TrackMeta>) -> usize {
        let start = self.tracks.len();
        // 已经拍过「加入顺序」快照时，新加的也要进快照尾部，否则它们会被当成
        // 「未知路径」排到所有老曲目后面（排序一次后新拖进来的歌顺序就乱了）
        if !self.added.is_empty() {
            self.added.extend(tracks.iter().map(|t| t.path.clone()));
        }
        self.tracks.extend(tracks);
        start
    }

    /// 移除指定曲目。选中/播放中索引由调用方（view）修正。
    pub fn remove_at(&mut self, row: usize) {
        if row < self.tracks.len() {
            let path = self.tracks.remove(row).path;
            if let Some(pos) = self.added.iter().position(|p| *p == path) {
                self.added.remove(pos);
            }
        }
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
        self.added.clear();
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
        // 手动拖过之后「加入顺序」就该等于用户现在的排列：清掉快照，
        // 下次 `sort_by(Added)` 会按当前顺序重新拍一张
        self.added.clear();
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

    /// 当前键盘/悬停选中行（面板底部「移除」优先删它）。
    pub fn selected_row(&self) -> Option<usize> {
        self.selected.map(|ix| ix.row)
    }

    /// 按指定方式重排，返回**当前播放项**重排后的新下标。
    ///
    /// 「加入顺序」靠 `added`（路径的插入顺序）还原：`tracks` 会被原地排序，不留
    /// 一份路径顺序就再也回不到"我刚拖进来的那个顺序"了。
    ///
    /// 同一路径可以出现多次（同一首歌加两次），配对时按出现次序依次消费 `added`
    /// 里的位置，而不是一律取第一次出现的下标——否则重复项全部并列，稳定性全靠
    /// 运气。完全相同的路径本来就无法区分「哪个是哪个」，这里只保证不并列。
    pub fn sort_by(&mut self, order: SortOrder) -> Option<usize> {
        if order == SortOrder::Added && self.added.is_empty() {
            return self.confirmed_row();
        }
        if self.added.is_empty() {
            self.added = self.tracks.iter().map(|t| t.path.clone()).collect();
        }

        let mut used = vec![false; self.added.len()];
        let ranks: Vec<usize> = self
            .tracks
            .iter()
            .map(
                |t| match (0..self.added.len()).find(|&i| !used[i] && self.added[i] == t.path) {
                    Some(i) => {
                        used[i] = true;
                        i
                    }
                    None => usize::MAX,
                },
            )
            .collect();

        let mut ix: Vec<usize> = (0..self.tracks.len()).collect();
        match order {
            SortOrder::Added => ix.sort_by_key(|&i| ranks[i]),
            SortOrder::Title => ix.sort_by(|&a, &b| {
                self.tracks[a]
                    .title
                    .to_lowercase()
                    .cmp(&self.tracks[b].title.to_lowercase())
                    .then(ranks[a].cmp(&ranks[b]))
            }),
            SortOrder::Duration => ix.sort_by(|&a, &b| {
                let da = self.tracks[a].duration_secs.unwrap_or(f64::MAX);
                let db = self.tracks[b].duration_secs.unwrap_or(f64::MAX);
                da.total_cmp(&db).then(ranks[a].cmp(&ranks[b]))
            }),
        }

        let current_path = self
            .confirmed
            .and_then(|i| self.tracks.get(i.row))
            .map(|t| t.path.clone());

        // 按 `ix` 重排（`Option` take 一遍，避免给 TrackMeta 加 Clone）
        let mut slots: Vec<Option<TrackMeta>> = self.tracks.drain(..).map(Some).collect();
        self.tracks = ix.into_iter().filter_map(|i| slots[i].take()).collect();

        let new_row = current_path.and_then(|path| self.tracks.iter().position(|t| t.path == path));
        self.confirmed = new_row.map(IndexPath::new);
        self.selected = None;
        new_row
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

        // 当前曲目的视觉线索按设计稿来：行底色（Mauve 12%）+ 1px Mauve 描边 +
        // 标题 Mauve + 序号位换成均衡器图标。**不做左侧强调条**——设计稿的列表行
        // 是一颗圆角胶囊，没有装饰条。
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

        Some(
            ListItem::new(ix)
                .h(px(48.))
                .px(px(10.))
                .py(px(0.))
                .rounded(px(10.))
                .bg(row_bg)
                // 当前项多一圈 1px Mauve 描边：只靠底色在浅底面板上几乎看不出来
                // （`theme.list_active` 是 Mauve 8%，和 `mantle` 面板底色太近）
                .when(is_current, |el| {
                    el.border_1().border_color(accent.opacity(0.35))
                })
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
                // 行内不再放删除按钮：设计稿的行是一条干净的胶囊，删除入口在面板
                // 底部的「−」。以前那枚 ✕ 每行都在，十行就是十个小叉，把标题压没了。
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
                // 入口在**面板底部**（设计稿把 + / 📁 / − / 清空 放在 footer），
                // 别再写「上方」——那会把人指到标题栏去
                Label::new("用底部的 + 添加文件，或 📁 添加文件夹")
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
