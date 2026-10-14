#![allow(unknown_lints, clippy::manual_is_multiple_of)]

#[cfg(any(windows, target_os = "macos", test))]
use std::io;

#[cfg(any(windows, target_os = "macos"))]
use std::env;

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

#[cfg(any(windows, target_os = "macos"))]
use std::sync::atomic::AtomicU64;

#[cfg(windows)]
use std::{thread, thread::JoinHandle};

#[cfg(windows)]
use openshazam_controller::{
    EventEmitter, RecognitionErrorKind, RecognitionEventKind, RecognitionMode, SessionEndReason,
    SessionId,
};

#[cfg(any(windows, target_os = "macos"))]
static NEXT_UI_CONFIG_GENERATION: AtomicU64 = AtomicU64::new(1);

#[cfg(target_os = "macos")]
use std::process::Command;

#[cfg(any(windows, test, target_os = "macos"))]
use sigx_core::{Config, SigX};

#[cfg(windows)]
use std::{
    collections::BTreeMap,
    sync::{
        atomic::AtomicBool,
        mpsc::{self, TryRecvError},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use shazam_progressive::{
    NativeSampleFrameTargets, ProgressiveSession, RecognitionOutcome, RecognitionTiming,
    RollingPcmBuffer, NATIVE_NETWORK_REQUEST_TIMEOUT_MS, NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS,
    NATIVE_TAGGING_TYPE_2_DEFAULT_INTERVAL_MS,
};

use shazam_progressive::NativeTaggingType;

#[cfg(windows)]
use crate::worker_lifecycle::WorkerSet;
use shazam_http_lab::NativeTaggingRequestContext;

#[cfg(windows)]
use shazam_progressive::SessionStatus;

#[cfg(all(target_os = "macos", feature = "dev-cli"))]
use shazam_progressive::SessionStatus;

#[cfg(test)]
const CAPTURE_RATE: u32 = 16_000;
#[cfg(all(target_os = "macos", feature = "dev-cli"))]
#[path = "macos_capture.rs"]
pub(crate) mod macos_capture;

#[cfg(all(target_os = "macos", feature = "dev-cli"))]
pub(super) fn manual_completion_is_normal(
    status: SessionStatus,
    matched: bool,
    no_match_seen: bool,
) -> bool {
    status == SessionStatus::Succeeded
        || (status == SessionStatus::Exhausted && (matched || no_match_seen))
}

#[cfg(target_os = "macos")]
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressiveCaptureMode {
    pub tagging_type: NativeTaggingType,
    pub request_context: Option<NativeTaggingRequestContext>,
}

#[derive(Clone)]
pub struct NativeContextState(Arc<AtomicU8>);

impl NativeContextState {
    pub fn new(initial: Option<NativeTaggingRequestContext>) -> Self {
        Self(Arc::new(AtomicU8::new(encode_native_context(initial))))
    }

    pub fn current(&self) -> Option<NativeTaggingRequestContext> {
        decode_native_context(self.0.load(Ordering::Acquire))
    }

    pub fn set(&self, context: NativeTaggingRequestContext) {
        self.0
            .store(encode_native_context(Some(context)), Ordering::Release);
    }
}

const fn encode_native_context(context: Option<NativeTaggingRequestContext>) -> u8 {
    match context {
        None => 0,
        Some(NativeTaggingRequestContext::Foreground) => 1,
        Some(NativeTaggingRequestContext::Background) => 2,
    }
}

const fn decode_native_context(value: u8) -> Option<NativeTaggingRequestContext> {
    match value {
        1 => Some(NativeTaggingRequestContext::Foreground),
        2 => Some(NativeTaggingRequestContext::Background),
        _ => None,
    }
}

#[cfg(all(feature = "dev-cli", any(windows, target_os = "macos")))]
fn progressive_log(jsonl_output: bool, message: impl std::fmt::Display) {
    if jsonl_output {
        eprintln!("{message}");
    } else {
        println!("{message}");
    }
}

#[cfg(all(windows, feature = "dev-cli"))]
fn progressive_log_for_backend(
    events: &Option<(SessionId, RecognitionMode, EventEmitter)>,
    jsonl_output: bool,
    message: impl FnOnce() -> String,
) {
    if events.is_none() {
        progressive_log(jsonl_output, message());
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

#[cfg(all(windows, not(feature = "dev-cli")))]
fn progressive_log_for_backend(
    _events: &Option<(SessionId, RecognitionMode, EventEmitter)>,
    _jsonl_output: bool,
    _message: impl FnOnce() -> String,
) {
}

#[cfg(all(feature = "dev-cli", any(windows, target_os = "macos")))]
fn emit_manual_ui_event(
    event: &shazam_http_lab::NativeManualUiEvent,
    jsonl_output: bool,
    config_generation: u64,
) {
    if jsonl_output {
        println!("{}", shazam_http_lab::native_manual_ui_event_to_json(event));
        if ui_config_snapshot_jsonl_enabled() {
            println!(
                "{}",
                shazam_http_lab::UiRecognitionInput {
                    config_generation,
                    event: shazam_http_lab::UiRecognitionEvent::Manual(event.clone()),
                }
                .to_json()
            );
        }
        return;
    }

    match event {
        shazam_http_lab::NativeManualUiEvent::Result(result) => {
            println!(
                "match title={:?} artist={:?}",
                result.primary_track.title, result.primary_track.artist
            );
            if let Some(alternative) = result.alternative_track.as_deref() {
                println!(
                    "alternative_track title={:?} artist={:?}",
                    alternative.title, alternative.artist
                );
            }
        }
        shazam_http_lab::NativeManualUiEvent::NoMatch => println!("No match."),
        shazam_http_lab::NativeManualUiEvent::Error { kind } => {
            eprintln!("Recognition failed ({kind:?}).");
        }
    }
}

#[cfg(all(feature = "dev-cli", any(windows, target_os = "macos")))]
fn emit_auto_ui_event(
    event: &shazam_http_lab::NativeAutoUiEvent,
    jsonl_output: bool,
    config_generation: u64,
) {
    if jsonl_output {
        println!("{}", shazam_http_lab::native_auto_ui_event_to_json(event));
        if ui_config_snapshot_jsonl_enabled() {
            println!(
                "{}",
                shazam_http_lab::UiRecognitionInput {
                    config_generation,
                    event: shazam_http_lab::UiRecognitionEvent::Auto(event.clone()),
                }
                .to_json()
            );
        }
        return;
    }

    match event {
        shazam_http_lab::NativeAutoUiEvent::ContinuousMatchCandidate(result) => {
            println!(
                "continuous match candidate title={:?} artist={:?}",
                result.primary_track.title, result.primary_track.artist
            );
            if let Some(alternative) = result.alternative_track.as_deref() {
                println!(
                    "alternative_track title={:?} artist={:?}",
                    alternative.title, alternative.artist
                );
            }
        }
    }
}

#[cfg(all(feature = "dev-cli", any(windows, target_os = "macos")))]
fn ui_config_snapshot_jsonl_enabled() -> bool {
    env::var_os("OPENSHZAM_UI_CONFIG_SNAPSHOT_JSONL").is_some_and(|value| value == "1")
}

#[cfg(all(feature = "dev-cli", any(windows, target_os = "macos")))]
fn emit_session_timeout_event(tagging_type: NativeTaggingType, jsonl_output: bool) {
    if jsonl_output {
        println!(
            "{}",
            shazam_http_lab::native_session_timeout_event_to_json(tagging_type)
        );
    } else {
        println!("Recognition session timed out.");
    }
}

#[cfg(any(windows, target_os = "macos"))]
#[derive(Clone, Copy)]
struct ProgressiveUiConfigInputs {
    tagging_type: NativeTaggingType,
    request_context: Option<NativeTaggingRequestContext>,
    sample_rate_hz: u32,
    pre_record_capacity_frames: usize,
    diagnostic_pre_record_override: bool,
    request_timeout_ms: u64,
    jsonl_output: bool,
}

#[cfg(all(feature = "dev-cli", any(windows, target_os = "macos")))]
fn emit_progressive_ui_config(
    configuration: &shazam_http_lab::ShazamConfiguration,
    inputs: ProgressiveUiConfigInputs,
) -> Result<u64, io::Error> {
    let generation = NEXT_UI_CONFIG_GENERATION.fetch_add(1, Ordering::AcqRel);
    if !inputs.jsonl_output {
        return Ok(generation);
    }
    let record = match inputs.tagging_type {
        NativeTaggingType::Manual => configuration.native_manual_ui_config_json(
            inputs.sample_rate_hz,
            inputs.pre_record_capacity_frames,
            inputs.diagnostic_pre_record_override,
            inputs.request_timeout_ms,
        ),
        NativeTaggingType::Auto => configuration.native_auto_ui_config_json(
            inputs.sample_rate_hz,
            inputs.request_context.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Auto UI config requires explicit foreground/background context",
                )
            })?,
            inputs.request_timeout_ms,
        ),
    }
    .map_err(io::Error::other)?;
    println!("{record}");
    if ui_config_snapshot_jsonl_enabled() {
        println!("{}", configuration.ui_config_snapshot(generation).to_json());
    }
    Ok(generation)
}

#[cfg(all(not(feature = "dev-cli"), any(windows, target_os = "macos")))]
fn emit_progressive_ui_config(
    _configuration: &shazam_http_lab::ShazamConfiguration,
    _inputs: ProgressiveUiConfigInputs,
) -> Result<u64, io::Error> {
    Ok(NEXT_UI_CONFIG_GENERATION.fetch_add(1, Ordering::AcqRel))
}

#[cfg(any(windows, target_os = "macos"))]
fn notify_after_retry_dispatch_ack<E>(
    wait_for_dispatch_ack: impl FnOnce() -> Result<(), E>,
    notify: impl FnOnce(),
) -> Result<(), E> {
    let dispatch_result = wait_for_dispatch_ack();
    notify();
    dispatch_result
}

#[cfg(any(windows, test))]
fn qpc_timestamp_to_unix_ms(
    packet_qpc_100ns: u64,
    current_qpc_100ns: u64,
    current_unix_ms: u64,
) -> Option<u64> {
    let elapsed_ms = current_qpc_100ns
        .checked_sub(packet_qpc_100ns)?
        .checked_div(10_000)?;
    current_unix_ms.checked_sub(elapsed_ms)
}

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ManualTriggerSignal {
    Enter,
    #[cfg(any(test, feature = "dev-cli"))]
    EndOfInput,
    #[cfg(any(test, feature = "dev-cli"))]
    InputError(String),
}

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
enum ManualTriggerPoll {
    Pending,
    Started,
    Ended,
    #[cfg(any(test, feature = "dev-cli"))]
    InputError(String),
    Cancelled,
}

#[cfg(any(windows, test))]
fn classify_manual_trigger_poll(
    cancelled: bool,
    received: Result<ManualTriggerSignal, std::sync::mpsc::TryRecvError>,
) -> ManualTriggerPoll {
    if cancelled {
        return ManualTriggerPoll::Cancelled;
    }
    match received {
        Ok(ManualTriggerSignal::Enter) => ManualTriggerPoll::Started,
        #[cfg(any(test, feature = "dev-cli"))]
        Ok(ManualTriggerSignal::EndOfInput) => ManualTriggerPoll::Ended,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => ManualTriggerPoll::Ended,
        #[cfg(any(test, feature = "dev-cli"))]
        Ok(ManualTriggerSignal::InputError(error)) => ManualTriggerPoll::InputError(error),
        Err(std::sync::mpsc::TryRecvError::Empty) => ManualTriggerPoll::Pending,
    }
}

#[cfg(windows)]
fn poll_manual_trigger(
    receiver: &std::sync::mpsc::Receiver<ManualTriggerSignal>,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<Option<bool>, io::Error> {
    match classify_manual_trigger_poll(
        cancelled.load(std::sync::atomic::Ordering::Acquire),
        receiver.try_recv(),
    ) {
        ManualTriggerPoll::Pending => Ok(None),
        ManualTriggerPoll::Started => Ok(Some(true)),
        ManualTriggerPoll::Ended => Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "manual recognition trigger input closed",
        )),
        #[cfg(any(test, feature = "dev-cli"))]
        ManualTriggerPoll::InputError(error) => Err(io::Error::other(error)),
        ManualTriggerPoll::Cancelled => Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "cancelled while waiting for manual recognition trigger",
        )),
    }
}

