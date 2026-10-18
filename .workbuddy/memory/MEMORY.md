# Simple Media Player (Chroma) — 项目长期备忘

> 详细过程见 `.workbuddy/memory/2026-10-17.md`、`2026-10-18.md`。本文件只放「结论 + 别重复踩的坑」。

## 项目概览
- 双轨：① Ardot 设计稿（Chroma UI，file 737444551354649，Catppuccin Latte + Mauve #8839EF，macOS=Page 1 / Windows=「Chroma · Windows」两页）；② Rust 实现（GPUI Kit）。
- 计划 docs/gpui-kit-plan.md。栈：GPUI Kit v0.7.3（锁 0.7.x）+ gpui-component 样式层；音频 rodio；视频 mpv.exe 子进程 + `--wid` 叠加窗。
- **Stage 0–5 全部完成，v1.0.0 已发布**（M0–M3 四 tag，`scripts/package.ps1` 打包）。

## 工程约定
- 五步流程：plan review → modify → implement → verify completion → next stage；验证**只认真实文件 / 构建产物 / git status**，不信自报告。
- Cargo workspace：crates/app（UI bin）+ crates/player-core（内核 lib，无 UI 依赖）。基线 commit 41c13ef。git 身份仓库级 Kevin Zhang / kevin.zhang@local。用户自装 Rust + VS Build Tools，agent 不动系统环境。

## 视频叠加窗（第七轮）
- GPUI Windows 后端 = DirectComposition + WS_EX_NOREDIRECTIONBITMAP → **不能承载原生子窗口** → 视频走 mpv + `--wid` **自建 WS_POPUP 顶层窗**：无 owner、`keep_below` 垫在主窗下面，主窗逐像素透明才透出画面（`overlay::glass()`；`render()` 里视频舞台先声明、绝对定位铺满整窗，chrome 排后面）。
- **逐像素透明可用**：合成 swapchain `PREMULTIPLIED`，`window_background != Opaque` 时 `pre_draw` 清 `[0,0,0,0]`。真正挡路的是 gpui-component `RootPlugin::style` 给窗口表面 refine 了 `.bg(tokens.background)` → 在 `Render::render` 里 `window.root::<gpui_kit::base::Root>()` 后 `root.style().refine(&StyleRefinement::default().bg(transparent_black()))`（`new` 里做无效；无头测试没有 Root 要守卫）。
- **被完全遮挡的窗口停止出帧**（系统只看窗口矩形/区域，不看 alpha）→ 主窗区域必须减掉画面：`region = 整窗 − stage + chrome(RGN_OR)`。不减就黑屏（`PrintWindow` 也黑、`--d3d11-flip=no` 无效）。**别给叠加窗加 `WS_EX_TRANSPARENT`**（整窗不再被合成，帧在、看不见）。
- 区域细节：物理像素、**相对窗口矩形**（`ClientToScreen − GetWindowRect` 偏移）；`CreateRoundRectRgn` 收**直径**不是半径；去重键不含窗口位置（纯移动不重设，否则拖动每帧重绘、画面发抖）；视频窗铺主窗**外框**不铺客户区（否则边框那圈透桌面）。
- **胶囊尺寸不能写死**（高度随字号/行距变）→ 胶囊内部 `canvas` 每帧把 bounds 写进 `capsule_rect`，`chrome_rects` 用它算区域；chrome 外扩 1.5px 免得切掉 1px 白描边。视频模式标题带也要铺 `overlay::glass()`。
- **圆角要两块窗口一起设**：`DwmSetWindowAttribute(hwnd, 33, &2 /*ROUND*/, 4)`；上两角属主窗标题带、下两角属视频叠加窗，只设一块会在角上留方角。退出视频模式设 1（DONOTROUND）；Win10 静默忽略。别用 `SetWindowRgn` 切圆角（1bit 必锯齿）。验收读回属性值，别用像素。
- 验收：`scripts/verify_floating_chrome.py`（真机）+ `ui_tests::chrome_region_covers_capsule_and_panel`（区域矩形 vs 布局）。
- 代价：无区域级 backdrop blur（acrylic 只能整窗且糊的是窗口**背后**）、无 Aero Snap、缩放动画可能闪一帧。歌单面板保持 Latte 浅底（深底会让 `List` 行文字不可读）。

