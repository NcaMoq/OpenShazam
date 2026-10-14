use super::CaptureAccumulator;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Data, FromSample, Sample, SampleFormat, SizedSample,
};
use shazam_progressive::{
    NativeTaggingType, ProgressiveSession, RecognitionOutcome, RollingPcmBuffer, SessionStatus,
    NATIVE_NETWORK_REQUEST_TIMEOUT_MS, NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS,
    NATIVE_TAGGING_TYPE_2_DEFAULT_INTERVAL_MS,
};
use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::{self, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, sync_channel, TryRecvError, TrySendError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PACKET_QUEUE_CAPACITY: usize = 8;
const PACKET_TIMEOUT: Duration = Duration::from_secs(30);
const RECOGNITION_QUEUE_TIMEOUT: Duration = Duration::from_millis(25);

pub(super) fn capture_signature(
    output: &Path,
    capture_seconds: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no default microphone found"))?;
    let supported = device.default_input_config()?;
    let sample_rate = supported.sample_rate();
    let channels = usize::from(supported.channels());
    let sample_format = supported.sample_format();
    if sample_format.is_dsd() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("DSD microphone format is unsupported: {sample_format}"),
        )
        .into());
    }
    if channels == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "microphone reported zero channels",
        )
        .into());
    }
    let target_frames = usize::try_from(sample_rate)?
        .checked_mul(capture_seconds)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "capture length overflow"))?;
    let mut accumulator = CaptureAccumulator::new_for_rate(sample_rate, target_frames)
        .map_err(|error| io::Error::new(io::ErrorKind::Unsupported, error))?;

    let (sender, receiver) = sync_channel(PACKET_QUEUE_CAPACITY);
    let callback_fault = Arc::new(Mutex::new(None::<String>));
    let stream_fault = Arc::clone(&callback_fault);
    let queue_overflow = Arc::new(AtomicBool::new(false));
    let callback_overflow = Arc::clone(&queue_overflow);
    let stream = device.build_input_stream_raw(
        supported.into(),
        sample_format,
        move |data, _| {
            let Some(samples) = convert_to_f32(data) else {
                set_fault(&stream_fault, "could not decode microphone sample buffer");
                return;
            };
            if let Err(error) = sender.try_send(samples) {
                if matches!(error, TrySendError::Full(_)) {
                    callback_overflow.store(true, Ordering::Relaxed);
                }
            }
        },
        {
            let stream_error = Arc::clone(&callback_fault);
            move |error| set_fault(&stream_error, &format!("CoreAudio stream error: {error}"))
        },
        Some(Duration::from_secs(2)),
    )?;

    println!(
        "Capturing {capture_seconds}s from the default CoreAudio input ({sample_rate} Hz, {channels} channel(s), {sample_format}); microphone data stays local."
    );
    stream.play()?;

    while accumulator.remaining_frames() > 0 {
        if queue_overflow.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "capture processing fell behind; an audio packet was dropped",
            )
            .into());
        }
        if let Some(error) = take_fault(&callback_fault)? {
            return Err(io::Error::other(error).into());
        }
        match receiver.recv_timeout(PACKET_TIMEOUT) {
            Ok(samples) => accumulator.push_interleaved_f32(&samples, channels)?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(error) = take_fault(&callback_fault)? {
                    return Err(io::Error::other(error).into());
                }
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "CoreAudio supplied no microphone packet for 30 seconds",
                )
                .into());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "CoreAudio capture callback stopped unexpectedly",
                )
                .into());
            }
        }
    }
    drop(stream);

    let signature = accumulator.finish()?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(&signature)?;
    println!(
        "Wrote {} bytes to {} (local only; no network request was made)",
        signature.len(),
        output.display()
    );
    Ok(())
}

