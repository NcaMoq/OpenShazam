//! OS-independent PCM-to-Signature recognition control.
//!
//! Audio adapters feed mono PCM with [`ProgressiveSession::push_i16`] or
//! [`ProgressiveSession::push_f32`]. A caller polls for a [`RecognitionJob`]
//! and runs its HTTP request on a separate worker. While that request is in
//! flight, more PCM can continue to be pushed and each Native-generated
//! Signature can become its own independent recognition request.
//! No audio source, HTTP client, clock, or platform API is owned here.
//!
//! Shazam 2.11.0 manual TaggingType 1 has a zero default retry interval and
//! enables `useBufferedAudio`. The Native recorder can therefore prefeed its
//! current ring-buffer contents when attaching the tagging tap. This crate
//! models the tagging/retry policy; capture adapters must explicitly provide
//! pre-roll PCM if they need to reproduce an already-running Native recorder.

use std::collections::{BTreeSet, VecDeque};

use sigx_core::{Config as SigXConfig, SigX};
use sigx_format::Signature;

const NANOS_PER_MILLISECOND: u64 = 1_000_000;

/// Statically observed in Shazam 2.11.0 Auto Shazam (TaggingType 2).
/// `SHKTaggingOptions.defaultIntervalSeconds` returns 12, and the Type 2
/// `SHKAudioClosure` uses it as the fallback between stop/start tagging
/// cycles when no Retry is present. This does not apply to manual TaggingType 1.
pub const NATIVE_TAGGING_TYPE_2_DEFAULT_INTERVAL_MS: u64 = 12_000;
/// Statically observed for ordinary manual recognition (TaggingType 1).
/// `SHKTaggingOptions.defaultIntervalSeconds` returns zero.
pub const NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS: u64 = 0;
/// Empirical Type-2/options-0 `SigX` header adjustment observed in Native
/// outputs: `sample_length_samples` is cumulative PCM samples for the current
/// generator cycle plus 240 ms at the active input rate. Verified at 16 kHz
/// (3/4/5/6/7 s), 44.1 kHz (3/6 s), and 48 kHz (3/6 s). The exact internal
/// origin of this adjustment remains unresolved.
pub const NATIVE_SIGX_SAMPLE_LENGTH_OFFSET_MS: u64 = 240;
/// Native `sig_stream::now()` subtracts its 45-frame peak warm-up from the
/// internal frame counter. `freeze_signature` clamps a negative result to
/// zero before passing it to `get_sig`. This is statically reconstructed from
/// ShazamTagging arm64 (`now` at 0x50980, `stream_flow` at 0x50e0c,
/// `flow_single_frame` at 0x67d6c, and the Type-2 freeze path at 0x67a38); it
/// has not been checked with a live Native trace.
pub const NATIVE_SIG_STREAM_INITIAL_DELAY_FRAMES: u64 = 45;
/// Shazam 2.11.0's common `SHNNetworkRequest` initializes
/// `timeoutInterval` to 60 seconds. The Manual tagging request builder calls
/// `init` and does not override it; this is a request timeout, not a
/// TaggingType 2 policy. Adapters may expose a local override for diagnostics.
pub const NATIVE_NETWORK_REQUEST_TIMEOUT_MS: u64 = 60_000;
/// ShazamFoundation's static fallback for `continuousForegroundIdleTimeout`.
/// The ShazamTagging `timeoutSeconds` getter uses this value for both ordinary
/// Manual Type 1 and Auto Type 2; its separate `maxTagSeconds` branch is for
/// TaggingType 7. The exact caller and purpose of that type are not established
/// here.
pub const NATIVE_DEFAULT_CONTINUOUS_FOREGROUND_IDLE_TIMEOUT_MS: u64 = 3_600_000;
/// Backward-compatible name for the ordinary Manual Type 1 fallback.
pub const NATIVE_TAGGING_TYPE_1_DEFAULT_SESSION_TIMEOUT_MS: u64 =
    NATIVE_DEFAULT_CONTINUOUS_FOREGROUND_IDLE_TIMEOUT_MS;
/// Backward-compatible name for the Auto Type 2 fallback.
pub const NATIVE_TAGGING_TYPE_2_DEFAULT_SESSION_TIMEOUT_MS: u64 =
    NATIVE_DEFAULT_CONTINUOUS_FOREGROUND_IDLE_TIMEOUT_MS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecognitionTiming {
    pub sample_rate: u32,
    /// Lower bound enforced by the native signature pipe before generation.
    pub minimum_signature_ms: u64,
    /// Initial cumulative audio target; this is not a wall-clock POST time.
    pub initial_sample_ms: u64,
    /// Per-cycle PCM ceiling from the Native tagging configuration.
    pub maximum_sample_ms: u64,
    /// Native default interval between cycles for the selected tagging type.
    pub default_retry_interval_ms: u64,
    /// Native maximumSignatureLength assertion threshold, if available. The
    /// ARM64 implementation only asserts above it when its exception flag is
    /// enabled; that runtime flag has not been dynamically verified.
    pub maximum_retry_duration_ms: Option<u64>,
    /// Native session timeout selected for the active TaggingType.
    pub session_timeout_ms: Option<u64>,
    /// Per-request timeout. Native Shazam 2.11.0's request object defaults to
    /// 60 seconds; adapters may override this local setting.
    pub request_timeout_ms: u64,
    /// Optional local safety bound for server-supplied retry delays.
    pub maximum_retry_delay_ms: Option<u64>,
    /// Optional local loop-prevention bound. Native stops by session timeout.
    pub maximum_attempts: Option<u64>,
}

/// Shazam 2.11.0 tagging policy relevant to capture/retry control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTaggingType {
    /// Ordinary user-triggered recognition (Shazam TaggingType 1).
    Manual,
    /// Continuous Auto Shazam (Shazam TaggingType 2).
    Auto,
}

