//! Dependency-free DSP stages for an independent SigX research implementation.
//!
//! This crate contains dependency-free reference FFT/window, max-field peak
//! selection, and `spectral_peak_t`-compatible quantization. The analysis
//! window is generated from its mathematical formula. The resampler
//! implementation and numeric FIR coefficients are retained from private Mac
//! ARM64 oracle research; rights to redistribute those coefficients have not
//! been established. Do not publish this source tree or coefficient table
//! until that is resolved.

use std::f32::consts::PI;
use std::sync::OnceLock;

// Keep the established lane visitation order: it is significant to this
// bit-sensitive accumulation schedule.
#[allow(clippy::needless_range_loop)]
pub mod resampler;
// Retain the original coefficient array representation and values verbatim.
#[allow(clippy::large_const_arrays)]
mod resampler_tables;
pub mod stream;

pub use resampler::{Resampler, ResamplerError, INTERNAL_SAMPLE_RATE};

pub const FFT_SIZE: usize = 2048;
pub const FFT_STEP: usize = 128;
pub const FFT_BINS: usize = FFT_SIZE / 2 + 1;
pub const WINDOW_SCALE: f32 = 1.0;
// vDSP's real-FFT packed output contributes a factor of four to the
// power-magnitude normalization observed by the native `fft_stream` path.
// The reciprocal literal used by the setup is 1/524288; the resulting
// zvmags values reach sig_stream at 4/524288 = 1/131072.
pub const MAGNITUDE_SCALE: f32 = 1.0 / 131_072.0;
pub const MAGNITUDE_FLOOR: f32 = 0.015625;
pub const LOG_OFFSET: f32 = 4.158_883;
pub const LOG_SCALE: f32 = 1_477.319_7;
pub const PEAK_RING_FRAMES: usize = 256;
pub const PEAK_FRAME_STRIDE_FLOATS: usize = 0x1004 / std::mem::size_of::<f32>();
pub const PEAK_DELAY_FRAMES: u64 = 45;
pub const PEAK_FIRST_BIN: usize = 10;
pub const PEAK_LAST_BIN_EXCLUSIVE: usize = 0x3f7;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Complex32 {
    pub re: f32,
    pub im: f32,
}

impl Complex32 {
    fn from_real(value: f32) -> Self {
        Self { re: value, im: 0.0 }
    }

    fn mul(self, other: Self) -> Self {
        Self {
            re: self.re * other.re - self.im * other.im,
            im: self.re * other.im + self.im * other.re,
        }
    }

