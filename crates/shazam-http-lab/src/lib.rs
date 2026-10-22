//! Offline request-shape, loopback mock, and opt-in live HTTP laboratory.
//!
//! The request body is a partial static reconstruction. Mock requests use a
//! loopback-only transport; the separate live path fetches public Config over
//! verified HTTPS and restricts recognition to Config-provided first-party
//! Manual or explicitly contextualized Auto routes. This is an experimental
//! client for a non-public route, not an official API implementation. The
//! request carries the observed Shazam 2.11.0 platform/version headers and
//! omits the conditional AMP authentication header.
#![deny(clippy::undocumented_unsafe_blocks)]

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use shazam_progressive::{
    EngineError, NativeSampleFrameTargets, NativeTaggingType, ProgressiveSession,
    RecognitionOutcome, RecognitionTiming, RetryDirective, SessionStatus,
    NATIVE_TAGGING_TYPE_1_DEFAULT_SESSION_TIMEOUT_MS,
    NATIVE_TAGGING_TYPE_2_DEFAULT_SESSION_TIMEOUT_MS,
};

const SIGNATURE_DATA_URI: &str = "data:audio/vnd.shazam.sig;base64,";
const SHAZAM_MAC_PLATFORM: &str = "MAC";
const SHAZAM_MAC_APP_VERSION: &str = "2.11.0";
const MAX_RESPONSE_BYTES: usize = 1_048_576;
const MAX_HEADER_BYTES: usize = 16_384;
/// Maximum retained UI web-track template size (4 KiB).
pub const MAX_WEB_TRACK_URL_TEMPLATE_BYTES: usize = 4_096;
const CURL_STATUS_MARKER: &[u8] = b"\n__OPENSHZAM_HTTP_STATUS__";

fn progressive_request_timestamp_ms(source_timestamp_ms: Option<u64>) -> Result<u64, Error> {
    if let Some(timestamp_ms) = source_timestamp_ms {
        return Ok(timestamp_ms);
    }
    use std::time::{SystemTime, UNIX_EPOCH};
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Transport("system clock is before Unix epoch".into()))?
        .as_millis()
        .min(u128::from(u64::MAX)) as u64)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackCandidate {
    pub track_id: Option<String>,
    pub title: String,
    /// Normalized artist display value. The inspected Native main view reads
    /// this role through `SHALTrack.subtitle` backed by `TagResult.artist`.
    pub artist: String,
    /// TagResult-backed values used by Native `SHALTrack` display getters,
    /// copied only when the recognition response supplies them.
    pub ui_metadata: Option<NativeTrackUiMetadata>,
    /// Native's separate alternative-track object, when the root `track`
    /// dictionary carries a non-empty `alternativetrack` dictionary.
    /// This is not derived from the second entry in `matches`.
    pub alternative_track: Option<Box<TrackCandidate>>,
    /// Provenance used to keep Manual root-track callbacks distinct from the
    /// ordered Auto Type 2 `matches` candidates.
    #[doc(hidden)]
    pub origin: TrackCandidateOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackCandidateOrigin {
    /// The single top-level `track` dictionary, statically mapped to the
    /// Native tag selected by `tags.firstObject`.
    RootTrack,
    /// The separate `alternativetrack` dictionary from that root response.
    AlternativeTrack,
    /// An entry from `matches`, preserved in response array order. Native
    /// Shazam converts these entries to tags in order; Auto uses the first.
    MatchesEntry,
    /// A synthetic candidate created by tests or a caller.
    Synthetic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTrackUiMetadata {
    /// Response string selected for Native UI handoff. This is deliberately
    /// distinct from `TrackCandidate::track_id`; no numeric ID fallback is
    /// inferred for web-page links.
    pub track_key: Option<String>,
    pub album_adam_id: Option<String>,
    pub isrc: Option<String>,
    pub date: Option<String>,
    pub artwork_url: Option<String>,
    pub hq_artwork_url: Option<String>,
    pub background_image_url: Option<String>,
}

/// Manual Type 1 result callback stage exposed to a UI host.
///
/// A tag-bearing response with a positive Retry is delivered through
/// `willFetchCompleteResultForContext:track:alternativeTrack:`. A response
/// without a positive Retry follows `matchFoundForContext:inTagSession:`;
/// `SHALAppTagManager.tagResultMatched:alternativeTagResult:context:inTaggingSession:`
/// then converts the results and calls the UI delegate's
/// `taggingDidFinishWithTrack:alternativeTrack:context:inTaggingSession:`.
/// This Rust event reports the corresponding phase and the two normalized
/// track values passed to the Native UI delegate; it does not synthesize
/// Native `SHALTrack` objects or location enrichment. Callers must still apply the Progressive session's
/// context-delivery and Retry-authorization gates before publishing it to a
/// UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeManualResultStage {
    /// Positive Manual Retry: Native performs its early-result callback and
    /// keeps the Manual generator eligible for another Signature.
    WillFetchCompleteResult,
    /// No positive Manual Retry: Native routes the match through
    /// `matchFoundForContext:inTagSession:` and then calls the UI delegate's
    /// final `taggingDidFinishWithTrack:alternativeTrack:context:inTaggingSession:`.
    TaggingDidFinish,
}

/// UI-facing, data-only result for the verified Manual Type 1 callback paths.
/// `SHALAppTagManager` selects the first tag and passes its primary track plus
/// that tag's optional alternative as separate callback arguments; additional
/// tags in the matcher array are not UI callback tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeManualResultEvent {
    pub stage: NativeManualResultStage,
    pub primary_track: TrackCandidate,
    pub alternative_track: Option<Box<TrackCandidate>>,
}

/// One normalized projection of Manual Type 1 callback values for an application UI.
///
/// The enum preserves the Native callback phase (early complete-result,
/// final match, no-match, or terminal error) without pretending that Rust has
/// Native `SHALTrack`, context/session objects, or an `NSError` instance. The
/// JSON result includes `native_main_view_action` to distinguish the early
/// no-op from the final primary-track display observed in Shazam 2.11.0's main
/// controller. This is not a runtime observation of the Native delegate callback: the Rust
/// event is created when the HTTP response is applied, while Native's
/// TagResult conversion can conditionally perform asynchronous Smoid
/// enrichment before invoking its delegate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeManualUiEvent {
    Result(NativeManualResultEvent),
    NoMatch,
    Error { kind: NativeManualErrorKind },
}

/// Normalized candidate inputs for Auto Type 2's continuous-result delegate.
/// Shazam's app performs duplicate checks before invoking that delegate; this
/// projection exposes the supported root-track values but does not claim that
/// Native's stateful duplicate reporter accepted the candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAutoResultEvent {
    pub primary_track: TrackCandidate,
    pub alternative_track: Option<Box<TrackCandidate>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeAutoUiEvent {
    ContinuousMatchCandidate(NativeAutoResultEvent),
}

/// Serialize the result values that Shazam 2.11.0 passes through
/// `continuousTaggingDidFinishWithTrack:alternativeTrack:context:` after its
/// app-level duplicate checks. This event is deliberately named a candidate:
/// the Rust HTTP adapter does not implement the Native duplicate reporter or
/// manufacture its tag-context object.
pub fn native_auto_ui_event_to_json(event: &NativeAutoUiEvent) -> String {
    native_auto_ui_event_to_value(event).to_string()
}

fn native_auto_ui_event_to_value(event: &NativeAutoUiEvent) -> Value {
    match event {
        NativeAutoUiEvent::ContinuousMatchCandidate(result) => json!({
            "schema": "openshazam.auto-ui-event.v1",
            "event": "continuous_match_candidate",
            "tagging_type": 2,
            "native_callback_candidate": "continuousTaggingDidFinishWithTrack:alternativeTrack:context:",
            "native_duplicate_filter_applied": false,
            "primary_track": track_candidate_json(&result.primary_track),
            "alternative_track": result.alternative_track.as_deref().map(track_candidate_json),
        }),
    }
}

/// Build the Auto Type 2 UI candidate from the first delivered tag candidate.
/// The inspected ShazamTagging path converts root `matches` entries to tags
/// in array order, and ShazamAppLogic selects the first tag for the continuous
/// callback. Manual Type 1 continues to require the separate root `track` path.
pub fn native_auto_result_event(candidates: Vec<TrackCandidate>) -> Option<NativeAutoResultEvent> {
    let mut primary_track = candidates.into_iter().next()?;
    if !matches!(
        primary_track.origin,
        TrackCandidateOrigin::RootTrack | TrackCandidateOrigin::MatchesEntry
    ) {
        return None;
    }
    let alternative_track = primary_track.alternative_track.take();
    Some(NativeAutoResultEvent {
        primary_track,
        alternative_track,
    })
}

/// Serialize one normalized Manual callback input as a versioned JSONL record
/// for an external UI host. `native_main_view_inputs` names only the three
/// values read by Shazam 2.11.0's final main-view update; early complete-result
/// callbacks carry their callback tracks but request no main-view update.
/// Native objects are not synthesized. The parser's documented image
/// fallback is an explicit normalization, not a raw response copy.
pub fn native_manual_ui_event_to_json(event: &NativeManualUiEvent) -> String {
    native_manual_ui_event_to_value(event).to_string()
}

fn native_manual_ui_event_to_value(event: &NativeManualUiEvent) -> Value {
    let value = match event {
        NativeManualUiEvent::Result(result) => {
            let (stage, native_main_view_action) = match result.stage {
                NativeManualResultStage::WillFetchCompleteResult => {
                    ("will_fetch_complete_result", "no_update")
                }
                NativeManualResultStage::TaggingDidFinish => {
                    ("tagging_did_finish", "display_primary")
                }
            };
            let native_main_view_inputs =
                (result.stage == NativeManualResultStage::TaggingDidFinish).then(|| {
                    let artwork_url = result
                        .primary_track
                        .ui_metadata
                        .as_ref()
                        .and_then(|metadata| metadata.artwork_url.as_deref());
                    json!({
                        "title": result.primary_track.title.as_str(),
                        "subtitle": result.primary_track.artist.as_str(),
                        "artwork_url": artwork_url,
                    })
                });
            json!({
                "schema": "openshazam.manual-ui-event.v2",
                "event": "result",
                "stage": stage,
                "native_main_view_action": native_main_view_action,
                "native_main_view_inputs": native_main_view_inputs,
                "primary_track": track_candidate_json(&result.primary_track),
                "alternative_track": result.alternative_track.as_deref().map(track_candidate_json),
            })
        }
        NativeManualUiEvent::NoMatch => json!({
            "schema": "openshazam.manual-ui-event.v2",
            "event": "no_match",
        }),
        NativeManualUiEvent::Error { kind } => {
            let error_kind = match kind {
                NativeManualErrorKind::MatcherFailure => "matcher_failure",
                NativeManualErrorKind::InvalidResponse => "invalid_response",
                NativeManualErrorKind::TransportFailure => "transport_failure",
            };
            json!({
                "schema": "openshazam.manual-ui-event.v2",
                "event": "error",
                "error_kind": error_kind,
            })
        }
    };

    value
}

/// Serialize the normalized terminal event for the Native tagging-session
/// timeout. Shazam 2.11.0 statically routes `taggingTimedOutForSession:` into
/// `SHKAudioMatcherBridge` and the context delegate's error callback; this
/// record preserves only the observed reason and tagging type, not an NSError
/// code, native object, context identifier, or session identifier.
pub fn native_session_timeout_event_to_json(tagging_type: NativeTaggingType) -> String {
    let native_tagging_type = match tagging_type {
        NativeTaggingType::Manual => 1,
        NativeTaggingType::Auto => 2,
    };
    json!({
        "schema": "openshazam.progressive-session-event.v1",
        "event": "session_timeout",
        "tagging_type": native_tagging_type,
    })
    .to_string()
}

/// Native Manual Type 1 terminal callback kind for a no-match or a failure
/// that did not continue through an accepted Retry. Native's No Match UI
/// selector receives the tag context and tagging session, not the Retry;
/// Retry remains an internal control value. Shazam's UI-facing callbacks are
/// dispatched asynchronously on the main queue. This event is data-only and
/// does not itself carry Native context/session objects or NSError values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeManualTerminalEvent {
    /// Native reports No Match and stops the Manual tagging context when no
    /// accepted Retry continues the matched callback.
    NoMatch,
    /// Normalized local failure category for Native's error callback. This is
    /// not a byte-for-byte NSError replacement; response bodies and raw OS
    /// errors are deliberately not copied into this event.
    Error { kind: NativeManualErrorKind },
}

/// Bounded failure category for the Native Manual error callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeManualErrorKind {
    MatcherFailure,
    InvalidResponse,
    TransportFailure,
}

/// Build a UI-phase event for a delivered, tag-bearing Manual Type 1 response
/// whose primary is the statically mapped root `track` dictionary.
///
/// Native Shazam 2.11 routes a positive Retry with non-empty tags to its
/// complete-result callback; zero/missing retry duration follows
/// `matchFoundForContext` and the asynchronously produced
/// `taggingDidFinishWithTrack` UI callback. The Native app selects only the
/// first tag and its separate alternative track. Candidate fields are
/// normalized values from the supported root-track response shape, not a
/// byte-for-byte `SHALTrack` model. Native's `SHDTagAccessor` may also perform
/// asynchronous Smoid enrichment based on TagOrigin before calling its
/// delegate; this Rust projection is emitted at response-application time and
/// does not reproduce that completion timing or location enrichment. The
/// caller is responsible for filtering late/cancelled responses and using
/// this only for Manual Type 1, since other Native tagging types have
/// different delegate semantics.
pub fn native_manual_result_event(
    candidates: Vec<TrackCandidate>,
    retry: Option<RetryDirective>,
) -> Option<NativeManualResultEvent> {
    let mut primary_track = candidates.into_iter().next()?;
    if primary_track.origin != TrackCandidateOrigin::RootTrack {
        return None;
    }
    let alternative_track = primary_track.alternative_track.take();

    let stage = if retry.is_some_and(RetryDirective::has_positive_retry_duration) {
        NativeManualResultStage::WillFetchCompleteResult
    } else {
        NativeManualResultStage::TaggingDidFinish
    };

    Some(NativeManualResultEvent {
        stage,
        primary_track,
        alternative_track,
    })
}

/// Result of applying one progressive matcher response to its session.
/// `manual_result_event` is populated only after the response passes Native's
/// shared-context delivery gate and the session is Manual Type 1. UI callers
/// should consume that event rather than infer a callback phase from HTTP 200.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressiveResponseDelivery {
    pub status: SessionStatus,
    pub delivered_to_matcher: bool,
    /// Candidates are included only for a response delivered to the session.
    pub candidates: Vec<TrackCandidate>,
    pub manual_result_event: Option<NativeManualResultEvent>,
    /// Manual No Match/Error callback data, present only after the shared
    /// context gate has accepted the response. Accepted MatcherFailure Retry
    /// directives do not produce an error event because Native continues
    /// tagging instead of notifying the UI of a terminal failure.
    pub manual_terminal_event: Option<NativeManualTerminalEvent>,
    /// Auto Type 2 continuous-match candidate, created only after the response
    /// passes Native's shared-context delivery gate. Native app duplicate
    /// filtering remains outside this platform-independent response mapper.
    pub auto_result_event: Option<NativeAutoResultEvent>,
}

impl ProgressiveResponseDelivery {
    /// Return the single Manual UI callback input for this delivered response.
    /// Stale/cancelled responses, Auto sessions, and accepted Retry-only
    /// failures have no UI event.
    pub fn native_manual_ui_event(&self) -> Option<NativeManualUiEvent> {
        match (&self.manual_result_event, self.manual_terminal_event) {
            (Some(result), None) => Some(NativeManualUiEvent::Result(result.clone())),
            (None, Some(NativeManualTerminalEvent::NoMatch)) => Some(NativeManualUiEvent::NoMatch),
            (None, Some(NativeManualTerminalEvent::Error { kind })) => {
                Some(NativeManualUiEvent::Error { kind })
            }
            // The response mapper emits at most one callback. Treat an
            // impossible mixed/empty payload as absent instead of inventing
            // a callback ordering.
            _ => None,
        }
    }

    pub fn native_auto_ui_event(&self) -> Option<NativeAutoUiEvent> {
        self.auto_result_event
            .as_ref()
            .map(|event| NativeAutoUiEvent::ContinuousMatchCandidate(event.clone()))
    }
}

/// Apply a parsed response and expose the Native Manual UI callback payload.
/// This is a reusable library boundary for a UI host: it applies the same
/// context gate/state transition first, then returns the event and observed
/// candidate data. Callers still own polling/dispatch of any Retry Signature.
pub fn apply_progressive_response(
    session: &mut ProgressiveSession,
    attempt: u64,
    outcome: RecognitionOutcome,
    candidates: Vec<TrackCandidate>,
    now_ms: u64,
) -> Result<ProgressiveResponseDelivery, EngineError> {
    let now_ns = now_ms
        .checked_mul(1_000_000)
        .ok_or(EngineError::InvalidTiming(
            "monotonic timestamp exceeds nanosecond clock range",
        ))?;
    apply_progressive_response_at_ns(session, attempt, outcome, candidates, now_ns)
}

/// Nanosecond-resolution response adapter for live capture/controller clocks.
pub fn apply_progressive_response_at_ns(
    session: &mut ProgressiveSession,
    attempt: u64,
    outcome: RecognitionOutcome,
    candidates: Vec<TrackCandidate>,
    now_ns: u64,
) -> Result<ProgressiveResponseDelivery, EngineError> {
    let retry = match outcome {
        RecognitionOutcome::Match { retry } => retry,
        _ => None,
    };
    let (status, delivered_to_matcher) =
        match session.complete_with_delivery_at_ns(attempt, outcome, now_ns) {
            Ok(delivery) => delivery,
            Err(EngineError::NoRequestInFlight) => {
                return Ok(ProgressiveResponseDelivery {
                    status: session.status(),
                    delivered_to_matcher: false,
                    candidates: Vec::new(),
                    manual_result_event: None,
                    manual_terminal_event: None,
                    auto_result_event: None,
                });
            }
            Err(error) => return Err(error),
        };
    let event_candidates = candidates.clone();
    let manual_result_event = if delivered_to_matcher
        && session.tagging_type() == NativeTaggingType::Manual
        && matches!(outcome, RecognitionOutcome::Match { .. })
    {
        native_manual_result_event(event_candidates.clone(), retry)
    } else {
        None
    };
    let manual_terminal_event = if delivered_to_matcher
        && session.tagging_type() == NativeTaggingType::Manual
    {
        match outcome {
            RecognitionOutcome::NoMatch { retry: None } => Some(NativeManualTerminalEvent::NoMatch),
            RecognitionOutcome::MatcherFailure { retry: None } => {
                Some(NativeManualTerminalEvent::Error {
                    kind: NativeManualErrorKind::MatcherFailure,
                })
            }
            RecognitionOutcome::InvalidResponse => Some(NativeManualTerminalEvent::Error {
                kind: NativeManualErrorKind::InvalidResponse,
            }),
            RecognitionOutcome::TransportFailure => Some(NativeManualTerminalEvent::Error {
                kind: NativeManualErrorKind::TransportFailure,
            }),
            RecognitionOutcome::NoMatch { retry: Some(_) }
            | RecognitionOutcome::Match { .. }
            | RecognitionOutcome::MatcherFailure { retry: Some(_) } => None,
        }
    } else {
        None
    };
    let auto_result_event = if delivered_to_matcher
        && session.tagging_type() == NativeTaggingType::Auto
        && matches!(outcome, RecognitionOutcome::Match { .. })
    {
        native_auto_result_event(event_candidates)
    } else {
        None
    };

    Ok(ProgressiveResponseDelivery {
        status,
        delivered_to_matcher,
        candidates: if delivered_to_matcher {
            candidates
        } else {
            Vec::new()
        },
        manual_result_event,
        manual_terminal_event,
        auto_result_event,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Transport boundary for one Config-resolved recognition HTTPS POST.
/// Implementations must preserve TLS verification, the caller's timeout and
/// cancellation semantics, and must not follow redirects or use ambient
/// cookies/proxy credentials.
pub trait RecognitionHttpTransport: Send + Sync {
    fn post_json_cancellable(
        &self,
        endpoint: &str,
        body: &[u8],
        timeout: Duration,
        cancelled: &AtomicBool,
        on_transport_started: &mut dyn FnMut(),
    ) -> Result<HttpResponse, Error>;
}

/// Existing verified HTTPS transport. It uses the host's curl executable and
/// remains the default until a native Windows transport is independently
/// verified against the same request and cancellation tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct CurlRecognitionTransport;

/// Classification of one progressive recognition HTTP response. The response
/// body is consumed immediately; callers only receive parsed track data or
/// bounded control fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressiveHttpResponse {
    Matched {
        candidates: Vec<TrackCandidate>,
        retry: Option<RetryDirective>,
    },
    NoMatch(Option<RetryDirective>),
    HttpError {
        status: u16,
        retry: Option<RetryDirective>,
    },
    /// The response could not be decoded as a usable tag dictionary. Native
    /// sends this through its matcher-failure callback, which can synthesize
    /// a zero-interval Retry to the request maximum when the Signature is
    /// still shorter.
    ParseFailure {
        retry: Option<RetryDirective>,
    },
    InvalidResponse,
}

/// Parsed result from one live progressive recognition request. The response
/// body and Signature are deliberately not retained in this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressiveRecognitionResult {
    pub status: u16,
    pub response: ProgressiveHttpResponse,
}

/// Non-secret host profile sent to the Config endpoint. The installation ID
/// is deliberately not part of this type; it is parsed from the HTTPS response
/// and is never persisted by this crate.
#[derive(Clone, PartialEq, Eq)]
pub struct ConfigRequestProfile {
    pub country: String,
    pub language: String,
    pub locale: String,
    pub model: String,
    pub os_version: String,
    pub os_build: String,
    pub app_version: String,
}

/// Foreground/background state consumed by the Native continuous-tag request
/// builder. Callers provide the observed UI/app state; this crate does not
/// infer it from the CLI process or OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTaggingRequestContext {
    Foreground,
    Background,
}

/// Config values required to construct supported Mac tagging routes. No
/// `Debug` implementation is provided so accidental logging cannot expose
/// INID or the retained route templates.
#[derive(Clone)]
pub struct ShazamConfiguration {
    tag_url_template: String,
    web_track_url_template: Option<String>,
    continuous_foreground_url_template: Option<String>,
    continuous_background_url_template: Option<String>,
    installation_id: String,
    manual_timing: ManualTaggingTiming,
    native_frame_seconds: NativeFrameSeconds,
}

/// Minimal Config data intended for a UI PageLinkResolver. The generation is
/// assigned by the caller that owns Config/session lifecycle, not by parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiConfigSnapshot {
    pub generation: u64,
    pub web_track_url_template: Option<String>,
}

impl UiConfigSnapshot {
    /// Versioned standalone record for an explicitly opted-in UI JSONL host.
    pub fn to_json(&self) -> String {
        json!({
            "schema": "openshazam.ui-config-snapshot.v1",
            "event": "ui_config_snapshot",
            "generation": self.generation,
            "web_track_url_template": self.web_track_url_template.as_deref(),
        })
        .to_string()
    }
}

/// Typed UI result input paired with the Config generation active for its
/// recognition session. Track Key remains inside the existing result event;
/// this wrapper does not generate or validate a page URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiRecognitionInput {
    pub config_generation: u64,
    pub event: UiRecognitionEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiRecognitionEvent {
    Manual(NativeManualUiEvent),
    Auto(NativeAutoUiEvent),
}

impl UiRecognitionInput {
    /// Independent versioned record for hosts that explicitly opt into the
    /// new JSONL bridge. Existing Manual/Auto event schemas are nested intact.
    pub fn to_json(&self) -> String {
        let recognition_event = match &self.event {
            UiRecognitionEvent::Manual(event) => native_manual_ui_event_to_value(event),
            UiRecognitionEvent::Auto(event) => native_auto_ui_event_to_value(event),
        };
        json!({
            "schema": "openshazam.ui-recognition-input.v1",
            "event": "recognition_ui_input",
            "config_generation": self.config_generation,
            "recognition_event": recognition_event,
        })
        .to_string()
    }
}

#[derive(Clone, Copy)]
struct NativeFrameSeconds {
    sample_seconds_bits: Option<u64>,
    progressive_seconds_bits: Option<u64>,
}

/// Recognized numeric fields from the Shazam Config response. These remain
/// optional: a missing or ambiguous field is not replaced with an assumed
/// server value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManualTaggingTiming {
    /// Config `tagging.sampleSeconds`: Native per-cycle maximum.
    pub sample_ms: Option<u64>,
    /// Config `tagging.progressiveRecordingSeconds`: Native initial target.
    pub progressive_ms: Option<u64>,
    /// Config `settings.recording.silentRecordingSeconds` converted using the
    /// Native `NSNumber.integerValue` behavior, in milliseconds.
    /// Use `effective_pre_record_buffer_ms` for Native fallback behavior.
    pub pre_record_buffer_ms: Option<u64>,
    /// Config `tagging.minsamplems`: minimum Signature duration in ms.
    pub minimum_signature_ms: Option<u64>,
    /// Config maximum signature length; Native uses it in a conditional
    /// assertion when a Retry duration exceeds the value.
    pub maximum_signature_ms: Option<u64>,
    /// Native Manual Type 1 deadline, sourced from
    /// `autoTag.continuousForegroundIdleTimeout` (seconds).
    pub session_timeout_ms: Option<u64>,
    /// Native Auto Type 2 deadline from the same Config field. Retained as a
    /// separate value in the public timing model for mode-specific reporting.
    pub auto_session_timeout_ms: Option<u64>,
}

