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
    point, px, size, AnyWindowHandle, AppContext as _, Bounds, Entity, ExternalPaths,
    FileDropEvent, InputEvent as _, Keystroke, TestAppContext, WindowBounds, WindowOptions,
};
use player_core::TrackMeta;
use smallvec::SmallVec;

use crate::first_run::{self, FirstRunState, FirstRunView, Launch};
use crate::persistence::{self, Recent, RecentEntry, Settings};
use crate::player_view::{
    chrome_visible_target, recent_title, seek_target, short_codec, PlayerView,
};

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
    write_silent_wav_named(dir, "silence.wav", secs)
}

/// 同上，但可以指定文件名（同一个用例里要几个**不同路径**的样片时用）。
fn write_silent_wav_named(dir: &Path, name: &str, secs: f64) -> PathBuf {
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
    let path = dir.join(name);
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
    open_view_with(cx, tracks, None)
}

/// 同上，但可以指定「启动即播放」的曲目（对应命令行 `--play`）。
fn open_view_with(
    cx: &mut TestAppContext,
    tracks: Vec<TrackMeta>,
    autoplay: Option<usize>,
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
                cx.new(|cx| {
                    PlayerView::new(window, cx, tracks, None, Settings::default(), autoplay)
                })
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
        // 新设计默认收起播放列表（面板悬浮在画面上），先展开再查面板里的入口
        window.click("toggle-playlist", cx);
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
        // 面板默认收起，先展开再量 add-file 的几何
        window.click("toggle-playlist", cx);
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

/// 舞台（视频/音频内容区）必须撑满标题带与底部胶囊之间。
///
/// 守的是一条很隐蔽的回归：`h_flex()` 默认 `items_center`，而舞台的子元素全是绝对
/// 定位（内容高度 0），漏了 `self_stretch` 的话舞台高度会塌成 0——GPUI 里画面上
/// 看不出异常（背景色由根节点铺满），但视频区 canvas 捕获到 `0x0`，mpv 叠加窗
/// 永远定不到位，表现为「有声音、黑屏」。
#[gpui_kit::test]
fn stage_fills_space_between_title_band_and_bottom_band(cx: &mut TestAppContext) {
    fresh_dir("stage");
    let (window, _view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);

        let stage = window.find("stage").bounds();
        let window_bounds = window.bounds();
        let height = stage.size.height.as_f32();

        assert!(
            height > window_bounds.size.height.as_f32() * 0.5,
            "舞台高度塌陷：{height}（窗口 {}），视频区会拿到 0 高度导致黑屏",
            window_bounds.size.height
        );
        assert!(
            (stage.left() - window_bounds.left()).as_f32().abs() <= 1.0
                && (stage.right() - window_bounds.right()).as_f32().abs() <= 1.0,
            "舞台应横向铺满：{stage:?} vs {window_bounds:?}"
        );
        // 顶部让开标题带、底部让开悬浮胶囊
        assert!(
            stage.top() > window_bounds.top(),
            "舞台不应盖住标题带：{stage:?}"
        );
        assert!(
            stage.bottom() < window_bounds.bottom(),
            "舞台应给底部胶囊留出空间：{stage:?}"
        );
    })
    .unwrap();
}

/// 视频模式：画面铺满**整窗**，标题带 / 胶囊 / 歌单压在画面上。
///
/// 两句都来自用户：「顶栏和控制栏未悬停在视频上方」「播放列表在视频上方，不挤视频位置」。
/// 分两层实现——GPUI 侧靠子节点声明顺序（后画的在上面），系统侧靠
/// `chrome_holes` 把这几块从 mpv 叠加窗的窗口区域里挖掉（`SetWindowRgn`）。
/// 这条用例守的是 GPUI 侧：画面必须铺满，chrome 必须落在画面范围内。
#[gpui_kit::test]
fn video_mode_floats_chrome_over_a_full_window_picture(cx: &mut TestAppContext) {
    fresh_dir("video-float");
    let (window, view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| view.test_set_video_mode(true));
        window.render_frame(cx);

        let window_bounds = window.bounds();
        let stage = window.find("stage").bounds();
        let near = |a: f32, b: f32| (a - b).abs() <= 1.0;

        assert!(
            near(stage.top().as_f32(), window_bounds.top().as_f32()),
            "画面要顶到窗口上沿（标题带压在画面上，不是让位）：{stage:?} vs {window_bounds:?}"
        );
        assert!(
            near(stage.bottom().as_f32(), window_bounds.bottom().as_f32()),
            "画面要铺到窗口下沿（胶囊压在画面上）：{stage:?} vs {window_bounds:?}"
        );
        assert!(
            near(stage.left().as_f32(), window_bounds.left().as_f32())
                && near(stage.right().as_f32(), window_bounds.right().as_f32()),
            "画面要铺满整个横向宽度：{stage:?} vs {window_bounds:?}"
        );

        // 胶囊落在画面范围内 = 它浮在画面上，而不是把画面顶上去
        let seek = window.find("seek-bar").bounds();
        assert!(
            seek.top() > stage.top() && seek.bottom() < stage.bottom(),
            "进度条应在画面内部：{seek:?} vs 画面 {stage:?}"
        );

        // 展开歌单：面板浮在右侧，画面宽度一像素都不该变
        window.click("toggle-playlist", cx);
        window.render_frame(cx);

        let stage_after = window.find("stage").bounds();
        let panel = window.find("playlist-panel").bounds();
        assert!(
            near(stage_after.size.width.as_f32(), stage.size.width.as_f32()),
            "展开歌单不能挤窄画面：{stage_after:?} vs {stage:?}"
        );
        assert!(
            near(stage_after.right().as_f32(), window_bounds.right().as_f32()),
            "歌单展开后画面仍铺到窗口右沿：{stage_after:?}"
        );
        assert!(
            panel.right() <= window_bounds.right()
                && panel.left() >= window_bounds.left()
                && panel.right() > stage_after.center().x,
            "歌单面板应浮在画面右侧且不出窗口：{panel:?} vs {window_bounds:?}"
        );
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

// ----------------------------------------------------------------------
// 文件拖放：外部文件拖进窗口应当「入列表 + 立刻播第一首」
// ----------------------------------------------------------------------

/// 拖入文件后应当把它加进播放列表并立刻起播。
///
/// 这条链路是 GPUI 内部完成的：系统拖放 → `FileDropEvent::Entered`（载荷 `ExternalPaths`）
/// → 内部拖拽 → `Submit` 变成 MouseUp → 鼠标下元素的 `on_drop`。
/// 以前根节点上什么都没挂，所以「拖进去毫无反应」。
#[gpui_kit::test]
fn dropping_files_adds_them_and_starts_playback(cx: &mut TestAppContext) {
    let dir = fresh_dir("drop");
    let wav = write_silent_wav(&dir, 1.0);
    let (window, view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        // 落在舞台中间：整窗都该是投放区，而不是只有控制栏那一条
        let position = point(px(520.), px(340.));
        let paths = ExternalPaths(SmallVec::from_vec(vec![wav.clone()]));
        window.dispatch_event(
            FileDropEvent::Entered { position, paths }.to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        window.dispatch_event(FileDropEvent::Submit { position }.to_platform_input(), cx);
        window.render_frame(cx);
    })
    .unwrap();

    // 读元数据走的是后台任务，等它回来
    cx.run_until_parked();

    cx.update(|cx| {
        let snapshot = view.read(cx).snapshot();
        assert_eq!(
            snapshot.current,
            Some(0),
            "拖入的文件应成为当前曲目（空列表时是第 0 首）"
        );
        assert!(
            snapshot.last_error.is_none(),
            "拖入已有文件不应报错：{:?}",
            snapshot.last_error
        );
        assert_eq!(
            snapshot.pending_info, 0,
            "拖进来就安静地播，不要排任何提示（用户明确要求去掉通知）"
        );
    });
}

/// 最大化状态下，窗口按钮必须换成「还原」。
///
/// 回归的是用户报的那个 bug 的一半：切换只发生在 GPUI 之外的 Win32 调用里
/// （`ShowWindowAsync`），如果图标文案还读 `window.is_maximized()` 或干脆写死，
/// 用户看到的就是「最大化之后按钮还是最大化」。
#[cfg(not(target_os = "macos"))]
#[gpui_kit::test]
fn maximize_button_reflects_window_state(cx: &mut TestAppContext) {
    fresh_dir("zoom-label");
    let (window, view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("win-max").label(),
            Some("最大化"),
            "普通状态下按钮应提示「最大化」"
        );

        view.update(cx, |view, _| view.test_set_maximized(true));
        window.render_frame(cx);
        assert_eq!(
            window.find("win-max").label(),
            Some("还原"),
            "最大化后按钮必须变成「还原」，否则用户不知道还能点回来"
        );

        view.update(cx, |view, _| view.test_set_maximized(false));
        window.render_frame(cx);
        assert_eq!(window.find("win-max").label(), Some("最大化"));
    })
    .unwrap();
}

