//! Streaming resample → FFT → max-field/peak reference pipeline.

use std::collections::VecDeque;

use super::{
    fft_frame, fft_frame_full_complex_f32, fft_frame_full_complex_f32_direct_twiddle,
    fft_frame_full_complex_f32_fma, fft_frame_full_complex_f32_fma_butterfly,
    fft_frame_full_complex_f32_fma_twiddle, fft_frame_full_complex_f64, FftError, FftKernel,
    PeakFrameSlice, PeakMaxFieldRing, PeakQuantizationError, SpectralPeak, FFT_BINS, FFT_SIZE,
    FFT_STEP, PEAK_DELAY_FRAMES,
};
use crate::resampler::{Resampler, ResamplerError, INTERNAL_SAMPLE_RATE};

/// `sig_extractor_pipe::flush_fft` runs 61 zero blocks before rewinding the
/// FFT and SigX state. This gives the temporal peak search its native tail
/// lookahead; the captured peaks are retained by the caller.
pub const SIGNATURE_FLUSH_FRAMES: usize = 61;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamError {
    Resampler(ResamplerError),
    Fft(FftError),
    Peak(PeakQuantizationError),
    NonFiniteFloat,
}

impl From<ResamplerError> for StreamError {
    fn from(error: ResamplerError) -> Self {
        Self::Resampler(error)
    }
}

impl From<FftError> for StreamError {
    fn from(error: FftError) -> Self {
        Self::Fft(error)
    }
}

impl From<PeakQuantizationError> for StreamError {
    fn from(error: PeakQuantizationError) -> Self {
        Self::Peak(error)
    }
}

/// A stateful reference pipeline matching the native block cadence:
/// resampled samples arrive in 128-sample blocks, each block advances a
/// 2048-sample ring, and the peak stream begins after its 45-frame delay.
#[derive(Debug, Clone)]
pub struct StreamingDsp {
    fft_kernel: FftKernel,
    resampler: Resampler,
    pending: VecDeque<f32>,
    fft_ring: [f32; FFT_SIZE],
    fft_write_index: usize,
    magnitudes: Vec<f32>,
    peak_ring: PeakMaxFieldRing,
    peaks: Vec<SpectralPeak>,
}

impl StreamingDsp {
    pub fn new(input_rate: u32) -> Result<Self, ResamplerError> {
        Self::new_with_fft_kernel(input_rate, FftKernel::PortablePackedRealF32)
    }

    /// Construct the streaming pipeline with an explicitly selected Rust
    /// FFT kernel. The default constructor retains the packed-real f32 path.
    pub fn new_with_fft_kernel(
        input_rate: u32,
        fft_kernel: FftKernel,
    ) -> Result<Self, ResamplerError> {
        Ok(Self {
            fft_kernel,
            resampler: Resampler::new(input_rate, INTERNAL_SAMPLE_RATE)?,
            pending: VecDeque::new(),
            fft_ring: [0.0; FFT_SIZE],
            fft_write_index: 0,
            magnitudes: vec![0.0; 256 * FFT_BINS],
            peak_ring: PeakMaxFieldRing::new(),
            peaks: Vec::new(),
        })
    }

    pub fn input_rate(&self) -> u32 {
        self.resampler.input_rate()
    }

    pub fn frames_processed(&self) -> u64 {
        self.peak_ring.current_frame()
    }

    pub fn pending_resampled_samples(&self) -> usize {
        self.pending.len()
    }

    pub fn push_f32(&mut self, input: &[f32]) -> Result<(), StreamError> {
        if input.iter().any(|sample| !sample.is_finite()) {
            return Err(StreamError::NonFiniteFloat);
        }
        self.pending.extend(self.resampler.push_f32(input));
        self.process_pending()
    }

    pub fn push_i16(&mut self, input: &[i16]) -> Result<(), StreamError> {
        self.pending.extend(self.resampler.push_i16(input));
        self.process_pending()
    }

    pub fn drain_peaks(&mut self) -> Vec<SpectralPeak> {
        std::mem::take(&mut self.peaks)
    }

    pub fn peaks(&self) -> &[SpectralPeak] {
        &self.peaks
    }

