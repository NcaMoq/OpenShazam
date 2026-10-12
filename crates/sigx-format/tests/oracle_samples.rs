use sigx_format::{
    decode_compact_records, encode_compact_records, encode_legacy_compact_peaks,
    encode_legacy_signature, CompactRecord, LegacyPeak, LegacySignatureInput, Signature,
    LEGACY_DEFAULT_DENSITY,
};

fn synthetic_signature() -> Vec<u8> {
    let sections = (0..4)
        .map(|band| {
            vec![
                LegacyPeak {
                    time_ticks: 0,
                    magnitude: 0x8939 + band,
                    frequency_q6: 0x0e15 + band,
                },
                LegacyPeak {
                    time_ticks: 125,
                    magnitude: 0x7234 + band,
                    frequency_q6: 0x1240 + band,
                },
                LegacyPeak {
                    time_ticks: 510,
                    magnitude: 0x6012 + band,
                    frequency_q6: 0x1a20 + band,
                },
            ]
        })
        .collect();

    encode_legacy_signature(&LegacySignatureInput {
        sample_rate: 16_000,
        stream_pos_ticks: 0,
        sample_length_samples: 51_840,
        density: LEGACY_DEFAULT_DENSITY,
        sections,
    })
    .expect("synthetic test Signature must encode")
}

#[test]
fn parses_and_round_trips_synthetic_signature_sections() {
    let bytes = synthetic_signature();
    let signature = Signature::parse(&bytes).expect("synthetic Signature must parse");
    assert_eq!(signature.as_bytes(), bytes);
    assert_eq!(signature.rate_marker_be(), 24);
    assert_eq!(signature.legacy_header().sample_length_samples, 51_840);

    let table = signature.section_table().expect("sections must parse");
    assert_eq!(table.sections.len(), 4);
    assert!(table
        .sections
        .iter()
        .all(|section| section.opaque_records5().is_ok()));
    assert_eq!(table.encode(), bytes);
}

#[test]
fn synthetic_legacy_packer_rebuilds_decoded_peak_records() {
    let bytes = synthetic_signature();
    let signature = Signature::parse(&bytes).unwrap();
    let table = signature.section_table().unwrap();
    let sections = table
        .sections
        .iter()
        .map(|section| {
            section
                .compact_records()
                .unwrap()
                .into_iter()
                .filter_map(|record| match record {
                    CompactRecord::Peak {
                        time_ticks,
                        magnitude,
                        frequency_q6,
                    } => Some(LegacyPeak {
                        time_ticks,
                        magnitude,
                        frequency_q6,
                    }),
                    CompactRecord::AbsoluteTime { .. } => None,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let header = signature.legacy_header();
    let rebuilt = encode_legacy_signature(&LegacySignatureInput {
        sample_rate: 16_000,
        stream_pos_ticks: header.stream_pos_ticks_raw,
        sample_length_samples: header.sample_length_samples,
        density: ((header.flags_word_raw >> 19) & 0xff) as u8,
        sections,
    })
    .unwrap();

    assert_eq!(rebuilt, bytes);
}

#[test]
fn compact_records_round_trip_synthetic_peaks_and_time_escape() {
    let peaks = [
        LegacyPeak {
            time_ticks: 0,
            magnitude: 0x1111,
            frequency_q6: 0x2222,
        },
        LegacyPeak {
            time_ticks: 254,
            magnitude: 0x3333,
            frequency_q6: 0x4444,
        },
        LegacyPeak {
            time_ticks: 509,
            magnitude: 0x5555,
            frequency_q6: 0x6666,
        },
    ];
    let encoded = encode_legacy_compact_peaks(&peaks).unwrap();
    let decoded = decode_compact_records(&encoded).unwrap();
    assert_eq!(decoded.len(), 4);
    assert_eq!(decoded[2], CompactRecord::AbsoluteTime { time_ticks: 509 });
    assert_eq!(encode_compact_records(&decoded), encoded);
}

#[test]
fn legacy_compact_escape_boundary_is_254_then_255() {
    let peaks = [
        LegacyPeak {
            time_ticks: 0,
            magnitude: 0x1111,
            frequency_q6: 0x2222,
        },
        LegacyPeak {
            time_ticks: 254,
            magnitude: 0x3333,
            frequency_q6: 0x4444,
        },
        LegacyPeak {
            time_ticks: 509,
            magnitude: 0x5555,
            frequency_q6: 0x6666,
        },
        LegacyPeak {
            time_ticks: 765,
            magnitude: 0x7777,
            frequency_q6: 0x8888,
        },
    ];
    let encoded = encode_legacy_compact_peaks(&peaks).unwrap();
    assert_eq!(encoded[0], 0);
    assert_eq!(encoded[5], 254);
    assert_eq!(encoded[10], 0xff);
    assert_eq!(encoded[15], 0);
    assert_eq!(encoded[20], 0xff);
    assert_eq!(encoded[25], 0);

    let decoded = decode_compact_records(&encoded).unwrap();
    assert_eq!(decoded.len(), 6);
    assert!(matches!(
        decoded[1],
        CompactRecord::Peak {
            time_ticks: 254,
            ..
        }
    ));
    assert_eq!(decoded[2], CompactRecord::AbsoluteTime { time_ticks: 509 });
    assert!(matches!(
        decoded[3],
        CompactRecord::Peak {
            time_ticks: 509,
            ..
        }
    ));
    assert_eq!(decoded[4], CompactRecord::AbsoluteTime { time_ticks: 765 });
    assert!(matches!(
        decoded[5],
        CompactRecord::Peak {
            time_ticks: 765,
            ..
        }
    ));
    assert_eq!(encode_compact_records(&decoded), encoded);
}