// ----------------------------------------------------------------------
// 提示：视频模式不能用 in-app toast（被 mpv 叠加窗盖住）
// ----------------------------------------------------------------------

/// 视频模式下整块内容区被 mpv 的叠加窗口盖着，gpui-component 的 toast 落在里面
/// 等于不可见——用户拖进一个片子，界面上什么反馈都没有。所以视频模式改画标题带
/// 上的小胶囊（那条带子在叠加窗之上）。
#[gpui_kit::test]
fn video_mode_uses_title_band_pill_instead_of_toast(cx: &mut TestAppContext) {
    fresh_dir("pill");
    let (window, view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);

        // 音频模式：提示走右上角 toast，标题带里不该有胶囊
        view.update(cx, |view, cx| {
            view.test_notify("已导入 1 个文件", window, cx);
        });
        window.render_frame(cx);
        assert!(
            window.try_find("status-pill").is_none(),
            "音频模式应走 toast，不要在标题带里叠胶囊"
        );

        // 视频模式：同一条提示改画胶囊，而且必须落在标题带内
        view.update(cx, |view, cx| {
            view.test_set_video_mode(true);
            view.test_notify("已导入 1 个文件", window, cx);
        });
        window.render_frame(cx);

        let bounds = window
            .try_find("status-pill")
            .expect("视频模式必须在标题带里画出状态胶囊")
            .bounds();
        assert!(
            bounds.bottom() <= px(crate::player_view::TITLE_BAND),
            "胶囊要压在标题带内（否则会被叠加窗盖住），实际: {bounds:?}"
        );
        assert!(
            view.read(cx)
                .snapshot()
                .pill
                .is_some_and(|text| text.contains("已导入 1 个文件")),
            "胶囊文案应是刚发生的提示"
        );
    })
    .unwrap();
}

/// 视频模式下主窗口的窗口区域必须**正好**盖住实际画出来的 chrome。
///
/// 这是第七轮最容易被静默破坏的契约：画面区从主窗口区域里减掉之后，
/// 没被 `chrome_rects` 列进来的控件会被系统整块剪掉，而"被剪掉"和"没画"
/// 在截图里长得一样，只能靠几何对齐来守。
///
/// 反过来也不能多列：多出来的矩形等于让主窗口在不透明表面之外的地方
/// 也参与绘制，会把画面盖住。
#[gpui_kit::test]
fn chrome_region_covers_capsule_and_panel(cx: &mut TestAppContext) {
    fresh_dir("region-rects");
    let (window, view) = open_view(cx, Vec::new());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| view.test_set_video_mode(true));
        // 胶囊高度是内容撑开的，必须先把一帧画出来，`capsule_rect` 才有值
        window.render_frame(cx);

        let rects = view.update(cx, |view, _| view.test_chrome_rects(window));
        assert!(
            rects.len() >= 2,
            "视频模式的区域至少要含标题带和胶囊，实际 {rects:?}"
        );

        // 注意：`covered` 必须显式收 rects。写成闭包捕获的话，后面 `let rects = ...`
        // 重新绑定只会遮蔽外层变量，闭包仍然指着**第一版**（还没含面板），
        // 断言就会假失败——这条踩过一次。
        fn covered(rects: &[crate::ui_state::Rect], x: f32, y: f32) -> bool {
            rects
                .iter()
                .any(|r| x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h)
        }

        // 标题带左端（拖动区）和窗口按钮都在里面（mac 没有自绘窗口按钮）
        assert!(covered(&rects, 8., 17.), "标题带必须在区域里：{rects:?}");
        if !cfg!(target_os = "macos") {
            let max_button = window.find("win-max").bounds();
            assert!(
                covered(
                    &rects,
                    max_button.center().x.as_f32(),
                    max_button.center().y.as_f32()
                ),
                "最大化按钮必须在区域里，否则点不到：{rects:?} vs {max_button:?}"
            );
        }

        // 胶囊里的控件同理
        let seek = window.find("seek-bar").bounds();
        assert!(
            covered(&rects, seek.center().x.as_f32(), seek.center().y.as_f32()),
            "进度条必须在区域里，否则整条走带都被剪掉：{rects:?} vs {seek:?}"
        );
        let seek_bottom = seek.bottom().as_f32();
        assert!(
            covered(&rects, seek.center().x.as_f32(), seek_bottom + 20.),
            "胶囊的控制行（进度条下方）也要在区域里：{rects:?} vs 进度条底 {seek_bottom:.1}"
        );

        // 画面中心必须**不**在区域里——那正好是减法要空出来的地方
        let bounds = window.bounds();
        assert!(
            !covered(
                &rects,
                bounds.center().x.as_f32(),
                bounds.center().y.as_f32()
            ),
            "画面中心不该留在主窗口区域里，否则 mpv 会被判成完全遮挡而停止出帧：{rects:?}"
        );

        // 展开歌单：面板整块浮在画面上，区域必须把它加回去
        window.click("toggle-playlist", cx);
        window.render_frame(cx);
        let rects = view.update(cx, |view, _| view.test_chrome_rects(window));
        let panel = window.find("playlist-panel").bounds();
        assert!(
            covered(&rects, panel.center().x.as_f32(), panel.center().y.as_f32()),
            "歌单面板必须在区域里，否则展开后是一片画面：{rects:?} vs {panel:?}"
        );
    })
    .unwrap();
}

