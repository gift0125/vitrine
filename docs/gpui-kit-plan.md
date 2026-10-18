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

### 🟡 Stage 5 — 打磨与发布（1 天）

- [ ] UI 集成测试：`gpui_kit::test` 无头窗口驱动指针/键盘，断言播放状态、焦点、布局
- [ ] 无障碍检查（AccessKit 角色/名称）——列表行已补 `accessibility_label`（含“正在播放”后缀），其余控件待查
- [ ] 错误处理：损坏文件、解码失败、设备占用的 toast 提示
- [ ] `cargo build --release`，Windows 打包（单 exe + mpv dll 或 innosetup）
- [ ] README + 截图

**验收**：release 包在新装 Windows 机器可运行；测试套件全绿。

---

## 4. 里程碑与时间线

| 里程碑 | 内容 | 累计工期 |
|---|---|---|
| **M0** | Stage 0~1：窗口骨架 | ~1.5 天 |
| **M1** | Stage 2~3：**可交付的音频播放器** 🟢 | ~4 天 |
| **M2** | Stage 4：视频播放 ✅ | ~7 天 |
| **M3** | Stage 5：发布版 | ~8 天 |

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
│   └── player-core/        # 播放内核（rodio/mpv、元数据）— 无 UI 依赖
├── assets/                 # 图标、占位封面
├── docs/
│   └── gpui-kit-plan.md    # 本文档
└── tests/                  # UI 集成测试
```

---

*下一步：Stage 0 环境验证。确认计划后开工。*
