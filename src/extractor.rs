use anyhow::{Context, Result, anyhow};
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{info, warn};

const AUDIO_FORMAT_SELECTOR: &str = "bestaudio[ext=m4a]/bestaudio";

pub fn shared_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(15))
            .pool_max_idle_per_host(8)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioMetadata {
    pub id: String,
    pub title: String,
    pub uploader: Option<String>,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
    pub stream_url: String,
    pub audio_codec: Option<String>,
    #[serde(default)]
    pub start_offset_secs: f32,
    #[serde(default)]
    pub http_headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub loudness_db: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct PlaylistItem {
    pub id: String,
    pub title: String,
    pub uploader: Option<String>,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub title: String,
    pub uploader: Option<String>,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
}

#[derive(Deserialize)]
struct FlatPlaylist {
    entries: Option<Vec<FlatPlaylistEntry>>,
}

#[derive(Deserialize)]
struct FlatPlaylistEntry {
    id: Option<String>,
    title: Option<String>,
    uploader: Option<String>,
    channel: Option<String>,
    duration: Option<f64>,
    thumbnail: Option<String>,
}

impl FlatPlaylistEntry {
    fn into_search_result(self) -> Option<SearchResult> {
        Some(SearchResult {
            id: self.id?,
            title: self.title.unwrap_or_else(|| "Unknown title".into()),
            uploader: self.uploader.or(self.channel),
            duration: self.duration,
            thumbnail: self.thumbnail,
        })
    }

    fn into_playlist_item(self) -> Option<PlaylistItem> {
        let result = self.into_search_result()?;
        Some(PlaylistItem {
            id: result.id,
            title: result.title,
            uploader: result.uploader,
            duration: result.duration,
            thumbnail: result.thumbnail,
        })
    }
}

fn parse_search_results(json: &[u8]) -> Result<Vec<SearchResult>> {
    let playlist: FlatPlaylist =
        serde_json::from_slice(json).context("Failed to parse YouTube search results")?;
    Ok(playlist
        .entries
        .unwrap_or_default()
        .into_iter()
        .filter_map(FlatPlaylistEntry::into_search_result)
        .collect())
}

#[derive(Deserialize)]
struct YtDlpOutput {
    id: String,
    title: String,
    uploader: Option<String>,
    channel: Option<String>,
    duration: Option<f64>,
    thumbnail: Option<String>,
    url: String,
    acodec: Option<String>,
    http_headers: Option<HashMap<String, String>>,
    #[serde(default)]
    loudness_db: Option<f32>,
}

pub struct Extractor {
    yt_dlp_path: PathBuf,
}

pub(crate) fn silent_command<S: AsRef<std::ffi::OsStr>>(program: S) -> Command {
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

pub(crate) fn async_silent_command<S: AsRef<std::ffi::OsStr>>(
    program: S,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.stdin(std::process::Stdio::null());
    cmd.kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

pub(crate) fn extract_stream_url_sync(
    yt_dlp_path: &std::path::Path,
    query: &str,
) -> Result<(String, Option<HashMap<String, String>>)> {
    let is_url =
        query.starts_with("http://") || query.starts_with("https://") || query.starts_with("www.");
    let target = if is_url {
        query.to_string()
    } else {
        format!("ytsearch1:{}", query)
    };

    let output = silent_command(yt_dlp_path)
        .args([
            "--no-warnings",
            "--no-playlist",
            "-f",
            AUDIO_FORMAT_SELECTOR,
            "-j",
            &target,
        ])
        .output()
        .context("Failed to execute yt-dlp to refresh stream URL")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("yt-dlp stream refresh failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_json_line = stdout
        .lines()
        .find(|line| line.starts_with('{') && line.ends_with('}'))
        .ok_or_else(|| anyhow!("yt-dlp returned no valid JSON during stream refresh"))?;

    let parsed: YtDlpOutput = serde_json::from_str(first_json_line)
        .context("Failed to parse yt-dlp JSON during stream refresh")?;

    Ok((parsed.url, parsed.http_headers))
}

impl Extractor {
    pub async fn new() -> Result<Self> {
        let yt_dlp_path = Self::ensure_yt_dlp().await?;
        let extractor = Self { yt_dlp_path };
        extractor.maybe_auto_update_in_background();
        Ok(extractor)
    }

    pub fn yt_dlp_path(&self) -> &std::path::Path {
        &self.yt_dlp_path
    }

    /// Background non-blocking check to keep yt-dlp up-to-date against YouTube signature changes.
    fn maybe_auto_update_in_background(&self) {
        let path = self.yt_dlp_path.clone();
        let should_update = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| mtime.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(7 * 24 * 3600));
        if should_update {
            tokio::spawn(async move {
                info!("Running background yt-dlp self-update check (-U)...");
                let _ = async_silent_command(&path).arg("-U").output().await;
            });
        }
    }

    pub fn trigger_self_update(&self) {
        let path = self.yt_dlp_path.clone();
        tokio::spawn(async move {
            info!("Triggering yt-dlp self-update (-U) after extraction failure...");
            let _ = async_silent_command(&path).arg("-U").output().await;
        });
    }

    /// Search YouTube and return selectable metadata without starting playback.
    pub async fn search_tracks(&self, query: &str) -> Result<Vec<SearchResult>> {
        let target = format!("ytsearch10:{}", query);
        let output = async_silent_command(&self.yt_dlp_path)
            .args([
                "--no-warnings",
                "--flat-playlist",
                "--dump-single-json",
                &target,
            ])
            .output()
            .await
            .context("Failed to execute YouTube search")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("YouTube search failed: {}", stderr.trim()));
        }

        parse_search_results(&output.stdout)
    }

    pub fn start_offset_secs(query: &str) -> Option<f32> {
        let query = query.split_once('?')?.1.split('#').next()?;
        query.split('&').find_map(|part| {
            let (key, value) = part.split_once('=')?;
            if key != "t" && key != "start" && key != "time_continue" {
                return None;
            }
            parse_timestamp(value).map(|seconds| seconds.max(0.0) as f32)
        })
    }

    /// Check if yt-dlp is available in PATH or project bin dir; if not, download it.
    async fn ensure_yt_dlp() -> Result<PathBuf> {
        if let Ok(output) = silent_command("where.exe").arg("yt-dlp").output()
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = stdout.lines().next() {
                let path = PathBuf::from(first_line.trim());
                if path.exists() {
                    info!("Found yt-dlp in PATH: {:?}", path);
                    return Ok(path);
                }
            }
        }

        if let Ok(exe_path) = std::env::current_exe()
            && let Some(exe_dir) = exe_path.parent()
        {
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

        let local_bin = PathBuf::from("bin").join("yt-dlp.exe");
        if local_bin.exists() {
            info!("Found local yt-dlp at: {:?}", local_bin);
            return Ok(local_bin);
        }

        info!("yt-dlp not found. Downloading latest yt-dlp.exe from GitHub releases...");
        std::fs::create_dir_all("bin").context("Failed to create bin directory for yt-dlp.exe")?;

        let download_url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
        let response = shared_http_client()
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

    /// Extract audio stream URL and video metadata with kill_on_drop cancellation and exponential backoff retry.
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

        let mut last_err = anyhow!("Extraction failed");
        for attempt in 0..2 {
            let output = async_silent_command(&self.yt_dlp_path)
                .args([
                    "--no-warnings",
                    "--no-playlist",
                    "-f",
                    AUDIO_FORMAT_SELECTOR,
                    "-j",
                    &target,
                ])
                .output()
                .await
                .context("Failed to execute yt-dlp process")?;

            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let first_json_line = stdout
                    .lines()
                    .find(|line| line.starts_with('{') && line.ends_with('}'))
                    .ok_or_else(|| {
                        anyhow!("yt-dlp returned no valid JSON line. Stdout: {}", stdout)
                    })?;

                let parsed: YtDlpOutput = serde_json::from_str(first_json_line)
                    .context("Failed to parse yt-dlp JSON output")?;

                return Ok(AudioMetadata {
                    id: parsed.id,
                    title: parsed.title,
                    uploader: parsed.uploader.or(parsed.channel),
                    duration: parsed.duration,
                    thumbnail: parsed.thumbnail,
                    stream_url: parsed.url,
                    audio_codec: parsed.acodec.map(|codec| {
                        if codec.starts_with("mp4a") {
                            "AAC".to_string()
                        } else if codec.contains("opus") {
                            "OPUS".to_string()
                        } else {
                            codec.to_uppercase()
                        }
                    }),
                    start_offset_secs: Self::start_offset_secs(query).unwrap_or(0.0),
                    http_headers: parsed.http_headers,
                    loudness_db: parsed.loudness_db,
                });
            }

            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if stderr.contains("403") || stderr.contains("Sign in") || stderr.contains("nsig") {
                self.trigger_self_update();
            }
            // Do not retry permanent unavailability errors (private, deleted, members-only)
            if stderr.contains("Private video")
                || stderr.contains("Video unavailable")
                || stderr.contains("members-only")
                || stderr.contains("has been removed")
            {
                return Err(anyhow!("Track unavailable: {}", stderr));
            }
            last_err = anyhow!("yt-dlp failed: {}", stderr);
            if attempt == 0 {
                warn!("Transient extraction failure (attempt 1), retrying after 600ms: {}", stderr);
                tokio::time::sleep(Duration::from_millis(600)).await;
            }
        }

        Err(last_err)
    }

    /// Extract video entries from a YouTube playlist URL (cancellable via kill_on_drop)
    pub async fn extract_playlist_items(&self, playlist_url: &str) -> Result<Vec<PlaylistItem>> {
        let output = async_silent_command(&self.yt_dlp_path)
            .args([
                "--no-warnings",
                "--flat-playlist",
                "--dump-single-json",
                playlist_url,
            ])
            .output()
            .await
            .context("Failed to execute yt-dlp playlist process")?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let playlist: FlatPlaylist = serde_json::from_slice(&output.stdout)
            .context("Failed to parse yt-dlp playlist output")?;
        Ok(playlist
            .entries
            .unwrap_or_default()
            .into_iter()
            .filter_map(FlatPlaylistEntry::into_playlist_item)
            .collect())
    }

    /// Extract next related tracks with FULL metadata (title, artist, duration, thumbnail)
    /// for continuous autoplay (YouTube Radio Mix)
    pub async fn extract_autoplay_next(&self, current_video_id: &str) -> Result<Vec<PlaylistItem>> {
        let mix_url = format!(
            "https://www.youtube.com/watch?v={}&list=RD{}",
            current_video_id, current_video_id
        );

        let output = async_silent_command(&self.yt_dlp_path)
            .args([
                "--no-warnings",
                "--flat-playlist",
                "--playlist-items",
                "2:6",
                "--dump-single-json",
                &mix_url,
            ])
            .output()
            .await
            .context("Failed to execute yt-dlp autoplay process")?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let playlist: FlatPlaylist = serde_json::from_slice(&output.stdout)
            .context("Failed to parse yt-dlp autoplay output")?;
        Ok(playlist
            .entries
            .unwrap_or_default()
            .into_iter()
            .filter_map(FlatPlaylistEntry::into_playlist_item)
            .filter(|item| item.id != current_video_id)
            .collect())
    }
}

