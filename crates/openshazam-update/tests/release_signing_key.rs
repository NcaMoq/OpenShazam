use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::{
    env,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn manifest_command_rejects_a_mismatched_release_key_without_echoing_it() {
    let encoded_wrong_key = STANDARD.encode([7_u8; 32]);
    let unique_suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after UNIX epoch")
        .as_nanos();
    let output_manifest = env::temp_dir().join(format!(
        "openshazam-key-mismatch-{}-{unique_suffix}.json",
        std::process::id()
    ));
    let output_signature = PathBuf::from(format!("{}.sig", output_manifest.display()));

    let output = Command::new(env!("CARGO_BIN_EXE_openshazam-update-tool"))
        .arg("manifest")
        .arg("installer-that-must-not-be-read.exe")
        .args(["0.1.2", "0.1.0", "NcaMoq/OpenShazam"])
        .arg(&output_manifest)
        .arg(&output_signature)
        .env("OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64", &encoded_wrong_key)
        .output()
        .expect("update manifest tool must start");

    assert!(!output.status.success());
    let mut captured_output = output.stdout;
    captured_output.extend_from_slice(&output.stderr);
    let captured_output = String::from_utf8_lossy(&captured_output);
    assert!(captured_output.contains("signing key does not match"));
    assert!(!captured_output.contains(&encoded_wrong_key));
    assert!(!output_manifest.exists());
    assert!(!output_signature.exists());
}
