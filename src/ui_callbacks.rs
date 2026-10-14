use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::info;

use crate::extractor::Extractor;
use crate::library::{LibraryStore, TrackRecord};
use crate::playback_coordinator::{
    play_track, play_with_metadata, start_track_playback, trigger_next, trigger_previous,
    trigger_toggle_play_pause, LoadingAbortHandle, SmtcHandle,
};
use crate::player::AudioPlayer;
use crate::queue::PlayQueue;
use crate::ui_models::{
    apply_queue_expanded, cache_track_cover, is_url, query_video_id, queue_items_model,
    record_from_search, records_model,
};
use crate::AppWindow;
use slint::ComponentHandle;

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
struct PlaybackContext {
    player: Arc<AudioPlayer>,
    extractor: Arc<Extractor>,
    smtc: SmtcHandle,
    queue: PlayQueue,
    library: LibraryStore,
    last_query: Arc<Mutex<Option<String>>>,
    loading_abort: LoadingAbortHandle,
    ui_weak: slint::Weak<AppWindow>,
}

impl PlaybackContext {
    fn start_track(&self, key: String, force_reextract: bool) {
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

    fn toggle_play_pause(&self) {
        trigger_toggle_play_pause(
            Arc::clone(&self.player),
            Arc::clone(&self.extractor),
            self.smtc.clone(),
            self.ui_weak.clone(),
            Arc::clone(&self.last_query),
        );
    }

    fn trigger_previous(&self, is_advancing: &Arc<Mutex<bool>>) {
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

    fn trigger_next(&self, is_advancing: &Arc<Mutex<bool>>) {
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

    bind_window_controls(app, main_hwnd);
    bind_navigation_controls(app, play_queue, library);
    bind_search_controls(app, ctx.clone(), Arc::clone(&search_records));
    bind_library_controls(app, ctx.clone(), search_records);
    bind_queue_controls(app, ctx.clone(), Arc::clone(&is_auto_advancing));
    bind_playback_controls(app, ctx, is_auto_advancing);
    bind_seek_and_volume_controls(app, player, smtc, pending_seek);
}

// ============================================================================
// 0. Window Management Controls
// ============================================================================
fn bind_window_controls(
    app: &AppWindow,
    main_hwnd: Arc<Mutex<Option<windows::Win32::Foundation::HWND>>>,
) {
    let hwnd_for_drag = Arc::clone(&main_hwnd);
    app.on_start_window_drag(move || {
        if let Some(hwnd) = *hwnd_for_drag.lock().unwrap() {
            crate::win32_window::start_window_drag(hwnd);
        }
    });

    let hwnd_for_min = Arc::clone(&main_hwnd);
    app.on_minimize_window(move || {
        if let Some(hwnd) = *hwnd_for_min.lock().unwrap() {
            crate::win32_window::minimize_to_tray(hwnd);
        }
    });

    app.on_close_window(move || {
        let _ = slint::quit_event_loop();
    });
}

// ============================================================================
// 1. Navigation & Panel Controls
// ============================================================================
fn bind_navigation_controls(app: &AppWindow, queue: PlayQueue, library: LibraryStore) {
    let ui_weak = app.as_weak();
    let queue_for_toggle = queue;
    let library_for_toggle = library;
    app.on_toggle_queue(move || {
        if let Some(ui) = ui_weak.upgrade() {
            let expanded = !ui.get_queue_expanded();
            apply_queue_expanded(&ui, expanded);
            ui.set_queue_items(queue_items_model(&queue_for_toggle, &library_for_toggle));
        }
    });

    let ui_weak_tab = app.as_weak();
    app.on_select_panel_tab(move |tab| {
        if let Some(ui) = ui_weak_tab.upgrade() {
            ui.set_panel_tab(tab);
            ui.set_panel_message(slint::SharedString::default());
        }
    });
}

// ============================================================================
// 2. Search Controls & Results
// ============================================================================
fn bind_search_controls(
    app: &AppWindow,
    ctx: PlaybackContext,
    search_records: Arc<Mutex<Vec<TrackRecord>>>,
) {
    let search_generation = Arc::new(AtomicU64::new(0));

    // on_search_and_play
    let ctx_for_search = ctx.clone();
    let search_records_for_search = Arc::clone(&search_records);
    let search_generation_for_search = Arc::clone(&search_generation);
    app.on_search_and_play(move |query| {
        let query_str = query.to_string();
        if query_str.trim().is_empty() {
            return;
        }

        if is_url(&query_str) {
            ctx_for_search.start_track(query_str.clone(), !query_str.contains("list="));
            return;
        }

        let token = search_generation_for_search.fetch_add(1, Ordering::SeqCst) + 1;
        let ui_for_start = ctx_for_search.ui_weak.clone();
        let library_for_start = ctx_for_search.library.clone();
        let query_title = query_str.clone();
        let _ = ui_for_start.upgrade_in_event_loop(move |ui| {
            ui.set_track_title(slint::SharedString::from(format!("Searching: {}", query_title)));
            ui.set_track_status(slint::SharedString::from("SEARCHING"));
            ui.set_is_loading(true);
            ui.set_panel_tab(slint::SharedString::from("SEARCH"));
            ui.set_panel_message(slint::SharedString::from("Searching YouTube…"));
            ui.set_search_results(records_model(Vec::new(), &library_for_start));
        });

        let search_records = Arc::clone(&search_records_for_search);
        let search_gen = Arc::clone(&search_generation_for_search);
        let extractor = Arc::clone(&ctx_for_search.extractor);
        let library = ctx_for_search.library.clone();
        let ui_weak = ctx_for_search.ui_weak.clone();

        tokio::spawn(async move {
            let search_result = extractor.search_tracks(&query_str).await;
            match search_result {
                Ok(results) => {
                    let records: Vec<TrackRecord> =
                        results.into_iter().map(record_from_search).collect();
                    let records_are_current = {
                        let mut saved_records = search_records.lock().unwrap();
                        if search_gen.load(Ordering::SeqCst) == token {
                            *saved_records = records.clone();
                            true
                        } else {
                            false
                        }
                    };
                    if records_are_current && search_gen.load(Ordering::SeqCst) == token {
                        let message = if records.is_empty() {
                            "No results found".to_string()
                        } else {
                            format!("{} results found", records.len())
                        };
                        let records_for_ui = records.clone();
                        let library_for_ui = library.clone();
                        let gen_for_ui = Arc::clone(&search_gen);
                        let is_empty = records.is_empty();
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            if gen_for_ui.load(Ordering::SeqCst) == token {
                                ui.set_search_results(records_model(
                                    records_for_ui,
                                    &library_for_ui,
                                ));
                                ui.set_panel_message(slint::SharedString::from(message));
                                ui.set_is_loading(false);
                                if is_empty {
                                    ui.set_track_status(slint::SharedString::from("NO RESULTS"));
                                } else if ui.get_track_status() == "SEARCHING" {
                                    ui.set_track_status(slint::SharedString::from("READY"));
                                }
                                if !ui.get_queue_expanded() {
                                    apply_queue_expanded(&ui, true);
                                }
                            }
                        });
                    }
                    if let Ok(client) = reqwest::Client::builder()
                        .timeout(Duration::from_secs(8))
                        .build()
                    {
                        let downloads = records
                            .iter()
                            .cloned()
                            .map(|track| cache_track_cover(client.clone(), library.clone(), track));
                        futures_util::future::join_all(downloads).await;
                        if search_gen.load(Ordering::SeqCst) == token {
                            let gen_for_ui = Arc::clone(&search_gen);
                            let library_for_ui = library.clone();
                            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                                if gen_for_ui.load(Ordering::SeqCst) == token {
                                    ui.set_search_results(records_model(records, &library_for_ui));
                                }
                            });
                        }
                    }
                }
                Err(error) => {
                    let message = format!("Search failed: {}", error);
                    if search_gen.load(Ordering::SeqCst) == token {
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            if search_gen.load(Ordering::SeqCst) == token {
                                ui.set_is_loading(false);
                                ui.set_track_status(slint::SharedString::from("ERROR"));
                                ui.set_panel_message(slint::SharedString::from(message));
                            }
                        });
                    }
                }
            }
        });
    });

    // on_enqueue_search_result
    let queue_for_add = ctx.queue.clone();
    let library_for_add = ctx.library.clone();
    let search_records_for_add = Arc::clone(&search_records);
    let ui_weak_add = ctx.ui_weak.clone();
    app.on_enqueue_search_result(move |key| {
        let key = key.to_string();
        let Some(record) = search_records_for_add
            .lock()
            .unwrap()
            .iter()
            .find(|record| record.video_url() == key)
            .cloned()
        else {
            return;
        };
        queue_for_add.set_track_info(
            &key,
            record.title.clone(),
            record.artist.clone(),
            record.duration_secs,
            record.thumbnail_url.clone(),
        );
        queue_for_add.push_back(key);
        if let Some(ui) = ui_weak_add.upgrade() {
            ui.set_panel_tab(slint::SharedString::from("QUEUE"));
            ui.set_queue_items(queue_items_model(&queue_for_add, &library_for_add));
            ui.set_panel_message(slint::SharedString::from("Added to queue"));
        }
    });

    // on_toggle_search_favorite
    let library_for_toggle = ctx.library.clone();
    let search_records_for_fav = Arc::clone(&search_records);
    let ui_weak_fav = ctx.ui_weak.clone();
    app.on_toggle_search_favorite(move |key| {
        let key = key.to_string();
        let record = search_records_for_fav
            .lock()
            .unwrap()
            .iter()
            .find(|record| record.video_url() == key)
            .cloned();
        if let Some(record) = record {
            library_for_toggle.toggle_favorite(record);
            if let Some(ui) = ui_weak_fav.upgrade() {
                let results = search_records_for_fav.lock().unwrap().clone();
                ui.set_search_results(records_model(results, &library_for_toggle));
                ui.set_favorite_items(records_model(
                    library_for_toggle.favorites(),
                    &library_for_toggle,
                ));
                ui.set_panel_message(slint::SharedString::from("Favorites updated"));
            }
        }
    });

    // on_play_search_result
    let ctx_for_play = ctx;
    app.on_play_search_result(move |key| {
        ctx_for_play.start_track(key.to_string(), false);
    });
}

