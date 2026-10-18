# design/ — Chroma 设计稿导出（只读）

设计稿是**云端 Ardot 文件**（`ardot.tencent.com/file/737444551354649`），仓库里没有、也不会有 `.ardot`。
只有设计画布会话能读它；开发会话读不到，也不需要读——**所有开发要用的东西都在这个目录里**。

> 本目录是导出物，**不要手改**。改了下次重新导出会被覆盖，而且和画布对不上。

## 怎么用

| 你要什么 | 看哪 |
|---|---|
| 尺寸 / 间距 / 颜色 / 圆角 / 字号 | `spec.json`（唯一数值来源，别从图里量） |
| 某个界面长什么样 | `frames/*.png`（2x，透明背景，**已裁到窗口内容**，图像尺寸 = 设计尺寸 ×2） |
| 应用图标 | `icons/chroma-{256,128,64,32,16}.png` |
| 一眼看全貌 | `../docs/design-overview.png`（16 帧缩略图） |
| 控制栏质感到什么程度 | `../docs/capsule-compare.png`（画布 L3 vs 真模糊）、`../docs/texture-layers.png`（L0→L3 四层，自上而下） |

`spec.json` 的结构：`tokens`（浅/深色板 + 玻璃浮层 + OSC）→ `logo` → `windows`（双平台窗口尺寸）→ `control_bar` → `playlist_panel` → `first_run` → `osd_video_info` → `frames`/`icons`（清单与节点 id）→ `known_gaps`（已知偏差，**先看这个再量图**）。

## 帧清单（16）

| 文件 | 平台 | 设计尺寸 | 说明 |
|---|---|---|---|
| `frames/mac-main-player.png` | mac | 1280×800 | 主播放窗（音乐） |
| `frames/mac-music-mode.png` | mac | 400×780 | 音乐模式 mini |
| `frames/mac-settings.png` | mac | 960×680 | 设置窗（无标题带） |
| `frames/mac-firstrun-recent.png` | mac | 560×440 | 首屏 · 最近打开 |
| `frames/mac-firstrun-empty.png` | mac | 560×440 | 首屏 · 空状态 |
| `frames/mac-firstrun-drop.png` | mac | 560×440 | 首屏 · 拖入中 |
| `frames/mac-playlist-expanded.png` | mac | 1280×800 | 播放列表展开 |
| `frames/mac-osd-video-info.png` | mac | 1280×800 | 视频信息 OSD |
| `frames/win-main-player.png` | win | 1280×800 | 主播放窗（音乐） |
| `frames/win-music-mode.png` | win | 400×780 | 音乐模式 mini |
| `frames/win-settings.png` | win | 960×720 | 设置窗（680 + 40 标题带） |
| `frames/win-firstrun-recent.png` | win | 560×440 | 首屏 · 最近打开 |
| `frames/win-firstrun-empty.png` | win | 560×440 | 首屏 · 空状态 |
| `frames/win-firstrun-drop.png` | win | 560×440 | 首屏 · 拖入中 |
| `frames/win-playlist-expanded.png` | win | 1280×800 | 播放列表展开（面板 y=48） |
| `frames/win-osd-video-info.png` | win | 1280×800 | 视频信息 OSD |

图标：`icons/chroma-256/128/64/32/16.png`（尺寸即文件名；16px 是圆角方块变体）。

## 控制栏质感：已定稿 L3

控制栏不是一块「半透明黑」。它分三层，按依赖顺序叠加（精确参数见 `spec.json` → `texture_layers`）：

| 层 | 做什么 | 实现代价 |
|---|---|---|
| **L1 方向光** | 顶部高光 + 底部暗线 + 双段投影 + 画面底部 vignette + hover 响应 + 等宽数字 | 纯样式，**现在就能做** |
| **L2 画面染色** | 从画面底部取样，**只取色相**（S 固定 46%、L 固定 36%）给玻璃染色 | 低：切歌/seek 后截一帧算一次并缓存 |
| **L3 真模糊** | `backdrop blur(56px) saturate(175%) brightness(1.05)`，底色降到 `.48` | 高：依赖 libmpv render API 进自己的合成树 |

