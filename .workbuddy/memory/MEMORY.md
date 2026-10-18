# Simple Media Player (Chroma) — 项目长期备忘

> 详细过程见 `.workbuddy/memory/2026-10-17.md`、`2026-10-18.md`。本文件只放「结论 + 别重复踩的坑」。

## 项目概览
- 双轨：① Ardot 设计稿（Chroma UI，file 737444551354649，Catppuccin Latte + Mauve #8839EF，macOS/Windows 两页并存）；② Rust 实现（GPUI Kit）。
- 计划 docs/gpui-kit-plan.md（2026-10-17 v1.0）。栈：GPUI Kit v0.7.3（锁定 0.7.x，Longbridge / Zed GPUI）+ gpui-component 带样式层；音频 rodio；视频 mpv.exe 子进程 + `--wid` 叠加窗口。
- **Stage 0–5 全部完成，v1.0.0 已发布**（M0/M1/M2/M3 四 tag，`scripts/package.ps1` 打包）。后续方向见 Stage 5「已知限制」。

## 工程约定
- 五步流程：plan review → modify → implement → verify completion → next stage；验证**只认真实文件 / 构建产物 / git status**，不信自报告。
- Cargo workspace：crates/app（UI bin）+ crates/player-core（内核 lib，无 UI 依赖）。
- 基线 commit 41c13ef（2026-10-17）。git 身份仓库级：Kevin Zhang / kevin.zhang@local。用户自装 Rust + VS Build Tools，agent 不动系统环境。

## 平台与窗口（勿重复验证）
- GPUI Windows 后端 = DirectComposition + WS_EX_NOREDIRECTIONBITMAP → **不能承载原生子窗口** → 视频走 mpv + `--wid` 自建 WS_POPUP 顶层窗，**无 owner、垫在主窗下面**（第七轮）。
- **逐像素透明是支持的**：合成 swapchain `PREMULTIPLIED`，`window_background != Opaque` 时 `pre_draw` 清 `[0,0,0,0]`。挡路的是 gpui-component `RootPlugin::style` 给窗口表面 refine 了 `.bg(tokens.background)` → 在 `Render::render` 里 `window.root::<gpui_kit::base::Root>()` 后 `root.style().refine(&StyleRefinement::default().bg(transparent_black()))`（`new` 里做无效；无头测试没有 Root 要守卫）。
- **被完全遮挡的窗口停止出帧**（系统只看窗口矩形/区域，不看 alpha）→ 主窗必须把画面区从区域里减掉：`region = 整窗 − stage + chrome(RGN_OR)`。不减就黑屏（`PrintWindow` 也黑、`--d3d11-flip=no` 无效）。
- 区域细节：物理像素、**相对窗口矩形**（`ClientToScreen − GetWindowRect` 偏移）；`CreateRoundRectRgn` 收**直径**不是半径；去重键不含窗口位置（纯移动不重设，否则拖动每帧重绘、画面发抖）；视频窗铺**外框**不铺客户区。
- 别给叠加窗加 `WS_EX_TRANSPARENT`：整窗不再被合成（帧在、看不见）。
- `Window::zoom_window()` 在 Win 只最大化不还原（gpui-pre 0.3.9 只发 `SW_MAXIMIZE`）→ 自写 `window_actions::{is_zoomed,set_zoomed}`，**必须 `ShowWindowAsync`**（同步 `ShowWindow` 在事件派发里重入 `WM_SIZE` → 界面仍画旧尺寸、hitbox 全错位）。异步代价：`IsZoomed` 仍旧值 → 状态乐观更新 + `zoom_pending` 300ms 不反向纠正。
- 拖动/双击**自己实现**：`WM_NCLBUTTONDOWN/HTCAPTION` 只跟上头一两步（GPUI wndproc 吃掉 `WM_MOUSEMOVE`），`WM_NCLBUTTONDBLCLK` 会丢 `WS_MAXIMIZE`；双击需窗口类 `CS_DBLCLKS`。标题栏拖拽区映射 HTCAPTION 后，**`on_double_click` 根本不被调用**（日志可证）。
- 叠加窗 `WS_EX_NOACTIVATE` → 点画面**不激活/不前置**主窗，且"后台时第一次点击"会被系统当激活吃掉（表现为双击要按两次）→ wndproc 里 `WM_LBUTTONDOWN` / `WM_LBUTTONDBLCLK` 先 `SetForegroundWindow(main)` 再干活。
- `cx.on_app_quit` 只在显式 `App::quit()` 时触发，关窗口不走 → 清理靠 Drop（退出留痕 `persistence::log_session` → `%APPDATA%/simple-media-player/session.log`）。
- 进程"自行退出"分三档：日志有 `player_view dropped`+`event loop returned`=优雅；只有 `start`=被硬杀（宿主清进程树）；应用本身无自杀路径。
- 叠加窗拖动：wndproc 自己实现（`SetCapture` + 按**累计位移** `SetWindowPos` 主窗 + 叠加窗同步粘住），不要转交系统（见上一条）。
- `WindowBackgroundAppearance::{Transparent,Blurred}` 可用且**逐像素透明真的生效**；但 acrylic 模糊的是窗口**背后**、且只能整窗 → 做不到"只糊 chrome 底下那块"。

