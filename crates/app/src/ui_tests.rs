//! UI 集成测试：在无头窗口里渲染真实的 `PlayerView`，用原生指针/键盘事件驱动，
//! 断言播放状态、无障碍名称与布局可见性。
//!
//! 与 `player-core` 的单测互补：那边验证内核（rodio / mpv IPC），这边验证「点了按钮之后
//! 视图状态对不对」。因此这里不碰真实媒体解码（用 0.5s 静音 WAV 与不存在的路径），
//! 也不启动 mpv（视频链路需要真实叠加窗口，留给打包后的手工冒烟）。
//!
//! 注意：`test-support` feature 会让 `gpui_kit::*` 通配导入也带进 GPUI 的 `test` 属性，
//! 所以本模块不用通配导入，只导入用到的名字。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, TestAppContext, WindowBounds, WindowOptions,
    point, px, size,
};
use player_core::TrackMeta;

use crate::persistence::{self, Settings};
use crate::player_view::PlayerView;

/// 每个用例一个独立临时目录，避免并行执行时互相踩。
fn fresh_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "smp-ui-{tag}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录");
    // 把持久化重定向到临时目录：`play_track` 会写 playlist.json，
    // 不能污染真实的 %APPDATA%/simple-media-player。
    persistence::set_config_dir_for_test(dir.join("config"));
    dir
}

/// 写一个 `secs` 秒的 8kHz 单声道 16bit 静音 WAV（不产生声音，只为让 rodio 能解码）。
fn write_silent_wav(dir: &Path, secs: f64) -> PathBuf {
    const SAMPLE_RATE: u32 = 8_000;
    let samples = (SAMPLE_RATE as f64 * secs) as u32;
    let data_len = samples * 2;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(44 + data_len as usize, 0);
    let path = dir.join("silence.wav");
    std::fs::write(&path, bytes).expect("写入测试 WAV");
    path
}

fn track(title: &str, path: &Path, duration: f64) -> TrackMeta {
    TrackMeta {
        title: title.to_string(),
        artist: Some("测试".into()),
        duration_secs: Some(duration),
        path: path.to_string_lossy().to_string(),
    }
}

/// 打开一个 1040×680 的无头窗口并挂上 `PlayerView`。
fn open_view(
    cx: &mut TestAppContext,
    tracks: Vec<TrackMeta>,
) -> (AnyWindowHandle, Entity<PlayerView>) {
    cx.update(|cx| {
        // 主题 / 组件插件必须先初始化，否则渲染时取不到 Theme 全局状态。
        gpui_kit::init(cx);
        // 测试跑的是真实皮肤（Catppuccin Latte），别让断言在别一套配色下通过
        crate::theme::install(cx, false);
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(1040.), px(680.)),
                })),
                ..Default::default()
            },
            cx,
            move |window, cx| {
                let tracks = tracks.clone();
                cx.new(|cx| PlayerView::new(window, cx, tracks, None, Settings::default()))
            },
        )
        .expect("打开测试窗口")
    })
}

// ----------------------------------------------------------------------
// 无障碍：图标按钮必须有可读名称
// ----------------------------------------------------------------------

#[gpui_kit::test]
fn icon_buttons_expose_accessibility_labels(cx: &mut TestAppContext) {
    fresh_dir("a11y");
    let (window, _view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        for (id, label) in [
            ("add-file", "添加文件"),
            ("add-folder", "添加文件夹"),
            ("clear-playlist", "清空播放列表"),
            ("prev", "上一首"),
            ("play-pause", "播放"),
            ("next", "下一首"),
            ("theme-toggle", "切换到深色主题"),
        ] {
            assert_eq!(
                window.find(id).label(),
                Some(label),
                "{id} 缺少无障碍名称（读屏软件只会读出图标）"
            );
        }
        // 纯音频时不应出现影院按钮
        assert!(
            window.try_find("cinema").is_none(),
            "非视频模式不应渲染影院按钮"
        );
    })
    .unwrap();
}

// ----------------------------------------------------------------------
// 播放控制：指针事件驱动真实的 on_click
// ----------------------------------------------------------------------

#[gpui_kit::test]
fn clicking_play_pause_starts_then_pauses(cx: &mut TestAppContext) {
    let dir = fresh_dir("playpause");
    let wav = write_silent_wav(&dir, 2.0);
    let (window, view) = open_view(cx, vec![track("静音", &wav, 2.0)]);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let before = view.read(cx).snapshot();
        assert_eq!(before.current, None, "初始不应有当前曲目");
        assert!(!before.playing);

        // 空转态点播放：从列表第 0 首开始
        window.click("play-pause", cx);
        window.render_frame(cx);

        let playing = view.read(cx).snapshot();
        assert_eq!(playing.current, Some(0), "点播放应选中第 0 首");
        if playing.using_audio_device {
            assert!(playing.playing, "有音频设备时应进入播放态");
            assert!(
                (playing.seek_max - 2.0).abs() < 0.01,
                "进度条量程应取元数据时长 2.0s，实际 {}",
                playing.seek_max
            );
        }
        assert_eq!(
            window.find("play-pause").label(),
            Some(if playing.playing { "暂停" } else { "播放" }),
            "无障碍名称应跟随播放状态"
        );

        // 再点一次：暂停
        window.click("play-pause", cx);
        window.render_frame(cx);
        let paused = view.read(cx).snapshot();
        if paused.using_audio_device {
            assert!(!paused.playing, "第二次点击应暂停");
            assert_eq!(window.find("play-pause").label(), Some("播放"));
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn skip_buttons_wrap_around_playlist(cx: &mut TestAppContext) {
    let dir = fresh_dir("skip");
    let wav = write_silent_wav(&dir, 0.5);
    let (window, view) = open_view(
        cx,
        vec![
            track("第一首", &wav, 0.5),
            track("第二首", &wav, 0.5),
            track("第三首", &wav, 0.5),
        ],
    );

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("next", cx);
        window.render_frame(cx);
        // 无当前曲目时从第 0 首开始
        assert_eq!(view.read(cx).snapshot().current, Some(0));

        window.click("next", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).snapshot().current, Some(1));

        window.click("prev", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).snapshot().current, Some(0));

        // 首曲再往前：循环到末尾
        window.click("prev", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).snapshot().current, Some(2));
    })
    .unwrap();
}

