//! Conservative parser for the fields proven by the Mac oracle.
//!
//! Unknown bytes are preserved. This is intentional: the observed `0x34`
//! length field is inside the first 48 bytes but does not prove that the
//! format has a fixed 48-byte header.

use std::fmt;

pub const MAGIC: [u8; 4] = [0x80, 0x25, 0xfe, 0xca];
pub const LENGTH_FIELD_OFFSET: usize = 0x08;
pub const SECOND_LENGTH_FIELD_OFFSET: usize = 0x34;
pub const SECTION_TABLE_OFFSET: usize = 0x38;
pub const SECTION_ALIGNMENT: usize = 4;
/// Observed payload granularity in all 108 saved Mac-oracle signatures.
///
/// This is a framing-level observation only. The bytes are not yet assigned
/// to time/frequency/energy fields.
pub const OBSERVED_RECORD_BYTES: usize = 5;
pub const COMPACT_ESCAPE: u8 = 0xff;
pub const COMPACT_TIME_TICKS_PER_SECOND: u32 = 125;
pub const LEGACY_VERSION_WORD: u32 = 0x9400_1c00;
pub const LEGACY_OUTER_TAG_BE: u32 = 0x40;
pub const LEGACY_SECTION_TAG_SUFFIX: [u8; 3] = [0x00, 0x03, 0x60];
/// Value stored in the legacy Signature header's packed density bits.
/// It is not the `BandSpectralPeaks` per-window crop limit (24).
pub const LEGACY_DEFAULT_DENSITY: u8 = 0x0f;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    TooShort { actual: usize, minimum: usize },
    BadMagic([u8; 4]),
    LengthMismatch { declared: u32, actual: usize },
    CrcMismatch { declared: u32, actual: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionParseError {
    Signature(ParseError),
    MissingSectionHeader {
        offset: usize,
        remaining: usize,
    },
    BadSectionTag {
        offset: usize,
        tag: [u8; 4],
    },
    SectionOverrun {
        offset: usize,
        declared: u32,
        available: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordParseError {
    LengthNotMultiple { tag: [u8; 4], length: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactParseError {
    LengthNotMultiple { length: usize },
    TimeOverflow { previous: u32, delta: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    UnsupportedSampleRate(u32),
    NonMonotonicTime { previous: u32, current: u32 },
    LengthOverflow,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSampleRate(rate) => {
                write!(
                    f,
                    "legacy signature does not define a rate marker for {rate} Hz"
                )
            }
            Self::NonMonotonicTime { previous, current } => {
                write!(f, "peak time moved backwards: {current} after {previous}")
            }
            Self::LengthOverflow => write!(f, "legacy signature length does not fit in u32"),
        }
    }
}

impl std::error::Error for EncodeError {}

impl fmt::Display for CompactParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LengthNotMultiple { length } => write!(
                f,
                "compact payload has {length} bytes, not a multiple of {OBSERVED_RECORD_BYTES}"
            ),
            Self::TimeOverflow { previous, delta } => {
                write!(f, "compact time overflow: {previous} + {delta}")
            }
        }
    }
}

impl std::error::Error for CompactParseError {}

impl fmt::Display for RecordParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LengthNotMultiple { tag, length } => write!(
                f,
                "opaque section payload for {tag:02x?} has {length} bytes, not a multiple of {OBSERVED_RECORD_BYTES}"
            ),
        }
    }
}

impl std::error::Error for RecordParseError {}

impl fmt::Display for SectionParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Signature(error) => write!(f, "signature parse failed: {error}"),
            Self::MissingSectionHeader { offset, remaining } => write!(
                f,
                "section header at {offset:#x} is too short: {remaining} bytes remain"
            ),
            Self::BadSectionTag { offset, tag } => {
                write!(f, "bad section tag at {offset:#x}: {tag:02x?}")
            }
            Self::SectionOverrun {
                offset,
                declared,
                available,
            } => write!(
                f,
                "section at {offset:#x} declares {declared} bytes, only {available} remain"
            ),
        }
    }
}

