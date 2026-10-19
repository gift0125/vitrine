# Chroma 设计稿要点（Ardot file 737444551354649）

> 从 MEMORY.md 拆出，避免主备忘超注入上限。

- Catppuccin Latte 官方色板，Mauve #8839EF 唯一强调色；应用表面 Latte/Mocha + 叠加层深色玻璃（`theme::overlay`）。布局 = IINA 式沉浸：自绘 34px 标题带 + 画面真铺满整窗，chrome 压画面上。
- **OSC 不跟浅色主题走**：`#0B0C10` 60% + blur 32 + 白 10% 描边，图标 `#D5D9E2`，已播放/音量 Mocha Mauve `#CBA6F7`，播放键 `#8839EF`。透明度**必须 ≤65%**（无 blur 时读成实心黑板）。
- 主播放窗（mac `2:7` / Win `2:258`）：画面 1280×800；控制栏 760×92（x260 y684，底距 24，圆角 14，padding 20/20/8/8，gap 8）；进度区两行：时间行 16 高（`42:17` 左 / `-1:26:27` 右，SPACE_BETWEEN）→ gap 4 → 轨道 714（4px、手柄 10、章节点 2），左右各 `padding 3`。**光学对齐不是布局框对齐**：图标在 20×20 盒内缩 3.3px，轨道贴 360/1080 会比图标宽 3px → 收 3 让三行端点落 55/768。右侧 6 图标（字幕/音轨/设置/画中画/列表/全屏，gap 20）；**ⓘ 在左组最左、音量之前**（右组第 7 个会超宽：7×20+6×20=260 > 240）。
- 播放列表 = 320×633 玻璃（x936 y16），默认隐藏；标签栏（播放列表/章节 + 关闭）、10 行（行高 48、双行）、底部操作条（循环/随机/排序 · 总计 · 添加/移除/清空）。展开态 mac `21:162` / Win `21:317`（旧 `20:1` 已删）：控制栏 x=88 居中于剩余画面；**Win 面板 y=48**（标题带 40 下沿），mac 仍 y16。
- 首屏三态 `21:1`（最近/继续上次 560×440、图标 68、行高 28）/ `21:58`（空态）/ `21:483`（拖入中）；Win 版 `21:587`/`21:644`/`21:710`：无红绿灯，右上 `最小化+关闭`（92×32，x462 y6）。
- **Win 设置窗 `21:807`**（960×720 = 680 + 40 标题带）：root 改 vertical → [标题带 40, 主体 horizontal]，标题带 layout:none、"设置" x16 y11、控件组 x868 y4。mac 仍无标题带 `2:139`（960×680）。
- **窗口控件图标浅色 `#E6E9EF`**（为压深画面设计），搬到浅色首屏/设置窗会看不见 → 逐 vector 改 `#4C4F69`。**`I()` 新建 frame 别传 `fill_container` 尺寸**（实测 width 变 480、height 变 fill_container，标题带吃满整窗），建完用 `U()` 给数值。
- 音量条 96 宽（`2:84` / Win `2:350`），填充 70%。**Ardot `strokeDashes` 不生效**（`U()` 回 `updated:{}` 实际仍实线），要虚线只能自己拼帧。
- **视频信息 OSD**：240 宽深玻璃卡（同 OSC 玻璃），贴左下 x24、底距控制栏 16（y=488、高 180）；标题 11/Medium + 6 行（分辨率/编码/码率/帧率/色彩/音频），行高 22、label `#A6ADC8` 左、值 `#D5D9E2` 右（SPACE_BETWEEN，值 JetBrains Mono）。演示帧 mac `21:928` / Win `21:1108`。
- 图标规范：三角占圆直径 48%，32px 以下重绘不缩放（50–54%、去描边）；图标为正圆。Ardot 字体仅 Inter / Noto 系列 / JetBrains Mono。截图素材「AI生成」水印：原图裁底（1536×1024 → 1536×960）再覆盖。