    fn mul_fma(self, other: Self) -> Self {
        Self {
            re: self.re.mul_add(other.re, -(self.im * other.im)),
            im: self.re.mul_add(other.im, self.im * other.re),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FftFrame {
    pub spectrum: Vec<Complex32>,
    pub magnitudes: Vec<f32>,
}

/// Selectable Rust FFT arithmetic for oracle differential experiments.
///
/// The default core remains the portable packed-real f32 implementation.
/// `FullComplexF64Diagnostic` is a platform-independent reference variant;
/// it is not asserted to reproduce Apple's internal vDSP arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FftKernel {
    PortablePackedRealF32,
    FullComplexF32Diagnostic,
    FullComplexF32DirectTwiddleDiagnostic,
    FullComplexF32FmaButterflyDiagnostic,
    FullComplexF32FmaTwiddleDiagnostic,
    FullComplexF32FmaDiagnostic,
    FullComplexF64Diagnostic,
}

#[derive(Debug, Clone, Copy)]
struct Complex64 {
    re: f64,
    im: f64,
}

impl Complex64 {
    fn mul(self, other: Self) -> Self {
        Self {
            re: self.re * other.re - self.im * other.im,
            im: self.re * other.im + self.im * other.re,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FftError {
    WrongFrameLength { actual: usize },
}

/// Evaluate the sine-squared analysis window in f64, then round once to f32.
///
/// This is an independent mathematical implementation of the window shape;
/// it does not load or recreate a table extracted from the Native binary.
pub fn window_coefficient(index: usize) -> f32 {
    assert!(index < FFT_SIZE);
    let phase = std::f64::consts::PI * (index + 1) as f64 / (FFT_SIZE + 1) as f64;
    phase.sin().powi(2) as f32
}

pub fn windowed_frame(input: &[f32]) -> Result<Vec<f32>, FftError> {
    if input.len() != FFT_SIZE {
        return Err(FftError::WrongFrameLength {
            actual: input.len(),
        });
    }
    Ok(input
        .iter()
        .enumerate()
        .map(|(index, &sample)| sample * window_coefficient(index))
        .collect())
}

/// A portable radix-2 real FFT corresponding to the native 2048-point
/// transform. Packing the even/odd samples into a 1024-point complex FFT
/// follows the real-FFT decomposition used by the native vDSP path more
/// closely than a direct 2048-point complex transform.
pub fn fft_frame(input: &[f32]) -> Result<FftFrame, FftError> {
    let windowed = windowed_frame(input)?;
    const PACKED_SIZE: usize = FFT_SIZE / 2;
    let mut spectrum: Vec<Complex32> = (0..PACKED_SIZE)
        .map(|index| Complex32 {
            re: windowed[index * 2],
            im: windowed[index * 2 + 1],
        })
        .collect();

    let mut j = 0usize;
    for i in 1..PACKED_SIZE {
        let mut bit = PACKED_SIZE >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            spectrum.swap(i, j);
        }
    }

    let mut length = 2;
    while length <= PACKED_SIZE {
        let angle = -2.0 * PI / length as f32;
        let twiddle = Complex32 {
            re: angle.cos(),
            im: angle.sin(),
        };
        for start in (0..PACKED_SIZE).step_by(length) {
            let mut factor = Complex32 { re: 1.0, im: 0.0 };
            for offset in 0..length / 2 {
                let even = spectrum[start + offset];
                let odd = spectrum[start + offset + length / 2].mul(factor);
                spectrum[start + offset] = Complex32 {
                    re: even.re + odd.re,
                    im: even.im + odd.im,
                };
                spectrum[start + offset + length / 2] = Complex32 {
                    re: even.re - odd.re,
                    im: even.im - odd.im,
                };
                factor = factor.mul(twiddle);
            }
        }
        length <<= 1;
    }

    let mut real_spectrum = vec![Complex32::from_real(0.0); FFT_BINS];
    for bin in 1..FFT_SIZE / 2 {
        let mirror = (PACKED_SIZE - bin) & (PACKED_SIZE - 1);
        let y = spectrum[bin];
        let mirror_conjugate = Complex32 {
            re: spectrum[mirror].re,
            im: -spectrum[mirror].im,
        };
        let sum = Complex32 {
            re: y.re + mirror_conjugate.re,
            im: y.im + mirror_conjugate.im,
        };
        let difference = Complex32 {
            re: y.re - mirror_conjugate.re,
            im: y.im - mirror_conjugate.im,
        };
        let angle = -2.0 * PI * bin as f32 / FFT_SIZE as f32;
        let twiddle = Complex32 {
            re: angle.cos(),
            im: angle.sin(),
        };
        let rotated = Complex32 {
            re: twiddle.re * difference.im + twiddle.im * difference.re,
            im: twiddle.im * difference.im - twiddle.re * difference.re,
        };
        real_spectrum[bin] = Complex32 {
            re: 0.5 * (sum.re + rotated.re),
            im: 0.5 * (sum.im + rotated.im),
        };
    }
    let dc = spectrum[0];
    real_spectrum[0] = Complex32::from_real(dc.re + dc.im);
    real_spectrum[FFT_SIZE / 2] = Complex32::from_real(dc.re - dc.im);
    // `fft_stream::stream_flow` clamps every zvmags result to the native
    // 1/64 floor before the frame is handed to sig_stream. Keeping that
    // clamp in the frame, rather than only in peak quantization, matters for
    // the max-field history comparisons.
    let magnitudes = real_spectrum
        .iter()
        .map(|value| {
            value
                .re
                .mul_add(value.re, value.im * value.im)
                .mul_add(MAGNITUDE_SCALE, 0.0)
                .max(MAGNITUDE_FLOOR)
        })
        .collect();
    Ok(FftFrame {
        spectrum: real_spectrum,
        magnitudes,
    })
}

/// Diagnostic full-complex f64 radix-2 FFT using the recovered f32 window.
///
/// Window multiplication is intentionally performed in f32 before conversion
/// to f64, matching `dump_fft_magnitudes_f64.rs`. The transform then follows
/// the conventional full-complex 2048-point radix-2 butterfly and converts
/// its normalized powers back to f32 for the shared peak selector.
pub fn fft_frame_full_complex_f64(input: &[f32]) -> Result<FftFrame, FftError> {
    let windowed = windowed_frame(input)?;
    let mut spectrum: Vec<Complex64> = windowed
        .into_iter()
        .map(|sample| Complex64 {
            re: f64::from(sample),
            im: 0.0,
        })
        .collect();

    let mut j = 0usize;
    for i in 1..FFT_SIZE {
        let mut bit = FFT_SIZE >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            spectrum.swap(i, j);
        }
    }

    let mut length = 2;
    while length <= FFT_SIZE {
        let angle = -2.0 * std::f64::consts::PI / length as f64;
        let twiddle = Complex64 {
            re: angle.cos(),
            im: angle.sin(),
        };
        for start in (0..FFT_SIZE).step_by(length) {
            let mut factor = Complex64 { re: 1.0, im: 0.0 };
            for offset in 0..length / 2 {
                let even = spectrum[start + offset];
                let odd = spectrum[start + offset + length / 2].mul(factor);
                spectrum[start + offset] = Complex64 {
                    re: even.re + odd.re,
                    im: even.im + odd.im,
                };
                spectrum[start + offset + length / 2] = Complex64 {
                    re: even.re - odd.re,
                    im: even.im - odd.im,
                };
                factor = factor.mul(twiddle);
            }
        }
        length <<= 1;
    }

    let complex32 = spectrum
        .iter()
        .take(FFT_BINS)
        .map(|value| Complex32 {
            re: value.re as f32,
            im: value.im as f32,
        })
        .collect();
    let magnitudes = spectrum
        .iter()
        .take(FFT_BINS)
        .map(|value| {
            (((value.re * value.re + value.im * value.im) / 131_072.0) as f32).max(MAGNITUDE_FLOOR)
        })
        .collect();

    Ok(FftFrame {
        spectrum: complex32,
        magnitudes,
    })
}

/// Diagnostic full-complex f32 radix-2 FFT with recurrent stage twiddles.
pub fn fft_frame_full_complex_f32(input: &[f32]) -> Result<FftFrame, FftError> {
    fft_frame_full_complex_f32_variant(input, false, false, false)
}

/// Diagnostic full-complex f32 radix-2 FFT with each twiddle computed
/// directly from its stage and offset instead of recurrent multiplication.
pub fn fft_frame_full_complex_f32_direct_twiddle(input: &[f32]) -> Result<FftFrame, FftError> {
    fft_frame_full_complex_f32_variant(input, true, false, false)
}

/// Diagnostic f32 FFT with FMA in butterfly products but not recurrent
/// twiddle updates.
pub fn fft_frame_full_complex_f32_fma_butterfly(input: &[f32]) -> Result<FftFrame, FftError> {
    fft_frame_full_complex_f32_variant(input, false, true, false)
}

/// Diagnostic f32 FFT with FMA in recurrent twiddle updates but not butterfly
/// products.
pub fn fft_frame_full_complex_f32_fma_twiddle(input: &[f32]) -> Result<FftFrame, FftError> {
    fft_frame_full_complex_f32_variant(input, false, false, true)
}

/// Diagnostic full-complex f32 radix-2 FFT using fused butterfly products and
/// recurrent twiddle updates.
pub fn fft_frame_full_complex_f32_fma(input: &[f32]) -> Result<FftFrame, FftError> {
    fft_frame_full_complex_f32_variant(input, false, true, true)
}

fn fft_frame_full_complex_f32_variant(
    input: &[f32],
    use_direct_twiddles: bool,
    use_fma_butterfly: bool,
    use_fma_twiddle: bool,
) -> Result<FftFrame, FftError> {
    let windowed = windowed_frame(input)?;
    let mut spectrum: Vec<Complex32> = windowed.into_iter().map(Complex32::from_real).collect();

    let mut j = 0usize;
    for i in 1..FFT_SIZE {
        let mut bit = FFT_SIZE >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            spectrum.swap(i, j);
        }
    }

    let mut length = 2;
    let direct_twiddle_table = use_direct_twiddles.then(full_complex_f32_direct_twiddles);
    let mut stage_index = 0;
    while length <= FFT_SIZE {
        let angle = -2.0 * PI / length as f32;
        let stage_twiddle = Complex32 {
            re: angle.cos(),
            im: angle.sin(),
        };
        for start in (0..FFT_SIZE).step_by(length) {
            let mut recurrent_factor = Complex32 { re: 1.0, im: 0.0 };
            for offset in 0..length / 2 {
                let factor = if let Some(table) = direct_twiddle_table {
                    table[stage_index][offset]
                } else {
                    recurrent_factor
                };
                let even = spectrum[start + offset];
                let odd_value = spectrum[start + offset + length / 2];
                let odd = if use_fma_butterfly {
                    odd_value.mul_fma(factor)
                } else {
                    odd_value.mul(factor)
                };
                spectrum[start + offset] = Complex32 {
                    re: even.re + odd.re,
                    im: even.im + odd.im,
                };
                spectrum[start + offset + length / 2] = Complex32 {
                    re: even.re - odd.re,
                    im: even.im - odd.im,
                };
                if !use_direct_twiddles {
                    recurrent_factor = if use_fma_twiddle {
                        recurrent_factor.mul_fma(stage_twiddle)
                    } else {
                        recurrent_factor.mul(stage_twiddle)
                    };
                }
            }
        }
        length <<= 1;
        stage_index += 1;
    }

    let magnitudes = spectrum
        .iter()
        .take(FFT_BINS)
        .map(|value| {
            (value.re.mul_add(value.re, value.im * value.im) * MAGNITUDE_SCALE).max(MAGNITUDE_FLOOR)
        })
        .collect();
    let spectrum = spectrum.into_iter().take(FFT_BINS).collect();
    Ok(FftFrame {
        spectrum,
        magnitudes,
    })
}

fn full_complex_f32_direct_twiddles() -> &'static [Vec<Complex32>] {
    static TABLE: OnceLock<Vec<Vec<Complex32>>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut stages = Vec::new();
        let mut length = 2;
        while length <= FFT_SIZE {
            let angle = -2.0 * PI / length as f32;
            stages.push(
                (0..length / 2)
                    .map(|offset| {
                        let direct_angle = angle * offset as f32;
                        Complex32 {
                            re: direct_angle.cos(),
                            im: direct_angle.sin(),
                        }
                    })
                    .collect(),
            );
            length <<= 1;
        }
        stages
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpectralPeak {
    pub time_ticks: u32,
    pub frequency_q6: u16,
    pub magnitude: u16,
}

/// The five q6 boundaries recovered from
/// `addPeakToBands<spectral_peak_t>`.
///
/// Native computes `frequency_q6 * 7.8125 * (1.0 / 64.0)` and compares the
/// result against `[250, 520, 1450, 3500, 5500]`.  These integer boundaries
/// are the smallest q6 values that compare at-or-above each float boundary;
/// the first value is a low-frequency discard cutoff. The exact final
/// 5500-Hz endpoint is retained by the native equality path; values above it
/// are discarded.
pub const LEGACY_BAND_THRESHOLDS_Q6: [u32; 5] = [2_048, 4_260, 11_879, 28_672, 45_056];
pub const LEGACY_BAND_COUNT: usize = 4;
/// Native `get_sig` passes 24 as the per-band/per-window crop density.
/// This is separate from the legacy header's packed density field (`0x0f`).
pub const LEGACY_BAND_DENSITY: usize = 24;

/// Ordering used only when a per-window band exceeds its crop density.
/// `FrequencyThenMagnitude` is the standard path translated from the static
/// ARM64 comparator. `MagnitudeThenFrequency` is a diagnostic counterfactual;
/// it must not be treated as the recovered Native behavior without direct
/// end-to-end evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CropOrder {
    FrequencyThenMagnitude,
    MagnitudeThenFrequency,
}

/// Ordering of the retained range after the over-density crop.
/// `TimeThenFrequency` is the existing Rust path; `FrequencyThenTime` is the
/// alternative suggested by the ARM64 `std::__less<spectral_peak_t>` sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CropResortOrder {
    TimeThenFrequency,
    FrequencyThenTime,
}

