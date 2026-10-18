# Win32 / GPUI 行为备忘（从 MEMORY.md 拆出）

> Win 后端行为、窗口过程、键盘与输入派发、全屏/尺寸、拖放与无头测试。
> 这些是「不按它写就一定踩」的平台事实，跟功能无关，所以单独成篇。

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
- 上游硬限制：0.7.3 `Slider` 无 `FocusHandle`、无 ARIA、不支持键盘。

## 输入 / 拖放 / 测试
- **GPUI 侧**：`FileDropEvent::Entered{paths: ExternalPaths}` → `cx.active_drag = AnyDrag{..}`；`Submit` 转合成 MouseUp 才触发鼠标下 hitbox 的 drop listener。根节点 `.on_drop::<ExternalPaths>` = 整窗可投。`cx.active_drag` 是 `pub(crate)`；构造 `ExternalPaths` 需 dev-dep `smallvec`。
- **叠加窗侧**：OLE 找目标 = `WindowFromPoint` + 父链向上，叠加窗截断；加 `WS_EX_TRANSPARENT` **不改变** `WindowFromPoint`（死路）。正解：主窗 `DragAcceptFiles(hwnd,TRUE)` + `WS_EX_ACCEPTFILES` → `WM_DROPFILES` → `DragQueryFileW` → 队列 → tick 消费。
- `drag_over::<T>` 回调只给 `&StyleRefinement`（**没有 Context**）→ 时间戳写 `Cell<Option<Instant>>`，tick 判断并 notify。拖进来**不弹提示**（动作即意图），只留「没有可播放的媒体文件」这类真失败。
- `TestAppContext` **不自动初始化**：`cx.update` 第一句必须 `gpui_kit::init(cx)`，否则 `no state of type Theme exists`。`test-support` 带入 GPUI 的 `test` 属性 → 测试模块**不能 glob import prelude**（遮蔽 `#[test]`）。改配置目录用 `thread_local!`，**不要 `set_var`**。
- `window.find(id)` 只认注册进命中测试树的元素 → 要 `.test_support()`（`seek-bar`、`stage`）；`find` 缺元素会 panic，判不存在用 `try_find`。视频模式无头测试用 `#[cfg(test)] test_set_video_mode` / `test_notify`，不必真起引擎。
