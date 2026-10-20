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
use library::{LibraryStore, TrackRecord, format_duration_secs};
use player::AudioPlayer;
use queue::PlayQueue;
use slint::ComponentHandle;
use smtc::SmtcManager;
use std::sync::{Arc, Mutex};
use tracing::{Level, error, info};
use tracing_subscriber::FmtSubscriber;
use tray::SystemTray;
use ui_callbacks::{PendingSeekState, SleepTimerMode, register_ui_callbacks};
use ui_models::{
    apply_queue_expanded, load_cached_cover_image, queue_items_model, records_model,
};
use win32_window::{acquire_single_instance, setup_windows_app_identity};
use windows::Win32::Foundation::HWND;

#[tokio::main]
async fn main() -> Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let is_smoke_test = raw_args.iter().any(|a| a == "--smoke-test");
    let is_cli = raw_args.iter().any(|a| a == "--cli");

    if is_smoke_test || is_cli {
        return run_cli_mode(raw_args, is_smoke_test).await;
    }

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

    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

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

    let app = AppWindow::new()?;
    let extractor = Arc::new(Extractor::new().await?);
    let (stream, stream_handle) = rodio::OutputStream::try_default().map_err(|e| {
        anyhow::anyhow!("Failed to open default Windows audio output device: {}", e)
    })?;
    let player = Arc::new(AudioPlayer::new(
        &stream_handle,
        extractor.yt_dlp_path().to_path_buf(),
    )?);

    // Load persisted Library & Session state
    let play_queue = PlayQueue::new();
    let library = LibraryStore::load();
    library.prune_cache_async(300);

    play_queue.set_repeat_mode(library.repeat_mode());
    play_queue.set_shuffle_enabled(library.shuffle_enabled());
    play_queue.set_autoplay_enabled(library.autoplay_enabled());

    // Restore volume and mute state
    let initial_vol = library.volume();
    let initial_muted = library.is_muted();
    player.set_volume(if initial_muted { 0.0 } else { initial_vol });
    app.set_volume_ratio(if initial_muted { 0.0 } else { initial_vol });
    app.set_is_muted(initial_muted);

    app.set_repeat_mode_label(slint::SharedString::from(library.repeat_mode().label()));
    app.set_shuffle_enabled(library.shuffle_enabled());
    app.set_autoplay_enabled(library.autoplay_enabled());
    app.set_pinned_on_top(library.always_on_top());

    // Restore last session track, position, and upcoming queue
    let (last_track_opt, last_pos_secs, saved_queue) = library.last_session();
    play_queue.restore_saved_queue(saved_queue);

    let last_query = Arc::new(Mutex::new(None::<String>));
    if let Some(last_track) = &last_track_opt {
        let track_url = last_track.video_url();
        *last_query.lock().unwrap() = Some(track_url.clone());
        play_queue.record_current(track_url.clone());
        app.set_track_title(slint::SharedString::from(last_track.title.clone()));
        app.set_track_artist(slint::SharedString::from(last_track.artist.clone()));
        app.set_current_track_key(slint::SharedString::from(track_url));
        app.set_current_is_favorite(library.is_favorite(&last_track.id));
        if let Some(dur) = last_track.duration_secs.filter(|&d| d > 0.0) {
            let pos = (last_pos_secs as f64).min(dur);
            let cur_str = format_duration_secs(pos);
            let tot_str = format_duration_secs(dur);
            app.set_progress_ratio((pos / dur) as f32);
            app.set_time_current(slint::SharedString::from(cur_str.clone()));
            app.set_time_total(slint::SharedString::from(tot_str.clone()));
            app.set_time_display(slint::SharedString::from(format!("{} / {}", cur_str, tot_str)));
        }
        if let Some(img) = load_cached_cover_image(&library, &last_track.id) {
            app.set_cover_image(img);
            app.set_has_cover(true);
        }
    }

    let current_key = play_queue.current_query();
    app.set_favorite_items(records_model(
        library.favorites(),
        &library,
        current_key.as_deref(),
    ));
    app.set_history_items(records_model(
        library.recent(),
        &library,
        current_key.as_deref(),
    ));
    app.set_queue_items(queue_items_model(&play_queue, &library));

    if library.queue_expanded() {
        apply_queue_expanded(&app, true);
    }

    app.show()?;

    use i_slint_backend_winit::WinitWindowAccessor;
    let initial_hwnd: Option<HWND> = app
        .window()
        .with_winit_window(|winit_window| {
            use raw_window_handle::HasWindowHandle;
            if let Ok(handle) = winit_window.window_handle()
                && let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw()
            {
                return Some(HWND(h.hwnd.get() as _));
            }
            None
        })
        .flatten();

    info!(
        "Directly obtained Slint HWND via WinitWindowAccessor: {:?}",
        initial_hwnd
    );
    if let Some(hwnd) = initial_hwnd {
        unsafe {
            win32_window::attach_window_hooks(hwnd);
        }
        if library.always_on_top() {
            win32_window::set_always_on_top(hwnd, true);
        }
        if let Some((x, y)) = library.window_pos() {
            win32_window::restore_window_position(hwnd, x, y);
        }
    }

    let main_hwnd: Arc<Mutex<Option<HWND>>> = Arc::new(Mutex::new(initial_hwnd));

    let initial_hwnd = *main_hwnd.lock().unwrap();
    let (smtc, smtc_rx) = match SmtcManager::new(initial_hwnd) {
        Ok((manager, rx)) => {
            info!("Windows SMTC successfully attached.");
            (Some(Arc::new(Mutex::new(manager))), Some(rx))
        }
        Err(e) => {
            info!("SMTC initialization note (non-critical): {:#}", e);
            (None, None)
        }
    };

    let is_auto_advancing = Arc::new(Mutex::new(false));
    let current_loading_abort: playback_coordinator::LoadingAbortHandle =
        Arc::new(Mutex::new(None));
    let pending_seek = Arc::new(Mutex::new(Option::<PendingSeekState>::None));
    let search_records = Arc::new(Mutex::new(Vec::<TrackRecord>::new()));
    let seek_seq = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let seek_lock = Arc::new(tokio::sync::Mutex::new(()));
    let sleep_timer = Arc::new(Mutex::new(SleepTimerMode::Off));

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
        Arc::clone(&main_hwnd),
        Arc::clone(&seek_seq),
        Arc::clone(&seek_lock),
        Arc::clone(&sleep_timer),
    );

    let _timer = app_timer::start_playback_polling_timer(
        &app,
        Arc::clone(&player),
        Arc::clone(&extractor),
        play_queue.clone(),
        library.clone(),
        tray,
        smtc,
        smtc_rx,
        Arc::clone(&main_hwnd),
        Arc::clone(&is_auto_advancing),
        Arc::clone(&last_query),
        Arc::clone(&current_loading_abort),
        Arc::clone(&pending_seek),
        stream,
        seek_seq,
        seek_lock,
        sleep_timer,
    );

    app.run()?;

    if player.has_track() {
        library.update_playback_position(player.current_position());
    }
    library.save_queue(play_queue.to_track_records());
    player.stop();
    Ok(())
}