## UI / 布局 / 样式（gpui-component 坑）
- `ListItem::confirmed()` 无视觉（只有 check_icon 画勾）；默认主题 `primary` 与正文色重合，不能用它表达"当前项"→ 用 `list_active` / `list_active_border`。
- **字体必须用系统自带**：设计稿 Inter / Noto Sans SC 在 Win 不预装（写死退宋体）。映射在 `theme::ui_font_family()`（Win `Microsoft YaHei UI` / mac `PingFang SC` / 其他 `Noto Sans SC`）+ `mono_font_family()`；`ThemeConfig.font_family` 铺到整棵树（`root.rs`）。
- 换肤：`ThemeConfig`（`#[serde(default)]`）配 `Theme::update` + `apply_config`。**`ThemeConfigColors` 的 `blue/red/green/cyan/magenta/yellow`(±`_light`) 是私有字段** → crate 外只能 `default()` 后逐字段赋值。别在同一 closure 里既换 mode 又换 config（`installed_by_edit` 跳过 `apply_config`）。
- 布局铁律：要让一组控件居中，左右两栏必须**等宽**（都 `flex_1` = `1 1 0%`）；否则 `justify_between` 把"中间"顶偏（走带偏左就是这么来的）。
- **`h_flex()` 默认 `items_center`**：子元素全是绝对定位（内容高 0）会在交叉轴塌成 0 → `canvas` bounds 变 `Wx0`（视觉看不出，背景由根节点铺满）。视频区要 `.self_stretch()`，回归用例 `stage_fills_space_between_title_band_and_bottom_band` 守着（黑屏有声音的根因）。
- **`height:100%` 在不定高父级退化成 auto** → 子元素 `.h_full()` 解析不出来（按钮只剩图标高）。写死 `.h(px(..))`，父级再补 `.h_full()`。
- 自绘标题带：`appears_transparent = true` 后**只有左侧拖动区**能设 `WindowControlArea::Drag`（整条都设会让右侧按钮变非客户区）。
- 窗口按钮：`IconName::WindowMinimize/Maximize/Close` 是 24 网格 + 2px 描边，缩到 10px 会糊 → 手写 10×10、1px 描边 SVG（`Icon::default().data(SVG_BYTES)`）。按钮 46 宽 × 标题栏高，关闭键悬停 `#C42B1C`。
- **chrome 前景色跟底色走不跟主题走**：视频模式整块深色画布，浅色主题的 `muted_foreground` 只有 ≈3:1 → 用 `chrome_fg` / `chrome_dim`。
- 提示分流（`flush_errors`）：`video_active` → 标题带 `status_pill`（4s 退场，由 tick 比对 `status_pill_shown`）；否则 `push_notification`（视频模式内容区被 mpv 盖住，toast 等于没画）。通知层位置/宽度在 `theme::install` 改（`margins.top = TITLE_BAND+12`、`width=320`），`apply_config`/`change` 不重置。
- `push_notification` 要 `&mut Window`（回调里没有）→ "入队 + 100ms tick flush"，延迟 ≤1 帧。
- 上游硬限制：0.7.3 `Slider` 无 `FocusHandle`、无 ARIA、不支持键盘。

