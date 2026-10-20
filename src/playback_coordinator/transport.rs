use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{error, info};

use crate::AppWindow;
use crate::extractor::Extractor;
use crate::library::{LibraryStore, RepeatMode, format_duration_secs};
use crate::player::AudioPlayer;
use crate::queue::PlayQueue;
use crate::ui_callbacks::PendingSeekState;
use crate::ui_models::queue_items_model;

use super::{LoadingAbortHandle, SmtcHandle, play_track, play_with_metadata};

static LAST_PREV_CLICK: Mutex<Option<Instant>> = Mutex::new(None);

struct AdvancingGuard(Arc<Mutex<bool>>);

impl Drop for AdvancingGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.0.lock() {
            *guard = false;
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

    if natural_end
        && queue.repeat_mode() == RepeatMode::One
        && let Some(current) = queue.current_query()
    {
        let adv_lock = Arc::clone(&is_advancing);
        let abort_handle = loading_abort.clone();
        let join_handle = tokio::spawn(async move {
            let _guard = AdvancingGuard(adv_lock);
            play_track(
                current, extractor, player, smtc, ui_weak, queue, library, last_query,
            )
            .await;
        });
        if let Ok(mut guard) = abort_handle.lock()
            && let Some(prev) = guard.replace(join_handle.abort_handle())
        {
            prev.abort();
        }
        return;
    }

    if let Some((next_query, maybe_preloaded)) = queue.pop_next() {
        info!("Advancing to next track in queue: {}", next_query);
        library.save_queue(queue.to_track_records());
        let q_ui = queue.clone();
        let l_ui = library.clone();
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.set_queue_items(queue_items_model(&q_ui, &l_ui));
        });
        let adv_lock = Arc::clone(&is_advancing);
        let abort_handle = loading_abort.clone();
        let join_handle = tokio::spawn(async move {
            let _guard = AdvancingGuard(adv_lock);
            if let Some(preloaded) = maybe_preloaded {
                info!(
                    "Seamless transition using preloaded track: {}",
                    preloaded.meta.title
                );
                let cache_path = library.cover_path(&preloaded.meta.id);
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
        });
        if let Ok(mut guard) = abort_handle.lock()
            && let Some(prev) = guard.replace(join_handle.abort_handle())
        {
            prev.abort();
        }
    } else {
        *advancing = false;
        info!("Next track requested, but queue is empty.");
        if natural_end {
            player.stop();
            let _ = ui_weak.upgrade_in_event_loop(|ui| {
                ui.set_is_playing(false);
                ui.set_track_status(slint::SharedString::from("STOPPED"));
            });
        }
    }
}

/// Implements the industry-standard 3-Second Rule:
/// - If playback is > 3.0s into the track (and Previous wasn't double-pressed within 2.5s),
///   rewind to 00:00 of the current track.
/// - Otherwise, jump to the previous track in history (or rewind to 00:00 if history is empty).
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
    let now = Instant::now();
    let double_pressed = {
        let mut last = LAST_PREV_CLICK.lock().unwrap();
        let is_double = last.is_some_and(|t| now.duration_since(t) < Duration::from_millis(2500));
        *last = Some(now);
        is_double
    };

    let current_pos = player.current_position();
    if current_pos > 3.0 && !double_pressed {
        info!(
            "3-Second Rule: playback at {:.1}s (>3s); rewinding current track to 00:00.",
            current_pos
        );
        rewind_to_beginning(&player, &smtc, &ui_weak, "Rewound to start of track");
        return;
    }

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
        library.save_queue(queue.to_track_records());
        let q_ui = queue.clone();
        let l_ui = library.clone();
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.set_queue_items(queue_items_model(&q_ui, &l_ui));
        });
        let adv_lock = Arc::clone(&is_advancing);
        let abort_handle = loading_abort.clone();
        let join_handle = tokio::spawn(async move {
            let _guard = AdvancingGuard(adv_lock);
            play_track(
                prev_query, extractor, player, smtc, ui_weak, queue, library, last_query,
            )
            .await;
        });
        if let Ok(mut guard) = abort_handle.lock()
            && let Some(prev) = guard.replace(join_handle.abort_handle())
        {
            prev.abort();
        }
    } else {
        *advancing = false;
        info!("No previous track in history; seeking to beginning.");
        rewind_to_beginning(
            &player,
            &smtc,
            &ui_weak,
            "Rewound to start (no previous track)",
        );
    }
}