    pub fn flush_signature(&mut self) -> Result<(), StreamError> {
        self.flush_signature_with_frames(SIGNATURE_FLUSH_FRAMES)
    }

    pub fn flush_signature_with_frames(&mut self, frames: usize) -> Result<(), StreamError> {
        for _ in 0..frames {
            self.pending.extend(vec![0.0; FFT_STEP]);
            self.process_pending()?;
        }
        Ok(())
    }

    /// Diagnostic finalizer for testing whether an incomplete FFT hop is
    /// discarded before native-style zero-tail lookahead. This does not alter
    /// the default finalizer and does not flush the input-rate resampler.
    pub fn flush_signature_with_frames_discarding_partial_fft_hop(
        &mut self,
        frames: usize,
    ) -> Result<(), StreamError> {
        self.pending.clear();
        self.flush_signature_with_frames(frames)
    }

    pub fn reset(&mut self) {
        self.resampler.reset();
        self.pending.clear();
        self.fft_ring = [0.0; FFT_SIZE];
        self.fft_write_index = 0;
        self.magnitudes.fill(0.0);
        self.peak_ring = PeakMaxFieldRing::new();
        self.peaks.clear();
    }

    fn magnitude_frame(&self, frame: u64) -> &[f32] {
        let start = ((frame as usize) & 255) * FFT_BINS;
        &self.magnitudes[start..start + FFT_BINS]
    }

