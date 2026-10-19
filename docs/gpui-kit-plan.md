# Simple Media Player — GPUI Kit 实现计划

> 版本: v1.0 · 日期: 2026-10-17 · 平台目标: Windows 优先，macOS/Linux 兼容
> 框架: [GPUI Kit](https://gpui-kit.com/) v0.7（Longbridge 出品，基于 Zed 的 GPUI，Apache-2.0）

---

## 0. 结论先行

| 项 | 决策 |
|---|---|
| UI 层 | `gpui-kit` 单 crate 依赖（内部锁定 GPUI 版本并重导出 base/component/assets） |
| 组件策略 | 起步直接用 **gpui-component** 带样式组件层；不自建设计系统（暂不上 gpui-base 裸原语） |
| 音频播放 | `rodio`（纯 Rust，零系统依赖，先跑通） |
| 视频播放 | `mpv.exe` 进程 + `--wid` 叠加顶层窗口 + JSON IPC（GPUI 在 Windows 无法承载子窗口，改用进程外渲染），Stage 4 可选增强 |
| 工程纪律 | 沿用五步法：**plan review → modify → implement → verify completion → next stage**，每阶段以实际构建产物 + 运行验证为准，不信任自报告 |
| 风险最高项 | Windows 下 mpv 与 GPUI 渲染表面集成 → 放到最后阶段，音频播放器先行交付 |

---

## 1. 项目目标

构建一个跨平台桌面媒体播放器 MVP：

- 🎵 音频播放（MP3/FLAC/WAV/OGG）：播放/暂停/进度/音量/上一首下一首
- 📋 播放列表：拖拽排序、本地文件导入、持久化
- 🖥️ 原生桌面体验：120Hz GPU 渲染、明暗主题、中文界面
- 🎬 （增强）视频播放：mpv 内核嵌入

**非目标（本期不做）**：在线流媒体、歌词同步、均衡器 DSP、插件系统（gpui-shell 留到未来）。

---

## 2. 技术架构

```
┌─────────────────────────────────────────────┐
│  app crate (simple-media-player)            │
│  ┌─────────────┐ ┌──────────┐ ┌───────────┐ │
│  │ PlayerView  │ │Playlist  │ │ SeekBar / │ │   ← UI 层（gpui-component 组件）
│  │ (主窗口)     │ │Panel     │ │ VolumeCtl │ │
│  └──────┬──────┘ └────┬─────┘ └─────┬─────┘ │
│         └─────────────┼─────────────┘       │
│                  ┌────▼────┐                │
│                  │ AppState │  ← GPUI Entity │
│                  │ (Model)  │    状态中心     │
│                  └────┬────┘                │
├───────────────────────┼─────────────────────┤
│  player-core crate    │                     │
│  ┌────────▼────┐  ┌───▼────────┐            │
│  │ AudioEngine │  │ TrackMeta  │            │
│  │ (rodio)     │  │ (lofty 解析 │            │
│  │             │  │  元数据)    │            │
│  └─────────────┘  └────────────┘            │
│  ┌─────────────┐   （Stage 4 ✅）              │
│  │ VideoEngine │ ← 进程内 libmpv（C API）      │
│  │  + Render   │   SW render API → BGRA 帧      │
│  └──────┬──────┘   → app::video_surface 纹理    │
└─────────┼───────────────────────────────────┘
          └→ gpui_kit::img()（画面在 GPUI 合成树里）
        gpui-kit (base / component / assets)
        GPUI (GPU 加速保留模式渲染)
```

### 依赖清单（Cargo.toml 草案）

```toml
[dependencies]
gpui-kit = "0.7"          # UI 框架（含 component/assets 默认 feature）
rodio = "0.21"            # 音频解码与播放
lofty = "0.22"            # 音频元数据（封面、时长、标签）
serde = { version = "1", features = ["derive"] }
serde_json = "1"          # 播放列表持久化
directories = "6"         # 配置目录定位
# Stage 4 引入（实际落地）：
# libloading = "0.8"        # 运行时加载 libmpv-2.dll
# image = "0.25"            # RenderImage 的帧格式（与 gpui-pre 同版本）
# async-channel = "2"       # render 回调（mpv 线程）→ UI 线程的帧信号
# libmpv-2.dll 为外部依赖（scripts/fetch-libmpv.ps1）
```

---

## 3. 阶段划分（Stage 0 → 5）

### 🟢 Stage 0 — 环境验证（0.5 天）✅ 2026-10-17

**目标**：空窗口跑起来，验证工具链。

- [x] 安装 Rust stable（1.85+），确认 MSVC 工具链（Windows）——实测 rustc 1.99.1 x86_64-pc-windows-msvc
- [x] `cargo new simple-media-player`，添加 `gpui-kit` 依赖（workspace：crates/app + crates/player-core）
- [x] 跑通官方 hello_world 模式：`gpui_kit::application().run(...)` 打开一个窗口显示 "Hello"
- [ ] （可选）`npx skills add longbridge/gpui-kit` 给编码 Agent 装官方技能

**验收**：`cargo run` 弹出窗口，120Hz 渲染无报错；`git commit` 基线。✅（基线 41c13ef，首次全量构建 18m14s 通过）

---

### 🟢 Stage 1 — 主窗口骨架与布局（1 天）✅ 2026-10-17

**目标**：播放器三栏布局落地，纯静态 UI。

- [x] 主窗口 + 标题栏定制（`gpui_kit::open_window` + TitlebarOptions，1040x680，min 720x480）
- [x] 布局：左侧 Playlist 面板 / 右上封面区 / 底部控制栏（播放按钮、SeekBar、音量）
- [x] 用 gpui-component 组件：`Button` / `Slider` / `List`（虚拟滚动 delegate）/ `Icon`（Lucide 图标；`icon_assets!` 注册 SkipBack/SkipForward/Volume 系列/Music/ListMusic 额外图标）
- [x] 明暗主题切换（`Theme::change`；`--dark` 启动参数 + 底栏月亮/太阳按钮）
- [x] 中文文案 + 字体渲染验证（截图确认无豆腐块）

**验收**：窗口可缩放，布局不错乱；主题切换即时生效（--dark 启动路径实测通过，按钮 handler 调同一 API）；截图存档 `docs/screenshots/stage1-light.png` / `stage1-dark.png`。✅

---

### 🟢 Stage 2 — 音频播放内核（1.5 天）✅ 2026-10-17

**目标**：能选文件、能出声。

- [x] `player-core` crate 拆分：`AudioEngine` 封装 rodio `Sink`（OutputStreamBuilder + connect_new，无设备时降级为只读 UI）
- [x] 播放控制：play / pause / stop / seek（`Sink::try_seek`）/ set_volume
- [x] 文件选择对话框（`rfd::AsyncFileDialog`，元数据解析放 background_spawn）
- [x] 元数据解析：lofty 读时长/标题/艺术家（WAV 无 tag 回退文件名）
- [x] GPUI Entity 状态同步：进度通过 `cx.spawn_in` + 100ms 定时器轮询推送（`update_in` 带窗口回写 Slider）；拖动期间 user_seeking 抑制回写，Release 才 seek（防爆音）
- [x] 播放结束自动切下一首（tick 检测 `track_finished()`，列表末尾停止）

**验收**：`cargo test -p player-core` 9 项全绿（含真实设备播放/seek/暂停/播完检测）；实机 PostMessage 点击曲目 → `playlist.json` current_index 持久化验证链路通。播放中截图 `docs/screenshots/m1-playing.png`。✅

---

### 🟢 Stage 3 — 播放列表（1 天）✅ 2026-10-17

**目标**：完整歌单管理。

- [x] `List` 组件 + 虚拟滚动（Stage 1 已接）
- [x] 添加文件/文件夹（递归扫描，支持 mp3/flac/wav/ogg/m4a/aac/aiff）、行尾删除、清空
- [x] 拖拽排序（ListItem on_drag/on_drop + DragTrack 预览，confirmed/selected/current 三方索引重映射）
- [x] 单击播放（List 内建 Confirm 语义）、当前曲目高亮
- [x] 持久化：`%APPDATA%/simple-media-player/playlist.json`（曲目 + current_index），启动恢复
- [x] 记住音量（settings.json，音量变化即落盘）与窗口尺寸（resize 后 ~5s 节流落盘）

**验收**：预置 playlist.json 重启恢复实测（2 首曲目完整显示，截图 `m1-restored-playlist.png`）；player-core/app 单测 11 项全绿。✅

---

### 🟢 Stage 4 — 视频播放（可选增强，2~3 天，风险最高）✅ 2026-10-17

**目标**：mpv 内核嵌入播放视频。

**实施结论（与原始方案 A/B 不同）**：纹理共享与软解帧两条路都没有走，改为 **mpv.exe 进程 + `--wid` 顶层子弹窗**：
GPUI 的 Windows 后端使用 `DirectComposition + WS_EX_NOREDIRECTIONBITMAP`，**无法承载原生子窗口**；而 `--wid` 交给 mpv 一个我们自建的
`WS_POPUP` 顶层窗口（owner = 主窗口），mpv 用 `vo=gpu` + `hwdec=auto-safe` 直接 DXVA 硬解上屏，零拷贝、零帧回传。
代价：视频区内的 GPUI 内容会被视频覆盖（当前无浮层需求，可接受）。控制通道走 mpv JSON IPC（`--input-ipc-server` 命名管道）。

- [x] 视频内核 `player-core::video`：mpv 进程生命周期 + JSON IPC（`VideoEngine` / `VideoStatus` / `find_mpv`）
- [x] 叠加窗口 `app::video_overlay`：纯 FFI（无新 crate），`SetWindowPos` 跟随主窗口 bounds，客户区原点换算物理像素
- [x] 视频/音频模式自动切换（按扩展名，15 种容器）
- [x] 全屏切换（影院模式：系统全屏 + 隐藏歌单）、宽高比保持（mpv 默认 letterbox）
- [x] 视频模式复用同一 mpv 进程（切曲只 `loadfile`，不重启进程）
- [ ] ~~`libmpv2` + render API 回调取帧~~（改为 mpv.exe `--wid`，见上）

**验收实测**（2026-10-17，debug 构建，3840×2160 屏 / 16 逻辑核）：
- 1080p H.264（testsrc2 10s，30fps）：mpv 峰值 9.27%、均值 5.26% 单核占用 → 折算整机 ~0.33%，**满足 < 15%** ✅
- 播放/暂停/seek/上一首下一首/自动连播（视频→视频→音频）/影院模式进出 均实机截图验证 ✅
- 音视频同步：由 mpv 内部时钟保证，UI 侧只做 100ms 轮询显示，无二次补偿 ✅

**已知限制 / 遗留**：
1. 视频区内的 GPUI 元素会被视频窗口遮挡（未来要加浮层需改纹理共享方案）。
2. ~~播放列表“当前曲目”高亮用 `theme.primary` 着色，区分度弱~~ → **已修（2026-10-18）**：
   根因是默认主题里 `primary` 与正文色几乎重合（light: neutral-900 vs neutral-950；dark: neutral-50 vs neutral-50）。
   改用列表语义 token（`list_active` 行底色 + `list_active_border` 左侧强调条）+ 图标换成 Volume2 + 标题半粗，
   共 4 处视觉线索；明暗两套主题均截图验证，非当前行留 2px 透明占位条避免布局跳动。
3. mpv 需外部提供：`SMP_MPV_PATH` 环境变量 → PATH → 常见安装路径依次探测；缺失时视频区显示明确错误文案，音频功能不受影响。
4. 退出路径清理：`cx.on_app_quit` 只在显式 `App::quit()` 时触发，**窗口关闭不走它**；mpv 回收实际由
   `VideoEngine::drop` 兜底（实测：播放中关窗口，mpv 进程确实被回收，无孤儿）。
5. 空闲自行退出（M2 期间观察到一次，无崩溃/无 stderr）→ 已加会话留痕
   `%APPDATA%/simple-media-player/session.log`（start / player_view dropped / app_quit / panic / event loop returned），
   下次复现即可区分「应用主动退出」与「被外部终止」。

**降级预案（未触发）**：若纹理共享/叠加窗口受阻超过 1 天，MVP 以音频播放器交付，视频功能砍为 v2。

---

### 🟢 Stage 5 — 打磨与发布（1 天）✅ 2026-10-18

- [x] UI 集成测试：`gpui_kit::test` 无头窗口驱动指针/键盘，断言播放状态、焦点、布局
  - `crates/app/src/ui_tests.rs`，7 个用例：无障碍名称、播放/暂停、上一首/下一首循环、空列表无副作用、
    坏文件报错、主题切换、控件不溢出窗口。
  - 持久化目录用线程局部 override 重定向到临时目录（`persistence::set_config_dir_for_test`），
    不污染真实 `%APPDATA%`。
  - 代价：`gpui-kit` 的 `test-support` feature 会带进 `gpui-fast-platform` 依赖，
    与本机 lockfile 里 pin 的 `cocoa 0.26.1` / `unicode-properties 0.1.4` 冲突，
    需要把它们降到 `0.26.0` / `0.1.3`（仅影响 macOS / wasm 目标，Windows 不编译）。
- [x] 无障碍检查（AccessKit 角色/名称）
  - 全部图标按钮补 `accessibility_label`（添加文件/文件夹、清空、上一首、播放/暂停、下一首、全屏、主题），
    播放按钮名称随状态在「播放/暂停」间切换；列表行已有 `accessibility_label`（含「正在播放」后缀）；
    行尾删除按钮补名称 `从播放列表移除 <曲名>`。
  - 进度条/音量条：上游 `Slider`（gpui-component 0.7.3）没有 `FocusHandle`、没有 ARIA 角色与值、
    也不响应键盘，组件层无法补齐。已用带 `aria_label` 的容器提供分组名，键盘操作列为已知限制。
- [x] 错误处理：损坏文件、解码失败、设备占用的 toast 提示
  - `PlayerView::report_error` 入队 → `tick` 统一 `window.push_notification(Notification::error)`；
    覆盖：音频打不开/解码失败、无音频设备、mpv 缺失/启动失败/拿不到窗口句柄/连接中断、
    视频加载失败（区分「文件不存在」与「mpv 无法加载」）、导入后无可用文件（info 提示）。
  - `VideoStatus.error` 透出 mpv 的 `end-file` 原因与 `file_error` 细节（如 `error: loading failed`）；
    视频加载失败后自动跳到下一首，不卡在错误态。
  - 诊断开关：`SMP_MPV_LOG=<路径>` 落盘 mpv stderr，`SMP_MPV_TRACE=<路径>` 落盘 IPC 收发报文。
- [x] `cargo build --release`，Windows 打包
  - `scripts/package.ps1`：release 构建 → `dist/simple-media-player-1.0.0/`（exe + README + MANIFEST 校验和）
    → 同名 zip（8.7 MB）；`-WithMpv` 可把 mpv.exe 一并打入（GPL 合规需自行确认）。
  - 实测：解压到临时目录 + 全新 `APPDATA` 启动，视频/音频/影院/错误提示全部正常，mpv 进程随窗口关闭回收。
- [x] README + 截图（`docs/screenshots/chroma-{light,dark,video}.png`）

**验收**：release 包在干净目录 + 全新配置下可运行 ✅；测试套件 15（player-core）+ 11（app）全绿 ✅；
release 1080p H.264 播放 mpv 峰值 10.85% 单核（整机 ~0.68%），满足 < 15% ✅。

**已知限制（Stage 5 结束时）**：
1. 视频区不可绘制 GPUI 浮层（DirectComposition 架构约束，需纹理共享方案）。
   因此 v2 布局里画面只占「标题带以下、胶囊以上」，展开歌单时右侧再让位 344px——
   这两处偏差是架构决定的，不是没做完。
2. 进度条/音量条仅支持鼠标（上游 Slider 缺键盘与 ARIA 支持）。
3. mpv 默认不随包分发，需 `-WithMpv` 或用户自备。
4. 仅在 Windows 实机验证；macOS / Linux 分支未测。

---

### 🎨 设计稿对齐（2026-10-18 补做）

**问题**：Stage 1–5 一直用 gpui-component 的默认中性灰主题，而设计稿（Ardot `737444551354649`）
是 Catppuccin Latte + 单一 Mauve 强调色——两套视觉体系，所以"样式差别很大"。

**做法**：新增 `crates/app/src/theme.rs`，把设计稿色板翻译成 gpui-component 的语义 token
（`ThemeConfig` → `Theme::update` + `apply_config`），界面代码只读 token，不再出现写死色值。

| 项 | 设计稿 | 落地 |
|---|---|---|
| 墙体 / 分隔 / 文案 | Base `#EFF1F5` · Surface0 `#CCD0DA` · Text `#4C4F69` | `background` / `border` / `foreground` |
| 强调色 | Mauve `#8839EF`（唯一彩色） | `primary` + 播放按钮 / 进度已播放 / 当前曲目 / 滑块拇指 |
| 播放列表 | 右侧 320、padding 20、条目 48、圆角 8 | 面板 `w(320).px(20)`，`ListItem::h(48).rounded(8)` |
| 条目内容 | 序号（等宽 11）/ 曲名 13 / 元信息 11 | 序号列 22 宽，当前项换成均衡器图标 |
| 控制栏 | padding 16 + gap 12，进度在上、时间在下 | `render_control_bar` / `render_seek_row` |
| 播放按钮 | 44 正圆、Mauve 实心 | `Button.with_size(44).rounded(22).primary()` |
| 字体 | Noto Sans SC（CJK）+ 等宽 | `font_family` / `mono_font_family`，字号 13/11/15 |

深色模式用同结构的 Catppuccin **Mocha**（`#1E1E2E` / `#CBA6F7`），切换时不跳出这套体系。

**刻意保留的偏差**（都源于 mpv 叠加窗口这一架构约束，已写入 README「视觉规范」）：
视频不铺满整窗、控制栏在视频下方而非浮于其上、右侧 CC/音轨/设置/PiP 图标因无对应功能不画。

**验证**：`theme::tests` 两个用例断言 token 真的落到全局（Base/Mauve/Mocha）；
UI 测试改为先装真实皮肤再断言；`scripts/shot.py` 按 DPI 缩放截图，产出
`docs/screenshots/chroma-{light,dark,video}.png` 三张对照图。

---

### 🎬 设计稿 v2：IINA 式沉浸布局（2026-10-18 补做）

设计稿在另一个会话里迭代成沉浸式布局，与 v1（右侧常驻 320 歌单 + 底部 106 控制栏）
是两套结构，所以渲染层整体重写（`player_view.rs` 的 render 部分）。

| 项 | v1 | v2（当前） |
|---|---|---|
| 窗口框架 | 系统标题栏 | 自绘 **34px 标题带**：拖动区 + 双击最大化 + 最小化/最大化/关闭 |
| 内容区 | 左歌单 + 右内容 | **内容占满**（`flex_1` + `self_stretch`），无固定分栏 |
| 控制条 | 底部 106 实心条 | **悬浮玻璃胶囊**：内缩 24、圆角 14、进度 + 时间 + 走带三段 |
| 播放列表 | 常驻 320 左栏 | **默认收起的悬浮面板**（右侧 12、宽 320、Latte 实心底），右下角按钮开合 |
| 视频模式 | 画面占左半 | 画面占满（展开歌单时右侧让位 344） |
| 色板 | 单一 Latte/Mocha | **两套**：应用表面 Latte/Mocha + 叠加层深色玻璃（`theme::overlay`） |

**踩到的坑（重要）**：改完布局后视频"有声音、黑屏"。根因不在 mpv，而是 `h_flex()`
默认 `items_center`——舞台的子元素全是绝对定位（内容高度 0），舞台在交叉轴上塌成
**0 高度**，视频区 `canvas` 捕获到 `0x0`，叠加窗永远 `SetWindowPos(0,0,0,0)`。
表面上看不出来（背景色由根节点铺满，音频模式也不报错），只有视频会空。
修法：舞台加 `self_stretch()`，并补回归用例
`stage_fills_space_between_title_band_and_bottom_band`（断言舞台高度 > 窗口一半）。
排查手法：临时把 `canvas` prepaint 拿到的 bounds 写进 `session.log`，一眼看出 `x=1040 h=0`。

**另外补的**：`--play <文件>` / 位置参数打开即播（顺带解决了"模拟点击不稳定、没法
自动化验证视频链路"的问题，现在用 `scripts/shot.py -- --play x.mp4` 就能拍视频截图）。

**验证**：三张截图重拍（1040×680，1:1 逻辑像素）；测试 15（player-core）+ 15（app）
全绿（新增舞台高度用例）；影院模式、视频+歌单同屏、音频模式实机截图确认。

---

### 🖱 拖放 / 窗口按钮 / 胶囊比例（2026-10-18 第二轮修）

用户反馈三条：拖文件进窗口不播、右上角窗口按钮太小不像 Windows、控制条相对窗口过于笨重。

**1. 拖放（原实现完全没有监听）**

GPUI 的拖放链路：系统投放 → `FileDropEvent::Entered { position, paths: ExternalPaths }`
→ 存进 `cx.active_drag`（载荷 `Arc<ExternalPaths>`）→ `Submit` 变成一次合成 MouseUp →
鼠标下元素的 drop listener 被调用（前提：`drag.value.type_id()` 与 listener 的 `T` 相同、
且该 hitbox 处于 hover）。所以根节点挂 `.on_drop::<ExternalPaths>(...)` 就是「整窗可投放」。

但叠加窗口把窗口 80% 盖住了，而 OLE 找投放目标是 `WindowFromPoint` + 沿父链向上，
断在叠加窗口（GPUI 的 target 注册在主窗口上）。于是叠加窗口自己收：
`DragAcceptFiles(hwnd, TRUE)` + wndproc 里处理 `WM_DROPFILES` → `DragQueryFileW` 取路径
→ 队列 → 100ms tick 消费。**两个前置事实要先确认**（都实测过）：
`cx.active_drag` 是 `pub(crate)`（应用侧读不到状态）、`ExternalPaths` 的载荷类型
是私有 smallvec（应用侧构造不了，除非自己加一份 `smallvec` dev-dependency）。

**拖入提示**只能靠 `drag_over::<ExternalPaths>`——它的回调只给 `&StyleRefinement`，
拿不到 `Context`，既不能改普通字段也不能 `cx.notify()`。做法：回调里只写
`Cell<Option<Instant>>` 时间戳，tick 判断「250ms 内还有时间戳 = 仍在拖」再
`cx.notify()`。这样拖走时提示会自己熄灭，不依赖 `Exited` 有没有送到。

**2. 窗口按钮**：`IconName::WindowMinimize/Maximize/Close` 是 24 网格 + 2px 描边的
字形，缩到 10px 只剩半个像素，栅格化是一团糊。改成手写 10×10、1px 描边的 SVG，
用 `Icon::default().data(SVG_BYTES).size(px(10.))` 渲染；按钮本体 46 宽 × 标题栏高，
悬停铺满底色，关闭键用系统红 `#C42B1C`；最大化时按 `window.is_maximized()` 换还原图标。

**3. 胶囊比例**：padding 20×12 → 16×8、圆角 14 → 12、行距 6 → 4、进度条容器 12 → 10、
时间码 11 → 10，实高 107 → ~90，`BOTTOM_BAND` 132 → 116。

**顺带修的对比度问题**：视频模式下整块 chrome 是深色画布，但标题带/窗口按钮还在取
浅色主题的 `muted_foreground` / `secondary_foreground`，3:1 左右，「看着像没渲染」。
新增 `chrome_fg` / `chrome_dim`：前景色跟着**底色**走（`video_active ? overlay::* : theme.*`）。

**验证手段**（这次没靠肉眼）：
- UI 用例 `dropping_files_adds_them_and_starts_playback`：直接 `window.dispatch_event(
  FileDropEvent::Entered/Submit)` 打进无头窗口，断言「入列表 + 当前曲目 = 0」——
  这是唯一能覆盖 GPUI drop 链路的办法（`ExternalPaths` 应用侧构造不了，只能靠
  `smallvec` dev-dependency 补齐）。
- 叠加窗口那条路径：用 `PostMessage(WM_DROPFILES)` + 手工拼 `DROPFILES` 结构体验
  wndproc 解析正确（路径原样进 `session.log` 的 `drop N path(s): …`），
  并确认运行时 `GetWindowLong(GWL_EXSTYLE)` 带上了 `WS_EX_ACCEPTFILES`（0x10）。
- 真实 OLE 拖源（ctypes 手搓 `IDropSource` + `SHCreateDataObject`）试过：能让应用
  收到投放，但 `DoDragDrop` 的模态循环在自动化环境里不返回（需要真人松手），
  所以没把它做成常驻测试。

**测试** 15（player-core）+ 16（app）全绿，`clippy --workspace --all-targets` 零警告。

---

### 💬 提示的两种呈现 / 按钮悬停条（2026-10-18 第三轮修）

用户反馈两条：**打开视频文件时弹一条 toast 就够，不要展开右侧面板**；**窗口按钮悬停时
底色只有一条横条，没铺满**。

**1. 打开/拖入不再自动展开歌单**

`import_dropped` 里原来有 `self.playlist_open = true`（"让用户立刻看到东西进来了"）。
但在视频模式下展开面板会挤占画面（画面右侧让位 344px），用户只是打开一个文件却被顶开
一块面板，反而碍事。改为：不动面板，用一条提示回执（`已导入 N 个文件 · 正在播放「X」`）。

**2. toast 在视频模式下根本不可见 → 换成标题带胶囊**

发现的问题：视频模式整块内容区被 mpv 叠加窗盖着，gpui-component 的 toast 画在里面 = 没人看得见；
而音频模式下默认位置（`TITLE_BAR_HEIGHT + 16 = 50`）正好贴着窗口按钮下沿，宽度 382
在 1040 窗口里占 37%，像块公告板。

处理：
- `flush_errors` 按**底色**分流：`video_active` 时写 `status_pill`
  （`Option<(SharedString, Instant, bool)>`），否则 `window.push_notification`。
- 胶囊画在标题带右侧、窗口按钮左边（`id("status-pill")` + `.test_support()`），
  玻璃底 + 状态图标，`STATUS_PILL_DURATION = 4s` 自动退场。
- 有效期判断统一交给 tick（`status_pill_shown` 与当前状态比对后 `cx.notify()`）——
  出现和消失都没有别的事件源，靠 tick 是唯一稳的办法（同拖放提示的处理）。
- `theme::install` 里把通知层的 `margins.top` 改成 `TITLE_BAND + 12`、`width` 收到 320，
  所以 `TITLE_BAND` 提升为 `pub(crate)`。

**3. 按钮悬停条 = CSS `height: 100%` 在不定高父级上退化成 auto**

`caption_button` 里写了 `.h_full()`，外层那圈 `h_flex()` 没有显式高度（高度由按钮撑开），
于是 `height: 100%` 解析不出来 → 按钮只剩图标那 10px 高 → 悬停底色成了一条横线。
修法：给按钮写死 `.h(px(TITLE_BAND))`，父级补 `.h_full()`。
实测悬停块 = 46×34 逻辑像素（scale 1.5 下 69×46 物理像素），右上角关闭键整块 `#C42B1C`。

**验证**：
- 新用例 `video_mode_uses_title_band_pill_instead_of_toast`：音频模式断言
  `try_find("status-pill")` 为空（走 toast）；切 `video_active` 后断言胶囊存在、
  `bounds().bottom() <= TITLE_BAND`（必须压在叠加窗之上）、文案正确。
  开关用 `#[cfg(test)]` 的 `test_set_video_mode` / `test_notify`，不必真起 mpv。
- 真机：往叠加窗口投 `WM_DROPFILES` 后截图，标题带出现「已导入 1 个文件 · 正在播放」，
  4s 后自动消失；音频模式用损坏的 wav 触发错误，toast 落在按钮下方。

**测试** 15 + 17 全绿。

---

### 🔤 字体与任务栏图标（2026-10-18 第四轮）

用户问：「title 字体在 win 上默认是否用雅黑更好？在 win 的任务栏上显示时，没有 logo。」

**1. 字体：设计稿的字体 Windows 不预装**

`theme.rs` 里写死的是 `Noto Sans SC`（Ardot 设计稿用的）。本机装过这套字体，
所以一直没暴露问题；但 Windows 出货不带它，发行包到用户机器上字体解析失败会退到
系统兜底——中文常落到宋体，11px 下横细竖粗，和设计稿的观感完全不是一回事。

改成按平台取**系统自带**的那一套：

| 平台 | 正文 | 等宽 |
|---|---|---|
| Windows | `Microsoft YaHei UI` | `Consolas` |
| macOS | `PingFang SC` | `Menlo` |
| 其他 | `Noto Sans SC` | `DejaVu Sans Mono` |

同时把 `player_view` 里写死的 `MONO_FONT = "Consolas"` 收进 `theme::mono_font_family()`——
写死会让 macOS/Linux 退到非等宽字体，时间码一跳动整行宽度跟着抖。

选型方法是三方截同一帧对比（`target/verify/font-compare3.png`）：Noto / Segoe UI 主族 /
雅黑。Segoe 主族时 CJK 走系统回退，不同机器可能落到不同字体；雅黑主族则是确定的，
中英混排都在同一套字面里。用例 `ui_font_is_a_system_font_not_a_design_tool_font` 守住
「不许回退成设计稿专用字体」。

**2. 任务栏没有 logo：PE 里没有 ID=1 的图标组**

GPUI 的 Windows 后端注册窗口类时调的是
`LoadImageW(module, MAKEINTRESOURCE(1), IMAGE_ICON, 0, 0, LR_DEFAULTSIZE | LR_SHARED)`
（`gpui-pre-windows` 的 `load_icon`）——**只认 ID = 1**。exe 里没有这条资源时它拿到空句柄，
窗口类就没图标，任务栏/Alt+Tab 显示系统占位图。`WindowOptions.icon` 帮不上忙（x11 only）。

做法：`assets/chroma.ico` 由 `scripts/make-icon.py` 生成（1024 超采样 → 16…256 多尺寸），
`crates/app/build.rs` 用 `embed-resource` 把 `1 ICON …` + VERSIONINFO 编进 PE。
找不到 `rc.exe`（没装 Windows SDK）时只 `cargo:warning` 不失败。

踩到的坑：`fs::canonicalize` 返回的是 `\\?\C:\...` 扩展长度路径，直接塞进 `.rc`
会被 rc 的字符串转义吃掉（`\\` 是转义序列），rc 报「找不到文件」而且不告诉你为什么。
修法是去掉 `\\?\` 前缀并换成正斜杠。

**验证**（三层，都不靠肉眼猜）：
- PE 资源：`LoadLibraryExW(DATAFILE)` + `FindResourceW(1, RT_GROUP_ICON/RT_ICON/RT_VERSION)`
  三个 ID 都存在（debug 与 release 都验过）。
- 运行时：`GetClassLongPtrW(GCLP_HICON)` 非 0（`0x8DA0B95`），说明窗口类真的拿到了图标。
- 观感：截任务栏，按 Mauve 色值定位到按钮，确认是「紫底 + 白三角」的 Chroma 图标。

**3. 顺带：截图不再污染真实数据**

`SMP_DATA_DIR` 环境变量可以把配置目录整体挪走（便携模式 / 自动化）。
起因是这轮截图为了拍中文标题，直接改了 `%APPDATA%` 里的 `playlist.json`，
把用户正在用的列表覆盖了——以后所有截图/验收都跑在 `target/verify/*` 的临时目录里。

**测试** 15 + 18 全绿，clippy 0 告警。

---

### 🪟 画面上的 chrome：给叠加窗口开洞（2026-10-18 第五轮）

用户：「顶栏和控制栏未悬停在视频上方；拖放进去时，不用通知了；播放列表在视频上方，
不挤视频位置；鼠标左键点中视频时，可以拖动窗体。」

**问题的真身不是布局，是「谁在上面」**

之前视频模式的画面只占「标题带以下、胶囊以上」，歌单展开时还要再让出右侧 344px——
因为整个内容区被 mpv 的叠加窗口（独立顶层 `WS_POPUP`）盖着，GPUI 画在那里的东西一律看不见。
用户要的其实是「画面占满整窗、控件浮在画面上」，这在旧架构下做不到。

**正解：`SetWindowRgn` 开洞**

窗口区域同时裁剪**渲染**和**命中测试**。把 chrome 那几块从叠加窗口的区域里 `RGN_DIFF` 掉，
下面的 GPUI 主窗口就露出来了，chrome 于是真的压在画面上：

```rust
let region = CreateRectRgn(0, 0, width, height);            // 整窗
for hole in holes {
    let cut = CreateRoundRectRgn(l, t, r, b, d, d);         // d 是**直径**，不是半径
    CombineRgn(region, region, cut, RGN_DIFF);              // 复制语义，cut 用完要删
    DeleteObject(cut);
}
SetWindowRgn(hwnd, region, 1);                              // 成功后 region 归系统，别再删
```

挖三块：标题带整条（右端有窗口按钮，中间截断很怪）、胶囊（圆角 12 + 1.5px 抗锯齿留白）、
歌单（展开时）。于是 `render()` 的顺序也倒过来了——视频舞台先声明、绝对定位铺满整窗，
chrome 全部排在它后面（GPUI 按子节点顺序绘制，后画的在上面）。

四个细节（都踩过）：
- 区域只认**物理像素**，且 `SetWindowPos` 改尺寸时**不会**跟着缩放 → `set_rect` 之后必须重算；
- 胶囊高度是内容撑开的（随字号/行距变），不能写死 → 胶囊里塞一个 `canvas` 每帧量
  `capsule_rect`，再外扩 1.5px（挖太贴边会把 1px 白描边切掉半个像素）；
- `set_holes` 要按内容去重（`Vec<Hole>` 比较），否则 100ms tick 会不停打系统调用；
- 空 holes = `SetWindowRgn(hwnd, 0, 1)` 恢复整窗，退出视频模式时要还原。

**按住画面拖窗体**

叠加窗口是不抢焦点的工具窗，按下去本来什么都不发生。在它的 wndproc 里接：

```rust
if msg == WM_LBUTTONDOWN {
    let owner = GetWindow(hwnd, GW_OWNER);
    ReleaseCapture();
    SendMessageW(owner, WM_NCLBUTTONDOWN, HTCAPTION, 0);   // 转交给系统当标题栏拖动
}
```

系统自己的移动循环去搬主窗口，叠加窗由 tick 追上。比手写 `SetWindowPos` 跟手：贴边吸附、
最大化还原、多显示器跨屏这些系统行为都白拿。

**拖放不再弹提示**

`append_dropped_and_play` 里那条「已导入 N 个文件 · 正在播放「xxx」」删掉了。
拖进来这个动作本身就是意图，画面换了、走带动了就是回执，再复述一遍是噪音。
（列表为空时仍保留一条「没有可播放的媒体文件」——那是真的失败了。）

**验收**（`scripts/verify_floating_chrome.py`，不是靠肉眼看截图）

| 检查 | 手段 | 结果 |
|---|---|---|
| 画面铺满整窗 | 取样 (lw/2, 40) 应为画面彩条 | `(0,0,78)` 蓝 → 是画面 |
| 标题带压在画面上 | 取样 (8,17) 应为画布深色 | `(8,9,12)` → 挖出来了 |
| 胶囊压在画面上 | 取样 (40, lh-60) 应为玻璃深色 | `(10,11,14)` |
| 歌单悬浮 | 点列表键后取样面板内 | `(231,227,245)` 浅底，画面中心仍是画面 |
| 最大化后仍然开洞 | 点最大化键后取样标题带 | 客户区 3840×2088，标题带仍 `(8,9,12)` |
| 再点一次能还原 | 新几何下再点同一个按钮 | 客户区回到 1898×1189 |
| 按住画面拖窗 | 真实鼠标拖 96px | 窗口位移 `(144,72)` |

脚本还留了两个可复用的经验：逻辑尺寸要用**实测客户区**换算（`SetWindowPos` 设的是外框，
1898÷1.5 = 1265 而不是 1280，按目标尺寸换算会整体偏几像素，点 28px 图标正好点进间隙）；
GPUI 的命中测试认**真实光标**，`PostMessage` 的 lparam 不总被采信，交互验收一律走
`SetCursorPos` + `mouse_event`。

第六轮又补了三条，都是「看起来像 bug、其实是测试环境」的坑：

- **第一次点击只用来激活窗口**：脚本进程调 `SetForegroundWindow` 常被系统拒（只有前台
  进程能抢焦点），于是第一次真实点击被消耗在激活上，事件到不了 GPUI（第一次点歌单没反应、
  第二次点同一个按钮反而灵）。做法是先对着画面「预热」点一下（叠加窗当 `HTCAPTION` 处理，
  没移动等于没发生），并打印 `GetForegroundWindow()` 确认。
- **样片要够长**：仓库里的样片只有 5 秒，整套流程跑二十几秒，视频中途播完 → 叠加窗被隐藏，
  后面的「按住画面拖窗」其实点在 GPUI 画布上，报假失败。脚本优先用现生成的 40 秒长片
  （`mpv av://lavfi:testsrc2=... --o=target/verify-long.mp4`，不进仓库）。
- **状态切换要等，不要 sleep**：`ShowWindowAsync` 排队 + DWM 最大化动画，固定 0.9s 偶尔
  读到中间态（实测读到过 1898×2086）。改成轮询 `IsZoomed` + 客户区尺寸连续两次一致。
- 再加一条：每次点击打印 `WindowFromPoint` 的窗口类——「点了没反应」时能立刻看出是
  叠加窗的洞没挖对，还是事件根本没到 GPUI。

**新增用例** `video_mode_floats_chrome_over_a_full_window_picture`：视频模式下舞台四边必须
贴住窗口，胶囊/进度条落在舞台内部，展开歌单后舞台宽度一像素都不变——把「浮在上面」这件事
从肉眼判断变成断言。

**残留的两处不一致**（写进 README 的「与设计稿的差异」）：洞露出来的是 GPUI 的**不透明**画布，
所以 chrome 是实心深色而不是设计稿的毛玻璃；歌单面板仍用应用表面色（深底会让列表行文字不可读）。

**测试** 15 + 19 全绿，clippy 0 告警。

---

### 🔁 最大化之后能点回来了（2026-10-18 第六轮）

用户：「最大化之后，再点不能恢复原窗口大小。」（另问：GPUI 最新版支持窗口透明吗？）

**根因：`zoom_window()` 在 Windows 上只最大化、不还原**

```rust
// gpui-pre-windows 0.3.9 / window.rs
fn zoom(&self) {
    ShowWindowAsync(self.0.hwnd, SW_MAXIMIZE);   // ← 没有还原分支
}
```

而 `is_maximized()` 本身是对的（`!is_fullscreen() && IsZoomed(hwnd)`），
所以图标会正确变成「还原」，点下去却毫无反应——这正是用户看到的现象。

**自己实现，但第一版又踩了一个更深的坑**

第一版：`IsZoomed` + 同步 `ShowWindow(SW_MAXIMIZE / SW_RESTORE)`。结果最大化之后
「再点一次」依然点不到按钮。日志证明第二次点击根本没进回调，探针进一步发现：

- 窗口真变成了 3840×2088（`GetClientRect` 这么报）；
- 但界面还画在 1040×680 上，右边一大片空白，hitbox 全在旧位置。

原因是**同步 `ShowWindow` 在 GPUI 的事件派发里重入 `WM_SIZE`**：GPUI 的 wndproc
在鼠标回调中途被调用，这条尺寸变更没有走到「重新布局」那条路径，于是绘制/命中测试
全停在旧尺寸。改成 `ShowWindowAsync`（命令投回消息队列，等当前事件派发结束后再处理）
之后，布局正常跟上，点「还原」立刻回到 1898×1189。

**代价与配套**

- 异步 → 调用返回时 `IsZoomed` 还是旧值。所以 `maximized` 先乐观更新，
  `zoom_pending` 记一笔时间，tick 在 300ms 内不做反向纠正（否则图标会闪一下）。
- tick 每帧比对 `IsZoomed`：用户用 Aero Snap / Win+↑ / 右键菜单改状态时，
  300ms 后图标自动跟上。
- 双击标题栏不用管：拖拽区映射成 `HTCAPTION`，Windows 原生就做了最大化/还原
  （实测确实不走我们的回调，日志里没有 toggle 记录）。
- 顺带修了持久化：最大化时不再把最大化尺寸当成用户尺寸存下来。

**测试** 15 + 20 全绿。新增 `maximize_button_reflects_window_state`（顺带给
`caption_button` 加 `.test_support()`，否则 `window.find("win-max")` 找不到）。

**关于「GPUI 最新版支持透明吗」**：不是版本问题。我们锁的这版（gpui-pre 0.3.6 /
gpui-pre-windows 0.3.9）已经有 `WindowBackgroundAppearance::{Transparent, Blurred,
MicaBackdrop, MicaAltBackdrop}`，非 Opaque 时渲染器会把后备缓冲清成 alpha 0，
Windows 后端的 `Blurred` 走 `SetWindowCompositionAttribute(accent_state = 4)`
（acrylic）。但它模糊的是窗口**背后**的内容，而视频画在**上面**那个叠加窗口里，
所以对我们的场景无效。要让 chrome 真模糊视频，得把视频窗挪到主窗口下面
（owned 窗口恒在 owner 之上，必须放弃 owner 关系自己管 z 序），属于重构级改动，
这一轮没有做，也没必要为了 32px 的模糊去换。

---

### 🧊 第七轮（已落地）：把视频窗挪到主窗下面，chrome 变真半透明

用户一句话点题：「视频窗放在主窗下听起来更像正解」。方向是对的，但**实施路径和
最初的方案几乎全被实测推翻了三次**。这一节记的是最终做法 + 那些坑，别再走一遍。

#### 结论先说

视频窗垫在主窗口**下面**，主窗口逐像素透明；画面区从主窗口的**窗口区域**里减掉，
标题带/胶囊/歌单再加回去。于是 `overlay::glass()`（`#0B0C10` @72%）真的透出画面，
按住画面拖动 / 双击最大化由**我们自己实现**的桥接处理（不是转交系统）。

#### 一、逐像素透明是支持的（原判断是错的）

之前的结论「GPUI Windows 后端（DComp + `WS_EX_NOREDIRECTIONBITMAP`）不支持窗口级
透明」**是错的**。20 行实验：`window_background = Transparent` + 根节点只画一块红方块
→ 截图整窗透出**后面那个浏览器窗口**，只剩红方块和 DWM 圆角。三个源码事实支撑：

1. 合成 swapchain 的 alpha 模式是 `DXGI_ALPHA_MODE_PREMULTIPLIED`，不是 `IGNORE`；
2. `background_appearance != Opaque` 时 `pre_draw` 把后备缓冲清成 `[0,0,0,0]`；
3. 混合状态是 `SrcBlend = SRC_ALPHA / DestBlend = INV_SRC_ALPHA / SrcBlendAlpha = ONE`
   ——半透明图形写进清成 0 的缓冲后，正好是 DWM 要的预乘形式。

#### 二、挡住第一版的是窗口表面那层底色

只把根节点改成透明，截图仍是 `#EFF1F5` 一模一样。原因是 gpui-component 的
`RootPlugin::style` 给**每个窗口的表面** refine 了一层 `.bg(tokens.background)`，
它在 `Root` 里、不在我们的 view 下面。解法（不改主题 token，toast/list 还在用它）：

```rust
// `Root` 实现了 Styled，实例样式 refine 在插件之后，所以能逐窗覆盖
root.style().refine(&StyleRefinement::default().bg(transparent_black()));
```

必须在 `render` 里做一次（`PlayerView::new` 那会儿 `Root` 还没挂上窗口，取不到；
无头测试窗口也没有 `Root`，要 `window.root::<Root>().flatten()` 守卫）。

#### 三、真正的拦路虎：被完全盖住的窗口会**停止出帧**

透明做完，画面仍然是黑的。三次实测（同一个 mpv、同一份文件）：

- 视频窗垫在主窗下面、主窗整块盖着它 → 空白，`PrintWindow` 取内容也是黑的；
- 把视频窗挪到没被盖住的地方 → 画面立刻正常；
- 在主窗口的窗口区域上挖个 300×300 的洞 → 画面正好出现在那个洞里。

**系统只认窗口矩形/区域，不认逐像素透明度**：它判定视频窗 100% 被遮挡，mpv 那边
的呈现就停在那儿不出帧（`--d3d11-flip=no` 也没用）。所以主窗口必须把画面区从
窗口区域里减掉：

```text
region = 整窗矩形 − 画面区（stage） + chrome 矩形（标题带/胶囊/歌单，RGN_OR 回去）
```

- 非客户区的边框留在区域里 → 四边拖动缩放、系统边框照常；
- chrome 必须加回去，否则自己也被剪掉（看不到，而且点不到——区域同时裁剪输入）；
- `CreateRoundRectRgn` 收的是椭圆**直径**不是半径；区域坐标是**物理像素、相对窗口矩形**，
  所以要用 `ClientToScreen − GetWindowRect` 的偏移换算，逻辑坐标乘 scale；
- **去重键不含窗口位置**（区域坐标是相对窗口矩形的，纯移动不用重设，
  带上位置会让拖动/贴边时每帧 `SetWindowRgn` → 整窗重绘 + 画面抖），尺寸变化由
  chrome 自身坐标体现，scale 单独记一笔（跨屏换 DPI）。

#### 四、视频窗铺**外框**，不铺客户区

第一版按客户区定位，结果左边和底部透出桌面/别的窗口文字——因为边框那一圈仍留在
主窗口的区域里，而我们不在那儿画任何东西。改成铺满 `GetWindowRect`（外框），
边框那一圈下面就有视频窗的黑底兜着。

#### 五、输入桥被推翻重做（三个坑）

画面区不在主窗口区域里了，点在上面命中的是视频窗，所以三件事得自己接：

| 坑 | 现象 | 正解 |
|---|---|---|
| 给叠加窗加 `WS_EX_TRANSPARENT`（想"命中穿透"） | **整块窗口不再被合成**，帧还在、看不见 | 不要加，注释里写着别加 |
| 把舞台挂 `WindowControlArea::Drag`（想白拿原生拖动） | 舞台压根不在主窗口区域里，GPUI 收不到 | 拖动在叠加窗 wndproc 里自己实现 |
| `ReleaseCapture()` + `SendMessageW(main, WM_NCLBUTTONDOWN, HTCAPTION)`（想白拿贴边吸附） | 拖动只跟上头一两步就停（GPUI 的 wndproc 吃掉 `WM_MOUSEMOVE`，系统模态循环拿不到位移）；紧接着的双击被当成"继续拖动"，窗口挪一段而不是最大化 | 自己记起点、自己 `SetWindowPos`，叠加窗同步粘住 |
| 双击转交 `WM_NCLBUTTONDBLCLK` | 窗口撑到工作区，但 `WS_MAXIMIZE` 位丢了 → `IsZoomed` 为假，标题带图标和"再点还原"全乱 | 直接调 `window_actions::set_zoomed(main, !is_zoomed)` |
| 双击消息根本不出现 | 窗口类没设 `CS_DBLCLKS`，系统只连发两条 `WM_LBUTTONDOWN` | 类样式加 `CS_DBLCLKS` |

文件拖放仍走 `DragAcceptFiles` + `WM_DROPFILES` → 队列 → tick（OLE 沿**父链**找投放
目标，叠加窗会截断；`WS_EX_TRANSPARENT` 不改变 `WindowFromPoint`，别试）。
`WM_ERASEBKGND` 里铺黑：第一帧到达前不铺黑会闪一下桌面。

#### 六、标题带也要玻璃

画面铺满整窗之后，标题带直接压在画面上。16:9 片源顶部常有黑边，看着"还好"，
换成顶到边的画面曲名就读不出来了 → 视频模式下给标题带铺 `overlay::glass()`，
和胶囊同一套口径。

#### 七、验收

`scripts/verify_floating_chrome.py`（真机）+ `ui_tests::chrome_region_covers_capsule_and_panel`
（几何）双层守着：

- 画面中心必须是彩条（区域减法生效才会出帧）；
- `GetWindowRgn` 类型 = COMPLEXREGION 且外接框仍是整窗；
- 视频窗与主窗 `GetWindowRect` **逐像素相同**；
- 胶囊里压在两个色条上的两点色差 > 25（>`72%` 玻璃留住色相，实心黑板会抹平）；
- 展开歌单后 `PtInRegion(面板中心)` 为真、面板取样是浅底；
- 按住画面拖动：位移非零 + 拖动途中画面中心仍是彩条 + 松手后外框仍对齐；
- 双击画面：`IsZoomed` 变真，再双击变假，两次都对齐。

#### 已知代价 / 不做的事

- **仍然没有区域级 backdrop blur**。系统的 acrylic 只能整窗，要「只糊 chrome 底下
  那块」得自己拿视频纹理做区域级 backdrop filter——视频纹理在 mpv 进程/D3D 里，
  不在 GPUI 手里，属「GPUI 自己合成视频」的量级。
- **没有 Aero Snap / 拖最大化窗口还原**：自己实现的拖动省掉了系统的贴边吸附。
  最大化状态下按住画面拖动会先把窗口还原再跟着走（系统 `WM_NCLBUTTONDOWN/HTCAPTION`
  那套是"拖出来就还原"，我们这版是"还原后继续拖"，位置会跳一下）。
- 窗口**缩放/最大化动画期间**会逐帧重设区域，极端情况下闪一帧。
- 区域与 chrome 是两份几何（一份画、一份算），只能靠上面那条单测盯着它们对齐。


---

### 📐 第八轮：窗体大小 = 内容大小（2026-10-18）

用户第三条要求：「窗口播放视频时，窗体大小等于内容大小，overlay 展示于视频上方」。
后半句第七轮已经做到了（chrome 浮在画面上），这一轮补前半句，并顺带把
「双击画面最大化」修到真的可靠。

#### 一、尺寸从哪来：`dwidth/dheight`，不是 `width/height`

mpv 的 `width/height` 是**编码尺寸**，`dwidth/dheight` 才是**显示尺寸**（已按
`pixel_aspect` 和旋转校正）。用前者碰到变形宽银幕（anamorphic）就会拉扁。所以
`VideoStatus` 多两个字段 `width/height`，订阅 id 5/6：

```rust
(5, "dwidth"), (6, "dheight")
```

两个细节：解码出第一帧之前 mpv 报的是 `null`，要落成 0（表示"还不知道"）；
`start-file` 时清 0，否则换片时窗口会先按**上一部**的尺寸调一次，闪一下。
`dwidth/dheight` 都是正整数字面量，`dimension()` 顺手把 `> 0` 和 `u32` 上界一起挡掉。

**还有一次假报**：1280×720 的片子起播时先报 `960×540`（正好 0.75 倍），隔一帧才报真值。
第一版"一报到就调窗口"，于是窗口先被调到 960×540 的档位、再跳回 1:1——肉眼是明显
的两次跳变。`fit_seen` 就是为了这个：同一个尺寸**连着出现两次**才当真（100ms tick
两帧 ≈ 200ms，用户感知不到延迟）。会话日志里现在只剩一行：

```text
fit_to_video: video=1280x720 client=1560x1020 -> outer=1302x731@(129,145)
```

#### 二、`plan_fit`：纯函数，好测

窗口几何的全部策略收进一个不碰 Win32 的纯函数
（`window_actions::plan_fit(FitScene) -> Option<FitPlan>`），三条规则按顺序：

1. 画得下就 **1:1**（客户区正好等于画面像素，一个像素都不缩放）；
2. 屏幕放不下就等比缩到工作区（留 12 逻辑像素边）；
3. 缩完比窗口最小尺寸还小就**反过来放大**（否则画面周围一圈黑边）。

位置以**当前窗口中心**为锚点，再夹回 `rcWork`——既不会因为调尺寸而跳到屏幕左上，
也不会跑出屏幕。外框比客户区大的那一圈（边框）是实测出来的
（`GetWindowRect − GetClientRect`），不是拍的常量。

单测 5 条：1:1 / 缩进工作区 / 放大到最小尺寸 / 尺寸未知不处理 / 已经对上了要报"没动"。

#### 三、`SetWindowPos` 不能在 `Context::update` 里调

第一版直接在 tick 里调，结果：窗口变了、界面还画在旧尺寸上，hitbox 全错位。
`SetWindowPos` 会**同步**派发 `WM_SIZE` → GPUI 立刻重新布局，而此刻 `App` 正被本轮
更新可变借用，回调里 `handle.update(&mut cx, …)` 直接失败（`log_err`），布局就停在
旧尺寸——和第六轮 `ShowWindow` 那个坑是同一个病。

正解是丢到**下一轮消息循环**：

```rust
cx.spawn_in(window, async move |_this, _cx| {
    window_actions::fit_to_video(main, (width, height));
})
.detach();
```

前台任务由平台投递、在借用释放后轮询（`update_window_erased` 会把窗口取出来再放回），
所以 `SetWindowPos` 的同步回调拿到借用权是安全的。

#### 四、每部片子只调一次，且不落盘

- `fit_done: Option<(u32,u32)>` 记住"已经按这个尺寸调过"，`start_video` 里清空。
  用户之后自己拉窗口不会被弹回去（那条反馈踩过）。
- 最大化 / 影院 / 全屏时**不调**：那三种尺寸归系统管。此时 `fit_done` 还是 `None`，
  退出后下一帧自然补上。
- 窗口尺寸落盘（`settings.window_width/height`）在**视频模式下一律跳过**：那份尺寸是
  按画面算出来的，存下来会让"音频模式"下次启动跟着某部片子的比例走。

#### 五、双击画面上不来？自己判定双击

第七轮给窗口类加了 `CS_DBLCLKS`，仍收不到 `WM_LBUTTONDBLCLK`——**系统不会给一个
从未被激活过的 `WS_EX_NOACTIVATE` 窗口发双击消息**（合成输入下只来两条
`WM_LBUTTONDOWN`，`SMP_OVERLAY_TRACE` 里看得清清楚楚）。既然双击的语义就是我们自己
定的，那就自己判：记上一次按下的时间与坐标，`GetDoubleClickTime()` 内的
`SM_CXDOUBLECLK`/`SM_CYDOUBLECLK` 范围里再来一次就算双击，命中后清空、先
`end_drag` 再 `set_zoomed`。真的 `WM_LBUTTONDBLCLK` 来了也照旧处理（两条路都留着）。

顺带把拖动放开：以前 `IsZoomed` 时按下直接无视，现在只在最小化时无视——最大化
状态下按住画面也能拖（先还原再跟随，见上一节「已知代价」）。

#### 六、z 序每帧查一次，别每帧都设

`ShowWindow` 会把它提到最前、别的进程也能把主窗口提上来，所以 `set_rect` 里不再
"位置没变就 return"，而是每次 `win::ensure_below`：只 `GetWindow(main, GW_HWNDNEXT)`
比对一下，不对了才 `SetWindowPos`。查一次是用户态调用，比每 100ms 打一次
`SetWindowPos`（整窗重排 + 画面抖）便宜得多。

#### 七、验收，以及一个**差点误判**的坑

`scripts/verify_floating_chrome.py` 新增第 0 步：

- 起播后客户区必须是 `1280x720`（样片像素，1:1），差 ≤2px；
- 画面**右缘**（`w-4, h/2`）必须是彩条——有 letterbox 或比例不对那里就是黑的/白的；
- 同时打印 `视频窗是否已跟上新几何`。

第一次跑报 `FAIL: 画面右缘是 (255, 255, 255)`，以为是主窗口的玻璃把画面洗白了。
二分排查（隐藏视频窗再截图）证明**画面干净得很**：根因是**取样太早**——窗口刚被
改到 1280×720，叠加窗的几何已经到位，但 mpv 还没按新尺寸重配 VO，那一瞬间拍到的
是「画面还按旧尺寸画，右边一条露出桌面/别的窗口」。改成"对齐 + 最多 3 秒重试取样"
之后稳定通过。教训：**这类取样必须等到 VO 重配完成，否则会把瞬态当成设计缺陷。**

真机最终 11 条全 OK，其中和这一轮直接相关的：

```text
OK: 起播即把窗口调成画面尺寸 (1280, 720)（1:1），画面铺满不留黑边
OK: 双击画面最大化到 3840x2088，画面仍铺满 (13, 19, 254)（IsZoomed=True）
OK: 再双击还原回 1800x1144（IsZoomed=False）
ALL OK
```

#### 已知代价

- 画面尺寸变了（换片、同一部片子的分辨率切换）才重调；用户拖动窗口改过的尺寸不记忆
  （视频模式下不落盘），退出视频模式后回到音频模式自己那份尺寸。
- 竖屏/超宽画面会按工作区夹，贴边留 12 逻辑像素——不做"贴满整屏"。

#### 补：视频模式的圆角（同日追加）

用户接着提「窗口播放时，加上圆角」。做法只有一句
`DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE=33, DWMWCP_ROUND=2)`，
但**两块窗口都得设**：

| 角 | 谁负责 |
|---|---|
| 左上 / 右上 | 主窗口（标题带压在画面上） |
| 左下 / 右下 | **下面的那块视频窗**（主窗口在这一带的区域只有悬浮胶囊） |

只设主窗口的话，下面两个角还是方的；只设视频窗的话，上面两个角被标题带的方角盖住。
两边的半径都由 DWM 算，所以形状天然一致。

- 不用 `SetWindowRgn` 切圆角：区域是 1 bit 的，必有锯齿；DWM 这条属性按 DPI 算半径 +
  抗锯齿，和系统其它窗口一个样子；
- 进入视频模式设 `ROUND`、退出设 `DONOTROUND`（音频模式保持原来的直角），
  切换的判断放在每帧都会跑的 `sync_overlay_rect` 里，只在状态变化时调一次；
- 最大化时系统自动不圆，不用特殊处理；
- Windows 10 上这条属性不存在（返回 `E_INVALIDARG`），静默忽略即可，**不要**为此记日志。

验收里用 `DwmGetWindowAttribute` 读回来两条属性（都是 2 才算过）——像素判据不稳
（角外是桌面，颜色不可控），顺手打一行"角上像素 / 往里 8px 像素"供人眼复核：

```text
圆角：主窗=2 视频窗=2（2=ROUND）  角上像素=(3, 92, 163) 往里 8px=(61, 0, 0)
OK: 视频模式两块窗口都设了圆角（角上像素 (3, 92, 163)，不是画面本体）
```

另外 `scripts/shot.py` 加了 `--margin <逻辑像素>`：圆角和 DWM 阴影都在客户区**之外**，
只截客户区是看不见它们的（截图里会是一条直边）。


---

### 🖥 第九轮：全屏播放的四个出口 + 顶栏真的铺满（2026-10-18）

用户报了一组四连（原文）：

> 双击时，不是全屏播放。全屏播放后，最大化按钮回不到窗口。顶栏没有铺满宽度。
> 全屏后，esc不能返回窗口模式。

#### 1. 双击 = 全屏播放（不是最大化）

`WM_LBUTTONDBLCLK` 不能指望：叠加窗是 `WS_EX_NOACTIVATE`，系统对"没被激活的窗口"
只连发两条 `WM_LBUTTONDOWN`。所以双击**自己判**——记上次按下时刻 + 位置，按
`GetDoubleClickTime()` / `SM_CXDOUBLECLK`(36) / `SM_CYDOUBLECLK`(37) 比对。

坐标必须取**消息 lparam**（`ClientToScreen` 换算），不能用 `GetCursorPos()`：
合成的 `PostMessage` 输入下真实光标在别处，用它判的话两次点击永远对不上（实测
日志里两条 `lbuttondown` 的坐标差了半个屏幕，双击永远不成立）。

wndproc 里没有 `App`/`Window`，所以只把事件排进队列（`OverlayEvent::ToggleCinema`
→ `static Mutex<Vec<_>>`），由 100ms 的 tick 取出来调 `PlayerView::set_cinema`——
和 `take_dropped()` 同一套路。

`set_cinema(on)` 是唯一入口：`on && video_active` 夹一下、`video_overlay::set_cinema`
（全屏时叠加窗不再拖窗体）、和 `window.is_fullscreen()` 对齐、记一行
`cinema -> {on} (fullscreen=…, video_active=…)`。

#### 2. 全屏里的四个出口

`Esc` / 再双击 / 右上角「最大化」/ 胶囊的 `exit-cinema`，全走 `set_cinema(false)`。

`Esc` 走 `App::intercept_keystrokes`（跑在派发**之前**），不是 `bind_keys` + `on_action`：
没有焦点元素时 GPUI 把按键派发给 dispatch 树的**根**，而那是 gpui-component 的
`Root`，在我们视图之外——实测 `dispatch_keystroke` 直接返回 `false`，影院状态不动。

「最大化」按钮在全屏时必须先退影院：GPUI 全屏时已经把 `WS_MAXIMIZEBOX/WS_CAPTION`
摘掉了，这时再 `SW_MAXIMIZE` 只会卡在错乱态（窗口大了、界面还画在旧尺寸上）。

#### 3. 顶栏没铺满宽度 = 客户区没铺满窗口

自绘标题栏的窗口有一圈**隐形外框**（实测 1.5× 缩放下外框比客户区宽 22、高 11，
即每边 7 逻辑像素）。叠加窗按**外框**铺、GPUI 只画**客户区**，于是标题带左右各缺
一条，露出的是没被压暗的画面。

解法是接管 `WM_NCCALCSIZE`（子类化窗口过程，先调 GPUI 的再改）：

- `wParam != 0` 时窗口矩形在 `rgrc[1]`，`wParam == 0` 时就是 `lparam` 那个 `RECT`
  的**初值**（`CallWindowProcW` 会就地把这块改成客户区，所以要先抄一份）；
- **不能**用 `GetWindowRect` 取窗口矩形：这一刻窗口矩形正在变（尤其进出全屏），
  读到的还是旧值——实测表现为「窗口已经缩回 1280×731，客户区还报 3840×2010」；
- 顶边不碰：GPUI 在最大化时会把 `top` 往下让一条边框厚度，那是它刻意做的。

铺满之后那圈边框变成客户区，`DefWindowProc` 不再往里认边框 → 自己补缩放热区
（`WM_NCHITTEST` 返回 `HTLEFT/HTRIGHT/HTBOTTOM/…`，6 逻辑像素；最大化/最小化/无
`WS_THICKFRAME`（全屏）时跳过）。

**副作用一**：GPUI 在窗口创建时把边框厚度记进 `border_offset`，并拿它算
`ptMinTrackSize`（最小客户区 + 厚度）。720p 片子正好等于最小高度 480 逻辑像素 ×
DPI，于是被顶成 `720 + 11 = 731`——「窗口 = 内容」就差这 11 像素。所以拦
`WM_GETMINMAXINFO`，把创建时量到的厚度减掉。

**副作用二**：`handle_calc_client_size` 只在 `wParam != 0` 时被 GPUI 处理，单纯改
尺寸那一路仍退回 `DefWindowProc`（客户区 = 窗口 − 边框），于是每次改尺寸都要等
tick 里的自愈再补一次 `SetWindowPos`（画面会抖一下）。上面两条路都改之后就没有中间态了。

#### 4. 最费劲的一条：进出全屏时 GPUI 会丢掉自己的尺寸通知

现象：双击进全屏后，画面铺满整屏，但顶栏只在左上角画了 853×34 一小块，右上角的
窗口按钮**根本不存在**——点「最大化」没反应、点 `Esc` 反而正常（键盘不走命中测试）。

根因：`WM_SIZE` 是在 `SetWindowPos` 里**同步**派发的，此刻 App 正被本轮 `update`
借用，GPUI resize 回调里的 `handle.update()` 拿不到借用权，`log_err` 之后静默失败。
于是 `window.bounds()`（平台侧 `logical_size`）已经是 2560×1440，**布局用的
`viewport_size` 还是旧值**，而命中测试用的是上一帧的布局树 → 整片区域点不到。

试过两条都不行，记下来别再走：

- `window.refresh()`：只标脏，不碰 `viewport_size`，重绘循环里 viewport 一直是旧值；
- `window.bounds_changed(cx)`（那是平台 resize 回调里干的事）：会把
  `bounds_observers` 全跑一遍，其中就有对本视图的 `update`，而视图正在被 update
  → 直接 panic `cannot update PlayerView while it is already being updated`。

可用的是**补一条 `WM_SIZE`**：`window_actions::kick_layout` 用 `PostMessage`
（不是 `SendMessage`，必须落在消息循环里）把真实客户区尺寸投给主窗口。GPUI 的尺寸
处理只认 lparam、是幂等的，重放一次布局就按新尺寸重排。tick 里每 100ms 比一次
`viewport_size` 与 `window.bounds()`，不一致就补（300ms 限流，避免变成无效重绘）。

#### 验收脚本这轮的教训

- **双击不能靠模拟光标**：`SetCursorPos` + `mouse_event` 两次按下之间只要漏进一条
  `WM_MOUSEMOVE`，叠加窗就当成「按住画面拖窗体」把窗口挪走（实测挪了 12,-16 像素），
  双击也随之失效。改成把真实双击的那组消息
  （`DOWN → UP → DBLCLK → UP`）直接 `PostMessage` 给叠加窗。
- **改尺寸后要等区域追上**：新布局（canvas 报新的 chrome 矩形）→ tick 重建区域，
  中间那几帧胶囊还不在区域里，点击落到下面的视频窗上（`WindowFromPoint` 直接报
  `SimpleMediaPlayerVideoOverlay`）。加了 `wait_settled()`：外框对齐 + 顶栏两端/胶囊
  都回到区域里再取样。
- **顶栏暗不暗要按三通道之和比**：testsrc2 最左那根色条本来就接近黑（60,0,0），
  除以 3 之后差值只有 19，卡在阈值边缘会误判「没铺满」。改成
  `sum(带下) - sum(带内) > 12` 且 `sum(带内) < 200`。

真机最终 14 条全 OK（新增四条）：

```text
OK: 窗口态顶栏铺到窗口最左/最右一列 ((0, 0, 3), (60, 0, 0), (2, 3, 7), (0, 230, 255))
OK: 双击画面进全屏 3840x2160，画面铺满 (33, 48, 255)，顶栏也铺满
OK: Esc 从全屏回窗口 1800x1140（窗口态 1800x1140）
OK: 全屏态点「最大化」回窗口 1800x1140
ALL OK
```

#### 打包与 release 验收

debug 版跑绿之后，又用**打包出来的那个 exe**（用户真正双击的那份）重跑了一遍：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/package.ps1 -WithMpv
$env:SMP_EXE = "dist/simple-media-player-1.0.0/simple-media-player.exe"
python scripts/verify_floating_chrome.py   # → ALL OK（14 项）
```

结论：14 项全绿，与 debug 版一致（`1280x720` 1:1 起播、圆角、胶囊透画面、歌单加回区域、
窗口态顶栏铺到最左/最右一列、双击进全屏 `3840x2160`、Esc 回窗口 `1800x1140`、
全屏点最大化回窗口、拖动 `144,72` 且叠加窗跟得上）。打包产物：
`dist/simple-media-player-1.0.0/`（`simple-media-player.exe` 25.6 MB + `mpv.exe` 110.4 MB
+ `README.md` + `licenses/` + `MANIFEST.txt`），`simple-media-player.exe` 的 PE
`Subsystem = 2 (GUI)`——双击不会先弹 cmd。

顺手修掉 `scripts/package.ps1` 的一个假失败：cargo 把进度写到 stderr，而脚本头部
`$ErrorActionPreference = "Stop"` 会让原生命令的 stderr 变成终止性错误
（`NativeCommandError`）——**打包其实成功、退出码却是 1**。改成构建期间临时
`Continue`、只认 `$LASTEXITCODE`，跑完立刻还原。现在退出码 0。

#### 已知限制

- 影院模式下 chrome（标题带 + 胶囊）一直可见，没有做"IINA 那样鼠标静止 2 秒自动隐藏"；
- 全屏只有"铺满所在显示器"一种，不做"无边框覆盖多屏"。

### ⌨ 第十轮：方向键接通播放器（2026-10-18）

需求：`←`/`→` 前进后退、`↑`/`↓` 调音量。此前**一个都没接**——`intercept_keystrokes`
里只有 `escape` 和 `f11`。

#### 1. 只能在这一层接

gpui-component 0.7.3 的 `Slider` 没有 `FocusHandle`、也不参与键盘（上游硬限制），
焦点永远落不到进度条/音量条身上。所以没有"给 Slider 挂 `on_key_down`"这条路，
`PlayerView` 的 App 级拦截是唯一入口。

key 名是 GPUI 的命名键（`platform/keystroke.rs` 的 `is_named_key`）：`left` /
`right` / `up` / `down`。**写错不会报错**，只会静默匹配不上，所以解析单独抽了
`ArrowKey::parse`，并在 `ui_tests` 里用真实 `dispatch_keystroke` 盯着。

#### 2. 步长

| 键 | 动作 | 理由 |
|---|---|---|
| `←` / `→` | ±5 秒 | 走带上的 ±10 秒按钮是粗调，方向键微调（mpv / IINA 同款） |
| `↑` / `↓` | ±5% 音量 | 20 档；1% 要按到手酸，10% 一按就过头 |

#### 3. 三个必须一起改的地方

- **音量要写 `SliderState`**：`volume_pct` 只是镜像，拇指在 `SliderState` 里。
  只改镜像 = 声音变了、圆点不动。而 `set_value` 不发 `SliderEvent::Change`，
  所以引擎 + 持久化还得自己调一次 `set_volume_pct`。
- **顶到 0/100 就别再写盘**：按住不放会以 ~30Hz 打 `settings.json`。
- **播完后 `seek` 是空操作**：`AudioEngine::seek` 在 `Stopped` 直接返回 `false`
  （sink 空了，没有可 seek 的东西），用户连按右键到结尾再按左键就是"没反应"。
  这条路径改走新增的 `AudioEngine::play_at(path, secs)`（起播 + `try_seek`）。
  视频例外：播完 tick 会 `stop()` + 退出视频模式，此刻用音频引擎重起 mp4 只会解码
  失败，所以先判 `is_video_file` 直接返回——视频的"播完往回跳"这一版没做。

#### 4. 验收

`scripts/verify_floating_chrome.py` 新增第 9 项，判据落 **mpv 的 IPC trace**
（`SMP_MPV_TRACE`）：界面动没动不算数（可能只是 tick 的乐观更新），必须看到
`{"command":["seek",…]}` 和 `{"command":["set_property","volume",…]}` 真的发出去。

真机实测（40s 彩条片，投递 `WM_KEYDOWN`）：

```text
seek 目标（秒）： [8.92, 14.04, 19.29, 14.96]      ← 右键×3 后左键×1
volume 命令：    [75.0, 80.0, 85.0, 80.0]          ← 默认 70 起
settings.volume = 0.8
```

验收脚本 `ALL OK` 扩到 16 项；测试 `30（app）+ 17（core）` 全过，新增
`arrow_keys_seek_and_adjust_volume`（UI）与 `play_at_starts_near_requested_position`
（core，真音频设备）。

#### 已知限制

- 视频播完不能靠方向键倒回去（见上一节第 3 条）；
- 方向键不分长短按：没有"按住加速"（连续按即可，等价于 mpv 的连击）。


### 🎛 第十一轮：控制栏改版 + `ⓘ` 视频信息卡（2026-10-18）

需求：对齐设计稿的新控制栏，并把它的功能全实现（本轮范围：控制栏改版 + `ⓘ` OSD；
首屏三态、设置窗、播放列表改版留到后面）。设计稿没有可用的 Ardot 连接器，
所以按项目备忘里记录的设计参数落地。

#### 1. 胶囊从「铺满减去两侧内缩」改成「固定 760×92 居中」

以前 `w_full` + `px(24)`，窗口一宽就成了一条黑条；设计稿里它是一条窄带、两侧
留出画面。改成固定宽 + `max_w_full`（窄窗口不会溢出）+ `justify_center`。
高度仍是内容撑开（`12 + 16 + 8 + 44 + 12 = 92`），写死会在换字号时错位。

#### 2. 进度行改成「时间夹滑块」

`42:17 │ 轨道 596 │ -1:26:27`。左右两段时间各 `flex_1` 并贴向轨道那一侧，轨道固定
596。比原来的"轨道一行、时间一行"少 16px 高度，也不用上下跳着看。超过一小时时
`fmt_time` 出 `h:mm:ss`，否则 `1:26:27` 会显示成 `86:27`。

#### 3. `ⓘ` 视频信息卡（240×180）

数据全部来自 mpv 的 `observe_property`（新增 13 个属性：`video-codec` / `width` /
`height` / `container-fps` / `video-bitrate` / `audio-bitrate` / `video-params/primaries`
/ `video-params/gamma` / `audio-codec-name` / `audio-params/channel-count` /
`audio-params/samplerate` / `sid` / `aid`）。三条经验：

- **卡片压在画面上 → 必须进 `chrome_rects`**。漏了就是"节点在、看不见、点不到"，
  用户看到的是"点了 `ⓘ` 没反应"（胶囊当初同一个坑）。测试用区域块数 2→3→2 守着。
- **值要截断**：mpv 的编码名是并列别名（`h264 (High) / AVC / MPEG-4 AVC / MPEG-4`），
  整串塞进 240 宽的卡直接冲出右边（实机截图里已经压到画面上）。取第一段
  （`short_codec`）+ `min_w_0` + `truncate`。
- **没音轨说「无音轨」**，别留 `—`：`—` 是"还没读到"的语气，而"有没有音轨"是确定的。
  码率 `—` 才是真读不到（mpv 对合成 mp4 报 `unavailable`）。

#### 4. 字幕 / 音轨键：`cycle` 没有返回值，只能靠"比对观测值"确认

`cycle sid` / `cycle aid` 是 fire-and-forget，素材只有一条轨时它什么也不做，
界面上"点了没反应"和"切到了"一模一样。做法：切换前记下轨号 + 时间戳，下一轮 tick
比对 observed 值——变了报新轨，`800ms` 没变就明说「没有其他字幕轨可切」，
落在标题带胶囊上（视频模式的即时反馈统一在那里，in-app toast 会被叠加窗盖住）。

#### 5. 验收（18 项，两轮连续 `ALL OK`）

`scripts/verify_floating_chrome.py` 新增两项：

- **第 10 项 信息卡**：点 `ⓘ` 后左下那块从彩条变成深玻璃（像素判据，相对亮度差），
  两个角点必须在主窗口区域里（否则会被叠加窗盖住），再点一次收起后该点必须
  **不在**区域里（结构判据，比"变亮了"稳——换暗素材不会误判）。
- **第 11 项 字轨键**：判据取 IPC trace 里的 `cycle` 命令条数（+2）。

这一轮顺手修掉三个**验收脚本自己**的坑（它们都伪装成"应用 bug"）：

| 症状 | 真因 |
|---|---|
| 点列表没反应、歌单没展开 | 坐标按旧布局写死 `lw-123`；胶囊改成固定 760 后那一点落到胶囊外的画面上，点击被叠加窗吃掉。改成按胶囊几何算（`right_group_button`） |
| 方向键没发出 `seek`、信息卡采样是主窗底色 | 整轮跑 50s，素材只有 40s，**播完了**——叠加窗隐藏后取样全是 `#EFF1F5`。改成自动生成 120s 长片 |
| 整轮多花十几秒才结束 | `wait_settled()` 里同一个旧坐标，每次都要等满 3s 超时 |

测试 `33（app）+ 18（core）` 全过，新增 `codec_string_keeps_only_its_first_alias`。

#### 已知限制（本轮新增）

- 卡片是**实心深玻璃**而非毛玻璃：GPUI Windows 后端不支持区域级 backdrop blur，
  设计稿的"60% 黑 + 32px 模糊"只能退成 72% 深色（和胶囊同一限制）；
- 信息卡没有"记住展开状态"：每次启动都是收起的（设计稿也没画这个状态）；
- 字幕/音轨只能循环，不能选具体轨（要做需要 `track-list` + 一个弹出列表，属下一轮）。


### 🧩 第十二轮：视频内核换成进程内 libmpv（2026-10-18）

分叉点来自一个对照：IINA 把 `libmpv.2.dylib`（+44 个依赖 dylib）打进 `IINA.app/Contents/Frameworks/`，
`mpv_create()` 进程内调用、视频走 render API。同一份 mpv，IINA 选择**库**，我们此前选择**进程**。
于是先算清三条路，再动手：

| 方案 | 做法 | 代价 |
|---|---|---|
| **A（本轮采用）** | 进程内 libmpv，**保留** `--wid` 叠加窗 | 净增约 100 行；窗口合成本身的债原样留着 |
| B | A 之上换 SW render API → RGBA 帧 → GPUI `img()` | 可删掉 `video_overlay.rs` 965 行；但每帧换 `ImageId`，Win atlas 逐帧新 tile + ~8MB/帧上传，1080p 勉强、4K 不行 |
| C | D3D11 共享纹理 + DirectComposition | 要 fork `gpui-pre-windows`，开放式工程 |

选 A 的理由不在于"更好"，而在于 A 之后仍能升级到 B/C，反过来不行。

#### 落地形态

- `crates/player-core/src/libmpv.rs`（新，约 470 行）：**运行时加载**（`libloading 0.8` + 手写 ~20 个符号）
  而不是构建期链接。构建机不需要 DLL，缺库时给一条可见错误（`VideoError::LibmpvNotFound`），
  而不是进程在加载阶段被系统拒绝。DLL 来源是 `zhongfly/mpv-winbuild` 的 `mpv-dev` 包 —— 官方
  `mpv.exe` 是**静态链接**的，拆不出 DLL。
- `crates/player-core/src/video.rs`：删掉命名管道 + JSON 报文 + 子进程管理（约 400 行），换成
  「`libmpv` 客户端 → 事件线程 → `EngineEvent` → `apply_event` 纯函数 → `VideoStatus`」。
  `VideoStatus` 字段保持与旧实现一致，`player_view.rs` 的状态机一行没改。
- 对外 API 只改了一处：`SpawnOptions { libmpv: PathBuf, wid, volume, extra_args }`（原来是 `mpv_path`）。
- `scripts/fetch-libmpv.ps1`（新）：从 GitHub release 拉 `mpv-dev-x86_64-*.7z`（32MB）并用系统自带
  `tar.exe` 解出 DLL；`-CopyToExe` 顺手拷到 `target/debug`、`target/release`。
- `scripts/package.ps1 -WithLibmpv`（原 `-WithMpv`）：把 DLL 放进 exe 同目录（运行时加载**必须**能
  在 exe 旁边找到它），并带上许可文件。

#### 踩到的坑（都记进 `MEMORY.md` / `libmpv-migration.md`）

| 现象 | 真因 |
|---|---|
| `sid`/`aid` 永远不更新 | `observe_property` 的格式必须匹配属性**真实类型**；按 `INT64` 观察字符串属性会让 mpv 整条拒绝该观察 |
| `--wid` 报错 / 花屏 | `wid` 得用 `mpv_set_option` + `MPV_FORMAT_INT64`，当字符串传时 HWND 高位置 1 会解析成负数 |
| 字符串属性读一次涨一点内存 | `MPV_FORMAT_STRING` 的结果由 mpv 分配，必须 `mpv_free` |
| 退出时偶发卡死 | 顺序：set quit → `mpv_wakeup` → join 事件线程 → `mpv_terminate_destroy`；`mpv_wait_event` 用 0.05s 超时而不是 -1 死等 |

#### 验收脚本跟着改的三处

换成进程内解码后，"起播到出首帧"从 ~1s 变成 3~4s（要先初始化 hwdec/d3d11va），脚本里那些
固定 `sleep` 全部变成了竞态。三处都改成**轮询到结果**：

1. **窗口尺寸**：`wait_client_size(1280×720)` —— 原来只等"尺寸稳定"，会在起播前就返回旧尺寸；
2. **方向键 / 字幕音轨**：`wait_trace(needle, n)` —— 命令真的写进 trace 才算数；
3. **信息卡开合**：`wait_darker()` / `wait_region_clear()`。

另外两个是**判据本身**的洞：

- 音轨、字幕两个按钮只隔 40 逻辑像素，连点两次落在 `SM_CXDOUBLECLK`(36) 与双击时限(500ms)
  里会被判成双击，第二次点击不当单击处理 → 只发出 1 条 `cycle`。拉开到 0.8s；
- `wait_settled()` 以前只等"顶栏两端/胶囊在区域里"。窗口刚被改尺寸时应用还没按新的视频区
  重算区域，`GetWindowRgn` 短暂返回**整个窗口**，于是"任意点都在区域里"——歌单明明没展开也被
  判通过。现在先等**画面中心成为洞**，再用区域判断面板。

#### 结果

- `cargo test`：22（core，含真 DLL 的端到端用例）+ 33（app）全绿；
- `scripts/verify_floating_chrome.py`：**18 项 ALL OK**（debug 与打包后的 exe 都跑）；
- 发布包：`dist/simple-media-player-1.0.0.zip` **55.7 MB**（exe 25.6 MB + `libmpv-2.dll` 116.3 MB，
  压缩后）。DLL 比原来的 `mpv.exe`（110.4 MB）还大 5.9 MB——进程模型不再需要 `mpv.exe`，
  但同一份 mpv 的代码量没变。
- 顺手修掉一个**测试自身的竞态**：`find_libmpv` 的 env override 用例原来 `set_var` 改进程环境，
  和并行跑的真 DLL 用例互相污染（偶发失败）。改成 `find_libmpv_in(exe_dir, overrides, current_exe, PATH)`
  传参，不再有共享状态。

#### 已知限制

- mpv 崩了就是整个应用一起死（进程隔离消失）。IINA 也接受这个取舍；
- 叠加窗、窗口区域减法、`keep_below` 这套窗口合成的债**原样保留**——A 只换引擎，不换渲染路径；
- 想彻底删掉那 965 行 `video_overlay.rs`，只能上 B（SW render API），而 B 卡在逐帧上传。


---

### 🎨 第十三轮：按本地导出的设计稿对齐控制栏与播放列表面板（2026-10-18）

设计稿从 Ardot 导到了 `design/`：`spec.json`（数值唯一来源）+ 16 张帧（mac/win 各 8）+ 图标。
原则是**不猜圆整数**——所有尺寸都先在帧上量，再落到代码常数，最后用验收脚本量回来。

#### 量出来的数字（帧是 1280×800 画布的 2× 导出，窗口铺满画布）

| 项 | 设计稿 | 实现 | 差 |
|---|---|---|---|
| 胶囊 | 760×92，x=260，底距 24，R14，padding 20/8 | 同 | 0 |
| 播放键 | 直径 44，中心距胶囊左沿 379.75 | 44 / 379.5 | −0.25 |
| 走带图标 | 中心 −96 / −54 / 0 / +54 / +96 | −96.5 / −54.5 / 0 / +53.5 / +95.5 | ≤0.5 |
| 右组节奏 | 20 图标 + gap 20 → 中心距 40，最右距右沿 30 | 24 盒 + gap 16 → 中心距 40，最右 33.5 | +3.5（见下） |
| 进度轨道 | 283..997，y 28..32（4px），已播放 `#CBA6F7` | 284..1009，y 28..32，`#CBA6F7` | 端点 1px |
| 时间行 | 字高 8，左上 (24, 12)，11px mono | 8，(23, 11) | 1px |
| 展开态胶囊 | x=88（居中于剩余画面 936） | 88，播放键中心 467.5 | 0.25 |
| 面板 | x 936..1255（320 宽），y 48，R14 | 937..1254，y 48 | 1px |

右组那 3.5px 是**有意留的**：盒子从 20 加到 24（点击热区 24 比 20 好用），图标在盒里居中，
所以离右沿多 3px；节奏（40）和最右键贴边这两点与设计稿一致。

#### 控制栏 v3 的三个结构变化

1. **进度区从一行变两行**：时间行 16 高（`SPACE_BETWEEN`，11px mono）→ gap 4 → 轨道 4px。
   轨道因此拿到整宽 `760 − 2×20 − 2×side_pad − 4 = 714`。`side_pad` 是光学修正：图标在
   20 盒里只占 13.2（内缩 3.3），轨道贴盒子边会比图标两端各宽一截，所以收 2
   （spec 写 3，扣掉胶囊那 1px 边框）。
2. **播放键 44 + Mauve 外发光**（`BoxShadow` 0/4/18/#8839EF 35%），两侧走带键盒 28、图标 22。
3. **右组顺序按设计稿**：从右往左 全屏 / 列表 / 主题 / 音轨 / 字幕（设计稿 6 个，含画中画；
   画中画要先有独立的置顶小窗，本轮没接）。

#### 播放列表 v2

- 面板 320 宽、`top = 标题带 40 + 8`、`right 24`、`bottom 119`，R14，浅底（Latte）。
- 头 48：药丸 tab（选中 Mauve 12% 底 + Mauve 字）+ ✕；行 48 双行（序号列 22 / 标题 13 /
  `规格 · 时长` 11）；当前行 Mauve 8% 底 + 1px Mauve 35% 描边、圆角 10；尾 44：
  循环 / 随机 / 排序 · `N 个文件 · 总时长` · 添加文件 / 添加文件夹 / 移除 / 清空。
- 新增三档播放模式（`RepeatMode::{Off,All,One}`、`shuffle`、`SortOrder::{Added,Title,Duration}`），
  全部落盘；「加入顺序」靠 `added: Vec<String>` 快照 + 逐次出现计数，重复路径也不会打平。
- 「章节」页读 mpv 的 `chapter-list`（`chapters` 计数 + `chapter-list/{i}/time|title`，
  避开 `MPV_FORMAT_NODE` 的联合体）；一章都没有时说人话，不画空表。

#### 顺手修的四个真问题（都是截图量出来的，不是猜的）

| 现象 | 真因 |
|---|---|
| 页脚的循环/随机/排序在浅底面板上几乎看不见 | 复用了 `chrome_button`（浮层前景 `#D5D9E2`）。面板是主题浅底，必须跟主题色走 → 新 `panel_button(fg, accent)` |
| 列表最后一排钻到页脚下面、两层字叠在一起 | 中间那层 flex 容器只声明了 `min_h_0`（能被压扁）却没裁切，`overflow_hidden` 补上 |
| 面板的窗口区域比面板本身大一圈（右多 12、上多 2） | `chrome_rects` 里还写着最早一版面板的 `width-12` / `TITLE_BAND+6`，改成和 `render_playlist_panel` 同一组常数 |
| 视频轨入列时 `duration_secs = None` → 页脚只显示「1 个文件」 | 音频入列就能问时长，视频要等 mpv 起播才报。tick 拿到 `status.duration` 后 `set_duration` 补一次（同值不重写，避免每帧落盘） |

#### 验收脚本跟着改的三处

1. **`capsule_left()`**：展开歌单时胶囊居中于剩余画面（x=88），不是整窗居中。以前写死
   整窗居中，"再点一次收起歌单"那一下点到胶囊外面 → 面板一直开着 → 后面每个按钮坐标都偏
   344，连着报出"字幕/音轨没反应""ⓘ 没反应"一串**假失败**。现在收歌单也进了判据。
2. **`CHROME_MISSES`**：每次点击都记下 `WindowFromPoint` 的窗口类名，只要不是主窗就累加，
   判定段统一报一条 FAIL。坐标偏出胶囊时点击会落到下层视频叠加窗上，界面毫无反应而截图
   看不出区别——这一条能直接把"点了没反应"指向坐标问题。
3. **ⓘ 按结果重试**：合成输入下第一次点击偶尔被吞（同一坐标在探针里点 4 次都成功），
   改成"点完看卡片真的出来了没，没出来再点，最多 3 次"。判据没放松，三次都不动才算 bug。

`scripts/shot.py` 补了 `--real-click`：`PostMessage` 那条路（不移动光标、截图干净）对
GPUI 不总生效，要展开面板/弹卡片这种有状态的点击得用真实光标。

#### 结果

- `cargo test --workspace`：22（core）+ **37**（app，新增 4 个几何/模式/时长用例）全绿；
- `scripts/verify_floating_chrome.py`：**18 项 ALL OK**（连跑三次稳定）；
- 文档截图重拍（`docs/screenshots/chroma-video*.png`，1280×720）：新胶囊 + 面板 + 信息卡。

#### 还没做的设计项

- **画中画**：需要第二个置顶视频窗（`ICON_PIP` 已经画好，按钮故意没接）；
- **首屏三态 560×440**（最近 / 空 / 拖入中）、**设置窗 960×720**（40 标题带）、
  **音乐模式 400×780**：都是独立窗口/整页，属于下一批；
- 应用图标换成 `design/icons/chroma-256.png`；标题带那三枚窗口按钮在深色画面上应取 `#E6E9EF`。


---

### ✨ 第十四轮：按 `texture_layers` 补质感 + 控制层自动淡出（2026-10-18）

设计稿的 `spec.json` 里有一节 `texture_layers`，把「质感」拆成三层，按依赖顺序叠加：
**L1 纯样式 → L2 一次画面取色 → L3 真背景模糊**。本轮把 L1、L2 全量落地，L3 记成
架构债（它要先做 `libmpv` render API 迁移，跟 `--wid` 叠加窗互斥）。

#### L1：定向光（纯样式，一次改完）

| 项 | 设计稿 | 实现 |
|---|---|---|
| 胶囊底 | `linear-gradient(180deg, rgba(26,28,40,.60), rgba(11,12,16,.74))` | `theme::v_gradient(capsule_top(), capsule_bottom())` |
| 边框 | `1px rgba(255,255,255,.14)` | 新增 `overlay::edge()`（白 14%，比信息卡那圈 `border()` 的 10% 亮一档） |
| 内描边 | `inset 0 1px 0 rgba(255,255,255,.16)` + `inset 0 -1px 0 rgba(0,0,0,.30)` | 两个绝对定位的 1px 矩形（见下） |
| 阴影 | `0 2px 6px rgba(0,0,0,.26)` + `0 16px 40px rgba(0,0,0,.32)` | `shadow(vec![contact, ambient])` |
| 底部遮罩 | 240 高，`rgba(11,12,16,.55) → 0` | `render_vignette()`，见下 |
| 图标悬停 | `rgba(255,255,255,.10)` / R6 / active scale .92 | `ButtonCustomVariant`（见下） |
| 播放键 | 径向渐变 + `inset 0 1px 0 rgba(255,255,255,.35)` | 竖向渐变 + 内阴影高光（见下） |
| 数字 | `tabular-nums` | 时间码本来就取等宽族（`mono_font_family()`），天然等宽 |

三处 GPUI 没有对应能力，只能凑：

1. **内阴影**：`BoxShadow { inset: true }` 能画，但它跟着圆角走、只在一整块元素的内侧，
   而「上沿吃光 + 下沿压暗」是两条**不连续**的线。做法是胶囊 `.overflow_hidden()` 之后挂两个
   `absolute` 的 1px 矩形（top 用 `highlight()`、bottom 用 `shade()`）。`overflow_hidden`
   同时也是 L2 染色层要的——那层是 `size_full()` 的方角矩形，不裁就会把四个圆角涂成直角。
2. **径向渐变**：`gpui_kit::linear_gradient` 只有线性，没有 radial。播放键改成
   `play_top #A05BF5 → play_bottom #6E24D8` 的竖向渐变（正好是径向渐变的高光→暗部两端），
   再叠一条 `inset 0 1px 0 rgba(255,255,255,.35)` 的内阴影当高光点。44px 的圆上，
   这套和真径向渐变肉眼分不出——渐变方向一致、高光位置也一致。
3. **悬停色**：`ghost` 变体的悬停底色写死在 `ButtonVariant::hovered` 里，是 `theme.accent`
   （浅底主题下是一块亮 Lavender），压在深玻璃上完全不对。只有 `custom` 变体肯用调用方
   给的颜色 → `chrome_button` 改成 `ButtonCustomVariant::{color,foreground,hover,active}`，
   顺手把 `rounded(6)` 补上（设计稿 `icon_hover.radius`）。`panel_button` **不动**——
   歌单是浅底面板，那里必须跟主题色走（第十三轮踩过的坑）。

`v_gradient` 这个小工具是必需的：`linear_gradient(angle, from, to)` 只收两个
`LinearColorStop`，而 `Hsla`/`Rgba` 都**没有** `From` 实现，必须手动包
`linear_color_stop(color, pct)`。所有竖向两段渐变都走它。

#### 底部遮罩为什么必须存在，又为什么必须吃鼠标

`bottom_vignette` 在设计稿里叫「补的这层」：设计稿只有顶部遮罩，控制栏是直接压在画面上的。
亮画面上（雪景、白墙、天空）11px 时间码和 1px 图标会被糊掉，玻璃只能靠**加浓**自救
（现在是平铺 72%，设计稿要 60%）。底下先压一层 240px 的渐变，玻璃就敢薄。

它是**画在画面上**的东西 → 必须进 `chrome_rects`（否则被 mpv 叠加窗整块盖住，画了看不见，
这是第五轮/第十一轮各踩过一次的坑）。而进了区域就吃鼠标 → 窗口最下面三分之一会变成
「点哪都没反应」。所以遮罩自己补了 `on_double_click → toggle_cinema`，和画面同款手势。
它**不设** `WindowControlArea::Drag`（会让压在它上面的胶囊按钮变成非客户区）。

#### L2：从画面取色，只取色相

规则来自设计稿，并且有实测依据：**只取 HSL 的色相，饱和度固定 46%、明度固定 36%**。
不取明度的原因写在设计稿里——素材底部均值只有 `rgb(46,62,71)`，直接混进去等于把玻璃染得更黑；
固定明度才能保证暗画面也有稳定色温。

实现走 mpv 的 `screenshot-raw`：

```
mpv_command_node(["screenshot-raw", "video"])  →  mpv_node(MAP: w/h/stride/format/data)
   ↓  mpv_free_node_contents 立刻释放
RawFrame::bottom_band_mean()   // x 20..80%、y 62..100%，抽样到 ~2 万点
   ↓
libmpv::rgb_hue()              // 0..360
   ↓
theme::overlay::tint(hue, α)   // hsla(h/360, .46, .36, α)，α 上 .26 / 下 .08
```

实测这张素材报 `1280x720 stride=5120 bgr0`，底部均值 `(119,135,123)`、色相 `136°`——
胶囊上确实浮出一层很淡的薄荷色偏（青绿画面），和画面同调。

两个坑：

- **`mpv_free_node_contents` 必须调**：`screenshot-raw` 把整帧（720p ≈ 5.5MB）塞进
  `mpv_byte_array`，归属于我们给的 `mpv_node`，不释放就是每次换片漏 5MB。
- **不能刚 `loadfile` 就去抓**：抓的是「当前显示的那一帧」，那会儿还是上一部片子（或黑帧），
  染错的色相**不会自己恢复**。`schedule_frame_tint()` 延迟 400ms、失败重试 3 次 ×500ms。
  加一次取色的时机只有两个：`play_track` 起播、`seek_to` 之后（镜头变了色相就变了）。

#### L3 为什么停在账上

`L3_real_blur` 要的是区域级 backdrop blur——Win 现在这块玻璃是**主窗口的一块 div**，
它背后是 mpv 的自建叠加窗，不是 GPUI 的合成树，所以拿不到「糊窗口背后」的能力
（acrylic 只能整窗，而且糊的是窗口**背后**的桌面）。正解是 libmpv render API 把画面
画进我们自己的合成树（`mpv_render_context`），那是架构级改造，见
`.workbuddy/memory/libmpv-migration.md`。在那之前**不伪造**：不加假的模糊层骗验收。

#### 自动淡出：设计比实现难

现象是「打开视频后标题栏和控制栏都不淡出」——因为压根没写过。需求侧要回答三个问题：

1. **什么时候该淡出**：抽成纯函数 `chrome_visible_target(video, playing, busy, idle)`：
   只有「视频 + 正在播放 + 用户没在碰控件」才淡。暂停必须留着（用户停下来就是为了找控件）；
   面板 / 信息卡 / 拖放提示 / 状态胶囊 / 正在拖进度同理，那会儿他正在读东西。
2. **怎么淡**：`chrome_alpha` 每 100ms tick 走 0.25 一步，400ms 走完。**不用
   `with_animation`**——那个动画只在有帧驱动时推进，而这个界面的帧是 tick 拉起来的。
   4 步在 92px 的胶囊上已经看不出台阶。
3. **淡出之后交互归谁**：这是容易漏的另一半。只把元素画成透明、区域还留着的话，那几块
   依然是主窗口的地盘：鼠标移过去落在**看不见的按钮**上，画面也少了一块能拖动/双击的区域。
   所以 `chrome_rects()` 在 `alpha ≤ 0.01` 时返回**空 Vec**，整块让给叠加窗。

「用户还在」这条信号有**三个来源**，缺一个就会出现「动了鼠标它不回来」：

| 来源 | 为什么要单独接 |
|---|---|
| 根节点 `on_mouse_move` | 标题带、胶囊、窗口边缘这些还在主窗口区域里的地方 |
| `video_overlay` 的 `WM_MOUSEMOVE` | **画面区已经被从主窗区域里剪掉了**，那边的鼠标事件根本到不了 GPUI → 叠加窗 wndproc 写一个 `static ACTIVITY: Mutex<Option<Instant>>`，tick 用 `activity_stamp()`（take 语义，取一次就清）读走 |
| `intercept_keystrokes` | 淡掉之后按方向键，得先把它叫回来再动 |

鼠标移动**只写时间戳、不 notify**：每帧通知会让整窗跟着鼠标重绘，而淡出判据本来就是
tick 在算的（和 `drop_activity` 同一个套路）。

两个副作用要一起修：

- `set_region` 收到**空区域**会把窗口从 DWM 合成树里摘掉（`SetWindowRgn` 传空 region 的语义）。
  `chrome_rects()` 现在会合法地返回空 → 在 `set_region` 末尾用 `GetRgnBox` 判一下，
  空的话塞一个 1×1 px 进去，保证「区域为零 ≠ 窗口消失」。
- 验收脚本会**因为淡出而假失败**：脚本空闲 2.5 秒后控制层自动淡掉，`region_contains(any)`
  变 False → `panel_open()` 判 False → 点「展开歌单」那一下落到叠加窗上（被当成拖动/双击），
  后面每个按钮坐标全错。这跟应用行为无关，全是脚本的锅 → `click()` / `wait_settled()`
  开头统一 `wake()`（把光标抖到画面中心，等 0.45s 淡入），`cycle sid/aid` 那两个点击改成
  按结果重试。诊断用 `SMP_CHROME_TRACE=1` 打出来的 `[chrome] alpha/target/idle` 证实：
  `alpha=1.00 → target=0.00` 恰好在 `idle=2.5s`，鼠标一动回 `1.00`。

#### 结果

- `cargo test --workspace`：24（core）+ **39**（app，新增 `chrome_fades_only_when_a_video_is_playing_and_idle`、
  `hidden_chrome_leaves_no_window_region`）全绿；
- `scripts/verify_floating_chrome.py`：**18 项 ALL OK**（连跑三次）；
- 实测淡出后视频继续解码（两次取样左下角时间码不同）；遮罩区双击进全屏
  （`逻辑(150,410) 处窗口类 = Zed::Window`，外框 `(0,0,3840,2160)`）；
- 文档截图重拍（`docs/screenshots/chroma-video*.png`）：胶囊有渐变 + 薄荷色偏，
  底三分之一压暗，播放键是一颗球。

#### 留给下一轮

- **L3** 需要 libmpv render API（架构级）；
- 设计稿 `L1.track_hover`（轨道 4→6、滑块 10→12）没做：`gpui-kit 0.7.3` 的 `Slider`
  不给 hover 状态，得先有可替换的滑块；
- `icon_hover.active_scale .92`（按下缩到 92%）没做：`ButtonCustomVariant::active()` 收的是
  **颜色**不是缩放，上游没有按下缩放这个口子；要么等版本，要么自己画按钮。


### 🎞 第十五轮：mpv 软件渲染出帧，删掉 `--wid` 叠加窗（2026-10-18）

上一轮的结尾写着「**L3 需要 libmpv render API（架构级）**」。本轮就是这一刀：不再让 mpv 往另一个
窗口里画，而是把帧取回进程、塞进 GPUI 的合成树。

#### 为什么必须换掉 `--wid`

`--wid` 的本质是「把画面交给另一个顶层窗口」，GPUI 永远拿不到那些像素，于是：

- 玻璃只能**平铺 72%** 自救（设计稿要 60% + backdrop blur），L3 只能伪造；
- 主窗口必须逐像素透明、再把自己的窗口区域改成「整窗 − 画面 + chrome」，否则被完全遮挡的窗口
  **不出帧**（系统只看矩形不看 alpha）；
- chrome 得 `RGN_OR` 加回区域，不然它自己被剪掉（区域同时裁渲染和命中测试）；
- z 序每帧对账、鼠标事件在叠加窗 wndproc 里兜（`SetForegroundWindow`、自己判双击、自己拖动）、
  拖放要两条接收路径；
- 顺手的两个雷：区域为空会把窗口从 DWM 合成树里摘掉、`WS_EX_TRANSPARENT` 会让整窗不被合成。

#### 三个 API 事实（决定实现形状）

1. `MPV_RENDER_PARAM_API_TYPE` 的 **data 就是 `char*` 本身**，不是指向指针的指针；参数 ID：
   `SW_SIZE=17`（`int[2]`）、`SW_FORMAT=18`（`char*`，`"bgr0"`）、`SW_STRIDE=19`（`size_t*`）、
   `SW_POINTER=20`；参数数组以 `type == 0` 结尾。
2. **必须 `vo=libmpv`**：mpv 默认的 vo 是 `gpu`（要一个真窗口），没有窗口时它自己换了 vo，
   渲染上下文就只清一次黑、update 回调也只来一次。再加 `video-timing-offset=0`——否则
   `render()` 会等到显示时刻才返回，把 UI 线程堵在那里。
3. 所有 `mpv_render_context_*` 只能在**创建它的线程**上调；update 回调跑在 mpv 自己的线程上、
   **禁止在里面调 mpv API** → 只 `try_send` 一个信号，真正的取帧由主线程做。

#### 帧怎么变成屏幕上的像素

| 环节 | 做法 |
|---|---|
| 通知 | `async_channel::bounded(1)` + `try_send`：天然合并成「有帧了」，回调不会积压 |
| 取帧 | `cx.spawn_in` 前后台任务 `recv().await` → `pump_frame`（随后 `try_recv` 排空） |
| 尺寸 | 纯函数 `render::fit_render_size`：contain 适配、**只缩不放**、上限 2560×1440 |
| 格式 | `bgr0`，**第 4 字节是垃圾，必须补 255**（否则画面全透明） |
| 上传 | `RenderImage::new(vec![Frame::new(RgbaImage)])`（字节按 BGRA 解释）→ `ImageSource::Render` → `gpui_kit::img(...).object_fit(Contain).size_full()` |
| 回收 | 展示后 `window.drop_image()`，留 2 帧退役队列（正在显示的那张不能释放） |
| L3 | 同一帧降到 **48×20** 存成第二张图，胶囊里 `object_fit(Fill)` 铺满（GPU 双线性 ≈ 大半径 box blur） |

上限 2560×1440 不是审美选择：`RenderImage` 每帧都是**新的 `ImageId`**，Windows 的图集按
`(image_id, frame_index)` 分配 tile（从 1024² 起、最大 16384²），1080p 勉强、4K 直接把图集吃满。

#### 删掉/迁移了什么

- `crates/app/src/video_overlay.rs`：**1010 行全删**（窗口创建、区域、z 序、双击判定、拖动、拖放）；
- 拖动：画面元素 `on_mouse_down` 记「光标 + 窗口原点」，8ms 循环 `GetCursorPos` → `SetWindowPos`
  （`SWP_NOSIZE|NOZORDER|NOACTIVATE`），`on_mouse_up` 清锚点。`Window::start_window_move()`
  在 Windows 后端**没有实现**，`WM_NCLBUTTONDOWN/HTCAPTION` 只跟上头一两步（GPUI 的 wndproc
  吃掉了后续 `WM_MOUSEMOVE`）；
- 双击：`on_double_click` → `set_cinema`。能直接收到是因为窗口现在是**正常激活**的 GPUI 窗口
  （以前叠加窗 `WS_EX_NOACTIVATE`，系统压根不发 `WM_LBUTTONDBLCLK`）。注意它只挂在
  `Stateful<Div>` 上（元素得有 `.id()`），普通 `Div` 没这个方法；
- 拖放：只剩 GPUI 的 `IDropTarget` 一条路（画面区也在主窗口里了）；
- `chrome_rects()` → `chrome_layout()`（`ui_state::ChromeLayout`）：**用途变了**，不再递给
  `SetWindowRgn`，而是给单测和验收探针用；
- 验收：`scripts/verify_floating_chrome.py` 从读 `GetWindowRgn` 改成读 `SMP_UI_STATE` 探针的 JSON
  （画面区 / chrome 矩形 / 淡出强度 / 面板态），`panel_open()` 从「像素猜」变成权威字段。

#### 两个新坑

1. **`window.bounds()` 也会假**。进出全屏那一下 GPUI 把 `WM_SIZE` 丢了 → `logical_size` 不更新，
   `bounds()` 和 `viewport_size` **一起**停在旧值，只比它俩永远发现不了。真实值只能问系统：
   新增 `window_actions::client_size()`（`GetClientRect`），判据改成
   `viewport ≠ bounds`（普通改尺寸）**或** `bounds ≠ 真实客户区`（进出全屏）。
   症状很有辨识度：全屏后画面铺满，chrome 却画在 853×580 的位置、点哪都不对。
2. **`Div::bg()` 先于子节点绘制**。胶囊的玻璃渐变铺在 `.bg()` 上，被整幅模糊图盖住 → 胶囊变成
   一块亮白板、白字读不出来。改法：渐变也是一个 `absolute` 子层，顺序 = 模糊图 → 玻璃 → 染色
   → 两条 1px 内描边 → 内容。

#### 结果

- `cargo test --workspace`：29（core，含 `render_api_delivers_opaque_pixels` 真 DLL 用例）
  + 42（app）全绿，零告警；
- `scripts/verify_floating_chrome.py`：**17 项 ALL OK**——画面出帧（青/品红色条取样）、窗口长成
  画面尺寸、玻璃透色、歌单开合、`ⓘ` 信息卡压住画面、字幕/音轨各发一条 `cycle`、双击全屏
  3840×2160 且画面铺满、全屏点「最大化」回窗口、按住画面拖动位移 144,72。

#### 留给下一轮

- **真 backdrop blur**：现在是「降采样 + 双线性」的近似，大块纯色边界会比设计稿软一点；
- **4K 点对点**：要共享纹理 / D3D11 互操作（`--wid` 时代的零拷贝现在已经换成了每帧一次 CPU 往返）；
- 设计稿剩下的 `track_hover`、`active_scale .92`、PiP 窗口（卡在 gpui-component 上游）。

---

### 🔍 第十六轮：玻璃贴图按屏幕 1:1 烘，圆角才成立（2026-10-19）

用户报「控制栏的圆角有明显问题」→ 修完仍是阶梯，追问「无法做到圆角液态玻璃吗」。上一轮只
换了裁切方式（贴图里不裁、交给外层矢量裁剪），把毛边换成了阶梯，**弧线本身还是糊的**。

#### 根因

贴图按 `PLATE_SCALE = 0.5` **逻辑像素**烘焙，1.5× DPI 下等于放大 3 倍。胶囊圆角半径 14、
折射带 14 → 贴图里只有 7 个像素，弧长方向剩 11 个采样点。设计稿的 HTML 原型一直是
`canvas = W × devicePixelRatio`（1:1 物理像素）——Rust 这边从第一版就低了一档。

#### 改法

1. **`plate_scale(job, dpi)`**：目标 `window.scale_factor()`（封顶 2.0），单块面 **22 万像素**
   封顶，超了按面积降。暗底（胶囊 / 信息卡）在 150% 下就是 1:1（1140×138）；浅底歌单乘 0.72
   再降（奶白盖到 0.87，本来就看不出来）。`BakeKey` 加 `dpi_q`（×64 定点），换显示器重烤。
2. **圆角裁回贴图里**，但用**物理 1 像素**的 alpha 覆盖率：`cover = clamp(0.5 − d·dpi, 0, 1)`。
   过渡带宽度与贴图分辨率解耦 —— 二值裁切那套灰黑毛边不会再回来，圆角也终于是矢量级的干净。
   外层 `rounded + overflow_hidden` 留作双保险。
3. **内层循环按行/列预计算**：SDF 的 `qx` 只跟 x、`qy` 只跟 y，采样基准坐标和暗底压暗的
   仿射系数同样可提。`sample` 展开三通道 + `#[inline(always)]`。三块面 17.9 → **14.5ms**，
   胶囊 **4.9ms**（release）。
4. **重烤节流统一 20ms**（原「debug 90 / release 0」）：像素数 ×9，每帧重烤吃满预算，而玻璃
   里装的是模糊倒影，慢一两帧看不出来。布局变化（开合面板、改窗口、切 DPI）不节流。
5. **`[profile.dev/testing.package.*] opt-level = 2`**：液态玻璃在 opt-level=0 下慢一个数量级，
   debug 构建的窗口会卡成幻灯片。依赖仍用 0（它们慢在启动，不在每帧）。

#### 结果

- `cargo test --workspace`：29 + 51 全绿；
- `scripts/verify_floating_chrome.py`：**18 项 ALL OK**；
- 物理像素四角 8× 放大：弧线连续，只剩 1 物理像素的边缘过渡。

---

## 4. 里程碑与时间线

| 里程碑 | 内容 | 累计工期 |
|---|---|---|
| **M0** | Stage 0~1：窗口骨架 | ~1.5 天 |
| **M1** | Stage 2~3：**可交付的音频播放器** 🟢 | ~4 天 |
| **M2** | Stage 4：视频播放 ✅ | ~7 天 |
| **M3** | Stage 5：发布版 ✅ `v1.0.0` | ~8 天 |

**关键路径**：Stage 2 的 Entity 状态同步模式决定整体代码质量 → 优先做扎实；Stage 4 是唯一可能延期的项，有明确降级预案。

---

## 5. 工程纪律（沿用既有约定）

1. **五步流程**：每阶段 plan review → modify → implement → **verify completion** → next stage
2. **验证方式**：读实际源码 + 检查 `target/` 构建产物 + `cargo run` 实测 + `git status` 确认变更范围，不信任自报告
3. **提交粒度**：每个 Stage 至少一个 commit，验收项打勾后打 tag（`m0`/`m1`/...）
4. **测试**：player-core 纯逻辑单测 + gpui-kit 无头 UI 集成测试双轨

---

## 6. 风险登记

| 风险 | 等级 | 应对 |
|---|---|---|
| mpv ↔ GPUI 渲染表面集成复杂 | 🔴 高 | Stage 4 限时 1 天攻关，超时降级为纯音频 MVP |
| GPUI Windows 端边缘问题（IME、DPI） | 🟡 中 | Stage 1 就验证中文显示与高 DPI 缩放 |
| gpui-kit 版本迭代快、API 变动 | 🟡 中 | Cargo.toml 锁定 `=0.7.x`；升级单独评估 |
| rodio seek 支持不完善（部分格式） | 🟢 低 | 解码前用 lofty 预读时长，seek 失败时重开流 |

---

## 7. 目录结构约定

```
simple-media-player/
├── Cargo.toml              # workspace 根
├── crates/
│   ├── app/                # GPUI 应用（窗口、视图、状态）
│   │   └── src/ui_tests.rs # UI 集成测试（gpui_kit::test 无头窗口）
│   └── player-core/        # 播放内核（rodio/mpv、元数据）— 无 UI 依赖
├── docs/
│   ├── gpui-kit-plan.md    # 本文档
│   └── screenshots/        # 各阶段验证截图 + README 用图
├── scripts/
│   ├── package.ps1         # release 构建 + 打包（-WithLibmpv 附带 libmpv-2.dll）
│   ├── fetch-libmpv.ps1    # 下载 mpv-dev 的 DLL/头文件，-CopyToExe 直接放到 target
│   ├── verify_floating_chrome.py  # 真机验收：17 项（画面/几何/交互，读 SMP_UI_STATE）
│   ├── shot.py             # 截图：置顶、按 DPI 定位、真实点击、可选 --no-resize
│   ├── normalize-frames.py # 设计导出图归位（读 spec.json，按 2× 裁、清透明区 RGB）
│   └── make-overview.py    # 重拼 docs/design-overview.png
└── dist/                   # 打包产物（gitignore）
```

---

*状态：Stage 0–5 全部完成，`v1.0.0` 已打包（`scripts/package.ps1`）；第十五轮把视频改成
sw-render → GPUI 纹理，`--wid` 叠加窗已删除；第十六轮把液态玻璃贴图改成按屏幕 1:1 烘焙，
圆角才终于成立。后续方向见第十五轮「留给下一轮」：真 backdrop blur、4K 共享纹理、
`track_hover` / PiP。*
