# libmpv 进程内迁移（A 方案，2026-10-18 已落地）

> 结果：`cargo test` 22（core）+ 33（app）全绿；`verify_floating_chrome.py` 18 项 ALL OK（debug 与打包 exe）；
> 发布包 55.7 MB（含 `libmpv-2.dll` 116.3 MB）。过程见 `2026-10-18.md` §24 与 `docs/gpui-kit-plan.md` 第十二轮。

- 三选一：A 进程内 libmpv + 保留 `wid` 叠加窗（选它）；B 加 **SW render API** → RGBA 帧 → GPUI `img()`（叠加窗/区域/z 序全删）；C D3D11 共享纹理 + DirectComposition（要 fork `gpui-pre-windows`）。选 A 的理由是**仍能升级**到 B/C。
- 官方 `mpv.exe` 静态链接拆不出 DLL → 用 `mpv-dev` 包（`zhongfly/mpv-winbuild`）的 `libmpv-2.dll` + `include/mpv/*.h`；解 `.7z` 用 `C:\Windows\System32\tar.exe`（机器没装 7z）。`scripts/fetch-libmpv.ps1` 已自动化（`-CopyToExe` 拷到 target/{debug,release}，`-Lgpl` 取 LGPL 变体）。
- 运行时加载（`libloading 0.8` + 手写 ~20 符号）而非构建期链接：`cargo build` 不需 DLL，缺库给可见 `VideoError::LibmpvNotFound` 而不是进程加载失败。
- 线程契约：`mpv_wait_event` 只能一个线程调；关闭顺序 = set quit → `mpv_wakeup` → join 事件线程 → `mpv_terminate_destroy`；`wait_event` 用 0.05s 超时不死等。
- **`observe_property` 格式必须匹配真实类型**：字符串属性（`sid`/`aid`）按 INT64 观察会被整条拒绝、永不更新 → 按 `MPV_FORMAT_STRING`，`no` 折成 0。
- **`wid` 必须按 int64 设**（否则 HWND 高位置 1 会解析成负数）。`MPV_FORMAT_STRING` 结果由 mpv 分配，要 `mpv_free` 释放。
- 验收契约保留：`SMP_MPV_TRACE` 仍按老 JSON 形状（`-> {"command":[...]}`）写，脚本数 `"seek"`/`"volume"`/`"cycle"` 行不用改。
- 可测性：无 FFI 的 `EngineEvent` + 纯函数 `apply_event(event, &Arc<Mutex<VideoStatus>>)`，可无声卡/显示器单测；真 DLL 集成测试缺库优雅跳过。
- **搜索路径的单测不要 `set_var`**（与并行的真 DLL 用例抢进程环境，偶发失败）→ `find_libmpv_in(exe_dir, overrides, current_exe, PATH)` 纯函数传参。
- 进程内解码"起播到首帧"约 3~4s（hwdec/d3d11va 初始化），比子进程慢。验收脚本里任何"点击后固定 sleep 再取样"都会变成竞态，必须轮询到结果（`wait_trace` / `wait_darker` / `wait_client_size`）。

- B 的代价（别再论证）：`ImageSource::Render`/`RenderImage::new`/`image::Frame::new` 都公开、不用改上游；但每帧换 `ImageId`，Win atlas 按 `(image_id, frame_index)` 缓存（1024² 起最大 16384²）→ 逐帧新 tile + ~8MB/帧 上传，1080p 勉强、4K 不行。
- IINA 参考：`Contents/Frameworks/` 塞 `libmpv.2.dylib` + 44 依赖 dylib，`mpv_create()` 进程内调、视频走 render API，dylib 由 `other/download_libs.sh` 构建期下载（不入库）。
