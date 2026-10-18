# Simple Media Player (Chroma) — 长期备忘

> 完整过程与数字见 `.workbuddy/memory/2026-10-17.md` / `2026-10-18.md`。本文件只留结论、常数、易踩的坑。

## 概览
- 设计稿在 Ardot（file 737444551354649，Catppuccin Latte + Mauve #8839EF；macOS=Page 1，Windows=「Chroma · Windows」）。栈：GPUI Kit 0.7.3 + gpui-component、rodio、视频 = **进程内 libmpv**（`libmpv-2.dll`）+ `--wid` 叠加窗。计划 `docs/gpui-kit-plan.md`；Stage 0–5 完成，v1.0.0 已发布（`scripts/package.ps1 -WithLibmpv`）。
- 五步流程：plan review → modify → implement → verify completion → next stage。验证**只认真实文件 / 产物 / git status**，不信自报告。
- workspace = crates/app（UI bin）+ crates/player-core（内核，无 UI 依赖）。基线 commit 41c13ef；git 身份仓库级 Kevin Zhang / kevin.zhang@local；用户自装 Rust + VS Build Tools，agent 不动系统环境。

## 视频叠加窗（主架构债）
- GPUI Win 后端 = DirectComposition + WS_EX_NOREDIRECTIONBITMAP → **不能承载原生子窗口** → 视频走 `--wid` **自建 WS_POPUP 顶层窗**：无 owner、`keep_below` 垫主窗下，主窗逐像素透明才透画面（`render()` 里视频舞台先声明铺满整窗，chrome 排后面）。
- **逐像素透明可用**：swapchain `PREMULTIPLIED`，`window_background != Opaque` 时 `pre_draw` 清 `[0,0,0,0]`。挡路的是 gpui-component `RootPlugin::style` 给窗口表面 refine 了 `.bg(tokens.background)` → `Render::render` 里 `window.root::<Root>()` 后 `root.style().refine(&StyleRefinement::default().bg(transparent_black()))`（`new` 里做无效；无头测试无 Root 要守卫）。
- **被完全遮挡的窗口停止出帧**（系统只看窗口矩形不看 alpha）→ 主窗区域必须减掉画面：`region = 整窗 − stage + chrome(RGN_OR)`。不减就黑屏（`PrintWindow` 也黑，`--d3d11-flip=no` 无效）。**别给叠加窗加 `WS_EX_TRANSPARENT`**（整窗不再合成，帧在、看不见）。
- 区域细节：物理像素、**相对窗口矩形**；`CreateRoundRectRgn` 收**直径**不是半径；去重键不含窗口位置（纯移动不重设，否则拖动每帧重绘、画面抖）；视频窗铺主窗**外框**不是客户区（否则边框那圈透桌面）。
- **胶囊尺寸不能写死**（高度随字号/行距变）→ 胶囊内部 `canvas` 每帧把 bounds 写进 `capsule_rect`，`chrome_rects` 用它算区域；chrome 外扩 1.5px 免得切 1px 白描边。视频模式标题带也要铺 `overlay::glass()`。
- **圆角要两块窗口一起设**：`DwmSetWindowAttribute(hwnd, 33, &2, 4)`；上两角属主窗标题带、下两角属视频叠加窗，只设一块留方角。退出视频模式设 1；Win10 静默忽略。别用 `SetWindowRgn`（1bit 必锯齿）；验收读回属性值别用像素。
- 代价：无区域级 backdrop blur（acrylic 只能整窗且糊窗口**背后**）、无 Aero Snap、缩放可能闪一帧。歌单面板保持 Latte 浅底。验收 = `verify_floating_chrome.py` + `ui_tests::chrome_region_covers_capsule_and_panel`。