/// `SHNTaggingConfiguration.maxSampleSeconds` falls back to 12 seconds when
/// the selected Native tag block has no valid `maxsamplems` value. This is a
/// maximum Signature length, not the Auto Shazam cycle interval.
pub const NATIVE_DEFAULT_MAX_SIGNATURE_LENGTH_MS: u64 = 12_000;
/// `SHNTaggingConfiguration.minSampleSeconds` falls back to 2 seconds when
/// `tagging.minsamplems` is missing or invalid. Confirmed in the Shazam 2.11.0
/// ARM64 getter; this is the Signature-generation gate, not the initial target.
pub const NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS: u64 = 2_000;
/// `SHNTaggingConfiguration.silentRecordingSeconds` falls back to 3 seconds
/// when the setting is absent or its `NSNumber.integerValue` is at most 2.
pub const NATIVE_DEFAULT_PRE_RECORD_BUFFER_MS: u64 = 3_000;
/// Shazam 2.11's ARM64 recorder baseline uses the Apple Silicon page size
/// observed on the machine running its `vm_page_size`-based ring allocator.
pub const NATIVE_MACOS_VM_PAGE_SIZE_BYTES: u64 = 16_384;
/// `SHKTaggingComponents.recorderConfiguration` creates a standard mono
/// `AVAudioFormat`; its Float32 stream has four bytes per audio frame.
pub const NATIVE_RECORDER_BYTES_PER_FRAME: u64 = 4;

impl ManualTaggingTiming {
    /// Effective first recording target for Native manual TaggingType 1.
    /// The native getter uses progressiveRecordingSeconds when present and
    /// otherwise falls back to sampleSeconds. `None` means neither value was
    /// supplied by Config; callers must not invent a replacement.
    pub const fn effective_initial_sample_ms(self) -> Option<u64> {
        match self.progressive_ms {
            Some(milliseconds) => Some(milliseconds),
            None => self.sample_ms,
        }
    }

    /// Effective Native recorder ring-buffer length. The framework returns
    /// the configured NSNumber only when its integer value is greater than 2.
    pub const fn effective_pre_record_buffer_ms(self) -> u64 {
        match self.pre_record_buffer_ms {
            Some(milliseconds) if milliseconds / 1_000 > 2 => (milliseconds / 1_000) * 1_000,
            _ => NATIVE_DEFAULT_PRE_RECORD_BUFFER_MS,
        }
    }

    /// Reproduce the Native recorder's page-rounded circular-buffer capacity
    /// for the Shazam 2.11 ARM64 baseline. The binary computes
    /// `ceil(sampleRate * integerSeconds * mBytesPerFrame / vm_page_size)` pages
    /// before allocating the ring; the mono Float32 stream uses four bytes per
    /// audio frame. The returned value is an audio-frame count for the Rust
    /// mono PCM ring.
    pub fn effective_pre_record_capacity_frames(self, sample_rate: u32) -> Result<usize, Error> {
        if sample_rate == 0 {
            return Err(Error::InvalidRequest(
                "pre-record buffer sample rate must be nonzero".into(),
            ));
        }
        let frames = u64::from(sample_rate)
            .checked_mul(self.effective_pre_record_buffer_ms() / 1_000)
            .ok_or_else(|| Error::InvalidRequest("pre-record frame count overflow".into()))?;
        let bytes = frames
            .checked_mul(NATIVE_RECORDER_BYTES_PER_FRAME)
            .ok_or_else(|| Error::InvalidRequest("pre-record byte count overflow".into()))?;
        let page_rounded_bytes = bytes
            .checked_add(NATIVE_MACOS_VM_PAGE_SIZE_BYTES - 1)
            .map(|value| value / NATIVE_MACOS_VM_PAGE_SIZE_BYTES * NATIVE_MACOS_VM_PAGE_SIZE_BYTES)
            .ok_or_else(|| Error::InvalidRequest("pre-record page alignment overflow".into()))?;
        usize::try_from(page_rounded_bytes / NATIVE_RECORDER_BYTES_PER_FRAME)
            .map_err(|_| Error::InvalidRequest("pre-record frame capacity is too large".into()))
    }

    /// Effective Native maximum Signature length used by MatcherRequest.
    pub const fn effective_maximum_signature_length_ms(self) -> u64 {
        match self.maximum_signature_ms {
            Some(value) => value,
            None => NATIVE_DEFAULT_MAX_SIGNATURE_LENGTH_MS,
        }
    }

    /// Build the control-engine profile from Config values plus caller policy.
    /// Attempt count is local; callers use the Native 60-second request
    /// timeout by default but may override it for diagnostics.
    pub fn recognition_timing(
        self,
        sample_rate: u32,
        minimum_sample_fallback_ms: u64,
        default_retry_interval_ms: u64,
        request_timeout_ms: u64,
        maximum_retry_delay_ms: Option<u64>,
        maximum_attempts: Option<u64>,
    ) -> Result<RecognitionTiming, Error> {
        let initial_sample_ms = self.effective_initial_sample_ms().ok_or_else(|| {
            Error::BadResponse("Config has no usable manual sample duration".into())
        })?;
        let maximum_sample_ms = self.sample_ms.ok_or_else(|| {
            Error::BadResponse("Config has no usable per-cycle sample duration".into())
        })?;
        let timing = RecognitionTiming {
            sample_rate,
            minimum_signature_ms: self
                .minimum_signature_ms
                .unwrap_or(minimum_sample_fallback_ms),
            initial_sample_ms,
            maximum_sample_ms,
            default_retry_interval_ms,
            maximum_retry_duration_ms: Some(self.effective_maximum_signature_length_ms()),
            session_timeout_ms: Some(
                self.session_timeout_ms
                    .unwrap_or(NATIVE_TAGGING_TYPE_1_DEFAULT_SESSION_TIMEOUT_MS),
            ),
            request_timeout_ms,
            maximum_retry_delay_ms,
            maximum_attempts,
        };
        if timing
            .maximum_retry_delay_ms
            .is_some_and(|maximum| timing.default_retry_interval_ms > maximum)
        {
            return Err(Error::BadResponse(
                "local default retry interval exceeds the local safety bound".into(),
            ));
        }
        Ok(timing)
    }

    /// Build the same sample profile with Auto TaggingType 2's session
    /// timeout source and Native fallback.
    pub fn auto_recognition_timing(
        self,
        sample_rate: u32,
        minimum_sample_fallback_ms: u64,
        default_retry_interval_ms: u64,
        request_timeout_ms: u64,
        maximum_retry_delay_ms: Option<u64>,
        maximum_attempts: Option<u64>,
    ) -> Result<RecognitionTiming, Error> {
        let mut timing = self.recognition_timing(
            sample_rate,
            minimum_sample_fallback_ms,
            default_retry_interval_ms,
            request_timeout_ms,
            maximum_retry_delay_ms,
            maximum_attempts,
        )?;
        // Native TaggingType 2's secondsToRecord branch returns sampleSeconds
        // directly; it does not use progressiveRecordingSeconds.
        timing.initial_sample_ms = timing.maximum_sample_ms;
        timing.session_timeout_ms = Some(
            self.auto_session_timeout_ms
                .unwrap_or(NATIVE_TAGGING_TYPE_2_DEFAULT_SESSION_TIMEOUT_MS),
        );
        Ok(timing)
    }
}

impl ShazamConfiguration {
    /// Optional UI page-link template from `settings.webtrackurl`. This is
    /// separate from `tag_url_template()`, which is an internal recognition
    /// POST route. The value is not validated as a safe URL and is not
    /// expanded with a track identifier; those are UI responsibilities.
    pub fn web_track_url_template(&self) -> Option<&str> {
        self.web_track_url_template.as_deref()
    }

    /// Project only the UI-safe page-link input. `generation` belongs to the
    /// caller's Config/session lifecycle and is not allocated by the parser.
    pub fn ui_config_snapshot(&self, generation: u64) -> UiConfigSnapshot {
        UiConfigSnapshot {
            generation,
            web_track_url_template: self.web_track_url_template.clone(),
        }
    }

    pub fn resolve_tag_url(&self, tag_id: &str, streaming_provider: &str) -> Result<String, Error> {
        resolve_shazam_tag_url(
            &self.tag_url_template,
            &self.installation_id,
            tag_id,
            streaming_provider,
        )
    }

    /// Resolve the foreground Auto Shazam endpoint selected by Native for
    /// continuous matching. The route remains optional because Manual Config
    /// validity does not require an Auto endpoint.
    pub fn resolve_continuous_tag_url(
        &self,
        tag_id: &str,
        streaming_provider: &str,
    ) -> Result<String, Error> {
        self.resolve_continuous_tag_url_for_context(
            NativeTaggingRequestContext::Foreground,
            tag_id,
            streaming_provider,
        )
    }

    /// Resolve the Config route selected by Native's `inForegroundForTagging`
    /// argument. A missing route is an error; the caller's state is never
    /// replaced with a guessed foreground/background default.
    pub fn resolve_continuous_tag_url_for_context(
        &self,
        context: NativeTaggingRequestContext,
        tag_id: &str,
        streaming_provider: &str,
    ) -> Result<String, Error> {
        let template = match context {
            NativeTaggingRequestContext::Foreground => {
                self.continuous_foreground_url_template.as_deref()
            }
            NativeTaggingRequestContext::Background => {
                self.continuous_background_url_template.as_deref()
            }
        }
        .ok_or_else(|| match context {
            NativeTaggingRequestContext::Foreground => {
                Error::BadResponse("Config has no Auto foreground route".into())
            }
            NativeTaggingRequestContext::Background => {
                Error::BadResponse("Config has no Auto background route".into())
            }
        })?;
        resolve_shazam_continuous_tag_url(
            template,
            &self.installation_id,
            tag_id,
            streaming_provider,
        )
    }

    /// Resolve the Native endpoint selected for the requested tagging type.
    /// Manual keeps the strict `apis.tag.href` route. Auto requires explicit
    /// Native foreground/background context and selects the matching route.
    pub fn resolve_tag_url_for_type(
        &self,
        tagging_type: NativeTaggingType,
        context: Option<NativeTaggingRequestContext>,
        tag_id: &str,
        streaming_provider: &str,
    ) -> Result<String, Error> {
        match tagging_type {
            NativeTaggingType::Manual => self.resolve_tag_url(tag_id, streaming_provider),
            NativeTaggingType::Auto => self.resolve_continuous_tag_url_for_context(
                context.ok_or_else(|| {
                    Error::InvalidRequest(
                        "Auto recognition requires explicit foreground/background context".into(),
                    )
                })?,
                tag_id,
                streaming_provider,
            ),
        }
    }

    pub fn tag_url_template(&self) -> &str {
        &self.tag_url_template
    }

    pub fn manual_timing(&self) -> ManualTaggingTiming {
        self.manual_timing
    }

    /// Frame thresholds for Native manual TaggingType 1, computed directly
    /// from the original Config Double values as `sampleRate * seconds`,
    /// truncated toward zero like `SHKSignatureGenerator`.
    pub fn manual_native_frame_targets(
        &self,
        sample_rate: u32,
    ) -> Result<NativeSampleFrameTargets, Error> {
        self.native_frame_targets_for_type(NativeTaggingType::Manual, sample_rate)
    }

    /// Frame thresholds selected by Native `secondsToRecord` for the active
    /// tagging type. Manual Type 1 uses `progressiveRecordingSeconds` with a
    /// `sampleSeconds` fallback; Auto Type 2 uses `sampleSeconds` for both its
    /// initial target and per-cycle maximum. Conversion uses the original
    /// Config Double and truncates toward zero at the active sample rate.
    pub fn native_frame_targets_for_type(
        &self,
        tagging_type: NativeTaggingType,
        sample_rate: u32,
    ) -> Result<NativeSampleFrameTargets, Error> {
        let sample_seconds = self
            .native_frame_seconds
            .sample_seconds_bits
            .map(f64::from_bits)
            .ok_or_else(|| Error::BadResponse("Config has no usable sampleSeconds".into()))?;
        let initial_seconds = match tagging_type {
            NativeTaggingType::Manual => self
                .native_frame_seconds
                .progressive_seconds_bits
                .map(f64::from_bits)
                .unwrap_or(sample_seconds),
            NativeTaggingType::Auto => sample_seconds,
        };
        let maximum_frames = native_frames_from_seconds(sample_rate, sample_seconds)?;
        let initial_frames = native_frames_from_seconds(sample_rate, initial_seconds)?;
        if maximum_frames == 0 || initial_frames == 0 || initial_frames > maximum_frames {
            return Err(Error::BadResponse(
                "Config Native frame targets are zero or inconsistent".into(),
            ));
        }
        Ok(NativeSampleFrameTargets {
            initial_frames,
            maximum_frames,
        })
    }

    /// Serialize the verified Native inputs and resolved Manual Type 1
    /// controls for an external UI host. This deliberately omits the Config
    /// route and installation identifier. `pre_record_capacity_frames` is
    /// the capacity actually selected by the capture adapter, which may be a
    /// diagnostic override rather than the Native page-rounded value.
    pub fn native_manual_ui_config_json(
        &self,
        sample_rate_hz: u32,
        pre_record_capacity_frames: usize,
        diagnostic_pre_record_override: bool,
        request_timeout_ms: u64,
    ) -> Result<String, Error> {
        let timing = self.manual_timing.recognition_timing(
            sample_rate_hz,
            NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
            shazam_progressive::NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS,
            request_timeout_ms,
            None,
            None,
        )?;
        let targets = self.manual_native_frame_targets(sample_rate_hz)?;
        let sample_seconds = self
            .native_frame_seconds
            .sample_seconds_bits
            .map(f64::from_bits)
            .ok_or_else(|| Error::BadResponse("Config has no usable sampleSeconds".into()))?;
        let progressive_seconds = self
            .native_frame_seconds
            .progressive_seconds_bits
            .map(f64::from_bits);
        let effective_initial_seconds = progressive_seconds.unwrap_or(sample_seconds);
        let effective_pre_record_seconds =
            self.manual_timing.effective_pre_record_buffer_ms() / 1_000;
        let pre_record_capacity_ms =
            frames_to_ceil_milliseconds(pre_record_capacity_frames, sample_rate_hz)?;

        Ok(json!({
            "schema": "openshazam.manual-ui-config.v1",
            "event": "session_config",
            "native_ui_start_inputs": {
                "tagging_type": 1,
                "debugging_enabled": false
            },
            "native_origin_observations": [{
                "code": 14,
                "label": "webbridge",
                "scope": "one_observed_main_view_callsite"
            }],
            "native_controls": {
                "use_buffered_audio": true,
                "default_retry_interval_ms": timing.default_retry_interval_ms,
                "pre_record": {
                    "configured_integer_seconds": self.manual_timing.pre_record_buffer_ms.map(|ms| ms / 1_000),
                    "effective_seconds": effective_pre_record_seconds,
                    "capacity_frames": pre_record_capacity_frames,
                    "capacity_ms_ceil": pre_record_capacity_ms,
                    "capacity_source": if diagnostic_pre_record_override { "diagnostic_override" } else { "native_page_rounded" }
                }
            },
            "sigx": {
                "signature_type": 2,
                "signature_options": 0,
                "input": {
                    "sample_rate_hz": sample_rate_hz,
                    "channels": 1,
                    "sample_format": "mono_float32"
                }
            },
            "timing": {
                "config_sample_seconds": sample_seconds,
                "config_progressive_seconds": progressive_seconds,
                "effective_initial_signature_seconds": effective_initial_seconds,
                "initial_signature_target_frames": targets.initial_frames,
                "maximum_recording_frames": targets.maximum_frames,
                "initial_signature_target_ms": timing.initial_sample_ms,
                "maximum_recording_ms": timing.maximum_sample_ms,
                "minimum_signature_config_ms": self.manual_timing.minimum_signature_ms,
                "minimum_signature_effective_ms": timing.minimum_signature_ms,
                "maximum_signature_config_ms": self.manual_timing.maximum_signature_ms,
                "maximum_signature_effective_ms": self.manual_timing.effective_maximum_signature_length_ms(),
                "session_timeout_config_ms": self.manual_timing.session_timeout_ms,
                "session_timeout_effective_ms": timing.session_timeout_ms,
                "request_timeout_ms": request_timeout_ms
            }
        }).to_string())
    }

    /// Serialize the observed Auto Type 2 controls and the caller-supplied
    /// foreground/background snapshot for an external UI host. The context is
    /// deliberately passed by the caller rather than inferred by this crate.
    /// Config URLs and the installation identifier are omitted.
    pub fn native_auto_ui_config_json(
        &self,
        sample_rate_hz: u32,
        request_context: NativeTaggingRequestContext,
        request_timeout_ms: u64,
    ) -> Result<String, Error> {
        let timing = self.manual_timing.auto_recognition_timing(
            sample_rate_hz,
            NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
            shazam_progressive::NATIVE_TAGGING_TYPE_2_DEFAULT_INTERVAL_MS,
            request_timeout_ms,
            None,
            None,
        )?;
        let targets =
            self.native_frame_targets_for_type(NativeTaggingType::Auto, sample_rate_hz)?;
        let sample_seconds = self
            .native_frame_seconds
            .sample_seconds_bits
            .map(f64::from_bits)
            .ok_or_else(|| Error::BadResponse("Config has no usable sampleSeconds".into()))?;
        let progressive_seconds = self
            .native_frame_seconds
            .progressive_seconds_bits
            .map(f64::from_bits);

        Ok(json!({
            "schema": "openshazam.auto-ui-config.v1",
            "event": "session_config",
            "caller_inputs": {
                "tagging_type": 2,
                "request_context": match request_context {
                    NativeTaggingRequestContext::Foreground => "foreground",
                    NativeTaggingRequestContext::Background => "background",
                }
            },
            "native_controls": {
                "use_buffered_audio": false,
                "default_retry_interval_ms": timing.default_retry_interval_ms,
                "pre_record_enabled": false
            },
            "timing": {
                "config_sample_seconds": sample_seconds,
                "config_progressive_seconds": progressive_seconds,
                "progressive_seconds_applied": false,
                "initial_signature_target_frames": targets.initial_frames,
                "maximum_recording_frames": targets.maximum_frames,
                "initial_signature_target_ms": timing.initial_sample_ms,
                "maximum_recording_ms": timing.maximum_sample_ms,
                "minimum_signature_config_ms": self.manual_timing.minimum_signature_ms,
                "minimum_signature_effective_ms": timing.minimum_signature_ms,
                "maximum_signature_config_ms": self.manual_timing.maximum_signature_ms,
                "maximum_signature_effective_ms": self.manual_timing.effective_maximum_signature_length_ms(),
                "session_timeout_config_ms": self.manual_timing.auto_session_timeout_ms,
                "session_timeout_effective_ms": timing.session_timeout_ms,
                "request_timeout_ms": request_timeout_ms
            }
        }).to_string())
    }
}

fn frames_to_ceil_milliseconds(frames: usize, sample_rate_hz: u32) -> Result<u64, Error> {
    if sample_rate_hz == 0 {
        return Err(Error::InvalidRequest(
            "capture sample rate must be nonzero".into(),
        ));
    }
    let numerator = (frames as u128)
        .checked_mul(1_000)
        .and_then(|value| value.checked_add(u128::from(sample_rate_hz) - 1))
        .ok_or_else(|| Error::InvalidRequest("capture duration overflow".into()))?;
    u64::try_from(numerator / u128::from(sample_rate_hz))
        .map_err(|_| Error::InvalidRequest("capture duration is too large".into()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidSignature(String),
    InvalidEndpoint(String),
    InvalidRequest(String),
    Transport(String),
    /// The HTTPS client started, but the recognition request completed with a
    /// network-level failure (for example DNS/TLS/timeout). Native routes
    /// these through a matcher result with no tag dictionary, which can carry
    /// its implicit Retry-to-maximum directive.
    NetworkFailure(String),
    BadResponse(String),
    NoMatches,
}

/// Replace the three placeholders used by the Mac tagging endpoint builder.
///
/// The installed binary calls `stringByReplacingOccurrencesOfString` on the
/// configured URL string for these values. This mirrors that literal
/// replacement behavior; it does not invent a URL, add query parameters, or
/// URL-encode replacement values.
pub fn substitute_endpoint_tokens(
    template: &str,
    tag_id: &str,
    streaming_provider: &str,
    installation_id: &str,
) -> String {
    template
        .replace("{tagid}", tag_id)
        .replace("{streamingprovider}", streaming_provider)
        .replace("{inid}", installation_id)
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSignature(message)
            | Self::InvalidEndpoint(message)
            | Self::InvalidRequest(message)
            | Self::Transport(message)
            | Self::NetworkFailure(message)
            | Self::BadResponse(message) => f.write_str(message),
            Self::NoMatches => f.write_str("response contained no usable track candidates"),
        }
    }
}

impl std::error::Error for Error {}

/// Build the observed Config bootstrap URL. The response supplies both the
/// `apis.tag.href` template and its INID; this function does not embed the
/// recognition endpoint.
pub fn build_config_url(profile: &ConfigRequestProfile) -> Result<String, Error> {
    if profile.country.is_empty()
        || !profile
            .country
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(Error::InvalidRequest(
            "Config country must be a safe country token or `-`".into(),
        ));
    }
    for (name, value) in [
        ("language", profile.language.as_str()),
        ("locale", profile.locale.as_str()),
        ("model", profile.model.as_str()),
        ("OS version", profile.os_version.as_str()),
        ("OS build", profile.os_build.as_str()),
        ("app version", profile.app_version.as_str()),
    ] {
        if value.is_empty()
            || value.len() > 128
            || value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(Error::InvalidRequest(format!(
                "Config {name} is empty, too long, or contains control characters"
            )));
        }
    }

    let pairs = [
        ("app", "SHAZAM"),
        ("l", profile.language.as_str()),
        ("locale", profile.locale.as_str()),
        ("model", profile.model.as_str()),
        ("platform", SHAZAM_MAC_PLATFORM),
        ("os", "MACOS"),
        ("osVersion", profile.os_version.as_str()),
        ("appVersion", profile.app_version.as_str()),
        ("osBuild", profile.os_build.as_str()),
    ];
    let query = pairs
        .iter()
        .map(|(key, value)| format!("{key}={}", percent_encode_query_component(value)))
        .collect::<Vec<_>>()
        .join("&");
    Ok(format!(
        "https://config.shazam.com/configuration/v1/{}/bag?{query}",
        profile.country
    ))
}

fn percent_encode_query_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if is_url_unreserved(byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// Parse the Config response without retaining any unrelated user/config data.
/// The root INID, manual route, optional Auto foreground/background routes,
/// and Native timing blocks are extracted.
pub fn parse_shazam_config(body: &[u8]) -> Result<ShazamConfiguration, Error> {
    let root: Value = serde_json::from_slice(body)
        .map_err(|_| Error::BadResponse("Config response is not valid JSON".into()))?;
    let installation_id = root
        .get("inid")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= 128 && value.bytes().all(is_url_unreserved)
        })
        .ok_or_else(|| {
            Error::BadResponse("Config response has no usable root `inid` value".into())
        })?;
    let tag_url_template = root
        .get("apis")
        .and_then(|apis| apis.get("tag"))
        .and_then(|tag| tag.get("href"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::BadResponse("Config response has no `apis.tag.href` value".into()))?;
    validate_shazam_tag_template(tag_url_template)?;
    let continuous_foreground_url_template = root
        .get("apis")
        .and_then(|apis| apis.get("autotag"))
        .and_then(|autotag| autotag.get("foregroundhref"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let continuous_background_url_template = root
        .get("apis")
        .and_then(|apis| apis.get("autotag"))
        .and_then(|autotag| autotag.get("backgroundhref"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    // This optional UI value has no bearing on the recognition route and must
    // never make an otherwise valid recognition Config unusable.
    // The live Config bag may encode webtrackurl as {"href": "..."}.
    // Retain compatibility with older bags that use a plain string.
    let web_track_url_template = root
        .get("settings")
        .and_then(|settings| settings.get("webtrackurl"))
        .and_then(|entry| {
            entry
                .as_str()
                .or_else(|| entry.get("href").and_then(Value::as_str))
        })
        .filter(|value| !value.is_empty() && value.len() <= MAX_WEB_TRACK_URL_TEMPLATE_BYTES)
        .map(str::to_owned);
    Ok(ShazamConfiguration {
        tag_url_template: tag_url_template.to_owned(),
        web_track_url_template,
        continuous_foreground_url_template,
        continuous_background_url_template,
        installation_id: installation_id.to_owned(),
        manual_timing: parse_manual_tagging_timing(&root),
        native_frame_seconds: parse_native_frame_seconds(&root),
    })
}

fn parse_native_frame_seconds(root: &Value) -> NativeFrameSeconds {
    let tag_block = root.get("apis").and_then(|apis| apis.get("tag"));
    NativeFrameSeconds {
        sample_seconds_bits: config_number(tag_block, "sampleseconds").map(f64::to_bits),
        progressive_seconds_bits: config_number(tag_block, "progressiveseconds").map(f64::to_bits),
    }
}

fn native_frames_from_seconds(sample_rate: u32, seconds: f64) -> Result<usize, Error> {
    if sample_rate == 0 || !seconds.is_finite() || seconds < 0.0 {
        return Err(Error::BadResponse(
            "Config Native sample duration or sample rate is invalid".into(),
        ));
    }
    let frames = f64::from(sample_rate) * seconds;
    if !frames.is_finite() || frames >= usize::MAX as f64 {
        return Err(Error::BadResponse(
            "Config Native sample duration exceeds addressable PCM".into(),
        ));
    }
    Ok(frames as usize)
}

fn parse_manual_tagging_timing(root: &Value) -> ManualTaggingTiming {
    // SHNTaggingConfiguration's sample/min/max getters read its `tagBlock`,
    // which resolves config.apis.tag. Both Manual Type 1 and Auto Type 2
    // timeoutSeconds getters resolve config.apis.autotag's
    // continuousForegroundIdleTimeout. `settings.recording.maxTagSeconds` is
    // selected by the separate Type 7 getter branch, not these Progressive
    // modes; Type 7's caller/purpose is not established here.
    // Avoid recursive key search: the same field names can occur in unrelated
    // route/config blocks.
    let tag_block = root.get("apis").and_then(|apis| apis.get("tag"));
    let recording_block = root
        .get("settings")
        .and_then(|settings| settings.get("recording"));
    let auto_tag_block = root.get("apis").and_then(|apis| apis.get("autotag"));
    ManualTaggingTiming {
        sample_ms: config_number(tag_block, "sampleseconds").and_then(seconds_to_ms),
        progressive_ms: config_number(tag_block, "progressiveseconds").and_then(seconds_to_ms),
        pre_record_buffer_ms: config_number(recording_block, "silentrecordingseconds")
            .and_then(native_integer_seconds_to_ms),
        minimum_signature_ms: config_number(tag_block, "minsamplems").and_then(number_to_ms),
        maximum_signature_ms: config_number(tag_block, "maxsamplems").and_then(number_to_ms),
        session_timeout_ms: config_number(auto_tag_block, "continuousforegroundidletimeout")
            .and_then(timeout_seconds_to_ms),
        auto_session_timeout_ms: config_number(auto_tag_block, "continuousforegroundidletimeout")
            .and_then(timeout_seconds_to_ms),
    }
}

fn config_number(block: Option<&Value>, key: &str) -> Option<f64> {
    block?
        .as_object()?
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))?
        .1
        .as_f64()
        .filter(|number| number.is_finite())
}

fn seconds_to_ms(seconds: f64) -> Option<u64> {
    if !(0.0..=600.0).contains(&seconds) {
        return None;
    }
    let millis = seconds * 1_000.0;
    (millis.is_finite() && millis <= u64::MAX as f64).then_some(millis.round() as u64)
}

/// `SHNTaggingConfiguration.silentRecordingSeconds` uses NSNumber.integerValue
/// before constructing `SHREAudioRecorderConfiguration`; unlike the tagging
/// thresholds, fractional seconds are truncated rather than rounded.
fn native_integer_seconds_to_ms(seconds: f64) -> Option<u64> {
    if !(0.0..=600.0).contains(&seconds) {
        return None;
    }
    Some((seconds.trunc() as u64) * 1_000)
}

/// Project Native `intervalDuration` onto the controller's millisecond clock.
/// Shazam multiplies seconds by 1e9, truncates to signed nanoseconds, then
/// passes that duration to `dispatch_after`. Ceiling here prevents a restart
/// earlier than the Native delay when the controller can observe only ms.
fn native_interval_seconds_to_controller_ms(seconds: f64) -> Option<u64> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    let nanoseconds = (seconds * 1_000_000_000.0) as i64;
    u64::try_from(nanoseconds)
        .ok()
        .map(|ns| ns.div_ceil(1_000_000))
}

/// Session deadlines are not audio-recording durations. The Native getters
/// return the configured `NSNumber` for `maxTagSeconds` and
/// `continuousForegroundIdleTimeout` without the 600-second bound used for
/// sample-length fields; keep any finite, nonnegative value representable in
/// the controller's millisecond clock instead of silently falling back.
fn timeout_seconds_to_ms(seconds: f64) -> Option<u64> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    let millis = (seconds * 1_000.0).round();
    (millis.is_finite() && millis >= 0.0 && millis < u64::MAX as f64).then_some(millis as u64)
}