    fn process_pending(&mut self) -> Result<(), StreamError> {
        while self.pending.len() >= FFT_STEP {
            let mut block = [0.0f32; FFT_STEP];
            for sample in &mut block {
                *sample = self.pending.pop_front().expect("length checked");
            }
            let old_write_index = self.fft_write_index;
            for (offset, sample) in block.into_iter().enumerate() {
                self.fft_ring[(old_write_index + offset) & (FFT_SIZE - 1)] = sample;
            }
            self.fft_write_index = (old_write_index + FFT_STEP) & (FFT_SIZE - 1);

            let frame: Vec<f32> = (0..FFT_SIZE)
                .map(|offset| self.fft_ring[(self.fft_write_index + offset) & (FFT_SIZE - 1)])
                .collect();
            let fft = match self.fft_kernel {
                FftKernel::PortablePackedRealF32 => fft_frame(&frame)?,
                FftKernel::FullComplexF32Diagnostic => fft_frame_full_complex_f32(&frame)?,
                FftKernel::FullComplexF32DirectTwiddleDiagnostic => {
                    fft_frame_full_complex_f32_direct_twiddle(&frame)?
                }
                FftKernel::FullComplexF32FmaButterflyDiagnostic => {
                    fft_frame_full_complex_f32_fma_butterfly(&frame)?
                }
                FftKernel::FullComplexF32FmaTwiddleDiagnostic => {
                    fft_frame_full_complex_f32_fma_twiddle(&frame)?
                }
                FftKernel::FullComplexF32FmaDiagnostic => fft_frame_full_complex_f32_fma(&frame)?,
                FftKernel::FullComplexF64Diagnostic => fft_frame_full_complex_f64(&frame)?,
            };
            let current_frame = self.peak_ring.current_frame();
            // Native sig_stream advances the magnitude-ring cursor before
            // `at(0)` resolves the row just written. The FFT output itself
            // was written to row N by get_writable_fft_frame_magnitude();
            // max-field/peak processing then runs with N as its current
            // frame and advances the max-field cursor only after find_peaks.
            let magnitude_start = ((current_frame as usize) & 255) * FFT_BINS;
            self.magnitudes[magnitude_start..magnitude_start + FFT_BINS]
                .copy_from_slice(&fft.magnitudes);
            self.peak_ring.push_frame_max(&fft.magnitudes)?;

            if current_frame >= PEAK_DELAY_FRAMES {
                let slice = PeakFrameSlice {
                    minus_two: self.magnitude_frame(current_frame.wrapping_sub(47)),
                    minus_one: self.magnitude_frame(current_frame.wrapping_sub(46)),
                    center: self.magnitude_frame(current_frame.wrapping_sub(45)),
                    plus_one: self.magnitude_frame(current_frame.wrapping_sub(44)),
                    plus_two: self.magnitude_frame(current_frame.wrapping_sub(43)),
                };
                self.peaks
                    .extend(self.peak_ring.find_peaks(current_frame, slice)?);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_cadence_waits_for_128_resampled_samples() {
        let mut stream = StreamingDsp::new(16_000).unwrap();
        stream.push_f32(&[0.0; 127]).unwrap();
        assert_eq!(stream.frames_processed(), 0);
        assert_eq!(stream.pending_resampled_samples(), 127);
        stream.push_f32(&[0.0]).unwrap();
        assert_eq!(stream.frames_processed(), 1);
        assert_eq!(stream.pending_resampled_samples(), 0);
    }

    #[test]
    fn fft_ring_frame_order_starts_at_the_next_write_position() {
        let mut stream = StreamingDsp::new(16_000).unwrap();
        let input: Vec<f32> = (0..(2 * FFT_STEP)).map(|sample| sample as f32).collect();

        stream.push_f32(&input[..FFT_STEP]).unwrap();
        assert_eq!(stream.fft_write_index, FFT_STEP);
        let first_frame: Vec<f32> = (0..FFT_SIZE)
            .map(|offset| stream.fft_ring[(stream.fft_write_index + offset) & (FFT_SIZE - 1)])
            .collect();
        assert!(first_frame[..FFT_SIZE - FFT_STEP]
            .iter()
            .all(|sample| *sample == 0.0));
        assert_eq!(&first_frame[FFT_SIZE - FFT_STEP..], &input[..FFT_STEP]);

        stream.push_f32(&input[FFT_STEP..]).unwrap();
        assert_eq!(stream.fft_write_index, 2 * FFT_STEP);
        let second_frame: Vec<f32> = (0..FFT_SIZE)
            .map(|offset| stream.fft_ring[(stream.fft_write_index + offset) & (FFT_SIZE - 1)])
            .collect();
        assert!(second_frame[..FFT_SIZE - 2 * FFT_STEP]
            .iter()
            .all(|sample| *sample == 0.0));
        assert_eq!(&second_frame[FFT_SIZE - 2 * FFT_STEP..], &input);
    }

    #[test]
    fn chunked_stream_matches_one_shot_peak_output() {
        let input: Vec<i16> = (0..(128 * 60))
            .map(|index| ((index as f32 * 0.031).sin() * 16000.0) as i16)
            .collect();
        let mut one_shot = StreamingDsp::new(16_000).unwrap();
        one_shot.push_i16(&input).unwrap();
        let expected = one_shot.drain_peaks();

        let mut chunked = StreamingDsp::new(16_000).unwrap();
        for chunk in input.chunks(73) {
            chunked.push_i16(chunk).unwrap();
        }
        assert_eq!(chunked.drain_peaks(), expected);
        assert_eq!(chunked.frames_processed(), one_shot.frames_processed());
    }

    #[test]
    fn reset_restarts_frame_cadence() {
        let mut stream = StreamingDsp::new(48_000).unwrap();
        stream.push_f32(&[0.0; 480]).unwrap();
        assert!(stream.frames_processed() > 0);
        stream.reset();
        assert_eq!(stream.frames_processed(), 0);
        assert_eq!(stream.pending_resampled_samples(), 0);
    }

    #[test]
    fn discarding_partial_fft_hop_matches_zero_tail_control() {
        let input: Vec<i16> = (0..47_999)
            .map(|index| ((index as f32 * 0.037).sin() * 12_000.0) as i16)
            .collect();
        let mut zero_tail = input.clone();
        zero_tail[47_872..].fill(0);

        let mut discard = StreamingDsp::new(16_000).unwrap();
        discard.push_i16(&input).unwrap();
        assert_eq!(discard.pending_resampled_samples(), 127);
        discard
            .flush_signature_with_frames_discarding_partial_fft_hop(61)
            .unwrap();

        let mut control = StreamingDsp::new(16_000).unwrap();
        control.push_i16(&zero_tail).unwrap();
        control.flush_signature_with_frames(61).unwrap();

        assert_eq!(discard.peaks(), control.peaks());
        assert_eq!(discard.frames_processed(), control.frames_processed());
    }
}
