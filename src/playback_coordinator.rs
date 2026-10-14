use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{error, info};

use crate::extractor::{AudioMetadata, Extractor};
use crate::library::{LibraryStore, RepeatMode};
use crate::player::AudioPlayer;
use crate::queue::PlayQueue;
use crate::smtc::SmtcManager;
use crate::ui_models::{
    apply_queue_expanded, queue_items_model, record_from_audio, records_model, requested_video_id,
};
use crate::AppWindow;
use souvlaki::MediaControlEvent;

pub type SmtcHandle = Option<Arc<Mutex<(SmtcManager, std::sync::mpsc::Receiver<MediaControlEvent>)>>>;
pub type LoadingAbortHandle = Arc<Mutex<Option<tokio::task::AbortHandle>>>;

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
    let m = (duration_secs as u64) / 60;
    let s = (duration_secs as u64) % 60;
    let duration_str = format!("{:02}:{:02}", m, s);
    let badge = meta
        .audio_codec
        .unwrap_or_else(|| "AAC".to_string())
        .to_uppercase();
    let current_id = meta.id.clone();
    let request_headers = meta.http_headers.clone();

    // Start streaming the selected AAC format through Symphonia.
    let stream_url = meta.stream_url.clone();
    let query_for_player = query_str.clone();
    let play_res = tokio::task::spawn_blocking(move || {
        player.set_current_query(Some(query_for_player));
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
        queue.record_current(query_str.clone());
        library.record_played(record.clone());
    }

    // Update Windows SMTC if available
    if let Some(smtc_arc) = &smtc
        && let Ok(mut lock) = smtc_arc.lock() {
            lock.0.set_metadata(
                &title,
                &uploader,
                if duration_secs > 0.0 {
                    Some(Duration::from_secs_f32(duration_secs))
                } else {
                    None
                },
            );
            lock.0.set_playback(playback_succeeded);
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
                ui.set_current_track_key(slint::SharedString::from(query_str));
                ui.set_current_is_favorite(library_for_ui.is_favorite(&record.id));
                ui.set_progress_ratio(if duration_secs > 0.0 {
                    start_offset_secs / duration_secs
                } else {
                    0.0
                });
                ui.set_buffer_ratio(0.0);
                let position_m = (start_offset_secs as u64) / 60;
                let position_s = (start_offset_secs as u64) % 60;
                ui.set_time_display(slint::SharedString::from(format!(
                    "{:02}:{:02} / {}",
                    position_m, position_s, duration_str
                )));

                if let Some(path) = cover_path {
                    if let Ok(img) = slint::Image::load_from_path(&path) {
                        ui.set_cover_image(img);
                        ui.set_has_cover(true);
                    } else {
                        ui.set_has_cover(false);
                    }
                } else {
                    ui.set_has_cover(false);
                }
                ui.set_favorite_items(records_model(library_for_ui.favorites(), &library_for_ui));
                ui.set_history_items(records_model(library_for_ui.recent(), &library_for_ui));
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
                ui.set_time_display(slint::SharedString::from("00:00 / 00:00"));
            }
        }
    });

    // Autoplay prefetch: If queue is empty, prefetch YouTube related songs for seamless continuous playback
    if playback_succeeded {
        queue.prefetch_autoplay(Arc::clone(&extractor), current_id);
    }
}

/// Core async track player supporting single songs, playlists, and autoplay queues
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
    *last_query.lock().unwrap() = Some(query_str.clone());
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.set_is_loading(true);
        ui.set_can_retry(false);
        ui.set_track_status(slint::SharedString::from("LOADING..."));
    });

    // Check if query is a playlist URL
    let is_playlist_url = query_str.contains("list=");
    let requested_offset_secs = Extractor::start_offset_secs(&query_str).unwrap_or(0.0);
    let mut target_query = query_str.clone();
    let mut queue_expanded = false;

    if is_playlist_url {
        info!("Detected YouTube playlist URL: {}", query_str);
        let mut items = match extractor.extract_playlist_items(&query_str).await {
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
        if let Some(video_id) = requested_video_id(&query_str)
            && let Some(selected_index) = items.iter().position(|item| item.id == video_id) {
                items.rotate_left(selected_index);
            }
        if let Some(first) = queue.load_playlist(items) {
            target_query = first;
        }
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
            let temp_cover_path = std::env::temp_dir().join(format!("rs_yt_cover_{}.png", meta.id));
            if let Some(thumb_url) = &meta.thumbnail {
                let client = reqwest::Client::new();
                if let Ok(resp) = client.get(thumb_url).send().await
                    && let Ok(bytes) = resp.bytes().await
                        && let Ok(dynamic_img) = image::load_from_memory(&bytes)
                            && dynamic_img
                                .save_with_format(&temp_cover_path, image::ImageFormat::Png)
                                .is_ok()
                            {
                                cover_path = Some(temp_cover_path);
                            }
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
        }
        Err(err) => {
            error!("Extraction error: {:#}", err);
            let error_message = err.to_string();
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.set_is_loading(false);
                ui.set_can_retry(true);
                ui.set_track_status(slint::SharedString::from("ERROR"));
                ui.set_track_artist(slint::SharedString::from(error_message));
            });
        }
    }
}

