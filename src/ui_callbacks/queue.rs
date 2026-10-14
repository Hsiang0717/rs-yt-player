use std::sync::{Arc, Mutex};

use crate::playback_coordinator::{play_track, play_with_metadata};
use crate::ui_models::queue_items_model;
use crate::AppWindow;

use super::PlaybackContext;

pub fn bind_queue_controls(
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
