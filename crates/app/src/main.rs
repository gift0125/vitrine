//! Simple Media Player — 主入口
//!
//! M1（Stage 2+3）：音频播放内核 + 播放列表管理。
//! 启动时恢复播放列表/音量/窗口尺寸；`--dark` 以深色主题启动。

mod assets;
mod persistence;
mod player_view;
mod playlist;

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::assets::AppAssets;
use crate::player_view::PlayerView;

fn main() {
    let start_dark = std::env::args().any(|arg| arg == "--dark");

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

            if start_dark {
                gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
            }

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
                    title: Some("Simple Media Player".into()),
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
}
