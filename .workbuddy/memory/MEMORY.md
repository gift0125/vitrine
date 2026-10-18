# Simple Media Player (Chroma) — 项目长期备忘

## 项目概览
- 双轨：① Ardot 设计稿（Chroma 播放器 UI，file 737444551354649，Catppuccin Latte 浅色 + Mauve 强调色，macOS/Windows 两版页面并存）；② Rust 实现（GPUI Kit）。
- 实现计划：docs/gpui-kit-plan.md（2026-10-17 v1.0）。框架 GPUI Kit v0.7.3（Longbridge，基于 Zed GPUI）；UI 用 gpui-component 带样式层；音频 rodio；视频 mpv.exe 子进程 + `--wid` 叠加窗口。**Stage 0–5 全部完成，v1.0.0 已发布**（M0/M1/M2/M3 四个 tag，`scripts/package.ps1` 打包）。后续方向见 Stage 5「已知限制」，详见 2026-10-18 日志第 10 节。

## 工程约定
- 五步流程：plan review → modify → implement → verify completion → next stage；验证以读实际文件/构建产物/git status 为准。
- Cargo workspace：crates/app（UI bin）+ crates/player-core（播放内核 lib，无 UI 依赖）。gpui-kit 版本锁定 0.7.x。
- 基线 commit 41c13ef（2026-10-17）。git 身份本仓库级配置：Kevin Zhang / kevin.zhang@local。
- 工具链：用户自行安装 Rust + VS Build Tools（2026-10-17 确认），agent 不动系统环境。

