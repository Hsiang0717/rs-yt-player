#![windows_subsystem = "windows"]

mod buffer;
mod cli;
mod extractor;
mod player;
mod queue;
mod smtc;
mod stream_reader;
mod tray;

slint::include_modules!();

use anyhow::Result;
use cli::run_cli_mode;
use extractor::{AudioMetadata, Extractor};
use muda::MenuEvent;
use player::AudioPlayer;
use queue::PlayQueue;
use slint::ComponentHandle;
use smtc::SmtcManager;
use souvlaki::MediaControlEvent;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;
use tray::SystemTray;
use tray_icon::TrayIconEvent;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SetForegroundWindow, ShowWindow, SW_HIDE, SW_RESTORE, SW_SHOW, WM_SYSCOMMAND,
};

/// Win32 window subclass procedure to intercept SC_MINIMIZE
/// This hides the window completely from desktop and taskbar,
/// keeping the Slint event loop and background audio playing uninterrupted.
unsafe extern "system" fn window_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _uid: usize,
    _ref_data: usize,
) -> LRESULT {
    const SC_MINIMIZE_CODE: usize = 0xF020;
    if msg == WM_SYSCOMMAND && (wparam.0 & 0xFFF0) == SC_MINIMIZE_CODE {
        info!("Window minimize requested: Hiding to system tray without stopping audio.");
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        return LRESULT(0);
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

type SmtcHandle = Option<Arc<Mutex<(SmtcManager, std::sync::mpsc::Receiver<MediaControlEvent>)>>>;

/// Play track directly from pre-extracted metadata and cover image
async fn play_with_metadata(
    query_str: String,
    meta: AudioMetadata,
    cover_path: Option<std::path::PathBuf>,
    extractor: Arc<Extractor>,
    player: Arc<AudioPlayer>,
    smtc: SmtcHandle,
    ui_weak: slint::Weak<AppWindow>,
    queue: PlayQueue,
) {
    queue.record_current(query_str);
    let title = meta.title.clone();
    let uploader = meta.uploader.clone().unwrap_or_else(|| "Unknown Artist".to_string());
    let duration_secs = meta.duration.unwrap_or(0.0) as f32;
    let m = (duration_secs as u64) / 60;
    let s = (duration_secs as u64) % 60;
    let duration_str = format!("{:02}:{:02}", m, s);
    let badge = meta.audio_codec.unwrap_or_else(|| "OPUS 160K".to_string()).to_uppercase();
    let current_id = meta.id.clone();

    // Start streaming via ffmpeg PCM pipe immediately
    let stream_url = meta.stream_url.clone();
    let play_res = tokio::task::spawn_blocking(move || {
        player.play_stream(&stream_url, duration_secs)
    }).await;

    // Update Windows SMTC if available
    if let Some(smtc_arc) = &smtc {
        if let Ok(mut lock) = smtc_arc.lock() {
            lock.0.set_metadata(
                &title,
                &uploader,
                if duration_secs > 0.0 {
                    Some(Duration::from_secs_f32(duration_secs))
                } else {
                    None
                },
            );
            lock.0.set_playback(true);
        }
    }

    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.set_is_loading(false);
        match play_res {
            Ok(Ok(())) => {
                ui.set_track_title(slint::SharedString::from(title));
                ui.set_track_artist(slint::SharedString::from(uploader));
                ui.set_track_badge(slint::SharedString::from(badge));
                ui.set_is_playing(true);
                ui.set_track_status(slint::SharedString::from("PLAYING"));
                ui.set_has_active_track(true);
                ui.set_progress_ratio(0.0);
                ui.set_buffer_ratio(0.0);
                ui.set_time_display(slint::SharedString::from(format!("00:00 / {}", duration_str)));

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
            }
            _ => {
                ui.set_track_status(slint::SharedString::from("PLAYBACK ERROR"));
            }
        }
    });

    // Autoplay prefetch: If queue is empty, prefetch YouTube related songs for seamless continuous playback
    queue.prefetch_autoplay(Arc::clone(&extractor), current_id);
}

