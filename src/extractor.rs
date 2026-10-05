use anyhow::{anyhow, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioMetadata {
    pub id: String,
    pub title: String,
    pub uploader: Option<String>,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
    pub stream_url: String,
    pub audio_codec: Option<String>,
}

#[derive(Deserialize)]
struct YtDlpOutput {
    id: String,
    title: String,
    uploader: Option<String>,
    duration: Option<f64>,
    thumbnail: Option<String>,
    url: String,
    acodec: Option<String>,
}

pub struct Extractor {
    yt_dlp_path: PathBuf,
}

fn silent_command<S: AsRef<std::ffi::OsStr>>(program: S) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

impl Extractor {
    pub async fn new() -> Result<Self> {
        let yt_dlp_path = Self::ensure_yt_dlp().await?;
        Ok(Self { yt_dlp_path })
    }

    /// Check if yt-dlp is available in PATH or project bin dir; if not, download it.
    async fn ensure_yt_dlp() -> Result<PathBuf> {
        // 1. Check if yt-dlp is already in PATH
        if let Ok(output) = silent_command("where.exe").arg("yt-dlp").output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(first_line) = stdout.lines().next() {
                    let path = PathBuf::from(first_line.trim());
                    if path.exists() {
                        info!("Found yt-dlp in PATH: {:?}", path);
                        return Ok(path);
                    }
                }
            }
        }

        // 2. Check executable directory and local ./bin/yt-dlp.exe
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                let candidate1 = exe_dir.join("bin").join("yt-dlp.exe");
                if candidate1.exists() {
                    info!("Found yt-dlp near executable: {:?}", candidate1);
                    return Ok(candidate1);
                }
                let candidate2 = exe_dir.join("yt-dlp.exe");
                if candidate2.exists() {
                    info!("Found yt-dlp in executable directory: {:?}", candidate2);
                    return Ok(candidate2);
                }
            }
        }

        let local_bin = PathBuf::from("bin").join("yt-dlp.exe");
        if local_bin.exists() {
            info!("Found local yt-dlp at: {:?}", local_bin);
            return Ok(local_bin);
        }

        // 3. Download yt-dlp.exe from GitHub
        info!("yt-dlp not found. Downloading latest yt-dlp.exe from GitHub releases...");
        std::fs::create_dir_all("bin")
            .context("Failed to create bin directory for yt-dlp.exe")?;

        let download_url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
        let client = reqwest::Client::new();
        let response = client
            .get(download_url)
            .send()
            .await
            .context("Failed to connect to yt-dlp release URL")?;

        let total_size = response.content_length().unwrap_or(0);
        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                .unwrap()
                .progress_chars("#>-"),
        );

        use futures_util::StreamExt;
        let mut file = tokio::fs::File::create(&local_bin)
            .await
            .context("Failed to create local yt-dlp.exe file")?;
        let mut stream = response.bytes_stream();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.context("Error downloading chunk of yt-dlp")?;
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .context("Error writing chunk to yt-dlp.exe")?;
            pb.inc(chunk.len() as u64);
        }
        pb.finish_with_message("yt-dlp downloaded successfully");

        info!("Successfully downloaded yt-dlp to {:?}", local_bin);
        Ok(local_bin)
    }

    /// Extract audio stream URL and video metadata from a YouTube URL or search query.
    pub async fn extract_audio(&self, query: &str) -> Result<AudioMetadata> {
        let is_url = query.starts_with("http://")
            || query.starts_with("https://")
            || query.starts_with("www.");

        let target = if is_url {
            query.to_string()
        } else {
            format!("ytsearch1:{}", query)
        };

        info!("Extracting audio stream using yt-dlp for target: {}", target);

        let yt_dlp_path = self.yt_dlp_path.clone();
        let target_clone = target.clone();

        // Run yt-dlp in a blocking task so Tokio runtime isn't blocked
        let output = tokio::task::spawn_blocking(move || {
            silent_command(&yt_dlp_path)
                .args([
                    "--no-warnings",
                    "--no-playlist",
                    "-f",
                    "bestaudio",
                    "-j",
                    &target_clone,
                ])
                .output()
        })
        .await
        .context("Failed to spawn yt-dlp worker task")?
        .context("Failed to execute yt-dlp process")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("yt-dlp failed: {}", stderr.trim()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let first_json_line = stdout
            .lines()
            .find(|line| line.starts_with('{') && line.ends_with('}'))
            .ok_or_else(|| anyhow!("yt-dlp returned no valid JSON line. Stdout: {}", stdout))?;

        let parsed: YtDlpOutput = serde_json::from_str(first_json_line)
            .context("Failed to parse yt-dlp JSON output")?;

        Ok(AudioMetadata {
            id: parsed.id,
            title: parsed.title,
            uploader: parsed.uploader,
            duration: parsed.duration,
            thumbnail: parsed.thumbnail,
            stream_url: parsed.url,
            audio_codec: parsed.acodec,
        })
    }

    /// Extract video entries from a YouTube playlist URL (fast flat metadata scan)
    pub async fn extract_playlist_items(&self, playlist_url: &str) -> Result<Vec<String>> {
        let yt_dlp_path = self.yt_dlp_path.clone();
        let url_clone = playlist_url.to_string();

        let output = tokio::task::spawn_blocking(move || {
            silent_command(&yt_dlp_path)
                .args([
                    "--no-warnings",
                    "--flat-playlist",
                    "--print",
                    "%(id)s",
                    &url_clone,
                ])
                .output()
        })
        .await
        .context("Failed to spawn yt-dlp playlist task")?
        .context("Failed to execute yt-dlp playlist process")?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let ids: Vec<String> = stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        Ok(ids)
    }

    /// Extract next related video IDs for continuous autoplay (YouTube Radio Mix)
    pub async fn extract_autoplay_next(&self, current_video_id: &str) -> Result<Vec<String>> {
        let mix_url = format!("https://www.youtube.com/watch?v={}&list=RD{}", current_video_id, current_video_id);
        let yt_dlp_path = self.yt_dlp_path.clone();

        let output = tokio::task::spawn_blocking(move || {
            silent_command(&yt_dlp_path)
                .args([
                    "--no-warnings",
                    "--flat-playlist",
                    "--playlist-items",
                    "2:6",
                    "--print",
                    "%(id)s",
                    &mix_url,
                ])
                .output()
        })
        .await
        .context("Failed to spawn yt-dlp autoplay task")?
        .context("Failed to execute yt-dlp autoplay process")?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let ids: Vec<String> = stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && l != current_video_id)
            .collect();

        Ok(ids)
    }
}