impl std::error::Error for SectionParseError {}

impl From<ParseError> for SectionParseError {
    fn from(error: ParseError) -> Self {
        Self::Signature(error)
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { actual, minimum } => {
                write!(f, "signature too short: {actual} < {minimum}")
            }
            Self::BadMagic(magic) => write!(f, "bad magic: {magic:02x?}"),
            Self::LengthMismatch { declared, actual } => write!(
                f,
                "length field {declared} does not match payload length {actual}"
            ),
            Self::CrcMismatch { declared, actual } => write!(
                f,
                "crc32 mismatch: declared {declared:#x}, calculated {actual:#x}"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    bytes: Vec<u8>,
}

/// Header fields whose layout is directly supported by `LegacyFatSigInfo` and
/// the `basic_signature_t` pack path. Names ending in `_raw` intentionally
/// avoid assigning meaning to flag bits not yet proven by a getter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyHeader {
    pub version_word: u32,
    pub rate_marker_be: u32,
    pub rate_code: Option<u8>,
    pub stream_pos_ticks_raw: u64,
    pub sample_length_samples: u64,
    pub flags_word_raw: u32,
    pub outer_tag_be: u32,
}

/// One native `spectral_peak_t` value in the legacy `.sig` pack path.
///
/// The time unit is the proven 1/125-second tick. The two quantized fields
/// are copied into the five-byte `spectral_peak_compact_t` record by the Mac
/// `convert_array` routine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyPeak {
    pub time_ticks: u32,
    pub magnitude: u16,
    pub frequency_q6: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacySignatureInput {
    pub sample_rate: u32,
    pub stream_pos_ticks: u64,
    pub sample_length_samples: u64,
    pub density: u8,
    pub sections: Vec<Vec<LegacyPeak>>,
}

/// A framing-level section observed after the outer signature prefix.
///
/// The payload is intentionally opaque. This type does not claim that the
/// bytes are decoded spectral peaks; it only captures the proven tag/length
/// framing and preserves alignment padding for round-trip serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub tag: [u8; 4],
    pub payload: Vec<u8>,
    pub padding: Vec<u8>,
}

/// An opaque five-byte payload slice.
///
/// The name deliberately avoids calling this a spectral peak. The current
/// evidence establishes the width, not the field layout or semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpaqueRecord5(pub [u8; OBSERVED_RECORD_BYTES]);

/// One decoded record from `spectral_peak_compact_t`.
///
/// The Mac implementation uses an escape record to move the running time
/// cursor when a delta does not fit in one byte. That record is not itself a
/// spectral peak and is retained explicitly here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactRecord {
    Peak {
        time_ticks: u32,
        magnitude: u16,
        frequency_q6: u16,
    },
    AbsoluteTime {
        time_ticks: u32,
    },
}

impl CompactRecord {
    pub fn encode(self, previous_time_ticks: u32) -> [u8; OBSERVED_RECORD_BYTES] {
        match self {
            Self::Peak {
                time_ticks,
                magnitude,
                frequency_q6,
            } => {
                let delta = time_ticks
                    .checked_sub(previous_time_ticks)
                    .expect("compact peak time must be monotonic");
                assert!(delta < COMPACT_ESCAPE as u32);
                let mut bytes = [0; OBSERVED_RECORD_BYTES];
                bytes[0] = delta as u8;
                bytes[1..3].copy_from_slice(&magnitude.to_le_bytes());
                bytes[3..5].copy_from_slice(&frequency_q6.to_le_bytes());
                bytes
            }
            Self::AbsoluteTime { time_ticks } => {
                let mut bytes = [0; OBSERVED_RECORD_BYTES];
                bytes[0] = COMPACT_ESCAPE;
                bytes[1..5].copy_from_slice(&time_ticks.to_le_bytes());
                bytes
            }
        }
    }
}

