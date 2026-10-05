use anyhow::Result;
use crate::extractor::Extractor;
use crate::player::AudioPlayer;
use tracing::error;

/// Fallback / Headless CLI mode for automated tests, scripts, and terminal usage
pub async fn run_cli_mode(raw_args: Vec<String>, is_smoke_test: bool) -> Result<()> {
    if raw_args.iter().any(|a| a == "--inspect-smtc") {
        #[cfg(target_os = "windows")]
        {
            use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;
            if let Ok(op) = GlobalSystemMediaTransportControlsSessionManager::RequestAsync() {
                if let Ok(mgr) = op.get() {
                    if let Ok(s) = mgr.GetCurrentSession() {
                        if let Ok(app_id) = s.SourceAppUserModelId() {
                            println!("Current Active SMTC Session SourceAppUserModelId: {}", app_id);
                            if let Ok(info_op) = s.TryGetMediaPropertiesAsync() {
                                if let Ok(props) = info_op.get() {
                                    let title = props.Title().map(|t| t.to_string()).unwrap_or_default();
                                    let artist = props.Artist().map(|a| a.to_string()).unwrap_or_default();
                                    println!(" -> Title: '{}', Artist: '{}'", title, artist);
                                }
                            }
                        }
                    } else {
                        println!("No active SMTC session currently playing.");
                    }
                }
            }
        }
        return Ok(());
    }

    println!("============================================================");
    println!("     Rust YouTube Pure-Audio Player (CLI / Smoke Mode)      ");
    println!("============================================================\n");

    let args: Vec<String> = raw_args
        .into_iter()
        .filter(|a| a != "--smoke-test" && a != "--cli" && a != "--inspect-smtc")
        .collect();
    let query = if !args.is_empty() {
        args.join(" ")
    } else {
        "Rick Astley Never Gonna Give You Up".to_string()
    };

    println!("Target Query / URL: \"{}\"", query);
    println!("Checking yt-dlp extractor...");
    let extractor = Extractor::new().await?;

    println!("Extracting audio stream...");
    let meta = extractor.extract_audio(&query).await?;
    let duration_secs = meta.duration.unwrap_or(0.0) as f32;

    let m = (duration_secs as u64) / 60;
    let s = (duration_secs as u64) % 60;
    let duration_str = format!("{:02}:{:02}", m, s);

    println!("\n------------------ Stream Metadata ------------------");
    println!("Title:    {}", meta.title);
    println!("Uploader: {}", meta.uploader.as_deref().unwrap_or("Unknown"));
    println!("Duration: {}", duration_str);
    println!("Codec:    {}", meta.audio_codec.as_deref().unwrap_or("Unknown"));
    println!("Stream:   {}...", &meta.stream_url[..meta.stream_url.len().min(80)]);
    println!("-----------------------------------------------------\n");

    println!("Initializing audio engine (WASAPI)...");
    let (_stream, stream_handle) = rodio::OutputStream::try_default()
        .map_err(|e| anyhow::anyhow!("Failed to open default Windows audio output device: {}", e))?;
    let player = AudioPlayer::new(&stream_handle)?;

    println!("Streaming audio on-demand via ffmpeg PCM pipe...");
    let player_clone = player.clone();
    let stream_url = meta.stream_url.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = player_clone.play_stream(&stream_url, duration_secs) {
            error!("Audio playback error: {:#}", e);
        }
    });

    // Attach SMTC in CLI / Smoke mode for end-to-end SMTC verification
    #[cfg(target_os = "windows")]
    let mut smtc = crate::smtc::SmtcManager::new(None).ok();
    #[cfg(target_os = "windows")]
    if let Some(s) = &mut smtc {
        s.set_metadata(
            &meta.title,
            meta.uploader.as_deref().unwrap_or("Unknown Artist"),
            if duration_secs > 0.0 {
                Some(std::time::Duration::from_secs_f32(duration_secs))
            } else {
                None
            },
        );
        s.set_playback(true);
        println!("Windows SMTC active: Metadata and playback set to Playing.");
    }

    if is_smoke_test {
        println!("Smoke test active: Playing audio for 6 seconds...");
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        #[cfg(target_os = "windows")]
        {
            use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;
            if let Ok(op) = GlobalSystemMediaTransportControlsSessionManager::RequestAsync() {
                if let Ok(mgr) = op.get() {
                    if let Ok(s) = mgr.GetCurrentSession() {
                        if let Ok(app_id) = s.SourceAppUserModelId() {
                            println!("Empirical SMTC Verification: Active Session SourceAppUserModelId: {}", app_id);
                            if let Ok(info_op) = s.TryGetMediaPropertiesAsync() {
                                if let Ok(props) = info_op.get() {
                                    let title = props.Title().map(|t| t.to_string()).unwrap_or_default();
                                    let artist = props.Artist().map(|a| a.to_string()).unwrap_or_default();
                                    println!("Empirical SMTC Verification: Title='{}', Artist='{}'", title, artist);
                                }
                            }
                        }
                    }
                }
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
        println!("Smoke test: Testing pause...");
        player.pause();
        #[cfg(target_os = "windows")]
        if let Some(s) = &mut smtc {
            s.set_playback(false);
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        println!("Smoke test: Testing resume...");
        player.resume();
        #[cfg(target_os = "windows")]
        if let Some(s) = &mut smtc {
            s.set_playback(true);
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        println!("Smoke test: Testing seek to 30s...");
        let _ = player.seek_to(30.0);
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        player.stop();
        #[cfg(target_os = "windows")]
        if let Some(s) = &mut smtc {
            s.set_playback(false);
        }
        println!("Smoke test completed successfully!");
        return Ok(());
    }

    Ok(())
}
