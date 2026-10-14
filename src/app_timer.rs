use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::info;

use muda::MenuEvent;
use souvlaki::MediaControlEvent;
use tray_icon::TrayIconEvent;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
};

use crate::extractor::Extractor;
use crate::library::LibraryStore;
use crate::playback_coordinator::{
    trigger_next, trigger_previous, trigger_toggle_play_pause, LoadingAbortHandle, SmtcHandle,
};
use crate::player::AudioPlayer;
use crate::queue::PlayQueue;
use crate::tray::SystemTray;
use crate::ui_callbacks::PendingSeekState;
use crate::ui_models::queue_items_model;
use crate::win32_window::{set_taskbar_button, try_find_and_hook};
use crate::AppWindow;
use slint::ComponentHandle;

pub fn start_playback_polling_timer(
    app: &AppWindow,
    player: Arc<AudioPlayer>,
    extractor: Arc<Extractor>,
    play_queue: PlayQueue,
    library: LibraryStore,
    tray: Option<Arc<SystemTray>>,
    smtc: SmtcHandle,
    main_hwnd: Arc<Mutex<Option<HWND>>>,
    is_auto_advancing: Arc<Mutex<bool>>,
    last_query: Arc<Mutex<Option<String>>>,
    current_loading_abort: LoadingAbortHandle,
    pending_seek: Arc<Mutex<Option<PendingSeekState>>>,
    audio_stream: rodio::OutputStream,
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
    let mut queue_display_snapshot = Vec::<String>::new();
    let mut stream_error_displayed = false;
    #[allow(unused_assignments)]
    let mut active_audio_stream: Option<rodio::OutputStream> = Some(audio_stream);

    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            let _ = &active_audio_stream;
            // Check for system sleep suspension event
            if crate::win32_window::take_system_suspended() && !player_for_timer.is_paused() {
                info!("System sleep detected: auto-pausing playback to preserve position.");
                player_for_timer.pause();
                if let Some(ui) = ui_weak_timer.upgrade() {
                    ui.set_is_playing(false);
                    ui.set_track_status(slint::SharedString::from("PAUSED"));
                }
                if let Some(smtc_arc) = &smtc_clone
                    && let Ok(mut lock) = smtc_arc.lock() {
                        lock.0.set_playback(false);
                    }
            }

            // Check for system resume event
            if crate::win32_window::take_system_resumed() {
                info!("System resume detected: re-validating default audio output device.");
                match rodio::OutputStream::try_default() {
                    Ok((new_stream, new_handle)) => {
                        info!("Successfully re-initialized audio output device after system resume.");
                        player_for_timer.update_stream_handle(new_handle);
                        active_audio_stream = Some(new_stream);
                    }
                    Err(e) => {
                        tracing::warn!("Could not re-open audio device immediately after resume: {:#}", e);
                    }
                }
            }

            let current_queue_display = queue_for_timer.display_items();
            if current_queue_display != queue_display_snapshot {
                queue_display_snapshot = current_queue_display;
                if let Some(ui) = ui_weak_timer.upgrade() {
                    ui.set_queue_items(queue_items_model(&queue_for_timer, &library_for_timer));
                }
            }

            // Ensure HWND is subclassed if not caught on startup
            {
                let mut guard = hwnd_for_timer.lock().unwrap();
                if guard.is_none() {
                    if let Some(hwnd) = try_find_and_hook() {
                        info!(
                            "Late-attached Win32 minimize-to-tray subclass to HWND: {:?}",
                            hwnd
                        );
                        *guard = Some(hwnd);
                    }
                }
            }

            // 1. Process Tray Menu Events
            while let Ok(menu_event) = MenuEvent::receiver().try_recv() {
                if let Some(t) = &tray_clone {
                    if menu_event.id == t.quit_id {
                        let _ = slint::quit_event_loop();
                    } else if menu_event.id == t.show_id {
                        if let Some(hwnd) = *hwnd_for_timer.lock().unwrap() {
                            unsafe {
                                set_taskbar_button(hwnd, true);
                                let _ = ShowWindow(hwnd, SW_SHOW);
                                let _ = ShowWindow(hwnd, SW_RESTORE);
                                let _ = SetForegroundWindow(hwnd);
                            }
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
                        );
                    }
                }
            }

            // 2. Process Tray Icon Double-Click (Directly Restore window from tray)
            while let Ok(tray_event) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::DoubleClick { .. } = tray_event {
                    info!("Tray double-clicked: Restoring main window.");
                    if let Some(hwnd) = *hwnd_for_timer.lock().unwrap() {
                        unsafe {
                            set_taskbar_button(hwnd, true);
                            let _ = ShowWindow(hwnd, SW_SHOW);
                            let _ = ShowWindow(hwnd, SW_RESTORE);
                            let _ = SetForegroundWindow(hwnd);
                        }
                    }
                    if let Some(ui) = ui_weak_timer.upgrade() {
                        let _ = ui.show();
                    }
                }
            }

            // 3. Process Windows SMTC Media Key Events
            if let Some(smtc_arc) = &smtc_clone
                && let Ok(mut lock) = smtc_arc.try_lock() {
                    while let Ok(event) = lock.1.try_recv() {
                        match event {
                            MediaControlEvent::Play => {
                                if player_for_timer.is_paused() {
                                    trigger_toggle_play_pause(
                                        Arc::clone(&player_for_timer),
                                        Arc::clone(&extractor_for_timer),
                                        smtc_clone.clone(),
                                        ui_weak_timer.clone(),
                                        Arc::clone(&last_query_for_timer),
                                    );
                                }
                            }
                            MediaControlEvent::Pause => {
                                if !player_for_timer.is_paused() {
                                    trigger_toggle_play_pause(
                                        Arc::clone(&player_for_timer),
                                        Arc::clone(&extractor_for_timer),
                                        smtc_clone.clone(),
                                        ui_weak_timer.clone(),
                                        Arc::clone(&last_query_for_timer),
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
                                player_for_timer.stop();
                                lock.0.set_playback(false);
                                if let Some(ui) = ui_weak_timer.upgrade() {
                                    ui.set_is_playing(false);
                                    ui.set_track_status(slint::SharedString::from("STOPPED"));
                                }
                            }
                            _ => {}
                        }
                    }
                }

            // 4. Update Playback Progress Bar (Only when user is NOT dragging)
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
                            && let Ok(mut lock) = smtc_arc.lock() {
                                lock.0.set_playback(false);
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

                    if let Some(ui) = ui_weak_timer.upgrade()
                        && ui.get_has_active_track() {
                            if player_for_timer.is_buffering() || is_pending_seek {
                                ui.set_track_status(slint::SharedString::from("BUFFERING"));
                            } else if ui.get_track_status() == "BUFFERING" {
                                ui.set_track_status(slint::SharedString::from("PLAYING"));
                            }
                        }
                }

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

                let current_pos = player_for_timer.current_position();
                let total_dur = player_for_timer.total_duration();
                if total_dur > 0.0
                    && let Some(ui) = ui_weak_timer.upgrade() {
                        if !ui.get_is_scrubbing() && !is_pending_seek {
                            let ratio = (current_pos / total_dur).clamp(0.0, 1.0);
                            ui.set_progress_ratio(ratio);

                            let cur_m = (current_pos as u64) / 60;
                            let cur_s = (current_pos as u64) % 60;
                            let tot_m = (total_dur as u64) / 60;
                            let tot_s = (total_dur as u64) % 60;
                            ui.set_time_current(slint::SharedString::from(format!("{:02}:{:02}", cur_m, cur_s)));
                            ui.set_time_total(slint::SharedString::from(format!("{:02}:{:02}", tot_m, tot_s)));
                            ui.set_time_display(slint::SharedString::from(format!(
                                "{:02}:{:02} / {:02}:{:02}",
                                cur_m, cur_s, tot_m, tot_s
                            )));
                        }

                        let buf_ratio = player_for_timer.buffer_ratio();
                        if !is_pending_seek || buf_ratio > 0.0 {
                            ui.set_buffer_ratio(buf_ratio);
                        }
                    }

                // 5. Gapless Preload: Pre-fetch next track when <= 10s remaining
                if total_dur > 15.0 && (total_dur - current_pos) <= 10.0 {
                    queue_for_timer.preload_next(Arc::clone(&extractor_for_timer));
                }

                // 6. Continuous Autoplay: Auto-advance with preloaded metadata for seamless transition
                if player_for_timer.has_ended() {
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
        },
    );

    timer
}