/// Treatment of a peak exactly on the final high-frequency crop boundary.
/// `IncludeInFinalBand` is the recovered Native equality behavior;
/// `Exclusive` remains available for counterfactual diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CropUpperEndpointPolicy {
    Exclusive,
    IncludeInFinalBand,
}

/// Apply the native `BandSpectralPeaks` windowing, band assignment, and
/// per-window density crop to an already time-ordered peak stream.
///
/// `include_negative_start` selects the first native boundary: the Mac
/// `get_sig(..., bool=true, ...)` path starts at `-45 + 256 = 211` ticks;
/// the false path starts at `256`. Peaks below the first frequency threshold
/// or above the last threshold are discarded; equality at the last threshold
/// is retained. The
/// crop comparator keeps the highest frequency, then highest magnitude, for
/// the newly accumulated peaks and finally orders the retained range by time
/// then frequency, matching the two native sort instantiations.
pub fn band_spectral_peaks(
    input: &[SpectralPeak],
    density: usize,
    include_negative_start: bool,
) -> [Vec<SpectralPeak>; LEGACY_BAND_COUNT] {
    band_spectral_peaks_with_order(
        input,
        density,
        include_negative_start,
        CropOrder::FrequencyThenMagnitude,
    )
}

/// Diagnostic variant of [`band_spectral_peaks`] with an explicitly selected
/// crop ranking. The default function above remains the standard implementation.
pub fn band_spectral_peaks_with_order(
    input: &[SpectralPeak],
    density: usize,
    include_negative_start: bool,
    crop_order: CropOrder,
) -> [Vec<SpectralPeak>; LEGACY_BAND_COUNT] {
    band_spectral_peaks_with_policy(
        input,
        density,
        include_negative_start,
        crop_order,
        CropResortOrder::TimeThenFrequency,
    )
}

