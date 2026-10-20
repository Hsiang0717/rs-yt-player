use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{error, info, warn};

use crate::AppWindow;
use crate::extractor::{AudioMetadata, Extractor, shared_http_client};
use crate::library::{LibraryStore, format_duration_secs};
use crate::player::AudioPlayer;
use crate::queue::PlayQueue;
use crate::smtc::SmtcManager;
use crate::ui_models::{
    apply_queue_expanded, load_cached_cover_image, query_video_id, queue_items_model,
    record_from_audio, records_model, requested_video_id,
};

pub mod transport;
pub use transport::{
    trigger_next, trigger_previous, trigger_seek_to_ratio, trigger_seek_to_secs,
    trigger_toggle_play_pause,
};

pub type SmtcHandle = Option<Arc<Mutex<SmtcManager>>>;
pub type LoadingAbortHandle = Arc<Mutex<Option<tokio::task::AbortHandle>>>;

static CONSECUTIVE_FAILURES: AtomicUsize = AtomicUsize::new(0);

/// Play track directly from pre-extracted metadata and cover image
pub async fn play_with_metadata(
    query_str: String,
    meta: AudioMetadata,
    cover_path: Option<std::path::PathBuf>,
    extractor: Arc<Extractor>,
    player: Arc<AudioPlayer>,
    smtc: SmtcHandle,
    ui_weak: slint::Weak<AppWindow>,
    queue: PlayQueue,
    library: LibraryStore,
    last_query: Arc<Mutex<Option<String>>>,
) {
    *last_query.lock().unwrap() = Some(query_str.clone());
    let title = meta.title.clone();
    let record = record_from_audio(&meta);
    queue.set_track_info(
        &query_str,
        title.clone(),
        record.artist.clone(),
        meta.duration,
        meta.thumbnail.clone(),
    );
    let uploader = meta
        .uploader
        .clone()
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let duration_secs = meta.duration.unwrap_or(0.0) as f32;
    let start_offset_secs = if duration_secs > 0.0 {
        meta.start_offset_secs.min(duration_secs)
    } else {
        meta.start_offset_secs
    };
    let duration_str = format_duration_secs(duration_secs as f64);
    let badge = meta
        .audio_codec
        .unwrap_or_else(|| "AAC".to_string())
        .to_uppercase();
    let current_id = meta.id.clone();
    let request_headers = meta.http_headers.clone();
    let loudness_db = meta.loudness_db;

    let stream_url = meta.stream_url.clone();
    let query_for_player = query_str.clone();
    let play_res = tokio::task::spawn_blocking(move || {
        player.set_current_query(Some(query_for_player));
        player.set_loudness_db(loudness_db);
        player.play_stream_from_with_headers(
            &stream_url,
            request_headers,
            start_offset_secs,
            duration_secs,
        )
    })
    .await;

    let playback_error = match &play_res {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(error) => Some(error.to_string()),
    };
    let playback_succeeded = playback_error.is_none();
    if playback_succeeded {
        CONSECUTIVE_FAILURES.store(0, Ordering::Relaxed);
        queue.record_current(query_str.clone());
        library.record_played(record.clone());
        library.update_playback_position(start_offset_secs);
        library.save_queue(queue.to_track_records());
    }

    if let Some(smtc_arc) = &smtc
        && let Ok(mut lock) = smtc_arc.lock()
    {
        lock.set_metadata(
            &title,
            &uploader,
            if duration_secs > 0.0 {
                Some(Duration::from_secs_f32(duration_secs))
            } else {
                None
            },
            cover_path.as_deref(),
        );
        lock.set_playback(
            playback_succeeded,
            Some(Duration::from_secs_f32(start_offset_secs)),
        );
    }

    let queue_for_ui = queue.clone();
    let library_for_ui = library.clone();
    let error_title = title.clone();
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.set_is_loading(false);
        match play_res {
            Ok(Ok(())) => {
                ui.set_track_title(slint::SharedString::from(title));
                ui.set_track_artist(slint::SharedString::from(uploader));
                ui.set_track_badge(slint::SharedString::from(badge));
                ui.set_is_playing(true);
                ui.set_track_status(slint::SharedString::from("PLAYING"));
                ui.set_can_retry(false);
                ui.set_has_active_track(true);
                ui.set_current_track_key(slint::SharedString::from(query_str.clone()));
                ui.set_current_is_favorite(library_for_ui.is_favorite(&record.id));
                ui.set_progress_ratio(if duration_secs > 0.0 {
                    start_offset_secs / duration_secs
                } else {
                    0.0
                });
                ui.set_buffer_ratio(0.0);
                let pos_str = format_duration_secs(start_offset_secs as f64);
                ui.set_time_current(slint::SharedString::from(pos_str.clone()));
                ui.set_time_total(slint::SharedString::from(duration_str.clone()));
                ui.set_time_display(slint::SharedString::from(format!(
                    "{} / {}",
                    pos_str, duration_str
                )));

                if let Some(img) = load_cached_cover_image(&library_for_ui, &record.id)
                    .or_else(|| cover_path.and_then(|p| slint::Image::load_from_path(&p).ok()))
                {
                    ui.set_cover_image(img);
                    ui.set_has_cover(true);
                } else {
                    ui.set_has_cover(false);
                }
                ui.set_favorite_items(records_model(
                    library_for_ui.favorites(),
                    &library_for_ui,
                    Some(&query_str),
                ));
                ui.set_history_items(records_model(
                    library_for_ui.recent(),
                    &library_for_ui,
                    Some(&query_str),
                ));
                ui.set_queue_items(queue_items_model(&queue_for_ui, &library_for_ui));
                ui.set_panel_message(slint::SharedString::default());
            }
            _ => {
                ui.set_is_playing(false);
                ui.set_has_active_track(false);
                ui.set_can_retry(true);
                ui.set_track_status(slint::SharedString::from("ERROR"));
                ui.set_track_title(slint::SharedString::from(error_title));
                ui.set_track_artist(slint::SharedString::from(
                    playback_error.unwrap_or_else(|| "Playback could not start".into()),
                ));
                ui.set_current_track_key(slint::SharedString::default());
                ui.set_current_is_favorite(false);
                ui.set_has_cover(false);
                ui.set_progress_ratio(0.0);
                ui.set_buffer_ratio(0.0);
                ui.set_time_current(slint::SharedString::from("00:00"));
                ui.set_time_total(slint::SharedString::from("00:00"));
                ui.set_time_display(slint::SharedString::from("00:00 / 00:00"));
            }
        }
    });

    if playback_succeeded {
        queue.prefetch_autoplay(Arc::clone(&extractor), library, current_id);
        crate::win32_window::trim_process_working_set();
    }
}