#[cfg(any(windows, test))]
fn ignore_expected_timeout<E>(
    result: Result<(), E>,
    is_timeout: impl FnOnce(&E) -> bool,
) -> Result<(), E> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if is_timeout(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "macos")]
fn config_request_profile_from_host() -> Result<shazam_http_lab::ConfigRequestProfile, String> {
    let language = env_setting("SHAZAM_CONFIG_LANGUAGE")
        .or_else(|| {
            command_value("defaults", &["read", "-g", "AppleLanguages"]).and_then(|raw| {
                first_quoted_value(&raw).map(|value| {
                    value
                        .split(['-', '_'])
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                })
            })
        })
        .or_else(|| {
            locale_from_environment().and_then(|locale| locale.split('_').next().map(str::to_owned))
        })
        .unwrap_or_else(|| "en".into());
    let locale = env_setting("SHAZAM_CONFIG_LOCALE")
        .or_else(|| {
            command_value("defaults", &["read", "-g", "AppleLocale"])
                .map(|value| {
                    value
                        .trim()
                        .split('.')
                        .next()
                        .unwrap_or_default()
                        .replace('-', "_")
                })
                .filter(|value| !value.is_empty())
        })
        .or_else(locale_from_environment)
        .unwrap_or_else(|| "en_US".into());
    let model = env_setting("SHAZAM_CONFIG_MODEL")
        .or_else(|| command_value("sysctl", &["-n", "hw.model"]))
        .ok_or_else(|| {
            "could not read the Mac model (set SHAZAM_CONFIG_MODEL to override)".to_owned()
        })?;
    let os_version = env_setting("SHAZAM_CONFIG_OS_VERSION")
        .or_else(|| command_value("sw_vers", &["-productVersion"]))
        .ok_or_else(|| {
            "could not read macOS version (set SHAZAM_CONFIG_OS_VERSION to override)".to_owned()
        })?;
    let os_build = env_setting("SHAZAM_CONFIG_OS_BUILD")
        .or_else(|| command_value("sw_vers", &["-buildVersion"]))
        .ok_or_else(|| {
            "could not read macOS build (set SHAZAM_CONFIG_OS_BUILD to override)".to_owned()
        })?;

    Ok(shazam_http_lab::ConfigRequestProfile {
        country: env_setting("SHAZAM_CONFIG_COUNTRY").unwrap_or_else(|| "-".into()),
        language,
        locale,
        model,
        os_version,
        os_build,
        app_version: "2.11.0".into(),
    })
}

#[cfg(target_os = "macos")]
fn env_setting(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
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
fn first_quoted_value(value: &str) -> Option<String> {
    let start = value.find('"')? + 1;
    let end = start + value[start..].find('"')?;
    Some(value[start..end].to_owned())
}

#[cfg(target_os = "macos")]
fn locale_from_environment() -> Option<String> {
    let locale = env::var("LC_ALL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| env::var("LANG").ok())?;
    let locale = locale.split('.').next()?.replace('-', "_");
    (!locale.is_empty()).then_some(locale)
}

#[cfg(test)]
fn decode_mono_f32le(bytes: &[u8]) -> io::Result<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "capture packet ends in a partial Float32 sample",
        ));
    }

    bytes
        .chunks_exact(4)
        .map(|chunk| {
            let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            if sample.is_finite() {
                Ok(sample)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "capture packet contains a non-finite sample",
                ))
            }
        })
        .collect()
}

#[cfg(any(windows, test, target_os = "macos"))]
fn core_error(error: impl std::fmt::Debug) -> io::Error {
    io::Error::other(format!("SigX: {error:?}"))
}

/// Platform-independent packet-to-SigX boundary shared by capture adapters.
#[cfg(any(windows, test, target_os = "macos"))]
struct CaptureAccumulator {
    sigx: SigX,
    target_frames: usize,
    frames_seen: usize,
}

#[cfg(any(windows, test, target_os = "macos"))]
impl CaptureAccumulator {
    #[cfg(test)]
    fn new(target_frames: usize) -> io::Result<Self> {
        Self::new_for_rate(CAPTURE_RATE, target_frames)
    }

    fn new_for_rate(sample_rate: u32, target_frames: usize) -> io::Result<Self> {
        if target_frames == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "capture length must be nonzero",
            ));
        }
        let sigx = SigX::new(Config {
            sample_rate,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: target_frames,
        })
        .map_err(core_error)?;
        Ok(Self {
            sigx,
            target_frames,
            frames_seen: 0,
        })
    }

    fn remaining_frames(&self) -> usize {
        self.target_frames - self.frames_seen
    }

    #[cfg(test)]
    fn frames_seen(&self) -> usize {
        self.frames_seen
    }

    #[cfg(test)]
    fn push_packet(
        &mut self,
        bytes: &[u8],
        packet_frames: usize,
        silent: bool,
        data_discontinuity: bool,
    ) -> io::Result<()> {
        if data_discontinuity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "capture packet reports a data discontinuity",
            ));
        }

        let frames_to_use = packet_frames.min(self.remaining_frames());
        if frames_to_use == 0 {
            return Ok(());
        }
        let samples = if silent {
            vec![0.0; frames_to_use]
        } else {
            let byte_count = frames_to_use
                .checked_mul(4)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "packet too large"))?;
            let packet = bytes.get(..byte_count).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "capture packet has fewer bytes than reported frames",
                )
            })?;
            decode_mono_f32le(packet)?
        };

        self.push_samples(&samples)
    }

    #[cfg(any(windows, test, target_os = "macos"))]
    fn push_interleaved_f32(&mut self, samples: &[f32], channels: usize) -> io::Result<()> {
        if channels == 0 || samples.len() % channels != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "capture packet is not a whole number of interleaved frames",
            ));
        }

        let frames_to_use = (samples.len() / channels).min(self.remaining_frames());
        let mut mono = Vec::with_capacity(frames_to_use);
        for frame in samples.chunks_exact(channels).take(frames_to_use) {
            if frame.iter().any(|sample| !sample.is_finite()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "capture packet contains a non-finite sample",
                ));
            }
            mono.push(frame.iter().copied().sum::<f32>() / channels as f32);
        }
        self.push_samples(&mono)
    }

    fn push_samples(&mut self, samples: &[f32]) -> io::Result<()> {
        let frames_to_use = samples.len().min(self.remaining_frames());
        let samples = &samples[..frames_to_use];
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "capture packet contains a non-finite sample",
            ));
        }
        self.sigx.push_f32(samples).map_err(core_error)?;
        self.frames_seen += samples.len();
        Ok(())
    }

    fn finish(&self) -> io::Result<Vec<u8>> {
        if self.frames_seen != self.target_frames {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "capture ended after {} of {} frames",
                    self.frames_seen, self.target_frames
                ),
            ));
        }
        self.sigx.signature().map_err(core_error)
    }
}

#[cfg(all(windows, feature = "dev-cli"))]
pub fn list_render_devices() -> Result<(), Box<dyn std::error::Error>> {
    use wasapi::{initialize_mta, DeviceEnumerator, Direction};

    initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    let default_id = enumerator
        .get_default_device(&Direction::Render)
        .and_then(|device| device.get_id())
        .ok();
    let devices = enumerator.get_device_collection(&Direction::Render)?;
    if devices.get_nbr_devices()? == 0 {
        println!("No active Windows playback devices were found.");
        return Ok(());
    }
    for index in 0..devices.get_nbr_devices()? {
        let device = devices.get_device_at_index(index)?;
        let id = device.get_id()?;
        let default_marker = if default_id.as_deref() == Some(id.as_str()) {
            " (default)"
        } else {
            ""
        };
        println!("{}{}\n  {id}", device.get_friendlyname()?, default_marker);
    }
    Ok(())
}

#[cfg(all(windows, feature = "dev-cli"))]
pub fn capture_loopback_signature(
    selected_device_id: Option<&str>,
    capture_seconds: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use wasapi::{initialize_mta, AudioCaptureClient, DeviceEnumerator, Direction, StreamMode};

    initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    let device = match selected_device_id {
        Some(device_id) => enumerator.get_device(device_id)?,
        None => enumerator
            .get_default_device(&Direction::Render)
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no default Windows playback device is available: {error:?}"),
                )
            })?,
    };
    if device.get_direction() != Direction::Render {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the selected endpoint is not a playback device; use `list-devices` to choose an output",
        )
        .into());
    }
    let device_name = device.get_friendlyname()?;
    let mut audio_client = device.get_iaudioclient()?;
    let native_format = audio_client.get_mixformat()?;
    let (format, auto_convert) = if format_usable_by_sigx(&native_format) {
        let (_, minimum_period_hns) = audio_client.get_device_period()?;
        let mode = StreamMode::EventsShared {
            autoconvert: false,
            buffer_duration_hns: minimum_period_hns.max(200_000),
        };
        if audio_client
            .initialize_client(&native_format, &Direction::Capture, &mode)
            .is_ok()
        {
            (native_format, false)
        } else {
            audio_client = device.get_iaudioclient()?;
            (canonical_loopback_format(), true)
        }
    } else {
        (canonical_loopback_format(), true)
    };
    if auto_convert {
        println!(
            "Using Windows shared-mode conversion because the endpoint mix format is not directly supported by SigX."
        );
    }
    if auto_convert {
        let (_, minimum_period_hns) = audio_client.get_device_period()?;
        audio_client.initialize_client(
            &format,
            &Direction::Capture,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: minimum_period_hns.max(200_000),
            },
        )?;
    }
    let sample_rate = format.get_samplespersec();
    let channels = usize::from(format.get_nchannels());
    let frames = (sample_rate as usize)
        .checked_mul(capture_seconds)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "capture length overflow"))?;
    let mut accumulator = CaptureAccumulator::new_for_rate(sample_rate, frames)?;
    let event = audio_client.set_get_eventhandle()?;
    let capture: AudioCaptureClient = audio_client.get_audiocaptureclient()?;
    let mut packet_scratch = match audio_client.get_buffer_size() {
        Ok(buffer_frames) => WasapiPacketScratch::with_capacity(
            buffer_frames as usize,
            channels,
            format.get_blockalign() as usize,
        )?,
        Err(_) => WasapiPacketScratch::default(),
    };

    println!(
        "Capturing {capture_seconds}s of system playback from {device_name:?} at {sample_rate} Hz, {channels} channel(s)..."
    );
    audio_client.start_stream()?;
    let capture_result = capture_loopback_frames(
        &capture,
        &event,
        &format,
        channels,
        &mut accumulator,
        &mut packet_scratch,
    );
    let stop_result = audio_client.stop_stream();
    capture_result?;
    stop_result?;

    let signature = accumulator.finish()?;
    println!(
        "WASAPI loopback capture complete: {capture_seconds}s PCM -> Pure Rust SigX ({} bytes).",
        signature.len()
    );
    Ok(signature)
}

#[cfg(windows)]
#[cfg(windows)]
pub(crate) struct WindowsProgressiveControl {
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) stop_requested: Arc<AtomicBool>,
    pub(crate) config_generation: Arc<AtomicU64>,
    pub(crate) config_fetching: Arc<AtomicBool>,
    pub(crate) accepted_control: Arc<std::sync::atomic::AtomicU8>,
    pub(crate) cleanup_failed: Arc<AtomicBool>,
    pub(crate) manual_trigger_receiver: Option<std::sync::mpsc::Receiver<ManualTriggerSignal>>,
    pub(crate) events: Option<(SessionId, RecognitionMode, EventEmitter)>,
}

