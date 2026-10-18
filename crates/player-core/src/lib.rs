//! player-core: 播放内核（无 UI 依赖）
//!
//! - [`AudioEngine`]：rodio 封装的音频播放引擎
//! - [`VideoEngine`]：mpv 子进程 + JSON IPC 的视频播放内核
//! - [`TrackMeta`]：曲目元数据（lofty 解析）
//! - 目录扫描 / 扩展名过滤等纯数据工具

mod audio;
mod metadata;
mod video;

pub use audio::{AudioEngine, EngineError};
pub use metadata::{is_supported_audio, load_track_meta, scan_dir_for_tracks};
pub use video::{find_mpv, is_video_file, SpawnOptions, VideoEngine, VideoError, VideoStatus};

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
