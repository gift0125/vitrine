//! 播放列表与设置的持久化。
//!
//! - `%APPDATA%/simple-media-player/playlist.json`：曲目列表 + 上次播放位置
//! - `%APPDATA%/simple-media-player/settings.json`：音量、窗口尺寸
//! - `%APPDATA%/simple-media-player/recent.json`：最近打开 + 各自停在哪（首屏用）

use std::path::PathBuf;

use player_core::TrackMeta;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct PlaylistFile {
    pub tracks: Vec<TrackMeta>,
    pub current_index: Option<usize>,
}

/// 循环模式（设计稿面板底部那一枚「循环」）。
///
/// 默认 `All`：旧版 `skip()` 一直用 `rem_euclid` 首尾相接，行为不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    /// 播完最后一首就停
    Off,
    /// 列表循环
    #[default]
    All,
    /// 单曲循环
    One,
}

impl RepeatMode {
    /// 点一下切到下一档：关 → 列表 → 单曲 → 关。
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "循环：关",
            Self::All => "循环：列表",
            Self::One => "循环：单曲",
        }
    }

    pub fn is_on(self) -> bool {
        self != Self::Off
    }
}

/// 播放列表排序方式（设计稿面板底部的「排序」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    /// 加入顺序（默认，等价于「不排序」）
    #[default]
    Added,
    /// 按标题（中文按字符序，够用且可预测）
    Title,
    /// 按时长从短到长
    Duration,
}

impl SortOrder {
    pub fn next(self) -> Self {
        match self {
            Self::Added => Self::Title,
            Self::Title => Self::Duration,
            Self::Duration => Self::Added,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "排序：加入顺序",
            Self::Title => "排序：标题",
            Self::Duration => "排序：时长",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// 0.0 ~ 1.0
    pub volume: f32,
    pub window_width: f32,
    pub window_height: f32,
    /// 循环模式（老配置文件没有这个字段 → 取 `All`，和旧行为一致）
    #[serde(default)]
    pub repeat: RepeatMode,
    /// 随机播放
    #[serde(default)]
    pub shuffle: bool,
    /// 播放列表排序
    #[serde(default)]
    pub sort: SortOrder,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.7,
            window_width: 1040.0,
            window_height: 680.0,
            repeat: RepeatMode::All,
            shuffle: false,
            sort: SortOrder::Added,
        }
    }
}

/// 配置目录：Windows 上为 `%APPDATA%/simple-media-player`（与计划约定一致）。
/// 其他平台回退 directories 标准路径。
///
/// 环境变量 `SMP_DATA_DIR` 可以整体顶掉它（便携模式 / 自动化）：
/// 截图和拖放验证会起真实进程，若不隔离就会把用户正在用的播放列表改掉。
pub fn config_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(dir) = test_dir_override::current() {
        return dir;
    }

    if let Some(dir) = std::env::var_os("SMP_DATA_DIR") {
        return PathBuf::from(dir);
    }

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

/// 测试专用：把配置目录重定向到临时目录。
///
/// 用线程局部存储而不是环境变量：Rust 测试默认多线程并行，改环境变量会串味；
/// `TestAppContext` 的每个用例都跑在自己的线程上，线程局部天然隔离。
#[cfg(test)]
mod test_dir_override {
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        static DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    pub fn current() -> Option<PathBuf> {
        DIR.with(|slot| slot.borrow().clone())
    }

    pub fn set(dir: impl Into<PathBuf>) {
        DIR.with(|slot| *slot.borrow_mut() = Some(dir.into()));
    }
}

