use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

pub fn format_duration_secs(seconds: f64) -> String {
    let total_secs = seconds.max(0.0).round() as u64;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, secs)
    } else {
        format!("{:02}:{:02}", minutes, secs)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackRecord {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration_secs: Option<f64>,
    pub thumbnail_url: Option<String>,
}

impl TrackRecord {
    pub fn video_url(&self) -> String {
        if self.id.starts_with("http://") || self.id.starts_with("https://") {
            self.id.clone()
        } else {
            format!("https://www.youtube.com/watch?v={}", self.id)
        }
    }

    pub fn duration_label(&self) -> String {
        let Some(duration) = self.duration_secs else {
            return "--:--".into();
        };
        format_duration_secs(duration)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    #[default]
    Off,
    One,
    All,
}

impl RepeatMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "REPEAT OFF",
            Self::One => "REPEAT ONE",
            Self::All => "REPEAT ALL",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::One,
            Self::One => Self::All,
            Self::All => Self::Off,
        }
    }
}

fn default_volume() -> f32 {
    1.0
}

fn default_autoplay() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LibraryData {
    #[serde(default)]
    favorites: Vec<TrackRecord>,
    #[serde(default)]
    recent: Vec<TrackRecord>,
    #[serde(default)]
    repeat_mode: RepeatMode,
    #[serde(default)]
    shuffle_enabled: bool,
    #[serde(default = "default_volume")]
    volume: f32,
    #[serde(default)]
    muted: bool,
    #[serde(default = "default_autoplay")]
    autoplay_enabled: bool,
    #[serde(default)]
    always_on_top: bool,
    #[serde(default)]
    queue_expanded: bool,
    #[serde(default)]
    window_pos: Option<(i32, i32)>,
    #[serde(default)]
    last_track: Option<TrackRecord>,
    #[serde(default)]
    last_position_secs: f32,
    #[serde(default)]
    saved_queue: Vec<TrackRecord>,
}

