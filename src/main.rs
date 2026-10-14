#![windows_subsystem = "windows"]

mod app_timer;
mod buffer;
mod cli;
mod extractor;
mod library;
mod playback_coordinator;
mod player;
mod queue;
mod smtc;
mod stream_reader;
mod tray;
mod ui_callbacks;
mod ui_models;
mod win32_window;

slint::include_modules!();

use anyhow::Result;
use cli::run_cli_mode;
use extractor::Extractor;
use library::{LibraryStore, TrackRecord};
use player::AudioPlayer;
use queue::PlayQueue;
use smtc::SmtcManager;
use std::sync::{Arc, Mutex};
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;
use tray::SystemTray;
use ui_callbacks::{register_ui_callbacks, PendingSeekState};
use ui_models::{queue_items_model, records_model};
use win32_window::{acquire_single_instance, setup_windows_app_identity, try_find_and_hook};
use windows::Win32::Foundation::HWND;

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
    let _single_instance_guard = match acquire_single_instance() {
        Some(guard) => guard,
        None => {
            info!("Single instance already active; brought existing window to front. Exiting.");
            return Ok(());
        }
    };

    #[cfg(target_os = "windows")]
    setup_windows_app_identity();

    info!("Starting YouTube Audio Player in Morandi GUI mode...");

    let app = AppWindow::new()?;
    // Initialize yt-dlp first so the player uses the exact binary selected by Extractor.
    let extractor = Arc::new(Extractor::new().await?);
    let (stream, stream_handle) = rodio::OutputStream::try_default().map_err(|e| {
        anyhow::anyhow!("Failed to open default Windows audio output device: {}", e)
    })?;
    let player = Arc::new(AudioPlayer::new(
        &stream_handle,
        extractor.yt_dlp_path().to_path_buf(),
    )?);

    // Continuous Playback & Playlist Queue
    let play_queue = PlayQueue::new();
    let library = LibraryStore::load();
    play_queue.set_repeat_mode(library.repeat_mode());
    play_queue.set_shuffle_enabled(library.shuffle_enabled());
    app.set_repeat_mode_label(slint::SharedString::from(library.repeat_mode().label()));
    app.set_shuffle_enabled(library.shuffle_enabled());
    app.set_favorite_items(records_model(library.favorites(), &library));
    app.set_history_items(records_model(library.recent(), &library));
    app.set_queue_items(queue_items_model(&play_queue, &library));

    // Initialize System Tray (kept on main thread event loop)
    let tray = match SystemTray::new() {
        Ok(t) => {
            info!("System Tray icon initialized successfully.");
            #[allow(clippy::arc_with_non_send_sync)]
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
        for _ in 0..20 {
            if let Some(hwnd) = try_find_and_hook() {
                info!(
                    "Attached Win32 minimize-to-tray subclass to HWND: {:?}",
                    hwnd
                );
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
            )
            .as_bool()
            {
                let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
                windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    // Initialize Windows SMTC (System Media Transport Controls)
    let initial_hwnd = *main_hwnd.lock().unwrap();
    let smtc: playback_coordinator::SmtcHandle = match SmtcManager::new(initial_hwnd) {
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

    // Shared state coordinators
    let is_auto_advancing = Arc::new(Mutex::new(false));
    let current_loading_abort: playback_coordinator::LoadingAbortHandle =
        Arc::new(Mutex::new(None));
    let last_query = Arc::new(Mutex::new(None::<String>));
    let pending_seek = Arc::new(Mutex::new(Option::<PendingSeekState>::None));
    let search_records = Arc::new(Mutex::new(Vec::<TrackRecord>::new()));

    // Register all Slint GUI callbacks
    register_ui_callbacks(
        &app,
        Arc::clone(&player),
        Arc::clone(&extractor),
        smtc.clone(),
        play_queue.clone(),
        library.clone(),
        Arc::clone(&is_auto_advancing),
        Arc::clone(&current_loading_abort),
        Arc::clone(&last_query),
        Arc::clone(&pending_seek),
        Arc::clone(&search_records),
    );

    // Start background polling timer
    let _timer = app_timer::start_playback_polling_timer(
        &app,
        Arc::clone(&player),
        Arc::clone(&extractor),
        play_queue,
        library,
        tray,
        smtc,
        Arc::clone(&main_hwnd),
        Arc::clone(&is_auto_advancing),
        Arc::clone(&last_query),
        Arc::clone(&current_loading_abort),
        Arc::clone(&pending_seek),
        stream,
    );

    // Run Slint Event Loop
    app.run()?;

    // Clean up upon exit
    player.stop();
    Ok(())
}
