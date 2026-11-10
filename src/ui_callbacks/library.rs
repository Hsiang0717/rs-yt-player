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

    // Batch Favorite All: saves all items in the playlist queue to favorites
    let ctx_for_fav_all = ctx.clone();
    let search_for_fav_all = Arc::clone(&search_records);
    app.on_favorite_all(move || {
        let tracks = ctx_for_fav_all.queue.to_track_records();
        if tracks.is_empty() {
            if let Some(ui) = ctx_for_fav_all.ui_weak.upgrade() {
                ui.set_panel_message(slint::SharedString::from("Playlist is empty"));
            }
            return;
        }

        // Fill any missing metadata from library or search records
        let resolved_tracks: Vec<TrackRecord> = tracks
            .into_iter()
            .map(|mut track| {
                if track.title.is_empty() {
                    if let Some(known) = ctx_for_fav_all.library.find_track(&track.id) {
                        track.title = known.title;
                        track.artist = known.artist;
                        track.duration_secs = known.duration_secs;
                        track.thumbnail_url = known.thumbnail_url;
                    } else if let Some(known) = search_for_fav_all
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|r| r.id == track.id)
                    {
                        track.title = known.title.clone();
                        track.artist = known.artist.clone();
                        track.duration_secs = known.duration_secs;
                        track.thumbnail_url = known.thumbnail_url.clone();
                    }
                }
                track
            })
            .collect();

        let added = ctx_for_fav_all.library.add_favorites(resolved_tracks);
        let added_count = added.len();

        if let Some(ui) = ctx_for_fav_all.ui_weak.upgrade() {
            let curr_key = ui.get_current_track_key();
            ui.set_favorite_items(records_model(
                ctx_for_fav_all.library.favorites(),
                &ctx_for_fav_all.library,
                Some(&curr_key),
            ));
            let results = search_for_fav_all.lock().unwrap().clone();
            ui.set_search_results(records_model(
                results,
                &ctx_for_fav_all.library,
                Some(&curr_key),
            ));
            sync_queue_to_ui(
                &ui,
                &ctx_for_fav_all.queue,
                &ctx_for_fav_all.library,
            );
            let current_id = query_video_id(&curr_key);
            ui.set_current_is_favorite(ctx_for_fav_all.library.is_favorite(&current_id));

            if added_count == 0 {
                ui.set_panel_message(slint::SharedString::from(
                    "All playlist tracks already in favorites",
                ));
            } else {
                ui.set_panel_message(slint::SharedString::from(format!(
                    "Added {} tracks to favorites",
                    added_count
                )));
            }
        }

        // Sequential background caching for newly favorited tracks
        if !added.is_empty() {
            let extractor_bg = Arc::clone(&ctx_for_fav_all.extractor);
            let library_bg = ctx_for_fav_all.library.clone();
            let cache_dir = library_bg.audio_cache_dir();
            let ui_weak_refresh = ctx_for_fav_all.ui_weak.clone();
            let queue_refresh = ctx_for_fav_all.queue.clone();
            tokio::spawn(async move {
                let mut any_downloaded = false;
                for track in added {
                    if library_bg.find_cached_audio(&track.id).is_none() {
                        if extractor_bg
                            .download_audio_track(&track.id, &cache_dir)
                            .await
                            .is_ok()
                        {
                            any_downloaded = true;
                        }
                    }
                }
                if any_downloaded {
                    let _ = ui_weak_refresh.upgrade_in_event_loop(move |ui| {
                        sync_queue_to_ui(&ui, &queue_refresh, &library_bg);
                    });
                }
            });
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