// ============================================================================
// 3. Favorites & History Controls
// ============================================================================
fn bind_library_controls(
    app: &AppWindow,
    ctx: PlaybackContext,
    search_records: Arc<Mutex<Vec<TrackRecord>>>,
) {
    let ctx_for_play_fav = ctx.clone();
    app.on_play_favorite(move |key| {
        ctx_for_play_fav.start_track(key.to_string(), false);
    });

    let library_for_remove = ctx.library.clone();
    let search_records_for_remove = Arc::clone(&search_records);
    let ui_weak_remove = ctx.ui_weak.clone();
    app.on_remove_favorite(move |key| {
        let id = query_video_id(key.as_str());
        if let Some(track) = library_for_remove
            .favorites()
            .into_iter()
            .find(|track| track.id == id)
        {
            library_for_remove.toggle_favorite(track);
            if let Some(ui) = ui_weak_remove.upgrade() {
                ui.set_favorite_items(records_model(
                    library_for_remove.favorites(),
                    &library_for_remove,
                ));
                let results = search_records_for_remove.lock().unwrap().clone();
                ui.set_search_results(records_model(results, &library_for_remove));
                ui.set_history_items(records_model(
                    library_for_remove.recent(),
                    &library_for_remove,
                ));
                ui.set_current_is_favorite(library_for_remove.is_favorite(&id));
                ui.set_panel_message(slint::SharedString::from("Removed from favorites"));
            }
        }
    });

    let ctx_for_play_history = ctx.clone();
    app.on_play_history(move |key| {
        ctx_for_play_history.start_track(key.to_string(), false);
    });

    let library_for_hist_fav = ctx.library.clone();
    let ui_weak_hist_fav = ctx.ui_weak.clone();
    app.on_favorite_history(move |key| {
        let id = query_video_id(key.as_str());
        if let Some(track) = library_for_hist_fav
            .recent()
            .into_iter()
            .find(|track| track.id == id)
        {
            library_for_hist_fav.toggle_favorite(track);
            if let Some(ui) = ui_weak_hist_fav.upgrade() {
                ui.set_history_items(records_model(
                    library_for_hist_fav.recent(),
                    &library_for_hist_fav,
                ));
                ui.set_favorite_items(records_model(
                    library_for_hist_fav.favorites(),
                    &library_for_hist_fav,
                ));
                ui.set_current_is_favorite(library_for_hist_fav.is_favorite(&id));
            }
        }
    });

    let library_for_curr_fav = ctx.library;
    let ui_weak_curr_fav = ctx.ui_weak;
    app.on_toggle_current_favorite(move || {
        let Some(ui) = ui_weak_curr_fav.upgrade() else {
            return;
        };
        let key = ui.get_current_track_key().to_string();
        if key.is_empty() {
            return;
        }
        let id = query_video_id(&key);
        let record = library_for_curr_fav
            .recent()
            .into_iter()
            .find(|track| track.id == id)
            .or_else(|| {
                library_for_curr_fav
                    .favorites()
                    .into_iter()
                    .find(|track| track.id == id)
            });
        if let Some(record) = record {
            let is_favorite = library_for_curr_fav.toggle_favorite(record);
            ui.set_current_is_favorite(is_favorite);
            ui.set_favorite_items(records_model(
                library_for_curr_fav.favorites(),
                &library_for_curr_fav,
            ));
            ui.set_history_items(records_model(
                library_for_curr_fav.recent(),
                &library_for_curr_fav,
            ));
        }
    });
}

