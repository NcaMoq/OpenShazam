fn main() {
    println!("cargo:rerun-if-env-changed=OPEN_SHAZAM_UPDATE_PUBLIC_KEY_HEX");
}