/// Core async track player supporting single songs, playlists, and autoplay queues
async fn play_track(
    query_str: String,
    extractor: Arc<Extractor>,
    player: Arc<AudioPlayer>,
    smtc: SmtcHandle,
    ui_weak: slint::Weak<AppWindow>,
    queue: PlayQueue,
) {
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.set_is_loading(true);
        ui.set_track_status(slint::SharedString::from("LOADING..."));
    });

    // Check if query is a playlist URL
    let is_playlist_url = query_str.contains("list=");
    let mut target_query = query_str.clone();

    if is_playlist_url {
        info!("Detected YouTube playlist URL: {}", query_str);
        if let Ok(items) = extractor.extract_playlist_items(&query_str).await {
            if !items.is_empty() {
                info!("Extracted {} items from playlist", items.len());
                if let Some(first) = queue.load_playlist(items) {
                    target_query = first;
                }
            }
        }
    }

    match extractor.extract_audio(&target_query).await {
        Ok(meta) => {
            let mut cover_path = None;
            let temp_cover_path = std::env::temp_dir().join(format!("rs_yt_cover_{}.png", meta.id));
            if let Some(thumb_url) = &meta.thumbnail {
                let client = reqwest::Client::new();
                if let Ok(resp) = client.get(thumb_url).send().await {
                    if let Ok(bytes) = resp.bytes().await {
                        if let Ok(dynamic_img) = image::load_from_memory(&bytes) {
                            if dynamic_img.save_with_format(&temp_cover_path, image::ImageFormat::Png).is_ok() {
                                cover_path = Some(temp_cover_path);
                            }
                        }
                    }
                }
            }

            play_with_metadata(target_query, meta, cover_path, extractor, player, smtc, ui_weak, queue).await;
        }
        Err(err) => {
            error!("Extraction error: {:#}", err);
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.set_is_loading(false);
                ui.set_track_status(slint::SharedString::from("NOT FOUND"));
            });
        }
    }
}

#[cfg(target_os = "windows")]
fn setup_windows_app_identity() {
    if let Ok(exe_path) = std::env::current_exe() {
        let exe_str = exe_path.to_string_lossy().to_string();
        std::thread::spawn(move || {
            use std::os::windows::process::CommandExt;
            let script = format!(
                r#"
                $appName = "YouTube Audio Player"
                $exe = "{}"

                # 1. Register in Applications for FriendlyAppName and ApplicationName
                $appReg = "HKCU:\Software\Classes\Applications\YouTubeAudioPlayer.exe"
                if (!(Test-Path $appReg)) {{
                    New-Item -Path $appReg -Force | Out-Null
                }}
                Set-ItemProperty -Path $appReg -Name "FriendlyAppName" -Value $appName -Force
                Set-ItemProperty -Path $appReg -Name "ApplicationName" -Value $appName -Force

                # 2. Register App Paths
                $appPaths = "HKCU:\Software\Microsoft\Windows\CurrentVersion\App Paths\YouTubeAudioPlayer.exe"
                if (!(Test-Path $appPaths)) {{
                    New-Item -Path $appPaths -Force | Out-Null
                }}
                Set-ItemProperty -Path $appPaths -Name "(Default)" -Value $exe -Force

                # 3. Create or update user Start Menu shortcut
                $programs = [Environment]::GetFolderPath("Programs")
                if ($programs -and (Test-Path $programs)) {{
                    $lnkPath = Join-Path $programs "$appName.lnk"
                    $ws = New-Object -ComObject WScript.Shell
                    $s = $ws.CreateShortcut($lnkPath)
                    $s.TargetPath = $exe
                    $s.IconLocation = "$exe,0"
                    $s.Description = $appName
                    $s.Save()
                }}

                # 4. Create or update common (All Users) Start Menu shortcut
                $commonPrograms = [Environment]::GetFolderPath("CommonPrograms")
                if ($commonPrograms -and (Test-Path $commonPrograms)) {{
                    try {{
                        $commonLnk = Join-Path $commonPrograms "$appName.lnk"
                        $ws = New-Object -ComObject WScript.Shell
                        $s = $ws.CreateShortcut($commonLnk)
                        $s.TargetPath = $exe
                        $s.IconLocation = "$exe,0"
                        $s.Description = $appName
                        $s.Save()
                    }} catch {{}}
                }}
                "#,
                exe_str.replace('\\', "\\\\")
            );
            let _ = std::process::Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script])
                .creation_flags(0x08000000)
                .output();
        });
    }
}