// ============================================================================
// 4. Queue Management Controls
// ============================================================================
fn bind_queue_controls(
    app: &AppWindow,
    ctx: PlaybackContext,
    is_auto_advancing: Arc<Mutex<bool>>,
) {
    let queue_for_repeat = ctx.queue.clone();
    let library_for_repeat = ctx.library.clone();
    let ui_weak_repeat = ctx.ui_weak.clone();
    app.on_cycle_repeat_mode(move || {
        let mode = queue_for_repeat.repeat_mode().next();
        queue_for_repeat.set_repeat_mode(mode);
        library_for_repeat.set_repeat_mode(mode);
        if let Some(ui) = ui_weak_repeat.upgrade() {
            ui.set_repeat_mode_label(slint::SharedString::from(mode.label()));
        }
    });

    let queue_for_shuffle = ctx.queue.clone();
    let library_for_shuffle = ctx.library.clone();
    let ui_weak_shuffle = ctx.ui_weak.clone();
    app.on_toggle_shuffle(move || {
        let enabled = !queue_for_shuffle.shuffle_enabled();
        queue_for_shuffle.set_shuffle_enabled(enabled);
        library_for_shuffle.set_shuffle_enabled(enabled);
        if let Some(ui) = ui_weak_shuffle.upgrade() {
            ui.set_shuffle_enabled(enabled);
        }
    });

    let ctx_for_retry = ctx.clone();
    app.on_retry_playback(move || {
        let query = ctx_for_retry.last_query.lock().unwrap().clone();
        if let Some(query) = query {
            ctx_for_retry.start_track(query, false);
        }
    });

    let queue_for_remove = ctx.queue.clone();
    let library_for_remove = ctx.library.clone();
    let ui_weak_remove = ctx.ui_weak.clone();
    app.on_remove_queue_item(move |index| {
        if queue_for_remove.remove_at(index as usize).is_some()
            && let Some(ui) = ui_weak_remove.upgrade() {
                ui.set_queue_items(queue_items_model(&queue_for_remove, &library_for_remove));
                ui.set_panel_message(slint::SharedString::from("Removed from queue"));
            }
    });

    let queue_for_move_up = ctx.queue.clone();
    let library_for_move_up = ctx.library.clone();
    let ui_weak_move_up = ctx.ui_weak.clone();
    app.on_move_queue_item_up(move |index| {
        let index = index as usize;
        if index > 0 && queue_for_move_up.move_item(index, index - 1)
            && let Some(ui) = ui_weak_move_up.upgrade() {
                ui.set_queue_items(queue_items_model(&queue_for_move_up, &library_for_move_up));
            }
    });

    let queue_for_move_down = ctx.queue.clone();
    let library_for_move_down = ctx.library.clone();
    let ui_weak_move_down = ctx.ui_weak.clone();
    app.on_move_queue_item_down(move |index| {
        let index = index as usize;
        if queue_for_move_down.move_item(index, index + 1)
            && let Some(ui) = ui_weak_move_down.upgrade() {
                ui.set_queue_items(queue_items_model(
                    &queue_for_move_down,
                    &library_for_move_down,
                ));
            }
    });

    let queue_for_clear = ctx.queue.clone();
    let library_for_clear = ctx.library.clone();
    let ui_weak_clear = ctx.ui_weak.clone();
    app.on_clear_queue(move || {
        queue_for_clear.clear_upcoming();
        if let Some(ui) = ui_weak_clear.upgrade() {
            ui.set_queue_items(queue_items_model(&queue_for_clear, &library_for_clear));
            ui.set_panel_message(slint::SharedString::from("Queue cleared"));
        }
    });

    let ctx_for_item = ctx;
    let advance_for_item = is_auto_advancing;
    app.on_play_queue_item(move |index| {
        let mut advancing = advance_for_item.lock().unwrap();
        if *advancing {
            return;
        }
        let Some((query, preloaded)) = ctx_for_item.queue.pop_at(index as usize) else {
            return;
        };
        *advancing = true;
        drop(advancing);

        if let Some(ui) = ctx_for_item.ui_weak.upgrade() {
            ui.set_queue_items(queue_items_model(
                &ctx_for_item.queue,
                &ctx_for_item.library,
            ));
        }

        let queue = ctx_for_item.queue.clone();
        let library = ctx_for_item.library.clone();
        let extractor = Arc::clone(&ctx_for_item.extractor);
        let player = Arc::clone(&ctx_for_item.player);
        let smtc = ctx_for_item.smtc.clone();
        let ui_weak = ctx_for_item.ui_weak.clone();
        let advance = Arc::clone(&advance_for_item);
        let last_query = Arc::clone(&ctx_for_item.last_query);
        let abort_handle = ctx_for_item.loading_abort.clone();

        let join_handle = tokio::spawn(async move {
            if let Some(preloaded) = preloaded {
                let cache_path = library.cover_path(&preloaded.meta.id);
                if !cache_path.exists()
                    && let Some(path) = &preloaded.cover_path
                        && let Ok(bytes) = std::fs::read(path) {
                            let _ = library.store_cover(&preloaded.meta.id, &bytes);
                        }
                let cover_path = cache_path
                    .exists()
                    .then_some(cache_path)
                    .or(preloaded.cover_path);
                play_with_metadata(
                    preloaded.query,
                    preloaded.meta,
                    cover_path,
                    extractor,
                    player,
                    smtc,
                    ui_weak,
                    queue,
                    library,
                    last_query,
                )
                .await;
            } else {
                play_track(
                    query, extractor, player, smtc, ui_weak, queue, library, last_query,
                )
                .await;
            }
            *advance.lock().unwrap() = false;
        });

        if let Ok(mut guard) = abort_handle.lock()
            && let Some(prev) = guard.replace(join_handle.abort_handle()) {
                prev.abort();
            }
    });
}

