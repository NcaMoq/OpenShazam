//! OS-independent PCM-to-Signature recognition control.
//!
//! Audio adapters feed mono PCM with [`ProgressiveSession::push_i16`] or
//! [`ProgressiveSession::push_f32`]. A caller polls for a [`RecognitionJob`]
//! and runs its HTTP request on a separate worker. While that request is in
//! flight, more PCM can continue to be pushed; at most one request is active.
//! No audio source, HTTP client, clock, or platform API is owned here.

use sigx_core::{Config as SigXConfig, SigX};
use sigx_format::Signature;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecognitionTiming {
    pub sample_rate: u32,
    /// Lower bound enforced by the native signature pipe before generation.
    pub minimum_signature_ms: u64,
    /// Initial cumulative audio target; this is not a wall-clock POST time.
    pub initial_sample_ms: u64,
    /// Cumulative audio increment requested for subsequent attempts.
    pub progressive_step_ms: u64,
    pub maximum_sample_ms: u64,
    /// Default delay between attempts when a response supplies no usable delay.
    pub default_retry_interval_ms: u64,
    /// Local safety bound for one outstanding request; not a Shazam value.
    pub request_timeout_ms: u64,
    /// Local safety bound for server-supplied retry delays.
    pub maximum_retry_delay_ms: u64,
    /// Local loop-prevention bound for recognition submissions.
    pub maximum_attempts: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryDirective {
    /// Optional wait before another recording/signature attempt.
    pub delay_ms: Option<u64>,
    /// Optional cumulative PCM target for the next Signature. The Mac
    /// generator assigns retryDuration * sampleRate to totalFramesToRecord.
    pub target_sample_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    RequestTimeout,
    TransportFailure,
    InvalidResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Capturing,
    InFlight {
        attempt: u8,
    },
    WaitingForRetry {
        not_before_ms: u64,
        target_sample_ms: u64,
    },
    Succeeded,
    Exhausted,
    Failed(StopReason),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecognitionJob {
    pub attempt: u8,
    /// Cumulative audio represented by this Signature, derived from input PCM.
    pub sample_ms: u64,
    /// Relative monotonic time supplied by the caller when the Signature was built.
    pub generated_at_ms: u64,
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
    StaleAttempt { expected: u8, received: u8 },
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
    Match,
    NoMatch { retry: Option<RetryDirective> },
    InvalidResponse,
    TransportFailure,
}

/// Stateful, platform-independent recognition control for one audio session.
pub struct ProgressiveSession {
    timing: RecognitionTiming,
    sigx: SigX,
    maximum_frames: usize,
    minimum_frames: usize,
    initial_frames: usize,
    progressive_step_frames: usize,
    frames_seen: usize,
    attempt_count: u8,
    last_submission_frames: usize,
    last_now_ms: u64,
    last_now_for_request_ms: u64,
    input_finished: bool,
    status: SessionStatus,
}

impl ProgressiveSession {
    pub fn new(timing: RecognitionTiming) -> Result<Self, EngineError> {
        validate_timing(timing)?;
        let maximum_frames = frames_for_ms(timing.sample_rate, timing.maximum_sample_ms)?;
        let minimum_frames = frames_for_ms(timing.sample_rate, timing.minimum_signature_ms)?;
        let initial_frames = frames_for_ms(timing.sample_rate, timing.initial_sample_ms)?;
        let progressive_step_frames =
            frames_for_ms(timing.sample_rate, timing.progressive_step_ms)?;
        let sigx = SigX::new(SigXConfig {
            sample_rate: timing.sample_rate,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: maximum_frames,
        })
        .map_err(|error| EngineError::SigX(format!("{error:?}")))?;

        Ok(Self {
            timing,
            sigx,
            maximum_frames,
            minimum_frames,
            initial_frames,
            progressive_step_frames,
            frames_seen: 0,
            attempt_count: 0,
            last_submission_frames: 0,
            last_now_ms: 0,
            last_now_for_request_ms: 0,
            input_finished: false,
            status: SessionStatus::Capturing,
        })
    }

    pub fn status(&self) -> SessionStatus {
        self.status
    }

    pub fn frames_seen(&self) -> usize {
        self.frames_seen
    }

    pub fn sample_ms(&self) -> u64 {
        duration_ms(self.timing.sample_rate, self.frames_seen)
    }

    /// Mark PCM end-of-stream. A pending request may still complete; a later
    /// retry that requires audio beyond the received prefix will be exhausted.
    pub fn finish_input(&mut self) {
        self.input_finished = true;
        match self.status {
            SessionStatus::Capturing
                if self.frames_seen < self.initial_frames.max(self.minimum_frames) =>
            {
                self.status = SessionStatus::Exhausted;
            }
            SessionStatus::WaitingForRetry {
                target_sample_ms, ..
            } if self.frames_seen
                < frames_for_ms(self.timing.sample_rate, target_sample_ms)
                    .unwrap_or(usize::MAX) =>
            {
                self.status = SessionStatus::Exhausted;
            }
            _ => {}
        }
    }

    /// Feed mono Int16 PCM. Input remains accepted while HTTP work is in flight.
    pub fn push_i16(&mut self, samples: &[i16]) -> Result<PushReport, EngineError> {
        self.ensure_accepting()?;
        let accepted = samples.len().min(self.maximum_frames - self.frames_seen);
        if accepted > 0 {
            self.sigx
                .push_i16(&samples[..accepted])
                .map_err(|error| EngineError::SigX(format!("{error:?}")))?;
            self.frames_seen += accepted;
        }
        Ok(self.push_report(accepted))
    }

    /// Feed mono normalized Float32 PCM. Input remains accepted while HTTP work
    /// is in flight. The SigX core owns its existing Float32 preprocessing.
    pub fn push_f32(&mut self, samples: &[f32]) -> Result<PushReport, EngineError> {
        self.ensure_accepting()?;
        let accepted = samples.len().min(self.maximum_frames - self.frames_seen);
        if accepted > 0 {
            self.sigx
                .push_f32(&samples[..accepted])
                .map_err(|error| EngineError::SigX(format!("{error:?}")))?;
            self.frames_seen += accepted;
        }
        Ok(self.push_report(accepted))
    }

    /// Generate a Signature when both the PCM threshold and retry wait permit.
    /// `now_ms` is a caller-provided monotonic time relative to session start;
    /// it is intentionally distinct from the cumulative sample duration.
    pub fn poll(&mut self, now_ms: u64) -> Result<Option<RecognitionJob>, EngineError> {
        self.observe_time(now_ms)?;

        match self.status {
            SessionStatus::InFlight { attempt } => {
                if now_ms.saturating_sub(self.last_now_for_request_ms)
                    >= self.timing.request_timeout_ms
                {
                    self.status = SessionStatus::Failed(StopReason::RequestTimeout);
                    return Ok(None);
                }
                let _ = attempt;
                Ok(None)
            }
            SessionStatus::WaitingForRetry {
                not_before_ms,
                target_sample_ms,
            } => {
                let target_frames = frames_for_ms(self.timing.sample_rate, target_sample_ms)?;
                if now_ms < not_before_ms || self.frames_seen < target_frames {
                    return Ok(None);
                }
                self.create_job(now_ms).map(Some)
            }
            SessionStatus::Capturing => {
                let first_target = self.minimum_frames.max(self.initial_frames);
                if self.frames_seen < first_target {
                    return Ok(None);
                }
                self.create_job(now_ms).map(Some)
            }
            SessionStatus::Succeeded
            | SessionStatus::Exhausted
            | SessionStatus::Failed(_)
            | SessionStatus::Cancelled => Ok(None),
        }
    }

    /// Apply the result for the currently in-flight attempt. Successful match,
    /// invalid response, and transport failure are terminal. No-match continues
    /// progressively until the configured sample/attempt bounds are reached.
    pub fn complete(
        &mut self,
        attempt: u8,
        outcome: RecognitionOutcome,
        now_ms: u64,
    ) -> Result<SessionStatus, EngineError> {
        self.observe_time(now_ms)?;
        let expected = match self.status {
            SessionStatus::InFlight { attempt } => attempt,
            _ => return Err(EngineError::NoRequestInFlight),
        };
        if expected != attempt {
            return Err(EngineError::StaleAttempt {
                expected,
                received: attempt,
            });
        }
        if now_ms.saturating_sub(self.last_now_for_request_ms) >= self.timing.request_timeout_ms {
            self.status = SessionStatus::Failed(StopReason::RequestTimeout);
            return Ok(self.status);
        }

        match outcome {
            RecognitionOutcome::Match => self.status = SessionStatus::Succeeded,
            RecognitionOutcome::InvalidResponse => {
                self.status = SessionStatus::Failed(StopReason::InvalidResponse)
            }
            RecognitionOutcome::TransportFailure => {
                self.status = SessionStatus::Failed(StopReason::TransportFailure)
            }
            RecognitionOutcome::NoMatch { retry } => self.schedule_retry(retry, now_ms)?,
        }
        Ok(self.status)
    }

    /// Stop accepting audio and invalidate any outstanding attempt. The caller
    /// can use the returned attempt number to cancel its transport task.
    pub fn cancel(&mut self) -> Option<u8> {
        let active = match self.status {
            SessionStatus::InFlight { attempt } => Some(attempt),
            _ => None,
        };
        if !matches!(
            self.status,
            SessionStatus::Succeeded | SessionStatus::Exhausted | SessionStatus::Failed(_)
        ) {
            self.status = SessionStatus::Cancelled;
        }
        active
    }

    fn create_job(&mut self, now_ms: u64) -> Result<RecognitionJob, EngineError> {
        if self.attempt_count >= self.timing.maximum_attempts {
            self.status = SessionStatus::Exhausted;
            return Err(EngineError::Closed);
        }
        let signature = self
            .sigx
            .signature()
            .map_err(|error| EngineError::SigX(format!("{error:?}")))?;
        Signature::parse(&signature)
            .map_err(|error| EngineError::InvalidSignature(error.to_string()))?;
        self.attempt_count += 1;
        self.last_submission_frames = self.frames_seen;
        self.last_now_for_request_ms = now_ms;
        self.status = SessionStatus::InFlight {
            attempt: self.attempt_count,
        };
        Ok(RecognitionJob {
            attempt: self.attempt_count,
            sample_ms: self.sample_ms(),
            generated_at_ms: now_ms,
            signature,
        })
    }

    fn schedule_retry(
        &mut self,
        retry: Option<RetryDirective>,
        now_ms: u64,
    ) -> Result<(), EngineError> {
        if self.attempt_count >= self.timing.maximum_attempts
            || (self.frames_seen >= self.maximum_frames
                && self.last_submission_frames >= self.maximum_frames)
        {
            self.status = SessionStatus::Exhausted;
            return Ok(());
        }

        let next_default = self
            .last_submission_frames
            .saturating_add(self.progressive_step_frames)
            .min(self.maximum_frames);
        let requested_delay = retry
            .and_then(|directive| directive.delay_ms)
            .filter(|delay| *delay <= self.timing.maximum_retry_delay_ms);
        let delay_ms = requested_delay.unwrap_or(self.timing.default_retry_interval_ms);
        let requested_target = retry
            .and_then(|directive| directive.target_sample_ms)
            .and_then(|target| frames_for_ms(self.timing.sample_rate, target).ok())
            .filter(|target| {
                *target > self.last_submission_frames && *target <= self.maximum_frames
            });
        let target_frames = requested_target
            .unwrap_or_else(|| next_default.max(self.frames_seen.min(self.maximum_frames)));

        if target_frames <= self.last_submission_frames
            || target_frames > self.maximum_frames
            || (self.input_finished && target_frames > self.frames_seen)
        {
            self.status = SessionStatus::Exhausted;
            return Ok(());
        }

        let not_before_ms = now_ms.saturating_add(delay_ms);
        self.status = SessionStatus::WaitingForRetry {
            not_before_ms,
            target_sample_ms: duration_ms(self.timing.sample_rate, target_frames),
        };
        Ok(())
    }

    fn observe_time(&mut self, now_ms: u64) -> Result<(), EngineError> {
        if now_ms < self.last_now_ms {
            return Err(EngineError::ClockMovedBackwards {
                previous_ms: self.last_now_ms,
                current_ms: now_ms,
            });
        }
        self.last_now_ms = now_ms;
        Ok(())
    }

    fn ensure_accepting(&self) -> Result<(), EngineError> {
        if self.input_finished
            || matches!(
                self.status,
                SessionStatus::Succeeded
                    | SessionStatus::Exhausted
                    | SessionStatus::Failed(_)
                    | SessionStatus::Cancelled
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
            maximum_reached: self.frames_seen == self.maximum_frames,
        }
    }
}

fn validate_timing(timing: RecognitionTiming) -> Result<(), EngineError> {
    if timing.sample_rate == 0 {
        return Err(EngineError::InvalidTiming("sample rate must be nonzero"));
    }
    if timing.minimum_signature_ms == 0
        || timing.initial_sample_ms == 0
        || timing.progressive_step_ms == 0
        || timing.maximum_sample_ms == 0
    {
        return Err(EngineError::InvalidTiming(
            "sample durations must be nonzero",
        ));
    }
    if timing.minimum_signature_ms > timing.maximum_sample_ms
        || timing.initial_sample_ms > timing.maximum_sample_ms
        || timing.progressive_step_ms > timing.maximum_sample_ms
    {
        return Err(EngineError::InvalidTiming(
            "minimum, initial, and step durations must each be within the maximum sample duration",
        ));
    }
    if timing.request_timeout_ms == 0 || timing.maximum_attempts == 0 {
        return Err(EngineError::InvalidTiming(
            "request timeout and maximum attempts must be nonzero",
        ));
    }
    if timing.default_retry_interval_ms > timing.maximum_retry_delay_ms {
        return Err(EngineError::InvalidTiming(
            "default retry interval exceeds the configured retry-delay safety bound",
        ));
    }
    for duration in [
        timing.minimum_signature_ms,
        timing.initial_sample_ms,
        timing.progressive_step_ms,
        timing.maximum_sample_ms,
    ] {
        let _ = frames_for_ms(timing.sample_rate, duration)?;
    }
    Ok(())
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

fn duration_ms(sample_rate: u32, frames: usize) -> u64 {
    (frames as u128 * 1_000 / u128::from(sample_rate)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(maximum_sample_ms: u64, maximum_attempts: u8) -> RecognitionTiming {
        RecognitionTiming {
            sample_rate: 16_000,
            minimum_signature_ms: 3_000,
            initial_sample_ms: 3_000,
            progressive_step_ms: 3_000,
            maximum_sample_ms,
            default_retry_interval_ms: 100,
            request_timeout_ms: 1_000,
            maximum_retry_delay_ms: 500,
            maximum_attempts,
        }
    }

    fn pcm(frames: usize) -> Vec<i16> {
        (0..frames)
            .map(|index| ((index * 73 % 30_001) as i32 - 15_000) as i16)
            .collect()
    }

    #[test]
    fn initial_threshold_is_cumulative_pcm_not_wall_clock() {
        let mut session = ProgressiveSession::new(timing(9_000, 3)).unwrap();
        let samples = pcm(48_000);
        session.push_i16(&samples[..47_999]).unwrap();
        assert!(session.poll(80_000).unwrap().is_none());
        session.push_i16(&samples[47_999..]).unwrap();
        let job = session.poll(80_001).unwrap().unwrap();
        assert_eq!(job.sample_ms, 3_000);
        assert_eq!(job.generated_at_ms, 80_001);
        assert_eq!(job.attempt, 1);
        assert_eq!(
            Signature::parse(&job.signature).unwrap().len(),
            job.signature.len()
        );
    }

    #[test]
    fn minimum_signature_length_can_extend_the_initial_config_target() {
        let mut session = ProgressiveSession::new(RecognitionTiming {
            sample_rate: 16_000,
            minimum_signature_ms: 2_000,
            initial_sample_ms: 1_000,
            progressive_step_ms: 1_000,
            maximum_sample_ms: 9_000,
            default_retry_interval_ms: 100,
            request_timeout_ms: 1_000,
            maximum_retry_delay_ms: 500,
            maximum_attempts: 3,
        })
        .unwrap();
        let samples = pcm(32_000);
        session.push_i16(&samples[..31_999]).unwrap();
        assert!(session.poll(2_000).unwrap().is_none());
        session.push_i16(&samples[31_999..]).unwrap();
        assert_eq!(session.poll(2_001).unwrap().unwrap().sample_ms, 2_000);
    }

    #[test]
    fn intermediate_signature_is_non_destructive_and_input_continues_in_flight() {
        let mut session = ProgressiveSession::new(timing(9_000, 3)).unwrap();
        let samples = pcm(96_000);
        session.push_i16(&samples[..48_000]).unwrap();
        let first = session.poll(3_000).unwrap().unwrap();

        let mut reference = SigX::new(SigXConfig {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: 144_000,
        })
        .unwrap();
        reference.push_i16(&samples[..48_000]).unwrap();
        assert_eq!(first.signature, reference.signature().unwrap());

        // The response is still outstanding, but the audio producer does not
        // wait for HTTP and can submit another cumulative second.
        let pushed = session.push_i16(&samples[48_000..64_000]).unwrap();
        assert_eq!(pushed.total_frames, 64_000);
        assert_eq!(session.status(), SessionStatus::InFlight { attempt: 1 });
        session
            .complete(1, RecognitionOutcome::NoMatch { retry: None }, 3_050)
            .unwrap();
        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 3_150,
                target_sample_ms: 6_000,
            }
        );

        assert!(session.poll(3_150).unwrap().is_none());
        session.push_i16(&samples[64_000..96_000]).unwrap();
        let second = session.poll(3_150).unwrap().unwrap();
        assert_eq!(second.sample_ms, 6_000);
        assert_eq!(second.attempt, 2);
        Signature::parse(&second.signature).expect("intermediate Signature parses");

        // Later flow is still accepted after two non-destructive snapshots.
        session
            .complete(2, RecognitionOutcome::NoMatch { retry: None }, 3_200)
            .unwrap();
        let final_chunk = pcm(48_000);
        session.push_i16(&final_chunk).unwrap();
        let third = session.poll(3_300).unwrap().unwrap();
        assert_eq!(third.sample_ms, 9_000);
        Signature::parse(&third.signature).expect("final Signature parses");
    }

    #[test]
    fn retry_directive_controls_sample_target_and_wait_separately() {
        let mut session = ProgressiveSession::new(timing(12_000, 4)).unwrap();
        session.push_i16(&pcm(48_000)).unwrap();
        let first = session.poll(100).unwrap().unwrap();
        session
            .complete(
                first.attempt,
                RecognitionOutcome::NoMatch {
                    retry: Some(RetryDirective {
                        delay_ms: Some(250),
                        target_sample_ms: Some(9_000),
                    }),
                },
                120,
            )
            .unwrap();
        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 370,
                target_sample_ms: 9_000,
            }
        );
        session.push_i16(&pcm(96_000)).unwrap();
        assert!(session.poll(369).unwrap().is_none());
        assert_eq!(session.poll(370).unwrap().unwrap().sample_ms, 9_000);
    }

    #[test]
    fn invalid_retry_values_fall_back_to_bounded_progression() {
        let mut session = ProgressiveSession::new(timing(9_000, 3)).unwrap();
        session.push_i16(&pcm(48_000)).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        session
            .complete(
                first.attempt,
                RecognitionOutcome::NoMatch {
                    retry: Some(RetryDirective {
                        delay_ms: Some(10_000),
                        target_sample_ms: Some(1_000),
                    }),
                },
                10,
            )
            .unwrap();
        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 110,
                target_sample_ms: 6_000,
            }
        );
    }

    #[test]
    fn match_ends_the_session_and_no_audio_is_accepted_afterward() {
        let mut session = ProgressiveSession::new(timing(9_000, 3)).unwrap();
        session.push_i16(&pcm(48_000)).unwrap();
        let job = session.poll(0).unwrap().unwrap();
        assert_eq!(
            session
                .complete(job.attempt, RecognitionOutcome::Match, 1)
                .unwrap(),
            SessionStatus::Succeeded
        );
        assert_eq!(session.push_i16(&[0]).unwrap_err(), EngineError::Closed);
    }

    #[test]
    fn retry_limit_exhaustion_prevents_infinite_submissions() {
        let mut session = ProgressiveSession::new(timing(12_000, 2)).unwrap();
        session.push_i16(&pcm(48_000)).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        session
            .complete(
                first.attempt,
                RecognitionOutcome::NoMatch { retry: None },
                1,
            )
            .unwrap();
        session.push_i16(&pcm(48_000)).unwrap();
        let second = session.poll(101).unwrap().unwrap();
        assert_eq!(second.sample_ms, 6_000);
        assert_eq!(
            session
                .complete(
                    second.attempt,
                    RecognitionOutcome::NoMatch { retry: None },
                    102
                )
                .unwrap(),
            SessionStatus::Exhausted
        );
        assert!(session.poll(10_000).unwrap().is_none());
    }

    #[test]
    fn timeout_http_failure_and_invalid_response_are_terminal() {
        let mut timed = ProgressiveSession::new(timing(9_000, 3)).unwrap();
        timed.push_i16(&pcm(48_000)).unwrap();
        let job = timed.poll(5).unwrap().unwrap();
        assert!(timed.poll(1_005).unwrap().is_none());
        assert_eq!(
            timed.status(),
            SessionStatus::Failed(StopReason::RequestTimeout)
        );
        assert_eq!(
            timed.complete(job.attempt, RecognitionOutcome::Match, 1_006),
            Err(EngineError::NoRequestInFlight)
        );

        for (outcome, reason) in [
            (
                RecognitionOutcome::TransportFailure,
                StopReason::TransportFailure,
            ),
            (
                RecognitionOutcome::InvalidResponse,
                StopReason::InvalidResponse,
            ),
        ] {
            let mut session = ProgressiveSession::new(timing(9_000, 3)).unwrap();
            session.push_i16(&pcm(48_000)).unwrap();
            let job = session.poll(0).unwrap().unwrap();
            assert_eq!(
                session.complete(job.attempt, outcome, 1).unwrap(),
                SessionStatus::Failed(reason)
            );
        }
    }

    #[test]
    fn cancellation_invalidates_in_flight_and_waiting_work() {
        let mut session = ProgressiveSession::new(timing(9_000, 3)).unwrap();
        session.push_i16(&pcm(48_000)).unwrap();
        let job = session.poll(0).unwrap().unwrap();
        assert_eq!(session.cancel(), Some(job.attempt));
        assert_eq!(session.status(), SessionStatus::Cancelled);
        assert_eq!(session.push_i16(&[0]).unwrap_err(), EngineError::Closed);
    }

    #[test]
    fn chunk_partition_does_not_change_signature_at_same_audio_length() {
        let samples = pcm(48_000);
        let mut one = ProgressiveSession::new(timing(9_000, 3)).unwrap();
        one.push_i16(&samples).unwrap();
        let one_signature = one.poll(0).unwrap().unwrap().signature;

        let mut chunked = ProgressiveSession::new(timing(9_000, 3)).unwrap();
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
            ..timing(9_000, 3)
        })
        .is_err());

        let mut session = ProgressiveSession::new(timing(9_000, 3)).unwrap();
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