pub(super) fn recognize_progressive(
    tagging_type: NativeTaggingType,
    context_state: super::NativeContextState,
    jsonl_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if tagging_type == NativeTaggingType::Auto && context_state.current().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Auto recognition requires an explicit foreground/background context",
        )
        .into());
    }
    let profile = super::config_request_profile_from_host().map_err(io::Error::other)?;
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no default microphone found"))?;
    let supported = device.default_input_config()?;
    let sample_rate = supported.sample_rate();
    let channels = usize::from(supported.channels());
    let sample_format = supported.sample_format();
    if sample_format.is_dsd() || channels == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "default microphone format is not supported for Progressive recognition",
        )
        .into());
    }

    let request_timeout_ms = env_u64_setting(
        "OPENSHZAM_PROGRESSIVE_REQUEST_TIMEOUT_MS",
        NATIVE_NETWORK_REQUEST_TIMEOUT_MS,
        1,
        600_000,
    )?;
    let maximum_retry_delay_ms =
        env_optional_u64_setting("OPENSHZAM_PROGRESSIVE_RETRY_DELAY_CAP_MS", 600_000)?;
    let maximum_attempts =
        env_optional_u64_setting("OPENSHZAM_PROGRESSIVE_MAX_ATTEMPTS", u64::MAX)?;
    let timezone = mac_timezone_name();
    let trace_enabled =
        std::env::var_os("OPENSHZAM_PROGRESSIVE_TRACE").is_some_and(|value| value == "1");

    let (progressive_sender, progressive_receiver) = mpsc::channel();
    let pending_audio_packets = Arc::new(AtomicUsize::new(0));
    let callback_pending_audio = Arc::clone(&pending_audio_packets);
    let callback_fault = Arc::new(Mutex::new(None::<String>));
    let stream_fault = Arc::clone(&callback_fault);
    let queue_overflow = Arc::new(AtomicBool::new(false));
    let callback_overflow = Arc::clone(&queue_overflow);
    let stream_stopping = Arc::new(AtomicBool::new(false));
    let callback_stream_stopping = Arc::clone(&stream_stopping);
    let stream_sender = progressive_sender.clone();
    let stream = device.build_input_stream_raw(
        supported.into(),
        sample_format,
        move |data, info| {
            let Some(samples) = convert_to_f32(data) else {
                set_fault(&stream_fault, "could not decode microphone sample buffer");
                return;
            };
            enqueue_audio_event(
                &stream_sender,
                &callback_pending_audio,
                &callback_overflow,
                &stream_fault,
                &callback_stream_stopping,
                samples,
                input_capture_timestamp_ms(info),
            );
        },
        {
            let stream_error = Arc::clone(&callback_fault);
            move |error| set_fault(&stream_error, &format!("CoreAudio stream error: {error}"))
        },
        Some(Duration::from_secs(2)),
    )?;

    let cancelled = Arc::new(AtomicBool::new(false));
    let signal_cancelled = Arc::clone(&cancelled);
    ctrlc::set_handler(move || signal_cancelled.store(true, Ordering::Release))?;
    let maximum_prerecord_frames = if tagging_type.uses_pre_recorded_audio() {
        super::frames_for_ms(sample_rate, 60_000)?
    } else {
        0
    };
    let mut preroll = Some(RollingPcmBuffer::new(maximum_prerecord_frames));
    let mut preroll_start_timestamp_ms = None;
    let capture_started = Instant::now();
    stream.play()?;
    trace_progressive(trace_enabled, capture_started, "capture-stream-start", "");
    super::progressive_log(
        jsonl_output,
        "Microphone capture started while Shazam Config loads; PCM remains in a bounded local rolling buffer.",
    );

    let (configuration_sender, configuration_receiver) = sync_channel(1);
    let configuration_task = thread::Builder::new()
        .name("openshazam-config-fetch".into())
        .spawn(move || {
            let result =
                shazam_http_lab::fetch_shazam_config(&profile).map_err(|error| error.to_string());
            let _ = configuration_sender.send(result);
        })?;
    let configuration = loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "cancelled while loading Shazam Config",
            )
            .into());
        }
        if queue_overflow.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "pre-record buffering fell behind while loading Config; a microphone packet was dropped",
            )
            .into());
        }
        if let Some(error) = take_fault(&callback_fault)? {
            return Err(io::Error::other(error).into());
        }
        match configuration_receiver.try_recv() {
            Ok(result) => break result.map_err(io::Error::other)?,
            Err(TryRecvError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Shazam Config worker stopped unexpectedly",
                )
                .into());
            }
            Err(TryRecvError::Empty) => {}
        }
        match progressive_receiver.recv_timeout(RECOGNITION_QUEUE_TIMEOUT) {
            Ok(ProgressiveEvent::Audio {
                samples,
                source_timestamp_ms,
                processed,
            }) => {
                release_audio_packet(&pending_audio_packets);
                let result = super::downmix_interleaved_f32(&samples, channels);
                if let Ok(mono) = &result {
                    push_timed_preroll(
                        preroll
                            .as_mut()
                            .expect("pre-roll buffer exists until Config is resolved"),
                        &mut preroll_start_timestamp_ms,
                        mono,
                        source_timestamp_ms,
                        sample_rate,
                    );
                }
                let _ = processed.send(());
                result?;
            }
            Ok(ProgressiveEvent::Completion(_)) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "recognition response arrived before the Progressive session started",
                )
                .into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "CoreAudio capture callback stopped while loading Config",
                )
                .into());
            }
        }
    };
    configuration_task
        .join()
        .map_err(|_| io::Error::other("Shazam Config worker panicked"))?;

    let manual_timing = configuration.manual_timing();
    let native_frame_targets = configuration
        .native_frame_targets_for_type(tagging_type, sample_rate)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let native_pre_record_capacity_ms = if tagging_type.uses_pre_recorded_audio() {
        manual_timing.effective_pre_record_buffer_ms()
    } else {
        0
    };
    if native_pre_record_capacity_ms > 60_000 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Config pre-record buffer exceeds the local 60-second safety limit",
        )
        .into());
    }
    let default_interval_ms = match tagging_type {
        NativeTaggingType::Manual => NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS,
        NativeTaggingType::Auto => NATIVE_TAGGING_TYPE_2_DEFAULT_INTERVAL_MS,
    };
    let timing = match tagging_type {
        NativeTaggingType::Manual => manual_timing.recognition_timing(
            sample_rate,
            shazam_http_lab::NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
            default_interval_ms,
            request_timeout_ms,
            maximum_retry_delay_ms,
            maximum_attempts,
        ),
        NativeTaggingType::Auto => manual_timing.auto_recognition_timing(
            sample_rate,
            shazam_http_lab::NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
            default_interval_ms,
            request_timeout_ms,
            maximum_retry_delay_ms,
            maximum_attempts,
        ),
    }
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let pre_record_capacity_override_ms = if tagging_type.uses_pre_recorded_audio() {
        env_optional_u64_setting("OPENSHZAM_PRERECORD_BUFFER_MS", 60_000)?
    } else if std::env::var_os("OPENSHZAM_PRERECORD_BUFFER_MS").is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "OPENSHZAM_PRERECORD_BUFFER_MS applies only to Manual recognition",
        )
        .into());
    } else {
        None
    };
    let pre_record_capacity_mode = if !tagging_type.uses_pre_recorded_audio() {
        "not used by Auto"
    } else if pre_record_capacity_override_ms.is_some() {
        "diagnostic override"
    } else {
        "Native page-rounded"
    };
    let pre_record_capacity_frames = if !tagging_type.uses_pre_recorded_audio() {
        0
    } else {
        match pre_record_capacity_override_ms {
            Some(duration_ms) => super::frames_for_ms(sample_rate, duration_ms)?,
            None => manual_timing
                .effective_pre_record_capacity_frames(sample_rate)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        }
    };
    let pre_record_capacity_ms =
        super::milliseconds_for_frames(sample_rate, pre_record_capacity_frames)?;
    let preroll_ref = preroll
        .as_mut()
        .expect("pre-roll buffer exists until manual session setup");
    let frames_before_resize = preroll_ref.frames();
    preroll_ref.set_capacity_frames(pre_record_capacity_frames);
    if let Some(start_ms) = preroll_start_timestamp_ms {
        preroll_start_timestamp_ms = advance_timestamp_ms(
            start_ms,
            frames_before_resize.saturating_sub(preroll_ref.frames()),
            sample_rate,
        );
    }
    super::emit_progressive_ui_config(
        &configuration,
        super::ProgressiveUiConfigInputs {
            tagging_type,
            request_context: context_state.current(),
            sample_rate_hz: sample_rate,
            pre_record_capacity_frames,
            diagnostic_pre_record_override: pre_record_capacity_override_ms.is_some(),
            request_timeout_ms,
            jsonl_output,
        },
    )?;
    let mut session = if pre_record_capacity_frames == 0 {
        preroll = None;
        while let Ok(event) = progressive_receiver.try_recv() {
            match event {
                ProgressiveEvent::Audio { processed, .. } => {
                    release_audio_packet(&pending_audio_packets);
                    let _ = processed.send(());
                }
                ProgressiveEvent::Completion(_) => {}
            }
        }
        let mut session = ProgressiveSession::new_for_type_with_native_frame_targets(
            timing,
            tagging_type,
            pre_record_capacity_frames,
            Some(native_frame_targets),
        )
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        session
            .set_session_start_timestamp_ms(
                unix_timestamp_ms()
                    .ok_or_else(|| io::Error::other("system clock is before Unix epoch"))?,
            )
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Some(session)
    } else {
        None
    };
    let mut active_workers = BTreeSet::new();
    let mut matched = false;
    let mut no_match_seen = false;

    super::progressive_log(
        jsonl_output,
        format!(
            "Progressive {tagging_type:?} recognition will use the default microphone ({sample_rate} Hz, {channels} channel(s), {sample_format}). Only generated Signatures are sent; PCM is not saved. Press Ctrl+C to stop. initial_ms={} maximum_ms={} pre_record_capacity_ms={pre_record_capacity_ms} pre_record_capacity_frames={pre_record_capacity_frames} pre_record_capacity_mode={pre_record_capacity_mode} session_timeout_ms={:?}. Audio from before this command is unavailable.",
            timing.initial_sample_ms,
            timing.maximum_sample_ms,
            timing.session_timeout_ms
        ),
    );

    if session.is_none() {
        let trigger_receiver = super::spawn_manual_trigger_receiver();
        let mut ring = preroll.take().expect("pre-roll buffer is active");
        super::progressive_log(
            jsonl_output,
            format!("Rolling pre-record buffer is active; press Enter to start manual recognition (retaining up to {pre_record_capacity_ms} ms; {pre_record_capacity_mode} capacity)."),
        );
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "cancelled while waiting for manual recognition trigger",
                )
                .into());
            }
            if queue_overflow.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "pre-record buffering fell behind; a microphone packet was dropped",
                )
                .into());
            }
            if let Some(error) = take_fault(&callback_fault)? {
                return Err(io::Error::other(error).into());
            }

            match trigger_receiver.try_recv() {
                Ok(Ok(())) => {
                    trace_progressive(
                        trace_enabled,
                        capture_started,
                        "manual-trigger-observed",
                        "",
                    );
                    loop {
                        match progressive_receiver.try_recv() {
                            Ok(ProgressiveEvent::Audio {
                                samples,
                                source_timestamp_ms,
                                processed,
                            }) => {
                                release_audio_packet(&pending_audio_packets);
                                let result = super::downmix_interleaved_f32(&samples, channels);
                                if let Ok(mono) = &result {
                                    push_timed_preroll(
                                        &mut ring,
                                        &mut preroll_start_timestamp_ms,
                                        mono,
                                        source_timestamp_ms,
                                        sample_rate,
                                    );
                                }
                                let _ = processed.send(());
                                result?;
                            }
                            Ok(ProgressiveEvent::Completion(_)) => {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "recognition response arrived before the Progressive session started",
                                )
                                .into());
                            }
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => {
                                return Err(io::Error::new(
                                    io::ErrorKind::BrokenPipe,
                                    "CoreAudio capture callback stopped unexpectedly",
                                )
                                .into());
                            }
                        }
                    }
                    break;
                }
                Ok(Err(error)) => return Err(io::Error::other(error).into()),
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "manual recognition trigger input closed",
                    )
                    .into());
                }
                Err(TryRecvError::Empty) => {}
            }

            match progressive_receiver.recv_timeout(RECOGNITION_QUEUE_TIMEOUT) {
                Ok(ProgressiveEvent::Audio {
                    samples,
                    source_timestamp_ms,
                    processed,
                }) => {
                    release_audio_packet(&pending_audio_packets);
                    let result = super::downmix_interleaved_f32(&samples, channels);
                    if let Ok(mono) = &result {
                        push_timed_preroll(
                            &mut ring,
                            &mut preroll_start_timestamp_ms,
                            mono,
                            source_timestamp_ms,
                            sample_rate,
                        );
                    }
                    let _ = processed.send(());
                    result?;
                }
                Ok(ProgressiveEvent::Completion(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "recognition response arrived before the Progressive session started",
                    )
                    .into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "CoreAudio capture callback stopped unexpectedly",
                    )
                    .into());
                }
            }
        }

        let pre_recorded = ring.into_samples();
        let mut seeded = ProgressiveSession::new_for_type_with_native_frame_targets(
            timing,
            tagging_type,
            pre_record_capacity_frames,
            Some(native_frame_targets),
        )
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        seeded
            .set_session_start_timestamp_ms(
                preroll_start_timestamp_ms
                    .or_else(unix_timestamp_ms)
                    .ok_or_else(|| io::Error::other("system clock is before Unix epoch"))?,
            )
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let seeded_frames = seeded
            .prime_pre_recorded_f32(&pre_recorded)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        super::progressive_log(
            jsonl_output,
            format!("Manual recognition started with {seeded_frames} buffered PCM frames."),
        );
        session = Some(seeded);
    }
    let mut session = session.expect("a session is initialized before progressive capture");
    let started = Instant::now();
    trace_progressive(
        trace_enabled,
        capture_started,
        "session-start",
        &format!(
            "session_elapsed_ms=0 seeded_frames={} seeded_sample_ms={} timeout_ms={:?}",
            session.frames_seen(),
            session.sample_ms(),
            timing.session_timeout_ms,
        ),
    );
    let capture_result = (|| -> Result<(), Box<dyn std::error::Error>> {
        while !session.is_terminal() {
            if cancelled.load(Ordering::Acquire) {
                session.cancel();
                break;
            }
            if queue_overflow.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Progressive processing fell behind; a microphone packet was dropped",
                )
                .into());
            }
            if let Some(error) = take_fault(&callback_fault)? {
                return Err(io::Error::other(error).into());
            }

            service_progressive(
                &mut session,
                &configuration,
                context_state.clone(),
                &timezone,
                started,
                Duration::from_millis(request_timeout_ms),
                None,
                &progressive_sender,
                &progressive_receiver,
                channels,
                &pending_audio_packets,
                &cancelled,
                &mut active_workers,
                &mut matched,
                &mut no_match_seen,
                capture_started,
                trace_enabled,
                jsonl_output,
            )?;

            let first_event = match progressive_receiver
                .recv_timeout(PACKET_TIMEOUT.min(RECOGNITION_QUEUE_TIMEOUT))
            {
                Ok(event) => Some(event),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "CoreAudio capture callback stopped unexpectedly",
                    )
                    .into());
                }
            };

            service_progressive(
                &mut session,
                &configuration,
                context_state.clone(),
                &timezone,
                started,
                Duration::from_millis(request_timeout_ms),
                first_event,
                &progressive_sender,
                &progressive_receiver,
                channels,
                &pending_audio_packets,
                &cancelled,
                &mut active_workers,
                &mut matched,
                &mut no_match_seen,
                capture_started,
                trace_enabled,
                jsonl_output,
            )?;
        }
        Ok(())
    })();

    stream_stopping.store(true, Ordering::Release);
    drop(stream);
    let mut outstanding_at_stop = active_workers.len();
    while outstanding_at_stop > 0 {
        match progressive_receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(ProgressiveEvent::Audio { processed, .. }) => {
                release_audio_packet(&pending_audio_packets);
                let _ = processed.send(());
            }
            Ok(ProgressiveEvent::Completion(completion)) => {
                if active_workers.remove(&completion.attempt) {
                    outstanding_at_stop = outstanding_at_stop.saturating_sub(1);
                }
                if session.in_flight_attempts().contains(&completion.attempt) {
                    let _ = session.complete_with_delivery_at_ns(
                        completion.attempt,
                        completion.outcome,
                        super::elapsed_ns(started),
                    );
                }
            }
            Err(_) => break,
        }
    }
    cancelled.store(true, Ordering::Release);
    capture_result?;

    trace_progressive(
        trace_enabled,
        capture_started,
        "session-terminal",
        &format!(
            "session_elapsed_ms={} state={:?}",
            elapsed_ms(started),
            session.status()
        ),
    );

    let status = session.status();
    if matches!(
        status,
        SessionStatus::Failed(shazam_progressive::StopReason::SessionTimeout)
    ) {
        super::emit_session_timeout_event(session.tagging_type(), jsonl_output);
    }
    if super::manual_completion_is_normal(status, matched, no_match_seen) {
        return Ok(());
    }
    match status {
        SessionStatus::Cancelled if matched => {
            super::progressive_log(
                jsonl_output,
                "Stopped after at least one recognition match.",
            );
            Ok(())
        }
        SessionStatus::Cancelled => Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Progressive microphone recognition was cancelled",
        )
        .into()),
        SessionStatus::Failed(reason) => Err(io::Error::other(format!(
            "Progressive microphone recognition stopped: {reason:?}"
        ))
        .into()),
        SessionStatus::Exhausted => {
            Err(io::Error::other("Progressive microphone recognition ended without a match").into())
        }
        status => Err(io::Error::other(format!(
            "Progressive microphone recognition ended unexpectedly: {status:?}"
        ))
        .into()),
    }
}

