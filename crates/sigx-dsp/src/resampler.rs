//! Reference implementation of the Mac resample_stream dispatch.
//!
//! The FIR coefficients are numeric constants generated from the inspected
//! arm64 tables. The implementation mirrors the native ring, phase, and
//! decimation counters; vDSP is replaced by an explicit Rust f32 reduction.

use super::resampler_tables::{
    RESAMPLE_2_TO_1, RESAMPLE_3_TO_1, RESAMPLE_44K_FIRST_STAGE, RESAMPLE_44_TO_320,
};

pub const INTERNAL_SAMPLE_RATE: u32 = 16_000;
const RING_SIZE: usize = 256;
const RING_STORAGE: usize = RING_SIZE * 2;
const SMALL_RING_SIZE: usize = 64;
const SMALL_RING_STORAGE: usize = SMALL_RING_SIZE * 2;
const FOURTY_FOUR_SECOND_TAPS: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResamplerError {
    UnsupportedSampleRate { input_rate: u32, target_rate: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Direct,
    TwoToOne,
    ThreeToOne,
    FortyFourToSixteen,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resampler {
    input_rate: u32,
    target_rate: u32,
    kind: Kind,
    ring: [f32; RING_STORAGE],
    small_ring: [f32; SMALL_RING_STORAGE],
    ring_index: u32,
    first_ring_index: u32,
    decimation_counter: u32,
    phase_base: u32,
    phase_input: u32,
}

impl Resampler {
    pub fn new(input_rate: u32, target_rate: u32) -> Result<Self, ResamplerError> {
        let kind = match (input_rate, target_rate) {
            (16_000, 16_000) => Kind::Direct,
            (32_000, 16_000) => Kind::TwoToOne,
            (44_100, 16_000) => Kind::FortyFourToSixteen,
            (48_000, 16_000) => Kind::ThreeToOne,
            _ => {
                return Err(ResamplerError::UnsupportedSampleRate {
                    input_rate,
                    target_rate,
                })
            }
        };
        Ok(Self {
            input_rate,
            target_rate,
            kind,
            ring: [0.0; RING_STORAGE],
            small_ring: [0.0; SMALL_RING_STORAGE],
            ring_index: 0,
            first_ring_index: 0,
            decimation_counter: 0,
            phase_base: 0,
            phase_input: 0,
        })
    }

    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }

    pub fn target_rate(&self) -> u32 {
        self.target_rate
    }

    pub fn reset(&mut self) {
        self.ring = [0.0; RING_STORAGE];
        self.small_ring = [0.0; SMALL_RING_STORAGE];
        self.ring_index = 0;
        self.first_ring_index = 0;
        self.decimation_counter = 0;
        self.phase_base = 0;
        self.phase_input = 0;
    }

    /// Flow native-style float samples and return the newly produced 16 kHz
    /// samples. The returned vector owns only this call's output.
    pub fn push_f32(&mut self, input: &[f32]) -> Vec<f32> {
        let mut output = Vec::new();
        for &sample in input {
            match self.kind {
                Kind::Direct => output.push(sample),
                Kind::TwoToOne => self.push_two_to_one(sample, &mut output),
                Kind::ThreeToOne => self.push_three_to_one(sample, &mut output),
                Kind::FortyFourToSixteen => self.push_forty_four(sample, &mut output),
            }
        }
        output
    }

    /// Flow signed Int16 samples using the native `scvtf`-equivalent
    /// conversion. No 32767 normalization is applied.
    pub fn push_i16(&mut self, input: &[i16]) -> Vec<f32> {
        let mut output = Vec::new();
        for &sample in input {
            let converted = sample as f32;
            match self.kind {
                Kind::Direct => output.push(converted),
                Kind::TwoToOne => self.push_two_to_one(converted, &mut output),
                Kind::ThreeToOne => self.push_three_to_one(converted, &mut output),
                Kind::FortyFourToSixteen => self.push_forty_four(converted, &mut output),
            }
        }
        output
    }

    fn write_ring(ring: &mut [f32; RING_STORAGE], index: u32, sample: f32) {
        let slot = (index as usize) & (RING_SIZE - 1);
        ring[slot] = sample;
        ring[slot + RING_SIZE] = sample;
    }

    fn write_small_ring(ring: &mut [f32; SMALL_RING_STORAGE], index: u32, sample: f32) {
        let slot = (index as usize) & (SMALL_RING_SIZE - 1);
        ring[slot] = sample;
        ring[slot + SMALL_RING_SIZE] = sample;
    }

    fn dot_ring<const N: usize>(
        ring: &[f32; RING_STORAGE],
        index: u32,
        coefficients: &[u32; N],
    ) -> f32 {
        let slot = (index as usize) & (RING_SIZE - 1);
        Self::dot_vdsp_arm64_schedule(&ring[slot..slot + N], coefficients)
    }

    fn dot_small_ring<const N: usize>(
        ring: &[f32; SMALL_RING_STORAGE],
        index: u32,
        coefficients: &[u32; N],
    ) -> f32 {
        let slot = (index as usize) & (SMALL_RING_SIZE - 1);
        Self::dot_vdsp_arm64_schedule(&ring[slot..slot + N], coefficients)
    }

    /// Reproduce the measured unit-stride `vDSP_dotpr` FMA and reduction
    /// schedule without linking Apple frameworks. For 24 taps the native path
    /// uses two four-lane accumulators over eight-sample groups. For 96/128
    /// taps it assigns each 32-sample block to eight four-lane accumulators in
    /// the observed register order, then applies the observed pairwise tree.
    /// This is validated against the local Mac implementation and saved Native
    /// resampler outputs; it is not an Apple API guarantee or a promise that
    /// every future Accelerate build uses the same schedule.
    fn dot_vdsp_arm64_schedule(samples: &[f32], coefficients: &[u32]) -> f32 {
        let count = samples.len();
        debug_assert_eq!(count, coefficients.len());
        let mut merged = [0.0f32; 4];
        if count == 24 {
            let mut acc0 = [0.0f32; 4];
            let mut acc1 = [0.0f32; 4];
            for base in (0..count).step_by(8) {
                for lane in 0..4 {
                    let i0 = base + lane;
                    let i1 = base + 4 + lane;
                    acc0[lane] = samples[i0].mul_add(f32::from_bits(coefficients[i0]), acc0[lane]);
                    acc1[lane] = samples[i1].mul_add(f32::from_bits(coefficients[i1]), acc1[lane]);
                }
            }
            for lane in 0..4 {
                merged[lane] = acc0[lane] + acc1[lane];
            }
        } else {
            debug_assert!(count == 96 || count == 128);
            const VECTOR_OFFSETS: [usize; 8] = [8, 12, 16, 20, 24, 28, 4, 0];
            let mut accumulators = [[0.0f32; 4]; 8];
            for base in (0..count).step_by(32) {
                for (vector, offset) in VECTOR_OFFSETS.iter().copied().enumerate() {
                    for lane in 0..4 {
                        let index = base + offset + lane;
                        accumulators[vector][lane] = samples[index].mul_add(
                            f32::from_bits(coefficients[index]),
                            accumulators[vector][lane],
                        );
                    }
                }
            }
            for (lane, merged_lane) in merged.iter_mut().enumerate() {
                let t01 = accumulators[0][lane] + accumulators[1][lane];
                let t23 = accumulators[2][lane] + accumulators[3][lane];
                let t45 = accumulators[4][lane] + accumulators[5][lane];
                let t67 = accumulators[6][lane] + accumulators[7][lane];
                let first_half = t01 + t23;
                let second_half = t45 + t67;
                *merged_lane = first_half + second_half;
            }
        }

        let pair01 = merged[0] + merged[1];
        let pair23 = merged[2] + merged[3];
        pair01 + pair23
    }

    fn push_two_to_one(&mut self, sample: f32, output: &mut Vec<f32>) {
        let old_index = self.ring_index;
        Self::write_ring(&mut self.ring, old_index, sample);
        if old_index & 1 == 0 {
            output.push(Self::dot_ring(
                &self.ring,
                old_index + 0xa1,
                &RESAMPLE_2_TO_1,
            ));
        }
        self.ring_index = old_index.wrapping_add(1);
    }

    fn push_three_to_one(&mut self, sample: f32, output: &mut Vec<f32>) {
        let old_index = self.ring_index;
        Self::write_ring(&mut self.ring, old_index, sample);
        let old_counter = self.decimation_counter;
        self.decimation_counter = old_counter + 1;
        if old_counter == 0 {
            output.push(Self::dot_ring(
                &self.ring,
                old_index + 0x81,
                &RESAMPLE_3_TO_1,
            ));
        }
        if self.decimation_counter >= 3 {
            self.decimation_counter = 0;
        }
        self.ring_index = old_index.wrapping_add(1);
    }

    fn push_forty_four(&mut self, sample: f32, output: &mut Vec<f32>) {
        let old_index = self.first_ring_index;
        Self::write_small_ring(&mut self.small_ring, old_index, sample);
        if old_index & 1 == 0 {
            let intermediate = Self::dot_small_ring(
                &self.small_ring,
                old_index + 0x29,
                &RESAMPLE_44K_FIRST_STAGE,
            );
            self.push_forty_four_second_stage(intermediate, output);
        }
        self.first_ring_index = old_index.wrapping_add(1);
    }

    fn push_forty_four_second_stage(&mut self, sample: f32, output: &mut Vec<f32>) {
        let old_index = self.ring_index;
        Self::write_ring(&mut self.ring, old_index, sample);
        // The native 44.1 kHz stage advances the input-side phase by 320
        // units for every intermediate sample and advances the output-side
        // phase by 441 units only when a 16 kHz output is emitted. The
        // signed difference is tested against 319; this is the 320/441
        // rational schedule recovered from push_441_to_320_tight.
        let difference = self.phase_input as i64 - self.phase_base as i64;
        if difference <= 319 {
            let phase = (self.phase_base as i64 - self.phase_input as i64 + 319) as usize;
            debug_assert!(phase < 320);
            let coefficient_offset = phase * FOURTY_FOUR_SECOND_TAPS;
            let coefficients = &RESAMPLE_44_TO_320
                [coefficient_offset..coefficient_offset + FOURTY_FOUR_SECOND_TAPS];
            let slot = ((old_index + 0xa1) as usize) & (RING_SIZE - 1);
            let sum = Self::dot_vdsp_arm64_schedule(
                &self.ring[slot..slot + FOURTY_FOUR_SECOND_TAPS],
                coefficients,
            );
            output.push(sum);
            self.phase_input = self.phase_input.wrapping_add(441);
        }
        self.phase_base = self.phase_base.wrapping_add(320);
        self.ring_index = old_index.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_accepts_only_observed_rates() {
        for rate in [16_000, 32_000, 44_100, 48_000] {
            assert!(Resampler::new(rate, INTERNAL_SAMPLE_RATE).is_ok());
        }
        assert_eq!(
            Resampler::new(8_000, INTERNAL_SAMPLE_RATE),
            Err(ResamplerError::UnsupportedSampleRate {
                input_rate: 8_000,
                target_rate: INTERNAL_SAMPLE_RATE,
            })
        );
    }

    #[test]
    fn direct_and_int16_conversion_match_native_contract() {
        let mut float = Resampler::new(16_000, INTERNAL_SAMPLE_RATE).unwrap();
        assert_eq!(float.push_f32(&[1.25, -2.5]), vec![1.25, -2.5]);
        let mut int16 = Resampler::new(16_000, INTERNAL_SAMPLE_RATE).unwrap();
        assert_eq!(
            int16.push_i16(&[i16::MAX, i16::MIN]),
            vec![32767.0, -32768.0]
        );
    }

    #[test]
    fn decimation_schedules_match_static_dispatch() {
        let mut two = Resampler::new(32_000, INTERNAL_SAMPLE_RATE).unwrap();
        assert_eq!(two.push_f32(&[0.0; 6]).len(), 3);
        let mut three = Resampler::new(48_000, INTERNAL_SAMPLE_RATE).unwrap();
        assert_eq!(three.push_f32(&[0.0; 7]).len(), 3);
        let mut fourty_four = Resampler::new(44_100, INTERNAL_SAMPLE_RATE).unwrap();
        assert_eq!(fourty_four.push_f32(&[0.0; 8]).len(), 3);
    }

    #[test]
    fn chunked_flow_is_identical_to_one_shot_for_all_paths() {
        let input: Vec<f32> = (0..1024)
            .map(|index| (index as f32 * 0.013).sin())
            .collect();
        for rate in [16_000, 32_000, 44_100, 48_000] {
            let mut one_shot = Resampler::new(rate, INTERNAL_SAMPLE_RATE).unwrap();
            let expected = one_shot.push_f32(&input);
            let mut chunked = Resampler::new(rate, INTERNAL_SAMPLE_RATE).unwrap();
            let mut actual = Vec::new();
            for chunk in input.chunks(37) {
                actual.extend(chunked.push_f32(chunk));
            }
            assert_eq!(actual, expected, "rate={rate}");
        }
    }

    #[test]
    fn forty_four_khz_phase_schedule_produces_sixteen_khz_count() {
        let mut resampler = Resampler::new(44_100, INTERNAL_SAMPLE_RATE).unwrap();
        let output = resampler.push_f32(&[0.0; 44_100]);
        assert_eq!(output.len(), 16_000);
    }

    #[test]
    fn arm64_vdsp_dot_schedule_matches_saved_direct_dot_golden_bits() {
        fn xorshift32(state: &mut u32) -> u32 {
            *state ^= *state << 13;
            *state ^= *state >> 17;
            *state ^= *state << 5;
            *state
        }

        for (count, expected_bits) in [(24, 0xc49a_a6b5), (96, 0xc546_1c1a), (128, 0xc54b_3336)] {
            let mut state = 0x9e37_79b9;
            let mut samples = Vec::with_capacity(count);
            let mut coefficients = Vec::with_capacity(count);
            for _ in 0..count {
                let sample = (xorshift32(&mut state) & 0xffff) as i32 - 32768;
                let coefficient = (xorshift32(&mut state) & 0xffff) as i32 - 32768;
                samples.push(sample as f32);
                coefficients.push(((coefficient as f32) * (1.0 / 1_048_576.0)).to_bits());
            }
            assert_eq!(
                Resampler::dot_vdsp_arm64_schedule(&samples, &coefficients).to_bits(),
                expected_bits,
                "tap_count={count}"
            );
        }
    }

    #[test]
    fn generated_table_edges_are_preserved_as_f32_bits() {
        assert_eq!(RESAMPLE_3_TO_1.len(), 128);
        assert_eq!(RESAMPLE_2_TO_1.len(), 96);
        assert_eq!(RESAMPLE_44K_FIRST_STAGE.len(), 24);
        assert_eq!(RESAMPLE_44_TO_320.len(), 320 * FOURTY_FOUR_SECOND_TAPS);
        assert_eq!(RESAMPLE_3_TO_1[0], 0xb823e341);
        assert_eq!(RESAMPLE_44_TO_320[0], 0xb3e7934d);
        assert_eq!(RESAMPLE_44_TO_320[RESAMPLE_44_TO_320.len() - 1], 0xb3e7934d);
    }
}
