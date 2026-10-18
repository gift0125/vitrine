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
mod persistence;
mod player_view;
mod playlist;
mod theme;
#[cfg(test)]
mod ui_tests;
mod video_overlay;

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::assets::AppAssets;
use crate::player_view::PlayerView;

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

    // 恢复持久化状态（播放列表 + 设置）
    let saved_playlist = persistence::load_playlist();
    let settings = persistence::load_settings();
    let (initial_tracks, initial_current) = match saved_playlist {
        Some(file) => (file.tracks, file.current_index),
        None => (Vec::new(), None),
    };

    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            // 设计稿色板（Catppuccin Latte / Mocha），必须在 init 之后覆盖默认主题
            theme::install(cx, start_dark);

            let w = settings.window_width.clamp(720.0, 4096.0);
            let h = settings.window_height.clamp(480.0, 4096.0);
            let bounds = Bounds {
                origin: Default::default(),
                size: size(px(w), px(h)),
            };
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(720.), px(480.))),
                titlebar: Some(TitlebarOptions {
                    // 设计稿的产品名是 Chroma（音乐播放器的品牌名）
                    title: Some("Chroma".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };

            let initial_current = initial_current;
            let settings = settings.clone();
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    PlayerView::new(
                        window,
                        cx,
                        initial_tracks.clone(),
                        initial_current,
                        settings,
                    )
                })
            })
            .expect("failed to open window");
        });

    // 能走到这里说明 GPUI 事件循环正常结束（最后一个窗口关闭 / 主动 quit）；
    // 若日志里只有 start 没有这一行，就是被外部终止。
    persistence::log_session("event loop returned, exiting");
}