## 窗口行为（Win32 / GPUI）
- **`SetWindowPos` / `ShowWindow` 不能在 `Context::update` 里同步调**：它们同步派发 `WM_SIZE` → 立刻重布局，而 App 正被可变借用 → `handle.update` 失败、布局停旧尺寸、hitbox 全错位。丢进 `cx.spawn_in(window, async move {…}).detach()`。
- `Window::zoom_window()` 在 Win 只最大化不还原（gpui-pre 0.3.9 只发 `SW_MAXIMIZE`）→ 自写 `window_actions::{is_zoomed,set_zoomed}`，**必须 `ShowWindowAsync`**。异步代价：`IsZoomed` 仍旧值 → 状态乐观更新 + `zoom_pending` 300ms 不反向纠正。
- 拖动/双击**自己实现**：`WM_NCLBUTTONDOWN/HTCAPTION` 只跟上头一两步（GPUI wndproc 吃掉 `WM_MOUSEMOVE`）；标题栏拖拽区映射 HTCAPTION 后 **`on_double_click` 根本不被调用**。**`WM_LBUTTONDBLCLK` 压根不会来**（`WS_EX_NOACTIVATE` + 从未激活，带 `CS_DBLCLKS` 也没用）→ 自己判：`GetDoubleClickTime` + `SM_CXDOUBLECLK(36)/SM_CYDOUBLECLK(37)`，判双击的坐标**必须取消息 lparam** 换算（`ClientToScreen`），`GetCursorPos` 在合成 PostMessage 下读到的是别处的光标（两条日志坐标差半屏，双击永远不成立）；命中后清空，三连击 = 一次双击 + 一次单击。
- 叠加窗 `WS_EX_NOACTIVATE` → 点画面不激活主窗，且"后台第一次点击"被系统当激活吃掉（双击要按两次）→ wndproc 里 `WM_LBUTTONDOWN`/`WM_LBUTTONDBLCLK` 先 `SetForegroundWindow(main)`。拖动也在 wndproc 自己实现（`SetCapture` + **累计位移** `SetWindowPos` 主窗 + 叠加窗粘住）。
- z 序：`ShowWindow` 会把叠加窗提到最前、别的进程也能提主窗 → 每帧 `GetWindow(main, GW_HWNDNEXT) == overlay` 查一次，不对才 `SetWindowPos`。
- **mpv 的 `dwidth/dheight` 先报 0.75 倍假值**（1280×720 先报 960×540），隔一帧才真 → "同一尺寸连报两次"才动窗口（`fit_seen`）。窗口尺寸取 `dwidth/dheight`（显示尺寸）而非 `width/height`（编码尺寸，anamorphic 会拉扁）。视频模式的窗口尺寸**不落盘**。
- `cx.on_app_quit` 只在显式 `App::quit()` 触发，关窗口不走 → 清理靠 Drop（`persistence::log_session` → `%APPDATA%/simple-media-player/session.log`）。进程"自行退出"三档：有 `player_view dropped`+`event loop returned`=优雅；只有 `start`=被硬杀；应用无自杀路径。

## 窗体大小 = 内容大小（第八轮）
- 起播按 `dwidth/dheight` 调窗口：`plan_fit`（纯函数：1:1 → 缩进工作区 → 放大到最小窗口，中心锚点 + 夹回 `rcWork`）→ `fit_to_video`（执行走 `cx.spawn_in`）。
- `fit_done: Option<(w,h)>` 每片只调一次（`start_video` 清空）；最大化/影院/全屏时不动窗口，`fit_done` 留空、退出后下一帧补上。最大化下按住画面也能拖（只在最小化时无视）。

