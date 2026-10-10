use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::warn;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrackRecord {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration_secs: Option<f64>,
    pub thumbnail_url: Option<String>,
}

impl TrackRecord {
    pub fn video_url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.id)
    }

    pub fn duration_label(&self) -> String {
        let Some(duration) = self.duration_secs else {
            return "--:--".into();
        };
        let seconds = duration.max(0.0) as u64;
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct LibraryData {
    #[serde(default)]
    favorites: Vec<TrackRecord>,
    #[serde(default)]
    recent: Vec<TrackRecord>,
    #[serde(default)]
    repeat_mode: RepeatMode,
    #[serde(default)]
    shuffle_enabled: bool,
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
        let data = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<LibraryData>(&bytes) {
                Ok(data) => data,
                Err(error) => {
                    warn!("Could not read saved music library: {:#}", error);
                    LibraryData::default()
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
        data.recent.insert(0, track);
        data.recent.truncate(100);
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

    pub fn store_cover(&self, id: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let path = self.cover_path(id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let image = image::load_from_memory(bytes)?;
        image.save_with_format(path, image::ImageFormat::Png)?;
        Ok(())
    }

    fn save_locked(&self, data: &LibraryData) {
        let result = (|| -> anyhow::Result<()> {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = serde_json::to_vec_pretty(data)?;
            std::fs::write(self.path.as_ref(), bytes)?;
            Ok(())
        })();
        if let Err(error) = result {
            warn!("Could not save music library: {:#}", error);
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
    use super::{LibraryStore, RepeatMode, TrackRecord};
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
    fn favorites_history_and_modes_survive_reload() {
        let temp = TempLibraryDir::new();
        let path = temp.0.join("library.json");
        let store = LibraryStore::load_from_path(path.clone());
        store.toggle_favorite(sample_track("favorite"));
        store.record_played(sample_track("recent"));
        store.set_repeat_mode(RepeatMode::All);
        store.set_shuffle_enabled(true);
        drop(store);

        let reloaded = LibraryStore::load_from_path(path);
        assert!(reloaded.is_favorite("favorite"));
        assert_eq!(reloaded.recent()[0].id, "recent");
        assert_eq!(reloaded.repeat_mode(), RepeatMode::All);
        assert!(reloaded.shuffle_enabled());
    }
}
