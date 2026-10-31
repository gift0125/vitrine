//! 视频播放内核：**进程内** libmpv 客户端。
//!
//! ## 为什么进程内
//!
//! 早先用「`mpv.exe` 子进程 + JSON IPC 命名管道」：控制通道要自己实现管道读写、
//! 事件是 JSON 文本、播放状态靠解析字符串、mpv 一崩只能看到一个「进程没了」。换成
//! libmpv（`libmpv-2.dll`）后这些全部消失：
//!
//! | | 子进程 + IPC | 进程内 libmpv |
//! |---|---|---|
//! | 控制 | 命名管道写 JSON | `mpv_command` 直接调 |
//! | 状态 | 解析 `property-change` 文本 | 类型化的 `mpv_event_property` |
//! | 失败原因 | stderr 重定向 / `end-file` 的错误码 | `mpv_error_string` |
//! | 延迟 | 管道 + 线程调度 | 一次函数调用 |
//!
//! 代价是**隔离性**：mpv 崩了应用一起崩（子进程时代只死子进程）。这是 IINA 等
//! libmpv 客户端的共同取舍——mpv 本身的崩溃点集中在本机文件解码上，收益大于风险。
//!
//! ## 画面怎么出来
//!
//! 走**软件渲染 API**（`mpv_render_context_*` + `MPV_RENDER_API_TYPE_SW`）：
//! mpv 把每一帧缩放到我们给的缓冲里（BGRA），画面随我们的合成树一起上屏。
//! 早先是 `wid` 选项塞进一个自建叠加窗，那套做法要付区域挖洞、z 序核对、
//! 事件桥接三份代价，也拿不到「画面像素」（做不了玻璃模糊）——见 [`crate::render`]。
//!
//! `SpawnOptions::wid` 还留着：探针脚本靠它把画面塞进普通窗口看效果，
//! 真正播放时传 `None`。
//!
//! ## 线程模型
//!
//! `mpv_wait_event` 只能在一个线程里调 → 一个专职「事件线程」循环取事件、写
//! `VideoStatus`；命令/属性接口从 UI 线程直接调用（libmpv 客户端 API 是线程安全的）。
//! 退出顺序固定为：置 quit → `mpv_wakeup` 打断等待 → join 事件线程 → `mpv_terminate_destroy`，
//! 保证销毁时已经没有人还在用这个句柄。
//!
//! ## 可测性
//!
//! 事件到状态的映射抽成了纯函数 [`apply_event`]，输入是 [`EngineEvent`]（与 FFI 无关）。
//! 这样「end-file 的 error 文案」「新片清空上一部的信息」这类逻辑能在没有声卡、
//! 没有显示器的环境里跑测试，不需要真的起一个 mpv。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;

use crate::libmpv::{
    LibMpv, MpvCtx, MpvEventEndFile, MpvEventLogMessage, MpvEventProperty,
    MPV_END_FILE_REASON_ERROR, MPV_END_FILE_REASON_QUIT, MPV_END_FILE_REASON_REDIRECT,
    MPV_EVENT_END_FILE, MPV_EVENT_FILE_LOADED, MPV_EVENT_LOG_MESSAGE, MPV_EVENT_PROPERTY_CHANGE,
    MPV_EVENT_START_FILE, MPV_FORMAT_DOUBLE, MPV_FORMAT_FLAG, MPV_FORMAT_INT64, MPV_FORMAT_STRING,
};
use crate::render::{FrameBuffer, FrameRef, RenderContext};

/// 视频文件扩展名（由 mpv 解码，与音频走不同路径）。
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "mov", "avi", "flv", "wmv", "ts", "m2ts", "mpg", "mpeg", "3gp",
    "ogv", "rmvb",
];

/// 事件线程的单次等待上限（秒）。
///
/// 不用 `-1` 死等：`mpv_wakeup` 能打断它，但给个上限之后，即使 wakeup 因为平台差异
/// 没生效，退出也只多等这几十毫秒，不会挂死。
const EVENT_WAIT_TIMEOUT: f64 = 0.05;

/// 路径是否为视频文件（按扩展名判断）。
pub fn is_video_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// 串流协议：这些「路径」不在本地磁盘上，一律交给 mpv（它自己会分辨是音是视）。
const STREAM_SCHEMES: &[&str] = &[
    "http", "https", "rtmp", "rtmps", "rtsp", "rtp", "srt", "udp", "mms", "ftp", "ytdl",
];

/// 是不是串流地址（`https://…`、`rtmp://…` 这类）。
///
/// 只认 `协议://`：Windows 盘符 `C:\…` 也带冒号，不能把它当成协议。
pub fn is_stream_url(path: &str) -> bool {
    path.split_once("://").is_some_and(|(scheme, rest)| {
        !rest.is_empty() && STREAM_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str())
    })
}

/// 视频内核错误。
#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    #[error("未找到 {}（可用环境变量 SMP_LIBMPV_PATH 指定路径）", crate::LIBMPV_NAME)]
    LibmpvNotFound,
    #[error("无法加载 libmpv：{0}")]
    Load(String),
    #[error("libmpv 初始化失败：{0}")]
    Init(String),
}

/// 视频播放状态快照（由事件线程写入，UI 线程读取）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoStatus {
    /// 当前播放位置（秒）
    pub position: f64,
    /// 总时长（秒），未知为 0
    pub duration: f64,
    /// mpv 当前是否暂停
    pub paused: bool,
    /// 视频显示尺寸（像素）：mpv 的 `dwidth`/`dheight`，已按像素宽高比与旋转校正。
    ///
    /// 音频文件、尚未解码出画、idle 时为 0。UI 拿它把窗口调成画面的比例
    /// （窗口尺寸 = 内容尺寸，画面刚好铺满、不留黑边）。
    pub width: u32,
    pub height: u32,
    /// 未加载任何文件
    pub idle: bool,
    /// 已自然播放结束（end-file reason=eof）
    pub finished: bool,
    /// 加载/解码失败原因（end-file reason=error 等），由 UI 取走提示后清空
    pub error: Option<String>,
    /// 客户端是否仍然可用（初始化成功且未销毁）
    pub connected: bool,

    // ---- 下面这些只给「视频信息」卡用（`ⓘ`）----
    //
    // mpv 通过 `observe_property` 推，未加载/音频文件时为空串或 0。UI 侧只读不写，
    // 所以字段一律是「显示用的原样字符串 + 数字」，格式化留给视图层：
    // 这里换成 `HumanWidth` 之类反而会让 `PartialEq` 每秒都判不等。
    /// 编码描述，mpv 的 `video-codec`（例：`h264 (High)`）
    pub video_codec: String,
    /// 编码尺寸（`width`/`height`），与 `width`/`height`（显示尺寸）不同：
    /// 变形像素的片子两者不等，信息卡要显示的是**编码尺寸**。
    pub source_width: u32,
    pub source_height: u32,
    /// 容器帧率（`container-fps`）
    pub fps: f64,
    /// 视频码率（bps）
    pub video_bitrate: f64,
    /// 音频码率（bps）
    pub audio_bitrate: f64,
    /// 色彩：`video-params/primaries`（例 `bt.709`）
    pub primaries: String,
    /// 色彩：`video-params/gamma`（例 `bt.1886`）
    pub gamma: String,
    /// 音频编码名（`audio-codec-name`，例 `aac`）
    pub audio_codec: String,
    /// 声道数（`audio-params/channel-count`）
    pub audio_channels: u32,
    /// 采样率（`audio-params/samplerate`）
    pub audio_rate: u32,
    /// 当前字幕轨号，0 = 关闭（mpv 的 `sid` 在「无字幕」时是 `no`）
    pub sub_track: u32,
    /// 当前音轨号，0 = 无音频（`aid`）
    pub audio_track: u32,
}

