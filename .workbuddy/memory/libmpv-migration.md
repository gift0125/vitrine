# libmpv 进程内迁移（A → B，2026-10-18 全部落地）

> **A**：进程内 libmpv + `--wid` 顶层叠加窗 —— 曾经可用，已彻底删除（`video_overlay.rs` 1010 行）。
> **B**：**SW render API → BGRA 帧 → `RenderImage` → GPUI `img()`** —— 当前架构。
> 结果：`cargo test --workspace` 29（core）+ 42（app）；`verify_floating_chrome.py` 17 项 ALL OK；
> 发布包 55.7 MB（含 `libmpv-2.dll` 116.3 MB）。过程见 `2026-10-18.md` §24 / §25，
> 正式记录见 `docs/gpui-kit-plan.md` 第十二轮 / 第十五轮。

## 库从哪来

- 官方 `mpv.exe` 是**静态链接**，拆不出 DLL → 用 `mpv-dev` 包（`zhongfly/mpv-winbuild`）的
  `libmpv-2.dll` + `include/mpv/*.h`；解 `.7z` 用 `C:\Windows\System32\tar.exe`（机器没装 7z）。
  `scripts/fetch-libmpv.ps1` 已自动化（`-CopyToExe` 拷到 target/{debug,release}，`-Lgpl` 取 LGPL 变体）。
- **运行时加载**（`libloading 0.8` + 手写符号表）而非构建期链接：`cargo build` 不需要 DLL，
  缺库给可见的 `VideoError::LibmpvNotFound` 而不是进程加载失败。
- render API 的符号用 `opt_sym!` 取——旧版 DLL 没有它们时只是 `has_render_api() == false`，
  不该整块加载失败。

## 内核契约（A/B 通用）

- `mpv_wait_event` 只能一个线程调；关闭顺序 = set quit → `mpv_wakeup` → join 事件线程
  → `mpv_terminate_destroy`；`wait_event` 用 0.05s 超时不死等。
- **`observe_property` 格式必须匹配真实类型**：字符串属性（`sid`/`aid`）按 INT64 观察会被整条拒绝、
  永不更新 → 用 `MPV_FORMAT_STRING`，`no` 折成 0。`MPV_FORMAT_STRING` 的结果由 mpv 分配，要 `mpv_free`。
- `wid` 必须按 **int64** 设（HWND 高位置 1 会被解析成负数）——B 之后已不再使用，留着当历史。
- 搜索路径的单测**不要 `set_var`**（与并行的真 DLL 用例抢进程环境）→ `find_libmpv_in(exe_dir, overrides,
  current_exe, PATH)` 纯函数传参。
- 可测性：无 FFI 的 `EngineEvent` + 纯函数 `apply_event(event, &Arc<Mutex<VideoStatus>>)`，
  可无声卡/显示器单测；真 DLL 集成测试缺库时优雅跳过。
- 进程内解码「起播到首帧」约 3~4s（hwdec/d3d11va 初始化），比子进程慢。验收脚本里任何
  「点击后固定 sleep 再取样」都会变成竞态 → 必须轮询到结果（`wait_trace` / `wait_darker` / `wait_client_size`）。
- 验收契约保留：`SMP_MPV_TRACE` 仍按老 JSON 形状（`-> {"command":[...]}`）写，脚本数
  `"seek"`/`"volume"`/`"cycle"` 行不用改。

## SW render API（B 的硬事实）

| 参数 / 行为 | 值 | 踩过的坑 |
|---|---|---|
| `MPV_RENDER_PARAM_API_TYPE` | `"sw"` | **data 就是 `char*` 本身**，不是 `&ptr`（否则 unknown API type） |
| `SW_SIZE=17` | `int[2]` | 目标尺寸；`fit_render_size` 算出来的，不是原分辨率 |
| `SW_FORMAT=18` | `"bgr0"` | 第 4 字节是垃圾 → **必须补 255**，否则整幅半透明 |
| `SW_STRIDE=19` | `size_t` | 按 `w*4` 反推 buffer 长度，别用 `w*h*4` 猜 |
| `SW_POINTER=20` | 输出缓冲指针 | 参数数组以 `type == 0` 结尾 |
| `vo` | **必须 `libmpv`** | 默认 `gpu` 要真窗口；不设时上下文照样创建成功但只清一帧黑、回调只来一次 |
| `video-timing-offset` | `0` | 否则 `render()` 等到显示时刻才返回，卡住 UI 线程 |
| 线程 | 全部 `mpv_render_context_*` 在**创建线程** | update 回调跑在 mpv 线程，**禁止在里面调 mpv API** |

- 通知机制：`async_channel::bounded(1)` + `try_send`（满了丢，正好是「有帧了」的合并语义）。
- 上传：`RenderImage::new(vec![image::Frame::new(RgbaImage)])` → `ImageSource::Render` →
  `gpui_kit::img(...)`；**字节按 BGRA 解释**，`image` crate 要和 gpui-pre 同版本（0.25）。
- 图集：每帧都是新 `ImageId`，Win 按 `(image_id, frame_index)` 分 tile（1024² 起、最大 16384²）
  → 送进去的尺寸封顶 **2560×1440**；展示完 `window.drop_image()`，留 2 帧退役队列。
- L3 模糊：同一帧降到 **48×20** 当底图拉伸（GPU 双线性 ≈ 大半径 box blur）。这不是空间精确的
  backdrop blur，做折射/液态玻璃前必须先换成矩形区域对位采样。
- 取色：帧回调里不再有 `screenshot-raw`；色相/均值直接读 `FrameBuffer`（`band_mean` / `band_hue`）。
- 旧的 `screenshot-raw` + `mpv_node` 那套已全删——抓的是「当前显示的那一帧」，刚 `loadfile`
  会抓到上一部片子，延迟+重试的补丁全是为此而生。

## 参考 IINA

`Contents/Frameworks/` 塞 `libmpv.2.dylib` + 44 个依赖 dylib，`mpv_create()` 进程内调、视频走
render API，dylib 由 `other/download_libs.sh` 构建期下载（不入库）。和我们现在的形状一致。