// ----------------------------------------------------------------------
// 全屏播放（影院模式）：入口、出口、顶栏宽度
// ----------------------------------------------------------------------

/// 双击画面 → 全屏播放，`Esc` → 回到窗口。
///
/// 用户报的是三连：「双击时不是全屏播放」「全屏后 esc 不能返回窗口」。
/// 之前双击走的是最大化（等价双击标题栏），而全屏之后**根本没有任何键盘出口**——
/// 应用里一条 `bind_keys` 都没有，`Esc` 从来没被派发过。
#[gpui_kit::test]
fn double_click_enters_cinema_and_escape_returns_to_window(cx: &mut TestAppContext) {
    fresh_dir("cinema-keys");
    let (window, view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| view.test_set_video_mode(true));
        window.render_frame(cx);
        assert!(!view.read(cx).test_cinema(), "视频模式默认不该是全屏");

        // 画面上的双击：视频窗把事件排队，tick 取走 → 切全屏
        view.update(cx, |view, cx| {
            view.test_toggle_cinema(window, cx);
        });
        assert!(view.read(cx).test_cinema(), "双击画面应进入全屏播放");
        assert!(
            window.is_fullscreen(),
            "影院模式必须真的把窗口切成全屏，否则只是看起来像"
        );

        // Esc 走的是真实键盘链路：PlatformInput::KeyDown → App 级 keystroke 拦截
        let handled = window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
        assert!(handled, "Esc 应被 PlayerView 的按键拦截吃掉");
        assert!(!view.read(cx).test_cinema(), "Esc 应退出全屏播放");
        assert!(!window.is_fullscreen(), "Esc 之后窗口不能再是全屏态");

        // 不在全屏时不该吞 Esc（将来有对话框/弹层要靠它关闭）
        let handled = window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
        assert!(!handled, "窗口态下 Esc 应继续往下派发");
    })
    .unwrap();
}

/// 全屏时点「最大化」按钮 = 回到窗口。
///
/// 全屏时 GPUI 把 `WS_MAXIMIZEBOX` / `WS_CAPTION` 一起摘掉了，此时再
/// `ShowWindowAsync(SW_MAXIMIZE)` 会把窗口弄成「铺满工作区、但 GPUI 仍认为在全屏」
/// 的错乱态——用户点完的感受就是「回不到窗口」。
#[gpui_kit::test]
fn maximize_button_leaves_cinema(cx: &mut TestAppContext) {
    fresh_dir("cinema-max");
    let (window, view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| view.test_set_video_mode(true));
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            view.test_toggle_cinema(window, cx);
        });
        assert!(view.read(cx).test_cinema());

        view.update(cx, |view, cx| view.test_toggle_maximize(window, cx));
        assert!(
            !view.read(cx).test_cinema(),
            "全屏时「最大化」应退回窗口模式"
        );
        assert!(!window.is_fullscreen(), "退回之后窗口不能还是全屏");
        assert!(
            !view.read(cx).test_maximized(),
            "退回窗口后不该顺手把自己标成最大化（图标会变成「还原」）"
        );
    })
    .unwrap();
}

/// mac 用系统红绿灯：标题带里不能再有自绘的窗口按钮，标题文字要让开左上角。
#[cfg(target_os = "macos")]
#[gpui_kit::test]
fn mac_title_band_leaves_window_buttons_to_the_system(cx: &mut TestAppContext) {
    fresh_dir("band-mac");
    let (window, view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        for video in [false, true] {
            view.update(cx, |view, _| view.test_set_video_mode(video));
            window.render_frame(cx);

            for id in ["win-min", "win-max", "win-close"] {
                assert!(
                    window.try_find(id).is_none(),
                    "mac 不该自绘 {id}（video={video}），红绿灯由系统画"
                );
            }
            let drag = window.find("title-drag").bounds();
            let (light_x, _) = crate::player_view::TRAFFIC_LIGHT_ORIGIN;
            assert!(
                drag.left().as_f32() >= light_x + 14.0 * 3.0 + 6.0 * 2.0,
                "标题文字会压到红绿灯上（video={video}）：{drag:?}"
            );
        }
    })
    .unwrap();
}