fn number_to_ms(millis: f64) -> Option<u64> {
    if !(0.0..=600_000.0).contains(&millis) {
        return None;
    }
    (millis.is_finite() && millis <= u64::MAX as f64).then_some(millis.round() as u64)
}

/// Decode explicit retry fields handled by the Mac 2.11 ARM64 response parser.
/// Without request context, this function cannot model Native's implicit
/// retry-to-maximum branch; production Progressive paths should call
/// `retry_directive_for_signature_from_response`.
pub fn retry_directive_from_response(body: &[u8]) -> Result<Option<RetryDirective>, Error> {
    let root: Value = serde_json::from_slice(body)
        .map_err(|_| Error::BadResponse("recognition response is not valid JSON".into()))?;
    Ok(explicit_retry_directive(&root))
}

/// Decode a response Retry using the same precedence as Mac 2.11. For a
/// present tag-response dictionary, explicit `retryms` or
/// `sampleseconds`/`intervalseconds` fields win regardless of current length.
/// The implicit zero-interval Retry to `maximum_signature_length_ms` applies
/// only when no tag-response dictionary is available and the current
/// Signature is shorter.
pub fn retry_directive_for_signature_from_response(
    body: &[u8],
    current_signature_length_ms: u64,
    maximum_signature_length_ms: u64,
) -> Result<Option<RetryDirective>, Error> {
    let root: Value = serde_json::from_slice(body)
        .map_err(|_| Error::BadResponse("recognition response is not valid JSON".into()))?;
    if let Some(retry) = explicit_retry_ms(&root) {
        return Ok(Some(retry));
    }
    if root.is_object() {
        return Ok(explicit_interval_retry(&root));
    }
    Ok(native_retry_to_maximum_if_signature_is_shorter(
        current_signature_length_ms,
        maximum_signature_length_ms,
    ))
}

/// Native `SHKNetworkMatcher.retryRequestFromTagResponse:matcherRequest:`
/// (ShazamTagging 2.11.0, 0x6200) constructs this Retry when a matcher
/// completion has no usable tag-response dictionary, including a failed
/// request, provided the submitted Signature is shorter than the request
/// maximum. The failure callback then applies it through the bridge when the
/// app delegate allows retry.
pub fn native_retry_to_maximum_if_signature_is_shorter(
    current_signature_length_ms: u64,
    maximum_signature_length_ms: u64,
) -> Option<RetryDirective> {
    (current_signature_length_ms < maximum_signature_length_ms).then_some(RetryDirective {
        retry_duration_ms: Some(maximum_signature_length_ms),
        interval_ms: Some(0),
        retry_duration_seconds_bits: None,
        interval_seconds_bits: None,
    })
}

/// Reproduce the Native no-tag-response failure path for a request that was
/// actually started. Cancellation and local setup failures do not reach this
/// matcher callback and therefore must not synthesize a Retry.
pub fn native_retry_for_network_failure(
    error: &Error,
    current_signature_length_ms: u64,
    maximum_signature_length_ms: u64,
    cancelled: bool,
) -> Option<RetryDirective> {
    if cancelled || !matches!(error, Error::NetworkFailure(_)) {
        return None;
    }
    native_retry_to_maximum_if_signature_is_shorter(
        current_signature_length_ms,
        maximum_signature_length_ms,
    )
}

/// Apply the observed network-failure Retry policy for the active Native
/// tagging type. Ordinary Manual Type 1 synthesizes a Retry to the configured
/// maximum for a dispatched short Signature; Auto Type 2 keeps its enclosing
/// context alive without forwarding a synthetic Retry to the SignaturePipe.
pub fn native_retry_for_network_failure_for_tagging_type(
    tagging_type: NativeTaggingType,
    error: &Error,
    current_signature_length_ms: u64,
    maximum_signature_length_ms: u64,
    cancelled: bool,
) -> Option<RetryDirective> {
    if tagging_type != NativeTaggingType::Manual {
        return None;
    }
    native_retry_for_network_failure(
        error,
        current_signature_length_ms,
        maximum_signature_length_ms,
        cancelled,
    )
}

fn explicit_retry_directive(root: &Value) -> Option<RetryDirective> {
    explicit_retry_ms(root).or_else(|| explicit_interval_retry(root))
}

fn explicit_retry_ms(root: &Value) -> Option<RetryDirective> {
    if let Some(retry_ms) = root
        .get("retryms")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0)
    {
        return Some(RetryDirective {
            retry_duration_ms: retry_milliseconds_for_display(retry_ms),
            interval_ms: Some(0),
            // ShazamFoundation converts `retryms` by dividing the original
            // NSNumber Double by 1000.0; the SignatureGenerator later
            // multiplies those seconds by sampleRate and truncates frames.
            retry_duration_seconds_bits: Some((retry_ms / 1_000.0).to_bits()),
            interval_seconds_bits: Some(0.0_f64.to_bits()),
        });
    }
    None
}

fn retry_milliseconds_for_display(milliseconds: f64) -> Option<u64> {
    let rounded = milliseconds.round();
    (rounded.is_finite() && rounded >= 0.0 && rounded < u64::MAX as f64).then_some(rounded as u64)
}

fn explicit_interval_retry(root: &Value) -> Option<RetryDirective> {
    root.get("intervalseconds")?;

    let sample_seconds = root
        .get("sampleseconds")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .filter(|value| (0.0..=600.0).contains(value));
    let sample_ms = sample_seconds.and_then(seconds_to_ms).unwrap_or(0);
    let interval_seconds = root
        .get("intervalseconds")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(0.0);
    let interval_ms = native_interval_seconds_to_controller_ms(interval_seconds).unwrap_or(0);

    Some(RetryDirective {
        retry_duration_ms: Some(sample_ms),
        interval_ms: Some(interval_ms),
        retry_duration_seconds_bits: sample_seconds.map(f64::to_bits),
        interval_seconds_bits: Some(interval_seconds.to_bits()),
    })
}

/// Convert the response status/body into the minimal state transition used by
/// the progressive controller. Unknown retry fields do not force sampling or
/// waiting; callers fall back to their bounded local policy.
pub fn interpret_progressive_http_response(status: u16, body: &[u8]) -> ProgressiveHttpResponse {
    interpret_progressive_http_response_for_signature(status, body, None, None)
}

/// Context-aware Progressive response classifier used by live attempts. The
/// Signature/max pair is omitted only by tests exercising body-only parsing.
/// Static ShazamTagging ARM64 evidence: `SHKTaggingNetworkRequest`
/// `parseNetworkResponse:error:completion:` at `0x17a04` routes only HTTP
/// 200–299 through `SHKTag` dictionary parsing; other statuses use
/// `networkRequestFailedWithResponse:error:completion:`. The failure path
/// supplies no tag-response dictionary, so `SHKNetworkMatcher` at `0x6200`
/// may synthesize a zero-interval Retry to the request maximum when the
/// submitted Signature is shorter. A non-2xx body’s `retryms` is therefore
/// not interpreted as a tag Retry.
pub fn interpret_progressive_http_response_for_signature(
    status: u16,
    body: &[u8],
    current_signature_length_ms: Option<u64>,
    maximum_signature_length_ms: Option<u64>,
) -> ProgressiveHttpResponse {
    interpret_progressive_http_response_for_tagging_type(
        status,
        body,
        current_signature_length_ms,
        maximum_signature_length_ms,
        NativeTaggingType::Manual,
    )
}

/// Interpret a progressive HTTP response using the response-to-tag path for
/// the active Native tagging type. Auto Type 2 builds its ordered tags from
/// root `matches`; Manual Type 1 keeps its root `track` bridge.
pub fn interpret_progressive_http_response_for_tagging_type(
    status: u16,
    body: &[u8],
    current_signature_length_ms: Option<u64>,
    maximum_signature_length_ms: Option<u64>,
    tagging_type: NativeTaggingType,
) -> ProgressiveHttpResponse {
    let retry_for_response = || {
        match (current_signature_length_ms, maximum_signature_length_ms) {
            (Some(current), Some(maximum)) => {
                retry_directive_for_signature_from_response(body, current, maximum)
            }
            _ => retry_directive_from_response(body),
        }
        .ok()
        .flatten()
    };
    if !(200..300).contains(&status) {
        let retry = current_signature_length_ms
            .zip(maximum_signature_length_ms)
            .and_then(|(current, maximum)| {
                native_retry_to_maximum_if_signature_is_shorter(current, maximum)
            });
        return ProgressiveHttpResponse::HttpError { status, retry };
    }

    let parsed_candidates = match tagging_type {
        NativeTaggingType::Manual => parse_track_candidates(body),
        NativeTaggingType::Auto => parse_auto_track_candidates(body),
    };
    match parsed_candidates {
        Ok(candidates) if !candidates.is_empty() => ProgressiveHttpResponse::Matched {
            candidates,
            retry: retry_for_response(),
        },
        Ok(_) | Err(Error::NoMatches) => ProgressiveHttpResponse::NoMatch(retry_for_response()),
        Err(_) if serde_json::from_slice::<Value>(body).is_ok_and(|value| value.is_object()) => {
            // A valid JSON dictionary with an unfamiliar/no-track shape is a
            // parsed tag response, not the Native nil-tag-response error
            // path. Keep only its explicit retry fields.
            ProgressiveHttpResponse::NoMatch(retry_for_response())
        }
        Err(_) => {
            let retry = current_signature_length_ms
                .zip(maximum_signature_length_ms)
                .and_then(|(current, maximum)| {
                    native_retry_to_maximum_if_signature_is_shorter(current, maximum)
                });
            ProgressiveHttpResponse::ParseFailure { retry }
        }
    }
}

/// Resolve a Config-provided Mac tag template while keeping the route
/// allowlist and token validation in force.
pub fn resolve_shazam_tag_url(
    template: &str,
    installation_id: &str,
    tag_id: &str,
    streaming_provider: &str,
) -> Result<String, Error> {
    validate_shazam_tag_template(template)?;
    for (name, value) in [("INID", installation_id), ("tag ID", tag_id)] {
        if value.is_empty() || !value.bytes().all(is_url_unreserved) {
            return Err(Error::InvalidEndpoint(format!(
                "Config {name} is not a safe URL path token"
            )));
        }
    }
    if !streaming_provider.bytes().all(is_url_unreserved) {
        return Err(Error::InvalidEndpoint(
            "streaming provider is not a safe URL query token".into(),
        ));
    }
    let endpoint =
        substitute_endpoint_tokens(template, tag_id, streaming_provider, installation_id);
    validate_shazam_endpoint(&endpoint)?;
    Ok(endpoint)
}

/// Resolve a Config-provided endpoint used by Native continuous matching.
/// Only the first-party AMP host is accepted. The Native URL getter replaces
/// the INID, while the request builder may also replace tag ID and streaming-provider tokens when present.
pub fn resolve_shazam_continuous_tag_url(
    template: &str,
    installation_id: &str,
    tag_id: &str,
    streaming_provider: &str,
) -> Result<String, Error> {
    validate_continuous_tag_template(template)?;
    for (name, value) in [("INID", installation_id), ("tag ID", tag_id)] {
        if value.is_empty() || !value.bytes().all(is_url_unreserved) {
            return Err(Error::InvalidEndpoint(format!(
                "Config {name} is not a safe URL path token"
            )));
        }
    }
    if !streaming_provider.bytes().all(is_url_unreserved) {
        return Err(Error::InvalidEndpoint(
            "streaming provider is not a safe URL query token".into(),
        ));
    }
    let endpoint =
        substitute_endpoint_tokens(template, tag_id, streaming_provider, installation_id);
    validate_continuous_shazam_endpoint(&endpoint)?;
    Ok(endpoint)
}

fn validate_shazam_tag_template(template: &str) -> Result<(), Error> {
    if template.matches("{inid}").count() != 1
        || template.matches("{tagid}").count() != 1
        || template.matches("{streamingprovider}").count() > 1
    {
        return Err(Error::InvalidEndpoint(
            "Config tag template has missing or duplicate placeholders".into(),
        ));
    }
    let sanitized = template
        .replace("{inid}", "installation-id")
        .replace("{tagid}", "tag-id")
        .replace("{streamingprovider}", "");
    if sanitized.contains(['{', '}']) {
        return Err(Error::InvalidEndpoint(
            "Config tag template contains an unknown placeholder".into(),
        ));
    }
    validate_shazam_endpoint(&sanitized)
}

fn validate_continuous_tag_template(template: &str) -> Result<(), Error> {
    if template.matches("{inid}").count() != 1
        || template.matches("{tagid}").count() > 1
        || template.matches("{streamingprovider}").count() > 1
    {
        return Err(Error::InvalidEndpoint(
            "Config Auto continuous template has missing or duplicate placeholders".into(),
        ));
    }
    let sanitized = template
        .replace("{inid}", "installation-id")
        .replace("{tagid}", "tag-id")
        .replace("{streamingprovider}", "");
    if sanitized.contains(['{', '}']) {
        return Err(Error::InvalidEndpoint(
            "Config Auto continuous template contains an unknown placeholder".into(),
        ));
    }
    validate_continuous_shazam_endpoint(&sanitized)
}

/// Construct the statically observed Mac tagging body shape.
///
/// The required fields and data-URI prefix were found in the installed Mac
/// app's request builder. Optional context/origin values and the service's
/// acceptance of this independently constructed body remain unverified.
pub fn build_request_body(
    signature: &[u8],
    sample_ms: u64,
    timestamp_ms: u64,
    timezone: &str,
    context: Value,
    geolocation: Value,
    third_party: Option<Value>,
) -> Result<Vec<u8>, Error> {
    sigx_format::Signature::parse(signature)
        .map_err(|error| Error::InvalidSignature(error.to_string()))?;
    if timezone.is_empty() {
        return Err(Error::InvalidRequest("timezone must not be empty".into()));
    }

    let mut request = json!({
        "signature": {
            "uri": format!("{SIGNATURE_DATA_URI}{}", STANDARD.encode(signature)),
            "samplems": sample_ms,
        },
        "context": context,
        "geolocation": geolocation,
        "timestamp": timestamp_ms,
        "timezone": timezone,
    });
    if let Some(origin) = third_party {
        request["thirdparty"] = origin;
    }
    serde_json::to_vec(&request)
        .map_err(|error| Error::InvalidRequest(format!("serialize request body: {error}")))
}

/// POST JSON to a loopback-only HTTP endpoint for local mock testing.
///
/// HTTPS, hostnames, proxies, redirects, and non-loopback destinations are
/// intentionally unsupported. This is not suitable for a live Shazam request.
pub fn post_loopback_json(endpoint: &str, body: &[u8]) -> Result<HttpResponse, Error> {
    let (address, path, authority) = parse_loopback_endpoint(endpoint)?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .map_err(|error| Error::Transport(format!("connect to local mock: {error}")))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| Error::Transport(format!("set mock read timeout: {error}")))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| Error::Transport(format!("set mock write timeout: {error}")))?;

    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nAccept: application/json\r\nX-Shazam-Platform: {SHAZAM_MAC_PLATFORM}\r\nX-Shazam-AppVersion: {SHAZAM_MAC_APP_VERSION}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .and_then(|()| stream.write_all(body))
    .map_err(|error| Error::Transport(format!("write request to local mock: {error}")))?;

    let mut response = Vec::new();
    stream
        .take((MAX_RESPONSE_BYTES + MAX_HEADER_BYTES + 1) as u64)
        .read_to_end(&mut response)
        .map_err(|error| Error::Transport(format!("read local mock response: {error}")))?;
    parse_http_response(&response)
}

/// POST to an observed Config-provided Mac recognition route over verified
/// HTTPS. Manual routes use strict template validation; Auto continuous
/// routes are restricted to `amp.shazam.com` and safe URL characters.
///
/// This allows only the Shazam `amp.shazam.com` host. Manual routes use the
/// observed path/query allowlist; Auto routes use a separate URL-safety check.
/// It sends no cookies or AMP token, does not use a proxy or follow redirects,
/// and requires a locally installed curl.
/// Call only when you are authorized to use this first-party endpoint.
pub fn post_shazam_json(endpoint: &str, body: &[u8]) -> Result<HttpResponse, Error> {
    post_shazam_json_with_timeout(endpoint, body, Duration::from_secs(20))
}

/// POST the observed Mac manual-tag request with a caller-selected local
/// timeout. The timeout controls the curl process only; server retry timing is
/// a separate value carried by `RetryDirective`.
pub fn post_shazam_json_with_timeout(
    endpoint: &str,
    body: &[u8],
    timeout: Duration,
) -> Result<HttpResponse, Error> {
    post_shazam_json_with_timeout_cancellable(endpoint, body, timeout, &AtomicBool::new(false))
}

/// Cancellable variant used by streaming capture adapters. The curl process
/// is terminated when `cancelled` becomes true; stdout is drained separately
/// and capped so the child cannot block on a full pipe or grow memory without
/// bound.
pub fn post_shazam_json_with_timeout_cancellable(
    endpoint: &str,
    body: &[u8],
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<HttpResponse, Error> {
    post_shazam_json_with_timeout_cancellable_and_dispatch(
        endpoint,
        body,
        timeout,
        cancelled,
        || {},
    )
}

/// Cancellable HTTPS POST with a callback after the local HTTPS client
/// process is spawned. This marks transport handoff, not remote receipt.
pub fn post_shazam_json_with_timeout_cancellable_and_dispatch<F>(
    endpoint: &str,
    body: &[u8],
    timeout: Duration,
    cancelled: &AtomicBool,
    on_transport_started: F,
) -> Result<HttpResponse, Error>
where
    F: FnOnce(),
{
    post_shazam_json_with_transport(
        &CurlRecognitionTransport,
        endpoint,
        body,
        timeout,
        cancelled,
        on_transport_started,
    )
}

/// Send a validated recognition POST through an explicitly selected
/// transport. The callback indicates local transport startup, not remote
/// receipt. Current production paths use [`CurlRecognitionTransport`].
pub fn post_shazam_json_with_transport<T, F>(
    transport: &T,
    endpoint: &str,
    body: &[u8],
    timeout: Duration,
    cancelled: &AtomicBool,
    on_transport_started: F,
) -> Result<HttpResponse, Error>
where
    T: RecognitionHttpTransport + ?Sized,
    F: FnOnce(),
{
    if timeout.is_zero() || timeout > Duration::from_secs(600) {
        return Err(Error::BadResponse(
            "HTTPS timeout must be greater than zero and at most 600 seconds".into(),
        ));
    }
    validate_shazam_request_endpoint(endpoint)?;

    let mut callback = Some(on_transport_started);
    transport.post_json_cancellable(endpoint, body, timeout, cancelled, &mut || {
        if let Some(callback) = callback.take() {
            callback();
        }
    })
}

impl RecognitionHttpTransport for CurlRecognitionTransport {
    fn post_json_cancellable(
        &self,
        endpoint: &str,
        body: &[u8],
        timeout: Duration,
        cancelled: &AtomicBool,
        on_transport_started: &mut dyn FnMut(),
    ) -> Result<HttpResponse, Error> {
        curl_post_shazam_json_with_timeout_cancellable_and_dispatch(
            endpoint,
            body,
            timeout,
            cancelled,
            on_transport_started,
        )
    }
}

fn curl_post_shazam_json_with_timeout_cancellable_and_dispatch<F>(
    endpoint: &str,
    body: &[u8],
    timeout: Duration,
    cancelled: &AtomicBool,
    on_transport_started: F,
) -> Result<HttpResponse, Error>
where
    F: FnOnce(),
{
    if timeout.is_zero() || timeout > Duration::from_secs(600) {
        return Err(Error::BadResponse(
            "HTTPS timeout must be greater than zero and at most 600 seconds".into(),
        ));
    }
    validate_shazam_request_endpoint(endpoint)?;

    let mut command = Command::new(system_curl_program()?);
    command
        .args([
            "--disable",
            "--noproxy",
            "*",
            "--silent",
            "--show-error",
            "--max-time",
        ])
        .arg(format!("{:.3}", timeout.as_secs_f64().max(0.001)))
        .args([
            "--proto",
            "=https",
            "--http1.1",
            "--header",
            "Content-Type: application/json",
            "--header",
            "Accept: application/json",
            "--header",
            "X-Shazam-Platform: MAC",
            "--header",
            "X-Shazam-AppVersion: 2.11.0",
            "--data-binary",
            "@-",
            "--write-out",
            "\n__OPENSHZAM_HTTP_STATUS__%{http_code}",
            "--url",
        ])
        .arg(endpoint)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = spawn_http_client_with_dispatch(&mut command, on_transport_started)?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Transport("HTTPS client stdout was unavailable".into()))?;
    let output_limit = MAX_RESPONSE_BYTES + CURL_STATUS_MARKER.len() + 8;
    let reader = thread::Builder::new()
        .name("openshazam-http-response-reader".into())
        .spawn(move || read_bounded_response(stdout, output_limit))
        .map_err(|_| Error::Transport("could not start the HTTPS response reader".into()))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Transport("HTTPS client stdin was unavailable".into()))?;
    if stdin.write_all(body).is_err() {
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        return Err(Error::Transport(
            "could not write the JSON request body".into(),
        ));
    }
    drop(stdin);

    let status =
        match wait_for_child_cancellable(&mut child, cancelled, "HTTPS request was cancelled") {
            Ok(status) => status,
            Err(error) => {
                let _ = reader.join();
                return Err(error);
            }
        };
    let (combined, exceeded_limit) = reader
        .join()
        .map_err(|_| Error::Transport("HTTPS response reader stopped unexpectedly".into()))?
        .map_err(|_| Error::Transport("could not read the HTTPS response".into()))?;
    if exceeded_limit {
        return Err(Error::BadResponse(
            "HTTPS response exceeded the local size limit".into(),
        ));
    }
    if !status.success() {
        return Err(Error::NetworkFailure(format!(
            "HTTPS request failed (curl exit {})",
            status.code().unwrap_or(-1)
        )));
    }

    let marker_index = combined
        .windows(CURL_STATUS_MARKER.len())
        .rposition(|window| window == CURL_STATUS_MARKER)
        .ok_or_else(|| Error::BadResponse("HTTPS client did not return a status code".into()))?;
    let status_start = marker_index + CURL_STATUS_MARKER.len();
    let status = std::str::from_utf8(&combined[status_start..])
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|value| *value != 0)
        .ok_or_else(|| Error::BadResponse("HTTPS client returned an invalid status code".into()))?;
    let response_body = combined[..marker_index].to_vec();
    if response_body.len() > MAX_RESPONSE_BYTES {
        return Err(Error::BadResponse(
            "HTTPS response exceeded the local size limit".into(),
        ));
    }
    Ok(HttpResponse {
        status,
        body: response_body,
    })
}

