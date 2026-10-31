// release 版按 GUI 程序链接（PE subsystem = Windows），双击 exe 不会再弹一个 cmd 窗口。
// debug 版保留控制台子系统，方便直接看到 eprintln! 诊断输出。
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

//! Simple Media Player — 主入口
//!
//! M1（Stage 2+3）：音频播放内核 + 播放列表管理。
//! 启动时恢复播放列表/音量/窗口尺寸；`--dark` 以深色主题启动。

mod assets;
mod first_run;
mod liquid_glass;
mod persistence;
mod player_view;
mod playlist;
mod theme;
mod ui_state;
#[cfg(test)]
mod ui_tests;
mod video_surface;
mod window_actions;

use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::*;
use player_core::TrackMeta;

use crate::assets::AppAssets;
use crate::first_run::{FirstRunView, Launch};
use crate::player_view::PlayerView;

/// 从命令行取「启动即播放」的文件路径。
///
/// 支持三种写法（媒体播放器的惯例）：
/// - `simple-media-player.exe D:\movie.mp4`（位置参数，等价于资源管理器「打开方式」）
/// - `--play D:\movie.mp4`
/// - `--play=D:\movie.mp4`
fn open_arg() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(rest) = arg.strip_prefix("--play=") {
            return Some(rest.to_string());
        }
        if arg == "--play" {
            return args.next();
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg);
    }
    None
}

fn main() {
    let start_dark = std::env::args().any(|arg| arg == "--dark");

    // 会话留痕：用于排查"进程自行退出"这类无法复现的问题，
    // 能区分「GPUI 主动退出」与「被外部终止」（见 persistence::log_session）。
    persistence::log_session(if start_dark {
        "start (dark)"
    } else {
        "start (light)"
    });
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        persistence::log_session(&format!("panic: {info}"));
        default_hook(info);
    }));

    // 命令行带了文件就直接开主窗口播它；什么都没带先给首屏
    let launch = open_arg().map(|raw| Launch::Open {
        path: raw,
        resume_at: None,
    });

    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            // 设计稿色板（Catppuccin Latte / Mocha），必须在 init 之后覆盖默认主题
            theme::install(cx, start_dark);

            // 两个窗口（首屏 → 主窗）之间没有「主窗口」可依附：最后一个关掉就退出。
            // mac 上 GPUI 不会自己退（菜单栏应用的惯例），这里显式收尾。
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            match launch.clone() {
                Some(launch) => open_player(cx, Some(launch)),
                None => open_first_run(cx),
            }
        });

    // 能走到这里说明 GPUI 事件循环正常结束（最后一个窗口关闭 / 主动 quit）；
    // 若日志里只有 start 没有这一行，就是被外部终止。
    persistence::log_session("event loop returned, exiting");
}

/// 首屏（560×440，不可缩放）。选好要放什么之后先开主窗口、再关掉自己——
/// 顺序不能反：先关的话那一刻一个窗口都没有，`on_window_closed` 会直接退出。
fn open_first_run(cx: &mut App) {
    let (w, h) = first_run::WINDOW_SIZE;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(w), px(h)),
            cx,
        ))),
        is_resizable: false,
        titlebar: Some(TitlebarOptions {
            title: Some("Vitrine".into()),
            appears_transparent: true,
            traffic_light_position: cfg!(target_os = "macos").then(|| {
                let (x, y) = first_run::TRAFFIC_LIGHT_ORIGIN;
                point(px(x), px(y))
            }),
        }),
        ..Default::default()
    };
    let recent = persistence::load_recent();
    gpui_kit::open_window(options, cx, move |window, cx| {
        let on_launch: first_run::OnLaunch = Rc::new(|launch, window, cx| {
            persistence::log_session(&format!("first run -> {launch:?}"));
            open_player(cx, Some(launch));
            window.remove_window();
        });
        let recent = recent.clone();
        cx.new(|cx| FirstRunView::new(recent, on_launch, window, cx))
    })
    .expect("failed to open first-run window");
}

