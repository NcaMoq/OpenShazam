use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::{Command, ExitCode},
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(any(test, not(windows)))]
use std::path::Path;

use serde_json::{json, Value};
use shazam_progressive::{
    NativeTaggingType, ProgressiveSession, RecognitionOutcome, SessionStatus,
    NATIVE_NETWORK_REQUEST_TIMEOUT_MS, NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS,
};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        usage();
        return ExitCode::from(2);
    };
    if command == "pcm-mock" {
        return run_pcm_mock(args.collect());
    }
    if command == "audio-mock" {
        return run_audio(args.collect(), false);
    }
    if command == "audio-live" {
        return run_audio(args.collect(), true);
    }
    if command == "audio-progressive-live" {
        return run_audio_progressive(args.collect());
    }
    if command == "signature-live" {
        return run_signature_live(args.collect());
    }
    if command == "config-check" {
        if args.next().is_some() {
            eprintln!("usage: shazam-http-lab config-check");
            return ExitCode::from(2);
        }
        return run_config_check();
    }
    if command != "mock-roundtrip" {
        usage();
        return ExitCode::from(2);
    }
    let Some(signature_path) = args.next() else {
        usage();
        return ExitCode::from(2);
    };
    let sample_ms = match args.next().and_then(|value| value.parse::<u64>().ok()) {
        Some(value) => value,
        None => {
            eprintln!("sample duration must be an integer number of milliseconds");
            return ExitCode::from(2);
        }
    };
    if args.next().is_some() {
        usage();
        return ExitCode::from(2);
    }

    let signature = match fs::read(&signature_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read signature {signature_path}: {error}");
            return ExitCode::from(2);
        }
    };
    mock_signature(&signature, sample_ms)
}

fn run_pcm_mock(args: Vec<String>) -> ExitCode {
    if args.len() != 2 {
        eprintln!("usage: shazam-http-lab pcm-mock <mono-i16le.pcm> <sample-rate-hz>");
        return ExitCode::from(2);
    }
    let sample_rate = match args[1].parse::<u32>() {
        Ok(rate) => rate,
        Err(_) => {
            eprintln!("sample rate must be an integer in Hz");
            return ExitCode::from(2);
        }
    };
    let bytes = match fs::read(&args[0]) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read PCM {}: {error}", args[0]);
            return ExitCode::from(2);
        }
    };
    if bytes.len() % 2 != 0 {
        eprintln!("PCM input must contain an even number of bytes (Int16 LE)");
        return ExitCode::from(2);
    }
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    if samples.is_empty() {
        eprintln!("PCM input must contain at least one sample");
        return ExitCode::from(2);
    }
    run_samples(samples, sample_rate, false)
}

fn run_audio(args: Vec<String>, live: bool) -> ExitCode {
    if args.len() != 2 {
        eprintln!("usage: shazam-http-lab audio-mock <audio-file> <seconds>");
        return ExitCode::from(2);
    }
    let duration_seconds = match args[1].parse::<u32>() {
        Ok(seconds @ 1..=30) => seconds,
        _ => {
            eprintln!("duration must be an integer from 1 to 30 seconds");
            return ExitCode::from(2);
        }
    };
    let input_path = &args[0];
    let (samples, sample_rate) = match decode_audio_file(input_path, duration_seconds) {
        Ok(decoded) => decoded,
        Err(error) => {
            eprintln!("could not decode audio input: {error}");
            return ExitCode::from(1);
        }
    };
    eprintln!(
        "Decoded {duration_seconds}s to mono Int16 at the source rate ({sample_rate} Hz); DSP remains Pure Rust."
    );
    run_samples(samples, sample_rate, live)
}

fn decode_audio_file(input_path: &str, duration_seconds: u32) -> Result<(Vec<i16>, u32), String> {
    let probe = match Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=sample_rate",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(input_path)
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(_) => return Err("ffprobe could not read an audio stream".into()),
        Err(error) => {
            return Err(format!("could not start ffprobe: {error}"));
        }
    };
    let rate_text = String::from_utf8_lossy(&probe.stdout);
    let sample_rate = match rate_text.trim().parse::<u32>() {
        Ok(rate) => rate,
        Err(_) => return Err("ffprobe returned an unsupported sample-rate value".into()),
    };
    let decoded = match Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(input_path)
        .args([
            "-map",
            "0:a:0",
            "-t",
            &duration_seconds.to_string(),
            "-ac",
            "1",
            "-ar",
        ])
        .arg(sample_rate.to_string())
        .args(["-f", "s16le", "-acodec", "pcm_s16le", "pipe:1"])
        .output()
    {
        Ok(output) if output.status.success() => output.stdout,
        Ok(_) => return Err("ffmpeg failed to decode the requested audio segment".into()),
        Err(error) => {
            return Err(format!("could not start ffmpeg: {error}"));
        }
    };
    if decoded.len() % 2 != 0 {
        return Err("ffmpeg returned a partial Int16 sample".into());
    }
    let samples: Vec<i16> = decoded
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    if samples.is_empty() {
        return Err("decoded audio segment contains no samples".into());
    }
    Ok((samples, sample_rate))
}

fn run_audio_progressive(args: Vec<String>) -> ExitCode {
    if args.len() != 2 {
        eprintln!("usage: shazam-http-lab audio-progressive-live <audio-file> <seconds>");
        return ExitCode::from(2);
    }
    let duration_seconds = match args[1].parse::<u32>() {
        Ok(seconds @ 1..=30) => seconds,
        _ => {
            eprintln!("duration must be an integer from 1 to 30 seconds");
            return ExitCode::from(2);
        }
    };
    let (samples, sample_rate) = match decode_audio_file(&args[0], duration_seconds) {
        Ok(decoded) => decoded,
        Err(error) => {
            eprintln!("could not decode audio input: {error}");
            return ExitCode::from(1);
        }
    };
    run_progressive_samples(samples, sample_rate)
}

#[derive(Debug, Clone, Copy)]
struct ProgressiveLocalPolicy {
    default_retry_interval_ms: u64,
    maximum_retry_delay_ms: Option<u64>,
    request_timeout_ms: u64,
    maximum_attempts: Option<u64>,
}

fn progressive_local_policy(
    timing: shazam_http_lab::ManualTaggingTiming,
) -> Result<ProgressiveLocalPolicy, String> {
    timing
        .progressive_ms
        .or(timing.sample_ms)
        .ok_or_else(|| "Config has no initial sample target".to_owned())?;
    let maximum_retry_delay_ms =
        env_optional_u64_setting("OPENSHZAM_PROGRESSIVE_RETRY_DELAY_CAP_MS", 0, 600_000)?;
    // Ordinary manual recognition is TaggingType 1; its native default
    // interval is zero. Auto Shazam's Type 2 interval is 12 seconds.
    let default_retry_interval_ms = NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS;
    let request_timeout_ms = env_u64_setting(
        "OPENSHZAM_PROGRESSIVE_REQUEST_TIMEOUT_MS",
        NATIVE_NETWORK_REQUEST_TIMEOUT_MS,
        1,
        600_000,
    )?;
    let maximum_attempts =
        env_optional_u64_setting("OPENSHZAM_PROGRESSIVE_MAX_ATTEMPTS", 1, u64::MAX)?;

    Ok(ProgressiveLocalPolicy {
        default_retry_interval_ms,
        maximum_retry_delay_ms,
        request_timeout_ms,
        maximum_attempts,
    })
}

fn env_u64_setting(name: &str, default: u64, minimum: u64, maximum: u64) -> Result<u64, String> {
    match env::var(name) {
        Ok(value) => {
            let parsed = value
                .parse::<u64>()
                .map_err(|_| format!("{name} must be an unsigned integer"))?;
            if !(minimum..=maximum).contains(&parsed) {
                return Err(format!("{name} must be between {minimum} and {maximum}"));
            }
            Ok(parsed)
        }
        Err(env::VarError::NotPresent) => Ok(default),
        Err(env::VarError::NotUnicode(_)) => Err(format!("{name} is not valid Unicode")),
    }
}

