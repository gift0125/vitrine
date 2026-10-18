//! AudioEngine：rodio 播放引擎封装。
//!
//! 职责：单文件播放（play/pause/stop/seek/volume）、当前曲目状态查询。
//! 播放队列、自动连播等逻辑由 UI 层基于 [`crate::TrackMeta`] 列表实现。

use std::fs::File;
use std::io::BufReader;
use std::time::Duration;

use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink};

use crate::PlaybackState;

/// 引擎错误（设备打开失败 / 解码失败等）
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("无法打开音频设备: {0}")]
    Device(String),
    #[error("无法打开文件: {0}")]
    OpenFile(String),
    #[error("解码失败: {0}")]
    Decode(String),
}

/// rodio 封装：一个 sink 串行播放当前曲目。
pub struct AudioEngine {
    /// rodio 要求输出流存活期间才能发声，字段仅为保活。
    _stream: OutputStream,
    sink: Sink,
    volume: f32,
    state: PlaybackState,
}

impl AudioEngine {
    /// 连接默认输出设备。失败（无设备/被占用）返回 [`EngineError::Device`]。
    pub fn new() -> Result<Self, EngineError> {
        let stream = OutputStreamBuilder::from_default_device()
            .map_err(|e| EngineError::Device(e.to_string()))?
            .open_stream()
            .map_err(|e| EngineError::Device(e.to_string()))?;
        let sink = Sink::connect_new(stream.mixer());
        Ok(Self {
            _stream: stream,
            sink,
            volume: 1.0,
            state: PlaybackState::Stopped,
        })
    }

    /// 停止当前曲目并播放指定文件。
    pub fn play(&mut self, path: &str) -> Result<(), EngineError> {
        let file = File::open(path).map_err(|e| EngineError::OpenFile(e.to_string()))?;
        let source =
            Decoder::new(BufReader::new(file)).map_err(|e| EngineError::Decode(e.to_string()))?;
        self.sink.stop();
        self.sink.set_volume(self.volume);
        self.sink.append(source);
        self.state = PlaybackState::Playing;
        Ok(())
    }

    pub fn pause(&mut self) {
        if self.state.is_playing() {
            self.sink.pause();
            self.state = PlaybackState::Paused;
        }
    }

    pub fn resume(&mut self) {
        if self.state == PlaybackState::Paused {
            self.sink.play();
            self.state = PlaybackState::Playing;
        }
    }

    pub fn stop(&mut self) {
        self.sink.stop();
        self.state = PlaybackState::Stopped;
    }

    /// 跳转到指定秒数。不支持 seek 的解码器返回 false。
    pub fn seek(&mut self, secs: f64) -> bool {
        if self.state == PlaybackState::Stopped {
            return false;
        }
        self.sink
            .try_seek(Duration::from_secs_f64(secs.max(0.0)))
            .is_ok()
    }

    /// 从指定秒数开始播放。
    ///
    /// 播到末尾后引擎是 `Stopped`，此时 [`Self::seek`] 是空操作（sink 已经空了，
    /// 没有可 seek 的东西）。用户按「后退」却什么都没发生，看起来就是功能坏了；
    /// 这条路径负责重新起播并落到目标位置。
    pub fn play_at(&mut self, path: &str, secs: f64) -> Result<(), EngineError> {
        self.play(path)?;
        if secs > 0.0 {
            // 刚 append 的源可能还没被解码线程接管，`try_seek` 会短暂失败；
            // 失败也不致命（等价于从头播），不能把整次播放判成错误。
            let _ = self.sink.try_seek(Duration::from_secs_f64(secs));
        }
        Ok(())
    }

