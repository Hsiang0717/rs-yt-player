use std::sync::{Arc, Mutex};

use crate::AppWindow;
use crate::library::TrackRecord;
use crate::ui_models::{query_video_id, queue_items_model, records_model};

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
        ctx_for_play_fav.start_track(key.to_string(), false);
    });

    // Batch Play All: replaces upcoming queue with all items in current tab and plays the first
    let ctx_for_play_all = ctx.clone();
    let search_for_play_all = Arc::clone(&search_records);
    app.on_play_all(move |tab| {
        let records = resolve_tab_records(tab.as_str(), &search_for_play_all, &ctx_for_play_all.library);
        let mut iter = records.into_iter();
        let Some(first) = iter.next() else {
            return;
        };
        let first_url = first.video_url();
        ctx_for_play_all.queue.clear_upcoming();
        ctx_for_play_all.queue.set_track_info(
            &first_url,
            first.title,
            first.artist,
            first.duration_secs,
            first.thumbnail_url,
        );
        ctx_for_play_all.queue.begin_repeat_cycle(first_url.clone());

        let mut count = 1usize;
        for record in iter {
            let url = record.video_url();
            ctx_for_play_all.queue.set_track_info(
                &url,
                record.title,
                record.artist,
                record.duration_secs,
                record.thumbnail_url,
            );
            ctx_for_play_all.queue.push_back(url);
            count += 1;
        }
        ctx_for_play_all
            .library
            .save_queue(ctx_for_play_all.queue.to_track_records());
        if let Some(ui) = ctx_for_play_all.ui_weak.upgrade() {
            ui.set_queue_items(queue_items_model(
                &ctx_for_play_all.queue,
                &ctx_for_play_all.library,
            ));
            ui.set_panel_message(slint::SharedString::from(format!(
                "Playing all ({} tracks)",
                count
            )));
        }
        ctx_for_play_all.start_track(first_url, false);
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
            ui.set_queue_items(queue_items_model(
                &ctx_for_enqueue_all.queue,
                &ctx_for_enqueue_all.library,
            ));
            ui.set_panel_message(slint::SharedString::from(format!(
                "Added {} tracks to queue",
                count
            )));
        }
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
                ui.set_history_items(records_model(
                    library_for_remove.recent(),
                    &library_for_remove,
                    Some(&curr_key),
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
                let curr_key = ui.get_current_track_key();
                ui.set_history_items(records_model(
                    library_for_hist_fav.recent(),
                    &library_for_hist_fav,
                    Some(&curr_key),
                ));
                ui.set_favorite_items(records_model(
                    library_for_hist_fav.favorites(),
                    &library_for_hist_fav,
                    Some(&curr_key),
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
        let record = library_for_curr_fav.find_track(&id);
        if let Some(record) = record {
            let is_favorite = library_for_curr_fav.toggle_favorite(record);
            ui.set_current_is_favorite(is_favorite);
            let curr_key = ui.get_current_track_key();
            ui.set_favorite_items(records_model(
                library_for_curr_fav.favorites(),
                &library_for_curr_fav,
                Some(&curr_key),
            ));
            ui.set_history_items(records_model(
                library_for_curr_fav.recent(),
                &library_for_curr_fav,
                Some(&curr_key),
            ));
        }
    });
}