struct ProgressiveCompletion {
    attempt: u64,
    status: Option<u16>,
    outcome: RecognitionOutcome,
    candidates: Vec<shazam_http_lab::TrackCandidate>,
}

// Native flowAudioData synchronously enters the generator's serial queue while
// Retry updates are asynchronously enqueued on that same queue. Keep both
// operations in one FIFO and acknowledge audio only after its session work and
// any threshold-triggered request have been dispatched.
enum ProgressiveEvent {
    Audio {
        samples: Vec<f32>,
        source_timestamp_ms: Option<u64>,
        processed: mpsc::SyncSender<()>,
    },
    Completion(ProgressiveCompletion),
}

fn reserve_audio_packet(pending: &AtomicUsize) -> bool {
    pending
        .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |count| {
            (count < PACKET_QUEUE_CAPACITY).then_some(count + 1)
        })
        .is_ok()
}

fn release_audio_packet(pending: &AtomicUsize) {
    let previous = pending.fetch_sub(1, Ordering::AcqRel);
    debug_assert!(previous > 0, "audio packet counter underflow");
}

fn enqueue_audio_event(
    sender: &mpsc::Sender<ProgressiveEvent>,
    pending_audio_packets: &AtomicUsize,
    queue_overflow: &AtomicBool,
    callback_fault: &Mutex<Option<String>>,
    stream_stopping: &AtomicBool,
    samples: Vec<f32>,
    source_timestamp_ms: Option<u64>,
) {
    if stream_stopping.load(Ordering::Acquire) {
        return;
    }
    let (processed_sender, processed_receiver) = mpsc::sync_channel(0);
    if !reserve_audio_packet(pending_audio_packets) {
        queue_overflow.store(true, Ordering::Relaxed);
        return;
    }
    if sender
        .send(ProgressiveEvent::Audio {
            samples,
            source_timestamp_ms,
            processed: processed_sender,
        })
        .is_err()
    {
        release_audio_packet(pending_audio_packets);
        set_fault(
            callback_fault,
            "Progressive event queue closed during capture",
        );
        return;
    }
    loop {
        match processed_receiver.recv_timeout(RECOGNITION_QUEUE_TIMEOUT) {
            Ok(()) => break,
            Err(mpsc::RecvTimeoutError::Timeout) if stream_stopping.load(Ordering::Acquire) => {
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                set_fault(callback_fault, "Progressive PCM event was not acknowledged");
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn service_progressive(
    session: &mut ProgressiveSession,
    configuration: &shazam_http_lab::ShazamConfiguration,
    context_state: super::NativeContextState,
    timezone: &str,
    started: Instant,
    request_timeout: Duration,
    first_event: Option<ProgressiveEvent>,
    event_sender: &mpsc::Sender<ProgressiveEvent>,
    event_receiver: &mpsc::Receiver<ProgressiveEvent>,
    channels: usize,
    pending_audio_packets: &AtomicUsize,
    cancelled: &Arc<AtomicBool>,
    active_workers: &mut BTreeSet<u64>,
    matched: &mut bool,
    no_match_seen: &mut bool,
    capture_started: Instant,
    trace_enabled: bool,
    jsonl_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut audio_ack = None;
    let mut deferred_post_stop_job = None;
    let mut deferred_manual_ui_event = None;
    let event = match first_event {
        Some(event) => Some(event),
        None => match event_receiver.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Progressive event queue disconnected",
                )
                .into());
            }
        },
    };
    if let Some(event) = event {
        match event {
            ProgressiveEvent::Audio {
                samples,
                source_timestamp_ms,
                processed,
            } => {
                release_audio_packet(pending_audio_packets);
                audio_ack = Some(processed);
                let result = if session.is_terminal() {
                    Ok(())
                } else {
                    (|| -> Result<(), io::Error> {
                        let mono = super::downmix_interleaved_f32(&samples, channels)?;
                        session
                            .push_f32_at_with_source_timestamp_ns(
                                &mono,
                                super::elapsed_ns(started),
                                source_timestamp_ms,
                            )
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                        Ok(())
                    })()
                };
                result?;
            }
            ProgressiveEvent::Completion(completion) => {
                active_workers.remove(&completion.attempt);
                let response_outcome = completion.outcome;
                if let Some(status) = completion.status {
                    super::progressive_log(
                        jsonl_output,
                        format!(
                            "recognition attempt {} returned HTTP {status}",
                            completion.attempt
                        ),
                    );
                }
                let tracked = session.in_flight_attempts().contains(&completion.attempt);
                let was_terminal = session.is_terminal();
                let mut manual_ui_event = None;
                let mut auto_ui_event = None;
                let mut delivered_candidates = Vec::new();
                let delivered = if tracked {
                    let delivery = shazam_http_lab::apply_progressive_response_at_ns(
                        session,
                        completion.attempt,
                        completion.outcome,
                        completion.candidates.clone(),
                        super::elapsed_ns(started),
                    )
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                    if !delivery.delivered_to_matcher {
                        super::progressive_log(
                            jsonl_output,
                            format!(
                                "recognition attempt {} response was ignored by the Native context gate (state={:?})",
                                completion.attempt,
                                delivery.status
                            ),
                        );
                    }
                    manual_ui_event = delivery.native_manual_ui_event();
                    auto_ui_event = delivery.native_auto_ui_event();
                    delivered_candidates = delivery.candidates;
                    delivery.delivered_to_matcher
                } else {
                    false
                };
                if tracked && delivered && !was_terminal {
                    deferred_post_stop_job = session.take_post_stop_retry_job();
                }
                match &manual_ui_event {
                    Some(shazam_http_lab::NativeManualUiEvent::Result(event)) => {
                        trace_progressive(
                            trace_enabled,
                            capture_started,
                            "manual-ui-result-event",
                            &format!(
                                "stage={:?} alternative_present={}",
                                event.stage,
                                event.alternative_track.is_some()
                            ),
                        );
                    }
                    Some(shazam_http_lab::NativeManualUiEvent::NoMatch) => trace_progressive(
                        trace_enabled,
                        capture_started,
                        "manual-ui-terminal-event-ready",
                        "kind=no-match",
                    ),
                    Some(shazam_http_lab::NativeManualUiEvent::Error { kind }) => {
                        trace_progressive(
                            trace_enabled,
                            capture_started,
                            "manual-ui-terminal-event-ready",
                            &format!("kind=error error_kind={kind:?}"),
                        );
                    }
                    None => {}
                }
                trace_progressive(
            trace_enabled,
            capture_started,
            "response-applied",
            &format!(
                "session_elapsed_ms={} attempt={} http_status={:?} outcome={:?} applied={} state={:?}",
                elapsed_ms(started),
                completion.attempt,
                completion.status,
                response_outcome,
                tracked && delivered && !was_terminal,
                session.status()
            ),
        );
                if tracked && delivered && !was_terminal {
                    if let Some(event) = manual_ui_event.as_ref() {
                        match event {
                            shazam_http_lab::NativeManualUiEvent::Result(_) => {
                                *matched = true;
                            }
                            shazam_http_lab::NativeManualUiEvent::NoMatch => {
                                *no_match_seen = true;
                            }
                            shazam_http_lab::NativeManualUiEvent::Error { .. } => {}
                        }
                        if deferred_post_stop_job.is_some()
                            && matches!(event, &shazam_http_lab::NativeManualUiEvent::NoMatch)
                        {
                            deferred_manual_ui_event = Some(event.clone());
                        } else {
                            super::emit_manual_ui_event(event, jsonl_output);
                        }
                    } else if let Some(event) = auto_ui_event.as_ref() {
                        *matched = true;
                        super::emit_auto_ui_event(event, jsonl_output);
                    } else if !delivered_candidates.is_empty() {
                        *matched = true;
                        super::progressive_log(
                            jsonl_output,
                            "match response withheld: Native UI ordering is unverified for this response shape",
                        );
                    }
                }
            }
        }
    }

    let mut jobs = Vec::new();
    let tagging_type = session.tagging_type();
    let post_stop_job = deferred_post_stop_job.or_else(|| session.take_post_stop_retry_job());
    let detached_post_stop_job = post_stop_job.is_some();
    if let Some(job) = post_stop_job {
        jobs.push(job);
    } else {
        while let Some(job) = session
            .poll_at_ns(super::elapsed_ns(started))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            jobs.push(job);
        }
    }
    for job in jobs {
        let attempt = job.attempt;
        let configuration = configuration.clone();
        let timezone = timezone.to_owned();
        let request_cancelled = if detached_post_stop_job {
            Arc::new(AtomicBool::new(false))
        } else {
            Arc::clone(cancelled)
        };
        let context_state_for_worker = context_state.clone();
        let sender = event_sender.clone();
        let (dispatch_ack_sender, dispatch_ack_receiver) = if detached_post_stop_job {
            let (sender, receiver) = mpsc::sync_channel(1);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        trace_progressive(
            trace_enabled,
            capture_started,
            "signature-ready",
            &format!(
                "session_elapsed_ms={} attempt={} sample_ms={}",
                job.generated_at_ms, job.attempt, job.sample_ms
            ),
        );
        let spawn_result = thread::Builder::new()
            .name(format!("openshazam-recognition-{attempt}"))
            .spawn(move || {
                let request_context = context_state_for_worker.current();
                trace_progressive(
                    trace_enabled,
                    capture_started,
                    "http-submit-call-start",
                    &format!(
                        "session_elapsed_ms={} attempt={} sample_ms={}",
                        elapsed_ms(started),
                        attempt,
                        job.sample_ms
                    ),
                );
                let mut transport_started = false;
                let response = if let Some(dispatch_ack_sender) = &dispatch_ack_sender {
                    shazam_http_lab::submit_progressive_signature_for_tagging_type_with_transport_started_and_timestamp_ms(
                        &configuration,
                        tagging_type,
                        request_context,
                        &job.signature,
                        job.sample_ms,
                        job.signature_timestamp_ms,
                        &timezone,
                        request_timeout,
                        &request_cancelled,
                        || {
                            transport_started = true;
                            let _ = dispatch_ack_sender.send(true);
                        },
                    )
                } else {
                    shazam_http_lab::submit_progressive_signature_for_tagging_type_with_transport_started_and_timestamp_ms(
                        &configuration,
                        tagging_type,
                        request_context,
                        &job.signature,
                        job.sample_ms,
                        job.signature_timestamp_ms,
                        &timezone,
                        request_timeout,
                        &request_cancelled,
                        || {},
                    )
                };
                if let Some(dispatch_ack_sender) = &dispatch_ack_sender {
                    if !transport_started {
                        let _ = dispatch_ack_sender.send(false);
                    }
                }
                let status = response.as_ref().ok().map(|result| result.status);
                trace_progressive(
                    trace_enabled,
                    capture_started,
                    "http-submit-call-complete",
                    &format!(
                        "session_elapsed_ms={} attempt={} http_status={status:?}",
                        elapsed_ms(started),
                        attempt
                    ),
                );
                let completion = match response {
                    Ok(result) => {
                        let (outcome, candidates) = match result.response {
                            shazam_http_lab::ProgressiveHttpResponse::Matched {
                                candidates,
                                retry,
                            } => (RecognitionOutcome::Match { retry }, candidates),
                            shazam_http_lab::ProgressiveHttpResponse::NoMatch(retry) => {
                                (RecognitionOutcome::NoMatch { retry }, Vec::new())
                            }
                            shazam_http_lab::ProgressiveHttpResponse::HttpError {
                                retry, ..
                            } => (RecognitionOutcome::MatcherFailure { retry }, Vec::new()),
                            shazam_http_lab::ProgressiveHttpResponse::ParseFailure { retry } => {
                                (RecognitionOutcome::MatcherFailure { retry }, Vec::new())
                            }
                            shazam_http_lab::ProgressiveHttpResponse::InvalidResponse => {
                                (RecognitionOutcome::InvalidResponse, Vec::new())
                            }
                        };
                        ProgressiveCompletion {
                            attempt,
                            status: Some(result.status),
                            outcome,
                            candidates,
                        }
                    }
                    Err(error) => {
                        let retry = shazam_http_lab::native_retry_for_network_failure_for_tagging_type(
                            tagging_type,
                            &error,
                            job.sample_ms,
                            configuration
                                .manual_timing()
                                .effective_maximum_signature_length_ms(),
                            request_cancelled.load(Ordering::Acquire),
                        );
                        ProgressiveCompletion {
                            attempt,
                            status: None,
                            outcome: retry.map_or(RecognitionOutcome::TransportFailure, |retry| {
                                RecognitionOutcome::MatcherFailure { retry: Some(retry) }
                            }),
                            candidates: Vec::new(),
                        }
                    }
                };
                let _ = sender.send(ProgressiveEvent::Completion(completion));
            });
        if let Err(error) = spawn_result {
            if detached_post_stop_job {
                if let Some(event) = deferred_manual_ui_event.take() {
                    super::emit_manual_ui_event(&event, jsonl_output);
                }
            }
            return Err(error.into());
        }
        if detached_post_stop_job {
            let dispatch_ack_receiver = dispatch_ack_receiver
                .expect("detached Retry has a dispatch acknowledgement receiver");
            super::notify_after_retry_dispatch_ack(
                || {
                    let transport_started = dispatch_ack_receiver.recv().map_err(|_| {
                        io::Error::other("Retry worker exited before transport dispatch")
                    })?;
                    if transport_started {
                        Ok(())
                    } else {
                        Err(io::Error::other(
                            "Retry request failed before transport startup",
                        ))
                    }
                },
                || {
                    if let Some(event) = deferred_manual_ui_event.take() {
                        super::emit_manual_ui_event(&event, jsonl_output);
                    }
                },
            )?;
        }
        active_workers.insert(attempt);
        if detached_post_stop_job {
            session.finish_post_stop_retry_submission();
        }
    }

    if let Some(audio_ack) = audio_ack {
        let _ = audio_ack.send(());
    }

    Ok(())
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn trace_progressive(enabled: bool, capture_started: Instant, event: &str, details: &str) {
    if enabled {
        if details.is_empty() {
            eprintln!(
                "progressive_trace event={event} capture_elapsed_ms={}",
                elapsed_ms(capture_started)
            );
        } else {
            eprintln!(
                "progressive_trace event={event} capture_elapsed_ms={} {details}",
                elapsed_ms(capture_started)
            );
        }
    }
}

fn env_u64_setting(name: &str, default: u64, minimum: u64, maximum: u64) -> io::Result<u64> {
    let value = match std::env::var(name) {
        Ok(value) => value.parse::<u64>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} must be an integer"),
            )
        })?,
        Err(std::env::VarError::NotPresent) => default,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} must be valid Unicode"),
            ));
        }
    };
    if !(minimum..=maximum).contains(&value) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be between {minimum} and {maximum}"),
        ));
    }
    Ok(value)
}