#[gpui_kit::test]
fn play_on_empty_playlist_is_harmless(cx: &mut TestAppContext) {
    fresh_dir("empty");
    let (window, view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("play-pause", cx);
        window.click("next", cx);
        window.click("prev", cx);
        window.render_frame(cx);

        let snap = view.read(cx).snapshot();
        assert_eq!(snap.current, None, "空列表不应产生当前曲目");
        assert!(!snap.playing);
        assert!(snap.last_error.is_none(), "空列表是正常状态，不应报错");
    })
    .unwrap();
}

// ----------------------------------------------------------------------
// 错误处理：损坏/不存在的文件必须给出可见提示
// ----------------------------------------------------------------------

#[gpui_kit::test]
fn unplayable_file_reports_visible_error(cx: &mut TestAppContext) {
    let dir = fresh_dir("broken");
    let missing = dir.join("does-not-exist.mp3");
    let (window, view) = open_view(cx, vec![track("坏文件", &missing, 12.0)]);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("play-pause", cx);
        window.render_frame(cx);

        let snap = view.read(cx).snapshot();
        let message = snap
            .last_error
            .expect("文件不存在时必须给出可见错误提示，而不是静默失败");
        assert!(
            message.contains("坏文件"),
            "错误文案应点明是哪一首，实际: {message}"
        );
        assert_eq!(snap.current, None, "播放失败不应留下当前曲目");
    })
    .unwrap();
}

// ----------------------------------------------------------------------
// 主题切换
// ----------------------------------------------------------------------

#[gpui_kit::test]
fn theme_toggle_switches_mode_and_updates_label(cx: &mut TestAppContext) {
    fresh_dir("theme");
    let (window, _view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let dark_before = cx.theme().mode.is_dark();
        let label_before = window.find("theme-toggle").label().map(str::to_string);
        assert_eq!(
            label_before.as_deref(),
            Some(if dark_before {
                "切换到浅色主题"
            } else {
                "切换到深色主题"
            })
        );

        window.click("theme-toggle", cx);
        window.render_frame(cx);

        assert_ne!(cx.theme().mode.is_dark(), dark_before, "点击后应切换明暗");
        assert_eq!(
            window.find("theme-toggle").label(),
            Some(if dark_before {
                "切换到深色主题"
            } else {
                "切换到浅色主题"
            }),
            "无障碍名称应描述「切到哪一边」"
        );
    })
    .unwrap();
}

// ----------------------------------------------------------------------
// 布局：三栏结构在最小窗口尺寸下仍然成立
// ----------------------------------------------------------------------

#[gpui_kit::test]
fn layout_keeps_playlist_and_controls_inside_window(cx: &mut TestAppContext) {
    fresh_dir("layout");
    let (window, _view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);

        let add_file = window.find("add-file");
        let play = window.find("play-pause");
        let seek = window.find("seek-bar");
        let volume = window.find("volume-bar");
        let window_bounds = window.bounds();

        for (name, bounds) in [
            ("add-file", add_file.bounds()),
            ("play-pause", play.bounds()),
            ("seek-bar", seek.bounds()),
            ("volume-bar", volume.bounds()),
        ] {
            assert!(
                bounds.left() >= window_bounds.left() && bounds.right() <= window_bounds.right(),
                "{name} 横向溢出窗口: {bounds:?} vs {window_bounds:?}"
            );
            assert!(
                bounds.top() >= window_bounds.top() && bounds.bottom() <= window_bounds.bottom(),
                "{name} 纵向溢出窗口: {bounds:?} vs {window_bounds:?}"
            );
        }

        // 播放按钮应在窗口下半部分（控制栏），进度条在它上方
        assert!(play.bounds().top() > window_bounds.center().y);
        assert!(seek.bounds().bottom() <= play.bounds().top() + px(4.));
    })
    .unwrap();
}

/// 走带控制（上一首 / 播放 / 下一首）必须落在窗口水平正中。
///
/// 这里守的是一个很容易回归的布局写法：左右两栏宽度不等时（例如
/// `justify_between` 把音量也塞进左侧组），中间那组会被挤到偏左的位置。
#[gpui_kit::test]
fn transport_controls_are_horizontally_centered(cx: &mut TestAppContext) {
    fresh_dir("centered");
    let (window, _view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);

        let play = window.find("play-pause").bounds();
        let prev = window.find("prev").bounds();
        let next = window.find("next").bounds();
        let window_bounds = window.bounds();

        let offset = play.center().x - window_bounds.center().x;
        assert!(
            offset.as_f32().abs() <= 1.0,
            "播放按钮应居中：圆心偏移 {offset:?}（窗口 {window_bounds:?}，按钮 {play:?}）"
        );

        // 走带三键次序正确、间距对称
        assert!(prev.right() <= play.left(), "上一首应在播放左侧");
        assert!(next.left() >= play.right(), "下一首应在播放右侧");
        let left_gap = play.left() - prev.right();
        let right_gap = next.left() - play.right();
        assert!(
            (left_gap - right_gap).as_f32().abs() <= 1.0,
            "两侧间距应对称：{left_gap:?} vs {right_gap:?}"
        );
    })
    .unwrap();
}