## Logo（2026-10-18 定稿：双层盘）
- 结构 = **外盘（100%）+ 内盘（80%，每边内缩 10%）+ 白三角**。三角沿用旧尺寸（宽 39%、高 48% 直径，`2:231` 62.1×76.8 在 160 里 x52.7）。
- 外盘 `#7321D9 → #5E12B8`、内盘 `#B37AFF → #8F45F5`，均为 `GRADIENT_LINEAR`，`gradientTransform:[[0,1,0],[-1,0,1]]`（顶→底）。外圈白 18% 1px 描边 + 投影 Mauve 30% 0/12/28（Win `2:501` 是 0/8/20 22%）。
- 尺寸换算：内盘 = 直径×0.8，内缩 = 直径×0.1。实测 160→0.81、256→0.82、68→0.82、32→0.88、16→0.88（小尺寸被量化，OK）。
- 16px 任务栏变体 `2:528` 是**圆角方块**（16×16 R4）：外 16 深渐变 + 内 12 @(2,2) R2（原来 12.8/1.6 只出 1px 环，太细）。
- 已落地：`2:229` mac 160 / `2:501` Win 160 / `2:545` 比例-48 / 尺寸表 `2:564` 256 · `2:567` 128 · `2:570` 64 · `2:573` 32 · `2:576` 16 · `2:558` 32-B / 品牌区 `21:6`、`21:68`、`21:494`（mac）+ `21:597`、`21:655`、`21:721`（Win 首屏三态 68）。探索帧 A/B/C 与标注已删。
- 为什么不用细环：1px 白环在 32px 就消失，16px 全无；**图标层次只能靠明度块面**（外深内浅），不能靠细线或低透明度细节。波形环试过（18 根 2.5×5~17 的白棒在 r51–68）读成边缘脏点，弃。


## 本地导出核对（2026-10-18 第十三轮，`design/` 目录）

- `design/spec.json` 是数值唯一来源；16 张帧是 **1280×800 画布的 2× 导出**（2560×1600，窗口铺满画布、
  透明留白按 alpha≥250 裁掉）。量像素时**坐标 ×2**，别再按老的 64/76 偏移裁。
- 帧上实测（逻辑像素）：播放键直径 **44**、中心距胶囊左沿 **379.75**；走带键中心 **−96/−54/0/+54/+96**；
  轨道 **283..997 @ y28..32**（4px）、已播放 `#CBA6F7`；时间墨迹字高 **8**、左上 **(24,12)**；
  右组 6 枚中心距 **40**、最右距右沿 **30**；展开态胶囊 **x=88**、面板 **936..1255 / y48**。
- 实现侧两处**有意偏差**：右组盒子 20→**24**（点击热区），图标居中所以离右沿多 3.3px。
- 面板是 Latte 浅底 → 面板上的图标/文字**一律走主题色**，`overlay::*` 那套是给深玻璃胶囊的。

> 2026-10-18 第十五轮之后：**L3 已落地**（画面进 GPUI 合成树，模糊底 = 同一帧降到 48×20 再拉伸），
> 「平 72% 玻璃」这条偏差只对信息卡/标题带成立（用的是同一张模糊图 + 72% 平铺）。


## 控制栏质感（2026-10-18 定稿 L3）

- 三层：L1 方向光（纯样式，现在就能做）→ L2 画面染色（只取 HSL hue，S 46% / L 36%，切歌算一次缓存）→ L3 真模糊（`backdrop blur 56 + saturate 175% + brightness 1.05`，底色降 `.48`，**依赖 libmpv render API 进合成树**）。
- 已定稿 **L3**，精确数值在 `design/spec.json` → `texture_layers` 与 `control_bar.capsule`；HTML 原型 `?layers=enh,tint,realblur` 即默认档。
- 画布落地：~~6 个视频帧~~ → **这 6 帧已于 2026-10-18 删除**（和 HTML 原型重复、改一次质感要同步 6 份）。参数仍然有效，但 L3 现在只在 `preview/index.html` + `spec.json → texture_layers` 里：控制栏染色玻璃 `rgba(50,103,134,.24→.06→#0B0C10 .48)` + 白 18% 描边 + `BACKGROUND_BLUR 56`；底部遮罩 `0,560 1280×240` `#0B0C10` 0→55%。结构快照 `design/archive/video-frames-2026-10-18.jsonl`。
- 音乐模式 / 设置 / 首屏没有视频底，不适用 L2/L3。
- **静态导出表达不了真模糊**：PNG 里只有均匀染色玻璃，色相是素材那一帧的固定值；代码必须运行时取样。