## 全屏播放（第九轮，2026-10-18）
- 双击画面 = **全屏播放**（不再最大化）：叠加窗 wndproc 把 `OverlayEvent::ToggleCinema` 塞进 `static Mutex<Vec<_>>`，tick 取出调 `PlayerView::set_cinema(on)`（唯一入口：夹 `on && video_active`、`video_overlay::set_cinema`、和 `window.is_fullscreen()` 对齐）。四个出口：`Esc`/`F11`、再双击、右上角「最大化」（`toggle_maximize` 里先判 `cinema`）、胶囊 `exit-cinema`。
- 快捷键**必须用 `App::intercept_keystrokes`**，`bind_keys` + `on_action` 收不到：无焦点时 GPUI 派发给 dispatch 树的**根**（gpui-component 的 `Root`，在视图之外），实测 `dispatch_keystroke` 返回 `false`。
- **客户区铺满窗口**（顶栏才铺满宽度）：子类化窗口过程（`SetWindowLongPtrW(GWLP_WNDPROC)`，先调 GPUI 的）拦 `WM_NCCALCSIZE`，左/右/下 = 窗口矩形（顶边不碰）。窗口矩形：`wParam!=0` 取 `rgrc[1]`，`wParam==0` 取 `lparam` 所指 `RECT` 的**初值**（`CallWindowProcW` 会就地改写）；**别用 `GetWindowRect`**（进出全屏时是旧值）。铺满后边框变客户区 → 自己补 `WM_NCHITTEST` 缩放热区（6 逻辑像素，最大化/最小化/无 `WS_THICKFRAME` 时跳过）。
- 连带两条：① GPUI 用创建时记的 `border_offset` 算 `ptMinTrackSize` → 720p 片子被顶成 `720+11`，拦 `WM_GETMINMAXINFO` 减掉；② 单纯改尺寸那路（`wParam==0`）GPUI 不管，同一条 NCCALCSIZE 分支一并处理，免得每次改尺寸靠 tick 自愈补一次（会抖）。
- **GPUI 会丢掉进出全屏那一次的尺寸通知**（`WM_SIZE` 在 `SetWindowPos` 里同步派发，App 正被 update 借用 → resize 回调 `handle.update()` 失败、`log_err` 静默）→ `window.bounds()` 已是新尺寸、布局用的 `viewport_size` 还是旧的：画面铺满、顶栏只画在左上角一小块、右上角按钮不存在、点哪都没反应（Esc 反而正常，键盘不走命中测试）。`refresh()` 治不好；`bounds_changed(cx)` 会跑 `bounds_observers` → 对本视图 `update` → panic。正解 `window_actions::kick_layout()`：`PostMessage` 补一条 `WM_SIZE`（lparam = 真实客户区，幂等），tick 里比 `viewport_size` vs `window.bounds()`，不一致就补、300ms 限流。
- 验收脚本侧：双击**别用模拟光标**（两次按下之间漏一条 `WM_MOUSEMOVE` 就变成拖窗体），直接 PostMessage `DOWN→UP→DBLCLK→UP` 给叠加窗；改尺寸后要 `wait_settled()`（外框对齐 + 顶栏两端/胶囊都回到窗口区域）否则测的是过渡态；顶栏明暗按**三通道之和**比（testsrc2 最左色条近黑，均值差只有 19 卡阈值）。
- 发布验收要看**打包后的 exe**：`SMP_EXE=dist/simple-media-player-1.0.0/simple-media-player.exe python scripts/verify_floating_chrome.py`。release 是 GUI 子系统、无 stderr，但判定只读像素与窗口状态，不受影响；PE `Subsystem` 用读头的方式核（2=GUI）。
- **`package.ps1` 曾"成功却退出码 1"**：cargo 进度写 stderr，脚本头 `$ErrorActionPreference = "Stop"` 把原生命令的 stderr 当 `NativeCommandError` 终止。构建期间临时 `Continue`、只认 `$LASTEXITCODE`，跑完还原。（同源坑：任何 `$ErrorActionPreference="Stop"` 的 .ps1 里调 cargo/git 都要这么包一层。）