pub fn trigger_next(
    extractor: Arc<Extractor>,
    player: Arc<AudioPlayer>,
    smtc: SmtcHandle,
    ui_weak: slint::Weak<AppWindow>,
    queue: PlayQueue,
    library: LibraryStore,
    is_advancing: Arc<Mutex<bool>>,
    natural_end: bool,
    last_query: Arc<Mutex<Option<String>>>,
    loading_abort: LoadingAbortHandle,
) {
    let mut advancing = match is_advancing.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if *advancing {
        return;
    }
    *advancing = true;

    if natural_end && queue.repeat_mode() == RepeatMode::One
        && let Some(current) = queue.current_query() {
            let adv_lock = Arc::clone(&is_advancing);
            let abort_handle = loading_abort.clone();
            let join_handle = tokio::spawn(async move {
                play_track(
                    current, extractor, player, smtc, ui_weak, queue, library, last_query,
                )
                .await;
                if let Ok(mut guard) = adv_lock.lock() {
                    *guard = false;
                }
            });
            if let Ok(mut guard) = abort_handle.lock()
                && let Some(prev) = guard.replace(join_handle.abort_handle()) {
                    prev.abort();
                }
            return;
        }

    if let Some((next_query, maybe_preloaded)) = queue.pop_next() {
        info!("Advancing to next track in queue: {}", next_query);
        let adv_lock = Arc::clone(&is_advancing);
        let abort_handle = loading_abort.clone();
        let join_handle = tokio::spawn(async move {
            if let Some(preloaded) = maybe_preloaded {
                info!(
                    "Seamless transition using preloaded track: {}",
                    preloaded.meta.title
                );
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
                    next_query, extractor, player, smtc, ui_weak, queue, library, last_query,
                )
                .await;
            }
            if let Ok(mut guard) = adv_lock.lock() {
                *guard = false;
            }
        });
        if let Ok(mut guard) = abort_handle.lock()
            && let Some(prev) = guard.replace(join_handle.abort_handle()) {
                prev.abort();
            }
    } else {
        *advancing = false;
        info!("Next track requested, but queue is empty.");
    }
}