    /// 音量，0.0 ~ 1.0，立即生效于当前曲目。
    pub fn set_volume(&mut self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        self.volume = volume;
        self.sink.set_volume(volume);
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// 当前播放位置（秒）。停止时为 0。
    pub fn position_secs(&self) -> f64 {
        self.sink.get_pos().as_secs_f64()
    }

    pub fn state(&self) -> PlaybackState {
        self.state
    }

    /// 当前曲目是否已播完（用于自动连播判断）。
    pub fn track_finished(&self) -> bool {
        self.state.is_playing() && self.sink.empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::tests_support::{write_test_wav, write_test_wav_with};

    fn engine_or_skip() -> Option<AudioEngine> {
        match AudioEngine::new() {
            Ok(engine) => Some(engine),
            Err(err) => {
                eprintln!("SKIP: 无音频设备（{err}），跳过引擎测试");
                None
            }
        }
    }

    #[test]
    fn volume_clamps_and_persists() {
        let Some(mut engine) = engine_or_skip() else {
            return;
        };
        engine.set_volume(0.5);
        assert_eq!(engine.volume(), 0.5);
        engine.set_volume(3.0);
        assert_eq!(engine.volume(), 1.0);
        engine.set_volume(-1.0);
        assert_eq!(engine.volume(), 0.0);
    }

    #[test]
    fn play_pause_resume_stop_cycle() {
        let Some(mut engine) = engine_or_skip() else {
            return;
        };
        let wav = write_test_wav(2.0);

        engine.play(wav.to_str().unwrap()).expect("play wav");
        assert_eq!(engine.state(), PlaybackState::Playing);

        std::thread::sleep(Duration::from_millis(400));
        let pos = engine.position_secs();
        assert!(pos > 0.2, "播放 400ms 后位置应前进，实际 {pos}");

        engine.pause();
        assert_eq!(engine.state(), PlaybackState::Paused);
        engine.resume();
        assert_eq!(engine.state(), PlaybackState::Playing);

        engine.stop();
        assert_eq!(engine.state(), PlaybackState::Stopped);
    }

    #[test]
    fn seek_moves_position() {
        let Some(mut engine) = engine_or_skip() else {
            return;
        };
        let wav = write_test_wav(4.0);

        engine.play(wav.to_str().unwrap()).expect("play wav");
        std::thread::sleep(Duration::from_millis(200));

        let ok = engine.seek(2.0);
        assert!(ok, "WAV 解码器应支持 seek");
        std::thread::sleep(Duration::from_millis(100));
        let pos = engine.position_secs();
        assert!(
            (1.9..=2.6).contains(&pos),
            "seek 到 2s 后位置应在 1.9~2.6，实际 {pos}"
        );
    }

    #[test]
    fn play_at_starts_near_requested_position() {
        let Some(mut engine) = engine_or_skip() else {
            return;
        };
        let wav = write_test_wav(8.0);

        engine
            .play_at(wav.to_str().unwrap(), 4.0)
            .expect("play_at wav");
        std::thread::sleep(Duration::from_millis(300));
        let pos = engine.position_secs();
        assert!(
            (3.8..=4.9).contains(&pos),
            "play_at(4.0) 应从 4s 附近开始，实际 {pos}"
        );
        assert_eq!(engine.state(), PlaybackState::Playing);
    }

    #[test]
    fn play_missing_file_fails_cleanly() {
        let Some(mut engine) = engine_or_skip() else {
            return;
        };
        let err = engine.play("Z:/不存在/坏文件.mp3").unwrap_err();
        assert!(matches!(err, EngineError::OpenFile(_)));
        assert_eq!(engine.state(), PlaybackState::Stopped);
    }

    #[test]
    fn track_finished_reports_true_after_end() {
        let Some(mut engine) = engine_or_skip() else {
            return;
        };
        // 0.2 秒的极短 WAV
        let wav = write_test_wav_with(0.2);
        engine.play(wav.to_str().unwrap()).expect("play wav");
        assert!(!engine.track_finished());
        std::thread::sleep(Duration::from_millis(700));
        assert!(engine.track_finished(), "短曲目播完后应报告 finished");
    }
}