fn spawn_http_client_with_dispatch<F>(
    command: &mut Command,
    on_transport_started: F,
) -> Result<std::process::Child, Error>
where
    F: FnOnce(),
{
    #[cfg(windows)]
    suppress_child_console(command);
    let child = command
        .spawn()
        .map_err(|_| Error::Transport("could not start the system HTTPS client".into()))?;
    on_transport_started();
    Ok(child)
}

fn wait_for_child_cancellable(
    child: &mut std::process::Child,
    cancelled: &AtomicBool,
    cancellation_message: &'static str,
) -> Result<std::process::ExitStatus, Error> {
    loop {
        if cancelled.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Transport(cancellation_message.into()));
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Transport(
                    "could not wait for the HTTPS client".into(),
                ));
            }
        }
    }
}

fn read_bounded_response<R: Read>(mut reader: R, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut stored = Vec::with_capacity(limit.min(64 * 1024));
    let mut exceeded_limit = false;
    let mut buffer = [0u8; 8 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(stored.len());
        let kept = remaining.min(count);
        stored.extend_from_slice(&buffer[..kept]);
        if kept != count {
            // Drop the pipe promptly so the HTTPS child sees a closed stdout
            // instead of continuing to download and emit an oversized body.
            exceeded_limit = true;
            break;
        }
    }
    Ok((stored, exceeded_limit))
}

/// Resolve the operating system's HTTPS client without searching the current
/// directory or `PATH`.
///
/// On Windows this uses `GetSystemDirectoryW` and accepts only a regular
/// `curl.exe` beneath that protected directory. This prevents an unrelated
/// executable in the application's working directory from intercepting
/// recognition or update traffic.
pub fn system_curl_program() -> Result<PathBuf, Error> {
    #[cfg(windows)]
    {
        system_windows_program(&["curl.exe"], "the Windows system HTTPS client")
    }
    #[cfg(not(windows))]
    {
        Ok(PathBuf::from("/usr/bin/curl"))
    }
}

/// Resolve Windows PowerShell from the protected system directory instead of
/// searching the current directory or `PATH`.
#[cfg(windows)]
pub fn system_powershell_program() -> Result<PathBuf, Error> {
    system_windows_program(
        &["WindowsPowerShell", "v1.0", "powershell.exe"],
        "Windows PowerShell",
    )
}

#[cfg(windows)]
fn system_windows_program(
    relative_components: &[&str],
    description: &str,
) -> Result<PathBuf, Error> {
    use std::{
        ffi::OsString,
        os::windows::{ffi::OsStringExt, fs::MetadataExt},
    };
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    let mut directory = vec![0u16; 32_768];
    // SAFETY: the mutable slice supplies writable UTF-16 storage for the
    // synchronous Win32 call, and the returned length is checked before use.
    let length = unsafe { GetSystemDirectoryW(Some(&mut directory)) } as usize;
    if length == 0 || length >= directory.len() {
        return Err(Error::Transport(
            "Windows could not resolve the system directory".into(),
        ));
    }
    let mut program = PathBuf::from(OsString::from_wide(&directory[..length]));
    for component in relative_components {
        program.push(component);
    }
    let metadata = std::fs::symlink_metadata(&program)
        .map_err(|_| Error::Transport(format!("{description} is unavailable")))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(Error::Transport(format!(
            "{description} is not a regular file"
        )));
    }
    Ok(program)
}

#[cfg(windows)]
fn suppress_child_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    command.creation_flags(CREATE_NO_WINDOW);
}

/// Fetch the current Shazam Config using an ordinary verified HTTPS GET.
/// Redirects, cookies, proxies, AMP credentials, and Shazam app storage are
/// not used. The response is parsed in memory and only `inid`, the Manual
/// `apis.tag.href`, optional Auto `apis.autotag.foregroundhref` and
/// `backgroundhref`, and relevant timing fields are retained for the current
/// process.
pub fn fetch_shazam_config(profile: &ConfigRequestProfile) -> Result<ShazamConfiguration, Error> {
    let cancelled = AtomicBool::new(false);
    fetch_shazam_config_cancellable(profile, &cancelled)
}

/// Fetch Shazam Config while allowing the caller to stop the HTTPS process.
pub fn fetch_shazam_config_cancellable(
    profile: &ConfigRequestProfile,
    cancelled: &AtomicBool,
) -> Result<ShazamConfiguration, Error> {
    if cancelled.load(Ordering::Acquire) {
        return Err(Error::Transport(
            "Config HTTPS request was cancelled".into(),
        ));
    }
    let url = build_config_url(profile)?;
    let app_version_header = format!("X-Shazam-AppVersion: {}", profile.app_version);
    let mut command = Command::new(system_curl_program()?);
    #[cfg(windows)]
    suppress_child_console(&mut command);
    let mut child = command
        .args([
            "--disable",
            "--noproxy",
            "*",
            "--silent",
            "--show-error",
            "--max-time",
            "20",
            "--proto",
            "=https",
            "--http1.1",
            "--header",
            "Accept: application/json",
            "--header",
            "Content-Type: application/json",
            "--header",
            "X-Shazam-Platform: MAC",
            "--header",
            app_version_header.as_str(),
            "--write-out",
            "\n__OPENSHZAM_CONFIG_STATUS__%{http_code}",
            "--url",
        ])
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Error::Transport("could not start the system HTTPS client".into()))?;

    let stdout = child.stdout.take().ok_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        Error::Transport("HTTPS client stdout was unavailable".into())
    })?;
    let output_limit = MAX_RESPONSE_BYTES + b"\n__OPENSHZAM_CONFIG_STATUS__".len() + 8;
    let reader = thread::Builder::new()
        .name("openshazam-config-response-reader".into())
        .spawn(move || read_bounded_response(stdout, output_limit))
        .map_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
            Error::Transport("could not start the Config response reader".into())
        })?;
    let status = match wait_for_child_cancellable(
        &mut child,
        cancelled,
        "Config HTTPS request was cancelled",
    ) {
        Ok(status) => status,
        Err(error) => {
            let _ = reader.join();
            return Err(error);
        }
    };
    let (combined, exceeded_limit) = reader
        .join()
        .map_err(|_| Error::Transport("Config response reader stopped unexpectedly".into()))?
        .map_err(|_| Error::Transport("could not read the Config response".into()))?;
    if exceeded_limit {
        return Err(Error::BadResponse(
            "Config response exceeded the local size limit".into(),
        ));
    }
    if !status.success() {
        return Err(Error::Transport(format!(
            "Config HTTPS request failed (curl exit {})",
            status.code().unwrap_or(-1)
        )));
    }

    const MARKER: &[u8] = b"\n__OPENSHZAM_CONFIG_STATUS__";
    let marker_index = combined
        .windows(MARKER.len())
        .rposition(|window| window == MARKER)
        .ok_or_else(|| Error::BadResponse("HTTPS client did not return a Config status".into()))?;
    let status = std::str::from_utf8(&combined[marker_index + MARKER.len()..])
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|value| *value != 0)
        .ok_or_else(|| {
            Error::BadResponse("HTTPS client returned an invalid Config status".into())
        })?;
    if !(200..300).contains(&status) {
        return Err(Error::BadResponse(format!(
            "Config endpoint returned HTTP {status}"
        )));
    }
    parse_shazam_config(&combined[..marker_index])
}

/// Resolve the host's Shazam Config route and submit an in-memory Pure Rust
/// Signature using the Windows host profile and Windows HTTPS transport.
/// The request tag and signature are never written to disk by this helper.
#[cfg(windows)]
pub fn recognize_signature_for_windows(
    signature: &[u8],
    sample_ms: u64,
) -> Result<(u16, Vec<TrackCandidate>), Error> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let profile = windows_config_request_profile()?;
    let config = fetch_shazam_config(&profile)?;
    let tag_id = random_uuid_v4()?;
    let streaming_provider = std::env::var("SHAZAM_STREAMING_PROVIDER").unwrap_or_default();
    let endpoint = config.resolve_tag_url(&tag_id, &streaming_provider)?;
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::Transport(format!("system clock is before Unix epoch: {error}")))?
        .as_millis() as u64;
    let timezone = std::env::var("TZ")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| iana_time_zone::get_timezone().ok())
        .unwrap_or_else(|| "UTC".into());
    let body = build_request_body(
        signature,
        sample_ms,
        timestamp_ms,
        &timezone,
        json!({}),
        json!({}),
        None,
    )?;
    let response = post_shazam_json(&endpoint, &body)?;
    if !(200..300).contains(&response.status) {
        return Err(Error::BadResponse(format!(
            "Shazam recognition returned HTTP {}; response body withheld",
            response.status
        )));
    }
    let candidates = parse_track_candidates(&response.body)?;
    Ok((response.status, candidates))
}

/// Submit one ProgressiveSession Signature through a previously resolved
/// manual-tag Config route. The caller supplies the host's IANA timezone and
/// owns capture/session timing; this helper only builds and performs one
/// authorized request, returning parsed control fields and candidates.
pub fn submit_progressive_signature(
    configuration: &ShazamConfiguration,
    signature: &[u8],
    sample_ms: u64,
    timezone: &str,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<ProgressiveRecognitionResult, Error> {
    submit_progressive_signature_with_timestamp_ms(
        configuration,
        signature,
        sample_ms,
        None,
        timezone,
        timeout,
        cancelled,
    )
}

/// Progressive submission with the Native Signature/session date supplied by
/// the controller. `None` preserves the previous current-time fallback for
/// callers without an audio-origin timestamp.
pub fn submit_progressive_signature_with_timestamp_ms(
    configuration: &ShazamConfiguration,
    signature: &[u8],
    sample_ms: u64,
    native_timestamp_ms: Option<u64>,
    timezone: &str,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<ProgressiveRecognitionResult, Error> {
    submit_progressive_signature_with_transport_started_and_timestamp_ms(
        configuration,
        signature,
        sample_ms,
        native_timestamp_ms,
        timezone,
        timeout,
        cancelled,
        || {},
    )
}

/// Progressive submission variant that notifies after the local HTTPS client
/// process has been spawned. This lets the stopped-generator Manual Retry path
/// preserve Native's synchronous request-dispatch-before-stop ordering.
pub fn submit_progressive_signature_with_transport_started<F>(
    configuration: &ShazamConfiguration,
    signature: &[u8],
    sample_ms: u64,
    timezone: &str,
    timeout: Duration,
    cancelled: &AtomicBool,
    on_transport_started: F,
) -> Result<ProgressiveRecognitionResult, Error>
where
    F: FnOnce(),
{
    submit_progressive_signature_with_transport_started_and_timestamp_ms(
        configuration,
        signature,
        sample_ms,
        None,
        timezone,
        timeout,
        cancelled,
        on_transport_started,
    )
}

/// Progressive submission variant that preserves both Native's request
/// dispatch ordering and the Signature/session timestamp selected by the
/// caller.
#[allow(clippy::too_many_arguments)]
pub fn submit_progressive_signature_with_transport_started_and_timestamp_ms<F>(
    configuration: &ShazamConfiguration,
    signature: &[u8],
    sample_ms: u64,
    native_timestamp_ms: Option<u64>,
    timezone: &str,
    timeout: Duration,
    cancelled: &AtomicBool,
    on_transport_started: F,
) -> Result<ProgressiveRecognitionResult, Error>
where
    F: FnOnce(),
{
    submit_progressive_signature_for_tagging_type_with_transport_started_and_timestamp_ms(
        configuration,
        NativeTaggingType::Manual,
        None,
        signature,
        sample_ms,
        native_timestamp_ms,
        timezone,
        timeout,
        cancelled,
        on_transport_started,
    )
}

/// Submit a Progressive Signature using the Native endpoint selected by its
/// TaggingType. Manual may use the paired Manual route overrides; Auto
/// requires an explicit foreground/background request context. The callback
/// indicates only that the local HTTPS client process started.
#[allow(clippy::too_many_arguments)]
pub fn submit_progressive_signature_for_tagging_type_with_transport_started_and_timestamp_ms<F>(
    configuration: &ShazamConfiguration,
    tagging_type: NativeTaggingType,
    request_context: Option<NativeTaggingRequestContext>,
    signature: &[u8],
    sample_ms: u64,
    native_timestamp_ms: Option<u64>,
    timezone: &str,
    timeout: Duration,
    cancelled: &AtomicBool,
    on_transport_started: F,
) -> Result<ProgressiveRecognitionResult, Error>
where
    F: FnOnce(),
{
    submit_progressive_signature_for_tagging_type_with_transport(
        &CurlRecognitionTransport,
        configuration,
        tagging_type,
        request_context,
        signature,
        sample_ms,
        native_timestamp_ms,
        timezone,
        timeout,
        cancelled,
        on_transport_started,
    )
}

/// Same Progressive recognition request path with an injectable verified
/// HTTPS transport. Request construction, endpoint allowlisting, and response
/// interpretation stay shared with the default curl path.
#[allow(clippy::too_many_arguments)]
pub fn submit_progressive_signature_for_tagging_type_with_transport<T, F>(
    transport: &T,
    configuration: &ShazamConfiguration,
    tagging_type: NativeTaggingType,
    request_context: Option<NativeTaggingRequestContext>,
    signature: &[u8],
    sample_ms: u64,
    native_timestamp_ms: Option<u64>,
    timezone: &str,
    timeout: Duration,
    cancelled: &AtomicBool,
    on_transport_started: F,
) -> Result<ProgressiveRecognitionResult, Error>
where
    T: RecognitionHttpTransport + ?Sized,
    F: FnOnce(),
{
    let tag_id = random_uuid_v4()?;
    let streaming_provider = std::env::var("SHAZAM_STREAMING_PROVIDER").unwrap_or_default();
    let endpoint = match tagging_type {
        NativeTaggingType::Manual => match (
            std::env::var("SHAZAM_TAG_URL_TEMPLATE"),
            std::env::var("SHAZAM_INID"),
        ) {
            (Ok(template), Ok(installation_id)) => {
                resolve_shazam_tag_url(&template, &installation_id, &tag_id, &streaming_provider)?
            }
            (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent)) => {
                configuration.resolve_tag_url_for_type(
                    tagging_type,
                    request_context,
                    &tag_id,
                    &streaming_provider,
                )?
            }
            _ => {
                return Err(Error::InvalidRequest(
                    "manual recognition route overrides must be paired".into(),
                ));
            }
        },
        NativeTaggingType::Auto => {
            if std::env::var_os("SHAZAM_TAG_URL_TEMPLATE").is_some()
                || std::env::var_os("SHAZAM_INID").is_some()
            {
                return Err(Error::InvalidRequest(
                    "manual route overrides cannot select an Auto endpoint".into(),
                ));
            }
            configuration.resolve_tag_url_for_type(
                tagging_type,
                request_context,
                &tag_id,
                &streaming_provider,
            )?
        }
    };
    let timestamp_ms = progressive_request_timestamp_ms(native_timestamp_ms)?;
    let body = build_request_body(
        signature,
        sample_ms,
        timestamp_ms,
        timezone,
        json!({}),
        json!({}),
        None,
    )?;
    let response = post_shazam_json_with_transport(
        transport,
        &endpoint,
        &body,
        timeout,
        cancelled,
        on_transport_started,
    )?;
    Ok(ProgressiveRecognitionResult {
        status: response.status,
        response: interpret_progressive_http_response_for_tagging_type(
            response.status,
            &response.body,
            Some(sample_ms),
            Some(
                configuration
                    .manual_timing()
                    .effective_maximum_signature_length_ms(),
            ),
            tagging_type,
        ),
    })
}

/// Submit one cumulative ProgressiveSession Signature using a Config fetched
/// once for the Windows run. This follows the Mac-observed retry response
/// fields while keeping the request timeout and cancellation local to the
/// adapter. Only parsed candidates/retry metadata leave this function.
#[cfg(windows)]
pub fn submit_progressive_signature_for_windows(
    configuration: &ShazamConfiguration,
    signature: &[u8],
    sample_ms: u64,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<ProgressiveRecognitionResult, Error> {
    submit_progressive_signature_for_windows_with_timestamp_ms(
        configuration,
        signature,
        sample_ms,
        None,
        timeout,
        cancelled,
    )
}

#[cfg(windows)]
pub fn submit_progressive_signature_for_windows_with_timestamp_ms(
    configuration: &ShazamConfiguration,
    signature: &[u8],
    sample_ms: u64,
    native_timestamp_ms: Option<u64>,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<ProgressiveRecognitionResult, Error> {
    submit_progressive_signature_for_windows_with_transport_started_and_timestamp_ms(
        configuration,
        signature,
        sample_ms,
        native_timestamp_ms,
        timeout,
        cancelled,
        || {},
    )
}

#[cfg(windows)]
pub fn submit_progressive_signature_for_windows_with_transport_started<F>(
    configuration: &ShazamConfiguration,
    signature: &[u8],
    sample_ms: u64,
    timeout: Duration,
    cancelled: &AtomicBool,
    on_transport_started: F,
) -> Result<ProgressiveRecognitionResult, Error>
where
    F: FnOnce(),
{
    submit_progressive_signature_for_windows_with_transport_started_and_timestamp_ms(
        configuration,
        signature,
        sample_ms,
        None,
        timeout,
        cancelled,
        on_transport_started,
    )
}

#[cfg(windows)]
pub fn submit_progressive_signature_for_windows_with_transport_started_and_timestamp_ms<F>(
    configuration: &ShazamConfiguration,
    signature: &[u8],
    sample_ms: u64,
    native_timestamp_ms: Option<u64>,
    timeout: Duration,
    cancelled: &AtomicBool,
    on_transport_started: F,
) -> Result<ProgressiveRecognitionResult, Error>
where
    F: FnOnce(),
{
    submit_progressive_signature_for_windows_tagging_type_with_transport_started_and_timestamp_ms(
        configuration,
        NativeTaggingType::Manual,
        None,
        signature,
        sample_ms,
        native_timestamp_ms,
        timeout,
        cancelled,
        on_transport_started,
    )
}

/// Windows adapter for the type-aware progressive submit path. It derives
/// the host timezone using the same precedence as the legacy Manual helper,
/// then delegates route selection and request construction to the shared
/// cross-platform implementation.
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub fn submit_progressive_signature_for_windows_tagging_type_with_transport_started_and_timestamp_ms<
    F,
>(
    configuration: &ShazamConfiguration,
    tagging_type: NativeTaggingType,
    request_context: Option<NativeTaggingRequestContext>,
    signature: &[u8],
    sample_ms: u64,
    native_timestamp_ms: Option<u64>,
    timeout: Duration,
    cancelled: &AtomicBool,
    on_transport_started: F,
) -> Result<ProgressiveRecognitionResult, Error>
where
    F: FnOnce(),
{
    let timezone = std::env::var("TZ")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| iana_time_zone::get_timezone().ok())
        .unwrap_or_else(|| "UTC".into());
    submit_progressive_signature_for_tagging_type_with_transport_started_and_timestamp_ms(
        configuration,
        tagging_type,
        request_context,
        signature,
        sample_ms,
        native_timestamp_ms,
        &timezone,
        timeout,
        cancelled,
        on_transport_started,
    )
}

/// Read locale, OS, and device fields used by the existing Windows Config
/// route. The API currently returns the supported MAC/MACOS route, so these
/// fields describe the host while the established route identifier is kept.
#[cfg(windows)]
pub fn windows_config_request_profile() -> Result<ConfigRequestProfile, Error> {
    let cancelled = AtomicBool::new(false);
    windows_config_request_profile_cancellable(&cancelled)
}

/// Read the Windows Config profile while allowing the caller to cancel the
/// PowerShell child before the HTTPS request begins.
#[cfg(windows)]
pub fn windows_config_request_profile_cancellable(
    cancelled: &AtomicBool,
) -> Result<ConfigRequestProfile, Error> {
    const SCRIPT: &str = r#"$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); $culture=Get-Culture; $os=Get-CimInstance Win32_OperatingSystem; $computer=Get-CimInstance Win32_ComputerSystem; [ordered]@{language=$culture.TwoLetterISOLanguageName; locale=$culture.Name; model=$computer.Model; os_version=$os.Version; os_build=$os.BuildNumber} | ConvertTo-Json -Compress"#;
    const OUTPUT_LIMIT: usize = 16 * 1024;
    if cancelled.load(Ordering::Acquire) {
        return Err(Error::Transport(
            "Windows Config profile request was cancelled".into(),
        ));
    }
    let mut command = Command::new(system_powershell_program()?);
    suppress_child_console(&mut command);
    let mut child = command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            SCRIPT,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            Error::Transport(format!(
                "could not start PowerShell for Windows Config profile: {error}"
            ))
        })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        Error::Transport("PowerShell profile stdout was unavailable".into())
    })?;
    let reader = thread::Builder::new()
        .name("openshazam-profile-response-reader".into())
        .spawn(move || read_bounded_response(stdout, OUTPUT_LIMIT))
        .map_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
            Error::Transport("could not start the Windows profile response reader".into())
        })?;
    let status = match wait_for_child_cancellable(
        &mut child,
        cancelled,
        "Windows Config profile request was cancelled",
    ) {
        Ok(status) => status,
        Err(error) => {
            let _ = reader.join();
            return Err(error);
        }
    };
    let (output, exceeded_limit) = reader
        .join()
        .map_err(|_| Error::Transport("Windows profile reader stopped unexpectedly".into()))?
        .map_err(|_| Error::Transport("could not read the Windows Config profile".into()))?;
    if exceeded_limit {
        return Err(Error::BadResponse(
            "Windows Config profile exceeded the local size limit".into(),
        ));
    }
    if !status.success() {
        return Err(Error::Transport(
            "PowerShell could not read the Windows locale, OS, or device model".into(),
        ));
    }
    let host = parse_windows_host_profile(&output)?;
    let profile_value = |name: &str, detected: String| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(detected)
    };
    Ok(ConfigRequestProfile {
        country: profile_value("SHAZAM_CONFIG_COUNTRY", "-".into()),
        language: profile_value("SHAZAM_CONFIG_LANGUAGE", host.language),
        locale: profile_value("SHAZAM_CONFIG_LOCALE", host.locale),
        model: profile_value("SHAZAM_CONFIG_MODEL", host.model),
        os_version: profile_value("SHAZAM_CONFIG_OS_VERSION", host.os_version),
        os_build: profile_value("SHAZAM_CONFIG_OS_BUILD", host.os_build),
        app_version: "2.11.0".into(),
    })
}

#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
struct WindowsHostProfile {
    language: String,
    locale: String,
    model: String,
    os_version: String,
    os_build: String,
}

#[cfg(windows)]
fn parse_windows_host_profile(bytes: &[u8]) -> Result<WindowsHostProfile, Error> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        Error::BadResponse("PowerShell returned invalid Windows profile JSON".into())
    })?;
    let field = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| Error::BadResponse(format!("Windows profile is missing {name}")))
    };
    Ok(WindowsHostProfile {
        language: field("language")?,
        locale: field("locale")?.replace('-', "_"),
        model: field("model")?,
        os_version: field("os_version")?,
        os_build: field("os_build")?,
    })
}

fn random_uuid_v4() -> Result<String, Error> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| Error::Transport(format!("could not generate request tag ID: {error}")))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    ))
}

fn validate_shazam_endpoint(endpoint: &str) -> Result<(), Error> {
    let rest = endpoint.strip_prefix("https://").ok_or_else(|| {
        Error::InvalidEndpoint("Shazam live transport requires verified HTTPS".into())
    })?;
    let (authority, path_and_query) = rest.split_once('/').ok_or_else(|| {
        Error::InvalidEndpoint("Shazam endpoint must include the manual-tag route".into())
    })?;
    if authority != "amp.shazam.com" || path_and_query.contains(['#', '@']) {
        return Err(Error::InvalidEndpoint(
            "only the configured amp.shazam.com host is accepted".into(),
        ));
    }
    if path_and_query
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b' ' || byte == b'{' || byte == b'}')
    {
        return Err(Error::InvalidEndpoint(
            "Shazam endpoint contains unsafe or unresolved characters".into(),
        ));
    }

    let (path, query) = path_and_query.split_once('?').ok_or_else(|| {
        Error::InvalidEndpoint("Shazam manual-tag route must include its configured query".into())
    })?;
    let path_parts: Vec<&str> = path.split('/').collect();
    if path_parts.len() != 9
        || path_parts[0] != "discovery"
        || path_parts[1] != "v5"
        || path_parts[2].is_empty()
        || !path_parts[2].bytes().all(is_url_unreserved)
        || path_parts[3].is_empty()
        || !path_parts[3].bytes().all(is_url_unreserved)
        || path_parts[4] != "mac"
        || path_parts[5] != "-"
        || path_parts[6] != "tag"
        || path_parts[7].is_empty()
        || !path_parts[7].bytes().all(is_url_unreserved)
        || path_parts[8].is_empty()
        || !path_parts[8].bytes().all(is_url_unreserved)
    {
        return Err(Error::InvalidEndpoint(
            "only a safe configured Mac manual-tag route is accepted".into(),
        ));
    }

    let mut seen = std::collections::BTreeMap::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(Error::InvalidEndpoint(
                "malformed Shazam query string".into(),
            ));
        };
        if !matches!(
            key,
            "sync" | "webv3" | "sampling" | "connected" | "sharehub" | "hidelb"
        ) || !value.bytes().all(is_url_unreserved)
            || seen.insert(key, value).is_some()
        {
            return Err(Error::InvalidEndpoint(
                "Shazam query contains an unexpected or duplicate parameter".into(),
            ));
        }
    }
    for (key, value) in [
        ("sync", "true"),
        ("webv3", "true"),
        ("sampling", "true"),
        ("sharehub", "false"),
        ("hidelb", "true"),
    ] {
        if seen.get(key) != Some(&value) {
            return Err(Error::InvalidEndpoint(
                "Shazam query does not match the observed manual-tag template".into(),
            ));
        }
    }
    if !seen.contains_key("connected") {
        return Err(Error::InvalidEndpoint(
            "Shazam query is missing `connected`".into(),
        ));
    }
    Ok(())
}

