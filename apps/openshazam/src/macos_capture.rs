use super::CaptureAccumulator;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Data, FromSample, Sample, SampleFormat, SizedSample,
};
use std::{
    fs::OpenOptions,
    io::{self, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{sync_channel, TrySendError},
        Arc, Mutex,
    },
    time::Duration,
};

const CAPTURE_SECONDS: usize = 12;
const PACKET_QUEUE_CAPACITY: usize = 8;
const PACKET_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) fn capture_signature(output: &Path) -> Result<(), Box<dyn std::error::Error>> {
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
        .checked_mul(CAPTURE_SECONDS)
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
        "Capturing {CAPTURE_SECONDS}s from the default CoreAudio input ({sample_rate} Hz, {channels} channel(s), {sample_format}); microphone data stays local."
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
