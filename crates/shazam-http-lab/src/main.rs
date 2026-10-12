use std::{
    env, fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::{Command, ExitCode},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(any(test, not(windows)))]
use std::path::Path;

use serde_json::{json, Value};
use shazam_progressive::{ProgressiveSession, RecognitionOutcome, SessionStatus};

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
    if !(1..=2).contains(&args.len()) {
        eprintln!("usage: shazam-http-lab audio-mock <audio-file> [seconds]");
        return ExitCode::from(2);
    }
    let duration_seconds = match args
        .get(1)
        .map(String::as_str)
        .unwrap_or("12")
        .parse::<u32>()
    {
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
    if !(1..=2).contains(&args.len()) {
        eprintln!("usage: shazam-http-lab audio-progressive-live <audio-file> [seconds]");
        return ExitCode::from(2);
    }
    let duration_seconds = match args
        .get(1)
        .map(String::as_str)
        .unwrap_or("12")
        .parse::<u32>()
    {
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
    minimum_sample_fallback_ms: u64,
    default_retry_delay_ms: u64,
    maximum_retry_delay_ms: u64,
    request_timeout_ms: u64,
    maximum_attempts: u8,
}

fn progressive_local_policy(
    timing: shazam_http_lab::ManualTaggingTiming,
) -> Result<ProgressiveLocalPolicy, String> {
    let initial_sample_ms = timing
        .progressive_ms
        .or(timing.sample_ms)
        .ok_or_else(|| "Config has no initial sample target".to_owned())?;
    let maximum_retry_delay_ms = env_u64_setting(
        "OPENSHZAM_PROGRESSIVE_RETRY_DELAY_CAP_MS",
        30_000,
        0,
        600_000,
    )?;
    let default_retry_delay_ms = env_u64_setting(
        "OPENSHZAM_PROGRESSIVE_DEFAULT_RETRY_DELAY_MS",
        0,
        0,
        maximum_retry_delay_ms,
    )?;
    let request_timeout_ms = env_u64_setting(
        "OPENSHZAM_PROGRESSIVE_REQUEST_TIMEOUT_MS",
        20_000,
        1,
        600_000,
    )?;
    let minimum_sample_fallback_ms = env_u64_setting(
        "OPENSHZAM_PROGRESSIVE_MINIMUM_SAMPLE_FALLBACK_MS",
        initial_sample_ms,
        1,
        600_000,
    )?;
    let maximum_attempts = env_u64_setting("OPENSHZAM_PROGRESSIVE_MAX_ATTEMPTS", 4, 1, 32)?
        .try_into()
        .map_err(|_| "maximum attempts does not fit in u8".to_owned())?;

    Ok(ProgressiveLocalPolicy {
        minimum_sample_fallback_ms,
        default_retry_delay_ms,
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
    let local_policy = match progressive_local_policy(configuration.manual_timing()) {
        Ok(policy) => policy,
        Err(error) => {
            eprintln!("invalid local progressive settings: {error}");
            return ExitCode::from(1);
        }
    };
    let mut timing = match configuration.manual_timing().recognition_timing(
        sample_rate,
        local_policy.minimum_sample_fallback_ms,
        local_policy.default_retry_delay_ms,
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
    if timing.maximum_sample_ms < timing.initial_sample_ms {
        eprintln!("audio prefix is shorter than Config's initial sample target");
        return ExitCode::from(1);
    }
    let mut session = match ProgressiveSession::new(timing) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("could not initialize progressive recognition: {error}");
            return ExitCode::from(1);
        }
    };
    println!(
        "progressive session: sample_rate_hz={sample_rate} first_signature_threshold_ms={} maximum_sample_ms={} maximum_attempts={} elapsed_origin=after_config_and_decode",
        timing.initial_sample_ms.max(timing.minimum_signature_ms),
        timing.maximum_sample_ms,
        timing.maximum_attempts
    );
    let started = Instant::now();
    let chunk_frames = (sample_rate as usize / 10).max(1);
    let (completion_sender, completion_receiver) = std::sync::mpsc::channel();
    let mut offset = 0;
    let mut attempts = 0u8;
    let mut matched = false;
    let mut worker_active = false;

    while offset < samples.len() {
        if session_is_terminal(session.status()) {
            break;
        }
        let end = (offset + chunk_frames).min(samples.len());
        let report = match session.push_i16(&samples[offset..end]) {
            Ok(report) => report,
            Err(error) => {
                eprintln!("progressive PCM input failed: {error}");
                return ExitCode::from(1);
            }
        };
        offset += report.accepted_frames;
        // Replay file PCM at its sample cadence. The recognition worker runs
        // concurrently, so request latency does not stop PCM delivery. Poll
        // only after this chunk's represented audio time has elapsed; otherwise
        // the final chunk would be recognized up to one chunk (100 ms) early.
        let target_elapsed_ms =
            (offset as u128 * 1_000 / u128::from(sample_rate)).min(u128::from(u64::MAX)) as u64;
        while elapsed_ms(started) < target_elapsed_ms && !session_is_terminal(session.status()) {
            let remaining = target_elapsed_ms.saturating_sub(elapsed_ms(started));
            let wait = Duration::from_millis(remaining.min(25));
            if worker_active {
                match completion_receiver.recv_timeout(wait) {
                    Ok(completion) => {
                        worker_active = false;
                        if let Err(error) = apply_progressive_completion(
                            &mut session,
                            completion,
                            started,
                            &mut attempts,
                            &mut matched,
                        ) {
                            eprintln!("progressive response could not be applied: {error}");
                            return ExitCode::from(1);
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        eprintln!("recognition worker stopped without a result");
                        worker_active = false;
                        fail_disconnected_worker(&mut session, elapsed_ms(started), &mut attempts);
                    }
                }
                if worker_active {
                    if let Err(error) = session.poll(elapsed_ms(started)) {
                        eprintln!("progressive timeout check failed: {error}");
                        return ExitCode::from(1);
                    }
                }
            } else {
                thread::sleep(wait);
            }
        }
        if matched || session_is_terminal(session.status()) {
            break;
        }
        let job = match session.poll(elapsed_ms(started)) {
            Ok(job) => job,
            Err(error) => {
                eprintln!("progressive session failed: {error}");
                return ExitCode::from(1);
            }
        };
        if let Some(job) = job {
            if worker_active {
                eprintln!("progressive engine produced overlapping HTTP work");
                return ExitCode::from(1);
            }
            if let Err(error) = dispatch_progressive_job(
                &configuration,
                job,
                started,
                timing.request_timeout_ms,
                completion_sender.clone(),
            ) {
                eprintln!("could not start recognition worker: {error}");
                return ExitCode::from(1);
            }
            worker_active = true;
        }
        if report.accepted_frames == 0 {
            break;
        }
    }

    session.finish_input();
    loop {
        if worker_active {
            match completion_receiver.recv_timeout(Duration::from_millis(25)) {
                Ok(completion) => {
                    worker_active = false;
                    if let Err(error) = apply_progressive_completion(
                        &mut session,
                        completion,
                        started,
                        &mut attempts,
                        &mut matched,
                    ) {
                        eprintln!("progressive response could not be applied: {error}");
                        return ExitCode::from(1);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if let Err(error) = session.poll(elapsed_ms(started)) {
                        eprintln!("progressive timeout check failed: {error}");
                        return ExitCode::from(1);
                    }
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    eprintln!("recognition worker stopped without a result");
                    fail_disconnected_worker(&mut session, elapsed_ms(started), &mut attempts);
                    break;
                }
            }
            continue;
        }

        if session_is_terminal(session.status()) {
            break;
        }
        let now_ms = elapsed_ms(started);
        match session.poll(now_ms) {
            Ok(Some(job)) => {
                if let Err(error) = dispatch_progressive_job(
                    &configuration,
                    job,
                    started,
                    timing.request_timeout_ms,
                    completion_sender.clone(),
                ) {
                    eprintln!("could not start recognition worker: {error}");
                    return ExitCode::from(1);
                }
                worker_active = true;
            }
            Ok(None) => match session.status() {
                SessionStatus::WaitingForRetry { not_before_ms, .. } if now_ms < not_before_ms => {
                    thread::sleep(Duration::from_millis((not_before_ms - now_ms).min(25)));
                }
                SessionStatus::InFlight { .. } => {
                    eprintln!("in-flight request has no recognition worker");
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

    if matched {
        ExitCode::SUCCESS
    } else {
        println!(
            "no match after {attempts} attempt(s); accepted sample_ms={}",
            session.sample_ms()
        );
        ExitCode::from(1)
    }
}

#[derive(Debug)]
struct ProgressiveAttemptResult {
    status: Option<u16>,
    outcome: RecognitionOutcome,
    candidates: Vec<shazam_http_lab::TrackCandidate>,
    error: Option<&'static str>,
    post_started_ms: Option<u64>,
    response_received_ms: Option<u64>,
}

struct ProgressiveCompletion {
    attempt: u8,
    sample_ms: u64,
    signature_ready_ms: u64,
    result: ProgressiveAttemptResult,
}

fn dispatch_progressive_job(
    configuration: &shazam_http_lab::ShazamConfiguration,
    job: shazam_progressive::RecognitionJob,
    session_started: Instant,
    request_timeout_ms: u64,
    sender: std::sync::mpsc::Sender<ProgressiveCompletion>,
) -> Result<(), String> {
    let attempt = job.attempt;
    let sample_ms = job.sample_ms;
    let signature_ready_ms = job.generated_at_ms;
    let configuration = (*configuration).clone();
    thread::Builder::new()
        .name(format!("openshazam-recognition-{attempt}"))
        .spawn(move || {
            let result =
                submit_progressive_job(&configuration, &job, session_started, request_timeout_ms);
            let _ = sender.send(ProgressiveCompletion {
                attempt,
                sample_ms,
                signature_ready_ms,
                result,
            });
        })
        .map(|_| ())
        .map_err(|_| "could not start the bounded recognition worker".to_owned())
}

fn submit_progressive_job(
    configuration: &shazam_http_lab::ShazamConfiguration,
    job: &shazam_progressive::RecognitionJob,
    session_started: Instant,
    request_timeout_ms: u64,
) -> ProgressiveAttemptResult {
    let failed = |error| ProgressiveAttemptResult {
        status: None,
        outcome: RecognitionOutcome::TransportFailure,
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
    let timestamp_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(timestamp) => timestamp.as_millis().min(u128::from(u64::MAX)) as u64,
        Err(_) => return failed("system clock is before the Unix epoch"),
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
    let response = shazam_http_lab::post_shazam_json_with_timeout(
        &endpoint,
        &body,
        Duration::from_millis(request_timeout_ms),
    );
    let response_received_ms = elapsed_ms(session_started);
    let response = match response {
        Ok(response) => response,
        Err(_) => {
            return ProgressiveAttemptResult {
                status: None,
                outcome: RecognitionOutcome::TransportFailure,
                candidates: Vec::new(),
                error: Some("HTTPS recognition request failed"),
                post_started_ms: Some(post_started_ms),
                response_received_ms: Some(response_received_ms),
            };
        }
    };
    let (outcome, candidates, error) =
        match shazam_http_lab::interpret_progressive_http_response(response.status, &response.body)
        {
            shazam_http_lab::ProgressiveHttpResponse::Matched(candidates) => {
                (RecognitionOutcome::Match, candidates, None)
            }
            shazam_http_lab::ProgressiveHttpResponse::NoMatch(retry) => {
                (RecognitionOutcome::NoMatch { retry }, Vec::new(), None)
            }
            shazam_http_lab::ProgressiveHttpResponse::HttpError(_) => (
                RecognitionOutcome::TransportFailure,
                Vec::new(),
                Some("recognition HTTP response was unsuccessful; body withheld"),
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
    attempts: &mut u8,
    matched: &mut bool,
) -> Result<(), String> {
    *attempts = completion.attempt;
    let post_rtt_ms = completion
        .result
        .post_started_ms
        .zip(completion.result.response_received_ms)
        .map(|(started, received)| received.saturating_sub(started));
    println!(
        "attempt={} cumulative_sample_ms={} signature_ready_elapsed_ms={} post_started_elapsed_ms={:?} response_received_elapsed_ms={:?} post_rtt_ms={:?} HTTP status={:?}",
        completion.attempt,
        completion.sample_ms,
        completion.signature_ready_ms,
        completion.result.post_started_ms,
        completion.result.response_received_ms,
        post_rtt_ms,
        completion.result.status
    );

    let in_flight_attempt = match session.status() {
        SessionStatus::InFlight { attempt } => attempt,
        _ => return Ok(()),
    };
    if in_flight_attempt != completion.attempt {
        return Err("received a response for a non-current attempt".into());
    }
    session
        .complete(
            completion.attempt,
            completion.result.outcome,
            elapsed_ms(session_started),
        )
        .map_err(|error| error.to_string())?;
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
    Ok(())
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn fail_disconnected_worker(session: &mut ProgressiveSession, now_ms: u64, attempts: &mut u8) {
    if let SessionStatus::InFlight { attempt } = session.status() {
        *attempts = attempt;
        if let Err(error) = session.complete(attempt, RecognitionOutcome::TransportFailure, now_ms)
        {
            eprintln!("could not close disconnected recognition attempt: {error}");
        }
    }
}

fn session_is_terminal(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Succeeded
            | SessionStatus::Exhausted
            | SessionStatus::Failed(_)
            | SessionStatus::Cancelled
    )
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
                "manual timing fields (ms): sample={:?} progressive={:?} minimum={:?} maximum={:?}",
                timing.sample_ms,
                timing.progressive_ms,
                timing.minimum_sample_ms,
                timing.maximum_sample_ms
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
    eprintln!("   or: shazam-http-lab audio-mock <audio-file> [seconds]");
    eprintln!("   or: shazam-http-lab signature-live <signature.bin> <sample-duration-ms>");
    eprintln!("   or: shazam-http-lab audio-live <audio-file> [seconds]");
    eprintln!("   or: shazam-http-lab audio-progressive-live <audio-file> [seconds]");
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
        apply_progressive_completion, run_mock_roundtrip_with_gate,
        run_mock_roundtrip_with_response, run_mock_roundtrip_with_status,
        timezone_from_zoneinfo_path, ProgressiveAttemptResult, ProgressiveCompletion,
    };
    use serde_json::json;
    use shazam_progressive::{
        ProgressiveSession, RecognitionOutcome, RecognitionTiming, SessionStatus, StopReason,
    };
    use std::{
        path::Path,
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
    fn extracts_iana_timezone_from_macos_zoneinfo_symlink() {
        assert_eq!(
            timezone_from_zoneinfo_path(Path::new("/var/db/timezone/zoneinfo/Asia/Tokyo")),
            Some("Asia/Tokyo".into())
        );
    }

    #[test]
    fn progressive_signature_mock_http_retry_and_match_end_to_end() {
        let mut session = ProgressiveSession::new(RecognitionTiming {
            sample_rate: 16_000,
            minimum_signature_ms: 1_000,
            initial_sample_ms: 1_000,
            progressive_step_ms: 1_000,
            maximum_sample_ms: 3_000,
            default_retry_interval_ms: 0,
            request_timeout_ms: 5_000,
            maximum_retry_delay_ms: 1_000,
            maximum_attempts: 3,
        })
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
        let second = session.poll(51).unwrap().unwrap();
        assert_eq!(second.sample_ms, 2_000);
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
        let (status, response) = run_mock_roundtrip_with_response(
            &match_body,
            br#"{"track":{"title":"Mock Progressive Song","subtitle":"Mock Artist"}}"#,
        )
        .unwrap();
        assert_eq!(status, 200);
        let candidates =
            match shazam_http_lab::interpret_progressive_http_response(status, &response) {
                shazam_http_lab::ProgressiveHttpResponse::Matched(candidates) => candidates,
                other => panic!("expected mock track response, got {other:?}"),
            };
        assert_eq!(candidates[0].title, "Mock Progressive Song");
        assert_eq!(candidates[0].artist, "Mock Artist");
        assert_eq!(
            session
                .complete(second.attempt, RecognitionOutcome::Match, 52)
                .unwrap(),
            SessionStatus::Succeeded
        );
    }

    #[test]
    fn progressive_http_mock_initial_match_finishes_first_attempt() {
        let mut session = ProgressiveSession::new(RecognitionTiming {
            sample_rate: 16_000,
            minimum_signature_ms: 1_000,
            initial_sample_ms: 1_000,
            progressive_step_ms: 1_000,
            maximum_sample_ms: 3_000,
            default_retry_interval_ms: 0,
            request_timeout_ms: 5_000,
            maximum_retry_delay_ms: 1_000,
            maximum_attempts: 3,
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
        let (status, response) = run_mock_roundtrip_with_response(
            &request_body,
            br#"{"track":{"title":"Initial Mock Song","subtitle":"Initial Mock Artist"}}"#,
        )
        .unwrap();
        let candidates =
            match shazam_http_lab::interpret_progressive_http_response(status, &response) {
                shazam_http_lab::ProgressiveHttpResponse::Matched(candidates) => candidates,
                other => panic!("expected initial match, got {other:?}"),
            };
        assert_eq!(candidates[0].title, "Initial Mock Song");
        assert_eq!(candidates[0].artist, "Initial Mock Artist");
        assert_eq!(
            session
                .complete(job.attempt, RecognitionOutcome::Match, 1)
                .unwrap(),
            SessionStatus::Succeeded
        );
    }

    #[test]
    fn progressive_http_mock_error_and_invalid_json_are_terminal() {
        for (status_code, response_body, expected, reason) in [
            (
                503,
                &br#"{"track":{"title":"ignored","subtitle":"ignored"}}"#[..],
                shazam_http_lab::ProgressiveHttpResponse::HttpError(503),
                StopReason::TransportFailure,
            ),
            (
                200,
                &b"not json"[..],
                shazam_http_lab::ProgressiveHttpResponse::InvalidResponse,
                StopReason::InvalidResponse,
            ),
        ] {
            let mut session = ProgressiveSession::new(RecognitionTiming {
                sample_rate: 16_000,
                minimum_signature_ms: 1_000,
                initial_sample_ms: 1_000,
                progressive_step_ms: 1_000,
                maximum_sample_ms: 3_000,
                default_retry_interval_ms: 0,
                request_timeout_ms: 5_000,
                maximum_retry_delay_ms: 1_000,
                maximum_attempts: 3,
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
                shazam_http_lab::ProgressiveHttpResponse::HttpError(_) => {
                    RecognitionOutcome::TransportFailure
                }
                shazam_http_lab::ProgressiveHttpResponse::InvalidResponse => {
                    RecognitionOutcome::InvalidResponse
                }
                other => panic!("unexpected mock failure classification: {other:?}"),
            };
            assert_eq!(
                session.complete(job.attempt, outcome, 1).unwrap(),
                SessionStatus::Failed(reason)
            );
        }
    }

    #[test]
    fn progressive_http_mock_response_after_cancellation_is_ignored() {
        let mut session = ProgressiveSession::new(RecognitionTiming {
            sample_rate: 16_000,
            minimum_signature_ms: 1_000,
            initial_sample_ms: 1_000,
            progressive_step_ms: 1_000,
            maximum_sample_ms: 3_000,
            default_retry_interval_ms: 0,
            request_timeout_ms: 5_000,
            maximum_retry_delay_ms: 1_000,
            maximum_attempts: 3,
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
                shazam_http_lab::ProgressiveHttpResponse::Matched(candidates) => {
                    (RecognitionOutcome::Match, candidates, None)
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
                result: ProgressiveAttemptResult {
                    status: Some(status),
                    outcome,
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
