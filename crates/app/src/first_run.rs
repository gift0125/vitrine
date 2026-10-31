//! 首屏（560×440）：启动时还没有要放的东西，先给一个「从哪开始」的小窗。
//!
//! 设计稿 `21:1` / `21:58` / `21:483`（mac）与 `21:587` / `21:644` / `21:710`（Win），
//! 几何以 `design/preview/index.html` 的首屏一节为准——那里的坐标都是画布值，
//! 这里照着做绝对定位。三态共用骨架（标题带 + 品牌区 + 两个按钮），下面按状态换：
//!
//! - **最近打开**：继续上次 + 最多 4 条最近 + 拖放提示；
//! - **空**：从没打开过任何东西，居中一块引导；
//! - **拖入中**：外部文件悬在窗口上，整块变成投放区。
//!
//! 首屏自己不放任何东西：选好之后把「放什么」交给 [`Launch`]，由 `main` 开主窗口、
//! 关掉首屏。和播放态分成两个窗口是设计稿的口径：两者没有共享几何，塞进同一个
//! 窗口只会让主窗的缩放、玻璃采样坐标全对不上。

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Icon, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::*;
use player_core::is_stream_url;

use crate::persistence::{Recent, RecentEntry};
use crate::player_view::{native_window_move, MEDIA_EXTENSIONS};
use crate::theme::mono_font_family;

/// 首屏窗口尺寸（设计稿 `first_run.size`）。
pub const WINDOW_SIZE: (f32, f32) = (560.0, 440.0);
/// mac 红绿灯（关闭钮）左上角。设计稿是 12px 的圆点、左上 20,20；系统按钮的外框
/// 是 14×16，圆点居中，所以外框要往左上各退一点才对得上圆心 (26, 26)。
pub const TRAFFIC_LIGHT_ORIGIN: (f32, f32) = (19.0, 18.0);
/// 「最近打开」最多露几条（行高 28，再多就压到底下的拖放提示了）。
const RECENT_ROWS: usize = 4;
/// 外部文件离开窗口多久算拖走了（`drag_over` 只在拖动时持续来，没有「离开」事件）。
const DROP_IDLE: Duration = Duration::from_millis(250);
const NATIVE_TRAFFIC_LIGHTS: bool = cfg!(target_os = "macos");

/// 首屏选好的「要放什么」。
#[derive(Debug, Clone, PartialEq)]
pub enum Launch {
    /// 某个文件或串流；`resume_at` 是续播起点（最近打开里记着的进度）
    Open { path: String, resume_at: Option<f64> },
    /// 「打开文件」选的 / 拖进来的：按拖放导入（文件夹递归扫描）并播第一条
    Import(Vec<PathBuf>),
}

pub type OnLaunch = Rc<dyn Fn(Launch, &mut Window, &mut App)>;

static LOGO: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../../../assets/vitrine.png").to_vec(),
    ))
});