pub fn trigger_previous(
    extractor: Arc<Extractor>,
    player: Arc<AudioPlayer>,
    smtc: SmtcHandle,
    ui_weak: slint::Weak<AppWindow>,
    queue: PlayQueue,
    library: LibraryStore,
    is_advancing: Arc<Mutex<bool>>,
    last_query: Arc<Mutex<Option<String>>>,
    loading_abort: LoadingAbortHandle,
) {
    let mut advancing = match is_advancing.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if *advancing {
        return;
    }
    *advancing = true;
    if let Some(prev_query) = queue.pop_previous() {
        info!("Switching to previous track in history: {}", prev_query);
        let adv_lock = Arc::clone(&is_advancing);
        let abort_handle = loading_abort.clone();
        let join_handle = tokio::spawn(async move {
            play_track(
                prev_query, extractor, player, smtc, ui_weak, queue, library, last_query,
            )
            .await;
            if let Ok(mut guard) = adv_lock.lock() {
                *guard = false;
            }
        });
        if let Ok(mut guard) = abort_handle.lock()
            && let Some(prev) = guard.replace(join_handle.abort_handle()) {
                prev.abort();
            }
    } else {
        *advancing = false;
        info!("No previous track in history; seeking to beginning.");
        let _ = player.seek_to(0.0);
        if let Some(smtc_arc) = &smtc
            && let Ok(mut lock) = smtc_arc.lock() {
                lock.0.set_playback(true);
            }
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_progress_ratio(0.0);
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
        && let Some(prev) = guard.replace(join_handle.abort_handle()) {
            prev.abort();
        }
}

pub fn trigger_toggle_play_pause(
    player: Arc<AudioPlayer>,
    extractor: Arc<Extractor>,
    smtc: SmtcHandle,
    ui_weak: slint::Weak<AppWindow>,
    last_query: Arc<Mutex<Option<String>>>,
) {
    if !player.has_track() {
        return;
    }

    let is_paused = player.is_paused();
    if !is_paused {
        // Pausing is instantaneous and synchronous
        player.pause();
        if let Some(smtc_arc) = &smtc
            && let Ok(mut lock) = smtc_arc.lock() {
                lock.0.set_playback(false);
            }
        let _ = ui_weak.upgrade_in_event_loop(|ui| {
            ui.set_is_playing(false);
            ui.set_track_status(slint::SharedString::from("PAUSED"));
        });
    } else {
        // Resuming: check if direct unpause is safe or if background refresh is required
        let needs_refresh = player.is_stream_expired_or_stale() || crate::win32_window::take_system_resumed();
        if !needs_refresh {
            player.resume_direct();
            if let Some(smtc_arc) = &smtc
                && let Ok(mut lock) = smtc_arc.lock() {
                    lock.0.set_playback(true);
                }
            let _ = ui_weak.upgrade_in_event_loop(|ui| {
                ui.set_is_playing(true);
                ui.set_track_status(slint::SharedString::from("PLAYING"));
            });
        } else {
            // Asynchronously refresh the stream in background without blocking the Slint UI thread
            let _ = ui_weak.upgrade_in_event_loop(|ui| {
                ui.set_is_loading(true);
                ui.set_is_playing(true);
                ui.set_track_status(slint::SharedString::from("RESUMING..."));
            });
            let player_clone = Arc::clone(&player);
            let extractor_clone = Arc::clone(&extractor);
            let smtc_clone = smtc.clone();
            let ui_weak_clone = ui_weak.clone();
            let last_query_clone = Arc::clone(&last_query);

            tokio::spawn(async move {
                let current_pos = player_clone.current_position();
                let total_dur = player_clone.total_duration();
                let query_opt = player_clone
                    .current_query()
                    .or_else(|| last_query_clone.lock().ok().and_then(|g| g.clone()));

                let refresh_result = if let Some(query) = query_opt {
                    info!("Refreshing stream URL in background on resume for: {}", query);
                    let yt_dlp = extractor_clone.yt_dlp_path().to_path_buf();
                    let mut extract_res = Err(anyhow::anyhow!("Initial"));
                    for attempt in 0..2 {
                        let query_clone = query.clone();
                        let yt_dlp_clone = yt_dlp.clone();
                        extract_res = tokio::task::spawn_blocking(move || {
                            crate::extractor::extract_stream_url_sync(&yt_dlp_clone, &query_clone)
                        })
                        .await
                        .unwrap_or_else(|e| Err(anyhow::anyhow!("Task failed: {}", e)));

                        if extract_res.is_ok() {
                            break;
                        }
                        if attempt == 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                        }
                    }

                    match extract_res {
                        Ok((new_url, new_headers)) => {
                            let p = Arc::clone(&player_clone);
                            tokio::task::spawn_blocking(move || {
                                let _ = p.recreate_sink();
                                p.seek_stream_to(&new_url, new_headers, None, current_pos, total_dur)
                            })
                            .await
                            .unwrap_or_else(|e| Err(anyhow::anyhow!("Seek task failed: {}", e)))
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    Err(anyhow::anyhow!("No active query to refresh"))
                };

                match refresh_result {
                    Ok(()) => {
                        info!("Stream resumed successfully at {:.1}s.", current_pos);
                        if let Some(smtc_arc) = &smtc_clone
                            && let Ok(mut lock) = smtc_arc.lock() {
                                lock.0.set_playback(true);
                            }
                        let _ = ui_weak_clone.upgrade_in_event_loop(|ui| {
                            ui.set_is_loading(false);
                            ui.set_is_playing(true);
                            ui.set_track_status(slint::SharedString::from("PLAYING"));
                        });
                    }
                    Err(err) => {
                        error!("Failed to refresh stream on resume: {:#}", err);
                        let _ = ui_weak_clone.upgrade_in_event_loop(|ui| {
                            ui.set_is_loading(false);
                            ui.set_is_playing(false);
                            ui.set_can_retry(true);
                            ui.set_track_status(slint::SharedString::from("ERROR"));
                            ui.set_track_artist(slint::SharedString::from(
                                "Network reconnect failed. Click play to retry.",
                            ));
                        });
                    }
                }
            });
        }
    }
}
