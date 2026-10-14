use crate::extractor::{AudioMetadata, SearchResult};
use crate::library::{LibraryStore, TrackRecord};
use crate::queue::PlayQueue;
use crate::{AppWindow, TrackRow};
use slint::ComponentHandle;
use tracing::info;

pub fn sanitize_text(raw: &str) -> String {
    raw.chars()
        .filter(|&c| c != '\u{FFFD}' && c != '\u{FFFC}' && (!c.is_control() || c == ' '))
        .collect()
}

pub fn record_from_search(result: SearchResult) -> TrackRecord {
    TrackRecord {
        id: result.id,
        title: sanitize_text(&result.title),
        artist: sanitize_text(&result.uploader.unwrap_or_else(|| "Unknown artist".into())),
        duration_secs: result.duration,
        thumbnail_url: result.thumbnail,
    }
}

pub fn record_from_audio(meta: &AudioMetadata) -> TrackRecord {
    TrackRecord {
        id: meta.id.clone(),
        title: sanitize_text(&meta.title),
        artist: sanitize_text(
            &meta
                .uploader
                .clone()
                .unwrap_or_else(|| "Unknown artist".into()),
        ),
        duration_secs: meta.duration,
        thumbnail_url: meta.thumbnail.clone(),
    }
}

pub fn requested_video_id(query: &str) -> Option<&str> {
    let query = query.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|part| {
        part.split_once('=')
            .filter(|(key, _)| *key == "v")
            .map(|(_, value)| value)
    })
}

pub fn query_video_id(query: &str) -> String {
    requested_video_id(query)
        .map(str::to_string)
        .unwrap_or_else(|| query.to_string())
}

pub fn track_row_model(
    tracks: Vec<(TrackRecord, String)>,
    library: &LibraryStore,
) -> slint::ModelRc<TrackRow> {
    let rows = tracks
        .into_iter()
        .map(|(track, key)| {
            let cover_file = library.cover_path(&track.id);
            let cover = if cover_file.exists() {
                slint::Image::load_from_path(&cover_file).unwrap_or_default()
            } else {
                slint::Image::default()
            };
            let duration = track.duration_label();
            TrackRow {
                key: slint::SharedString::from(key),
                title: slint::SharedString::from(track.title),
                artist: slint::SharedString::from(track.artist),
                duration: slint::SharedString::from(duration),
                cover,
                is_current: false,
                is_favorite: library.is_favorite(&track.id),
            }
        })
        .collect::<Vec<_>>();
    slint::ModelRc::new(slint::VecModel::from(rows))
}

pub fn records_model(records: Vec<TrackRecord>, library: &LibraryStore) -> slint::ModelRc<TrackRow> {
    track_row_model(
        records
            .into_iter()
            .map(|record| {
                let key = record.video_url();
                (record, key)
            })
            .collect(),
        library,
    )
}

pub fn queue_items_model(queue: &PlayQueue, library: &LibraryStore) -> slint::ModelRc<TrackRow> {
    let rows = queue
        .upcoming_items()
        .into_iter()
        .map(|item| {
            let track = TrackRecord {
                id: query_video_id(&item.query),
                title: item.title,
                artist: item.artist,
                duration_secs: item.duration_secs,
                thumbnail_url: item.thumbnail_url,
            };
            (track, item.query)
        })
        .collect();
    track_row_model(rows, library)
}

pub fn apply_queue_expanded(ui: &AppWindow, expanded: bool) {
    ui.set_queue_expanded(expanded);
    let current_size = ui.window().size().to_logical(ui.window().scale_factor());
    let logical_w = current_size.width.max(540.0);
    let target_h = if expanded {
        current_size.height.max(460.0)
    } else {
        185.0
    };
    ui.window().set_size(slint::LogicalSize::new(logical_w, target_h));
}

#[allow(dead_code)]
pub fn update_queue_ui(
    ui_weak: slint::Weak<AppWindow>,
    queue: PlayQueue,
    library: LibraryStore,
    expanded: Option<bool>,
) {
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.set_queue_items(queue_items_model(&queue, &library));
        if let Some(expanded) = expanded {
            apply_queue_expanded(&ui, expanded);
        }
    });
}

pub async fn cache_track_cover(client: reqwest::Client, library: LibraryStore, track: TrackRecord) {
    if library.cover_path(&track.id).exists() {
        return;
    }
    let Some(url) = track.thumbnail_url else {
        return;
    };
    if let Ok(response) = client.get(url).send().await
        && response.status().is_success()
            && let Ok(bytes) = response.bytes().await
                && let Err(error) = library.store_cover(&track.id, &bytes) {
                    info!("Could not cache cover art: {:#}", error);
                }
}

pub fn is_url(query: &str) -> bool {
    query.starts_with("http://") || query.starts_with("https://") || query.starts_with("www.")
}