## 控制栏功能键顺序（2026-10-18 修正）
- 右组 6 键 = 字幕 / 音轨 / **设置（齿轮）** / 画中画 / 播放列表 / 全屏。第 3 位是**设置**不是亮度——依据 iina：悬浮控制栏那颗齿轮点开就是快捷设置（长宽比/倍速/亮度/音轨/字幕）。
- 齿轮用 Material settings 轮廓路径，自造的 6 齿路径在 20px + 1.5 描边下会糊成花瓣。
- 应用图标：`scripts/make-icon.py` 生成双层盘，每档单独画、不烤投影、16px 用圆盘（`2:528` 方块变体不采用）。

## 参考：iina 的窗体边缘处理（2026-10-18 查源码）
- **不自绘窗框**：`MainWindowController.swift` 里没有 `cornerRadius`／`borderWidth`／`layer.border`。圆角是系统窗框（Big Sur 起约 10pt），阴影是 `window.hasShadow`（只在 legacy 全屏关掉），窗口底色深色 `.black`／浅色 `.white`（填 letterbox）。
- 内容顶到最上沿：`styleMask.insert(.fullSizeContentView)` + `titlebarAppearsTransparent = true`，标题栏不是一条独立实心带。
- 标题栏 = `NSVisualEffectView(material: .titlebar, blendingMode: .withinWindow)`，**糊的是同窗口内的视频**（这正是 Win 叠加窗拿不到的那种 blur）。栏高 32/28pt（macOS 26+/旧），标题容器 +8。
- 唯一一条线：标题栏底 `NSBox.boxType = .separator`、高 1pt、色 `NSColor.titleBarBorder`。另有标题文字黑 shadow（blur 8）保可读。
- 流量灯进 `fadeableViews`，闲置时 `alphaValue = 1e-100`（**不是 isHidden**，保留命中区）；窗口模式 `titleVisibility = .hidden`。
- ~~对照我们：Chroma 窗口有一圈 1px 内描边~~ → **2026-10-18 晚上已删掉**（16 个帧 `strokes: []`，代码根节点本来就没有 border）。这圈线正是「贴图感」的来源，去掉后画面直通边缘、只靠投影托起窗口，和 iina 的口径一致。仍保留的差异：标题带独立 40/48 高、按钮常驻、无分隔线。
- iina 的「闲置淡出」是 `fadeableViews` + `alphaValue = 1e-100`（保留命中区）；我们的口径不同：**淡出后连区域一起让出去**（`chrome_rects()` 返回空），否则看不见的按钮照样吃点击。代价是底部遮罩要自己补 `on_double_click`。

## UI / 样式坑（从 MEMORY.md 拆出）

- `ListItem::confirmed()` 无视觉、默认 `primary` 与正文色重合 → 用 `list_active` / `list_active_border`。
- **字体必须系统自带**：Inter / Noto Sans SC 在 Win 不预装。`theme::ui_font_family()`（Win
  `Microsoft YaHei UI` / mac `PingFang SC` / 其他 `Noto Sans SC`）+ `mono_font_family()`；
  `ThemeConfig.font_family` 铺整棵树（`root.rs`）。