fn validate_shazam_request_endpoint(endpoint: &str) -> Result<(), Error> {
    validate_shazam_endpoint(endpoint).or_else(|_| validate_continuous_shazam_endpoint(endpoint))
}

fn validate_continuous_shazam_endpoint(endpoint: &str) -> Result<(), Error> {
    let rest = endpoint
        .strip_prefix("https://amp.shazam.com/")
        .ok_or_else(|| {
            Error::InvalidEndpoint(
                "Auto continuous requests require the configured amp.shazam.com HTTPS host".into(),
            )
        })?;
    if rest.is_empty()
        || rest.contains(['#', '@', '\\', '{', '}'])
        || rest
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ' || !byte.is_ascii())
    {
        return Err(Error::InvalidEndpoint(
            "Auto continuous endpoint contains unsafe or unresolved characters".into(),
        ));
    }
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) if !query.is_empty() && !query.contains('?') => (path, Some(query)),
        Some(_) => {
            return Err(Error::InvalidEndpoint(
                "Auto continuous endpoint has a malformed query".into(),
            ));
        }
        None => (rest, None),
    };
    if path.is_empty() || path.starts_with('/') || path.ends_with('/') {
        return Err(Error::InvalidEndpoint(
            "Auto continuous endpoint has an invalid path".into(),
        ));
    }
    if let Some(query) = query {
        for pair in query.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                return Err(Error::InvalidEndpoint(
                    "Auto continuous endpoint has a malformed query parameter".into(),
                ));
            };
            if key.is_empty()
                || !key.bytes().all(is_url_unreserved)
                || !value.bytes().all(|byte| {
                    is_url_unreserved(byte)
                        || matches!(byte, b'%' | b'+' | b':' | b'/' | b',' | b';')
                })
            {
                return Err(Error::InvalidEndpoint(
                    "Auto continuous endpoint has unsafe query characters".into(),
                ));
            }
        }
    }
    Ok(())
}

fn is_url_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// Decode the currently observed response keys into displayable candidates.
///
/// The `matches`, `trackid`, `title`, and `artist` keys exist in the installed
/// binary. Auto Type 2's live response uses an ordered `matches` array; those
/// entries are parsed in place and promoted by the Auto callback adapter.
/// Manual callback construction remains limited to a root `track` dictionary.
pub fn parse_track_candidates(response_body: &[u8]) -> Result<Vec<TrackCandidate>, Error> {
    let root: Value = serde_json::from_slice(response_body)
        .map_err(|error| Error::BadResponse(format!("response is not valid JSON: {error}")))?;

    if let Some(track) = root.get("track").filter(|value| value.is_object()) {
        if let Some(mut candidate) = track_candidate(track, TrackCandidateOrigin::RootTrack) {
            // SHKTag's root-track path retains the response dictionary. The
            // Native data bridge then looks up this exact lower-case key on
            // that dictionary and only creates a child tag for a non-empty
            // dictionary. Matches-array entries are not promoted to this
            // role, and matches[1] is never treated as an alternative.
            candidate.alternative_track = root
                .get("alternativetrack")
                .filter(|value| value.as_object().is_some_and(|object| !object.is_empty()))
                .and_then(|track| track_candidate(track, TrackCandidateOrigin::AlternativeTrack))
                .map(Box::new);
            return Ok(vec![candidate]);
        }
    }

    parse_matches_candidates(&root)
}

/// Parse Auto Type 2 candidates from the root `matches` array, preserving the
/// order that ShazamTagging uses when it creates continuous tags. A root
/// `track` object does not take precedence on the Auto route.
pub fn parse_auto_track_candidates(response_body: &[u8]) -> Result<Vec<TrackCandidate>, Error> {
    let root: Value = serde_json::from_slice(response_body)
        .map_err(|error| Error::BadResponse(format!("response is not valid JSON: {error}")))?;
    parse_matches_candidates(&root)
}

fn parse_matches_candidates(root: &Value) -> Result<Vec<TrackCandidate>, Error> {
    let matches = root
        .get("matches")
        .ok_or_else(|| Error::BadResponse("response has no `matches` field".into()))?;
    let entries: Vec<&Value> = match matches {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![matches],
        _ => {
            return Err(Error::BadResponse(
                "response `matches` must be an object or array".into(),
            ));
        }
    };

    let mut candidates = Vec::new();
    for entry in entries {
        let track = entry
            .get("track")
            .filter(|value| value.is_object())
            .unwrap_or(entry);
        if let Some(candidate) = track_candidate(track, TrackCandidateOrigin::MatchesEntry) {
            candidates.push(candidate);
        }
    }
    if candidates.is_empty() {
        return Err(Error::NoMatches);
    }
    Ok(candidates)
}

fn track_candidate(track: &Value, origin: TrackCandidateOrigin) -> Option<TrackCandidate> {
    let images = track.get("images").and_then(Value::as_object);
    let artwork_url = images
        .and_then(|images| images.get("coverart").or_else(|| images.get("image100")))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let hq_artwork_url = images
        .and_then(|images| images.get("coverarthq").or_else(|| images.get("image400")))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let background_image_url = images
        .and_then(|images| images.get("background"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| artwork_url.clone());
    let ui_metadata = NativeTrackUiMetadata {
        track_key: track
            .get("key")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                track
                    .get("trackKey")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
            })
            .map(str::to_owned),
        album_adam_id: track
            .get("albumadamid")
            .or_else(|| track.get("albumAdamId"))
            .and_then(|value| match value {
                Value::String(value) => Some(value.clone()),
                Value::Number(value) => Some(value.to_string()),
                _ => None,
            }),
        isrc: track.get("isrc").and_then(Value::as_str).map(str::to_owned),
        date: track.get("date").and_then(Value::as_str).map(str::to_owned),
        artwork_url,
        hq_artwork_url,
        background_image_url,
    };
    let has_ui_metadata = ui_metadata.track_key.is_some()
        || ui_metadata.album_adam_id.is_some()
        || ui_metadata.isrc.is_some()
        || ui_metadata.date.is_some()
        || ui_metadata.artwork_url.is_some()
        || ui_metadata.hq_artwork_url.is_some()
        || ui_metadata.background_image_url.is_some();
    let ui_metadata = has_ui_metadata.then_some(ui_metadata);

    // Native's manual root-track bridge derives the visible value from
    // SHALTrack.subtitle -> TagResult.artist -> TagResult.artists. The V5 track
    // converter carries wire `subtitle` into that artist representation. A
    // scalar wire `artist` fallback is not present in the inspected path, so a
    // root result without subtitle is withheld instead of fabricating a value.
    let artist = match origin {
        TrackCandidateOrigin::RootTrack => track.get("subtitle").and_then(Value::as_str),
        // Matcher entries can expose a distinct scalar `artist` alongside
        // their presentation subtitle; use the explicit artist field first.
        TrackCandidateOrigin::MatchesEntry => track
            .get("artist")
            .and_then(Value::as_str)
            .or_else(|| track.get("subtitle").and_then(Value::as_str))
            .or_else(|| {
                track
                    .get("heading")
                    .and_then(|heading| heading.get("subtitle"))
                    .and_then(Value::as_str)
            })
            .or_else(|| track.get("description").and_then(Value::as_str)),
        _ => track
            .get("artist")
            .and_then(Value::as_str)
            .or_else(|| track.get("subtitle").and_then(Value::as_str)),
    }?;

    Some(TrackCandidate {
        track_id: track
            .get("trackid")
            .or_else(|| track.get("trackId"))
            .or_else(|| track.get("key"))
            .or_else(|| track.get("trackKey"))
            .and_then(json_string_value),
        title: track.get("title")?.as_str()?.to_owned(),
        artist: artist.to_owned(),
        ui_metadata,
        alternative_track: None,
        origin,
    })
}

fn json_string_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn track_candidate_json(candidate: &TrackCandidate) -> Value {
    let mut value = json!({
        "track_id": candidate.track_id.as_deref(),
        "title": candidate.title.as_str(),
        "artist": candidate.artist.as_str(),
    });
    if let Some(metadata) = &candidate.ui_metadata {
        value["native_track_values"] = json!({
            "track_key": metadata.track_key.as_deref(),
            "album_adam_id": metadata.album_adam_id.as_deref(),
            "isrc": metadata.isrc.as_deref(),
            "date": metadata.date.as_deref(),
            "artwork_url": metadata.artwork_url.as_deref(),
            "hq_artwork_url": metadata.hq_artwork_url.as_deref(),
            "background_image_url": metadata.background_image_url.as_deref(),
        });
    }
    value
}

/// Summarize JSON response structure without exposing any response values.
pub fn summarize_response_shape(response_body: &[u8]) -> String {
    let Ok(root) = serde_json::from_slice::<Value>(response_body) else {
        return format!("non-JSON body ({} bytes)", response_body.len());
    };
    let Some(object) = root.as_object() else {
        return format!("JSON root type={}", json_type(&root));
    };

    let mut summary = format!("root keys={}", joined_keys(object));
    if let Some(track) = object.get("track") {
        if let Some(track) = track.as_object() {
            summary.push_str(&format!("; root-track keys={}", joined_keys(track)));
        } else {
            summary.push_str(&format!("; root-track type={}", json_type(track)));
        }
    }
    if let Some(matches) = object.get("matches") {
        match matches {
            Value::Array(entries) => {
                summary.push_str(&format!("; matches=array[{}]", entries.len()));
                if let Some(first) = entries.first().and_then(Value::as_object) {
                    summary.push_str(&format!("; first-match keys={}", joined_keys(first)));
                    if let Some(track) = first.get("track").and_then(Value::as_object) {
                        summary.push_str(&format!("; first-track keys={}", joined_keys(track)));
                    }
                }
            }
            Value::Object(matches) => {
                summary.push_str(&format!("; matches=object keys={}", joined_keys(matches)));
            }
            other => summary.push_str(&format!("; matches type={}", json_type(other))),
        }
    }
    summary
}

fn joined_keys(object: &serde_json::Map<String, Value>) -> String {
    object.keys().cloned().collect::<Vec<_>>().join(",")
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn parse_loopback_endpoint(endpoint: &str) -> Result<(SocketAddr, &str, &str), Error> {
    let rest = endpoint.strip_prefix("http://").ok_or_else(|| {
        Error::InvalidEndpoint("only http:// literal loopback URLs are accepted".into())
    })?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.contains('@') || authority.contains('?') || authority.contains('#') {
        return Err(Error::InvalidEndpoint(
            "credentials, query strings, and fragments are not accepted".into(),
        ));
    }
    let address: SocketAddr = authority
        .parse()
        .map_err(|_| Error::InvalidEndpoint("endpoint must use an IP literal and port".into()))?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(Error::InvalidEndpoint(
            "only a nonzero port on a literal loopback IP is accepted".into(),
        ));
    }
    let path = if path.is_empty() {
        "/"
    } else {
        &rest[authority.len()..]
    };
    if path
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(Error::InvalidEndpoint(
            "endpoint path contains invalid bytes".into(),
        ));
    }
    Ok((address, path, authority))
}

