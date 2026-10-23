use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::{
    core::HSTRING, Foundation::DateTime, Globalization::DateTimeFormatting::DateTimeFormatter,
};
use windows_collections::IIterable;

use super::{directwrite_locale, ui_language, ValidatedWebTrackUrl};

#[derive(Clone, Debug)]
pub(super) struct Track {
    pub(super) display_id: u64,
    pub(super) title: String,
    pub(super) artist: String,
    pub(super) recognized_at: Instant,
    pub(super) recognized_at_utc: SystemTime,
    pub(super) recognized_date_label: String,
    #[allow(dead_code)]
    pub(super) timestamp_source: HistoryTimestampSource,
    pub(super) artwork_url: Option<String>,
    pub(super) hq_artwork_url: Option<String>,
    #[allow(dead_code)]
    // Kept distinct from the album-art rendition by the UI data contract.
    pub(super) background_artwork_url: Option<String>,
    #[allow(dead_code)]
    // Retained alongside the Config-resolved page URL for the presentation model.
    pub(super) track_key: Option<String>,
    pub(super) config_generation: Option<u64>,
    pub(super) page_url: Option<ValidatedWebTrackUrl>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HistoryTimestampSource {
    ControllerEvent,
    HostReceivedFallback,
}

pub(super) fn resolve_history_timestamp(
    event_time_utc: Option<SystemTime>,
    received_at_utc: SystemTime,
) -> (SystemTime, HistoryTimestampSource) {
    match event_time_utc {
        Some(when) => (when, HistoryTimestampSource::ControllerEvent),
        None => (
            received_at_utc,
            HistoryTimestampSource::HostReceivedFallback,
        ),
    }
}

pub(super) const WINDOWS_EPOCH_OFFSET_100NS: i64 = 116_444_736_000_000_000;
pub(super) const WINDOWS_DATETIME_TEMPLATE: &str = "day month.abbreviated year hour minute";

pub(super) fn winrt_datetime_from_system_time(value: SystemTime) -> DateTime {
    let elapsed = value.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let unix_100ns =
        (elapsed.as_secs() as i128) * 10_000_000 + i128::from(elapsed.subsec_nanos() / 100);
    DateTime {
        UniversalTime: (unix_100ns + i128::from(WINDOWS_EPOCH_OFFSET_100NS))
            .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64,
    }
}

pub(super) fn format_history_date_label(value: SystemTime) -> String {
    let template = HSTRING::from(WINDOWS_DATETIME_TEMPLATE);
    let languages =
        IIterable::<HSTRING>::from(vec![HSTRING::from(directwrite_locale(ui_language()))]);
    DateTimeFormatter::CreateDateTimeFormatterLanguages(&template, &languages)
        .and_then(|formatter| formatter.Format(winrt_datetime_from_system_time(value)))
        .map(|formatted| formatted.to_string())
        .unwrap_or_default()
}

pub(super) fn refresh_history_date_labels(history: &mut [Track]) {
    for track in history {
        track.recognized_date_label = format_history_date_label(track.recognized_at_utc);
    }
}
