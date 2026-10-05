use crate::extractor::{AudioMetadata, Extractor};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::info;

#[derive(Clone, Debug)]
pub struct PreloadedTrack {
    pub query: String,
    pub meta: AudioMetadata,
    pub cover_path: Option<PathBuf>,
}

#[derive(Clone, Default)]
pub struct PlayQueue {
    inner: Arc<Mutex<VecDeque<String>>>,
    history: Arc<Mutex<Vec<String>>>,
    current: Arc<Mutex<Option<String>>>,
    preloaded: Arc<Mutex<Option<PreloadedTrack>>>,
    is_preloading: Arc<Mutex<bool>>,
}

impl PlayQueue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            history: Arc::new(Mutex::new(Vec::new())),
            current: Arc::new(Mutex::new(None)),
            preloaded: Arc::new(Mutex::new(None)),
            is_preloading: Arc::new(Mutex::new(false)),
        }
    }

    pub fn push_back(&self, item: String) {
        self.inner.lock().unwrap().push_back(item);
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
        *curr_lock = Some(query);
    }

    /// Pop next item, returning preloaded track assets if ready
    pub fn pop_next(&self) -> Option<(String, Option<PreloadedTrack>)> {
        let mut q = self.inner.lock().unwrap();
        let query = q.pop_front()?;
        let mut pre_lock = self.preloaded.lock().unwrap();
        if let Some(pre) = pre_lock.take() {
            if pre.query == query {
                return Some((query, Some(pre)));
            }
        }
        Some((query, None))
    }

    /// Pop previous track from history and push current back to upcoming queue
    pub fn pop_previous(&self) -> Option<String> {
        let mut hist = self.history.lock().unwrap();
        let prev = hist.pop()?;
        let mut curr = self.current.lock().unwrap();
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
    pub fn load_playlist(&self, items: Vec<String>) -> Option<String> {
        let mut q = self.inner.lock().unwrap();
        q.clear();
        self.history.lock().unwrap().clear();
        *self.current.lock().unwrap() = None;
        if let Ok(mut pre_lock) = self.preloaded.lock() {
            *pre_lock = None;
        }
        let mut iter = items.into_iter();
        let first = iter.next();
        for id in iter {
            q.push_back(id);
        }
        first
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
            info!("Gapless preloader: Pre-fetching metadata for next track: {}", next_query);
            if let Ok(meta) = extractor.extract_audio(&next_query).await {
                // Pre-download cover art
                let mut cover_path = None;
                if let Some(thumb_url) = &meta.thumbnail {
                    let temp_path = std::env::temp_dir().join(format!("rs_yt_cover_pre_{}.png", meta.id));
                    let client = reqwest::Client::new();
                    if let Ok(resp) = client.get(thumb_url).send().await {
                        if let Ok(bytes) = resp.bytes().await {
                            if let Ok(dynamic_img) = image::load_from_memory(&bytes) {
                                if dynamic_img.save_with_format(&temp_path, image::ImageFormat::Png).is_ok() {
                                    cover_path = Some(temp_path);
                                }
                            }
                        }
                    }
                }

                // Check if queue front still matches this track
                if queue_clone.peek_front().as_ref() == Some(&next_query) {
                    info!("Gapless preloader: Next track preloaded successfully: {}", meta.title);
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
        let queue_clone = self.clone();
        tokio::spawn(async move {
            let is_empty = queue_clone.is_empty();
            if is_empty {
                if let Ok(related_ids) = extractor.extract_autoplay_next(&current_id).await {
                    if queue_clone.is_empty() {
                        let count = related_ids.len();
                        for id in related_ids {
                            queue_clone.push_back(id);
                        }
                        info!("Prefetched {} autoplay recommendations into queue", count);
                    }
                }
            }
        });
    }
}