## 输入 / 拖放
- **GPUI 侧**：`FileDropEvent::Entered{paths: ExternalPaths}` → `cx.active_drag = AnyDrag{..}`；`Submit` 转合成 MouseUp 才触发鼠标下 hitbox 的 drop listener。根节点 `.on_drop::<ExternalPaths>` = 整窗可投放。`cx.active_drag` 是 `pub(crate)`；构造 `ExternalPaths` 需 dev-dep `smallvec = "1"`。UI 测试用 `window.dispatch_event(FileDropEvent::Entered/Submit)`（`dropping_files_adds_them_and_starts_playback`）。
- **叠加窗口侧**：OLE 找目标 = `WindowFromPoint` + 父链向上，mpv 叠加窗截断；给窗口/mpv 子窗加 `WS_EX_TRANSPARENT` **不改变** `WindowFromPoint`（死路）。正解：主窗 `DragAcceptFiles(hwnd,TRUE)`（带 `WS_EX_ACCEPTFILES`）→ wndproc `WM_DROPFILES` → `DragQueryFileW` → 队列 → tick 消费。
- `drag_over::<T>` 样式回调只给 `&StyleRefinement`（**没有 Context**，改不了字段、notify 不了）→ 时间戳写进 `Cell<Option<Instant>>`，由 tick 判断并 notify。
- 拖进来**不弹提示**（动作即意图，画面换了就是回执），只留「没有可播放的媒体文件」这类真失败。

## 测试
- `TestAppContext` **不自动初始化**：`cx.update` 第一句必须 `gpui_kit::init(cx)`，否则 `no state of type Theme exists`。`test-support` feature 带入 GPUI 的 `test` 属性 → 测试模块**不能 glob import prelude**（会遮蔽 `#[test]`）。
- 改配置目录用 `thread_local!` 覆盖，**不要 `set_var`**（进程级，多测试互串）。
- `window.find(id)` 只认**注册进命中测试树**的元素 → 普通 `div().id(x)` 查不到，要 `.test_support()`（`seek-bar`、`stage`）。`find` 缺元素会 **panic**，判"不存在"用 `try_find`（Option）；`bounds()` 直接返回 `Bounds`。
- 视频模式无头测试用 `#[cfg(test)] test_set_video_mode` / `test_notify`，不必真起 mpv。
- 模拟点击：`PostMessage(WM_LBUTTONDOWN)` 的 lparam 不总被采信（命中测试用真实光标）；`SetCursorPos`+`mouse_event` 被窗口抖动带偏（误点自绘标题带 = 拖动窗口）。稳定做法 = `scripts/shot.py` 的 PostMessage + 按布局算的逻辑坐标。

## 构建 / 发布 / 工具
- 发布版必须是 **GUI 子系统**（`windows_subsystem="windows"`），否则双击先弹 cmd。验证读 PE `OptionalHeader.Subsystem`（2=GUI）。GUI 无 stderr，但 `eprintln!` 空句柄不 panic。
- **任务栏图标靠 PE 资源 ID=1**：`gpui-pre-windows::load_icon` 用 `LoadImageW(module, MAKEINTRESOURCE(1), ..)`；`WindowOptions.icon` 是 x11 only。`crates/app/build.rs` 用 embed-resource 写 `1 ICON` + VERSIONINFO；`canonicalize` 的 `\\?\` 前缀会让 rc 报「找不到文件」，要剥掉换正斜杠。
- 含中文的 `.ps1` **必须 UTF-8 with BOM**（否则 PS5.1 按 GBK 解 → 乱码 + 语法错）。
- `cargo update -p X --precise` 在咬死依赖图上死循环 → 直接删 `Cargo.lock` 对应 `[[package]]` 块让 cargo 重解（`test-support` → `gpui-fast-macos` 要 cocoa 0.26.0 就是一例）。
- `scripts/shot.py`：必须先 `SetProcessDpiAwareness(2)`（否则按 96dpi 虚拟化 → 只截到左上角，"歌单和控制栏消失"）；`SetWindowPos` 设外框（1560×1020 外框 ≈ 1538×964 客户区），按目标客户区用 `AdjustWindowRectExForDpi` 反推；DWM 动画期间抢截会留黑边，需轮询等位置稳定。
- 验收取样要用**实测客户区**换算：实测 1898×1189 物理 ÷1.5 = 1265×793 而非设的 1280×800，按设定尺寸换算会点进间隙。
- 脚本一律跑隔离配置目录：`SMP_DATA_DIR` 顶掉 `%APPDATA%\simple-media-player`（曾直接改真实 playlist.json 覆盖用户在用的列表，踩过两次）。
- 诊断开关（保留）：`SMP_MPV_LOG` 存 mpv stderr、`SMP_MPV_TRACE` 存 JSON IPC 双向报文。mpv 对路径重复分隔符不宽容（`C://` → `end-file reason=error file_error="loading failed"`），且 `--msg-level=all=error` 下 stderr 是空的，只有 IPC 报文能看出真相。

