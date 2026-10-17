//! player-core: 播放内核（无 UI 依赖）
//!
//! Stage 2 将在此实现 AudioEngine（rodio 封装）与 TrackMeta（lofty 元数据解析）。

/// 曲目元数据（Stage 2 填充完整字段）
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