/// Core async track player supporting single songs, playlists, and dead-track auto-skip
pub async fn play_track(
    query_str: String,
    extractor: Arc<Extractor>,
    player: Arc<AudioPlayer>,
    smtc: SmtcHandle,
    ui_weak: slint::Weak<AppWindow>,
    queue: PlayQueue,
    library: LibraryStore,
    last_query: Arc<Mutex<Option<String>>>,
) {
    let mut active_query = query_str;

    loop {
        *last_query.lock().unwrap() = Some(active_query.clone());
        let query_vid = query_video_id(&active_query);
        let library_for_load = library.clone();
        let vid_for_load = query_vid.clone();
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.set_is_loading(true);
            ui.set_can_retry(false);
            ui.set_track_status(slint::SharedString::from("LOADING..."));
            if let Some(img) = load_cached_cover_image(&library_for_load, &vid_for_load) {
                ui.set_cover_image(img);
                ui.set_has_cover(true);
            } else {
                ui.set_has_cover(false);
            }
        });

        let is_playlist_url = active_query.contains("list=");
        let requested_offset_secs = Extractor::start_offset_secs(&active_query).unwrap_or(0.0);
        let mut target_query = active_query.clone();
        let mut queue_expanded = false;

        if is_playlist_url {
            info!("Detected YouTube playlist URL: {}", active_query);
            let mut items = match extractor.extract_playlist_items(&active_query).await {
                Ok(items) if !items.is_empty() => items,
                Ok(_) => {
                    let _ = ui_weak.upgrade_in_event_loop(|ui| {
                        ui.set_is_loading(false);
                        ui.set_can_retry(true);
                        ui.set_track_status(slint::SharedString::from("ERROR"));
                        ui.set_track_artist(slint::SharedString::from(
                            "The playlist has no playable tracks",
                        ));
                    });
                    return;
                }
                Err(error) => {
                    let message = error.to_string();
                    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                        ui.set_is_loading(false);
                        ui.set_can_retry(true);
                        ui.set_track_status(slint::SharedString::from("ERROR"));
                        ui.set_track_artist(slint::SharedString::from(message));
                    });
                    return;
                }
            };
            info!("Extracted {} items", items.len());
            if let Some(video_id) = requested_video_id(&active_query)
                && let Some(selected_index) = items.iter().position(|item| item.id == video_id)
            {
                items.rotate_left(selected_index);
            }
            if let Some(first) = queue.load_playlist(items) {
                target_query = first;
            }
            library.save_queue(queue.to_track_records());
            queue_expanded = true;
        }

        let queue_for_ui = queue.clone();
        let library_for_ui = library.clone();
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.set_queue_items(queue_items_model(&queue_for_ui, &library_for_ui));
            if queue_expanded {
                apply_queue_expanded(&ui, true);
            }
        });

        info!("Resolving stream metadata for query: {}", target_query);
        let extracted = extractor.extract_audio(&target_query).await;

        match extracted {
            Ok(mut meta) => {
                if meta.start_offset_secs == 0.0 && requested_offset_secs > 0.0 {
                    meta.start_offset_secs = requested_offset_secs;
                }
                let mut cover_path = None;
                let local_cache = library.cover_path(&meta.id);
                if local_cache.exists() {
                    cover_path = Some(local_cache);
                } else if let Some(thumb_url) = &meta.thumbnail
                    && let Ok(resp) = shared_http_client().get(thumb_url).send().await
                    && let Ok(bytes) = resp.bytes().await
                    && let Ok(saved_path) = library.store_cover(&meta.id, &bytes)
                {
                    cover_path = Some(saved_path);
                }
                play_with_metadata(
                    target_query,
                    meta,
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
                return;
            }
            Err(err) => {
                error!("Extraction error: {:#}", err);
                let failures = CONSECUTIVE_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
                // Dead-Track Auto-Skip: if queue has remaining tracks and we haven't failed 3 times in a row, skip!
                if failures <= 3
                    && let Some((next_in_queue, _)) = queue.pop_next()
                {
                    warn!(
                        "Dead-track auto-skip (#{}): skipping unavailable track and advancing to {}",
                        failures, next_in_queue
                    );
                    library.save_queue(queue.to_track_records());
                    let q_ui = queue.clone();
                    let l_ui = library.clone();
                    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                        ui.set_queue_items(queue_items_model(&q_ui, &l_ui));
                        ui.set_panel_message(slint::SharedString::from(
                            "Skipped unavailable track; advancing to next...",
                        ));
                    });
                    tokio::time::sleep(Duration::from_millis(750)).await;
                    active_query = next_in_queue;
                    continue;
                }

                CONSECUTIVE_FAILURES.store(0, Ordering::Relaxed);
                let error_message = err.to_string();
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_is_loading(false);
                    ui.set_can_retry(true);
                    ui.set_track_status(slint::SharedString::from("ERROR"));
                    ui.set_track_artist(slint::SharedString::from(error_message));
                });
                return;
            }
        }
    }
}

pub fn start_track_playback(
    query: String,
    extractor: Arc<Extractor>,
    player: Arc<AudioPlayer>,
    smtc: SmtcHandle,
    ui_weak: slint::Weak<AppWindow>,
    queue: PlayQueue,
    library: LibraryStore,
    last_query: Arc<Mutex<Option<String>>>,
    starts_new_cycle: bool,
    loading_abort: LoadingAbortHandle,
) {
    if starts_new_cycle {
        queue.clear_upcoming();
        queue.begin_repeat_cycle(query.clone());
    }
    let join_handle = tokio::spawn(async move {
        play_track(
            query, extractor, player, smtc, ui_weak, queue, library, last_query,
        )
        .await;
    });
    if let Ok(mut guard) = loading_abort.lock()
        && let Some(prev) = guard.replace(join_handle.abort_handle())
    {
        prev.abort();
    }
}