/// 测试专用：后续所有持久化读写都落在这个目录里（不会碰到真实 `%APPDATA%`）。
#[cfg(test)]
pub fn set_config_dir_for_test(dir: impl Into<PathBuf>) {
    test_dir_override::set(dir);
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

// ----------------------------------------------------------------------
// 最近打开（首屏的「继续上次」与「最近打开」）
// ----------------------------------------------------------------------

/// 最多记多少条。首屏只露 4 条，多留一些是为了文件被删 / 挪走之后还有得补。
pub const RECENT_LIMIT: usize = 20;
/// 看了不到这么久不算「看到一半」，首屏不提供「继续上次」。
const RESUME_MIN_SECS: f64 = 10.0;
/// 离结尾不到这么久就当看完了：片尾字幕里续播没有意义。
const RESUME_TAIL_SECS: f64 = 15.0;

/// 最近打开过的一条媒体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentEntry {
    /// 文件路径（分隔符统一成 `/`）或串流 URL
    pub path: String,
    pub title: String,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    /// 上次停在哪（秒）。从没播过 / 已经播完都是 0。
    #[serde(default)]
    pub position_secs: f64,
}

impl RecentEntry {
    /// 这一条值得「从上次的位置接着放」。
    pub fn resumable(&self) -> bool {
        self.position_secs >= RESUME_MIN_SECS
            && self
                .duration_secs
                .is_none_or(|total| self.position_secs < total - RESUME_TAIL_SECS)
    }
}

/// `recent.json`：最近打开的在最前。
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    pub entries: Vec<RecentEntry>,
}

fn recent_key(path: &str) -> String {
    path.replace('\\', "/")
}

impl Recent {
    /// 打开了一条：挪到最前（按路径去重）。已经记下的进度保留，续播要用。
    pub fn touch(&mut self, path: &str, title: &str, duration_secs: Option<f64>) {
        let key = recent_key(path);
        let mut entry = match self.entries.iter().position(|e| e.path == key) {
            Some(i) => self.entries.remove(i),
            None => RecentEntry {
                path: key,
                title: String::new(),
                duration_secs: None,
                position_secs: 0.0,
            },
        };
        entry.title = title.to_string();
        if duration_secs.is_some() {
            entry.duration_secs = duration_secs;
        }
        self.entries.insert(0, entry);
        self.entries.truncate(RECENT_LIMIT);
    }

    /// 记下播放进度。返回值：有没有变（变了才值得落盘）。
    pub fn record_position(&mut self, path: &str, position_secs: f64, duration_secs: Option<f64>) -> bool {
        let key = recent_key(path);
        let Some(entry) = self.entries.iter_mut().find(|e| e.path == key) else {
            return false;
        };
        let position_secs = position_secs.max(0.0);
        let duration_secs = duration_secs.filter(|d| *d > 0.0).or(entry.duration_secs);
        let changed = (entry.position_secs - position_secs).abs() >= 1.0
            || entry.duration_secs != duration_secs;
        entry.position_secs = position_secs;
        entry.duration_secs = duration_secs;
        changed
    }

    /// 某个文件上次停在哪；没有可续的进度返回 `None`。
    #[cfg(test)]
    pub fn resume_point(&self, path: &str) -> Option<f64> {
        let key = recent_key(path);
        self.entries
            .iter()
            .find(|e| e.path == key)
            .filter(|e| e.resumable())
            .map(|e| e.position_secs)
    }

    /// 首屏「继续上次」：只看**最近那一条**，而且得真的看到一半。
    pub fn continue_last(&self) -> Option<&RecentEntry> {
        self.entries.first().filter(|e| e.resumable())
    }

    /// 首屏「最近打开」列表：「继续上次」已经单独摆出来的那条不再重复。
    pub fn list(&self) -> impl Iterator<Item = &RecentEntry> {
        let skip = usize::from(self.continue_last().is_some());
        self.entries.iter().skip(skip)
    }
}