// ============================================================================
// 5. Playback Transport Controls
// ============================================================================
fn bind_playback_controls(
    app: &AppWindow,
    ctx: PlaybackContext,
    is_auto_advancing: Arc<Mutex<bool>>,
) {
    let ctx_for_toggle = ctx.clone();
    app.on_toggle_play_pause(move || {
        ctx_for_toggle.toggle_play_pause();
    });

    let ctx_for_prev = ctx.clone();
    let advance_for_prev = Arc::clone(&is_auto_advancing);
    app.on_play_previous(move || {
        ctx_for_prev.trigger_previous(&advance_for_prev);
    });

    let ctx_for_next = ctx.clone();
    let advance_for_next = Arc::clone(&is_auto_advancing);
    app.on_play_next(move || {
        ctx_for_next.trigger_next(&advance_for_next);
    });

    let abort_for_cancel = ctx.loading_abort.clone();
    let ui_weak_cancel = ctx.ui_weak.clone();
    let advance_for_cancel = is_auto_advancing;
    app.on_cancel_loading(move || {
        if let Ok(mut guard) = abort_for_cancel.lock()
            && let Some(handle) = guard.take() {
                handle.abort();
                info!("Active loading task was aborted by user.");
            }
        if let Ok(mut advancing) = advance_for_cancel.lock() {
            *advancing = false;
        }
        if let Some(ui) = ui_weak_cancel.upgrade() {
            ui.set_is_loading(false);
            if ui.get_track_status() == "LOADING..." {
                ui.set_track_status(slint::SharedString::from(
                    if ui.get_is_playing() { "PLAYING" } else { "READY" }
                ));
            }
        }
    });

    let player_for_stop = Arc::clone(&ctx.player);
    let smtc_for_stop = ctx.smtc;
    let ui_weak_stop = ctx.ui_weak;
    let abort_for_stop = ctx.loading_abort;
    app.on_stop(move || {
        if let Ok(mut guard) = abort_for_stop.lock()
            && let Some(handle) = guard.take() {
                handle.abort();
            }
        player_for_stop.stop();
        if let Some(smtc_arc) = &smtc_for_stop
            && let Ok(mut lock) = smtc_arc.lock() {
                lock.0.set_playback(false);
            }
        if let Some(ui) = ui_weak_stop.upgrade() {
            ui.set_is_playing(false);
            ui.set_can_retry(false);
            ui.set_track_status(slint::SharedString::from("STOPPED"));
            ui.set_progress_ratio(0.0);
            ui.set_buffer_ratio(0.0);
            ui.set_time_current(slint::SharedString::from("00:00"));
            ui.set_time_total(slint::SharedString::from("00:00"));
            ui.set_time_display(slint::SharedString::from("00:00 / 00:00"));
        }
    });
}