/// 启动参数。
#[derive(Debug, Clone)]
pub struct SpawnOptions {
    /// `libmpv-2.dll` 路径（由 [`find_libmpv`] 找到）。
    pub libmpv: PathBuf,
    /// 叠加窗口句柄（Windows HWND）。`None` 时使用 `vo=null`（无窗口，用于测试）。
    pub wid: Option<isize>,
    /// 初始音量（0.0~1.0）
    pub volume: f32,
    /// 追加选项（`--name=value` 形式，测试用，例如 `--ao=null`）
    pub extra_args: Vec<String>,
}

/// 属性值（已从 `mpv_event_property` 的裸指针转成 Rust 类型）。
#[derive(Debug, Clone, PartialEq)]
pub enum PropValue {
    Double(f64),
    Int(i64),
    Flag(bool),
    Text(String),
    /// 属性当前不可用（`MPV_FORMAT_NONE`）
    None,
}

/// end-file 的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Eof,
    Stop,
    Quit,
    Redirect,
    Error,
    Other,
}

/// 与 FFI 无关的引擎事件（事件线程把 `mpv_event` 转成它，测试直接构造它）。
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    Property { name: String, value: PropValue },
    StartFile,
    FileLoaded,
    EndFile { reason: EndReason, error: Option<String> },
}

/// mpv 客户端 + 事件线程。
pub struct VideoEngine {
    mpv: Arc<LibMpv>,
    ctx: MpvCtx,
    status: Arc<Mutex<VideoStatus>>,
    worker: Option<JoinHandle<()>>,
    quit: Arc<AtomicBool>,
    destroyed: bool,
    /// 软件渲染上下文（[`Self::enable_render`] 之后才有）
    render: Option<RenderContext>,
    /// 每帧复用的像素缓冲：尺寸不变就不重新分配
    buffer: FrameBuffer,
    /// 上一次渲染失败的原因（只用来去重日志，别每帧刷）
    last_render_error: Option<String>,
    /// 这个文件实际用上的硬解方式记过没有（每次加载记一次）
    hwdec_logged: AtomicBool,
}

impl VideoEngine {
    /// 加载 libmpv 并初始化客户端。
    pub fn spawn(options: SpawnOptions) -> Result<Self, VideoError> {
        let mpv = Arc::new(LibMpv::load(&options.libmpv).map_err(VideoError::Load)?);
        let ctx = MpvCtx(mpv.create());
        if ctx.is_null() {
            return Err(VideoError::Init("mpv_create 返回了空指针".into()));
        }

        // ---- 选项：必须在 mpv_initialize 之前设置 ----
        //
        // 名字与以前给 `mpv.exe` 的 CLI 参数一一对应，去掉前导 `--`。
        // `config` 不用管：libmpv 默认不读 mpv.conf（只有显式调 mpv_load_config_file 才读）。
        let apply = |name: &str, value: String| {
            let rc = mpv.set_option_string(ctx.raw(), name, &value);
            if rc < 0 {
                // 不是致命错误：不同版本的 mpv 选项集合有差异，记下来便于排查
                trace(&format!(
                    "!! 选项被拒 {name}={value}（{}）",
                    mpv.error_string(rc)
                ));
            }
        };
        apply("keep-open", "no".into());
        apply("osc", "no".into());
        apply("osd-level", "0".into());
        apply("input-default-bindings", "no".into());
        apply("hwdec", "auto-safe".into());
        apply("msg-level", "all=error".into());
        apply(
            "volume",
            format!("{:.2}", options.volume.clamp(0.0, 1.0) * 100.0),
        );
        apply("idle", "yes".into());
        // 软件渲染下不能让 `mpv_render_context_render()` 阻塞到「该显示的时刻」：
        // 它跑在 UI 线程上，阻塞就等于每帧掉 16ms 的界面响应（拖动、悬停全卡）。
        // 置 0 之后「有新帧就立刻渲染」，节奏交给我们的重绘。
        apply("video-timing-offset", "0".into());

        // 画面往哪去：
        // - `Some(wid)`：mpv 自己画进那个窗口（旧的叠加窗方案，留着给探针脚本用）；
        // - `None`：不设 `vo`，用 libmpv 的默认 `vo=libmpv`——这是 render API 的前提。
        //
        // 这里以前写的是 `vo=null`（纯音频客户端用），换成渲染 API 之后那行会把
        // 视频输出关掉，`mpv_render_context_render` 永远拿不到帧。
        if let Some(wid) = options.wid {
            // 句柄走 int64：字符串形式在 32 位句柄高位为 1 时会被解析成负数
            let rc = mpv.set_option_i64(ctx.raw(), "wid", wid as i64);
            if rc < 0 {
                trace(&format!(
                    "!! wid={wid} 被拒（{}）",
                    mpv.error_string(rc)
                ));
            }
            apply("force-window", "yes".into());
        } else {
            // 渲染 API 必须显式指定 `vo=libmpv`：mpv 默认的 vo 是 `gpu`
            // （要一个真窗口），没有窗口时它会去找别的 vo，**渲染上下文的
            // `draw_frame` 就永远不被调用**——表现是 `mpv_render_context_render()`
            // 一路返回 0、像素却全黑（它只是在清屏），而且 update 回调只在
            // 注册那一次触发过，非常容易误判成"渲染器没跑起来"。
            apply("vo", "libmpv".into());
        }

        for extra in &options.extra_args {
            let spec = extra.trim_start_matches('-');
            match spec.split_once('=') {
                Some((name, value)) => apply(name, value.to_string()),
                None => apply(spec, "yes".into()),
            }
        }

        // ---- 初始化 ----
        let rc = mpv.initialize(ctx.raw());
        if rc < 0 {
            let detail = mpv.error_string(rc);
            mpv.terminate_destroy(ctx.raw());
            return Err(VideoError::Init(detail));
        }

        // 诊断开关：`SMP_MPV_LOG=<路径>` 时把 mpv 的日志（error 级）落盘。
        // mpv 的失败原因（解码、VO 初始化、文件打不开）只在这里，没有它只能看到
        // `end-file reason=error` 这种无法定位的信息。
        if std::env::var_os("SMP_MPV_LOG").is_some() {
            mpv.request_log_messages(ctx.raw(), "error");
        }

        let status = Arc::new(Mutex::new(VideoStatus {
            connected: true,
            idle: true,
            ..Default::default()
        }));
        let quit = Arc::new(AtomicBool::new(false));

        // ---- 订阅属性 ----
        //
        // id 只是给事件的 reply_userdata 用的序号，UI 侧靠名字匹配。
        // 格式必须和属性真实类型一致：观测成 INT64 的字符串属性（如 `sid`）
        // mpv 会直接拒掉整条 observe，那个属性就永远收不到更新。
        const OBSERVED: &[(u64, &str, i32)] = &[
            (1, "time-pos", MPV_FORMAT_DOUBLE),
            (2, "duration", MPV_FORMAT_DOUBLE),
            (3, "pause", MPV_FORMAT_FLAG),
            (4, "idle-active", MPV_FORMAT_FLAG),
            // 画面尺寸：`d*` 是「实际显示尺寸」（含像素宽高比校正与旋转），
            // `width`/`height` 只是编码尺寸，变形画面的比例会算错。
            (5, "dwidth", MPV_FORMAT_INT64),
            (6, "dheight", MPV_FORMAT_INT64),
            // 视频信息卡（`ⓘ`）要的几项。mpv 对每个属性都会先推一次当前值，
            // 加载新文件时再推一次，所以不用额外 get_property。
            (7, "video-codec", MPV_FORMAT_STRING),
            (8, "width", MPV_FORMAT_INT64),
            (9, "height", MPV_FORMAT_INT64),
            (10, "container-fps", MPV_FORMAT_DOUBLE),
            (11, "video-bitrate", MPV_FORMAT_DOUBLE),
            (12, "audio-bitrate", MPV_FORMAT_DOUBLE),
            (13, "video-params/primaries", MPV_FORMAT_STRING),
            (14, "video-params/gamma", MPV_FORMAT_STRING),
            (15, "audio-codec-name", MPV_FORMAT_STRING),
            (16, "audio-params/channel-count", MPV_FORMAT_INT64),
            (17, "audio-params/samplerate", MPV_FORMAT_INT64),
            // 字幕/音轨的当前选择：两个字轨按钮循环切换后，回执靠它。
            // 这两个属性本身是字符串（`no` / `auto` / `1`），按 STRING 观测。
            (18, "sid", MPV_FORMAT_STRING),
            (19, "aid", MPV_FORMAT_STRING),
        ];
        for (id, name, format) in OBSERVED {
            let rc = mpv.observe_property(ctx.raw(), *id, name, *format);
            if rc < 0 {
                trace(&format!(
                    "!! observe {name} 失败（{}）",
                    mpv.error_string(rc)
                ));
            }
        }

        // ---- 事件线程 ----
        let worker = {
            let mpv = Arc::clone(&mpv);
            let status = Arc::clone(&status);
            let quit = Arc::clone(&quit);
            std::thread::spawn(move || event_worker(mpv, ctx, status, quit))
        };

        Ok(Self {
            mpv,
            ctx,
            status,
            worker: Some(worker),
            quit,
            destroyed: false,
            render: None,
            buffer: FrameBuffer::new(),
            last_render_error: None,
            hwdec_logged: AtomicBool::new(false),
        })
    }

