use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use tracing::info;

use crate::AppWindow;
use crate::library::{LibraryStore, format_duration_secs};
use crate::playback_coordinator::{SmtcHandle, trigger_seek_to_ratio, trigger_seek_to_secs};
use crate::player::AudioPlayer;
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
            && let Some(handle) = guard.take()
        {
            handle.abort();
            info!("Active loading task and yt-dlp subprocess aborted by user.");
        }
        if let Ok(mut advancing) = advance_for_cancel.lock() {
            *advancing = false;
        }
        if let Some(ui) = ui_weak_cancel.upgrade() {
            ui.set_is_loading(false);
            if ui.get_track_status() == "LOADING..." {
                ui.set_track_status(slint::SharedString::from(if ui.get_is_playing() {
                    "PLAYING"
                } else {
                    "READY"
                }));
            }
        }
    });

    let player_for_stop = Arc::clone(&ctx.player);
    let smtc_for_stop = ctx.smtc;
    let ui_weak_stop = ctx.ui_weak;
    let abort_for_stop = ctx.loading_abort;
    app.on_stop(move || {
        if let Ok(mut guard) = abort_for_stop.lock()
            && let Some(handle) = guard.take()
        {
            handle.abort();
        }
        player_for_stop.stop();
        if let Some(smtc_arc) = &smtc_for_stop
            && let Ok(mut lock) = smtc_arc.lock()
        {
            lock.set_stopped();
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
    library: LibraryStore,
    pending_seek: Arc<Mutex<Option<PendingSeekState>>>,
    seek_seq: Arc<AtomicU64>,
    seek_lock: Arc<tokio::sync::Mutex<()>>,
) {
    let player_for_scrub = Arc::clone(&player);
    let ui_weak_scrub = app.as_weak();
    app.on_on_user_scrub(move |ratio| {
        let total_dur = player_for_scrub.total_duration();
        let target_secs = ratio * total_dur;
        let cur_str = format_duration_secs(target_secs as f64);
        let tot_str = format_duration_secs(total_dur as f64);
        if let Some(ui) = ui_weak_scrub.upgrade() {
            ui.set_time_current(slint::SharedString::from(cur_str.clone()));
            ui.set_time_total(slint::SharedString::from(tot_str.clone()));
            ui.set_time_display(slint::SharedString::from(format!("{} / {}", cur_str, tot_str)));
        }
    });

    let player_for_seek = Arc::clone(&player);
    let ui_weak_seek = app.as_weak();
    let smtc_for_seek = smtc.clone();
    let pending_seek_for_seek = pending_seek.clone();
    let seek_seq_for_seek = Arc::clone(&seek_seq);
    let seek_lock_for_seek = Arc::clone(&seek_lock);
    app.on_on_user_seek(move |ratio| {
        trigger_seek_to_ratio(
            ratio,
            Arc::clone(&player_for_seek),
            smtc_for_seek.clone(),
            ui_weak_seek.clone(),
            pending_seek_for_seek.clone(),
            Arc::clone(&seek_seq_for_seek),
            Arc::clone(&seek_lock_for_seek),
        );
    });

    let player_for_rel = Arc::clone(&player);
    let ui_weak_rel = app.as_weak();
    let smtc_for_rel = smtc;
    let pending_seek_for_rel = pending_seek;
    let seek_seq_for_rel = Arc::clone(&seek_seq);
    let seek_lock_for_rel = Arc::clone(&seek_lock);
    app.on_seek_relative(move |delta_secs| {
        let target = (player_for_rel.current_position() + delta_secs).max(0.0);
        trigger_seek_to_secs(
            target,
            Arc::clone(&player_for_rel),
            smtc_for_rel.clone(),
            ui_weak_rel.clone(),
            pending_seek_for_rel.clone(),
            Arc::clone(&seek_seq_for_rel),
            Arc::clone(&seek_lock_for_rel),
        );
    });

    let initial_vol = library.volume();
    let saved_volume = Arc::new(Mutex::new(if initial_vol > 0.0 {
        initial_vol
    } else {
        1.0f32
    }));

    let player_for_vol = Arc::clone(&player);
    let ui_weak_vol = app.as_weak();
    let saved_vol_for_slider = Arc::clone(&saved_volume);
    let lib_for_vol = library.clone();
    app.on_on_user_volume(move |vol_ratio| {
        let vol_ratio = vol_ratio.clamp(0.0, 1.0);
        player_for_vol.set_volume(vol_ratio);
        let muted = vol_ratio <= 0.0;
        if !muted {
            *saved_vol_for_slider.lock().unwrap() = vol_ratio;
        }
        lib_for_vol.set_volume_state(vol_ratio, muted);
        if let Some(ui) = ui_weak_vol.upgrade() {
            ui.set_is_muted(muted);
        }
    });

    let player_for_mute = player;
    let ui_weak_mute = app.as_weak();
    let saved_vol_for_mute = saved_volume;
    let lib_for_mute = library;
    app.on_toggle_mute(move || {
        let Some(ui) = ui_weak_mute.upgrade() else {
            return;
        };
        let currently_muted = ui.get_is_muted();
        if currently_muted {
            let restore_vol = *saved_vol_for_mute.lock().unwrap();
            let vol = if restore_vol <= 0.0 { 1.0 } else { restore_vol };
            player_for_mute.set_volume(vol);
            lib_for_mute.set_volume_state(vol, false);
            ui.set_volume_ratio(vol);
            ui.set_is_muted(false);
        } else {
            let current_vol = ui.get_volume_ratio();
            if current_vol > 0.0 {
                *saved_vol_for_mute.lock().unwrap() = current_vol;
            }
            player_for_mute.set_volume(0.0);
            lib_for_mute.set_volume_state(current_vol, true);
            ui.set_volume_ratio(0.0);
            ui.set_is_muted(true);
        }
    });
}