/// 标题带必须铺满整窗宽度（含右上角那三颗窗口按钮）。
///
/// 用户报「顶栏没有铺满宽度」。布局这一层守的是：三个窗口按钮贴着窗口右沿、
/// 标题带在窗口区域里就是 `(0, 0, 窗口宽, TITLE_BAND)` —— 区域少算一段，
/// 那一段就会被系统剪掉，看着就是「顶栏缺了一角」。
#[gpui_kit::test]
fn title_band_spans_full_width(cx: &mut TestAppContext) {
    fresh_dir("band-width");
    let (window, view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        for video in [false, true] {
            view.update(cx, |view, _| view.test_set_video_mode(video));
            window.render_frame(cx);

            let bounds = window.bounds();
            if !cfg!(target_os = "macos") {
                let close = window.find("win-close").bounds();
                assert!(
                    (close.right() - bounds.right()).as_f32().abs() <= 1.0,
                    "关闭按钮应贴着窗口右沿（video={video}）：按钮 {close:?} vs 窗口 {bounds:?}"
                );
                assert!(
                    (close.top() - bounds.top()).as_f32().abs() <= 1.0,
                    "窗口按钮应贴着窗口上沿（video={video}）：按钮 {close:?} vs 窗口 {bounds:?}"
                );
            }

            let rects = view.read(cx).test_chrome_rects(window);
            assert!(
                rects.iter().any(|rect| rect.x <= 0.5
                    && rect.y <= 0.5
                    && (rect.w - bounds.size.width.as_f32()).abs() <= 1.0
                    && rect.h >= 30.0),
                "标题带在窗口区域里必须覆盖整宽（video={video}）：{rects:?} vs 窗口宽 {}",
                bounds.size.width.as_f32()
            );
        }
    })
    .unwrap();
}

// ----------------------------------------------------------------------
// 方向键：左右跳转、上下音量
// ----------------------------------------------------------------------

/// 方向键必须走真实键盘链路（`PlatformInput::KeyDown` → App 级 keystroke 拦截）。
///
/// 为什么不在滑块上挂键盘：gpui-component 0.7.3 的 `Slider` 没有 `FocusHandle`、
/// 也不参与键盘（上游硬限制），焦点永远落不到它身上。所以这层拦截是**唯一**能让
/// 方向键生效的地方，必须有测试盯着——写错 key 名不会报错，只会静默失效。
#[gpui_kit::test]
fn arrow_keys_seek_and_adjust_volume(cx: &mut TestAppContext) {
    let dir = fresh_dir("arrows");
    let wav = write_silent_wav(&dir, 30.0);
    let (window, view) = open_view(cx, vec![track("长曲", &wav, 30.0)]);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);

        // 1. 没有曲目：左右键不该被吃掉（留给将来的列表导航）
        assert!(
            !window.dispatch_keystroke(Keystroke::parse("left").unwrap(), cx),
            "空列表时左右键应继续往下派发"
        );

        // 2. 上下 = 音量 ±5%，而且滑块的拇指要跟着走
        let base = view.read(cx).snapshot().volume_pct;
        assert_eq!(base, 70, "默认音量 70%");
        assert!(
            window.dispatch_keystroke(Keystroke::parse("up").unwrap(), cx),
            "上键应被吃掉（否则会被别的控件当成滚动）"
        );
        window.render_frame(cx);
        assert_eq!(view.read(cx).snapshot().volume_pct, base + 5);
        let slider = view.read(cx).test_volume_slider();
        assert_eq!(
            slider.read(cx).value().start(),
            (base + 5) as f32,
            "音量滑块的拇指必须跟着动：只改镜像字段的话声音变了、圆点不动，看着像没反应"
        );

        assert!(window.dispatch_keystroke(Keystroke::parse("down").unwrap(), cx));
        window.render_frame(cx);
        assert_eq!(view.read(cx).snapshot().volume_pct, base, "下键应退回一档");

        // 顶到 100 之后继续按不该越界（也不该反复写盘）
        for _ in 0..10 {
            window.dispatch_keystroke(Keystroke::parse("up").unwrap(), cx);
        }
        window.render_frame(cx);
        assert_eq!(view.read(cx).snapshot().volume_pct, 100, "音量上限是 100%");

        // 3. 起播后左右 = ±5 秒
        window.click("play-pause", cx);
        window.render_frame(cx);
        let started = view.read(cx).snapshot();
        assert_eq!(started.current, Some(0), "点播放应选中第 0 首");
        if !started.using_audio_device {
            // 没有音频设备时引擎是 None，位置永远是 0——只能验「键被吃掉」
            assert!(
                window.dispatch_keystroke(Keystroke::parse("right").unwrap(), cx),
                "有曲目时右键必须被吃掉"
            );
            return;
        }

        let before = started.position;
        assert!(
            window.dispatch_keystroke(Keystroke::parse("right").unwrap(), cx),
            "有曲目时右键必须被吃掉"
        );
        let after = view.read(cx).snapshot().position;
        assert!(
            (after - (before + 5.0)).abs() < 1.5,
            "右键应前进约 5 秒：{before} -> {after}"
        );

        // 落点夹在 [0, 时长] 之间：越界后播放器会自己回弹，用户看到的就是
        // 「按右键进度反而往回退」。纯函数，直接验边界。
        assert_eq!(seek_target(1.0, -5.0, 30.0), 0.0, "不能退到负数");
        assert_eq!(seek_target(29.0, 5.0, 30.0), 30.0, "不能越过时长");

        // 播完之后引擎是 Stopped，`seek` 在那一刻是空操作——方向键会变成
        // 「按了没反应」。停止态按左键必须能重新起播到目标位置。
        // （无头测试不跑 tick 的自动连播，这里直接把引擎按回停止态来模拟）
        view.update(cx, |view, _| view.test_stop_engine());
        assert!(window.dispatch_keystroke(Keystroke::parse("left").unwrap(), cx));
        window.render_frame(cx);
        let revived = view.read(cx).snapshot();
        assert_eq!(
            revived.last_error, None,
            "停止态按左键不该报错：{:?}",
            revived.last_error
        );
        assert!(revived.playing, "停止态按左键应重新起播，而不是纹丝不动");
    })
    .unwrap();
}

// ----------------------------------------------------------------------
// 控制栏 v3：固定 760 宽 + 时间夹滑块 + ⓘ 视频信息卡
// ----------------------------------------------------------------------