    /// 加载并播放文件（替换当前文件）。
    pub fn load(&self, path: &str) {
        self.load_at(path, 0.0);
    }

    /// 加载文件并从 `start` 秒开始放（续播）。
    ///
    /// 走 mpv 的 `start` 属性而不是加载后再 seek：seek 要等文件打开才生效，
    /// 中间会先从头闪出一帧、响一下声。属性是全局的，每次加载都要重写一遍，
    /// 否则下一个文件也会从这个位置开始。
    pub fn load_at(&self, path: &str, start: f64) {
        let value = if start > 0.0 {
            format!("{start}")
        } else {
            "none".to_string()
        };
        trace_command(&["set_property", "start", &value]);
        self.mpv
            .set_property_string(self.ctx.raw(), "start", &value);
        self.hwdec_logged.store(false, Ordering::Relaxed);
        self.command(&["loadfile", path, "replace"]);
    }

    /// 出第一帧时记一下 mpv 实际选的解码方式（`videotoolbox` / `no` …）。
    /// `hwdec=auto-safe` 只是「允许」，真用没用上只有这个属性知道。
    fn log_hwdec_once(&self) {
        if self.hwdec_logged.load(Ordering::Relaxed) {
            return;
        }
        // 解码器起来之前这个属性不存在（头几帧可能还是清屏），拿到为止
        if let Some(current) = self.mpv.get_property_string(self.ctx.raw(), "hwdec-current") {
            self.hwdec_logged.store(true, Ordering::Relaxed);
            trace(&format!("render: hwdec-current={current}"));
        }
    }

    pub fn pause(&self) {
        self.set_paused(true);
    }

    pub fn resume(&self) {
        self.set_paused(false);
    }

    pub fn set_paused(&self, paused: bool) {
        trace_command(&["set_property", "pause", if paused { "true" } else { "false" }]);
        self.mpv
            .set_property_flag(self.ctx.raw(), "pause", paused);
    }

    /// 绝对定位（秒）。
    pub fn seek(&self, secs: f64) {
        let secs = secs.max(0.0);
        trace_command(&["seek", &format!("{secs}"), "absolute"]);
        self.mpv.command(
            self.ctx.raw(),
            &["seek", &format!("{secs}"), "absolute"],
        );
    }

    /// 设置音量（0.0~1.0）。
    pub fn set_volume(&self, volume: f32) {
        let pct = (volume.clamp(0.0, 1.0) * 100.0) as f64;
        trace_command(&["set_property", "volume", &format!("{pct}")]);
        self.mpv.set_property_f64(self.ctx.raw(), "volume", pct);
    }

    /// 停止当前文件（回到 idle，保留客户端）。
    pub fn stop(&self) {
        self.command(&["stop"]);
    }

    /// 循环切换字幕轨（含「关闭」）。
    ///
    /// 用 mpv 的 `cycle` 而不是自己算下一轨：轨到底是 1 条还是 5 条、有没有
    /// forced/外挂字幕，全在 mpv 那边；自己维护一份「第几条」立刻就会和它对不上。
    pub fn cycle_subtitles(&self) {
        self.command(&["cycle", "sid"]);
    }

    /// 循环切换音轨。
    pub fn cycle_audio_track(&self) {
        self.command(&["cycle", "aid"]);
    }

    /// 状态快照。
    pub fn status(&self) -> VideoStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// 当前文件的章节：`(起始秒, 标题)`，按时间升序。
    ///
    /// 走 `chapter-list/{i}/time|title` 这种**数组子路径**，而不是把
    /// `MPV_FORMAT_NODE` 的联合体搬进 Rust（那要多定义 3 个 `#[repr(C)]` 结构 +
    /// 一个 union，还得自己 `mpv_free_node_contents`）。mpv 属性系统原生支持
    /// `list/index/field`，数字类还会做格式转换，所以 `chapters`（int64）也能用
    /// `get_property_f64` 读。
    ///
    /// 上限 500：坏文件里章节数可能是垃圾值，别让一次读取把 UI 拖死。
    pub fn chapters(&self) -> Vec<(f64, String)> {
        let count = self
            .mpv
            .get_property_f64(self.ctx.raw(), "chapters")
            .unwrap_or(0.0);
        let count = count.max(0.0).min(500.0) as usize;
        (0..count)
            .filter_map(|i| {
                let time = self
                    .mpv
                    .get_property_f64(self.ctx.raw(), &format!("chapter-list/{i}/time"))?;
                let title = self
                    .mpv
                    .get_property_string(self.ctx.raw(), &format!("chapter-list/{i}/title"))
                    .unwrap_or_default();
                Some((time, title))
            })
            .collect()
    }

