use crate::extractor::{AudioMetadata, Extractor, PlaylistItem};
use crate::library::RepeatMode;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tracing::info;

fn shuffled_index(len: usize) -> usize {
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

fn upcoming_cycle_positions(cycle: &[String], upcoming: &[String]) -> Option<Vec<usize>> {
    let mut used = vec![false; cycle.len()];
    upcoming
        .iter()
        .map(|query| {
            let index = cycle
                .iter()
                .enumerate()
                .find(|(index, item)| !used[*index] && *item == query)
                .map(|(index, _)| index)?;
            used[index] = true;
            Some(index)
        })
        .collect()
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
pub struct PlayQueue {
    inner: Arc<Mutex<VecDeque<String>>>,
    history: Arc<Mutex<Vec<String>>>,
    current: Arc<Mutex<Option<String>>>,
    preloaded: Arc<Mutex<Option<PreloadedTrack>>>,
    is_preloading: Arc<Mutex<bool>>,
    titles: Arc<Mutex<HashMap<String, QueueItem>>>,
    repeat_mode: Arc<Mutex<RepeatMode>>,
    shuffle_enabled: Arc<Mutex<bool>>,
    repeat_cycle: Arc<Mutex<Vec<String>>>,
}

impl PlayQueue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            history: Arc::new(Mutex::new(Vec::new())),
            current: Arc::new(Mutex::new(None)),
            preloaded: Arc::new(Mutex::new(None)),
            is_preloading: Arc::new(Mutex::new(false)),
            titles: Arc::new(Mutex::new(HashMap::new())),
            repeat_mode: Arc::new(Mutex::new(RepeatMode::Off)),
            shuffle_enabled: Arc::new(Mutex::new(false)),
            repeat_cycle: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn push_back(&self, item: String) {
        let current = self.current.lock().unwrap().clone();
        {
            let mut cycle = self.repeat_cycle.lock().unwrap();
            if cycle.is_empty() {
                if let Some(current) = current {
                    cycle.push(current);
                }
            }
            cycle.push(item.clone());
        }
        self.inner.lock().unwrap().push_back(item);
    }

    /// Start a new repeat-all cycle at a directly selected track, followed by
    /// the tracks that are still queued.
    pub fn begin_repeat_cycle(&self, current: String) {
        let mut cycle = vec![current];
        cycle.extend(self.inner.lock().unwrap().iter().cloned());
        *self.repeat_cycle.lock().unwrap() = cycle;
    }

    #[allow(dead_code)]
    pub fn pop_front(&self) -> Option<String> {
        let mut q = self.inner.lock().unwrap();
        let item = q.pop_front();
        if let Ok(mut pre_lock) = self.preloaded.lock() {
            if let Some(pre) = pre_lock.as_ref() {
                if item.as_ref() == Some(&pre.query) {
                    *pre_lock = None;
                }
            }
        }
        item
    }

    pub fn peek_front(&self) -> Option<String> {
        self.inner.lock().unwrap().front().cloned()
    }

    /// Record currently playing track into history
    pub fn record_current(&self, query: String) {
        let mut curr_lock = self.current.lock().unwrap();
        if let Some(old) = curr_lock.take() {
            if old != query {
                let mut hist = self.history.lock().unwrap();
                if hist.last() != Some(&old) {
                    hist.push(old);
                }
            }
        }
        {
            let mut cycle = self.repeat_cycle.lock().unwrap();
            if !cycle.contains(&query) {
                cycle.insert(0, query.clone());
            }
        }
        *curr_lock = Some(query);
    }

    pub fn set_track_info(
        &self,
        query: &str,
        title: String,
        artist: String,
        duration_secs: Option<f64>,
        thumbnail_url: Option<String>,
    ) {
        self.titles.lock().unwrap().insert(
            query.to_string(),
            QueueItem {
                query: query.to_string(),
                title,
                artist,
                duration_secs,
                thumbnail_url,
            },
        );
    }

    pub fn upcoming_items(&self) -> Vec<QueueItem> {
        let queries: Vec<String> = self.inner.lock().unwrap().iter().cloned().collect();
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

    pub fn current_query(&self) -> Option<String> {
        self.current.lock().unwrap().clone()
    }

    pub fn display_items(&self) -> Vec<String> {
        let current = self.current.lock().unwrap().clone();
        let upcoming: Vec<String> = self.inner.lock().unwrap().iter().cloned().collect();
        let titles = self.titles.lock().unwrap();
        current
            .into_iter()
            .chain(upcoming)
            .map(|query| {
                titles
                    .get(&query)
                    .map(|item| item.title.clone())
                    .unwrap_or_else(|| query.clone())
            })
            .collect()
    }

    /// Pop next item, returning preloaded track assets if ready
    pub fn pop_next(&self) -> Option<(String, Option<PreloadedTrack>)> {
        if self.inner.lock().unwrap().is_empty() && self.repeat_mode() == RepeatMode::All {
            let mut cycle = self.repeat_cycle.lock().unwrap().clone();
            if cycle.is_empty() {
                cycle = self.history.lock().unwrap().clone();
                if let Some(current) = self.current.lock().unwrap().clone() {
                    if cycle.last() != Some(&current) {
                        cycle.push(current);
                    }
                }
                *self.repeat_cycle.lock().unwrap() = cycle.clone();
            }
            if !cycle.is_empty() {
                let mut q = self.inner.lock().unwrap();
                if q.is_empty() {
                    q.extend(cycle);
                }
            }
        }

        let shuffle = self.shuffle_enabled();
        let mut q = self.inner.lock().unwrap();
        let index = if shuffle && q.len() > 1 {
            shuffled_index(q.len())
        } else {
            0
        };
        let query = q.remove(index)?;
        let mut pre_lock = self.preloaded.lock().unwrap();
        if let Some(pre) = pre_lock.take() {
            if pre.query == query {
                return Some((query, Some(pre)));
            }
        }
        Some((query, None))
    }

    pub fn pop_at(&self, index: usize) -> Option<(String, Option<PreloadedTrack>)> {
        let mut q = self.inner.lock().unwrap();
        let query = q.remove(index)?;
        let mut pre_lock = self.preloaded.lock().unwrap();
        if let Some(pre) = pre_lock.take() {
            if pre.query == query {
                return Some((query, Some(pre)));
            }
        }
        Some((query, None))
    }

    pub fn remove_at(&self, index: usize) -> Option<String> {
        let (query, old_upcoming) = {
            let mut q = self.inner.lock().unwrap();
            let old_upcoming = q.iter().cloned().collect::<Vec<_>>();
            (q.remove(index)?, old_upcoming)
        };
        let mut cycle = self.repeat_cycle.lock().unwrap();
        if let Some(mut positions) = upcoming_cycle_positions(&cycle, &old_upcoming) {
            cycle.remove(positions.swap_remove(index));
        }
        if let Ok(mut pre_lock) = self.preloaded.lock() {
            if pre_lock.as_ref().is_some_and(|pre| pre.query == query) {
                *pre_lock = None;
            }
        }
        Some(query)
    }

    pub fn move_item(&self, from: usize, to: usize) -> bool {
        let (old_upcoming, new_upcoming) = {
            let mut q = self.inner.lock().unwrap();
            if from >= q.len() || to >= q.len() || from == to {
                return false;
            }
            let old_upcoming = q.iter().cloned().collect::<Vec<_>>();
            let Some(item) = q.remove(from) else {
                return false;
            };
            q.insert(to, item);
            let new_upcoming = q.iter().cloned().collect::<Vec<_>>();
            (old_upcoming, new_upcoming)
        };
        let mut cycle = self.repeat_cycle.lock().unwrap();
        if let Some(mut positions) = upcoming_cycle_positions(&cycle, &old_upcoming) {
            positions.sort_unstable();
            for (position, query) in positions.into_iter().zip(new_upcoming) {
                cycle[position] = query;
            }
        }
        if let Ok(mut pre_lock) = self.preloaded.lock() {
            *pre_lock = None;
        }
        true
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

    /// Pop previous track from history and push current back to upcoming queue
    pub fn pop_previous(&self) -> Option<String> {
        let mut curr = self.current.lock().unwrap();
        let mut hist = self.history.lock().unwrap();
        let prev = hist.pop()?;
        if let Some(c) = curr.take() {
            let mut q = self.inner.lock().unwrap();
            q.push_front(c);
        }
        *curr = Some(prev.clone());
        Some(prev)
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
        self.history.lock().unwrap().clear();
        *self.current.lock().unwrap() = None;
        self.repeat_cycle.lock().unwrap().clear();
        if let Ok(mut pre_lock) = self.preloaded.lock() {
            *pre_lock = None;
        }
        self.titles.lock().unwrap().clear();
    }

    /// Clear upcoming queue items while keeping currently playing track
    pub fn clear_upcoming(&self) {
        self.inner.lock().unwrap().clear();
        if let Some(current) = self.current.lock().unwrap().clone() {
            *self.repeat_cycle.lock().unwrap() = vec![current];
        } else {
            self.repeat_cycle.lock().unwrap().clear();
        }
        if let Ok(mut pre_lock) = self.preloaded.lock() {
            *pre_lock = None;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().is_empty()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// Load a list of playlist items: returns the first item to play immediately,
    /// and enqueues the remaining items.
    pub fn load_playlist(&self, items: Vec<PlaylistItem>) -> Option<String> {
        self.inner.lock().unwrap().clear();
        self.history.lock().unwrap().clear();
        *self.current.lock().unwrap() = None;
        self.repeat_cycle.lock().unwrap().clear();
        if let Ok(mut pre_lock) = self.preloaded.lock() {
            *pre_lock = None;
        }
        let mut titles = self.titles.lock().unwrap();
        let mut iter = items.into_iter();
        let first_item = iter.next();
        let mut repeat_cycle = Vec::new();
        if let Some(item) = &first_item {
            let query = format!("https://www.youtube.com/watch?v={}", item.id);
            repeat_cycle.push(query.clone());
            titles.insert(
                query.clone(),
                QueueItem {
                    query,
                    title: item.title.clone(),
                    artist: item.uploader.clone().unwrap_or_default(),
                    duration_secs: item.duration,
                    thumbnail_url: item.thumbnail.clone(),
                },
            );
        }
        let mut upcoming = Vec::new();
        for item in iter {
            let query = format!("https://www.youtube.com/watch?v={}", item.id);
            repeat_cycle.push(query.clone());
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
            upcoming.push(query);
        }
        drop(titles);
        self.inner.lock().unwrap().extend(upcoming);
        *self.repeat_cycle.lock().unwrap() = repeat_cycle;
        first_item.map(|item| format!("https://www.youtube.com/watch?v={}", item.id))
    }

    /// Preload the next track's metadata and thumbnail before current song ends
    pub fn preload_next(&self, extractor: Arc<Extractor>) {
        let next_query = match self.peek_front() {
            Some(q) => q,
            None => return,
        };

        {
            let mut preloading = self.is_preloading.lock().unwrap();
            if *preloading {
                return;
            }
            if let Ok(pre_lock) = self.preloaded.lock() {
                if let Some(pre) = pre_lock.as_ref() {
                    if pre.query == next_query {
                        return; // Already preloaded
                    }
                }
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
                // Pre-download cover art
                let mut cover_path = None;
                if let Some(thumb_url) = &meta.thumbnail {
                    let temp_path =
                        std::env::temp_dir().join(format!("rs_yt_cover_pre_{}.png", meta.id));
                    let client = reqwest::Client::new();
                    if let Ok(resp) = client.get(thumb_url).send().await {
                        if let Ok(bytes) = resp.bytes().await {
                            if let Ok(dynamic_img) = image::load_from_memory(&bytes) {
                                if dynamic_img
                                    .save_with_format(&temp_path, image::ImageFormat::Png)
                                    .is_ok()
                                {
                                    cover_path = Some(temp_path);
                                }
                            }
                        }
                    }
                }

                // Check if queue front still matches this track
                if queue_clone.peek_front().as_ref() == Some(&next_query) {
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

    /// Prefetch related recommendations when the queue is empty
    pub fn prefetch_autoplay(&self, extractor: Arc<Extractor>, current_id: String) {
        if self.repeat_mode() != RepeatMode::Off {
            return;
        }
        let queue_clone = self.clone();
        tokio::spawn(async move {
            let is_empty = queue_clone.is_empty();
            if is_empty {
                if let Ok(related_ids) = extractor.extract_autoplay_next(&current_id).await {
                    if queue_clone.is_empty() && queue_clone.repeat_mode() == RepeatMode::Off {
                        let count = related_ids.len();
                        for id in related_ids {
                            queue_clone
                                .push_back(format!("https://www.youtube.com/watch?v={}", id));
                        }
                        info!("Prefetched {} autoplay recommendations into queue", count);
                    }
                }
            }
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
        queue.push_back("B".into());
        queue.record_current("A".into());

        for expected in ["B", "A", "B", "A"] {
            let (query, _) = queue.pop_next().expect("repeat-all should provide a track");
            assert_eq!(query, expected);
            queue.record_current(query);
        }

        assert_eq!(*queue.repeat_cycle.lock().unwrap(), ["A", "B"]);
    }

    #[test]
    fn queue_edits_are_reflected_in_the_repeat_cycle() {
        let queue = PlayQueue::new();
        queue.record_current("A".into());
        queue.push_back("B".into());
        queue.push_back("C".into());
        queue.push_back("D".into());

        assert!(queue.move_item(0, 2));
        assert_eq!(*queue.repeat_cycle.lock().unwrap(), ["A", "C", "D", "B"]);
        assert_eq!(queue.remove_at(1).as_deref(), Some("D"));
        assert_eq!(*queue.repeat_cycle.lock().unwrap(), ["A", "C", "B"]);
    }

    #[test]
    fn directly_selected_track_starts_a_fresh_repeat_cycle() {
        let queue = PlayQueue::new();
        queue.push_back("old queued track".into());
        queue.record_current("old current track".into());
        queue.begin_repeat_cycle("new current track".into());

        assert_eq!(
            *queue.repeat_cycle.lock().unwrap(),
            ["new current track", "old queued track"]
        );
    }

    #[test]
    fn shuffle_can_select_both_tracks_in_a_two_track_queue() {
        let selections: HashSet<_> = (0..128).map(|_| shuffled_index(2)).collect();
        assert_eq!(selections, HashSet::from([0, 1]));
    }
}
