//! Deterministic, dependency-free PCM fixtures used by the oracle harness.

use std::f64::consts::PI;

pub fn mixed_tone_i16(sample_rate: u32, seconds: u32) -> Vec<i16> {
    let count = sample_rate as usize * seconds as usize;
    (0..count)
        .map(|i| {
            let t = i as f64 / sample_rate as f64;
            let value = 0.42 * (2.0 * PI * 440.0 * t).sin()
                + 0.21 * (2.0 * PI * 997.0 * t).sin()
                + 0.03 * (2.0 * PI * 37.0 * t).sin();
            (value * 32767.0).round() as i16
        })
        .collect()
}

pub fn write_i16_le(samples: &[i16]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_is_deterministic_and_little_endian() {
        let pcm = mixed_tone_i16(16_000, 3);
        assert_eq!(pcm.len(), 48_000);
        assert_eq!(&write_i16_le(&pcm)[..2], &pcm[0].to_le_bytes());
    }
}
