//! 开发用：手动驱动 VideoEngine 的 IPC 序列（不经过测试框架）。
//!
//! 运行：`cargo run -p player-core --example mpv_ipc [媒体文件路径]`
//!
//! 用途：验证 mpv 定位、IPC 握手、播放/暂停/seek/音量/停止链路是否正常。
//! 不带参数时自动生成一段 6 秒静音 WAV。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use player_core::{find_mpv, SpawnOptions, VideoEngine};

fn main() {
    let path = match std::env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => {
            let mut p = std::env::temp_dir();
            p.push("mpv-ipc-demo.wav");
            write_wav(&p, 6.0);
            p
        }
    };

    let mpv = find_mpv().expect("未找到 mpv.exe");
    println!("mpv  = {}", mpv.display());
    println!("file = {}", path.display());

    let mut engine = VideoEngine::spawn(SpawnOptions {
        mpv_path: mpv,
        wid: None,
        volume: 0.0,
        extra_args: vec!["--ao=null".into()],
    })
    .expect("spawn mpv");
    println!("pipe = {}", engine.pipe_name());

    engine.load(&path.to_string_lossy());

    for i in 0..10 {
        std::thread::sleep(Duration::from_millis(200));
        println!("t{i:02} {:?}", engine.status());
    }

    engine.pause();
    std::thread::sleep(Duration::from_millis(300));
    println!("pause -> {:?}", engine.status());

    // 关键回归点：暂停之后继续下发命令（历史上会永久阻塞）
    let t = Instant::now();
    engine.set_volume(0.6);
    engine.resume();
    engine.seek(3.0);
    println!("resume+seek+volume 下发耗时 {:?}", t.elapsed());

    for i in 0..6 {
        std::thread::sleep(Duration::from_millis(200));
        println!("s{i:02} {:?}", engine.status());
    }

    engine.stop();
    std::thread::sleep(Duration::from_millis(200));
    println!("stop -> {:?}", engine.status());

    let t = Instant::now();
    engine.shutdown();
    println!("shutdown 耗时 {:?}", t.elapsed());
}

fn write_wav(path: &std::path::Path, secs: f64) {
    let rate: u32 = 8000;
    let n = (rate as f64 * secs) as u32;
    let data_len = n * 2;
    let mut buf = Vec::new();
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&rate.to_le_bytes());
    buf.extend_from_slice(&(rate * 2).to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        let t = i as f64 / rate as f64;
        let s = (t * 440.0 * std::f64::consts::TAU).sin() * 0.5 * i16::MAX as f64;
        buf.extend_from_slice(&(s as i16).to_le_bytes());
    }
    std::fs::write(path, buf).ok();
}
