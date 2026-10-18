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

## 设计稿要点（详见 2026-10-17 日志）
- 色板严格取 Catppuccin Latte 官方值；Mauve #8839EF 唯一强调色。
- 图标规范：播放三角占圆直径 48%（B 档），32px 以下重绘而非缩放（占比 50-54%、去描边）；图标为正圆。
- Ardot 可用字体仅 Inter / Noto 系列 / JetBrains Mono。