/// Diagnostic variant allowing both the over-density ranking and retained
/// range ordering to be varied independently.
pub fn band_spectral_peaks_with_policy(
    input: &[SpectralPeak],
    density: usize,
    include_negative_start: bool,
    crop_order: CropOrder,
    resort_order: CropResortOrder,
) -> [Vec<SpectralPeak>; LEGACY_BAND_COUNT] {
    band_spectral_peaks_with_endpoint_policy(
        input,
        density,
        include_negative_start,
        crop_order,
        resort_order,
        CropUpperEndpointPolicy::IncludeInFinalBand,
    )
}

/// Diagnostic variant that independently controls crop ranking, output order,
/// and the exact final upper-boundary case. Use `Exclusive` for a
/// counterfactual; the recovered standard path includes exact equality.
pub fn band_spectral_peaks_with_endpoint_policy(
    input: &[SpectralPeak],
    density: usize,
    include_negative_start: bool,
    crop_order: CropOrder,
    resort_order: CropResortOrder,
    endpoint_policy: CropUpperEndpointPolicy,
) -> [Vec<SpectralPeak>; LEGACY_BAND_COUNT] {
    let mut bands: [Vec<SpectralPeak>; LEGACY_BAND_COUNT] = std::array::from_fn(|_| Vec::new());
    let mut committed = [0usize; LEGACY_BAND_COUNT];
    let mut boundary = if include_negative_start {
        256u32 - PEAK_DELAY_FRAMES as u32
    } else {
        256
    };

    let finish_window = |bands: &mut [Vec<SpectralPeak>; LEGACY_BAND_COUNT],
                         committed: &mut [usize; LEGACY_BAND_COUNT]| {
        for band in 0..LEGACY_BAND_COUNT {
            let start = committed[band];
            let new_count = bands[band].len().saturating_sub(start);
            if new_count > density {
                let mut kept = bands[band][start..].to_vec();
                kept.sort_by(|left, right| match crop_order {
                    CropOrder::FrequencyThenMagnitude => right
                        .frequency_q6
                        .cmp(&left.frequency_q6)
                        .then_with(|| right.magnitude.cmp(&left.magnitude)),
                    CropOrder::MagnitudeThenFrequency => right
                        .magnitude
                        .cmp(&left.magnitude)
                        .then_with(|| right.frequency_q6.cmp(&left.frequency_q6)),
                });
                kept.truncate(density);
                kept.sort_by(|left, right| match resort_order {
                    CropResortOrder::TimeThenFrequency => left
                        .time_ticks
                        .cmp(&right.time_ticks)
                        .then_with(|| left.frequency_q6.cmp(&right.frequency_q6)),
                    CropResortOrder::FrequencyThenTime => left
                        .frequency_q6
                        .cmp(&right.frequency_q6)
                        .then_with(|| left.time_ticks.cmp(&right.time_ticks)),
                });
                bands[band].truncate(start);
                bands[band].extend(kept);
            }
            committed[band] = bands[band].len();
        }
    };

    for &peak in input {
        while peak.time_ticks >= boundary {
            finish_window(&mut bands, &mut committed);
            boundary = boundary.saturating_add(256);
            if boundary == u32::MAX {
                break;
            }
        }
        let frequency = u32::from(peak.frequency_q6);
        let band = LEGACY_BAND_THRESHOLDS_Q6
            .windows(2)
            .position(|bounds| frequency >= bounds[0] && frequency < bounds[1])
            .or_else(|| {
                (endpoint_policy == CropUpperEndpointPolicy::IncludeInFinalBand
                    && frequency == *LEGACY_BAND_THRESHOLDS_Q6.last().expect("boundary exists"))
                .then_some(LEGACY_BAND_COUNT - 1)
            });
        let Some(band) = band else {
            continue;
        };
        bands[band].push(peak);
    }
    finish_window(&mut bands, &mut committed);
    bands
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeakQuantizationError {
    BinOutOfRange { bin: usize },
    NonPositiveCurvature,
    QuantizedValueOutOfRange,
}

/// Quantize one already-selected FFT local maximum using the arithmetic in
/// `spectral_peak_t`'s constructor. Selecting which bins are peaks remains a
/// separate, unresolved `sig_stream::find_peaks` policy.
pub fn quantize_spectral_peak(
    time_ticks: u32,
    bin: usize,
    magnitudes: &[f32],
) -> Result<SpectralPeak, PeakQuantizationError> {
    if bin == 0 || bin + 1 >= magnitudes.len() {
        return Err(PeakQuantizationError::BinOutOfRange { bin });
    }
    let left = magnitudes[bin - 1].max(MAGNITUDE_FLOOR);
    let center = magnitudes[bin].max(MAGNITUDE_FLOOR);
    let right = magnitudes[bin + 1].max(MAGNITUDE_FLOOR);
    let left_log = (left.ln() + LOG_OFFSET) * LOG_SCALE;
    let center_log = (center.ln() + LOG_OFFSET) * LOG_SCALE;
    let right_log = (right.ln() + LOG_OFFSET) * LOG_SCALE;
    let curvature = center_log + center_log - (left_log + right_log);
    if curvature <= 0.0 {
        return Err(PeakQuantizationError::NonPositiveCurvature);
    }
    let interpolation = ((right_log - left_log) * 32.0 / curvature) as i32;
    let frequency = (bin as i32)
        .checked_mul(64)
        .and_then(|value| value.checked_add(interpolation))
        .ok_or(PeakQuantizationError::QuantizedValueOutOfRange)?;
    let magnitude = center_log as i32;
    if !(0..=u16::MAX as i32).contains(&frequency) || !(0..=u16::MAX as i32).contains(&magnitude) {
        return Err(PeakQuantizationError::QuantizedValueOutOfRange);
    }
    Ok(SpectralPeak {
        time_ticks,
        frequency_q6: frequency as u16,
        magnitude: magnitude as u16,
    })
}

/// The five rows passed by the native `find_peaks` caller. They correspond to
/// the center row and its four temporal neighbors in the order used by the
/// stack-built `fft_slice` at `0x50e0c`.
pub struct PeakFrameSlice<'a> {
    pub minus_two: &'a [f32],
    pub minus_one: &'a [f32],
    pub center: &'a [f32],
    pub plus_one: &'a [f32],
    pub plus_two: &'a [f32],
}