#[cfg(windows)]
struct WindowsAudioStream {
    client: wasapi::AudioClient,
    started: bool,
}

#[cfg(windows)]
impl WindowsAudioStream {
    fn started(client: wasapi::AudioClient) -> Self {
        Self {
            client,
            started: true,
        }
    }

    fn stop(&mut self) -> Result<(), wasapi::WasapiError> {
        let result = self.client.stop_stream();
        if result.is_ok() {
            self.started = false;
        }
        result
    }
}

#[cfg(windows)]
impl std::ops::Deref for WindowsAudioStream {
    type Target = wasapi::AudioClient;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

#[cfg(windows)]
impl Drop for WindowsAudioStream {
    fn drop(&mut self) {
        if self.started {
            let _ = self.client.stop_stream();
        }
    }
}

#[cfg(windows)]
fn emit_backend_event(
    events: &Option<(SessionId, RecognitionMode, EventEmitter)>,
    attempt_id: Option<u64>,
    kind: RecognitionEventKind,
) {
    if let Some((session_id, mode, emitter)) = events {
        let _ = emitter.emit_attempt(Some(*session_id), Some(*mode), attempt_id, kind);
    }
}

#[cfg(windows)]
fn emit_backend_manual_event(
    events: &Option<(SessionId, RecognitionMode, EventEmitter)>,
    attempt_id: u64,
    event: &shazam_http_lab::NativeManualUiEvent,
) {
    emit_backend_event(
        events,
        Some(attempt_id),
        RecognitionEventKind::ManualUiEvent(event.clone()),
    );
    if matches!(event, shazam_http_lab::NativeManualUiEvent::NoMatch) {
        emit_backend_event(
            events,
            Some(attempt_id),
            RecognitionEventKind::NoMatch { attempt_id },
        );
    }
}

#[cfg(windows)]
pub(crate) fn recognize_loopback_progressive_controlled(
    selected_device_id: Option<&str>,
    mode: ProgressiveCaptureMode,
    context_state: NativeContextState,
    jsonl_output: bool,
    mut control: WindowsProgressiveControl,
) -> Result<SessionEndReason, Box<dyn std::error::Error>> {
    use wasapi::{initialize_mta, AudioCaptureClient, DeviceEnumerator, Direction, StreamMode};

    if mode.tagging_type == NativeTaggingType::Auto && context_state.current().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Auto recognition requires an explicit foreground/background context",
        )
        .into());
    }

    initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    let device = match selected_device_id {
        Some(device_id) => enumerator.get_device(device_id)?,
        None => enumerator
            .get_default_device(&Direction::Render)
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no default Windows playback device is available: {error:?}"),
                )
            })?,
    };
    if device.get_direction() != Direction::Render {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the selected endpoint is not a playback device; use `list-devices` to choose an output",
        )
        .into());
    }
    let device_name = device.get_friendlyname()?;
    let mut audio_client = device.get_iaudioclient()?;
    let native_format = audio_client.get_mixformat()?;
    let (format, auto_convert) = if format_usable_by_sigx(&native_format) {
        let (_, minimum_period_hns) = audio_client.get_device_period()?;
        let mode = StreamMode::EventsShared {
            autoconvert: false,
            buffer_duration_hns: minimum_period_hns.max(200_000),
        };
        if audio_client
            .initialize_client(&native_format, &Direction::Capture, &mode)
            .is_ok()
        {
            (native_format, false)
        } else {
            audio_client = device.get_iaudioclient()?;
            (canonical_loopback_format(), true)
        }
    } else {
        (canonical_loopback_format(), true)
    };
    if auto_convert {
        let (_, minimum_period_hns) = audio_client.get_device_period()?;
        audio_client.initialize_client(
            &format,
            &Direction::Capture,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: minimum_period_hns.max(200_000),
            },
        )?;
        progressive_log_for_backend(&control.events, jsonl_output, || {
            "Using Windows shared-mode conversion for the endpoint mix format.".to_owned()
        });
    }

    let sample_rate = format.get_samplespersec();
    let channels = usize::from(format.get_nchannels());
    let event = audio_client.set_get_eventhandle()?;
    let capture: AudioCaptureClient = audio_client.get_audiocaptureclient()?;
    let mut packet_scratch = match audio_client.get_buffer_size() {
        Ok(buffer_frames) => WasapiPacketScratch::with_capacity(
            buffer_frames as usize,
            channels,
            format.get_blockalign() as usize,
        )?,
        Err(_) => WasapiPacketScratch::default(),
    };
    let audio_clock = audio_client.get_audioclock().ok();
    if audio_clock.is_none() {
        #[cfg(feature = "dev-cli")]
        if control.events.is_none() {
            eprintln!(
                "WASAPI audio clock is unavailable; packet timestamps will use the first-packet wall-clock fallback."
            );
        }
    }

    let cancelled = Arc::clone(&control.cancelled);
    let request_cancelled = Arc::new(AtomicBool::new(false));
    let (completion_sender, completion_receiver) = mpsc::channel();
    let mut active_workers = WorkerSet::new(
        Arc::clone(&request_cancelled),
        Arc::clone(&control.cleanup_failed),
    );
    let mut saw_match_event = false;
    let mut saw_no_match_event = false;
    let mut received_audio_packet = false;
    let maximum_prerecord_frames = if mode.tagging_type.uses_pre_recorded_audio() {
        frames_for_ms(sample_rate, 60_000)?
    } else {
        0
    };
    let mut preroll = RollingPcmBuffer::new(maximum_prerecord_frames);
    let mut preroll_start_timestamp_ms = None;
    let mut timestamp_anchor = CaptureTimestampAnchor::default();

    progressive_log_for_backend(&control.events, jsonl_output, || {
        format!("Starting WASAPI capture from {device_name:?} ({sample_rate} Hz, {channels} channel(s)) while Shazam Config loads; PCM remains in a bounded local rolling buffer. Press Ctrl+C to stop.")
    });
    audio_client.start_stream()?;
    let mut audio_client = WindowsAudioStream::started(audio_client);
    emit_backend_event(&control.events, None, RecognitionEventKind::CaptureStarted);

    let (configuration_sender, configuration_receiver) = mpsc::sync_channel(1);
    control.config_fetching.store(true, Ordering::Release);
    let configuration_task = match thread::Builder::new()
        .name("openshazam-config-fetch".into())
        .spawn(move || {
            let result = windows_progressive_profile(sample_rate, mode.tagging_type)
                .map_err(|error| error.to_string());
            let _ = configuration_sender.send(result);
        }) {
        Ok(task) => task,
        Err(error) => {
            control.config_fetching.store(false, Ordering::Release);
            let _ = audio_client.stop();
            return Err(error.into());
        }
    };
    active_workers.insert(u64::MAX, configuration_task);
    let mut configuration_interrupted_by_control = false;
    let configuration_result = (|| -> Result<_, Box<dyn std::error::Error>> {
        loop {
            if cancelled.load(Ordering::Acquire) {
                configuration_interrupted_by_control = true;
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "cancelled while loading Shazam Config",
                )
                .into());
            }
            if control.stop_requested.load(Ordering::Acquire) {
                configuration_interrupted_by_control = true;
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "stopped while loading Shazam Config",
                )
                .into());
            }
            match configuration_receiver.try_recv() {
                Ok(result) => break Ok(result.map_err(io::Error::other)?),
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "Shazam Config worker stopped unexpectedly",
                    )
                    .into());
                }
                Err(TryRecvError::Empty) => {}
            }
            match event.wait_for_event(100) {
                Ok(()) | Err(wasapi::WasapiError::EventTimeout) => {}
                Err(error) => return Err(error.into()),
            }
            append_wasapi_packets(
                &capture,
                &format,
                channels,
                sample_rate,
                &mut preroll,
                &mut preroll_start_timestamp_ms,
                &mut timestamp_anchor,
                audio_clock.as_ref(),
                &mut received_audio_packet,
                &mut packet_scratch,
            )?;
        }
    })();
    let (configuration, timing, native_frame_targets) = match configuration_result {
        Ok(result) => result,
        Err(error) => {
            let stop_result = audio_client.stop();
            request_cancelled.store(true, Ordering::Release);
            let cleanup_result = active_workers.join_all();
            stop_result?;
            cleanup_result?;
            if configuration_interrupted_by_control {
                return Ok(crate::session_end::resolve_session_end(
                    SessionStatus::Capturing,
                    None,
                    false,
                    false,
                    crate::session_end::SessionControlRequests::from_first_request(
                        control.accepted_control.load(Ordering::Acquire),
                    ),
                    false,
                ));
            }
            return Err(error);
        }
    };
    control.config_fetching.store(false, Ordering::Release);
    if active_workers
        .remove(&u64::MAX)
        .expect("Config worker is owned until Config resolves")
        .join()
        .is_err()
    {
        let _ = audio_client.stop();
        control.cleanup_failed.store(true, Ordering::Release);
        return Err(io::Error::other("Shazam Config worker panicked").into());
    }

    let native_pre_record_capacity_ms = if mode.tagging_type.uses_pre_recorded_audio() {
        configuration
            .manual_timing()
            .effective_pre_record_buffer_ms()
    } else {
        0
    };
    if native_pre_record_capacity_ms > 60_000 {
        let _ = audio_client.stop();
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Config pre-record buffer exceeds the local 60-second safety limit",
        )
        .into());
    }
    let pre_record_capacity_override_ms = if mode.tagging_type.uses_pre_recorded_audio() {
        match env_optional_u64_setting("OPENSHZAM_PRERECORD_BUFFER_MS", 0, 60_000) {
            Ok(value) => value,
            Err(error) => {
                let _ = audio_client.stop();
                return Err(error);
            }
        }
    } else if env::var_os("OPENSHZAM_PRERECORD_BUFFER_MS").is_some() {
        let _ = audio_client.stop();
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "OPENSHZAM_PRERECORD_BUFFER_MS applies only to Manual recognition",
        )
        .into());
    } else {
        None
    };
    let pre_record_capacity_mode = if pre_record_capacity_override_ms.is_some() {
        "diagnostic override"
    } else {
        "Native page-rounded"
    };
    let pre_record_capacity_frames = if !mode.tagging_type.uses_pre_recorded_audio() {
        0
    } else {
        match pre_record_capacity_override_ms {
            Some(duration_ms) => match frames_for_ms(sample_rate, duration_ms) {
                Ok(frames) => frames,
                Err(error) => {
                    let _ = audio_client.stop();
                    return Err(error.into());
                }
            },
            None => match configuration
                .manual_timing()
                .effective_pre_record_capacity_frames(sample_rate)
            {
                Ok(frames) => frames,
                Err(error) => {
                    let _ = audio_client.stop();
                    return Err(io::Error::new(io::ErrorKind::InvalidData, error).into());
                }
            },
        }
    };
    let pre_record_capacity_ms =
        match milliseconds_for_frames(sample_rate, pre_record_capacity_frames) {
            Ok(milliseconds) => milliseconds,
            Err(error) => {
                let _ = audio_client.stop();
                return Err(error.into());
            }
        };
    let frames_before_resize = preroll.frames();
    preroll.set_capacity_frames(pre_record_capacity_frames);
    if let Some(start_ms) = preroll_start_timestamp_ms {
        preroll_start_timestamp_ms = advance_timestamp_ms(
            start_ms,
            frames_before_resize.saturating_sub(preroll.frames()),
            sample_rate,
        );
    }
    let config_generation = emit_progressive_ui_config(
        &configuration,
        ProgressiveUiConfigInputs {
            tagging_type: mode.tagging_type,
            request_context: context_state.current(),
            sample_rate_hz: sample_rate,
            pre_record_capacity_frames,
            diagnostic_pre_record_override: pre_record_capacity_override_ms.is_some(),
            request_timeout_ms: timing.request_timeout_ms,
            jsonl_output,
        },
    )?;
    control
        .config_generation
        .store(config_generation, Ordering::Release);
    if let Some((session_id, recognition_mode, emitter)) = control.events.clone() {
        let emitter = emitter.with_config_generation(config_generation);
        control.events = Some((session_id, recognition_mode, emitter.clone()));
        emit_backend_event(
            &control.events,
            None,
            RecognitionEventKind::UiConfigSnapshot(
                configuration.ui_config_snapshot(config_generation),
            ),
        );
    }
    progressive_log_for_backend(&control.events, jsonl_output, || {
        format!(
            "Config resolved: initial_ms={} maximum_ms={} pre_record_capacity_ms={pre_record_capacity_ms} pre_record_capacity_frames={pre_record_capacity_frames} pre_record_capacity_mode={pre_record_capacity_mode}.",
            timing.initial_sample_ms, timing.maximum_sample_ms
        )
    });

    let pre_recorded = if pre_record_capacity_frames == 0 {
        let result = append_wasapi_packets(
            &capture,
            &format,
            channels,
            sample_rate,
            &mut preroll,
            &mut preroll_start_timestamp_ms,
            &mut timestamp_anchor,
            audio_clock.as_ref(),
            &mut received_audio_packet,
            &mut packet_scratch,
        )
        .map(|_| None);
        match result {
            Ok(samples) => {
                drop(preroll);
                samples
            }
            Err(error) => {
                let _ = audio_client.stop();
                return Err(error);
            }
        }
    } else {
        progressive_log_for_backend(&control.events, jsonl_output, || {
            format!("WASAPI rolling pre-record buffer is active; press Enter to start manual recognition (retaining up to {pre_record_capacity_ms} ms; {pre_record_capacity_mode} capacity).")
        });
        let manual_trigger_receiver = control.manual_trigger_receiver.take().unwrap_or_else(|| {
            let (sender, receiver) = mpsc::channel();
            let _ = sender.send(ManualTriggerSignal::Enter);
            receiver
        });
        match capture_wasapi_prerecord(
            &event,
            &capture,
            &format,
            channels,
            sample_rate,
            &manual_trigger_receiver,
            preroll,
            &mut preroll_start_timestamp_ms,
            &mut timestamp_anchor,
            audio_clock.as_ref(),
            &cancelled,
            &control.stop_requested,
            &mut received_audio_packet,
            &mut packet_scratch,
        ) {
            Ok(PreRecordEnd::Triggered(samples)) => Some(samples),
            Ok(PreRecordEnd::Stopped | PreRecordEnd::Cancelled) => {
                request_cancelled.store(true, Ordering::Release);
                let stop_result = audio_client.stop();
                let cleanup_result = active_workers.join_all();
                stop_result?;
                cleanup_result?;
                return Ok(crate::session_end::resolve_session_end(
                    SessionStatus::Capturing,
                    None,
                    false,
                    false,
                    crate::session_end::SessionControlRequests::from_first_request(
                        control.accepted_control.load(Ordering::Acquire),
                    ),
                    false,
                ));
            }
            Err(error) => {
                let _ = audio_client.stop();
                return Err(error);
            }
        }
    };
    let mut session = match ProgressiveSession::new_for_type_with_native_frame_targets(
        timing,
        mode.tagging_type,
        pre_record_capacity_frames,
        Some(native_frame_targets),
    ) {
        Ok(session) => session,
        Err(error) => {
            let _ = audio_client.stop();
            return Err(io::Error::new(io::ErrorKind::InvalidInput, error).into());
        }
    };
    let session_start_timestamp_ms = if mode.tagging_type.uses_pre_recorded_audio() {
        preroll_start_timestamp_ms.or_else(unix_timestamp_ms)
    } else {
        unix_timestamp_ms()
    }
    .ok_or_else(|| io::Error::other("system clock is before Unix epoch"))?;
    if let Err(error) = session.set_session_start_timestamp_ms(session_start_timestamp_ms) {
        let _ = audio_client.stop();
        return Err(io::Error::new(io::ErrorKind::InvalidData, error).into());
    }
    if let Some(pre_recorded) = pre_recorded {
        let seeded_frames = match session.prime_pre_recorded_f32(&pre_recorded) {
            Ok(frames) => frames,
            Err(error) => {
                let _ = audio_client.stop();
                return Err(io::Error::new(io::ErrorKind::InvalidData, error).into());
            }
        };
        progressive_log_for_backend(&control.events, jsonl_output, || {
            format!("Manual recognition started with {seeded_frames} buffered PCM frames.")
        });
    }
    let started = Instant::now();

    let capture_result = (|| -> Result<(), Box<dyn std::error::Error>> {
        'capture: while !session.is_terminal() {
            if cancelled.load(Ordering::Acquire) {
                session.cancel();
                request_cancelled.store(true, Ordering::Release);
                break;
            }
            if control.stop_requested.load(Ordering::Acquire) {
                break;
            }

            service_windows_progressive(
                &mut session,
                &configuration,
                context_state.clone(),
                started,
                jsonl_output,
                config_generation,
                timing.request_timeout_ms,
                &completion_sender,
                &completion_receiver,
                &request_cancelled,
                &mut active_workers,
                &mut saw_match_event,
                &mut saw_no_match_event,
                &control.events,
            )?;
            if session.is_terminal() {
                break;
            }

            ignore_expected_timeout(event.wait_for_event(100), |error| {
                matches!(error, wasapi::WasapiError::EventTimeout)
            })?;
            if cancelled.load(Ordering::Acquire) {
                session.cancel();
                request_cancelled.store(true, Ordering::Release);
                break;
            }
            if control.stop_requested.load(Ordering::Acquire) {
                break;
            }
            service_windows_progressive(
                &mut session,
                &configuration,
                context_state.clone(),
                started,
                jsonl_output,
                config_generation,
                timing.request_timeout_ms,
                &completion_sender,
                &completion_receiver,
                &request_cancelled,
                &mut active_workers,
                &mut saw_match_event,
                &mut saw_no_match_event,
                &control.events,
            )?;
            loop {
                let Some(packet_frames) = capture.get_next_packet_size()? else {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "WASAPI capture client did not report shared-mode packet sizes",
                    )
                    .into());
                };
                if packet_frames == 0 {
                    break;
                }
                let byte_count = (packet_frames as usize)
                    .checked_mul(format.get_blockalign() as usize)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "packet too large")
                    })?;
                packet_scratch.bytes.resize(byte_count, 0);
                let (read_frames, info) = capture.read_from_device(&mut packet_scratch.bytes)?;
                if info.flags.data_discontinuity && received_audio_packet {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "WASAPI reported a capture discontinuity; retry after playback is stable",
                    )
                    .into());
                }
                if read_frames == 0 {
                    continue;
                }
                received_audio_packet = true;
                let frame_count = read_frames as usize;
                if frame_count != 0 {
                    let source_timestamp_ms = capture_timestamp_ms(
                        &mut timestamp_anchor,
                        audio_clock.as_ref(),
                        info.timestamp,
                        info.flags.timestamp_error,
                    );
                    if info.flags.silent {
                        fill_silent_samples(&mut packet_scratch.mono, frame_count);
                    } else {
                        let byte_count = frame_count
                            .checked_mul(format.get_blockalign() as usize)
                            .ok_or_else(|| {
                                io::Error::new(io::ErrorKind::InvalidData, "packet too large")
                            })?;
                        let packet_bytes =
                            packet_scratch.bytes.get(..byte_count).ok_or_else(|| {
                                io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "WASAPI returned fewer bytes than reported frames",
                                )
                            })?;
                        decode_wasapi_interleaved_into(
                            packet_bytes,
                            frame_count,
                            &format,
                            &mut packet_scratch.interleaved,
                        )?;
                        downmix_interleaved_f32_into(
                            &packet_scratch.interleaved,
                            channels,
                            &mut packet_scratch.mono,
                        )?;
                    }
                    let report = session
                        .push_f32_at_with_source_timestamp_ns(
                            &packet_scratch.mono,
                            elapsed_ns(started),
                            source_timestamp_ms,
                        )
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                    let _ = report;
                }

                if cancelled.load(Ordering::Acquire) {
                    session.cancel();
                    request_cancelled.store(true, Ordering::Release);
                    break 'capture;
                }
                if control.stop_requested.load(Ordering::Acquire) {
                    break 'capture;
                }

                service_windows_progressive(
                    &mut session,
                    &configuration,
                    context_state.clone(),
                    started,
                    jsonl_output,
                    config_generation,
                    timing.request_timeout_ms,
                    &completion_sender,
                    &completion_receiver,
                    &request_cancelled,
                    &mut active_workers,
                    &mut saw_match_event,
                    &mut saw_no_match_event,
                    &control.events,
                )?;
                if session.is_terminal() {
                    break;
                }
            }

            // Loopback can legitimately have no packet while playback is
            // idle. Native ends the session using its Config-derived timeout
            // when present; otherwise the user can stop it with Ctrl+C. Don't
            // substitute a shorter local empty-wakeup deadline here.
            service_windows_progressive(
                &mut session,
                &configuration,
                context_state.clone(),
                started,
                jsonl_output,
                config_generation,
                timing.request_timeout_ms,
                &completion_sender,
                &completion_receiver,
                &request_cancelled,
                &mut active_workers,
                &mut saw_match_event,
                &mut saw_no_match_event,
                &control.events,
            )?;
        }
        Ok(())
    })();

    let stop_result = audio_client.stop();
    let recognition_result = (|| -> Result<SessionEndReason, Box<dyn std::error::Error>> {
        capture_result?;
        stop_result?;
        let terminal_before_input_end = session.is_terminal().then(|| session.lifecycle_status());
        session.finish_input();

        while !session.is_terminal() || !active_workers.is_empty() {
            if cancelled.load(Ordering::Acquire) {
                session.cancel();
                request_cancelled.store(true, Ordering::Release);
                break;
            }
            service_windows_progressive(
                &mut session,
                &configuration,
                context_state.clone(),
                started,
                jsonl_output,
                config_generation,
                timing.request_timeout_ms,
                &completion_sender,
                &completion_receiver,
                &request_cancelled,
                &mut active_workers,
                &mut saw_match_event,
                &mut saw_no_match_event,
                &control.events,
            )?;
            if active_workers.is_empty() && !session.is_terminal() {
                thread::sleep(Duration::from_millis(20));
            }
        }

        Ok(crate::session_end::resolve_session_end(
            session.lifecycle_status(),
            terminal_before_input_end,
            saw_match_event,
            saw_no_match_event,
            crate::session_end::SessionControlRequests::from_first_request(
                control.accepted_control.load(Ordering::Acquire),
            ),
            false,
        ))
    })();

    if recognition_result.is_err() || cancelled.load(Ordering::Acquire) {
        request_cancelled.store(true, Ordering::Release);
        session.cancel();
    }

    let cleanup_result = active_workers.join_all();
    cleanup_result?;

    let status = session.status();
    if matches!(
        status,
        SessionStatus::Failed(shazam_progressive::StopReason::SessionTimeout)
    ) {
        #[cfg(feature = "dev-cli")]
        if control.events.is_none() {
            emit_session_timeout_event(session.tagging_type(), jsonl_output);
        }
        emit_backend_event(&control.events, None, RecognitionEventKind::SessionTimedOut);
    }
    recognition_result
}