impl Default for LibraryData {
    fn default() -> Self {
        Self {
            favorites: Vec::new(),
            recent: Vec::new(),
            repeat_mode: RepeatMode::Off,
            shuffle_enabled: false,
            volume: 1.0,
            muted: false,
            autoplay_enabled: true,
            always_on_top: false,
            queue_expanded: false,
            window_pos: None,
            last_track: None,
            last_position_secs: 0.0,
            saved_queue: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct LibraryStore {
    data: Arc<Mutex<LibraryData>>,
    path: Arc<PathBuf>,
}

impl LibraryStore {
    pub fn load() -> Self {
        Self::load_from_path(data_path())
    }

    fn load_from_path(path: PathBuf) -> Self {
        let bak_path = path.with_extension("json.bak");
        let data = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<LibraryData>(&bytes) {
                Ok(data) => data,
                Err(error) => {
                    warn!("Corrupted library.json detected ({:#}); backing up to .bak", error);
                    let _ = std::fs::copy(&path, &bak_path);
                    if let Ok(bak_bytes) = std::fs::read(&bak_path)
                        && let Ok(recovered) = serde_json::from_slice::<LibraryData>(&bak_bytes)
                    {
                        info!("Recovered library state from backup file.");
                        recovered
                    } else {
                        LibraryData::default()
                    }
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => LibraryData::default(),
            Err(error) => {
                warn!("Could not open saved music library: {:#}", error);
                LibraryData::default()
            }
        };
        Self {
            data: Arc::new(Mutex::new(data)),
            path: Arc::new(path),
        }
    }

    pub fn favorites(&self) -> Vec<TrackRecord> {
        self.data.lock().unwrap().favorites.clone()
    }

    pub fn recent(&self) -> Vec<TrackRecord> {
        self.data.lock().unwrap().recent.clone()
    }

    pub fn find_track(&self, id: &str) -> Option<TrackRecord> {
        let data = self.data.lock().unwrap();
        data.favorites
            .iter()
            .chain(data.recent.iter())
            .chain(data.saved_queue.iter())
            .chain(data.last_track.iter())
            .find(|t| t.id == id)
            .cloned()
    }

    pub fn is_favorite(&self, id: &str) -> bool {
        self.data
            .lock()
            .unwrap()
            .favorites
            .iter()
            .any(|track| track.id == id)
    }

    /// Toggle a favorite and return its new favorite state.
    pub fn toggle_favorite(&self, track: TrackRecord) -> bool {
        let mut data = self.data.lock().unwrap();
        if let Some(index) = data.favorites.iter().position(|saved| saved.id == track.id) {
            data.favorites.remove(index);
            self.save_locked(&data);
            false
        } else {
            data.favorites.insert(0, track);
            self.save_locked(&data);
            true
        }
    }

    pub fn record_played(&self, track: TrackRecord) {
        let mut data = self.data.lock().unwrap();
        data.recent.retain(|saved| saved.id != track.id);
        data.recent.insert(0, track.clone());
        data.recent.truncate(100);
        data.last_track = Some(track);
        self.save_locked(&data);
    }

    pub fn repeat_mode(&self) -> RepeatMode {
        self.data.lock().unwrap().repeat_mode
    }

    pub fn set_repeat_mode(&self, mode: RepeatMode) {
        let mut data = self.data.lock().unwrap();
        data.repeat_mode = mode;
        self.save_locked(&data);
    }

    pub fn shuffle_enabled(&self) -> bool {
        self.data.lock().unwrap().shuffle_enabled
    }

    pub fn set_shuffle_enabled(&self, enabled: bool) {
        let mut data = self.data.lock().unwrap();
        data.shuffle_enabled = enabled;
        self.save_locked(&data);
    }

    pub fn volume(&self) -> f32 {
        self.data.lock().unwrap().volume.clamp(0.0, 1.0)
    }

    pub fn is_muted(&self) -> bool {
        self.data.lock().unwrap().muted
    }

    pub fn set_volume_state(&self, volume: f32, muted: bool) {
        let mut data = self.data.lock().unwrap();
        let clamped = volume.clamp(0.0, 1.0);
        if (data.volume - clamped).abs() < 0.005 && data.muted == muted {
            return;
        }
        data.volume = clamped;
        data.muted = muted;
        self.save_locked(&data);
    }

    pub fn autoplay_enabled(&self) -> bool {
        self.data.lock().unwrap().autoplay_enabled
    }

    pub fn set_autoplay_enabled(&self, enabled: bool) {
        let mut data = self.data.lock().unwrap();
        data.autoplay_enabled = enabled;
        self.save_locked(&data);
    }

    pub fn always_on_top(&self) -> bool {
        self.data.lock().unwrap().always_on_top
    }

    pub fn set_always_on_top(&self, enabled: bool) {
        let mut data = self.data.lock().unwrap();
        data.always_on_top = enabled;
        self.save_locked(&data);
    }

    pub fn queue_expanded(&self) -> bool {
        self.data.lock().unwrap().queue_expanded
    }

    pub fn set_queue_expanded(&self, expanded: bool) {
        let mut data = self.data.lock().unwrap();
        if data.queue_expanded == expanded {
            return;
        }
        data.queue_expanded = expanded;
        self.save_locked(&data);
    }

    pub fn window_pos(&self) -> Option<(i32, i32)> {
        self.data.lock().unwrap().window_pos
    }

    pub fn set_window_pos(&self, x: i32, y: i32) {
        let mut data = self.data.lock().unwrap();
        if data.window_pos == Some((x, y)) {
            return;
        }
        data.window_pos = Some((x, y));
        self.save_locked(&data);
    }

    pub fn last_session(&self) -> (Option<TrackRecord>, f32, Vec<TrackRecord>) {
        let data = self.data.lock().unwrap();
        (
            data.last_track.clone(),
            data.last_position_secs,
            data.saved_queue.clone(),
        )
    }

    pub fn update_playback_position(&self, pos_secs: f32) {
        let mut data = self.data.lock().unwrap();
        if (data.last_position_secs - pos_secs).abs() < 2.0 {
            return;
        }
        data.last_position_secs = pos_secs.max(0.0);
        self.save_locked(&data);
    }

    pub fn save_queue(&self, items: Vec<TrackRecord>) {
        let mut data = self.data.lock().unwrap();
        if data.saved_queue == items {
            return;
        }
        data.saved_queue = items;
        self.save_locked(&data);
    }

    pub fn cover_path(&self, id: &str) -> PathBuf {
        let safe_id: String = id
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
            .collect();
        self.path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("covers")
            .join(format!("{}.png", safe_id))
    }

    pub fn store_cover(&self, id: &str, bytes: &[u8]) -> anyhow::Result<PathBuf> {
        let path = self.cover_path(id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let image = image::load_from_memory(bytes)?;
        let resized = if image.width() > 256 || image.height() > 256 {
            image.thumbnail(256, 256)
        } else {
            image
        };
        resized.save_with_format(&path, image::ImageFormat::Png)?;
        Ok(path)
    }

    /// Background non-blocking LRU disk cache pruning + legacy temp file cleanup.
    /// Does NOT decode PNG files on startup!
    pub fn prune_cache_async(&self, max_covers: usize) {
        let covers_dir = self
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("covers");
        let favorite_ids: std::collections::HashSet<String> = self
            .favorites()
            .into_iter()
            .map(|t| {
                t.id.chars()
                    .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
                    .collect()
            })
            .collect();

        std::thread::spawn(move || {
            // 1. Clean up any legacy %TEMP%\rs_yt_cover_*.png files
            if let Ok(temp_entries) = std::fs::read_dir(std::env::temp_dir()) {
                for entry in temp_entries.flatten() {
                    if let Some(name) = entry.file_name().to_str()
                        && name.starts_with("rs_yt_cover_")
                        && name.ends_with(".png")
                    {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }

            // 2. Evict oldest non-favorite cover files if count exceeds max_covers
            if !covers_dir.exists() {
                return;
            }
            let Ok(entries) = std::fs::read_dir(&covers_dir) else {
                return;
            };
            let mut files: Vec<(PathBuf, std::time::SystemTime)> = entries
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("png") {
                        return None;
                    }
                    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    if favorite_ids.contains(stem) {
                        return None;
                    }
                    let modified = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    Some((path, modified))
                })
                .collect();

            if files.len() > max_covers {
                files.sort_by_key(|(_, modified)| *modified);
                let to_remove = files.len() - max_covers;
                for (path, _) in files.into_iter().take(to_remove) {
                    let _ = std::fs::remove_file(path);
                }
            }
        });
    }

    fn save_locked(&self, data: &LibraryData) {
        let result = (|| -> anyhow::Result<()> {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp_path = self.path.with_extension("json.tmp");
            let bytes = serde_json::to_vec_pretty(data)?;
            std::fs::write(&tmp_path, &bytes)?;
            std::fs::rename(&tmp_path, self.path.as_ref())?;
            Ok(())
        })();
        if let Err(error) = result {
            warn!("Could not save music library atomically: {:#}", error);
        }
    }
}

fn data_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("LOCALAPPDATA"))
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("YouTubeAudioPlayer").join("library.json")
}

#[cfg(test)]
mod tests {
    use super::{LibraryStore, RepeatMode, TrackRecord, format_duration_secs};
    use std::path::PathBuf;

    struct TempLibraryDir(PathBuf);

    impl TempLibraryDir {
        fn new() -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "rs-yt-player-library-test-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create isolated library test directory");
            Self(path)
        }
    }

    impl Drop for TempLibraryDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample_track(id: &str) -> TrackRecord {
        TrackRecord {
            id: id.into(),
            title: format!("Track {id}"),
            artist: "Test artist".into(),
            duration_secs: Some(123.0),
            thumbnail_url: Some(format!("https://example.invalid/{id}.jpg")),
        }
    }

    #[test]
    fn favorites_history_session_and_modes_survive_atomic_reload() {
        let temp = TempLibraryDir::new();
        let path = temp.0.join("library.json");
        let store = LibraryStore::load_from_path(path.clone());
        store.toggle_favorite(sample_track("favorite"));
        store.record_played(sample_track("recent"));
        store.set_repeat_mode(RepeatMode::All);
        store.set_shuffle_enabled(true);
        store.set_volume_state(0.65, false);
        store.set_autoplay_enabled(false);
        store.set_always_on_top(true);
        store.update_playback_position(42.5);
        store.save_queue(vec![sample_track("queued-1")]);
        drop(store);

        let reloaded = LibraryStore::load_from_path(path);
        assert!(reloaded.is_favorite("favorite"));
        assert_eq!(reloaded.recent()[0].id, "recent");
        assert_eq!(reloaded.repeat_mode(), RepeatMode::All);
        assert!(reloaded.shuffle_enabled());
        assert!((reloaded.volume() - 0.65).abs() < 0.01);
        assert!(!reloaded.autoplay_enabled());
        assert!(reloaded.always_on_top());
        let (last_track, last_pos, saved_queue) = reloaded.last_session();
        assert_eq!(last_track.unwrap().id, "recent");
        assert!((last_pos - 42.5).abs() < 0.01);
        assert_eq!(saved_queue.len(), 1);
    }

    #[test]
    fn formats_multi_hour_and_standard_durations() {
        assert_eq!(format_duration_secs(65.0), "01:05");
        assert_eq!(format_duration_secs(11142.0), "03:05:42");
    }
}