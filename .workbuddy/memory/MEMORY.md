# Simple Media Player (Chroma) — 项目长期备忘

## 项目概览
- 双轨：① Ardot 设计稿（Chroma 播放器 UI，file 737444551354649，Catppuccin Latte 浅色 + Mauve 强调色，macOS/Windows 两版页面并存）；② Rust 实现（GPUI Kit）。
- 实现计划：docs/gpui-kit-plan.md（2026-10-17 v1.0）。框架 GPUI Kit v0.7（Longbridge，基于 Zed GPUI）；UI 用 gpui-component 带样式层；音频 rodio；视频 libmpv（Stage 4 可选，有降级预案）。

## 工程约定
- 五步流程：plan review → modify → implement → verify completion → next stage；验证以读实际文件/构建产物/git status 为准。
- Cargo workspace：crates/app（UI bin）+ crates/player-core（播放内核 lib，无 UI 依赖）。gpui-kit 版本锁定 0.7.x。
- 基线 commit 41c13ef（2026-10-17）。git 身份本仓库级配置：Kevin Zhang / kevin.zhang@local。
- 工具链：用户自行安装 Rust + VS Build Tools（2026-10-17 确认），agent 不动系统环境。

## 设计稿要点（详见 2026-10-17 日志）
- 色板严格取 Catppuccin Latte 官方值；Mauve #8839EF 唯一强调色。
- 图标规范：播放三角占圆直径 48%（B 档），32px 以下重绘而非缩放（占比 50-54%、去描边）；图标为正圆。
- Ardot 可用字体仅 Inter / Noto 系列 / JetBrains Mono。