fn env_optional_u64_setting(name: &str, maximum: u64) -> io::Result<Option<u64>> {
    match std::env::var(name) {
        Ok(value) => {
            let value = value.parse::<u64>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{name} must be an integer"),
                )
            })?;
            if value > maximum {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{name} must not exceed {maximum}"),
                ));
            }
            Ok((value != 0).then_some(value))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be valid Unicode"),
        )),
    }
}

fn mac_timezone_name() -> String {
    if let Some(timezone) = std::env::var("TZ")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        return timezone;
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|path| {
            let value = path.to_string_lossy();
            let marker = "/zoneinfo/";
            value
                .find(marker)
                .map(|index| value[index + marker.len()..].to_owned())
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "UTC".into())
}

fn set_fault(slot: &Mutex<Option<String>>, message: &str) {
    if let Ok(mut slot) = slot.lock() {
        if slot.is_none() {
            *slot = Some(message.to_owned());
        }
    }
}

fn take_fault(slot: &Mutex<Option<String>>) -> io::Result<Option<String>> {
    slot.lock()
        .map(|mut slot| slot.take())
        .map_err(|_| io::Error::other("CoreAudio error state lock was poisoned"))
}

fn convert_to_f32(data: &Data) -> Option<Vec<f32>> {
    match data.sample_format() {
        SampleFormat::F32 => convert_samples::<f32>(data),
        SampleFormat::F64 => convert_samples::<f64>(data),
        SampleFormat::I8 => convert_samples::<i8>(data),
        SampleFormat::I16 => convert_samples::<i16>(data),
        SampleFormat::I24 => convert_samples::<cpal::I24>(data),
        SampleFormat::I32 => convert_samples::<i32>(data),
        SampleFormat::I64 => convert_samples::<i64>(data),
        SampleFormat::U8 => convert_samples::<u8>(data),
        SampleFormat::U16 => convert_samples::<u16>(data),
        SampleFormat::U24 => convert_samples::<cpal::U24>(data),
        SampleFormat::U32 => convert_samples::<u32>(data),
        SampleFormat::U64 => convert_samples::<u64>(data),
        SampleFormat::DsdU8 | SampleFormat::DsdU16 | SampleFormat::DsdU32 => None,
        _ => None,
    }
}