## GPUI 平台事实（踩过的坑，勿重复验证）
- GPUI Windows 后端 = DirectComposition + WS_EX_NOREDIRECTIONBITMAP → **不能承载原生子窗口**；视频走 mpv.exe + `--wid` 自建 WS_POPUP 顶层窗口（owner=主窗口）。
- `ListItem::confirmed()` 不产生任何视觉（只有 check_icon 才画对勾）；默认主题 `primary` 与正文色重合（light neutral-900 vs neutral-950），不能用 primary 表达"当前项"。列表高亮用 `list_active` / `list_active_border` token。
- `cx.on_app_quit` **只在显式 App::quit() 时触发，窗口关闭不走它**；资源清理必须依赖 Drop。退出留痕见 `persistence::log_session` → `%APPDATA%/simple-media-player/session.log`。
- 观测到"进程自行退出"时先分三档：日志有 `player_view dropped`+`event loop returned`=优雅退出；只有 `start`=被硬杀（宿主清理进程树）；在宿主任务内启动的长驻进程可稳定存活 16min+，应用本身无自杀路径。
- `window.push_notification` 必须要 `&mut Window`，而事件回调里通常没有 → 用"入队 + 在 100ms tick 里 flush"的模式；延迟 ≤1 帧，用户无感。
- UI 集成测试（`gpui-kit` feature `test-support`）：`TestAppContext` **不会自动初始化**，`cx.update` 第一句必须 `gpui_kit::init(cx)`，否则 `no state of type Theme exists`。且该 feature 会带入 GPUI 的 `test` 属性 → 测试模块**不能 glob import prelude**（会遮蔽 `#[test]`）。
- 测试隔离：改配置目录用 `thread_local!` 覆盖，**不要用 `set_var`**（进程级，多测试并行互串）。
- 上游硬限制：gpui-component 0.7.3 的 `Slider` 无 `FocusHandle`、无 ARIA role/value、不支持键盘，组件层不可修。
- `cargo update -p X --precise` 在互相咬死的依赖图上会死循环 → 直接删 `Cargo.lock` 里对应 `[[package]]` 块让 cargo 重解（`test-support` → `gpui-fast-macos` 要求 cocoa 0.26.0 就是一例）。
- 含中文的 `.ps1` **必须存成 UTF-8 with BOM**，否则 PowerShell 5.1 按 GBK 解 → 乱码 + 语法报错。
- 诊断开关（保留）：`SMP_MPV_LOG` 存 mpv stderr、`SMP_MPV_TRACE` 存 JSON IPC 双向报文。mpv 对路径重复分隔符不宽容（`C://` → `end-file reason=error file_error="loading failed"`），且该失败在 `--msg-level=all=error` 下 stderr 是空的，只有 IPC 报文能看出真相。
- 换肤：`ThemeConfig`（`#[serde(default)]` + Default）配 `Theme::update` + `apply_config`。**`ThemeConfigColors` 的 `blue/red/green/cyan/magenta/yellow`(±`_light`) 是私有字段**，crate 外不能用结构体字面量 + `..Default::default()`，只能 `default()` 后逐字段赋值。别在同一个 closure 里既换 mode 又换 config（`installed_by_edit` 会跳过 `apply_config`）。
- 截图工具（`scripts/shot.py`）**必须先 `SetProcessDpiAwareness(2)`**：否则 Python 按 96dpi 虚拟化，`GetClientRect` 尺寸缩水、`ImageGrab` 只截到窗口左上角（表现为"界面只剩封面，歌单和控制栏消失"）。另两点：`SetWindowPos` 设的是**外框**尺寸（1560×1020 外框只给 1538×964 客户区），要按目标客户区用 `AdjustWindowRectExForDpi` 反推外框；DWM 动画期间抢截会在图左边留一条黑边，需轮询等外框位置稳定。
- 发布版必须保持 **GUI 子系统**（`#![cfg_attr(all(not(debug_assertions), target_os="windows"), windows_subsystem="windows")]`），否则双击先弹 cmd。验证读 PE 头 `OptionalHeader.Subsystem`（2=GUI / 3=console），不要靠肉眼。GUI 子系统无 stderr，但 `eprintln!` 在空句柄下不会 panic（std 当空设备），所以 `report_error` 只额外补一条 `session.log` 即可。
- 布局铁律：要让某一组控件在窗口正中，左右两栏必须**等宽**（都 `flex_1`，`flex: 1 1 0%`）。用 `justify_between` 把内容分左右两组时，两组宽度不等就会把"中间"顶偏——走带控制偏左就是这么来的（2026-10-18 修）。
- **`h_flex()` 默认 `items_center`**：容器里的"内容全是绝对定位"的子元素（内容高度 0）会在交叉轴上塌成 0 高度。视觉看不出来（背景色由根节点铺满），但 `canvas` 拿到的 bounds 是 `Wx0`。视频区要用 `.self_stretch()` 撑满，并有回归用例 `stage_fills_space_between_title_band_and_bottom_band` 守着（黑屏有声音的根因就是这个，2026-10-18）。
- 自绘标题带：`TitlebarOptions.appears_transparent = true` 后，**只有左侧拖动区域**能设 `WindowControlArea::Drag`；整条都设会把右侧最小化/关闭按钮变成非客户区，点击到不了 GPUI。双击最大化要 `on_double_click` + `.id()` + `InteractiveElementExt`。
- 测试里 `window.find(id)` 只认得**注册进命中测试树**的元素：普通 `div().id(x)` 查不到，要加 `.test_support()`（如 `seek-bar`、`stage`）。
- 模拟点击：`PostMessage(WM_LBUTTONDOWN)` 的 lparam 不总被采信，命中测试用**真实光标**；`SetCursorPos`+`mouse_event` 又会被窗口位置抖动带偏（误点在自绘标题带上会变成"拖动窗口"）。稳定做法是 `scripts/shot.py` 的 PostMessage 路径 + 按布局算出的逻辑坐标。
- **文件拖放（GPUI 侧）**：系统投放 → `FileDropEvent::Entered{paths: ExternalPaths}` → `cx.active_drag = AnyDrag{value: Arc<ExternalPaths>}`；`Submit` 转成合成 MouseUp，鼠标下 hitbox 的 drop listener 才被调用。根节点 `.on_drop::<ExternalPaths>(...)` = 整窗可投放。`cx.active_drag` 是 `pub(crate)`（读不到）；`ExternalPaths` 要自己构造必须加 `smallvec = "1"` dev-dependency。UI 测试可用 `window.dispatch_event(FileDropEvent::Entered/Submit)` 直接驱动整条链路（`dropping_files_adds_them_and_starts_playback`）。
- **拖放（叠加窗口侧）**：OLE 找投放目标 = `WindowFromPoint` + 沿**父链**向上，mpv 叠加窗会截断；实测给窗口/mpv 子窗口加 `WS_EX_TRANSPARENT` **不会**改变 `WindowFromPoint`（别走这条路）。正解是把路径从同一进程的 GPUI 主窗口取回来：`DragAcceptFiles(hwnd, TRUE)`（exstyle 带 `WS_EX_ACCEPTFILES`）→ wndproc 处理 `WM_DROPFILES` → `DragQueryFileW` → 队列 → tick 消费。
- `drag_over::<T>` 的样式回调只给 `&StyleRefinement`：**拿不到 `Context`**，改不了字段也 `notify` 不了。要视觉反馈就把时间戳写进 `Cell<Option<Instant>>`，由 100ms tick 判断并 `notify`（`on_file_drop_exit` 只给 `&mut App`、只对 `Exited` 且仍 hover 时触发，不够稳）。
- 标题栏窗口按钮：`IconName::WindowMinimize/Maximize/Close` 是 24 网格 + 2px 描边，缩到 10px 会糊；要手写 10×10、1px 描边的 SVG 用 `Icon::default().data(SVG_BYTES)`。按钮本体按 Windows 规范 **46 宽 × 标题栏高**，关闭键悬停 `#C42B1C`。
- **chrome 前景色跟底色走，不跟主题走**：视频模式整块 chrome 是深色画布，此时用浅色主题的 `muted_foreground`/`secondary_foreground` 只有 ≈3:1 对比度（`chrome_fg`/`chrome_dim` 就是干这个的）。
- **提示按底色分流**（`flush_errors`）：`video_active` → 标题带胶囊 `status_pill`（4s 自动退场，出现/消失由 tick 比对 `status_pill_shown` 触发重绘）；否则 `window.push_notification`。原因：视频模式内容区整块被 mpv 叠加窗盖着，in-app toast 等于没画。通知层位置/宽度在 `theme::install` 里改（`notification.margins.top = TITLE_BAND + 12`、`width = 320`），`apply_config`/`change` 不会重置它。
- **`height: 100%` 在不定高父级上退化成 auto**：`h_flex()` 高度由内容撑开时，子元素 `.h_full()` 解析不出来（实测按钮只剩图标 10px 高，悬停底色成一条横线）。要写死 `.h(px(...))`，父级再补 `.h_full()`。
- 测试侧：`window.find(id)` 缺元素会 **panic**，判"不存在"用 `try_find`（Option）；`bounds()` 直接返回 `Bounds`（不是 Option）。无头测试要覆盖视频模式用 `#[cfg(test)]` 的 `test_set_video_mode`/`test_notify`，不必真起 mpv。