/// 设计稿口径：胶囊**固定 760×92**、居中；进度区两行（时间行 16 → 4 → 轨道 714，
/// 两侧各 3 的光学修正）；右组 6 键中心距 40；走带中心距 42/54。
///
/// 这些都是写死的数字，一旦被 `flex_1`/`fill_container` 带跑，界面上看起来只是
/// "有点不一样"，很难用手眼发现——所以必须有测试直接量。
#[gpui_kit::test]
fn capsule_is_760_by_92_and_progress_is_two_rows(cx: &mut TestAppContext) {
    fresh_dir("capsule-v3");
    let (window, view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| view.test_set_video_mode(true));
        window.render_frame(cx);

        let bounds = window.bounds();
        let rects = view.read(cx).test_chrome_rects(window);
        // 顺序：0 标题带、1 底部渐隐遮罩、2 胶囊（挖洞时各自带 1.5 的抗锯齿留白）
        let capsule = rects.get(2).expect("视频模式必须有胶囊的窗口区域");
        assert!(
            (capsule.w - 760.0).abs() <= 3.5,
            "胶囊宽应贴设计稿 760（含留白），实际 {}",
            capsule.w
        );
        assert!(
            (capsule.h - 92.0).abs() <= 3.5,
            "胶囊高应贴设计稿 92（含留白），实际 {}",
            capsule.h
        );
        let capsule_center = capsule.x + capsule.w / 2.0;
        assert!(
            (capsule_center - bounds.size.width.as_f32() / 2.0).abs() <= 2.0,
            "胶囊必须水平居中：中心 {capsule_center} vs 窗口 {}",
            bounds.size.width.as_f32()
        );

        // 进度区：轨道满内宽 − 2×3（714），时间行在它**上面**
        let seek = window.find("seek-bar").bounds();
        assert!(
            (seek.size.width.as_f32() - 714.0).abs() <= 1.0,
            "进度轨道应 714（760 − 2×20 − 2×3），实际 {}",
            seek.size.width.as_f32()
        );
        let left = window.find("time-current").bounds();
        let right = window.find("time-remaining").bounds();
        assert!(
            left.bottom().as_f32() <= seek.top().as_f32() + 0.5,
            "时间行必须在轨道上方（设计稿：时间行 16 + 间距 4 + 轨道 4）：{left:?} vs {seek:?}"
        );
        assert!(
            right.bottom().as_f32() <= seek.top().as_f32() + 0.5,
            "两端时间码都在轨道上方：{right:?} vs {seek:?}"
        );
        // 两端对齐：左段贴内左沿，右段贴内右沿
        assert!(
            (left.left().as_f32() - seek.left().as_f32()).abs() <= 1.5,
            "当前时间应贴轨道左端（都带 3px 光学修正）：{left:?} vs {seek:?}"
        );
        assert!(
            (right.right().as_f32() - seek.right().as_f32()).abs() <= 1.5,
            "剩余时间应贴轨道右端：{right:?} vs {seek:?}"
        );

        // 音量条 96（设计稿），ⓘ 在它左边
        let volume = window.find("volume-bar").bounds();
        assert!(
            (volume.size.width.as_f32() - 96.0).abs() <= 1.0,
            "音量条应 96 宽，实际 {}",
            volume.size.width.as_f32()
        );
        let info = window.find("video-info").bounds();
        assert!(
            info.right().as_f32() <= volume.left().as_f32() + 0.5,
            "ⓘ 应在音量组左侧（设计稿：左组 = ⓘ + gap14 + 音量）"
        );

        // 播放键 44 正圆，而且真的在胶囊中线上
        let play = window.find("play-pause").bounds();
        assert_eq!(play.size.width.as_f32(), 44.0, "播放键宽 44");
        assert_eq!(play.size.height.as_f32(), 44.0, "播放键高 44（正圆）");
        let play_center = play.left().as_f32() + play.size.width.as_f32() / 2.0;
        assert!(
            (play_center - capsule_center).abs() <= 2.0,
            "播放键必须在胶囊正中：{play_center} vs {capsule_center}"
        );

        // 走带节奏：播放↔±10 = 54、±10↔上/下一首 = 42（设计稿实测中心距）
        let back = window.find("seek-back-10").bounds();
        let fwd = window.find("seek-fwd-10").bounds();
        let prev = window.find("prev").bounds();
        let next = window.find("next").bounds();
        let center =
            |b: gpui_kit::Bounds<gpui_kit::Pixels>| b.left().as_f32() + b.size.width.as_f32() / 2.0;
        assert!(
            (center(back) - (play_center - 54.0)).abs() <= 2.0,
            "快退键中心距播放键 54，实际 {:.1}",
            play_center - center(back)
        );
        assert!(
            (center(fwd) - (play_center + 54.0)).abs() <= 2.0,
            "快进键中心距播放键 54，实际 {:.1}",
            center(fwd) - play_center
        );
        assert!(
            (center(prev) - (play_center - 96.0)).abs() <= 2.0,
            "上一首中心距播放键 96，实际 {:.1}",
            play_center - center(prev)
        );
        assert!(
            (center(next) - (play_center + 96.0)).abs() <= 2.0,
            "下一首中心距播放键 96，实际 {:.1}",
            center(next) - play_center
        );

        // 右组：字幕 / 音轨 / 主题 / 列表 / 全屏，中心距 40，最右贴内右沿
        let subtitles = center(window.find("subtitles").bounds());
        let audio = center(window.find("audio-track").bounds());
        let theme = center(window.find("theme-toggle").bounds());
        let playlist = center(window.find("toggle-playlist").bounds());
        let cinema = center(window.find("cinema").bounds());
        for (name, a, b) in [
            ("字幕→音轨", subtitles, audio),
            ("音轨→主题", audio, theme),
            ("主题→列表", theme, playlist),
            ("列表→全屏", playlist, cinema),
        ] {
            assert!(
                (b - a - 40.0).abs() <= 1.5,
                "右组中心距应为 40（{name}），实际 {:.1}",
                b - a
            );
        }
        // 最右键中心 = 区域右沿 − 1.5（挖洞留白）− 1（描边）− 20（padding）− 12（半个键）
        assert!(
            (cinema - (capsule.x + capsule.w - 34.5)).abs() <= 2.5,
            "右组最右键应贴胶囊内右沿：{cinema} vs {}",
            capsule.x + capsule.w
        );
    })
    .unwrap();
}