pub fn load_recent() -> Recent {
    let path = config_dir().join("recent.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

pub fn save_recent(recent: &Recent) {
    let Ok(dir) = ensure_config_dir() else {
        return;
    };
    let Ok(out) = std::fs::File::create(dir.join("recent.json")) else {
        return;
    };
    if let Err(err) = serde_json::to_writer_pretty(out, recent) {
        eprintln!("最近打开保存失败: {err}");
    }
}

// ----------------------------------------------------------------------
// 会话日志
// ----------------------------------------------------------------------

/// 追加一行会话日志到 `%APPDATA%/simple-media-player/session.log`。
///
/// 存在意义：M2 期间观察到一次「空闲约 13 分钟后进程自行退出、无崩溃无 stderr」，
/// 无法复现。启动 / 退出请求 / 事件循环返回 / panic 全部留痕后，下次复现即可区分
/// 「GPUI 主动退出」与「被外部终止」。
///
/// 文件超过 256 KiB 时整体丢弃重写，避免长期使用无限增长。
pub fn log_session(event: &str) {
    let Ok(dir) = ensure_config_dir() else {
        return;
    };
    let path = dir.join("session.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            let _ = std::fs::remove_file(&path);
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let _ = writeln!(
            file,
            "[{}] pid={} {}",
            utc_timestamp(),
            std::process::id(),
            event
        );
        let _ = file.flush();
    }
}

/// UTC 时间戳 `YYYY-MM-DD HH:MM:SSZ`（不引入 chrono，用民用日历换算）。
fn utc_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant 的 `civil_from_days`：Unix 纪元起的天数 → (年, 月, 日)。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
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

    #[test]
    fn recent_moves_to_front_and_keeps_progress() {
        let mut recent = Recent::default();
        recent.touch("C:\\v\\a.mkv", "a.mkv", Some(3000.0));
        recent.touch("C:/v/b.mp4", "b.mp4", None);
        assert!(recent.record_position("C:/v/a.mkv", 1200.0, None));
        // 不到 1 秒不值得写盘，但内存里的进度照样跟上
        assert!(!recent.record_position("C:/v/a.mkv", 1200.4, None), "不到 1 秒不算变");

        // 再打开 a：回到最前，进度还在（续播要用），分隔符不同也算同一条
        recent.touch("C:\\v\\a.mkv", "a.mkv", None);
        let paths: Vec<_> = recent.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["C:/v/a.mkv", "C:/v/b.mp4"]);
        assert_eq!(recent.entries[0].duration_secs, Some(3000.0));
        assert_eq!(recent.resume_point("C:/v/a.mkv"), Some(1200.4));
    }

    #[test]
    fn continue_last_only_offers_a_half_watched_latest_entry() {
        let mut recent = Recent::default();
        recent.touch("b.mp4", "b.mp4", Some(600.0));
        recent.touch("a.mkv", "a.mkv", Some(3000.0));

        // 刚开头 / 已到片尾：都不该提示「继续上次」，列表里也不该少一条
        for pos in [3.0, 2990.0] {
            recent.record_position("a.mkv", pos, None);
            assert!(recent.continue_last().is_none(), "pos={pos}");
            assert_eq!(recent.list().count(), 2, "pos={pos}");
        }

        recent.record_position("a.mkv", 2537.0, None);
        assert_eq!(recent.continue_last().map(|e| e.path.as_str()), Some("a.mkv"));
        let rest: Vec<_> = recent.list().map(|e| e.path.as_str()).collect();
        assert_eq!(rest, ["b.mp4"], "「继续上次」那条不在列表里重复");
    }

    #[test]
    fn recent_is_capped() {
        let mut recent = Recent::default();
        for i in 0..(RECENT_LIMIT + 5) {
            recent.touch(&format!("{i}.mp4"), "x", None);
        }
        assert_eq!(recent.entries.len(), RECENT_LIMIT);
        assert_eq!(recent.entries[0].path, format!("{}.mp4", RECENT_LIMIT + 4));
    }

    #[test]
    fn civil_dates_from_unix_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(1), (1970, 1, 2));
        assert_eq!(civil_from_days(365), (1971, 1, 1));
        // 2026-10-18 = 1970-01-01 起第 20744 天
        assert_eq!(civil_from_days(20_744), (2026, 10, 18));
        // 闰日
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
    }

    #[test]
    fn timestamp_shape() {
        let ts = utc_timestamp();
        // "YYYY-MM-DD HH:MM:SSZ" = 20 字符
        assert_eq!(ts.len(), 20, "格式应为 YYYY-MM-DD HH:MM:SSZ，实际 {ts}");
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], " ");
        assert!(ts[..4].parse::<u32>().is_ok(), "年份不可解析: {ts}");
    }
}