    /// 建立软件渲染上下文（幂等）。失败时返回 mpv 的错误码文本。
    ///
    /// **必须在将来调 [`Self::render_frame`] 的那个线程上调**，`render.h` 要求同一个
    /// 渲染上下文的所有调用同线程。我们的调用方是 UI 线程，两处都从 UI 线程来。
    pub fn enable_render(&mut self) -> Result<(), String> {
        if self.render.is_some() {
            return Ok(());
        }
        if !self.mpv.has_render_api() {
            return Err("这个 libmpv 不带渲染 API（缺少 mpv_render_context_* 符号）".into());
        }
        // mac 先走 GPU（OpenGL → IOSurface）；建不起来再退回软件渲染。
        // `VITRINE_RENDER=sw` 强制软件渲染，方便两条路对比。
        #[cfg(target_os = "macos")]
        if std::env::var("VITRINE_RENDER").as_deref() != Ok("sw") {
            match RenderContext::create_gl(Arc::clone(&self.mpv), self.ctx.raw()) {
                Ok(render) => {
                    trace("render: OpenGL 渲染上下文已建立（IOSurface / NV12）");
                    self.render = Some(render);
                    return Ok(());
                }
                Err(detail) => trace(&format!("!! render: OpenGL 不可用，退回软件渲染（{detail}）")),
            }
        }
        match RenderContext::create(Arc::clone(&self.mpv), self.ctx.raw()) {
            Ok(render) => {
                trace("render: 软件渲染上下文已建立（sw / bgr0）");
                self.render = Some(render);
                Ok(())
            }
            Err(code) => {
                let detail = self.mpv.error_string(code);
                trace(&format!("!! render: 创建渲染上下文失败（{detail}）"));
                Err(if detail.is_empty() {
                    format!("mpv_render_context_create 失败（{code}）")
                } else {
                    detail
                })
            }
        }
    }

    pub fn has_render(&self) -> bool {
        self.render.is_some()
    }

    /// 画面走 GPU 通路（[`Self::render_surface`]）还是软件渲染（[`Self::render_frame`]）。
    pub fn renders_on_gpu(&self) -> bool {
        self.render.as_ref().is_some_and(RenderContext::is_gpu)
    }

    /// GPU 通路：把当前帧画成一块 NV12 的 `CVPixelBuffer`，直接交给界面合成。
    #[cfg(target_os = "macos")]
    pub fn render_surface(&mut self, width: usize, height: usize) -> Option<crate::PixelBuffer> {
        let render = self.render.as_mut()?;
        match render.render_surface(width, height) {
            Ok(buffer) => {
                self.last_render_error = None;
                render.report_swap();
                self.log_hwdec_once();
                Some(buffer)
            }
            Err(detail) => {
                if self.last_render_error.as_deref() != Some(detail.as_str()) {
                    trace(&format!("!! render: {detail}"));
                    self.last_render_error = Some(detail);
                }
                None
            }
        }
    }

    /// GPU 通路：最近一帧的缩小图（BGRA，长边 ≤ `max_long`），给液态玻璃用。
    #[cfg(target_os = "macos")]
    pub fn snapshot(&mut self, max_long: usize) -> Option<FrameRef<'_>> {
        let render = self.render.as_mut()?;
        render
            .snapshot(max_long, &mut self.buffer)
            .then(|| self.buffer.frame())
    }

    /// 每来一帧就调一次（mpv 的任意内部线程）。里面只能丢信号，不能碰 mpv API。
    pub fn set_frame_callback(&mut self, callback: Box<dyn Fn() + Send + Sync>) {
        if let Some(render) = self.render.as_mut() {
            render.set_update_callback(Some(callback));
        }
    }

    /// 把当前这一帧渲染进内部缓冲，返回只读视图（BGRA）。
    ///
    /// 尺寸变了会自动重配置 mpv 的缩放器；调用方按 [`crate::render::fit_render_size`]
    /// 算尺寸，不要每帧传窗口的物理尺寸（4K 窗里播 720p 会白白放大四倍）。
    pub fn render_frame(&mut self, width: usize, height: usize) -> Option<FrameRef<'_>> {
        let render = self.render.as_ref()?;
        self.buffer.ensure(width, height);
        if let Err(detail) = render.render(&mut self.buffer) {
            // 每帧都打日志会刷屏：只在错误文本变化时记一次
            if self.last_render_error.as_deref() != Some(detail.as_str()) {
                trace(&format!("!! render: {detail}"));
                self.last_render_error = Some(detail);
            }
            return None;
        }
        self.last_render_error = None;
        render.report_swap();
        self.log_hwdec_once();
        Some(self.buffer.frame())
    }
    /// 取走并清除「最近一次加载/解码失败」标记（UI 每轮轮询调用一次）。
    pub fn take_error(&self) -> Option<String> {
        let mut st = self.status.lock().ok()?;
        st.error.take()
    }

    /// 客户端是否仍然可用（未销毁、初始化成功）。
    pub fn is_connected(&self) -> bool {
        !self.quit.load(Ordering::SeqCst) && self.status().connected
    }

    /// 结束播放并销毁客户端。重复调用安全。
    pub fn shutdown(&mut self) {
        if self.destroyed {
            return;
        }
        self.destroyed = true;

        self.quit.store(true, Ordering::SeqCst);
        // 渲染上下文必须先于 mpv 销毁：`mpv_render_context_free` 会去碰已经没了的
        // 视频输出链（顺序反了是崩溃，不是报错）。
        drop(self.render.take());
        // 打断正在阻塞的 wait_event，事件线程立刻看到 quit 并退出
        self.mpv.wakeup(self.ctx.raw());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // 必须在事件线程退出之后销毁：否则它可能还在读已经释放的句柄
        self.mpv.terminate_destroy(self.ctx.raw());
        if let Ok(mut st) = self.status.lock() {
            st.connected = false;
        }
    }

    fn command(&self, args: &[&str]) {
        trace_command(args);
        if args.is_empty() {
            return;
        }
        let rc = self.mpv.command(self.ctx.raw(), args);
        if rc < 0 {
            trace(&format!(
                "!! 命令失败 {:?}：{}",
                args,
                self.mpv.error_string(rc)
            ));
        }
    }
}