// ============================================================================
// 6. Scrubbing, Seeking, Volume & Mute Controls
// ============================================================================
fn bind_seek_and_volume_controls(
    app: &AppWindow,
    player: Arc<AudioPlayer>,
    smtc: SmtcHandle,
    pending_seek: Arc<Mutex<Option<PendingSeekState>>>,
) {
    let player_for_scrub = Arc::clone(&player);
    let ui_weak_scrub = app.as_weak();
    app.on_on_user_scrub(move |ratio| {
        let total_dur = player_for_scrub.total_duration();
        let target_secs = ratio * total_dur;
        let cur_m = (target_secs as u64) / 60;
        let cur_s = (target_secs as u64) % 60;
        let tot_m = (total_dur as u64) / 60;
        let tot_s = (total_dur as u64) % 60;
        if let Some(ui) = ui_weak_scrub.upgrade() {
            ui.set_time_current(slint::SharedString::from(format!("{:02}:{:02}", cur_m, cur_s)));
            ui.set_time_total(slint::SharedString::from(format!("{:02}:{:02}", tot_m, tot_s)));
            ui.set_time_display(slint::SharedString::from(format!(
                "{:02}:{:02} / {:02}:{:02}",
                cur_m, cur_s, tot_m, tot_s
            )));
        }
    });

    let seek_seq = Arc::new(AtomicU64::new(0));
    let seek_lock = Arc::new(tokio::sync::Mutex::new(()));
    let player_for_seek = Arc::clone(&player);
    let ui_weak_seek = app.as_weak();
    let smtc_for_seek = smtc;
    let pending_seek_for_seek = pending_seek;
    let seek_seq_for_seek = Arc::clone(&seek_seq);
    let seek_lock_for_seek = Arc::clone(&seek_lock);
    app.on_on_user_seek(move |ratio| {
        let current_seq = seek_seq_for_seek.fetch_add(1, Ordering::SeqCst) + 1;
        let total_dur = player_for_seek.total_duration();
        let target_secs = ratio * total_dur;
        info!(
            "Executing user seek #{} to {:.1}s ({:.1}%)",
            current_seq,
            target_secs,
            ratio * 100.0
        );

        if let Some(ui) = ui_weak_seek.upgrade() {
            ui.set_progress_ratio(ratio);
            let cur_m = (target_secs as u64) / 60;
            let cur_s = (target_secs as u64) % 60;
            let tot_m = (total_dur as u64) / 60;
            let tot_s = (total_dur as u64) % 60;
            ui.set_time_current(slint::SharedString::from(format!("{:02}:{:02}", cur_m, cur_s)));
            ui.set_time_total(slint::SharedString::from(format!("{:02}:{:02}", tot_m, tot_s)));
            ui.set_time_display(slint::SharedString::from(format!(
                "{:02}:{:02} / {:02}:{:02}",
                cur_m, cur_s, tot_m, tot_s
            )));
            ui.set_track_status(slint::SharedString::from("BUFFERING"));
        }

        *pending_seek_for_seek.lock().unwrap() = Some(PendingSeekState {
            id: current_seq,
            target_ratio: ratio,
            target_secs,
            requested_at: Instant::now(),
        });

        let player = Arc::clone(&player_for_seek);
        let ui_weak = ui_weak_seek.clone();
        let smtc = smtc_for_seek.clone();
        let pending_seek = Arc::clone(&pending_seek_for_seek);
        let seek_seq = Arc::clone(&seek_seq_for_seek);
        let seek_lock = Arc::clone(&seek_lock_for_seek);

        tokio::spawn(async move {
            let _guard = seek_lock.lock().await;
            if seek_seq.load(Ordering::SeqCst) != current_seq {
                return;
            }

            let result = tokio::task::spawn_blocking(move || player.seek_to(target_secs)).await;
            if seek_seq.load(Ordering::SeqCst) == current_seq {
                if matches!(result, Ok(Ok(()))) {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_is_playing(true);
                        ui.set_track_status(slint::SharedString::from("PLAYING"));
                    }
                    if let Some(smtc_arc) = &smtc
                        && let Ok(mut lock) = smtc_arc.lock() {
                            lock.0.set_playback(true);
                        }
                }
                let mut lock = pending_seek.lock().unwrap();
                if let Some(p) = lock.as_ref()
                    && p.id == current_seq {
                        *lock = None;
                    }
            }
        });
    });

    let player_for_vol = Arc::clone(&player);
    let ui_weak_vol = app.as_weak();
    let saved_volume = Arc::new(Mutex::new(1.0f32));
    let saved_vol_for_slider = Arc::clone(&saved_volume);
    app.on_on_user_volume(move |vol_ratio| {
        player_for_vol.set_volume(vol_ratio);
        if vol_ratio > 0.0 {
            *saved_vol_for_slider.lock().unwrap() = vol_ratio;
        }
        if let Some(ui) = ui_weak_vol.upgrade() {
            ui.set_is_muted(vol_ratio <= 0.0);
        }
    });

    let player_for_mute = player;
    let ui_weak_mute = app.as_weak();
    let saved_vol_for_mute = saved_volume;
    app.on_toggle_mute(move || {
        let Some(ui) = ui_weak_mute.upgrade() else { return };
        let currently_muted = ui.get_is_muted();
        if currently_muted {
            let restore_vol = *saved_vol_for_mute.lock().unwrap();
            let vol = if restore_vol <= 0.0 { 1.0 } else { restore_vol };
            player_for_mute.set_volume(vol);
            ui.set_volume_ratio(vol);
            ui.set_is_muted(false);
        } else {
            let current_vol = ui.get_volume_ratio();
            if current_vol > 0.0 {
                *saved_vol_for_mute.lock().unwrap() = current_vol;
            }
            player_for_mute.set_volume(0.0);
            ui.set_volume_ratio(0.0);
            ui.set_is_muted(true);
        }
    });
}
