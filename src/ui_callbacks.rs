use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::extractor::Extractor;
use crate::library::{LibraryStore, TrackRecord};
use crate::playback_coordinator::{
    start_track_playback, trigger_next, trigger_previous, trigger_toggle_play_pause,
    LoadingAbortHandle, SmtcHandle,
};
use crate::player::AudioPlayer;
use crate::queue::PlayQueue;
use crate::AppWindow;
use slint::ComponentHandle;

pub mod library;
pub mod playback;
pub mod queue;
pub mod search;
pub mod window;

pub struct PendingSeekState {
    pub id: u64,
    #[allow(dead_code)]
    pub target_ratio: f32,
    #[allow(dead_code)]
    pub target_secs: f32,
    pub requested_at: Instant,
}

/// Shared playback dependency bundle to eliminate repetitive Arc/handle cloning.
#[derive(Clone)]
pub(crate) struct PlaybackContext {
    pub(crate) player: Arc<AudioPlayer>,
    pub(crate) extractor: Arc<Extractor>,
    pub(crate) smtc: SmtcHandle,
    pub(crate) queue: PlayQueue,
    pub(crate) library: LibraryStore,
    pub(crate) last_query: Arc<Mutex<Option<String>>>,
    pub(crate) loading_abort: LoadingAbortHandle,
    pub(crate) ui_weak: slint::Weak<AppWindow>,
}

impl PlaybackContext {
    pub(crate) fn start_track(&self, key: String, force_reextract: bool) {
        start_track_playback(
            key,
            Arc::clone(&self.extractor),
            Arc::clone(&self.player),
            self.smtc.clone(),
            self.ui_weak.clone(),
            self.queue.clone(),
            self.library.clone(),
            Arc::clone(&self.last_query),
            force_reextract,
            self.loading_abort.clone(),
        );
    }

    pub(crate) fn toggle_play_pause(&self) {
        trigger_toggle_play_pause(
            Arc::clone(&self.player),
            Arc::clone(&self.extractor),
            self.smtc.clone(),
            self.ui_weak.clone(),
            Arc::clone(&self.last_query),
        );
    }

    pub(crate) fn trigger_previous(&self, is_advancing: &Arc<Mutex<bool>>) {
        trigger_previous(
            Arc::clone(&self.extractor),
            Arc::clone(&self.player),
            self.smtc.clone(),
            self.ui_weak.clone(),
            self.queue.clone(),
            self.library.clone(),
            Arc::clone(is_advancing),
            Arc::clone(&self.last_query),
            self.loading_abort.clone(),
        );
    }

    pub(crate) fn trigger_next(&self, is_advancing: &Arc<Mutex<bool>>) {
        trigger_next(
            Arc::clone(&self.extractor),
            Arc::clone(&self.player),
            self.smtc.clone(),
            self.ui_weak.clone(),
            self.queue.clone(),
            self.library.clone(),
            Arc::clone(is_advancing),
            false,
            Arc::clone(&self.last_query),
            self.loading_abort.clone(),
        );
    }
}

pub fn register_ui_callbacks(
    app: &AppWindow,
    player: Arc<AudioPlayer>,
    extractor: Arc<Extractor>,
    smtc: SmtcHandle,
    play_queue: PlayQueue,
    library: LibraryStore,
    is_auto_advancing: Arc<Mutex<bool>>,
    current_loading_abort: LoadingAbortHandle,
    last_query: Arc<Mutex<Option<String>>>,
    pending_seek: Arc<Mutex<Option<PendingSeekState>>>,
    search_records: Arc<Mutex<Vec<TrackRecord>>>,
    main_hwnd: Arc<Mutex<Option<windows::Win32::Foundation::HWND>>>,
    seek_seq: Arc<std::sync::atomic::AtomicU64>,
    seek_lock: Arc<tokio::sync::Mutex<()>>,
) {
    let ctx = PlaybackContext {
        player: Arc::clone(&player),
        extractor,
        smtc: smtc.clone(),
        queue: play_queue.clone(),
        library: library.clone(),
        last_query,
        loading_abort: current_loading_abort,
        ui_weak: app.as_weak(),
    };

    window::bind_window_controls(app, main_hwnd);
    window::bind_navigation_controls(app, play_queue, library);
    search::bind_search_controls(app, ctx.clone(), Arc::clone(&search_records));
    library::bind_library_controls(app, ctx.clone(), search_records);
    queue::bind_queue_controls(app, ctx.clone(), Arc::clone(&is_auto_advancing));
    playback::bind_playback_controls(app, ctx, is_auto_advancing);
    playback::bind_seek_and_volume_controls(app, player, smtc, pending_seek, seek_seq, seek_lock);
}
