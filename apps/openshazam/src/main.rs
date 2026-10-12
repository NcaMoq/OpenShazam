#![allow(unknown_lints, clippy::manual_is_multiple_of)]

use std::{env, io, path::Path, process::ExitCode};

#[cfg(any(windows, test, target_os = "macos"))]
use sigx_core::{Config, SigX};

#[cfg(windows)]
use std::{fs::OpenOptions, io::Write};

#[cfg(test)]
const CAPTURE_RATE: u32 = 16_000;
#[cfg(windows)]
const CAPTURE_SECONDS: usize = 12;
#[cfg(target_os = "macos")]
mod macos_capture;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    #[cfg(windows)]
    let result = run_windows_command(&args);

    #[cfg(target_os = "macos")]
    let result = run_macos_command(&args);

    #[cfg(not(any(windows, target_os = "macos")))]
    let result: Result<(), Box<dyn std::error::Error>> = Err(Box::new(io::Error::new(
        io::ErrorKind::Unsupported,
        "audio capture is available only on Windows and macOS",
    )));

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("OpenShazam failed: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
fn run_windows_command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(String::as_str) {
        Some("list-devices") if args.len() == 1 => list_render_devices(),
        Some("capture-signature-loopback") if (2..=3).contains(&args.len()) => {
            let output = Path::new(&args[1]);
            refuse_overwrite(output)?;
            let signature = capture_loopback_signature(args.get(2).map(String::as_str))?;
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
        Some("recognize-loopback") if args.len() <= 2 => {
            let signature = capture_loopback_signature(args.get(1).map(String::as_str))?;
            let (status, candidates) = shazam_http_lab::recognize_signature_for_windows(
                &signature,
                (CAPTURE_SECONDS * 1_000) as u64,
            )?;
            println!("Shazam HTTP status={status}");
            for (index, candidate) in candidates.iter().enumerate() {
                println!(
                    "match[{}] title={:?} artist={:?}",
                    index + 1,
                    candidate.title,
                    candidate.artist
                );
            }
            Ok(())
        }
        _ => {
            print_windows_usage();
            Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid command or arguments").into())
        }
    }
}

#[cfg(windows)]
fn refuse_overwrite(output: &Path) -> io::Result<()> {
    if output.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "refusing to overwrite existing output: {}",
                output.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn print_windows_usage() {
    eprintln!("usage: openshazam list-devices");
    eprintln!("   or: openshazam recognize-loopback [device-id]");
    eprintln!("   or: openshazam capture-signature-loopback <new-output.sig> [device-id]");
}

#[cfg(target_os = "macos")]
fn run_macos_command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() != 2 || args[0] != "capture-signature" {
        eprintln!("usage: openshazam capture-signature <new-output.sig>");
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "invalid command or arguments").into(),
        );
    }
    macos_capture::capture_signature(Path::new(&args[1]))
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

#[cfg(windows)]
fn list_render_devices() -> Result<(), Box<dyn std::error::Error>> {
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

#[cfg(windows)]
fn capture_loopback_signature(
    selected_device_id: Option<&str>,
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
        .checked_mul(CAPTURE_SECONDS)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "capture length overflow"))?;
    let mut accumulator = CaptureAccumulator::new_for_rate(sample_rate, frames)?;
    let event = audio_client.set_get_eventhandle()?;
    let capture: AudioCaptureClient = audio_client.get_audiocaptureclient()?;

    println!(
        "Capturing {CAPTURE_SECONDS}s of system playback from {device_name:?} at {sample_rate} Hz, {channels} channel(s)..."
    );
    audio_client.start_stream()?;
    let capture_result =
        capture_loopback_frames(&capture, &event, &format, channels, &mut accumulator);
    let stop_result = audio_client.stop_stream();
    capture_result?;
    stop_result?;

    let signature = accumulator.finish()?;
    println!(
        "WASAPI loopback capture complete: 12s PCM -> Pure Rust SigX ({} bytes).",
        signature.len()
    );
    Ok(signature)
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

#[cfg(windows)]
fn decode_wasapi_interleaved(
    bytes: &[u8],
    frames: usize,
    format: &wasapi::WaveFormat,
) -> io::Result<Vec<f32>> {
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
    data.chunks_exact(sample_bytes)
        .map(|chunk| {
            let sample = match (sample_type, bits) {
                (SampleType::Float, 32) => {
                    f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                }
                (SampleType::Float, 64) => {
                    f64::from_le_bytes(chunk.try_into().expect("validated 64-bit float sample"))
                        as f32
                }
                (SampleType::Int, 8) => (i16::from(chunk[0]) - 128) as f32 / 128.0,
                (SampleType::Int, 16) => i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 32_768.0,
                (SampleType::Int, 24) => {
                    let packed = i32::from(chunk[0])
                        | (i32::from(chunk[1]) << 8)
                        | (i32::from(chunk[2]) << 16);
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
                Ok(sample.clamp(-1.0, 1.0))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WASAPI packet contains a non-finite sample",
                ))
            }
        })
        .collect()
}

#[cfg(windows)]
fn capture_loopback_frames(
    capture: &wasapi::AudioCaptureClient,
    event: &wasapi::Handle,
    format: &wasapi::WaveFormat,
    channels: usize,
    accumulator: &mut CaptureAccumulator,
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
            let mut bytes = vec![0u8; byte_count];
            let (read_frames, info) = capture.read_from_device(&mut bytes)?;
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
                accumulator.push_interleaved_f32(&vec![0.0; frame_count * channels], channels)?;
                continue;
            }
            let byte_count = frame_count
                .checked_mul(format.get_blockalign() as usize)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "packet too large"))?;
            let packet_bytes = bytes.get(..byte_count).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WASAPI returned fewer bytes than reported frames",
                )
            })?;
            let samples = decode_wasapi_interleaved(packet_bytes, frame_count, format)?;
            accumulator.push_interleaved_f32(&samples, channels)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{decode_mono_f32le, CaptureAccumulator, CAPTURE_RATE};
    use sigx_core::{Config, SigX};
    use sigx_format::Signature;

    #[test]
    fn decodes_mono_f32_little_endian_samples() {
        let samples = [0.25f32, -0.5, 1.0];
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        assert_eq!(decode_mono_f32le(&bytes).unwrap(), samples);
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
