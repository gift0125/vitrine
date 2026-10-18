//! 视频播放内核：以子进程 + JSON IPC 驱动 mpv。
//!
//! ## 为什么不是 libmpv
//!
//! Windows 上没有可用的 libmpv 动态库（本机 mpv 为静态单文件构建），而 GPUI 的 Windows
//! 后端使用 DirectComposition（`WS_EX_NOREDIRECTIONBITMAP`），无法在其窗口内承载原生子窗口。
//!
//! ## 采用的方案
//!
//! 「mpv 渲染进应用创建的顶层叠加窗口（`--wid`）+ JSON IPC 控制」：
//!
//! - **解码 / 渲染 / 音视频同步**全部交给 mpv（`vo=gpu` + `hwdec=auto-safe`），
//!   Rust 侧不做任何像素搬运；
//! - **控制通道**走 mpv 的 JSON IPC 命名管道（`--input-ipc-server`）。
//!
//! ## IPC 线程模型（踩过的坑）
//!
//! Windows 命名管道上，同一连接的**阻塞读**与**写**会互相串行：只要有一个 `ReadFile`
//! 挂起等待数据，后续 `WriteFile` 就会被阻塞。实测（mpv 暂停后不再推送事件时）表现为
//! 「发出 pause 之后，下一条命令永久卡死」。
//!
//! 因此这里**不**使用阻塞读 + 写双线程，而是单个 IPC 线程串行处理：
//! 用 `PeekNamedPipe` 非阻塞探测可读字节 → 有数据才 `read` → 再排空命令队列写管道。
//! 任何时刻都不会有挂起的读操作，写永远不会被读阻塞。

use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 视频文件扩展名（由 mpv 解码，与音频走不同路径）。
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "mov", "avi", "flv", "wmv", "ts", "m2ts", "mpg", "mpeg", "3gp",
    "ogv", "rmvb",
];

/// IPC 空闲轮询间隔（无数据、无命令时）。
const IPC_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// 路径是否为视频文件（按扩展名判断）。
pub fn is_video_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// 视频内核错误。
#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    #[error("未找到 mpv 可执行文件（可用环境变量 SMP_MPV_PATH 指定）")]
    MpvNotFound,
    #[error("无法启动 mpv: {0}")]
    Spawn(String),
    #[error("无法连接 mpv IPC 管道（{0}）")]
    Ipc(String),
}

/// 视频播放状态快照（由 IPC 线程写入，UI 线程读取）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoStatus {
    /// 当前播放位置（秒）
    pub position: f64,
    /// 总时长（秒），未知为 0
    pub duration: f64,
    /// mpv 当前是否暂停
    pub paused: bool,
    /// 未加载任何文件
    pub idle: bool,
    /// 已自然播放结束（end-file reason=eof）
    pub finished: bool,
    /// IPC 连接是否仍然有效
    pub connected: bool,
}

/// mpv 启动参数。
#[derive(Debug, Clone)]
pub struct SpawnOptions {
    pub mpv_path: PathBuf,
    /// 叠加窗口句柄（Windows HWND）。`None` 时使用 `--vo=null`（无窗口，用于测试）。
    pub wid: Option<isize>,
    /// 初始音量（0.0~1.0）
    pub volume: f32,
    /// 追加参数（测试用，例如 `--ao=null`）
    pub extra_args: Vec<String>,
}

/// 定位 mpv 可执行文件：环境变量 → PATH → 常见安装位置。
pub fn find_mpv() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SMP_MPV_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let exe = dir.join("mpv.exe");
            if exe.is_file() {
                return Some(exe);
            }
        }
    }

    const CANDIDATES: &[&str] = &[
        r"C:\ProgramData\chocolatey\lib\mpvio.install\tools\mpv.exe",
        r"C:\Program Files\mpv\mpv.exe",
        r"C:\Program Files (x86)\mpv\mpv.exe",
    ];
    CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// IPC 线程消息。
enum IpcMessage {
    Command(serde_json::Value),
    Quit,
}

/// mpv 子进程 + IPC 控制端。
pub struct VideoEngine {
    child: Child,
    tx: Option<mpsc::Sender<IpcMessage>>,
    status: Arc<Mutex<VideoStatus>>,
    worker: Option<JoinHandle<()>>,
    exited: Arc<AtomicBool>,
    pipe_name: String,
}

