//! Simple Media Player — Stage 1: 主窗口骨架与三栏布局
//!
//! 布局：左侧播放列表 / 右上封面区 / 底部控制栏，支持明暗主题切换。
//! Stage 2 将接入 player-core 的 AudioEngine 实现真实播放。

mod assets;
mod player_view;
mod playlist;

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::assets::AppAssets;
use crate::player_view::PlayerView;

fn main() {
    let start_dark = std::env::args().any(|arg| arg == "--dark");

    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);

            if start_dark {
                gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
            }

            let bounds = Bounds {
                origin: Default::default(),
                size: size(px(1040.), px(680.)),
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

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| PlayerView::new(window, cx))
            })
            .expect("failed to open window");
        });
}
