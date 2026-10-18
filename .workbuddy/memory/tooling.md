# 构建 / 发布 / 验收工具链

> 从 MEMORY.md 拆出。

- 发布版必须 **GUI 子系统**（`windows_subsystem="windows"`），读 PE `OptionalHeader.Subsystem`（2=GUI）验；GUI 无 stderr 但 `eprintln!` 不 panic。
- **任务栏图标靠 PE 资源 ID=1**（`gpui-pre-windows::load_icon` 用 `LoadImageW(module, MAKEINTRESOURCE(1), ..)`；`WindowOptions.icon` 是 x11 only）。`crates/app/build.rs` 用 embed-resource 写 `1 ICON` + VERSIONINFO；`canonicalize` 的 `\\?\` 前缀让 rc 找不到文件，要剥掉换正斜杠。
- 含中文的 `.ps1` **必须 UTF-8 with BOM**（PS5.1 按 GBK 解 → 乱码 + 语法错）。`cargo update -p X --precise` 在咬死依赖图上死循环 → 删 `Cargo.lock` 对应 `[[package]]` 块。
- **`package.ps1` 曾"成功却退出码 1"**：cargo 进度写 stderr，脚本头 `$ErrorActionPreference="Stop"` 把原生命令 stderr 当 `NativeCommandError` 终止。构建期临时 `Continue`、只认 `$LASTEXITCODE`。（任何 Stop 的 .ps1 调 cargo/git 都要这么包。）
- `scripts/shot.py`：先 `SetProcessDpiAwareness(2)`（否则只截左上角）；`SetWindowPos` 设外框，按目标客户区用 `AdjustWindowRectExForDpi` 反推；DWM 动画期抢截留黑边要轮询。`--margin N` 才看得到圆角/阴影。
- 取样用**实测客户区**换算（1898×1189 物理 ÷1.5 = 1265×793 ≠ 1280×800）。**改完尺寸后的取样必须等 mpv 重配 VO**（否则截到"右边露桌面"）：`wait_aligned()` 后 0.15s 重试最多 3s。`colorful()` 挑高饱和点（彩条自带近纯白条）。
- 脚本一律跑隔离配置目录 `SMP_DATA_DIR`（曾覆盖用户在用的 playlist.json）。诊断开关：`SMP_MPV_LOG` 存 stderr、`SMP_MPV_TRACE` 存命令报文；mpv 对路径重复分隔符不宽容（`C://` → `loading failed`），`msg-level=all=error` 下只有报文能看出真相。
- 发布验收看**打包后的 exe**：`SMP_EXE=dist/.../simple-media-player.exe python scripts/verify_floating_chrome.py`。


## 验收脚本的坑（`verify_floating_chrome.py`，从 MEMORY.md 拆出）

- 探针变了：第十五轮起读 `SMP_UI_STATE` 的 JSON（`stage` / `rects` / `chrome` / `panel` / `cinema`），
  不再 `GetWindowRgn`。`panel_open()` 读 `panel` 字段（权威），不要靠像素猜。
- **永远别按"胶囊铺满整宽"算坐标**；**胶囊左沿必须走 `capsule_left()`**（跟随面板展开态）。
  写死整窗居中 → 收起歌单那一下点到胶囊外 → 面板一直开着 → 后面按钮坐标全偏 344，报出一串**假失败**。
- 每次点击都要记 `WindowFromPoint` 的窗口类名，非主窗就判 FAIL —— 坐标偏了点击会静默落到别处，
  截图看不出来。
- 素材要 **120s**（`target/verify-long.mp4`）：40s 片子播完 → 取样全变 `#EFF1F5`，多项假失败。
- 合成输入（`mouse_event`）下的**第一次点击偶尔被吞**：有状态的点击都按**结果**重试（最多 3 次），
  判据不放松。
- **脚本会被自己的新功能打败**：控制层 2.5s 淡出 → `rects` 空 → `panel_open()` 假 False。
  凡有空闲间隔，`click()`/`wait_settled()` 开头统一 `wake()`（光标抖到画面中心、等 0.45s 淡入）。

## 设计导出管线

- `export_nodes` 的留白**不固定**（主窗四周 96 / Win 64、小窗 88，改阴影半径还会变），且
  **阴影 alpha>0 会被算进内容边界** → 不能按固定偏移裁。
- 落库流程：原始图下到独立目录 → `python scripts/normalize-frames.py <dir>`（按节点 id 找文件、
  `alpha≥250` 定位窗口矩形、按 `设计尺寸×2` 裁、透明区 RGB 清零）→ `python scripts/make-overview.py`
  重拼 `docs/design-overview.png`。两个脚本都读 `spec.json`。
- 导出图**透明区 RGB 必须清零**，否则 `convert('RGB')` 会泛绿/灰（查看器里像"设计元素"，其实是阴影底色残留）。
