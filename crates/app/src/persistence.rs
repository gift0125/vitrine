//! 播放列表与设置的持久化。
//!
//! - `%APPDATA%/simple-media-player/playlist.json`：曲目列表 + 上次播放位置
//! - `%APPDATA%/simple-media-player/settings.json`：音量、窗口尺寸

use std::path::PathBuf;

use player_core::TrackMeta;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct PlaylistFile {
    pub tracks: Vec<TrackMeta>,
    pub current_index: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// 0.0 ~ 1.0
    pub volume: f32,
    pub window_width: f32,
    pub window_height: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.7,
            window_width: 1040.0,
            window_height: 680.0,
        }
    }
}

/// 配置目录：Windows 上为 `%APPDATA%/simple-media-player`（与计划约定一致）。
/// 其他平台回退 directories 标准路径。
pub fn config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("simple-media-player")
    }
    #[cfg(not(target_os = "windows"))]
    {
        directories::ProjectDirs::from("", "", "simple-media-player")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

fn ensure_config_dir() -> std::io::Result<PathBuf> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn load_playlist() -> Option<PlaylistFile> {
    let path = config_dir().join("playlist.json");
    let content = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&content) {
        Ok(file) => Some(file),
        Err(err) => {
            eprintln!("播放列表反序列化失败，忽略: {err}");
            None
        }
    }
}

pub fn save_playlist(tracks: &[TrackMeta], current_index: Option<usize>) {
    let Ok(dir) = ensure_config_dir() else {
        return;
    };
    let file = PlaylistFile {
        tracks: tracks.to_vec(),
        current_index,
    };
    let Ok(out) = std::fs::File::create(dir.join("playlist.json")) else {
        return;
    };
    if let Err(err) = serde_json::to_writer_pretty(out, &file) {
        eprintln!("播放列表保存失败: {err}");
    }
}

pub fn load_settings() -> Settings {
    let path = config_dir().join("settings.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

pub fn save_settings(settings: &Settings) {
    let Ok(dir) = ensure_config_dir() else {
        return;
    };
    let Ok(out) = std::fs::File::create(dir.join("settings.json")) else {
        return;
    };
    if let Err(err) = serde_json::to_writer_pretty(out, settings) {
        eprintln!("设置保存失败: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 直接覆盖真实配置目录太危险，这里只验证序列化往返。
    #[test]
    fn playlist_roundtrip() {
        let tracks = vec![TrackMeta {
            title: "晴天".into(),
            artist: Some("周杰伦".into()),
            duration_secs: Some(269.0),
            path: "C:/music/晴天.flac".into(),
        }];
        let file = PlaylistFile {
            tracks,
            current_index: Some(0),
        };
        let json = serde_json::to_string(&file).unwrap();
        let back: PlaylistFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tracks.len(), 1);
        assert_eq!(back.tracks[0].title, "晴天");
        assert_eq!(back.current_index, Some(0));
    }

    #[test]
    fn settings_default_and_roundtrip() {
        let settings = Settings::default();
        let json = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.volume, settings.volume);
        assert_eq!(back.window_width, settings.window_width);
    }
}