impl VideoEngine {
    /// 启动 mpv 并连接 IPC。
    pub fn spawn(options: SpawnOptions) -> Result<Self, VideoError> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pipe_name = format!(
            r"\\.\pipe\simple-media-player-mpv-{}-{seq}",
            std::process::id()
        );

        let mut cmd = Command::new(&options.mpv_path);
        cmd.arg("--no-config")
            .arg("--idle=yes")
            .arg("--force-window=yes")
            .arg("--keep-open=no")
            .arg("--osc=no")
            .arg("--osd-level=0")
            .arg("--no-input-default-bindings")
            .arg("--hwdec=auto-safe")
            .arg("--msg-level=all=error")
            .arg(format!(
                "--volume={}",
                options.volume.clamp(0.0, 1.0) * 100.0
            ))
            .arg(format!("--input-ipc-server={pipe_name}"));

        match options.wid {
            Some(wid) => {
                cmd.arg(format!("--wid={wid}"));
            }
            None => {
                cmd.arg("--vo=null");
            }
        }
        for extra in &options.extra_args {
            cmd.arg(extra);
        }

        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let mut child = cmd.spawn().map_err(|e| VideoError::Spawn(e.to_string()))?;

        // mpv 需要一点时间创建管道服务端：轮询连接，最多等 2.5s
        let deadline = Instant::now() + Duration::from_millis(2500);
        let file = loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&pipe_name)
            {
                Ok(file) => break file,
                Err(err) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        return Err(VideoError::Ipc(err.to_string()));
                    }
                    std::thread::sleep(Duration::from_millis(40));
                }
            }
        };

        let status = Arc::new(Mutex::new(VideoStatus {
            connected: true,
            idle: true,
            ..Default::default()
        }));
        let exited = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<IpcMessage>();

        let worker = {
            let status = status.clone();
            let exited = exited.clone();
            std::thread::spawn(move || {
                ipc_worker(file, rx, status);
                exited.store(true, Ordering::SeqCst);
            })
        };

        let engine = Self {
            child,
            tx: Some(tx),
            status,
            worker: Some(worker),
            exited,
            pipe_name,
        };

        // 订阅进度/状态属性
        for (id, name) in [
            (1, "time-pos"),
            (2, "duration"),
            (3, "pause"),
            (4, "idle-active"),
        ] {
            engine.send(serde_json::json!({ "command": ["observe_property", id, name] }));
        }

        Ok(engine)
    }

    /// 已创建的 IPC 管道名（便于诊断）。
    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    /// 加载并播放文件（替换当前文件）。
    pub fn load(&self, path: &str) {
        self.send(serde_json::json!({ "command": ["loadfile", path, "replace"] }));
    }

    pub fn pause(&self) {
        self.set_paused(true);
    }

    pub fn resume(&self) {
        self.set_paused(false);
    }

    pub fn set_paused(&self, paused: bool) {
        self.send(serde_json::json!({ "command": ["set_property", "pause", paused] }));
    }

    /// 绝对定位（秒）。
    pub fn seek(&self, secs: f64) {
        self.send(serde_json::json!({ "command": ["seek", secs.max(0.0), "absolute"] }));
    }

    /// 设置音量（0.0~1.0）。
    pub fn set_volume(&self, volume: f32) {
        let pct = volume.clamp(0.0, 1.0) * 100.0;
        self.send(serde_json::json!({ "command": ["set_property", "volume", pct] }));
    }

    /// 停止当前文件（回到 idle，保留进程）。
    pub fn stop(&self) {
        self.send(serde_json::json!({ "command": ["stop"] }));
    }

    /// 状态快照。
    pub fn status(&self) -> VideoStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// IPC 是否仍然可用（管道未断开、进程未退出）。
    pub fn is_connected(&self) -> bool {
        !self.exited.load(Ordering::SeqCst) && self.status().connected
    }

    /// mpv 进程是否仍在运行。
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// 结束 mpv 进程（先发 quit，超时后强杀）。
    pub fn shutdown(&mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        // 先请求退出（IPC 线程会写出这条命令）
        if !self.exited.load(Ordering::SeqCst) {
            self.send(serde_json::json!({ "command": ["quit"] }));
        }
        // 唤醒 IPC 线程：请求退出并断开命令通道
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(IpcMessage::Quit);
            drop(tx);
        }

        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(30)),
                Err(_) => break,
            }
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();

        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.exited.store(true, Ordering::SeqCst);
        if let Ok(mut st) = self.status.lock() {
            st.connected = false;
        }
    }

    fn send(&self, value: serde_json::Value) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(IpcMessage::Command(value));
        }
    }
}

