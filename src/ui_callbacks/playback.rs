use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::info;

use crate::playback_coordinator::SmtcHandle;
use crate::player::AudioPlayer;
use crate::AppWindow;
use slint::ComponentHandle;

use super::{PendingSeekState, PlaybackContext};

pub fn bind_playback_controls(
    app: &AppWindow,
    ctx: PlaybackContext,
    is_auto_advancing: Arc<Mutex<bool>>,
) {
    let ctx_for_toggle = ctx.clone();
    app.on_toggle_play_pause(move || {
        ctx_for_toggle.toggle_play_pause();
    });

    let ctx_for_prev = ctx.clone();
    let advance_for_prev = Arc::clone(&is_auto_advancing);
    app.on_play_previous(move || {
        ctx_for_prev.trigger_previous(&advance_for_prev);
    });

    let ctx_for_next = ctx.clone();
    let advance_for_next = Arc::clone(&is_auto_advancing);
    app.on_play_next(move || {
        ctx_for_next.trigger_next(&advance_for_next);
    });

    let abort_for_cancel = ctx.loading_abort.clone();
    let ui_weak_cancel = ctx.ui_weak.clone();
    let advance_for_cancel = is_auto_advancing;
    app.on_cancel_loading(move || {
        if let Ok(mut guard) = abort_for_cancel.lock()
            && let Some(handle) = guard.take() {
                handle.abort();
                info!("Active loading task was aborted by user.");
            }
        if let Ok(mut advancing) = advance_for_cancel.lock() {
            *advancing = false;
        }
        if let Some(ui) = ui_weak_cancel.upgrade() {
            ui.set_is_loading(false);
            if ui.get_track_status() == "LOADING..." {
                ui.set_track_status(slint::SharedString::from(
                    if ui.get_is_playing() { "PLAYING" } else { "READY" }
                ));
            }
        }
    });

    let player_for_stop = Arc::clone(&ctx.player);
    let smtc_for_stop = ctx.smtc;
    let ui_weak_stop = ctx.ui_weak;
    let abort_for_stop = ctx.loading_abort;
    app.on_stop(move || {
        if let Ok(mut guard) = abort_for_stop.lock()
            && let Some(handle) = guard.take() {
                handle.abort();
            }
        player_for_stop.stop();
        if let Some(smtc_arc) = &smtc_for_stop
            && let Ok(mut lock) = smtc_arc.lock() {
                lock.0.set_playback(false);
            }
        if let Some(ui) = ui_weak_stop.upgrade() {
            ui.set_is_playing(false);
            ui.set_can_retry(false);
            ui.set_track_status(slint::SharedString::from("STOPPED"));
            ui.set_progress_ratio(0.0);
            ui.set_buffer_ratio(0.0);
            ui.set_time_current(slint::SharedString::from("00:00"));
            ui.set_time_total(slint::SharedString::from("00:00"));
            ui.set_time_display(slint::SharedString::from("00:00 / 00:00"));
        }
    });
}