fn rewind_to_beginning(
    player: &Arc<AudioPlayer>,
    smtc: &SmtcHandle,
    ui_weak: &slint::Weak<AppWindow>,
    message: &'static str,
) {
    let p = Arc::clone(player);
    tokio::task::spawn_blocking(move || {
        let _ = p.seek_to(0.0);
    });
    if let Some(smtc_arc) = smtc
        && let Ok(mut lock) = smtc_arc.lock()
    {
        lock.set_playback(true, Some(Duration::ZERO));
    }
    if let Some(ui) = ui_weak.upgrade() {
        let total_str = format_duration_secs(player.total_duration() as f64);
        ui.set_progress_ratio(0.0);
        ui.set_time_current(slint::SharedString::from("00:00"));
        ui.set_time_display(slint::SharedString::from(format!("00:00 / {}", total_str)));
        ui.set_panel_message(slint::SharedString::from(message));
    }
}

pub fn trigger_toggle_play_pause(
    player: Arc<AudioPlayer>,
    extractor: Arc<Extractor>,
    smtc: SmtcHandle,
    ui_weak: slint::Weak<AppWindow>,
    last_query: Arc<Mutex<Option<String>>>,
    queue: PlayQueue,
    library: LibraryStore,
    loading_abort: LoadingAbortHandle,
) {
    if !player.has_track() {
        // Check if we have a restored session track waiting to be played
        let restored_query = last_query.lock().unwrap().clone();
        if let Some(query) = restored_query {
            let (_, last_pos, _) = library.last_session();
            let query_with_offset = if last_pos > 2.0 && !query.contains("&t=") {
                format!("{}&t={}s", query, last_pos as u64)
            } else {
                query
            };
            super::start_track_playback(
                query_with_offset,
                extractor,
                player,
                smtc,
                ui_weak,
                queue,
                library,
                last_query,
                false,
                loading_abort,
            );
        }
        return;
    }

    let is_paused = player.is_paused();
    if !is_paused && player.is_active() {
        let cur_pos = player.current_position();
        player.pause();
        library.update_playback_position(cur_pos);
        if let Some(smtc_arc) = &smtc
            && let Ok(mut lock) = smtc_arc.lock()
        {
            lock.set_playback(false, Some(Duration::from_secs_f32(cur_pos)));
        }
        let _ = ui_weak.upgrade_in_event_loop(|ui| {
            ui.set_is_playing(false);
            ui.set_track_status(slint::SharedString::from("PAUSED"));
        });
    } else {
        let needs_refresh =
            player.is_stream_expired_or_stale() || crate::win32_window::take_system_resumed();
        if !needs_refresh {
            let cur_pos = player.current_position();
            player.resume_direct();
            if let Some(smtc_arc) = &smtc
                && let Ok(mut lock) = smtc_arc.lock()
            {
                lock.set_playback(true, Some(Duration::from_secs_f32(cur_pos)));
            }
            let _ = ui_weak.upgrade_in_event_loop(|ui| {
                ui.set_is_playing(true);
                ui.set_track_status(slint::SharedString::from("PLAYING"));
            });
        } else {
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
                    info!(
                        "Refreshing stream URL in background on resume for: {}",
                        query
                    );
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
                            tokio::time::sleep(Duration::from_millis(800)).await;
                        }
                    }

                    match extract_res {
                        Ok((new_url, new_headers)) => {
                            let p = Arc::clone(&player_clone);
                            tokio::task::spawn_blocking(move || {
                                let _ = p.recreate_sink();
                                p.seek_stream_to(
                                    &new_url,
                                    new_headers,
                                    None,
                                    current_pos,
                                    total_dur,
                                )
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
                            && let Ok(mut lock) = smtc_arc.lock()
                        {
                            lock.set_playback(true, Some(Duration::from_secs_f32(current_pos)));
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

pub fn trigger_seek_to_ratio(
    ratio: f32,
    player: Arc<AudioPlayer>,
    smtc: SmtcHandle,
    ui_weak: slint::Weak<AppWindow>,
    pending_seek: Arc<Mutex<Option<PendingSeekState>>>,
    seek_seq: Arc<AtomicU64>,
    seek_lock: Arc<tokio::sync::Mutex<()>>,
) {
    let ratio = ratio.clamp(0.0, 1.0);
    let current_seq = seek_seq.fetch_add(1, Ordering::SeqCst) + 1;
    let total_dur = player.total_duration();
    if total_dur <= 0.0 {
        return;
    }
    let target_secs = ratio * total_dur;
    info!(
        "Executing seek #{} to {:.1}s ({:.1}%)",
        current_seq,
        target_secs,
        ratio * 100.0
    );

    if let Some(ui) = ui_weak.upgrade() {
        ui.set_progress_ratio(ratio);
        let cur_str = format_duration_secs(target_secs as f64);
        let tot_str = format_duration_secs(total_dur as f64);
        ui.set_time_current(slint::SharedString::from(cur_str.clone()));
        ui.set_time_total(slint::SharedString::from(tot_str.clone()));
        ui.set_time_display(slint::SharedString::from(format!("{} / {}", cur_str, tot_str)));
        ui.set_track_status(slint::SharedString::from("BUFFERING"));
    }

    *pending_seek.lock().unwrap() = Some(PendingSeekState {
        id: current_seq,
        target_ratio: ratio,
        target_secs,
        requested_at: Instant::now(),
    });

    let player_clone = Arc::clone(&player);
    let ui_weak_clone = ui_weak.clone();
    let smtc_clone = smtc.clone();
    let pending_seek_clone = Arc::clone(&pending_seek);
    let seek_seq_clone = Arc::clone(&seek_seq);
    let seek_lock_clone = Arc::clone(&seek_lock);

    tokio::spawn(async move {
        let _guard = seek_lock_clone.lock().await;
        if seek_seq_clone.load(Ordering::SeqCst) != current_seq {
            return;
        }

        let result = tokio::task::spawn_blocking(move || player_clone.seek_to(target_secs)).await;
        if seek_seq_clone.load(Ordering::SeqCst) == current_seq {
            if matches!(result, Ok(Ok(()))) {
                if let Some(ui) = ui_weak_clone.upgrade() {
                    ui.set_is_playing(true);
                    ui.set_track_status(slint::SharedString::from("PLAYING"));
                }
                if let Some(smtc_arc) = &smtc_clone
                    && let Ok(mut lock) = smtc_arc.lock()
                {
                    lock.set_playback(true, Some(Duration::from_secs_f32(target_secs)));
                }
            }
            let mut lock = pending_seek_clone.lock().unwrap();
            if let Some(p) = lock.as_ref()
                && p.id == current_seq
            {
                *lock = None;
            }
        }
    });
}

pub fn trigger_seek_to_secs(
    target_secs: f32,
    player: Arc<AudioPlayer>,
    smtc: SmtcHandle,
    ui_weak: slint::Weak<AppWindow>,
    pending_seek: Arc<Mutex<Option<PendingSeekState>>>,
    seek_seq: Arc<AtomicU64>,
    seek_lock: Arc<tokio::sync::Mutex<()>>,
) {
    let total_dur = player.total_duration();
    if total_dur <= 0.0 {
        return;
    }
    let target_secs = target_secs.clamp(0.0, total_dur);
    let ratio = target_secs / total_dur;
    trigger_seek_to_ratio(
        ratio,
        player,
        smtc,
        ui_weak,
        pending_seek,
        seek_seq,
        seek_lock,
    );
}