impl Drop for VideoEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 事件线程：取事件 → 转 [`EngineEvent`] → 更新状态。
fn event_worker(
    mpv: Arc<LibMpv>,
    ctx: MpvCtx,
    status: Arc<Mutex<VideoStatus>>,
    quit: Arc<AtomicBool>,
) {
    // 诊断开关：`SMP_MPV_LOG=<路径>` 时把 mpv 的 error 级日志写进去。
    let mut log = std::env::var_os("SMP_MPV_LOG")
        .and_then(|path| std::fs::File::create(path).ok());

    while !quit.load(Ordering::SeqCst) {
        let event = mpv.wait_event(ctx.raw(), EVENT_WAIT_TIMEOUT);
        if event.is_null() {
            continue;
        }
        // SAFETY: `mpv_wait_event` 返回的指针在下次调用前有效，这里立刻读字段。
        let (event_id, error) = unsafe { ((*event).event_id, (*event).error) };
        let data = unsafe { (*event).data };
        if event_id == crate::libmpv::MPV_EVENT_NONE {
            continue;
        }

        // 日志事件先处理：它不属于播放状态
        if event_id == MPV_EVENT_LOG_MESSAGE {
            if let Some(file) = log.as_mut() {
                if !data.is_null() {
                    let msg = unsafe { &*(data as *const MpvEventLogMessage) };
                    let text = cstr(msg.text);
                    let prefix = cstr(msg.prefix);
                    let _ = writeln!(file, "[{prefix}] {text}");
                }
            }
            continue;
        }

        if let Some(engine_event) =
            unsafe { convert_event(event_id, error, data, &*mpv) }
        {
            apply_event(engine_event, &status);
        }
    }

    if let Ok(mut st) = status.lock() {
        st.connected = false;
    }
}