fn env_optional_u64_setting(name: &str, minimum: u64, maximum: u64) -> Result<Option<u64>, String> {
    match env::var(name) {
        Ok(value) => {
            let parsed = value
                .parse::<u64>()
                .map_err(|_| format!("{name} must be an unsigned integer"))?;
            if !(minimum..=maximum).contains(&parsed) {
                return Err(format!("{name} must be between {minimum} and {maximum}"));
            }
            Ok(Some(parsed))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(format!("{name} is not valid Unicode")),
    }
}

fn run_progressive_samples(samples: Vec<i16>, sample_rate: u32) -> ExitCode {
    let profile = match config_request_profile_from_host() {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("cannot identify this host for Config: {error}");
            return ExitCode::from(1);
        }
    };
    let configuration = match shazam_http_lab::fetch_shazam_config(&profile) {
        Ok(configuration) => configuration,
        Err(error) => {
            eprintln!("could not fetch Shazam Config: {error}");
            return ExitCode::from(1);
        }
    };
    let mut native_frame_targets = match configuration.manual_native_frame_targets(sample_rate) {
        Ok(targets) => targets,
        Err(error) => {
            eprintln!("Config has no usable Native frame targets: {error}");
            return ExitCode::from(1);
        }
    };
    let local_policy = match progressive_local_policy(configuration.manual_timing()) {
        Ok(policy) => policy,
        Err(error) => {
            eprintln!("invalid local progressive settings: {error}");
            return ExitCode::from(1);
        }
    };
    let mut timing = match configuration.manual_timing().recognition_timing(
        sample_rate,
        shazam_http_lab::NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
        local_policy.default_retry_interval_ms,
        local_policy.request_timeout_ms,
        local_policy.maximum_retry_delay_ms,
        local_policy.maximum_attempts,
    ) {
        Ok(timing) => timing,
        Err(error) => {
            eprintln!("Config has no usable progressive timing values: {error}");
            return ExitCode::from(1);
        }
    };
    let available_ms = (samples.len() as u128 * 1_000 / u128::from(sample_rate)) as u64;
    timing.maximum_sample_ms = timing.maximum_sample_ms.min(available_ms);
    native_frame_targets.maximum_frames = native_frame_targets.maximum_frames.min(samples.len());
    if native_frame_targets.initial_frames > native_frame_targets.maximum_frames {
        eprintln!("audio prefix is shorter than Config's initial sample target");
        return ExitCode::from(1);
    }
    let pre_record_capacity_ms = match env_optional_u64_setting(
        "OPENSHZAM_PRERECORD_BUFFER_MS",
        0,
        timing.maximum_sample_ms,
    ) {
        Ok(value) => value.unwrap_or(0),
        Err(error) => {
            eprintln!("invalid pre-record buffer setting: {error}");
            return ExitCode::from(1);
        }
    };
    let pre_record_capacity_frames = match frames_for_ms(sample_rate, pre_record_capacity_ms) {
        Ok(frames) => frames,
        Err(error) => {
            eprintln!("invalid pre-record buffer duration: {error}");
            return ExitCode::from(1);
        }
    };
    let mut session = match ProgressiveSession::new_for_type_with_native_frame_targets(
        timing,
        NativeTaggingType::Manual,
        pre_record_capacity_frames,
        Some(native_frame_targets),
    ) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("could not initialize progressive recognition: {error}");
            return ExitCode::from(1);
        }
    };
    let session_start_timestamp_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(timestamp) => timestamp.as_millis().min(u128::from(u64::MAX)) as u64,
        Err(_) => {
            eprintln!("system clock is before the Unix epoch");
            return ExitCode::from(1);
        }
    };
    if let Err(error) = session.set_session_start_timestamp_ms(session_start_timestamp_ms) {
        eprintln!("could not set Progressive session date: {error}");
        return ExitCode::from(1);
    }
    println!(
        "progressive session: sample_rate_hz={sample_rate} first_signature_threshold_ms={} maximum_sample_ms={} inter_cycle_prerecord_capacity_ms={pre_record_capacity_ms} native_session_timeout_ms={:?} local_maximum_attempts={:?} elapsed_origin=after_config_and_decode",
        timing.initial_sample_ms,
        timing.maximum_sample_ms,
        timing.session_timeout_ms,
        timing.maximum_attempts
    );
    let started = Instant::now();
    let chunk_frames = (sample_rate as usize / 10).max(1);
    let (completion_sender, completion_receiver) = std::sync::mpsc::channel();
    let mut runtime = ProgressiveWorkerRuntime {
        started,
        request_timeout_ms: timing.request_timeout_ms,
        attempts: 0,
        matched: false,
        active_workers: BTreeSet::new(),
        completion_sender,
    };
    let mut offset = 0;

    while offset < samples.len() {
        if session.is_terminal() {
            break;
        }
        let end = (offset + chunk_frames).min(samples.len());
        // Do not hand PCM to SigX before its represented source time. Feeding
        // a chunk and then sleeping would make generateSignature() appear up
        // to one chunk early even if HTTP dispatch were delayed until the end
        // of that chunk.
        let target_elapsed_ms = match progressive_source_time_ms(end, sample_rate) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("invalid progressive audio clock: {error}");
                return ExitCode::from(1);
            }
        };
        while elapsed_ms(runtime.started) < target_elapsed_ms && !session.is_terminal() {
            let remaining = target_elapsed_ms.saturating_sub(elapsed_ms(runtime.started));
            let wait = Duration::from_millis(remaining.min(25));
            if !runtime.active_workers.is_empty() {
                match completion_receiver.recv_timeout(wait) {
                    Ok(completion) => {
                        if let Err(error) = handle_progressive_completion(
                            completion,
                            &mut session,
                            &configuration,
                            &mut runtime,
                        ) {
                            eprintln!("progressive response could not be applied: {error}");
                            return ExitCode::from(1);
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if let Err(error) = dispatch_ready_progressive_jobs(
                            &mut session,
                            &configuration,
                            &mut runtime,
                        ) {
                            eprintln!("progressive timeout check failed: {error}");
                            return ExitCode::from(1);
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        eprintln!("recognition worker stopped without a result");
                        fail_disconnected_worker(
                            &mut session,
                            elapsed_ms(runtime.started),
                            &mut runtime.attempts,
                        );
                        runtime.active_workers.clear();
                    }
                }
            } else {
                thread::sleep(wait);
            }
        }
        if session.is_terminal() {
            break;
        }
        let source_elapsed_ms = match progressive_source_time_ms(offset, sample_rate) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("invalid progressive audio clock: {error}");
                return ExitCode::from(1);
            }
        };
        let source_timestamp_ms = match session_start_timestamp_ms.checked_add(source_elapsed_ms) {
            Some(value) => value,
            None => {
                eprintln!("progressive source timestamp overflow");
                return ExitCode::from(1);
            }
        };
        if let Err(error) = session.push_i16_at_with_source_timestamp(
            &samples[offset..end],
            elapsed_ms(runtime.started),
            Some(source_timestamp_ms),
        ) {
            eprintln!("progressive PCM input failed: {error}");
            return ExitCode::from(1);
        }
        // The source keeps advancing independently of an outstanding HTTP
        // request. A cycle that has reached its Config frame ceiling still
        // consumes later file audio just as a live capture source would.
        offset = end;
        if let Err(error) =
            dispatch_ready_progressive_jobs(&mut session, &configuration, &mut runtime)
        {
            eprintln!("could not dispatch progressive recognition: {error}");
            return ExitCode::from(1);
        }
    }

    session.finish_input();
    loop {
        if !runtime.active_workers.is_empty() {
            match completion_receiver.recv_timeout(Duration::from_millis(25)) {
                Ok(completion) => {
                    if let Err(error) = handle_progressive_completion(
                        completion,
                        &mut session,
                        &configuration,
                        &mut runtime,
                    ) {
                        eprintln!("progressive response could not be applied: {error}");
                        return ExitCode::from(1);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if let Err(error) =
                        dispatch_ready_progressive_jobs(&mut session, &configuration, &mut runtime)
                    {
                        eprintln!("progressive timeout check failed: {error}");
                        return ExitCode::from(1);
                    }
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    eprintln!("recognition worker stopped without a result");
                    fail_disconnected_worker(
                        &mut session,
                        elapsed_ms(runtime.started),
                        &mut runtime.attempts,
                    );
                    runtime.active_workers.clear();
                    break;
                }
            }
            continue;
        }

        if session.is_terminal() {
            break;
        }
        let now_ms = elapsed_ms(runtime.started);
        match dispatch_ready_progressive_jobs(&mut session, &configuration, &mut runtime) {
            Ok(()) => match session.status() {
                SessionStatus::WaitingForRetry { not_before_ms, .. } if now_ms < not_before_ms => {
                    thread::sleep(Duration::from_millis((not_before_ms - now_ms).min(25)));
                }
                SessionStatus::InFlight { .. } => {
                    if session.in_flight_attempts().is_empty() {
                        eprintln!("session reports an in-flight request without an attempt ID");
                        break;
                    }
                    eprintln!("session has in-flight attempts without HTTP workers");
                    break;
                }
                _ => break,
            },
            Err(error) => {
                eprintln!("progressive session failed: {error}");
                return ExitCode::from(1);
            }
        }
    }

    if runtime.matched {
        ExitCode::SUCCESS
    } else {
        println!(
            "no match after {} attempt(s); accepted sample_ms={}",
            runtime.attempts,
            session.sample_ms()
        );
        ExitCode::from(1)
    }
}

fn frames_for_ms(sample_rate: u32, duration_ms: u64) -> Result<usize, String> {
    let frames = u64::from(sample_rate)
        .checked_mul(duration_ms)
        .and_then(|value| value.checked_add(999))
        .ok_or_else(|| "pre-record duration overflows frame count".to_owned())?
        / 1_000;
    usize::try_from(frames).map_err(|_| "pre-record duration exceeds addressable PCM".to_owned())
}

fn progressive_source_time_ms(frames: usize, sample_rate: u32) -> Result<u64, String> {
    if sample_rate == 0 {
        return Err("sample rate must be nonzero".to_owned());
    }
    let rate = u128::from(sample_rate);
    let elapsed_ms = (frames as u128)
        .checked_mul(1_000)
        .and_then(|numerator| numerator.checked_add(rate - 1))
        .ok_or_else(|| "progressive audio clock overflow".to_owned())?
        / rate;
    u64::try_from(elapsed_ms).map_err(|_| "progressive audio clock exceeds u64".to_owned())
}

#[derive(Debug)]
struct ProgressiveAttemptResult {
    status: Option<u16>,
    outcome: RecognitionOutcome,
    retry: Option<shazam_progressive::RetryDirective>,
    candidates: Vec<shazam_http_lab::TrackCandidate>,
    error: Option<&'static str>,
    post_started_ms: Option<u64>,
    response_received_ms: Option<u64>,
}

struct ProgressiveCompletion {
    attempt: u64,
    sample_ms: u64,
    signature_ready_ms: u64,
    detached: bool,
    result: ProgressiveAttemptResult,
}

struct ProgressiveWorkerRuntime {
    started: Instant,
    request_timeout_ms: u64,
    attempts: u64,
    matched: bool,
    active_workers: BTreeSet<u64>,
    completion_sender: std::sync::mpsc::Sender<ProgressiveCompletion>,
}

fn await_post_stop_transport_start(
    dispatch: Result<Option<std::sync::mpsc::Receiver<bool>>, String>,
) -> Result<(), String> {
    let receiver = dispatch?
        .ok_or_else(|| "post-stop Retry dispatch acknowledgement was not created".to_owned())?;
    match receiver.recv() {
        Ok(true) => Ok(()),
        Ok(false) => Err("request preparation failed before HTTPS client startup".to_owned()),
        Err(_) => Err("post-stop Retry worker exited before transport dispatch".to_owned()),
    }
}

fn dispatch_progressive_job(
    configuration: &shazam_http_lab::ShazamConfiguration,
    job: shazam_progressive::RecognitionJob,
    session_started: Instant,
    request_timeout_ms: u64,
    sender: std::sync::mpsc::Sender<ProgressiveCompletion>,
    wait_for_transport_start: bool,
) -> Result<Option<std::sync::mpsc::Receiver<bool>>, String> {
    let attempt = job.attempt;
    let sample_ms = job.sample_ms;
    let signature_ready_ms = job.generated_at_ms;
    let configuration = (*configuration).clone();
    let (dispatch_sender, dispatch_receiver) = if wait_for_transport_start {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        (Some(sender), Some(receiver))
    } else {
        (None, None)
    };
    thread::Builder::new()
        .name(format!("openshazam-recognition-{attempt}"))
        .spawn(move || {
            let mut transport_started = false;
            let result = submit_progressive_job(
                &configuration,
                &job,
                session_started,
                request_timeout_ms,
                dispatch_sender.as_ref(),
                &mut transport_started,
            );
            if let Some(dispatch_sender) = &dispatch_sender {
                if !transport_started {
                    let _ = dispatch_sender.send(false);
                }
            }
            let _ = sender.send(ProgressiveCompletion {
                attempt,
                sample_ms,
                signature_ready_ms,
                detached: wait_for_transport_start,
                result,
            });
        })
        .map(|_| dispatch_receiver)
        .map_err(|_| "could not start the bounded recognition worker".to_owned())
}

fn submit_progressive_job(
    configuration: &shazam_http_lab::ShazamConfiguration,
    job: &shazam_progressive::RecognitionJob,
    session_started: Instant,
    request_timeout_ms: u64,
    dispatch_sender: Option<&std::sync::mpsc::SyncSender<bool>>,
    transport_started: &mut bool,
) -> ProgressiveAttemptResult {
    let failed = |error| ProgressiveAttemptResult {
        status: None,
        outcome: RecognitionOutcome::TransportFailure,
        retry: None,
        candidates: Vec::new(),
        error: Some(error),
        post_started_ms: None,
        response_received_ms: None,
    };
    let tag_id = match random_uuid_v4() {
        Ok(tag_id) => tag_id,
        Err(_) => return failed("could not create a request identifier"),
    };
    let streaming_provider = env::var("SHAZAM_STREAMING_PROVIDER").unwrap_or_default();
    let endpoint = match (env::var("SHAZAM_TAG_URL_TEMPLATE"), env::var("SHAZAM_INID")) {
        (Ok(template), Ok(installation_id)) => shazam_http_lab::resolve_shazam_tag_url(
            &template,
            &installation_id,
            &tag_id,
            &streaming_provider,
        ),
        (Err(_), Err(_)) => configuration.resolve_tag_url(&tag_id, &streaming_provider),
        _ => return failed("manual recognition route overrides must be paired"),
    };
    let endpoint = match endpoint {
        Ok(endpoint) => endpoint,
        Err(_) => return failed("could not resolve an authorized recognition route"),
    };
    let timestamp_ms = match job.signature_timestamp_ms {
        Some(timestamp_ms) => timestamp_ms,
        None => match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(timestamp) => timestamp.as_millis().min(u128::from(u64::MAX)) as u64,
            Err(_) => return failed("system clock is before the Unix epoch"),
        },
    };
    let timezone = local_timezone_name();
    let body = match shazam_http_lab::build_request_body(
        &job.signature,
        job.sample_ms,
        timestamp_ms,
        &timezone,
        json!({}),
        json!({}),
        None,
    ) {
        Ok(body) => body,
        Err(_) => return failed("could not build the recognition request"),
    };
    let post_started_ms = elapsed_ms(session_started);
    let response = if let Some(dispatch_sender) = dispatch_sender {
        let not_cancelled = AtomicBool::new(false);
        shazam_http_lab::post_shazam_json_with_timeout_cancellable_and_dispatch(
            &endpoint,
            &body,
            Duration::from_millis(request_timeout_ms),
            &not_cancelled,
            || {
                *transport_started = true;
                let _ = dispatch_sender.send(true);
            },
        )
    } else {
        shazam_http_lab::post_shazam_json_with_timeout(
            &endpoint,
            &body,
            Duration::from_millis(request_timeout_ms),
        )
    };
    let response_received_ms = elapsed_ms(session_started);
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            let retry = shazam_http_lab::native_retry_for_network_failure(
                &error,
                job.sample_ms,
                configuration
                    .manual_timing()
                    .effective_maximum_signature_length_ms(),
                false,
            );
            return ProgressiveAttemptResult {
                status: None,
                outcome: retry.map_or(RecognitionOutcome::TransportFailure, |retry| {
                    RecognitionOutcome::MatcherFailure { retry: Some(retry) }
                }),
                retry,
                candidates: Vec::new(),
                error: Some(if retry.is_some() {
                    "HTTPS request failed; following Native retry to the configured maximum"
                } else {
                    "HTTPS recognition request failed"
                }),
                post_started_ms: Some(post_started_ms),
                response_received_ms: Some(response_received_ms),
            };
        }
    };
    if env::var_os("OPENSHZAM_PROGRESSIVE_TRACE").is_some_and(|value| value == "1") {
        eprintln!(
            "recognition response shape: {}",
            shazam_http_lab::summarize_response_shape(&response.body)
        );
    }
    let interpreted = shazam_http_lab::interpret_progressive_http_response_for_signature(
        response.status,
        &response.body,
        Some(job.sample_ms),
        Some(
            configuration
                .manual_timing()
                .effective_maximum_signature_length_ms(),
        ),
    );
    let retry = match &interpreted {
        shazam_http_lab::ProgressiveHttpResponse::Matched { retry, .. }
        | shazam_http_lab::ProgressiveHttpResponse::NoMatch(retry)
        | shazam_http_lab::ProgressiveHttpResponse::HttpError { retry, .. }
        | shazam_http_lab::ProgressiveHttpResponse::ParseFailure { retry } => *retry,
        shazam_http_lab::ProgressiveHttpResponse::InvalidResponse => None,
    };
    let (outcome, candidates, error) = match interpreted {
        shazam_http_lab::ProgressiveHttpResponse::Matched { candidates, retry } => {
            (RecognitionOutcome::Match { retry }, candidates, None)
        }
        shazam_http_lab::ProgressiveHttpResponse::NoMatch(retry) => {
            (RecognitionOutcome::NoMatch { retry }, Vec::new(), None)
        }
        shazam_http_lab::ProgressiveHttpResponse::HttpError { retry, .. } => (
            RecognitionOutcome::MatcherFailure { retry },
            Vec::new(),
            Some(if retry.is_some() {
                "recognition HTTP response was unsuccessful; applying Native failure retry to the configured maximum"
            } else {
                "recognition HTTP response was unsuccessful; body withheld"
            }),
        ),
        shazam_http_lab::ProgressiveHttpResponse::ParseFailure { retry } => (
            RecognitionOutcome::MatcherFailure { retry },
            Vec::new(),
            Some(if retry.is_some() {
                "response parsing failed; following Native retry to the configured maximum"
            } else {
                "response parsing failed; no Native retry is available"
            }),
        ),
        shazam_http_lab::ProgressiveHttpResponse::InvalidResponse => (
            RecognitionOutcome::InvalidResponse,
            Vec::new(),
            Some("HTTP response did not contain a recognized track or retry"),
        ),
    };
    ProgressiveAttemptResult {
        status: Some(response.status),
        outcome,
        retry,
        candidates,
        error,
        post_started_ms: Some(post_started_ms),
        response_received_ms: Some(response_received_ms),
    }
}

