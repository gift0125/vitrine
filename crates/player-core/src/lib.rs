//! player-core: 播放内核（无 UI 依赖）
//!
//! - [`AudioEngine`]：rodio 封装的音频播放引擎
//! - [`VideoEngine`]：进程内 libmpv 的视频播放内核
//! - [`render`]：软件渲染（把 mpv 的画面交给宿主自己合成）
//! - [`TrackMeta`]：曲目元数据（lofty 解析）
//! - 目录扫描 / 扩展名过滤等纯数据工具

mod audio;
#[cfg(target_os = "macos")]
mod gl_render;
mod libmpv;
mod metadata;
mod render;
mod video;

pub use audio::{AudioEngine, EngineError};
#[cfg(target_os = "macos")]
pub use gl_render::{pixel_buffer_size, PixelBuffer};
pub use metadata::{is_supported_audio, load_track_meta, scan_dir_for_tracks};
pub use libmpv::{find_libmpv, LibMpv, LIBMPV_NAME};
pub use render::{fit_gpu_render_size, fit_render_size, FrameBuffer, FrameRef, RenderContext};
pub use video::{is_stream_url, is_video_file, SpawnOptions, VideoEngine, VideoError, VideoStatus};

/// 曲目元数据
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TrackMeta {
    pub title: String,
    pub artist: Option<String>,
    pub duration_secs: Option<f64>,
    pub path: String,
}

/// 播放状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

impl PlaybackState {
    pub fn is_playing(self) -> bool {
        self == Self::Playing
    }
}
