use openshazam_update::{
    create_manifest, embedded_public_key, public_key_hex, sign_manifest, signing_key_from_base64,
    signing_key_matches_public_key, verify_installer_bytes, verify_signed_manifest,
};
use std::{env, fs, path::Path};
use zeroize::Zeroizing;

fn main() {
    if let Err(error) = run() {
        eprintln!("update tool: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("public-key") => {
            let seed_path = args
                .next()
                .ok_or("usage: openshazam-update-tool public-key <seed-file>")?;
            reject_extra(args)?;
            let encoded = Zeroizing::new(fs::read_to_string(seed_path)?);
            let key = signing_key_from_base64(&encoded)?;
            println!("{}", public_key_hex(&key));
        }
        Some("manifest") => {
            let installer_path = args.next().ok_or("usage: openshazam-update-tool manifest <installer> <version> <minimum-version> <repository> <output-manifest> <output-signature>")?;
            let version = args.next().ok_or("missing release version")?;
            let minimum_version = args.next().ok_or("missing minimum supported version")?;
            let repository = args.next().ok_or("missing GitHub repository owner/name")?;
            let manifest_path = args.next().ok_or("missing output manifest path")?;
            let signature_path = args.next().ok_or("missing output signature path")?;
            reject_extra(args)?;
            let encoded_key = Zeroizing::new(env::var("OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64")?);
            let signing_key = signing_key_from_base64(&encoded_key)?;
            let public_key = embedded_public_key()?;
            if !signing_key_matches_public_key(&signing_key, &public_key) {
                return Err("signing key does not match OPEN_SHAZAM_UPDATE_PUBLIC_KEY_HEX".into());
            }
            let installer = fs::read(&installer_path)?;
            let manifest = create_manifest(&installer, &version, &minimum_version, &repository)?;
            let signature = sign_manifest(&manifest, &signing_key)?;
            let bytes = manifest.signed_bytes()?;
            fs::write(&manifest_path, &bytes)?;
            fs::write(&signature_path, format!("{signature}\n"))?;
            verify_signed_manifest(&bytes, &signature, &public_key)?;
            verify_installer_bytes(&manifest, &installer)?;
            println!(
                "Signed and verified {} ({} bytes, SHA-256 {})",
                manifest.installer_name, manifest.installer_size_bytes, manifest.installer_sha256
            );
        }
        Some("verify") => {
            let manifest_path = args
                .next()
                .ok_or("usage: openshazam-update-tool verify <manifest> <signature> <installer>")?;
            let signature_path = args.next().ok_or("missing signature path")?;
            let installer_path = args.next().ok_or("missing installer path")?;
            reject_extra(args)?;
            let manifest_bytes = fs::read(&manifest_path)?;
            let signature = fs::read_to_string(&signature_path)?;
            let installer = fs::read(&installer_path)?;
            let public_key = embedded_public_key()?;
            let manifest = verify_signed_manifest(&manifest_bytes, &signature, &public_key)?;
            verify_installer_bytes(&manifest, &installer)?;
            println!("Verified {}", Path::new(&installer_path).display());
        }
        _ => return Err("usage: openshazam-update-tool <public-key|manifest|verify> ...".into()),
    }
    Ok(())
}

fn reject_extra(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    if args.next().is_some() {
        return Err("unexpected extra arguments".into());
    }
    Ok(())
}