fn apply_progressive_completion(
    session: &mut ProgressiveSession,
    completion: ProgressiveCompletion,
    session_started: Instant,
    attempts: &mut u64,
    matched: &mut bool,
) -> Result<Option<shazam_progressive::RecognitionJob>, String> {
    *attempts = (*attempts).max(completion.attempt);
    let post_rtt_ms = completion
        .result
        .post_started_ms
        .zip(completion.result.response_received_ms)
        .map(|(started, received)| received.saturating_sub(started));
    println!(
        "attempt={} signature_sample_ms={} signature_ready_elapsed_ms={} post_started_elapsed_ms={:?} response_received_elapsed_ms={:?} post_rtt_ms={:?} HTTP status={:?} retry_duration_ms={:?} retry_interval_ms={:?}",
        completion.attempt,
        completion.sample_ms,
        completion.signature_ready_ms,
        completion.result.post_started_ms,
        completion.result.response_received_ms,
        post_rtt_ms,
        completion.result.status,
        completion
            .result
            .retry
            .and_then(|directive| directive.retry_duration_ms),
        completion
            .result
            .retry
            .and_then(|directive| directive.interval_ms)
    );

    if !session.in_flight_attempts().contains(&completion.attempt) {
        if session.is_terminal() {
            return Ok(None);
        }
        return Err(format!(
            "received a response for unknown attempt {}",
            completion.attempt
        ));
    }
    let response_applied_at = elapsed_ms(session_started);
    let (status, delivered) = session
        .complete_with_delivery(
            completion.attempt,
            completion.result.outcome,
            response_applied_at,
        )
        .map_err(|error| error.to_string())?;
    if !delivered {
        println!(
            "attempt={} response_delivered_to_matcher=false lifecycle_status={status:?}",
            completion.attempt
        );
        return Ok(None);
    }
    let post_stop_job = session.take_post_stop_retry_job();
    // After applying an accepted Retry, poll at this response boundary so a
    // stopped generator with sufficient retained PCM, or a threshold already
    // satisfied by written PCM, can immediately yield its next job. The
    // session remains attached to the same Manual context; it is not detached
    // or finalized as a post-stop NoMatch request.
    let immediate_retry_job = match post_stop_job {
        Some(job) => Some(job),
        None => session
            .poll(response_applied_at)
            .map_err(|error| error.to_string())?,
    };
    if let Some(error) = completion.result.error {
        eprintln!("progressive request state: {error}");
    }
    if !completion.result.candidates.is_empty() {
        for (index, candidate) in completion.result.candidates.iter().enumerate() {
            println!(
                "match[{}] title={:?} artist={:?}",
                index + 1,
                candidate.title,
                candidate.artist
            );
        }
        *matched = true;
    }
    Ok(immediate_retry_job)
}

fn dispatch_ready_progressive_jobs(
    session: &mut ProgressiveSession,
    configuration: &shazam_http_lab::ShazamConfiguration,
    runtime: &mut ProgressiveWorkerRuntime,
) -> Result<(), String> {
    loop {
        let now_ms = elapsed_ms(runtime.started);
        let Some(job) = session.poll(now_ms).map_err(|error| error.to_string())? else {
            return Ok(());
        };
        let attempt = job.attempt;
        dispatch_progressive_job(
            configuration,
            job,
            runtime.started,
            runtime.request_timeout_ms,
            runtime.completion_sender.clone(),
            false,
        )?;
        runtime.active_workers.insert(attempt);
    }
}

fn handle_progressive_completion(
    completion: ProgressiveCompletion,
    session: &mut ProgressiveSession,
    configuration: &shazam_http_lab::ShazamConfiguration,
    runtime: &mut ProgressiveWorkerRuntime,
) -> Result<(), String> {
    if !runtime.active_workers.remove(&completion.attempt) {
        return Err(format!(
            "received a duplicate or unknown worker completion for attempt {}",
            completion.attempt
        ));
    }

    if completion.detached {
        runtime.attempts = runtime.attempts.max(completion.attempt);
        println!(
            "attempt={} detached_post_stop_retry=true HTTP status={:?} response_received_elapsed_ms={:?}",
            completion.attempt,
            completion.result.status,
            completion.result.response_received_ms
        );
    } else if let Some(job) = apply_progressive_completion(
        session,
        completion,
        runtime.started,
        &mut runtime.attempts,
        &mut runtime.matched,
    )? {
        let attempt = job.attempt;
        let dispatch = dispatch_progressive_job(
            configuration,
            job,
            runtime.started,
            runtime.request_timeout_ms,
            runtime.completion_sender.clone(),
            true,
        );
        await_post_stop_transport_start(dispatch)?;
        runtime.active_workers.insert(attempt);
        session.finish_post_stop_retry_submission();
    }

    dispatch_ready_progressive_jobs(session, configuration, runtime)
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn fail_disconnected_worker(session: &mut ProgressiveSession, now_ms: u64, attempts: &mut u64) {
    for attempt in session.in_flight_attempts() {
        *attempts = attempt;
        if let Err(error) = session.complete(attempt, RecognitionOutcome::TransportFailure, now_ms)
        {
            eprintln!("could not close disconnected recognition attempt: {error}");
        }
    }
}

fn run_samples(samples: Vec<i16>, sample_rate: u32, live: bool) -> ExitCode {
    let mut sigx = match sigx_core::SigX::new(sigx_core::Config {
        sample_rate,
        channels: 1,
        signature_type: 2,
        signature_options: 0,
        max_frames: samples.len().max(1),
    }) {
        Ok(sigx) => sigx,
        Err(error) => {
            eprintln!("initialize Pure Rust SigX: {error:?}");
            return ExitCode::from(1);
        }
    };
    if let Err(error) = sigx.push_i16(&samples) {
        eprintln!("process Int16 PCM: {error:?}");
        return ExitCode::from(1);
    }
    let signature = match sigx.signature_with_flush_frames(61) {
        Ok(signature) => signature,
        Err(error) => {
            eprintln!("pack Pure Rust Signature: {error:?}");
            return ExitCode::from(1);
        }
    };
    let parsed = match sigx_format::Signature::parse(&signature) {
        Ok(signature) => signature,
        Err(error) => {
            eprintln!("Pure Rust Signature failed validation: {error}");
            return ExitCode::from(1);
        }
    };
    let sample_ms = (samples.len() as u128 * 1_000 / u128::from(sample_rate)) as u64;
    println!(
        "Pure Rust Signature ready: bytes={} payload_len={} crc32={:#010x} duration_ms={sample_ms}",
        parsed.len(),
        parsed.payload_len(),
        parsed.crc32()
    );
    if live {
        live_signature(&signature, sample_ms)
    } else {
        mock_signature(&signature, sample_ms)
    }
}

fn run_signature_live(args: Vec<String>) -> ExitCode {
    if args.len() != 2 {
        eprintln!("usage: shazam-http-lab signature-live <signature.bin> <sample-duration-ms>");
        return ExitCode::from(2);
    }
    let sample_ms = match args[1].parse::<u64>() {
        Ok(value) if value > 0 => value,
        _ => {
            eprintln!("sample duration must be a positive integer number of milliseconds");
            return ExitCode::from(2);
        }
    };
    let signature = match fs::read(&args[0]) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read signature {}: {error}", args[0]);
            return ExitCode::from(2);
        }
    };
    live_signature(&signature, sample_ms)
}

