use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::info;

use muda::MenuEvent;
use souvlaki::MediaControlEvent;
use tray_icon::TrayIconEvent;
use windows::Win32::Foundation::HWND;

use crate::AppWindow;
use crate::extractor::Extractor;
use crate::library::{LibraryStore, format_duration_secs};
use crate::playback_coordinator::{
    LoadingAbortHandle, SmtcHandle, trigger_next, trigger_previous, trigger_seek_to_secs,
    trigger_toggle_play_pause,
};
use crate::player::AudioPlayer;
use crate::queue::PlayQueue;
use crate::tray::SystemTray;
use crate::ui_callbacks::{PendingSeekState, SleepTimerMode};
use crate::ui_models::queue_items_model;
use crate::win32_window::{
    check_default_audio_device_changed, is_window_visible, restore_from_tray, try_find_and_hook,
};
use slint::ComponentHandle;

#[allow(clippy::too_many_arguments)]
pub fn start_playback_polling_timer(
    app: &AppWindow,
    player: Arc<AudioPlayer>,
    extractor: Arc<Extractor>,
    play_queue: PlayQueue,
    library: LibraryStore,
    tray: Option<Arc<SystemTray>>,
    smtc: SmtcHandle,
    smtc_rx: Option<std::sync::mpsc::Receiver<MediaControlEvent>>,
    main_hwnd: Arc<Mutex<Option<HWND>>>,
    is_auto_advancing: Arc<Mutex<bool>>,
    last_query: Arc<Mutex<Option<String>>>,
    current_loading_abort: LoadingAbortHandle,
    pending_seek: Arc<Mutex<Option<PendingSeekState>>>,
    audio_stream: rodio::OutputStream,
    seek_seq: Arc<AtomicU64>,
    seek_lock: Arc<tokio::sync::Mutex<()>>,
    sleep_timer: Arc<Mutex<SleepTimerMode>>,
) -> slint::Timer {
    let timer = slint::Timer::default();
    let tray_clone = tray;
    let smtc_clone = smtc;
    let player_for_timer = Arc::clone(&player);
    let extractor_for_timer = Arc::clone(&extractor);
    let queue_for_timer = play_queue;
    let library_for_timer = library;
    let ui_weak_timer = app.as_weak();
    let hwnd_for_timer = Arc::clone(&main_hwnd);
    let is_auto_advancing_timer = Arc::clone(&is_auto_advancing);
    let last_query_for_timer = Arc::clone(&last_query);
    let loading_abort_for_timer = Arc::clone(&current_loading_abort);
    let pending_seek_for_timer = Arc::clone(&pending_seek);
    let seek_seq_for_timer = seek_seq;
    let seek_lock_for_timer = seek_lock;
    let mut queue_display_snapshot = Vec::<String>::new();
    let mut last_tray_snapshot: Option<(String, String, bool)> = None;
    let mut stream_error_displayed = false;
    let mut last_smtc_sync = Instant::now();
    let mut last_pos_persist = Instant::now();
    let mut tick_counter: u64 = 0;
    #[allow(unused_assignments)]
    let mut active_audio_stream: Option<rodio::OutputStream> = Some(audio_stream);

    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            let _ = &active_audio_stream;
            tick_counter = tick_counter.wrapping_add(1);
            let window_visible = is_window_visible();

            // Persist window position if moved by user
            if let Some((x, y)) = crate::win32_window::take_window_moved_pos() {
                library_for_timer.set_window_pos(x, y);
            }

            // Check for system sleep suspension event
            if crate::win32_window::take_system_suspended() && !player_for_timer.is_paused() {
                info!("System sleep detected: auto-pausing playback to preserve position.");
                let cur_pos = player_for_timer.current_position();
                player_for_timer.pause();
                library_for_timer.update_playback_position(cur_pos);
                if let Some(ui) = ui_weak_timer.upgrade() {
                    ui.set_is_playing(false);
                    ui.set_track_status(slint::SharedString::from("PAUSED"));
                }
                if let Some(smtc_arc) = &smtc_clone
                    && let Ok(mut lock) = smtc_arc.lock()
                {
                    lock.set_playback(false, Some(Duration::from_secs_f32(cur_pos)));
                }
            }

            // Check for system resume OR default audio output device hot-swap (every 1.5s)
            let device_hot_swapped =
                tick_counter.is_multiple_of(15) && check_default_audio_device_changed();
            if crate::win32_window::take_system_resumed() || device_hot_swapped {
                info!("Audio output endpoint change or system resume detected; re-binding audio stream.");
                match rodio::OutputStream::try_default() {
                    Ok((new_stream, new_handle)) => {
                        player_for_timer.update_stream_handle(new_handle);
                        active_audio_stream = Some(new_stream);
                    }
                    Err(e) => {
                        tracing::warn!("Could not re-open audio device: {:#}", e);
                    }
                }
            }

            // Ensure HWND is subclassed if not caught on startup
            {
                let mut guard = hwnd_for_timer.lock().unwrap();
                if guard.is_none()
                    && let Some(hwnd) = try_find_and_hook()
                {
                    info!("Late-attached Win32 subclass to HWND: {:?}", hwnd);
                    *guard = Some(hwnd);
                    if let Some(smtc_arc) = &smtc_clone
                        && let Ok(mut lock) = smtc_arc.lock()
                        && lock.is_dummy()
                    {
                        let _ = lock.rebind_hwnd(hwnd);
                    }
                }
            }

            // 1. Process Tray Menu Events (Show, Play/Pause, Next, Previous, Quit)
            while let Ok(menu_event) = MenuEvent::receiver().try_recv() {
                if let Some(t) = &tray_clone {
                    if menu_event.id == t.quit_id {
                        let _ = slint::quit_event_loop();
                    } else if menu_event.id == t.show_id {
                        if let Some(hwnd) = *hwnd_for_timer.lock().unwrap() {
                            restore_from_tray(hwnd);
                        }
                        if let Some(ui) = ui_weak_timer.upgrade() {
                            let _ = ui.show();
                        }
                    } else if menu_event.id == t.play_pause_id {
                        trigger_toggle_play_pause(
                            Arc::clone(&player_for_timer),
                            Arc::clone(&extractor_for_timer),
                            smtc_clone.clone(),
                            ui_weak_timer.clone(),
                            Arc::clone(&last_query_for_timer),
                            queue_for_timer.clone(),
                            library_for_timer.clone(),
                            loading_abort_for_timer.clone(),
                        );
                    } else if menu_event.id == t.next_id {
                        trigger_next(
                            Arc::clone(&extractor_for_timer),
                            Arc::clone(&player_for_timer),
                            smtc_clone.clone(),
                            ui_weak_timer.clone(),
                            queue_for_timer.clone(),
                            library_for_timer.clone(),
                            Arc::clone(&is_auto_advancing_timer),
                            false,
                            Arc::clone(&last_query_for_timer),
                            loading_abort_for_timer.clone(),
                        );
                    } else if menu_event.id == t.prev_id {
                        trigger_previous(
                            Arc::clone(&extractor_for_timer),
                            Arc::clone(&player_for_timer),
                            smtc_clone.clone(),
                            ui_weak_timer.clone(),
                            queue_for_timer.clone(),
                            library_for_timer.clone(),
                            Arc::clone(&is_auto_advancing_timer),
                            Arc::clone(&last_query_for_timer),
                            loading_abort_for_timer.clone(),
                        );
                    }
                }
            }

            // 2. Process Tray Icon Double-Click
            while let Ok(tray_event) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::DoubleClick { .. } = tray_event {
                    info!("Tray double-clicked: Restoring main window.");
                    if let Some(hwnd) = *hwnd_for_timer.lock().unwrap() {
                        restore_from_tray(hwnd);
                    }
                    if let Some(ui) = ui_weak_timer.upgrade() {
                        let _ = ui.show();
                    }
                }
            }

            // 3. Process Windows SMTC Media Key Events
            if let Some(rx) = &smtc_rx {
                while let Ok(event) = rx.try_recv() {
                    match event {
                        MediaControlEvent::Play => {
                            if !player_for_timer.is_active() {
                                trigger_toggle_play_pause(
                                    Arc::clone(&player_for_timer),
                                    Arc::clone(&extractor_for_timer),
                                    smtc_clone.clone(),
                                    ui_weak_timer.clone(),
                                    Arc::clone(&last_query_for_timer),
                                    queue_for_timer.clone(),
                                    library_for_timer.clone(),
                                    loading_abort_for_timer.clone(),
                                );
                            }
                        }
                        MediaControlEvent::Pause => {
                            if player_for_timer.is_active() {
                                trigger_toggle_play_pause(
                                    Arc::clone(&player_for_timer),
                                    Arc::clone(&extractor_for_timer),
                                    smtc_clone.clone(),
                                    ui_weak_timer.clone(),
                                    Arc::clone(&last_query_for_timer),
                                    queue_for_timer.clone(),
                                    library_for_timer.clone(),
                                    loading_abort_for_timer.clone(),
                                );
                            }
                        }
                        MediaControlEvent::Toggle => {
                            trigger_toggle_play_pause(
                                Arc::clone(&player_for_timer),
                                Arc::clone(&extractor_for_timer),
                                smtc_clone.clone(),
                                ui_weak_timer.clone(),
                                Arc::clone(&last_query_for_timer),
                                queue_for_timer.clone(),
                                library_for_timer.clone(),
                                loading_abort_for_timer.clone(),
                            );
                        }
                        MediaControlEvent::Next => {
                            trigger_next(
                                Arc::clone(&extractor_for_timer),
                                Arc::clone(&player_for_timer),
                                smtc_clone.clone(),
                                ui_weak_timer.clone(),
                                queue_for_timer.clone(),
                                library_for_timer.clone(),
                                Arc::clone(&is_auto_advancing_timer),
                                false,
                                Arc::clone(&last_query_for_timer),
                                loading_abort_for_timer.clone(),
                            );
                        }
                        MediaControlEvent::Previous => {
                            trigger_previous(
                                Arc::clone(&extractor_for_timer),
                                Arc::clone(&player_for_timer),
                                smtc_clone.clone(),
                                ui_weak_timer.clone(),
                                queue_for_timer.clone(),
                                library_for_timer.clone(),
                                Arc::clone(&is_auto_advancing_timer),
                                Arc::clone(&last_query_for_timer),
                                loading_abort_for_timer.clone(),
                            );
                        }
                        MediaControlEvent::Stop => {
                            if let Ok(mut guard) = loading_abort_for_timer.lock()
                                && let Some(handle) = guard.take()
                            {
                                handle.abort();
                            }
                            player_for_timer.stop();
                            if let Some(smtc_arc) = &smtc_clone
                                && let Ok(mut lock) = smtc_arc.lock()
                            {
                                lock.set_stopped();
                            }
                            if let Some(ui) = ui_weak_timer.upgrade() {
                                ui.set_is_playing(false);
                                ui.set_can_retry(false);
                                ui.set_track_status(slint::SharedString::from("STOPPED"));
                                ui.set_progress_ratio(0.0);
                                ui.set_buffer_ratio(0.0);
                                ui.set_time_current(slint::SharedString::from("00:00"));
                                ui.set_time_total(slint::SharedString::from("00:00"));
                                ui.set_time_display(slint::SharedString::from("00:00 / 00:00"));
                            }
                        }
                        MediaControlEvent::SetPosition(pos) => {
                            trigger_seek_to_secs(
                                pos.0.as_secs_f32(),
                                Arc::clone(&player_for_timer),
                                smtc_clone.clone(),
                                ui_weak_timer.clone(),
                                pending_seek_for_timer.clone(),
                                Arc::clone(&seek_seq_for_timer),
                                Arc::clone(&seek_lock_for_timer),
                            );
                        }
                        MediaControlEvent::Seek(direction) => {
                            let current_pos = player_for_timer.current_position();
                            let delta = 10.0;
                            let target = match direction {
                                souvlaki::SeekDirection::Forward => current_pos + delta,
                                souvlaki::SeekDirection::Backward => (current_pos - delta).max(0.0),
                            };
                            trigger_seek_to_secs(
                                target,
                                Arc::clone(&player_for_timer),
                                smtc_clone.clone(),
                                ui_weak_timer.clone(),
                                pending_seek_for_timer.clone(),
                                Arc::clone(&seek_seq_for_timer),
                                Arc::clone(&seek_lock_for_timer),
                            );
                        }
                        MediaControlEvent::SeekBy(direction, amount) => {
                            let current_pos = player_for_timer.current_position();
                            let delta = amount.as_secs_f32();
                            let target = match direction {
                                souvlaki::SeekDirection::Forward => current_pos + delta,
                                souvlaki::SeekDirection::Backward => (current_pos - delta).max(0.0),
                            };
                            trigger_seek_to_secs(
                                target,
                                Arc::clone(&player_for_timer),
                                smtc_clone.clone(),
                                ui_weak_timer.clone(),
                                pending_seek_for_timer.clone(),
                                Arc::clone(&seek_seq_for_timer),
                                Arc::clone(&seek_lock_for_timer),
                            );
                        }
                        _ => {}
                    }
                }
            }

            // Update System Tray dynamic tooltip & Play/Pause menu text when track or state changes
            if tick_counter.is_multiple_of(5)
                && let Some(t) = &tray_clone
                && let Some(ui) = ui_weak_timer.upgrade()
            {
                let title = ui.get_track_title().to_string();
                let artist = ui.get_track_artist().to_string();
                let playing = ui.get_is_playing();
                let snap = (title.clone(), artist.clone(), playing);
                if last_tray_snapshot.as_ref() != Some(&snap) {
                    t.update_status(Some(&title), Some(&artist), playing);
                    last_tray_snapshot = Some(snap);
                }
            }

            // Evaluate Sleep Timer countdown and smooth 10-second volume fade-out
            {
                let mut timer_guard = sleep_timer.lock().unwrap();
                if let SleepTimerMode::Minutes { deadline, .. } = *timer_guard {
                    let now = Instant::now();
                    if now >= deadline {
                        *timer_guard = SleepTimerMode::Off;
                        drop(timer_guard);
                        player_for_timer.pause();
                        player_for_timer.set_fade_gain(1.0);
                        if let Some(ui) = ui_weak_timer.upgrade() {
                            ui.set_is_playing(false);
                            ui.set_sleep_timer_label(slint::SharedString::default());
                            ui.set_track_status(slint::SharedString::from("PAUSED"));
                            ui.set_panel_message(slint::SharedString::from(
                                "Sleep timer completed; playback paused",
                            ));
                        }
                    } else {
                        let remaining_secs = deadline.duration_since(now).as_secs_f32();
                        if remaining_secs <= 10.0 {
                            player_for_timer.set_fade_gain((remaining_secs / 10.0).clamp(0.05, 1.0));
                        }
                        if window_visible && tick_counter.is_multiple_of(10) {
                            let badge = timer_guard.badge_label();
                            if let Some(ui) = ui_weak_timer.upgrade() {
                                ui.set_sleep_timer_label(slint::SharedString::from(badge));
                            }
                        }
                    }
                }
            }

            // When hidden in System Tray, throttle background checks to 500ms and skip all Slint UI redraws!
            if !window_visible && !tick_counter.is_multiple_of(5) {
                return;
            }

            if window_visible {
                let current_queue_display = queue_for_timer.display_items();
                if current_queue_display != queue_display_snapshot {
                    queue_display_snapshot = current_queue_display;
                    if let Some(ui) = ui_weak_timer.upgrade() {
                        ui.set_queue_items(queue_items_model(&queue_for_timer, &library_for_timer));
                    }
                }
            }

            // 4. Update Playback Progress & Auto-Advance
            if !player_for_timer.is_paused() {
                if let Some(stream_error) = player_for_timer.stream_error() {
                    if !stream_error_displayed {
                        stream_error_displayed = true;
                        if let Some(ui) = ui_weak_timer.upgrade() {
                            ui.set_is_loading(false);
                            ui.set_is_playing(false);
                            ui.set_can_retry(true);
                            ui.set_has_active_track(false);
                            ui.set_track_status(slint::SharedString::from("ERROR"));
                            ui.set_track_artist(slint::SharedString::from(format!(
                                "Stream interrupted: {}",
                                stream_error
                            )));
                        }
                        if let Some(smtc_arc) = &smtc_clone
                            && let Ok(mut lock) = smtc_arc.lock()
                        {
                            lock.set_playback(
                                false,
                                Some(Duration::from_secs_f32(player_for_timer.current_position())),
                            );
                        }
                    }
                } else {
                    stream_error_displayed = false;
                    let is_pending_seek = {
                        let mut lock = pending_seek_for_timer.lock().unwrap();
                        if let Some(pending) = lock.as_ref() {
                            if pending.requested_at.elapsed() < Duration::from_secs(30) {
                                true
                            } else {
                                *lock = None;
                                false
                            }
                        } else {
                            false
                        }
                    };

                    if window_visible
                        && let Some(ui) = ui_weak_timer.upgrade()
                        && ui.get_has_active_track()
                    {
                        if player_for_timer.is_buffering() || is_pending_seek {
                            ui.set_track_status(slint::SharedString::from("BUFFERING"));
                        } else if ui.get_track_status() == "BUFFERING" {
                            ui.set_track_status(slint::SharedString::from("PLAYING"));
                        }
                    }

                    let current_pos = player_for_timer.current_position();
                    let total_dur = player_for_timer.total_duration();

                    // Persist playback position every 5 seconds
                    if player_for_timer.is_active()
                        && last_pos_persist.elapsed() >= Duration::from_secs(5)
                    {
                        last_pos_persist = Instant::now();
                        library_for_timer.update_playback_position(current_pos);
                    }

                    if window_visible
                        && total_dur > 0.0
                        && let Some(ui) = ui_weak_timer.upgrade()
                    {
                        if !ui.get_is_scrubbing() && !is_pending_seek {
                            let ratio = (current_pos / total_dur).clamp(0.0, 1.0);
                            ui.set_progress_ratio(ratio);

                            let cur_str = format_duration_secs(current_pos as f64);
                            let tot_str = format_duration_secs(total_dur as f64);
                            ui.set_time_current(slint::SharedString::from(cur_str.clone()));
                            ui.set_time_total(slint::SharedString::from(tot_str.clone()));
                            ui.set_time_display(slint::SharedString::from(format!(
                                "{} / {}",
                                cur_str, tot_str
                            )));
                        }

                        let buf_ratio = player_for_timer.buffer_ratio();
                        if !is_pending_seek || buf_ratio > 0.0 {
                            ui.set_buffer_ratio(buf_ratio);
                        }
                    }

                    // Periodically synchronize SMTC timeline position
                    if player_for_timer.is_active()
                        && !is_pending_seek
                        && total_dur > 0.0
                        && last_smtc_sync.elapsed() >= Duration::from_secs(4)
                    {
                        last_smtc_sync = Instant::now();
                        if let Some(smtc_arc) = &smtc_clone
                            && let Ok(mut lock) = smtc_arc.try_lock()
                        {
                            lock.set_playback(true, Some(Duration::from_secs_f32(current_pos)));
                        }
                    }

                    // 5. Gapless Preload: Pre-fetch next track when <= 10s remaining
                    if total_dur > 15.0 && (total_dur - current_pos) <= 10.0 {
                        queue_for_timer.preload_next(
                            Arc::clone(&extractor_for_timer),
                            library_for_timer.clone(),
                        );
                    }

                    // 6. Continuous Autoplay or Sleep Timer End-of-Track Stop
                    if player_for_timer.has_ended() {
                        let stop_at_eot = {
                            let mut guard = sleep_timer.lock().unwrap();
                            if matches!(*guard, SleepTimerMode::EndOfTrack) {
                                *guard = SleepTimerMode::Off;
                                true
                            } else {
                                false
                            }
                        };
                        if stop_at_eot {
                            player_for_timer.stop();
                            if let Some(ui) = ui_weak_timer.upgrade() {
                                ui.set_is_playing(false);
                                ui.set_sleep_timer_label(slint::SharedString::default());
                                ui.set_track_status(slint::SharedString::from("STOPPED"));
                                ui.set_panel_message(slint::SharedString::from(
                                    "Sleep timer: stopped after track finished",
                                ));
                            }
                        } else {
                            trigger_next(
                                Arc::clone(&extractor_for_timer),
                                Arc::clone(&player_for_timer),
                                smtc_clone.clone(),
                                ui_weak_timer.clone(),
                                queue_for_timer.clone(),
                                library_for_timer.clone(),
                                Arc::clone(&is_auto_advancing_timer),
                                true,
                                Arc::clone(&last_query_for_timer),
                                loading_abort_for_timer.clone(),
                            );
                        }
                    }
                }
            }
        },
    );

    timer
}