## UI / 布局 / 样式（gpui-component 坑）
- `ListItem::confirmed()` 无视觉；默认 `primary` 与正文色重合，不能用它表达"当前项" → 用 `list_active` / `list_active_border`。
- **字体必须系统自带**：设计稿 Inter / Noto Sans SC 在 Win 不预装（写死退宋体）。见 `theme::ui_font_family()`（Win `Microsoft YaHei UI` / mac `PingFang SC` / 其他 `Noto Sans SC`）+ `mono_font_family()`；`ThemeConfig.font_family` 铺整棵树（`root.rs`）。
- 换肤：`ThemeConfig`（`#[serde(default)]`）+ `Theme::update` + `apply_config`。**`ThemeConfigColors` 的 `blue/red/green/cyan/magenta/yellow`(±`_light`) 是私有字段** → crate 外只能 `default()` 后逐字段赋值。别在同一 closure 里既换 mode 又换 config。
- 布局铁律：一组控件居中，左右两栏必须**等宽**（都 `flex_1`），否则 `justify_between` 把中间顶偏。
- **`h_flex()` 默认 `items_center`**：子元素全绝对定位（内容高 0）会在交叉轴塌成 0 → `canvas` bounds 变 `Wx0`（视觉看不出）。视频区要 `.self_stretch()`，回归用例 `stage_fills_space_between_title_band_and_bottom_band`（黑屏有声音的根因）。
- **`height:100%` 在不定高父级退化成 auto** → `.h_full()` 解析不出来（按钮只剩图标高）。写死 `.h(px(..))`，父级再补 `.h_full()`。
- 自绘标题带：`appears_transparent = true` 后**只有左侧拖动区**能设 `WindowControlArea::Drag`（整条都设会让右侧按钮变非客户区）。窗口按钮：`IconName::WindowMinimize/Maximize/Close` 是 24 网格 + 2px 描边，缩到 10px 会糊 → 手写 10×10、1px 描边 SVG；按钮 46 宽 × 标题栏高，关闭键悬停 `#C42B1C`。
- **chrome 前景色跟底色走不跟主题走**：视频模式整块深画布，浅色主题 `muted_foreground` 只有 ≈3:1 → 用 `chrome_fg` / `chrome_dim`。
- 提示分流（`flush_errors`）：`video_active` → 标题带 `status_pill`（4s 退场，tick 比对 `status_pill_shown`）；否则 `push_notification`（视频内容区被 mpv 盖住，toast 等于没画）。通知层位置/宽度在 `theme::install` 改（`margins.top = TITLE_BAND+12`、`width=320`），`apply_config`/`change` 不重置。`push_notification` 要 `&mut Window`（回调里没有）→ "入队 + 100ms tick flush"。
- 上游硬限制：0.7.3 `Slider` 无 `FocusHandle`、无 ARIA、不支持键盘。

## 输入 / 拖放
- **GPUI 侧**：`FileDropEvent::Entered{paths: ExternalPaths}` → `cx.active_drag = AnyDrag{..}`；`Submit` 转合成 MouseUp 才触发鼠标下 hitbox 的 drop listener。根节点 `.on_drop::<ExternalPaths>` = 整窗可投放。`cx.active_drag` 是 `pub(crate)`；构造 `ExternalPaths` 需 dev-dep `smallvec`。测试用 `window.dispatch_event(FileDropEvent::Entered/Submit)`。
- **叠加窗侧**：OLE 找目标 = `WindowFromPoint` + 父链向上，mpv 叠加窗截断；给窗口/子窗加 `WS_EX_TRANSPARENT` **不改变** `WindowFromPoint`（死路）。正解：主窗 `DragAcceptFiles(hwnd,TRUE)` + `WS_EX_ACCEPTFILES` → `WM_DROPFILES` → `DragQueryFileW` → 队列 → tick 消费。
- `drag_over::<T>` 回调只给 `&StyleRefinement`（**没有 Context**）→ 时间戳写 `Cell<Option<Instant>>`，由 tick 判断并 notify。
- 拖进来**不弹提示**（动作即意图，画面换了就是回执），只留「没有可播放的媒体文件」这类真失败。