fn parse_timestamp(value: &str) -> Option<f64> {
    let value = value.replace("%3A", ":").replace("%3a", ":");
    if value.contains(':') {
        let parts: Vec<f64> = value
            .split(':')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        if parts.is_empty() || parts.len() > 3 {
            return None;
        }
        return Some(
            parts
                .into_iter()
                .fold(0.0, |seconds, part| seconds * 60.0 + part),
        );
    }

    let mut total = 0.0;
    let mut digits = String::new();
    let mut found_unit = false;
    for character in value.chars() {
        if character.is_ascii_digit() || character == '.' {
            digits.push(character);
            continue;
        }
        let multiplier = match character {
            'h' => 3600.0,
            'm' => 60.0,
            's' => 1.0,
            _ => return None,
        };
        total += digits.parse::<f64>().ok()? * multiplier;
        digits.clear();
        found_unit = true;
    }
    if found_unit {
        if !digits.is_empty() {
            total += digits.parse::<f64>().ok()?;
        }
        Some(total)
    } else {
        value.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::parse_search_results;

    #[test]
    fn search_results_keep_selection_metadata_and_skip_entries_without_ids() {
        let results = parse_search_results(
            br#"{
                "entries": [
                    {
                        "id": "video-1",
                        "title": "Song One",
                        "channel": "Artist One",
                        "duration": 187,
                        "thumbnail": "https://example.invalid/cover.jpg"
                    },
                    {"title": "Unavailable entry"}
                ]
            }"#,
        )
        .expect("parse search metadata");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "video-1");
        assert_eq!(results[0].title, "Song One");
        assert_eq!(results[0].uploader.as_deref(), Some("Artist One"));
        assert_eq!(results[0].duration, Some(187.0));
        assert_eq!(
            results[0].thumbnail.as_deref(),
            Some("https://example.invalid/cover.jpg")
        );
    }
}