impl PeakFrameSlice<'_> {
    fn validate(&self) -> bool {
        [
            self.minus_two,
            self.minus_one,
            self.center,
            self.plus_one,
            self.plus_two,
        ]
        .iter()
        .all(|row| row.len() >= FFT_BINS)
    }
}

/// The max-field ring used by `sig_stream::push_frame_max` and
/// `sig_stream::find_peaks`. This mirrors the recovered storage shape without
/// depending on an Apple framework. It is intentionally a DSP-stage type:
/// signature serialization and mode-specific crop rules remain elsewhere.
#[derive(Debug, Clone)]
pub struct PeakMaxFieldRing {
    frames: Vec<f32>,
    current_frame: u64,
    time_offset: i64,
}

impl Default for PeakMaxFieldRing {
    fn default() -> Self {
        Self::new()
    }
}

impl PeakMaxFieldRing {
    pub fn new() -> Self {
        Self {
            frames: vec![0.0; PEAK_RING_FRAMES * PEAK_FRAME_STRIDE_FLOATS],
            current_frame: 0,
            time_offset: 0,
        }
    }

    pub fn current_frame(&self) -> u64 {
        self.current_frame
    }

    pub fn set_time_offset(&mut self, offset: i64) {
        self.time_offset = offset;
    }

    fn frame_start(&self, frame: u64) -> usize {
        ((frame as usize) & (PEAK_RING_FRAMES - 1)) * PEAK_FRAME_STRIDE_FLOATS
    }

    fn frame(&self, frame: u64) -> &[f32] {
        let start = self.frame_start(frame);
        &self.frames[start..start + FFT_BINS]
    }

    fn frame_mut(&mut self, frame: u64) -> &mut [f32] {
        let start = self.frame_start(frame);
        &mut self.frames[start..start + FFT_BINS]
    }

    fn value_at(&self, frame: u64, offset: usize) -> f32 {
        self.value_at_offset(frame, offset as isize)
    }

    fn value_at_offset(&self, frame: u64, offset: isize) -> f32 {
        let len = self.frames.len() as isize;
        let flat = (self.frame_start(frame) as isize + offset).rem_euclid(len) as usize;
        self.frames[flat]
    }

    fn set_value(&mut self, frame: u64, offset: usize, value: f32) {
        let flat = (self.frame_start(frame) + offset) % self.frames.len();
        self.frames[flat] = value;
    }

