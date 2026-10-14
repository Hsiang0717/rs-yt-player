use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::library::TrackRecord;
use crate::ui_models::{
    apply_queue_expanded, cache_track_cover, is_url, queue_items_model, record_from_search,
    records_model,
};
use crate::AppWindow;

use super::PlaybackContext;

pub fn bind_search_controls(
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
