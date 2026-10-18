# Simple Media Player (Chroma) — 长期备忘

> 只留**索引 + 最容易重犯的结论**。过程在当天日志，细节在专题文件——动手前先按表打开对应文件。

| 想知道什么 | 看哪 |
|---|---|
| 色板 / 控制栏几何 / UI 样式坑 / 质感三层 / 控制层自动淡出 | `design-spec.md` |
| **L4 液态玻璃**（**默认档 edge**，三块面） | `spec.json → texture_layers.L4_liquid_glass` + `2026-10-18.md` 第 26/27 节 |
| Win 后端行为 / 窗口过程 / 全屏与尺寸 / 键盘派发 / 无头测试 | `win32-gpui.md` |
| libmpv 选型 / 线程契约 / **SW render API** / 图集 / 验收竞态 | `libmpv-migration.md` |
| 构建打包 / `shot.py` / 验收脚本坑 / 设计导出管线 / ps1 编码 | `tooling.md` |
| 逐日过程 | `2026-10-*.md` |

## 概览

- 产品名 **Chroma**（仓库/二进制仍是 `simple-media-player`）。设计稿在 Ardot（file 737444551354649，
  Catppuccin Latte + Mauve `#8839EF`；macOS = Page 1，Windows = 「Chroma · Windows」）。
- 栈：GPUI Kit 0.7.3 + gpui-component、rodio、**进程内 libmpv**（`libmpv-2.dll`），视频走
  **SW render API → BGRA → `RenderImage` → `img()`**；`--wid` 叠加窗那套（区域打洞 / z 序 / 逐像素
  透明）已在第十五轮全删。
- workspace = `crates/app`（UI bin）+ `crates/player-core`（内核，无 UI 依赖）。git 身份仓库级
  Kevin Zhang / kevin.zhang@local；用户自装 Rust + VS Build Tools，**agent 不动系统环境**。
- 五步流程：plan review → modify → implement → **verify completion** → next stage。
  验证**只认真实文件 / 产物 / git status**，不信自报告。
- 设计物全在 `design/`：`spec.json` 是**唯一数值来源**，`frames/*.png` 是 2× 窗口图。
  **开发会话读不到 Ardot 链接**，要改画布得去设计会话。
- **视频播放态只有 HTML 原型一份**：画布上 6 个视频帧已删（与 HTML 重复，改一次质感要同步 6 份）。
  `frames/` 只剩 HTML 表达不了的 10 张（音乐模式、设置、首屏三态 ×2 平台）；结构快照在
  `design/archive/video-frames-2026-10-18.jsonl`。

## 最容易重犯的几条（细节见专题文件）

- 画面已在 GPUI 合成树里：chrome 是普通子节点，浮层不必再「挖洞」才可见，但**必须进
  `chrome_layout()`**——淡出时靠它让位（透明的节点照样吃点击），验收脚本也按它取点。
- 每帧都是新 `ImageId`：送渲染的尺寸封顶 **2560×1440**，展示后 `drop_image()`、留 2 帧退役队列。
- 改尺寸 / 进出全屏后布局可能停在旧值，而且 **`bounds()` 也会假**：只能和 `GetClientRect`
  （`window_actions::client_size`）对账，命中就 `kick_layout`。
- `Div::bg()` 画在**子节点之前**；`on_double_click` 只在 `Stateful<Div>` 上；`start_window_move()`
  在 Win 后端**没有实现**。
- `SetWindowPos` / `ShowWindow` **不能**在 `Context::update` 里同步调 → 丢进 `cx.spawn_in`。
- 快捷键必须走 `App::intercept_keystrokes`（`bind_keys` + `on_action` 收不到，无焦点时派发给根）。
- 字体只取系统自带族（Win `Microsoft YaHei UI`）；浅底面板上的按钮用主题色，别用 `overlay::*`。
- 验收脚本：胶囊左沿走 `capsule_left()`、每次点击记 `WindowFromPoint` 的类名、有空闲间隔先 `wake()`。
- **玻璃层与压暗层必须分层**：折射层（canvas / 纹理）是不透明的，会整块盖掉父元素的
  `background`——染色与自适应压暗要画在它**上面**，否则等于没写。
- **浅底玻璃是反的**：浅面板（歌单）不能压暗，要铺奶白，且**边圈更实（更亮）**而不是更透
  ——一透就把深色画面透出来，看着像没擦干净的灰边。极性（dark / light）是一等参数。
- **玻璃面的位置别用 `offsetLeft`**：只对居中成立；一律 `(el.rect - win.rect) / scale`。
- **液态玻璃要进代码前先修采样**：L3 现在是「整块缩略图铺满胶囊」，**不对位**，做折射
  必须换成矩形区域对位采样；且折射要两份模糊（体部 ≈ 32 / 边圈 ≈ 7）。
- 并行会话会顺手 `git add -A`：判断「这轮改了多少」看工作区 vs HEAD，别信 `git diff` 行数。