#[cfg(windows)]
#[derive(Default)]
struct CaptureTimestampAnchor {
    first_qpc_timestamp_100ns: Option<u64>,
    first_wall_timestamp_ms: Option<u64>,
}

#[cfg(windows)]
fn unix_timestamp_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()
}

#[cfg(windows)]
fn capture_timestamp_ms(
    anchor: &mut CaptureTimestampAnchor,
    audio_clock: Option<&wasapi::AudioClock>,
    qpc_timestamp_100ns: u64,
    timestamp_error: bool,
) -> Option<u64> {
    if timestamp_error {
        return None;
    }
    if let Some(audio_clock) = audio_clock {
        if let (Ok((_, current_qpc_100ns)), Some(current_unix_ms)) =
            (audio_clock.get_position(), unix_timestamp_ms())
        {
            if let Some(timestamp_ms) =
                qpc_timestamp_to_unix_ms(qpc_timestamp_100ns, current_qpc_100ns, current_unix_ms)
            {
                return Some(timestamp_ms);
            }
        }
    }
    if anchor.first_qpc_timestamp_100ns.is_none() {
        anchor.first_wall_timestamp_ms = unix_timestamp_ms();
        anchor.first_qpc_timestamp_100ns = Some(qpc_timestamp_100ns);
    }
    let elapsed_ms = qpc_timestamp_100ns.checked_sub(anchor.first_qpc_timestamp_100ns?)? / 10_000;
    anchor.first_wall_timestamp_ms?.checked_add(elapsed_ms)
}