## Win32 / GPUI 行为
- **`SetWindowPos` / `ShowWindow` 不能在 `Context::update` 里同步调**：同步派发 `WM_SIZE` → 立刻重布局，而 App 正被可变借用 → `handle.update` 失败、布局停旧尺寸、hitbox 全错位。丢进 `cx.spawn_in(window, async{…}).detach()`。
- `zoom_window()` 在 Win 只最大化不还原（0.3.9 只发 `SW_MAXIMIZE`）→ 自写 `window_actions::{is_zoomed,set_zoomed}`，**必须 `ShowWindowAsync`**；`IsZoomed` 仍旧值 → 乐观更新 + `zoom_pending` 300ms 不反向纠正。
- 拖动/双击**自己实现**：`WM_NCLBUTTONDOWN/HTCAPTION` 只跟上头一两步（GPUI wndproc 吃掉 `WM_MOUSEMOVE`）；映射 HTCAPTION 后 `on_double_click` 不被调用。**`WM_LBUTTONDBLCLK` 压根不会来**（`WS_EX_NOACTIVATE` + 从未激活，`CS_DBLCLKS` 无用）→ 自己判：`GetDoubleClickTime` + `SM_CXDOUBLECLK(36)/SM_CYDOUBLECLK(37)`；坐标**必须用消息 lparam** 换算（`ClientToScreen`），`GetCursorPos` 在合成 PostMessage 下读别处光标（差半屏，双击永不成立）。三连击 = 一次双击 + 一次单击。
- 叠加窗 `WS_EX_NOACTIVATE` → 点画面不激活主窗，后台第一次点击被系统当激活吃掉 → wndproc 里 `WM_LBUTTONDOWN`/`WM_LBUTTONDBLCLK` 先 `SetForegroundWindow(main)`。拖动也在 wndproc（`SetCapture` + **累计位移** `SetWindowPos` 主窗 + 叠加窗粘住）。
- z 序：`ShowWindow` 会把叠加窗提到最前、别的进程也能提主窗 → 每帧查 `GetWindow(main, GW_HWNDNEXT) == overlay`，不对才 `SetWindowPos`。
- **mpv `dwidth/dheight` 先报 0.75 倍假值**（1280×720 先报 960×540）→ 同一尺寸连报两次才动窗口（`fit_seen`）。尺寸取显示尺寸 `dwidth/dheight` 而非 `width/height`（anamorphic 会拉扁）。视频模式的窗口尺寸**不落盘**。
- `cx.on_app_quit` 只在显式 `App::quit()` 触发，关窗口不走 → 清理靠 Drop（`persistence::log_session` → `%APPDATA%/simple-media-player/session.log`）。退出三档：有 `player_view dropped`+`event loop returned`=优雅；只有 `start`=被硬杀；应用无自杀路径。

## 窗体大小 / 全屏
- 起播按 `dwidth/dheight` 调窗：`plan_fit`（纯函数：1:1 → 缩进工作区 → 放大到最小窗口，中心锚点 + 夹 `rcWork`）→ `fit_to_video`（走 `cx.spawn_in`）。`fit_done` 每片只调一次；最大化/影院/全屏时不动窗、留空、退出后下一帧补。
- 双击画面 = **全屏播放**（不是最大化）：叠加窗 wndproc 把 `OverlayEvent::ToggleCinema` 塞 `static Mutex<Vec<_>>`，tick 取出调 `PlayerView::set_cinema(on)`（唯一入口）。出口：`Esc`/`F11`、再双击、右上「最大化」（`toggle_maximize` 先判 `cinema`）、胶囊 `exit-cinema`。
- 快捷键**必须用 `App::intercept_keystrokes`**，`bind_keys`+`on_action` 收不到：无焦点时事件派发给 dispatch 树**根**（gpui-component 的 `Root`，在视图外），实测返 `false`。
- **客户区铺满窗口**（顶栏才铺满宽度）：子类化窗口过程（`SetWindowLongPtrW(GWLP_WNDPROC)`，先调 GPUI 的）拦 `WM_NCCALCSIZE`，左/右/下 = 窗口矩形（顶边不碰）。窗口矩形：`wParam!=0` 取 `rgrc[1]`；`wParam==0` 取 `lparam` 所指 `RECT` 的**初值**（`CallWindowProcW` 会就地改写）；**别用 `GetWindowRect`**（进出全屏时是旧值）。之后自补 `WM_NCHITTEST` 缩放热区（6 逻辑像素；最大化/最小化/无 `WS_THICKFRAME` 跳过）。
- 连带：① GPUI 用创建时记的 `border_offset` 算 `ptMinTrackSize` → 720p 被顶成 720+11，拦 `WM_GETMINMAXINFO` 减掉；② 单纯改尺寸（`wParam==0`）GPUI 不管，同一条分支一并处理，否则靠 tick 自愈补一次（会抖）。
- **GPUI 会丢掉进出全屏那一次的尺寸通知**（`WM_SIZE` 在 `SetWindowPos` 里同步派发，App 正被借用 → resize 回调 `handle.update()` 失败、`log_err` 静默）→ `window.bounds()` 已新、`viewport_size` 还旧：画面铺满、顶栏只画左上角一小块、右上按钮不存在、点哪都没反应（Esc 正常，键盘不走命中测试）。`refresh()` 无救；`bounds_changed(cx)` 会跑 `bounds_observers` → 对本视图 `update` → panic。正解 `window_actions::kick_layout()`：`PostMessage` 补 `WM_SIZE`（lparam = 真实客户区，幂等），tick 比 `viewport_size` vs `bounds()`，不一致就补、300ms 限流。

