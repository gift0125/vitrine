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