    /// Store one magnitude frame and apply the native max-field update with
    /// the `find_peaks<spectral_peak_t>` call-site parameter `6`.
    pub fn push_frame_max(&mut self, magnitudes: &[f32]) -> Result<(), FftError> {
        if magnitudes.len() != FFT_BINS {
            return Err(FftError::WrongFrameLength {
                actual: magnitudes.len(),
            });
        }
        // The max-field storage is owned directly by sig_stream. Its cursor
        // addresses the current row, then propagates that row to the -1/-3/-6
        // history rows used by find_peaks.
        let frame_number = self.current_frame;
        let current = self.frame_mut(frame_number);
        current.copy_from_slice(magnitudes);
        // The native loop advances its byte cursor by 4 bytes, i.e. one
        // f32/bin at a time. Every bin 0..=1022 receives the in-place
        // three-bin maximum; bin 1023 is the last bin whose +2 neighbor is
        // still inside the 1025-element frame.
        for bin in 0..=FFT_BINS - 3 {
            current[bin] = current[bin].max(current[bin + 1]).max(current[bin + 2]);
        }

        let current_values = self.frame(frame_number).to_vec();
        let previous_one = self.frame(frame_number.wrapping_sub(1)).to_vec();
        let previous_three = self.frame(frame_number.wrapping_sub(3)).to_vec();
        let previous_six = self.frame(frame_number.wrapping_sub(6)).to_vec();
        for bin in 0..FFT_BINS {
            let value = current_values[bin];
            let one = previous_one[bin].max(value);
            let three = previous_three[bin].max(one);
            let six = previous_six[bin].max(three);
            self.set_value(frame_number.wrapping_sub(1), bin, one);
            self.set_value(frame_number.wrapping_sub(3), bin, three);
            self.set_value(frame_number.wrapping_sub(6), bin, six);
        }
        self.current_frame = self.current_frame.wrapping_add(1);
        Ok(())
    }