// 图标都是画布导出的路径（`design/preview/index.html` 里那几段），不是手画的近似形。
const ICON_FOLDER: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 14 14"><g transform="translate(.65 2.45)"><path fill="currentColor" transform="matrix(1 0 0 1 0.55 0.55)" d="M1.1 -0.55L4 -0.55Q4.1205 -0.5506 4.2298 -0.4997Q4.3396 -0.4499 4.4176 -0.3579L5.453 0.85L10.5 0.85Q11.1778 0.85 11.6639 1.3361Q12.15 1.8222 12.15 2.5L12.15 7.4Q12.15 8.0778 11.6639 8.5639Q11.1778 9.05 10.5 9.05L1.1 9.05Q0.4222 9.05 -0.0639 8.5639Q-0.55 8.0778 -0.55 7.4L-0.55 1.1Q-0.55 0.4222 -0.0639 -0.0639Q0.4222 -0.55 1.1 -0.55ZM1.1 0.55Q0.8778 0.55 0.7139 0.7139Q0.55 0.8778 0.55 1.1L0.55 7.4Q0.55 7.6222 0.7139 7.7861Q0.8778 7.95 1.1 7.95L10.5 7.95Q10.7222 7.95 10.8861 7.7861Q11.05 7.6222 11.05 7.4L11.05 2.5Q11.05 2.2778 10.8861 2.1139Q10.7222 1.95 10.5 1.95L5.2 1.95Q5.0795 1.9506 4.9702 1.8997Q4.8604 1.8499 4.7824 1.7579L3.747 0.55L1.1 0.55Z"/></g></svg>"#;
const ICON_LINK: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 14 14"><path fill="currentColor" transform="matrix(1 0 0 1 5.9 5.9)" d="M1.8111 -0.3889L-0.3889 1.8111Q-0.5566 1.9694 -0.55 2.2Q-0.5566 2.4306 -0.3889 2.5889Q-0.2306 2.7566 0 2.75Q0.2306 2.7566 0.3889 2.5889L2.5889 0.3889Q2.7566 0.2306 2.75 0Q2.7566 -0.2306 2.5889 -0.3889Q2.4306 -0.5566 2.2 -0.55Q1.9694 -0.5566 1.8111 -0.3889Z"/><path fill="currentColor" transform="matrix(1 0 0 1 7.2 2.45)" d="M0.6111 0.3611L-0.3889 1.3611Q-0.5566 1.5194 -0.55 1.75Q-0.5566 1.9806 -0.3889 2.1389Q-0.2306 2.3066 0 2.3Q0.2306 2.3066 0.3889 2.1389L1.3889 1.1389Q1.9778 0.55 2.8 0.55Q3.6222 0.55 4.2111 1.1389Q4.8 1.7278 4.8 2.55Q4.8 3.3722 4.2111 3.9611L3.2111 4.9611Q3.0434 5.1194 3.05 5.35Q3.0434 5.5806 3.2111 5.7389Q3.3694 5.9066 3.6 5.9Q3.8306 5.9066 3.9889 5.7389L4.9889 4.7389Q5.9 3.8278 5.9 2.55Q5.9 1.2722 4.9889 0.3611Q4.0778 -0.55 2.8 -0.55Q1.5222 -0.55 0.6111 0.3611Z"/><path fill="currentColor" transform="matrix(1 0 0 1 1.45 6.2)" d="M4.7389 4.9889L5.7389 3.9889Q5.9066 3.8306 5.9 3.6Q5.9066 3.3694 5.7389 3.2111Q5.5806 3.0434 5.35 3.05Q5.1194 3.0434 4.9611 3.2111L3.9611 4.2111Q3.3722 4.8 2.55 4.8Q1.7278 4.8 1.1389 4.2111Q0.55 3.6222 0.55 2.8Q0.55 1.9778 1.1389 1.3889L2.1389 0.3889Q2.3066 0.2306 2.3 0Q2.3066 -0.2306 2.1389 -0.3889Q1.9806 -0.5566 1.75 -0.55Q1.5194 -0.5566 1.3611 -0.3889L0.3611 0.6111Q-0.55 1.5222 -0.55 2.8Q-0.55 4.0778 0.3611 4.9889Q1.2722 5.9 2.55 5.9Q3.8278 5.9 4.7389 4.9889Z"/></svg>"#;
const ICON_HISTORY: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 14 14"><path fill="currentColor" transform="matrix(1 0 0 1 2.2 2.18823)" d="M2.2456 8.1145Q0.6051 6.8616 0.6 4.7971Q0.6066 4.5456 0.4232 4.3733Q0.25 4.1908 -0.0015 4.1986Q-0.253 4.192 -0.4253 4.3754Q-0.6079 4.5486 -0.6 4.8001Q-0.5935 7.4562 1.5173 9.0682Q3.6281 10.6803 6.192 9.9871Q8.7559 9.294 9.7671 6.8379Q10.7783 4.3819 9.4459 2.0843Q8.1134 -0.2134 5.4796 -0.5552Q2.8458 -0.897 0.971 0.9844Q0.7883 1.1575 0.796 1.409Q0.7892 1.6605 0.9725 1.8329Q1.1456 2.0156 1.3971 2.0079Q1.6486 2.0147 1.8211 1.8314Q3.2781 0.3691 5.3251 0.6348Q7.3722 0.9005 8.4078 2.6863Q9.4434 4.4721 8.6575 6.3811Q7.8715 8.29 5.8788 8.8287Q3.8862 9.3674 2.2456 8.1145Z"/><path fill="currentColor" transform="matrix(1 0 0 1 2.1 1.6)" d="M0.6 2.2L0.6 0Q0.6072 -0.2515 0.4243 -0.4243Q0.2515 -0.6072 0 -0.6Q-0.2515 -0.6072 -0.4243 -0.4243Q-0.6072 -0.2515 -0.6 0L-0.6 2.8Q-0.6072 3.0515 -0.4243 3.2243Q-0.2515 3.4072 0 3.4L2.8 3.4Q3.0515 3.4072 3.2243 3.2243Q3.4072 3.0515 3.4 2.8Q3.4072 2.5485 3.2243 2.3757Q3.0515 2.1928 2.8 2.2L0.6 2.2Z"/><path fill="currentColor" transform="matrix(1 0 0 1 6.6 4.9)" d="M0.55 2.1764L0.55 0Q0.5566 -0.2306 0.3889 -0.3889Q0.2306 -0.5566 0 -0.55Q-0.2306 -0.5566 -0.3889 -0.3889Q-0.5566 -0.2306 -0.55 0L-0.55 2.8236L1.5329 3.9808Q1.7312 4.0986 1.9511 4.0288Q2.1746 3.9719 2.2808 3.7671Q2.3986 3.5688 2.3288 3.3489Q2.2719 3.1254 2.0671 3.0192L0.55 2.1764Z"/></svg>"#;
const ICON_VIDEO: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 14 14"><path fill="currentColor" transform="matrix(1 0 0 1 1.2 3.2)" d="M1.6 -0.55L10 -0.55Q10.8906 -0.55 11.5203 0.0797Q12.15 0.7094 12.15 1.6L12.15 6Q12.15 8.15 10 8.15L1.6 8.15Q0.7094 8.15 0.0797 7.5203Q-0.55 6.8906 -0.55 6L-0.55 1.6Q-0.55 0.7094 0.0797 0.0797Q0.7094 -0.55 1.6 -0.55ZM1.6 0.55Q1.1651 0.55 0.8575 0.8575Q0.55 1.1651 0.55 1.6L0.55 6Q0.55 7.05 1.6 7.05L10 7.05Q11.05 7.05 11.05 6L11.05 1.6Q11.05 0.55 10 0.55L1.6 0.55Z"/><path fill="currentColor" transform="matrix(1 0 0 1 5.6 5.6)" d="M0 0L3.4 1.4L0 2.8L0 0Z"/></svg>"#;
/// 引导块的下载箭头（48 格；底下那枚圆盘由 div 画，颜色按状态换）。
const ICON_DOWNLOAD: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48"><path fill="currentColor" transform="matrix(1 0 0 1 24 17)" d="M0.75 8.5L0.75 0Q0.759 -0.3144 0.5303 -0.5303Q0.3144 -0.759 0 -0.75Q-0.3144 -0.759 -0.5303 -0.5303Q-0.759 -0.3144 -0.75 0L-0.75 8.5Q-0.759 8.8144 -0.5303 9.0303Q-0.3144 9.259 0 9.25Q0.3144 9.259 0.5303 9.0303Q0.759 8.8144 0.75 8.5Z"/><path fill="currentColor" transform="matrix(1 0 0 1 20.6 22.4)" d="M3.4 2.3393L0.5303 -0.5303Q0.3144 -0.759 0 -0.75Q-0.3144 -0.759 -0.5303 -0.5303Q-0.759 -0.3144 -0.75 0Q-0.759 0.3144 -0.5303 0.5303L2.8697 3.9303Q3.0856 4.159 3.4 4.15Q3.7144 4.159 3.9303 3.9303L7.3303 0.5303Q7.559 0.3144 7.55 0Q7.559 -0.3144 7.3303 -0.5303Q7.1144 -0.759 6.8 -0.75Q6.4856 -0.759 6.2697 -0.5303L3.4 2.3393Z"/><path fill="currentColor" transform="matrix(1 0 0 1 17.5 27.5)" d="M0.75 1.7L0.75 0Q0.759 -0.3144 0.5303 -0.5303Q0.3144 -0.759 0 -0.75Q-0.3144 -0.759 -0.5303 -0.5303Q-0.759 -0.3144 -0.75 0L-0.75 1.7Q-0.75 2.5357 -0.1428 3.1428Q0.4643 3.75 1.3 3.75L11.7 3.75Q12.5357 3.75 13.1428 3.1428Q13.75 2.5357 13.75 1.7L13.75 0Q13.759 -0.3144 13.5303 -0.5303Q13.3144 -0.759 13 -0.75Q12.6856 -0.759 12.4697 -0.5303Q12.241 -0.3144 12.25 0L12.25 1.7Q12.25 1.9143 12.0822 2.0822Q11.9143 2.25 11.7 2.25L1.3 2.25Q1.0857 2.25 0.9178 2.0822Q0.75 1.9143 0.75 1.7Z"/></svg>"#;
// Win 右上两格（46×32，字形落在格子正中）
const ICON_WIN_MIN: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 46 32"><rect fill="currentColor" x="19" y="15.95" width="8" height="1.1"/></svg>"#;
const ICON_WIN_CLOSE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 46 32"><path fill="currentColor" transform="matrix(1 0 0 1 19 12)" d="M4.7778 4L8.3889 0.3889Q8.5566 0.2306 8.55 0Q8.5566 -0.2306 8.3889 -0.3889Q8.2306 -0.5566 8 -0.55Q7.7694 -0.5566 7.6111 -0.3889L4 3.2222L0.3889 -0.3889Q0.2306 -0.5566 0 -0.55Q-0.2306 -0.5566 -0.3889 -0.3889Q-0.5566 -0.2306 -0.55 0Q-0.5566 0.2306 -0.3889 0.3889L3.2222 4L-0.3889 7.6111Q-0.5566 7.7694 -0.55 8Q-0.5566 8.2306 -0.3889 8.3889Q-0.2306 8.5566 0 8.55Q0.2306 8.5566 0.3889 8.3889L4 4.7778L7.6111 8.3889Q7.7694 8.5566 8 8.55Q8.2306 8.5566 8.3889 8.3889Q8.5566 8.2306 8.55 8Q8.5566 7.7694 8.3889 7.6111L4.7778 4Z"/></svg>"#;

