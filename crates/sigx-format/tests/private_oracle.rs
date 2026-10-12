use sigx_format::{
    encode_legacy_signature, CompactRecord, LegacyPeak, LegacySignatureInput, Signature,
};
use std::{fs, path::PathBuf};

fn research_root() -> PathBuf {
    PathBuf::from(
        std::env::var_os("SHZ_PRIVATE_VECTOR_ROOT")
            .expect("set SHZ_PRIVATE_VECTOR_ROOT to the authorized research root"),
    )
}

fn private_bytes(relative_path: &str) -> Vec<u8> {
    fs::read(research_root().join(relative_path))
        .unwrap_or_else(|error| panic!("cannot read private fixture {relative_path}: {error}"))
}

fn rebuild_signature(bytes: &[u8], path: &str) {
    let signature = Signature::parse(bytes).unwrap_or_else(|error| panic!("{path}: {error}"));
    let table = signature
        .section_table()
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    assert_eq!(table.sections.len(), 4, "{path}");
    let sections = table
        .sections
        .iter()
        .map(|section| {
            section
                .compact_records()
                .unwrap_or_else(|error| panic!("{path}: {error}"))
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
    let sample_rate = match header.rate_marker_be {
        8 => 8_000,
        24 => 16_000,
        32 => 32_000,
        40 => 44_100,
        48 => 48_000,
        marker => panic!("{path}: unregistered rate marker {marker}"),
    };
    let rebuilt = encode_legacy_signature(&LegacySignatureInput {
        sample_rate,
        stream_pos_ticks: header.stream_pos_ticks_raw,
        sample_length_samples: header.sample_length_samples,
        density: ((header.flags_word_raw >> 19) & 0xff) as u8,
        sections,
    })
    .unwrap_or_else(|error| panic!("{path}: {error}"));
    assert_eq!(rebuilt, bytes, "{path}");
}

#[test]
#[ignore = "requires authorized private Native Signature fixtures"]
fn parses_saved_mac_oracle_int16_signature() {
    let bytes = private_bytes("vectors/oracle_outputs/int16_16000Hz_3s_run1.bin");
    let signature = Signature::parse(&bytes).expect("saved oracle output must parse");
    assert_eq!(signature.len(), 120);
    assert_eq!(signature.payload_len(), 72);
    assert_eq!(signature.rate_marker_be(), 24);
}

#[test]
#[ignore = "requires authorized private Native Signature fixtures"]
fn parses_legacy_header_fields_from_saved_oracle() {
    let bytes = private_bytes("vectors/oracle_outputs/int16_16000Hz_3s_run1.bin");
    let signature = Signature::parse(&bytes).unwrap();
    let header = signature.legacy_header();
    assert_eq!(header.version_word, 0x9400_1c00);
    assert_eq!(header.rate_marker_be, 24);
    assert_eq!(header.rate_code, Some(3));
    assert_eq!(header.stream_pos_ticks_raw, 0);
    assert_eq!(header.sample_length_samples, 51_840);
    assert_eq!(header.flags_word_raw, 0x007c_0000);
    assert_eq!(header.outer_tag_be, 0x40);
}

#[test]
#[ignore = "requires authorized private Native Signature fixtures"]
fn parses_saved_progressive_signature_without_assuming_same_length() {
    let bytes = private_bytes("vectors/oracle_outputs/progressive_int16_16000Hz_at3s.bin");
    let signature = Signature::parse(&bytes).expect("saved progressive output must parse");
    assert_eq!(signature.len(), 728);
    assert_ne!(signature.len(), 120);
}

#[test]
#[ignore = "requires authorized private Native Signature fixtures"]
fn parses_and_round_trips_observed_section_framing() {
    let samples = [
        "vectors/oracle_outputs/int16_16000Hz_3s_run1.bin",
        "vectors/oracle_outputs/float_44100Hz_3s_run1.bin",
        "vectors/oracle_outputs/progressive_int16_16000Hz_at3s.bin",
        "vectors/edge/int16_noise_16000Hz_48000s_run1.bin",
    ];
    for path in samples {
        let bytes = private_bytes(path);
        let signature = Signature::parse(&bytes).unwrap();
        let table = signature.section_table().unwrap();
        assert_eq!(table.sections.len(), 4, "{path}");
        assert_eq!(table.sections[0].tag[1..], [0x00, 0x03, 0x60]);
        assert!(
            table
                .sections
                .iter()
                .all(|section| section.opaque_records5().is_ok()),
            "{path}"
        );
        assert_eq!(table.encode(), bytes, "{path}");
    }
}

#[test]
#[ignore = "requires authorized private Native Signature fixtures"]
fn decodes_and_reencodes_saved_compact_payloads() {
    let bytes = private_bytes("vectors/oracle_outputs/int16_16000Hz_3s_run1.bin");
    let signature = Signature::parse(&bytes).unwrap();
    let table = signature.section_table().unwrap();
    let records = table.sections[0].compact_records().unwrap();
    assert_eq!(records.len(), 3);
    assert!(matches!(
        records[0],
        CompactRecord::Peak {
            time_ticks: 0x54,
            magnitude: 0x8939,
            frequency_q6: 0x0e15
        }
    ));
    assert_eq!(
        sigx_format::encode_compact_records(&records),
        table.sections[0].payload
    );

    let escaped = private_bytes("vectors/edge/int16_chirp_16000Hz_47999s_run1.bin");
    let escaped = Signature::parse(&escaped).unwrap();
    let escaped_table = escaped.section_table().unwrap();
    let escaped_records = escaped_table.sections[3].compact_records().unwrap();
    assert!(escaped_records
        .iter()
        .any(|record| matches!(record, CompactRecord::AbsoluteTime { .. })));
    assert_eq!(
        sigx_format::encode_compact_records(&escaped_records),
        escaped_table.sections[3].payload
    );
}

#[test]
#[ignore = "requires authorized private Native Signature fixtures"]
fn legacy_packer_rebuilds_saved_mac_signatures() {
    let paths = [
        "vectors/oracle_outputs/int16_16000Hz_3s_run1.bin",
        "vectors/int16_contract/oracle/sine440_peak_64/process1/int16_16000Hz_3s_run1.bin",
    ];
    for path in paths {
        let bytes = private_bytes(path);
        rebuild_signature(&bytes, path);
    }
}

#[test]
#[ignore = "requires authorized private Native Signature fixtures"]
fn legacy_packer_rebuilds_all_registered_mac_vectors() {
    let root = research_root();
    let manifest = fs::read_to_string(root.join("vectors/manifest.jsonl"))
        .expect("private vector manifest must be readable");
    let output_paths: Vec<_> = manifest
        .lines()
        .filter_map(|line| {
            let key = "\"output_path\": \"";
            let start = line.find(key)? + key.len();
            let end = line[start..].find('"')? + start;
            Some(line[start..end].to_owned())
        })
        .collect();
    assert_eq!(
        output_paths.len(),
        108,
        "expected the registered private corpus"
    );

    for path in output_paths {
        let bytes = fs::read(root.join(&path)).unwrap_or_else(|error| panic!("{path}: {error}"));
        rebuild_signature(&bytes, &path);
    }
}
