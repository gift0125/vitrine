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
│  ┌─────────────┐   （Stage 4 ✅）             │
│  │ VideoEngine │ ← mpv.exe 子进程 + JSON IPC  │
│  └─────────────┘   + app::video_overlay 顶层窗口│
└─────────────────────────────────────────────┘
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
serde_json = "1"          # 播放列表持久化 + mpv IPC 消息
directories = "6"         # 配置目录定位
# Stage 4 引入（实际落地）：
# raw-window-handle = "0.6"  # 取 GPUI 窗口 HWND 作为叠加窗口 owner
# mpv.exe 为外部依赖（不引入 libmpv2）
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
│   ├── package.ps1         # release 构建 + 打包（-WithMpv 可附带 mpv）
│   └── verify_m2_video.py  # 冒烟工具：归位/置顶截图、点击、拖拽、叠加窗口探测
└── dist/                   # 打包产物（gitignore）
```

---

*状态：Stage 0–5 全部完成，`v1.0.0` 已打包（`scripts/package.ps1`）。
后续方向见 Stage 5「已知限制」：纹理共享浮层、键盘可用的 Slider、随包 mpv、跨平台验证。*