/// ⓘ 打开的信息卡：240×180、贴左下、**并且要进窗口区域**。
///
/// 最后一条最容易漏：卡片画在画面上，不进 `chrome_rects` 就会被叠加窗整块盖住，
/// 用户看到的是"点了 ⓘ 什么都没发生"（和胶囊当初一个坑）。
#[gpui_kit::test]
fn info_card_toggles_and_is_carved_out(cx: &mut TestAppContext) {
    fresh_dir("info-card");
    let (window, view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| view.test_set_video_mode(true));
        window.render_frame(cx);
        assert!(
            window.try_find("video-info-card").is_none(),
            "默认不该展开信息卡"
        );
        // 关着的时候 chrome 只有标题带 + 底部遮罩 + 胶囊
        assert_eq!(view.read(cx).test_chrome_rects(window).len(), 3);

        window.click("video-info", cx);
        window.render_frame(cx);
        assert!(view.read(cx).test_info_open(), "点 ⓘ 应展开信息卡");

        let card = window.find("video-info-card").bounds();
        assert_eq!(card.size.width.as_f32(), 240.0, "信息卡宽 240");
        assert_eq!(card.size.height.as_f32(), 180.0, "信息卡高 180");
        assert!(
            (card.left().as_f32() - 24.0).abs() <= 0.5,
            "信息卡贴画面左侧 24，实际 {}",
            card.left().as_f32()
        );

        let rects = view.read(cx).test_chrome_rects(window);
        assert_eq!(rects.len(), 4, "展开后 chrome 应多一块（信息卡）");
        // 区域比布局框多一圈 1.5 抗锯齿留白，`canvas` 量的又比布局框略小一点（边框 +
        // 内边距的盒模型差），所以按 ±3 比，不追那零点几像素
        assert!(
            rects.iter().any(|rect| {
                (rect.x - card.left().as_f32()).abs() <= 3.0
                    && (rect.w - card.size.width.as_f32()).abs() <= 3.0
            }),
            "信息卡必须进窗口区域，否则会被叠加窗盖住：{rects:?}"
        );

        // 再点一次收起，chrome 也要收回去（否则画面上留一块"洞"）
        window.click("video-info", cx);
        window.render_frame(cx);
        assert!(!view.read(cx).test_info_open());
        assert_eq!(view.read(cx).test_chrome_rects(window).len(), 3);
    })
    .unwrap();
}

/// 编码串只留第一段。
///
/// mpv 报的是并列别名（`h264 (High) / AVC / MPEG-4 AVC / MPEG-4`），整串塞进
/// 240 宽的卡片会冲出正文边界（实机截图里那段字已经压到画面上）。
#[test]
fn codec_string_keeps_only_its_first_alias() {
    assert_eq!(
        short_codec("h264 (High) / AVC / MPEG-4 AVC / MPEG-4"),
        "h264 (High)"
    );
    assert_eq!(short_codec("aac / AAC (LC)"), "aac");
    // 单段/空串不能被切断或留下空白
    assert_eq!(short_codec("vp9"), "vp9");
    assert_eq!(short_codec(""), "");
}

// ----------------------------------------------------------------------
// 播放列表面板 v2：几何、tab、循环/随机/排序
// ----------------------------------------------------------------------

/// 设计稿口径：面板 320 宽、贴右 24、顶边 = 标题带 + 8。
///
/// 之前是 `top: TITLE_BAND+6 / right: 12 / bottom: BOTTOM_BAND`，宽高都靠窗口
/// 撑，和设计稿的固定 320 差两圈；这类"整体缩进一格"的偏差光看截图很难发现。
#[gpui_kit::test]
fn playlist_panel_matches_design_geometry(cx: &mut TestAppContext) {
    fresh_dir("panel-geo");
    let (window, _view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-playlist", cx);
        window.render_frame(cx);

        let panel = window.find("playlist-panel").bounds();
        let window_bounds = window.bounds();
        assert_eq!(panel.size.width.as_f32(), 320.0, "面板宽 320");
        assert_eq!(
            panel.top().as_f32(),
            crate::player_view::TITLE_BAND + 8.0,
            "面板顶边 = 标题带 + 8"
        );
        assert_eq!(
            window_bounds.right().as_f32() - panel.right().as_f32(),
            24.0,
            "面板贴右 24"
        );

        // 头部两枚 tab + 关闭；底部七个入口
        for id in [
            "tab-playlist",
            "tab-chapters",
            "playlist-close",
            "repeat-mode",
            "shuffle-mode",
            "sort-mode",
            "add-file",
            "add-folder",
            "remove-track",
            "clear-playlist",
        ] {
            assert!(
                window.try_find(id).is_some(),
                "面板上应该有 {id}（设计稿的头部/底部都在这一层）"
            );
        }
    })
    .unwrap();
}

/// 循环 / 随机 / 排序：点一下真的改状态，排序还会重排列表。
///
/// 三个按钮都在面板底部，是设计稿里**唯一**没有对应功能的那一组——不能只画不管。
#[gpui_kit::test]
fn repeat_shuffle_and_sort_modes_work(cx: &mut TestAppContext) {
    let dir = fresh_dir("panel-modes");
    // 三个**不同**的文件：「加入顺序」是按路径记的，都用同一个样片就没法区分
    let a = write_silent_wav(&dir, 0.5);
    let b = write_silent_wav_named(&dir, "b.wav", 0.5);
    let c = write_silent_wav_named(&dir, "c.wav", 0.5);
    let (window, view) = open_view(
        cx,
        vec![
            track("Banana", &a, 0.5),
            track("Apple", &b, 0.5),
            track("Cherry", &c, 0.5),
        ],
    );

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-playlist", cx);
        window.render_frame(cx);

        use crate::persistence::{RepeatMode, SortOrder};
        assert_eq!(
            view.read(cx).test_play_modes(),
            (RepeatMode::All, false, SortOrder::Added),
            "默认：列表循环 + 不随机 + 加入顺序"
        );

        window.click("repeat-mode", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).test_play_modes().0, RepeatMode::One);

        window.click("shuffle-mode", cx);
        window.render_frame(cx);
        assert!(view.read(cx).test_play_modes().1, "点一下随机要打开");

        window.click("sort-mode", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).test_play_modes().2, SortOrder::Title);
        assert_eq!(
            view.read(cx).test_titles(cx),
            vec![
                "Apple".to_string(),
                "Banana".to_string(),
                "Cherry".to_string()
            ],
            "按标题排序后列表要真的重排"
        );

        // 再点两下：时长 → 加入顺序（还原）
        window.click("sort-mode", cx);
        window.render_frame(cx);
        window.click("sort-mode", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).test_play_modes().2, SortOrder::Added);
        assert_eq!(
            view.read(cx).test_titles(cx),
            vec![
                "Banana".to_string(),
                "Apple".to_string(),
                "Cherry".to_string()
            ],
            "「加入顺序」要能还原成最初拖进来的排列"
        );
    })
    .unwrap();
}