fn trigger_next(
    extractor: Arc<Extractor>,
    player: Arc<AudioPlayer>,
    smtc: SmtcHandle,
    ui_weak: slint::Weak<AppWindow>,
    queue: PlayQueue,
    is_advancing: Arc<Mutex<bool>>,
) {
    let mut advancing = match is_advancing.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if *advancing {
        return;
    }
    *advancing = true;
    if let Some((next_query, maybe_preloaded)) = queue.pop_next() {
        info!("Advancing to next track in queue: {}", next_query);
        let adv_lock = Arc::clone(&is_advancing);
        tokio::spawn(async move {
            if let Some(preloaded) = maybe_preloaded {
                info!("Seamless transition using preloaded track: {}", preloaded.meta.title);
                play_with_metadata(preloaded.query, preloaded.meta, preloaded.cover_path, extractor, player, smtc, ui_weak, queue).await;
            } else {
                play_track(next_query, extractor, player, smtc, ui_weak, queue).await;
            }
            if let Ok(mut guard) = adv_lock.lock() {
                *guard = false;
            }
        });
    } else {
        *advancing = false;
        info!("Next track requested, but queue is empty.");
    }
}

fn trigger_previous(
    extractor: Arc<Extractor>,
    player: Arc<AudioPlayer>,
    smtc: SmtcHandle,
    ui_weak: slint::Weak<AppWindow>,
    queue: PlayQueue,
    is_advancing: Arc<Mutex<bool>>,
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
        tokio::spawn(async move {
            play_track(prev_query, extractor, player, smtc, ui_weak, queue).await;
            if let Ok(mut guard) = adv_lock.lock() {
                *guard = false;
            }
        });
    } else {
        *advancing = false;
        info!("No previous track in history; seeking to beginning.");
        let _ = player.seek_to(0.0);
        if let Some(smtc_arc) = &smtc {
            if let Ok(mut lock) = smtc_arc.lock() {
                lock.0.set_playback(true);
            }
        }
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_progress_ratio(0.0);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let is_smoke_test = raw_args.iter().any(|a| a == "--smoke-test");
    let is_cli = raw_args.iter().any(|a| a == "--cli");

    // 2. Headless CLI / Smoke-Test Mode
    if is_smoke_test || is_cli {
        return run_cli_mode(raw_args, is_smoke_test).await;
    }

    // 3. GUI Mode (Slint + Windows System Tray + SMTC)
    #[cfg(target_os = "windows")]
    setup_windows_app_identity();

    info!("Starting YouTube Audio Player in Morandi GUI mode...");

    let app = AppWindow::new()?;
    let (_stream, stream_handle) = rodio::OutputStream::try_default()
        .map_err(|e| anyhow::anyhow!("Failed to open default Windows audio output device: {}", e))?;
    let player = Arc::new(AudioPlayer::new(&stream_handle)?);

    // Initialize yt-dlp extractor in background
    let extractor = Arc::new(Extractor::new().await?);

    // Continuous Playback & Playlist Queue
    let play_queue = PlayQueue::new();

    // Initialize System Tray
    let tray = match SystemTray::new() {
        Ok(t) => {
            info!("System Tray icon initialized successfully.");
            Some(Arc::new(t))
        }
        Err(e) => {
            error!("Failed to initialize System Tray: {:#}", e);
            None
        }
    };

    // Show initial window so Windows creates the HWND
    app.show()?;

    // Find and subclass the Slint window HWND to hook minimize -> tray
    let main_hwnd: Arc<Mutex<Option<HWND>>> = Arc::new(Mutex::new(None));
    unsafe {
        for _ in 0..10 {
            let hwnd = FindWindowW(None, w!("YouTube Audio Player"));
            if hwnd.0 != 0 {
                let _ = SetWindowSubclass(hwnd, Some(window_subclass_proc), 1001, 0);
                info!("Attached Win32 minimize-to-tray subclass to HWND: {:?}", hwnd);
                *main_hwnd.lock().unwrap() = Some(hwnd);
                break;
            }
            let mut msg = std::mem::zeroed();
            while windows::Win32::UI::WindowsAndMessaging::PeekMessageW(
                &mut msg,
                None,
                0,
                0,
                windows::Win32::UI::WindowsAndMessaging::PM_REMOVE,
            ).as_bool() {
                let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
                windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    // Initialize Windows SMTC (System Media Transport Controls)
    let initial_hwnd = *main_hwnd.lock().unwrap();
    let smtc: SmtcHandle = match SmtcManager::new(initial_hwnd) {
        Ok(mut manager) => {
            let (tx, rx) = std::sync::mpsc::channel();
            if let Ok(()) = manager.attach(move |event| {
                let _ = tx.send(event);
            }) {
                info!("Windows SMTC successfully attached.");
            }
            Some(Arc::new(Mutex::new((manager, rx))))
        }
        Err(e) => {
            info!("SMTC initialization note (non-critical): {:#}", e);
            None
        }
    };

    // Continuous Autoplay / Queue Advance Flag
    let is_auto_advancing = Arc::new(Mutex::new(false));

    // --- Wire GUI Callbacks ---

    // Callback: Search and play
    let player_for_play = Arc::clone(&player);
    let extractor_for_play = Arc::clone(&extractor);
    let smtc_for_play = smtc.clone();
    let ui_weak_play = app.as_weak();
    let queue_for_play = play_queue.clone();

    app.on_search_and_play(move |query| {
        let query_str = query.to_string();
        let player = Arc::clone(&player_for_play);
        let extractor = Arc::clone(&extractor_for_play);
        let smtc = smtc_for_play.clone();
        let ui_weak = ui_weak_play.clone();
        let queue = queue_for_play.clone();

        tokio::spawn(async move {
            play_track(query_str, extractor, player, smtc, ui_weak, queue).await;
        });
    });

    // Callback: Toggle Play / Pause
    let player_for_toggle = Arc::clone(&player);
    let smtc_for_toggle = smtc.clone();
    let ui_weak_toggle = app.as_weak();
    app.on_toggle_play_pause(move || {
        let player = &player_for_toggle;
        let is_paused = player.is_paused();
        if is_paused {
            player.resume();
        } else {
            player.pause();
        }

        if let Some(smtc_arc) = &smtc_for_toggle {
            if let Ok(mut lock) = smtc_arc.lock() {
                lock.0.set_playback(is_paused);
            }
        }

        if let Some(ui) = ui_weak_toggle.upgrade() {
            ui.set_is_playing(is_paused);
            ui.set_track_status(if is_paused {
                slint::SharedString::from("PLAYING")
            } else {
                slint::SharedString::from("PAUSED")
            });
        }
    });

    // Callback: Play Previous
    let player_for_prev = Arc::clone(&player);
    let extractor_for_prev = Arc::clone(&extractor);
    let smtc_for_prev = smtc.clone();
    let ui_weak_prev = app.as_weak();
    let queue_for_prev = play_queue.clone();
    let is_advancing_prev = Arc::clone(&is_auto_advancing);
    app.on_play_previous(move || {
        trigger_previous(
            Arc::clone(&extractor_for_prev),
            Arc::clone(&player_for_prev),
            smtc_for_prev.clone(),
            ui_weak_prev.clone(),
            queue_for_prev.clone(),
            Arc::clone(&is_advancing_prev),
        );
    });

    // Callback: Play Next
    let player_for_next = Arc::clone(&player);
    let extractor_for_next = Arc::clone(&extractor);
    let smtc_for_next = smtc.clone();
    let ui_weak_next = app.as_weak();
    let queue_for_next = play_queue.clone();
    let is_advancing_next = Arc::clone(&is_auto_advancing);
    app.on_play_next(move || {
        trigger_next(
            Arc::clone(&extractor_for_next),
            Arc::clone(&player_for_next),
            smtc_for_next.clone(),
            ui_weak_next.clone(),
            queue_for_next.clone(),
            Arc::clone(&is_advancing_next),
        );
    });

    // Callback: Stop
    let player_for_stop = Arc::clone(&player);
    let smtc_for_stop = smtc.clone();
    let ui_weak_stop = app.as_weak();
    let queue_for_stop = play_queue.clone();
    app.on_stop(move || {
        player_for_stop.stop();
        queue_for_stop.clear();
        if let Some(smtc_arc) = &smtc_for_stop {
            if let Ok(mut lock) = smtc_arc.lock() {
                lock.0.set_playback(false);
            }
        }
        if let Some(ui) = ui_weak_stop.upgrade() {
            ui.set_is_playing(false);
            ui.set_track_status(slint::SharedString::from("STOPPED"));
            ui.set_has_active_track(false);
            ui.set_progress_ratio(0.0);
            ui.set_buffer_ratio(0.0);
            ui.set_time_display(slint::SharedString::from("00:00 / 00:00"));
        }
    });

    // Callback: User Scrubbing (while dragging - pure visual time preview, 0 audio overhead)
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
            ui.set_time_display(slint::SharedString::from(format!(
                "{:02}:{:02} / {:02}:{:02}",
                cur_m, cur_s, tot_m, tot_s
            )));
        }
    });

    // Callback: User Seek (only fires on mouse release!)
    let player_for_seek = Arc::clone(&player);
    let ui_weak_seek = app.as_weak();
    let smtc_for_seek = smtc.clone();
    app.on_on_user_seek(move |ratio| {
        let total_dur = player_for_seek.total_duration();
        let target_secs = ratio * total_dur;
        info!("Executing user seek to {:.1}s ({:.1}%)", target_secs, ratio * 100.0);
        let _ = player_for_seek.seek_to(target_secs);
        if let Some(ui) = ui_weak_seek.upgrade() {
            ui.set_is_playing(true);
            ui.set_track_status(slint::SharedString::from("PLAYING"));
        }
        if let Some(smtc_arc) = &smtc_for_seek {
            if let Ok(mut lock) = smtc_arc.lock() {
                lock.0.set_playback(true);
            }
        }
    });

    // Callback: User Volume
    let player_for_vol = Arc::clone(&player);
    app.on_on_user_volume(move |vol_ratio| {
        player_for_vol.set_volume(vol_ratio);
    });

    // --- Background Polling Timer (100ms) for Tray, SMTC, Autoplay, and Progress Tracking ---
    let timer = slint::Timer::default();
    let tray_clone = tray.clone();
    let smtc_clone = smtc.clone();
    let player_for_timer = Arc::clone(&player);
    let extractor_for_timer = Arc::clone(&extractor);
    let queue_for_timer = play_queue.clone();
    let ui_weak_timer = app.as_weak();
    let hwnd_for_timer = Arc::clone(&main_hwnd);
    let is_auto_advancing_timer = Arc::clone(&is_auto_advancing);

    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        move || {
            // Ensure HWND is subclassed if not caught on startup
            {
                let mut guard = hwnd_for_timer.lock().unwrap();
                if guard.is_none() {
                    unsafe {
                        let hwnd = FindWindowW(None, w!("YouTube Audio Player"));
                        if hwnd.0 != 0 {
                            let _ = SetWindowSubclass(hwnd, Some(window_subclass_proc), 1001, 0);
                            info!("Late-attached Win32 minimize-to-tray subclass to HWND: {:?}", hwnd);
                            *guard = Some(hwnd);
                        }
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
                                let _ = ShowWindow(hwnd, SW_SHOW);
                                let _ = ShowWindow(hwnd, SW_RESTORE);
                                let _ = SetForegroundWindow(hwnd);
                            }
                        }
                        if let Some(ui) = ui_weak_timer.upgrade() {
                            let _ = ui.show();
                        }
                    } else if menu_event.id == t.play_pause_id {
                        let is_paused = player_for_timer.is_paused();
                        if is_paused {
                            player_for_timer.resume();
                        } else {
                            player_for_timer.pause();
                        }
                        if let Some(ui) = ui_weak_timer.upgrade() {
                            ui.set_is_playing(is_paused);
                            ui.set_track_status(if is_paused {
                                slint::SharedString::from("PLAYING")
                            } else {
                                slint::SharedString::from("PAUSED")
                            });
                        }
                    }
                }
            }

            // 2. Process Tray Icon Double-Click (Directly Restore window from tray)
            while let Ok(tray_event) = TrayIconEvent::receiver().try_recv() {
                match tray_event {
                    TrayIconEvent::DoubleClick { .. } => {
                        info!("Tray double-clicked: Restoring main window.");
                        if let Some(hwnd) = *hwnd_for_timer.lock().unwrap() {
                            unsafe {
                                let _ = ShowWindow(hwnd, SW_SHOW);
                                let _ = ShowWindow(hwnd, SW_RESTORE);
                                let _ = SetForegroundWindow(hwnd);
                            }
                        }
                        if let Some(ui) = ui_weak_timer.upgrade() {
                            let _ = ui.show();
                        }
                    }
                    _ => {}
                }
            }

            // 3. Process Windows SMTC Media Key Events
            if let Some(smtc_arc) = &smtc_clone {
                if let Ok(mut lock) = smtc_arc.try_lock() {
                    while let Ok(event) = lock.1.try_recv() {
                        match event {
                            MediaControlEvent::Play => {
                                player_for_timer.resume();
                                lock.0.set_playback(true);
                                if let Some(ui) = ui_weak_timer.upgrade() {
                                    ui.set_is_playing(true);
                                    ui.set_track_status(slint::SharedString::from("PLAYING"));
                                }
                            }
                            MediaControlEvent::Pause => {
                                player_for_timer.pause();
                                lock.0.set_playback(false);
                                if let Some(ui) = ui_weak_timer.upgrade() {
                                    ui.set_is_playing(false);
                                    ui.set_track_status(slint::SharedString::from("PAUSED"));
                                }
                            }
                            MediaControlEvent::Toggle => {
                                let is_paused = player_for_timer.is_paused();
                                if is_paused {
                                    player_for_timer.resume();
                                } else {
                                    player_for_timer.pause();
                                }
                                lock.0.set_playback(is_paused);
                                if let Some(ui) = ui_weak_timer.upgrade() {
                                    ui.set_is_playing(is_paused);
                                    ui.set_track_status(if is_paused {
                                        slint::SharedString::from("PLAYING")
                                    } else {
                                        slint::SharedString::from("PAUSED")
                                    });
                                }
                            }
                            MediaControlEvent::Next => {
                                trigger_next(
                                    Arc::clone(&extractor_for_timer),
                                    Arc::clone(&player_for_timer),
                                    smtc_clone.clone(),
                                    ui_weak_timer.clone(),
                                    queue_for_timer.clone(),
                                    Arc::clone(&is_auto_advancing_timer),
                                );
                            }
                            MediaControlEvent::Previous => {
                                trigger_previous(
                                    Arc::clone(&extractor_for_timer),
                                    Arc::clone(&player_for_timer),
                                    smtc_clone.clone(),
                                    ui_weak_timer.clone(),
                                    queue_for_timer.clone(),
                                    Arc::clone(&is_auto_advancing_timer),
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
            }

            // 4. Update Playback Progress Bar (Only when user is NOT dragging)
            if !player_for_timer.is_paused() {
                let current_pos = player_for_timer.current_position();
                let total_dur = player_for_timer.total_duration();
                if total_dur > 0.0 {
                    if let Some(ui) = ui_weak_timer.upgrade() {
                        if !ui.get_is_scrubbing() {
                            let ratio = (current_pos / total_dur).clamp(0.0, 1.0);
                            ui.set_progress_ratio(ratio);

                            let cur_m = (current_pos as u64) / 60;
                            let cur_s = (current_pos as u64) % 60;
                            let tot_m = (total_dur as u64) / 60;
                            let tot_s = (total_dur as u64) % 60;
                            ui.set_time_display(slint::SharedString::from(format!(
                                "{:02}:{:02} / {:02}:{:02}",
                                cur_m, cur_s, tot_m, tot_s
                            )));
                        }

                        let buf_ratio = player_for_timer.buffer_ratio();
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
                        Arc::clone(&is_auto_advancing_timer),
                    );
                }
            }
        },
    );

    // Run Slint Event Loop
    app.run()?;

    // Clean up upon exit
    player.stop();
    Ok(())
}