画布上 **6 个视频帧**（主窗 / 播放列表展开 / 视频信息 OSD，mac + Win）都已按 L3 落地：控制栏是染色的渐变玻璃 + 白 18% 描边 + `BACKGROUND_BLUR 56`，窗口底部另加一层 240 高的暗角遮罩（`#0B0C10` 0 → 55%），保证 11px 小字在亮画面上仍有对比。音乐模式 / 设置 / 首屏没有视频底，不适用。

## HTML 原型

`preview/index.html` —— 单文件、**无外部依赖**（视频静帧 `assets/video-still.jpg` 与应用图标都已 base64 内联，326KB），
可以直接双击打开，也可以单独发给别人。**默认档位就是 L3。**

- 交互：进度条点击/拖拽改时间、音量条拖拽、播放/暂停切换、ⓘ 与列表键开合面板。
- 底部工具条是**逐层叠加**的质感档位，也可用 URL 参数固定：
  `?layers=`（L0 设计稿原样）· `?layers=enh`（L1 方向光）· `?layers=enh,tint`（L2 画面染色）· `?layers=enh,tint,realblur`（L3 真模糊，默认）· `?layers=noblur`（Win 现状）
  另有 `&panel=osd|playlist`。三层的具体数值见 `spec.json` 的 `texture_layers`。
- 列表 10 条与设计稿 `21:170` **逐字一致**（标题 / 元信息 / 当前播放行的三柱图标 / 底部「10 个文件 · 3:42:18」）。HTML 里的列表数据在脚本开头的 `items` 数组，改数据只改那里。
- 换底图：改 `assets/video-still.jpg` 后要重新内联进 HTML（相对路径在预览面板里会被 403）。

### HTML 是「验收基准」，不是「实现来源」

- **数值**以 `spec.json` 为准（含 `texture_layers` 三层的精确参数）；HTML 里能读到同名参数，但不要从 CSS 里抄。
- **视觉**用 HTML 当对照：实现完截个图，和 `?layers=...` 的对应档位比。
- HTML 里这些**没有对应实现**，别照搬：`backdrop-filter`（GPUI 无）、CSS 过渡曲线、字体度量、以及所有动态行为（自动隐藏、拖拽吸附）。

## 更新流程

1. 在**设计画布会话**里说「重新导出 design/」——会重跑导出并更新 `spec.json`。
2. 检查 `git diff design/`（图片会全部显示为二进制变更，属正常）。
3. 若 `spec.json` 里 `meta.exported_at` 变了但数值没变，说明只是重导出，可以不改代码。

**不要**在开发会话里试图读取 Ardot 链接：那组设计工具不在开发会话里，这是宿主按会话注入的，没有配置开关。

## 已知偏差（量图前必读）

- 帧已裁到窗口内容：**图像 = 设计尺寸 ×2**，可直接除 2 量坐标；窗口四角是透明圆角（radius 10），不是描边。
- 静态导出画不出真模糊。L3 在 PNG 里只表现为一块**均匀染色玻璃**（染色的色相是从当前素材那一帧算出来的固定值 `rgb(50,103,134)`）。看真效果开 `preview/index.html`；进代码后必须运行时取样，换片重算。
- 背景是**透明**的；某些查看器会把透明区的 RGB 残余显示成绿色（预览工具里见过），实际 alpha=0，不是设计元素。
- Win 设置窗以 `21:807`（960×720，含标题带）为准；画布上还留着一个旧版 `2:406`（960×680 无标题带），**不要用**。
- OSD 里的分辨率/码率/色彩是设计稿示例值，代码应取 mpv 实际值；`video-bitrate` 不可用时显示 `—`，不要补假数。
- `assets/chroma.png` / `assets/chroma.ico` 已按 Logo C 重建（`scripts/make-icon.py`：16 倍超采样、9 档尺寸逐档重画、不烤投影）。16px 也用同一图形等比缩——画布上的方块探索稿 `2:528` 实测更难认，**不要**拿它做 ico。