## 设计稿要点（详见 2026-10-17 / 2026-10-18 日志）
- 色板严格取 Catppuccin Latte 官方值；Mauve #8839EF 唯一强调色。**v2 起是两套色板**：应用表面 Latte/Mocha，叠加层（胶囊）深色玻璃，见 `theme::overlay`。
- 布局 v2 = IINA 式沉浸布局：自绘 34px 标题带 + 悬浮玻璃胶囊（内缩 24、圆角 12、padding 16×8）+ 默认收起的 320 悬浮歌单面板；内容占满窗口。
- 尺寸口径要按「占窗口多少」定，不按像素堆：胶囊曾经 107 高（680 窗口的 15.7%）像一块黑板，现在约 90（13%），`BOTTOM_BAND` = 116。
- **皮肤落地在 `crates/app/src/theme.rs`**：设计稿色板 → gpui-component 语义 token（Latte/Mocha 两套），界面只读 token。改动前一直用默认中性灰主题，这是"与设计稿差别很大"的根因（2026-10-18 修复）。
- 图标规范：播放三角占圆直径 48%（B 档），32px 以下重绘而非缩放（占比 50-54%、去描边）；图标为正圆。
- Ardot 可用字体仅 Inter / Noto 系列 / JetBrains Mono。
- **视频浮层（OSC）不跟浅色主题走**：App 表面用 Latte，叠在画面上的工具栏/控制栏用深色玻璃（`#0B0C10` 60% + blur 32 + 白 10% 描边），图标 `#D5D9E2`，已播放进度与音量用 Mocha Mauve `#CBA6F7`，播放键仍是 `#8839EF`。玻璃透明度**必须 ≤65%**，否则在没有 backdrop blur 的预览/导出里会读成实心黑板。
- 主播放窗口默认态 = 画面全窗铺满 + 悬浮控件，**不展示播放列表**；列表是 320×633 的悬浮玻璃面板（x936 y16），靠控制栏右侧列表图标开合。展开态在 macOS 页单独一帧（`20:1`）演示。
- 视频截图素材注意「AI生成」水印：之前被右侧列表挡住，列表一收就暴露。修法是取原图裁掉底部（1536×1024 → 1536×960，正好 1.6 与 1280×800 同比）再覆盖。
