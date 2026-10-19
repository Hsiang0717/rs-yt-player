use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::AbortHandle;
use tracing::info;

use crate::library::TrackRecord;
use crate::ui_models::{
    apply_queue_expanded, cache_track_cover, is_url, queue_items_model, record_from_search,
    records_model,
};
use crate::AppWindow;

use super::PlaybackContext;

#[derive(Default)]
struct SearchCoordinator {
    last_query: Option<String>,
    last_requested_at: Option<Instant>,
    active_abort: Option<AbortHandle>,
    is_searching: bool,
}

pub fn bind_search_controls(
    app: &AppWindow,
    ctx: PlaybackContext,
    search_records: Arc<Mutex<Vec<TrackRecord>>>,
) {
    let search_generation = Arc::new(AtomicU64::new(0));
    let coordinator = Arc::new(Mutex::new(SearchCoordinator::default()));

    // on_search_and_play
    let ctx_for_search = ctx.clone();
    let search_records_for_search = Arc::clone(&search_records);
    let search_generation_for_search = Arc::clone(&search_generation);
    let coordinator_for_search = Arc::clone(&coordinator);
    app.on_search_and_play(move |query| {
        let query_str = query.trim().to_string();
        if query_str.is_empty() {
            return;
        }

        if is_url(&query_str) {
            ctx_for_search.start_track(query_str.clone(), !query_str.contains("list="));
            return;
        }

        // Defense 1 & 2: Throttle & Duplicate Query Guard
        {
            let mut state = coordinator_for_search.lock().unwrap();
            let now = Instant::now();

            // Throttle: Ignore requests within 400ms
            if let Some(last_time) = state.last_requested_at {
                if now.duration_since(last_time) < Duration::from_millis(400) {
                    info!("Search throttled: requested too rapidly");
                    return;
                }
            }

            // Duplicate query: Ignore if same query is currently searching or already populated
            let has_existing_results = !search_records_for_search.lock().unwrap().is_empty();
            if let Some(ref last_q) = state.last_query {
                if last_q == &query_str && (state.is_searching || has_existing_results) {
                    info!("Duplicate search ignored for query: {}", query_str);
                    let ui_for_dup = ctx_for_search.ui_weak.clone();
                    let _ = ui_for_dup.upgrade_in_event_loop(|ui| {
                        apply_queue_expanded(&ui, true);
                        ui.set_panel_tab(slint::SharedString::from("SEARCH"));
                    });
                    return;
                }
            }

            state.last_query = Some(query_str.clone());
            state.last_requested_at = Some(now);
            state.is_searching = true;

            // Defense 3: Cancel and kill previous search task / process
            if let Some(prev_abort) = state.active_abort.take() {
                prev_abort.abort();
                info!("Aborted previous search task");
            }
        }

        let token = search_generation_for_search.fetch_add(1, Ordering::SeqCst) + 1;
        let ui_for_start = ctx_for_search.ui_weak.clone();
        let library_for_start = ctx_for_search.library.clone();

        // Defense 4: UI State Isolation.
        // Keep active playing track title/status intact; update only the search panel.
        let _ = ui_for_start.upgrade_in_event_loop(move |ui| {
            apply_queue_expanded(&ui, true);
            ui.set_panel_tab(slint::SharedString::from("SEARCH"));
            ui.set_panel_message(slint::SharedString::from("Searching YouTube…"));
            ui.set_search_results(records_model(Vec::new(), &library_for_start, None));
        });

        let search_records = Arc::clone(&search_records_for_search);
        let search_gen = Arc::clone(&search_generation_for_search);
        let coordinator_task = Arc::clone(&coordinator_for_search);
        let extractor = Arc::clone(&ctx_for_search.extractor);
        let library = ctx_for_search.library.clone();
        let ui_weak = ctx_for_search.ui_weak.clone();

        let join_handle = tokio::spawn(async move {
            let search_result = extractor.search_tracks(&query_str).await;

            // Update coordinator status
            {
                let mut state = coordinator_task.lock().unwrap();
                if search_gen.load(Ordering::SeqCst) == token {
                    state.is_searching = false;
                    state.active_abort = None;
                }
            }

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
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            if gen_for_ui.load(Ordering::SeqCst) == token {
                                let curr_key = ui.get_current_track_key();
                                ui.set_search_results(records_model(
                                    records_for_ui,
                                    &library_for_ui,
                                    Some(&curr_key),
                                ));
                                ui.set_panel_message(slint::SharedString::from(message));
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
                                    let curr_key = ui.get_current_track_key();
                                    ui.set_search_results(records_model(
                                        records,
                                        &library_for_ui,
                                        Some(&curr_key),
                                    ));
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
                                ui.set_panel_message(slint::SharedString::from(message));
                            }
                        });
                    }
                }
            }
        });

        // Store active abort handle
        coordinator_for_search.lock().unwrap().active_abort = Some(join_handle.abort_handle());
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
                let curr_key = ui.get_current_track_key();
                ui.set_search_results(records_model(results, &library_for_toggle, Some(&curr_key)));
                ui.set_favorite_items(records_model(
                    library_for_toggle.favorites(),
                    &library_for_toggle,
                    Some(&curr_key),
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