## 测试
- `TestAppContext` **不自动初始化**：`cx.update` 第一句必须 `gpui_kit::init(cx)`，否则 `no state of type Theme exists`。`test-support` 带入 GPUI 的 `test` 属性 → 测试模块**不能 glob import prelude**（遮蔽 `#[test]`）。
- 改配置目录用 `thread_local!` 覆盖，**不要 `set_var`**（进程级互串）。
- `window.find(id)` 只认**注册进命中测试树**的元素 → 普通 `div().id(x)` 查不到，要 `.test_support()`（`seek-bar`、`stage`）。`find` 缺元素会 **panic**，判不存在用 `try_find`。
- 视频模式无头测试用 `#[cfg(test)] test_set_video_mode` / `test_notify`，不必真起 mpv。
- 模拟点击：`PostMessage(WM_LBUTTONDOWN)` 的 lparam 不总被采信；`SetCursorPos`+`mouse_event` 会被窗口抖动带偏（误点标题带=拖窗口）。稳定做法 = `scripts/shot.py` 的 PostMessage + 按布局算的逻辑坐标。

## 构建 / 发布 / 工具
- 发布版必须 **GUI 子系统**（`windows_subsystem="windows"`），验证读 PE `OptionalHeader.Subsystem`（2=GUI）。GUI 无 stderr，但 `eprintln!` 空句柄不 panic。
- **任务栏图标靠 PE 资源 ID=1**（`gpui-pre-windows::load_icon` 用 `LoadImageW(module, MAKEINTRESOURCE(1), ..)`；`WindowOptions.icon` 是 x11 only）。`crates/app/build.rs` 用 embed-resource 写 `1 ICON` + VERSIONINFO；`canonicalize` 的 `\\?\` 前缀会让 rc 报找不到文件，要剥掉换正斜杠。
- 含中文的 `.ps1` **必须 UTF-8 with BOM**（否则 PS5.1 按 GBK 解 → 乱码 + 语法错）。
- `cargo update -p X --precise` 在咬死依赖图上死循环 → 删 `Cargo.lock` 对应 `[[package]]` 块让 cargo 重解。
- `scripts/shot.py`：必须先 `SetProcessDpiAwareness(2)`（否则按 96dpi 虚拟化只截左上角）；`SetWindowPos` 设外框，按目标客户区用 `AdjustWindowRectExForDpi` 反推；DWM 动画期抢截会留黑边，需轮询等稳定。`--margin N` 才看得到圆角/阴影（窗口会摆到 `(N,N)`）。
- 验收取样用**实测客户区**换算（实测 1898×1189 物理 ÷1.5 = 1265×793，≠ 设的 1280×800）。
- **改完窗口尺寸后的取样必须等 mpv 重配 VO**：叠加窗几何先到位、画面还按旧尺寸画 → 截到"右边露桌面"。做法：`wait_aligned()` 后按 0.15s 重试，最多 3s。`verify-long.mp4` 彩条自带接近纯白的条，`colorful()` 要挑高饱和取样点。
- 脚本一律跑隔离配置目录：`SMP_DATA_DIR` 顶掉 `%APPDATA%\simple-media-player`（曾覆盖用户在用的 playlist.json）。
- 诊断开关：`SMP_MPV_LOG` 存 stderr、`SMP_MPV_TRACE` 存 JSON IPC 双向报文。mpv 对路径重复分隔符不宽容（`C://` → `loading failed`），且 `--msg-level=all=error` 下 stderr 是空的，只有 IPC 报文能看出真相。

