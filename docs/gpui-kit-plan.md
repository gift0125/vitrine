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
| 视频播放 | `libmpv`（mpv 嵌入渲染到 GPUI 窗口），作为 Stage 4 的可选增强 |
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
│  ┌─────────────┐   （Stage 4 可选）           │
│  │ VideoEngine │ ← libmpv2 + render context │
│  └─────────────┘                             │
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
serde_json = "1"          # 播放列表持久化
directories = "6"         # 配置目录定位
# Stage 4 才引入：
# libmpv2 = "3"           # 视频播放内核
```

---

## 3. 阶段划分（Stage 0 → 5）

### 🟡 Stage 0 — 环境验证（0.5 天）

**目标**：空窗口跑起来，验证工具链。

- [ ] 安装 Rust stable（1.85+），确认 MSVC 工具链（Windows）
- [ ] `cargo new simple-media-player`，添加 `gpui-kit` 依赖
- [ ] 跑通官方 hello_world 模式：`gpui_kit::application().run(...)` 打开一个窗口显示 "Hello"
- [ ] （可选）`npx skills add longbridge/gpui-kit` 给编码 Agent 装官方技能

**验收**：`cargo run` 弹出窗口，120Hz 渲染无报错；`git commit` 基线。

---

### 🟡 Stage 1 — 主窗口骨架与布局（1 天）

**目标**：播放器三栏布局落地，纯静态 UI。

- [ ] 主窗口 + 标题栏定制（参考 gpui-kit `example-dock`）
- [ ] 布局：左侧 Playlist 面板 / 右上封面区 / 底部控制栏（播放按钮、SeekBar、音量）
- [ ] 用 gpui-component 组件：`Button` / `Slider` / `List` / `Icon`（Lucide 图标）
- [ ] 明暗主题切换（`Theme` / `ActiveTheme`）
- [ ] 中文文案 + 字体渲染验证

**验收**：窗口可缩放，布局不错乱；主题切换即时生效；截图存档。

---

### 🟡 Stage 2 — 音频播放内核（1.5 天）

**目标**：能选文件、能出声。

- [ ] `player-core` crate 拆分：`AudioEngine` 封装 rodio `Sink`
- [ ] 播放控制：play / pause / stop / seek / set_volume
- [ ] 文件选择对话框（`rfd` 或 GPUI 平台 API）
- [ ] 元数据解析：lofty 读时长/标题/艺术家/封面图
- [ ] GPUI Entity 状态同步：进度通过 `cx.notify()` + 定时器（100ms）推送到 UI
- [ ] 播放结束自动切下一首（事件回调）

**验收**：播放 FLAC/MP3 正常；拖动 SeekBar 无爆音；状态栏时间显示准确；`cargo test -p player-core` 通过核心控制逻辑单测。

---

### 🟡 Stage 3 — 播放列表（1 天）

**目标**：完整歌单管理。

- [ ] `List` 组件 + 虚拟滚动（千级条目流畅）
- [ ] 添加文件/文件夹（递归扫描）、删除、清空
- [ ] 拖拽排序（gpui-base 拖拽行为）
- [ ] 双击播放、当前曲目高亮
- [ ] 持久化：退出时序列化到 `%APPDATA%/simple-media-player/playlist.json`，启动恢复
- [ ] 记住窗口尺寸与音量

**验收**：1000 条列表滚动 60fps+；重启后歌单/音量/窗口状态恢复。

---

### 🟡 Stage 4 — 视频播放（可选增强，2~3 天，风险最高）🔴

**目标**：mpv 内核嵌入播放视频。

- [ ] 引入 `libmpv2`，mpv render API 回调取帧
- [ ] 方案 A（首选）：mpv `--gpu-api=d3d11` 共享纹理 → GPUI 自定义 Element 绘制
- [ ] 方案 B（兜底）：mpv 软解帧 → `RenderImage` 每帧上传（性能差但必然能跑）
- [ ] 视频/音频模式自动切换（按扩展名）
- [ ] 全屏切换、宽高比保持

**验收**：1080p H.264 播放 CPU < 15%、不掉帧；音视频同步无漂移。
**降级预案**：若纹理共享集成受阻超过 1 天，MVP 以音频播放器交付，视频功能砍为 v2。

---

### 🟡 Stage 5 — 打磨与发布（1 天）

- [ ] UI 集成测试：`gpui_kit::test` 无头窗口驱动指针/键盘，断言播放状态、焦点、布局
- [ ] 无障碍检查（AccessKit 角色/名称）
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
| **M2** | Stage 4：视频播放 | ~7 天 |
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