fn convert_samples<T>(data: &Data) -> Option<Vec<f32>>
where
    T: SizedSample + Sample,
    f32: FromSample<T>,
{
    data.as_slice::<T>().map(|samples| {
        samples
            .iter()
            .copied()
            .map(<f32 as Sample>::from_sample)
            .collect()
    })
}

fn unix_timestamp_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()
}

/// Approximate a CoreAudio capture instant in Unix time from CPAL's paired
/// monotonic capture/callback timestamps. This is not claimed to be identical
/// to AVAudioTime.hostTime used by Shazam.
fn input_capture_timestamp_ms(info: &cpal::InputCallbackInfo) -> Option<u64> {
    let timestamp = info.timestamp();
    let capture_age = timestamp
        .callback
        .checked_duration_since(timestamp.capture)?;
    SystemTime::now()
        .checked_sub(capture_age)?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()
}

fn advance_timestamp_ms(timestamp_ms: u64, frames: usize, sample_rate: u32) -> Option<u64> {
    if sample_rate == 0 {
        return None;
    }
    let elapsed_ms = (frames as u128)
        .checked_mul(1_000)?
        .checked_div(u128::from(sample_rate))?;
    timestamp_ms.checked_add(u64::try_from(elapsed_ms).ok()?)
}

fn push_timed_preroll(
    ring: &mut RollingPcmBuffer<f32>,
    start_timestamp_ms: &mut Option<u64>,
    samples: &[f32],
    packet_timestamp_ms: Option<u64>,
    sample_rate: u32,
) {
    let old_frames = ring.frames();
    ring.push(samples);
    let discarded_frames = old_frames
        .saturating_add(samples.len())
        .saturating_sub(ring.frames());
    if let Some(start_ms) = *start_timestamp_ms {
        if discarded_frames > 0 {
            *start_timestamp_ms = advance_timestamp_ms(start_ms, discarded_frames, sample_rate);
        }
    } else if old_frames == 0 && !samples.is_empty() {
        *start_timestamp_ms = packet_timestamp_ms.and_then(|packet_start| {
            advance_timestamp_ms(
                packet_start,
                samples.len().saturating_sub(ring.frames()),
                sample_rate,
            )
        });
    }
}

