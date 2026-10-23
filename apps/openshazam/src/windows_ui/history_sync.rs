#![deny(unsafe_op_in_unsafe_fn)]

use std::collections::HashSet;

use windows::Win32::UI::HiDpi::GetDpiForWindow;

use crate::windows_history_store::PersistedTrack;

use super::{
    artwork_pipeline::start_history_artwork_load,
    artwork_state::{
        artwork_retry_is_suppressed, finish_active_artwork_request, ArtworkRequest,
        ArtworkRequestIdentity, TrackIdentity,
    },
    history::{format_history_date_label, HistoryTimestampSource, Track},
    notifications,
    page_link::ValidatedWebTrackUrl,
    resolve_track_page_url, AppState, HISTORY_HEIGHT, HISTORY_ROW_PITCH,
};

pub(super) fn restore_history_tracks(entries: Vec<PersistedTrack>) -> Vec<Track> {
    entries
        .into_iter()
        .take(40)
        .enumerate()
        .map(|(index, entry)| {
            let _source = entry.source;
            Track {
                display_id: index as u64 + 1,
                title: entry.title,
                artist: entry.artist,
                recognized_at: std::time::Instant::now(),
                recognized_date_label: format_history_date_label(entry.recognized_at_utc),
                recognized_at_utc: entry.recognized_at_utc,
                timestamp_source: HistoryTimestampSource::ControllerEvent,
                artwork_url: entry.artwork_url,
                hq_artwork_url: entry.hq_artwork_url,
                background_artwork_url: entry.background_artwork_url,
                track_key: entry.track_key,
                config_generation: None,
                page_url: entry
                    .shazam_page_url
                    .and_then(ValidatedWebTrackUrl::from_url),
            }
        })
        .collect()
}

pub(super) fn request_history_artwork_at_offset(app: &mut AppState, offset: f32) {
    let first = (offset.max(0.0) / HISTORY_ROW_PITCH).floor() as usize;
    let visible_count = (HISTORY_HEIGHT as f32 / HISTORY_ROW_PITCH).ceil() as usize + 2;
    let end = first.saturating_add(visible_count).min(app.history.len());
    let requests = (first..end)
        .filter_map(|index| {
            let track = app.history.get(index)?.clone();
            let identity = TrackIdentity {
                display_id: track.display_id,
            };
            if app.artwork_resources.contains_key(&track.display_id)
                || app.active_artwork_requests.contains_key(&identity)
                || artwork_retry_is_suppressed(
                    app.artwork_failures.get(&identity),
                    track.artwork_url.as_deref(),
                    track.hq_artwork_url.as_deref(),
                    std::time::Instant::now(),
                )
            {
                return None;
            }
            register_artwork_request(app, &track).map(|request| (track, request))
        })
        .collect::<Vec<_>>();
    let dpi = unsafe { GetDpiForWindow(app.main_hwnd) }.max(96);
    for (_, request) in requests {
        let identity = request.identity;
        if !start_history_artwork_load(app.main_hwnd, app.message_generation, request, dpi) {
            finish_active_artwork_request(&mut app.active_artwork_requests, identity);
        }
    }
}

pub(super) fn update_settings_history(app: &AppState) {
    let Some(host) = app.settings_host.as_ref() else {
        return;
    };

    let strings: Vec<[Vec<u16>; 4]> = app
        .history
        .iter()
        .map(|track| {
            [
                settings_wide_string(&track.title),
                settings_wide_string(&track.artist),
                settings_wide_string(&track.recognized_date_label),
                settings_wide_string(
                    &resolve_track_page_url(track, &app.ui_configs)
                        .map(|url| url.as_str().to_owned())
                        .unwrap_or_default(),
                ),
            ]
        })
        .collect();
    let items: Vec<_> = strings
        .iter()
        .map(|row| crate::windows_settings::SettingsHistoryItem {
            title: row[0].as_ptr(),
            artist: row[1].as_ptr(),
            recognized_at: row[2].as_ptr(),
            page_url: row[3].as_ptr(),
        })
        .collect();
    // SAFETY: each item points into one of the NUL-terminated UTF-16 vectors
    // owned by `strings`; both collections stay alive and unchanged until the
    // synchronous C++ export has copied every string before returning.
    unsafe { host.update_history(&items) };
}

fn settings_wide_string(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(super) fn register_artwork_request(
    app: &mut AppState,
    track: &Track,
) -> Option<ArtworkRequest> {
    if track.artwork_url.is_none() && track.hq_artwork_url.is_none() {
        return None;
    }
    let identity = TrackIdentity {
        display_id: track.display_id,
    };
    let generation = app
        .artwork_generations
        .get(&identity)
        .copied()
        .unwrap_or_default()
        .wrapping_add(1)
        .max(1);
    app.artwork_generations.insert(identity, generation);
    let request_id = app.next_artwork_request_id.max(1);
    app.next_artwork_request_id = request_id.wrapping_add(1).max(1);
    let request = ArtworkRequest {
        identity: ArtworkRequestIdentity {
            track: identity,
            generation,
            request_id,
        },
        standard_url: track.artwork_url.clone(),
        high_quality_url: track.hq_artwork_url.clone(),
    };
    app.active_artwork_requests
        .insert(identity, request.clone());
    Some(request)
}

pub(super) fn invalidate_orphaned_artwork_requests(app: &mut AppState) {
    let live_tracks: HashSet<_> = app
        .history
        .iter()
        .map(|track| TrackIdentity {
            display_id: track.display_id,
        })
        .collect();
    app.active_artwork_requests
        .retain(|identity, _| live_tracks.contains(identity));
    app.artwork_generations
        .retain(|identity, _| live_tracks.contains(identity));
    app.artwork_failures
        .retain(|identity, _| live_tracks.contains(identity));
    app.artwork_resources.retain(|display_id, _| {
        live_tracks.contains(&TrackIdentity {
            display_id: *display_id,
        })
    });
    let orphaned_notifications: Vec<_> = app
        .pending_match_notifications
        .keys()
        .filter(|display_id| {
            !live_tracks.contains(&TrackIdentity {
                display_id: **display_id,
            })
        })
        .copied()
        .collect();
    for display_id in orphaned_notifications {
        if let Some(track) = app.pending_match_notifications.remove(&display_id) {
            notifications::notify_hidden_match(app, &track, None);
        }
    }
}