#[cfg(windows)]
fn advance_timestamp_ms(timestamp_ms: u64, frames: usize, sample_rate: u32) -> Option<u64> {
    if sample_rate == 0 {
        return None;
    }
    let elapsed_ms = (frames as u128)
        .checked_mul(1_000)?
        .checked_div(u128::from(sample_rate))?;
    timestamp_ms.checked_add(u64::try_from(elapsed_ms).ok()?)
}

#[cfg(windows)]
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

#[cfg(windows)]
#[derive(Default)]
struct WasapiPacketScratch {
    bytes: Vec<u8>,
    interleaved: Vec<f32>,
    mono: Vec<f32>,
}

#[cfg(any(windows, test))]
fn fill_silent_samples(output: &mut Vec<f32>, sample_count: usize) {
    output.resize(sample_count, 0.0);
    output.fill(0.0);
}

#[cfg(windows)]
impl WasapiPacketScratch {
    fn with_capacity(
        frames: usize,
        channels: usize,
        block_align: usize,
    ) -> Result<Self, io::Error> {
        let byte_capacity = frames.checked_mul(block_align).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "WASAPI buffer too large")
        })?;
        let sample_capacity = frames.checked_mul(channels).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "WASAPI buffer too large")
        })?;
        let mut scratch = Self::default();
        scratch
            .bytes
            .try_reserve_exact(byte_capacity)
            .map_err(io::Error::other)?;
        scratch
            .interleaved
            .try_reserve_exact(sample_capacity)
            .map_err(io::Error::other)?;
        scratch
            .mono
            .try_reserve_exact(frames)
            .map_err(io::Error::other)?;
        Ok(scratch)
    }
}

#[cfg(windows)]
enum PreRecordEnd {
    Triggered(Vec<f32>),
    Stopped,
    Cancelled,
}

#[cfg(windows)]
// Keep WASAPI handles and rolling timestamp state explicit at this adapter boundary.
#[allow(clippy::too_many_arguments)]
fn capture_wasapi_prerecord(
    event: &wasapi::Handle,
    capture: &wasapi::AudioCaptureClient,
    format: &wasapi::WaveFormat,
    channels: usize,
    sample_rate: u32,
    trigger_receiver: &std::sync::mpsc::Receiver<ManualTriggerSignal>,
    mut ring: RollingPcmBuffer<f32>,
    preroll_start_timestamp_ms: &mut Option<u64>,
    timestamp_anchor: &mut CaptureTimestampAnchor,
    audio_clock: Option<&wasapi::AudioClock>,
    cancelled: &AtomicBool,
    stop_requested: &AtomicBool,
    received_audio_packet: &mut bool,
    packet_scratch: &mut WasapiPacketScratch,
) -> Result<PreRecordEnd, Box<dyn std::error::Error>> {
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok(PreRecordEnd::Cancelled);
        }
        if stop_requested.load(Ordering::Acquire) {
            return Ok(PreRecordEnd::Stopped);
        }
        let mut triggered = poll_manual_trigger(trigger_receiver, cancelled)?.unwrap_or(false);

        if !triggered {
            ignore_expected_timeout(event.wait_for_event(100), |error| {
                matches!(error, wasapi::WasapiError::EventTimeout)
            })?;
        }

        if cancelled.load(Ordering::Acquire) {
            return Ok(PreRecordEnd::Cancelled);
        }
        if stop_requested.load(Ordering::Acquire) {
            return Ok(PreRecordEnd::Stopped);
        }

        append_wasapi_packets(
            capture,
            format,
            channels,
            sample_rate,
            &mut ring,
            preroll_start_timestamp_ms,
            timestamp_anchor,
            audio_clock,
            received_audio_packet,
            packet_scratch,
        )?;

        if !triggered {
            triggered = poll_manual_trigger(trigger_receiver, cancelled)?.unwrap_or(false);
        }
        if triggered {
            return Ok(PreRecordEnd::Triggered(ring.into_samples()));
        }
    }
}

#[cfg(windows)]
// Keep packet conversion and timestamp state explicit at this adapter boundary.
#[allow(clippy::too_many_arguments)]
fn append_wasapi_packets(
    capture: &wasapi::AudioCaptureClient,
    format: &wasapi::WaveFormat,
    channels: usize,
    sample_rate: u32,
    ring: &mut RollingPcmBuffer<f32>,
    preroll_start_timestamp_ms: &mut Option<u64>,
    timestamp_anchor: &mut CaptureTimestampAnchor,
    audio_clock: Option<&wasapi::AudioClock>,
    received_audio_packet: &mut bool,
    packet_scratch: &mut WasapiPacketScratch,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut packet_available = false;
    loop {
        let Some(packet_frames) = capture.get_next_packet_size()? else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "WASAPI capture client did not report shared-mode packet sizes",
            )
            .into());
        };
        if packet_frames == 0 {
            break;
        }
        packet_available = true;
        let byte_count = (packet_frames as usize)
            .checked_mul(format.get_blockalign() as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "packet too large"))?;
        packet_scratch.bytes.resize(byte_count, 0);
        let (read_frames, info) = capture.read_from_device(&mut packet_scratch.bytes)?;
        if info.flags.data_discontinuity && *received_audio_packet {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WASAPI reported a capture discontinuity while buffering pre-record audio",
            )
            .into());
        }
        if read_frames == 0 {
            continue;
        }
        let source_timestamp_ms = capture_timestamp_ms(
            timestamp_anchor,
            audio_clock,
            info.timestamp,
            info.flags.timestamp_error,
        );
        *received_audio_packet = true;
        let frame_count = read_frames as usize;
        if info.flags.silent {
            fill_silent_samples(&mut packet_scratch.mono, frame_count);
        } else {
            let byte_count = frame_count
                .checked_mul(format.get_blockalign() as usize)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "packet too large"))?;
            let packet_bytes = packet_scratch.bytes.get(..byte_count).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WASAPI returned fewer bytes than reported frames",
                )
            })?;
            decode_wasapi_interleaved_into(
                packet_bytes,
                frame_count,
                format,
                &mut packet_scratch.interleaved,
            )?;
            downmix_interleaved_f32_into(
                &packet_scratch.interleaved,
                channels,
                &mut packet_scratch.mono,
            )?;
        }
        push_timed_preroll(
            ring,
            preroll_start_timestamp_ms,
            &packet_scratch.mono,
            source_timestamp_ms,
            sample_rate,
        );
    }
    Ok(packet_available)
}

#[cfg(windows)]
fn windows_progressive_profile(
    sample_rate: u32,
    tagging_type: NativeTaggingType,
) -> Result<
    (
        shazam_http_lab::ShazamConfiguration,
        RecognitionTiming,
        NativeSampleFrameTargets,
    ),
    Box<dyn std::error::Error>,
> {
    let profile = shazam_http_lab::windows_config_request_profile()?;
    let configuration = shazam_http_lab::fetch_shazam_config(&profile)?;
    let manual = configuration.manual_timing();
    let native_frame_targets =
        configuration.native_frame_targets_for_type(tagging_type, sample_rate)?;
    let timeout_ms = env_u64_setting(
        "OPENSHZAM_PROGRESSIVE_REQUEST_TIMEOUT_MS",
        NATIVE_NETWORK_REQUEST_TIMEOUT_MS,
        1,
        600_000,
    )?;
    let retry_delay_cap_ms =
        env_optional_u64_setting("OPENSHZAM_PROGRESSIVE_RETRY_DELAY_CAP_MS", 0, 600_000)?;
    let attempts = env_optional_u64_setting("OPENSHZAM_PROGRESSIVE_MAX_ATTEMPTS", 1, u64::MAX)?;
    let default_interval_ms = match tagging_type {
        NativeTaggingType::Manual => NATIVE_TAGGING_TYPE_1_DEFAULT_INTERVAL_MS,
        NativeTaggingType::Auto => NATIVE_TAGGING_TYPE_2_DEFAULT_INTERVAL_MS,
    };
    let timing = match tagging_type {
        NativeTaggingType::Manual => manual.recognition_timing(
            sample_rate,
            shazam_http_lab::NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
            default_interval_ms,
            timeout_ms,
            retry_delay_cap_ms,
            attempts,
        )?,
        NativeTaggingType::Auto => manual.auto_recognition_timing(
            sample_rate,
            shazam_http_lab::NATIVE_DEFAULT_MINIMUM_SIGNATURE_LENGTH_MS,
            default_interval_ms,
            timeout_ms,
            retry_delay_cap_ms,
            attempts,
        )?,
    };
    Ok((configuration, timing, native_frame_targets))
}

#[cfg(windows)]
fn env_u64_setting(
    name: &str,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> Result<u64, Box<dyn std::error::Error>> {
    let value = match env::var(name) {
        Ok(value) => value.parse::<u64>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} must be an unsigned integer"),
            )
        })?,
        Err(env::VarError::NotPresent) => default,
        Err(env::VarError::NotUnicode(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} is not Unicode"),
            )
            .into());
        }
    };
    if !(minimum..=maximum).contains(&value) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be between {minimum} and {maximum}"),
        )
        .into());
    }
    Ok(value)
}

#[cfg(windows)]
fn env_optional_u64_setting(
    name: &str,
    minimum: u64,
    maximum: u64,
) -> Result<Option<u64>, Box<dyn std::error::Error>> {
    match env::var(name) {
        Ok(value) => {
            let parsed = value.parse::<u64>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{name} must be an unsigned integer"),
                )
            })?;
            if !(minimum..=maximum).contains(&parsed) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{name} must be between {minimum} and {maximum}"),
                )
                .into());
            }
            Ok(Some(parsed))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} is not Unicode"),
        )
        .into()),
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn frames_for_ms(sample_rate: u32, duration_ms: u64) -> Result<usize, io::Error> {
    let frames = u64::from(sample_rate)
        .checked_mul(duration_ms)
        .and_then(|value| value.checked_add(999))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Config duration overflows frame count",
            )
        })?
        / 1_000;
    usize::try_from(frames).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Config duration exceeds addressable PCM",
        )
    })
}

#[cfg(any(windows, target_os = "macos", test))]
fn milliseconds_for_frames(sample_rate: u32, frames: usize) -> Result<u64, io::Error> {
    if sample_rate == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sample rate must be nonzero when converting pre-record capacity",
        ));
    }
    let milliseconds = (frames as u128)
        .checked_mul(1_000)
        .and_then(|value| value.checked_add(u128::from(sample_rate) - 1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "duration overflow"))?
        / u128::from(sample_rate);
    u64::try_from(milliseconds)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "duration exceeds u64"))
}

