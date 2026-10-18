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
- GPUI Windows 后端 = DirectComposition + WS_EX_NOREDIRECTIONBITMAP → **不能承载原生子窗口**；视频走 mpv.exe + `--wid` 自建 WS_POPUP 顶层窗口，**无 owner、垫在主窗口正下方**（第七轮改的，详见下面的"第七轮"条目）。
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
- **字体必须用系统自带的那套**：设计稿的 Inter / Noto Sans SC 在 Windows 上不预装（写死会在用户机器上退成宋体）。平台映射在 `theme::ui_font_family()`（Win `Microsoft YaHei UI` / mac `PingFang SC` / 其他 `Noto Sans SC`）与 `mono_font_family()`。`ThemeConfig.font_family` 改一处即全局生效（`root.rs` 把它铺到整棵树）。
- **任务栏图标靠 PE 资源 ID=1**：`gpui-pre-windows::load_icon` 用 `LoadImageW(module, MAKEINTRESOURCE(1), IMAGE_ICON, …)`，没有这条资源就是空句柄（任务栏显示系统占位图），`WindowOptions.icon` 是 x11 only。`crates/app/build.rs` 用 embed-resource 写 `1 ICON` + VERSIONINFO；`canonicalize` 的 `\\?\` 前缀会让 rc 报「找不到文件」，要剥掉并换正斜杠。
- **验收脚本跑隔离配置目录**：`SMP_DATA_DIR` 可整体顶掉 `%APPDATA%\simple-media-player`。曾直接改真实 playlist.json 覆盖了用户正在用的列表。
- **视频 chrome 压在画面上 = 给叠加窗口开洞**（2026-10-18 第五轮）：`SetWindowRgn` + `RGN_DIFF` 把标题带/胶囊/歌单从叠加窗口的区域里减掉，窗口区域同时裁剪渲染与命中测试 → 下面的 GPUI 主窗口露出来。要点：`CreateRoundRectRgn` 收的是椭圆**直径**不是半径；区域只认物理像素且 `SetWindowPos` 不会重算（`set_rect` 后要重新应用）；空 holes = `SetWindowRgn(hwnd, 0, 1)` 恢复整窗；洞外扩 1.5px 免得切掉描边。`render()` 里视频舞台先声明（绝对定位铺满整窗），chrome 排后面才是"在上面"。
- **胶囊尺寸不能写死**：高度是内容撑开的（随字号/行距变），用胶囊内部的 `canvas` 每帧把 bounds 写进 `capsule_rect`，`chrome_holes()` 用它算洞；`set_holes` 按内容去重，别让 100ms tick 反复打系统调用。
- **按住画面拖窗体**：叠加窗 wndproc 收 `WM_LBUTTONDOWN` → `GetWindow(hwnd, GW_OWNER)` → `ReleaseCapture()` + `SendMessageW(owner, WM_NCLBUTTONDOWN, HTCAPTION, 0)`，把这一下转交给系统当标题栏拖动（贴边吸附/最大化还原/跨屏白拿）；叠加窗自己由 tick 追上。
- **拖放不弹提示**：拖进来这个动作本身就是意图，画面换了就是回执。只保留「没有可播放的媒体文件」这类真失败提示。
- **验收取样要用实测客户区换算**：`SetWindowPos` 设的是外框，实测 1898×1189 物理 ÷1.5 = 1265×793 而非设的 1280×800；按目标尺寸换算整体偏几像素，点 28px 图标正好点进间隙（`scripts/verify_floating_chrome.py` 里记了这条）。
- ~~**洞露出来的是不透明画布，不是画面**：GPUI Windows 后端不支持窗口级透明~~ **这条已被第七轮推翻**：逐像素透明是支持的（见"第七轮"）。歌单面板仍保持 Latte 浅底（改深底会让 `List` 的行文字不可读，颜色取主题 token）。
- **`Window::zoom_window()` 在 Windows 上只最大化、不还原**（`gpui-pre-windows 0.3.9` 的 `zoom()` 只有 `ShowWindowAsync(SW_MAXIMIZE)`）→ 自己写 `window_actions::{is_zoomed, set_zoomed}`。**必须用 `ShowWindowAsync`**：同步 `ShowWindow` 会在 GPUI 的事件派发里重入 `WM_SIZE`，窗口变大了但界面还画在旧尺寸、hitbox 全错位（表现为"再点一次最大化/还原永远点不到按钮"）。异步的代价是调用后 `IsZoomed` 仍是旧值 → 状态先乐观更新 + `zoom_pending` 300ms 内不让 tick 反向纠正。
- **双击标题栏不用自己实现**：标题栏拖拽区映射成 `HTCAPTION`，Windows 原生就做最大化/还原，`on_double_click` 根本不会被调用（日志可证）。
- **GPUI 有窗口透明/模糊能力，但对我们的视频场景无用**：`WindowBackgroundAppearance::{Transparent, Blurred, MicaBackdrop}` 在 gpui-pre 0.3.6 就有（`WindowOptions.window_background`，非 Opaque 时清 alpha 0，Windows 的 `Blurred` → `SetWindowCompositionAttribute` acrylic），但模糊的是窗口**背后**的内容，而 mpv 画在**上面**的叠加窗里。要真模糊视频得反转 z 序（放弃 ownership 自己管 z 序），属重构。


## 第七轮（2026-10-18）：视频窗垫在主窗下面，chrome 变真半透明
- **逐像素透明是支持的**：合成 swapchain 是 `PREMULTIPLIED`，`background_appearance != Opaque` 时 `pre_draw` 清 `[0,0,0,0]`，混合是预乘。真正挡路的是 gpui-component 的 `RootPlugin::style` 给每个**窗口表面** refine 了 `.bg(tokens.background)` → 在 `Render::render` 里 `window.root::<gpui_kit::base::Root>()` 后 `root.style().refine(&StyleRefinement::default().bg(transparent_black()))`（`new` 里做无效：Root 还没挂；无头测试没有 Root，要守卫）。
- **被完全遮挡的窗口会停止出帧**：系统只认窗口矩形/区域，不看逐像素 alpha。所以主窗口必须把画面区从窗口区域里减掉：`region = 整窗 − stage + chrome(RGN_OR)`；不减就黑屏（`PrintWindow` 也黑、`--d3d11-flip=no` 无效）。
- 区域：物理像素、坐标**相对窗口矩形**（`ClientToScreen − GetWindowRect` 偏移）；`CreateRoundRectRgn` 收**直径**；去重键不含窗口位置（纯移动不用重设，否则拖动时每帧重绘、画面发抖）。视频窗铺**外框**不铺客户区（否则边框那圈透出桌面）。
- 别给叠加窗加 `WS_EX_TRANSPARENT`：整窗不再被合成（帧在、看不见）。
- 拖动/双击**自己实现**，别转交系统：`WM_NCLBUTTONDOWN/HTCAPTION` 只跟上头一两步（GPUI 的 wndproc 吃掉 `WM_MOUSEMOVE`，系统模态循环拿不到位移），`WM_NCLBUTTONDBLCLK` 会丢 `WS_MAXIMIZE`（窗口撑满工作区但 `IsZoomed` 假）；双击需要窗口类 `CS_DBLCLKS`，走 `window_actions::set_zoomed(main, !is_zoomed)`。
- 标题带在视频模式也要铺 `overlay::glass()`（画面顶到边时曲名读不出来）。
- 验收两层：`scripts/verify_floating_chrome.py`（真机：出帧/区域/对齐/玻璃透色/歌单 PtInRegion/拖动/双击）+ `ui_tests::chrome_region_covers_capsule_and_panel`（区域矩形 vs 实际布局）。
- 代价：没有区域级 backdrop blur（acrylic 只能整窗）、没有 Aero Snap、缩放动画期间可能闪一帧。
- `scripts/shot.py` 已改成走 `SMP_DATA_DIR` 隔离：`--play` 会写 playlist.json，之前直接跑会污染用户真实列表（踩过两次）。

## 设计稿要点（详见 2026-10-17 / 2026-10-18 日志）
- 色板严格取 Catppuccin Latte 官方值；Mauve #8839EF 唯一强调色。**v2 起是两套色板**：应用表面 Latte/Mocha，叠加层（胶囊）深色玻璃，见 `theme::overlay`。
- 布局 v2 = IINA 式沉浸布局：自绘 34px 标题带 + 悬浮玻璃胶囊（内缩 24、圆角 12、padding 16×8）+ 默认收起的 320 悬浮歌单面板；内容占满窗口。**视频模式下画面真的铺满整窗**，标题带/胶囊/歌单挖洞浮在画面上（2026-10-18 第五轮，之前的"画面让位"已废）。
- 尺寸口径要按「占窗口多少」定，不按像素堆：胶囊曾经 107 高（680 窗口的 15.7%）像一块黑板，现在约 90（13%），`BOTTOM_BAND` = 116。
- **皮肤落地在 `crates/app/src/theme.rs`**：设计稿色板 → gpui-component 语义 token（Latte/Mocha 两套），界面只读 token。改动前一直用默认中性灰主题，这是"与设计稿差别很大"的根因（2026-10-18 修复）。
- 图标规范：播放三角占圆直径 48%（B 档），32px 以下重绘而非缩放（占比 50-54%、去描边）；图标为正圆。
- Ardot 可用字体仅 Inter / Noto 系列 / JetBrains Mono。
- **视频浮层（OSC）不跟浅色主题走**：App 表面用 Latte，叠在画面上的工具栏/控制栏用深色玻璃（`#0B0C10` 60% + blur 32 + 白 10% 描边），图标 `#D5D9E2`，已播放进度与音量用 Mocha Mauve `#CBA6F7`，播放键仍是 `#8839EF`。玻璃透明度**必须 ≤65%**，否则在没有 backdrop blur 的预览/导出里会读成实心黑板。
- 主播放窗口默认态 = 画面全窗铺满 + 悬浮控件，**不展示播放列表**；列表是 320×633 的悬浮玻璃面板（x936 y16），靠控制栏右侧列表图标开合。展开态在 macOS 页单独一帧（`20:1`）演示。
- 视频截图素材注意「AI生成」水印：之前被右侧列表挡住，列表一收就暴露。修法是取原图裁掉底部（1536×1024 → 1536×960，正好 1.6 与 1280×800 同比）再覆盖。
