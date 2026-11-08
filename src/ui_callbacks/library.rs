use std::sync::{Arc, Mutex};

use crate::AppWindow;
use crate::library::TrackRecord;
use crate::ui_models::{query_video_id, records_model, sync_queue_to_ui};

use super::PlaybackContext;

fn resolve_tab_records(
    tab: &str,
    search_records: &Arc<Mutex<Vec<TrackRecord>>>,
    library: &crate::library::LibraryStore,
) -> Vec<TrackRecord> {
    match tab {
        "SEARCH" => search_records.lock().unwrap().clone(),
        "SAVED" => library.favorites(),
        "HISTORY" => library.recent(),
        _ => Vec::new(),
    }
}

pub fn bind_library_controls(
    app: &AppWindow,
    ctx: PlaybackContext,
    search_records: Arc<Mutex<Vec<TrackRecord>>>,
) {
    let ctx_for_play_fav = ctx.clone();
    app.on_play_favorite(move |key| {
        let key_str = key.to_string();
        let target_id = query_video_id(&key_str);
        if let Some(track) = ctx_for_play_fav
            .library
            .favorites()
            .into_iter()
            .find(|r| r.id == target_id)
        {
            let url = track.video_url();
            ctx_for_play_fav.queue.set_track_info(
                &url,
                track.title,
                track.artist,
                track.duration_secs,
                track.thumbnail_url,
            );
            ctx_for_play_fav.queue.record_current(url.clone());
            ctx_for_play_fav
                .library
                .save_queue(ctx_for_play_fav.queue.to_track_records());
            if let Some(ui) = ctx_for_play_fav.ui_weak.upgrade() {
                sync_queue_to_ui(
                    &ui,
                    &ctx_for_play_fav.queue,
                    &ctx_for_play_fav.library,
                );
            }
            ctx_for_play_fav.start_track(url, false);
        } else {
            ctx_for_play_fav.start_track(key_str, false);
        }
    });

    // Batch Enqueue All: appends all items in current tab to upcoming queue
    let ctx_for_enqueue_all = ctx.clone();
    let search_for_enqueue_all = Arc::clone(&search_records);
    app.on_enqueue_all(move |tab| {
        let records = resolve_tab_records(
            tab.as_str(),
            &search_for_enqueue_all,
            &ctx_for_enqueue_all.library,
        );
        if records.is_empty() {
            return;
        }
        let count = records.len();
        for record in records {
            let url = record.video_url();
            ctx_for_enqueue_all.queue.set_track_info(
                &url,
                record.title,
                record.artist,
                record.duration_secs,
                record.thumbnail_url,
            );
            ctx_for_enqueue_all.queue.push_back(url);
        }
        ctx_for_enqueue_all
            .library
            .save_queue(ctx_for_enqueue_all.queue.to_track_records());
        if let Some(ui) = ctx_for_enqueue_all.ui_weak.upgrade() {
            sync_queue_to_ui(
                &ui,
                &ctx_for_enqueue_all.queue,
                &ctx_for_enqueue_all.library,
            );
            ui.set_panel_message(slint::SharedString::from(format!(
                "Added {} tracks to playlist",
                count
            )));
        }
    });

    let library_for_remove = ctx.library.clone();
    let queue_for_remove = ctx.queue.clone();
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
                let curr_key = ui.get_current_track_key();
                ui.set_favorite_items(records_model(
                    library_for_remove.favorites(),
                    &library_for_remove,
                    Some(&curr_key),
                ));
                let results = search_records_for_remove.lock().unwrap().clone();
                ui.set_search_results(records_model(
                    results,
                    &library_for_remove,
                    Some(&curr_key),
                ));
                sync_queue_to_ui(&ui, &queue_for_remove, &library_for_remove);
                ui.set_current_is_favorite(library_for_remove.is_favorite(&id));
                ui.set_panel_message(slint::SharedString::from("Removed from favorites"));
            }
        }
    });

    let extractor_for_curr_fav = Arc::clone(&ctx.extractor);
    let library_for_curr_fav = ctx.library;
    let queue_for_curr_fav = ctx.queue;
    let search_records_for_curr_fav = search_records;
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
            .find_track(&id)
            .or_else(|| {
                search_records_for_curr_fav
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|r| r.id == id)
                    .cloned()
            })
            .or_else(|| {
                queue_for_curr_fav
                    .all_playlist_items()
                    .into_iter()
                    .find(|item| query_video_id(&item.query) == id)
                    .map(|item| TrackRecord {
                        id: id.clone(),
                        title: item.title,
                        artist: item.artist,
                        duration_secs: item.duration_secs,
                        thumbnail_url: item.thumbnail_url,
                    })
            });
        if let Some(record) = record {
            let is_favorite = library_for_curr_fav.toggle_favorite(record.clone());
            if is_favorite && library_for_curr_fav.find_cached_audio(&record.id).is_none() {
                let extractor_bg = Arc::clone(&extractor_for_curr_fav);
                let cache_dir = library_for_curr_fav.audio_cache_dir();
                let track_id = record.id.clone();
                let ui_weak_refresh = ui_weak_curr_fav.clone();
                let queue_refresh = queue_for_curr_fav.clone();
                let library_refresh = library_for_curr_fav.clone();
                tokio::spawn(async move {
                    if let Ok(_) =
                        extractor_bg.download_audio_track(&track_id, &cache_dir).await
                    {
                        let _ = ui_weak_refresh.upgrade_in_event_loop(move |ui| {
                            sync_queue_to_ui(&ui, &queue_refresh, &library_refresh);
                        });
                    }
                });
            }
            ui.set_current_is_favorite(is_favorite);
            let curr_key = ui.get_current_track_key();
            ui.set_favorite_items(records_model(
                library_for_curr_fav.favorites(),
                &library_for_curr_fav,
                Some(&curr_key),
            ));
            sync_queue_to_ui(
                &ui,
                &queue_for_curr_fav,
                &library_for_curr_fav,
            );
            let results = search_records_for_curr_fav.lock().unwrap().clone();
            ui.set_search_results(records_model(
                results,
                &library_for_curr_fav,
                Some(&curr_key),
            ));
            ui.set_panel_message(slint::SharedString::from(if is_favorite {
                "Added to favorites"
            } else {
                "Removed from favorites"
            }));
        }
    });
}