//! 曲目元数据：lofty 解析 + 扩展名过滤 + 目录递归扫描。

use std::path::{Path, PathBuf};

use lofty::file::AudioFile;
use lofty::prelude::*;
use lofty::tag::Accessor;

use crate::TrackMeta;

/// 本应用支持的音频扩展名（rodio 默认 features 支持的解码器集合）。
const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "ogg", "oga", "m4a", "mp4", "aac", "aiff",
];

/// 路径是否为受支持的音频文件（按扩展名判断）。
pub fn is_supported_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| SUPPORTED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// 解析单个音频文件的元数据。解析失败时回退为「文件名 + 未知时长」。
pub fn load_track_meta(path: &Path) -> TrackMeta {
    let fallback_title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());

    let mut meta = TrackMeta {
        title: fallback_title,
        artist: None,
        duration_secs: None,
        path: path.to_string_lossy().into_owned(),
    };

    match lofty::read_from_path(path) {
        Ok(tagged_file) => {
            meta.duration_secs = Some(tagged_file.properties().duration().as_secs_f64());
            let tag = tagged_file
                .primary_tag()
                .or_else(|| tagged_file.first_tag());
            if let Some(tag) = tag {
                if let Some(title) = tag.title() {
                    let title = title.trim();
                    if !title.is_empty() {
                        meta.title = title.to_string();
                    }
                }
                if let Some(artist) = tag.artist() {
                    let artist = artist.trim();
                    if !artist.is_empty() {
                        meta.artist = Some(artist.to_string());
                    }
                }
            }
        }
        Err(err) => {
            // 元数据读取失败不致命：保留文件名与未知时长
            eprintln!("lofty 解析失败 {}: {err}", path.display());
        }
    }

    meta
}

/// 递归扫描目录中的音频文件（跳过无法读取的子目录），按路径排序。
pub fn scan_dir_for_tracks(dir: &Path) -> Vec<TrackMeta> {
    let mut files = Vec::new();
    collect_audio_files(dir, &mut files, 0);
    files.sort();
    files.iter().map(|p| load_track_meta(p)).collect()
}

fn collect_audio_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 16 {
        return; // 防符号链接环
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_audio_files(&path, out, depth + 1);
        } else if is_supported_audio(&path) {
            out.push(path);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use std::io::Write;
    use std::path::PathBuf;

    /// 生成一个指定秒长的正弦波 16-bit PCM 单声道 WAV，返回临时文件路径。
    pub fn write_test_wav_with(secs: f64) -> PathBuf {
        let sample_rate: u32 = 8000;
        let total_samples = (sample_rate as f64 * secs) as u32;
        let data_len = total_samples * 2;

        let mut buf = Vec::with_capacity(44 + data_len as usize);
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&(36 + data_len).to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        // fmt chunk
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
        buf.extend_from_slice(&1u16.to_le_bytes()); // mono
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
        buf.extend_from_slice(&2u16.to_le_bytes()); // block align
        buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
                                                     // data chunk
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..total_samples {
            let t = i as f64 / sample_rate as f64;
            let sample = (t * 440.0 * std::f64::consts::TAU).sin() * 0.5 * i16::MAX as f64;
            buf.extend_from_slice(&(sample as i16).to_le_bytes());
        }

        let mut path = std::env::temp_dir();
        path.push(format!(
            "player-core-test-{}-{secs}.wav",
            std::process::id()
        ));
        let mut f = std::fs::File::create(&path).expect("create test wav");
        f.write_all(&buf).expect("write test wav");
        path
    }

    pub fn write_test_wav(secs: f64) -> PathBuf {
        write_test_wav_with(secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tests_support::{write_test_wav, write_test_wav_with};

    #[test]
    fn extension_filter() {
        assert!(is_supported_audio(Path::new("a/b/song.MP3")));
        assert!(is_supported_audio(Path::new("a/b/song.flac")));
        assert!(is_supported_audio(Path::new("a/b/song.wav")));
        assert!(is_supported_audio(Path::new("a/b/song.ogg")));
        assert!(!is_supported_audio(Path::new("a/b/song.txt")));
        assert!(!is_supported_audio(Path::new("a/b/song")));
        assert!(!is_supported_audio(Path::new("a/b/cover.png")));
    }

    #[test]
    fn meta_from_wav_has_duration_and_fallback_title() {
        let wav = write_test_wav(2.0);
        let meta = load_track_meta(&wav);
        let duration = meta.duration_secs.expect("wav 时长应可解析");
        assert!(
            (duration - 2.0).abs() < 0.05,
            "时长应约 2s，实际 {duration}"
        );
        // 无 tag 的 WAV 应回退到文件名
        assert!(
            meta.title.contains("player-core-test"),
            "title={}",
            meta.title
        );
        assert_eq!(meta.artist, None);
        std::fs::remove_file(&wav).ok();
    }

    #[test]
    fn meta_from_missing_file_falls_back() {
        let meta = load_track_meta(Path::new("Z:/不存在/坏 文件.mp3"));
        assert_eq!(meta.title, "坏 文件");
        assert_eq!(meta.duration_secs, None);
    }

    #[test]
    fn scan_dir_finds_audio_only() {
        let dir = std::env::temp_dir().join(format!("player-core-scan-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).expect("mkdir");
        let wav1 = write_test_wav_with(0.1);
        let wav2 = write_test_wav_with(0.2);
        std::fs::copy(&wav1, dir.join("a.wav")).expect("copy");
        std::fs::copy(&wav2, dir.join("sub/b.wav")).expect("copy");
        std::fs::write(dir.join("note.txt"), "not audio").expect("write");

        let tracks = scan_dir_for_tracks(&dir);
        let names: Vec<&str> = tracks
            .iter()
            .map(|t| Path::new(&t.path).file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, vec!["a.wav", "b.wav"], "应递归找到 2 个 wav");

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_file(&wav1).ok();
        std::fs::remove_file(&wav2).ok();
    }
}