impl Section {
    #[allow(unknown_lints, clippy::manual_is_multiple_of)]
    pub fn opaque_records5(&self) -> Result<Vec<OpaqueRecord5>, RecordParseError> {
        if self.payload.len() % OBSERVED_RECORD_BYTES != 0 {
            return Err(RecordParseError::LengthNotMultiple {
                tag: self.tag,
                length: self.payload.len(),
            });
        }
        Ok(self
            .payload
            .chunks_exact(OBSERVED_RECORD_BYTES)
            .map(|record| OpaqueRecord5(record.try_into().expect("chunk size is five")))
            .collect())
    }

    pub fn compact_records(&self) -> Result<Vec<CompactRecord>, CompactParseError> {
        decode_compact_records(&self.payload)
    }
}

#[allow(unknown_lints, clippy::manual_is_multiple_of)]
pub fn decode_compact_records(bytes: &[u8]) -> Result<Vec<CompactRecord>, CompactParseError> {
    if bytes.len() % OBSERVED_RECORD_BYTES != 0 {
        return Err(CompactParseError::LengthNotMultiple {
            length: bytes.len(),
        });
    }
    let mut previous_time_ticks = 0u32;
    let mut records = Vec::with_capacity(bytes.len() / OBSERVED_RECORD_BYTES);
    for chunk in bytes.chunks_exact(OBSERVED_RECORD_BYTES) {
        let record = if chunk[0] == COMPACT_ESCAPE {
            let time_ticks = u32::from_le_bytes(chunk[1..5].try_into().expect("four bytes"));
            CompactRecord::AbsoluteTime { time_ticks }
        } else {
            let delta = chunk[0] as u32;
            let time_ticks =
                previous_time_ticks
                    .checked_add(delta)
                    .ok_or(CompactParseError::TimeOverflow {
                        previous: previous_time_ticks,
                        delta: chunk[0],
                    })?;
            CompactRecord::Peak {
                time_ticks,
                magnitude: u16::from_le_bytes(chunk[1..3].try_into().expect("two bytes")),
                frequency_q6: u16::from_le_bytes(chunk[3..5].try_into().expect("two bytes")),
            }
        };
        previous_time_ticks = match record {
            CompactRecord::Peak { time_ticks, .. } | CompactRecord::AbsoluteTime { time_ticks } => {
                time_ticks
            }
        };
        records.push(record);
    }
    Ok(records)
}

pub fn encode_compact_records(records: &[CompactRecord]) -> Vec<u8> {
    let mut previous_time_ticks = 0u32;
    let mut bytes = Vec::with_capacity(records.len() * OBSERVED_RECORD_BYTES);
    for &record in records {
        bytes.extend_from_slice(&record.encode(previous_time_ticks));
        previous_time_ticks = match record {
            CompactRecord::Peak { time_ticks, .. } | CompactRecord::AbsoluteTime { time_ticks } => {
                time_ticks
            }
        };
    }
    bytes
}

/// Encode the five-byte records produced by the Mac `spectral_peak_compact_t`
/// conversion. A time gap greater than 254 ticks becomes an absolute-time
/// cursor escape followed by a zero-delta peak record. The Mac `.sig` bytes
/// confirm that the source peak is retained after the cursor is reset.
pub fn encode_legacy_compact_peaks(peaks: &[LegacyPeak]) -> Result<Vec<u8>, EncodeError> {
    let mut previous_time_ticks = 0u32;
    let mut bytes = Vec::with_capacity(peaks.len() * OBSERVED_RECORD_BYTES);
    for &peak in peaks {
        if peak.time_ticks < previous_time_ticks {
            return Err(EncodeError::NonMonotonicTime {
                previous: previous_time_ticks,
                current: peak.time_ticks,
            });
        }
        let delta = peak.time_ticks - previous_time_ticks;
        if delta < u8::MAX as u32 {
            bytes.push(delta as u8);
            bytes.extend_from_slice(&peak.magnitude.to_le_bytes());
            bytes.extend_from_slice(&peak.frequency_q6.to_le_bytes());
        } else {
            bytes.push(COMPACT_ESCAPE);
            bytes.extend_from_slice(&peak.time_ticks.to_le_bytes());
            bytes.push(0);
            bytes.extend_from_slice(&peak.magnitude.to_le_bytes());
            bytes.extend_from_slice(&peak.frequency_q6.to_le_bytes());
        }
        previous_time_ticks = peak.time_ticks;
    }
    Ok(bytes)
}