pub fn bind_seek_and_volume_controls(
    app: &AppWindow,
    player: Arc<AudioPlayer>,
    smtc: SmtcHandle,
    pending_seek: Arc<Mutex<Option<PendingSeekState>>>,
) {
    let player_for_scrub = Arc::clone(&player);
    let ui_weak_scrub = app.as_weak();
    app.on_on_user_scrub(move |ratio| {
        let total_dur = player_for_scrub.total_duration();
        let target_secs = ratio * total_dur;
        let cur_m = (target_secs as u64) / 60;
        let cur_s = (target_secs as u64) % 60;
        let tot_m = (total_dur as u64) / 60;
        let tot_s = (total_dur as u64) % 60;
        if let Some(ui) = ui_weak_scrub.upgrade() {
            ui.set_time_current(slint::SharedString::from(format!("{:02}:{:02}", cur_m, cur_s)));
            ui.set_time_total(slint::SharedString::from(format!("{:02}:{:02}", tot_m, tot_s)));
            ui.set_time_display(slint::SharedString::from(format!(
                "{:02}:{:02} / {:02}:{:02}",
                cur_m, cur_s, tot_m, tot_s
            )));
        }
    });

    let seek_seq = Arc::new(AtomicU64::new(0));
    let seek_lock = Arc::new(tokio::sync::Mutex::new(()));
    let player_for_seek = Arc::clone(&player);
    let ui_weak_seek = app.as_weak();
    let smtc_for_seek = smtc;
    let pending_seek_for_seek = pending_seek;
    let seek_seq_for_seek = Arc::clone(&seek_seq);
    let seek_lock_for_seek = Arc::clone(&seek_lock);
    app.on_on_user_seek(move |ratio| {
        let current_seq = seek_seq_for_seek.fetch_add(1, Ordering::SeqCst) + 1;
        let total_dur = player_for_seek.total_duration();
        let target_secs = ratio * total_dur;
        info!(
            "Executing user seek #{} to {:.1}s ({:.1}%)",
            current_seq,
            target_secs,
            ratio * 100.0
        );

        if let Some(ui) = ui_weak_seek.upgrade() {
            ui.set_progress_ratio(ratio);
            let cur_m = (target_secs as u64) / 60;
            let cur_s = (target_secs as u64) % 60;
            let tot_m = (total_dur as u64) / 60;
            let tot_s = (total_dur as u64) % 60;
            ui.set_time_current(slint::SharedString::from(format!("{:02}:{:02}", cur_m, cur_s)));
            ui.set_time_total(slint::SharedString::from(format!("{:02}:{:02}", tot_m, tot_s)));
            ui.set_time_display(slint::SharedString::from(format!(
                "{:02}:{:02} / {:02}:{:02}",
                cur_m, cur_s, tot_m, tot_s
            )));
            ui.set_track_status(slint::SharedString::from("BUFFERING"));
        }

        *pending_seek_for_seek.lock().unwrap() = Some(PendingSeekState {
            id: current_seq,
            target_ratio: ratio,
            target_secs,
            requested_at: Instant::now(),
        });

        let player = Arc::clone(&player_for_seek);
        let ui_weak = ui_weak_seek.clone();
        let smtc = smtc_for_seek.clone();
        let pending_seek = Arc::clone(&pending_seek_for_seek);
        let seek_seq = Arc::clone(&seek_seq_for_seek);
        let seek_lock = Arc::clone(&seek_lock_for_seek);

        tokio::spawn(async move {
            let _guard = seek_lock.lock().await;
            if seek_seq.load(Ordering::SeqCst) != current_seq {
                return;
            }

            let result = tokio::task::spawn_blocking(move || player.seek_to(target_secs)).await;
            if seek_seq.load(Ordering::SeqCst) == current_seq {
                if matches!(result, Ok(Ok(()))) {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_is_playing(true);
                        ui.set_track_status(slint::SharedString::from("PLAYING"));
                    }
                    if let Some(smtc_arc) = &smtc
                        && let Ok(mut lock) = smtc_arc.lock() {
                            lock.0.set_playback(true);
                        }
                }
                let mut lock = pending_seek.lock().unwrap();
                if let Some(p) = lock.as_ref()
                    && p.id == current_seq {
                        *lock = None;
                    }
            }
        });
    });

    let player_for_vol = Arc::clone(&player);
    let ui_weak_vol = app.as_weak();
    let saved_volume = Arc::new(Mutex::new(1.0f32));
    let saved_vol_for_slider = Arc::clone(&saved_volume);
    app.on_on_user_volume(move |vol_ratio| {
        player_for_vol.set_volume(vol_ratio);
        if vol_ratio > 0.0 {
            *saved_vol_for_slider.lock().unwrap() = vol_ratio;
        }
        if let Some(ui) = ui_weak_vol.upgrade() {
            ui.set_is_muted(vol_ratio <= 0.0);
        }
    });

    let player_for_mute = player;
    let ui_weak_mute = app.as_weak();
    let saved_vol_for_mute = saved_volume;
    app.on_toggle_mute(move || {
        let Some(ui) = ui_weak_mute.upgrade() else { return };
        let currently_muted = ui.get_is_muted();
        if currently_muted {
            let restore_vol = *saved_vol_for_mute.lock().unwrap();
            let vol = if restore_vol <= 0.0 { 1.0 } else { restore_vol };
            player_for_mute.set_volume(vol);
            ui.set_volume_ratio(vol);
            ui.set_is_muted(false);
        } else {
            let current_vol = ui.get_volume_ratio();
            if current_vol > 0.0 {
                *saved_vol_for_mute.lock().unwrap() = current_vol;
            }
            player_for_mute.set_volume(0.0);
            ui.set_volume_ratio(0.0);
            ui.set_is_muted(true);
        }
    });
}