/// 视频的时长要能补进轨道元数据（入列时问不到，mpv 起播后才报）。
///
/// 页脚那行「N 个文件 · 总时长」和行内第二行都读 `duration_secs`；视频轨入列时是
/// `None`，不补就永远显示「1 个文件」。同一条 tick 每帧都会调，所以「重复调用不能
/// 反复改」也是判据——否则每帧都要落盘一次。
#[gpui_kit::test]
fn video_duration_is_backfilled_once(cx: &mut TestAppContext) {
    let dir = fresh_dir("duration-backfill");
    let a = write_silent_wav(&dir, 0.5);
    let (window, view) = open_view(cx, vec![track("A", &a, 0.5)]);

    cx.update_window(window, |_, _window, cx| {
        let wrote = view.update(cx, |v, cx| v.test_backfill_duration(0, 116.4, cx));
        assert!(wrote, "第一次补时长必须写进去");
        let again = view.update(cx, |v, cx| v.test_backfill_duration(0, 116.4, cx));
        assert!(!again, "同一个值不该反复写（tick 每帧都会调）");
        let changed = view.update(cx, |v, cx| v.test_backfill_duration(0, 130.0, cx));
        assert!(changed, "时长明显变了要以 mpv 为准");
    })
    .unwrap();
}

/// 面板的 tab 要能切到「章节」页，且收面板时回到「播放列表」页。
#[gpui_kit::test]
fn chapter_tab_switches_and_resets_on_close(cx: &mut TestAppContext) {
    fresh_dir("panel-chapters");
    let (window, view) = open_view(cx, Vec::new());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-playlist", cx);
        window.render_frame(cx);
        assert!(!view.read(cx).test_chapter_tab());

        window.click("tab-chapters", cx);
        window.render_frame(cx);
        assert!(view.read(cx).test_chapter_tab(), "点「章节」应切页");

        window.click("playlist-close", cx);
        window.render_frame(cx);
        assert!(
            !view.read(cx).test_chapter_tab(),
            "收面板要回到「播放列表」，否则下次展开是一张空页"
        );
    })
    .unwrap();
}

// ----------------------------------------------------------------------
// 控制层自动淡出（视频播放中 2.5s 没输入）+ 淡出后区域收缩
// ----------------------------------------------------------------------

/// 淡出的判据：只有「视频 + 播放中 + 没在碰控件 + 超过 2.5s 没输入」才收起来。
///
/// 这条容易写反（反过来就是"看视频时控制栏永远在"或者"暂停了还自己消失"），
/// 而界面上很难一眼看出来——所以逐条钉死。
#[test]
fn chrome_fades_only_when_a_video_is_playing_and_idle() {
    use std::time::Duration;
    const IDLE: Duration = Duration::from_millis(2500);

    // 播放中、没输入、超过 2.5s → 淡出
    assert!(!chrome_visible_target(true, true, false, IDLE));
    assert!(!chrome_visible_target(
        true,
        true,
        false,
        IDLE + Duration::from_millis(500)
    ));
    // 差一点到 2.5s 还不淡（用户刚动过鼠标）
    assert!(chrome_visible_target(
        true,
        true,
        false,
        IDLE - Duration::from_millis(1)
    ));
    // 暂停：用户停下来就是为了找控件
    assert!(chrome_visible_target(true, false, false, IDLE * 4));
    // 正在拖进度 / 面板开着 / 信息卡开着 / 拖放提示 / 状态胶囊
    assert!(chrome_visible_target(true, true, true, IDLE * 4));
    // 音频模式：标题带和胶囊是界面本体，不存在"淡出"
    assert!(chrome_visible_target(false, true, false, IDLE * 4));
}

/// 淡到 0 之后，`chrome_rects` 必须**一个都不留**。
///
/// 它现在是验收脚本与测试共用的几何真相：脚本凭它算「控件在哪」，测试凭它断言
/// 「淡出后不该再有控件」。所以「淡掉」必须同时体现为「布局里没有 chrome」——
/// 只画成透明会让探针继续报告一组看不见的按钮，脚本照着点就会点空。
/// 反过来，淡出/淡入的中途（alpha 0.5）还得留着，否则玻璃会闪一下才回来。
#[gpui_kit::test]
fn hidden_chrome_leaves_no_rects(cx: &mut TestAppContext) {
    fresh_dir("chrome-fade-region");
    let (window, view) = open_view(cx, Vec::new());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| view.test_set_video_mode(true));
        window.render_frame(cx);
        assert!(
            !view.read(cx).test_chrome_rects(window).is_empty(),
            "亮着的时候必须报告 chrome"
        );

        // 淡到一半：还在区域里（玻璃正在变透明，但整块仍是主窗口画的）
        view.update(cx, |view, _| view.test_set_chrome_alpha(0.5));
        window.render_frame(cx);
        assert!(
            !view.read(cx).test_chrome_rects(window).is_empty(),
            "淡出途中不能提前清空，否则玻璃会闪一下才回来"
        );

        // 完全淡掉：一个矩形都不留
        view.update(cx, |view, _| view.test_set_chrome_alpha(0.0));
        window.render_frame(cx);
        assert!(
            view.read(cx).test_chrome_rects(window).is_empty(),
            "淡出完成后必须一块都不留，否则探针会报告一组看不见的按钮"
        );

        // 鼠标一动 → 淡回来 → 布局跟着回来
        view.update(cx, |view, _| view.test_set_chrome_alpha(1.0));
        window.render_frame(cx);
        assert!(!view.read(cx).test_chrome_rects(window).is_empty());
    })
    .unwrap();
}

// ----------------------------------------------------------------------
// 首屏：最近打开 / 空 / 拖入中 三态，选好之后交给 on_launch
// ----------------------------------------------------------------------

type Launched = std::rc::Rc<std::cell::RefCell<Vec<Launch>>>;

fn recent_entry(path: &Path, duration: f64, position: f64) -> RecentEntry {
    RecentEntry {
        path: path.to_string_lossy().replace('\\', "/"),
        title: recent_title(&path.to_string_lossy()),
        duration_secs: Some(duration),
        position_secs: position,
    }
}

/// 560×440 的首屏窗口；`on_launch` 收到的东西记进返回的列表里。
fn open_first_run(
    cx: &mut TestAppContext,
    recent: Recent,
) -> (AnyWindowHandle, Entity<FirstRunView>, Launched) {
    let launched = Launched::default();
    let sink = launched.clone();
    let (window, view) = cx.update(|cx| {
        gpui_kit::init(cx);
        crate::theme::install(cx, false);
        let (w, h) = first_run::WINDOW_SIZE;
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(w), px(h)),
                })),
                ..Default::default()
            },
            cx,
            |window, cx| {
                let on_launch: first_run::OnLaunch =
                    std::rc::Rc::new(move |launch, _, _| sink.borrow_mut().push(launch));
                cx.new(|cx| FirstRunView::new(recent, on_launch, window, cx))
            },
        )
        .expect("打开首屏窗口")
    });
    (window, view, launched)
}