## 设计稿要点
- 色板严格取 Catppuccin Latte 官方值；Mauve #8839EF 唯一强调色。两套：应用表面 Latte/Mocha，叠加层深色玻璃（`theme::overlay`）。
- 布局 = IINA 式沉浸：自绘 34px 标题带 + 画面**真铺满整窗**，chrome 半透明玻璃压画面上（主窗区域减法）。
- **OSC 不跟浅色主题走**：深色玻璃 `#0B0C10` 60% + blur 32 + 白 10% 描边，图标 `#D5D9E2`，已播放/音量 Mocha Mauve `#CBA6F7`，播放键 `#8839EF`。玻璃透明度**必须 ≤65%**（无 backdrop blur 时会读成实心黑板）。
- 主播放窗（macOS `2:7` / Win `2:258`）：画面 1280×800 铺满；**控制栏 = 760×92 居中窄胶囊**（x260 y684，底部距 24，圆角 14，padding 20/20/12/12，gap 8）；进度区**时间夹滑块**（`42:17 | 8 | 轨道 596 | 8 | -1:26:27`，等宽数字 11px，剩余更暗）；轨道 4px 高、手柄 10、章节点 2。**轨道必须是固定宽 596，不能 `fill_container`**（否则吞掉 gap、且 height 会被带成 26）。右侧图标组 6 个（字幕/音轨/设置/画中画/播放列表/全屏，gap 20）；**视频信息 ⓘ 在左侧控制组最左、音量图标之前**（左组 = ⓘ + gap16 + 音量组）。右组塞第 7 个会超宽（7×20+6×20=260 > 240 列宽），当初试过 gap 16 硬塞，用户要求挪到左边。
- 播放列表面板 = 320×633 悬浮玻璃（x936 y16），默认 `visible:false`；含标签栏（播放列表/章节 + 关闭）、10 行（行高 48、双行：曲名 + 规格·时长）、底部操作条（循环/随机/排序 · 总计 · 添加/移除/清空）。
- **展开态**：mac `21:162`、Win `21:317`（旧 `20:1` 已删），面板显示 + 控制栏 x=88 居中于剩余画面（88/88 对称）。**Win 面板 y=48**（标题带 40 下沿，y16 会盖住窗口控件组）；mac 面板仍 y16（红绿灯在左上不冲突）。
- 首屏三态：`21:1`（有最近/继续上次，560×440，图标 68、行高 28、继续上次 32）、`21:58`（空/拖放态）、`21:483`（拖入中）。**Win 版同名三态 `21:587`/`21:644`/`21:710`**（y1100，x1480/2120/2760）：无红绿灯，右上角 `最小化+关闭` 两键组（92×32，x462 y6）。
- **Win 设置窗 `21:807`**（960×720 = 原 680 + 40 标题带）：root 改 vertical → [标题带 40, 主体 horizontal]；标题带 layout:none，"设置" x16 y11、控件组 x868 y4。mac 版仍是无标题带的 `2:139`（960×680）。
- **窗口控件图标是浅色 `#E6E9EF`**（为压深色画面设计），复制到浅色首屏/设置窗上会看不见 → 必须逐 vector 改 `#4C4F69`。**`I()` 新建 frame 别传 `fill_container` 尺寸**（实测 width 变 480、height 变成 fill_container，标题带会吃满整窗），建完用 `U()` 给数值。
- 音量条 **96 宽**（`2:84` / Win `2:350`），已填充 70%。**Ardot 的 `strokeDashes` 不生效**（`U()` 回 `updated:{}` 但实际仍是实线），要虚线只能自己拼帧。
- 图标规范：播放三角占圆直径 48%（B 档），32px 以下重绘而非缩放（50–54%、去描边）；图标为正圆。
- Ardot 可用字体仅 Inter / Noto 系列 / JetBrains Mono。
- 视频截图素材的「AI生成」水印：取原图裁底（1536×1024 → 1536×960，与 1280×800 同比）再覆盖。