- 换肤：`ThemeConfig`(#\[serde(default)\]) + `Theme::update` + `apply_config`。
  **`ThemeConfigColors` 的 blue/red/green/cyan/magenta/yellow(±`_light`) 是私有字段** → 只能
  `default()` 后逐字段赋值。别在同一 closure 里既换 mode 又换 config。
- 布局铁律：一组控件居中，左右两栏必须**等宽**（都 `flex_1`），否则 `justify_between` 把中间顶偏。
- **控制栏 v3 几何（数字来自 `design/spec.json`，改任一条同步 `verify_floating_chrome.py` 的常数）**：
  胶囊 **760×92**、距下 24、圆角 14、padding **20/8**；进度区**两行**（时间行 16 → gap 4 → 轨道 4px
  714 宽，`PROGRESS_SIDE_PAD=2` = spec 3 扣掉 1px 边框）；走带行 44，播放键 **44 圆**（Mauve 发光
  0/4/18 35%）、走带键盒 28/图标 22，中心 −96/−54/0/+54/+96；左组 ⓘ + 音量 96；右组盒 24/gap 16
  → 中心距 **40**，从右往左 全屏 0、列表 1、主题 2、音轨 3、字幕 4（设计稿 6 个含画中画，未接）；
  信息卡 240×180 浮胶囊上方 16、贴 x24；`BOTTOM_BAND` 118。
- **展开歌单时胶囊居中于剩余画面**（x=88 = (1280−320−24−760)/2），不是整窗居中；面板 320 宽、
  `top = 标题带+8`、`right 24`、`bottom 119`、头 48/行 48/尾 44。
- **浅底面板上的按钮必须用主题色**（`panel_button(fg, primary)`）：`overlay::fg()`（#D5D9E2）在
  Latte 浅底上几乎看不见。列表容器要 `overflow_hidden`，只给 `min_h_0` 会让最后一行画到页脚上面。
- **质感三层**：L1 胶囊 `v_gradient(#1A1C28 60%, #0B0C10 74%)` + 白 14% 描边 `edge()` + 两条 1px
  绝对定位矩形当内描边（上白 16% / 下黑 30%）+ 双层阴影（`contact 0 2 6 @26%`、`ambient 0 16 40 @32%`）
  + 240 高底部渐隐。L2 画面取色**只取色相**（S 46% / L 36%），染 α .26/.08，8° 迟滞。
  L3 = 同一帧降到 **48×20** 拉伸当模糊底（GPU 双线性 ≈ 大半径 box blur），**别再伪造**。
- **`v_gradient()` 是必需的**：`linear_gradient(angle, from, to)` 只收两个 `LinearColorStop`，
  `Hsla`/`Rgba` 都没有 `From` → 必须 `linear_color_stop(color, pct)` 包一层。没有径向渐变：
  播放键用「竖向渐变 `#A05BF5→#6E24D8` + `inset 0 1px 0 rgba(255,255,255,.35)`」凑球面。
- **悬停白 10% 只能用 `ButtonCustomVariant`**：`ghost` 的悬停色写死在 `ButtonVariant::hovered`
  （= `theme.accent` 亮 Lavender）。`chrome_button(cx, …)` 用 `.color/.foreground/.hover/.active`
  + `.rounded(6)`；`.active()` 收的是**颜色**，没有按下缩放。`panel_button` 仍走主题色（浅底）。
- **`Div::bg()` 先于子节点绘制** → 整幅模糊 `img` 之上再 `.bg(渐变)` 会被盖住（胶囊变白板）。
  渐变要作为 `absolute` 子层，顺序：模糊图 → 玻璃 → 染色 → 内描边 → 内容。
- **`overlay::glass()` 是 72%（`0x0B0C10B8`）平铺**：信息卡/标题带用它；胶囊走「模糊图 + 渐变」。
- 画在画面上的东西（胶囊 / 歌单 / 信息卡 / 底部遮罩 / 状态胶囊）**都必须进 `chrome_layout()`**：
  一是淡出时一起让位（透明但仍在的节点照样吃点击），二是验收脚本按它取点。
- mpv 编码名是**并列别名**（`h264 (High) / AVC / …`）→ 取第一段（`short_codec`）；
  `video-bitrate` 对部分容器报 `unavailable`（合成 mp4 就是），显示 `—` 是对的，别补假数。
- `cycle sid/aid` **无返回值**、单轨不做事 → 记切换前轨号 + 时间戳，tick 比对 observed 值，
  800ms 超时就说"没有其他轨可切"。
- **`h_flex()` 默认 `items_center`**：子元素全绝对定位（内容高 0）交叉轴塌成 0 → `canvas` bounds
  `Wx0`。视频区要 `.self_stretch()`。
- **`height:100%` 在不定高父级退化成 auto** → `.h_full()` 解析不出。写死 `.h(px(..))`，父级补 `.h_full()`。
- 自绘标题带：`appears_transparent = true` 后**只有左侧拖动区**能设 `WindowControlArea::Drag`
  （整条都设会让右侧按钮变非客户区）。窗口按钮 `IconName::Window*` 是 24 网格 + 2px 描边，
  缩 10px 会糊 → 手写 10×10、1px SVG；46 宽 × 标题栏高，关闭键悬停 `#C42B1C`。
- **chrome 前景色跟底色走不跟主题走**：视频模式整块深画布，浅色主题 `muted_foreground` 只 ≈3:1
  → 用 `chrome_fg` / `chrome_dim`。
- 提示分流（`flush_errors`）：`video_active` → 标题带 `status_pill`（4s 退场，tick 比对）；
  否则 `push_notification`。通知层位置/宽度在 `theme::install` 改（`margins.top = TITLE_BAND+12`、
  `width=320`），`apply_config`/`change` 不重置。`push_notification` 要 `&mut Window` → 入队 + 100ms tick flush。
- 窗口外缘**不画描边**（`spec.json → window_edge.stroke = null`）：一圈亮线把画面「封」在框里，
  去掉后画面直通边缘、只靠投影托起来（iina 同口径）。别和胶囊内部那圈白 14–18% 混为一谈。
  浅底窗口压白台面边缘会偏软 → 台面用 `#F5F6FA`。**别为看清把描边加回来。**
- 圆角交给 DWM：`DwmSetWindowAttribute(hwnd, 33, &2, 4)`（Win11）；别用 `SetWindowRgn`（1bit 必锯齿），
  验收读回属性值别用像素。

## 控制层自动淡出（标题带 / 胶囊 / 底部遮罩 / 窗口按钮）

- 判据是纯函数 `chrome_visible_target(video_active, playing, busy, idle)`：**只有「视频 + 在播 +
  没在碰控件」才淡**；暂停 / 面板 / 信息卡 / 拖放提示 / 状态胶囊 / 正在拖进度一律留着。
  `CHROME_IDLE` 2500ms、`CHROME_FADE_STEP` 0.25/100ms tick、`CHROME_HIDDEN_EPS` 0.01。
- **不用 `with_animation`**：那个动画只在有帧驱动时推进，而这里的帧是 tick 拉起来的。
- 「用户还在」三个来源：根节点 `on_mouse_move`、视频舞台 `on_mouse_move`（render API 之后事件能
  直达，不再靠 wndproc 的 `WM_MOUSEMOVE`）、`intercept_keystrokes`。鼠标移动只写时间戳**不 notify**
  （每帧 notify = 整窗重绘）。
- 淡掉的另一半是**几何**：`chrome_layout()` 在 `alpha ≤ EPS` 时返回空 Vec，让透明区块不吃点击。
- 诊断开关 `SMP_CHROME_TRACE=1` 打 `[chrome] alpha/target/idle/...`（500ms 限流）。
- HTML 原型同步同一套判据：`preview/index.html` 播放后 2.5s 不动 → 淡三层 + `pointer-events:none`；
  `?idle=1` 冻结该态、`?bare=1` 去页面留白和工具条。对照图 `docs/chrome-fade.png`。

## 复查（2026-10-19）

- `overlay::dim()` = **#A6ADC8**（`tokens.osc.text_secondary`）。原来的 #8B909C 在彩条红/黄上
  让「剩余时间 / 标题带文件名」几乎读不出；`overlay::label()` 与它同值，已合并成一个。
- **L4 的自适应压暗是渐变的**（`scrim = min(.72, .42 + .85·(L−.30))` + 0.45 平铺底）：顶部几乎
  不压、底部到上限。所以拿它做像素判据时，取样点要放**底部 padding**，用**相对比例**而不是
  绝对差值（彩条这种饱和色 L 只有 0.28，中心只掉 30 出头）。
- 已知特征（不是 bug）：压暗按 Rec.709 亮度算，饱和红/黄（L≈0.28）判成「暗底」→ 压得很少 →
  OSD 卡片压红条时只有 ~0.6，11px 白字约 3.8:1。要更稳得让判据看「感知亮度」（max 通道），
  但那会改 L4 观感，得先和设计对一下。

## 玻璃圆角（2026-10-19，第二轮）

**根因是分辨率，不是裁切方式。** 上一轮以为「贴图里不裁圆角、交给矢量裁剪」就完事，其实那只是
把毛边换成阶梯：贴图原先按 `PLATE_SCALE = 0.5` **逻辑像素**烤，1.5× DPI 下等于放大 3 倍——
14px 折射带在贴图里只有 7 个采样点，弧长方向只剩 11 个点，圆角必然呈阶梯；外层矢量裁剪再准
也救不回贴图里没有的信息。设计稿 HTML 原型一直是 `canvas = W × devicePixelRatio`（1:1 物理
像素），Rust 这边从第一版就低了一档。

- **贴图按屏幕 1:1 出**：`plate_scale(job, dpi)` = `window.scale_factor()`（封顶 2.0），
  单块面 **22 万像素**封顶，超了才按面积降（歌单 320×633 会降到 ≈0.69× 物理）。
  150% DPI 下胶囊贴图就是 1140×138。
- **圆角裁在贴图里**，alpha 用**物理 1 像素**的覆盖率：`cover = clamp(0.5 − d·dpi, 0, 1)`。
  过渡带宽度与贴图分辨率无关，1:1 时就是 1 贴图像素。二值裁切不行（`d > 0` 清黑会糊出灰黑
  毛边），但带覆盖率的 alpha 可以；外层 `overflow_hidden` 留作双保险。
- **描边必须做成覆盖层**。GPUI 的 `border` 与 `bg()` 一样在**子节点之前**绘制
  （`Div::paint` → `interactivity.paint` 里先背景边框、再循环 paint children），
  根节点上的 `border_1()` 会被整幅玻璃贴图完整盖掉——圆角处那条"断掉的边"就是它。
  改成 `.absolute().size_full().rounded(r).border_1()` 的空节点，铺在贴图之后。
- **顶亮/底暗那两条 1px 矩形只在没有贴图时画**：它们是直条，进圆角处被裁掉，
  顶边亮线会突然断掉。液态玻璃的边只留整圈 `border`（白 12%），厚度感交给折射。
- 副作用：胶囊去掉 `border_1()` 后内容盒宽了 2px，`PROGRESS_SIDE_PAD` 要从 2
  回到设计稿的 3（否则轨道是 716 而不是 714）。
- **代价与应对**：像素数 ×9，三块面 47 万像素一次烘焙约 14ms（release）。内层循环按
  行/列预计算 SDF 的 `qx`/`qy` 与采样基准坐标（17.9 → 14.5ms），并把「只有新帧」的重烤
  节流到 **20ms**；布局变化（开合面板、改窗口、切 DPI）不节流。
- `[profile.dev/testing] opt-level = 2` 只给自己两个 crate：液态玻璃在 opt-level=0 下慢一个
  数量级，debug 构建会卡成幻灯片。依赖仍用 0（它们慢在启动，不在每帧）。