#[gpui_kit::test]
fn first_run_without_history_shows_the_empty_guide(cx: &mut TestAppContext) {
    fresh_dir("fr-empty");
    let (window, view, _) = open_first_run(cx, Recent::default());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).state(), FirstRunState::Empty);
        let guide = window.find("fr-empty");
        assert_eq!(guide.bounds().origin, point(px(24.), px(186.)), "空态引导块的位置");
        assert_eq!(guide.bounds().size, size(px(512.), px(220.)));
        assert!(window.try_find("fr-open").is_some(), "空态也要有「打开文件」");
        assert!(window.try_find("fr-stream").is_some(), "空态也要有「打开串流」");
        assert!(window.try_find("fr-continue").is_none(), "没有历史就没有「继续上次」");
        assert!(window.try_find(("fr-row", 0usize)).is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn first_run_lists_recent_and_resumes_the_last_one(cx: &mut TestAppContext) {
    let dir = fresh_dir("fr-recent");
    let last = write_silent_wav_named(&dir, "last.wav", 1.0);
    let older = write_silent_wav_named(&dir, "older.wav", 1.0);
    let oldest = write_silent_wav_named(&dir, "oldest.wav", 1.0);
    let recent = Recent {
        entries: vec![
            recent_entry(&last, 3600.0, 1234.0),
            recent_entry(&older, 200.0, 0.0),
            // 文件已经被挪走：不该露出来
            recent_entry(&dir.join("gone.mkv"), 100.0, 0.0),
            recent_entry(&oldest, 200.0, 50.0),
        ],
    };
    let (window, view, launched) = open_first_run(cx, recent);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).state(), FirstRunState::Recent);
        assert!(window.try_find("fr-empty").is_none());
        assert_eq!(
            window.find("fr-continue").bounds().origin,
            point(px(24.), px(196.)),
            "「继续上次」紧跟在按钮行下面"
        );
        // 「继续上次」那一条不再出现在列表里；找不到的文件也被滤掉
        assert!(window.try_find(("fr-row", 0usize)).is_some());
        assert!(window.try_find(("fr-row", 1usize)).is_some());
        assert!(window.try_find(("fr-row", 2usize)).is_none());

        window.click("fr-continue", cx);
        // 列表里的行：看到一半的也接着放
        window.click(("fr-row", 1usize), cx);
        // 没看过（进度 0）的从头放
        window.click(("fr-row", 0usize), cx);
    })
    .unwrap();

    let path = |p: &Path| p.to_string_lossy().replace('\\', "/");
    assert_eq!(
        *launched.borrow(),
        vec![
            Launch::Open {
                path: path(&last),
                resume_at: Some(1234.0)
            },
            Launch::Open {
                path: path(&oldest),
                resume_at: Some(50.0)
            },
            Launch::Open {
                path: path(&older),
                resume_at: None
            },
        ]
    );
}

#[gpui_kit::test]
fn first_run_keyboard_picks_the_highlighted_row(cx: &mut TestAppContext) {
    let dir = fresh_dir("fr-keys");
    let a = write_silent_wav_named(&dir, "a.wav", 1.0);
    let b = write_silent_wav_named(&dir, "b.wav", 1.0);
    // 都没看过：没有「继续上次」，两条都在列表里
    let recent = Recent {
        entries: vec![recent_entry(&a, 60.0, 0.0), recent_entry(&b, 60.0, 0.0)],
    };
    let (window, _view, launched) = open_first_run(cx, recent);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("fr-continue").is_none());
        assert_eq!(
            window.find(("fr-row", 0usize)).bounds().origin.y,
            px(196.) + px(22.),
            "没有「继续上次」时列表上移到按钮行下面"
        );
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();

    assert_eq!(
        *launched.borrow(),
        vec![Launch::Open {
            path: b.to_string_lossy().replace('\\', "/"),
            resume_at: None
        }]
    );
}

#[gpui_kit::test]
fn first_run_drop_zone_shows_and_imports(cx: &mut TestAppContext) {
    let dir = fresh_dir("fr-drop");
    let wav = write_silent_wav_named(&dir, "clip.wav", 1.0);
    let (window, view, launched) = open_first_run(cx, Recent::default());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        view.update(cx, |view, _| view.test_set_drag(Some("clip.wav")));
        window.render_frame(cx);
        assert_eq!(
            view.read(cx).state(),
            FirstRunState::Drop("clip.wav".into())
        );
        let zone = window.find("fr-drop");
        assert_eq!(zone.bounds().origin, point(px(24.), px(204.)));
        assert_eq!(zone.bounds().size, size(px(512.), px(212.)));
        assert!(window.try_find("fr-empty").is_none(), "投放区替换掉空态引导");

        let position = point(px(280.), px(300.));
        let paths = ExternalPaths(SmallVec::from_vec(vec![wav.clone()]));
        window.dispatch_event(
            FileDropEvent::Entered { position, paths }.to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        window.dispatch_event(FileDropEvent::Submit { position }.to_platform_input(), cx);
        window.render_frame(cx);
    })
    .unwrap();

    assert_eq!(*launched.borrow(), vec![Launch::Import(vec![wav])]);
}

/// 真实的交接：首屏选中之后，主窗口要先开出来、首屏再关掉——
/// 反过来的话中间有一刻一个窗口都没有，`on_window_closed` 会把整个应用退掉。
#[gpui_kit::test]
fn first_run_hands_over_to_the_player_window(cx: &mut TestAppContext) {
    let dir = fresh_dir("fr-handoff");
    let wav = write_silent_wav_named(&dir, "pick.wav", 1.0);
    persistence::save_recent(&Recent {
        entries: vec![recent_entry(&wav, 1.0, 0.0)],
    });

    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::theme::install(cx, false);
        crate::open_first_run(cx);
    });
    let first = cx.update(|cx| cx.windows());
    assert_eq!(first.len(), 1);
    cx.update_window(first[0], |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("fr-row", 0usize)).is_some(), "先出来的应当是首屏");
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let windows = cx.update(|cx| cx.windows());
    assert_eq!(windows.len(), 1, "首屏应当关掉，只剩主窗口");
    cx.update_window(windows[0], |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("play-pause").is_some(), "剩下的应当是主播放窗口");
        assert!(window.try_find("fr-open").is_none());
    })
    .unwrap();
}