## 键盘
- `←`/`→` ±5s、`↑`/`↓` ±5% 音量，都在 `intercept_keystrokes`：`Slider`（0.7.3）无 `FocusHandle`、不参与键盘，焦点到不了它。key 名 = `left`/`right`/`up`/`down`，写错**不报错、静默失效** → 解析走 `ArrowKey::parse`，用真实 `dispatch_keystroke` 守。
- 音量真正动的是 `SliderState`（`volume_pct` 只是镜像，只改镜像 = 声音变圆点不动）；`set_value` **不发 `SliderEvent::Change`**，引擎与持久化要自己再调 `set_volume_pct`。顶到 0/100 直接 return，别按住时 30Hz 写 settings.json。
- `AudioEngine::seek` 在 `Stopped` 返 `false` → 播完后方向键"没反应"。用 `play_at(path, secs)` 重起播；视频例外（播完 tick 退出视频模式，音频引擎重起 mp4 只解码失败）→ 先判 `is_video_file`。

## UI / 样式坑
- `ListItem::confirmed()` 无视觉、默认 `primary` 与正文色重合 → 用 `list_active` / `list_active_border`。
- **字体必须系统自带**：Inter / Noto Sans SC 在 Win 不预装。`theme::ui_font_family()`（Win `Microsoft YaHei UI` / mac `PingFang SC` / 其他 `Noto Sans SC`）+ `mono_font_family()`；`ThemeConfig.font_family` 铺整棵树（`root.rs`）。
- 换肤：`ThemeConfig`(#\[serde(default)\]) + `Theme::update` + `apply_config`。**`ThemeConfigColors` 的 blue/red/green/cyan/magenta/yellow(±`_light`) 是私有字段** → 只能 `default()` 后逐字段赋值。别在同一 closure 里既换 mode 又换 config。
- 布局铁律：一组控件居中，左右两栏必须**等宽**（都 `flex_1`），否则 `justify_between` 把中间顶偏。
- **控制栏 v3 几何（数字来自 `design/spec.json`，改任一条同步 `verify_floating_chrome.py` 的常数）**：胶囊 **760×92**、距下 24、圆角 14、padding **20/8**；进度区**两行**（时间行 16 → gap 4 → 轨道 4px 714 宽，`PROGRESS_SIDE_PAD=2` = spec 3 扣掉 1px 边框）；走带行 44，播放键 **44 圆**（Mauve 发光 0/4/18 35%）、走带键盒 28/图标 22，中心 −96/−54/0/+54/+96；左组 ⓘ + 音量 96；右组盒 24/gap 16 → 中心距 **40**，从右往左 全屏 0、列表 1、主题 2、音轨 3、字幕 4（设计稿 6 个含画中画，未接）；信息卡 240×180 浮胶囊上方 16、贴 x24；`BOTTOM_BAND` 118。
- **展开歌单时胶囊居中于剩余画面**（x=88 = (1280−320−24−760)/2），不是整窗居中；面板 320 宽、`top = 标题带+8`、`right 24`、`bottom 119`、头 48/行 48/尾 44。
- **浅底面板上的按钮必须用主题色**（`panel_button(fg, primary)`）：`overlay::fg()`（#D5D9E2）在 Latte 浅底上几乎看不见——页脚的循环/随机/排序踩过。面板列表容器要有 `overflow_hidden`，只给 `min_h_0` 会让最后一行画到页脚上面。
- **`overlay::glass()` 实际 72%（`0x0B0C10B8`）**，本地导出帧已是 L3（48% + backdrop blur 56）：这条渲染路径拿不到区域级 backdrop blur，48% 不糊就会糊字，所以维持平 72%。验收色差判据（两处 > 25）在 72% 下过。
- 画在画面上的东西（胶囊 / 歌单 / 信息卡）**都必须进 `chrome_rects`**：漏了 = 节点在、看不见、点不到（三次各踩一次，用户看到"点了没反应"）。
- mpv 编码名是**并列别名**（`h264 (High) / AVC / …`）→ 取第一段（`short_codec`）；`video-bitrate` 对部分容器报 `unavailable`（合成 mp4 就是），显示 `—` 是对的，别补假数。
- `cycle sid/aid` **无返回值**、单轨不做事 → 记切换前轨号 + 时间戳，tick 比对 observed 值，800ms 超时就说"没有其他轨可切"。
- 验收脚本**永远别按"胶囊铺满整宽"算坐标**：胶囊 760 居中后那些点落在画面上，点击被叠加窗吃掉，像"应用 bug"；`wait_settled()` 探针同理（每次白等 3s）。素材要 **120s**（`target/verify-long.mp4` 自动生成）：40s 片子播完 → 叠加窗隐藏 → 取样全变 `#EFF1F5`，多项假失败。
- **脚本里的胶囊左沿必须走 `capsule_left()`**（跟随面板展开态）。写死整窗居中 → 收起歌单那一下点到胶囊外 → 面板一直开着 → 后面所有按钮坐标偏 344，报出「字幕/音轨没反应」「ⓘ 没反应」两条**假失败**（应用一直是对的）。同理：每次点击都要记 `WindowFromPoint` 的窗口类名，非主窗就判 FAIL —— 坐标偏了点击会静默落到叠加窗上，截图看不出来。
- 合成输入（`mouse_event`）下的**第一次点击偶尔被吞**：验收里凡是有状态的点击都按**结果**重试（歌单 / Esc / 双击 / ⓘ 最多 3 次），判据不放松。
- **`h_flex()` 默认 `items_center`**：子元素全绝对定位（内容高 0）交叉轴塌成 0 → `canvas` bounds `Wx0`。视频区要 `.self_stretch()`，回归 `stage_fills_space_between_title_band_and_bottom_band`。
- **`height:100%` 在不定高父级退化成 auto** → `.h_full()` 解析不出（按钮只剩图标高）。写死 `.h(px(..))`，父级补 `.h_full()`。
- 自绘标题带：`appears_transparent = true` 后**只有左侧拖动区**能设 `WindowControlArea::Drag`（整条都设会让右侧按钮变非客户区）。窗口按钮 `IconName::Window*` 是 24 网格 + 2px 描边，缩 10px 会糊 → 手写 10×10、1px SVG；46 宽 × 标题栏高，关闭键悬停 `#C42B1C`。
- **chrome 前景色跟底色走不跟主题走**：视频模式整块深画布，浅色主题 `muted_foreground` 只 ≈3:1 → 用 `chrome_fg` / `chrome_dim`。
- 提示分流（`flush_errors`）：`video_active` → 标题带 `status_pill`（4s 退场，tick 比对）；否则 `push_notification`（内容区被 mpv 盖住，toast 等于没画）。通知层位置/宽度在 `theme::install` 改（`margins.top = TITLE_BAND+12`、`width=320`），`apply_config`/`change` 不重置。`push_notification` 要 `&mut Window` → 入队 + 100ms tick flush。
- 上游硬限制：0.7.3 `Slider` 无 `FocusHandle`、无 ARIA、不支持键盘。

## 输入 / 拖放 / 测试
- **GPUI 侧**：`FileDropEvent::Entered{paths: ExternalPaths}` → `cx.active_drag = AnyDrag{..}`；`Submit` 转合成 MouseUp 才触发鼠标下 hitbox 的 drop listener。根节点 `.on_drop::<ExternalPaths>` = 整窗可投。`cx.active_drag` 是 `pub(crate)`；构造 `ExternalPaths` 需 dev-dep `smallvec`。
- **叠加窗侧**：OLE 找目标 = `WindowFromPoint` + 父链向上，叠加窗截断；加 `WS_EX_TRANSPARENT` **不改变** `WindowFromPoint`（死路）。正解：主窗 `DragAcceptFiles(hwnd,TRUE)` + `WS_EX_ACCEPTFILES` → `WM_DROPFILES` → `DragQueryFileW` → 队列 → tick 消费。
- `drag_over::<T>` 回调只给 `&StyleRefinement`（**没有 Context**）→ 时间戳写 `Cell<Option<Instant>>`，tick 判断并 notify。拖进来**不弹提示**（动作即意图），只留「没有可播放的媒体文件」这类真失败。
- `TestAppContext` **不自动初始化**：`cx.update` 第一句必须 `gpui_kit::init(cx)`，否则 `no state of type Theme exists`。`test-support` 带入 GPUI 的 `test` 属性 → 测试模块**不能 glob import prelude**（遮蔽 `#[test]`）。改配置目录用 `thread_local!`，**不要 `set_var`**。
- `window.find(id)` 只认注册进命中测试树的元素 → 要 `.test_support()`（`seek-bar`、`stage`）；`find` 缺元素会 panic，判不存在用 `try_find`。视频模式无头测试用 `#[cfg(test)] test_set_video_mode` / `test_notify`，不必真起引擎。

## 构建 / 发布 / 验收工具
- 子系统/图标资源、ps1 编码与 BOM、`shot.py` 取样、`package.ps1` 退出码、`SMP_DATA_DIR` 隔离、诊断开关 → `tooling.md`。

## 设计稿要点 / libmpv 迁移
- 设计稿（色板、OSC 玻璃、控制栏几何、面板与首屏尺寸、Ardot 操作坑、视频信息 OSD）→ `design-spec.md`。
- 视频内核已是**进程内 libmpv**（第十二轮落地，`mpv.exe` 子进程 + IPC 已删）：选型、DLL 来源、线程契约、FFI 坑、验收竞态 → `libmpv-migration.md`。