#[cfg(windows)]
// Keep capture, worker, and cancellation handles explicit at this adapter boundary.
#[allow(clippy::too_many_arguments)]
fn service_windows_progressive(
    session: &mut ProgressiveSession,
    configuration: &shazam_http_lab::ShazamConfiguration,
    context_state: NativeContextState,
    started: Instant,
    jsonl_output: bool,
    config_generation: u64,
    timeout_ms: u64,
    sender: &mpsc::Sender<WindowsRecognitionCompletion>,
    receiver: &mpsc::Receiver<WindowsRecognitionCompletion>,
    cancelled: &Arc<AtomicBool>,
    active_workers: &mut BTreeMap<u64, JoinHandle<()>>,
    saw_match_event: &mut bool,
    saw_no_match_event: &mut bool,
    backend_events: &Option<(SessionId, RecognitionMode, EventEmitter)>,
) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(feature = "dev-cli"))]
    let _ = config_generation;
    let mut post_stop_job = None;
    let mut deferred_manual_ui_event = None;
    loop {
        let completion = match receiver.try_recv() {
            Ok(completion) => completion,
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "recognition worker channel disconnected",
                )
                .into());
            }
        };
        if let Some(worker) = active_workers.remove(&completion.attempt) {
            worker
                .join()
                .map_err(|_| io::Error::other("recognition HTTP worker panicked"))?;
        }
        if let Some(status) = completion.status {
            progressive_log_for_backend(backend_events, jsonl_output, || {
                format!(
                    "recognition attempt {} returned HTTP {status}",
                    completion.attempt
                )
            });
        }
        if let Some(message) = completion.error {
            #[cfg(feature = "dev-cli")]
            if backend_events.is_none() {
                eprintln!("recognition attempt {}: {message}", completion.attempt);
            }
            #[cfg(not(feature = "dev-cli"))]
            let _ = message;
        }
        let tracked = session.in_flight_attempts().contains(&completion.attempt);
        let was_terminal = session.is_terminal();
        if tracked {
            let recognition_error_kind = completion.error.map(|_| match &completion.outcome {
                RecognitionOutcome::TransportFailure => RecognitionErrorKind::Transport,
                RecognitionOutcome::MatcherFailure { .. } | RecognitionOutcome::InvalidResponse => {
                    RecognitionErrorKind::Internal
                }
                RecognitionOutcome::Match { .. } | RecognitionOutcome::NoMatch { .. } => {
                    RecognitionErrorKind::Internal
                }
            });
            let retry = match completion.outcome {
                RecognitionOutcome::Match { retry }
                | RecognitionOutcome::NoMatch { retry }
                | RecognitionOutcome::MatcherFailure { retry } => retry,
                RecognitionOutcome::InvalidResponse | RecognitionOutcome::TransportFailure => None,
            };
            let delivery = shazam_http_lab::apply_progressive_response_at_ns(
                session,
                completion.attempt,
                completion.outcome,
                completion.candidates.clone(),
                elapsed_ns(started),
            )
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            if !delivery.delivered_to_matcher {
                progressive_log_for_backend(backend_events, jsonl_output, || {
                    format!(
                        "recognition attempt {} response was ignored by the Native context gate (state={:?})",
                        completion.attempt,
                        delivery.status
                    )
                });
            }
            let newly_stopped_retry = session.take_post_stop_retry_job();
            let has_newly_stopped_retry = newly_stopped_retry.is_some();
            post_stop_job = newly_stopped_retry.or(post_stop_job);
            if !was_terminal && delivery.delivered_to_matcher {
                if let (Some(kind), Some(message)) = (recognition_error_kind, completion.error) {
                    emit_backend_event(
                        backend_events,
                        Some(completion.attempt),
                        RecognitionEventKind::RecognitionError {
                            kind,
                            message: message.to_owned(),
                        },
                    );
                }
                if let Some(retry) = retry {
                    emit_backend_event(
                        backend_events,
                        Some(completion.attempt),
                        RecognitionEventKind::RetryScheduled {
                            attempt_id: completion.attempt,
                            retry_duration_ms: retry.retry_duration_ms,
                            interval_ms: retry.interval_ms,
                        },
                    );
                }
                if let Some(candidate) = delivery.candidates.first() {
                    emit_backend_event(
                        backend_events,
                        Some(completion.attempt),
                        RecognitionEventKind::MatchFound {
                            attempt_id: completion.attempt,
                            title: Some(candidate.title.clone()),
                            artist: Some(candidate.artist.clone()),
                            track_id: candidate.track_id.clone(),
                        },
                    );
                }
                if let Some(event) = delivery.native_manual_ui_event() {
                    match &event {
                        shazam_http_lab::NativeManualUiEvent::Result(_) => {
                            *saw_match_event = true;
                        }
                        shazam_http_lab::NativeManualUiEvent::NoMatch => {
                            *saw_no_match_event = true;
                        }
                        shazam_http_lab::NativeManualUiEvent::Error { .. } => {}
                    }
                    if has_newly_stopped_retry
                        && matches!(event, shazam_http_lab::NativeManualUiEvent::NoMatch)
                    {
                        deferred_manual_ui_event = Some((event, completion.attempt));
                    } else {
                        emit_backend_manual_event(backend_events, completion.attempt, &event);
                        #[cfg(feature = "dev-cli")]
                        if backend_events.is_none() {
                            emit_manual_ui_event(&event, jsonl_output, config_generation);
                        }
                    }
                } else if let Some(event) = delivery.native_auto_ui_event() {
                    *saw_match_event = true;
                    emit_backend_event(
                        backend_events,
                        Some(completion.attempt),
                        RecognitionEventKind::AutoUiEvent(event.clone()),
                    );
                    #[cfg(feature = "dev-cli")]
                    if backend_events.is_none() {
                        emit_auto_ui_event(&event, jsonl_output, config_generation);
                    }
                } else if !delivery.candidates.is_empty() {
                    *saw_match_event = true;
                    progressive_log_for_backend(backend_events, jsonl_output, || {
                        "match response withheld: Native UI ordering is unverified for this response shape".to_owned()
                    });
                }
            }
        }
    }

    let now_ns = elapsed_ns(started);
    let tagging_type = session.tagging_type();
    let detached_post_stop_job = post_stop_job.is_some();
    let mut jobs = Vec::new();
    if let Some(job) = post_stop_job {
        jobs.push(job);
    } else {
        while let Some(job) = session
            .poll_at_ns(now_ns)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            jobs.push(job);
        }
    }
    for job in jobs {
        let attempt = job.attempt;
        let sample_ms = job.sample_ms;
        let signature_bytes = job.signature.len();
        emit_backend_event(
            backend_events,
            Some(attempt),
            RecognitionEventKind::SignatureReady {
                attempt_id: attempt,
                sample_ms,
                signature_bytes,
            },
        );
        let configuration = configuration.clone();
        let worker_cancelled = if detached_post_stop_job {
            Arc::new(AtomicBool::new(false))
        } else {
            Arc::clone(cancelled)
        };
        let (dispatch_ack_sender, dispatch_ack_receiver) = if detached_post_stop_job {
            let (sender, receiver) = mpsc::sync_channel(1);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let sender = sender.clone();
        let context_state_for_worker = context_state.clone();
        let backend_events_for_worker = backend_events.clone();
        let spawn_result = thread::Builder::new()
            .name(format!("openshazam-recognition-{attempt}"))
            .spawn(move || {
                let mut completion_guard = WindowsCompletionGuard {
                    sender,
                    attempt,
                    sent: false,
                };
                let request_context = context_state_for_worker.current();
                let mut transport_started = false;
                let result = if let Some(dispatch_ack_sender) = &dispatch_ack_sender {
                    shazam_http_lab::submit_progressive_signature_for_windows_tagging_type_with_transport_started_and_timestamp_ms(
                        &configuration,
                        tagging_type,
                        request_context,
                        &job.signature,
                        sample_ms,
                        job.signature_timestamp_ms,
                        Duration::from_millis(timeout_ms),
                        &worker_cancelled,
                        || {
                            transport_started = true;
                            emit_backend_event(
                                &backend_events_for_worker,
                                Some(attempt),
                                RecognitionEventKind::RequestStarted {
                                    attempt_id: attempt,
                                },
                            );
                            let _ = dispatch_ack_sender.send(true);
                        },
                    )
                } else {
                    shazam_http_lab::submit_progressive_signature_for_windows_tagging_type_with_transport_started_and_timestamp_ms(
                        &configuration,
                        tagging_type,
                        request_context,
                        &job.signature,
                        sample_ms,
                        job.signature_timestamp_ms,
                        Duration::from_millis(timeout_ms),
                        &worker_cancelled,
                        || {
                            emit_backend_event(
                                &backend_events_for_worker,
                                Some(attempt),
                                RecognitionEventKind::RequestStarted {
                                    attempt_id: attempt,
                                },
                            );
                        },
                    )
                };
                if let Some(dispatch_ack_sender) = &dispatch_ack_sender {
                    if !transport_started {
                        let _ = dispatch_ack_sender.send(false);
                    }
                }
                let completion = match result {
                    Ok(result) => match result.response {
                        shazam_http_lab::ProgressiveHttpResponse::Matched { candidates, retry } => {
                            WindowsRecognitionCompletion {
                                attempt,
                                status: Some(result.status),
                                outcome: RecognitionOutcome::Match { retry },
                                candidates,
                                error: None,
                            }
                        }
                        shazam_http_lab::ProgressiveHttpResponse::NoMatch(retry) => {
                            WindowsRecognitionCompletion {
                                attempt,
                                status: Some(result.status),
                                outcome: RecognitionOutcome::NoMatch { retry },
                                candidates: Vec::new(),
                                error: None,
                            }
                        }
                        shazam_http_lab::ProgressiveHttpResponse::HttpError { status, retry } => {
                            WindowsRecognitionCompletion {
                                attempt,
                                status: Some(status),
                                outcome: RecognitionOutcome::MatcherFailure { retry },
                                candidates: Vec::new(),
                                error: Some(if retry.is_some() {
                                    "HTTP response was unsuccessful; following its Retry directive"
                                } else {
                                    "HTTP response was unsuccessful; body withheld"
                                }),
                            }
                        }
                        shazam_http_lab::ProgressiveHttpResponse::InvalidResponse => {
                            WindowsRecognitionCompletion {
                                attempt,
                                status: Some(result.status),
                                outcome: RecognitionOutcome::InvalidResponse,
                                candidates: Vec::new(),
                                error: Some("response had no recognized track or retry metadata"),
                            }
                        }
                        shazam_http_lab::ProgressiveHttpResponse::ParseFailure { retry } => {
                            WindowsRecognitionCompletion {
                                attempt,
                                status: Some(result.status),
                                outcome: RecognitionOutcome::MatcherFailure { retry },
                                candidates: Vec::new(),
                                error: Some(if retry.is_some() {
                                    "response parsing failed; following Native retry to the configured maximum"
                                } else {
                                    "response parsing failed; no Native retry is available"
                                }),
                            }
                        }
                    },
                    Err(error) => {
                        let retry = shazam_http_lab::native_retry_for_network_failure_for_tagging_type(
                            tagging_type,
                            &error,
                            job.sample_ms,
                            configuration
                                .manual_timing()
                                .effective_maximum_signature_length_ms(),
                            worker_cancelled.load(Ordering::Acquire),
                        );
                        WindowsRecognitionCompletion {
                            attempt,
                            status: None,
                            outcome: retry.map_or(RecognitionOutcome::TransportFailure, |retry| {
                                RecognitionOutcome::MatcherFailure { retry: Some(retry) }
                            }),
                            candidates: Vec::new(),
                            error: Some(if retry.is_some() {
                                "HTTPS request failed; following Native retry to the configured maximum"
                            } else {
                                "HTTPS request failed; response details withheld"
                            }),
                        }
                    }
                };
                completion_guard.send(completion);
            });
        let worker = match spawn_result {
            Ok(worker) => worker,
            Err(error) => {
                if detached_post_stop_job {
                    if let Some((event, attempt_id)) = deferred_manual_ui_event.take() {
                        emit_backend_manual_event(backend_events, attempt_id, &event);
                        #[cfg(feature = "dev-cli")]
                        if backend_events.is_none() {
                            emit_manual_ui_event(&event, jsonl_output, config_generation);
                        }
                    }
                }
                return Err(error.into());
            }
        };
        if detached_post_stop_job {
            let dispatch_ack_receiver = dispatch_ack_receiver
                .expect("detached Retry has a dispatch acknowledgement receiver");
            notify_after_retry_dispatch_ack(
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
                    if let Some((event, attempt_id)) = deferred_manual_ui_event.take() {
                        emit_backend_manual_event(backend_events, attempt_id, &event);
                        #[cfg(feature = "dev-cli")]
                        if backend_events.is_none() {
                            emit_manual_ui_event(&event, jsonl_output, config_generation);
                        }
                    }
                },
            )?;
        }
        active_workers.insert(attempt, worker);
        if detached_post_stop_job {
            session.finish_post_stop_retry_submission();
        }
    }

    if session.is_terminal() && session.in_flight_attempts().is_empty() && active_workers.is_empty()
    {
        cancelled.store(true, Ordering::Release);
    }
    Ok(())
}

