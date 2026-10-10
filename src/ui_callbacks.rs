use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::info;

use crate::extractor::Extractor;
use crate::library::{LibraryStore, TrackRecord};
use crate::playback_coordinator::{
    play_with_metadata, start_track_playback, trigger_next, trigger_previous, LoadingAbortHandle,
    SmtcHandle,
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
) {
    // --- 1. Search and Play Callback ---
    let player_for_play = Arc::clone(&player);
    let extractor_for_play = Arc::clone(&extractor);
    let smtc_for_play = smtc.clone();
    let ui_weak_play = app.as_weak();
    let queue_for_play = play_queue.clone();
    let library_for_play = library.clone();
    let last_query_for_play = Arc::clone(&last_query);
    let search_records_for_search = Arc::clone(&search_records);
    let search_generation = Arc::new(AtomicU64::new(0));
    let search_generation_for_search = Arc::clone(&search_generation);
    let loading_abort_for_search = Arc::clone(&current_loading_abort);

    app.on_search_and_play(move |query| {
        let query_str = query.to_string();
        if query_str.trim().is_empty() {
            return;
        }
        let player = Arc::clone(&player_for_play);
        let extractor = Arc::clone(&extractor_for_play);
        let smtc = smtc_for_play.clone();
        let ui_weak = ui_weak_play.clone();
        let queue = queue_for_play.clone();
        let library = library_for_play.clone();
        let last_query = Arc::clone(&last_query_for_play);
        let token = search_generation_for_search.fetch_add(1, Ordering::SeqCst) + 1;

        if is_url(&query_str) {
            start_track_playback(
                query_str.clone(),
                extractor,
                player,
                smtc,
                ui_weak,
                queue,
                library,
                last_query,
                !query_str.contains("list="),
                loading_abort_for_search.clone(),
            );
            return;
        }

        let ui_for_start = ui_weak.clone();
        let library_for_start = library.clone();
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
        let search_generation = Arc::clone(&search_generation_for_search);
        let loading_abort = loading_abort_for_search.clone();
        tokio::spawn(async move {
            let search_result = extractor.search_tracks(&query_str).await;
            match search_result {
                Ok(results) => {
                    let records: Vec<TrackRecord> =
                        results.into_iter().map(record_from_search).collect();
                    let records_are_current = {
                        let mut saved_records = search_records.lock().unwrap();
                        if search_generation.load(Ordering::SeqCst) == token {
                            *saved_records = records.clone();
                            true
                        } else {
                            false
                        }
                    };
                    if records_are_current && search_generation.load(Ordering::SeqCst) == token {
                        let message = if records.is_empty() {
                            "No results found".to_string()
                        } else {
                            format!("{} results found", records.len())
                        };
                        let records_for_ui = records.clone();
                        let library_for_ui = library.clone();
                        let generation_for_ui = Arc::clone(&search_generation);
                        let is_empty = records.is_empty();
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            if generation_for_ui.load(Ordering::SeqCst) == token {
                                ui.set_search_results(records_model(
                                    records_for_ui,
                                    &library_for_ui,
                                ));
                                ui.set_panel_message(slint::SharedString::from(message));
                                if is_empty {
                                    ui.set_is_loading(false);
                                    ui.set_track_status(slint::SharedString::from("NO RESULTS"));
                                }
                            }
                        });

                        // Auto-play top match directly without forcing user to click
                        if let Some(best) = records.first() {
                            let best_url = best.video_url();
                            start_track_playback(
                                best_url,
                                extractor,
                                player,
                                smtc,
                                ui_weak.clone(),
                                queue,
                                library.clone(),
                                last_query,
                                true,
                                loading_abort,
                            );
                        }
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
                        if search_generation.load(Ordering::SeqCst) == token {
                            let generation_for_ui = Arc::clone(&search_generation);
                            let library_for_ui = library.clone();
                            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                                if generation_for_ui.load(Ordering::SeqCst) == token {
                                    ui.set_search_results(records_model(records, &library_for_ui));
                                }
                            });
                        }
                    }
                }
                Err(error) => {
                    let message = format!("Search failed: {}", error);
                    if search_generation.load(Ordering::SeqCst) == token {
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            if search_generation.load(Ordering::SeqCst) == token {
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

    // --- 2. Queue and Panel Navigation ---
    let queue_for_toggle = play_queue.clone();
    let library_for_toggle = library.clone();
    let ui_weak_queue = app.as_weak();
    app.on_toggle_queue(move || {
        if let Some(ui) = ui_weak_queue.upgrade() {
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

    // --- 3. Search Results Interaction ---
    let queue_for_add = play_queue.clone();
    let library_for_add = library.clone();
    let search_records_for_add = Arc::clone(&search_records);
    let ui_weak_add = app.as_weak();
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

    let library_for_toggle_favorite = library.clone();
    let search_records_for_favorite = Arc::clone(&search_records);
    let ui_weak_favorite = app.as_weak();
    app.on_toggle_search_favorite(move |key| {
        let key = key.to_string();
        let record = search_records_for_favorite
            .lock()
            .unwrap()
            .iter()
            .find(|record| record.video_url() == key)
            .cloned();
        if let Some(record) = record {
            library_for_toggle_favorite.toggle_favorite(record);
            if let Some(ui) = ui_weak_favorite.upgrade() {
                let results = search_records_for_favorite.lock().unwrap().clone();
                ui.set_search_results(records_model(results, &library_for_toggle_favorite));
                ui.set_favorite_items(records_model(
                    library_for_toggle_favorite.favorites(),
                    &library_for_toggle_favorite,
                ));
                ui.set_panel_message(slint::SharedString::from("Favorites updated"));
            }
        }
    });

    let player_for_search_result = Arc::clone(&player);
    let extractor_for_search_result = Arc::clone(&extractor);
    let smtc_for_search_result = smtc.clone();
    let ui_weak_search_result = app.as_weak();
    let queue_for_search_result = play_queue.clone();
    let library_for_search_result = library.clone();
    let last_query_for_search_result = Arc::clone(&last_query);
    let loading_abort_for_search_result = Arc::clone(&current_loading_abort);
    app.on_play_search_result(move |key| {
        start_track_playback(
            key.to_string(),
            Arc::clone(&extractor_for_search_result),
            Arc::clone(&player_for_search_result),
            smtc_for_search_result.clone(),
            ui_weak_search_result.clone(),
            queue_for_search_result.clone(),
            library_for_search_result.clone(),
            Arc::clone(&last_query_for_search_result),
            true,
            loading_abort_for_search_result.clone(),
        );
    });

    // --- 4. Favorites & History Interaction ---
    let player_for_favorite = Arc::clone(&player);
    let extractor_for_favorite = Arc::clone(&extractor);
    let smtc_for_favorite = smtc.clone();
    let ui_weak_favorite_play = app.as_weak();
    let queue_for_favorite = play_queue.clone();
    let library_for_favorite = library.clone();
    let last_query_for_favorite = Arc::clone(&last_query);
    let loading_abort_for_favorite = Arc::clone(&current_loading_abort);
    app.on_play_favorite(move |key| {
        start_track_playback(
            key.to_string(),
            Arc::clone(&extractor_for_favorite),
            Arc::clone(&player_for_favorite),
            smtc_for_favorite.clone(),
            ui_weak_favorite_play.clone(),
            queue_for_favorite.clone(),
            library_for_favorite.clone(),
            Arc::clone(&last_query_for_favorite),
            true,
            loading_abort_for_favorite.clone(),
        );
    });

    let library_for_remove_favorite = library.clone();
    let search_records_for_remove_favorite = Arc::clone(&search_records);
    let ui_weak_remove_favorite = app.as_weak();
    app.on_remove_favorite(move |key| {
        let id = query_video_id(key.as_str());
        if let Some(track) = library_for_remove_favorite
            .favorites()
            .into_iter()
            .find(|track| track.id == id)
        {
            library_for_remove_favorite.toggle_favorite(track);
            if let Some(ui) = ui_weak_remove_favorite.upgrade() {
                ui.set_favorite_items(records_model(
                    library_for_remove_favorite.favorites(),
                    &library_for_remove_favorite,
                ));
                ui.set_search_results(records_model(
                    search_records_for_remove_favorite.lock().unwrap().clone(),
                    &library_for_remove_favorite,
                ));
                ui.set_current_is_favorite(library_for_remove_favorite.is_favorite(&id));
            }
        }
    });

    let player_for_history = Arc::clone(&player);
    let extractor_for_history = Arc::clone(&extractor);
    let smtc_for_history = smtc.clone();
    let ui_weak_history = app.as_weak();
    let queue_for_history = play_queue.clone();
    let library_for_history = library.clone();
    let last_query_for_history = Arc::clone(&last_query);
    let loading_abort_for_history = Arc::clone(&current_loading_abort);
    app.on_play_history(move |key| {
        start_track_playback(
            key.to_string(),
            Arc::clone(&extractor_for_history),
            Arc::clone(&player_for_history),
            smtc_for_history.clone(),
            ui_weak_history.clone(),
            queue_for_history.clone(),
            library_for_history.clone(),
            Arc::clone(&last_query_for_history),
            true,
            loading_abort_for_history.clone(),
        );
    });

    let library_for_history_favorite = library.clone();
    let ui_weak_history_favorite = app.as_weak();
    app.on_favorite_history(move |key| {
        let id = query_video_id(key.as_str());
        if let Some(track) = library_for_history_favorite
            .recent()
            .into_iter()
            .find(|track| track.id == id)
        {
            library_for_history_favorite.toggle_favorite(track);
            if let Some(ui) = ui_weak_history_favorite.upgrade() {
                ui.set_history_items(records_model(
                    library_for_history_favorite.recent(),
                    &library_for_history_favorite,
                ));
                ui.set_favorite_items(records_model(
                    library_for_history_favorite.favorites(),
                    &library_for_history_favorite,
                ));
                ui.set_current_is_favorite(library_for_history_favorite.is_favorite(&id));
            }
        }
    });

    let library_for_current_favorite = library.clone();
    let ui_weak_current_favorite = app.as_weak();
    app.on_toggle_current_favorite(move || {
        let Some(ui) = ui_weak_current_favorite.upgrade() else {
            return;
        };
        let key = ui.get_current_track_key().to_string();
        if key.is_empty() {
            return;
        }
        let id = query_video_id(&key);
        let record = library_for_current_favorite
            .recent()
            .into_iter()
            .find(|track| track.id == id)
            .or_else(|| {
                library_for_current_favorite
                    .favorites()
                    .into_iter()
                    .find(|track| track.id == id)
            });
        if let Some(record) = record {
            let is_favorite = library_for_current_favorite.toggle_favorite(record);
            ui.set_current_is_favorite(is_favorite);
            ui.set_favorite_items(records_model(
                library_for_current_favorite.favorites(),
                &library_for_current_favorite,
            ));
            ui.set_history_items(records_model(
                library_for_current_favorite.recent(),
                &library_for_current_favorite,
            ));
        }
    });

    // --- 5. Mode Toggles (Repeat & Shuffle) ---
    let queue_for_repeat = play_queue.clone();
    let library_for_repeat = library.clone();
    let ui_weak_repeat = app.as_weak();
    app.on_cycle_repeat_mode(move || {
        let mode = queue_for_repeat.repeat_mode().next();
        queue_for_repeat.set_repeat_mode(mode);
        library_for_repeat.set_repeat_mode(mode);
        if let Some(ui) = ui_weak_repeat.upgrade() {
            ui.set_repeat_mode_label(slint::SharedString::from(mode.label()));
        }
    });

    let queue_for_shuffle = play_queue.clone();
    let library_for_shuffle = library.clone();
    let ui_weak_shuffle = app.as_weak();
    app.on_toggle_shuffle(move || {
        let enabled = !queue_for_shuffle.shuffle_enabled();
        queue_for_shuffle.set_shuffle_enabled(enabled);
        library_for_shuffle.set_shuffle_enabled(enabled);
        if let Some(ui) = ui_weak_shuffle.upgrade() {
            ui.set_shuffle_enabled(enabled);
        }
    });

    let last_query_for_retry = Arc::clone(&last_query);
    let player_for_retry = Arc::clone(&player);
    let extractor_for_retry = Arc::clone(&extractor);
    let smtc_for_retry = smtc.clone();
    let ui_weak_retry = app.as_weak();
    let queue_for_retry = play_queue.clone();
    let library_for_retry = library.clone();
    let loading_abort_for_retry = Arc::clone(&current_loading_abort);
    app.on_retry_playback(move || {
        let query = last_query_for_retry.lock().unwrap().clone();
        if let Some(query) = query {
            start_track_playback(
                query,
                Arc::clone(&extractor_for_retry),
                Arc::clone(&player_for_retry),
                smtc_for_retry.clone(),
                ui_weak_retry.clone(),
                queue_for_retry.clone(),
                library_for_retry.clone(),
                Arc::clone(&last_query_for_retry),
                false,
                loading_abort_for_retry.clone(),
            );
        }
    });

    // --- 6. Queue Management Items ---
    let queue_for_remove = play_queue.clone();
    let library_for_remove = library.clone();
    let ui_weak_remove = app.as_weak();
    app.on_remove_queue_item(move |index| {
        if queue_for_remove.remove_at(index as usize).is_some() {
            if let Some(ui) = ui_weak_remove.upgrade() {
                ui.set_queue_items(queue_items_model(&queue_for_remove, &library_for_remove));
                ui.set_panel_message(slint::SharedString::from("Removed from queue"));
            }
        }
    });

    let queue_for_move_up = play_queue.clone();
    let library_for_move_up = library.clone();
    let ui_weak_move_up = app.as_weak();
    app.on_move_queue_item_up(move |index| {
        let index = index as usize;
        if index > 0 && queue_for_move_up.move_item(index, index - 1) {
            if let Some(ui) = ui_weak_move_up.upgrade() {
                ui.set_queue_items(queue_items_model(&queue_for_move_up, &library_for_move_up));
            }
        }
    });

    let queue_for_move_down = play_queue.clone();
    let library_for_move_down = library.clone();
    let ui_weak_move_down = app.as_weak();
    app.on_move_queue_item_down(move |index| {
        let index = index as usize;
        if queue_for_move_down.move_item(index, index + 1) {
            if let Some(ui) = ui_weak_move_down.upgrade() {
                ui.set_queue_items(queue_items_model(
                    &queue_for_move_down,
                    &library_for_move_down,
                ));
            }
        }
    });

    let queue_for_clear = play_queue.clone();
    let library_for_clear = library.clone();
    let ui_weak_clear = app.as_weak();
    app.on_clear_queue(move || {
        queue_for_clear.clear_upcoming();
        if let Some(ui) = ui_weak_clear.upgrade() {
            ui.set_queue_items(queue_items_model(&queue_for_clear, &library_for_clear));
            ui.set_panel_message(slint::SharedString::from("Queue cleared"));
        }
    });

    let queue_for_play_item = play_queue.clone();
    let library_for_play_item = library.clone();
    let extractor_for_play_item = Arc::clone(&extractor);
    let player_for_play_item = Arc::clone(&player);
    let smtc_for_play_item = smtc.clone();
    let ui_weak_play_item = app.as_weak();
    let advance_for_play_item = Arc::clone(&is_auto_advancing);
    let last_query_for_play_item = Arc::clone(&last_query);
    let loading_abort_for_play_item = Arc::clone(&current_loading_abort);
    app.on_play_queue_item(move |index| {
        let mut advancing = advance_for_play_item.lock().unwrap();
        if *advancing {
            return;
        }
        let Some((query, preloaded)) = queue_for_play_item.pop_at(index as usize) else {
            return;
        };
        *advancing = true;
        drop(advancing);
        if let Some(ui) = ui_weak_play_item.upgrade() {
            ui.set_queue_items(queue_items_model(
                &queue_for_play_item,
                &library_for_play_item,
            ));
        }
        let queue = queue_for_play_item.clone();
        let library = library_for_play_item.clone();
        let extractor = Arc::clone(&extractor_for_play_item);
        let player = Arc::clone(&player_for_play_item);
        let smtc = smtc_for_play_item.clone();
        let ui_weak = ui_weak_play_item.clone();
        let advance = Arc::clone(&advance_for_play_item);
        let last_query = Arc::clone(&last_query_for_play_item);
        let abort_handle = loading_abort_for_play_item.clone();
        let join_handle = tokio::spawn(async move {
            if let Some(preloaded) = preloaded {
                let cache_path = library.cover_path(&preloaded.meta.id);
                if !cache_path.exists() {
                    if let Some(path) = &preloaded.cover_path {
                        if let Ok(bytes) = std::fs::read(path) {
                            let _ = library.store_cover(&preloaded.meta.id, &bytes);
                        }
                    }
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
                crate::playback_coordinator::play_track(
                    query, extractor, player, smtc, ui_weak, queue, library, last_query,
                )
                .await;
            }
            *advance.lock().unwrap() = false;
        });
        if let Ok(mut guard) = abort_handle.lock() {
            if let Some(prev) = guard.replace(join_handle.abort_handle()) {
                prev.abort();
            }
        }
    });

    // --- 7. Playback Control Callbacks ---
    let player_for_toggle = Arc::clone(&player);
    let smtc_for_toggle = smtc.clone();
    let ui_weak_toggle = app.as_weak();
    app.on_toggle_play_pause(move || {
        let player = &player_for_toggle;
        let is_paused = player.is_paused();
        if is_paused {
            player.resume();
        } else {
            player.pause();
        }

        if let Some(smtc_arc) = &smtc_for_toggle {
            if let Ok(mut lock) = smtc_arc.lock() {
                lock.0.set_playback(is_paused);
            }
        }

        if let Some(ui) = ui_weak_toggle.upgrade() {
            ui.set_is_playing(is_paused);
            ui.set_track_status(if is_paused {
                slint::SharedString::from("PLAYING")
            } else {
                slint::SharedString::from("PAUSED")
            });
        }
    });

    let player_for_prev = Arc::clone(&player);
    let extractor_for_prev = Arc::clone(&extractor);
    let smtc_for_prev = smtc.clone();
    let ui_weak_prev = app.as_weak();
    let queue_for_prev = play_queue.clone();
    let library_for_prev = library.clone();
    let is_advancing_prev = Arc::clone(&is_auto_advancing);
    let last_query_for_prev = Arc::clone(&last_query);
    let loading_abort_for_prev = Arc::clone(&current_loading_abort);
    app.on_play_previous(move || {
        trigger_previous(
            Arc::clone(&extractor_for_prev),
            Arc::clone(&player_for_prev),
            smtc_for_prev.clone(),
            ui_weak_prev.clone(),
            queue_for_prev.clone(),
            library_for_prev.clone(),
            Arc::clone(&is_advancing_prev),
            Arc::clone(&last_query_for_prev),
            loading_abort_for_prev.clone(),
        );
    });

    let player_for_next = Arc::clone(&player);
    let extractor_for_next = Arc::clone(&extractor);
    let smtc_for_next = smtc.clone();
    let ui_weak_next = app.as_weak();
    let queue_for_next = play_queue.clone();
    let library_for_next = library.clone();
    let is_advancing_next = Arc::clone(&is_auto_advancing);
    let last_query_for_next = Arc::clone(&last_query);
    let loading_abort_for_next = Arc::clone(&current_loading_abort);
    app.on_play_next(move || {
        trigger_next(
            Arc::clone(&extractor_for_next),
            Arc::clone(&player_for_next),
            smtc_for_next.clone(),
            ui_weak_next.clone(),
            queue_for_next.clone(),
            library_for_next.clone(),
            Arc::clone(&is_advancing_next),
            false,
            Arc::clone(&last_query_for_next),
            loading_abort_for_next.clone(),
        );
    });

    let loading_abort_for_cancel = Arc::clone(&current_loading_abort);
    let ui_weak_cancel = app.as_weak();
    let advance_for_cancel = Arc::clone(&is_auto_advancing);
    app.on_cancel_loading(move || {
        if let Ok(mut guard) = loading_abort_for_cancel.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
                info!("Active loading task was aborted by user.");
            }
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

    let player_for_stop = Arc::clone(&player);
    let smtc_for_stop = smtc.clone();
    let ui_weak_stop = app.as_weak();
    let queue_for_stop = play_queue.clone();
    let library_for_stop = library.clone();
    let loading_abort_for_stop = Arc::clone(&current_loading_abort);
    app.on_stop(move || {
        if let Ok(mut guard) = loading_abort_for_stop.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
            }
        }
        player_for_stop.stop();
        queue_for_stop.clear();
        if let Some(smtc_arc) = &smtc_for_stop {
            if let Ok(mut lock) = smtc_arc.lock() {
                lock.0.set_playback(false);
            }
        }
        if let Some(ui) = ui_weak_stop.upgrade() {
            ui.set_queue_items(queue_items_model(&queue_for_stop, &library_for_stop));
            ui.set_is_playing(false);
            ui.set_can_retry(false);
            ui.set_track_status(slint::SharedString::from("STOPPED"));
            ui.set_has_active_track(false);
            ui.set_progress_ratio(0.0);
            ui.set_buffer_ratio(0.0);
            ui.set_time_display(slint::SharedString::from("00:00 / 00:00"));
        }
    });

    // --- 8. Scrubbing and Seeking ---
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
    let smtc_for_seek = smtc.clone();
    let pending_seek_for_seek = Arc::clone(&pending_seek);
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
                    if let Some(smtc_arc) = &smtc {
                        if let Ok(mut lock) = smtc_arc.lock() {
                            lock.0.set_playback(true);
                        }
                    }
                }
                let mut lock = pending_seek.lock().unwrap();
                if let Some(p) = lock.as_ref() {
                    if p.id == current_seq {
                        *lock = None;
                    }
                }
            }
        });
    });

    // --- 9. Volume & Mute ---
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

    let player_for_mute = Arc::clone(&player);
    let ui_weak_mute = app.as_weak();
    let saved_vol_for_mute = Arc::clone(&saved_volume);
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