impl NativeTaggingType {
    /// Whether Shazam 2.11.0 asks its recorder to prefeed buffered audio when
    /// attaching the tagging tap. This describes Native policy; it does not
    /// mean a capture adapter has a pre-session buffer available.
    pub const fn uses_pre_recorded_audio(self) -> bool {
        match self {
            Self::Manual => true,
            Self::Auto => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryDirective {
    /// Native retryDuration as a frame threshold from the current
    /// SignatureGenerator's start; it is not added to the last Signature's
    /// duration or carried across a restarted generator cycle.
    pub retry_duration_ms: Option<u64>,
    /// Original Native `sampleseconds` Double, stored as IEEE-754 bits when
    /// present so the engine can reproduce its direct seconds×sample-rate
    /// frame conversion instead of round-tripping through milliseconds.
    pub retry_duration_seconds_bits: Option<u64>,
    /// Original Native `intervalDuration` Double, retained as IEEE-754 bits.
    /// Live adapters retain this value until integer-nanosecond scheduling;
    /// `interval_ms` is only the millisecond-clock scheduling projection.
    pub interval_seconds_bits: Option<u64>,
    /// Native intervalDuration before a subsequent tagging cycle.
    pub interval_ms: Option<u64>,
}

impl RetryDirective {
    pub fn has_positive_retry_duration(self) -> bool {
        self.retry_duration_seconds_bits
            .map(|bits| f64::from_bits(bits).is_finite() && f64::from_bits(bits) > 0.0)
            .unwrap_or_else(|| self.retry_duration_ms.is_some_and(|duration| duration > 0))
    }
}

/// Exact cumulative frame thresholds derived from Native `secondsToRecord`.
/// The Mac implementation multiplies its original Config `Double` by the
/// capture sample rate and truncates; callers that only have millisecond
/// durations can continue using the existing constructors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeSampleFrameTargets {
    pub initial_frames: usize,
    pub maximum_frames: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    SessionTimeout,
    RequestTimeout,
    SignatureGenerationFailure,
    TransportFailure,
    InvalidResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Capturing,
    InFlight {
        attempt: u64,
    },
    WaitingForRetry {
        not_before_ms: u64,
        target_sample_ms: u64,
    },
    /// Reserved for compatibility with an earlier post-stop Retry model.
    /// The currently modeled accepted Manual Retry keeps the context active
    /// and schedules the retained Signature through the ordinary poll path.
    SubmittingPostStopRetry {
        attempt: u64,
    },
    /// Native stopped the current SignatureGenerator after an insufficient
    /// signature attempt or after reaching its configured maximum without a
    /// further eligible Retry. Auto Type 2 can remain alive, and an already-
    /// scheduled stop/start may create a fresh generator later.
    GeneratorStopped,
    Succeeded,
    Exhausted,
    Failed(StopReason),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecognitionJob {
    pub attempt: u64,
    /// Cumulative PCM duration since this SignatureGenerator cycle began.
    /// Native uses `writtenFrames / sampleRate` for the Signature length sent
    /// as HTTP `signature.samplems`; this includes audio after earlier freezes.
    pub sample_ms: u64,
    /// Relative monotonic time supplied by the caller when the Signature was built.
    pub generated_at_ms: u64,
    /// Native request timestamp source: Signature.date when the generator's
    /// audio-start time is known, otherwise the tagging session start date.
    /// This is a Unix-epoch millisecond value and is separate from
    /// `generated_at_ms`, which belongs to the controller's monotonic clock.
    pub signature_timestamp_ms: Option<u64>,
    /// A validated Signature. Callers should not log or persist it by default.
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushReport {
    pub accepted_frames: usize,
    pub total_frames: usize,
    pub maximum_reached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    InvalidTiming(&'static str),
    SigX(String),
    InvalidSignature(String),
    Closed,
    NoRequestInFlight,
    StaleAttempt { expected: u64, received: u64 },
    ClockMovedBackwards { previous_ms: u64, current_ms: u64 },
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTiming(message) => f.write_str(message),
            Self::SigX(message) => write!(f, "SigX error: {message}"),
            Self::InvalidSignature(message) => {
                write!(f, "generated Signature is invalid: {message}")
            }
            Self::Closed => f.write_str("recognition session is no longer accepting PCM"),
            Self::NoRequestInFlight => f.write_str("no recognition request is in flight"),
            Self::StaleAttempt { expected, received } => {
                write!(
                    f,
                    "stale recognition response: expected attempt {expected}, got {received}"
                )
            }
            Self::ClockMovedBackwards {
                previous_ms,
                current_ms,
            } => write!(
                f,
                "relative monotonic clock moved backwards ({current_ms} after {previous_ms})"
            ),
        }
    }
}

impl std::error::Error for EngineError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecognitionOutcome {
    Match {
        retry: Option<RetryDirective>,
    },
    NoMatch {
        retry: Option<RetryDirective>,
    },
    /// The matcher reported an HTTP/application failure with an optional
    /// native Retry request. This is distinct from a transport error, where
    /// no matcher result (and therefore no Retry request) is available.
    MatcherFailure {
        retry: Option<RetryDirective>,
    },
    InvalidResponse,
    TransportFailure,
}

/// Stateful, platform-independent recognition control for one audio session.
pub struct ProgressiveSession {
    timing: RecognitionTiming,
    tagging_type: NativeTaggingType,
    sigx: SigX,
    signature_timeline: NativeSignatureTimeline,
    sigx_capacity_frames: usize,
    pre_record_history: PcmHistory,
    configured_maximum_frames: usize,
    maximum_frames: usize,
    minimum_frames: usize,
    initial_frames_equal_maximum: bool,
    native_total_frames_to_record: usize,
    retry_frames_active: bool,
    generator_running: bool,
    next_signature_target_frames: Option<usize>,
    pending_jobs: VecDeque<RecognitionJob>,
    in_flight_attempts: BTreeSet<u64>,
    // SHKTaggingOptions creates one UUID contextID for a tagging context.
    // SHKNetworkMatcher inserts that ID when each request starts and removes
    // it when a response callback begins. This is deliberately a membership
    // bit, not an in-flight reference count: a later request can re-register
    // the same context before an older response arrives.
    matcher_context_registered: bool,
    frames_seen: usize,
    attempt_count: u64,
    pending_restart_times_ms: Vec<u64>,
    pending_restart_times_ns: Vec<u64>,
    /// Last server Retry interval retained by Native Type 2. A cycle timer is
    /// created when a Signature is filtered, using the value that exists then;
    /// a later response updates only the following cycle's interval.
    next_type2_cycle_interval_ns: Option<u64>,
    next_signature_not_before_ns: Option<u64>,
    last_now_ns: u64,
    session_start_timestamp_ms: Option<u64>,
    generator_start_timestamp_ms: Option<u64>,
    generator_timestamp_initialized: bool,
    input_finished: bool,
    match_seen: bool,
    post_stop_retry_job: Option<RecognitionJob>,
    lifecycle_status: SessionStatus,
}

/// Minimal `SHKSignatureTimeline` state for the observed
/// `SHKBasicSignatureFilter` rule: select the last rated Signature of type 0
/// in `currentAttempt`; on a new current period, archive that attempt and
/// replace it. ARM64 static analysis shows that the filter factory chooses
/// `SHKBasicSignatureFilter` when `signatureFilterType` is zero, and the
/// inspected `SHKTaggingOptions` initializer does not write that field
/// (ivar +0x38). Whether every app-created manual context keeps that default
/// at runtime has not been instrumented.
#[derive(Default)]
struct NativeSignatureTimeline {
    current_attempt: Vec<NativeRatedSignature>,
    all_attempts: Vec<Vec<NativeRatedSignature>>,
}

struct NativeRatedSignature {
    signature_type: u8,
    bytes: Vec<u8>,
}

impl NativeSignatureTimeline {
    fn store_signature(&mut self, signature_type: u8, bytes: Vec<u8>) {
        self.current_attempt.push(NativeRatedSignature {
            signature_type,
            bytes,
        });
    }

    /// Mirror `createNewCurrentPeriodForRetry:`: archive the current attempt
    /// and replace it with a fresh one before another Signature is generated.
    fn create_new_current_period_for_retry(&mut self) {
        self.all_attempts
            .push(std::mem::take(&mut self.current_attempt));
    }

    fn pick_last_of_type(&self, signature_type: u8) -> Option<&[u8]> {
        self.current_attempt
            .iter()
            .rev()
            .find(|rated| rated.signature_type == signature_type)
            .map(|rated| rated.bytes.as_slice())
    }
}

impl ProgressiveSession {
    /// Backward-compatible constructor with the former Type 2 controller
    /// semantics. Runtime manual-recognition paths should use
    /// [`ProgressiveSession::new_for_type`].
    pub fn new(timing: RecognitionTiming) -> Result<Self, EngineError> {
        Self::new_for_type(timing, NativeTaggingType::Auto)
    }

    pub fn new_for_type(
        timing: RecognitionTiming,
        tagging_type: NativeTaggingType,
    ) -> Result<Self, EngineError> {
        Self::new_for_type_with_pre_record_capacity_frames(timing, tagging_type, 0)
    }

    /// Construct a session that retains the caller-specified number of recent
    /// PCM frames and replays them into each new recording cycle. Supply the
    /// capacity from the active recorder configuration; this crate does not
    /// guess Shazam's effective ring-buffer length.
    pub fn new_for_type_with_pre_record_capacity_frames(
        timing: RecognitionTiming,
        tagging_type: NativeTaggingType,
        pre_record_capacity_frames: usize,
    ) -> Result<Self, EngineError> {
        Self::new_for_type_with_native_frame_targets(
            timing,
            tagging_type,
            pre_record_capacity_frames,
            None,
        )
    }

    /// Construct a session using frame targets computed from the original
    /// Native floating-point seconds values. This avoids a lossy
    /// seconds->milliseconds->frames round trip for Config durations with
    /// sub-millisecond precision.
    pub fn new_for_type_with_native_frame_targets(
        timing: RecognitionTiming,
        tagging_type: NativeTaggingType,
        pre_record_capacity_frames: usize,
        native_frame_targets: Option<NativeSampleFrameTargets>,
    ) -> Result<Self, EngineError> {
        validate_timing(timing)?;
        let maximum_frames = match native_frame_targets {
            Some(targets) => targets.maximum_frames,
            None => native_frames_for_ms(timing.sample_rate, timing.maximum_sample_ms)?,
        };
        let minimum_frames = frames_for_ms(timing.sample_rate, timing.minimum_signature_ms)?;
        let initial_frames = match native_frame_targets {
            Some(targets) => targets.initial_frames,
            None => native_frames_for_ms(timing.sample_rate, timing.initial_sample_ms)?,
        };
        if maximum_frames == 0 || initial_frames == 0 || initial_frames > maximum_frames {
            return Err(EngineError::InvalidTiming(
                "Native frame targets must be nonzero and the initial target must not exceed the maximum",
            ));
        }
        if minimum_frames > maximum_frames {
            return Err(EngineError::InvalidTiming(
                "minimum Signature frame count exceeds the Native maximum target",
            ));
        }
        let sigx = create_sigx(timing, maximum_frames)?;
        let pre_record_capacity_frames = if tagging_type.uses_pre_recorded_audio() {
            pre_record_capacity_frames.min(maximum_frames)
        } else {
            0
        };

        Ok(Self {
            timing,
            tagging_type,
            sigx,
            signature_timeline: NativeSignatureTimeline::default(),
            sigx_capacity_frames: maximum_frames,
            pre_record_history: PcmHistory::new(pre_record_capacity_frames),
            configured_maximum_frames: maximum_frames,
            maximum_frames,
            minimum_frames,
            initial_frames_equal_maximum: initial_frames == maximum_frames,
            native_total_frames_to_record: initial_frames,
            retry_frames_active: false,
            generator_running: true,
            next_signature_target_frames: Some(initial_frames),
            pending_jobs: VecDeque::new(),
            in_flight_attempts: BTreeSet::new(),
            matcher_context_registered: false,
            frames_seen: 0,
            attempt_count: 0,
            pending_restart_times_ms: Vec::new(),
            pending_restart_times_ns: Vec::new(),
            next_type2_cycle_interval_ns: None,
            next_signature_not_before_ns: None,
            last_now_ns: 0,
            session_start_timestamp_ms: None,
            generator_start_timestamp_ms: None,
            generator_timestamp_initialized: false,
            input_finished: false,
            match_seen: false,
            post_stop_retry_job: None,
            lifecycle_status: SessionStatus::Capturing,
        })
    }

    pub fn status(&self) -> SessionStatus {
        self.in_flight_attempts
            .last()
            .copied()
            .map_or(self.lifecycle_status, |attempt| SessionStatus::InFlight {
                attempt,
            })
    }

    /// Tagging policy selected for this session. Adapters can use this to
    /// expose Native callback semantics without inferring them from timing.
    pub const fn tagging_type(&self) -> NativeTaggingType {
        self.tagging_type
    }

    /// Whether the tagging/capture lifecycle has reached a terminal state.
    /// This is distinct from [`ProgressiveSession::status`]: that method reports
    /// the latest outstanding attempt while earlier HTTP tasks are still being
    /// retired after capture has stopped.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.lifecycle_status,
            SessionStatus::Succeeded
                | SessionStatus::Exhausted
                | SessionStatus::Failed(_)
                | SessionStatus::Cancelled
        )
    }

    /// Attempts whose recognition responses have not yet been applied.
    pub fn in_flight_attempts(&self) -> Vec<u64> {
        self.in_flight_attempts.iter().copied().collect()
    }

    /// Whether Native's shared matcher context currently permits a response
    /// callback for this request. Multiple attempts can share the same
    /// contextID, so membership can be cleared by an earlier completion and
    /// re-established by a later request.
    pub fn response_is_deliverable(&self, attempt: u64) -> bool {
        self.matcher_context_registered && self.in_flight_attempts.contains(&attempt)
    }

    /// Legacy compatibility hook for detached post-stop Retry jobs. The
    /// currently modeled accepted Manual Retry keeps its context active and
    /// does not populate this slot.
    pub fn take_post_stop_retry_job(&mut self) -> Option<RecognitionJob> {
        self.post_stop_retry_job.take()
    }

    /// Legacy finalizer for the reserved detached post-stop Retry state.
    pub fn finish_post_stop_retry_submission(&mut self) {
        if matches!(
            self.lifecycle_status,
            SessionStatus::SubmittingPostStopRetry { .. }
        ) && self.post_stop_retry_job.is_none()
        {
            self.stop_with_status(SessionStatus::Exhausted);
        }
    }

    pub fn frames_seen(&self) -> usize {
        self.frames_seen
    }

    pub fn sample_ms(&self) -> u64 {
        duration_ms(self.timing.sample_rate, self.frames_seen)
    }

    /// Set Native's session-start-date fallback for request timestamps.
    /// Call before feeding or seeding PCM. Per-generator source timestamps,
    /// when supplied to a timed PCM method, take precedence for that cycle.
    pub fn set_session_start_timestamp_ms(&mut self, timestamp_ms: u64) -> Result<(), EngineError> {
        if self.frames_seen != 0 || self.attempt_count != 0 || self.last_now_ns != 0 {
            return Err(EngineError::InvalidTiming(
                "session start timestamp must be set before PCM input",
            ));
        }
        self.session_start_timestamp_ms = Some(timestamp_ms);
        Ok(())
    }

    /// Frames currently retained for Native-style pre-record replay.
    pub fn pre_recorded_frames(&self) -> usize {
        self.pre_record_history.frames()
    }

    /// Seed the first manual TaggingType 1 cycle with the capture adapter's
    /// recent ring-buffer contents. This must be called before ordinary PCM
    /// input; only the newest configured-capacity frames are accepted.
    pub fn prime_pre_recorded_i16(&mut self, samples: &[i16]) -> Result<usize, EngineError> {
        self.ensure_pre_record_seedable()?;
        let start = samples
            .len()
            .saturating_sub(self.pre_record_history.capacity_frames());
        let recent = &samples[start..];
        if !recent.is_empty() {
            self.generator_timestamp_initialized = true;
            self.generator_start_timestamp_ms = self.session_start_timestamp_ms;
            self.sigx
                .push_i16(recent)
                .map_err(|error| EngineError::SigX(format!("{error:?}")))?;
            self.pre_record_history.append_i16(recent);
            self.frames_seen = recent.len();
            self.handle_native_buffer_boundary(self.last_now_ns)?;
        }
        Ok(recent.len())
    }

    /// Float32 counterpart to [`ProgressiveSession::prime_pre_recorded_i16`].
    pub fn prime_pre_recorded_f32(&mut self, samples: &[f32]) -> Result<usize, EngineError> {
        self.ensure_pre_record_seedable()?;
        let start = samples
            .len()
            .saturating_sub(self.pre_record_history.capacity_frames());
        let recent = &samples[start..];
        if !recent.is_empty() {
            self.generator_timestamp_initialized = true;
            self.generator_start_timestamp_ms = self.session_start_timestamp_ms;
            self.sigx
                .push_f32(recent)
                .map_err(|error| EngineError::SigX(format!("{error:?}")))?;
            self.pre_record_history.append_f32(recent);
            self.frames_seen = recent.len();
            self.handle_native_buffer_boundary(self.last_now_ns)?;
        }
        Ok(recent.len())
    }

    /// Mark PCM end-of-stream. A pending request may still complete; a later
    /// retry that requires audio beyond the received prefix will be exhausted.
    pub fn finish_input(&mut self) {
        self.input_finished = true;
        if !self.in_flight_attempts.is_empty() || !self.pending_jobs.is_empty() {
            return;
        }
        match self.lifecycle_status {
            SessionStatus::GeneratorStopped => self.finish_status(),
            SessionStatus::Capturing | SessionStatus::WaitingForRetry { .. }
                if self
                    .next_signature_target_frames
                    .is_some_and(|target| self.frames_seen < target) =>
            {
                self.finish_status();
            }
            SessionStatus::WaitingForRetry { .. }
                if self.next_signature_target_frames.is_none() =>
            {
                self.finish_status();
            }
            SessionStatus::Capturing if self.next_signature_target_frames.is_none() => {
                self.finish_status();
            }
            _ => {}
        }
    }

    /// Feed mono Int16 PCM. Input remains accepted while HTTP work is in flight.
    pub fn push_i16(&mut self, samples: &[i16]) -> Result<PushReport, EngineError> {
        self.push_i16_at_ns(samples, self.last_now_ns)
    }

    /// Feed PCM at a caller-supplied monotonic time. A due stop/start cycle is
    /// applied before this event's packet. When a response and PCM event share
    /// a timestamp, callers must preserve their observed event order; Native's
    /// generator and main queues do not define a statically provable winner.
    pub fn push_i16_at(&mut self, samples: &[i16], now_ms: u64) -> Result<PushReport, EngineError> {
        self.push_i16_at_ns(samples, milliseconds_to_nanoseconds(now_ms)?)
    }

    /// Nanosecond-resolution counterpart for capture adapters that preserve
    /// Native `dispatch_after` timing.
    pub fn push_i16_at_ns(
        &mut self,
        samples: &[i16],
        now_ns: u64,
    ) -> Result<PushReport, EngineError> {
        self.push_i16_at_with_source_timestamp_ns(samples, now_ns, None)
    }

    /// Feed Int16 PCM with an optional source wall-clock timestamp for the
    /// first buffer in the active SignatureGenerator cycle. `now_ms` remains
    /// the independent monotonic controller clock.
    pub fn push_i16_at_with_source_timestamp(
        &mut self,
        samples: &[i16],
        now_ms: u64,
        source_timestamp_ms: Option<u64>,
    ) -> Result<PushReport, EngineError> {
        self.push_i16_at_with_source_timestamp_ns(
            samples,
            milliseconds_to_nanoseconds(now_ms)?,
            source_timestamp_ms,
        )
    }

    /// Nanosecond-resolution Int16 input with an optional wall-clock source
    /// timestamp for the first audio in this generator cycle.
    pub fn push_i16_at_with_source_timestamp_ns(
        &mut self,
        samples: &[i16],
        now_ns: u64,
        source_timestamp_ms: Option<u64>,
    ) -> Result<PushReport, EngineError> {
        self.advance_time_ns(now_ns)?;
        self.ensure_accepting()?;
        let accepted = self.accepted_frames_for_buffer(samples.len());
        let new_total = self
            .frames_seen
            .checked_add(accepted)
            .ok_or(EngineError::InvalidTiming("PCM frame count overflow"))?;
        if accepted > 0 && self.generator_running {
            if !self.generator_timestamp_initialized {
                self.generator_timestamp_initialized = true;
                self.generator_start_timestamp_ms =
                    source_timestamp_ms.or(self.session_start_timestamp_ms);
            }
            self.extend_sigx_capacity(new_total)?;
            self.sigx
                .push_i16(&samples[..accepted])
                .map_err(|error| EngineError::SigX(format!("{error:?}")))?;
        }
        self.pre_record_history.append_i16(&samples[..accepted]);
        self.frames_seen = new_total;
        if accepted > 0 {
            self.handle_native_buffer_boundary(now_ns)?;
        }
        Ok(self.push_report(accepted))
    }

    /// Feed mono normalized Float32 PCM. Input remains accepted while HTTP work
    /// is in flight. The SigX core owns its existing Float32 preprocessing.
    pub fn push_f32(&mut self, samples: &[f32]) -> Result<PushReport, EngineError> {
        self.push_f32_at_ns(samples, self.last_now_ns)
    }

    /// Time-aware Float32 variant for live capture adapters.
    pub fn push_f32_at(&mut self, samples: &[f32], now_ms: u64) -> Result<PushReport, EngineError> {
        self.push_f32_at_ns(samples, milliseconds_to_nanoseconds(now_ms)?)
    }

    /// Nanosecond-resolution counterpart for live capture adapters.
    pub fn push_f32_at_ns(
        &mut self,
        samples: &[f32],
        now_ns: u64,
    ) -> Result<PushReport, EngineError> {
        self.push_f32_at_with_source_timestamp_ns(samples, now_ns, None)
    }

    /// Feed Float32 PCM with an optional source wall-clock timestamp for the
    /// first buffer in the active SignatureGenerator cycle.
    pub fn push_f32_at_with_source_timestamp(
        &mut self,
        samples: &[f32],
        now_ms: u64,
        source_timestamp_ms: Option<u64>,
    ) -> Result<PushReport, EngineError> {
        self.push_f32_at_with_source_timestamp_ns(
            samples,
            milliseconds_to_nanoseconds(now_ms)?,
            source_timestamp_ms,
        )
    }

    /// Nanosecond-resolution Float32 input with an optional wall-clock source
    /// timestamp for the first audio in this generator cycle.
    pub fn push_f32_at_with_source_timestamp_ns(
        &mut self,
        samples: &[f32],
        now_ns: u64,
        source_timestamp_ms: Option<u64>,
    ) -> Result<PushReport, EngineError> {
        self.advance_time_ns(now_ns)?;
        self.ensure_accepting()?;
        let accepted = self.accepted_frames_for_buffer(samples.len());
        let new_total = self
            .frames_seen
            .checked_add(accepted)
            .ok_or(EngineError::InvalidTiming("PCM frame count overflow"))?;
        if accepted > 0 && self.generator_running {
            if !self.generator_timestamp_initialized {
                self.generator_timestamp_initialized = true;
                self.generator_start_timestamp_ms =
                    source_timestamp_ms.or(self.session_start_timestamp_ms);
            }
            self.extend_sigx_capacity(new_total)?;
            self.sigx
                .push_f32(&samples[..accepted])
                .map_err(|error| EngineError::SigX(format!("{error:?}")))?;
        }
        self.pre_record_history.append_f32(&samples[..accepted]);
        self.frames_seen = new_total;
        if accepted > 0 {
            self.handle_native_buffer_boundary(now_ns)?;
        }
        Ok(self.push_report(accepted))
    }

    /// Generate a Signature when the active Native PCM threshold is reached.
    /// `now_ms` is a caller-provided monotonic time relative to session start;
    /// it is intentionally distinct from the cumulative sample duration.
    pub fn poll(&mut self, now_ms: u64) -> Result<Option<RecognitionJob>, EngineError> {
        self.poll_at_ns(milliseconds_to_nanoseconds(now_ms)?)
    }

    /// Generate a Signature using a nanosecond monotonic session clock.
    pub fn poll_at_ns(&mut self, now_ns: u64) -> Result<Option<RecognitionJob>, EngineError> {
        self.advance_time_ns(now_ns)?;

        if self.lifecycle_status == SessionStatus::Failed(StopReason::SessionTimeout) {
            self.pending_jobs.clear();
            return Ok(None);
        }
        if let Some(job) = self.pending_jobs.pop_front() {
            self.in_flight_attempts.insert(job.attempt);
            self.matcher_context_registered = true;
            return Ok(Some(job));
        }

        let job = match self.lifecycle_status {
            SessionStatus::WaitingForRetry { .. } => {
                if self
                    .next_signature_not_before_ns
                    .is_some_and(|not_before_ns| now_ns < not_before_ns)
                {
                    None
                } else if let Some(target_frames) = self.next_signature_target_frames {
                    if self.frames_seen < target_frames {
                        None
                    } else {
                        self.create_job_if_minimum_is_met(now_ns)?
                    }
                } else {
                    None
                }
            }
            SessionStatus::Capturing => {
                if let Some(target) = self.next_signature_target_frames {
                    if self.frames_seen >= target {
                        self.create_job_if_minimum_is_met(now_ns)?
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            SessionStatus::GeneratorStopped | SessionStatus::SubmittingPostStopRetry { .. } => None,
            SessionStatus::Succeeded
            | SessionStatus::Exhausted
            | SessionStatus::Failed(_)
            | SessionStatus::Cancelled => None,
            SessionStatus::InFlight { .. } => None,
        };
        if let Some(job) = &job {
            self.in_flight_attempts.insert(job.attempt);
            self.matcher_context_registered = true;
        }
        Ok(job)
    }

    /// Apply the current response according to the selected Native tagging
    /// type; local maximum-attempt limits remain an explicit safety bound.
    pub fn complete(
        &mut self,
        attempt: u64,
        outcome: RecognitionOutcome,
        now_ms: u64,
    ) -> Result<SessionStatus, EngineError> {
        self.complete_with_delivery(attempt, outcome, now_ms)
            .map(|(status, _)| status)
    }

    /// Apply a response and report whether Native's shared contextID gate
    /// delivered it to the bridge. This lets adapters avoid surfacing matches
    /// from a transport response that Native would suppress.
    pub fn complete_with_delivery(
        &mut self,
        attempt: u64,
        outcome: RecognitionOutcome,
        now_ms: u64,
    ) -> Result<(SessionStatus, bool), EngineError> {
        self.complete_with_delivery_at_ns(attempt, outcome, milliseconds_to_nanoseconds(now_ms)?)
    }

    /// Apply a response using nanosecond monotonic time, preserving fractional
    /// `intervalDuration` deadlines from Native's dispatch queue.
    pub fn complete_with_delivery_at_ns(
        &mut self,
        attempt: u64,
        outcome: RecognitionOutcome,
        now_ns: u64,
    ) -> Result<(SessionStatus, bool), EngineError> {
        self.advance_time_ns(now_ns)?;
        if self.lifecycle_status == SessionStatus::Failed(StopReason::SessionTimeout) {
            return Ok((self.status(), false));
        }
        if !self.in_flight_attempts.contains(&attempt) {
            let expected = self.in_flight_attempts.last().copied();
            if let Some(expected) = expected {
                return Err(EngineError::StaleAttempt {
                    expected,
                    received: attempt,
                });
            }
            return Err(EngineError::NoRequestInFlight);
        }
        self.in_flight_attempts.remove(&attempt);

        // Native's URL completion first checks contextID membership. If an
        // earlier request already removed the shared ID, this response is
        // retired without reaching SHKAudioMatcherBridge. A new request may
        // have re-added that same ID in the meantime; in that case this older
        // response passes the gate just as the Native set-membership check
        if !self.matcher_context_registered {
            return Ok((self.status(), false));
        }
        self.matcher_context_registered = false;

        // A terminal response stops further recognition control, but other
        // requests already handed to Native's URLSession executor may still
        // finish and be retired by their own attempt IDs.
        if matches!(
            self.lifecycle_status,
            SessionStatus::Succeeded
                | SessionStatus::Exhausted
                | SessionStatus::Failed(_)
                | SessionStatus::Cancelled
        ) {
            return Ok((self.status(), false));
        }

        match outcome {
            RecognitionOutcome::Match { retry } => {
                self.match_seen = true;
                if self.tagging_type == NativeTaggingType::Manual {
                    // Native matchStrategyForTaggingOptions maps ordinary
                    // Type 1 to strategy 3. For a tag-bearing response with a
                    // positive Retry, SHKNetworkMatcher routes strategy 3
                    // through willFetchCompleteResult; the bridge applies
                    // updateRetry before notifying the app, so Type 1 extends
                    // this generator. Without positive Retry, normal matched
                    // handling stops Type 1 (keepTaggingAfterAMatch=false).
                    if let Some(retry) = retry.filter(|retry| retry.has_positive_retry_duration()) {
                        self.apply_generator_retry(retry, now_ns)?;
                    } else {
                        self.stop_with_status(SessionStatus::Succeeded);
                    }
                } else {
                    self.apply_type2_response_retry(retry)?;
                }
            }
            RecognitionOutcome::MatcherFailure { retry: Some(retry) } => {
                // SHKAudioMatcherBridge accepts a matcher Retry through
                // continueWithRetryIfDelegateAllowsRequest, then calls
                // SHKAudioSubsystem.updateRetry. SHKAudioClosure.updateRetry
                // only forwards that Retry to the SignaturePipe when
                // keepTaggingAfterAMatch is false. Native TaggingType 2
                // returns true, so this matcher-error path leaves its current
                // generator unchanged; it does not use retryDuration or
                // intervalDuration here. Manual Type 1 returns false and
                // applies retryDuration to the current/stopped generator.
                // The app's SHALAppTagManager constructs the linked
                // SHALTagMultiplexer and assigns itself as delegate. In the
                // shipped binary its method list lacks the optional
                // shouldRetryForRequest selector, so the multiplexer defaults
                // to allow. Runtime swizzling is unobserved; callers may
                // represent a denial as no Retry.
                if self.tagging_type == NativeTaggingType::Manual {
                    self.apply_generator_retry(retry, now_ns)?;
                } else {
                    // A finite-source adapter still needs to close after its
                    // input has ended; for live Auto capture this preserves
                    // the active generator until its ordinary buffer/max
                    // boundary, without synthesizing another request.
                    self.resume_type2_after_nonterminal_failure()?;
                }
            }
            RecognitionOutcome::MatcherFailure { retry: None }
            | RecognitionOutcome::InvalidResponse => {
                // Native network and response-parse failures reach
                // matcher:failedWithError:matcherResult:. Manual Type 1 cancels on
                // error; Type 2 does not. A completed finite source can still
                // finish normally once its request returns.
                if self.tagging_type == NativeTaggingType::Manual {
                    self.stop_with_status(SessionStatus::Failed(StopReason::InvalidResponse));
                } else {
                    self.resume_type2_after_nonterminal_failure()?;
                }
            }
            RecognitionOutcome::TransportFailure => {
                if self.tagging_type == NativeTaggingType::Manual {
                    self.stop_with_status(SessionStatus::Failed(StopReason::TransportFailure));
                } else {
                    self.resume_type2_after_nonterminal_failure()?;
                }
            }
            RecognitionOutcome::NoMatch { retry } => {
                if self.tagging_type == NativeTaggingType::Manual {
                    // An empty-tag 2xx response with a Retry enters
                    // SHKAudioMatcherBridge's matched callback. If the
                    // delegate allows the Retry, the bridge calls
                    // audioSubsystem.updateRetry and returns early before its
                    // noMatchFound/stopTagging path, even for Manual Type 1.
                    // `Some` therefore means an already-authorized Retry and
                    // keeps this context active; `None` represents no Retry
                    // or a denial supplied by a different/runtime-modified
                    // delegate.
                    if let Some(retry) = retry {
                        self.apply_generator_retry(retry, now_ns)?;
                    } else {
                        self.stop_with_status(SessionStatus::Exhausted);
                    }
                } else {
                    self.apply_type2_response_retry(retry)?;
                }
            }
        }
        Ok((self.status(), true))
    }

    /// Stop accepting audio and invalidate all attempts. Returns the most
    /// recent attempt for compatibility; callers managing concurrent workers
    /// should use [`ProgressiveSession::cancel_all`] to obtain every ID.
    pub fn cancel(&mut self) -> Option<u64> {
        self.cancel_all().into_iter().last()
    }

    /// Stop accepting audio and invalidate every request that is still
    /// awaiting a response. Returned IDs are sorted in attempt order so an
    /// adapter can retire or drain the corresponding workers. This does not
    /// imply transport cancellation: the inspected Native matcher path removes
    /// context IDs and gates callback delivery separately from task lifetime.
    pub fn cancel_all(&mut self) -> Vec<u64> {
        self.pending_jobs.clear();
        let active = self.in_flight_attempts();
        if !matches!(
            self.lifecycle_status,
            SessionStatus::Succeeded | SessionStatus::Exhausted | SessionStatus::Failed(_)
        ) {
            self.lifecycle_status = SessionStatus::Cancelled;
        }
        self.in_flight_attempts.clear();
        self.matcher_context_registered = false;
        active
    }

    fn create_job(&mut self, now_ns: u64) -> Result<RecognitionJob, EngineError> {
        if self
            .timing
            .maximum_attempts
            .is_some_and(|maximum| self.attempt_count >= maximum)
        {
            self.lifecycle_status = SessionStatus::Exhausted;
            return Err(EngineError::Closed);
        }
        let sample_length_samples =
            native_signature_sample_length_samples(self.timing.sample_rate, self.frames_seen)?;
        // Native's Signature header length is cycle-cumulative (not the delta
        // since the previous progressive snapshot). `sample_ms` uses the same
        // cumulative PCM count but intentionally does not include the header
        // adjustment below.
        let sample_ms = duration_ms(self.timing.sample_rate, self.frames_seen);
        // Native sig_stream::now() returns the frame cursor minus the 45-frame
        // peak warm-up; freeze_signature clamps negative positions to zero.
        let stream_pos_ticks =
            native_sig_stream_now_ticks(self.sigx.snapshot().fft_frames_processed);
        let generated_signature = self
            .sigx
            .take_type2_progressive_signature(stream_pos_ticks, sample_length_samples)
            .map_err(|error| EngineError::SigX(format!("{error:?}")))?;
        // Static call path: `generateSignature` requests outer type 0;
        // `processSignature:` stores it, sends the filter notification, then
        // continues to the configured filter. The Basic filter selects the
        // last current-attempt type-0 Signature, which the matcher bridge
        // reads from `pickedRatedSignature`. The ARM64 factory selects Basic
        // for signatureFilterType == 0; the inspected options initializer
        // leaves that ivar (+0x38) unset. Runtime mutation by the app remains
        // unobserved.
        self.signature_timeline
            .store_signature(0, generated_signature);
        let signature = self
            .signature_timeline
            .pick_last_of_type(0)
            .ok_or(EngineError::InvalidTiming(
                "Native Basic signature filter found no type-0 Signature",
            ))?
            .to_vec();
        Signature::parse(&signature)
            .map_err(|error| EngineError::InvalidSignature(error.to_string()))?;
        self.attempt_count = self
            .attempt_count
            .checked_add(1)
            .ok_or(EngineError::InvalidTiming("attempt counter overflow"))?;
        // Native clears retryFrames in generateSignature. After producing an
        // intermediate Signature, it keeps flowing to the Config maximum but
        // does not emit another one there unless a Retry interval is active.
        self.next_signature_target_frames = None;
        self.retry_frames_active = false;
        if self.tagging_type == NativeTaggingType::Auto {
            self.schedule_type2_cycle_after_signature(now_ns)?;
        }
        Ok(RecognitionJob {
            attempt: self.attempt_count,
            sample_ms,
            generated_at_ms: nanoseconds_to_milliseconds_floor(now_ns),
            signature_timestamp_ms: self
                .generator_start_timestamp_ms
                .or(self.session_start_timestamp_ms),
            signature,
        })
    }

    fn create_job_if_minimum_is_met(
        &mut self,
        now_ns: u64,
    ) -> Result<Option<RecognitionJob>, EngineError> {
        if self.frames_seen < self.minimum_frames {
            // SHKAudioSigPipe reports a generation error and stops its
            // generator instead of extending the current Config/Retry target
            // to the minimum. The matcher bridge reports that failure through
            // failedMatching; Manual Type 1 cancels tagging on that error,
            // while Type 2 may keep its enclosing context alive.
            self.generator_running = false;
            self.next_signature_target_frames = None;
            self.lifecycle_status = if self.tagging_type == NativeTaggingType::Manual {
                SessionStatus::Failed(StopReason::SignatureGenerationFailure)
            } else {
                SessionStatus::GeneratorStopped
            };
            return Ok(None);
        }
        self.create_job(now_ns).map(Some)
    }

    fn apply_generator_retry(
        &mut self,
        retry: RetryDirective,
        now_ns: u64,
    ) -> Result<(), EngineError> {
        if self
            .timing
            .maximum_attempts
            .is_some_and(|maximum| self.attempt_count >= maximum)
        {
            self.finish_status();
            return Ok(());
        }

        // SHKAudioClosure.updateRetry: creates a new SignatureTimeline
        // current period before forwarding Retry to the active SignaturePipe.
        // The Pipe passes retryDuration (not intervalDuration) to the
        // generator; while running, its queue updates totalFramesToRecord.
        self.signature_timeline
            .create_new_current_period_for_retry();

        let retry_duration_ms = retry.retry_duration_ms.unwrap_or(0);
        // Native updateRetry calls SigPipe.updateWithRetry and adjusts the
        // current SignatureGenerator threshold while it is running. If the
        // generator has stopped, Native compares secondsAvailable against the
        // requested duration and either produces a Signature immediately from
        // the retained generator state or keeps it stopped. The Native
        // maximumSignatureLength check is an assertion conditional on a
        // runtime exception flag; the ordinary non-asserting path does not
        // clamp the requested threshold. The flag's live value is unverified.
        let requested_frames = match retry.retry_duration_seconds_bits {
            Some(seconds_bits) => {
                native_frames_from_seconds_bits(self.timing.sample_rate, seconds_bits)?
            }
            None => native_frames_for_ms(self.timing.sample_rate, retry_duration_ms)?,
        };

        self.retry_frames_active = requested_frames > 0;
        if self.generator_running && requested_frames > self.maximum_frames {
            self.sigx
                .extend_max_frames(requested_frames)
                .map_err(|error| EngineError::SigX(format!("{error:?}")))?;
            self.sigx_capacity_frames = requested_frames;
            self.maximum_frames = requested_frames;
        }

        if !self.generator_running {
            if self.frames_seen >= requested_frames {
                self.next_signature_target_frames = Some(self.frames_seen);
                self.next_signature_not_before_ns = Some(now_ns);
                self.lifecycle_status = SessionStatus::WaitingForRetry {
                    not_before_ms: nanoseconds_to_milliseconds_ceil(now_ns),
                    target_sample_ms: duration_ms(self.timing.sample_rate, self.frames_seen),
                };
            } else {
                self.next_signature_target_frames = None;
                self.next_signature_not_before_ns = None;
                if self.input_finished {
                    // A finite source cannot satisfy a Retry that asks for
                    // more PCM than the retained stopped generator contains.
                    // Retire the session now instead of leaving it in a
                    // nonterminal GeneratorStopped state after its last HTTP
                    // response. Live capture remains eligible for later PCM.
                    self.finish_status();
                } else {
                    self.lifecycle_status = SessionStatus::GeneratorStopped;
                }
            }
            return Ok(());
        }

        self.next_signature_target_frames = if self.frames_seen < self.maximum_frames {
            Some(requested_frames.min(self.maximum_frames))
        } else if self.frames_seen >= requested_frames {
            Some(self.frames_seen)
        } else {
            None
        };
        if let Some(target) = self.next_signature_target_frames {
            self.native_total_frames_to_record = target;
        }

        // The retry setter is dispatched to the generator queue. If the new
        // target is already behind writtenFrames, the next flowSamples call
        // evaluates it; it does not synchronously emit from the old prefix.
        if self.generator_running
            && !self.input_finished
            && self.frames_seen < self.maximum_frames
            && self
                .next_signature_target_frames
                .is_some_and(|target| target <= self.frames_seen)
        {
            let next_frame = self
                .frames_seen
                .checked_add(1)
                .ok_or(EngineError::InvalidTiming("PCM frame count overflow"))?;
            self.next_signature_target_frames = Some(next_frame);
            self.native_total_frames_to_record = next_frame;
        }

        if self.input_finished
            && self
                .next_signature_target_frames
                .is_none_or(|target| target > self.frames_seen)
        {
            self.finish_status();
            return Ok(());
        }

        match self.next_signature_target_frames {
            Some(target) => {
                self.next_signature_not_before_ns = Some(now_ns);
                self.lifecycle_status = SessionStatus::WaitingForRetry {
                    not_before_ms: nanoseconds_to_milliseconds_ceil(now_ns),
                    target_sample_ms: duration_ms(self.timing.sample_rate, target),
                };
            }
            None => {
                self.next_signature_not_before_ns = None;
                self.lifecycle_status = SessionStatus::Capturing;
            }
        }
        Ok(())
    }

    fn accepted_frames_for_buffer(&self, input_frames: usize) -> usize {
        // Native flowAudioData returns before enqueueing to generatorQueue
        // when `generating` is false. Samples already processed by the last
        // callback remain available to Retry; later callbacks do not extend
        // writtenFrames or the retained generator history.
        if self.generator_running {
            input_frames
        } else {
            0
        }
    }

    fn extend_sigx_capacity(&mut self, required_frames: usize) -> Result<(), EngineError> {
        if required_frames > self.sigx_capacity_frames {
            self.sigx
                .extend_max_frames(required_frames)
                .map_err(|error| EngineError::SigX(format!("{error:?}")))?;
            self.sigx_capacity_frames = required_frames;
        }
        Ok(())
    }

    /// Mirror `SHKSignatureGenerator.flowAudioData`'s post-buffer branch.
    /// The native generator accounts for and processes an entire callback
    /// before testing its remaining frame count, so a callback may cross both
    /// the progressive target and the configured maximum.
    fn handle_native_buffer_boundary(&mut self, now_ns: u64) -> Result<(), EngineError> {
        if !self.generator_running || self.frames_seen < self.native_total_frames_to_record {
            return Ok(());
        }

        if self.frames_seen < self.configured_maximum_frames {
            // Native first checks remainingFrames = totalFramesToRecord -
            // writtenFrames. Once that threshold is reached or crossed, it
            // compares absolute writtenFrames with maximumFramesToRecord. If
            // the callback ends below that maximum, it raises totalFrames to
            // the maximum and generates a Signature, including when the
            // callback overshoots the initial/Retry threshold by some frames.
            self.native_total_frames_to_record = self.configured_maximum_frames;
            self.next_signature_target_frames = None;
            self.retry_frames_active = false;
            self.queue_native_signature(now_ns)?;
            return Ok(());
        }

        // At/above maximum, Native generates only for a positive Retry or
        // when the initial target equaled the maximum. Both paths then fall
        // through to stopGenerating/reachedMaxSigLength, even when the
        // callback ends exactly on the maximum.
        let should_generate_at_maximum =
            self.retry_frames_active || self.initial_frames_equal_maximum;
        self.next_signature_target_frames = None;
        self.retry_frames_active = false;
        if should_generate_at_maximum {
            self.queue_native_signature(now_ns)?;
        }
        self.generator_running = false;
        if !should_generate_at_maximum {
            // A callback that jumps from below the initial target to the
            // maximum (or beyond) takes the no-retry, non-equal branch: stop
            // without a Signature.
            self.lifecycle_status = SessionStatus::GeneratorStopped;
        }
        Ok(())
    }

    fn resume_type2_after_nonterminal_failure(&mut self) -> Result<(), EngineError> {
        if self.input_finished && self.in_flight_attempts.is_empty() {
            self.finish_status();
        } else if !self.pending_restart_times_ns.is_empty() {
            let deadline = self.pending_restart_times_ns[0];
            self.next_signature_not_before_ns = Some(deadline);
            self.lifecycle_status = SessionStatus::WaitingForRetry {
                not_before_ms: nanoseconds_to_milliseconds_ceil(deadline),
                target_sample_ms: duration_ms(
                    self.timing.sample_rate,
                    self.next_cycle_target_frames()?,
                ),
            };
        } else if !self.generator_running && self.frames_seen >= self.maximum_frames {
            self.finish_status();
        } else {
            self.lifecycle_status = SessionStatus::Capturing;
        }
        Ok(())
    }

    fn queue_native_signature(&mut self, now_ns: u64) -> Result<(), EngineError> {
        if let Some(job) = self.create_job_if_minimum_is_met(now_ns)? {
            self.pending_jobs.push_back(job);
        }
        Ok(())
    }

    fn apply_type2_response_retry(
        &mut self,
        retry: Option<RetryDirective>,
    ) -> Result<(), EngineError> {
        if self
            .timing
            .maximum_attempts
            .is_some_and(|maximum| self.attempt_count >= maximum)
        {
            self.finish_status();
            return Ok(());
        }

        // Native Type 2 updateRetry retains the Retry on SHKAudioClosure but
        // does not pass it to the running SignaturePipe. generateMoreSignatures
        // samples intervalDuration when the just-generated Signature is
        // filtered; this response value is for a later cycle, never a retiming
        // of the timer already scheduled for this Signature.
        if let Some(retry) = retry {
            self.next_type2_cycle_interval_ns = Some(native_interval_nanoseconds(
                Some(retry),
                self.timing.default_retry_interval_ms,
            )?);
        }
        self.resume_type2_after_nonterminal_failure()?;
        Ok(())
    }

    fn schedule_type2_cycle_after_signature(&mut self, now_ns: u64) -> Result<(), EngineError> {
        if self
            .timing
            .maximum_attempts
            .is_some_and(|maximum| self.attempt_count >= maximum)
        {
            return Ok(());
        }

        // SHKAudioClosure.processFilterResult starts the recognition attempt,
        // then Type 2's keepTagging path calls generateMoreSignatures. That
        // method dispatches stop/start using intervalDuration (or the Native
        // fallback) before the network response callback has supplied a new
        // Retry. A later response updates only the next cycle's interval.
        self.signature_timeline
            .create_new_current_period_for_retry();
        let interval_ns = self
            .next_type2_cycle_interval_ns
            .unwrap_or(milliseconds_to_nanoseconds(
                self.timing.default_retry_interval_ms,
            )?);
        if self.timing.maximum_retry_delay_ms.is_some_and(|maximum| {
            u128::from(interval_ns) > u128::from(maximum) * u128::from(NANOS_PER_MILLISECOND)
        }) {
            self.lifecycle_status = SessionStatus::Failed(StopReason::InvalidResponse);
            return Ok(());
        }
        let restart_at_ns = now_ns.saturating_add(interval_ns);
        self.pending_restart_times_ns.push(restart_at_ns);
        self.pending_restart_times_ns.sort_unstable();
        self.pending_restart_times_ms
            .push(nanoseconds_to_milliseconds_ceil(restart_at_ns));
        self.pending_restart_times_ms.sort_unstable();
        Ok(())
    }

    fn advance_time_ns(&mut self, now_ns: u64) -> Result<(), EngineError> {
        if now_ns < self.last_now_ns {
            return Err(EngineError::ClockMovedBackwards {
                previous_ms: nanoseconds_to_milliseconds_floor(self.last_now_ns),
                current_ms: nanoseconds_to_milliseconds_floor(now_ns),
            });
        }
        self.last_now_ns = now_ns;
        if !matches!(
            self.lifecycle_status,
            SessionStatus::Succeeded
                | SessionStatus::Exhausted
                | SessionStatus::Failed(_)
                | SessionStatus::Cancelled
        ) && self.timing.session_timeout_ms.is_some_and(|timeout_ms| {
            u128::from(now_ns) >= u128::from(timeout_ms) * u128::from(NANOS_PER_MILLISECOND)
        }) {
            self.pending_jobs.clear();
            self.in_flight_attempts.clear();
            self.matcher_context_registered = false;
            self.lifecycle_status = SessionStatus::Failed(StopReason::SessionTimeout);
            return Ok(());
        }
        while self
            .pending_restart_times_ns
            .first()
            .is_some_and(|deadline| now_ns >= *deadline)
        {
            self.pending_restart_times_ns.remove(0);
            if !self.pending_restart_times_ms.is_empty() {
                self.pending_restart_times_ms.remove(0);
            }
            if self.input_finished {
                if self.in_flight_attempts.is_empty() {
                    self.finish_status();
                }
                break;
            } else {
                self.start_new_cycle()?;
            }
        }
        Ok(())
    }

    fn start_new_cycle(&mut self) -> Result<(), EngineError> {
        let mut sigx = create_sigx(self.timing, self.configured_maximum_frames)?;
        let pre_record_frames = self.pre_record_history.replay_into(&mut sigx)?;
        self.sigx = sigx;
        self.sigx_capacity_frames = self.configured_maximum_frames;
        self.maximum_frames = self.configured_maximum_frames;
        self.frames_seen = pre_record_frames;
        self.generator_running = true;
        self.generator_start_timestamp_ms = None;
        self.generator_timestamp_initialized = false;
        // TaggingOptions.secondsToRecord is progressiveRecordingSeconds for
        // manual Type 1, but sampleSeconds for Auto Type 2.
        let target = self.next_cycle_target_frames()?;
        self.native_total_frames_to_record = target;
        self.next_signature_target_frames = Some(target);
        self.next_signature_not_before_ns = None;
        self.initial_frames_equal_maximum = target == self.maximum_frames;
        self.retry_frames_active = false;
        self.lifecycle_status = SessionStatus::Capturing;
        Ok(())
    }

    fn ensure_pre_record_seedable(&self) -> Result<(), EngineError> {
        if !self.tagging_type.uses_pre_recorded_audio() {
            return Err(EngineError::InvalidTiming(
                "Native Auto Shazam does not consume pre-recorded audio",
            ));
        }
        if self.frames_seen != 0
            || self.attempt_count != 0
            || self.last_now_ns != 0
            || !self.generator_running
            || self.input_finished
            || self.lifecycle_status != SessionStatus::Capturing
        {
            return Err(EngineError::InvalidTiming(
                "pre-recorded PCM must be supplied before ordinary PCM input",
            ));
        }
        Ok(())
    }

    fn finish_status(&mut self) {
        self.matcher_context_registered = false;
        self.lifecycle_status = if self.match_seen {
            SessionStatus::Succeeded
        } else {
            SessionStatus::Exhausted
        };
    }

    fn next_cycle_target_frames(&self) -> Result<usize, EngineError> {
        match self.tagging_type {
            NativeTaggingType::Manual => {
                native_frames_for_ms(self.timing.sample_rate, self.timing.initial_sample_ms)
            }
            NativeTaggingType::Auto => Ok(self.maximum_frames),
        }
    }

    fn stop_with_status(&mut self, status: SessionStatus) {
        self.generator_running = false;
        self.pending_jobs.clear();
        self.matcher_context_registered = false;
        self.next_signature_target_frames = None;
        self.pending_restart_times_ms.clear();
        self.pending_restart_times_ns.clear();
        self.next_signature_not_before_ns = None;
        self.lifecycle_status = status;
    }

    fn ensure_accepting(&self) -> Result<(), EngineError> {
        if self.input_finished
            || matches!(
                self.lifecycle_status,
                SessionStatus::Succeeded
                    | SessionStatus::Exhausted
                    | SessionStatus::Failed(_)
                    | SessionStatus::Cancelled
                    | SessionStatus::SubmittingPostStopRetry { .. }
            )
        {
            Err(EngineError::Closed)
        } else {
            Ok(())
        }
    }

    fn push_report(&self, accepted_frames: usize) -> PushReport {
        PushReport {
            accepted_frames,
            total_frames: self.frames_seen,
            maximum_reached: self.frames_seen >= self.maximum_frames,
        }
    }
}

enum PcmHistoryChunk {
    Int16(Vec<i16>),
    Float32(Vec<f32>),
}

impl PcmHistoryChunk {
    fn frames(&self) -> usize {
        match self {
            Self::Int16(samples) => samples.len(),
            Self::Float32(samples) => samples.len(),
        }
    }
}

/// Bounded, typed PCM history for a caller-configured recorder pre-roll.
/// Values keep their input representation so replay does not quantize Float32
/// input or change the existing SigX preprocessing path.
struct PcmHistory {
    capacity_frames: usize,
    frames: usize,
    chunks: VecDeque<PcmHistoryChunk>,
}

impl PcmHistory {
    fn new(capacity_frames: usize) -> Self {
        Self {
            capacity_frames,
            frames: 0,
            chunks: VecDeque::new(),
        }
    }

    fn frames(&self) -> usize {
        self.frames
    }

    fn capacity_frames(&self) -> usize {
        self.capacity_frames
    }

    fn append_i16(&mut self, samples: &[i16]) {
        if self.capacity_frames == 0 || samples.is_empty() {
            return;
        }
        self.chunks
            .push_back(PcmHistoryChunk::Int16(samples.to_vec()));
        self.frames += samples.len();
        self.trim_to_capacity();
    }

    fn append_f32(&mut self, samples: &[f32]) {
        if self.capacity_frames == 0 || samples.is_empty() {
            return;
        }
        self.chunks
            .push_back(PcmHistoryChunk::Float32(samples.to_vec()));
        self.frames += samples.len();
        self.trim_to_capacity();
    }

    fn trim_to_capacity(&mut self) {
        while self.frames > self.capacity_frames {
            let overflow = self.frames - self.capacity_frames;
            let Some(front_frames) = self.chunks.front().map(PcmHistoryChunk::frames) else {
                self.frames = 0;
                break;
            };
            if overflow >= front_frames {
                self.chunks.pop_front();
                self.frames -= front_frames;
                continue;
            }
            match self.chunks.front_mut() {
                Some(PcmHistoryChunk::Int16(samples)) => {
                    samples.drain(..overflow);
                }
                Some(PcmHistoryChunk::Float32(samples)) => {
                    samples.drain(..overflow);
                }
                None => unreachable!("the front chunk was just observed"),
            }
            self.frames -= overflow;
        }
    }

    fn replay_into(&self, sigx: &mut SigX) -> Result<usize, EngineError> {
        for chunk in &self.chunks {
            match chunk {
                PcmHistoryChunk::Int16(samples) => sigx
                    .push_i16(samples)
                    .map_err(|error| EngineError::SigX(format!("{error:?}")))?,
                PcmHistoryChunk::Float32(samples) => sigx
                    .push_f32(samples)
                    .map_err(|error| EngineError::SigX(format!("{error:?}")))?,
            }
        }
        Ok(self.frames)
    }
}

/// Bounded rolling PCM storage for a live capture adapter that records before
/// the user starts a manual recognition attempt. The buffer keeps only the
/// newest `capacity_frames` and does not know about sampling rates or SigX.
pub struct RollingPcmBuffer<T> {
    capacity_frames: usize,
    frames: usize,
    chunks: VecDeque<Vec<T>>,
}

impl<T: Clone> RollingPcmBuffer<T> {
    pub fn new(capacity_frames: usize) -> Self {
        Self {
            capacity_frames,
            frames: 0,
            chunks: VecDeque::new(),
        }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Change the retention limit while preserving only the newest frames.
    /// Capture adapters can use a conservative pre-Config bound, then shrink
    /// the ring to Native's configured circular-buffer duration once known.
    pub fn set_capacity_frames(&mut self, capacity_frames: usize) {
        self.capacity_frames = capacity_frames;
        if capacity_frames == 0 {
            self.chunks.clear();
            self.frames = 0;
        } else if self.frames > capacity_frames {
            self.discard_oldest(self.frames - capacity_frames);
        }
    }

    pub fn push(&mut self, samples: &[T]) {
        if self.capacity_frames == 0 || samples.is_empty() {
            return;
        }
        if samples.len() >= self.capacity_frames {
            self.chunks.clear();
            self.chunks
                .push_back(samples[samples.len() - self.capacity_frames..].to_vec());
            self.frames = self.capacity_frames;
            return;
        }
        self.chunks.push_back(samples.to_vec());
        self.frames += samples.len();
        while self.frames > self.capacity_frames {
            self.discard_oldest(self.frames - self.capacity_frames);
        }
    }

    fn discard_oldest(&mut self, mut frames_to_discard: usize) {
        while frames_to_discard > 0 {
            let front_len = self
                .chunks
                .front()
                .map(Vec::len)
                .expect("discarding frames requires a non-empty ring buffer");
            if frames_to_discard >= front_len {
                self.chunks.pop_front();
                self.frames -= front_len;
                frames_to_discard -= front_len;
            } else {
                if let Some(front) = self.chunks.front_mut() {
                    front.drain(..frames_to_discard);
                }
                self.frames -= frames_to_discard;
                frames_to_discard = 0;
            }
        }
    }

    pub fn into_samples(self) -> Vec<T> {
        let mut samples = Vec::with_capacity(self.frames);
        for chunk in self.chunks {
            samples.extend(chunk);
        }
        samples
    }
}

fn validate_timing(timing: RecognitionTiming) -> Result<(), EngineError> {
    if timing.sample_rate == 0 {
        return Err(EngineError::InvalidTiming("sample rate must be nonzero"));
    }
    if timing.initial_sample_ms == 0 || timing.maximum_sample_ms == 0 {
        return Err(EngineError::InvalidTiming(
            "initial and maximum sample durations must be nonzero",
        ));
    }
    if timing.minimum_signature_ms > timing.maximum_sample_ms
        || timing.initial_sample_ms > timing.maximum_sample_ms
    {
        return Err(EngineError::InvalidTiming(
            "minimum and initial durations must each be within the maximum sample duration",
        ));
    }
    if timing.request_timeout_ms == 0 || timing.maximum_attempts == Some(0) {
        return Err(EngineError::InvalidTiming(
            "request timeout and maximum attempts must be nonzero",
        ));
    }
    if timing
        .maximum_retry_delay_ms
        .is_some_and(|maximum| timing.default_retry_interval_ms > maximum)
    {
        return Err(EngineError::InvalidTiming(
            "default retry interval exceeds the configured retry-delay safety bound",
        ));
    }
    let _ = frames_for_ms(timing.sample_rate, timing.minimum_signature_ms)?;
    let _ = native_frames_for_ms(timing.sample_rate, timing.initial_sample_ms)?;
    let _ = native_frames_for_ms(timing.sample_rate, timing.maximum_sample_ms)?;
    if let Some(duration) = timing.maximum_retry_duration_ms {
        let _ = native_frames_for_ms(timing.sample_rate, duration)?;
    }
    Ok(())
}

fn create_sigx(timing: RecognitionTiming, maximum_frames: usize) -> Result<SigX, EngineError> {
    // Native TaggingType (Manual/Auto) is independent from `signature_type`.
    // Shazam's ordinary manual Type 1 path initializes SigX with
    // signatureType=2/options=0, whose mapped freeze branch is the live-peak
    // snapshot path rather than the low-level flush_fft checkpoint/rewind path.
    SigX::new(SigXConfig {
        sample_rate: timing.sample_rate,
        channels: 1,
        signature_type: 2,
        signature_options: 0,
        max_frames: maximum_frames,
    })
    .map_err(|error| EngineError::SigX(format!("{error:?}")))
}

fn frames_for_ms(sample_rate: u32, duration_ms: u64) -> Result<usize, EngineError> {
    let frames = u64::from(sample_rate)
        .checked_mul(duration_ms)
        .and_then(|product| product.checked_add(999))
        .ok_or(EngineError::InvalidTiming(
            "sample duration overflows frame count",
        ))?
        / 1_000;
    usize::try_from(frames)
        .map_err(|_| EngineError::InvalidTiming("sample duration exceeds addressable PCM"))
}

/// ShazamTagging converts positive configured/retry durations to frame counts
/// with floating-point multiplication followed by `fcvtzu`/`fcvtzs`, which
/// truncates toward zero. For integer-millisecond inputs this is equivalent
/// to floor(rate * milliseconds / 1000); unlike `frames_for_ms`, it does not
/// round a fractional frame up.
fn native_frames_for_ms(sample_rate: u32, duration_ms: u64) -> Result<usize, EngineError> {
    let frames =
        u64::from(sample_rate)
            .checked_mul(duration_ms)
            .ok_or(EngineError::InvalidTiming(
                "sample duration overflows frame count",
            ))?
            / 1_000;
    usize::try_from(frames)
        .map_err(|_| EngineError::InvalidTiming("sample duration exceeds addressable PCM"))
}

fn native_frames_from_seconds_bits(
    sample_rate: u32,
    seconds_bits: u64,
) -> Result<usize, EngineError> {
    let seconds = f64::from_bits(seconds_bits);
    if sample_rate == 0 || !seconds.is_finite() || seconds < 0.0 {
        return Err(EngineError::InvalidTiming(
            "Native retry duration or sample rate is invalid",
        ));
    }
    let frames = f64::from(sample_rate) * seconds;
    if !frames.is_finite() || frames >= usize::MAX as f64 {
        return Err(EngineError::InvalidTiming(
            "Native retry duration exceeds addressable PCM",
        ));
    }
    Ok(frames as usize)
}

fn milliseconds_to_nanoseconds(milliseconds: u64) -> Result<u64, EngineError> {
    milliseconds
        .checked_mul(NANOS_PER_MILLISECOND)
        .ok_or(EngineError::InvalidTiming(
            "monotonic timestamp exceeds nanosecond clock range",
        ))
}

fn nanoseconds_to_milliseconds_floor(nanoseconds: u64) -> u64 {
    nanoseconds / NANOS_PER_MILLISECOND
}

fn nanoseconds_to_milliseconds_ceil(nanoseconds: u64) -> u64 {
    nanoseconds / NANOS_PER_MILLISECOND + u64::from(nanoseconds % NANOS_PER_MILLISECOND != 0)
}

fn native_interval_nanoseconds(
    retry: Option<RetryDirective>,
    default_interval_ms: u64,
) -> Result<u64, EngineError> {
    let Some(retry) = retry else {
        return milliseconds_to_nanoseconds(default_interval_ms);
    };
    if let Some(seconds_bits) = retry.interval_seconds_bits {
        // Native converts Config's Double seconds to signed nanoseconds before
        // dispatch_after. Rust's float-to-int cast has the same truncation
        // direction and saturating behavior as ARM64 FCVTZS; a negative
        // deadline is already due when handed to dispatch_after.
        let native_nanoseconds = (f64::from_bits(seconds_bits) * 1_000_000_000.0) as i64;
        return Ok(native_nanoseconds.max(0) as u64);
    }
    milliseconds_to_nanoseconds(retry.interval_ms.unwrap_or(0))
}

fn duration_ms(sample_rate: u32, frames: usize) -> u64 {
    (frames as u128 * 1_000 / u128::from(sample_rate)) as u64
}

fn native_sig_stream_now_ticks(fft_frames_processed: u64) -> u64 {
    fft_frames_processed.saturating_sub(NATIVE_SIG_STREAM_INITIAL_DELAY_FRAMES)
}

fn native_signature_sample_length_samples(
    sample_rate: u32,
    cumulative_frames: usize,
) -> Result<u64, EngineError> {
    let cumulative_frames = u64::try_from(cumulative_frames)
        .map_err(|_| EngineError::InvalidTiming("sample count exceeds 64-bit range"))?;
    let offset = u64::from(sample_rate)
        .checked_mul(NATIVE_SIGX_SAMPLE_LENGTH_OFFSET_MS)
        .ok_or(EngineError::InvalidTiming(
            "Native Signature length offset overflows sample count",
        ))?
        / 1_000;
    cumulative_frames
        .checked_add(offset)
        .ok_or(EngineError::InvalidTiming(
            "Native Signature length overflows sample count",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(maximum_sample_ms: u64, maximum_attempts: u64) -> RecognitionTiming {
        RecognitionTiming {
            sample_rate: 16_000,
            minimum_signature_ms: 250,
            initial_sample_ms: 1_750,
            maximum_sample_ms,
            default_retry_interval_ms: NATIVE_TAGGING_TYPE_2_DEFAULT_INTERVAL_MS,
            maximum_retry_duration_ms: Some(9_000),
            session_timeout_ms: None,
            request_timeout_ms: 1_000,
            maximum_retry_delay_ms: Some(30_000),
            maximum_attempts: Some(maximum_attempts),
        }
    }

    #[test]
    fn constructor_uses_exact_native_frame_targets_when_provided() {
        let mut configured = timing(1_235, 2);
        configured.sample_rate = 44_100;
        configured.minimum_signature_ms = 1_000;
        configured.initial_sample_ms = 1_000;
        let session = ProgressiveSession::new_for_type_with_native_frame_targets(
            configured,
            NativeTaggingType::Manual,
            0,
            Some(NativeSampleFrameTargets {
                initial_frames: 44_104,
                maximum_frames: 54_420,
            }),
        )
        .unwrap();

        assert_eq!(session.next_signature_target_frames, Some(44_104));
        assert_eq!(session.native_total_frames_to_record, 44_104);
        assert_eq!(session.maximum_frames, 54_420);
        assert_eq!(session.sigx_capacity_frames, 54_420);
    }

    fn inject_parallel_transport_request(
        session: &mut ProgressiveSession,
        source: &RecognitionJob,
        now_ms: u64,
    ) -> RecognitionJob {
        // Isolate the shared matcher-context membership rule from the ordinary
        // Native PCM path, which has not been observed to emit overlapping
        // requests from one SignatureGenerator without a Retry response.
        session.attempt_count += 1;
        let mut synthetic = source.clone();
        synthetic.attempt = session.attempt_count;
        session.pending_jobs.push_back(synthetic);
        session
            .poll(now_ms)
            .unwrap()
            .expect("synthetic transport request is queued")
    }

    #[test]
    fn manual_generator_request_timestamp_uses_source_start_and_survives_retry() {
        let mut configured = timing(3_000, 2);
        configured.initial_sample_ms = 1_000;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        session
            .set_session_start_timestamp_ms(1_800_000_000_000)
            .unwrap();

        let samples = pcm(48_000);
        session
            .push_i16_at_with_source_timestamp(&samples[..16_000], 1_000, Some(1_800_000_000_123))
            .unwrap();
        let first = session.poll(1_000).unwrap().unwrap();
        assert_eq!(first.signature_timestamp_ms, Some(1_800_000_000_123));

        session
            .complete(
                first.attempt,
                RecognitionOutcome::MatcherFailure {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(2_500),
                        interval_ms: Some(0),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                1_100,
            )
            .unwrap();
        session
            .push_i16_at_with_source_timestamp(
                &samples[16_000..40_000],
                2_600,
                Some(1_800_000_002_999),
            )
            .unwrap();
        let retry = session.poll(2_600).unwrap().unwrap();
        assert_eq!(retry.signature_timestamp_ms, Some(1_800_000_000_123));
    }

    #[test]
    fn auto_generator_cycle_uses_its_own_source_start_timestamp() {
        let mut configured = timing(1_000, 2);
        configured.initial_sample_ms = 1_000;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Auto).unwrap();
        session
            .set_session_start_timestamp_ms(1_800_000_000_000)
            .unwrap();

        let samples = pcm(16_000);
        session
            .push_i16_at_with_source_timestamp(&samples, 1_000, Some(1_800_000_000_111))
            .unwrap();
        let first = session.poll(1_000).unwrap().unwrap();
        assert_eq!(first.signature_timestamp_ms, Some(1_800_000_000_111));
        session
            .complete(
                first.attempt,
                RecognitionOutcome::NoMatch {
                    retry: Some(RetryDirective {
                        retry_duration_ms: None,
                        interval_ms: Some(0),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                1_100,
            )
            .unwrap();

        assert_eq!(session.pending_restart_times_ms, [13_000]);
        session
            .push_i16_at_with_source_timestamp(&samples, 13_000, Some(1_800_000_002_222))
            .unwrap();
        let second = session.poll(13_000).unwrap().unwrap();
        assert_eq!(second.signature_timestamp_ms, Some(1_800_000_002_222));
    }

    #[test]
    fn native_config_and_retry_targets_truncate_fractional_frames() {
        assert_eq!(native_frames_for_ms(44_100, 1).unwrap(), 44);
        assert_eq!(native_frames_for_ms(44_100, 2).unwrap(), 88);

        let mut configured = timing(2, 1);
        configured.sample_rate = 44_100;
        configured.minimum_signature_ms = 0;
        configured.initial_sample_ms = 1;
        let session = ProgressiveSession::new(configured).unwrap();
        assert_eq!(session.next_signature_target_frames, Some(44));
        assert_eq!(session.maximum_frames, 88);

        // Native compares available seconds with the minimum. This is the
        // first whole frame count meeting an integer-millisecond threshold.
        assert_eq!(frames_for_ms(44_100, 1).unwrap(), 45);
    }

    #[test]
    fn retry_uses_original_submillisecond_seconds_for_native_frame_target() {
        let mut configured = timing(6_000, 2);
        configured.sample_rate = 44_100;
        configured.minimum_signature_ms = 1;
        configured.initial_sample_ms = 250;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        session.push_i16(&vec![0; 11_025]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        assert_eq!(first.sample_ms, 250);

        session
            .complete(
                first.attempt,
                RecognitionOutcome::MatcherFailure {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(1_235),
                        interval_ms: Some(0),
                        retry_duration_seconds_bits: Some(1.2345_f64.to_bits()),
                        interval_seconds_bits: None,
                    }),
                },
                1,
            )
            .unwrap();

        assert_eq!(session.native_total_frames_to_record, 54_441);
        assert!(session.retry_frames_active);
    }

    #[test]
    fn native_manual_and_auto_interval_and_buffer_policies_are_distinct() {
        assert_eq!(NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS, 0);
        assert_eq!(NATIVE_TAGGING_TYPE_2_DEFAULT_INTERVAL_MS, 12_000);
        assert!(NativeTaggingType::Manual.uses_pre_recorded_audio());
        assert!(!NativeTaggingType::Auto.uses_pre_recorded_audio());
    }

    #[test]
    fn native_basic_filter_picks_latest_type_zero_signature() {
        let mut timeline = NativeSignatureTimeline::default();
        timeline.store_signature(0, vec![1, 2]);
        timeline.store_signature(1, vec![3, 4]);
        timeline.store_signature(0, vec![5, 6]);

        assert_eq!(timeline.pick_last_of_type(0), Some([5, 6].as_slice()));
        assert_eq!(timeline.pick_last_of_type(1), Some([3, 4].as_slice()));
        assert_eq!(timeline.pick_last_of_type(2), None);

        timeline.create_new_current_period_for_retry();
        assert_eq!(timeline.all_attempts.len(), 1);
        assert_eq!(timeline.all_attempts[0].len(), 3);
        assert_eq!(timeline.pick_last_of_type(0), None);
        timeline.store_signature(0, vec![7, 8]);
        assert_eq!(timeline.pick_last_of_type(0), Some([7, 8].as_slice()));
    }

    fn pcm(frames: usize) -> Vec<i16> {
        (0..frames)
            .map(|index| ((index * 73 % 30_001) as i32 - 15_000) as i16)
            .collect()
    }

    fn signature_for_prefix(samples: &[i16]) -> Vec<u8> {
        let mut fresh = SigX::new(SigXConfig {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: 96_000,
        })
        .unwrap();
        fresh.push_i16(samples).unwrap();
        let stream_pos_ticks = native_sig_stream_now_ticks(fresh.snapshot().fft_frames_processed);
        fresh
            .take_type2_progressive_signature(
                stream_pos_ticks,
                native_signature_sample_length_samples(16_000, samples.len()).unwrap(),
            )
            .unwrap()
    }

    #[test]
    fn native_signature_length_offset_matches_saved_native_rates_and_durations() {
        let cases = [
            (16_000, 48_000, 51_840),
            (16_000, 64_000, 67_840),
            (16_000, 80_000, 83_840),
            (16_000, 96_000, 99_840),
            (16_000, 112_000, 115_840),
            (44_100, 132_300, 142_884),
            (44_100, 264_600, 275_184),
            (48_000, 144_000, 155_520),
            (48_000, 288_000, 299_520),
        ];
        for (sample_rate, input_frames, expected_samples) in cases {
            assert_eq!(
                native_signature_sample_length_samples(sample_rate, input_frames).unwrap(),
                expected_samples,
                "sample_rate={sample_rate}, input_frames={input_frames}"
            );
        }
    }

    #[test]
    fn native_sig_stream_now_applies_the_45_frame_warmup_and_zero_clamp() {
        for (processed, expected) in [(0, 0), (44, 0), (45, 0), (46, 1), (48, 3), (100, 55)] {
            assert_eq!(native_sig_stream_now_ticks(processed), expected);
        }
    }

    #[test]
    fn generated_signature_header_uses_native_sig_stream_now_ticks() {
        let cases = [(4_000, 250), (48_000, 3_000)];
        for (frames, initial_sample_ms) in cases {
            let maximum_sample_ms = initial_sample_ms.max(3_000);
            let mut configured = timing(maximum_sample_ms, 1);
            configured.initial_sample_ms = initial_sample_ms;
            let mut session =
                ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
            session.push_i16(&pcm(frames)).unwrap();
            let expected_ticks =
                native_sig_stream_now_ticks(session.sigx.snapshot().fft_frames_processed);
            let job = session.poll(0).unwrap().unwrap();
            let header = Signature::parse(&job.signature).unwrap().legacy_header();
            assert_eq!(header.stream_pos_ticks_raw, expected_ticks);
        }
    }

    #[test]
    fn generated_signature_header_uses_cumulative_cycle_length() {
        let mut configured = timing(6_000, 1);
        configured.initial_sample_ms = 6_000;
        let mut session = ProgressiveSession::new(configured).unwrap();
        session.push_i16(&pcm(96_000)).unwrap();
        let job = session.poll(0).unwrap().unwrap();
        let signature = Signature::parse(&job.signature).unwrap();
        assert_eq!(job.sample_ms, 6_000);
        assert_eq!(signature.legacy_header().sample_length_samples, 99_840);
    }

    #[test]
    fn initial_threshold_is_cumulative_pcm_not_wall_clock() {
        let mut session = ProgressiveSession::new(timing(6_875, 3)).unwrap();
        let samples = pcm(28_000);
        assert_eq!(session.frames_seen(), 0);
        assert!(session.poll(80_000).unwrap().is_none());
        session.push_i16(&samples[..27_999]).unwrap();
        assert!(session.poll(80_000).unwrap().is_none());
        session.push_i16_at(&samples[27_999..], 80_001).unwrap();
        let job = session.poll(80_001).unwrap().unwrap();
        assert_eq!(job.sample_ms, 1_750);
        assert_eq!(job.generated_at_ms, 80_001);
        assert_eq!(job.attempt, 1);
        assert_eq!(session.signature_timeline.all_attempts.len(), 1);
        assert_eq!(
            session.signature_timeline.all_attempts[0][0].bytes,
            job.signature
        );
        assert_eq!(session.signature_timeline.pick_last_of_type(0), None);
        assert_eq!(
            Signature::parse(&job.signature).unwrap().len(),
            job.signature.len()
        );
    }

    #[test]
    fn native_callback_overshoot_of_initial_target_below_maximum_generates_signature() {
        let mut configured = timing(6_875, 3);
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();

        let report = session.push_i16(&pcm(28_001)).unwrap();
        assert_eq!(report.accepted_frames, 28_001);
        assert_eq!(session.frames_seen(), 28_001);
        assert!(session.generator_running);
        assert_eq!(session.native_total_frames_to_record, 110_000);
        let job = session.poll(0).unwrap().unwrap();
        assert_eq!(job.sample_ms, 1_750);
        assert_eq!(job.attempt, 1);
        let signature = Signature::parse(&job.signature).unwrap();
        assert_eq!(
            signature.legacy_header().sample_length_samples,
            native_signature_sample_length_samples(16_000, 28_001).unwrap()
        );
    }

    #[test]
    fn native_retry_at_exact_maximum_generates_then_stops() {
        let mut configured = timing(2_000, 3);
        configured.initial_sample_ms = 1_000;
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        session.push_i16(&pcm(16_000)).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        assert_eq!(first.sample_ms, 1_000);
        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::MatcherFailure {
                        retry: Some(RetryDirective {
                            retry_duration_ms: Some(2_000),
                            interval_ms: Some(0),
                            retry_duration_seconds_bits: None,
                            interval_seconds_bits: None,
                        }),
                    },
                    100,
                )
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 100,
                target_sample_ms: 2_000,
            }
        );

        session.push_i16_at(&pcm(16_000), 100).unwrap();
        assert_eq!(session.frames_seen(), 32_000);
        assert!(!session.generator_running);
        let at_maximum = session.poll(100).unwrap().unwrap();
        assert_eq!(at_maximum.attempt, 2);
        assert_eq!(at_maximum.sample_ms, 2_000);
        assert_eq!(session.status(), SessionStatus::InFlight { attempt: 2 });
    }

    #[test]
    fn native_callback_crossing_initial_and_maximum_without_retry_stops_without_signature() {
        let mut configured = timing(4_000, 3);
        configured.initial_sample_ms = 3_900;
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();

        // One callback crosses the 3.9 s initial target and the 4 s maximum.
        // Native's flowAudioData branch stops without generating because the
        // initial target was not equal to the configured maximum.
        let report = session.push_i16(&pcm(65_536)).unwrap();
        assert_eq!(report.accepted_frames, 65_536);
        assert!(report.maximum_reached);
        assert_eq!(session.frames_seen(), 65_536);
        assert_eq!(session.status(), SessionStatus::GeneratorStopped);
        assert!(!session.generator_running);
        assert!(session.poll(0).unwrap().is_none());
    }

    #[test]
    fn native_callback_crossing_equal_initial_and_maximum_generates_then_stops() {
        let mut configured = timing(4_000, 3);
        configured.initial_sample_ms = 4_000;
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();

        let report = session.push_i16(&pcm(65_536)).unwrap();
        assert_eq!(report.accepted_frames, 65_536);
        assert_eq!(session.frames_seen(), 65_536);
        assert!(!session.generator_running);

        let job = session.poll(0).unwrap().unwrap();
        assert_eq!(job.sample_ms, 4_096);
        assert_eq!(job.attempt, 1);
        assert_eq!(session.status(), SessionStatus::InFlight { attempt: 1 });
    }

    #[test]
    fn native_minimum_gate_stops_generator_and_rejects_later_pcm() {
        let mut session = ProgressiveSession::new(RecognitionTiming {
            sample_rate: 16_000,
            minimum_signature_ms: 2_000,
            initial_sample_ms: 1_000,
            maximum_sample_ms: 9_000,
            default_retry_interval_ms: NATIVE_TAGGING_TYPE_2_DEFAULT_INTERVAL_MS,
            maximum_retry_duration_ms: Some(12_000),
            session_timeout_ms: None,
            request_timeout_ms: 1_000,
            maximum_retry_delay_ms: Some(30_000),
            maximum_attempts: Some(3),
        })
        .unwrap();
        assert_eq!(session.next_signature_target_frames, Some(16_000));
        let samples = pcm(32_000);
        session.push_i16(&samples[..31_999]).unwrap();
        assert!(session.poll(2_000).unwrap().is_none());
        session.push_i16(&samples[31_999..]).unwrap();
        assert!(session.poll(2_001).unwrap().is_none());
        assert_eq!(session.status(), SessionStatus::GeneratorStopped);

        // The outer Tagging Type 2 context may remain alive, but
        // flowAudioData returns when `generating` is false; this generator's
        // writtenFrames and retained PCM therefore do not advance.
        let report = session.push_i16(&samples[..1]).unwrap();
        assert_eq!(report.accepted_frames, 0);
        assert_eq!(report.total_frames, 31_999);
        assert_eq!(session.frames_seen(), 31_999);
        assert!(session.poll(2_002).unwrap().is_none());
        session.finish_input();
        assert_eq!(session.status(), SessionStatus::Exhausted);
    }

    #[test]
    fn manual_generator_rejects_pcm_after_minimum_gate_stops_it() {
        let mut configured = timing(3_000, 3);
        configured.initial_sample_ms = 500;
        configured.minimum_signature_ms = 1_000;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();

        session.push_i16(&pcm(8_000)).unwrap();
        assert_eq!(
            session.status(),
            SessionStatus::Failed(StopReason::SignatureGenerationFailure)
        );
        assert!(session.is_terminal());
        assert_eq!(session.frames_seen(), 8_000);

        assert_eq!(
            session.push_i16_at(&pcm(8_000), 100),
            Err(EngineError::Closed)
        );
        assert_eq!(session.frames_seen(), 8_000);
        assert!(session.poll(100).unwrap().is_none());
    }

    #[test]
    fn retry_duration_threshold_is_relative_to_the_current_cycle() {
        let mut configured = timing(6_875, 3);
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples = pcm(110_000);
        session.push_i16(&samples[..28_000]).unwrap();
        let first = session.poll(3_000).unwrap().unwrap();

        let mut reference = SigX::new(SigXConfig {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: 110_000,
        })
        .unwrap();
        reference.push_i16(&samples[..28_000]).unwrap();
        let first_reference = reference
            .take_type2_progressive_signature(
                native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed),
                native_signature_sample_length_samples(16_000, 28_000).unwrap(),
            )
            .unwrap();
        assert_eq!(first.signature, first_reference);
        // A signatureType=2 snapshot must leave the active generator's stream
        // cursor intact so the later Retry includes the same accumulated PCM.
        assert_eq!(session.sigx.snapshot(), reference.snapshot());

        // Audio continues flowing while the HTTP request is in flight.
        reference.push_i16(&samples[28_000..32_000]).unwrap();
        let pushed = session.push_i16(&samples[28_000..32_000]).unwrap();
        assert_eq!(pushed.total_frames, 32_000);
        assert_eq!(session.status(), SessionStatus::InFlight { attempt: 1 });
        session
            .complete(
                1,
                RecognitionOutcome::MatcherFailure {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(2_500),
                        interval_ms: Some(20_000),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                3_050,
            )
            .unwrap();
        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 3_050,
                target_sample_ms: 2_500,
            }
        );
        assert!(session.pending_restart_times_ms.is_empty());

        assert!(session.poll(3_050).unwrap().is_none());
        reference.push_i16(&samples[32_000..40_000]).unwrap();
        let pushed = session.push_i16(&samples[32_000..40_000]).unwrap();
        assert_eq!(pushed.total_frames, 40_000);
        let second = session.poll(3_051).unwrap().unwrap();
        assert_eq!(second.sample_ms, 2_500);
        assert_eq!(second.attempt, 2);
        let second_reference = reference
            .take_type2_progressive_signature(
                native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed),
                native_signature_sample_length_samples(16_000, 40_000).unwrap(),
            )
            .unwrap();
        assert_eq!(second.signature, second_reference);
        assert_eq!(session.sigx.snapshot(), reference.snapshot());
        Signature::parse(&second.signature).expect("intermediate Signature parses");

        // A later matcher error without Retry terminates the manual Type 1
        // session; no Type 2 interval fallback applies to this path.
        session
            .complete(2, RecognitionOutcome::MatcherFailure { retry: None }, 3_200)
            .unwrap();
        assert_eq!(
            session.status(),
            SessionStatus::Failed(StopReason::InvalidResponse)
        );
        assert!(session.pending_restart_times_ms.is_empty());
    }

    #[test]
    fn active_retry_below_written_frames_waits_for_the_next_pcm_chunk() {
        let mut configured = timing(6_875, 3);
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples = pcm(28_001);
        session.push_i16(&samples[..28_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        assert_eq!(first.sample_ms, 1_750);
        let mut reference = SigX::new(SigXConfig {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: 28_001,
        })
        .unwrap();
        reference.push_i16(&samples[..28_000]).unwrap();
        let first_stream_pos =
            native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed);
        reference
            .take_type2_progressive_signature(
                first_stream_pos,
                native_signature_sample_length_samples(16_000, 28_000).unwrap(),
            )
            .unwrap();

        session
            .complete(
                first.attempt,
                RecognitionOutcome::MatcherFailure {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(1_000),
                        interval_ms: Some(500),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                100,
            )
            .unwrap();

        // The retry threshold is already behind writtenFrames. Native applies
        // the new threshold asynchronously and evaluates it after the next
        // audio flow, so the existing prefix alone cannot trigger an emit.
        assert!(session.poll(100).unwrap().is_none());
        reference.push_i16(&samples[28_000..]).unwrap();
        session.push_i16(&samples[28_000..]).unwrap();
        let retry = session.poll(100).unwrap().unwrap();
        assert_eq!(retry.sample_ms, 1_750);
        let stream_pos = native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed);
        let expected = reference
            .take_type2_progressive_signature(
                stream_pos,
                native_signature_sample_length_samples(16_000, 28_001).unwrap(),
            )
            .unwrap();
        assert_eq!(retry.signature, expected);
    }

    #[test]
    fn timer_restart_uses_config_sample_seconds_not_initial_progressive_seconds() {
        let mut configured = timing(12_000, 3);
        configured.initial_sample_ms = 3_000;
        let mut session = ProgressiveSession::new(configured).unwrap();
        let first_cycle = pcm(192_000);
        session.push_i16_at(&first_cycle[..48_000], 3_000).unwrap();
        let first = session.poll(3_000).unwrap().unwrap();
        assert_eq!(first.sample_ms, 3_000);

        session.push_i16(&first_cycle[48_000..54_400]).unwrap();
        session
            .complete(
                first.attempt,
                RecognitionOutcome::NoMatch {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(5_000),
                        interval_ms: Some(0),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                3_400,
            )
            .unwrap();

        // The first cycle's timer was already scheduled from the Native
        // default interval when its Signature was filtered. The response's
        // zero interval applies to the following cycle, not this one.
        assert_eq!(session.pending_restart_times_ms, [15_000]);
        assert!(session.poll(14_999).unwrap().is_none());
        assert!(session.poll(15_000).unwrap().is_none());
        assert_eq!(session.sample_ms(), 0);
        let restarted = session
            .push_i16_at(&first_cycle[..191_999], 15_000)
            .unwrap();
        assert_eq!(restarted.total_frames, 191_999);
        assert!(session.poll(26_999).unwrap().is_none());
        session
            .push_i16_at(&first_cycle[191_999..], 27_000)
            .unwrap();
        let second = session
            .poll(27_000)
            .unwrap()
            .expect("Config sampleSeconds threshold should trigger the next Signature");
        assert_eq!(second.sample_ms, 12_000);
        assert_eq!(second.attempt, 2);

        let mut fresh_cycle = SigX::new(SigXConfig {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: first_cycle.len(),
        })
        .unwrap();
        fresh_cycle.push_i16(&first_cycle).unwrap();
        let fresh_cycle_signature = fresh_cycle
            .take_type2_progressive_signature(
                native_sig_stream_now_ticks(fresh_cycle.snapshot().fft_frames_processed),
                native_signature_sample_length_samples(16_000, first_cycle.len()).unwrap(),
            )
            .unwrap();
        assert_eq!(second.signature, fresh_cycle_signature);
    }

    #[test]
    fn type2_no_match_restarts_with_fresh_signature_stream_after_interval() {
        let mut session = ProgressiveSession::new(timing(6_000, 4)).unwrap();
        let first_cycle = pcm(32_000);
        let next_cycle = pcm(96_000);
        session.push_i16(&first_cycle[..28_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        assert_eq!(first.sample_ms, 1_750);
        assert_eq!(session.signature_timeline.all_attempts.len(), 1);
        assert!(session.signature_timeline.current_attempt.is_empty());
        session.push_i16(&first_cycle[28_000..]).unwrap();
        session
            .complete(
                first.attempt,
                RecognitionOutcome::NoMatch {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(2_000),
                        interval_ms: Some(100),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                1,
            )
            .unwrap();
        assert_eq!(session.signature_timeline.all_attempts.len(), 1);
        assert!(session.signature_timeline.current_attempt.is_empty());
        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 6_000,
            }
        );
        assert_eq!(session.pending_restart_times_ms, [12_000]);
        assert!(session.poll(11_999).unwrap().is_none());
        assert!(session.poll(12_000).unwrap().is_none());
        session.push_i16_at(&next_cycle, 12_000).unwrap();
        let second = session.poll(12_000).unwrap().unwrap();
        assert_eq!(second.sample_ms, 6_000);
        assert_eq!(session.signature_timeline.all_attempts.len(), 2);
        assert_eq!(session.signature_timeline.all_attempts[0].len(), 1);
        assert!(session.signature_timeline.current_attempt.is_empty());
        assert_eq!(session.pending_restart_times_ms, [12_100]);

        let mut reference = SigX::new(SigXConfig {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: next_cycle.len(),
        })
        .unwrap();
        reference.push_i16(&next_cycle).unwrap();
        let reference_signature = reference
            .take_type2_progressive_signature(
                native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed),
                native_signature_sample_length_samples(16_000, next_cycle.len()).unwrap(),
            )
            .unwrap();
        assert_eq!(second.signature, reference_signature);
    }

    #[test]
    fn insufficient_retry_duration_waits_then_starts_a_fresh_cycle() {
        let mut session = ProgressiveSession::new(timing(5_000, 4)).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let first = session.poll(100).unwrap().unwrap();
        session
            .complete(
                first.attempt,
                RecognitionOutcome::NoMatch {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(2_500),
                        interval_ms: Some(250),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                120,
            )
            .unwrap();
        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 5_000,
            }
        );
        assert!(session.poll(200).unwrap().is_none());
        assert!(session.poll(11_999).unwrap().is_none());
        assert!(session.poll(12_000).unwrap().is_none());
        assert_eq!(session.status(), SessionStatus::Capturing);
        session.push_i16_at(&pcm(80_000), 12_000).unwrap();
        assert_eq!(session.poll(12_001).unwrap().unwrap().sample_ms, 5_000);
    }

    #[test]
    fn native_auto_generator_does_not_emit_again_at_max_without_retry() {
        let mut session = ProgressiveSession::new(timing(6_000, 3)).unwrap();
        let cycle_a = pcm(96_000);
        let cycle_b: Vec<i16> = (0..96_000)
            .map(|index| ((index * 97 % 27_001) - 13_500) as i16)
            .collect();
        session.push_i16(&cycle_a[..28_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        assert_eq!(first.sample_ms, 1_750);

        session.push_i16(&cycle_a[28_000..]).unwrap();
        assert_eq!(session.frames_seen(), 96_000);
        assert!(!session.generator_running);
        assert!(session.poll(0).unwrap().is_none());
        assert_eq!(session.in_flight_attempts(), vec![1]);
        session
            .complete(
                first.attempt,
                RecognitionOutcome::NoMatch { retry: None },
                10,
            )
            .unwrap();
        assert_eq!(
            session.lifecycle_status,
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 6_000,
            }
        );
        assert!(session.poll(11_999).unwrap().is_none());
        assert!(session.poll(12_000).unwrap().is_none());

        let restarted = session.push_i16_at(&cycle_b, 12_000).unwrap();
        assert_eq!(restarted.total_frames, 96_000);
        let second = session.poll(12_001).unwrap().unwrap();
        assert_eq!(second.attempt, 2);
        assert_eq!(second.sample_ms, 6_000);
        assert_eq!(second.signature, signature_for_prefix(&cycle_b));
        assert_ne!(second.signature, signature_for_prefix(&cycle_a));
        assert_eq!(session.in_flight_attempts(), vec![2]);
    }

    #[test]
    fn manual_generator_stops_at_max_without_retry_signature() {
        let mut configured = timing(3_000, 3);
        configured.initial_sample_ms = 1_000;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples = pcm(48_001);
        session.push_i16(&samples[..16_000]).unwrap();
        let initial = session.poll(0).unwrap().unwrap();
        session.push_i16(&samples[16_000..48_000]).unwrap();
        session.push_i16(&samples[48_000..]).unwrap();
        assert!(!session.generator_running);
        assert!(session.poll(0).unwrap().is_none());
        assert_eq!(session.in_flight_attempts(), vec![1]);

        assert_eq!(
            session
                .complete(
                    initial.attempt,
                    RecognitionOutcome::Match { retry: None },
                    10,
                )
                .unwrap(),
            SessionStatus::Succeeded
        );
        assert_eq!(session.lifecycle_status, SessionStatus::Succeeded);
        assert!(session.is_terminal());
        assert!(session.in_flight_attempts().is_empty());
    }

    #[test]
    fn shared_native_context_drops_late_response_until_a_request_reregisters_it() {
        let mut configured = timing(2_000, 5);
        configured.initial_sample_ms = 1_000;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples = pcm(48_000);

        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        let overlapping = inject_parallel_transport_request(&mut session, &first, 0);
        assert!(session.response_is_deliverable(first.attempt));
        assert!(session.response_is_deliverable(overlapping.attempt));

        let (status, delivered) = session
            .complete_with_delivery(
                first.attempt,
                RecognitionOutcome::MatcherFailure {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(3_000),
                        interval_ms: Some(0),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                100,
            )
            .unwrap();
        assert!(delivered);
        assert_eq!(
            status,
            SessionStatus::InFlight {
                attempt: overlapping.attempt,
            }
        );
        assert_eq!(
            session.lifecycle_status,
            SessionStatus::WaitingForRetry {
                not_before_ms: 100,
                target_sample_ms: 3_000,
            }
        );
        assert!(!session.response_is_deliverable(overlapping.attempt));

        let (status, delivered) = session
            .complete_with_delivery(
                overlapping.attempt,
                RecognitionOutcome::NoMatch { retry: None },
                200,
            )
            .unwrap();
        assert!(!delivered);
        assert_eq!(
            status,
            SessionStatus::WaitingForRetry {
                not_before_ms: 100,
                target_sample_ms: 3_000,
            }
        );
        assert!(session.in_flight_attempts().is_empty());

        session.push_i16_at(&samples[16_000..48_000], 250).unwrap();
        let later = session.poll(250).unwrap().unwrap();
        assert!(session.response_is_deliverable(later.attempt));
    }

    #[test]
    fn shared_native_context_membership_can_admit_an_older_response_after_reregistration() {
        let mut configured = timing(2_000, 5);
        configured.initial_sample_ms = 1_000;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples = pcm(48_000);

        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        let older = inject_parallel_transport_request(&mut session, &first, 0);
        session
            .complete_with_delivery(
                first.attempt,
                RecognitionOutcome::MatcherFailure {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(3_000),
                        interval_ms: Some(0),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                100,
            )
            .unwrap();

        session.push_i16_at(&samples[16_000..48_000], 150).unwrap();
        let newer = session.poll(150).unwrap().unwrap();
        assert!(session.response_is_deliverable(older.attempt));
        let (_, delivered) = session
            .complete_with_delivery(
                older.attempt,
                RecognitionOutcome::NoMatch { retry: None },
                200,
            )
            .unwrap();
        assert!(delivered);
        assert!(session.is_terminal());
        assert!(session.in_flight_attempts().contains(&newer.attempt));
    }

    #[test]
    fn cancellation_returns_the_single_native_request_id() {
        let mut session = ProgressiveSession::new(timing(6_000, 3)).unwrap();
        let samples = pcm(96_000);
        session.push_i16(&samples[..28_000]).unwrap();
        let request = session.poll(0).unwrap().unwrap();
        assert_eq!(request.attempt, 1);
        assert!(session.response_is_deliverable(request.attempt));
        session.push_i16(&samples[28_000..]).unwrap();
        assert!(session.poll(0).unwrap().is_none());
        assert_eq!(session.cancel_all(), vec![1]);
        assert_eq!(session.status(), SessionStatus::Cancelled);
        assert!(!session.response_is_deliverable(request.attempt));
        assert!(session.in_flight_attempts().is_empty());
    }

    #[test]
    fn cancellation_retires_every_attempt_sharing_the_native_context() {
        let mut session = ProgressiveSession::new(timing(6_000, 3)).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        let overlapping = inject_parallel_transport_request(&mut session, &first, 0);
        assert_eq!(
            session.in_flight_attempts(),
            vec![first.attempt, overlapping.attempt]
        );
        assert!(session.response_is_deliverable(first.attempt));
        assert!(session.response_is_deliverable(overlapping.attempt));

        assert_eq!(
            session.cancel_all(),
            vec![first.attempt, overlapping.attempt]
        );
        assert_eq!(session.status(), SessionStatus::Cancelled);
        assert!(!session.response_is_deliverable(first.attempt));
        assert!(!session.response_is_deliverable(overlapping.attempt));
        assert!(session.in_flight_attempts().is_empty());
    }

    #[test]
    fn type2_match_without_retry_keeps_tagging_after_native_default_interval() {
        let mut session = ProgressiveSession::new(timing(6_875, 3)).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let first = session.poll(1_000).unwrap().unwrap();
        assert_eq!(first.sample_ms, 1_750);
        assert_eq!(session.signature_timeline.all_attempts.len(), 1);
        assert!(session.signature_timeline.current_attempt.is_empty());

        // Static ARM64: keepTaggingAfterAMatch is true for TaggingType 2.
        // With no server Retry, AudioClosure schedules its fallback
        // stop/start cycle after defaultIntervalSeconds (12 seconds).
        session
            .complete(
                first.attempt,
                RecognitionOutcome::Match { retry: None },
                1_250,
            )
            .unwrap();
        assert_eq!(session.signature_timeline.all_attempts.len(), 1);
        assert!(session.signature_timeline.current_attempt.is_empty());
        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 6_875,
            }
        );
        assert!(session.poll(11_999).unwrap().is_none());

        let next_cycle = session.push_i16_at(&pcm(110_000), 12_000).unwrap();
        assert_eq!(next_cycle.total_frames, 110_000);
        let second = session
            .poll(12_000)
            .unwrap()
            .expect("Type 2 continues after a match and starts another cycle");
        assert_eq!(second.attempt, 2);
        assert_eq!(second.sample_ms, 6_875);
        assert_eq!(session.signature_timeline.all_attempts.len(), 2);
        assert!(session.signature_timeline.current_attempt.is_empty());
    }

    #[test]
    fn manual_type1_match_without_retry_stops_without_a_12_second_restart() {
        let mut configured = timing(6_875, 3);
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let first = session.poll(1_000).unwrap().unwrap();

        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::Match { retry: None },
                    1_250
                )
                .unwrap(),
            SessionStatus::Succeeded
        );
        assert!(matches!(
            session.push_i16(&pcm(1)),
            Err(EngineError::Closed)
        ));
    }

    #[test]
    fn native_generator_3s_threshold_and_4s_retry_match_live_probe() {
        let mut configured = timing(12_000, 3);
        configured.initial_sample_ms = 3_000;
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        configured.maximum_retry_duration_ms = Some(12_000);
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples: Vec<f32> = (0..64_000)
            .map(|index| {
                let time = index as f64 / 16_000.0;
                (0.38 * (2.0 * std::f64::consts::PI * 440.0 * time).sin()
                    + 0.17 * (2.0 * std::f64::consts::PI * 997.0 * time).sin())
                    as f32
            })
            .collect();

        let mut first_cycle_frames = 0usize;
        for chunk in samples[..48_000].chunks(1_024) {
            first_cycle_frames += chunk.len();
            session
                .push_f32_at(chunk, first_cycle_frames as u64 * 1_000 / 16_000)
                .unwrap();
        }
        let first = session.poll(3_000).unwrap().unwrap();
        assert_eq!(first.sample_ms, 3_000);
        assert_eq!(session.frames_seen(), 48_000);

        session
            .complete(
                first.attempt,
                RecognitionOutcome::Match {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(4_000),
                        interval_ms: Some(0),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                3_100,
            )
            .unwrap();
        assert!(session.pending_restart_times_ms.is_empty());
        assert!(session.generator_running);
        assert_eq!(session.next_signature_target_frames, Some(64_000));

        for chunk in samples[48_000..].chunks(1_024) {
            session.push_f32_at(chunk, 3_200).unwrap();
        }
        let retry = session.poll(3_200).unwrap().unwrap();
        assert_eq!(retry.attempt, 2);
        assert_eq!(retry.sample_ms, 4_000);
        assert_eq!(session.frames_seen(), 64_000);
        assert_eq!(session.maximum_frames, 192_000);
        assert!(session.generator_running);
    }

    #[test]
    fn manual_type1_match_with_positive_retry_requests_complete_result_signature() {
        let mut configured = timing(4_000, 3);
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples = pcm(96_000);
        session.push_i16_at(&samples[..28_000], 1_750).unwrap();
        let first = session.poll(1_750).unwrap().unwrap();
        assert_eq!(first.generated_at_ms, 1_750);

        let mut reference = SigX::new(SigXConfig {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: 64_000,
        })
        .unwrap();
        reference.push_i16(&samples[..28_000]).unwrap();
        let first_reference = reference
            .take_type2_progressive_signature(
                native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed),
                native_signature_sample_length_samples(16_000, 28_000).unwrap(),
            )
            .unwrap();
        assert_eq!(first.signature, first_reference);
        assert_eq!(session.sigx.snapshot(), reference.snapshot());

        // PCM continues to enter the same stream while the first HTTP request
        // is in flight; the positive Match+Retry response arrives afterward.
        reference.push_i16(&samples[28_000..32_000]).unwrap();
        session
            .push_i16_at(&samples[28_000..32_000], 2_000)
            .unwrap();
        assert_eq!(session.status(), SessionStatus::InFlight { attempt: 1 });

        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::Match {
                        retry: Some(RetryDirective {
                            retry_duration_ms: Some(6_000),
                            // The active Manual generator consumes the retry
                            // duration as its next PCM threshold; this path
                            // does not schedule a stop/start interval.
                            interval_ms: Some(5_000),
                            retry_duration_seconds_bits: None,
                            interval_seconds_bits: None,
                        }),
                    },
                    2_100,
                )
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 2_100,
                target_sample_ms: 6_000,
            }
        );
        assert_eq!(session.maximum_frames, 96_000);
        reference.extend_max_frames(96_000).unwrap();
        assert!(session.pending_restart_times_ms.is_empty());
        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 2_100,
                target_sample_ms: 6_000,
            }
        );

        // The bridge applies this positive Retry to the still-running
        // generator; it is not an inter-cycle wait or a fresh SigX instance.
        reference.push_i16(&samples[32_000..]).unwrap();
        session.push_i16_at(&samples[32_000..], 6_000).unwrap();
        let complete_result = session.poll(6_000).unwrap().unwrap();
        assert_eq!(complete_result.attempt, 2);
        assert_eq!(complete_result.sample_ms, 6_000);
        let complete_reference = reference
            .take_type2_progressive_signature(
                native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed),
                native_signature_sample_length_samples(16_000, samples.len()).unwrap(),
            )
            .unwrap();
        assert_eq!(complete_result.signature, complete_reference);
        assert_eq!(session.sigx.snapshot(), reference.snapshot());
        assert_eq!(session.pending_restart_times_ms.len(), 0);

        assert_eq!(
            session
                .complete(
                    complete_result.attempt,
                    RecognitionOutcome::Match { retry: None },
                    6_100,
                )
                .unwrap(),
            SessionStatus::Succeeded
        );
    }

    #[test]
    fn manual_match_retry_after_generator_stop_uses_retained_pcm_after_input_end() {
        let mut configured = timing(3_000, 3);
        configured.initial_sample_ms = 1_000;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples = pcm(48_000);
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();

        session.push_i16(&samples[16_000..]).unwrap();
        assert!(!session.generator_running);
        assert_eq!(session.frames_seen(), 48_000);
        assert_eq!(session.status(), SessionStatus::InFlight { attempt: 1 });
        session.finish_input();

        let (status, delivered) = session
            .complete_with_delivery(
                first.attempt,
                RecognitionOutcome::Match {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(2_500),
                        // Manual TaggingType 1 applies the duration to the
                        // current generator and does not schedule this interval.
                        interval_ms: Some(7_000),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                4_000,
            )
            .unwrap();
        assert!(delivered);
        assert_eq!(
            status,
            SessionStatus::WaitingForRetry {
                not_before_ms: 4_000,
                target_sample_ms: 3_000,
            }
        );
        assert!(session.pending_restart_times_ms.is_empty());

        let retry = session
            .poll(4_000)
            .unwrap()
            .expect("stopped Native generator emits from its retained PCM immediately");
        assert_eq!(retry.attempt, 2);
        assert_eq!(retry.sample_ms, 3_000);
        assert_eq!(retry.generated_at_ms, 4_000);
        assert!(!session.is_terminal());
        assert_eq!(session.status(), SessionStatus::InFlight { attempt: 2 });
        assert!(session.response_is_deliverable(retry.attempt));

        let mut reference = SigX::new(SigXConfig {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: 48_000,
        })
        .unwrap();
        reference.push_i16(&samples).unwrap();
        let expected = reference
            .take_type2_progressive_signature(
                native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed),
                native_signature_sample_length_samples(16_000, samples.len()).unwrap(),
            )
            .unwrap();
        assert_eq!(retry.signature, expected);

        assert_eq!(
            session
                .complete(
                    retry.attempt,
                    RecognitionOutcome::Match { retry: None },
                    4_100,
                )
                .unwrap(),
            SessionStatus::Succeeded
        );
    }

    #[test]
    fn end_of_input_unfulfillable_positive_match_retry_finishes_succeeded() {
        let mut configured = timing(3_000, 3);
        configured.initial_sample_ms = 1_000;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples = pcm(48_000);
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();

        session.push_i16(&samples[16_000..]).unwrap();
        assert!(!session.generator_running);
        session.finish_input();

        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::Match {
                        retry: Some(RetryDirective {
                            retry_duration_ms: Some(4_000),
                            interval_ms: Some(0),
                            retry_duration_seconds_bits: None,
                            interval_seconds_bits: None,
                        }),
                    },
                    4_000,
                )
                .unwrap(),
            SessionStatus::Succeeded
        );
        assert!(session.poll(4_001).unwrap().is_none());
        assert!(session.in_flight_attempts().is_empty());
    }

    #[test]
    fn manual_type1_no_match_without_retry_stops_context() {
        let mut configured = timing(2_000, 3);
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let first = session.poll(1_000).unwrap().unwrap();

        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::NoMatch { retry: None },
                    1_250
                )
                .unwrap(),
            SessionStatus::Exhausted
        );
        assert!(session.poll(13_250).unwrap().is_none());
        assert_eq!(session.push_i16(&[0]).unwrap_err(), EngineError::Closed);
    }

    #[test]
    fn manual_type1_empty_match_with_accepted_retry_keeps_context_active() {
        let mut configured = timing(2_000, 3);
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        session.push_i16_at(&pcm(28_000), 1_000).unwrap();
        let first = session.poll(1_000).unwrap().unwrap();

        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::NoMatch {
                        retry: Some(RetryDirective {
                            retry_duration_ms: Some(2_500),
                            interval_ms: Some(0),
                            retry_duration_seconds_bits: None,
                            interval_seconds_bits: None,
                        }),
                    },
                    1_250,
                )
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 1_250,
                target_sample_ms: 2_500,
            }
        );
        assert_eq!(session.maximum_frames, 40_000);
        assert_eq!(session.next_signature_target_frames, Some(40_000));
        assert!(session.take_post_stop_retry_job().is_none());
        session.push_i16_at(&pcm(12_000), 1_500).unwrap();
        let followup = session.poll(1_500).unwrap().unwrap();
        assert_eq!(followup.attempt, 2);
        assert_eq!(followup.sample_ms, 2_500);
    }

    #[test]
    fn manual_type1_retry_duration_is_absolute_from_generator_start() {
        let mut configured = timing(12_000, 3);
        configured.initial_sample_ms = 3_000;
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();

        session.push_i16_at(&pcm(48_000), 3_000).unwrap();
        let first = session.poll(3_000).unwrap().unwrap();
        assert_eq!(first.sample_ms, 3_000);
        session.push_i16_at(&pcm(4_800), 3_300).unwrap();
        assert_eq!(session.frames_seen(), 52_800);

        // The service returned retryDuration=5s while the active generator
        // had written 3.3s. Native converts that duration to 5s worth of
        // frames and assigns it directly to totalFramesToRecord; it is not
        // added to the 3.3s already written. Since the empty-tag response's
        // accepted Retry returns early from Native's matched callback, this
        // Manual context remains active for the next Signature.
        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::NoMatch {
                        retry: Some(RetryDirective {
                            retry_duration_ms: Some(5_000),
                            interval_ms: Some(0),
                            retry_duration_seconds_bits: None,
                            interval_seconds_bits: None,
                        }),
                    },
                    3_307,
                )
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 3_307,
                target_sample_ms: 5_000,
            }
        );
        assert_eq!(session.native_total_frames_to_record, 80_000);
        assert_eq!(session.maximum_frames, 192_000);
        assert_eq!(session.next_signature_target_frames, Some(80_000));
        assert!(session.poll(5_000).unwrap().is_none());
        assert!(session.push_i16(&[0]).unwrap().accepted_frames > 0);
    }

    #[test]
    fn manual_no_match_retry_after_generator_stop_keeps_context_for_followup() {
        let mut configured = timing(3_000, 3);
        configured.initial_sample_ms = 1_000;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples = pcm(48_001);
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        session.push_i16(&samples[16_000..48_000]).unwrap();
        assert!(session.poll(0).unwrap().is_none());
        session.push_i16(&samples[48_000..]).unwrap();
        assert!(!session.generator_running);
        assert_eq!(session.frames_seen(), 48_000);

        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::NoMatch {
                        retry: Some(RetryDirective {
                            retry_duration_ms: Some(2_500),
                            // Manual updateWithRetry uses retryDuration for
                            // the generator target; intervalDuration is not a
                            // wait on this stopped-generator branch.
                            interval_ms: Some(1_500),
                            retry_duration_seconds_bits: None,
                            interval_seconds_bits: None,
                        }),
                    },
                    4_000,
                )
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 4_000,
                target_sample_ms: 3_000,
            }
        );

        let followup = session
            .poll(4_000)
            .unwrap()
            .expect("accepted Retry exposes the retained stopped-generator Signature");
        assert_eq!(followup.attempt, 2);
        assert_eq!(followup.sample_ms, 3_000);
        assert_eq!(followup.generated_at_ms, 4_000);
        assert!(session.take_post_stop_retry_job().is_none());
        assert_eq!(session.signature_timeline.all_attempts.len(), 1);
        assert_eq!(
            session.signature_timeline.all_attempts[0][0].bytes,
            first.signature
        );
        assert_eq!(session.signature_timeline.current_attempt.len(), 1);
        assert_eq!(
            session.signature_timeline.pick_last_of_type(0),
            Some(followup.signature.as_slice())
        );
        assert!(session.poll(4_001).unwrap().is_none());

        let mut reference = SigX::new(SigXConfig {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: 48_000,
        })
        .unwrap();
        reference.push_i16(&samples[..48_000]).unwrap();
        let expected = reference
            .take_type2_progressive_signature(
                native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed),
                native_signature_sample_length_samples(16_000, 48_000).unwrap(),
            )
            .unwrap();
        assert_eq!(followup.signature, expected);
    }

    #[test]
    fn manual_no_match_retry_too_long_keeps_generator_stopped_but_context_open() {
        let mut configured = timing(3_000, 3);
        configured.initial_sample_ms = 1_000;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples = pcm(48_000);
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        session.push_i16(&samples[16_000..]).unwrap();
        assert!(session.poll(0).unwrap().is_none());
        assert!(!session.generator_running);

        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::NoMatch {
                        retry: Some(RetryDirective {
                            retry_duration_ms: Some(3_500),
                            interval_ms: Some(0),
                            retry_duration_seconds_bits: None,
                            interval_seconds_bits: None,
                        }),
                    },
                    4_000,
                )
                .unwrap(),
            SessionStatus::GeneratorStopped
        );
        assert_eq!(session.lifecycle_status, SessionStatus::GeneratorStopped);
        assert!(session.take_post_stop_retry_job().is_none());
        assert!(session.poll(4_001).unwrap().is_none());
    }

    #[test]
    fn first_manual_signature_includes_seeded_ring_audio() {
        let mut configured = timing(6_875, 3);
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session = ProgressiveSession::new_for_type_with_pre_record_capacity_frames(
            configured,
            NativeTaggingType::Manual,
            32_000,
        )
        .unwrap();
        let pre_recorded = pcm(8_000);
        let live = pcm(20_000);

        assert_eq!(
            session.prime_pre_recorded_i16(&pre_recorded).unwrap(),
            8_000
        );
        assert_eq!(session.frames_seen(), 8_000);
        assert_eq!(session.sample_ms(), 500);
        assert!(session.poll(0).unwrap().is_none());
        session.push_i16_at(&live, 0).unwrap();
        let job = session.poll(1).unwrap().unwrap();

        let mut expected_audio = pre_recorded;
        expected_audio.extend_from_slice(&live);
        assert_eq!(job.sample_ms, 1_750);
        assert_eq!(job.signature, signature_for_prefix(&expected_audio));
    }

    #[test]
    fn seeded_ring_can_reach_the_initial_signature_threshold_before_live_pcm() {
        let configured = timing(6_875, 3);
        let mut session = ProgressiveSession::new_for_type_with_pre_record_capacity_frames(
            configured,
            NativeTaggingType::Manual,
            32_000,
        )
        .unwrap();
        let pre_recorded = pcm(28_000);

        assert_eq!(
            session.prime_pre_recorded_i16(&pre_recorded).unwrap(),
            28_000
        );
        let job = session
            .poll(0)
            .unwrap()
            .expect("buffered PCM already satisfies the initial threshold");
        assert_eq!(job.sample_ms, 1_750);
        assert_eq!(job.signature, signature_for_prefix(&pre_recorded));
    }

    #[test]
    fn float32_seeded_ring_can_reach_initial_threshold_before_live_pcm() {
        let mut configured = timing(6_875, 3);
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let pre_recorded: Vec<f32> = pcm(28_000)
            .into_iter()
            .map(|sample| f32::from(sample) / 32_768.0)
            .collect();
        let mut session = ProgressiveSession::new_for_type_with_pre_record_capacity_frames(
            configured,
            NativeTaggingType::Manual,
            32_000,
        )
        .unwrap();
        let seeded_frames = session.prime_pre_recorded_f32(&pre_recorded).unwrap();
        assert_eq!(seeded_frames, 28_000);

        let job = session
            .poll(0)
            .unwrap()
            .expect("Float32 buffered PCM already satisfies the initial threshold");
        assert_eq!(job.sample_ms, 1_750);

        let mut reference = SigX::new(SigXConfig {
            sample_rate: configured.sample_rate,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: native_frames_for_ms(configured.sample_rate, configured.maximum_sample_ms)
                .unwrap(),
        })
        .unwrap();
        reference.push_f32(&pre_recorded).unwrap();
        let expected = reference
            .take_type2_progressive_signature(
                native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed),
                native_signature_sample_length_samples(configured.sample_rate, pre_recorded.len())
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(job.signature, expected);
    }

    #[test]
    fn manual_pre_record_seed_is_rejected_after_pcm_or_for_auto_mode() {
        let configured = timing(6_875, 3);
        let mut manual = ProgressiveSession::new_for_type_with_pre_record_capacity_frames(
            configured,
            NativeTaggingType::Manual,
            16_000,
        )
        .unwrap();
        manual.push_i16_at(&pcm(1), 1).unwrap();
        assert!(matches!(
            manual.prime_pre_recorded_i16(&pcm(1)),
            Err(EngineError::InvalidTiming(_))
        ));

        let mut auto = ProgressiveSession::new_for_type_with_pre_record_capacity_frames(
            configured,
            NativeTaggingType::Auto,
            16_000,
        )
        .unwrap();
        assert!(matches!(
            auto.prime_pre_recorded_i16(&pcm(1)),
            Err(EngineError::InvalidTiming(_))
        ));
    }

    #[test]
    fn rolling_pcm_buffer_keeps_exact_tail_across_packet_boundaries() {
        let mut buffer = RollingPcmBuffer::new(5);
        buffer.push(&[0, 1, 2]);
        buffer.push(&[3, 4, 5, 6]);
        assert_eq!(buffer.frames(), 5);
        assert_eq!(buffer.into_samples(), [2, 3, 4, 5, 6]);

        let exact = [
            f32::from_bits(0x3f80_0001),
            f32::from_bits(0x8000_0000),
            f32::from_bits(0x3f00_0001),
        ];
        let mut float_buffer = RollingPcmBuffer::new(2);
        float_buffer.push(&exact);
        let retained = float_buffer.into_samples();
        assert_eq!(retained.len(), 2);
        assert_eq!(retained[0].to_bits(), exact[1].to_bits());
        assert_eq!(retained[1].to_bits(), exact[2].to_bits());
    }

    #[test]
    fn rolling_pcm_buffer_shrinks_to_native_capacity_and_zero_releases_audio() {
        let mut buffer = RollingPcmBuffer::new(8);
        buffer.push(&[0, 1, 2, 3, 4]);
        buffer.push(&[5, 6, 7, 8, 9]);
        buffer.set_capacity_frames(4);
        assert_eq!(buffer.frames(), 4);
        assert_eq!(buffer.into_samples(), [6, 7, 8, 9]);

        let float_bits = [
            0x3f80_0001,
            0x8000_0000,
            0x3f00_0001,
            0xbf00_0001,
            0x3e80_0001,
            0x3f40_0001,
            0x3d80_0001,
            0xbf40_0001,
            0x0000_0001,
            0x7f7f_ffff,
        ];
        let float_samples: Vec<f32> = float_bits.into_iter().map(f32::from_bits).collect();
        let mut float_buffer = RollingPcmBuffer::new(8);
        float_buffer.push(&float_samples[..5]);
        float_buffer.push(&float_samples[5..]);
        float_buffer.set_capacity_frames(4);
        let retained = float_buffer.into_samples();
        assert_eq!(
            retained
                .iter()
                .map(|sample| sample.to_bits())
                .collect::<Vec<_>>(),
            float_bits[6..]
        );

        let mut zero_capacity = RollingPcmBuffer::new(8);
        zero_capacity.push(&[10, 11, 12]);
        zero_capacity.set_capacity_frames(0);
        assert_eq!(zero_capacity.frames(), 0);
        assert!(zero_capacity.into_samples().is_empty());
    }

    #[test]
    fn pre_record_history_preserves_float_bits_and_int16_chunk_boundaries() {
        let float_samples = [
            f32::from_bits(0x3f80_0001),
            f32::from_bits(0x8000_0000),
            f32::from_bits(0x3f00_0001),
            f32::from_bits(0x3e80_0001),
        ];
        let mut history = PcmHistory::new(3);
        history.append_f32(&float_samples);
        history.append_i16(&[7, 8]);

        assert_eq!(history.frames(), 3);
        assert!(matches!(
            history.chunks.front(),
            Some(PcmHistoryChunk::Float32(samples))
                if samples.len() == 1 && samples[0].to_bits() == float_samples[3].to_bits()
        ));
        assert!(matches!(
            history.chunks.get(1),
            Some(PcmHistoryChunk::Int16(samples)) if samples == &[7, 8]
        ));
    }

    #[test]
    fn auto_tagging_ignores_pre_record_capacity_because_native_disables_it() {
        let session = ProgressiveSession::new_for_type_with_pre_record_capacity_frames(
            timing(6_875, 3),
            NativeTaggingType::Auto,
            16_000,
        )
        .unwrap();
        assert_eq!(session.pre_recorded_frames(), 0);
    }

    #[test]
    fn manual_type1_transport_failure_without_retry_is_terminal() {
        let mut configured = timing(6_875, 3);
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let first = session.poll(1_000).unwrap().unwrap();

        assert_eq!(
            session
                .complete(first.attempt, RecognitionOutcome::TransportFailure, 1_250)
                .unwrap(),
            SessionStatus::Failed(StopReason::TransportFailure)
        );
    }

    #[test]
    fn server_retry_interval_applies_to_the_following_type2_cycle() {
        let mut session = ProgressiveSession::new(timing(9_000, 3)).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let first = session.poll(1_000).unwrap().unwrap();

        session
            .complete(
                first.attempt,
                RecognitionOutcome::NoMatch {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(6_000),
                        interval_ms: None,
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                1_100,
            )
            .unwrap();

        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 9_000,
            }
        );
        assert_eq!(session.pending_restart_times_ms, vec![12_000]);
        assert!(session.poll(11_999).unwrap().is_none());
        assert!(session.poll(12_000).unwrap().is_none());
        // Missing interval is retained as zero for the cycle after this one.
        assert_eq!(session.status(), SessionStatus::Capturing);
    }

    #[test]
    fn type2_retry_interval_does_not_retime_the_already_scheduled_cycle() {
        let mut session = ProgressiveSession::new(timing(9_000, 4)).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        assert_eq!(session.pending_restart_times_ms, [12_000]);
        session.push_i16_at(&pcm(16), 1_100).unwrap();
        assert_eq!(session.frames_seen(), 28_016);

        session
            .complete(
                first.attempt,
                RecognitionOutcome::Match {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(5_000),
                        interval_ms: Some(0),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                1_100,
            )
            .unwrap();
        // Native has already scheduled this cycle using the default. The
        // arriving Retry interval is retained for the next generated Signature.
        assert_eq!(session.pending_restart_times_ms, [12_000]);
        assert!(session.poll(11_999).unwrap().is_none());
        assert!(session.poll(12_000).unwrap().is_none());
        assert_eq!(session.frames_seen(), 0);

        session.push_i16_at(&pcm(144_000), 12_000).unwrap();
        let second = session.poll(12_000).unwrap().unwrap();
        assert_eq!(second.attempt, 2);
        assert!(session.pending_restart_times_ms.is_empty());
        assert!(session.poll(12_000).unwrap().is_none());
        assert_eq!(session.frames_seen(), 0);
    }

    #[test]
    fn retry_above_configured_max_generates_at_retry_target_then_stops() {
        let mut configured = timing(2_000, 3);
        configured.maximum_retry_duration_ms = Some(2_000);
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        session
            .complete(
                first.attempt,
                RecognitionOutcome::MatcherFailure {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(2_500),
                        interval_ms: Some(0),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                10,
            )
            .unwrap();
        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 10,
                target_sample_ms: 2_500,
            }
        );
        assert_eq!(session.maximum_frames, 40_000);
        assert_eq!(
            session.push_i16(&pcm(12_000)).unwrap().accepted_frames,
            12_000
        );
        let retry_job = session.poll(10).unwrap().unwrap();
        assert_eq!(retry_job.sample_ms, 2_500);
        assert!(!session.generator_running);
    }

    #[test]
    fn retry_above_configured_max_keeps_a_stopped_generator_stopped() {
        let mut configured = timing(2_000, 3);
        configured.initial_sample_ms = 2_000;
        configured.maximum_retry_duration_ms = Some(2_000);
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        session.push_i16(&pcm(32_000)).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        assert_eq!(first.sample_ms, 2_000);
        assert!(!session.generator_running);

        session
            .complete(
                first.attempt,
                RecognitionOutcome::MatcherFailure {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(2_500),
                        interval_ms: Some(0),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                10,
            )
            .unwrap();
        assert_eq!(session.status(), SessionStatus::GeneratorStopped);
        assert_eq!(session.maximum_frames, 32_000);
    }

    #[test]
    fn match_keeps_tagging_until_the_local_attempt_safety_bound() {
        let mut session = ProgressiveSession::new(timing(6_875, 2)).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let job = session.poll(0).unwrap().unwrap();
        assert_eq!(
            session
                .complete(job.attempt, RecognitionOutcome::Match { retry: None }, 1,)
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 6_875,
            }
        );
        assert!(session.poll(11_999).unwrap().is_none());
        session.push_i16_at(&pcm(110_000), 12_000).unwrap();
        let second = session.poll(12_001).unwrap().unwrap();
        assert_eq!(second.attempt, 2);
        assert_eq!(second.sample_ms, 6_875);
        assert_eq!(
            session
                .complete(
                    second.attempt,
                    RecognitionOutcome::Match { retry: None },
                    12_003,
                )
                .unwrap(),
            SessionStatus::Succeeded
        );
        assert_eq!(session.push_i16(&[0]).unwrap_err(), EngineError::Closed);
    }

    #[test]
    fn type2_match_retry_uses_interval_then_fresh_sample_seconds_cycle() {
        let mut session = ProgressiveSession::new(timing(6_000, 3)).unwrap();
        let first_cycle = pcm(28_000);
        let second_cycle = pcm(96_000);
        session.push_i16(&first_cycle).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        assert_eq!(first.sample_ms, 1_750);

        session
            .complete(
                first.attempt,
                RecognitionOutcome::Match {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(2_500),
                        interval_ms: Some(5_000),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                10,
            )
            .unwrap();
        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 6_000,
            }
        );
        assert!(session.poll(11_999).unwrap().is_none());
        assert_eq!(session.pending_restart_times_ms, vec![12_000]);

        session.push_i16_at(&second_cycle, 12_000).unwrap();
        let retry_job = session.poll(12_000).unwrap().unwrap();
        assert_eq!(retry_job.attempt, 2);
        assert_eq!(retry_job.sample_ms, 6_000);
        assert_eq!(retry_job.signature, signature_for_prefix(&second_cycle));
        assert_ne!(retry_job.signature, first.signature);
        assert_eq!(session.pending_restart_times_ms, [17_000]);
        assert_eq!(
            session
                .complete(
                    retry_job.attempt,
                    RecognitionOutcome::Match { retry: None },
                    12_020,
                )
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 17_000,
                target_sample_ms: 6_000,
            }
        );
        assert!(session.poll(16_999).unwrap().is_none());

        let third_cycle = pcm(96_000);
        session.push_i16_at(&third_cycle, 17_000).unwrap();
        let third = session.poll(17_000).unwrap().unwrap();
        assert_eq!(third.attempt, 3);
        assert_eq!(third.sample_ms, 6_000);
        assert_eq!(third.signature, signature_for_prefix(&third_cycle));
        assert!(session.pending_restart_times_ms.is_empty());
        assert_eq!(
            session
                .complete(
                    third.attempt,
                    RecognitionOutcome::Match { retry: None },
                    17_020,
                )
                .unwrap(),
            SessionStatus::Succeeded
        );
    }

    #[test]
    fn matcher_failure_retry_is_distinct_from_transport_failure() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 250,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS,
                maximum_retry_duration_ms: Some(3_000),
                session_timeout_ms: None,
                request_timeout_ms: 1_000,
                maximum_retry_delay_ms: Some(12_000),
                maximum_attempts: Some(3),
            },
            NativeTaggingType::Manual,
        )
        .unwrap();
        let samples = pcm(32_001);
        let mut reference = SigX::new(SigXConfig {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: 32_001,
        })
        .unwrap();
        session.push_i16(&samples[..16_000]).unwrap();
        reference.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        let first_stream_pos =
            native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed);
        reference
            .take_type2_progressive_signature(
                first_stream_pos,
                native_signature_sample_length_samples(16_000, 16_000).unwrap(),
            )
            .unwrap();
        reference.push_i16(&samples[16_000..32_000]).unwrap();
        session.push_i16(&samples[16_000..32_000]).unwrap();

        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::MatcherFailure {
                        retry: Some(RetryDirective {
                            retry_duration_ms: Some(2_000),
                            interval_ms: Some(500),
                            retry_duration_seconds_bits: None,
                            interval_seconds_bits: None,
                        }),
                    },
                    100,
                )
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 100,
                target_sample_ms: 2_000,
            }
        );
        assert!(session.poll(100).unwrap().is_none());
        reference.push_i16(&samples[32_000..]).unwrap();
        session.push_i16(&samples[32_000..]).unwrap();
        let retry = session.poll(100).unwrap().unwrap();
        assert_eq!(retry.sample_ms, 2_000);
        let stream_pos = native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed);
        let expected = reference
            .take_type2_progressive_signature(
                stream_pos,
                native_signature_sample_length_samples(16_000, 32_001).unwrap(),
            )
            .unwrap();
        assert_eq!(retry.signature, expected);
    }

    #[test]
    fn type2_matcher_failure_without_retry_keeps_session_until_native_maximum() {
        let mut session = ProgressiveSession::new(RecognitionTiming {
            sample_rate: 16_000,
            minimum_signature_ms: 250,
            initial_sample_ms: 1_000,
            maximum_sample_ms: 3_000,
            default_retry_interval_ms: NATIVE_TAGGING_TYPE_2_DEFAULT_INTERVAL_MS,
            maximum_retry_duration_ms: Some(3_000),
            session_timeout_ms: None,
            request_timeout_ms: 1_000,
            maximum_retry_delay_ms: None,
            maximum_attempts: None,
        })
        .unwrap();
        session.push_i16(&pcm(16_000)).unwrap();
        let first = session.poll(0).unwrap().unwrap();

        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::MatcherFailure { retry: None },
                    100,
                )
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 3_000,
            }
        );
        session.push_i16_at(&pcm(32_000), 200).unwrap();
        assert_eq!(session.frames_seen(), 48_000);
        assert!(session.poll(12_100).unwrap().is_none());
        assert_eq!(session.status(), SessionStatus::Capturing);
        assert!(session.generator_running);

        session.finish_input();
        assert_eq!(session.status(), SessionStatus::Exhausted);
    }

    #[test]
    fn type2_matcher_failure_retry_does_not_update_or_restart_generator() {
        let mut session = ProgressiveSession::new(timing(3_000, 3)).unwrap();
        let first_audio = pcm(28_000);
        let to_maximum = pcm(20_000);
        session.push_i16(&first_audio).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        assert_eq!(first.sample_ms, 1_750);
        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::MatcherFailure {
                        retry: Some(RetryDirective {
                            retry_duration_ms: Some(2_500),
                            interval_ms: Some(5_000),
                            retry_duration_seconds_bits: None,
                            interval_seconds_bits: None,
                        }),
                    },
                    100,
                )
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 3_000,
            }
        );
        assert_eq!(session.pending_restart_times_ms, [12_000]);
        assert_eq!(session.next_signature_target_frames, None);
        assert_eq!(session.signature_timeline.all_attempts.len(), 1);
        assert!(session.signature_timeline.current_attempt.is_empty());

        session.push_i16_at(&to_maximum, 600).unwrap();
        assert_eq!(session.frames_seen(), 48_000);
        assert!(!session.generator_running);
        assert_eq!(session.status(), SessionStatus::GeneratorStopped);
        assert!(session.poll(600).unwrap().is_none());
        assert_eq!(session.attempt_count, 1);
    }

    #[test]
    fn stopped_generator_matcher_retry_emits_immediately_when_audio_is_sufficient() {
        let mut configured = timing(3_000, 3);
        configured.initial_sample_ms = 1_000;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples = pcm(48_001);
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        session.push_i16(&samples[16_000..48_000]).unwrap();
        assert!(session.poll(0).unwrap().is_none());
        session.push_i16(&samples[48_000..]).unwrap();
        assert!(!session.generator_running);
        assert_eq!(session.frames_seen(), 48_000);

        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::MatcherFailure {
                        retry: Some(RetryDirective {
                            retry_duration_ms: Some(2_500),
                            interval_ms: Some(12_000),
                            retry_duration_seconds_bits: None,
                            interval_seconds_bits: None,
                        }),
                    },
                    4_000,
                )
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 4_000,
                target_sample_ms: 3_000,
            }
        );
        let retry = session
            .poll(4_000)
            .unwrap()
            .expect("Native's stopped-generator path produces immediately");
        assert_eq!(retry.attempt, 2);
        assert_eq!(retry.sample_ms, 3_000);
        assert_eq!(retry.generated_at_ms, 4_000);
        let mut reference = SigX::new(SigXConfig {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: 48_000,
        })
        .unwrap();
        reference.push_i16(&samples[..48_000]).unwrap();
        let expected = reference
            .take_type2_progressive_signature(
                native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed),
                native_signature_sample_length_samples(16_000, 48_000).unwrap(),
            )
            .unwrap();
        assert_eq!(retry.signature, expected);
    }

    #[test]
    fn stopped_generator_matcher_retry_stays_stopped_when_audio_is_short() {
        let mut configured = timing(3_000, 3);
        configured.initial_sample_ms = 1_000;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples = pcm(48_000);
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        session.push_i16(&samples[16_000..]).unwrap();
        assert!(session.poll(0).unwrap().is_none());
        assert!(!session.generator_running);

        assert_eq!(
            session
                .complete(
                    first.attempt,
                    RecognitionOutcome::MatcherFailure {
                        retry: Some(RetryDirective {
                            retry_duration_ms: Some(3_500),
                            interval_ms: Some(0),
                            retry_duration_seconds_bits: None,
                            interval_seconds_bits: None,
                        }),
                    },
                    4_000,
                )
                .unwrap(),
            SessionStatus::GeneratorStopped
        );
        assert!(session.poll(4_001).unwrap().is_none());
        assert!(session.next_signature_target_frames.is_none());
    }

    #[test]
    fn end_of_input_finishes_without_waiting_for_an_audio_restart() {
        for (outcome, expected) in [
            (
                RecognitionOutcome::Match { retry: None },
                SessionStatus::Succeeded,
            ),
            (
                RecognitionOutcome::NoMatch { retry: None },
                SessionStatus::Exhausted,
            ),
        ] {
            let mut session = ProgressiveSession::new(timing(6_875, 3)).unwrap();
            session.push_i16(&pcm(28_000)).unwrap();
            let job = session.poll(0).unwrap().unwrap();
            session.finish_input();
            assert_eq!(
                session.complete(job.attempt, outcome, 10).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn manual_matcher_retry_can_use_pcm_buffered_before_end_of_input() {
        let mut configured = timing(6_875, 3);
        configured.default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        let samples = pcm(40_000);
        let mut reference = SigX::new(SigXConfig {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: 40_000,
        })
        .unwrap();
        session.push_i16(&samples[..28_000]).unwrap();
        reference.push_i16(&samples[..28_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        let first_stream_pos =
            native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed);
        reference
            .take_type2_progressive_signature(
                first_stream_pos,
                native_signature_sample_length_samples(16_000, 28_000).unwrap(),
            )
            .unwrap();
        reference.push_i16(&samples[28_000..]).unwrap();
        session.push_i16(&samples[28_000..]).unwrap();
        session.finish_input();

        session
            .complete(
                first.attempt,
                RecognitionOutcome::MatcherFailure {
                    retry: Some(RetryDirective {
                        retry_duration_ms: Some(2_500),
                        interval_ms: Some(500),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                10,
            )
            .unwrap();
        let retry = session.poll(10).unwrap().unwrap();
        assert_eq!(retry.sample_ms, 2_500);
        assert_eq!(retry.generated_at_ms, 10);
        assert_eq!(session.signature_timeline.all_attempts.len(), 1);
        assert_eq!(
            session.signature_timeline.all_attempts[0][0].bytes,
            first.signature
        );
        assert_eq!(session.signature_timeline.current_attempt.len(), 1);
        assert_eq!(
            session.signature_timeline.pick_last_of_type(0),
            Some(retry.signature.as_slice())
        );
        let stream_pos = native_sig_stream_now_ticks(reference.snapshot().fft_frames_processed);
        let expected = reference
            .take_type2_progressive_signature(
                stream_pos,
                native_signature_sample_length_samples(16_000, 40_000).unwrap(),
            )
            .unwrap();
        assert_eq!(retry.signature, expected);

        assert_eq!(
            session
                .complete(retry.attempt, RecognitionOutcome::Match { retry: None }, 20)
                .unwrap(),
            SessionStatus::Succeeded
        );
    }

    #[test]
    fn retry_limit_exhaustion_prevents_unbounded_capture_cycles() {
        let mut session = ProgressiveSession::new(timing(6_875, 2)).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        session
            .complete(
                first.attempt,
                RecognitionOutcome::NoMatch { retry: None },
                1,
            )
            .unwrap();
        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 6_875,
            }
        );
        assert_eq!(session.pending_restart_times_ms, [12_000]);
        assert!(session.poll(12_000).unwrap().is_none());
        session.push_i16_at(&pcm(110_000), 12_001).unwrap();
        let second = session.poll(12_002).unwrap().unwrap();
        assert_eq!(second.sample_ms, 6_875);
        assert_eq!(
            session
                .complete(
                    second.attempt,
                    RecognitionOutcome::NoMatch { retry: None },
                    12_003
                )
                .unwrap(),
            SessionStatus::Exhausted
        );
        assert!(session.poll(20_000).unwrap().is_none());
    }

    #[test]
    fn local_transport_timeout_does_not_cancel_native_type2_session() {
        let mut configured = timing(6_875, 3);
        configured.request_timeout_ms = 1_000;
        let mut session = ProgressiveSession::new(configured).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let job = session.poll(5).unwrap().unwrap();
        assert!(session.poll(1_005).unwrap().is_none());
        assert_eq!(session.status(), SessionStatus::InFlight { attempt: 1 });
        assert_eq!(
            session
                .complete(job.attempt, RecognitionOutcome::TransportFailure, 1_006,)
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 6_875,
            }
        );
    }

    #[test]
    fn native_type2_errors_without_retry_keep_live_session_until_eof() {
        for outcome in [
            RecognitionOutcome::MatcherFailure { retry: None },
            RecognitionOutcome::TransportFailure,
            RecognitionOutcome::InvalidResponse,
        ] {
            let mut session = ProgressiveSession::new(timing(6_875, 3)).unwrap();
            session.push_i16(&pcm(28_000)).unwrap();
            let job = session.poll(0).unwrap().unwrap();
            assert_eq!(
                session.complete(job.attempt, outcome, 1).unwrap(),
                SessionStatus::WaitingForRetry {
                    not_before_ms: 12_000,
                    target_sample_ms: 6_875,
                }
            );
            assert!(session.poll(2).unwrap().is_none());
            session.finish_input();
            assert_eq!(session.status(), SessionStatus::Exhausted);
        }
    }

    #[test]
    fn native_type2_errors_without_retry_do_not_end_a_live_capture() {
        for outcome in [
            RecognitionOutcome::MatcherFailure { retry: None },
            RecognitionOutcome::TransportFailure,
            RecognitionOutcome::InvalidResponse,
        ] {
            let mut session = ProgressiveSession::new(timing(6_875, 3)).unwrap();
            session.push_i16(&pcm(28_000)).unwrap();
            let job = session.poll(0).unwrap().unwrap();
            assert_eq!(
                session.complete(job.attempt, outcome, 1).unwrap(),
                SessionStatus::WaitingForRetry {
                    not_before_ms: 12_000,
                    target_sample_ms: 6_875,
                }
            );
            let report = session.push_i16_at(&pcm(128), 2).unwrap();
            assert_eq!(report.accepted_frames, 128);
            assert!(session.poll(2).unwrap().is_none());
            assert_eq!(
                session.status(),
                SessionStatus::WaitingForRetry {
                    not_before_ms: 12_000,
                    target_sample_ms: 6_875,
                }
            );
        }
    }

    #[test]
    fn native_session_timeout_ends_in_flight_and_ignores_late_response() {
        let mut configured = timing(6_875, 3);
        configured.session_timeout_ms = Some(150);
        let mut session = ProgressiveSession::new(configured).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let job = session.poll(100).unwrap().unwrap();
        assert_eq!(session.status(), SessionStatus::InFlight { attempt: 1 });

        assert!(session.poll(150).unwrap().is_none());
        assert_eq!(
            session.status(),
            SessionStatus::Failed(StopReason::SessionTimeout)
        );
        assert_eq!(
            session
                .complete(job.attempt, RecognitionOutcome::Match { retry: None }, 151,)
                .unwrap(),
            SessionStatus::Failed(StopReason::SessionTimeout)
        );
        assert_eq!(session.push_i16(&[0]).unwrap_err(), EngineError::Closed);
    }

    #[test]
    fn manual_type1_session_timeout_ends_in_flight_and_ignores_late_response() {
        let mut configured = timing(6_875, 3);
        configured.session_timeout_ms = Some(150);
        let mut session =
            ProgressiveSession::new_for_type(configured, NativeTaggingType::Manual).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let job = session.poll(100).unwrap().unwrap();

        assert!(session.poll(150).unwrap().is_none());
        assert_eq!(
            session.status(),
            SessionStatus::Failed(StopReason::SessionTimeout)
        );
        assert_eq!(
            session
                .complete(job.attempt, RecognitionOutcome::Match { retry: None }, 151,)
                .unwrap(),
            SessionStatus::Failed(StopReason::SessionTimeout)
        );
    }

    #[test]
    fn native_session_timeout_precedes_a_scheduled_retry_restart() {
        let mut configured = timing(6_875, 3);
        configured.session_timeout_ms = Some(200);
        let mut session = ProgressiveSession::new(configured).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let job = session.poll(10).unwrap().unwrap();
        session
            .complete(
                job.attempt,
                RecognitionOutcome::NoMatch {
                    retry: Some(RetryDirective {
                        retry_duration_ms: None,
                        interval_ms: Some(100),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                20,
            )
            .unwrap();
        assert!(matches!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                ..
            }
        ));

        assert!(session.poll(200).unwrap().is_none());
        assert_eq!(
            session.status(),
            SessionStatus::Failed(StopReason::SessionTimeout)
        );
    }

    #[test]
    fn unset_local_caps_preserve_native_retry_interval_and_session_deadline() {
        let mut configured = timing(6_875, 3);
        configured.maximum_attempts = None;
        configured.maximum_retry_delay_ms = None;
        configured.session_timeout_ms = Some(100_000);
        let mut session = ProgressiveSession::new(configured).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let first = session.poll(10).unwrap().unwrap();
        session
            .complete(
                first.attempt,
                RecognitionOutcome::NoMatch {
                    retry: Some(RetryDirective {
                        retry_duration_ms: None,
                        interval_ms: Some(60_000),
                        retry_duration_seconds_bits: None,
                        interval_seconds_bits: None,
                    }),
                },
                20,
            )
            .unwrap();
        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 6_875,
            }
        );
        assert!(session.poll(12_010).unwrap().is_none());
        assert_eq!(session.status(), SessionStatus::Capturing);
    }

    #[test]
    fn cancellation_invalidates_in_flight_and_waiting_work() {
        let mut session = ProgressiveSession::new(timing(6_875, 3)).unwrap();
        session.push_i16(&pcm(28_000)).unwrap();
        let job = session.poll(0).unwrap().unwrap();
        assert_eq!(session.cancel(), Some(job.attempt));
        assert_eq!(session.status(), SessionStatus::Cancelled);
        assert_eq!(session.push_i16(&[0]).unwrap_err(), EngineError::Closed);
    }

    #[test]
    fn chunk_partition_does_not_change_signature_at_same_audio_length() {
        let samples = pcm(28_000);
        let mut one = ProgressiveSession::new(timing(6_875, 3)).unwrap();
        one.push_i16(&samples).unwrap();
        let one_signature = one.poll(0).unwrap().unwrap().signature;

        let mut chunked = ProgressiveSession::new(timing(6_875, 3)).unwrap();
        for chunk in samples.chunks(997) {
            chunked.push_i16(chunk).unwrap();
        }
        let chunked_signature = chunked.poll(0).unwrap().unwrap().signature;
        assert_eq!(one_signature, chunked_signature);
    }

    #[test]
    fn validates_duration_order_and_clock_monotonicity() {
        assert!(ProgressiveSession::new(RecognitionTiming {
            minimum_signature_ms: 10_000,
            ..timing(6_875, 3)
        })
        .is_err());

        let mut session = ProgressiveSession::new(timing(6_875, 3)).unwrap();
        assert!(session.poll(10).unwrap().is_none());
        assert_eq!(
            session.poll(9),
            Err(EngineError::ClockMovedBackwards {
                previous_ms: 10,
                current_ms: 9,
            })
        );
    }
}