fn live_signature(signature: &[u8], sample_ms: u64) -> ExitCode {
    let timestamp_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(error) => {
            eprintln!("system clock is before Unix epoch: {error}");
            return ExitCode::from(1);
        }
    };
    let timezone = local_timezone_name();
    let body = match shazam_http_lab::build_request_body(
        signature,
        sample_ms,
        timestamp_ms,
        &timezone,
        json!({}),
        json!({}),
        None,
    ) {
        Ok(body) => body,
        Err(error) => {
            eprintln!("build Shazam request: {error}");
            return ExitCode::from(1);
        }
    };
    let endpoint = match live_endpoint_from_environment() {
        Ok(endpoint) => endpoint,
        Err(message) => {
            eprintln!("cannot prepare live request: {message}");
            return ExitCode::from(2);
        }
    };

    match shazam_http_lab::post_shazam_json(&endpoint, &body) {
        Ok(response) => {
            eprintln!(
                "LIVE POST completed to allowlisted amp.shazam.com route; IDs are redacted and no AMP token was sent."
            );
            println!("Shazam HTTP status={}", response.status);
            if !(200..300).contains(&response.status) {
                eprintln!("recognition did not succeed; response body withheld from logs");
                return ExitCode::from(1);
            }
            match shazam_http_lab::parse_track_candidates(&response.body) {
                Ok(candidates) => {
                    for (index, candidate) in candidates.iter().enumerate() {
                        println!(
                            "match[{}] title={:?} artist={:?}",
                            index + 1,
                            candidate.title,
                            candidate.artist
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!(
                        "HTTP response had no usable track result: {error}; {}",
                        shazam_http_lab::summarize_response_shape(&response.body)
                    );
                    ExitCode::from(1)
                }
            }
        }
        Err(error) => {
            eprintln!("Shazam request failed: {error}");
            ExitCode::from(1)
        }
    }
}

fn live_endpoint_from_environment() -> Result<String, String> {
    let streaming_provider = env::var("SHAZAM_STREAMING_PROVIDER").unwrap_or_default();
    let tag_id = random_uuid_v4()
        .map_err(|error| format!("could not generate a request tag ID: {error}"))?;
    let template = env::var("SHAZAM_TAG_URL_TEMPLATE").ok();
    let installation_id = env::var("SHAZAM_INID").ok();
    match (template, installation_id) {
        (Some(template), Some(installation_id)) => shazam_http_lab::resolve_shazam_tag_url(
            &template,
            &installation_id,
            &tag_id,
            &streaming_provider,
        )
        .map_err(|error| format!("manual Shazam route settings were invalid: {error}")),
        (None, None) => {
            let profile = config_request_profile_from_host()?;
            let configuration = shazam_http_lab::fetch_shazam_config(&profile)
                .map_err(|error| format!("could not fetch or parse Shazam Config: {error}"))?;
            configuration
                .resolve_tag_url(&tag_id, &streaming_provider)
                .map_err(|error| format!("Config returned an unsupported Mac tag route: {error}"))
        }
        _ => Err("set both manual route values or unset both for automatic Config".into()),
    }
}

fn run_config_check() -> ExitCode {
    let profile = match config_request_profile_from_host() {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("cannot identify this host for Config: {error}");
            return ExitCode::from(1);
        }
    };
    match shazam_http_lab::fetch_shazam_config(&profile) {
        Ok(configuration) => {
            if let Err(error) = configuration.resolve_tag_url("config-check", "") {
                eprintln!("Config was fetched but its tag route could not be resolved: {error}");
                return ExitCode::from(1);
            }
            let timing = configuration.manual_timing();
            println!(
                "Config loaded over verified HTTPS; route resolved in memory, INID redacted; no Shazam app cache was read or written."
            );
            println!(
                "manual timing fields (ms): sample={:?} progressive={:?} pre_record_buffer={:?} minimum_signature={:?} maximum_signature={:?} native_session_timeout={:?}",
                timing.sample_ms,
                timing.progressive_ms,
                timing.pre_record_buffer_ms,
                timing.minimum_signature_ms,
                timing.maximum_signature_ms,
                timing.session_timeout_ms
            );
            println!(
                "effective Native manual controls (ms): tagging_type=1 use_buffered_audio=true first_signature_target={:?} maximum_recording={:?} pre_record_buffer={} minimum_signature={} maximum_signature={} session_timeout={} default_retry_interval={}",
                timing.effective_initial_sample_ms(),
                timing.sample_ms,
                timing.effective_pre_record_buffer_ms(),
                timing
                    .minimum_signature_ms
                    .unwrap_or(shazam_http_lab::NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS),
                timing.effective_maximum_signature_length_ms(),
                timing.session_timeout_ms.unwrap_or(
                    shazam_progressive::NATIVE_TAGGING_TYPE_1_DEFAULT_SESSION_TIMEOUT_MS
                ),
                NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("could not fetch Shazam Config: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(target_os = "macos")]
fn config_request_profile_from_host() -> Result<shazam_http_lab::ConfigRequestProfile, String> {
    let language = env::var("SHAZAM_CONFIG_LANGUAGE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(mac_preferred_language)
        .unwrap_or_else(|| "en".into());
    let locale = env::var("SHAZAM_CONFIG_LOCALE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(mac_locale_identifier)
        .unwrap_or_else(|| "en_US".into());
    let model = env::var("SHAZAM_CONFIG_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| command_value("sysctl", &["-n", "hw.model"]))
        .ok_or_else(|| {
            "could not read the Mac model (set SHAZAM_CONFIG_MODEL to override)".to_owned()
        })?;
    let os_version = env::var("SHAZAM_CONFIG_OS_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| command_value("sw_vers", &["-productVersion"]))
        .ok_or_else(|| {
            "could not read the macOS version (set SHAZAM_CONFIG_OS_VERSION to override)".to_owned()
        })?;
    let os_build = env::var("SHAZAM_CONFIG_OS_BUILD")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| command_value("sw_vers", &["-buildVersion"]))
        .ok_or_else(|| {
            "could not read the macOS build (set SHAZAM_CONFIG_OS_BUILD to override)".to_owned()
        })?;
    let country = env::var("SHAZAM_CONFIG_COUNTRY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "-".into());

    Ok(shazam_http_lab::ConfigRequestProfile {
        country,
        language,
        locale,
        model,
        os_version,
        os_build,
        app_version: "2.11.0".into(),
    })
}

#[cfg(windows)]
fn config_request_profile_from_host() -> Result<shazam_http_lab::ConfigRequestProfile, String> {
    shazam_http_lab::windows_config_request_profile().map_err(|error| error.to_string())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn config_request_profile_from_host() -> Result<shazam_http_lab::ConfigRequestProfile, String> {
    Err("automatic Config profile detection is available only on Windows and macOS".into())
}

#[cfg(target_os = "macos")]
fn command_value(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

#[cfg(target_os = "macos")]
fn mac_preferred_language() -> Option<String> {
    let raw = command_value("defaults", &["read", "-g", "AppleLanguages"])?;
    let first = first_quoted_value(&raw)?;
    let language = first.split(['-', '_']).next()?.trim();
    (!language.is_empty()).then(|| language.to_owned())
}

#[cfg(target_os = "macos")]
fn mac_locale_identifier() -> Option<String> {
    let raw = command_value("defaults", &["read", "-g", "AppleLocale"])?;
    let locale = raw.trim().split('.').next()?.replace('-', "_");
    (!locale.is_empty()).then_some(locale)
}

#[cfg(target_os = "macos")]
fn first_quoted_value(value: &str) -> Option<String> {
    let start = value.find('"')? + 1;
    let end = start + value[start..].find('"')?;
    Some(value[start..end].to_owned())
}

fn random_uuid_v4() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    ))
}

fn mock_signature(signature: &[u8], sample_ms: u64) -> ExitCode {
    let timestamp_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(error) => {
            eprintln!("system clock is before Unix epoch: {error}");
            return ExitCode::from(1);
        }
    };
    let timezone = local_timezone_name();
    eprintln!("Diagnostic request timezone: {timezone}");
    let body = match shazam_http_lab::build_request_body(
        signature,
        sample_ms,
        timestamp_ms,
        &timezone,
        json!({}),
        json!({}),
        None,
    ) {
        Ok(body) => body,
        Err(error) => {
            eprintln!("build diagnostic request: {error}");
            return ExitCode::from(1);
        }
    };

    match run_mock_roundtrip(&body) {
        Ok((status, response)) => {
            println!("MOCK ONLY — no Shazam service was contacted; HTTP status={status}");
            match shazam_http_lab::parse_track_candidates(&response) {
                Ok(candidates) => {
                    for (index, candidate) in candidates.iter().enumerate() {
                        println!(
                            "candidate[{}] title={:?} artist={:?} trackid={:?}",
                            index + 1,
                            candidate.title,
                            candidate.artist,
                            candidate.track_id
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("decode mock response: {error}");
                    ExitCode::from(1)
                }
            }
        }
        Err(error) => {
            eprintln!("local mock roundtrip failed: {error}");
            ExitCode::from(1)
        }
    }
}

/// Use the host's local IANA timezone for the diagnostic recognition request.
#[cfg(windows)]
fn local_timezone_name() -> String {
    env::var("TZ")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| iana_time_zone::get_timezone().ok())
        .unwrap_or_else(|| "UTC".into())
}

#[cfg(not(windows))]
fn local_timezone_name() -> String {
    fs::read_link("/etc/localtime")
        .ok()
        .and_then(|path| timezone_from_zoneinfo_path(&path))
        .or_else(|| env::var("TZ").ok().filter(|value| !value.trim().is_empty()))
        .unwrap_or_else(|| "UTC".into())
}

#[cfg(any(test, not(windows)))]
fn timezone_from_zoneinfo_path(path: &Path) -> Option<String> {
    let path = path.to_str()?;
    ["/zoneinfo/", "/zoneinfo.default/"]
        .into_iter()
        .find_map(|marker| path.rsplit_once(marker).map(|(_, zone)| zone))
        .filter(|zone| !zone.is_empty() && !zone.split('/').any(|part| part == ".."))
        .map(str::to_owned)
}

fn run_mock_roundtrip(
    body: &[u8],
) -> Result<(u16, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
    run_mock_roundtrip_with_response(
        body,
        br#"{"matches":[{"trackid":"mock-track-001","title":"Mock Result Only","artist":"Local Test Fixture"}]}"#,
    )
}

fn run_mock_roundtrip_with_response(
    body: &[u8],
    response: &[u8],
) -> Result<(u16, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
    run_mock_roundtrip_with_status_and_gate(200, body, response, None)
}

#[cfg(test)]
fn run_mock_roundtrip_with_status(
    status: u16,
    body: &[u8],
    response: &[u8],
) -> Result<(u16, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
    run_mock_roundtrip_with_status_and_gate(status, body, response, None)
}

#[cfg(test)]
fn run_mock_roundtrip_with_gate(
    body: &[u8],
    response: &[u8],
    gate: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
) -> Result<(u16, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
    run_mock_roundtrip_with_status_and_gate(200, body, response, gate)
}

fn run_mock_roundtrip_with_status_and_gate(
    status: u16,
    body: &[u8],
    response: &[u8],
    gate: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
) -> Result<(u16, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let response = response.to_vec();
    let server = thread::spawn(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let (stream, _) = listener.accept()?;
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line)?;
            if request_line != "POST /mock HTTP/1.1\r\n" {
                return Err(format!("unexpected local mock request line: {request_line:?}").into());
            }
            let mut content_length = None;
            let mut platform = None;
            let mut app_version = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line)?;
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.trim_end().split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        content_length = Some(value.trim().parse::<usize>()?);
                    }
                    if name.eq_ignore_ascii_case("x-shazam-platform") {
                        platform = Some(value.trim().to_owned());
                    }
                    if name.eq_ignore_ascii_case("x-shazam-appversion") {
                        app_version = Some(value.trim().to_owned());
                    }
                }
            }
            if platform.as_deref() != Some("MAC") || app_version.as_deref() != Some("2.11.0") {
                return Err(
                    "local request omitted the observed Shazam platform/version headers".into(),
                );
            }
            let mut request_body = vec![0; content_length.ok_or("missing Content-Length")?];
            reader.read_exact(&mut request_body)?;
            let parsed: Value = serde_json::from_slice(&request_body)?;
            if parsed.get("signature").is_none() {
                return Err("mock request did not contain a signature".into());
            }

            if let Some((accepted, release)) = gate {
                accepted
                    .send(())
                    .map_err(|_| io::Error::other("mock request observer disconnected"))?;
                release
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|_| io::Error::other("mock response release timed out"))?;
            }

            let mut stream = reader.into_inner();
            let reason = match status {
                200 => "OK",
                503 => "Service Unavailable",
                _ => "Mock Response",
            };
            write!(
                stream,
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            )?;
            stream.write_all(&response)?;
            Ok(())
        },
    );

    let endpoint = format!("http://{address}/mock");
    let response = shazam_http_lab::post_loopback_json(&endpoint, body)?;
    server
        .join()
        .map_err(|_| io::Error::other("local mock server thread panicked"))??;
    Ok((response.status, response.body))
}

fn usage() {
    eprintln!("usage: shazam-http-lab mock-roundtrip <signature.bin> <sample-duration-ms>");
    eprintln!("   or: shazam-http-lab pcm-mock <mono-i16le.pcm> <sample-rate-hz>");
    eprintln!("   or: shazam-http-lab audio-mock <audio-file> <seconds>");
    eprintln!("   or: shazam-http-lab signature-live <signature.bin> <sample-duration-ms>");
    eprintln!("   or: shazam-http-lab audio-live <audio-file> <seconds>");
    eprintln!("   or: shazam-http-lab audio-progressive-live <audio-file> <seconds>");
    eprintln!("   or: shazam-http-lab config-check");
    eprintln!(
        "Live commands fetch Config automatically unless both manual route overrides are set."
    );
    eprintln!("Config profile overrides: SHAZAM_CONFIG_COUNTRY, _LANGUAGE, _LOCALE, _MODEL, _OS_VERSION, _OS_BUILD.");
    eprintln!("The live tag request sends no AMP token; use only when authorized.");
}

#[cfg(test)]
mod tests {
    use super::{
        apply_progressive_completion, await_post_stop_transport_start,
        handle_progressive_completion, progressive_source_time_ms, run_mock_roundtrip_with_gate,
        run_mock_roundtrip_with_response, run_mock_roundtrip_with_status,
        run_mock_roundtrip_with_status_and_gate, timezone_from_zoneinfo_path,
        ProgressiveAttemptResult, ProgressiveCompletion, ProgressiveWorkerRuntime,
    };
    use serde_json::json;
    use shazam_progressive::{
        NativeTaggingType, ProgressiveSession, RecognitionOutcome, RecognitionTiming, SessionStatus,
    };
    use std::{
        collections::BTreeSet,
        path::Path,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn creates_uuid_v4_from_the_platform_random_source() {
        let id = super::random_uuid_v4().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn post_stop_retry_context_closes_only_after_transport_start_ack() {
        let (dispatch_sender, dispatch_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            release_receiver.recv().unwrap();
            dispatch_sender.send(true).unwrap();
        });
        let (result_sender, result_receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = await_post_stop_transport_start(Ok(Some(dispatch_receiver)));
            let _ = result_sender.send(result);
        });

        assert!(result_receiver
            .recv_timeout(Duration::from_millis(20))
            .is_err());
        release_sender.send(()).unwrap();
        assert!(result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok());
        worker.join().unwrap();

        let (dispatch_sender, dispatch_receiver) = mpsc::sync_channel(1);
        dispatch_sender.send(false).unwrap();
        assert!(await_post_stop_transport_start(Ok(Some(dispatch_receiver))).is_err());
    }

    #[test]
    fn progressive_file_replay_times_signatures_at_the_chunk_end() {
        assert_eq!(progressive_source_time_ms(4_410, 44_100).unwrap(), 100);
        assert_eq!(progressive_source_time_ms(132_300, 44_100).unwrap(), 3_000);
        assert_eq!(progressive_source_time_ms(1, 44_100).unwrap(), 1);
        assert!(progressive_source_time_ms(1, 0).is_err());

        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 44_100,
                minimum_signature_ms: 1_000,
                initial_sample_ms: 3_000,
                maximum_sample_ms: 6_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: Some(6_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: None,
                maximum_attempts: None,
            },
            NativeTaggingType::Manual,
        )
        .unwrap();
        let samples: Vec<i16> = (0..132_300)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        let chunk_frames = 4_410;
        let mut first_job = None;
        for offset in (0..samples.len()).step_by(chunk_frames) {
            let end = (offset + chunk_frames).min(samples.len());
            let now_ms = progressive_source_time_ms(end, 44_100).unwrap();
            session.push_i16_at(&samples[offset..end], now_ms).unwrap();
            if let Some(job) = session.poll(now_ms).unwrap() {
                first_job = Some(job);
                break;
            }
        }
        let job = first_job.expect("the first configured Signature threshold is reached");
        assert_eq!(job.sample_ms, 3_000);
        assert_eq!(job.generated_at_ms, 3_000);
    }

    #[test]
    fn progressive_http_mock_does_not_retry_past_finite_source_after_last_retry() {
        const SAMPLE_RATE: u32 = 16_000;
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: SAMPLE_RATE,
                minimum_signature_ms: 250,
                initial_sample_ms: 3_000,
                maximum_sample_ms: 9_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: Some(12_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: None,
                maximum_attempts: None,
            },
            NativeTaggingType::Manual,
        )
        .unwrap();
        let samples: Vec<i16> = (0..144_000)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        let retry_targets = [
            (3_000, 5_000),
            (5_000, 7_000),
            (7_000, 9_000),
            (9_000, 12_000),
        ];
        let mut previous_frames = 0;

        for (attempt_index, (sample_ms, retry_ms)) in retry_targets.into_iter().enumerate() {
            let target_frames = sample_ms as usize * SAMPLE_RATE as usize / 1_000;
            session
                .push_i16_at(&samples[previous_frames..target_frames], sample_ms)
                .unwrap();
            let job = session
                .poll(sample_ms)
                .unwrap()
                .expect("the configured or Retry threshold produces a Signature");
            assert_eq!(job.attempt, attempt_index as u64 + 1);
            assert_eq!(job.sample_ms, sample_ms);

            let request_body = shazam_http_lab::build_request_body(
                &job.signature,
                job.sample_ms,
                1_800_000_000_000 + sample_ms,
                "Asia/Tokyo",
                json!({}),
                json!({}),
                None,
            )
            .unwrap();
            let response_body = json!({
                "matches": [],
                "sampleseconds": retry_ms as f64 / 1_000.0,
                "intervalseconds": 0.0,
            })
            .to_string();
            let (http_status, response) =
                run_mock_roundtrip_with_response(&request_body, response_body.as_bytes()).unwrap();
            assert_eq!(http_status, 200);
            let retry = match shazam_http_lab::interpret_progressive_http_response_for_signature(
                http_status,
                &response,
                Some(job.sample_ms),
                Some(12_000),
            ) {
                shazam_http_lab::ProgressiveHttpResponse::NoMatch(Some(retry)) => retry,
                other => panic!("expected retry-bearing NoMatch response, got {other:?}"),
            };
            assert_eq!(retry.retry_duration_ms, Some(retry_ms));
            assert_eq!(retry.interval_ms, Some(0));

            // The finite source ends while the 9-second request is in flight.
            // Its 12-second Retry cannot create another Signature without PCM.
            if sample_ms == 9_000 {
                session.finish_input();
            }
            let completed_at_ms = sample_ms + 250;
            let status = session
                .complete(
                    job.attempt,
                    RecognitionOutcome::NoMatch { retry: Some(retry) },
                    completed_at_ms,
                )
                .unwrap();
            if sample_ms == 9_000 {
                assert_eq!(status, SessionStatus::Exhausted);
            } else {
                assert!(matches!(status, SessionStatus::WaitingForRetry { .. }));
            }
            previous_frames = target_frames;
        }

        assert_eq!(previous_frames, samples.len());
        assert!(session.poll(12_000).unwrap().is_none());
        assert_eq!(session.status(), SessionStatus::Exhausted);
        assert!(session.in_flight_attempts().is_empty());
    }

    #[test]
    fn extracts_iana_timezone_from_macos_zoneinfo_symlink() {
        assert_eq!(
            timezone_from_zoneinfo_path(Path::new("/var/db/timezone/zoneinfo/Asia/Tokyo")),
            Some("Asia/Tokyo".into())
        );
    }

    #[test]
    fn manual_no_match_stopped_generator_retry_remains_in_context() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 1_000,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: Some(12_000),
                session_timeout_ms: Some(45_000),
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: None,
                maximum_attempts: Some(4),
            },
            NativeTaggingType::Manual,
        )
        .unwrap();
        let samples: Vec<i16> = (0..48_001)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        session.push_i16(&samples[16_000..48_000]).unwrap();
        assert_eq!(session.frames_seen(), 48_000);
        assert!(session.poll(0).unwrap().is_none());
        session.push_i16(&samples[48_000..]).unwrap();
        assert_eq!(session.frames_seen(), 48_000);

        let first_body = shazam_http_lab::build_request_body(
            &first.signature,
            first.sample_ms,
            1_800_000_000_000,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (status, response) = run_mock_roundtrip_with_response(
            &first_body,
            br#"{"matches":[],"sampleseconds":3,"intervalseconds":0}"#,
        )
        .unwrap();
        assert_eq!(status, 200);
        let (outcome, retry) =
            match shazam_http_lab::interpret_progressive_http_response_for_signature(
                status,
                &response,
                Some(first.sample_ms),
                Some(3_000),
            ) {
                shazam_http_lab::ProgressiveHttpResponse::NoMatch(retry) => {
                    (RecognitionOutcome::NoMatch { retry }, retry)
                }
                other => panic!("expected first mock no-match, got {other:?}"),
            };
        assert!(retry.is_some());
        assert_eq!(
            session.complete(first.attempt, outcome, 3_100).unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 3_100,
                target_sample_ms: 3_000,
            }
        );

        let followup = session
            .poll(3_100)
            .unwrap()
            .expect("accepted Retry makes the retained Signature eligible");
        assert_eq!(followup.attempt, 2);
        assert_eq!(followup.sample_ms, 3_000);
        let post_stop_body = shazam_http_lab::build_request_body(
            &followup.signature,
            followup.sample_ms,
            1_800_000_000_001,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (post_stop_status, post_stop_response) = run_mock_roundtrip_with_response(
            &post_stop_body,
            br#"{"track":{"title":"Mock Follow-up","subtitle":"Mock Artist"}}"#,
        )
        .unwrap();
        assert_eq!(post_stop_status, 200);
        assert!(matches!(
            shazam_http_lab::interpret_progressive_http_response(
                post_stop_status,
                &post_stop_response
            ),
            shazam_http_lab::ProgressiveHttpResponse::Matched { .. }
        ));
        let (match_outcome, candidates) = match shazam_http_lab::interpret_progressive_http_response(
            post_stop_status,
            &post_stop_response,
        ) {
            shazam_http_lab::ProgressiveHttpResponse::Matched { candidates, retry } => {
                (RecognitionOutcome::Match { retry }, candidates)
            }
            other => panic!("expected matched follow-up, got {other:?}"),
        };
        assert_eq!(
            session
                .complete(followup.attempt, match_outcome, 3_200)
                .unwrap(),
            SessionStatus::Succeeded
        );
        assert!(!candidates.is_empty());
        assert!(session.take_post_stop_retry_job().is_none());
        assert!(session.poll(3_200).unwrap().is_none());
    }

    #[test]
    fn progressive_signature_mock_http_auto_type2_retry_and_match_end_to_end() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 1_000,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: Some(3_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: Some(1_000),
                // Local test bound: Native Type 2 keeps tagging after a
                // match, so finish only when this two-attempt guard fires.
                maximum_attempts: Some(2),
            },
            NativeTaggingType::Auto,
        )
        .unwrap();
        let samples: Vec<i16> = (0..48_000)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();

        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        assert_eq!(first.sample_ms, 1_000);

        let retry_body = shazam_http_lab::build_request_body(
            &first.signature,
            first.sample_ms,
            1_800_000_000_000,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (accepted_sender, accepted_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let request = thread::spawn(move || {
            run_mock_roundtrip_with_gate(
                &retry_body,
                br#"{"sampleseconds":2,"intervalseconds":0.05}"#,
                Some((accepted_sender, release_receiver)),
            )
        });
        accepted_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("mock server accepted the in-flight request");
        // The HTTP request is held open after its body is received. The PCM
        // engine must still accept samples without changing the active attempt.
        session.push_i16(&samples[16_000..32_000]).unwrap();
        assert_eq!(session.status(), SessionStatus::InFlight { attempt: 1 });
        release_sender.send(()).unwrap();
        let (status, response) = request
            .join()
            .expect("mock HTTP task did not panic")
            .unwrap();
        assert_eq!(status, 200);
        let retry = match shazam_http_lab::interpret_progressive_http_response(status, &response) {
            shazam_http_lab::ProgressiveHttpResponse::NoMatch(Some(retry)) => retry,
            other => panic!("expected retry response, got {other:?}"),
        };
        session
            .complete(
                first.attempt,
                RecognitionOutcome::NoMatch { retry: Some(retry) },
                1,
            )
            .unwrap();
        assert!(session.poll(50).unwrap().is_none());
        assert!(session.poll(51).unwrap().is_none());
        // The zero default interval had already started a fresh cycle before
        // the held HTTP response arrived; 16k samples are already in it.
        session.push_i16_at(&samples[16_000..], 51).unwrap();
        let second = session.poll(51).unwrap().unwrap();
        assert_eq!(second.sample_ms, 3_000);
        assert_eq!(second.attempt, 2);

        let match_body = shazam_http_lab::build_request_body(
            &second.signature,
            second.sample_ms,
            1_800_000_000_001,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&match_body).unwrap()["signature"]
                ["samplems"],
            3_000
        );
        let (status, response) = run_mock_roundtrip_with_response(
            &match_body,
            br#"{"track":{"title":"Mock Progressive Song","subtitle":"Mock Artist"}}"#,
        )
        .unwrap();
        assert_eq!(status, 200);
        let (candidates, retry) =
            match shazam_http_lab::interpret_progressive_http_response(status, &response) {
                shazam_http_lab::ProgressiveHttpResponse::Matched { candidates, retry } => {
                    (candidates, retry)
                }
                other => panic!("expected mock track response, got {other:?}"),
            };
        assert_eq!(candidates[0].title, "Mock Progressive Song");
        assert_eq!(candidates[0].artist, "Mock Artist");
        let delivery = shazam_http_lab::apply_progressive_response(
            &mut session,
            second.attempt,
            RecognitionOutcome::Match { retry },
            candidates,
            52,
        )
        .unwrap();
        assert!(delivery.delivered_to_matcher);
        assert!(delivery.native_manual_ui_event().is_none());
        let auto_event = delivery
            .native_auto_ui_event()
            .expect("mock Auto match yields the continuous UI candidate");
        let auto_json: serde_json::Value =
            serde_json::from_str(&shazam_http_lab::native_auto_ui_event_to_json(&auto_event))
                .unwrap();
        assert_eq!(auto_json["event"], "continuous_match_candidate");
        assert_eq!(auto_json["primary_track"]["title"], "Mock Progressive Song");
        assert_eq!(auto_json["primary_track"]["artist"], "Mock Artist");
        assert_eq!(delivery.status, SessionStatus::Succeeded);
        assert_eq!(session.status(), SessionStatus::Succeeded);
    }

    #[test]
    fn type2_submillisecond_native_interval_applies_to_the_following_cycle() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 1_000,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: 12_000,
                maximum_retry_duration_ms: Some(3_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: Some(20_000),
                maximum_attempts: Some(3),
            },
            NativeTaggingType::Auto,
        )
        .unwrap();
        let first_pcm = vec![0_i16; 16_000];
        session.push_i16(&first_pcm).unwrap();
        let first = session.poll(0).unwrap().unwrap();

        let retry = match shazam_http_lab::interpret_progressive_http_response(
            200,
            br#"{"matches":[],"sampleseconds":2,"intervalseconds":0.0000009}"#,
        ) {
            shazam_http_lab::ProgressiveHttpResponse::NoMatch(Some(retry)) => retry,
            other => panic!("expected Native interval retry, got {other:?}"),
        };
        assert_eq!(retry.interval_ms, Some(1));
        assert_eq!(retry.interval_seconds_bits, Some(0.0000009_f64.to_bits()));
        session
            .complete(
                first.attempt,
                RecognitionOutcome::NoMatch { retry: Some(retry) },
                100,
            )
            .unwrap();

        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 3_000,
            }
        );
        assert!(session.poll_at_ns(11_999_999_999).unwrap().is_none());
        let restart_time_ns = 12_000_000_000;
        assert!(session.poll_at_ns(restart_time_ns).unwrap().is_none());
        session
            .push_i16_at_ns(&vec![0_i16; 48_000], restart_time_ns)
            .unwrap();
        let second = session.poll_at_ns(restart_time_ns).unwrap().unwrap();
        assert_eq!(second.attempt, 2);
        assert_eq!(second.sample_ms, 3_000);
        assert!(session.poll_at_ns(12_000_000_899).unwrap().is_none());
        assert_eq!(session.frames_seen(), 48_000);
        assert!(session.poll_at_ns(12_000_000_900).unwrap().is_none());
        assert_eq!(session.frames_seen(), 0);
    }

    #[test]
    fn progressive_http_mock_initial_match_uses_explicit_retry_for_manual_type1() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 1_000,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 2_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: Some(3_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: Some(1_000),
                maximum_attempts: Some(3),
            },
            NativeTaggingType::Manual,
        )
        .unwrap();
        let samples: Vec<i16> = (0..16_000)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        session.push_i16(&samples).unwrap();
        let job = session.poll(0).unwrap().unwrap();
        let request_body = shazam_http_lab::build_request_body(
            &job.signature,
            job.sample_ms,
            1_800_000_000_000,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (status, response) = run_mock_roundtrip_with_response(
            &request_body,
            br#"{"track":{"title":"Initial Mock Song","subtitle":"Initial Mock Artist"},"retryms":3000}"#,
        )
        .unwrap();
        let (candidates, retry) =
            match shazam_http_lab::interpret_progressive_http_response_for_signature(
                status,
                &response,
                Some(job.sample_ms),
                Some(3_000),
            ) {
                shazam_http_lab::ProgressiveHttpResponse::Matched { candidates, retry } => {
                    (candidates, retry)
                }
                other => panic!("expected initial match, got {other:?}"),
            };
        assert_eq!(candidates[0].title, "Initial Mock Song");
        assert_eq!(candidates[0].artist, "Initial Mock Artist");
        assert_eq!(
            retry,
            Some(shazam_progressive::RetryDirective {
                retry_duration_ms: Some(3_000),
                interval_ms: Some(0),
                retry_duration_seconds_bits: Some(3.0_f64.to_bits()),
                interval_seconds_bits: Some(0.0_f64.to_bits()),
            })
        );
        assert_eq!(
            session
                .complete(job.attempt, RecognitionOutcome::Match { retry }, 1)
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 1,
                target_sample_ms: 3_000,
            }
        );
        let extended_input = session.push_i16_at(&vec![0; 32_000], 2_000).unwrap();
        assert_eq!(extended_input.accepted_frames, 32_000);
        assert_eq!(extended_input.total_frames, 48_000);
        let complete_result = session.poll(2_000).unwrap().unwrap();
        assert_eq!(complete_result.attempt, 2);
        assert_eq!(complete_result.sample_ms, 3_000);
        let complete_body = shazam_http_lab::build_request_body(
            &complete_result.signature,
            complete_result.sample_ms,
            1_800_000_000_100,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (status, response) = run_mock_roundtrip_with_response(
            &complete_body,
            br#"{"track":{"title":"Complete Mock Song","subtitle":"Complete Mock Artist"}}"#,
        )
        .unwrap();
        let response = shazam_http_lab::interpret_progressive_http_response_for_signature(
            status,
            &response,
            Some(complete_result.sample_ms),
            Some(3_000),
        );
        let final_match = match response {
            shazam_http_lab::ProgressiveHttpResponse::Matched { candidates, retry } => {
                assert_eq!(candidates[0].title, "Complete Mock Song");
                assert_eq!(retry, None);
                RecognitionOutcome::Match { retry }
            }
            other => panic!("expected final mock match, got {other:?}"),
        };
        assert_eq!(
            session
                .complete(complete_result.attempt, final_match, 2_100)
                .unwrap(),
            SessionStatus::Succeeded
        );
        assert!(session.push_i16(&[0]).is_err());
    }

    #[test]
    fn progressive_http_mock_initial_no_match_retry_continues_manual_type1() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 1_000,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: Some(3_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: Some(1_000),
                maximum_attempts: Some(3),
            },
            NativeTaggingType::Manual,
        )
        .unwrap();
        let samples: Vec<i16> = (0..16_000)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        session.push_i16(&samples).unwrap();
        let job = session.poll(0).unwrap().unwrap();
        let request_body = shazam_http_lab::build_request_body(
            &job.signature,
            job.sample_ms,
            1_800_000_000_000,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (status, response) =
            run_mock_roundtrip_with_response(&request_body, br#"{"matches":[],"retryms":2500}"#)
                .unwrap();
        let retry = match shazam_http_lab::interpret_progressive_http_response(status, &response) {
            shazam_http_lab::ProgressiveHttpResponse::NoMatch(retry) => retry,
            other => panic!("expected no-match response, got {other:?}"),
        };
        assert_eq!(
            retry,
            Some(shazam_progressive::RetryDirective {
                retry_duration_ms: Some(2_500),
                interval_ms: Some(0),
                retry_duration_seconds_bits: Some(2.5_f64.to_bits()),
                interval_seconds_bits: Some(0.0_f64.to_bits()),
            })
        );
        assert_eq!(
            session
                .complete(job.attempt, RecognitionOutcome::NoMatch { retry }, 1)
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 1,
                target_sample_ms: 2_500,
            }
        );
        session.push_i16(&vec![0; 24_000]).unwrap();
        let followup = session.poll(2).unwrap().unwrap();
        assert_eq!(followup.attempt, 2);
        assert_eq!(followup.sample_ms, 2_500);
        assert!(session.push_i16(&[0]).unwrap().accepted_frames > 0);

        let second_body = shazam_http_lab::build_request_body(
            &followup.signature,
            followup.sample_ms,
            1_800_000_000_100,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (second_status, second_response) =
            run_mock_roundtrip_with_response(&second_body, br#"{"matches":[],"retryms":5000}"#)
                .unwrap();
        let second_retry = match shazam_http_lab::interpret_progressive_http_response_for_signature(
            second_status,
            &second_response,
            Some(followup.sample_ms),
            Some(3_000),
        ) {
            shazam_http_lab::ProgressiveHttpResponse::NoMatch(retry) => retry,
            other => panic!("expected second no-match response, got {other:?}"),
        };
        assert_eq!(
            session
                .complete(
                    followup.attempt,
                    RecognitionOutcome::NoMatch {
                        retry: second_retry,
                    },
                    3,
                )
                .unwrap(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 3,
                target_sample_ms: 5_000,
            }
        );

        session.push_i16(&vec![0; 40_000]).unwrap();
        let final_signature = session.poll(4).unwrap().unwrap();
        assert_eq!(final_signature.attempt, 3);
        assert_eq!(final_signature.sample_ms, 5_000);
        let final_body = shazam_http_lab::build_request_body(
            &final_signature.signature,
            final_signature.sample_ms,
            1_800_000_000_200,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (final_status, final_response) = run_mock_roundtrip_with_response(
            &final_body,
            br#"{"track":{"title":"Retry Chain Match","subtitle":"Mock Artist"}}"#,
        )
        .unwrap();
        let final_retry = match shazam_http_lab::interpret_progressive_http_response_for_signature(
            final_status,
            &final_response,
            Some(final_signature.sample_ms),
            Some(3_000),
        ) {
            shazam_http_lab::ProgressiveHttpResponse::Matched { candidates, retry } => {
                assert_eq!(candidates[0].title, "Retry Chain Match");
                retry
            }
            other => panic!("expected final match response, got {other:?}"),
        };
        assert_eq!(final_retry, None);
        assert_eq!(
            session
                .complete(
                    final_signature.attempt,
                    RecognitionOutcome::Match { retry: final_retry },
                    5,
                )
                .unwrap(),
            SessionStatus::Succeeded
        );
    }

    #[test]
    fn progressive_http_mock_retries_manual_matcher_failure_then_recognizes() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 250,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: Some(2_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: Some(12_000),
                maximum_attempts: Some(3),
            },
            NativeTaggingType::Manual,
        )
        .unwrap();
        let samples: Vec<i16> = (0..32_001)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        let first_body = shazam_http_lab::build_request_body(
            &first.signature,
            first.sample_ms,
            1_800_000_000_000,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (status, response) = run_mock_roundtrip_with_status(
            503,
            &first_body,
            br#"{"error":"retry","sampleseconds":2,"intervalseconds":0.5}"#,
        )
        .unwrap();
        let retry = match shazam_http_lab::interpret_progressive_http_response_for_signature(
            status,
            &response,
            Some(first.sample_ms),
            Some(2_000),
        ) {
            shazam_http_lab::ProgressiveHttpResponse::HttpError { retry, .. } => retry,
            other => panic!("expected retry-bearing matcher failure, got {other:?}"),
        };
        assert_eq!(
            retry,
            Some(shazam_progressive::RetryDirective {
                retry_duration_ms: Some(2_000),
                interval_ms: Some(0),
                retry_duration_seconds_bits: None,
                interval_seconds_bits: None,
            })
        );

        // Like the Native audio generator, PCM continues while the request is
        // in flight; the matcher Retry updates this cycle's next threshold.
        session.push_i16(&samples[16_000..32_000]).unwrap();
        session
            .complete(
                first.attempt,
                RecognitionOutcome::MatcherFailure { retry },
                100,
            )
            .unwrap();
        assert!(session.poll(100).unwrap().is_none());
        session.push_i16(&samples[32_000..]).unwrap();
        let second = session.poll(100).unwrap().unwrap();
        assert_eq!(second.sample_ms, 2_000);

        let second_body = shazam_http_lab::build_request_body(
            &second.signature,
            second.sample_ms,
            1_800_000_000_000,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (status, response) = run_mock_roundtrip_with_response(
            &second_body,
            br#"{"track":{"title":"Retry Mock Song","subtitle":"Retry Mock Artist"}}"#,
        )
        .unwrap();
        let (candidates, retry) =
            match shazam_http_lab::interpret_progressive_http_response(status, &response) {
                shazam_http_lab::ProgressiveHttpResponse::Matched { candidates, retry } => {
                    (candidates, retry)
                }
                other => panic!("expected mock track response, got {other:?}"),
            };
        assert_eq!(candidates[0].title, "Retry Mock Song");
        session
            .complete(second.attempt, RecognitionOutcome::Match { retry }, 150)
            .unwrap();
        assert_eq!(session.status(), SessionStatus::Succeeded);
        session.finish_input();
        assert_eq!(session.status(), SessionStatus::Succeeded);
    }

    #[test]
    fn progressive_http_mock_parse_failure_retries_to_native_maximum_then_recognizes() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 250,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: Some(3_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: None,
                maximum_attempts: Some(3),
            },
            NativeTaggingType::Manual,
        )
        .unwrap();
        let samples: Vec<i16> = (0..48_000)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        let first_body = shazam_http_lab::build_request_body(
            &first.signature,
            first.sample_ms,
            1_800_000_000_000,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (status, response) =
            run_mock_roundtrip_with_response(&first_body, b"not json").unwrap();
        let failure = shazam_http_lab::interpret_progressive_http_response_for_signature(
            status,
            &response,
            Some(first.sample_ms),
            Some(3_000),
        );
        let retry = match failure {
            shazam_http_lab::ProgressiveHttpResponse::ParseFailure { retry } => retry,
            other => panic!("expected Native-style parse failure, got {other:?}"),
        };
        assert_eq!(
            retry,
            Some(shazam_progressive::RetryDirective {
                retry_duration_ms: Some(3_000),
                interval_ms: Some(0),
                retry_duration_seconds_bits: None,
                interval_seconds_bits: None,
            })
        );

        // More PCM arrives while the failed matcher request is being handled;
        // the retry extends this generator to the configured Native maximum.
        session.push_i16(&samples[16_000..32_000]).unwrap();
        session
            .complete(
                first.attempt,
                RecognitionOutcome::MatcherFailure { retry },
                100,
            )
            .unwrap();
        assert!(session.poll(100).unwrap().is_none());
        session.push_i16_at(&samples[32_000..], 101).unwrap();
        let second = session.poll(101).unwrap().unwrap();
        assert_eq!(second.sample_ms, 3_000);

        let second_body = shazam_http_lab::build_request_body(
            &second.signature,
            second.sample_ms,
            1_800_000_000_001,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (status, response) = run_mock_roundtrip_with_response(
            &second_body,
            br#"{"track":{"title":"Parse Retry Mock Song","subtitle":"Mock Artist"}}"#,
        )
        .unwrap();
        let candidates =
            match shazam_http_lab::interpret_progressive_http_response(status, &response) {
                shazam_http_lab::ProgressiveHttpResponse::Matched {
                    candidates,
                    retry: None,
                } => candidates,
                other => panic!("expected final mock recognition, got {other:?}"),
            };
        assert_eq!(candidates[0].title, "Parse Retry Mock Song");
        session
            .complete(
                second.attempt,
                RecognitionOutcome::Match { retry: None },
                150,
            )
            .unwrap();
        assert_eq!(session.status(), SessionStatus::Succeeded);
    }

    #[test]
    fn progressive_http_mock_retries_stopped_generator_then_recognizes() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 250,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: Some(3_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: Some(12_000),
                maximum_attempts: Some(3),
            },
            NativeTaggingType::Manual,
        )
        .unwrap();
        let samples: Vec<i16> = (0..48_000)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        let first_body = shazam_http_lab::build_request_body(
            &first.signature,
            first.sample_ms,
            1_800_000_000_000,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();

        let (accepted_sender, accepted_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let request = thread::spawn(move || {
            run_mock_roundtrip_with_status_and_gate(
                503,
                &first_body,
                br#"{"error":"retry","sampleseconds":2.5,"intervalseconds":0.5}"#,
                Some((accepted_sender, release_receiver)),
            )
        });
        accepted_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("mock server accepted the first request");

        // Let the live PCM stream reach its configured maximum while the
        // request is still in flight, which stops Native's SignatureGenerator.
        session.push_i16(&samples[16_000..]).unwrap();
        release_sender.send(()).unwrap();
        let (status, response) = request
            .join()
            .expect("mock HTTP task did not panic")
            .unwrap();
        let retry = match shazam_http_lab::interpret_progressive_http_response_for_signature(
            status,
            &response,
            Some(first.sample_ms),
            Some(3_000),
        ) {
            shazam_http_lab::ProgressiveHttpResponse::HttpError { retry, .. } => retry,
            other => panic!("expected retry-bearing matcher failure, got {other:?}"),
        };
        assert_eq!(
            retry,
            Some(shazam_progressive::RetryDirective {
                retry_duration_ms: Some(3_000),
                interval_ms: Some(0),
                retry_duration_seconds_bits: None,
                interval_seconds_bits: None,
            })
        );
        session
            .complete(
                first.attempt,
                RecognitionOutcome::MatcherFailure { retry },
                100,
            )
            .unwrap();
        let second = session
            .poll(100)
            .unwrap()
            .expect("sufficient stopped-generator audio produces a Signature immediately");
        assert_eq!(second.sample_ms, 3_000);

        let second_body = shazam_http_lab::build_request_body(
            &second.signature,
            second.sample_ms,
            1_800_000_000_001,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (status, response) = run_mock_roundtrip_with_response(
            &second_body,
            br#"{"track":{"title":"Stopped Retry Mock Song","subtitle":"Mock Artist"}}"#,
        )
        .unwrap();
        let candidates =
            match shazam_http_lab::interpret_progressive_http_response(status, &response) {
                shazam_http_lab::ProgressiveHttpResponse::Matched { candidates, .. } => candidates,
                other => panic!("expected mock track response, got {other:?}"),
            };
        assert_eq!(candidates[0].title, "Stopped Retry Mock Song");
        assert_eq!(
            session
                .complete(
                    second.attempt,
                    RecognitionOutcome::Match { retry: None },
                    150
                )
                .unwrap(),
            SessionStatus::Succeeded
        );
    }

    #[test]
    fn stopped_generator_retry_dispatches_at_response_boundary_without_another_pcm_chunk() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 250,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: Some(12_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: None,
                maximum_attempts: None,
            },
            NativeTaggingType::Manual,
        )
        .unwrap();
        let samples: Vec<i16> = (0..48_000)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        session.push_i16(&samples[16_000..]).unwrap();
        assert_eq!(session.frames_seen(), 48_000);
        assert!(session.poll(0).unwrap().is_none());

        let retry = shazam_progressive::RetryDirective {
            retry_duration_ms: Some(2_500),
            interval_ms: Some(12_000),
            retry_duration_seconds_bits: None,
            interval_seconds_bits: None,
        };
        let mut attempts = 0;
        let mut matched = false;
        let immediate = apply_progressive_completion(
            &mut session,
            ProgressiveCompletion {
                attempt: first.attempt,
                sample_ms: first.sample_ms,
                signature_ready_ms: first.generated_at_ms,
                detached: false,
                result: ProgressiveAttemptResult {
                    status: Some(503),
                    outcome: RecognitionOutcome::MatcherFailure { retry: Some(retry) },
                    retry: Some(retry),
                    candidates: Vec::new(),
                    error: Some("mock matcher failure"),
                    post_started_ms: Some(1_025),
                    response_received_ms: Some(1_100),
                },
            },
            Instant::now(),
            &mut attempts,
            &mut matched,
        )
        .unwrap()
        .expect("stopped Native generator retries immediately from retained PCM");

        assert_eq!(attempts, first.attempt);
        assert!(!matched);
        assert_eq!(immediate.attempt, first.attempt + 1);
        assert_eq!(immediate.sample_ms, 3_000);
        assert_eq!(
            session.status(),
            SessionStatus::InFlight {
                attempt: first.attempt + 1,
            }
        );
    }

    #[test]
    fn progressive_http_mock_type2_ignores_matcher_retry_until_generator_max() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 250,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: 12_000,
                maximum_retry_duration_ms: Some(2_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: Some(12_000),
                maximum_attempts: Some(2),
            },
            NativeTaggingType::Auto,
        )
        .unwrap();
        let samples: Vec<i16> = (0..48_000)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        let first_body = shazam_http_lab::build_request_body(
            &first.signature,
            first.sample_ms,
            1_800_000_000_000,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (status, response) = run_mock_roundtrip_with_status(
            503,
            &first_body,
            br#"{"error":"retry","sampleseconds":2,"intervalseconds":0.5}"#,
        )
        .unwrap();
        let retry = match shazam_http_lab::interpret_progressive_http_response_for_signature(
            status,
            &response,
            Some(first.sample_ms),
            Some(2_000),
        ) {
            shazam_http_lab::ProgressiveHttpResponse::HttpError { retry, .. } => retry,
            other => panic!("expected retry-bearing matcher failure, got {other:?}"),
        };
        assert_eq!(
            retry,
            Some(shazam_progressive::RetryDirective {
                retry_duration_ms: Some(2_000),
                interval_ms: Some(0),
                retry_duration_seconds_bits: None,
                interval_seconds_bits: None,
            })
        );

        // Native Type 2's SHKAudioClosure.updateRetry returns without
        // forwarding matcher-error Retry to the SignaturePipe because
        // keepTaggingAfterAMatch is true. The cycle timer was already
        // scheduled when this Signature was filtered; the matcher-error
        // Retry does not retime it or replace its 12-second default.
        session.push_i16(&samples[16_000..32_000]).unwrap();
        session
            .complete(
                first.attempt,
                RecognitionOutcome::MatcherFailure { retry },
                100,
            )
            .unwrap();
        assert_eq!(
            session.status(),
            SessionStatus::WaitingForRetry {
                not_before_ms: 12_000,
                target_sample_ms: 3_000,
            }
        );
        session.push_i16_at(&samples[32_000..], 100).unwrap();
        assert_eq!(session.status(), SessionStatus::GeneratorStopped);
        assert!(session.poll(100).unwrap().is_none());
        assert_eq!(session.frames_seen(), 48_000);
        assert!(session.poll(5_000).unwrap().is_none());
        assert_eq!(session.status(), SessionStatus::GeneratorStopped);
        assert!(session.poll(12_000).unwrap().is_none());
        assert_eq!(session.status(), SessionStatus::Capturing);
        session.push_i16_at(&samples, 12_000).unwrap();
        let second = session.poll(12_000).unwrap().unwrap();
        assert_eq!(second.attempt, 2);
        assert_eq!(second.sample_ms, 3_000);
    }

    #[test]
    fn progressive_http_mock_manual_match_retry_accepts_pcm_while_request_is_in_flight() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 250,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: Some(3_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: None,
                maximum_attempts: Some(3),
            },
            NativeTaggingType::Manual,
        )
        .unwrap();
        let samples: Vec<i16> = (0..80_000)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        let request_body = shazam_http_lab::build_request_body(
            &first.signature,
            first.sample_ms,
            1_800_000_000_000,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();

        let (accepted_sender, accepted_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let request = thread::spawn(move || {
            run_mock_roundtrip_with_gate(
                &request_body,
                br#"{"track":{"title":"In-flight Mock Song","subtitle":"Mock Artist"},"sampleseconds":2,"intervalseconds":0.5}"#,
                Some((accepted_sender, release_receiver)),
            )
        });
        accepted_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("mock server accepted the in-flight request");

        // The HTTP request is blocked at the mock server while new PCM is fed
        // to the same SigX-backed session.
        let pushed = session.push_i16(&samples[16_000..32_000]).unwrap();
        assert_eq!(pushed.total_frames, 32_000);
        release_sender.send(()).unwrap();
        let (status, response) = request
            .join()
            .expect("mock HTTP task did not panic")
            .unwrap();
        let (first_candidates, retry) =
            match shazam_http_lab::interpret_progressive_http_response(status, &response) {
                shazam_http_lab::ProgressiveHttpResponse::Matched { candidates, retry } => {
                    (candidates, retry)
                }
                other => panic!("expected matched response with retry, got {other:?}"),
            };
        assert_eq!(first_candidates[0].title, "In-flight Mock Song");
        assert_eq!(
            retry,
            Some(shazam_progressive::RetryDirective {
                retry_duration_ms: Some(2_000),
                interval_ms: Some(500),
                retry_duration_seconds_bits: Some(2.0_f64.to_bits()),
                interval_seconds_bits: Some(0.5_f64.to_bits()),
            })
        );
        session
            .complete(first.attempt, RecognitionOutcome::Match { retry }, 100)
            .unwrap();
        assert!(session.poll(100).unwrap().is_none());
        let pushed_after_retry = session.push_i16_at(&samples[32_000..32_001], 101).unwrap();
        assert_eq!(pushed_after_retry.total_frames, 32_001);
        let second = session.poll(101).unwrap().unwrap();
        assert_eq!(second.sample_ms, 2_000);
        assert_eq!(second.attempt, 2);

        let second_body = shazam_http_lab::build_request_body(
            &second.signature,
            second.sample_ms,
            1_800_000_000_001,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let (status, response) = run_mock_roundtrip_with_response(
            &second_body,
            br#"{"track":{"title":"Final Mock Song","subtitle":"Mock Artist"}}"#,
        )
        .unwrap();
        assert_eq!(status, 200);
        let final_candidates =
            match shazam_http_lab::interpret_progressive_http_response(status, &response) {
                shazam_http_lab::ProgressiveHttpResponse::Matched {
                    candidates,
                    retry: None,
                } => candidates,
                other => panic!("expected final match without retry, got {other:?}"),
            };
        assert_eq!(final_candidates[0].title, "Final Mock Song");
        session
            .complete(
                second.attempt,
                RecognitionOutcome::Match { retry: None },
                102,
            )
            .unwrap();
        session.finish_input();
        assert_eq!(session.status(), SessionStatus::Succeeded);
    }

    #[test]
    fn progressive_http_mock_errors_without_retry_preserve_type2_session() {
        for (status_code, response_body, expected, expected_status) in [
            (
                503,
                &br#"{"track":{"title":"ignored","subtitle":"ignored"}}"#[..],
                shazam_http_lab::ProgressiveHttpResponse::HttpError {
                    status: 503,
                    retry: None,
                },
                SessionStatus::Capturing,
            ),
            (
                200,
                &b"not json"[..],
                shazam_http_lab::ProgressiveHttpResponse::ParseFailure { retry: None },
                SessionStatus::Capturing,
            ),
        ] {
            let mut session = ProgressiveSession::new(RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 1_000,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: Some(3_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: Some(1_000),
                maximum_attempts: Some(3),
            })
            .unwrap();
            let samples: Vec<i16> = (0..16_000)
                .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
                .collect();
            session.push_i16(&samples).unwrap();
            let job = session.poll(0).unwrap().unwrap();
            let request_body = shazam_http_lab::build_request_body(
                &job.signature,
                job.sample_ms,
                1_800_000_000_000,
                "Asia/Tokyo",
                json!({}),
                json!({}),
                None,
            )
            .unwrap();
            let (status, response) =
                run_mock_roundtrip_with_status(status_code, &request_body, response_body).unwrap();
            let classification =
                shazam_http_lab::interpret_progressive_http_response(status, &response);
            assert_eq!(classification, expected);
            let outcome = match classification {
                shazam_http_lab::ProgressiveHttpResponse::HttpError { retry, .. } => {
                    RecognitionOutcome::MatcherFailure { retry }
                }
                shazam_http_lab::ProgressiveHttpResponse::ParseFailure { retry } => {
                    RecognitionOutcome::MatcherFailure { retry }
                }
                shazam_http_lab::ProgressiveHttpResponse::InvalidResponse => {
                    RecognitionOutcome::InvalidResponse
                }
                other => panic!("unexpected mock failure classification: {other:?}"),
            };
            assert_eq!(
                session.complete(job.attempt, outcome, 1).unwrap(),
                expected_status
            );
            if expected_status == SessionStatus::Capturing {
                session.finish_input();
                assert_eq!(session.status(), SessionStatus::Exhausted);
            }

            let mut manual = ProgressiveSession::new_for_type(
                RecognitionTiming {
                    sample_rate: 16_000,
                    minimum_signature_ms: 1_000,
                    initial_sample_ms: 1_000,
                    maximum_sample_ms: 3_000,
                    default_retry_interval_ms: 0,
                    maximum_retry_duration_ms: Some(3_000),
                    session_timeout_ms: None,
                    request_timeout_ms: 5_000,
                    maximum_retry_delay_ms: Some(1_000),
                    maximum_attempts: Some(3),
                },
                NativeTaggingType::Manual,
            )
            .unwrap();
            manual.push_i16(&samples).unwrap();
            let manual_job = manual.poll(0).unwrap().unwrap();
            assert_eq!(
                manual.complete(manual_job.attempt, outcome, 1).unwrap(),
                SessionStatus::Failed(shazam_progressive::StopReason::InvalidResponse)
            );
        }
    }

    #[test]
    fn progressive_http_mock_response_after_cancellation_is_ignored() {
        let mut session = ProgressiveSession::new(RecognitionTiming {
            sample_rate: 16_000,
            minimum_signature_ms: 1_000,
            initial_sample_ms: 1_000,
            maximum_sample_ms: 3_000,
            default_retry_interval_ms: 0,
            maximum_retry_duration_ms: Some(3_000),
            session_timeout_ms: None,
            request_timeout_ms: 5_000,
            maximum_retry_delay_ms: Some(1_000),
            maximum_attempts: Some(3),
        })
        .unwrap();
        let samples: Vec<i16> = (0..16_000)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        session.push_i16(&samples).unwrap();
        let job = session.poll(0).unwrap().unwrap();
        let request_body = shazam_http_lab::build_request_body(
            &job.signature,
            job.sample_ms,
            1_800_000_000_000,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();

        let (accepted_sender, accepted_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let request = thread::spawn(move || {
            run_mock_roundtrip_with_gate(
                &request_body,
                br#"{"track":{"title":"Late Mock Song","subtitle":"Late Mock Artist"}}"#,
                Some((accepted_sender, release_receiver)),
            )
        });
        accepted_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("mock server accepted the in-flight request");
        assert_eq!(session.cancel(), Some(job.attempt));
        assert_eq!(session.status(), SessionStatus::Cancelled);

        release_sender.send(()).unwrap();
        let (status, response) = request
            .join()
            .expect("mock HTTP task did not panic")
            .unwrap();
        let (outcome, candidates, error) =
            match shazam_http_lab::interpret_progressive_http_response(status, &response) {
                shazam_http_lab::ProgressiveHttpResponse::Matched { candidates, retry } => {
                    (RecognitionOutcome::Match { retry }, candidates, None)
                }
                other => panic!("expected late mock match, got {other:?}"),
            };
        assert_eq!(candidates[0].title, "Late Mock Song");
        let mut attempts = 0;
        let mut matched = false;
        apply_progressive_completion(
            &mut session,
            ProgressiveCompletion {
                attempt: job.attempt,
                sample_ms: job.sample_ms,
                signature_ready_ms: 1,
                detached: false,
                result: ProgressiveAttemptResult {
                    status: Some(status),
                    outcome,
                    retry: None,
                    candidates,
                    error,
                    post_started_ms: Some(1),
                    response_received_ms: Some(2),
                },
            },
            Instant::now(),
            &mut attempts,
            &mut matched,
        )
        .unwrap();
        assert_eq!(attempts, job.attempt);
        assert!(!matched);
        assert_eq!(session.status(), SessionStatus::Cancelled);
    }

    #[test]
    fn signature_response_applies_after_generator_stops_at_maximum() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 1_000,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: Some(12_000),
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: None,
                maximum_attempts: None,
            },
            NativeTaggingType::Manual,
        )
        .unwrap();
        let samples: Vec<i16> = (0..48_001)
            .map(|index| ((index * 73 % 30_001) - 15_000) as i16)
            .collect();
        session.push_i16(&samples[..16_000]).unwrap();
        let initial = session.poll(0).unwrap().unwrap();
        session.push_i16(&samples[16_000..48_000]).unwrap();
        session.push_i16(&samples[48_000..]).unwrap();
        assert!(session.poll(0).unwrap().is_none());

        let started = Instant::now();
        let (completion_sender, _completion_receiver) = std::sync::mpsc::channel();
        let mut runtime = ProgressiveWorkerRuntime {
            started,
            request_timeout_ms: 5_000,
            attempts: 0,
            matched: false,
            active_workers: BTreeSet::from([initial.attempt]),
            completion_sender,
        };
        let configuration = shazam_http_lab::parse_shazam_config(
            br#"{"inid":"fixture-id","apis":{"tag":{"href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true"}}}"#,
        )
        .unwrap();

        handle_progressive_completion(
            ProgressiveCompletion {
                attempt: initial.attempt,
                sample_ms: initial.sample_ms,
                signature_ready_ms: initial.generated_at_ms,
                detached: false,
                result: ProgressiveAttemptResult {
                    status: Some(200),
                    outcome: RecognitionOutcome::Match { retry: None },
                    retry: None,
                    candidates: vec![shazam_http_lab::TrackCandidate {
                        track_id: None,
                        title: "Concurrent mock match".into(),
                        artist: "Test artist".into(),
                        ui_metadata: None,
                        alternative_track: None,
                        origin: shazam_http_lab::TrackCandidateOrigin::RootTrack,
                    }],
                    error: None,
                    post_started_ms: Some(1),
                    response_received_ms: Some(2),
                },
            },
            &mut session,
            &configuration,
            &mut runtime,
        )
        .unwrap();
        assert!(runtime.matched);
        assert!(runtime.active_workers.is_empty());
        assert_eq!(runtime.attempts, initial.attempt);
        assert_eq!(session.status(), SessionStatus::Succeeded);
    }

    #[test]
    fn progressive_adapter_ignores_duplicate_response_after_terminal_result() {
        let mut session = ProgressiveSession::new_for_type(
            RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 250,
                initial_sample_ms: 1_000,
                maximum_sample_ms: 2_000,
                default_retry_interval_ms: 0,
                maximum_retry_duration_ms: None,
                session_timeout_ms: None,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: None,
                maximum_attempts: None,
            },
            NativeTaggingType::Manual,
        )
        .unwrap();
        let samples: Vec<i16> = (0..32_000)
            .map(|index| ((index * 61 % 24_001) - 12_000) as i16)
            .collect();
        session.push_i16(&samples[..16_000]).unwrap();
        let first = session.poll(0).unwrap().unwrap();
        let mut attempts = 0;
        let mut matched = false;
        assert!(apply_progressive_completion(
            &mut session,
            ProgressiveCompletion {
                attempt: first.attempt,
                sample_ms: first.sample_ms,
                signature_ready_ms: first.generated_at_ms,
                detached: false,
                result: ProgressiveAttemptResult {
                    status: Some(200),
                    outcome: RecognitionOutcome::NoMatch { retry: None },
                    retry: None,
                    candidates: Vec::new(),
                    error: None,
                    post_started_ms: Some(1),
                    response_received_ms: Some(2),
                },
            },
            Instant::now(),
            &mut attempts,
            &mut matched,
        )
        .unwrap()
        .is_none());

        assert!(apply_progressive_completion(
            &mut session,
            ProgressiveCompletion {
                attempt: first.attempt,
                sample_ms: first.sample_ms,
                signature_ready_ms: first.generated_at_ms,
                detached: false,
                result: ProgressiveAttemptResult {
                    status: Some(200),
                    outcome: RecognitionOutcome::Match { retry: None },
                    retry: None,
                    candidates: vec![shazam_http_lab::TrackCandidate {
                        track_id: None,
                        title: "Should not surface".into(),
                        artist: "Context-gated artist".into(),
                        ui_metadata: None,
                        alternative_track: None,
                        origin: shazam_http_lab::TrackCandidateOrigin::RootTrack,
                    }],
                    error: None,
                    post_started_ms: Some(1),
                    response_received_ms: Some(2),
                },
            },
            Instant::now(),
            &mut attempts,
            &mut matched,
        )
        .unwrap()
        .is_none());
        assert!(!matched);
        assert_eq!(attempts, first.attempt);
        assert_eq!(session.status(), SessionStatus::Exhausted);
    }

    #[test]
    fn extracts_iana_timezone_from_default_zoneinfo_symlink() {
        assert_eq!(
            timezone_from_zoneinfo_path(Path::new(
                "/var/db/timezone/zoneinfo.default/Europe/Paris"
            )),
            Some("Europe/Paris".into())
        );
    }

    #[test]
    fn rejects_paths_outside_zoneinfo_tree() {
        assert_eq!(
            timezone_from_zoneinfo_path(Path::new("/etc/localtime")),
            None
        );
    }
}