/// 首屏色板。浅色逐个取自设计稿（Latte），深色按 Mocha 对位。
struct Palette {
    base: Hsla,
    mantle: Hsla,
    mantle_hover: Hsla,
    text: Hsla,
    subtext0: Hsla,
    overlay1: Hsla,
    surface1: Hsla,
    mauve: Hsla,
    mauve_hover: Hsla,
    on_mauve: Hsla,
    row_hover: Hsla,
    row_on: Hsla,
    drop_bg: Hsla,
    drop_edge: Hsla,
    button_glow: Hsla,
}

impl Palette {
    fn new(dark: bool) -> Self {
        let c = |hex: u32| -> Hsla { rgb(hex).into() };
        let a = |hex: u32| -> Hsla { rgba(hex).into() };
        if dark {
            Self {
                base: c(0x1E1E2E),
                mantle: c(0x313244),
                mantle_hover: c(0x45475A),
                text: c(0xCDD6F4),
                subtext0: c(0xA6ADC8),
                overlay1: c(0x7F849C),
                surface1: c(0x45475A),
                mauve: c(0xCBA6F7),
                mauve_hover: c(0xD7B8F9),
                on_mauve: c(0x11111B),
                row_hover: a(0xCDD6F40F),
                row_on: a(0xCBA6F71F),
                drop_bg: a(0xCBA6F714),
                drop_edge: a(0xCBA6F780),
                button_glow: a(0xCBA6F733),
            }
        } else {
            Self {
                base: c(0xEFF1F5),
                mantle: c(0xE6E9EF),
                mantle_hover: c(0xDFE3EA),
                text: c(0x4C4F69),
                subtext0: c(0x6C6F85),
                overlay1: c(0x8C8FA1),
                surface1: c(0xBCC0CC),
                mauve: c(0x8839EF),
                mauve_hover: c(0x7C2FE0),
                on_mauve: c(0xFFFFFF),
                // rgba(76,79,105,.05) / rgba(136,57,239,.08|.06|.50|.30)
                row_hover: a(0x4C4F690D),
                row_on: a(0x8839EF14),
                drop_bg: a(0x8839EF0F),
                drop_edge: a(0x8839EF80),
                button_glow: a(0x8839EF4D),
            }
        }
    }
}

