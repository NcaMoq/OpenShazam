#![allow(unknown_lints, clippy::manual_is_multiple_of)]

use std::{env, io, path::Path, process::ExitCode};

#[cfg(any(windows, test, target_os = "macos"))]
use sigx_core::{Config, SigX};

#[cfg(windows)]
use std::{fs::OpenOptions, io::Write};

#[cfg(any(windows, test))]
const CAPTURE_RATE: u32 = 16_000;
#[cfg(windows)]
const CAPTURE_SECONDS: usize = 12;
#[cfg(windows)]
const CAPTURE_FRAMES: usize = CAPTURE_RATE as usize * CAPTURE_SECONDS;
#[cfg(target_os = "macos")]
mod macos_capture;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 2 || args[0] != "capture-signature" {
        eprintln!("usage: openshazam capture-signature <new-output.sig>");
        return ExitCode::from(2);
    }

    let output = Path::new(&args[1]);
    if output.exists() {
        eprintln!(
            "refusing to overwrite existing output: {}",
            output.display()
        );
        return ExitCode::from(2);
    }

    #[cfg(windows)]
    let result = capture_signature(output);

    #[cfg(target_os = "macos")]
    let result = macos_capture::capture_signature(output);

    #[cfg(not(any(windows, target_os = "macos")))]
    let result: Result<(), Box<dyn std::error::Error>> = Err(Box::new(io::Error::new(
        io::ErrorKind::Unsupported,
        "microphone capture is available only on Windows and macOS",
    )));

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("capture failed: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(any(windows, test))]
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
    #[cfg(any(windows, test))]
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

    #[cfg(any(windows, test))]
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

    #[cfg(any(test, target_os = "macos"))]
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
fn capture_signature(output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use wasapi::{
        initialize_mta, AudioCaptureClient, DeviceEnumerator, Direction, SampleType, StreamMode,
        WaveFormat,
    };

    initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    let device = enumerator.get_default_device(&Direction::Capture)?;
    let mut audio_client = device.get_iaudioclient()?;

    // Request the canonical input shape; shared-mode WASAPI may convert the
    // endpoint mix format to this stream format.
    let format = WaveFormat::new(32, 32, &SampleType::Float, CAPTURE_RATE as usize, 1, None);
    let (_, minimum_period_hns) = audio_client.get_device_period()?;
    audio_client.initialize_client(
        &format,
        &Direction::Capture,
        &StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: minimum_period_hns.max(200_000),
        },
    )?;
    let event = audio_client.set_get_eventhandle()?;
    let capture: AudioCaptureClient = audio_client.get_audiocaptureclient()?;
    let mut accumulator = CaptureAccumulator::new(CAPTURE_FRAMES)?;

    println!("Capturing {CAPTURE_SECONDS}s from the default microphone as 16-kHz mono Float32...");
    audio_client.start_stream()?;
    let capture_result = capture_frames(&capture, &event, &mut accumulator);
    let stop_result = audio_client.stop_stream();
    capture_result?;
    stop_result?;

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

#[cfg(windows)]
fn capture_frames(
    capture: &wasapi::AudioCaptureClient,
    event: &wasapi::Handle,
    accumulator: &mut CaptureAccumulator,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut empty_wakeups = 0usize;

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
                .checked_mul(4)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "packet too large"))?;
            let mut bytes = vec![0u8; byte_count];
            let (read_frames, info) = capture.read_from_device(&mut bytes)?;
            if read_frames == 0 {
                accumulator.push_packet(
                    &[],
                    0,
                    info.flags.silent,
                    info.flags.data_discontinuity,
                )?;
                break;
            }

            let sample_count = read_frames as usize;
            let byte_count = sample_count
                .checked_mul(4)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "packet too large"))?;
            let packet_bytes = bytes.get(..byte_count).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WASAPI returned fewer bytes than reported frames",
                )
            })?;
            accumulator.push_packet(
                packet_bytes,
                sample_count,
                info.flags.silent,
                info.flags.data_discontinuity,
            )?;
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