## 视频叠加窗架构（第七轮，2026-10-18）
- 叠加窗**垫在主窗下面**（无 owner、`keep_below`），主窗逐像素透明 → `overlay::glass()` 才真的透出画面（`render()` 里视频舞台先声明、绝对定位铺满整窗，chrome 排后面才是"在上面"）。
- 透明能生效的前提：`WindowOptions.window_background = Transparent` + 覆盖 gpui-component `RootPlugin::style` 铺的窗口表面底色（`render` 里 `window.root::<gpui_kit::base::Root>()` 后 `root.style().refine(&StyleRefinement::default().bg(transparent_black()))`；`new` 里无效，无头测试没 Root 要守卫）。
- **被完全遮挡的窗口会停止出帧**（系统只看窗口矩形/区域，不看逐像素 alpha）→ 主窗区域 = 整窗 − stage + chrome（`RGN_OR` 把标题带/胶囊/歌单加回去）。区域坐标相对**窗口矩形**、物理像素；`CreateRoundRectRgn` 收直径；去重键不含窗口位置。
- 视频窗铺主窗**外框**（`GetWindowRect`），铺客户区会让边框那圈透出桌面。
- **胶囊尺寸不能写死**：高度随字号/行距变 → 胶囊内部 `canvas` 每帧把 bounds 写进 `capsule_rect`，`chrome_rects` 用它算区域；chrome 外扩 1.5px 免得切掉 1px 白描边。
- 视频模式标题带也要铺 `overlay::glass()`（画面顶到边时曲名读不出来）。
- 验收两层：`scripts/verify_floating_chrome.py`（真机：出帧/区域/对齐/玻璃透色/歌单 PtInRegion/拖动/双击）+ `ui_tests::chrome_region_covers_capsule_and_panel`（区域矩形 vs 布局）。
- 代价：无区域级 backdrop blur（acrylic 只能整窗）、无 Aero Snap、缩放动画可能闪一帧。歌单面板保持 Latte 浅底（改深底会让 `List` 行文字不可读）。

## 设计稿要点
- 色板严格取 Catppuccin Latte 官方值；Mauve #8839EF 唯一强调色。**v2 起两套色板**：应用表面 Latte/Mocha，叠加层深色玻璃（`theme::overlay`）。
- 布局 v2 = IINA 式沉浸布局：自绘 34px 标题带 + 悬浮玻璃胶囊（内缩 24、圆角 12、padding 16×8）+ 默认收起的 320 悬浮歌单面板；视频模式画面**真铺满整窗**，chrome 用半透明玻璃压在画面上（主窗区域减法，见上一节）。尺寸按「占窗口多少」定：胶囊 ≈90（13%），`BOTTOM_BAND` = 116。
- **皮肤落地在 `crates/app/src/theme.rs`**：设计稿色板 → gpui-component 语义 token（Latte/Mocha），界面只读 token。
- 图标规范：播放三角占圆直径 48%（B 档），32px 以下重绘而非缩放（占比 50–54%、去描边）；图标为正圆。
- Ardot 可用字体仅 Inter / Noto 系列 / JetBrains Mono。
- **OSC 不跟浅色主题走**：应用表面 Latte，压在画面上的工具栏用深色玻璃（`#0B0C10` 60% + blur 32 + 白 10% 描边），图标 `#D5D9E2`，已播放进度与音量用 Mocha Mauve `#CBA6F7`，播放键 `#8839EF`。玻璃透明度**必须 ≤65%**（无 backdrop blur 时会读成实心黑板）。
- 主窗口默认态 = 画面铺满 + 悬浮控件，不展示列表；列表是 320×633 悬浮玻璃面板（x936 y16），控制栏右侧图标开合。展开态在 macOS 页单独一帧（`20:1`）。
- 视频截图素材注意「AI生成」水印：取原图裁底（1536×1024 → 1536×960，与 1280×800 同比）再覆盖。