/// 首屏当前露哪一态。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FirstRunState {
    Recent,
    Empty,
    /// 拖入中；带着投放区里显示的名字
    Drop(String),
}

pub struct FirstRunView {
    /// 只留还找得到的（文件被删 / 挪走的不露出来，串流地址一律保留）
    recent: Recent,
    /// 列表里高亮的那一行（悬停跟着走，↑↓ 也能挪）
    selected: usize,
    /// 外部文件悬在窗口上：最近一次 `drag_over` 的时刻 + 投放区里的名字。
    /// 样式回调拿不到 `Context`，只能留戳，由轮询决定显隐（和主窗的拖放提示同一个套路）。
    drag: Rc<RefCell<Option<(Instant, String)>>>,
    drop_shown: Option<String>,
    move_armed: Rc<Cell<Option<Point<Pixels>>>>,
    focus: FocusHandle,
    on_launch: OnLaunch,
    stream_input: Option<Entity<InputState>>,
    _subscriptions: Vec<Subscription>,
}

impl FirstRunView {
    pub fn new(recent: Recent, on_launch: OnLaunch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let entries = recent
            .entries
            .into_iter()
            .filter(|entry| is_stream_url(&entry.path) || Path::new(&entry.path).exists())
            .collect();
        let focus = cx.focus_handle();
        focus.focus(window, cx);

        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let Ok(()) = this.update(cx, |view, cx| view.tick(cx)) else {
                break;
            };
        })
        .detach();

        Self {
            recent: Recent { entries },
            selected: 0,
            drag: Rc::new(RefCell::new(None)),
            drop_shown: None,
            move_armed: Rc::new(Cell::new(None)),
            focus,
            on_launch,
            stream_input: None,
            _subscriptions: Vec::new(),
        }
    }

    pub(crate) fn state(&self) -> FirstRunState {
        if let Some(label) = &self.drop_shown {
            FirstRunState::Drop(label.clone())
        } else if self.recent.entries.is_empty() {
            FirstRunState::Empty
        } else {
            FirstRunState::Recent
        }
    }

    fn rows(&self) -> Vec<&RecentEntry> {
        self.recent.list().take(RECENT_ROWS).collect()
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let live = self
            .drag
            .borrow()
            .as_ref()
            .filter(|(stamp, _)| stamp.elapsed() < DROP_IDLE)
            .map(|(_, label)| label.clone());
        if live.is_none() {
            self.drag.replace(None);
        }
        if live != self.drop_shown {
            self.drop_shown = live;
            cx.notify();
        }
    }

    fn launch(&mut self, launch: Launch, window: &mut Window, cx: &mut Context<Self>) {
        let on_launch = self.on_launch.clone();
        on_launch(launch, window, cx);
    }

    fn open_entry(&mut self, entry: &RecentEntry, window: &mut Window, cx: &mut Context<Self>) {
        let resume_at = entry.resumable().then_some(entry.position_secs);
        self.launch(
            Launch::Open {
                path: entry.path.clone(),
                resume_at,
            },
            window,
            cx,
        );
    }

    fn open_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let files = rfd::AsyncFileDialog::new()
                .set_title("打开媒体文件")
                .add_filter("媒体文件", MEDIA_EXTENSIONS)
                .pick_files()
                .await;
            let Some(files) = files else { return };
            let paths: Vec<PathBuf> = files.into_iter().map(|f| f.path().to_path_buf()).collect();
            let _ = this.update_in(cx, |view, window, cx| {
                view.launch(Launch::Import(paths), window, cx);
            });
        })
        .detach();
    }

    fn open_stream(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("https://… / rtmp://… / rtsp://…"));
        self._subscriptions = vec![cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let value = input.read(cx).value();
                    this.submit_stream(&value, window, cx);
                }
            },
        )];
        self.stream_input = Some(input.clone());

        let view = cx.entity().downgrade();
        let first_build = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first_build.replace(false) {
                input.update(cx, |state, cx| state.focus(window, cx));
            }
            let submit = {
                let view = view.clone();
                let input = input.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let value = input.read(cx).value();
                    let _ = view.update(cx, |view, cx| view.submit_stream(&value, window, cx));
                }
            };
            dialog
                .title("打开串流")
                .w(px(420.))
                .child(
                    v_flex()
                        .gap(px(8.))
                        .child(Input::new(&input))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(cx.theme().muted_foreground)
                                .child("支持 http(s) / rtmp / rtsp / srt 等 mpv 能打开的地址"),
                        ),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("stream-cancel")
                                .label("取消")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("stream-open")
                                .primary()
                                .label("打开")
                                .on_click(submit),
                        ),
                )
        });
    }

    fn submit_stream(&mut self, raw: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(url) = normalize_stream(raw) else {
            return;
        };
        window.close_dialog(cx);
        self.stream_input = None;
        self.launch(
            Launch::Open {
                path: url,
                resume_at: None,
            },
            window,
            cx,
        );
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.rows().len();
        match event.keystroke.key.as_str() {
            "down" if count > 0 => self.selected = (self.selected + 1).min(count - 1),
            "up" => self.selected = self.selected.saturating_sub(1),
            "enter" => {
                if let Some(entry) = self.rows().get(self.selected).map(|e| (*e).clone()) {
                    self.open_entry(&entry, window, cx);
                }
                return;
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    // ------------------------------------------------------------------
    // 渲染
    // ------------------------------------------------------------------

    fn render_title_band(&self, p: &Palette) -> impl IntoElement {
        let band = div()
            .id("fr-title")
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .h(px(44.))
            .window_control_area(WindowControlArea::Drag)
            .when(NATIVE_TRAFFIC_LIGHTS, |el| native_window_move(&self.move_armed, el));
        let caption = |id: &'static str, glyph: &'static [u8], label: &'static str| {
            div()
                .id(id)
                .w(px(46.))
                .h(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(p.text)
                .hover(|s| s.bg(p.row_hover))
                .aria_label(label)
                .child(Icon::default().data(glyph).w(px(46.)).h(px(32.)))
        };
        div().child(band).when(!NATIVE_TRAFFIC_LIGHTS, |el| {
            el.child(
                h_flex()
                    .absolute()
                    .left(px(462.))
                    .top(px(6.))
                    .child(
                        caption("fr-min", ICON_WIN_MIN, "最小化")
                            .on_click(|_, window, _| window.minimize_window()),
                    )
                    .child(
                        caption("fr-close", ICON_WIN_CLOSE, "关闭")
                            .on_click(|_, window, _| window.remove_window()),
                    ),
            )
        })
    }

    fn render_brand(&self, p: &Palette) -> impl IntoElement {
        h_flex()
            .absolute()
            .left(px(24.))
            .top(px(54.))
            .w(px(512.))
            .h(px(68.))
            .gap(px(14.))
            .child(img(LOGO.clone()).size(px(68.)).flex_shrink_0())
            .child(
                v_flex()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(20.))
                            .line_height(px(28.))
                            .font_weight(FontWeight::BOLD)
                            .child("Vitrine"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(17.))
                            .text_color(p.overlay1)
                            .child(concat!("版本 ", env!("CARGO_PKG_VERSION"))),
                    ),
            )
    }

    fn render_actions(&self, p: &Palette, cx: &Context<Self>) -> impl IntoElement {
        let (glow, hover, mantle_hover) = (p.button_glow, p.mauve_hover, p.mantle_hover);
        let button = |id: &'static str, width: f32| {
            div()
                .id(id)
                .w(px(width))
                .h(px(32.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
        };
        h_flex()
            .absolute()
            .left(px(24.))
            .top(px(148.))
            .gap(px(10.))
            .child(
                button("fr-open", 116.)
                    .bg(p.mauve)
                    .text_color(p.on_mauve)
                    .shadow(vec![BoxShadow {
                        color: glow,
                        offset: point(px(0.), px(3.)),
                        blur_radius: px(12.),
                        spread_radius: px(0.),
                        inset: false,
                    }])
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(|this, _, window, cx| this.open_files(window, cx)))
                    .child(Icon::default().data(ICON_FOLDER).size(px(14.)))
                    .child("打开文件")
                    .test_support(),
            )
            .child(
                button("fr-stream", 104.)
                    .bg(p.mantle)
                    .border_1()
                    .border_color(p.surface1)
                    .text_color(p.text)
                    .hover(move |s| s.bg(mantle_hover))
                    .on_click(cx.listener(|this, _, window, cx| this.open_stream(window, cx)))
                    .child(Icon::default().data(ICON_LINK).size(px(14.)))
                    .child("打开串流")
                    .test_support(),
            )
    }

    fn render_recent(&self, p: &Palette, cx: &Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let mono = mono_font_family();
        let resume = self.recent.continue_last().cloned();
        // 没有可续的就把「最近打开」整段提到「继续上次」的位置，不留一块空
        let section_top = if resume.is_some() { 246. } else { 196. };

        if let Some(entry) = resume {
            let mantle_hover = p.mantle_hover;
            out.push(
                h_flex()
                    .id("fr-continue")
                    .absolute()
                    .left(px(24.))
                    .top(px(196.))
                    .w(px(512.))
                    .h(px(32.))
                    .px(px(10.))
                    .gap(px(8.))
                    .rounded(px(8.))
                    .bg(p.mantle)
                    .cursor_pointer()
                    .hover(move |s| s.bg(mantle_hover))
                    .child(Icon::default().data(ICON_HISTORY).size(px(14.)).text_color(p.mauve))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .line_height(px(16.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.mauve)
                            .child("继续上次"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .line_height(px(17.))
                            .child(entry.title.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(mono)
                            .text_size(px(11.))
                            .line_height(px(16.))
                            .text_color(p.overlay1)
                            .child(clock(entry.position_secs)),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.open_entry(&entry, window, cx)))
                    .test_support()
                    .into_any_element(),
            );
        }

        let rows = self.rows();
        if !rows.is_empty() {
            out.push(
                div()
                    .absolute()
                    .left(px(24.))
                    .top(px(section_top))
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.overlay1)
                    .child("最近打开")
                    .into_any_element(),
            );
            let list = v_flex()
                .absolute()
                .left(px(24.))
                .top(px(section_top + 22.))
                .w(px(512.))
                .children(rows.into_iter().enumerate().map(|(i, entry)| {
                    let on = i == self.selected;
                    let entry = entry.clone();
                    let row_hover = p.row_hover;
                    h_flex()
                        .id(("fr-row", i))
                        .h(px(28.))
                        .px(px(4.))
                        .gap(px(8.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .when(on, |el| el.bg(p.row_on))
                        .when(!on, |el| el.hover(move |s| s.bg(row_hover)))
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if *hovered && this.selected != i {
                                this.selected = i;
                                cx.notify();
                            }
                        }))
                        .child(
                            Icon::default()
                                .data(ICON_VIDEO)
                                .size(px(14.))
                                .text_color(if on { p.mauve } else { p.overlay1 }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.))
                                .line_height(px(17.))
                                .child(entry.title.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family(mono)
                                .text_size(px(11.))
                                .line_height(px(16.))
                                .text_color(p.overlay1)
                                .child(entry.duration_secs.map(long_clock).unwrap_or_default()),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| this.open_entry(&entry, window, cx)))
                        .test_support()
                }));
            out.push(list.into_any_element());
        }

        out.push(
            div()
                .absolute()
                .left(px(24.))
                .top(px(396.))
                .text_size(px(11.))
                .line_height(px(16.))
                .text_color(p.subtext0)
                .child("也可以直接把文件或文件夹拖进这个窗口")
                .into_any_element(),
        );
        out
    }

    /// 空状态 / 投放区共用的引导块：图标 48 + gap10 + 15/21 + gap10 + 12/17 ×2。
    fn render_guide(&self, p: &Palette, drop: Option<&str>) -> impl IntoElement {
        let (id, top, height) = match drop {
            Some(_) => ("fr-drop", 204., 212.),
            None => ("fr-empty", 186., 220.),
        };
        let (disc, arrow, title_color) = match drop {
            Some(_) => (p.mauve, p.on_mauve, p.mauve),
            None => (p.mantle, p.mauve, p.text),
        };
        let line = |text: String, color: Hsla| {
            div()
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(color)
                .max_w(px(480.))
                .truncate()
                .child(text)
        };
        v_flex()
            .id(id)
            .absolute()
            .left(px(24.))
            .top(px(top))
            .w(px(512.))
            .h(px(height))
            .pt(px(2.))
            .items_center()
            .justify_center()
            .gap(px(10.))
            .when(drop.is_some(), |el| {
                el.rounded(px(12.))
                    .bg(p.drop_bg)
                    .border_color(p.drop_edge)
                    .map(|mut el| {
                        // 1.5px（从帧上逐像素量过，不是 1px）；Styled 只有整数档
                        let width = Some(AbsoluteLength::Pixels(px(1.5)));
                        let edges = &mut el.style().border_widths;
                        edges.top = width;
                        edges.right = width;
                        edges.bottom = width;
                        edges.left = width;
                        el
                    })
            })
            .child(
                div()
                    .size(px(48.))
                    .flex_none()
                    .rounded_full()
                    .bg(disc)
                    .child(Icon::default().data(ICON_DOWNLOAD).size(px(48.)).text_color(arrow)),
            )
            .child(
                div()
                    .text_size(px(15.))
                    .line_height(px(21.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(title_color)
                    .child(if drop.is_some() {
                        "松手即可播放"
                    } else {
                        "把视频拖进来就能开始"
                    }),
            )
            .map(|el| match drop {
                Some(label) => el
                    .child(line(label.to_string(), p.subtext0))
                    .child(line("将加入播放列表并立即开始播放".into(), p.overlay1)),
                None => el
                    .child(line("支持 MP4 · MKV · MOV · MP3 · FLAC".into(), p.overlay1))
                    .child(line("或点击上方「打开文件」选择".into(), p.overlay1)),
            })
            .test_support()
    }

    #[cfg(test)]
    pub(crate) fn test_set_drag(&mut self, label: Option<&str>) {
        self.drag
            .replace(label.map(|label| (Instant::now(), label.to_string())));
        self.drop_shown = label.map(str::to_string);
    }
}

impl Render for FirstRunView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::new(cx.theme().mode.is_dark());
        let drag = self.drag.clone();
        let state = self.state();

        div()
            .id("first-run")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(p.base)
            .text_color(p.text)
            .on_key_down(cx.listener(Self::on_key))
            .drag_over::<ExternalPaths>(move |style, paths, _, _| {
                let mut slot = drag.borrow_mut();
                match slot.as_mut() {
                    Some((stamp, _)) => *stamp = Instant::now(),
                    None => *slot = Some((Instant::now(), drop_label(paths.paths()))),
                }
                style
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drag.replace(None);
                this.drop_shown = None;
                let paths = paths.paths().to_vec();
                if !paths.is_empty() {
                    this.launch(Launch::Import(paths), window, cx);
                }
            }))
            .child(self.render_title_band(&p))
            .child(self.render_brand(&p))
            .child(self.render_actions(&p, cx))
            .map(|el| match &state {
                FirstRunState::Recent => el.children(self.render_recent(&p, cx)),
                FirstRunState::Empty => el.child(self.render_guide(&p, None)),
                FirstRunState::Drop(label) => el.child(self.render_guide(&p, Some(label))),
            })
    }
}