/// 主播放窗口。`launch` 是这次要放的东西（命令行参数或首屏选的），
/// 没有就只恢复上次的播放列表。
fn open_player(cx: &mut App, launch: Option<Launch>) {
    // 恢复持久化状态（播放列表 + 设置）
    let settings = persistence::load_settings();
    let (mut tracks, mut initial_current) = match persistence::load_playlist() {
        Some(file) => (file.tracks, file.current_index),
        None => (Vec::new(), None),
    };

    let mut autoplay: Option<usize> = None;
    let mut resume_at = None;
    let mut import = Vec::new();
    match launch {
        Some(Launch::Open {
            path,
            resume_at: start,
        }) => {
            if let Some(index) = add_track(&mut tracks, &path) {
                persistence::log_session(&format!("open: {index} {path}"));
                autoplay = Some(index);
                initial_current = Some(index);
                resume_at = start;
            }
        }
        Some(Launch::Import(paths)) => import = paths,
        None => {}
    }

    let w = settings.window_width.clamp(720.0, 4096.0);
    let h = settings.window_height.clamp(480.0, 4096.0);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(w), px(h)),
            cx,
        ))),
        window_min_size: Some(size(px(720.), px(480.))),
        titlebar: Some(TitlebarOptions {
            // 设计稿的产品名是 Vitrine（音乐播放器的品牌名）
            title: Some("Vitrine".into()),
            // 关掉系统标题栏，由视图顶部的标题带自绘（拖动/双击/窗口按钮）。
            // GPUI 的 Windows 后端据 appears_transparent 决定是否隐藏非客户区，
            // 边框与四边缩放手柄仍由系统提供。
            appears_transparent: true,
            // mac 保留系统红绿灯，挪到标题带里竖直居中
            traffic_light_position: cfg!(target_os = "macos").then(|| {
                let (x, y) = player_view::TRAFFIC_LIGHT_ORIGIN;
                point(px(x), px(y))
            }),
        }),
        // 窗口需要**逐像素透明**：视频模式的画面上要浮着半透明玻璃
        // （标题带 / 胶囊 / 信息卡），窗口圆角的外侧也要真的透出桌面。
        //
        // 这一项只是前提之一：非 Opaque 时渲染器会把后备缓冲清成 alpha 0，
        // 而合成用的 swapchain 本来就是 `DXGI_ALPHA_MODE_PREMULTIPLIED`，
        // DWM 会按 alpha 合成。另一处前提是窗口表面那层底色——它由
        // gpui-component 的 RootPlugin 铺，在 `PlayerView::render` 里覆盖成透明。
        //
        // 画面本身（视频帧 + 黑边）由我们自己铺，所以不会透出桌面。
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };

    gpui_kit::open_window(options, cx, move |window, cx| {
        let tracks = tracks.clone();
        let settings = settings.clone();
        let import = import.clone();
        cx.new(|cx| {
            PlayerView::new(window, cx, tracks, initial_current, settings, autoplay)
                .with_launch(resume_at, import)
        })
    })
    .expect("failed to open window");
}

/// 把要放的东西放进列表（已在列表里就复用那一条），返回它的下标。
///
/// 本地文件统一成单斜杠：mpv 对重复分隔符不宽容（`C://` 会直接 file_error）。
/// 串流地址原样放进去，标题就是地址本身。
fn add_track(tracks: &mut Vec<TrackMeta>, raw: &str) -> Option<usize> {
    let normalized = if player_core::is_stream_url(raw) {
        raw.to_string()
    } else {
        let path = std::path::Path::new(raw);
        if !path.exists() {
            persistence::log_session(&format!("open missing: {raw}"));
            return None;
        }
        path.to_string_lossy().replace('\\', "/")
    };
    let stream = player_core::is_stream_url(raw);
    if let Some(index) = tracks.iter().position(|track| track.path == normalized) {
        let track = &mut tracks[index];
        if track.duration_secs.is_none() && !stream {
            track.duration_secs = player_core::load_track_meta(std::path::Path::new(raw)).duration_secs;
        }
        return Some(index);
    }
    let track = if stream {
        TrackMeta {
            title: raw.to_string(),
            artist: None,
            duration_secs: None,
            path: normalized,
        }
    } else {
        // 只读一个文件的标签，很快；拿到时长首屏的「最近打开」才有得显示
        TrackMeta {
            path: normalized,
            ..player_core::load_track_meta(std::path::Path::new(raw))
        }
    };
    tracks.push(track);
    Some(tracks.len() - 1)
}