fn parse_http_response(bytes: &[u8]) -> Result<HttpResponse, Error> {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Err(Error::BadResponse(
            "HTTP response has no header terminator".into(),
        ));
    };
    if header_end > MAX_HEADER_BYTES {
        return Err(Error::BadResponse(
            "HTTP response headers are too large".into(),
        ));
    }
    let headers = std::str::from_utf8(&bytes[..header_end])
        .map_err(|error| Error::BadResponse(format!("HTTP headers are not UTF-8: {error}")))?;
    let mut lines = headers.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| Error::BadResponse("HTTP response has no status line".into()))?;
    let mut status_parts = status_line.split_ascii_whitespace();
    if !status_parts
        .next()
        .is_some_and(|protocol| protocol.starts_with("HTTP/1."))
    {
        return Err(Error::BadResponse("unsupported HTTP status line".into()));
    }
    let status = status_parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| Error::BadResponse("invalid HTTP status code".into()))?;
    let mut content_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(Error::BadResponse("malformed HTTP response header".into()));
        };
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value.trim().eq_ignore_ascii_case("chunked")
        {
            return Err(Error::BadResponse(
                "chunked mock responses are not supported".into(),
            ));
        }
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| Error::BadResponse("invalid HTTP Content-Length".into()))?,
            );
        }
    }
    let body_start = header_end + 4;
    let available = &bytes[body_start..];
    let body_length = content_length.unwrap_or(available.len());
    if body_length > MAX_RESPONSE_BYTES || available.len() < body_length {
        return Err(Error::BadResponse(
            "HTTP response body length is missing data or exceeds the local limit".into(),
        ));
    }
    Ok(HttpResponse {
        status,
        body: available[..body_length].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        thread,
    };

    use shazam_progressive::{
        NativeTaggingType, ProgressiveSession, NATIVE_NETWORK_REQUEST_TIMEOUT_MS,
        NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS, NATIVE_TAGGING_TYPE_2_DEFAULT_INTERVAL_MS,
    };

    use super::*;

    #[test]
    fn system_https_client_is_an_absolute_regular_file() {
        let program = system_curl_program().unwrap();
        let metadata = std::fs::symlink_metadata(&program).unwrap();
        assert!(program.is_absolute());
        assert!(metadata.is_file());
        assert!(!metadata.file_type().is_symlink());
        #[cfg(windows)]
        assert_eq!(
            program.file_name().and_then(|name| name.to_str()),
            Some("curl.exe")
        );
        #[cfg(not(windows))]
        assert_eq!(program, PathBuf::from("/usr/bin/curl"));
    }

    #[cfg(windows)]
    #[test]
    fn system_powershell_is_an_absolute_regular_file() {
        use std::os::windows::fs::MetadataExt;

        let program = system_powershell_program().unwrap();
        let metadata = std::fs::symlink_metadata(&program).unwrap();
        assert!(program.is_absolute());
        assert!(metadata.is_file());
        assert!(!metadata.file_type().is_symlink());
        assert!(metadata.file_attributes() & 0x0400 == 0);
        assert_eq!(
            program.file_name().and_then(|name| name.to_str()),
            Some("powershell.exe")
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_already_cancelled_windows_profile_does_not_start_powershell() {
        let cancelled = AtomicBool::new(true);
        let error = match windows_config_request_profile_cancellable(&cancelled) {
            Err(error) => error,
            Ok(_) => panic!("a cancelled profile request must not start"),
        };

        assert!(error
            .to_string()
            .contains("Windows Config profile request was cancelled"));
    }

    fn valid_signature() -> Vec<u8> {
        sigx_format::encode_legacy_signature(&sigx_format::LegacySignatureInput {
            sample_rate: 16_000,
            stream_pos_ticks: 0,
            sample_length_samples: 0,
            density: 15,
            sections: Vec::new(),
        })
        .expect("empty legacy signature is valid")
    }

    #[test]
    fn transport_dispatch_callback_runs_only_after_child_process_spawn() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--list")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut dispatched = false;
        let mut child =
            spawn_http_client_with_dispatch(&mut command, || dispatched = true).unwrap();

        assert!(dispatched);
        assert!(child.wait().unwrap().success());
    }

    #[cfg(windows)]
    #[test]
    fn cancelling_a_config_request_terminates_the_https_child_promptly() {
        let mut command = Command::new(system_powershell_program().unwrap());
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 30",
        ]);
        suppress_child_console(&mut command);
        let mut child = command.spawn().expect("start the cancellable test child");
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let cancellation_signal = std::sync::Arc::clone(&cancelled);
        let signaler = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancellation_signal.store(true, Ordering::Release);
        });
        let started = std::time::Instant::now();

        let error = wait_for_child_cancellable(
            &mut child,
            &cancelled,
            "Config HTTPS request was cancelled",
        )
        .expect_err("cancellation must terminate a still-running Config process");

        signaler
            .join()
            .expect("join the cancellation signal worker");
        assert!(error
            .to_string()
            .contains("Config HTTPS request was cancelled"));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancellation must not wait for the HTTPS timeout"
        );
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn an_already_cancelled_config_request_does_not_start_http() {
        let cancelled = AtomicBool::new(true);
        let error = match fetch_shazam_config_cancellable(&config_profile(), &cancelled) {
            Err(error) => error,
            Ok(_) => panic!("a cancelled request must not start"),
        };

        assert!(error
            .to_string()
            .contains("Config HTTPS request was cancelled"));
    }

    #[test]
    fn bounded_response_reader_stops_after_first_excess_byte() {
        struct CountingReader {
            remaining: usize,
            bytes_read: usize,
        }

        impl std::io::Read for CountingReader {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                if self.remaining == 0 || output.is_empty() {
                    return Ok(0);
                }
                output[0] = b'x';
                self.remaining -= 1;
                self.bytes_read += 1;
                Ok(1)
            }
        }

        let mut reader = CountingReader {
            remaining: 16,
            bytes_read: 0,
        };
        let (body, exceeded_limit) = read_bounded_response(&mut reader, 3).unwrap();

        assert_eq!(body, b"xxx");
        assert!(exceeded_limit);
        assert_eq!(reader.bytes_read, 4);
        assert_eq!(reader.remaining, 12);
    }

    #[test]
    fn body_contains_observed_data_uri_and_fields() {
        let body = build_request_body(
            &valid_signature(),
            12_000,
            1_800_000_000_123,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            Some(json!("test-origin")),
        )
        .unwrap();
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        let uri = parsed["signature"]["uri"].as_str().unwrap();
        assert!(uri.starts_with(SIGNATURE_DATA_URI));
        assert_eq!(parsed["signature"]["samplems"], 12_000);
        assert_eq!(parsed["timestamp"], 1_800_000_000_123_u64);
        assert_eq!(parsed["timezone"], "Asia/Tokyo");
        assert_eq!(parsed["thirdparty"], "test-origin");
        assert_eq!(parsed["context"], json!({}));
        assert_eq!(parsed["geolocation"], json!({}));
    }

    #[test]
    fn progressive_request_uses_signature_timestamp_before_legacy_now_fallback() {
        let source_timestamp_ms = 1_800_000_000_123;
        assert_eq!(
            progressive_request_timestamp_ms(Some(source_timestamp_ms)).unwrap(),
            source_timestamp_ms
        );

        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let fallback = progressive_request_timestamp_ms(None).unwrap();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!((before..=after).contains(&fallback));
    }

    #[test]
    fn response_parser_reads_mock_matches_without_claiming_live_schema() {
        let candidates = parse_track_candidates(
            br#"{"matches":[{"trackid":"mock-1","title":"Mock Song","artist":"Test Artist"}]}"#,
        )
        .unwrap();
        assert_eq!(
            candidates,
            vec![TrackCandidate {
                track_id: Some("mock-1".into()),
                title: "Mock Song".into(),
                artist: "Test Artist".into(),
                ui_metadata: None,
                alternative_track: None,
                origin: TrackCandidateOrigin::MatchesEntry,
            }]
        );
        assert!(native_manual_result_event(candidates, None).is_none());
    }

    #[test]
    fn native_manual_result_event_distinguishes_complete_result_from_final_callback() {
        let alternative = TrackCandidate {
            track_id: Some("alternative-1".into()),
            title: "Alternative Song".into(),
            artist: "Alternative Artist".into(),
            ui_metadata: None,
            alternative_track: None,
            origin: TrackCandidateOrigin::AlternativeTrack,
        };
        let candidate = TrackCandidate {
            track_id: Some("mock-1".into()),
            title: "Mock Song".into(),
            artist: "Test Artist".into(),
            ui_metadata: None,
            alternative_track: Some(Box::new(alternative.clone())),
            origin: TrackCandidateOrigin::RootTrack,
        };

        let later_match = TrackCandidate {
            track_id: Some("second-match".into()),
            title: "Not a Native callback track".into(),
            artist: "Not a Native callback artist".into(),
            ui_metadata: None,
            alternative_track: None,
            origin: TrackCandidateOrigin::MatchesEntry,
        };
        let early = native_manual_result_event(
            vec![candidate.clone(), later_match],
            Some(RetryDirective {
                retry_duration_ms: Some(6_000),
                interval_ms: Some(2_000),
                retry_duration_seconds_bits: None,
                interval_seconds_bits: None,
            }),
        )
        .unwrap();
        let mut expected_primary = candidate.clone();
        expected_primary.alternative_track = None;
        assert_eq!(
            early.stage,
            NativeManualResultStage::WillFetchCompleteResult
        );
        assert_eq!(early.primary_track, expected_primary);
        assert_eq!(early.alternative_track.as_deref(), Some(&alternative));

        for retry in [
            None,
            Some(RetryDirective {
                retry_duration_ms: Some(0),
                interval_ms: Some(2_000),
                retry_duration_seconds_bits: None,
                interval_seconds_bits: None,
            }),
        ] {
            let final_event = native_manual_result_event(vec![candidate.clone()], retry).unwrap();
            assert_eq!(final_event.stage, NativeManualResultStage::TaggingDidFinish);
            assert_eq!(final_event.primary_track, expected_primary);
            assert_eq!(final_event.alternative_track.as_deref(), Some(&alternative));
        }

        let submillisecond_retry = native_manual_result_event(
            vec![candidate.clone()],
            Some(RetryDirective {
                retry_duration_ms: Some(0),
                interval_ms: Some(0),
                retry_duration_seconds_bits: Some(0.0001_f64.to_bits()),
                interval_seconds_bits: None,
            }),
        )
        .unwrap();
        assert_eq!(
            submillisecond_retry.stage,
            NativeManualResultStage::WillFetchCompleteResult,
            "Native callback phase depends on the original positive Double, not rounded milliseconds"
        );

        assert!(native_manual_result_event(Vec::new(), None).is_none());
    }

    #[test]
    fn auto_match_exposes_native_continuous_candidate_not_manual_callback() {
        let timing = RecognitionTiming {
            sample_rate: 16_000,
            minimum_signature_ms: 2_000,
            initial_sample_ms: 3_000,
            maximum_sample_ms: 6_000,
            default_retry_interval_ms: 12_000,
            maximum_retry_duration_ms: Some(6_000),
            session_timeout_ms: None,
            request_timeout_ms: 5_000,
            maximum_retry_delay_ms: None,
            maximum_attempts: None,
        };
        let mut session =
            ProgressiveSession::new_for_type(timing, NativeTaggingType::Auto).unwrap();
        session.push_i16_at(&vec![0; 48_000], 3_000).unwrap();
        let job = session.poll(3_000).unwrap().unwrap();
        let delivery = apply_progressive_response(
            &mut session,
            job.attempt,
            RecognitionOutcome::Match {
                retry: Some(RetryDirective {
                    retry_duration_ms: Some(6_000),
                    interval_ms: Some(2_000),
                    retry_duration_seconds_bits: None,
                    interval_seconds_bits: None,
                }),
            },
            vec![TrackCandidate {
                track_id: Some("auto-track".into()),
                title: "Auto Song".into(),
                artist: "Auto Artist".into(),
                ui_metadata: None,
                alternative_track: None,
                origin: TrackCandidateOrigin::RootTrack,
            }],
            3_100,
        )
        .unwrap();

        assert!(delivery.delivered_to_matcher);
        assert!(delivery.native_manual_ui_event().is_none());
        let auto_event = delivery
            .native_auto_ui_event()
            .expect("Auto Type 2 match exposes the continuous callback candidate");
        let auto_json: Value =
            serde_json::from_str(&native_auto_ui_event_to_json(&auto_event)).unwrap();
        assert_eq!(auto_json["schema"], "openshazam.auto-ui-event.v1");
        assert_eq!(auto_json["event"], "continuous_match_candidate");
        assert_eq!(auto_json["tagging_type"], 2);
        assert_eq!(
            auto_json["native_callback_candidate"],
            "continuousTaggingDidFinishWithTrack:alternativeTrack:context:"
        );
        assert_eq!(auto_json["native_duplicate_filter_applied"], false);
        assert_eq!(auto_json["primary_track"]["title"], "Auto Song");
        assert_eq!(auto_json["primary_track"]["artist"], "Auto Artist");
        assert!(auto_json.get("context").is_none());
        assert!(auto_json.get("session_id").is_none());
    }

    #[test]
    fn auto_matches_response_uses_first_native_tag_candidate() {
        let body = r#"{
            "intervalseconds": 12,
            "track": {"title": "Manual root song", "subtitle": "Manual root artist"},
            "matches": [
                {
                    "title": "Fluorite [Instrumental]",
                    "description": "初星学園 & 有村麻央",
                    "caption": "一致しました…",
                    "key": "shazam-track-key",
                    "trackId": 1098765432,
                    "images": {
                        "image100": "https://img.test/fluorite-100.jpg",
                        "image400": "https://img.test/fluorite-400.jpg"
                    },
                    "tjson": {
                        "matches": [],
                        "tag": {"timestamp": "2026-10-17T00:00:00Z"},
                        "track": {"id": "only-an-id"}
                    }
                },
                {
                    "title": "Later candidate",
                    "description": "Other artist",
                    "caption": "一致しました…",
                    "key": "later-key"
                }
            ]
        }"#
        .as_bytes();
        let ProgressiveHttpResponse::Matched {
            candidates: manual_candidates,
            ..
        } = interpret_progressive_http_response_for_signature(
            200,
            body,
            Some(12_000),
            Some(12_000),
        )
        else {
            panic!("Manual response parsing must retain the root track path");
        };
        assert_eq!(manual_candidates.len(), 1);
        assert_eq!(manual_candidates[0].title, "Manual root song");

        let ProgressiveHttpResponse::Matched { candidates, .. } =
            interpret_progressive_http_response_for_tagging_type(
                200,
                body,
                Some(12_000),
                Some(12_000),
                NativeTaggingType::Auto,
            )
        else {
            panic!("Auto matches response must parse as a match");
        };
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].title, "Fluorite [Instrumental]");
        assert_eq!(candidates[0].artist, "初星学園 & 有村麻央");
        assert_eq!(candidates[0].track_id.as_deref(), Some("1098765432"));
        let metadata = candidates[0].ui_metadata.as_ref().unwrap();
        assert_eq!(metadata.track_key.as_deref(), Some("shazam-track-key"));
        assert_eq!(
            metadata.artwork_url.as_deref(),
            Some("https://img.test/fluorite-100.jpg")
        );
        assert_eq!(
            metadata.hq_artwork_url.as_deref(),
            Some("https://img.test/fluorite-400.jpg")
        );
        assert!(native_manual_result_event(candidates.clone(), None).is_none());

        let timing = RecognitionTiming {
            sample_rate: 16_000,
            minimum_signature_ms: 2_000,
            initial_sample_ms: 3_000,
            maximum_sample_ms: 6_000,
            default_retry_interval_ms: 12_000,
            maximum_retry_duration_ms: Some(6_000),
            session_timeout_ms: None,
            request_timeout_ms: 5_000,
            maximum_retry_delay_ms: None,
            maximum_attempts: None,
        };
        let mut session =
            ProgressiveSession::new_for_type(timing, NativeTaggingType::Auto).unwrap();
        session.push_i16_at(&vec![0; 48_000], 3_000).unwrap();
        let job = session.poll(3_000).unwrap().unwrap();
        let delivery = apply_progressive_response(
            &mut session,
            job.attempt,
            RecognitionOutcome::Match { retry: None },
            candidates,
            3_100,
        )
        .unwrap();
        let event = delivery
            .native_auto_ui_event()
            .expect("first matches entry must reach the Auto continuous callback");
        let NativeAutoUiEvent::ContinuousMatchCandidate(result) = event;
        assert_eq!(result.primary_track.title, "Fluorite [Instrumental]");
        assert_eq!(result.primary_track.artist, "初星学園 & 有村麻央");
        assert_eq!(result.primary_track.track_id.as_deref(), Some("1098765432"));
        assert!(result.alternative_track.is_none());
    }

    #[test]
    fn native_manual_ui_events_have_stable_jsonl_records_without_native_only_values() {
        let result = NativeManualResultEvent {
            stage: NativeManualResultStage::WillFetchCompleteResult,
            primary_track: TrackCandidate {
                track_id: Some("fixture-track".into()),
                title: "Fixture Song".into(),
                artist: "Fixture Artist".into(),
                ui_metadata: None,
                alternative_track: None,
                origin: TrackCandidateOrigin::RootTrack,
            },
            alternative_track: Some(Box::new(TrackCandidate {
                track_id: Some("alternative-track".into()),
                title: "Alternative Song".into(),
                artist: "Alternative Artist".into(),
                ui_metadata: None,
                alternative_track: None,
                origin: TrackCandidateOrigin::AlternativeTrack,
            })),
        };
        let early_event = NativeManualUiEvent::Result(result.clone());
        let early_json: Value =
            serde_json::from_str(&native_manual_ui_event_to_json(&early_event)).unwrap();
        assert_eq!(early_json["schema"], "openshazam.manual-ui-event.v2");
        assert_eq!(early_json["event"], "result");
        assert_eq!(early_json["stage"], "will_fetch_complete_result");
        assert_eq!(early_json["native_main_view_action"], "no_update");
        assert!(early_json["native_main_view_inputs"].is_null());
        assert_eq!(early_json["primary_track"]["track_id"], "fixture-track");
        assert_eq!(
            early_json["alternative_track"]["track_id"],
            "alternative-track"
        );
        assert!(early_json.get("candidates").is_none());
        assert_eq!(early_json.as_object().unwrap().len(), 7);

        let mut final_result = result;
        final_result.stage = NativeManualResultStage::TaggingDidFinish;
        let final_event = NativeManualUiEvent::Result(final_result);
        let final_json: Value =
            serde_json::from_str(&native_manual_ui_event_to_json(&final_event)).unwrap();
        assert_eq!(final_json["stage"], "tagging_did_finish");
        assert_eq!(final_json["native_main_view_action"], "display_primary");
        assert_eq!(
            final_json["native_main_view_inputs"]["title"],
            "Fixture Song"
        );
        assert_eq!(
            final_json["native_main_view_inputs"]["subtitle"],
            "Fixture Artist"
        );
        assert!(final_json["native_main_view_inputs"]["artwork_url"].is_null());

        let no_match: Value = serde_json::from_str(&native_manual_ui_event_to_json(
            &NativeManualUiEvent::NoMatch,
        ))
        .unwrap();
        assert_eq!(no_match["schema"], "openshazam.manual-ui-event.v2");
        assert_eq!(no_match["event"], "no_match");
        assert_eq!(no_match.as_object().unwrap().len(), 2);

        let error: Value = serde_json::from_str(&native_manual_ui_event_to_json(
            &NativeManualUiEvent::Error {
                kind: NativeManualErrorKind::TransportFailure,
            },
        ))
        .unwrap();
        assert_eq!(error["schema"], "openshazam.manual-ui-event.v2");
        assert_eq!(error["event"], "error");
        assert_eq!(error["error_kind"], "transport_failure");
        assert_eq!(error.as_object().unwrap().len(), 3);
    }

    #[test]
    fn native_session_timeout_event_is_typed_and_contains_no_native_identifiers() {
        let manual: Value = serde_json::from_str(&native_session_timeout_event_to_json(
            NativeTaggingType::Manual,
        ))
        .unwrap();
        assert_eq!(manual["schema"], "openshazam.progressive-session-event.v1");
        assert_eq!(manual["event"], "session_timeout");
        assert_eq!(manual["tagging_type"], 1);
        assert!(manual.get("inid").is_none());
        assert!(manual.get("context_id").is_none());
        assert!(manual.get("session_id").is_none());

        let auto: Value = serde_json::from_str(&native_session_timeout_event_to_json(
            NativeTaggingType::Auto,
        ))
        .unwrap();
        assert_eq!(auto["tagging_type"], 2);
    }

    #[test]
    fn applied_manual_response_exposes_only_context_delivered_ui_events() {
        let make_session = || {
            ProgressiveSession::new_for_type(
                RecognitionTiming {
                    sample_rate: 16_000,
                    minimum_signature_ms: 2_000,
                    initial_sample_ms: 3_000,
                    maximum_sample_ms: 6_000,
                    default_retry_interval_ms: NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS,
                    maximum_retry_duration_ms: Some(6_000),
                    session_timeout_ms: None,
                    request_timeout_ms: 5_000,
                    maximum_retry_delay_ms: None,
                    maximum_attempts: None,
                },
                NativeTaggingType::Manual,
            )
            .unwrap()
        };
        let samples: Vec<i16> = (0..48_000)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        let parse_match = |body: &[u8]| match interpret_progressive_http_response(200, body) {
            ProgressiveHttpResponse::Matched { candidates, retry } => {
                (RecognitionOutcome::Match { retry }, candidates)
            }
            other => panic!("expected a parsed mock match, got {other:?}"),
        };
        let early_response = parse_match(
            br#"{"track":{"key":"mock-track","title":"Mock Song","subtitle":"Test Artist","artist":"Unverified Alias","images":{"coverart":"https://example.test/cover.jpg","coverarthq":"https://example.test/cover-hq.jpg","background":"https://example.test/background.jpg"}},"alternativetrack":{"key":"alternative-track","title":"Alternative Song","subtitle":"Alternative Artist"},"matches":[{"key":"ignored-match","title":"Not the root callback","subtitle":"Ignored Artist"}],"sampleseconds":6,"intervalseconds":0}"#,
        );
        let alternative = TrackCandidate {
            track_id: Some("alternative-track".into()),
            title: "Alternative Song".into(),
            artist: "Alternative Artist".into(),
            ui_metadata: Some(NativeTrackUiMetadata {
                track_key: Some("alternative-track".into()),
                album_adam_id: None,
                isrc: None,
                date: None,
                artwork_url: None,
                hq_artwork_url: None,
                background_image_url: None,
            }),
            alternative_track: None,
            origin: TrackCandidateOrigin::AlternativeTrack,
        };
        let candidate = TrackCandidate {
            track_id: Some("mock-track".into()),
            title: "Mock Song".into(),
            artist: "Test Artist".into(),
            ui_metadata: Some(NativeTrackUiMetadata {
                track_key: Some("mock-track".into()),
                album_adam_id: None,
                isrc: None,
                date: None,
                artwork_url: Some("https://example.test/cover.jpg".into()),
                hq_artwork_url: Some("https://example.test/cover-hq.jpg".into()),
                background_image_url: Some("https://example.test/background.jpg".into()),
            }),
            alternative_track: Some(Box::new(alternative.clone())),
            origin: TrackCandidateOrigin::RootTrack,
        };
        assert_eq!(early_response.1, [candidate.clone()]);

        let mut early_session = make_session();
        early_session.push_i16_at(&samples, 3_000).unwrap();
        let first = early_session.poll(3_000).unwrap().unwrap();
        let early = apply_progressive_response(
            &mut early_session,
            first.attempt,
            early_response.0,
            early_response.1,
            3_200,
        )
        .unwrap();
        assert!(early.delivered_to_matcher);
        assert_eq!(early.candidates, [candidate.clone()]);
        assert_eq!(
            early.native_manual_ui_event(),
            Some(NativeManualUiEvent::Result(
                early.manual_result_event.clone().unwrap()
            ))
        );
        assert_eq!(
            early.manual_result_event.as_ref().unwrap().stage,
            NativeManualResultStage::WillFetchCompleteResult
        );
        let early_json: Value = serde_json::from_str(&native_manual_ui_event_to_json(
            &early.native_manual_ui_event().unwrap(),
        ))
        .unwrap();
        assert_eq!(early_json["stage"], "will_fetch_complete_result");
        assert_eq!(early_json["native_main_view_action"], "no_update");
        assert!(early_json["native_main_view_inputs"].is_null());
        assert_eq!(early_json["primary_track"]["title"], "Mock Song");
        assert_eq!(early_json["primary_track"]["artist"], "Test Artist");
        assert_eq!(
            early_json["primary_track"]["native_track_values"]["artwork_url"],
            "https://example.test/cover.jpg"
        );
        assert_eq!(
            early_json["alternative_track"]["track_id"],
            "alternative-track"
        );
        assert!(early_json.get("candidates").is_none());

        let mut final_session = make_session();
        final_session.push_i16_at(&samples, 3_000).unwrap();
        let first = final_session.poll(3_000).unwrap().unwrap();
        let final_response = parse_match(
            br#"{"track":{"key":"mock-track","title":"Mock Song","subtitle":"Test Artist","artist":"Unverified Alias","images":{"coverart":"https://example.test/cover.jpg","coverarthq":"https://example.test/cover-hq.jpg","background":"https://example.test/background.jpg"}},"alternativetrack":{"key":"alternative-track","title":"Alternative Song","subtitle":"Alternative Artist"}}"#,
        );
        assert_eq!(final_response.1, [candidate.clone()]);
        let final_result = apply_progressive_response(
            &mut final_session,
            first.attempt,
            final_response.0,
            final_response.1,
            3_200,
        )
        .unwrap();
        assert!(final_result.delivered_to_matcher);
        assert_eq!(
            final_result.native_manual_ui_event(),
            Some(NativeManualUiEvent::Result(
                final_result.manual_result_event.clone().unwrap()
            ))
        );
        assert_eq!(
            final_result.manual_result_event.as_ref().unwrap().stage,
            NativeManualResultStage::TaggingDidFinish
        );
        let final_json: Value = serde_json::from_str(&native_manual_ui_event_to_json(
            &final_result.native_manual_ui_event().unwrap(),
        ))
        .unwrap();
        assert_eq!(final_json["stage"], "tagging_did_finish");
        assert_eq!(final_json["native_main_view_action"], "display_primary");
        assert_eq!(final_json["native_main_view_inputs"]["title"], "Mock Song");
        assert_eq!(
            final_json["native_main_view_inputs"]["subtitle"],
            "Test Artist"
        );
        assert_eq!(
            final_json["native_main_view_inputs"]["artwork_url"],
            "https://example.test/cover.jpg"
        );
        assert_eq!(final_json["primary_track"]["title"], "Mock Song");
        assert_eq!(final_json["primary_track"]["artist"], "Test Artist");
        assert_eq!(
            final_json["alternative_track"]["track_id"],
            "alternative-track"
        );

        let mut cancelled_session = make_session();
        cancelled_session.push_i16_at(&samples, 3_000).unwrap();
        let first = cancelled_session.poll(3_000).unwrap().unwrap();
        cancelled_session.cancel();
        let cancelled_response = parse_match(
            br#"{"track":{"key":"mock-track","title":"Mock Song","subtitle":"Test Artist"}}"#,
        );
        let suppressed = apply_progressive_response(
            &mut cancelled_session,
            first.attempt,
            cancelled_response.0,
            cancelled_response.1,
            3_200,
        )
        .unwrap();
        assert!(!suppressed.delivered_to_matcher);
        assert!(suppressed.candidates.is_empty());
        assert!(suppressed.manual_result_event.is_none());
        assert!(suppressed.manual_terminal_event.is_none());
        assert!(suppressed.native_manual_ui_event().is_none());
    }

    #[test]
    fn applied_manual_no_match_and_terminal_failures_emit_native_callback_events() {
        let make_session = |tagging_type| {
            ProgressiveSession::new_for_type(
                RecognitionTiming {
                    sample_rate: 16_000,
                    minimum_signature_ms: 2_000,
                    initial_sample_ms: 3_000,
                    maximum_sample_ms: 6_000,
                    default_retry_interval_ms: NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS,
                    maximum_retry_duration_ms: Some(6_000),
                    session_timeout_ms: None,
                    request_timeout_ms: 5_000,
                    maximum_retry_delay_ms: None,
                    maximum_attempts: None,
                },
                tagging_type,
            )
            .unwrap()
        };
        let samples: Vec<i16> = (0..48_000)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        let cases = [
            (
                RecognitionOutcome::NoMatch { retry: None },
                NativeManualTerminalEvent::NoMatch,
            ),
            (
                RecognitionOutcome::MatcherFailure { retry: None },
                NativeManualTerminalEvent::Error {
                    kind: NativeManualErrorKind::MatcherFailure,
                },
            ),
            (
                RecognitionOutcome::InvalidResponse,
                NativeManualTerminalEvent::Error {
                    kind: NativeManualErrorKind::InvalidResponse,
                },
            ),
            (
                RecognitionOutcome::TransportFailure,
                NativeManualTerminalEvent::Error {
                    kind: NativeManualErrorKind::TransportFailure,
                },
            ),
        ];

        for (outcome, expected_event) in cases {
            let mut session = make_session(NativeTaggingType::Manual);
            session.push_i16_at(&samples, 3_000).unwrap();
            let job = session.poll(3_000).unwrap().unwrap();
            let delivery =
                apply_progressive_response(&mut session, job.attempt, outcome, Vec::new(), 3_200)
                    .unwrap();

            assert!(delivery.delivered_to_matcher);
            assert_eq!(delivery.manual_terminal_event, Some(expected_event));
            let expected_ui_event = match expected_event {
                NativeManualTerminalEvent::NoMatch => NativeManualUiEvent::NoMatch,
                NativeManualTerminalEvent::Error { kind } => NativeManualUiEvent::Error { kind },
            };
            assert_eq!(delivery.native_manual_ui_event(), Some(expected_ui_event));
        }

        let mut retrying_no_match_session = make_session(NativeTaggingType::Manual);
        retrying_no_match_session
            .push_i16_at(&samples, 3_000)
            .unwrap();
        let job = retrying_no_match_session.poll(3_000).unwrap().unwrap();
        let retrying_no_match = apply_progressive_response(
            &mut retrying_no_match_session,
            job.attempt,
            RecognitionOutcome::NoMatch {
                retry: Some(RetryDirective {
                    retry_duration_ms: Some(6_000),
                    interval_ms: Some(0),
                    retry_duration_seconds_bits: None,
                    interval_seconds_bits: None,
                }),
            },
            Vec::new(),
            3_200,
        )
        .unwrap();
        assert!(retrying_no_match.delivered_to_matcher);
        assert!(retrying_no_match.manual_terminal_event.is_none());
        assert!(retrying_no_match.native_manual_ui_event().is_none());
        assert_eq!(
            retrying_no_match.status,
            SessionStatus::WaitingForRetry {
                not_before_ms: 3_200,
                target_sample_ms: 6_000,
            }
        );

        let mut retrying_session = make_session(NativeTaggingType::Manual);
        retrying_session.push_i16_at(&samples, 3_000).unwrap();
        let job = retrying_session.poll(3_000).unwrap().unwrap();
        let retrying_failure = apply_progressive_response(
            &mut retrying_session,
            job.attempt,
            RecognitionOutcome::MatcherFailure {
                retry: Some(RetryDirective {
                    retry_duration_ms: Some(6_000),
                    interval_ms: Some(0),
                    retry_duration_seconds_bits: None,
                    interval_seconds_bits: None,
                }),
            },
            Vec::new(),
            3_200,
        )
        .unwrap();
        assert!(retrying_failure.manual_terminal_event.is_none());
        assert!(retrying_failure.native_manual_ui_event().is_none());

        let mut auto_session = make_session(NativeTaggingType::Auto);
        auto_session.push_i16_at(&samples, 3_000).unwrap();
        let job = auto_session.poll(3_000).unwrap().unwrap();
        let auto_no_match = apply_progressive_response(
            &mut auto_session,
            job.attempt,
            RecognitionOutcome::NoMatch { retry: None },
            Vec::new(),
            3_200,
        )
        .unwrap();
        assert!(auto_no_match.manual_terminal_event.is_none());
        assert!(auto_no_match.native_manual_ui_event().is_none());
    }

    #[test]
    fn response_parser_accepts_track_key_and_subtitle_shape() {
        let candidates = parse_track_candidates(
            br#"{"matches":[{"track":{"key":"mock-key","title":"Mock Song","subtitle":"Test Artist"}}]}"#,
        )
        .unwrap();
        assert_eq!(candidates[0].track_id.as_deref(), Some("mock-key"));
        assert_eq!(candidates[0].title, "Mock Song");
        assert_eq!(candidates[0].artist, "Test Artist");
    }

    #[test]
    fn native_track_key_prefers_valid_string_fields_without_id_fallbacks() {
        let cases = [
            (r#"{"key":"key-only"}"#, Some("key-only")),
            (r#"{"trackKey":"camel-only"}"#, Some("camel-only")),
            (
                r#"{"key":"preferred","trackKey":"secondary"}"#,
                Some("preferred"),
            ),
            (r#"{"key":null,"trackKey":"camel"}"#, Some("camel")),
            (r#"{"key":42,"trackKey":"camel"}"#, Some("camel")),
            (r#"{"key":"","trackKey":"camel"}"#, Some("camel")),
            (r#"{"trackid":"wire-id"}"#, None),
            (r#"{"trackId":123}"#, None),
        ];
        for (fields, expected) in cases {
            let body = format!(
                r#"{{"track":{{"title":"Song","subtitle":"Artist",{}}}}}"#,
                &fields[1..fields.len() - 1]
            );
            let candidates = parse_track_candidates(body.as_bytes()).unwrap();
            let metadata = candidates[0].ui_metadata.as_ref();
            assert_eq!(
                metadata.and_then(|metadata| metadata.track_key.as_deref()),
                expected
            );
        }

        let matches_only = parse_track_candidates(
            br#"{"matches":[{"track":{"trackid":"match-id","title":"Song","subtitle":"Artist"}}]}"#,
        )
        .unwrap();
        assert_eq!(matches_only[0].ui_metadata, None);
        assert_eq!(matches_only[0].track_id.as_deref(), Some("match-id"));
    }

    #[test]
    fn web_track_config_is_optional_bounded_and_independent_of_tag_route() {
        let base = json!({
            "inid":"private-installation-id",
            "apis":{"tag":{"href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"}},
        });
        let parse = |web: Option<Value>| {
            let mut root = base.clone();
            if let Some(web) = web {
                root["settings"] = json!({"webtrackurl": web});
            }
            parse_shazam_config(root.to_string().as_bytes()).unwrap()
        };
        assert_eq!(parse(None).web_track_url_template(), None);
        let mut settings_without_web_template = base.clone();
        settings_without_web_template["settings"] = json!({});
        assert_eq!(
            parse_shazam_config(settings_without_web_template.to_string().as_bytes())
                .unwrap()
                .web_track_url_template(),
            None
        );
        for invalid in [json!(null), json!(""), json!(7), json!([]), json!({})] {
            assert_eq!(parse(Some(invalid)).web_track_url_template(), None);
        }
        assert_eq!(
            parse(Some(json!("https://web.invalid/track/{trackid}"))).web_track_url_template(),
            Some("https://web.invalid/track/{trackid}")
        );
        // Current Config responses wrap the track-page URL inside href.
        let href_template = "https://www.shazam.com/track/{trackid}#referrer=shazamapp";
        let wrapped = parse(Some(json!({"href": href_template})));
        assert_eq!(wrapped.web_track_url_template(), Some(href_template));
        assert_eq!(
            wrapped
                .ui_config_snapshot(19)
                .web_track_url_template
                .as_deref(),
            Some(href_template)
        );
        for invalid in [
            json!({"href": null}),
            json!({"href": ""}),
            json!({"href": 7}),
            json!({"href": []}),
            json!({"href": {}}),
            json!({"href": "x".repeat(MAX_WEB_TRACK_URL_TEMPLATE_BYTES + 1)}),
        ] {
            assert_eq!(parse(Some(invalid)).web_track_url_template(), None);
        }
        assert_eq!(
            parse(Some(
                json!("x".repeat(MAX_WEB_TRACK_URL_TEMPLATE_BYTES + 1))
            ))
            .web_track_url_template(),
            None
        );
        let config = parse(Some(json!("https://web.invalid/track/{trackid}")));
        assert!(config
            .tag_url_template()
            .starts_with("https://amp.shazam.com/"));
        let snapshot = config.ui_config_snapshot(17);
        assert_eq!(snapshot.generation, 17);
        assert_eq!(
            snapshot.web_track_url_template.as_deref(),
            config.web_track_url_template()
        );
        let serialized: Value = serde_json::from_str(&snapshot.to_json()).unwrap();
        assert_eq!(serialized["schema"], "openshazam.ui-config-snapshot.v1");
        assert_eq!(serialized["generation"], 17);
        assert_eq!(
            serialized["web_track_url_template"],
            "https://web.invalid/track/{trackid}"
        );
        assert!(snapshot.to_json().contains("web.invalid"));
        assert!(!snapshot.to_json().contains("private-installation-id"));
        assert!(!snapshot.to_json().contains("amp.shazam.com"));

        let updated =
            parse(Some(json!("https://new-web.invalid/song/{trackid}"))).ui_config_snapshot(18);
        assert_ne!(snapshot.generation, updated.generation);
        assert_ne!(
            snapshot.web_track_url_template,
            updated.web_track_url_template
        );
    }

    #[test]
    fn ui_recognition_input_preserves_result_schema_and_config_generation() {
        let input = UiRecognitionInput {
            config_generation: 23,
            event: UiRecognitionEvent::Manual(NativeManualUiEvent::NoMatch),
        };
        let record: Value = serde_json::from_str(&input.to_json()).unwrap();
        assert_eq!(record["schema"], "openshazam.ui-recognition-input.v1");
        assert_eq!(record["config_generation"], 23);
        assert_eq!(
            record["recognition_event"]["schema"],
            "openshazam.manual-ui-event.v2"
        );
        assert_eq!(record["recognition_event"]["event"], "no_match");
        assert_eq!(
            record["recognition_event"],
            serde_json::from_str::<Value>(&native_manual_ui_event_to_json(
                &NativeManualUiEvent::NoMatch
            ))
            .unwrap()
        );

        let auto_candidates = parse_track_candidates(
            br#"{"matches":[{"trackid":"auto-key","title":"Auto Song","subtitle":"Auto Artist"}]}"#,
        )
        .unwrap();
        let auto_event = NativeAutoUiEvent::ContinuousMatchCandidate(
            native_auto_result_event(auto_candidates).unwrap(),
        );
        let auto_record: Value = UiRecognitionInput {
            config_generation: 25,
            event: UiRecognitionEvent::Auto(auto_event.clone()),
        }
        .to_json()
        .parse()
        .unwrap();
        assert_eq!(auto_record["config_generation"], 25);
        assert_eq!(
            auto_record["recognition_event"],
            serde_json::from_str::<Value>(&native_auto_ui_event_to_json(&auto_event)).unwrap()
        );

        let candidates =
            parse_track_candidates(br#"{"track":{"title":"Song","subtitle":"Artist"}}"#).unwrap();
        let no_key_event =
            NativeManualUiEvent::Result(native_manual_result_event(candidates, None).unwrap());
        let missing_web_config = UiConfigSnapshot {
            generation: 24,
            web_track_url_template: None,
        };
        let result_without_key = UiRecognitionInput {
            config_generation: missing_web_config.generation,
            event: UiRecognitionEvent::Manual(no_key_event),
        };
        assert!(missing_web_config.web_track_url_template.is_none());
        assert_eq!(
            serde_json::from_str::<Value>(&result_without_key.to_json()).unwrap()
                ["config_generation"],
            24
        );

        let alternative = parse_track_candidates(
            br#"{"track":{"key":"primary-key","title":"Song","subtitle":"Artist"},"alternativetrack":{"trackKey":"alternative-key","title":"Other Song","subtitle":"Other Artist"}}"#,
        )
        .unwrap();
        assert_eq!(
            alternative[0]
                .alternative_track
                .as_deref()
                .and_then(|track| track.ui_metadata.as_ref())
                .and_then(|metadata| metadata.track_key.as_deref()),
            Some("alternative-key")
        );
    }

    #[test]
    fn native_root_track_uses_subtitle_before_artist_alias() {
        let candidates = parse_track_candidates(
            br#"{"track":{"title":"Mock Song","subtitle":"Native Subtitle","artist":"Other Alias"}}"#,
        )
        .unwrap();
        assert_eq!(candidates[0].artist, "Native Subtitle");

        let scalar_artist_only = parse_track_candidates(
            br#"{"track":{"title":"Mock Song","artist":"Unverified Alias"}}"#,
        );
        assert!(scalar_artist_only.is_err());

        let diagnostic_matches = parse_track_candidates(
            br#"{"matches":[{"title":"Mock Song","subtitle":"Native Subtitle","artist":"Diagnostic Artist"}]}"#,
        )
        .unwrap();
        assert_eq!(diagnostic_matches[0].artist, "Diagnostic Artist");
    }

    #[test]
    fn response_parser_preserves_native_track_display_values_when_present() {
        let candidates = parse_track_candidates(
            br#"{"track":{"key":"mock-key","title":"Mock Song","subtitle":"Test Artist","albumadamid":12345,"isrc":"USABC2600001","date":"2026-01-02","images":{"coverart":"https://img.test/cover.jpg","coverarthq":"https://img.test/cover-hq.jpg"}}}"#,
        )
        .unwrap();
        let metadata = candidates[0].ui_metadata.as_ref().unwrap();
        assert_eq!(metadata.track_key.as_deref(), Some("mock-key"));
        assert_eq!(metadata.album_adam_id.as_deref(), Some("12345"));
        assert_eq!(metadata.isrc.as_deref(), Some("USABC2600001"));
        assert_eq!(metadata.date.as_deref(), Some("2026-01-02"));
        assert_eq!(
            metadata.artwork_url.as_deref(),
            Some("https://img.test/cover.jpg")
        );
        assert_eq!(
            metadata.hq_artwork_url.as_deref(),
            Some("https://img.test/cover-hq.jpg")
        );
        assert_eq!(
            metadata.background_image_url.as_deref(),
            Some("https://img.test/cover.jpg"),
            "Native background image falls back to the standard artwork URL"
        );

        let event =
            NativeManualUiEvent::Result(native_manual_result_event(candidates, None).unwrap());
        let event: Value = serde_json::from_str(&native_manual_ui_event_to_json(&event)).unwrap();
        assert_eq!(
            event["primary_track"]["native_track_values"]["artwork_url"],
            "https://img.test/cover.jpg"
        );
        assert_eq!(
            event["primary_track"]["native_track_values"]["track_key"],
            "mock-key"
        );
    }

    #[test]
    fn response_parser_maps_native_alternative_dictionary_separately_from_matches() {
        let candidates = parse_track_candidates(
            br#"{"track":{"key":"primary-key","title":"Primary Song","subtitle":"Primary Artist"},"alternativetrack":{"trackKey":"alternative-key","title":"Alternative Song","artist":"Alternative Artist"}}"#,
        )
        .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].track_id.as_deref(), Some("primary-key"));
        let alternative = candidates[0].alternative_track.as_deref().unwrap();
        assert_eq!(alternative.track_id.as_deref(), Some("alternative-key"));
        assert_eq!(alternative.title, "Alternative Song");
        assert_eq!(alternative.artist, "Alternative Artist");

        let empty_alternative = parse_track_candidates(
            br#"{"track":{"key":"primary-key","title":"Primary Song","subtitle":"Primary Artist"},"alternativetrack":{}}"#,
        )
        .unwrap();
        assert!(empty_alternative[0].alternative_track.is_none());

        let matches = parse_track_candidates(
            br#"{"matches":[{"trackid":"first","title":"First","artist":"A"},{"trackid":"second","title":"Second","artist":"B"}]}"#,
        )
        .unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].track_id.as_deref(), Some("first"));
        assert_eq!(matches[1].track_id.as_deref(), Some("second"));
        assert!(matches[0].alternative_track.is_none());
        assert!(matches[1].alternative_track.is_none());
    }

    #[test]
    fn response_parser_accepts_top_level_track_alongside_match_offsets() {
        let candidates = parse_track_candidates(
            br#"{"matches":[{"id":"match-1","offset":0.25,"timeskew":0.0,"frequencyskew":0.0}],"track":{"key":"track-1","title":"Mock Song","subtitle":"Test Artist"}}"#,
        )
        .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].track_id.as_deref(), Some("track-1"));
        assert_eq!(candidates[0].title, "Mock Song");
        assert_eq!(candidates[0].artist, "Test Artist");
    }

    #[test]
    fn response_shape_summary_reports_keys_and_counts_without_values() {
        let summary = summarize_response_shape(
            br#"{"matches":[{"track":{"title":"secret title","subtitle":"secret artist"},"id":"private-id"}],"requestId":"private-request"}"#,
        );
        assert!(summary.contains("root keys=matches,requestId"));
        assert!(summary.contains("matches=array[1]"));
        assert!(summary.contains("first-match keys=id,track"));
        assert!(!summary.contains("secret"));
        assert!(!summary.contains("private"));
    }

    #[test]
    fn endpoint_token_replacement_matches_static_string_replacement() {
        let resolved = substitute_endpoint_tokens(
            "https://fixture.invalid/tag/{tagid}?provider={streamingprovider}&inid={inid}",
            "tag-123",
            "provider-x",
            "install-456",
        );
        assert_eq!(
            resolved,
            "https://fixture.invalid/tag/tag-123?provider=provider-x&inid=install-456"
        );
    }

    fn config_profile() -> ConfigRequestProfile {
        ConfigRequestProfile {
            country: "-".into(),
            language: "ja".into(),
            locale: "ja_JP".into(),
            model: "MacBookPro18,2".into(),
            os_version: "15.7.4".into(),
            os_build: "24G517".into(),
            app_version: SHAZAM_MAC_APP_VERSION.into(),
        }
    }

    #[test]
    fn config_url_encodes_host_profile_and_uses_country_fallback() {
        assert_eq!(
            build_config_url(&config_profile()).unwrap(),
            "https://config.shazam.com/configuration/v1/-/bag?app=SHAZAM&l=ja&locale=ja_JP&model=MacBookPro18%2C2&platform=MAC&os=MACOS&osVersion=15.7.4&appVersion=2.11.0&osBuild=24G517"
        );
        let mut profile = config_profile();
        profile.country = "../amp.shazam.com".into();
        assert!(matches!(
            build_config_url(&profile),
            Err(Error::InvalidRequest(_))
        ));
    }

    #[test]
    fn config_fixture_yields_ephemeral_inid_and_dynamic_tag_route() {
        let fixture = br#"{
            "inid":"fixture-installation-id",
            "apis":{"tag":{"href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true"}},
            "unrelated":"must not be retained"
        }"#;
        let config = parse_shazam_config(fixture).unwrap();
        let endpoint = config.resolve_tag_url("fixture-tag-id", "").unwrap();
        assert_eq!(
            endpoint,
            "https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/fixture-installation-id/fixture-tag-id?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"
        );
        assert_eq!(config.tag_url_template().matches("{inid}").count(), 1);
    }

    #[test]
    fn native_manual_ui_config_exposes_observed_inputs_without_secrets() {
        let fixture = br#"{
            "inid":"fixture-installation-id",
            "apis":{"tag":{
                "href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true",
                "sampleseconds":4.75,
                "progressiveseconds":1.375,
                "minsamplems":null,
                "maxsamplems":5000
            }},
            "settings":{"recording":{"silentrecordingseconds":2.999,"maxtagseconds":null}}
        }"#;
        let configuration = parse_shazam_config(fixture).unwrap();
        let json = configuration
            .native_manual_ui_config_json(44_100, 135_168, false, 60_000)
            .unwrap();
        let event: Value = serde_json::from_str(&json).unwrap();

        assert_eq!(event["schema"], "openshazam.manual-ui-config.v1");
        assert_eq!(event["event"], "session_config");
        assert_eq!(event["native_ui_start_inputs"]["tagging_type"], 1);
        assert_eq!(event["native_ui_start_inputs"]["debugging_enabled"], false);
        assert_eq!(
            event["native_ui_start_inputs"].get("tagging_origin_code"),
            None
        );
        assert_eq!(event["native_origin_observations"][0]["code"], 14);
        assert_eq!(event["native_origin_observations"][0]["label"], "webbridge");
        assert_eq!(
            event["native_origin_observations"][0]["scope"],
            "one_observed_main_view_callsite"
        );
        assert_eq!(event["native_controls"]["use_buffered_audio"], true);
        assert_eq!(event["sigx"]["signature_type"], 2);
        assert_eq!(event["sigx"]["signature_options"], 0);
        assert_eq!(event["sigx"]["input"]["sample_rate_hz"], 44_100);
        assert_eq!(event["sigx"]["input"]["channels"], 1);
        assert_eq!(event["sigx"]["input"]["sample_format"], "mono_float32");
        assert_eq!(
            event["native_controls"]["pre_record"]["configured_integer_seconds"],
            2
        );
        assert_eq!(
            event["native_controls"]["pre_record"]["effective_seconds"],
            3
        );
        assert_eq!(
            event["native_controls"]["pre_record"]["capacity_frames"],
            135_168
        );
        assert_eq!(
            event["native_controls"]["pre_record"]["capacity_ms_ceil"],
            3_066
        );
        assert_eq!(
            event["native_controls"]["pre_record"]["capacity_source"],
            "native_page_rounded"
        );
        assert_eq!(event["timing"]["config_sample_seconds"], 4.75);
        assert_eq!(event["timing"]["config_progressive_seconds"], 1.375);
        assert_eq!(
            event["timing"]["effective_initial_signature_seconds"],
            1.375
        );
        assert_eq!(event["timing"]["initial_signature_target_frames"], 60_637);
        assert_eq!(event["timing"]["maximum_recording_frames"], 209_475);
        assert_eq!(event["timing"]["initial_signature_target_ms"], 1_375);
        assert_eq!(event["timing"]["maximum_recording_ms"], 4_750);
        assert_eq!(event["timing"]["minimum_signature_config_ms"], Value::Null);
        assert_eq!(event["timing"]["minimum_signature_effective_ms"], 2_000);
        assert_eq!(event["timing"]["maximum_signature_config_ms"], 5_000);
        assert_eq!(event["timing"]["maximum_signature_effective_ms"], 5_000);
        assert_eq!(event["timing"]["session_timeout_config_ms"], Value::Null);
        assert_eq!(
            event["timing"]["session_timeout_effective_ms"],
            NATIVE_TAGGING_TYPE_1_DEFAULT_SESSION_TIMEOUT_MS
        );
        assert_eq!(event["timing"]["request_timeout_ms"], 60_000);
        assert_eq!(event["native_controls"]["default_retry_interval_ms"], 0);
        assert!(!json.contains("fixture-installation-id"));
        assert!(!json.contains("amp.shazam.com"));
    }

    #[test]
    fn native_manual_ui_config_reports_sample_seconds_fallback() {
        let configuration = parse_shazam_config(
            br#"{"inid":"fixture-id","apis":{"tag":{"href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true","sampleseconds":12}}}"#,
        )
        .unwrap();
        let event: Value = serde_json::from_str(
            &configuration
                .native_manual_ui_config_json(48_000, 147_456, false, 60_000)
                .unwrap(),
        )
        .unwrap();

        assert_eq!(event["timing"]["config_progressive_seconds"], Value::Null);
        assert_eq!(event["timing"]["config_sample_seconds"], 12.0);
        assert_eq!(event["timing"]["effective_initial_signature_seconds"], 12.0);
        assert_eq!(event["timing"]["initial_signature_target_frames"], 576_000);
        assert_eq!(event["timing"]["maximum_recording_frames"], 576_000);
    }

    #[test]
    fn native_auto_ui_config_exposes_type2_controls_without_config_secrets() {
        let configuration = parse_shazam_config(
            br#"{
                "inid":"fixture-installation-id",
                "apis":{
                    "tag":{"href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true","sampleseconds":4.75,"progressiveseconds":1.375,"minsamplems":2000,"maxsamplems":6000},
                    "autotag":{"foregroundhref":"https://amp.shazam.com/foreground/{inid}/{tagid}","backgroundhref":"https://amp.shazam.com/background/{inid}/{tagid}","continuousforegroundidletimeout":48}
                }
            }"#,
        )
        .unwrap();
        let json = configuration
            .native_auto_ui_config_json(44_100, NativeTaggingRequestContext::Background, 60_000)
            .unwrap();
        let event: Value = serde_json::from_str(&json).unwrap();

        assert_eq!(event["schema"], "openshazam.auto-ui-config.v1");
        assert_eq!(event["event"], "session_config");
        assert_eq!(event["caller_inputs"]["tagging_type"], 2);
        assert_eq!(event["caller_inputs"]["request_context"], "background");
        assert_eq!(event["native_controls"]["use_buffered_audio"], false);
        assert_eq!(event["native_controls"]["pre_record_enabled"], false);
        assert_eq!(
            event["native_controls"]["default_retry_interval_ms"],
            12_000
        );
        assert_eq!(event["timing"]["config_sample_seconds"], 4.75);
        assert_eq!(event["timing"]["config_progressive_seconds"], 1.375);
        assert_eq!(event["timing"]["progressive_seconds_applied"], false);
        assert_eq!(event["timing"]["initial_signature_target_frames"], 209_475);
        assert_eq!(event["timing"]["maximum_recording_frames"], 209_475);
        assert_eq!(event["timing"]["session_timeout_effective_ms"], 48_000);
        assert!(!json.contains("fixture-installation-id"));
        assert!(!json.contains("amp.shazam.com"));
    }

    #[test]
    fn config_parser_uses_native_selected_timing_blocks() {
        let fixture = br#"{
            "inid":"fixture-installation-id",
            "apis":{
                "tag":{
                    "href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true",
                    "sampleseconds":12,"progressiveseconds":3,"minsamplems":3000,"maxsamplems":12000
                },
                "autotag":{
                    "sampleseconds":90,"progressiveseconds":60,"minsamplems":1000,"maxsamplems":90000,
                    "continuousforegroundidletimeout":48
                },
                "rollingtag":{
                    "sampleseconds":99,"progressiveseconds":80,"minsamplems":500,"maxsamplems":99000
                }
            },
            "settings":{"recording":{"silentrecordingseconds":6,"maxtagseconds":22}},
            "other":{"sampleseconds":99,"progressiveseconds":99,"minsamplems":999,"maxsamplems":999000}
        }"#;
        let configuration = parse_shazam_config(fixture).unwrap();
        assert_eq!(
            configuration.manual_native_frame_targets(44_100).unwrap(),
            NativeSampleFrameTargets {
                initial_frames: 132_300,
                maximum_frames: 529_200,
            }
        );
        assert_eq!(
            configuration.manual_timing(),
            ManualTaggingTiming {
                sample_ms: Some(12_000),
                progressive_ms: Some(3_000),
                pre_record_buffer_ms: Some(6_000),
                minimum_signature_ms: Some(3_000),
                maximum_signature_ms: Some(12_000),
                session_timeout_ms: Some(48_000),
                auto_session_timeout_ms: Some(48_000),
            }
        );
        assert_eq!(
            configuration.manual_timing().effective_initial_sample_ms(),
            Some(3_000)
        );
        let timing = configuration
            .manual_timing()
            .recognition_timing(
                44_100,
                NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
                0,
                20_000,
                Some(30_000),
                Some(4),
            )
            .unwrap();
        assert_eq!(timing.initial_sample_ms, 3_000);
        assert_eq!(timing.maximum_sample_ms, 12_000);
        assert_eq!(timing.default_retry_interval_ms, 0);
        assert_eq!(timing.maximum_retry_duration_ms, Some(12_000));
        assert_eq!(timing.session_timeout_ms, Some(48_000));
        let auto_timing = configuration
            .manual_timing()
            .auto_recognition_timing(
                44_100,
                NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
                12_000,
                20_000,
                Some(30_000),
                Some(4),
            )
            .unwrap();
        assert_eq!(auto_timing.initial_sample_ms, 12_000);
        assert_eq!(auto_timing.maximum_sample_ms, 12_000);
        assert_eq!(auto_timing.session_timeout_ms, Some(48_000));

        let no_timeout_config = parse_shazam_config(
            br#"{"inid":"fixture-installation-id","apis":{"tag":{"href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true","sampleseconds":12},"autotag":{"maxsamplems":90000}}}"#,
        )
        .unwrap();
        assert_eq!(no_timeout_config.manual_timing().session_timeout_ms, None);
        assert_eq!(
            no_timeout_config
                .manual_timing()
                .effective_initial_sample_ms(),
            Some(12_000)
        );
        assert_eq!(no_timeout_config.manual_timing().pre_record_buffer_ms, None);
        assert_eq!(
            no_timeout_config
                .manual_timing()
                .effective_pre_record_buffer_ms(),
            NATIVE_DEFAULT_PRE_RECORD_BUFFER_MS
        );
        assert_eq!(
            no_timeout_config
                .manual_timing()
                .effective_maximum_signature_length_ms(),
            NATIVE_DEFAULT_MAX_SIGNATURE_LENGTH_MS
        );
        let native_timing = no_timeout_config
            .manual_timing()
            .recognition_timing(
                44_100,
                NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
                12_000,
                20_000,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            native_timing.minimum_signature_ms,
            NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS
        );
        assert_eq!(native_timing.initial_sample_ms, 12_000);
        assert_eq!(
            native_timing.maximum_retry_duration_ms,
            Some(NATIVE_DEFAULT_MAX_SIGNATURE_LENGTH_MS)
        );
        assert_eq!(
            native_timing.session_timeout_ms,
            Some(NATIVE_TAGGING_TYPE_1_DEFAULT_SESSION_TIMEOUT_MS)
        );
        let auto_native_timing = no_timeout_config
            .manual_timing()
            .auto_recognition_timing(
                44_100,
                NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
                12_000,
                20_000,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            auto_native_timing.session_timeout_ms,
            Some(NATIVE_TAGGING_TYPE_2_DEFAULT_SESSION_TIMEOUT_MS)
        );
    }

    #[test]
    fn tagging_type_and_context_select_manual_foreground_or_background_route() {
        let configuration = parse_shazam_config(
            br#"{
                "inid":"fixture-installation-id",
                "apis":{
                    "tag":{"href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true","sampleseconds":12,"progressiveseconds":3},
                    "autotag":{"foregroundhref":"https://amp.shazam.com/fixture/continuous/{inid}/{tagid}?mode=foreground&connected={streamingprovider}","backgroundhref":"https://amp.shazam.com/fixture/continuous/{inid}/{tagid}?mode=background"}
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            configuration
                .resolve_tag_url_for_type(NativeTaggingType::Manual, None, "manual-id", "")
                .unwrap(),
            "https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/fixture-installation-id/manual-id?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"
        );
        let continuous = configuration
            .resolve_tag_url_for_type(
                NativeTaggingType::Auto,
                Some(NativeTaggingRequestContext::Foreground),
                "auto-id",
                "",
            )
            .unwrap();
        assert_eq!(
            continuous,
            "https://amp.shazam.com/fixture/continuous/fixture-installation-id/auto-id?mode=foreground&connected="
        );
        assert!(validate_shazam_request_endpoint(&continuous).is_ok());
        assert!(validate_shazam_endpoint(&continuous).is_err());

        let background = configuration
            .resolve_tag_url_for_type(
                NativeTaggingType::Auto,
                Some(NativeTaggingRequestContext::Background),
                "auto-id",
                "",
            )
            .unwrap();
        assert_eq!(
            background,
            "https://amp.shazam.com/fixture/continuous/fixture-installation-id/auto-id?mode=background"
        );
        assert!(validate_shazam_request_endpoint(&background).is_ok());
        assert!(configuration
            .resolve_tag_url_for_type(NativeTaggingType::Auto, None, "auto-id", "")
            .is_err());
    }

    #[test]
    fn continuous_foreground_route_is_optional_and_host_restricted() {
        let configuration = parse_shazam_config(
            br#"{"inid":"fixture-id","apis":{"tag":{"href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"}}}"#,
        )
        .unwrap();
        assert!(configuration
            .resolve_continuous_tag_url("auto-id", "")
            .is_err());
        assert!(configuration
            .resolve_continuous_tag_url_for_context(
                NativeTaggingRequestContext::Background,
                "auto-id",
                "",
            )
            .is_err());
        assert!(resolve_shazam_continuous_tag_url(
            "https://example.com/continuous/{inid}/{tagid}",
            "fixture-id",
            "auto-id",
            ""
        )
        .is_err());
        assert!(resolve_shazam_continuous_tag_url(
            "https://amp.shazam.com/continuous/{inid}/{unknown}",
            "fixture-id",
            "auto-id",
            ""
        )
        .is_err());
    }

    #[test]
    fn native_config_frame_targets_preserve_submillisecond_seconds() {
        let fixture = br#"{
            "inid":"fixture-installation-id",
            "apis":{"tag":{
                "href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true",
                "sampleseconds":1.2345,"progressiveseconds":1.0001,
                "minsamplems":1000,"maxsamplems":2000
            }}
        }"#;
        let configuration = parse_shazam_config(fixture).unwrap();
        let timing = configuration
            .manual_timing()
            .recognition_timing(44_100, 1_000, 0, 60_000, None, None)
            .unwrap();

        // Native uses the raw JSON Double: floor(44,100 * seconds). The
        // legacy integer-ms route would instead produce 44,100 and 54,463.
        let native_targets = configuration.manual_native_frame_targets(44_100).unwrap();
        let auto_native_targets = configuration
            .native_frame_targets_for_type(NativeTaggingType::Auto, 44_100)
            .unwrap();
        assert_eq!(timing.initial_sample_ms, 1_000);
        assert_eq!(timing.maximum_sample_ms, 1_235);
        assert_eq!(native_targets.initial_frames, 44_104);
        assert_eq!(native_targets.maximum_frames, 54_441);
        // Type 2 ignores progressiveRecordingSeconds and uses the raw
        // sampleSeconds Double for both the first and maximum cycle target.
        assert_eq!(auto_native_targets.initial_frames, 54_441);
        assert_eq!(auto_native_targets.maximum_frames, 54_441);
        assert_eq!(44_100u64 * timing.initial_sample_ms / 1_000, 44_100);
        assert_eq!(44_100u64 * timing.maximum_sample_ms / 1_000, 54_463);

        let auto_timing = configuration
            .manual_timing()
            .auto_recognition_timing(44_100, 1_000, 12_000, 60_000, None, None)
            .unwrap();
        let mut auto_session = ProgressiveSession::new_for_type_with_native_frame_targets(
            auto_timing,
            NativeTaggingType::Auto,
            0,
            Some(auto_native_targets),
        )
        .unwrap();
        let samples = vec![0_i16; auto_native_targets.maximum_frames];
        auto_session.push_i16_at(&samples[..54_440], 1_234).unwrap();
        assert!(auto_session.poll(1_234).unwrap().is_none());
        auto_session.push_i16_at(&samples[54_440..], 1_235).unwrap();
        let job = auto_session.poll(1_235).unwrap().unwrap();
        assert_eq!(job.sample_ms, 1_234);
        assert_eq!(auto_session.tagging_type(), NativeTaggingType::Auto);
    }

    #[test]
    fn config_parser_preserves_native_session_timeouts_beyond_audio_duration_limit() {
        let configuration = parse_shazam_config(
            br#"{
                "inid":"fixture-installation-id",
                "apis":{"tag":{"href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true","sampleseconds":12,"progressiveseconds":3},
                        "autotag":{"continuousforegroundidletimeout":21600}},
                "settings":{"recording":{"maxtagseconds":3600}}
            }"#,
        )
        .unwrap();
        assert_eq!(
            configuration.manual_timing().session_timeout_ms,
            Some(21_600_000)
        );
        assert_eq!(
            configuration.manual_timing().auto_session_timeout_ms,
            Some(21_600_000)
        );

        let manual = configuration
            .manual_timing()
            .recognition_timing(
                44_100,
                NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
                NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS,
                NATIVE_NETWORK_REQUEST_TIMEOUT_MS,
                None,
                None,
            )
            .unwrap();
        let auto = configuration
            .manual_timing()
            .auto_recognition_timing(
                44_100,
                NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
                NATIVE_TAGGING_TYPE_2_DEFAULT_INTERVAL_MS,
                NATIVE_NETWORK_REQUEST_TIMEOUT_MS,
                None,
                None,
            )
            .unwrap();
        assert_eq!(manual.session_timeout_ms, Some(21_600_000));
        assert_eq!(auto.session_timeout_ms, Some(21_600_000));
    }

    #[test]
    fn native_prerecord_default_applies_when_integer_seconds_are_at_most_two() {
        let base = ManualTaggingTiming {
            sample_ms: None,
            progressive_ms: None,
            pre_record_buffer_ms: None,
            minimum_signature_ms: None,
            maximum_signature_ms: None,
            session_timeout_ms: None,
            auto_session_timeout_ms: None,
        };
        assert_eq!(
            base.effective_pre_record_buffer_ms(),
            NATIVE_DEFAULT_PRE_RECORD_BUFFER_MS
        );
        for milliseconds in [0, 1_000, 2_000, 2_500, 2_999] {
            assert_eq!(
                ManualTaggingTiming {
                    pre_record_buffer_ms: Some(milliseconds),
                    ..base
                }
                .effective_pre_record_buffer_ms(),
                NATIVE_DEFAULT_PRE_RECORD_BUFFER_MS
            );
        }
        for milliseconds in [3_000, 3_001, 3_500] {
            assert_eq!(
                ManualTaggingTiming {
                    pre_record_buffer_ms: Some(milliseconds),
                    ..base
                }
                .effective_pre_record_buffer_ms(),
                3_000
            );
        }
    }

    #[test]
    fn silent_recording_config_matches_native_integer_seconds_conversion() {
        let below_threshold: Value = serde_json::from_slice(
            br#"{"settings":{"recording":{"silentRecordingSeconds":2.9999}}}"#,
        )
        .unwrap();
        let fractional_seconds: Value =
            serde_json::from_slice(br#"{"settings":{"recording":{"silentRecordingSeconds":3.9}}}"#)
                .unwrap();

        assert_eq!(
            parse_manual_tagging_timing(&below_threshold).effective_pre_record_buffer_ms(),
            NATIVE_DEFAULT_PRE_RECORD_BUFFER_MS
        );
        assert_eq!(
            parse_manual_tagging_timing(&fractional_seconds).effective_pre_record_buffer_ms(),
            3_000
        );
    }

    #[test]
    fn native_prerecord_capacity_rounds_mono_float_bytes_to_arm64_vm_pages() {
        let timing = ManualTaggingTiming {
            sample_ms: None,
            progressive_ms: None,
            pre_record_buffer_ms: None,
            minimum_signature_ms: None,
            maximum_signature_ms: None,
            session_timeout_ms: None,
            auto_session_timeout_ms: None,
        };

        assert_eq!(
            timing.effective_pre_record_capacity_frames(44_100).unwrap(),
            135_168
        );
        assert_eq!(
            timing.effective_pre_record_capacity_frames(48_000).unwrap(),
            147_456
        );
        assert!(timing.effective_pre_record_capacity_frames(0).is_err());
    }

    #[test]
    fn missing_native_minimum_uses_two_second_gate_before_signature_dispatch() {
        let fixture = br#"{
            "inid":"fixture-installation-id",
            "apis":{"tag":{
                "href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true",
                "sampleseconds":12,
                "progressiveseconds":1.5
            }}
        }"#;
        let configuration = parse_shazam_config(fixture).unwrap();
        let timing = configuration
            .manual_timing()
            .recognition_timing(
                16_000,
                NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
                NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS,
                60_000,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            timing.minimum_signature_ms,
            NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS
        );

        let mut session =
            ProgressiveSession::new_for_type(timing, NativeTaggingType::Manual).unwrap();
        session.push_i16(&vec![0; 24_000]).unwrap();
        assert_eq!(
            session.status(),
            shazam_progressive::SessionStatus::Failed(
                shazam_progressive::StopReason::SignatureGenerationFailure
            )
        );
        assert!(session.poll(1_500).unwrap().is_none());
        assert!(session.is_terminal());
    }

    #[test]
    fn parsed_config_controls_manual_progressive_signature_and_capture_ceiling() {
        let fixture = br#"{
            "inid":"fixture-installation-id",
            "apis":{"tag":{
                "href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true",
                "sampleseconds":4.75,
                "progressiveseconds":1.375,
                "minsamplems":500,
                "maxsamplems":4750
            }},
            "settings":{"recording":{"maxtagseconds":45}}
        }"#;
        let configuration = parse_shazam_config(fixture).unwrap();
        let timing = configuration
            .manual_timing()
            .recognition_timing(
                16_000,
                0,
                NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS,
                60_000,
                None,
                None,
            )
            .unwrap();
        assert_eq!(timing.initial_sample_ms, 1_375);
        assert_eq!(timing.maximum_sample_ms, 4_750);

        let mut session =
            ProgressiveSession::new_for_type(timing, NativeTaggingType::Manual).unwrap();
        let audio: Vec<i16> = (0..76_000)
            .map(|index| ((index * 37) % 30_001 - 15_000) as i16)
            .collect();
        for chunk in audio[..21_999].chunks(997) {
            session.push_i16_at(chunk, 1_374).unwrap();
        }
        assert!(session.poll(1_374).unwrap().is_none());

        session.push_i16_at(&audio[21_999..22_000], 1_375).unwrap();
        let first = session
            .poll(1_375)
            .unwrap()
            .expect("the Config progressive threshold is the first Signature target");
        assert_eq!(first.sample_ms, 1_375);
        assert_eq!(first.generated_at_ms, 1_375);

        // The audio adapter keeps feeding PCM while the recognition request
        // represented by `first` is in flight. The Config maximum is a
        // per-cycle ceiling, not a fixed 12-second send interval.
        let report = session.push_i16_at(&audio[22_000..], 1_400).unwrap();
        assert_eq!(report.accepted_frames, 54_000);
        assert_eq!(session.frames_seen(), 76_000);
        assert!(session.poll(1_400).unwrap().is_none());
        assert_eq!(
            session
                .complete(
                    first.attempt,
                    shazam_progressive::RecognitionOutcome::Match { retry: None },
                    1_500,
                )
                .unwrap(),
            shazam_progressive::SessionStatus::Succeeded
        );
    }

    #[test]
    fn config_timing_does_not_fall_through_to_other_blocks() {
        let value: Value = serde_json::from_slice(
            br#"{"apis":{"tag":{"sampleseconds":"invalid"},"autotag":{"sampleseconds":9,"progressiveseconds":8,"maxsamplems":90000,"continuousforegroundidletimeout":48}},"settings":{"recording":{"silentrecordingseconds":"invalid"}},"other":{"sampleseconds":12,"progressiveseconds":3,"silentrecordingseconds":6,"maxsamplems":12000}}"#,
        )
        .unwrap();
        assert_eq!(parse_manual_tagging_timing(&value).sample_ms, None);
        assert_eq!(
            parse_manual_tagging_timing(&value).pre_record_buffer_ms,
            None
        );
        let timing = parse_manual_tagging_timing(&value);
        assert_eq!(timing.progressive_ms, None);
        assert_eq!(timing.maximum_signature_ms, None);
        assert_eq!(timing.auto_session_timeout_ms, Some(48_000));
    }

    #[test]
    fn retry_response_fields_are_parsed_without_retaining_response_values() {
        assert_eq!(
            retry_directive_from_response(
                br#"{"retryms":6000,"sampleseconds":12,"intervalseconds":2}"#
            )
            .unwrap(),
            Some(RetryDirective {
                retry_duration_ms: Some(6_000),
                interval_ms: Some(0),
                retry_duration_seconds_bits: Some(6.0_f64.to_bits()),
                interval_seconds_bits: Some(0.0_f64.to_bits()),
            })
        );
        assert_eq!(
            retry_directive_from_response(br#"{"sampleseconds":6,"intervalseconds":2}"#).unwrap(),
            Some(RetryDirective {
                retry_duration_ms: Some(6_000),
                interval_ms: Some(2_000),
                retry_duration_seconds_bits: Some(6.0_f64.to_bits()),
                interval_seconds_bits: Some(2.0_f64.to_bits()),
            })
        );
        let fractional_retry =
            retry_directive_from_response(br#"{"sampleseconds":1.2345,"intervalseconds":0}"#)
                .unwrap()
                .unwrap();
        assert_eq!(fractional_retry.retry_duration_ms, Some(1_235));
        assert_eq!(
            fractional_retry.retry_duration_seconds_bits,
            Some(1.2345_f64.to_bits())
        );
        assert_eq!(fractional_retry.interval_ms, Some(0));
        assert_eq!(
            fractional_retry.interval_seconds_bits,
            Some(0.0_f64.to_bits())
        );
        let fractional_interval =
            retry_directive_from_response(br#"{"sampleseconds":1,"intervalseconds":1.2341}"#)
                .unwrap()
                .unwrap();
        assert_eq!(fractional_interval.interval_ms, Some(1_235));
        assert_eq!(
            fractional_interval.interval_seconds_bits,
            Some(1.2341_f64.to_bits())
        );
        let submillisecond_interval =
            retry_directive_from_response(br#"{"sampleseconds":1,"intervalseconds":0.0000009}"#)
                .unwrap()
                .unwrap();
        assert_eq!(submillisecond_interval.interval_ms, Some(1));
        assert_eq!(
            submillisecond_interval.interval_seconds_bits,
            Some(0.0000009_f64.to_bits())
        );
        let fractional_retry_ms = retry_directive_from_response(
            br#"{"retryms":1234.567,"sampleseconds":6,"intervalseconds":2}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(fractional_retry_ms.retry_duration_ms, Some(1_235));
        assert_eq!(
            fractional_retry_ms.retry_duration_seconds_bits,
            Some((1_234.567_f64 / 1_000.0).to_bits())
        );
        assert_eq!(fractional_retry_ms.interval_ms, Some(0));
        assert_eq!(
            fractional_retry_ms.interval_seconds_bits,
            Some(0.0_f64.to_bits())
        );
        let long_retry_ms = retry_directive_from_response(br#"{"retryms":600001}"#)
            .unwrap()
            .unwrap();
        assert_eq!(long_retry_ms.retry_duration_ms, Some(600_001));
        assert_eq!(
            long_retry_ms.retry_duration_seconds_bits,
            Some((600_001.0_f64 / 1_000.0).to_bits())
        );
        assert_eq!(
            retry_directive_from_response(br#"{"sampleseconds":6}"#).unwrap(),
            None
        );
        assert_eq!(
            retry_directive_from_response(br#"{"intervalseconds":2}"#).unwrap(),
            Some(RetryDirective {
                retry_duration_ms: Some(0),
                interval_ms: Some(2_000),
                retry_duration_seconds_bits: None,
                interval_seconds_bits: Some(2.0_f64.to_bits()),
            })
        );
        assert_eq!(
            retry_directive_from_response(br#"{"sampleseconds":"invalid","intervalseconds":null}"#)
                .unwrap(),
            Some(RetryDirective {
                retry_duration_ms: Some(0),
                interval_ms: Some(0),
                retry_duration_seconds_bits: None,
                interval_seconds_bits: Some(0.0_f64.to_bits()),
            })
        );
        assert_eq!(
            retry_directive_from_response(br#"{"retryInMilliseconds":1250}"#).unwrap(),
            None
        );
        assert_eq!(
            retry_directive_from_response(br#"{"matches":[]}"#).unwrap(),
            None
        );
        assert!(retry_directive_from_response(b"not json").is_err());
    }

    #[test]
    fn retry_parser_obeys_native_tag_response_presence_precedence() {
        assert_eq!(
            retry_directive_for_signature_from_response(br#"{"matches":[]}"#, 3_000, 12_000)
                .unwrap(),
            None
        );
        assert_eq!(
            retry_directive_for_signature_from_response(
                br#"{"retryms":2500,"intervalseconds":7}"#,
                3_000,
                12_000,
            )
            .unwrap(),
            Some(RetryDirective {
                retry_duration_ms: Some(2_500),
                interval_ms: Some(0),
                retry_duration_seconds_bits: Some(2.5_f64.to_bits()),
                interval_seconds_bits: Some(0.0_f64.to_bits()),
            })
        );
        assert_eq!(
            retry_directive_for_signature_from_response(
                br#"{"sampleseconds":6,"intervalseconds":2}"#,
                3_000,
                12_000,
            )
            .unwrap(),
            Some(RetryDirective {
                retry_duration_ms: Some(6_000),
                interval_ms: Some(2_000),
                retry_duration_seconds_bits: Some(6.0_f64.to_bits()),
                interval_seconds_bits: Some(2.0_f64.to_bits()),
            })
        );
        assert_eq!(
            retry_directive_for_signature_from_response(
                br#"{"sampleseconds":6,"intervalseconds":2}"#,
                12_000,
                12_000,
            )
            .unwrap(),
            Some(RetryDirective {
                retry_duration_ms: Some(6_000),
                interval_ms: Some(2_000),
                retry_duration_seconds_bits: Some(6.0_f64.to_bits()),
                interval_seconds_bits: Some(2.0_f64.to_bits()),
            })
        );
        assert_eq!(
            retry_directive_for_signature_from_response(br#"{"matches":[]}"#, 12_000, 12_000)
                .unwrap(),
            None
        );
        assert_eq!(
            retry_directive_for_signature_from_response(b"null", 3_000, 12_000).unwrap(),
            Some(RetryDirective {
                retry_duration_ms: Some(12_000),
                interval_ms: Some(0),
                retry_duration_seconds_bits: None,
                interval_seconds_bits: None,
            })
        );
    }

    #[test]
    fn network_failure_uses_native_retry_to_maximum_only_for_live_short_signature() {
        let network_error = Error::NetworkFailure("network failed".into());
        let local_error = Error::Transport("HTTPS client unavailable".into());
        let expected = Some(RetryDirective {
            retry_duration_ms: Some(12_000),
            interval_ms: Some(0),
            retry_duration_seconds_bits: None,
            interval_seconds_bits: None,
        });
        assert_eq!(
            native_retry_for_network_failure(&network_error, 3_000, 12_000, false),
            expected
        );
        assert_eq!(
            native_retry_for_network_failure(&network_error, 12_000, 12_000, false),
            None
        );
        assert_eq!(
            native_retry_for_network_failure(&network_error, 3_000, 12_000, true),
            None
        );
        assert_eq!(
            native_retry_for_network_failure(&local_error, 3_000, 12_000, false),
            None
        );
        assert_eq!(
            native_retry_for_network_failure_for_tagging_type(
                NativeTaggingType::Manual,
                &network_error,
                3_000,
                12_000,
                false,
            ),
            expected
        );
        assert_eq!(
            native_retry_for_network_failure_for_tagging_type(
                NativeTaggingType::Auto,
                &network_error,
                3_000,
                12_000,
                false,
            ),
            None
        );
    }

    #[test]
    fn native_network_failure_retry_extends_manual_session_to_maximum() {
        let maximum_sample_ms = 6_000;
        let timing = RecognitionTiming {
            sample_rate: 16_000,
            minimum_signature_ms: 2_000,
            initial_sample_ms: 3_000,
            maximum_sample_ms,
            default_retry_interval_ms: NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS,
            maximum_retry_duration_ms: Some(maximum_sample_ms),
            session_timeout_ms: None,
            request_timeout_ms: 5_000,
            maximum_retry_delay_ms: None,
            maximum_attempts: None,
        };
        let mut session =
            ProgressiveSession::new_for_type(timing, NativeTaggingType::Manual).unwrap();
        let samples: Vec<i16> = (0..96_000)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        session.push_i16_at(&samples[..48_000], 3_000).unwrap();
        let first = session.poll(3_000).unwrap().unwrap();
        assert_eq!(first.sample_ms, 3_000);

        let network_error = Error::NetworkFailure("request failed".into());
        let retry = native_retry_for_network_failure(
            &network_error,
            first.sample_ms,
            maximum_sample_ms,
            false,
        )
        .expect("a dispatched short Signature gets the Native maximum-length Retry");
        assert_eq!(retry.retry_duration_ms, Some(maximum_sample_ms));
        assert_eq!(retry.interval_ms, Some(0));
        assert_eq!(
            session
                .complete(
                    first.attempt,
                    shazam_progressive::RecognitionOutcome::MatcherFailure { retry: Some(retry) },
                    3_200,
                )
                .unwrap(),
            shazam_progressive::SessionStatus::WaitingForRetry {
                not_before_ms: 3_200,
                target_sample_ms: maximum_sample_ms,
            }
        );

        session
            .push_i16_at(&samples[48_000..], 6_000)
            .expect("Native-allowed Retry keeps accepting PCM");
        let followup = session.poll(6_000).unwrap().unwrap();
        assert_eq!(followup.attempt, 2);
        assert_eq!(followup.sample_ms, maximum_sample_ms);
    }

    #[test]
    fn progressive_response_classifier_covers_match_retry_http_and_invalid() {
        let track_response = br#"{"track":{"title":"Mock Song","subtitle":"Mock Artist"}}"#;
        for status in [200, 201, 299] {
            assert!(matches!(
                interpret_progressive_http_response(status, track_response),
                ProgressiveHttpResponse::Matched { .. }
            ));
        }
        for status in [199, 300] {
            assert_eq!(
                interpret_progressive_http_response(status, track_response),
                ProgressiveHttpResponse::HttpError {
                    status,
                    retry: None,
                }
            );
        }
        assert_eq!(
            interpret_progressive_http_response(
                200,
                br#"{"track":{"title":"Mock Song","subtitle":"Mock Artist"}}"#
            ),
            ProgressiveHttpResponse::Matched {
                candidates: vec![TrackCandidate {
                    track_id: None,
                    title: "Mock Song".into(),
                    artist: "Mock Artist".into(),
                    ui_metadata: None,
                    alternative_track: None,
                    origin: TrackCandidateOrigin::RootTrack,
                }],
                retry: None,
            }
        );
        assert_eq!(
            interpret_progressive_http_response(200, br#"{"matches":[]}"#),
            ProgressiveHttpResponse::NoMatch(None)
        );
        // Native parses a successful tag dictionary before extracting tracks;
        // an unknown but valid dictionary shape therefore represents an empty
        // tag set, not the nil-dictionary failure path that retries to max.
        assert_eq!(
            interpret_progressive_http_response(200, br#"{"unrecognized":true}"#),
            ProgressiveHttpResponse::NoMatch(None)
        );
        assert_eq!(
            interpret_progressive_http_response(
                200,
                br#"{"matches":[],"sampleseconds":6,"intervalseconds":0.25}"#
            ),
            ProgressiveHttpResponse::NoMatch(Some(RetryDirective {
                retry_duration_ms: Some(6_000),
                interval_ms: Some(250),
                retry_duration_seconds_bits: Some(6.0_f64.to_bits()),
                interval_seconds_bits: Some(0.25_f64.to_bits()),
            }))
        );
        assert_eq!(
            interpret_progressive_http_response(
                200,
                br#"{"track":{"title":"Mock Song","subtitle":"Mock Artist"},"sampleseconds":2.5,"intervalseconds":5}"#
            ),
            ProgressiveHttpResponse::Matched {
                candidates: vec![TrackCandidate {
                    track_id: None,
                    title: "Mock Song".into(),
                    artist: "Mock Artist".into(),
                    ui_metadata: None,
                    alternative_track: None,
                    origin: TrackCandidateOrigin::RootTrack,
                }],
                retry: Some(RetryDirective {
                    retry_duration_ms: Some(2_500),
                    interval_ms: Some(5_000),
                    retry_duration_seconds_bits: Some(2.5_f64.to_bits()),
                    interval_seconds_bits: Some(5.0_f64.to_bits()),
                }),
            }
        );
        assert_eq!(
            interpret_progressive_http_response(
                200,
                br#"{"matches":[],"retryInMilliseconds":250}"#
            ),
            ProgressiveHttpResponse::NoMatch(None)
        );
        assert_eq!(
            interpret_progressive_http_response(503, br#"{"matches":[]}"#),
            ProgressiveHttpResponse::HttpError {
                status: 503,
                retry: None,
            }
        );
        assert_eq!(
            interpret_progressive_http_response(
                503,
                br#"{"error":"temporarily unavailable","retryms":2500}"#
            ),
            ProgressiveHttpResponse::HttpError {
                status: 503,
                retry: None,
            }
        );
        assert_eq!(
            interpret_progressive_http_response_for_signature(
                503,
                br#"{"error":"temporarily unavailable","retryms":2500}"#,
                Some(3_000),
                Some(12_000),
            ),
            ProgressiveHttpResponse::HttpError {
                status: 503,
                retry: Some(RetryDirective {
                    retry_duration_ms: Some(12_000),
                    interval_ms: Some(0),
                    retry_duration_seconds_bits: None,
                    interval_seconds_bits: None,
                }),
            }
        );
        assert_eq!(
            interpret_progressive_http_response_for_signature(
                503,
                br#"{"error":"temporarily unavailable","retryms":2500}"#,
                Some(12_000),
                Some(12_000),
            ),
            ProgressiveHttpResponse::HttpError {
                status: 503,
                retry: None,
            }
        );
        assert_eq!(
            interpret_progressive_http_response(200, b"not json"),
            ProgressiveHttpResponse::ParseFailure { retry: None }
        );
    }

    #[test]
    fn malformed_tag_response_uses_native_failure_retry_with_request_context() {
        assert_eq!(
            interpret_progressive_http_response_for_signature(
                200,
                b"not json",
                Some(3_000),
                Some(12_000),
            ),
            ProgressiveHttpResponse::ParseFailure {
                retry: Some(RetryDirective {
                    retry_duration_ms: Some(12_000),
                    interval_ms: Some(0),
                    retry_duration_seconds_bits: None,
                    interval_seconds_bits: None,
                }),
            }
        );
        assert_eq!(
            interpret_progressive_http_response_for_signature(
                200,
                b"not json",
                Some(12_000),
                Some(12_000),
            ),
            ProgressiveHttpResponse::ParseFailure { retry: None }
        );
        assert_eq!(
            interpret_progressive_http_response(200, b"not json"),
            ProgressiveHttpResponse::ParseFailure { retry: None }
        );
    }

    #[test]
    fn config_parser_rejects_missing_ids_and_non_allowlisted_tag_routes() {
        assert!(matches!(
            parse_shazam_config(br#"{"apis":{"tag":{"href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"}}}"#),
            Err(Error::BadResponse(_))
        ));
        assert!(matches!(
            parse_shazam_config(br#"{"inid":"fixture-id","apis":{"tag":{"href":"https://example.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"}}}"#),
            Err(Error::InvalidEndpoint(_))
        ));
    }

    #[test]
    fn endpoint_resolver_rejects_unsafe_inid_or_streaming_provider() {
        let template = "https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true";
        assert!(resolve_shazam_tag_url(template, "id/escape", "tag", "").is_err());
        assert!(resolve_shazam_tag_url(template, "id", "tag", "x&extra=y").is_err());
    }

    #[test]
    fn live_endpoint_allows_only_resolved_manual_tag_template() {
        let endpoint = "https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/installer-1/tag-2?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true";
        assert!(validate_shazam_endpoint(endpoint).is_ok());
        assert!(validate_shazam_endpoint(
            "https://example.com/discovery/v5/ja/JP/mac/-/tag/installer-1/tag-2?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"
        )
        .is_err());
        assert!(validate_shazam_endpoint(
            "https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"
        )
        .is_err());
        assert!(validate_shazam_endpoint(
            "http://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/installer-1/tag-2?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"
        )
        .is_err());
    }

    #[test]
    fn loopback_http_roundtrip_posts_json_and_parses_mock_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            assert_eq!(request_line, "POST /mock HTTP/1.1\r\n");
            let mut content_length = None;
            let mut platform = None;
            let mut app_version = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.trim_end().split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        content_length = Some(value.trim().parse::<usize>().unwrap());
                    }
                    if name.eq_ignore_ascii_case("x-shazam-platform") {
                        platform = Some(value.trim().to_owned());
                    }
                    if name.eq_ignore_ascii_case("x-shazam-appversion") {
                        app_version = Some(value.trim().to_owned());
                    }
                }
            }
            assert_eq!(platform.as_deref(), Some("MAC"));
            assert_eq!(app_version.as_deref(), Some("2.11.0"));
            let mut body = vec![0; content_length.unwrap()];
            reader.read_exact(&mut body).unwrap();
            let parsed: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(parsed["signature"]["samplems"], 12_000);

            let response = br#"{"matches":[{"trackid":"mock-1","title":"Local Mock Song","artist":"Local Test Artist"}]}"#;
            let mut stream = reader.into_inner();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            )
            .unwrap();
            stream.write_all(response).unwrap();
        });

        let endpoint = format!("http://{address}/mock");
        let body = build_request_body(
            &valid_signature(),
            12_000,
            1_800_000_000_123,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let response = post_loopback_json(&endpoint, &body).unwrap();
        assert_eq!(response.status, 200);
        let candidates = parse_track_candidates(&response.body).unwrap();
        assert_eq!(candidates[0].title, "Local Mock Song");
        assert_eq!(candidates[0].artist, "Local Test Artist");
        server.join().unwrap();
    }

    #[test]
    fn transport_refuses_non_loopback_and_https_destinations() {
        let body = b"{}";
        assert!(matches!(
            post_loopback_json("http://example.com:80/match", body),
            Err(Error::InvalidEndpoint(_))
        ));
        assert!(matches!(
            post_loopback_json("https://127.0.0.1:443/match", body),
            Err(Error::InvalidEndpoint(_))
        ));
    }

    #[test]
    fn recognition_transport_is_injectable_behind_shared_request_validation() {
        struct TestTransport(std::sync::atomic::AtomicUsize);

        impl RecognitionHttpTransport for TestTransport {
            fn post_json_cancellable(
                &self,
                endpoint: &str,
                body: &[u8],
                timeout: Duration,
                _cancelled: &AtomicBool,
                on_transport_started: &mut dyn FnMut(),
            ) -> Result<HttpResponse, Error> {
                assert_eq!(endpoint, "https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/inid/tag?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true");
                assert_eq!(body, b"{\"signature\":{}}");
                assert_eq!(timeout, Duration::from_secs(7));
                self.0.fetch_add(1, Ordering::Relaxed);
                on_transport_started();
                Ok(HttpResponse {
                    status: 200,
                    body: b"{}".to_vec(),
                })
            }
        }

        let transport = TestTransport(std::sync::atomic::AtomicUsize::new(0));
        let started = std::cell::Cell::new(false);
        let response = post_shazam_json_with_transport(
            &transport,
            "https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/inid/tag?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true",
            b"{\"signature\":{}}",
            Duration::from_secs(7),
            &AtomicBool::new(false),
            || started.set(true),
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert!(started.get());
        assert_eq!(transport.0.load(Ordering::Relaxed), 1);

        assert!(post_shazam_json_with_transport(
            &transport,
            "https://example.com/not-allowlisted",
            b"{}",
            Duration::from_secs(7),
            &AtomicBool::new(false),
            || {},
        )
        .is_err());
        assert_eq!(transport.0.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn live_request_timeout_is_bounded_before_transport_start() {
        assert!(post_shazam_json_with_timeout("", b"{}", Duration::ZERO).is_err());
        assert!(post_shazam_json_with_timeout("", b"{}", Duration::from_secs(601)).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_host_profile_keeps_supported_mac_config_route_and_windows_fields() {
        let host = parse_windows_host_profile(
            br#"{"language":"ja","locale":"ja-JP","model":"Windows Test Device","os_version":"10.0.26200","os_build":"26200"}"#,
        )
        .unwrap();
        assert_eq!(host.locale, "ja_JP");
        let url = build_config_url(&ConfigRequestProfile {
            country: "-".into(),
            language: host.language,
            locale: host.locale,
            model: host.model,
            os_version: host.os_version,
            os_build: host.os_build,
            app_version: "2.11.0".into(),
        })
        .unwrap();
        assert!(url.contains("platform=MAC&os=MACOS"));
        assert!(url.contains("model=Windows%20Test%20Device"));
        assert!(url.contains("osVersion=10.0.26200"));
    }
}