/// 投放区里显示什么：第一项的名字；文件夹标出来；多项写「等 N 项」。
fn drop_label(paths: &[PathBuf]) -> String {
    let Some(first) = paths.first() else {
        return String::new();
    };
    let mut name = first
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| first.display().to_string());
    if first.is_dir() {
        name.push_str("（文件夹）");
    }
    match paths.len() {
        1 => name,
        n => format!("{name} 等 {n} 项"),
    }
}

/// 用户贴进来的串流地址：去掉首尾空白；没写协议的按 https 补上。
fn normalize_stream(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if is_stream_url(raw) {
        return Some(raw.to_string());
    }
    // 像个域名才补：`C:/movies/a.mkv` 的「主机」是 `C:`，不算
    let host = raw.split('/').next().unwrap_or_default();
    let looks_like_host = host.contains('.') && !host.starts_with('.');
    (!raw.contains("://") && looks_like_host && !raw.contains(char::is_whitespace))
        .then(|| format!("https://{raw}"))
}

/// 「继续上次」的位置：不满 1 小时写 `42:17`，满了写 `1:02:03`。
fn clock(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// 列表里的时长一律带小时位（设计稿 `0:04:12`），一列竖着才对得齐。
fn long_clock(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    format!("{}:{:02}:{:02}", total / 3600, (total % 3600) / 60, total % 60)
}

#[cfg(test)]
mod tests {
    // 不能 `use super::*`：那会把 GPUI 的 `test` 属性宏一起带进来，盖掉标准库的
    use std::path::PathBuf;

    use super::{clock, drop_label, long_clock, normalize_stream};

    #[test]
    fn clocks_match_the_design() {
        assert_eq!(clock(2537.0), "42:17");
        assert_eq!(clock(3723.0), "1:02:03");
        assert_eq!(long_clock(252.0), "0:04:12");
        assert_eq!(long_clock(5825.0), "1:37:05");
    }

    #[test]
    fn stream_input_is_forgiving_but_not_blind() {
        assert_eq!(
            normalize_stream("  https://a.tv/live.m3u8 ").as_deref(),
            Some("https://a.tv/live.m3u8")
        );
        assert_eq!(
            normalize_stream("a.tv/live.m3u8").as_deref(),
            Some("https://a.tv/live.m3u8"),
            "没写协议的按 https 补"
        );
        assert_eq!(normalize_stream(""), None);
        assert_eq!(normalize_stream("随便几个字"), None);
        assert_eq!(normalize_stream("C:/movies/a.mkv"), None, "本地路径不是串流");
    }

    #[test]
    fn drop_label_names_the_first_item() {
        assert_eq!(drop_label(&[PathBuf::from("/x/星际漫游者 S01E04.mkv")]), "星际漫游者 S01E04.mkv");
        assert_eq!(
            drop_label(&[PathBuf::from("/x/a.mp4"), PathBuf::from("/x/b.mp4")]),
            "a.mp4 等 2 项"
        );
    }
}
