use super::Track;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ArtworkSource {
    Standard,
    HighQuality,
}

#[derive(Clone, Debug)]
pub(super) struct ArtworkPixels {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) fingerprint: u128,
    pub(super) bgra: Vec<u8>,
}

pub(super) struct CachedArtworkBitmap {
    pub(super) source: ArtworkSource,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) byte_len: usize,
    pub(super) fingerprint: u128,
    pub(super) bitmap: ID2D1Bitmap,
}

pub(super) fn should_replace_cached_artwork(
    cached: Option<(ArtworkSource, u32, u32, u128)>,
    incoming_source: ArtworkSource,
    incoming_width: u32,
    incoming_height: u32,
    incoming_fingerprint: u128,
) -> bool {
    match cached {
        None => true,
        Some((cached_source, cached_width, cached_height, cached_fingerprint)) => {
            let cached_dimensions = (
                cached_width.min(cached_height),
                cached_width.max(cached_height),
            );
            let incoming_dimensions = (
                incoming_width.min(incoming_height),
                incoming_width.max(incoming_height),
            );
            match incoming_dimensions.cmp(&cached_dimensions) {
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Equal => {
                    if cached_source == ArtworkSource::HighQuality
                        && incoming_source == ArtworkSource::Standard
                    {
                        false
                    } else {
                        cached_source != incoming_source
                            || cached_fingerprint != incoming_fingerprint
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct TrackIdentity {
    pub(super) display_id: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct ArtworkRequestIdentity {
    pub(super) track: TrackIdentity,
    pub(super) generation: u64,
    pub(super) request_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ArtworkRequest {
    pub(super) identity: ArtworkRequestIdentity,
    pub(super) standard_url: Option<String>,
    pub(super) high_quality_url: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct ArtworkLoadFailure {
    pub(super) standard_url: Option<String>,
    pub(super) high_quality_url: Option<String>,
    pub(super) failed_at: Instant,
}

const ARTWORK_RETRY_COOLDOWN: Duration = Duration::from_secs(60);

pub(super) fn artwork_retry_is_suppressed(
    failure: Option<&ArtworkLoadFailure>,
    standard_url: Option<&str>,
    high_quality_url: Option<&str>,
    now: Instant,
) -> bool {
    failure.is_some_and(|failure| {
        failure.standard_url.as_deref() == standard_url
            && failure.high_quality_url.as_deref() == high_quality_url
            && now.saturating_duration_since(failure.failed_at) < ARTWORK_RETRY_COOLDOWN
    })
}

pub(super) fn artwork_request_is_current(
    current: Option<&ArtworkRequest>,
    incoming: ArtworkRequestIdentity,
) -> bool {
    current.is_some_and(|request| request.identity == incoming)
}

pub(super) fn artwork_request_urls_match_track(
    request_standard: Option<&str>,
    request_high_quality: Option<&str>,
    track_standard: Option<&str>,
    track_high_quality: Option<&str>,
) -> bool {
    request_standard == track_standard && request_high_quality == track_high_quality
}

pub(super) fn artwork_request_matches_track(
    current: Option<&ArtworkRequest>,
    incoming: ArtworkRequestIdentity,
    track: Option<&Track>,
) -> bool {
    if !artwork_request_is_current(current, incoming) {
        return false;
    }
    let Some(track) = track else {
        return false;
    };
    track.display_id == incoming.track.display_id
        && current.is_some_and(|request| {
            artwork_request_urls_match_track(
                request.standard_url.as_deref(),
                request.high_quality_url.as_deref(),
                track.artwork_url.as_deref(),
                track.hq_artwork_url.as_deref(),
            )
        })
}

pub(super) fn finish_active_artwork_request(
    active_requests: &mut HashMap<TrackIdentity, ArtworkRequest>,
    identity: ArtworkRequestIdentity,
) -> bool {
    if artwork_request_is_current(active_requests.get(&identity.track), identity) {
        active_requests.remove(&identity.track);
        true
    } else {
        false
    }
}

#[derive(Debug)]
pub(super) struct ArtworkReady {
    pub(super) identity: ArtworkRequestIdentity,
    pub(super) source: ArtworkSource,
    pub(super) pixels: ArtworkPixels,
}

#[cfg(test)]
mod tests {
    use super::{artwork_retry_is_suppressed, ArtworkLoadFailure};
    use std::time::{Duration, Instant};

    #[test]
    fn suppresses_same_artwork_urls_during_failure_cooldown() {
        let failed_at = Instant::now();
        let failure = ArtworkLoadFailure {
            standard_url: Some("https://example.test/standard.jpg".to_owned()),
            high_quality_url: Some("https://example.test/high.jpg".to_owned()),
            failed_at,
        };

        assert!(artwork_retry_is_suppressed(
            Some(&failure),
            failure.standard_url.as_deref(),
            failure.high_quality_url.as_deref(),
            failed_at + Duration::from_secs(30),
        ));
    }

    #[test]
    fn retries_after_failure_cooldown_expires() {
        let failed_at = Instant::now();
        let failure = ArtworkLoadFailure {
            standard_url: Some("https://example.test/standard.jpg".to_owned()),
            high_quality_url: None,
            failed_at,
        };

        assert!(!artwork_retry_is_suppressed(
            Some(&failure),
            failure.standard_url.as_deref(),
            failure.high_quality_url.as_deref(),
            failed_at + Duration::from_secs(60),
        ));
    }

    #[test]
    fn retries_immediately_when_artwork_urls_change() {
        let failed_at = Instant::now();
        let failure = ArtworkLoadFailure {
            standard_url: Some("https://example.test/old.jpg".to_owned()),
            high_quality_url: None,
            failed_at,
        };

        assert!(!artwork_retry_is_suppressed(
            Some(&failure),
            Some("https://example.test/new.jpg"),
            None,
            failed_at + Duration::from_secs(1),
        ));
    }
}