#[cfg(windows)]
struct WindowsRecognitionCompletion {
    attempt: u64,
    status: Option<u16>,
    outcome: RecognitionOutcome,
    candidates: Vec<shazam_http_lab::TrackCandidate>,
    error: Option<&'static str>,
}

#[cfg(windows)]
struct WindowsCompletionGuard {
    sender: mpsc::Sender<WindowsRecognitionCompletion>,
    attempt: u64,
    sent: bool,
}

#[cfg(windows)]
impl WindowsCompletionGuard {
    fn send(&mut self, completion: WindowsRecognitionCompletion) {
        self.sent = true;
        let _ = self.sender.send(completion);
    }
}

#[cfg(windows)]
impl Drop for WindowsCompletionGuard {
    fn drop(&mut self) {
        if !self.sent {
            let _ = self.sender.send(WindowsRecognitionCompletion {
                attempt: self.attempt,
                status: None,
                outcome: RecognitionOutcome::InvalidResponse,
                candidates: Vec::new(),
                error: Some("recognition HTTP worker panicked"),
            });
        }
    }
}

#[cfg(target_os = "macos")]
fn downmix_interleaved_f32(samples: &[f32], channels: usize) -> Result<Vec<f32>, io::Error> {
    let mut mono = Vec::new();
    downmix_interleaved_f32_into(samples, channels, &mut mono)?;
    Ok(mono)
}

#[cfg(any(windows, target_os = "macos"))]
fn downmix_interleaved_f32_into(
    samples: &[f32],
    channels: usize,
    output: &mut Vec<f32>,
) -> Result<(), io::Error> {
    if channels == 0 || samples.len() % channels != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "capture packet is not a whole number of interleaved frames",
        ));
    }
    output.clear();
    output
        .try_reserve(samples.len() / channels)
        .map_err(io::Error::other)?;
    for frame in samples.chunks_exact(channels) {
        if frame.iter().any(|sample| !sample.is_finite()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "capture packet contains a non-finite sample",
            ));
        }
        output.push(frame.iter().copied().sum::<f32>() / channels as f32);
    }
    Ok(())
}

#[cfg(windows)]
fn canonical_loopback_format() -> wasapi::WaveFormat {
    wasapi::WaveFormat::new(32, 32, &wasapi::SampleType::Float, 48_000, 1, None)
}

#[cfg(windows)]
fn format_usable_by_sigx(format: &wasapi::WaveFormat) -> bool {
    let sample_rate = format.get_samplespersec();
    let channels = usize::from(format.get_nchannels());
    let bits = format.get_bitspersample();
    let valid_bits = format.get_validbitspersample();
    let block_align = format.get_blockalign() as usize;
    let Some(sample_bytes) = usize::from(bits).checked_add(7).map(|bits| bits / 8) else {
        return false;
    };
    let sample_type = match format.get_subformat() {
        Ok(sample_type) => sample_type,
        Err(_) => return false,
    };
    let valid_bits_supported = valid_bits == bits || (bits == 32 && valid_bits == 24);
    sample_rate_supported(sample_rate)
        && (1..=32).contains(&channels)
        && sample_bytes > 0
        && block_align == sample_bytes.saturating_mul(channels)
        && valid_bits_supported
        && match sample_type {
            wasapi::SampleType::Float => matches!(bits, 32 | 64) && valid_bits == bits,
            wasapi::SampleType::Int => matches!(bits, 8 | 16 | 24 | 32),
        }
}

#[cfg(windows)]
fn sample_rate_supported(sample_rate: u32) -> bool {
    matches!(
        sample_rate,
        8_000 | 16_000 | 22_050 | 32_000 | 44_100 | 48_000 | 96_000
    )
}

#[cfg(all(windows, test))]
fn decode_wasapi_interleaved(
    bytes: &[u8],
    frames: usize,
    format: &wasapi::WaveFormat,
) -> io::Result<Vec<f32>> {
    let mut output = Vec::new();
    decode_wasapi_interleaved_into(bytes, frames, format, &mut output)?;
    Ok(output)
}

