use std::sync::{Arc, Mutex};

use crate::library::TrackRecord;
use crate::ui_models::{query_video_id, records_model};
use crate::AppWindow;

use super::PlaybackContext;

pub fn bind_library_controls(
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