fn cstr(ptr: *const std::ffi::c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// `mpv_event` → [`EngineEvent`]。属性值为 `MPV_FORMAT_NONE` 时折成 [`PropValue::None`]。
///
/// SAFETY：调用方保证 `data` 与 `event_id` 匹配（mpv 的约定）。
unsafe fn convert_event(
    event_id: i32,
    error: i32,
    data: *mut std::ffi::c_void,
    mpv: &LibMpv,
) -> Option<EngineEvent> {
    match event_id {
        MPV_EVENT_START_FILE => Some(EngineEvent::StartFile),
        MPV_EVENT_FILE_LOADED => Some(EngineEvent::FileLoaded),
        MPV_EVENT_END_FILE => {
            if data.is_null() {
                return None;
            }
            let end = unsafe { &*(data as *const MpvEventEndFile) };
            let reason = if end.reason == MPV_END_FILE_REASON_ERROR {
                EndReason::Error
            } else if end.reason == crate::libmpv::MPV_END_FILE_REASON_EOF {
                EndReason::Eof
            } else if end.reason == crate::libmpv::MPV_END_FILE_REASON_STOP {
                EndReason::Stop
            } else if end.reason == MPV_END_FILE_REASON_QUIT {
                EndReason::Quit
            } else if end.reason == MPV_END_FILE_REASON_REDIRECT {
                EndReason::Redirect
            } else {
                EndReason::Other
            };
            let detail = if end.error < 0 {
                Some(mpv.error_string(end.error))
            } else {
                None
            };
            let _ = error;
            Some(EngineEvent::EndFile {
                reason,
                error: detail,
            })
        }
        MPV_EVENT_PROPERTY_CHANGE => {
            if data.is_null() {
                return None;
            }
            let prop = unsafe { &*(data as *const MpvEventProperty) };
            let name = cstr(prop.name);
            let value = unsafe { read_property_value(prop) };
            Some(EngineEvent::Property { name, value })
        }
        _ => None,
    }
}

unsafe fn read_property_value(prop: &MpvEventProperty) -> PropValue {
    match prop.format {
        crate::libmpv::MPV_FORMAT_NONE => PropValue::None,
        MPV_FORMAT_DOUBLE => {
            if prop.data.is_null() {
                PropValue::None
            } else {
                PropValue::Double(unsafe { *(prop.data as *const f64) })
            }
        }
        MPV_FORMAT_INT64 => {
            if prop.data.is_null() {
                PropValue::None
            } else {
                PropValue::Int(unsafe { *(prop.data as *const i64) })
            }
        }
        MPV_FORMAT_FLAG => {
            if prop.data.is_null() {
                PropValue::None
            } else {
                PropValue::Flag(unsafe { *(prop.data as *const i32) } != 0)
            }
        }
        MPV_FORMAT_STRING => {
            if prop.data.is_null() {
                PropValue::None
            } else {
                PropValue::Text(unsafe { cstr(*(prop.data as *const *const std::ffi::c_char)) })
            }
        }
        _ => PropValue::None,
    }
}

/// 把事件映射到状态。纯逻辑，测试直接构造 [`EngineEvent`] 驱动。
pub fn apply_event(event: EngineEvent, status: &Arc<Mutex<VideoStatus>>) {
    let Ok(mut st) = status.lock() else { return };
    match event {
        EngineEvent::Property { name, value } => match name.as_str() {
            "time-pos" => {
                if let PropValue::Double(pos) = value {
                    st.position = pos;
                }
            }
            "duration" => {
                if let PropValue::Double(dur) = value {
                    st.duration = dur;
                }
            }
            "pause" => {
                // mpv 在 idle 时 pause 可能是 None
                st.paused = matches!(value, PropValue::Flag(true));
            }
            "dwidth" => st.width = dimension(&value),
            "dheight" => st.height = dimension(&value),
            "idle-active" => {
                if let PropValue::Flag(idle) = value {
                    st.idle = idle;
                }
            }
            // ---- 视频信息卡 ----
            "video-codec" => st.video_codec = text(&value),
            "width" => st.source_width = dimension(&value),
            "height" => st.source_height = dimension(&value),
            "container-fps" => st.fps = number(&value),
            "video-bitrate" => st.video_bitrate = number(&value),
            "audio-bitrate" => st.audio_bitrate = number(&value),
            "video-params/primaries" => st.primaries = text(&value),
            "video-params/gamma" => st.gamma = text(&value),
            "audio-codec-name" => st.audio_codec = text(&value),
            "audio-params/channel-count" => st.audio_channels = dimension(&value),
            "audio-params/samplerate" => st.audio_rate = dimension(&value),
            // 「无字幕/无音轨」时 mpv 推的是 `no`（字符串），这里统一折成 0
            "sid" => st.sub_track = track_number(&value),
            "aid" => st.audio_track = track_number(&value),
            _ => {}
        },
        EngineEvent::StartFile => {
            st.finished = false;
            st.error = None;
            st.position = 0.0;
            // 上一部片子的尺寸不能留给下一部：UI 会据此把窗口调成画面比例，
            // 留着旧值就会先跳成上一部片的形状，再跳回来。
            st.width = 0;
            st.height = 0;
            // 信息卡同理：新片子还没解码出参数之前，卡上不能挂着上一部片的编码/
            // 码率/色彩——那种"看起来有值"的错数据比空白更难发现。
            st.video_codec.clear();
            st.source_width = 0;
            st.source_height = 0;
            st.fps = 0.0;
            st.video_bitrate = 0.0;
            st.audio_bitrate = 0.0;
            st.primaries.clear();
            st.gamma.clear();
            st.audio_codec.clear();
            st.audio_channels = 0;
            st.audio_rate = 0;
            st.sub_track = 0;
            st.audio_track = 0;
        }
        EngineEvent::FileLoaded => {
            st.idle = false;
            st.finished = false;
        }
        EngineEvent::EndFile { reason, error } => {
            st.finished = reason == EndReason::Eof;
            // eof/stop/quit/redirect 是正常收尾；error（以及未知原因）说明加载或解码失败。
            if !st.finished && !matches!(reason, EndReason::Stop | EndReason::Quit | EndReason::Redirect)
            {
                st.error = Some(match error {
                    Some(detail) if !detail.is_empty() => detail,
                    _ => match reason {
                        EndReason::Error => "error".to_string(),
                        _ => "unknown".to_string(),
                    },
                });
            }
        }
    }
}

/// 尺寸类属性折成 `u32`：mpv 未加载文件时可能给 0、负数或 None。
fn dimension(value: &PropValue) -> u32 {
    match value {
        PropValue::Int(v) if *v > 0 => (*v).min(u32::MAX as i64) as u32,
        PropValue::Double(v) if *v > 0.0 => *v as u32,
        _ => 0,
    }
}

fn number(value: &PropValue) -> f64 {
    match value {
        PropValue::Double(v) => *v,
        PropValue::Int(v) => *v as f64,
        _ => 0.0,
    }
}

/// 轨号：字符串 `no`/`auto` 一律当 0，数字串按整数解析。
fn track_number(value: &PropValue) -> u32 {
    match value {
        PropValue::Int(v) if *v > 0 => *v as u32,
        PropValue::Text(t) => t.trim().parse::<u32>().unwrap_or(0),
        _ => 0,
    }
}

fn text(value: &PropValue) -> String {
    match value {
        PropValue::Text(t) => t.clone(),
        _ => String::new(),
    }
}

/// 下游追踪：`SMP_MPV_TRACE=<路径>` 时把事件与命令逐行落盘。
///
/// 命令沿用子进程时代的 JSON 形状（`-> {"command":[...]}`），这样
/// `scripts/verify_floating_chrome.py` 里按 `"seek"` / `"volume"` / `"cycle"`
/// 数命令条数的判据不用改——它测的是「命令有没有真的发给 mpv」，与传输方式无关。
fn trace(line: &str) {
    use std::sync::OnceLock;

    static SINK: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    let sink = SINK.get_or_init(|| {
        std::env::var_os("SMP_MPV_TRACE").and_then(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
                .map(Mutex::new)
        })
    });
    if let Some(sink) = sink {
        if let Ok(mut file) = sink.lock() {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// 记录一条「发出的命令」（JSON 形状，兼容既有验收脚本）。
fn trace_command(args: &[&str]) {
    let items: Vec<String> = args.iter().map(|a| json_string(a)).collect();
    trace(&format!("-> {{\"command\":[{}]}}", items.join(",")));
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libmpv::find_libmpv;

    fn fresh_status() -> Arc<Mutex<VideoStatus>> {
        Arc::new(Mutex::new(VideoStatus::default()))
    }

    fn prop(status: &Arc<Mutex<VideoStatus>>, name: &str, value: PropValue) {
        apply_event(
            EngineEvent::Property {
                name: name.to_string(),
                value,
            },
            status,
        );
    }

    #[test]
    fn stream_url_detection() {
        assert!(is_stream_url("https://example.com/live.m3u8"));
        assert!(is_stream_url("RTMP://host/app/key"));
        assert!(!is_stream_url("C:\\movies\\a.mkv"), "盘符不是协议");
        assert!(!is_stream_url("C:/movies/a.mkv"));
        assert!(!is_stream_url("/Users/me/a.mkv"));
        assert!(!is_stream_url("https://"), "只有协议没有地址");
        assert!(!is_stream_url("file:///tmp/a.mkv"), "本地文件走正常路径");
    }

    #[test]
    fn video_extension_detection() {
        assert!(is_video_file(Path::new("movie.MP4")));
        assert!(is_video_file(Path::new("clip.mkv")));
        assert!(!is_video_file(Path::new("song.flac")));
        assert!(!is_video_file(Path::new("noext")));
    }

    /// end-file 的 error 原因要把 mpv 的错误文案带出来：只报一个 "error"
    /// 用户没法判断是解码不了还是文件不存在。
    #[test]
    fn end_file_error_exposes_detail() {
        let status = fresh_status();
        apply_event(
            EngineEvent::EndFile {
                reason: EndReason::Error,
                error: Some("Failed to recognize file format.".into()),
            },
            &status,
        );
        let st = status.lock().unwrap();
        assert!(!st.finished);
        assert_eq!(
            st.error.as_deref(),
            Some("Failed to recognize file format.")
        );
    }

    /// eof/stop/quit/redirect 都是正常收尾，不能弹错误。
    #[test]
    fn normal_end_reasons_are_not_errors() {
        for reason in [
            EndReason::Eof,
            EndReason::Stop,
            EndReason::Quit,
            EndReason::Redirect,
        ] {
            let status = fresh_status();
            apply_event(
                EngineEvent::EndFile {
                    reason,
                    error: None,
                },
                &status,
            );
            let st = status.lock().unwrap();
            assert_eq!(st.error, None, "{reason:?} 不该报错");
            assert_eq!(st.finished, reason == EndReason::Eof);
        }
    }

    /// error 原因但 mpv 没给文案时，退化成一个可读的兜底，而不是空串。
    #[test]
    fn end_file_error_without_detail_falls_back() {
        let status = fresh_status();
        apply_event(
            EngineEvent::EndFile {
                reason: EndReason::Error,
                error: None,
            },
            &status,
        );
        assert_eq!(status.lock().unwrap().error.as_deref(), Some("error"));
    }

    #[test]
    fn start_file_clears_previous_error() {
        let status = fresh_status();
        apply_event(
            EngineEvent::EndFile {
                reason: EndReason::Error,
                error: Some("boom".into()),
            },
            &status,
        );
        assert!(status.lock().unwrap().error.is_some());

        apply_event(EngineEvent::StartFile, &status);
        let st = status.lock().unwrap();
        assert_eq!(st.error, None);
        assert!(!st.finished);
    }

    #[test]
    fn display_size_is_tracked() {
        let status = fresh_status();
        prop(&status, "dwidth", PropValue::Int(1280));
        prop(&status, "dheight", PropValue::Int(720));
        {
            let st = status.lock().unwrap();
            assert_eq!((st.width, st.height), (1280, 720));
        }
        // 新片开始：上一部的尺寸必须清掉，否则窗口会先跳成上一部的形状
        apply_event(EngineEvent::StartFile, &status);
        let st = status.lock().unwrap();
        assert_eq!((st.width, st.height), (0, 0));
    }

    /// `ⓘ` 卡片依赖的属性都要能落到状态里；`sid`/`aid` 的 `no` 要折成 0。
    #[test]
    fn info_card_properties_are_tracked() {
        let status = fresh_status();
        prop(&status, "video-codec", PropValue::Text("h264 (High) / AVC".into()));
        prop(&status, "width", PropValue::Int(1920));
        prop(&status, "height", PropValue::Int(1080));
        prop(&status, "container-fps", PropValue::Double(23.976));
        prop(&status, "video-bitrate", PropValue::Double(4_400_000.0));
        prop(&status, "audio-bitrate", PropValue::Double(192_000.0));
        prop(&status, "video-params/primaries", PropValue::Text("bt.709".into()));
        prop(&status, "video-params/gamma", PropValue::Text("bt.1886".into()));
        prop(&status, "audio-codec-name", PropValue::Text("aac".into()));
        prop(&status, "audio-params/channel-count", PropValue::Int(2));
        prop(&status, "audio-params/samplerate", PropValue::Int(48000));
        prop(&status, "sid", PropValue::Text("1".into()));
        prop(&status, "aid", PropValue::Text("no".into()));

        let st = status.lock().unwrap().clone();
        assert_eq!(st.video_codec, "h264 (High) / AVC");
        assert_eq!((st.source_width, st.source_height), (1920, 1080));
        assert!((st.fps - 23.976).abs() < 1e-6);
        assert_eq!(st.video_bitrate, 4_400_000.0);
        assert_eq!(st.audio_bitrate, 192_000.0);
        assert_eq!(st.primaries, "bt.709");
        assert_eq!(st.gamma, "bt.1886");
        assert_eq!(st.audio_codec, "aac");
        assert_eq!(st.audio_channels, 2);
        assert_eq!(st.audio_rate, 48000);
        assert_eq!(st.sub_track, 1);
        assert_eq!(st.audio_track, 0, "`no` 应折成 0");

        // 新片开始：卡片上的旧值必须清空
        apply_event(EngineEvent::StartFile, &status);
        let st = status.lock().unwrap();
        assert!(st.video_codec.is_empty());
        assert_eq!((st.source_width, st.source_height), (0, 0));
        assert_eq!(st.fps, 0.0);
        assert_eq!(st.sub_track, 0);
    }

    /// 属性不可用（`MPV_FORMAT_NONE`）不能把已有的值冲掉，也不能 panic。
    #[test]
    fn unavailable_property_is_ignored() {
        let status = fresh_status();
        prop(&status, "duration", PropValue::Double(120.0));
        prop(&status, "duration", PropValue::None);
        assert_eq!(status.lock().unwrap().duration, 120.0);

        prop(&status, "sid", PropValue::None);
        assert_eq!(status.lock().unwrap().sub_track, 0);
    }

    #[test]
    fn find_libmpv_prefers_env_override() {
        let dir = std::env::temp_dir().join("smp-libmpv-find");
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("from-env.dll");
        std::fs::write(&fake, b"x").unwrap();

        std::env::set_var("SMP_LIBMPV_PATH", &fake);
        let found = find_libmpv(None);
        std::env::remove_var("SMP_LIBMPV_PATH");

        assert_eq!(found.as_deref(), Some(fake.as_path()));
    }

    /// 真 libmpv 集成测试：找不到 DLL 时跳过（不是失败——开发机不一定装了）。
    ///
    /// 查找顺序除了生产用的那几个位置，还额外看一眼 `target/mpv-dev/`：
    /// `scripts/fetch-libmpv.ps1` 会把官方 dev 构建解到那里，开发机不做额外配置
    /// 也能跑这条测试（它才是唯一真正验证「我们的调用打得进 mpv」的用例）。
    #[test]
    fn libmpv_controls_real_media() {
        let dev_copy = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("mpv-dev")
            .join(crate::LIBMPV_NAME);
        let Some(lib) = find_libmpv(None).or_else(|| dev_copy.is_file().then_some(dev_copy)) else {
            eprintln!("跳过：未找到 {}", crate::LIBMPV_NAME);
            return;
        };
        let media = std::env::temp_dir().join("smp-libmpv-test.wav");
        if !media.exists() {
            let _ = write_silent_wav(&media, 3.0);
        }
        if !media.exists() {
            eprintln!("跳过：无法生成测试素材");
            return;
        }

        let mut engine = VideoEngine::spawn(SpawnOptions {
            libmpv: lib,
            wid: None,
            volume: 0.5,
            extra_args: vec![
                "--ao=null".into(),
                "--video=no".into(),
                "--audio-display=no".into(),
            ],
        })
        .expect("libmpv 应该能初始化");

        engine.load(media.to_str().unwrap());
        assert!(
            wait_for(|| engine.status().duration > 0.5, 3000),
            "加载后应报出时长，实际 {:?}",
            engine.status()
        );

        engine.seek(1.0);
        assert!(
            wait_for(
                || (engine.status().position - 1.0).abs() < 0.6,
                3000
            ),
            "seek 后位置应接近 1.0，实际 {}",
            engine.status().position
        );

        engine.pause();
        assert!(
            wait_for(|| engine.status().paused, 2000),
            "pause 应生效"
        );

        engine.resume();
        assert!(
            wait_for(|| !engine.status().paused, 2000),
            "resume 应生效"
        );

        engine.stop();
        engine.shutdown();
        assert!(!engine.is_connected());
    }

    fn wait_for(mut cond: impl FnMut() -> bool, timeout_ms: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        while std::time::Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        cond()
    }

    /// GPU 通路端到端（mac）：上红下蓝的图 → mpv(OpenGL) → NV12 的 CVPixelBuffer。
    ///
    /// 一次验三件最容易错、错了界面上又只是「颜色怪 / 画面倒了」的事：
    /// - **方向**：IOSurface 第一行（Metal 眼里的顶）必须是画面的顶；
    /// - **换算**：Y / CbCr 是 BT.601 全范围（GPUI 的着色器按它逆回 RGB）；
    /// - **玻璃回读**：缩小图也是从上到下、BGRA。
    #[cfg(target_os = "macos")]
    #[test]
    fn gpu_surface_is_upright_nv12() {
        use objc2_core_video::{
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, CVPixelBufferGetBaseAddressOfPlane,
            CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight,
            CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
            CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
        };

        let Some(lib) = find_libmpv(None) else {
            eprintln!("跳过：未找到 {}", crate::LIBMPV_NAME);
            return;
        };
        let image = std::env::temp_dir().join(format!("smp-gl-{}.bmp", std::process::id()));
        write_split_bmp(&image, 64, 64).expect("写测试图");

        let mut engine = VideoEngine::spawn(SpawnOptions {
            libmpv: lib,
            wid: None,
            volume: 0.0,
            extra_args: vec![
                "--ao=null".into(),
                "--image-display-duration=inf".into(),
                // 缩放器不能把红蓝交界抹到采样点上：采样点离交界远，默认的也够
                "--scale=bilinear".into(),
            ],
        })
        .expect("libmpv 应该能初始化");
        engine.enable_render().expect("应能建立渲染上下文");
        assert!(engine.renders_on_gpu(), "mac 上默认应走 OpenGL 通路");

        let frames = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&frames);
        engine.set_frame_callback(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        engine.load(&image.to_string_lossy());
        assert!(
            wait_for(|| frames.load(Ordering::SeqCst) > 0, 8000),
            "update 回调应被触发"
        );

        // 第一帧可渲染之前 render 只是清屏（全黑），给一段重试窗口
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut planes = None;
        while std::time::Instant::now() < deadline {
            if let Some(buffer) = engine.render_surface(64, 64) {
                assert_eq!(
                    CVPixelBufferGetPixelFormatType(&buffer),
                    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                    "GPUI 只认 420f"
                );
                assert_eq!((CVPixelBufferGetWidth(&buffer), CVPixelBufferGetHeight(&buffer)), (64, 64));
                unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly) };
                let read = |plane: usize, x: usize, y: usize, bpp: usize| -> Vec<u8> {
                    let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane) as *const u8;
                    let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
                    (0..bpp)
                        .map(|i| unsafe { *base.add(y * stride + x * bpp + i) })
                        .collect()
                };
                // 采样点离红蓝交界（y=32）远：上 8 行、下 56 行；色度是半尺寸
                let sample = (
                    read(0, 32, 8, 1)[0],
                    read(0, 32, 56, 1)[0],
                    read(1, 16, 4, 2),
                    read(1, 16, 28, 2),
                );
                unsafe { CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly) };
                if sample.0 > 40 {
                    planes = Some(sample);
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
        let (y_top, y_bottom, c_top, c_bottom) = planes.expect("应在 8 秒内渲染出画面");
        // 纯红 Y=76 Cb=85 Cr=255；纯蓝 Y=29 Cb=255 Cr=107（BT.601 全范围）
        let near = |v: u8, want: i32| (v as i32 - want).abs() <= 6;
        assert!(near(y_top, 76) && near(y_bottom, 29), "Y 上 {y_top} 下 {y_bottom}（倒了的话会反过来）");
        assert!(near(c_top[0], 85) && near(c_top[1], 255), "上半 CbCr {c_top:?} 应是红");
        assert!(near(c_bottom[0], 255) && near(c_bottom[1], 107), "下半 CbCr {c_bottom:?} 应是蓝");

        let snap = engine.snapshot(16).expect("应能读回缩小图");
        assert_eq!((snap.width, snap.height), (16, 16), "64 → 长边 16 是第 2 级 mipmap");
        let px = |x: usize, y: usize| &snap.data[(y * snap.width + x) * 4..][..4];
        assert!(px(8, 1)[2] > 200 && px(8, 1)[0] < 60, "缩小图顶部应是红（BGRA）：{:?}", px(8, 1));
        assert!(px(8, 14)[0] > 200 && px(8, 14)[2] < 60, "缩小图底部应是蓝：{:?}", px(8, 14));
        assert_eq!(px(8, 1)[3], 255);

        engine.shutdown();
        let _ = std::fs::remove_file(&image);
    }

    /// 24 位 BMP，上半纯红、下半纯蓝（BMP 默认自下而上存行，这里用负高度存成自上而下）。
    #[cfg(target_os = "macos")]
    fn write_split_bmp(path: &Path, w: u32, h: u32) -> std::io::Result<()> {
        let row = (w * 3).div_ceil(4) * 4;
        let data_len = row * h;
        let mut out = Vec::with_capacity(54 + data_len as usize);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(54 + data_len).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(w as i32).to_le_bytes());
        out.extend_from_slice(&(-(h as i32)).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&[0u8; 24]);
        for y in 0..h {
            let bgr: [u8; 3] = if y < h / 2 { [0, 0, 255] } else { [255, 0, 0] };
            let start = out.len();
            for _ in 0..w {
                out.extend_from_slice(&bgr);
            }
            out.resize(start + row as usize, 0);
        }
        std::fs::write(path, out)
    }

    /// 写一段静音 WAV（16bit 单声道 8kHz），只为让 mpv 有个能解码的文件。
    fn write_silent_wav(path: &Path, secs: f64) -> std::io::Result<()> {
        let rate = 8000u32;
        let samples = (rate as f64 * secs) as u32;
        let data_len = samples * 2;
        let mut out = Vec::with_capacity(44 + data_len as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.extend(std::iter::repeat(0u8).take(data_len as usize));
        std::fs::write(path, out)
    }

    /// 渲染 API 端到端：建立 SW 上下文 → 有帧回调 → 真的拿到不透明的彩色像素。
    ///
    /// 这一段是「画面进了我们自己的合成树」的唯一硬证据：回调没来 = mpv 没出图；
    /// 像素全黑 = 缩放/格式参数错；alpha 全 0 = 忘了把 `bgr0` 的第 4 字节刷成不透明
    /// （表现出来就是「界面上一片空白」，而截图也是空的，很容易误判成没渲染）。
    #[test]
    fn render_api_delivers_opaque_pixels() {
        let dev_copy = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("mpv-dev")
            .join(crate::LIBMPV_NAME);
        let Some(lib) = find_libmpv(None).or_else(|| dev_copy.is_file().then_some(dev_copy)) else {
            eprintln!("跳过：未找到 {}", crate::LIBMPV_NAME);
            return;
        };
        let media = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("test-media")
            .join("gen-bars.mp4");
        if !media.is_file() {
            eprintln!("跳过：缺少 test-media/gen-bars.mp4");
            return;
        }

        let mut engine = VideoEngine::spawn(SpawnOptions {
            libmpv: lib,
            // 渲染 API 不能同时给 wid：给了就是两条输出互相打架
            wid: None,
            volume: 0.0,
            extra_args: vec!["--ao=null".into(), "--loop-file=inf".into()],
        })
        .expect("libmpv 应该能初始化");
        engine.enable_render().expect("应能建立软件渲染上下文");
        assert!(engine.has_render());

        let frames = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&frames);
        engine.set_frame_callback(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        engine.load(&media.to_string_lossy().replace('\\', "/"));
        assert!(
            wait_for(|| frames.load(Ordering::SeqCst) > 0, 8000),
            "update 回调应被触发（说明 mpv 出图了）"
        );

        // 首帧可能在回调之后几十毫秒才可渲染，给一段重试窗口（第一帧渲染出来之前
        // `mpv_render_context_render` 只是清屏，返回 0 但像素全黑）
        let mut got = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while std::time::Instant::now() < deadline {
            if let Some(frame) = engine.render_frame(320, 180) {
                let bright = frame
                    .band_mean()
                    .map(|(r, g, b)| r + g + b > 30.0)
                    .unwrap_or(false);
                if bright {
                    got = Some((
                        frame.width,
                        frame.height,
                        frame.data[3],
                        frame.band_hue(),
                        frame.band_thumbnail(8, 4).map(|t| t.len()),
                    ));
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(60));
        }

        let (w, h, alpha, hue, thumb) = got.expect("应在 8 秒内渲染出一帧有内容的彩色画面");
        assert_eq!((w, h), (320, 180), "渲染尺寸应等于请求的尺寸");
        assert_eq!(alpha, 255, "bgr0 的第 4 字节要刷成不透明");
        assert!(hue.is_some(), "彩条画面应能算出色相");
        assert_eq!(thumb, Some(8 * 4 * 4), "缩略图尺寸应固定");
    }
}