#[cfg(test)]
mod progressive_event_tests {
    use super::*;

    #[test]
    fn preroll_timestamp_advances_when_oldest_audio_is_discarded() {
        let mut ring = RollingPcmBuffer::new(6);
        let mut start_timestamp_ms = None;
        push_timed_preroll(
            &mut ring,
            &mut start_timestamp_ms,
            &[0.0; 4],
            Some(1_000),
            20,
        );
        push_timed_preroll(
            &mut ring,
            &mut start_timestamp_ms,
            &[1.0; 4],
            Some(1_200),
            20,
        );

        assert_eq!(start_timestamp_ms, Some(1_100));
        assert_eq!(ring.frames(), 6);
    }

    #[test]
    fn audio_event_is_fifo_and_waits_for_processing_acknowledgement() {
        let (sender, receiver) = mpsc::channel();
        let pending_audio_packets = Arc::new(AtomicUsize::new(0));
        let queue_overflow = Arc::new(AtomicBool::new(false));
        let callback_fault = Arc::new(Mutex::new(None));
        let stream_stopping = Arc::new(AtomicBool::new(false));
        let producer_finished = Arc::new(AtomicBool::new(false));

        let producer_sender = sender.clone();
        let producer_pending = Arc::clone(&pending_audio_packets);
        let producer_overflow = Arc::clone(&queue_overflow);
        let producer_fault = Arc::clone(&callback_fault);
        let producer_stopping = Arc::clone(&stream_stopping);
        let producer_finished_flag = Arc::clone(&producer_finished);
        let producer = thread::spawn(move || {
            enqueue_audio_event(
                &producer_sender,
                &producer_pending,
                &producer_overflow,
                &producer_fault,
                &producer_stopping,
                vec![0.25, -0.5],
                None,
            );
            producer_finished_flag.store(true, Ordering::Release);
        });

        let ProgressiveEvent::Audio {
            samples,
            source_timestamp_ms,
            processed,
        } = receiver.recv().unwrap()
        else {
            panic!("audio flow must be the first queued generator operation");
        };
        release_audio_packet(&pending_audio_packets);
        assert_eq!(samples, [0.25, -0.5]);
        assert_eq!(source_timestamp_ms, None);
        assert!(!producer_finished.load(Ordering::Acquire));

        sender
            .send(ProgressiveEvent::Completion(ProgressiveCompletion {
                attempt: 1,
                status: None,
                outcome: RecognitionOutcome::TransportFailure,
                candidates: Vec::new(),
            }))
            .unwrap();
        processed.send(()).unwrap();
        producer.join().unwrap();
        assert!(producer_finished.load(Ordering::Acquire));
        assert_eq!(pending_audio_packets.load(Ordering::Acquire), 0);
        assert!(!queue_overflow.load(Ordering::Relaxed));
        assert!(callback_fault.lock().unwrap().is_none());
        assert!(matches!(
            receiver.recv().unwrap(),
            ProgressiveEvent::Completion(ProgressiveCompletion { attempt: 1, .. })
        ));
    }

    #[test]
    fn pending_audio_queue_keeps_the_existing_bound() {
        let pending_audio_packets = AtomicUsize::new(0);
        for _ in 0..PACKET_QUEUE_CAPACITY {
            assert!(reserve_audio_packet(&pending_audio_packets));
        }
        assert!(!reserve_audio_packet(&pending_audio_packets));
        for _ in 0..PACKET_QUEUE_CAPACITY {
            release_audio_packet(&pending_audio_packets);
        }
        assert_eq!(pending_audio_packets.load(Ordering::Acquire), 0);
        assert!(reserve_audio_packet(&pending_audio_packets));
        release_audio_packet(&pending_audio_packets);
    }
}
