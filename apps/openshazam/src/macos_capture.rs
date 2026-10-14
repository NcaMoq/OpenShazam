use super::CaptureAccumulator;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Data, FromSample, Sample, SampleFormat, SizedSample, SupportedBufferSize,
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
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        mpsc::{self, sync_channel, TryRecvError, TrySendError},
        Arc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PACKET_QUEUE_CAPACITY: usize = 8;
const PROGRESSIVE_EVENT_QUEUE_CAPACITY: usize = 32;
const AUDIO_POOL_MEMORY_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const PACKET_TIMEOUT: Duration = Duration::from_secs(30);
const RECOGNITION_QUEUE_TIMEOUT: Duration = Duration::from_millis(25);
type AudioBufferPool = (mpsc::SyncSender<Vec<f32>>, mpsc::Receiver<Vec<f32>>);

#[repr(u8)]
#[derive(Clone, Copy)]
enum CallbackFault {
    None = 0,
    SampleBuffer = 1,
    Device = 2,
    EventQueueClosed = 3,
}

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
    let audio_buffer_pool = make_audio_buffer_pool(supported.buffer_size(), channels);
    let (recycled_buffers, mut available_buffers) = match audio_buffer_pool {
        Some(pool) => (Some(pool.0), Some(pool.1)),
        None => (None, None),
    };
    let callback_fault = Arc::new(AtomicU8::new(CallbackFault::None as u8));
    let stream_fault = Arc::clone(&callback_fault);
    let queue_overflow = Arc::new(AtomicBool::new(false));
    let callback_overflow = Arc::clone(&queue_overflow);
    let stream = device.build_input_stream_raw(
        supported.into(),
        sample_format,
        move |data, _| {
            let samples = if let (Some(pool_sender), Some(pool_receiver)) =
                (&recycled_buffers, &mut available_buffers)
            {
                let Ok(mut buffer) = pool_receiver.try_recv() else {
                    callback_overflow.store(true, Ordering::Relaxed);
                    return;
                };
                if !convert_to_f32_into(data, &mut buffer) {
                    let _ = pool_sender.try_send(buffer);
                    set_fault(&stream_fault, CallbackFault::SampleBuffer);
                    return;
                }
                AudioBuffer::pooled(buffer, pool_sender.clone())
            } else {
                let Some(samples) = convert_to_f32(data) else {
                    set_fault(&stream_fault, CallbackFault::SampleBuffer);
                    return;
                };
                AudioBuffer::owned(samples)
            };
            match sender.try_send(samples) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    callback_overflow.store(true, Ordering::Relaxed);
                }
                Err(TrySendError::Disconnected(_)) => {
                    set_fault(&stream_fault, CallbackFault::EventQueueClosed);
                }
            }
        },
        {
            let stream_error = Arc::clone(&callback_fault);
            move |_| set_fault(&stream_error, CallbackFault::Device)
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
        if let Some(error) = take_fault(&callback_fault) {
            return Err(io::Error::other(error).into());
        }
        match receiver.recv_timeout(PACKET_TIMEOUT) {
            Ok(samples) => accumulator.push_interleaved_f32(&samples.samples, channels)?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(error) = take_fault(&callback_fault) {
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

    let (progressive_sender, progressive_receiver) =
        mpsc::sync_channel(PROGRESSIVE_EVENT_QUEUE_CAPACITY);
    let audio_buffer_pool = make_audio_buffer_pool(supported.buffer_size(), channels);
    let (recycled_buffers, mut available_buffers) = match audio_buffer_pool {
        Some(pool) => (Some(pool.0), Some(pool.1)),
        None => (None, None),
    };
    let pending_audio_packets = Arc::new(AtomicUsize::new(0));
    let callback_pending_audio = Arc::clone(&pending_audio_packets);
    let callback_fault = Arc::new(AtomicU8::new(CallbackFault::None as u8));
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
            let samples = if let (Some(pool_sender), Some(pool_receiver)) =
                (&recycled_buffers, &mut available_buffers)
            {
                let Ok(mut buffer) = pool_receiver.try_recv() else {
                    callback_overflow.store(true, Ordering::Relaxed);
                    return;
                };
                if !convert_to_f32_into(data, &mut buffer) {
                    let _ = pool_sender.try_send(buffer);
                    set_fault(&stream_fault, CallbackFault::SampleBuffer);
                    return;
                }
                AudioBuffer::pooled(buffer, pool_sender.clone())
            } else {
                let Some(samples) = convert_to_f32(data) else {
                    set_fault(&stream_fault, CallbackFault::SampleBuffer);
                    return;
                };
                AudioBuffer::owned(samples)
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
            move |_| set_fault(&stream_error, CallbackFault::Device)
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
        if let Some(error) = take_fault(&callback_fault) {
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
            }) => {
                release_audio_packet(&pending_audio_packets);
                let result = super::downmix_interleaved_f32(&samples.samples, channels);
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
                ProgressiveEvent::Audio { .. } => {
                    release_audio_packet(&pending_audio_packets);
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
            if let Some(error) = take_fault(&callback_fault) {
                return Err(io::Error::other(error).into());
            }

            match trigger_receiver.try_recv() {
                Ok(super::ManualTriggerSignal::Enter) => {
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
                            }) => {
                                release_audio_packet(&pending_audio_packets);
                                let result =
                                    super::downmix_interleaved_f32(&samples.samples, channels);
                                if let Ok(mono) = &result {
                                    push_timed_preroll(
                                        &mut ring,
                                        &mut preroll_start_timestamp_ms,
                                        mono,
                                        source_timestamp_ms,
                                        sample_rate,
                                    );
                                }
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
                Ok(super::ManualTriggerSignal::EndOfInput) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "manual recognition trigger input closed",
                    )
                    .into());
                }
                Ok(super::ManualTriggerSignal::InputError(error)) => {
                    return Err(io::Error::other(error).into());
                }
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
                }) => {
                    release_audio_packet(&pending_audio_packets);
                    let result = super::downmix_interleaved_f32(&samples.samples, channels);
                    if let Ok(mono) = &result {
                        push_timed_preroll(
                            &mut ring,
                            &mut preroll_start_timestamp_ms,
                            mono,
                            source_timestamp_ms,
                            sample_rate,
                        );
                    }
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
            if let Some(error) = take_fault(&callback_fault) {
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
            Ok(ProgressiveEvent::Audio { .. }) => {
                release_audio_packet(&pending_audio_packets);
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
        samples: AudioBuffer,
        source_timestamp_ms: Option<u64>,
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

struct AudioBuffer {
    samples: Vec<f32>,
    recycle: Option<mpsc::SyncSender<Vec<f32>>>,
}

impl AudioBuffer {
    fn owned(samples: Vec<f32>) -> Self {
        Self {
            samples,
            recycle: None,
        }
    }

    fn pooled(samples: Vec<f32>, recycle: mpsc::SyncSender<Vec<f32>>) -> Self {
        Self {
            samples,
            recycle: Some(recycle),
        }
    }
}

impl Drop for AudioBuffer {
    fn drop(&mut self) {
        if let Some(recycle) = &self.recycle {
            self.samples.clear();
            let _ = recycle.try_send(std::mem::take(&mut self.samples));
        }
    }
}

fn make_audio_buffer_pool(
    supported_size: &SupportedBufferSize,
    channels: usize,
) -> Option<AudioBufferPool> {
    let SupportedBufferSize::Range { max, .. } = supported_size else {
        return None;
    };
    let samples_per_buffer = usize::try_from(*max).ok()?.checked_mul(channels)?;
    let total_bytes = samples_per_buffer
        .checked_mul(std::mem::size_of::<f32>())?
        .checked_mul(PACKET_QUEUE_CAPACITY)?;
    if samples_per_buffer == 0 || total_bytes > AUDIO_POOL_MEMORY_LIMIT_BYTES {
        return None;
    }

    let (sender, receiver) = sync_channel(PACKET_QUEUE_CAPACITY);
    for _ in 0..PACKET_QUEUE_CAPACITY {
        let mut buffer = Vec::new();
        buffer.try_reserve_exact(samples_per_buffer).ok()?;
        sender.try_send(buffer).ok()?;
    }
    Some((sender, receiver))
}

fn enqueue_audio_event(
    sender: &mpsc::SyncSender<ProgressiveEvent>,
    pending_audio_packets: &AtomicUsize,
    queue_overflow: &AtomicBool,
    callback_fault: &AtomicU8,
    stream_stopping: &AtomicBool,
    samples: AudioBuffer,
    source_timestamp_ms: Option<u64>,
) {
    if stream_stopping.load(Ordering::Acquire) {
        return;
    }
    if !reserve_audio_packet(pending_audio_packets) {
        queue_overflow.store(true, Ordering::Relaxed);
        return;
    }
    match sender.try_send(ProgressiveEvent::Audio {
        samples,
        source_timestamp_ms,
    }) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            release_audio_packet(pending_audio_packets);
            queue_overflow.store(true, Ordering::Relaxed);
        }
        Err(TrySendError::Disconnected(_)) => {
            release_audio_packet(pending_audio_packets);
            set_fault(callback_fault, CallbackFault::EventQueueClosed);
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
    event_sender: &mpsc::SyncSender<ProgressiveEvent>,
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
            } => {
                release_audio_packet(pending_audio_packets);
                let result = if session.is_terminal() {
                    Ok(())
                } else {
                    (|| -> Result<(), io::Error> {
                        let mono = super::downmix_interleaved_f32(&samples.samples, channels)?;
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

fn set_fault(slot: &AtomicU8, fault: CallbackFault) {
    let _ = slot.compare_exchange(
        CallbackFault::None as u8,
        fault as u8,
        Ordering::AcqRel,
        Ordering::Relaxed,
    );
}

fn take_fault(slot: &AtomicU8) -> Option<&'static str> {
    match slot.swap(CallbackFault::None as u8, Ordering::AcqRel) {
        value if value == CallbackFault::SampleBuffer as u8 => {
            Some("CoreAudio supplied an unsupported or oversized sample buffer")
        }
        value if value == CallbackFault::Device as u8 => {
            Some("CoreAudio input stream reported a device error")
        }
        value if value == CallbackFault::EventQueueClosed as u8 => {
            Some("Progressive event queue closed during capture")
        }
        _ => None,
    }
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

fn convert_to_f32_into(data: &Data, output: &mut Vec<f32>) -> bool {
    if data.len() > output.capacity() {
        return false;
    }
    output.clear();
    match data.sample_format() {
        SampleFormat::F32 => convert_samples_into::<f32>(data, output),
        SampleFormat::F64 => convert_samples_into::<f64>(data, output),
        SampleFormat::I8 => convert_samples_into::<i8>(data, output),
        SampleFormat::I16 => convert_samples_into::<i16>(data, output),
        SampleFormat::I24 => convert_samples_into::<cpal::I24>(data, output),
        SampleFormat::I32 => convert_samples_into::<i32>(data, output),
        SampleFormat::I64 => convert_samples_into::<i64>(data, output),
        SampleFormat::U8 => convert_samples_into::<u8>(data, output),
        SampleFormat::U16 => convert_samples_into::<u16>(data, output),
        SampleFormat::U24 => convert_samples_into::<cpal::U24>(data, output),
        SampleFormat::U32 => convert_samples_into::<u32>(data, output),
        SampleFormat::U64 => convert_samples_into::<u64>(data, output),
        SampleFormat::DsdU8 | SampleFormat::DsdU16 | SampleFormat::DsdU32 => false,
        _ => false,
    }
}

fn convert_samples_into<T>(data: &Data, output: &mut Vec<f32>) -> bool
where
    T: SizedSample + Sample,
    f32: FromSample<T>,
{
    let Some(samples) = data.as_slice::<T>() else {
        return false;
    };
    if samples.len() > output.capacity() {
        return false;
    }
    output.extend(samples.iter().copied().map(<f32 as Sample>::from_sample));
    true
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
    fn audio_event_enqueue_is_nonblocking_and_preserves_fifo() {
        let (sender, receiver) = mpsc::sync_channel(PROGRESSIVE_EVENT_QUEUE_CAPACITY);
        let pending_audio_packets = Arc::new(AtomicUsize::new(0));
        let queue_overflow = Arc::new(AtomicBool::new(false));
        let callback_fault = Arc::new(AtomicU8::new(CallbackFault::None as u8));
        let stream_stopping = Arc::new(AtomicBool::new(false));
        let producer_finished = Arc::new(AtomicBool::new(false));

        let producer_sender = sender.clone();
        let producer_pending = Arc::clone(&pending_audio_packets);
        let producer_overflow = Arc::clone(&queue_overflow);
        let producer_fault = Arc::clone(&callback_fault);
        let producer_stopping = Arc::clone(&stream_stopping);
        let producer_finished_flag = Arc::clone(&producer_finished);
        let producer = thread::spawn(move || {
            for (samples, timestamp) in [(vec![0.25, -0.5], Some(1_000)), (vec![0.75], Some(1_125))]
            {
                enqueue_audio_event(
                    &producer_sender,
                    &producer_pending,
                    &producer_overflow,
                    &producer_fault,
                    &producer_stopping,
                    AudioBuffer::owned(samples),
                    timestamp,
                );
            }
            producer_finished_flag.store(true, Ordering::Release);
        });
        producer.join().unwrap();
        assert!(producer_finished.load(Ordering::Acquire));

        for (expected_samples, expected_timestamp) in
            [(&[0.25, -0.5][..], Some(1_000)), (&[0.75][..], Some(1_125))]
        {
            let ProgressiveEvent::Audio {
                samples,
                source_timestamp_ms,
            } = receiver.recv().unwrap()
            else {
                panic!("audio flow must preserve FIFO packet order");
            };
            assert_eq!(samples.samples, expected_samples);
            assert_eq!(source_timestamp_ms, expected_timestamp);
            release_audio_packet(&pending_audio_packets);
        }
        assert_eq!(pending_audio_packets.load(Ordering::Acquire), 0);
        assert!(!queue_overflow.load(Ordering::Relaxed));
        assert_eq!(
            callback_fault.load(Ordering::Acquire),
            CallbackFault::None as u8
        );
    }

    #[test]
    fn callback_queue_full_is_reported_without_waiting() {
        let (sender, receiver) = mpsc::sync_channel(0);
        let pending_audio_packets = AtomicUsize::new(0);
        let queue_overflow = AtomicBool::new(false);
        let callback_fault = AtomicU8::new(CallbackFault::None as u8);
        let stream_stopping = AtomicBool::new(false);
        let supported = SupportedBufferSize::Range { min: 1, max: 16 };
        let (recycle_sender, recycle_receiver) = make_audio_buffer_pool(&supported, 1).unwrap();
        let mut buffer = recycle_receiver.try_recv().unwrap();
        buffer.extend_from_slice(&[0.5, -0.25]);
        let allocation = buffer.as_ptr();
        let capacity = buffer.capacity();

        enqueue_audio_event(
            &sender,
            &pending_audio_packets,
            &queue_overflow,
            &callback_fault,
            &stream_stopping,
            AudioBuffer::pooled(buffer, recycle_sender.clone()),
            Some(42),
        );

        assert!(queue_overflow.load(Ordering::Relaxed));
        assert_eq!(pending_audio_packets.load(Ordering::Acquire), 0);
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        assert_eq!(
            callback_fault.load(Ordering::Acquire),
            CallbackFault::None as u8
        );
        let recycled: Vec<_> = (0..PACKET_QUEUE_CAPACITY)
            .map(|_| recycle_receiver.try_recv().unwrap())
            .collect();
        assert!(recycled.iter().all(Vec::is_empty));
        assert!(recycled.iter().all(|buffer| buffer.capacity() == capacity));
        assert!(recycled.iter().any(|buffer| buffer.as_ptr() == allocation));
    }

    #[test]
    fn stopped_callback_drops_event_and_recycles_its_audio_buffer() {
        let (sender, receiver) = mpsc::sync_channel(PROGRESSIVE_EVENT_QUEUE_CAPACITY);
        let pending_audio_packets = AtomicUsize::new(0);
        let queue_overflow = AtomicBool::new(false);
        let callback_fault = AtomicU8::new(CallbackFault::None as u8);
        let stream_stopping = AtomicBool::new(true);
        let supported = SupportedBufferSize::Range { min: 1, max: 16 };
        let (recycle_sender, recycle_receiver) = make_audio_buffer_pool(&supported, 1).unwrap();
        let buffer = recycle_receiver.try_recv().unwrap();
        let allocation = buffer.as_ptr();

        enqueue_audio_event(
            &sender,
            &pending_audio_packets,
            &queue_overflow,
            &callback_fault,
            &stream_stopping,
            AudioBuffer::pooled(buffer, recycle_sender.clone()),
            Some(43),
        );

        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        assert_eq!(pending_audio_packets.load(Ordering::Acquire), 0);
        assert!(!queue_overflow.load(Ordering::Relaxed));
        assert_eq!(
            callback_fault.load(Ordering::Acquire),
            CallbackFault::None as u8
        );
        let recycled: Vec<_> = (0..PACKET_QUEUE_CAPACITY)
            .map(|_| recycle_receiver.try_recv().unwrap())
            .collect();
        assert!(recycled.iter().all(Vec::is_empty));
        assert!(recycled.iter().any(|buffer| buffer.as_ptr() == allocation));
    }

    #[test]
    fn disconnected_event_queue_faults_without_leaking_pooled_audio_buffer() {
        let (sender, receiver) = mpsc::sync_channel(PROGRESSIVE_EVENT_QUEUE_CAPACITY);
        drop(receiver);
        let pending_audio_packets = AtomicUsize::new(0);
        let queue_overflow = AtomicBool::new(false);
        let callback_fault = AtomicU8::new(CallbackFault::None as u8);
        let stream_stopping = AtomicBool::new(false);
        let supported = SupportedBufferSize::Range { min: 1, max: 16 };
        let (recycle_sender, recycle_receiver) = make_audio_buffer_pool(&supported, 1).unwrap();
        let buffer = recycle_receiver.try_recv().unwrap();
        let allocation = buffer.as_ptr();

        enqueue_audio_event(
            &sender,
            &pending_audio_packets,
            &queue_overflow,
            &callback_fault,
            &stream_stopping,
            AudioBuffer::pooled(buffer, recycle_sender.clone()),
            Some(44),
        );

        assert_eq!(pending_audio_packets.load(Ordering::Acquire), 0);
        assert!(!queue_overflow.load(Ordering::Relaxed));
        assert_eq!(
            callback_fault.load(Ordering::Acquire),
            CallbackFault::EventQueueClosed as u8
        );
        let recycled: Vec<_> = (0..PACKET_QUEUE_CAPACITY)
            .map(|_| recycle_receiver.try_recv().unwrap())
            .collect();
        assert!(recycled.iter().all(Vec::is_empty));
        assert!(recycled.iter().any(|buffer| buffer.as_ptr() == allocation));
    }

    #[test]
    fn concurrent_audio_and_completion_producers_keep_audio_fifo_and_accounting() {
        const AUDIO_PACKETS: u64 = 2_048;
        const COMPLETIONS: usize = 128;
        let (sender, receiver) = mpsc::sync_channel(8);
        let pending_audio_packets = Arc::new(AtomicUsize::new(0));
        let queue_overflow = Arc::new(AtomicBool::new(false));
        let callback_fault = Arc::new(AtomicU8::new(CallbackFault::None as u8));
        let stream_stopping = Arc::new(AtomicBool::new(false));
        let start = Arc::new(std::sync::Barrier::new(3));

        let audio_sender = sender.clone();
        let audio_pending = Arc::clone(&pending_audio_packets);
        let audio_overflow = Arc::clone(&queue_overflow);
        let audio_fault = Arc::clone(&callback_fault);
        let audio_stopping = Arc::clone(&stream_stopping);
        let audio_start = Arc::clone(&start);

        let completion_sender = sender.clone();
        let completion_start = Arc::clone(&start);
        drop(sender);

        thread::scope(|scope| {
            let audio = scope.spawn(move || {
                audio_start.wait();
                for sequence in 0..AUDIO_PACKETS {
                    enqueue_audio_event(
                        &audio_sender,
                        &audio_pending,
                        &audio_overflow,
                        &audio_fault,
                        &audio_stopping,
                        AudioBuffer::owned(vec![sequence as f32]),
                        Some(sequence),
                    );
                }
            });
            let completions = scope.spawn(move || {
                completion_start.wait();
                for attempt in 0..COMPLETIONS {
                    completion_sender
                        .send(ProgressiveEvent::Completion(ProgressiveCompletion {
                            attempt: attempt as u64,
                            status: None,
                            outcome: RecognitionOutcome::InvalidResponse,
                            candidates: Vec::new(),
                        }))
                        .unwrap();
                }
            });

            start.wait();
            let mut previous_audio_sequence = None;
            let mut delivered_audio_packets = 0usize;
            let mut audio_payloads_match = true;
            let mut audio_fifo_preserved = true;
            let mut completion_payloads_match = true;
            let mut duplicate_or_invalid_completion = false;
            let mut seen_completions = vec![false; COMPLETIONS];
            while let Ok(event) = receiver.recv() {
                match event {
                    ProgressiveEvent::Audio {
                        samples,
                        source_timestamp_ms,
                    } => {
                        let sequence = source_timestamp_ms.unwrap();
                        audio_payloads_match &= samples.samples.as_slice() == [sequence as f32];
                        audio_fifo_preserved &=
                            previous_audio_sequence.is_none_or(|previous| previous < sequence);
                        previous_audio_sequence = Some(sequence);
                        delivered_audio_packets += 1;
                        release_audio_packet(&pending_audio_packets);
                    }
                    ProgressiveEvent::Completion(completion) => {
                        completion_payloads_match &=
                            completion.outcome == RecognitionOutcome::InvalidResponse;
                        if let Some(seen) = seen_completions.get_mut(completion.attempt as usize) {
                            duplicate_or_invalid_completion |= *seen;
                            *seen = true;
                        } else {
                            duplicate_or_invalid_completion = true;
                        }
                    }
                }
            }
            audio.join().unwrap();
            completions.join().unwrap();

            assert!(delivered_audio_packets <= AUDIO_PACKETS as usize);
            assert!(audio_payloads_match);
            assert!(audio_fifo_preserved);
            assert!(completion_payloads_match);
            assert!(!duplicate_or_invalid_completion);
            if delivered_audio_packets < AUDIO_PACKETS as usize {
                assert!(queue_overflow.load(Ordering::Relaxed));
            }
            assert!(seen_completions.into_iter().all(|seen| seen));
        });

        assert_eq!(pending_audio_packets.load(Ordering::Acquire), 0);
        assert_eq!(
            callback_fault.load(Ordering::Acquire),
            CallbackFault::None as u8
        );
    }

    #[test]
    fn audio_buffer_pool_recycles_preallocated_storage() {
        let supported = SupportedBufferSize::Range { min: 1, max: 64 };
        let (sender, receiver) = make_audio_buffer_pool(&supported, 2).unwrap();
        let buffer = receiver.try_recv().unwrap();
        let capacity = buffer.capacity();
        let allocation = buffer.as_ptr();
        assert!(capacity >= 128);

        drop(AudioBuffer::pooled(buffer, sender.clone()));
        let recycled: Vec<_> = (0..PACKET_QUEUE_CAPACITY)
            .map(|_| receiver.try_recv().unwrap())
            .collect();
        assert!(recycled.iter().all(|buffer| buffer.capacity() == capacity));
        assert!(recycled.iter().any(|buffer| buffer.as_ptr() == allocation));
        assert!(make_audio_buffer_pool(&SupportedBufferSize::Unknown, 2).is_none());
    }

    #[test]
    fn nonblocking_audio_handoff_produces_the_same_signature_as_single_input() {
        use sigx_core::{Config, SigX};

        const SAMPLE_RATE: u32 = 16_000;
        const FRAMES: usize = SAMPLE_RATE as usize * 3;
        let samples: Vec<f32> = (0..FRAMES)
            .map(|index| {
                let state = (index as u32)
                    .wrapping_mul(1_103_515_245)
                    .wrapping_add(12_345);
                ((state >> 16) as i16) as f32 / 32_768.0
            })
            .collect();
        let config = Config {
            sample_rate: SAMPLE_RATE,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: FRAMES,
        };
        let mut direct = SigX::new(config).unwrap();
        direct.push_f32(&samples).unwrap();
        let expected = direct.signature().unwrap();

        let (sender, receiver) = mpsc::sync_channel(PROGRESSIVE_EVENT_QUEUE_CAPACITY);
        let pending_audio_packets = AtomicUsize::new(0);
        let queue_overflow = AtomicBool::new(false);
        let callback_fault = AtomicU8::new(CallbackFault::None as u8);
        let stream_stopping = AtomicBool::new(false);
        let mut start = 0;
        for chunk_len in [5_137, 7_003, 9_011, 6_281, 8_417, 12_151] {
            let end = (start + chunk_len).min(FRAMES);
            if end == start {
                break;
            }
            enqueue_audio_event(
                &sender,
                &pending_audio_packets,
                &queue_overflow,
                &callback_fault,
                &stream_stopping,
                AudioBuffer::owned(samples[start..end].to_vec()),
                None,
            );
            start = end;
        }
        if start < FRAMES {
            enqueue_audio_event(
                &sender,
                &pending_audio_packets,
                &queue_overflow,
                &callback_fault,
                &stream_stopping,
                AudioBuffer::owned(samples[start..].to_vec()),
                None,
            );
        }
        assert!(!queue_overflow.load(Ordering::Relaxed));

        let mut packetized = SigX::new(config).unwrap();
        while let Ok(ProgressiveEvent::Audio { samples, .. }) = receiver.try_recv() {
            packetized.push_f32(&samples.samples).unwrap();
            release_audio_packet(&pending_audio_packets);
        }
        assert_eq!(pending_audio_packets.load(Ordering::Acquire), 0);
        assert_eq!(packetized.signature().unwrap(), expected);
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
