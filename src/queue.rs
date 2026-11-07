use crate::extractor::{AudioMetadata, Extractor, PlaylistItem, shared_http_client};
use crate::library::{LibraryStore, RepeatMode, TrackRecord};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tracing::info;

fn shuffled_index(len: usize) -> usize {
    if len <= 1 {
        return 0;
    }
    static SHUFFLE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let time_seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let sequence = SHUFFLE_SEQUENCE.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    let mut value = time_seed ^ sequence;
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 31;
    ((value as u128 * len as u128) >> 64) as usize
}

/// Normalize a track query by stripping transient resume parameters (`&t=...`)
/// while preserving standard YouTube watch URLs or plain test identifiers.
pub fn canonical_query(query: &str) -> String {
    if !query.contains("list=")
        && let Some(vid) = crate::ui_models::requested_video_id(query)
        && !vid.is_empty()
    {
        return format!("https://www.youtube.com/watch?v={}", vid);
    }
    query.to_string()
}

fn same_track(a: &str, b: &str) -> bool {
    crate::ui_models::query_video_id(a) == crate::ui_models::query_video_id(b)
}

#[derive(Clone, Debug)]
pub struct PreloadedTrack {
    pub query: String,
    pub meta: AudioMetadata,
    pub cover_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct QueueItem {
    pub query: String,
    pub title: String,
    pub artist: String,
    pub duration_secs: Option<f64>,
    pub thumbnail_url: Option<String>,
}

#[derive(Clone, Default)]
struct PlaylistState {
    tracks: Vec<String>,
    current_index: Option<usize>,
}

#[derive(Clone, Default)]
pub struct PlayQueue {
    state: Arc<Mutex<PlaylistState>>,
    preloaded: Arc<Mutex<Option<PreloadedTrack>>>,
    is_preloading: Arc<Mutex<bool>>,
    titles: Arc<Mutex<HashMap<String, QueueItem>>>,
    repeat_mode: Arc<Mutex<RepeatMode>>,
    shuffle_enabled: Arc<Mutex<bool>>,
    autoplay_enabled: Arc<Mutex<bool>>,
}

impl PlayQueue {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(PlaylistState::default())),
            preloaded: Arc::new(Mutex::new(None)),
            is_preloading: Arc::new(Mutex::new(false)),
            titles: Arc::new(Mutex::new(HashMap::new())),
            repeat_mode: Arc::new(Mutex::new(RepeatMode::Off)),
            shuffle_enabled: Arc::new(Mutex::new(false)),
            autoplay_enabled: Arc::new(Mutex::new(true)),
        }
    }

    /// Append a track to the end of the playlist (idempotent: avoids duplicate entries).
    pub fn push_back(&self, item: String) {
        let item = canonical_query(&item);
        let mut state = self.state.lock().unwrap();
        if !state.tracks.iter().any(|q| same_track(q, &item)) {
            state.tracks.push(item);
        }
    }

    /// Insert a track immediately after the currently playing track ("Play Next").
    /// If the track already exists in the playlist, moves it to the next position without duplicating.
    pub fn push_front(&self, item: String) {
        let item = canonical_query(&item);
        {
            let mut state = self.state.lock().unwrap();
            if let Some(existing_idx) = state.tracks.iter().position(|q| same_track(q, &item)) {
                if state.current_index == Some(existing_idx) {
                    return;
                }
                state.tracks.remove(existing_idx);
                if let Some(curr) = state.current_index
                    && existing_idx < curr
                {
                    state.current_index = Some(curr - 1);
                }
            }
            let insert_pos = state
                .current_index
                .map(|c| (c + 1).min(state.tracks.len()))
                .unwrap_or(0);
            state.tracks.insert(insert_pos, item);
        }
        if let Ok(mut pre_lock) = self.preloaded.lock() {
            *pre_lock = None;
        }
    }

    /// Replace the playlist with a single starting track.
    pub fn begin_repeat_cycle(&self, current: String) {
        let current = canonical_query(&current);
        let mut state = self.state.lock().unwrap();
        state.tracks = vec![current];
        state.current_index = Some(0);
        if let Ok(mut pre_lock) = self.preloaded.lock() {
            *pre_lock = None;
        }
    }

    /// Peek the next upcoming track in the playlist for gapless preloading.
    pub fn peek_front(&self) -> Option<String> {
        let state = self.state.lock().unwrap();
        if state.tracks.is_empty() {
            return None;
        }
        match state.current_index {
            Some(curr) if curr + 1 < state.tracks.len() => Some(state.tracks[curr + 1].clone()),
            Some(_) if self.repeat_mode() == RepeatMode::All => state.tracks.first().cloned(),
            None => state.tracks.first().cloned(),
            _ => None,
        }
    }

    /// Set the active cursor to `query`. If `query` is already in the playlist, moves the
    /// cursor to its existing index (never duplicates). If not present, appends it and points to it.
    pub fn record_current(&self, query: String) {
        let query = canonical_query(&query);
        let mut state = self.state.lock().unwrap();
        if let Some(idx) = state.tracks.iter().position(|q| same_track(q, &query)) {
            state.current_index = Some(idx);
        } else {
            state.tracks.push(query);
            state.current_index = Some(state.tracks.len() - 1);
        }
    }

    pub fn set_track_info(
        &self,
        query: &str,
        title: String,
        artist: String,
        duration_secs: Option<f64>,
        thumbnail_url: Option<String>,
    ) {
        let key = canonical_query(query);
        self.titles.lock().unwrap().insert(
            key.clone(),
            QueueItem {
                query: key,
                title,
                artist,
                duration_secs,
                thumbnail_url,
            },
        );
    }

    pub fn to_track_records(&self) -> Vec<TrackRecord> {
        self.all_playlist_items()
            .into_iter()
            .map(|item| {
                let id = crate::ui_models::query_video_id(&item.query);
                TrackRecord {
                    id,
                    title: item.title,
                    artist: item.artist,
                    duration_secs: item.duration_secs,
                    thumbnail_url: item.thumbnail_url,
                }
            })
            .collect()
    }

    pub fn restore_saved_queue(&self, records: Vec<TrackRecord>) {
        let mut state = self.state.lock().unwrap();
        let mut titles = self.titles.lock().unwrap();
        state.tracks.clear();
        state.current_index = None;
        for record in records {
            let query = canonical_query(&record.video_url());
            titles.insert(
                query.clone(),
                QueueItem {
                    query: query.clone(),
                    title: record.title,
                    artist: record.artist,
                    duration_secs: record.duration_secs,
                    thumbnail_url: record.thumbnail_url,
                },
            );
            if !state.tracks.iter().any(|q| same_track(q, &query)) {
                state.tracks.push(query);
            }
        }
    }

    pub fn current_query(&self) -> Option<String> {
        let state = self.state.lock().unwrap();
        state
            .current_index
            .and_then(|idx| state.tracks.get(idx).cloned())
    }

    pub fn current_index(&self) -> Option<usize> {
        self.state.lock().unwrap().current_index
    }

    pub fn history_len(&self) -> usize {
        self.current_index().unwrap_or(0)
    }

    pub fn all_playlist_queries(&self) -> Vec<String> {
        self.state.lock().unwrap().tracks.clone()
    }

    pub fn all_playlist_items(&self) -> Vec<QueueItem> {
        let queries = self.all_playlist_queries();
        let titles = self.titles.lock().unwrap();
        queries
            .into_iter()
            .map(|query| {
                titles.get(&query).cloned().unwrap_or_else(|| QueueItem {
                    title: query.clone(),
                    query,
                    ..QueueItem::default()
                })
            })
            .collect()
    }

    fn take_preloaded_if_matches(&self, target: &str) -> Option<PreloadedTrack> {
        let mut pre_lock = self.preloaded.lock().ok()?;
        let pre = pre_lock.take()?;
        if same_track(&pre.query, target) {
            Some(pre)
        } else {
            None
        }
    }

    pub fn select_playlist_item(&self, index: usize) -> Option<(String, Option<PreloadedTrack>)> {
        let target = {
            let mut state = self.state.lock().unwrap();
            if index >= state.tracks.len() || state.current_index == Some(index) {
                return None;
            }
            state.current_index = Some(index);
            state.tracks[index].clone()
        };
        let preloaded = self.take_preloaded_if_matches(&target);
        Some((target, preloaded))
    }

    pub fn remove_playlist_item(&self, index: usize) -> Option<String> {
        let removed = {
            let mut state = self.state.lock().unwrap();
            if index >= state.tracks.len() {
                return None;
            }
            let removed = state.tracks.remove(index);
            if let Some(curr) = state.current_index {
                if state.tracks.is_empty() {
                    state.current_index = None;
                } else if index < curr {
                    state.current_index = Some(curr - 1);
                } else if index == curr && curr >= state.tracks.len() {
                    state.current_index = Some(state.tracks.len() - 1);
                }
            }
            removed
        };

        if let Ok(mut pre_lock) = self.preloaded.lock()
            && pre_lock
                .as_ref()
                .is_some_and(|pre| same_track(&pre.query, &removed))
        {
            *pre_lock = None;
        }
        Some(removed)
    }

    pub fn move_playlist_item(&self, from: usize, to: usize) -> bool {
        {
            let mut state = self.state.lock().unwrap();
            if from >= state.tracks.len() || to >= state.tracks.len() || from == to {
                return false;
            }
            let item = state.tracks.remove(from);
            state.tracks.insert(to, item);

            state.current_index = match state.current_index {
                Some(curr) if curr == from => Some(to),
                Some(curr) if from < curr && to >= curr => Some(curr - 1),
                Some(curr) if from > curr && to <= curr => Some(curr + 1),
                other => other,
            };
        }
        if let Ok(mut pre_lock) = self.preloaded.lock() {
            *pre_lock = None;
        }
        true
    }

    pub fn display_items(&self) -> Vec<String> {
        let state = self.state.lock().unwrap();
        let titles = self.titles.lock().unwrap();
        let curr_idx = state.current_index;
        state
            .tracks
            .iter()
            .enumerate()
            .map(|(i, query)| {
                let marker = if Some(i) == curr_idx { "*" } else { "" };
                let label = titles
                    .get(query)
                    .map(|item| item.title.clone())
                    .unwrap_or_else(|| query.clone());
                format!("{}{}", marker, label)
            })
            .collect()
    }

    /// Advance cursor to the next track in the playlist without mutating track order or length.
    pub fn pop_next(&self) -> Option<(String, Option<PreloadedTrack>)> {
        let target = {
            let mut state = self.state.lock().unwrap();
            let len = state.tracks.len();
            if len == 0 {
                return None;
            }
            let next_idx = match state.current_index {
                None => 0,
                Some(curr) => {
                    if self.shuffle_enabled() && len > 1 {
                        let offset = 1 + shuffled_index(len - 1);
                        (curr + offset) % len
                    } else if curr + 1 < len {
                        curr + 1
                    } else if self.repeat_mode() == RepeatMode::All {
                        0
                    } else {
                        return None;
                    }
                }
            };
            state.current_index = Some(next_idx);
            state.tracks[next_idx].clone()
        };
        let preloaded = self.take_preloaded_if_matches(&target);
        Some((target, preloaded))
    }

    /// Move cursor to the previous track in the playlist without mutating track order or length.
    pub fn pop_previous(&self) -> Option<String> {
        let mut state = self.state.lock().unwrap();
        let len = state.tracks.len();
        if len == 0 {
            return None;
        }
        let prev_idx = match state.current_index {
            None => 0,
            Some(curr) => {
                if curr > 0 {
                    curr - 1
                } else if self.repeat_mode() == RepeatMode::All && len > 1 {
                    len - 1
                } else {
                    return None;
                }
            }
        };
        state.current_index = Some(prev_idx);
        Some(state.tracks[prev_idx].clone())
    }

    pub fn repeat_mode(&self) -> RepeatMode {
        *self.repeat_mode.lock().unwrap()
    }

    pub fn set_repeat_mode(&self, mode: RepeatMode) {
        *self.repeat_mode.lock().unwrap() = mode;
    }

    pub fn shuffle_enabled(&self) -> bool {
        *self.shuffle_enabled.lock().unwrap()
    }

    pub fn set_shuffle_enabled(&self, enabled: bool) {
        *self.shuffle_enabled.lock().unwrap() = enabled;
    }

    pub fn autoplay_enabled(&self) -> bool {
        *self.autoplay_enabled.lock().unwrap()
    }

    pub fn set_autoplay_enabled(&self, enabled: bool) {
        *self.autoplay_enabled.lock().unwrap() = enabled;
    }

    pub fn clear(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.tracks.clear();
            state.current_index = None;
        }
        if let Ok(mut pre_lock) = self.preloaded.lock() {
            *pre_lock = None;
        }
        self.titles.lock().unwrap().clear();
    }

    /// Clear playlist items (legacy alias for full playlist reset).
    pub fn clear_upcoming(&self) {
        let mut state = self.state.lock().unwrap();
        state.tracks.clear();
        state.current_index = None;
        if let Ok(mut pre_lock) = self.preloaded.lock() {
            *pre_lock = None;
        }
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.state.lock().unwrap().tracks.is_empty()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.state.lock().unwrap().tracks.len()
    }

    /// Load a list of extracted YouTube playlist items, selecting the first track.
    pub fn load_playlist(&self, items: Vec<PlaylistItem>) -> Option<String> {
        let mut state = self.state.lock().unwrap();
        let mut titles = self.titles.lock().unwrap();
        state.tracks.clear();
        state.current_index = None;
        if let Ok(mut pre_lock) = self.preloaded.lock() {
            *pre_lock = None;
        }

        for item in items {
            let query = format!("https://www.youtube.com/watch?v={}", item.id);
            titles.insert(
                query.clone(),
                QueueItem {
                    query: query.clone(),
                    title: item.title,
                    artist: item.uploader.unwrap_or_default(),
                    duration_secs: item.duration,
                    thumbnail_url: item.thumbnail,
                },
            );
            if !state.tracks.iter().any(|q| same_track(q, &query)) {
                state.tracks.push(query);
            }
        }
        if !state.tracks.is_empty() {
            state.current_index = Some(0);
            Some(state.tracks[0].clone())
        } else {
            None
        }
    }

    /// Preload the next track's metadata and thumbnail before current song ends
    pub fn preload_next(&self, extractor: Arc<Extractor>, library: LibraryStore) {
        let next_query = match self.peek_front() {
            Some(q) => q,
            None => return,
        };

        {
            let mut preloading = self.is_preloading.lock().unwrap();
            if *preloading {
                return;
            }
            if let Ok(pre_lock) = self.preloaded.lock()
                && let Some(pre) = pre_lock.as_ref()
                && same_track(&pre.query, &next_query)
            {
                return; // Already preloaded
            }
            *preloading = true;
        }

        let queue_clone = self.clone();
        tokio::spawn(async move {
            info!(
                "Gapless preloader: Pre-fetching metadata for next track: {}",
                next_query
            );
            if let Ok(meta) = extractor.extract_audio(&next_query).await {
                queue_clone.set_track_info(
                    &next_query,
                    meta.title.clone(),
                    meta.uploader.clone().unwrap_or_default(),
                    meta.duration,
                    meta.thumbnail.clone(),
                );
                let mut cover_path = None;
                let cached = library.cover_path(&meta.id);
                if cached.exists() {
                    cover_path = Some(cached);
                } else if let Some(thumb_url) = &meta.thumbnail
                    && let Ok(resp) = shared_http_client().get(thumb_url).send().await
                    && let Ok(bytes) = resp.bytes().await
                    && let Ok(saved_path) = library.store_cover(&meta.id, &bytes)
                {
                    cover_path = Some(saved_path);
                }

                if queue_clone
                    .peek_front()
                    .is_some_and(|front| same_track(&front, &next_query))
                {
                    info!(
                        "Gapless preloader: Next track preloaded successfully: {}",
                        meta.title
                    );
                    *queue_clone.preloaded.lock().unwrap() = Some(PreloadedTrack {
                        query: next_query,
                        meta,
                        cover_path,
                    });
                }
            }
            *queue_clone.is_preloading.lock().unwrap() = false;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{PlayQueue, RepeatMode, shuffled_index};
    use std::collections::HashSet;

    #[test]
    fn repeat_all_reuses_a_fixed_cycle() {
        let queue = PlayQueue::new();
        queue.set_repeat_mode(RepeatMode::All);
        queue.record_current("A".into());
        queue.push_back("B".into());

        for expected in ["B", "A", "B", "A"] {
            let (query, _) = queue.pop_next().expect("repeat-all should provide a track");
            assert_eq!(query, expected);
            queue.record_current(query);
        }

        assert_eq!(queue.all_playlist_queries(), vec!["A", "B"]);
    }

    #[test]
    fn playlist_moves_and_removals_preserve_cursor_and_order() {
        let queue = PlayQueue::new();
        queue.record_current("A".into());
        queue.push_back("B".into());
        queue.push_back("C".into());
        queue.push_back("D".into());

        assert!(queue.move_playlist_item(1, 3));
        assert_eq!(queue.all_playlist_queries(), vec!["A", "C", "D", "B"]);
        assert_eq!(queue.remove_playlist_item(2).as_deref(), Some("D"));
        assert_eq!(queue.all_playlist_queries(), vec!["A", "C", "B"]);
    }

    #[test]
    fn rapid_next_prev_never_duplicates_tracks() {
        let queue = PlayQueue::new();
        queue.set_repeat_mode(RepeatMode::All);
        queue.push_back("https://www.youtube.com/watch?v=LifeGoesOn".into());
        queue.push_back("https://www.youtube.com/watch?v=NORMAL".into());
        queue.record_current("https://www.youtube.com/watch?v=LifeGoesOn&t=58s".into());

        for _ in 0..20 {
            let (next_q, _) = queue.pop_next().unwrap();
            queue.record_current(next_q);
            let prev_q = queue.pop_previous().unwrap();
            queue.record_current(prev_q);
        }

        assert_eq!(
            queue.all_playlist_queries(),
            vec![
                "https://www.youtube.com/watch?v=LifeGoesOn",
                "https://www.youtube.com/watch?v=NORMAL"
            ]
        );
    }

    #[test]
    fn shuffle_can_select_both_tracks_in_a_two_track_queue() {
        let selections: HashSet<_> = (0..128).map(|_| shuffled_index(2)).collect();
        assert_eq!(selections, HashSet::from([0, 1]));
    }

    #[test]
    fn pop_previous_moves_cursor_back_and_forward() {
        let queue = PlayQueue::new();
        queue.record_current("Song 1".into());
        queue.record_current("Song 2".into());
        queue.record_current("Song 3".into());

        assert_eq!(queue.pop_previous().as_deref(), Some("Song 2"));
        assert_eq!(queue.current_query().as_deref(), Some("Song 2"));

        let (next_song, _) = queue
            .pop_next()
            .expect("playlist should advance back to Song 3");
        assert_eq!(next_song, "Song 3");
    }

    #[test]
    fn pop_previous_when_at_first_track_without_repeat_returns_none() {
        let queue = PlayQueue::new();
        assert_eq!(queue.pop_previous(), None);

        queue.record_current("Song 1".into());
        assert_eq!(queue.pop_previous(), None);
        assert_eq!(queue.current_query().as_deref(), Some("Song 1"));
    }

    #[test]
    fn autoplay_toggle_state() {
        let queue = PlayQueue::new();
        assert!(queue.autoplay_enabled());
        queue.set_autoplay_enabled(false);
        assert!(!queue.autoplay_enabled());
        queue.set_autoplay_enabled(true);
        assert!(queue.autoplay_enabled());
    }

    #[test]
    fn select_playlist_item_preserves_absolute_playlist_order() {
        let queue = PlayQueue::new();
        queue.push_back("Track 1".into());
        queue.push_back("Track 2".into());
        queue.push_back("Track 3".into());
        queue.push_back("Track 4".into());
        queue.push_back("Track 5".into());

        assert_eq!(
            queue.all_playlist_queries(),
            vec!["Track 1", "Track 2", "Track 3", "Track 4", "Track 5"]
        );

        let selected = queue.select_playlist_item(2);
        assert_eq!(selected.map(|(q, _)| q), Some("Track 3".to_string()));
        assert_eq!(queue.current_query().as_deref(), Some("Track 3"));

        assert_eq!(
            queue.all_playlist_queries(),
            vec!["Track 1", "Track 2", "Track 3", "Track 4", "Track 5"]
        );

        let (next_q, _) = queue.pop_next().expect("Track 4 should be next");
        assert_eq!(next_q, "Track 4");
        queue.record_current(next_q);
        assert_eq!(
            queue.all_playlist_queries(),
            vec!["Track 1", "Track 2", "Track 3", "Track 4", "Track 5"]
        );

        let prev_q = queue.pop_previous().expect("Track 3 should be previous");
        assert_eq!(prev_q, "Track 3");
        assert_eq!(
            queue.all_playlist_queries(),
            vec!["Track 1", "Track 2", "Track 3", "Track 4", "Track 5"]
        );

        let selected_1 = queue.select_playlist_item(0);
        assert_eq!(selected_1.map(|(q, _)| q), Some("Track 1".to_string()));
        assert_eq!(
            queue.all_playlist_queries(),
            vec!["Track 1", "Track 2", "Track 3", "Track 4", "Track 5"]
        );
    }
}