#[cfg(windows)]
fn decode_wasapi_interleaved_into(
    bytes: &[u8],
    frames: usize,
    format: &wasapi::WaveFormat,
    output: &mut Vec<f32>,
) -> io::Result<()> {
    use wasapi::SampleType;

    if !format_usable_by_sigx(format) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "WASAPI returned an unsupported PCM format or SigX sample rate",
        ));
    }
    let channels = usize::from(format.get_nchannels());
    let bits = format.get_bitspersample();
    let sample_bytes = (usize::from(bits) + 7) / 8;
    let sample_count = frames
        .checked_mul(channels)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "capture packet too large"))?;
    let byte_count = sample_count
        .checked_mul(sample_bytes)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "capture packet too large"))?;
    let data = bytes.get(..byte_count).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "WASAPI packet is shorter than the reported frame count",
        )
    })?;
    let sample_type = format.get_subformat().map_err(|error| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("unsupported WASAPI sample type: {error:?}"),
        )
    })?;
    output.clear();
    output.try_reserve(sample_count).map_err(io::Error::other)?;
    for chunk in data.chunks_exact(sample_bytes) {
        let sample = match (sample_type, bits) {
            (SampleType::Float, 32) => f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
            (SampleType::Float, 64) => {
                f64::from_le_bytes(chunk.try_into().expect("validated 64-bit float sample")) as f32
            }
            (SampleType::Int, 8) => (i16::from(chunk[0]) - 128) as f32 / 128.0,
            (SampleType::Int, 16) => i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 32_768.0,
            (SampleType::Int, 24) => {
                let packed =
                    i32::from(chunk[0]) | (i32::from(chunk[1]) << 8) | (i32::from(chunk[2]) << 16);
                let signed = if packed & 0x80_0000 != 0 {
                    packed | !0xFF_FFFF
                } else {
                    packed
                };
                signed as f32 / 8_388_608.0
            }
            (SampleType::Int, 32) => {
                i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f32
                    / 2_147_483_648.0
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "unsupported WASAPI PCM container width",
                ));
            }
        };
        if sample.is_finite() {
            output.push(sample.clamp(-1.0, 1.0));
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WASAPI packet contains a non-finite sample",
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn capture_loopback_frames(
    capture: &wasapi::AudioCaptureClient,
    event: &wasapi::Handle,
    format: &wasapi::WaveFormat,
    channels: usize,
    accumulator: &mut CaptureAccumulator,
    packet_scratch: &mut WasapiPacketScratch,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut empty_wakeups = 0usize;
    let mut received_audio_packet = false;

    while accumulator.remaining_frames() > 0 {
        event.wait_for_event(2_000)?;
        loop {
            let Some(packet_frames) = capture.get_next_packet_size()? else {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "WASAPI capture client did not report shared-mode packet sizes",
                )
                .into());
            };
            if packet_frames == 0 {
                empty_wakeups += 1;
                if empty_wakeups > 10 {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "WASAPI signaled repeatedly without supplying audio frames",
                    )
                    .into());
                }
                break;
            }
            empty_wakeups = 0;

            let byte_count = (packet_frames as usize)
                .checked_mul(format.get_blockalign() as usize)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "packet too large"))?;
            packet_scratch.bytes.resize(byte_count, 0);
            let (read_frames, info) = capture.read_from_device(&mut packet_scratch.bytes)?;
            if info.flags.data_discontinuity && received_audio_packet {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WASAPI reported a playback-capture discontinuity; retry after playback is stable",
                )
                .into());
            }
            if read_frames == 0 {
                break;
            }
            received_audio_packet = true;

            let frame_count = (read_frames as usize).min(accumulator.remaining_frames());
            if info.flags.silent {
                let sample_count = frame_count.checked_mul(channels).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "capture packet too large")
                })?;
                fill_silent_samples(&mut packet_scratch.interleaved, sample_count);
                accumulator.push_interleaved_f32(&packet_scratch.interleaved, channels)?;
                continue;
            }
            let byte_count = frame_count
                .checked_mul(format.get_blockalign() as usize)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "packet too large"))?;
            let packet_bytes = packet_scratch.bytes.get(..byte_count).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WASAPI returned fewer bytes than reported frames",
                )
            })?;
            decode_wasapi_interleaved_into(
                packet_bytes,
                frame_count,
                format,
                &mut packet_scratch.interleaved,
            )?;
            accumulator.push_interleaved_f32(&packet_scratch.interleaved, channels)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(any(windows, target_os = "macos"))]
    use super::notify_after_retry_dispatch_ack;
    #[cfg(any(windows, target_os = "macos"))]
    use super::NativeContextState;
    use super::{
        classify_manual_trigger_poll, decode_mono_f32le, fill_silent_samples,
        ignore_expected_timeout, milliseconds_for_frames, qpc_timestamp_to_unix_ms,
        CaptureAccumulator, ManualTriggerPoll, ManualTriggerSignal, CAPTURE_RATE,
    };
    #[cfg(any(windows, target_os = "macos"))]
    use shazam_http_lab::NativeTaggingRequestContext;
    use sigx_core::{Config, SigX};
    use sigx_format::Signature;
    #[cfg(any(windows, target_os = "macos"))]
    use std::cell::RefCell;

    #[test]
    fn wasapi_event_timeout_is_a_wakeup_not_a_capture_failure() {
        #[derive(Debug, PartialEq, Eq)]
        enum WaitError {
            Timeout,
            DeviceFailure,
        }

        assert_eq!(
            ignore_expected_timeout::<WaitError>(Ok(()), |_| false),
            Ok(())
        );
        assert_eq!(
            ignore_expected_timeout(Err(WaitError::Timeout), |error| {
                *error == WaitError::Timeout
            }),
            Ok(())
        );
        assert_eq!(
            ignore_expected_timeout(Err(WaitError::DeviceFailure), |error| {
                *error == WaitError::Timeout
            }),
            Err(WaitError::DeviceFailure)
        );
    }

    #[test]
    fn manual_trigger_poll_handles_pending_eof_errors_and_cancellation() {
        let (sender, receiver) = std::sync::mpsc::channel();
        assert_eq!(
            classify_manual_trigger_poll(false, receiver.try_recv()),
            ManualTriggerPoll::Pending
        );

        sender.send(ManualTriggerSignal::Enter).unwrap();
        assert_eq!(
            classify_manual_trigger_poll(false, receiver.try_recv()),
            ManualTriggerPoll::Started
        );
        sender.send(ManualTriggerSignal::EndOfInput).unwrap();
        assert_eq!(
            classify_manual_trigger_poll(false, receiver.try_recv()),
            ManualTriggerPoll::Ended
        );
        sender
            .send(ManualTriggerSignal::InputError("read failed".into()))
            .unwrap();
        assert_eq!(
            classify_manual_trigger_poll(false, receiver.try_recv()),
            ManualTriggerPoll::InputError("read failed".into())
        );
        sender.send(ManualTriggerSignal::Enter).unwrap();
        assert_eq!(
            classify_manual_trigger_poll(true, receiver.try_recv()),
            ManualTriggerPoll::Cancelled
        );
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn native_context_state_changes_are_observed_by_later_request_workers() {
        let state = NativeContextState::new(Some(NativeTaggingRequestContext::Foreground));
        assert_eq!(
            state.current(),
            Some(NativeTaggingRequestContext::Foreground)
        );
        state.set(NativeTaggingRequestContext::Background);
        assert_eq!(
            state.current(),
            Some(NativeTaggingRequestContext::Background)
        );
    }

    #[test]
    fn page_rounded_prerecord_duration_is_reported_without_rounding_down() {
        assert_eq!(milliseconds_for_frames(44_100, 135_168).unwrap(), 3_066);
        assert_eq!(milliseconds_for_frames(48_000, 147_456).unwrap(), 3_072);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn dispatch_ack_helper_runs_callback_after_acknowledgement() {
        let order = RefCell::new(Vec::new());
        let result = notify_after_retry_dispatch_ack(
            || {
                order.borrow_mut().push("retry transport started");
                Ok::<(), ()>(())
            },
            || order.borrow_mut().push("callback"),
        );
        assert_eq!(result, Ok(()));
        assert_eq!(*order.borrow(), ["retry transport started", "callback"]);

        order.borrow_mut().clear();
        let result = notify_after_retry_dispatch_ack(
            || {
                order.borrow_mut().push("retry transport startup failed");
                Err("startup error")
            },
            || order.borrow_mut().push("callback"),
        );
        assert_eq!(result, Err("startup error"));
        assert_eq!(
            *order.borrow(),
            ["retry transport startup failed", "callback"]
        );
    }

    #[test]
    fn maps_wasapi_qpc_packet_timestamp_to_unix_time() {
        assert_eq!(
            qpc_timestamp_to_unix_ms(1_000_000, 1_025_000, 50_000),
            Some(49_998)
        );
        assert_eq!(qpc_timestamp_to_unix_ms(2_000, 1_000, 50_000), None);
        assert_eq!(qpc_timestamp_to_unix_ms(0, 20_000, 1), None);
    }

    #[test]
    fn decodes_mono_f32_little_endian_samples() {
        let samples = [0.25f32, -0.5, 1.0];
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        assert_eq!(decode_mono_f32le(&bytes).unwrap(), samples);
    }

    #[test]
    fn silent_wasapi_packets_clear_reused_buffers_at_same_smaller_and_larger_lengths() {
        let mut mono = vec![0.75, -0.5, 0.25, -1.0];
        let mono_ptr = mono.as_ptr();
        let mono_capacity = mono.capacity();
        fill_silent_samples(&mut mono, 4);
        assert_eq!(mono, [0.0; 4]);

        mono.fill(0.5);
        fill_silent_samples(&mut mono, 2);
        assert_eq!(mono, [0.0; 2]);

        mono.fill(-0.25);
        fill_silent_samples(&mut mono, 4);
        assert_eq!(mono, [0.0; 4]);
        assert_eq!(mono.as_ptr(), mono_ptr);
        assert_eq!(mono.capacity(), mono_capacity);

        let mut interleaved = vec![0.1, -0.2, 0.3, -0.4, 0.5, -0.6];
        fill_silent_samples(&mut interleaved, 6);
        assert_eq!(interleaved, [0.0; 6]);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn reused_downmix_buffer_preserves_sample_results() {
        let interleaved = [0.25, 0.5, -0.25, 0.75, 1.0, -1.0];
        let mut reused = vec![9.0; 16];
        super::downmix_interleaved_f32_into(&interleaved, 2, &mut reused).unwrap();
        assert_eq!(reused, [0.375, 0.25, 0.0]);

        super::downmix_interleaved_f32_into(&[0.5, -0.5], 2, &mut reused).unwrap();
        assert_eq!(reused, [0.0]);
    }

    #[cfg(windows)]
    #[test]
    fn decodes_stereo_int16_wasapi_mix_format() {
        let format = wasapi::WaveFormat::new(16, 16, &wasapi::SampleType::Int, 48_000, 2, None);
        let bytes = [0x00, 0x00, 0x00, 0x80, 0xff, 0x7f, 0x00, 0x00];
        let samples = super::decode_wasapi_interleaved(&bytes, 2, &format).unwrap();
        assert_eq!(samples, [0.0, -1.0, 32_767.0 / 32_768.0, 0.0]);

        let mut accumulator = CaptureAccumulator::new_for_rate(48_000, 2).unwrap();
        accumulator.push_interleaved_f32(&samples, 2).unwrap();
        assert_eq!(accumulator.frames_seen(), 2);
        Signature::parse(&accumulator.finish().unwrap()).expect("mixed down audio creates SigX");
    }

    #[cfg(windows)]
    #[test]
    fn decodes_packed_24_bit_pcm_without_losing_sign() {
        let format = wasapi::WaveFormat::new(24, 24, &wasapi::SampleType::Int, 44_100, 1, None);
        let bytes = [0x00, 0x00, 0x80, 0xff, 0xff, 0x7f];
        let samples = super::decode_wasapi_interleaved(&bytes, 2, &format).unwrap();
        assert_eq!(samples, [-1.0, 8_388_607.0 / 8_388_608.0]);
    }

    #[test]
    fn rejects_partial_float_sample() {
        assert!(decode_mono_f32le(&[0, 0, 0]).is_err());
    }

    #[test]
    fn rejects_non_finite_capture_sample() {
        assert!(decode_mono_f32le(&f32::NAN.to_le_bytes()).is_err());
    }

    fn encode_f32le(samples: &[f32]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect()
    }

    fn fnv1a64(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    fn push_range_in_chunks(
        accumulator: &mut CaptureAccumulator,
        samples: &[f32],
        chunk_sizes: &[usize],
        mark_zero_packets_silent: bool,
    ) {
        let mut offset = 0;
        let mut chunk_index = 0;
        while offset < samples.len() {
            let count = chunk_sizes[chunk_index % chunk_sizes.len()].min(samples.len() - offset);
            let packet = &samples[offset..offset + count];
            let silent = mark_zero_packets_silent && packet.iter().all(|sample| *sample == 0.0);
            let bytes = if silent {
                Vec::new()
            } else {
                encode_f32le(packet)
            };
            accumulator
                .push_packet(&bytes, count, silent, false)
                .expect("synthetic packet is valid");
            offset += count;
            chunk_index += 1;
        }
    }

    #[test]
    fn packetized_capture_matches_single_packet_and_is_repeatable() {
        const FRAMES: usize = 48_000;
        const SILENCE_START: usize = 16_384;
        const SILENCE_END: usize = 20_480;
        let samples: Vec<f32> = (0..FRAMES)
            .map(|index| {
                if (SILENCE_START..SILENCE_END).contains(&index) {
                    0.0
                } else {
                    ((index * 37 % 2_001) as i32 - 1_000) as f32 / 2_000.0
                }
            })
            .collect();

        let mut single_packet = CaptureAccumulator::new(FRAMES).unwrap();
        single_packet
            .push_packet(&encode_f32le(&samples), FRAMES, false, false)
            .unwrap();
        assert_eq!(single_packet.frames_seen(), FRAMES);
        let single_signature = single_packet.finish().unwrap();

        let mut direct_sigx = SigX::new(Config {
            sample_rate: CAPTURE_RATE,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: FRAMES,
        })
        .unwrap();
        direct_sigx.push_f32(&samples).unwrap();
        assert_eq!(direct_sigx.signature().unwrap(), single_signature);

        let mut packetized = CaptureAccumulator::new(FRAMES).unwrap();
        push_range_in_chunks(
            &mut packetized,
            &samples[..SILENCE_START],
            &[257, 1_021, 113, 2_048, 701],
            true,
        );
        push_range_in_chunks(
            &mut packetized,
            &samples[SILENCE_START..SILENCE_END],
            &[512, 1_024, 2_560],
            true,
        );
        push_range_in_chunks(
            &mut packetized,
            &samples[SILENCE_END..],
            &[997, 128, 4_097, 333, 2_111],
            true,
        );
        assert_eq!(packetized.frames_seen(), FRAMES);
        let packetized_signature = packetized.finish().unwrap();
        assert_eq!(packetized_signature, single_signature);
        assert_eq!(packetized.finish().unwrap(), packetized_signature);
        assert_eq!(packetized_signature.len(), 412);
        assert_eq!(fnv1a64(&packetized_signature), 0xc256_3225_4054_8eae);
        Signature::parse(&packetized_signature).expect("generated Signature parses");
    }

    #[test]
    fn capture_rejects_discontinuities_and_incomplete_streams() {
        let mut accumulator = CaptureAccumulator::new(16_000).unwrap();
        assert!(accumulator.push_packet(&[], 16, true, true).is_err());
        assert!(accumulator.finish().is_err());
    }

    #[test]
    fn interleaved_stereo_downmix_matches_mono_input() {
        const FRAMES: usize = 48_000;
        let mono: Vec<f32> = (0..FRAMES)
            .map(|index| ((index * 37 % 2_001) as i32 - 1_000) as f32 / 2_048.0)
            .collect();
        let stereo: Vec<f32> = mono
            .iter()
            .flat_map(|sample| [sample + 0.125, sample - 0.125])
            .collect();

        let mut interleaved = CaptureAccumulator::new_for_rate(16_000, FRAMES).unwrap();
        interleaved.push_interleaved_f32(&stereo, 2).unwrap();

        let mut direct = CaptureAccumulator::new_for_rate(16_000, FRAMES).unwrap();
        direct.push_interleaved_f32(&mono, 1).unwrap();

        assert_eq!(interleaved.finish().unwrap(), direct.finish().unwrap());
    }

    #[test]
    fn interleaved_capture_rejects_invalid_channel_layout_and_samples() {
        let mut accumulator = CaptureAccumulator::new_for_rate(48_000, 4).unwrap();
        assert!(accumulator.push_interleaved_f32(&[0.0], 0).is_err());
        assert!(accumulator
            .push_interleaved_f32(&[0.0, 1.0, 2.0], 2)
            .is_err());
        assert!(accumulator
            .push_interleaved_f32(&[0.0, f32::INFINITY], 2)
            .is_err());
        assert_eq!(accumulator.frames_seen(), 0);
    }

    #[test]
    fn high_rate_interleaved_capture_reaches_parseable_signature() {
        const RATE: u32 = 44_100;
        const FRAMES: usize = RATE as usize;
        let samples: Vec<f32> = (0..FRAMES)
            .map(|index| {
                let phase = std::f32::consts::TAU * 440.0 * index as f32 / RATE as f32;
                phase.sin() * 0.5
            })
            .collect();

        let mut accumulator = CaptureAccumulator::new_for_rate(RATE, FRAMES).unwrap();
        accumulator.push_interleaved_f32(&samples, 1).unwrap();
        assert_eq!(accumulator.frames_seen(), FRAMES);
        Signature::parse(&accumulator.finish().unwrap())
            .expect("high-rate local signature should parse");
    }
}