    /// Apply the native strict comparisons and quantize all selected peaks in
    /// ascending frequency-bin order. `current_frame` is the frame number
    /// before the native stream increments it after `find_peaks` returns.
    pub fn find_peaks(
        &self,
        current_frame: u64,
        slice: PeakFrameSlice<'_>,
    ) -> Result<Vec<SpectralPeak>, PeakQuantizationError> {
        if !slice.validate() {
            return Err(PeakQuantizationError::BinOutOfRange { bin: FFT_BINS });
        }
        let frequency_max_frame = current_frame.wrapping_sub(48);
        let mut peaks = Vec::new();
        for bin in PEAK_FIRST_BIN..PEAK_LAST_BIN_EXCLUSIVE {
            let center = slice.center[bin];
            if center < MAGNITUDE_FLOOR || center < self.value_at(frequency_max_frame, bin - 1) {
                continue;
            }
            let mut frequency_neighbor_max = self
                .value_at(frequency_max_frame, bin - 3)
                .max(self.value_at(frequency_max_frame, bin + 1));
            // The native inner loop advances by three bins and executes
            // three times: +2,+5,+8 and -4,-7,-10 relative to the current
            // bin. The byte cursor is not a seven-entry list.
            for offset in [2isize, 5, 8] {
                frequency_neighbor_max = frequency_neighbor_max
                    .max(self.value_at_offset(frequency_max_frame, bin as isize + offset));
            }
            for offset in [4isize, 7, 10] {
                frequency_neighbor_max = frequency_neighbor_max
                    .max(self.value_at_offset(frequency_max_frame, bin as isize - offset));
            }
            if center <= frequency_neighbor_max {
                continue;
            }
            let mut time_neighbor_max = self
                .value_at(current_frame.wrapping_sub(52), bin - 1)
                .max(self.value_at(current_frame.wrapping_sub(44), bin - 1));
            for step in 0..=5 {
                time_neighbor_max = time_neighbor_max
                    .max(self.value_at(current_frame.wrapping_sub(41 - step * 7), bin - 1))
                    .max(self.value_at(current_frame.wrapping_sub(55 + step * 7), bin - 1));
            }
            if center <= time_neighbor_max {
                continue;
            }
            let time = current_frame as i64 - self.time_offset - PEAK_DELAY_FRAMES as i64;
            if time < 0 || time > u32::MAX as i64 {
                continue;
            }
            peaks.push(quantize_spectral_peak(time as u32, bin, slice.center)?);
        }
        Ok(peaks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_shape_and_independent_window_boundaries() {
        assert_eq!(FFT_BINS, 1025);
        assert!(window_coefficient(0) > 0.000002);
        assert!(window_coefficient(0) < 0.000003);
        assert!(window_coefficient(1) > window_coefficient(0));
        assert!(window_coefficient(FFT_SIZE / 2) > 0.999);
        assert!((window_coefficient(FFT_SIZE - 1) - window_coefficient(0)).abs() < 1e-7);
    }

    #[test]
    fn window_calculates_in_f64_and_rounds_once_to_f32() {
        for index in 0..FFT_SIZE {
            let phase = std::f64::consts::PI * (index + 1) as f64 / (FFT_SIZE + 1) as f64;
            let exact_precision = phase.sin().powi(2);
            let rounded = exact_precision as f32;
            let actual = window_coefficient(index);
            assert_eq!(actual.to_bits(), rounded.to_bits(), "index={index}");
            let rounding_error = (actual as f64 - exact_precision).abs();
            assert!(
                rounding_error <= exact_precision.abs() * f32::EPSILON as f64,
                "index={index}, error={rounding_error}"
            );
            let mirror = window_coefficient(FFT_SIZE - 1 - index);
            assert!((actual - mirror).abs() < 1e-7, "index={index}");
        }
    }

    #[test]
    fn fft_rejects_non_native_frame_size() {
        assert_eq!(
            fft_frame(&[]),
            Err(FftError::WrongFrameLength { actual: 0 })
        );
    }

    #[test]
    fn fft_sine_has_a_dominant_positive_frequency_bin() {
        let input: Vec<f32> = (0..FFT_SIZE)
            .map(|index| (2.0 * PI * 32.0 * index as f32 / FFT_SIZE as f32).sin())
            .collect();
        let frame = fft_frame(&input).unwrap();
        let (bin, _) = frame
            .magnitudes
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|left, right| left.1.total_cmp(right.1))
            .unwrap();
        assert!((31..=33).contains(&bin));
    }

    #[test]
    fn full_complex_f32_variants_preserve_frame_contract() {
        type TestFftKernel = fn(&[f32]) -> Result<FftFrame, FftError>;
        let kernels: [TestFftKernel; 5] = [
            fft_frame_full_complex_f32,
            fft_frame_full_complex_f32_direct_twiddle,
            fft_frame_full_complex_f32_fma_butterfly,
            fft_frame_full_complex_f32_fma_twiddle,
            fft_frame_full_complex_f32_fma,
        ];
        let silence = vec![0.0; FFT_SIZE];
        let tone: Vec<f32> = (0..FFT_SIZE)
            .map(|index| {
                let phase = 2.0 * PI * 64.0 * index as f32 / FFT_SIZE as f32;
                phase.sin() * 12_000.0
            })
            .collect();

        for kernel in kernels {
            assert_eq!(
                kernel(&silence[..FFT_SIZE - 1]),
                Err(FftError::WrongFrameLength {
                    actual: FFT_SIZE - 1
                })
            );
            let silent_frame = kernel(&silence).unwrap();
            assert_eq!(silent_frame.spectrum.len(), FFT_BINS);
            assert_eq!(silent_frame.magnitudes.len(), FFT_BINS);
            assert!(silent_frame
                .magnitudes
                .iter()
                .all(|magnitude| *magnitude == MAGNITUDE_FLOOR));

            let tone_frame = kernel(&tone).unwrap();
            let max_bin = tone_frame
                .magnitudes
                .iter()
                .enumerate()
                .max_by(|left, right| left.1.total_cmp(right.1))
                .map(|(bin, _)| bin)
                .unwrap();
            assert_eq!(max_bin, 64);
        }
    }

    #[test]
    fn quantization_produces_q6_frequency() {
        let mut magnitudes = vec![0.02; FFT_BINS];
        magnitudes[32] = 1.0;
        let peak = quantize_spectral_peak(125, 32, &magnitudes).unwrap();
        assert_eq!(peak.time_ticks, 125);
        assert!((32 * 64 - 64..=32 * 64 + 64).contains(&(peak.frequency_q6 as usize)));
    }

    #[test]
    fn peak_selector_applies_native_delay_and_strict_neighborhood() {
        let mut center = vec![0.0; FFT_BINS];
        center[32] = 1.0;
        center[31] = MAGNITUDE_FLOOR;
        center[33] = MAGNITUDE_FLOOR;
        assert!(quantize_spectral_peak(0, 32, &center).is_ok());
        let zeros = vec![0.0; FFT_BINS];
        let mut ring = PeakMaxFieldRing::new();
        let floor = vec![MAGNITUDE_FLOOR; FFT_BINS];
        for _ in 0..49 {
            ring.push_frame_max(&floor).unwrap();
        }
        ring.set_time_offset(4);
        let peaks = ring
            .find_peaks(
                ring.current_frame(),
                PeakFrameSlice {
                    minus_two: &zeros,
                    minus_one: &zeros,
                    center: &center,
                    plus_one: &zeros,
                    plus_two: &zeros,
                },
            )
            .unwrap();
        assert_eq!(peaks.len(), 1);
        assert_eq!(peaks[0].time_ticks, 0);
        assert_eq!(peaks[0].frequency_q6 / 64, 32);
    }

    #[test]
    fn max_field_ring_updates_native_three_six_frame_chain() {
        let mut ring = PeakMaxFieldRing::new();
        let mut first = vec![0.0; FFT_BINS];
        first[64] = 3.0;
        ring.push_frame_max(&first).unwrap();
        let mut second = vec![0.0; FFT_BINS];
        second[64] = 5.0;
        ring.push_frame_max(&second).unwrap();
        assert_eq!(ring.frame(0)[64], 5.0);
        assert_eq!(ring.current_frame(), 2);
    }

    #[test]
    fn band_mapping_assigns_and_orders_synthetic_peaks() {
        let peaks = [
            SpectralPeak {
                time_ticks: 71,
                frequency_q6: 7_000,
                magnitude: 222,
            },
            SpectralPeak {
                time_ticks: 84,
                frequency_q6: 3_000,
                magnitude: 111,
            },
            SpectralPeak {
                time_ticks: 196,
                frequency_q6: 7_000,
                magnitude: 222,
            },
            SpectralPeak {
                time_ticks: 209,
                frequency_q6: 3_000,
                magnitude: 111,
            },
            SpectralPeak {
                time_ticks: 321,
                frequency_q6: 7_000,
                magnitude: 222,
            },
            SpectralPeak {
                time_ticks: 334,
                frequency_q6: 3_000,
                magnitude: 111,
            },
        ];
        let bands = band_spectral_peaks(&peaks, 24, true);
        assert_eq!(bands[0].len(), 3);
        assert_eq!(bands[1].len(), 3);
        assert!(bands[2].is_empty());
        assert!(bands[3].is_empty());
        assert_eq!(bands[0][0].frequency_q6, 3_000);
        assert_eq!(bands[1][0].frequency_q6, 7_000);
    }

    #[test]
    fn legacy_band_mapping_uses_native_density_24_not_header_density() {
        assert_eq!(LEGACY_BAND_DENSITY, 24);
        let peaks: Vec<SpectralPeak> = (0..25)
            .map(|index| SpectralPeak {
                time_ticks: 220,
                frequency_q6: 2_500 + index,
                // Make the largest magnitude belong to the lowest frequency
                // so this fixture distinguishes the native sort keys.
                magnitude: 24 - index,
            })
            .collect();
        let bands = band_spectral_peaks(&peaks, LEGACY_BAND_DENSITY, true);

        assert_eq!(bands[0].len(), 24);
        assert_eq!(
            bands[0]
                .iter()
                .map(|peak| peak.frequency_q6)
                .collect::<Vec<_>>(),
            (2_501..=2_524).collect::<Vec<_>>()
        );
        assert_eq!(
            bands[0]
                .iter()
                .map(|peak| peak.magnitude)
                .collect::<Vec<_>>(),
            (0..24).rev().collect::<Vec<_>>()
        );
        let magnitude_diagnostic = band_spectral_peaks_with_order(
            &peaks,
            LEGACY_BAND_DENSITY,
            true,
            CropOrder::MagnitudeThenFrequency,
        );
        assert_eq!(
            magnitude_diagnostic[0]
                .iter()
                .map(|peak| peak.frequency_q6)
                .collect::<Vec<_>>(),
            (2_500..=2_523).collect::<Vec<_>>()
        );
        assert_eq!(
            magnitude_diagnostic[0]
                .iter()
                .map(|peak| peak.magnitude)
                .collect::<Vec<_>>(),
            (1..=24).rev().collect::<Vec<_>>()
        );

        let same_frequency: Vec<SpectralPeak> = (0..25)
            .map(|magnitude| SpectralPeak {
                time_ticks: 220,
                frequency_q6: 3_000,
                magnitude,
            })
            .collect();
        let tied_frequency = band_spectral_peaks(&same_frequency, LEGACY_BAND_DENSITY, true);
        assert_eq!(tied_frequency[0].len(), 24);
        let mut selected_magnitudes: Vec<_> = tied_frequency[0]
            .iter()
            .map(|peak| peak.magnitude)
            .collect();
        selected_magnitudes.sort_unstable();
        assert_eq!(selected_magnitudes, (1..25).collect::<Vec<_>>());
    }

    #[test]
    fn diagnostic_crop_resort_orders_are_independent_of_crop_ranking() {
        let peaks: Vec<SpectralPeak> = (0..25)
            .map(|index| SpectralPeak {
                time_ticks: 244 - index,
                frequency_q6: 2_500 + index as u16,
                magnitude: index as u16,
            })
            .collect();
        let time_first = band_spectral_peaks_with_policy(
            &peaks,
            LEGACY_BAND_DENSITY,
            true,
            CropOrder::FrequencyThenMagnitude,
            CropResortOrder::TimeThenFrequency,
        );
        let frequency_first = band_spectral_peaks_with_policy(
            &peaks,
            LEGACY_BAND_DENSITY,
            true,
            CropOrder::FrequencyThenMagnitude,
            CropResortOrder::FrequencyThenTime,
        );
        assert_eq!(
            time_first[0]
                .iter()
                .map(|peak| peak.frequency_q6)
                .collect::<Vec<_>>(),
            (2_501..=2_524).rev().collect::<Vec<_>>()
        );
        assert_eq!(
            frequency_first[0]
                .iter()
                .map(|peak| peak.frequency_q6)
                .collect::<Vec<_>>(),
            (2_501..=2_524).collect::<Vec<_>>()
        );
    }

    #[test]
    fn legacy_band_mapping_retains_exact_final_endpoint_only() {
        let peaks = [
            SpectralPeak {
                time_ticks: 1,
                frequency_q6: 2_047,
                magnitude: 1,
            },
            SpectralPeak {
                time_ticks: 2,
                frequency_q6: 2_048,
                magnitude: 2,
            },
            SpectralPeak {
                time_ticks: 3,
                frequency_q6: 45_055,
                magnitude: 3,
            },
            SpectralPeak {
                time_ticks: 4,
                frequency_q6: 45_056,
                magnitude: 4,
            },
            SpectralPeak {
                time_ticks: 5,
                frequency_q6: 45_057,
                magnitude: 5,
            },
        ];
        let bands = band_spectral_peaks(&peaks, 24, true);
        assert_eq!(bands[0].len(), 1);
        assert_eq!(bands[0][0].frequency_q6, 2_048);
        assert_eq!(bands[3].len(), 2);
        assert_eq!(bands[3][0].frequency_q6, 45_055);
        assert_eq!(bands[3][1].frequency_q6, 45_056);
        assert!(bands[1].is_empty());
        assert!(bands[2].is_empty());
    }

    #[test]
    fn standard_band_mapping_includes_exact_final_endpoint() {
        let endpoint = SpectralPeak {
            time_ticks: 4,
            frequency_q6: 45_056,
            magnitude: 4,
        };
        let standard = band_spectral_peaks(&[endpoint], 24, true);
        let exclusive = band_spectral_peaks_with_endpoint_policy(
            &[endpoint],
            24,
            true,
            CropOrder::FrequencyThenMagnitude,
            CropResortOrder::TimeThenFrequency,
            CropUpperEndpointPolicy::Exclusive,
        );

        assert_eq!(standard[3], [endpoint]);
        assert!(exclusive.iter().all(Vec::is_empty));
    }
}