impl Drop for VideoEngine {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

/// IPC 线程：串行执行「写命令 → 探测可读 → 读取解析」，避免阻塞读与写互相串行。
fn ipc_worker(
    file: std::fs::File,
    rx: mpsc::Receiver<IpcMessage>,
    status: Arc<Mutex<VideoStatus>>,
) {
    let mut reader = BufReader::new(file);
    let mut pending = String::new();
    let mut buf = [0u8; 8192];

    loop {
        let mut did_work = false;

        // 1) 写命令（此时不会有挂起的读操作）
        loop {
            match rx.try_recv() {
                Ok(IpcMessage::Command(value)) => {
                    let mut line = value.to_string();
                    line.push('\n');
                    if reader.get_mut().write_all(line.as_bytes()).is_err() {
                        mark_disconnected(&status);
                        return;
                    }
                    did_work = true;
                }
                Ok(IpcMessage::Quit) => {
                    mark_disconnected(&status);
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    mark_disconnected(&status);
                    return;
                }
            }
        }

        // 2) 非阻塞探测可读字节，有数据才读（不会让读操作挂起）
        match pipe_available(reader.get_ref()) {
            Ok(0) => {}
            Ok(_) => match reader.read(&mut buf) {
                Ok(0) => {
                    mark_disconnected(&status);
                    return;
                }
                Ok(n) => {
                    did_work = true;
                    pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                    while let Some(idx) = pending.find('\n') {
                        let line: String = pending.drain(..=idx).collect();
                        handle_line(line.trim(), &status);
                    }
                }
                Err(_) => {
                    mark_disconnected(&status);
                    return;
                }
            },
            Err(_) => {
                mark_disconnected(&status);
                return;
            }
        }

        if !did_work {
            std::thread::sleep(IPC_POLL_INTERVAL);
        }
    }
}

fn mark_disconnected(status: &Arc<Mutex<VideoStatus>>) {
    if let Ok(mut st) = status.lock() {
        st.connected = false;
    }
}

/// 解析单行 mpv 事件并更新状态。
fn handle_line(line: &str, status: &Arc<Mutex<VideoStatus>>) {
    if line.is_empty() {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    let Some(event) = value.get("event").and_then(|e| e.as_str()) else {
        return; // 命令回执（含 request_id），忽略
    };

    let Ok(mut st) = status.lock() else { return };
    match event {
        "property-change" => {
            let name = value.get("name").and_then(|n| n.as_str()).unwrap_or_default();
            let data = value.get("data").cloned().unwrap_or(serde_json::Value::Null);
            match name {
                "time-pos" => {
                    if let Some(pos) = data.as_f64() {
                        st.position = pos;
                    }
                }
                "duration" => {
                    if let Some(dur) = data.as_f64() {
                        st.duration = dur;
                    }
                }
                "pause" => {
                    // mpv 在 idle 时 pause 可能为 null
                    st.paused = data.as_bool().unwrap_or(false);
                }
                "idle-active" => {
                    if let Some(idle) = data.as_bool() {
                        st.idle = idle;
                    }
                }
                _ => {}
            }
        }
        "start-file" => {
            st.finished = false;
            st.position = 0.0;
        }
        "file-loaded" => {
            st.idle = false;
            st.finished = false;
        }
        "end-file" => {
            let reason = value.get("reason").and_then(|r| r.as_str()).unwrap_or_default();
            st.finished = reason == "eof";
        }
        _ => {}
    }
}

/// 非阻塞探测管道中可读字节数（Windows 用 `PeekNamedPipe`）。
#[cfg(windows)]
fn pipe_available(file: &std::fs::File) -> std::io::Result<u32> {
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    extern "system" {
        fn PeekNamedPipe(
            handle: *mut std::ffi::c_void,
            buffer: *mut std::ffi::c_void,
            buffer_size: u32,
            bytes_read: *mut u32,
            total_bytes_avail: *mut u32,
            bytes_left: *mut u32,
        ) -> i32;
        fn GetLastError() -> u32;
    }

    let mut available: u32 = 0;
    let ok = unsafe {
        PeekNamedPipe(
            file.as_raw_handle(), // RawHandle == HANDLE == *mut c_void
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(std::io::Error::from_raw_os_error(err as i32));
    }
    Ok(available)
}

#[cfg(not(windows))]
fn pipe_available(_file: &std::fs::File) -> std::io::Result<u32> {
    // 非 Windows 平台暂无命名管道等价实现：返回 1 让调用方按阻塞读处理
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::tests_support::write_test_wav;
    use std::time::Instant;

    #[test]
    fn video_extension_detection() {
        assert!(is_video_file(Path::new("a/b/movie.MP4")));
        assert!(is_video_file(Path::new("a/b/movie.mkv")));
        assert!(is_video_file(Path::new("a/b/movie.webm")));
        assert!(!is_video_file(Path::new("a/b/song.flac")));
        assert!(!is_video_file(Path::new("a/b/song.mp3")));
        assert!(!is_video_file(Path::new("a/b/noext")));
    }

    #[test]
    fn find_mpv_prefers_env_override() {
        let fake = std::env::temp_dir().join(format!("fake-mpv-{}.exe", std::process::id()));
        std::fs::write(&fake, b"stub").expect("write stub");
        let previous = std::env::var_os("SMP_MPV_PATH");
        std::env::set_var("SMP_MPV_PATH", &fake);
        let found = find_mpv();
        match previous {
            Some(v) => std::env::set_var("SMP_MPV_PATH", v),
            None => std::env::remove_var("SMP_MPV_PATH"),
        }
        std::fs::remove_file(&fake).ok();
        assert_eq!(found.as_deref(), Some(fake.as_path()));
    }

    /// 端到端 IPC 测试：真实 mpv 进程 + WAV（无窗口、无音频输出设备依赖）。
    ///
    /// 覆盖「暂停后继续下发命令」这一曾经卡死的历史缺陷。
    #[test]
    fn ipc_controls_real_mpv() {
        let Some(mpv) = find_mpv() else {
            eprintln!("跳过：本机未安装 mpv");
            return;
        };
        let wav = write_test_wav(4.0);

        let mut engine = VideoEngine::spawn(SpawnOptions {
            mpv_path: mpv,
            wid: None,
            volume: 0.0,
            extra_args: vec!["--ao=null".into()],
        })
        .expect("启动 mpv 并连接 IPC");
        engine.load(&wav.to_string_lossy());

        // 1) 时长被观测到
        assert!(
            wait_for(|| engine.status().duration > 3.5, 8000),
            "应观测到时长 ≈4s，实际 {:?}",
            engine.status()
        );

        // 2) 播放位置前进
        assert!(
            wait_for(|| engine.status().position > 0.3, 8000),
            "播放位置应前进，实际 {:?}",
            engine.status()
        );

        // 3) 暂停生效且位置冻结
        engine.pause();
        assert!(
            wait_for(|| engine.status().paused, 3000),
            "pause 属性应为 true，实际 {:?}",
            engine.status()
        );
        let paused_at = engine.status().position;
        std::thread::sleep(Duration::from_millis(400));
        let after = engine.status().position;
        assert!(
            (after - paused_at).abs() < 0.3,
            "暂停后位置不应前进：{paused_at} -> {after}"
        );

        // 4) 暂停之后继续下发命令不得阻塞（历史缺陷回归）
        engine.set_volume(0.5);
        engine.resume();
        engine.seek(2.0);
        assert!(
            wait_for(|| (engine.status().position - 2.0).abs() < 0.8, 5000),
            "seek 到 2.0s 失败，实际 {:?}",
            engine.status()
        );
        assert!(
            wait_for(|| !engine.status().paused, 2000),
            "resume 应清除暂停状态"
        );

        engine.shutdown();
        std::fs::remove_file(&wav).ok();
    }

    fn wait_for(mut cond: impl FnMut() -> bool, timeout_ms: u64) -> bool {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        cond()
    }
}
