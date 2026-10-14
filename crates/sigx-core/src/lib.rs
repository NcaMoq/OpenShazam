//! Platform-independent state boundary for the staged SigX reimplementation.
//!
//! DSP stages and the proven Mac legacy `.sig` pack path are connected here
//! as a platform-independent streaming boundary. Peak selection is still a
//! reference stage with an unresolved strict-list difference versus the Mac
//! oracle; serialization itself is generated from the live Rust state.

use sigx_dsp::{
    band_spectral_peaks_with_endpoint_policy,
    stream::{StreamError, StreamingDsp, SIGNATURE_FLUSH_FRAMES},
    SpectralPeak, LEGACY_BAND_DENSITY,
};
pub use sigx_dsp::{CropOrder, CropResortOrder, CropUpperEndpointPolicy, FftKernel};
use sigx_format::{encode_legacy_signature, EncodeError, LegacyPeak, LegacySignatureInput};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub sample_rate: u32,
    pub channels: u16,
    pub signature_type: u32,
    pub signature_options: u32,
    pub max_frames: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    UnsupportedSampleRate(u32),
    InvalidConfig(&'static str),
    SampleCountOverflow,
    UnsupportedStage(&'static str),
    Dsp(StreamError),
    Signature(EncodeError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub sample_rate: u32,
    pub frames_seen: usize,
    pub channels: u16,
    pub fft_frames_processed: u64,
}

#[derive(Debug, Clone)]
pub struct SigX {
    config: Config,
    frames_seen: usize,
    dsp: StreamingDsp,
    progressive_peak_history: Vec<SpectralPeak>,
}

/// The Mac Objective-C Float32 entry point multiplies every caller sample
/// in place by the f32 constant 32767.0 before forwarding it to SigX.
const NATIVE_FLOAT_INPUT_SCALE: f32 = 32767.0;

impl SigX {
    pub fn new(config: Config) -> Result<Self, Error> {
        Self::new_with_fft_kernel(config, FftKernel::PortablePackedRealF32)
    }

    /// Construct SigX with an explicit FFT kernel for differential testing.
    /// The ordinary `new` constructor keeps the packed-real f32 reference.
    pub fn new_with_fft_kernel(config: Config, fft_kernel: FftKernel) -> Result<Self, Error> {
        if config.channels != 1 {
            return Err(Error::InvalidConfig(
                "only mono is implemented in the staged core",
            ));
        }
        if config.max_frames == 0 {
            return Err(Error::InvalidConfig("max_frames must be nonzero"));
        }
        if !matches!(
            config.sample_rate,
            8_000 | 16_000 | 22_050 | 32_000 | 44_100 | 48_000 | 96_000
        ) {
            return Err(Error::UnsupportedSampleRate(config.sample_rate));
        }
        let dsp = StreamingDsp::new_with_fft_kernel(config.sample_rate, fft_kernel)
            .map_err(|_| Error::UnsupportedSampleRate(config.sample_rate))?;
        Ok(Self {
            config,
            frames_seen: 0,
            dsp,
            progressive_peak_history: Vec::new(),
        })
    }

    pub fn push_i16(&mut self, samples: &[i16]) -> Result<(), Error> {
        self.reserve_frames(samples.len())?;
        let pending_before = self.dsp.peaks().len();
        self.dsp.push_i16(samples).map_err(Error::Dsp)?;
        self.progressive_peak_history
            .extend_from_slice(&self.dsp.peaks()[pending_before..]);
        self.frames_seen += samples.len();
        Ok(())
    }

    pub fn push_f32(&mut self, samples: &[f32]) -> Result<(), Error> {
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err(Error::InvalidConfig("non-finite float sample"));
        }
        self.reserve_frames(samples.len())?;
        // Mirror the Native SigX wrapper's preprocessing without mutating the
        // caller-owned Rust slice.
        let scaled: Vec<f32> = samples
            .iter()
            .map(|sample| *sample * NATIVE_FLOAT_INPUT_SCALE)
            .collect();
        let pending_before = self.dsp.peaks().len();
        self.dsp.push_f32(&scaled).map_err(Error::Dsp)?;
        self.progressive_peak_history
            .extend_from_slice(&self.dsp.peaks()[pending_before..]);
        self.frames_seen += samples.len();
        Ok(())
    }

    /// Increase the accepted streaming-input bound without resetting DSP
    /// state. Native SignatureGenerator can replace its current frame target
    /// after a Retry arrives while generation is still active.
    pub fn extend_max_frames(&mut self, max_frames: usize) -> Result<(), Error> {
        if max_frames < self.frames_seen {
            return Err(Error::InvalidConfig(
                "max_frames cannot be below frames already processed",
            ));
        }
        self.config.max_frames = max_frames;
        Ok(())
    }

    fn reserve_frames(&self, count: usize) -> Result<(), Error> {
        let next = self
            .frames_seen
            .checked_add(count)
            .ok_or(Error::SampleCountOverflow)?;
        if next > self.config.max_frames {
            return Err(Error::SampleCountOverflow);
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            sample_rate: self.config.sample_rate,
            frames_seen: self.frames_seen,
            channels: self.config.channels,
            fft_frames_processed: self.dsp.frames_processed(),
        }
    }

    pub fn reset(&mut self) {
        self.frames_seen = 0;
        self.dsp.reset();
        self.progressive_peak_history.clear();
    }

    /// Drain peaks pending in the connected DSP pipeline. This diagnostic API
    /// does not erase the separate cycle history used by Type-2 progressive
    /// snapshots; a full `reset` starts a new history.
    pub fn drain_peaks(&mut self) -> Vec<SpectralPeak> {
        self.dsp.drain_peaks()
    }

    /// Serialize the current live DSP output through the recovered Mac
    /// `basic_signature_t` → `io::pack` legacy path.
    ///
    /// Clone and zero-tail flush the live DSP state using the recovered
    /// 61-frame low-level lookahead length, then apply the four-band
    /// `BandSpectralPeaks` stage before the legacy pack. This existing path is
    /// retained for one-shot callers. Native TaggingType 2 uses a separate
    /// live-peak, no-flush freeze path; this method does not claim Native
    /// Progressive byte parity. It is non-destructive, so later PCM remains
    /// usable.
    pub fn signature(&self) -> Result<Vec<u8>, Error> {
        self.signature_with_flush_frames(SIGNATURE_FLUSH_FRAMES)
    }

    /// Snapshot the Native `signatureType=2` cycle's accumulated live peaks
    /// without zero-tail processing, then clear only the pending peak queue.
    /// This is distinct from Auto Shazam's `TaggingType=2`: the app's ordinary
    /// manual `TaggingType=1` path also configures `signatureType=2` with
    /// options 0. The statically mapped signature-type-2 dispatch calls
    /// `get_sig(..., false)` and clears the pending deque; unlike other freeze
    /// branches, it has no direct `flush_fft` checkpoint/zero-tail/rewind call.
    /// Saved staged-vs-fresh outputs show earlier peaks remain visible to
    /// later snapshots. `progressive_peak_history` models that observable
    /// behavior; exact Native internal storage remains unknown. The caller
    /// supplies Native `signature_info` fields; stream-position mapping is
    /// controlled by the Progressive adapter.
    pub fn take_type2_progressive_signature(
        &mut self,
        stream_pos_ticks: u64,
        sample_length_samples: u64,
    ) -> Result<Vec<u8>, Error> {
        let signature = self.encode_peaks(
            &self.progressive_peak_history,
            stream_pos_ticks,
            sample_length_samples,
            CropOrder::FrequencyThenMagnitude,
            CropResortOrder::TimeThenFrequency,
            CropUpperEndpointPolicy::IncludeInFinalBand,
        )?;
        self.dsp.drain_peaks();
        Ok(signature)
    }

    /// Diagnostic variant that changes only the zero-tail lookahead length.
    /// The production/default path uses the native-recovered 61 frames.
    pub fn signature_with_flush_frames(&self, flush_frames: usize) -> Result<Vec<u8>, Error> {
        self.signature_with_flush_frames_and_crop_order(
            flush_frames,
            CropOrder::FrequencyThenMagnitude,
        )
    }

    /// Diagnostic signature builder that changes only the over-density crop
    /// ranking. The default signature methods continue to use the statically
    /// recovered frequency-first ordering.
    pub fn signature_with_flush_frames_and_crop_order(
        &self,
        flush_frames: usize,
        crop_order: CropOrder,
    ) -> Result<Vec<u8>, Error> {
        self.signature_with_flush_frames_and_crop_policy(
            flush_frames,
            crop_order,
            CropResortOrder::TimeThenFrequency,
        )
    }

    /// Diagnostic signature builder that independently controls both the
    /// density ranking and the retained-range ordering after crop.
    pub fn signature_with_flush_frames_and_crop_policy(
        &self,
        flush_frames: usize,
        crop_order: CropOrder,
        resort_order: CropResortOrder,
    ) -> Result<Vec<u8>, Error> {
        self.signature_with_crop_endpoint_policy(
            flush_frames,
            crop_order,
            resort_order,
            CropUpperEndpointPolicy::IncludeInFinalBand,
        )
    }

    /// Diagnostic signature path for controlled tests of the exact final
    /// frequency boundary. Standard methods retain the recovered inclusive
    /// endpoint; this method also permits an exclusive counterfactual.
    pub fn signature_with_crop_endpoint_policy(
        &self,
        flush_frames: usize,
        crop_order: CropOrder,
        resort_order: CropResortOrder,
        endpoint_policy: CropUpperEndpointPolicy,
    ) -> Result<Vec<u8>, Error> {
        self.build_signature_with_crop_endpoint_policy(
            flush_frames,
            crop_order,
            resort_order,
            endpoint_policy,
            false,
        )
    }

    /// Diagnostic-only finalization that drops fewer than 128 pending
    /// resampled samples before the zero-tail flush. A direct Native trace
    /// showed this behavior for one 16 kHz terminal-remainder input; it is not
    /// generalized to other rates or promoted to the default path.
    pub fn signature_with_crop_endpoint_policy_discarding_partial_fft_hop(
        &self,
        flush_frames: usize,
        crop_order: CropOrder,
        resort_order: CropResortOrder,
        endpoint_policy: CropUpperEndpointPolicy,
    ) -> Result<Vec<u8>, Error> {
        self.build_signature_with_crop_endpoint_policy(
            flush_frames,
            crop_order,
            resort_order,
            endpoint_policy,
            true,
        )
    }

    fn build_signature_with_crop_endpoint_policy(
        &self,
        flush_frames: usize,
        crop_order: CropOrder,
        resort_order: CropResortOrder,
        endpoint_policy: CropUpperEndpointPolicy,
        discard_partial_fft_hop: bool,
    ) -> Result<Vec<u8>, Error> {
        let mut finalized_dsp = self.dsp.clone();
        if discard_partial_fft_hop {
            finalized_dsp
                .flush_signature_with_frames_discarding_partial_fft_hop(flush_frames)
                .map_err(Error::Dsp)?;
        } else {
            finalized_dsp
                .flush_signature_with_frames(flush_frames)
                .map_err(Error::Dsp)?;
        }
        let sample_length_samples = (self.config.sample_rate as u64)
            .saturating_mul(6)
            .checked_div(25)
            .and_then(|padding| (self.frames_seen as u64).checked_add(padding))
            .ok_or(Error::SampleCountOverflow)?;
        self.encode_peaks(
            finalized_dsp.peaks(),
            0,
            sample_length_samples,
            crop_order,
            resort_order,
            endpoint_policy,
        )
    }

    fn encode_peaks(
        &self,
        peaks: &[SpectralPeak],
        stream_pos_ticks: u64,
        sample_length_samples: u64,
        crop_order: CropOrder,
        resort_order: CropResortOrder,
        endpoint_policy: CropUpperEndpointPolicy,
    ) -> Result<Vec<u8>, Error> {
        let bands = band_spectral_peaks_with_endpoint_policy(
            peaks,
            LEGACY_BAND_DENSITY,
            true,
            crop_order,
            resort_order,
            endpoint_policy,
        );
        let sections = bands
            .into_iter()
            .map(|band| {
                band.into_iter()
                    .map(|peak| LegacyPeak {
                        time_ticks: peak.time_ticks,
                        magnitude: peak.magnitude,
                        frequency_q6: peak.frequency_q6,
                    })
                    .collect()
            })
            .collect();
        let input = LegacySignatureInput {
            sample_rate: self.config.sample_rate,
            stream_pos_ticks,
            sample_length_samples,
            density: sigx_format::LEGACY_DEFAULT_DENSITY,
            sections,
        };
        encode_legacy_signature(&input).map_err(Error::Signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: 10_000,
        }
    }

    #[test]
    fn tracks_bounded_int16_state_and_reset() {
        let mut sigx = SigX::new(config()).unwrap();
        sigx.push_i16(&[1, 2, 3]).unwrap();
        assert_eq!(sigx.snapshot().frames_seen, 3);
        sigx.reset();
        assert_eq!(sigx.snapshot().frames_seen, 0);
    }

    #[test]
    fn extends_stream_bound_without_resetting_or_changing_signature() {
        let first: Vec<i16> = (0..10_000)
            .map(|index| ((index * 73 % 30_001) as i64 - 15_000) as i16)
            .collect();
        let second: Vec<i16> = (10_000..12_000)
            .map(|index| ((index * 73 % 30_001) as i64 - 15_000) as i16)
            .collect();

        let mut extended = SigX::new(config()).unwrap();
        extended.push_i16(&first).unwrap();
        let before = extended.snapshot();
        extended.extend_max_frames(12_000).unwrap();
        assert_eq!(extended.snapshot(), before);
        extended.push_i16(&second).unwrap();

        let mut reference = SigX::new(Config {
            max_frames: 12_000,
            ..config()
        })
        .unwrap();
        reference.push_i16(&first).unwrap();
        reference.push_i16(&second).unwrap();
        assert_eq!(
            extended.signature().unwrap(),
            reference.signature().unwrap()
        );
        assert_eq!(extended.snapshot().frames_seen, 12_000);
    }

    #[test]
    fn cannot_shrink_stream_bound_below_existing_state() {
        let mut sigx = SigX::new(config()).unwrap();
        sigx.push_i16(&[1, 2, 3]).unwrap();
        assert_eq!(
            sigx.extend_max_frames(2),
            Err(Error::InvalidConfig(
                "max_frames cannot be below frames already processed"
            ))
        );
    }

    #[test]
    fn rejects_nonfinite_float() {
        let mut sigx = SigX::new(config()).unwrap();
        assert_eq!(
            sigx.push_f32(&[f32::NAN]),
            Err(Error::InvalidConfig("non-finite float sample"))
        );
    }

    #[test]
    fn float_input_uses_native_32767_scale_without_mutating_caller() {
        let input = [0.5_f32, -0.25, 0.125];
        let original_bits = input.map(f32::to_bits);
        let mut actual = SigX::new(config()).unwrap();
        actual.push_f32(&input).unwrap();

        let mut reference = SigX::new(config()).unwrap();
        let scaled = input.map(|sample| sample * NATIVE_FLOAT_INPUT_SCALE);
        reference.dsp.push_f32(&scaled).unwrap();
        reference.frames_seen = input.len();

        assert_eq!(input.map(f32::to_bits), original_bits);
        assert_eq!(actual.signature().unwrap(), reference.signature().unwrap());
    }

    #[test]
    fn connects_streaming_dsp_block_cadence() {
        let mut sigx = SigX::new(config()).unwrap();
        sigx.push_i16(&[0; 127]).unwrap();
        assert_eq!(sigx.snapshot().fft_frames_processed, 0);
        sigx.push_i16(&[0]).unwrap();
        assert_eq!(sigx.snapshot().fft_frames_processed, 1);
        assert!(sigx.drain_peaks().is_empty());
    }

    #[test]
    fn serializes_live_state_into_parseable_legacy_signature() {
        let mut sigx = SigX::new(config()).unwrap();
        sigx.push_i16(&[0; 128]).unwrap();
        let before = sigx.snapshot();
        let bytes = sigx.signature().unwrap();
        assert_eq!(bytes, sigx.signature().unwrap());
        assert_eq!(sigx.snapshot(), before);
        let signature = sigx_format::Signature::parse(&bytes).unwrap();
        assert_eq!(signature.legacy_header().rate_marker_be, 24);
        assert_eq!(signature.legacy_header().sample_length_samples, 3_968);
        assert_eq!(signature.section_table().unwrap().sections.len(), 4);
    }

    #[test]
    fn type2_progressive_freeze_uses_live_peaks_and_consumes_only_peak_queue() {
        let input: Vec<i16> = (0..48_000)
            .map(|index| ((index * 73 % 30_001) as i64 - 15_000) as i16)
            .collect();
        let mut progressive_config = config();
        progressive_config.max_frames = input.len() + 4_000;
        let mut sigx = SigX::new(progressive_config).unwrap();
        sigx.push_i16(&input).unwrap();
        let before = sigx.snapshot();

        let first = sigx
            .take_type2_progressive_signature(17, input.len() as u64)
            .unwrap();
        let first_parsed = sigx_format::Signature::parse(&first).unwrap();
        assert_eq!(first_parsed.legacy_header().stream_pos_ticks_raw, 17);
        assert_eq!(
            first_parsed.legacy_header().sample_length_samples,
            input.len() as u64
        );
        assert_eq!(sigx.snapshot(), before);

        let empty = sigx.take_type2_progressive_signature(18, 0).unwrap();
        let empty_parsed = sigx_format::Signature::parse(&empty).unwrap();
        assert_eq!(empty_parsed.legacy_header().stream_pos_ticks_raw, 18);
        assert_eq!(empty_parsed.legacy_header().sample_length_samples, 0);

        let more = [0_i16; 4_000];
        sigx.push_i16(&more).unwrap();
        let later = sigx
            .take_type2_progressive_signature(19, more.len() as u64)
            .unwrap();
        assert_eq!(
            sigx_format::Signature::parse(&later)
                .unwrap()
                .legacy_header()
                .sample_length_samples,
            more.len() as u64
        );
    }

    #[test]
    fn type2_progressive_snapshots_are_repeatable_and_match_fresh_cycle() {
        let samples: Vec<i16> = (0..96_000)
            .map(|index| ((index * 73 % 30_001) as i64 - 15_000) as i16)
            .collect();
        let mut cfg = config();
        cfg.max_frames = samples.len();
        let mut staged = SigX::new(cfg).unwrap();

        staged.push_i16(&samples[..48_000]).unwrap();
        let first_stream_pos = staged.snapshot().fft_frames_processed;
        let first = staged
            .take_type2_progressive_signature(first_stream_pos, 51_840)
            .unwrap();
        let repeated = staged
            .take_type2_progressive_signature(first_stream_pos, 51_840)
            .unwrap();
        assert_eq!(first, repeated);

        staged.push_i16(&samples[48_000..]).unwrap();
        let staged_signature = staged
            .take_type2_progressive_signature(staged.snapshot().fft_frames_processed, 99_840)
            .unwrap();

        let mut fresh = SigX::new(cfg).unwrap();
        fresh.push_i16(&samples).unwrap();
        let fresh_signature = fresh
            .take_type2_progressive_signature(fresh.snapshot().fft_frames_processed, 99_840)
            .unwrap();
        assert_eq!(staged_signature, fresh_signature);
    }

    #[test]
    fn type2_progressive_distinct_segments_survive_zero_one_or_two_intermediate_gets() {
        let segment_a: Vec<i16> = (0..48_000)
            .map(|index| ((index * 73 % 30_001) as i64 - 15_000) as i16)
            .collect();
        let segment_b: Vec<i16> = segment_a.iter().rev().copied().collect();
        let mut complete = segment_a.clone();
        complete.extend_from_slice(&segment_b);
        let mut cfg = config();
        cfg.max_frames = complete.len();

        let mut final_signatures = Vec::new();
        for intermediate_gets in 0..=2 {
            let mut staged = SigX::new(cfg).unwrap();
            staged.push_i16(&segment_a).unwrap();
            let first_stream_pos = staged.snapshot().fft_frames_processed;
            if intermediate_gets > 0 {
                let first_signature = staged
                    .take_type2_progressive_signature(first_stream_pos, 51_840)
                    .unwrap();
                for _ in 1..intermediate_gets {
                    assert_eq!(
                        staged
                            .take_type2_progressive_signature(first_stream_pos, 51_840)
                            .unwrap(),
                        first_signature
                    );
                }
            }

            staged.push_i16(&segment_b).unwrap();
            let final_signature = staged
                .take_type2_progressive_signature(staged.snapshot().fft_frames_processed, 99_840)
                .unwrap();
            final_signatures.push(final_signature);
        }

        let mut fresh = SigX::new(cfg).unwrap();
        fresh.push_i16(&complete).unwrap();
        let fresh_signature = fresh
            .take_type2_progressive_signature(fresh.snapshot().fft_frames_processed, 99_840)
            .unwrap();

        assert!(final_signatures
            .iter()
            .all(|signature| *signature == fresh_signature));
        assert!(final_signatures.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn signature_snapshot_preserves_distinct_segment_stream_state() {
        let mut cfg = config();
        cfg.max_frames = 96_000;
        let segment_a: Vec<i16> = (0..28_000)
            .map(|index| ((index * 73 % 30_001) as i64 - 15_000) as i16)
            .collect();
        let segment_b: Vec<i16> = (0..4_001)
            .map(|index| ((index * 97 % 27_001) as i64 - 13_500) as i16)
            .collect();

        let mut staged = SigX::new(cfg).unwrap();
        staged.push_i16(&segment_a).unwrap();
        let state_before_snapshot = staged.snapshot();
        let signature_a = staged.signature().unwrap();
        assert_eq!(staged.signature().unwrap(), signature_a);
        assert_eq!(staged.snapshot(), state_before_snapshot);

        staged.push_i16(&segment_b).unwrap();
        let signature_ab = staged.signature().unwrap();

        let mut concatenated = segment_a;
        concatenated.extend_from_slice(&segment_b);
        let mut fresh = SigX::new(cfg).unwrap();
        fresh.push_i16(&concatenated).unwrap();
        let fresh_signature_ab = fresh.signature().unwrap();

        assert_ne!(signature_a, signature_ab);
        assert_eq!(signature_ab, fresh_signature_ab);
    }

    #[test]
    fn diagnostic_f64_fft_generates_repeatable_legacy_signature() {
        let input: Vec<i16> = (0..4_096)
            .map(|index| {
                let phase = 2.0 * std::f64::consts::PI * 440.0 * index as f64 / 16_000.0;
                (phase.sin() * 4_096.0) as i16
            })
            .collect();
        let mut sigx =
            SigX::new_with_fft_kernel(config(), FftKernel::FullComplexF64Diagnostic).unwrap();
        sigx.push_i16(&input).unwrap();
        let before = sigx.snapshot();
        let first = sigx.signature().unwrap();
        let second = sigx.signature().unwrap();

        assert_eq!(first, second);
        assert_eq!(sigx.snapshot(), before);
        let parsed = sigx_format::Signature::parse(&first).unwrap();
        assert_eq!(parsed.legacy_header().rate_marker_be, 24);
    }

    #[test]
    fn diagnostic_full_complex_f32_variants_generate_repeatable_signatures() {
        let input: Vec<i16> = (0..4_096)
            .map(|index| {
                let phase = 2.0 * std::f64::consts::PI * 440.0 * index as f64 / 16_000.0;
                (phase.sin() * 4_096.0) as i16
            })
            .collect();
        for kernel in [
            FftKernel::FullComplexF32Diagnostic,
            FftKernel::FullComplexF32DirectTwiddleDiagnostic,
            FftKernel::FullComplexF32FmaButterflyDiagnostic,
            FftKernel::FullComplexF32FmaTwiddleDiagnostic,
            FftKernel::FullComplexF32FmaDiagnostic,
        ] {
            let mut sigx = SigX::new_with_fft_kernel(config(), kernel).unwrap();
            sigx.push_i16(&input).unwrap();
            let before = sigx.snapshot();
            let first = sigx.signature().unwrap();
            assert_eq!(first, sigx.signature().unwrap(), "kernel={kernel:?}");
            assert_eq!(sigx.snapshot(), before, "kernel={kernel:?}");
            assert_eq!(
                sigx_format::Signature::parse(&first)
                    .unwrap()
                    .legacy_header()
                    .rate_marker_be,
                24,
                "kernel={kernel:?}"
            );
        }
    }

    #[test]
    fn default_constructor_remains_packed_real_f32() {
        let input: Vec<i16> = (0..4_096)
            .map(|index| ((index as f32 * 0.071).sin() * 9_000.0) as i16)
            .collect();
        let mut default = SigX::new(config()).unwrap();
        let mut explicit =
            SigX::new_with_fft_kernel(config(), FftKernel::PortablePackedRealF32).unwrap();
        default.push_i16(&input).unwrap();
        explicit.push_i16(&input).unwrap();
        assert_eq!(default.signature().unwrap(), explicit.signature().unwrap());
    }

    #[test]
    fn diagnostic_partial_hop_discard_matches_same_length_zero_tail_signature() {
        let mut input: Vec<i16> = (0..47_999)
            .map(|index| ((index as f32 * 0.037).sin() * 12_000.0) as i16)
            .collect();
        let mut zero_tail = input.clone();
        zero_tail[47_872..].fill(0);
        let config_for = |frames| Config {
            sample_rate: 16_000,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: frames,
        };

        let mut native_tail_model =
            SigX::new_with_fft_kernel(config_for(input.len()), FftKernel::FullComplexF64Diagnostic)
                .unwrap();
        native_tail_model.push_i16(&input).unwrap();
        let discard = native_tail_model
            .signature_with_crop_endpoint_policy_discarding_partial_fft_hop(
                61,
                CropOrder::MagnitudeThenFrequency,
                CropResortOrder::TimeThenFrequency,
                CropUpperEndpointPolicy::IncludeInFinalBand,
            )
            .unwrap();

        let mut control = SigX::new_with_fft_kernel(
            config_for(zero_tail.len()),
            FftKernel::FullComplexF64Diagnostic,
        )
        .unwrap();
        control.push_i16(&zero_tail).unwrap();
        let zero_tail_signature = control
            .signature_with_crop_endpoint_policy(
                61,
                CropOrder::MagnitudeThenFrequency,
                CropResortOrder::TimeThenFrequency,
                CropUpperEndpointPolicy::IncludeInFinalBand,
            )
            .unwrap();

        assert_eq!(discard, zero_tail_signature);
        let standard = native_tail_model
            .signature_with_flush_frames_and_crop_order(61, CropOrder::MagnitudeThenFrequency)
            .unwrap();
        assert_ne!(standard, discard);
        input[47_872..].fill(0);
        assert_eq!(input, zero_tail);
    }
}