fn legacy_rate_marker_be(sample_rate: u32) -> Option<u32> {
    // This is the table at ShazamTagging __TEXT,__const+0x798d0, where the
    // rate codes 1..6 map to 8000, 16000, 32000, 44100, and 48000 Hz.
    match sample_rate {
        8_000 => Some(8),
        16_000 => Some(24),
        32_000 => Some(32),
        44_100 => Some(40),
        48_000 => Some(48),
        _ => None,
    }
}

/// Serialize the proven Mac `basic_signature_t` → `io::pack` legacy path.
///
/// This is intentionally a small, explicit serializer rather than a generic
/// “signature” claim. It reproduces the header fields, four-byte section
/// framing, compact peak records, padding, and CRC observed in the Mac
/// implementation. The DSP is responsible for choosing the peak sections;
/// this function only serializes the values it is given.
pub fn encode_legacy_signature(input: &LegacySignatureInput) -> Result<Vec<u8>, EncodeError> {
    let rate_marker_be = legacy_rate_marker_be(input.sample_rate)
        .ok_or(EncodeError::UnsupportedSampleRate(input.sample_rate))?;
    if input.stream_pos_ticks > 0xff_ff_ff_ff_ff {
        return Err(EncodeError::LengthOverflow);
    }
    if input.sample_length_samples > 0xffff_ffff_ffff {
        return Err(EncodeError::LengthOverflow);
    }

    let mut bytes = vec![0u8; SECTION_TABLE_OFFSET];
    bytes[0..4].copy_from_slice(&MAGIC);
    bytes[0x0c..0x10].copy_from_slice(&LEGACY_VERSION_WORD.to_le_bytes());
    bytes[0x1c..0x20].copy_from_slice(&rate_marker_be.to_be_bytes());
    bytes[0x20..0x24].copy_from_slice(&(input.stream_pos_ticks as u32).to_le_bytes());
    bytes[0x24] = (input.stream_pos_ticks >> 32) as u8;
    bytes[0x28..0x2c].copy_from_slice(&(input.sample_length_samples as u32).to_le_bytes());
    let sample_length_high = (input.sample_length_samples >> 32) as u32;
    let flags = (sample_length_high & 0x1f_fff) | ((input.density as u32) << 19) | 0x4_0000;
    bytes[0x2c..0x30].copy_from_slice(&flags.to_le_bytes());
    bytes[0x30..0x34].copy_from_slice(&LEGACY_OUTER_TAG_BE.to_be_bytes());

    for (section_index, peaks) in input.sections.iter().enumerate() {
        let payload = encode_legacy_compact_peaks(peaks)?;
        let tag = 0x40u8
            .checked_add(section_index as u8)
            .ok_or(EncodeError::LengthOverflow)?;
        bytes.extend_from_slice(&[
            tag,
            LEGACY_SECTION_TAG_SUFFIX[0],
            LEGACY_SECTION_TAG_SUFFIX[1],
            LEGACY_SECTION_TAG_SUFFIX[2],
        ]);
        let payload_length =
            u32::try_from(payload.len()).map_err(|_| EncodeError::LengthOverflow)?;
        bytes.extend_from_slice(&payload_length.to_le_bytes());
        bytes.extend_from_slice(&payload);
        let padding_len = align_up(bytes.len(), SECTION_ALIGNMENT) - bytes.len();
        bytes.resize(bytes.len() + padding_len, 0);
    }

    let outer_length =
        u32::try_from(bytes.len().saturating_sub(48)).map_err(|_| EncodeError::LengthOverflow)?;
    bytes[LENGTH_FIELD_OFFSET..LENGTH_FIELD_OFFSET + 4]
        .copy_from_slice(&outer_length.to_le_bytes());
    bytes[SECOND_LENGTH_FIELD_OFFSET..SECOND_LENGTH_FIELD_OFFSET + 4]
        .copy_from_slice(&outer_length.to_le_bytes());
    let crc = crc32_zlib(&bytes[8..]);
    bytes[4..8].copy_from_slice(&crc.to_le_bytes());
    Ok(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionTable {
    prefix: Vec<u8>,
    pub sections: Vec<Section>,
}

impl Signature {
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        if bytes.len() < SECOND_LENGTH_FIELD_OFFSET + 4 {
            return Err(ParseError::TooShort {
                actual: bytes.len(),
                minimum: SECOND_LENGTH_FIELD_OFFSET + 4,
            });
        }
        let magic = bytes[0..4].try_into().expect("checked length");
        if magic != MAGIC {
            return Err(ParseError::BadMagic(magic));
        }
        let declared = u32_le(bytes, LENGTH_FIELD_OFFSET);
        let actual = bytes.len().saturating_sub(48);
        if declared as usize != actual {
            return Err(ParseError::LengthMismatch { declared, actual });
        }
        let second = u32_le(bytes, SECOND_LENGTH_FIELD_OFFSET);
        if second != declared {
            return Err(ParseError::LengthMismatch {
                declared: second,
                actual: declared as usize,
            });
        }
        let declared_crc = u32_le(bytes, 4);
        let actual_crc = crc32_zlib(&bytes[8..]);
        if declared_crc != actual_crc {
            return Err(ParseError::CrcMismatch {
                declared: declared_crc,
                actual: actual_crc,
            });
        }
        Ok(Self {
            bytes: bytes.to_vec(),
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    pub fn payload_len(&self) -> u32 {
        u32_le(&self.bytes, LENGTH_FIELD_OFFSET)
    }
    pub fn crc32(&self) -> u32 {
        u32_le(&self.bytes, 4)
    }
    pub fn rate_marker_be(&self) -> u32 {
        u32_be(&self.bytes, 0x1c)
    }

    #[allow(unknown_lints, clippy::manual_is_multiple_of)]
    pub fn legacy_header(&self) -> LegacyHeader {
        let rate_marker_be = self.rate_marker_be();
        let rate_code = (rate_marker_be % 8 == 0)
            .then_some((rate_marker_be / 8) as u8)
            .filter(|code| *code <= 0x0f);
        let stream_pos_ticks_raw =
            u32_le(&self.bytes, 0x20) as u64 | ((self.bytes[0x24] as u64) << 32);
        let sample_length_samples = u32_le(&self.bytes, 0x28) as u64
            | ((u16::from_le_bytes([self.bytes[0x2c], self.bytes[0x2d]]) as u64) << 32);
        LegacyHeader {
            version_word: u32_le(&self.bytes, 0x0c),
            rate_marker_be,
            rate_code,
            stream_pos_ticks_raw,
            sample_length_samples,
            flags_word_raw: u32_le(&self.bytes, 0x2c),
            outer_tag_be: u32_be(&self.bytes, 0x30),
        }
    }

    pub fn section_table(&self) -> Result<SectionTable, SectionParseError> {
        SectionTable::parse(self)
    }
}

impl SectionTable {
    pub fn parse(signature: &Signature) -> Result<Self, SectionParseError> {
        let bytes = signature.as_bytes();
        let mut position = SECTION_TABLE_OFFSET;
        let mut sections = Vec::new();
        while position < bytes.len() {
            let remaining = bytes.len() - position;
            if remaining < 8 {
                return Err(SectionParseError::MissingSectionHeader {
                    offset: position,
                    remaining,
                });
            }
            let tag: [u8; 4] = bytes[position..position + 4]
                .try_into()
                .expect("checked section header length");
            if tag[1..] != [0x00, 0x03, 0x60] {
                return Err(SectionParseError::BadSectionTag {
                    offset: position,
                    tag,
                });
            }
            let declared = u32_le(bytes, position + 4);
            let payload_start = position + 8;
            let payload_end = payload_start.saturating_add(declared as usize);
            if payload_end > bytes.len() {
                return Err(SectionParseError::SectionOverrun {
                    offset: position,
                    declared,
                    available: bytes.len() - payload_start,
                });
            }
            let next = align_up(payload_end, SECTION_ALIGNMENT);
            if next > bytes.len() {
                return Err(SectionParseError::SectionOverrun {
                    offset: position,
                    declared,
                    available: bytes.len() - payload_start,
                });
            }
            sections.push(Section {
                tag,
                payload: bytes[payload_start..payload_end].to_vec(),
                padding: bytes[payload_end..next].to_vec(),
            });
            position = next;
        }
        Ok(Self {
            prefix: bytes[..SECTION_TABLE_OFFSET].to_vec(),
            sections,
        })
    }

    /// Rebuild the framing while preserving the opaque payload bytes.
    ///
    /// The proven outer length fields and CRC are recalculated. This is a
    /// framing serializer, not a spectral-peak encoder.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = self.prefix.clone();
        for section in &self.sections {
            bytes.extend_from_slice(&section.tag);
            bytes.extend_from_slice(&(section.payload.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&section.payload);
            let padding_len = align_up(bytes.len(), SECTION_ALIGNMENT) - bytes.len();
            if section.padding.len() == padding_len {
                bytes.extend_from_slice(&section.padding);
            } else {
                bytes.resize(bytes.len() + padding_len, 0);
            }
        }
        let outer_length = bytes.len().saturating_sub(48) as u32;
        bytes[LENGTH_FIELD_OFFSET..LENGTH_FIELD_OFFSET + 4]
            .copy_from_slice(&outer_length.to_le_bytes());
        bytes[SECOND_LENGTH_FIELD_OFFSET..SECOND_LENGTH_FIELD_OFFSET + 4]
            .copy_from_slice(&outer_length.to_le_bytes());
        let crc = crc32_zlib(&bytes[8..]);
        bytes[4..8].copy_from_slice(&crc.to_le_bytes());
        bytes
    }
}

fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

pub fn u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("field in bounds"),
    )
}

pub fn u32_be(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("field in bounds"),
    )
}

/// zlib/IEEE CRC-32, matching the oracle's `zlib.crc32(signature[8:])`.
pub fn crc32_zlib(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_minimal_signature() -> Vec<u8> {
        let mut bytes = vec![0u8; 56];
        bytes[0..4].copy_from_slice(&MAGIC);
        bytes[LENGTH_FIELD_OFFSET..LENGTH_FIELD_OFFSET + 4].copy_from_slice(&8u32.to_le_bytes());
        bytes[SECOND_LENGTH_FIELD_OFFSET..SECOND_LENGTH_FIELD_OFFSET + 4]
            .copy_from_slice(&8u32.to_le_bytes());
        let crc = crc32_zlib(&bytes[8..]);
        bytes[4..8].copy_from_slice(&crc.to_le_bytes());
        bytes
    }

    #[test]
    fn crc_matches_known_value() {
        assert_eq!(crc32_zlib(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn parse_rejects_short_and_bad_magic() {
        assert!(matches!(
            Signature::parse(&[0; 55]),
            Err(ParseError::TooShort { .. })
        ));
        assert!(matches!(
            Signature::parse(&[0; 56]),
            Err(ParseError::BadMagic(_))
        ));
        assert!(matches!(
            Signature::parse(&[0; 10]),
            Err(ParseError::TooShort { .. })
        ));
    }

    #[test]
    fn round_trip_preserves_unknown_bytes() {
        let mut bytes = vec![0u8; 56];
        bytes[0..4].copy_from_slice(&MAGIC);
        bytes[8..12].copy_from_slice(&(8u32).to_le_bytes());
        bytes[0x34..0x38].copy_from_slice(&(8u32).to_le_bytes());
        bytes[0x30] = 0x5a;
        let crc = crc32_zlib(&bytes[8..]);
        bytes[4..8].copy_from_slice(&crc.to_le_bytes());
        let parsed = Signature::parse(&bytes).unwrap();
        assert_eq!(parsed.as_bytes(), bytes.as_slice());
    }

    #[test]
    fn parse_rejects_corrupt_crc() {
        let mut bytes = valid_minimal_signature();
        bytes[0x30] ^= 1;

        assert!(matches!(
            Signature::parse(&bytes),
            Err(ParseError::CrcMismatch { .. })
        ));
    }

    #[test]
    fn parse_rejects_corrupt_primary_and_secondary_lengths() {
        let mut primary_length = valid_minimal_signature();
        primary_length[LENGTH_FIELD_OFFSET..LENGTH_FIELD_OFFSET + 4]
            .copy_from_slice(&7u32.to_le_bytes());
        assert_eq!(
            Signature::parse(&primary_length),
            Err(ParseError::LengthMismatch {
                declared: 7,
                actual: 8,
            })
        );

        let mut secondary_length = valid_minimal_signature();
        secondary_length[SECOND_LENGTH_FIELD_OFFSET..SECOND_LENGTH_FIELD_OFFSET + 4]
            .copy_from_slice(&7u32.to_le_bytes());
        assert_eq!(
            Signature::parse(&secondary_length),
            Err(ParseError::LengthMismatch {
                declared: 7,
                actual: 8,
            })
        );
    }

    #[test]
    fn section_table_round_trip_preserves_framing() {
        let mut bytes = vec![0u8; 0x4c];
        bytes[0..4].copy_from_slice(&MAGIC);
        bytes[0x30..0x34].copy_from_slice(&0x40u32.to_be_bytes());
        bytes[0x38..0x3c].copy_from_slice(&[0x40, 0x00, 0x03, 0x60]);
        bytes[0x3c..0x40].copy_from_slice(&4u32.to_le_bytes());
        bytes[0x40..0x44].copy_from_slice(&[1, 2, 3, 4]);
        bytes[0x44..0x48].copy_from_slice(&[0x41, 0x00, 0x03, 0x60]);
        bytes[0x48..0x4c].copy_from_slice(&0u32.to_le_bytes());
        let length = (bytes.len() - 48) as u32;
        bytes[8..12].copy_from_slice(&length.to_le_bytes());
        bytes[0x34..0x38].copy_from_slice(&length.to_le_bytes());
        let crc = crc32_zlib(&bytes[8..]);
        bytes[4..8].copy_from_slice(&crc.to_le_bytes());
        let signature = Signature::parse(&bytes).unwrap();
        let table = signature.section_table().unwrap();
        assert_eq!(table.sections.len(), 2);
        assert_eq!(table.sections[0].payload, [1, 2, 3, 4]);
        assert_eq!(table.encode(), bytes);
    }

    #[test]
    fn legacy_header_combines_packed_time_and_sample_fields() {
        let mut bytes = vec![0u8; 56];
        bytes[0..4].copy_from_slice(&MAGIC);
        bytes[0x0c..0x10].copy_from_slice(&LEGACY_VERSION_WORD.to_le_bytes());
        bytes[0x1c..0x20].copy_from_slice(&24u32.to_be_bytes());
        bytes[0x20..0x24].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        bytes[0x24] = 0x9a;
        bytes[0x28..0x2c].copy_from_slice(&0x8765_4321u32.to_le_bytes());
        bytes[0x2c..0x2e].copy_from_slice(&0x1234u16.to_le_bytes());
        bytes[0x30..0x34].copy_from_slice(&LEGACY_OUTER_TAG_BE.to_be_bytes());
        let length = (bytes.len() - 48) as u32;
        bytes[8..12].copy_from_slice(&length.to_le_bytes());
        bytes[0x34..0x38].copy_from_slice(&length.to_le_bytes());
        let crc = crc32_zlib(&bytes[8..]);
        bytes[4..8].copy_from_slice(&crc.to_le_bytes());
        let header = Signature::parse(&bytes).unwrap().legacy_header();
        assert_eq!(header.version_word, LEGACY_VERSION_WORD);
        assert_eq!(header.rate_code, Some(3));
        assert_eq!(header.stream_pos_ticks_raw, 0x009a_1234_5678);
        assert_eq!(header.sample_length_samples, 0x1234_8765_4321);
        assert_eq!(header.outer_tag_be, LEGACY_OUTER_TAG_BE);
    }

    #[test]
    fn compact_records_round_trip_delta_and_absolute_time() {
        let bytes = [
            0x10, 0x34, 0x12, 0x80, 0x01, // delta peak
            0xff, 0x2c, 0x01, 0x00, 0x00, // absolute time cursor reset
            0x02, 0x78, 0x56, 0x40, 0x02, // delta peak after reset
        ];
        let records = decode_compact_records(&bytes).unwrap();
        assert_eq!(
            records,
            vec![
                CompactRecord::Peak {
                    time_ticks: 0x10,
                    magnitude: 0x1234,
                    frequency_q6: 0x0180,
                },
                CompactRecord::AbsoluteTime { time_ticks: 0x12c },
                CompactRecord::Peak {
                    time_ticks: 0x12e,
                    magnitude: 0x5678,
                    frequency_q6: 0x0240,
                },
            ]
        );
        assert_eq!(encode_compact_records(&records), bytes);
    }

    #[test]
    fn legacy_packer_rebuilds_native_compact_sections() {
        let input = LegacySignatureInput {
            sample_rate: 16_000,
            stream_pos_ticks: 0,
            sample_length_samples: 51_840,
            density: LEGACY_DEFAULT_DENSITY,
            sections: vec![
                vec![
                    LegacyPeak {
                        time_ticks: 84,
                        magnitude: 35_129,
                        frequency_q6: 3_605,
                    },
                    LegacyPeak {
                        time_ticks: 209,
                        magnitude: 35_129,
                        frequency_q6: 3_605,
                    },
                ],
                vec![LegacyPeak {
                    time_ticks: 71,
                    magnitude: 32_994,
                    frequency_q6: 8_167,
                }],
                Vec::new(),
                Vec::new(),
            ],
        };
        let bytes = encode_legacy_signature(&input).unwrap();
        let signature = Signature::parse(&bytes).unwrap();
        assert_eq!(signature.legacy_header().rate_marker_be, 24);
        assert_eq!(signature.legacy_header().sample_length_samples, 51_840);
        let table = signature.section_table().unwrap();
        assert_eq!(table.sections.len(), 4);
        assert_eq!(table.sections[0].tag, [0x40, 0, 3, 0x60]);
        assert_eq!(
            table.sections[0].payload,
            [84, 0x39, 0x89, 0x15, 0x0e, 125, 0x39, 0x89, 0x15, 0x0e,]
        );
        assert_eq!(table.sections[1].tag, [0x41, 0, 3, 0x60]);
        assert_eq!(table.sections[1].payload, [71, 0xe2, 0x80, 0xe7, 0x1f]);
        assert!(table.sections[2].payload.is_empty());
        assert!(table.sections[3].payload.is_empty());
    }

    #[test]
    fn legacy_packer_uses_absolute_time_escape_for_large_delta() {
        let peaks = [
            LegacyPeak {
                time_ticks: 10,
                magnitude: 1,
                frequency_q6: 2,
            },
            LegacyPeak {
                time_ticks: 300,
                magnitude: 3,
                frequency_q6: 4,
            },
        ];
        assert_eq!(
            encode_legacy_compact_peaks(&peaks).unwrap(),
            [
                10, 1, 0, 2, 0, // normal peak
                0xff, 0x2c, 1, 0, 0, // absolute-time cursor reset
                0, 3, 0, 4, 0, // source peak at the reset time
            ]
        );
        assert_eq!(
            decode_compact_records(&encode_legacy_compact_peaks(&peaks).unwrap()).unwrap(),
            vec![
                CompactRecord::Peak {
                    time_ticks: 10,
                    magnitude: 1,
                    frequency_q6: 2,
                },
                CompactRecord::AbsoluteTime { time_ticks: 300 },
                CompactRecord::Peak {
                    time_ticks: 300,
                    magnitude: 3,
                    frequency_q6: 4,
                },
            ]
        );
    }
}
