use std::{
    env, fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::{Command, ExitCode},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(any(test, not(windows)))]
use std::path::Path;

use serde_json::{json, Value};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        usage();
        return ExitCode::from(2);
    };
    if command == "pcm-mock" {
        return run_pcm_mock(args.collect());
    }
    if command == "audio-mock" {
        return run_audio(args.collect(), false);
    }
    if command == "audio-live" {
        return run_audio(args.collect(), true);
    }
    if command == "signature-live" {
        return run_signature_live(args.collect());
    }
    if command == "config-check" {
        if args.next().is_some() {
            eprintln!("usage: shazam-http-lab config-check");
            return ExitCode::from(2);
        }
        return run_config_check();
    }
    if command != "mock-roundtrip" {
        usage();
        return ExitCode::from(2);
    }
    let Some(signature_path) = args.next() else {
        usage();
        return ExitCode::from(2);
    };
    let sample_ms = match args.next().and_then(|value| value.parse::<u64>().ok()) {
        Some(value) => value,
        None => {
            eprintln!("sample duration must be an integer number of milliseconds");
            return ExitCode::from(2);
        }
    };
    if args.next().is_some() {
        usage();
        return ExitCode::from(2);
    }

    let signature = match fs::read(&signature_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read signature {signature_path}: {error}");
            return ExitCode::from(2);
        }
    };
    mock_signature(&signature, sample_ms)
}

fn run_pcm_mock(args: Vec<String>) -> ExitCode {
    if args.len() != 2 {
        eprintln!("usage: shazam-http-lab pcm-mock <mono-i16le.pcm> <sample-rate-hz>");
        return ExitCode::from(2);
    }
    let sample_rate = match args[1].parse::<u32>() {
        Ok(rate) => rate,
        Err(_) => {
            eprintln!("sample rate must be an integer in Hz");
            return ExitCode::from(2);
        }
    };
    let bytes = match fs::read(&args[0]) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read PCM {}: {error}", args[0]);
            return ExitCode::from(2);
        }
    };
    if bytes.len() % 2 != 0 {
        eprintln!("PCM input must contain an even number of bytes (Int16 LE)");
        return ExitCode::from(2);
    }
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    if samples.is_empty() {
        eprintln!("PCM input must contain at least one sample");
        return ExitCode::from(2);
    }
    run_samples(samples, sample_rate, false)
}

fn run_audio(args: Vec<String>, live: bool) -> ExitCode {
    if !(1..=2).contains(&args.len()) {
        eprintln!("usage: shazam-http-lab audio-mock <audio-file> [seconds]");
        return ExitCode::from(2);
    }
    let duration_seconds = match args
        .get(1)
        .map(String::as_str)
        .unwrap_or("12")
        .parse::<u32>()
    {
        Ok(seconds @ 1..=30) => seconds,
        _ => {
            eprintln!("duration must be an integer from 1 to 30 seconds");
            return ExitCode::from(2);
        }
    };
    let input_path = &args[0];
    let probe = match Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=sample_rate",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(input_path)
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(_) => {
            eprintln!("ffprobe could not read an audio stream from the input");
            return ExitCode::from(1);
        }
        Err(error) => {
            eprintln!("could not start ffprobe: {error}");
            return ExitCode::from(1);
        }
    };
    let rate_text = String::from_utf8_lossy(&probe.stdout);
    let sample_rate = match rate_text.trim().parse::<u32>() {
        Ok(rate) => rate,
        Err(_) => {
            eprintln!("ffprobe returned an unsupported sample-rate value");
            return ExitCode::from(1);
        }
    };
    let decoded = match Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(input_path)
        .args([
            "-map",
            "0:a:0",
            "-t",
            &duration_seconds.to_string(),
            "-ac",
            "1",
            "-ar",
        ])
        .arg(sample_rate.to_string())
        .args(["-f", "s16le", "-acodec", "pcm_s16le", "pipe:1"])
        .output()
    {
        Ok(output) if output.status.success() => output.stdout,
        Ok(_) => {
            eprintln!("ffmpeg failed to decode the requested audio segment");
            return ExitCode::from(1);
        }
        Err(error) => {
            eprintln!("could not start ffmpeg: {error}");
            return ExitCode::from(1);
        }
    };
    if decoded.len() % 2 != 0 {
        eprintln!("ffmpeg returned a partial Int16 sample");
        return ExitCode::from(1);
    }
    let samples: Vec<i16> = decoded
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    if samples.is_empty() {
        eprintln!("decoded audio segment contains no samples");
        return ExitCode::from(1);
    }
    eprintln!(
        "Decoded {duration_seconds}s to mono Int16 at the source rate ({sample_rate} Hz); DSP remains Pure Rust."
    );
    run_samples(samples, sample_rate, live)
}

fn run_samples(samples: Vec<i16>, sample_rate: u32, live: bool) -> ExitCode {
    let mut sigx = match sigx_core::SigX::new(sigx_core::Config {
        sample_rate,
        channels: 1,
        signature_type: 2,
        signature_options: 0,
        max_frames: samples.len().max(1),
    }) {
        Ok(sigx) => sigx,
        Err(error) => {
            eprintln!("initialize Pure Rust SigX: {error:?}");
            return ExitCode::from(1);
        }
    };
    if let Err(error) = sigx.push_i16(&samples) {
        eprintln!("process Int16 PCM: {error:?}");
        return ExitCode::from(1);
    }
    let signature = match sigx.signature_with_flush_frames(61) {
        Ok(signature) => signature,
        Err(error) => {
            eprintln!("pack Pure Rust Signature: {error:?}");
            return ExitCode::from(1);
        }
    };
    let parsed = match sigx_format::Signature::parse(&signature) {
        Ok(signature) => signature,
        Err(error) => {
            eprintln!("Pure Rust Signature failed validation: {error}");
            return ExitCode::from(1);
        }
    };
    let sample_ms = (samples.len() as u128 * 1_000 / u128::from(sample_rate)) as u64;
    println!(
        "Pure Rust Signature ready: bytes={} payload_len={} crc32={:#010x} duration_ms={sample_ms}",
        parsed.len(),
        parsed.payload_len(),
        parsed.crc32()
    );
    if live {
        live_signature(&signature, sample_ms)
    } else {
        mock_signature(&signature, sample_ms)
    }
}

fn run_signature_live(args: Vec<String>) -> ExitCode {
    if args.len() != 2 {
        eprintln!("usage: shazam-http-lab signature-live <signature.bin> <sample-duration-ms>");
        return ExitCode::from(2);
    }
    let sample_ms = match args[1].parse::<u64>() {
        Ok(value) if value > 0 => value,
        _ => {
            eprintln!("sample duration must be a positive integer number of milliseconds");
            return ExitCode::from(2);
        }
    };
    let signature = match fs::read(&args[0]) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read signature {}: {error}", args[0]);
            return ExitCode::from(2);
        }
    };
    live_signature(&signature, sample_ms)
}

fn live_signature(signature: &[u8], sample_ms: u64) -> ExitCode {
    let timestamp_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(error) => {
            eprintln!("system clock is before Unix epoch: {error}");
            return ExitCode::from(1);
        }
    };
    let timezone = local_timezone_name();
    let body = match shazam_http_lab::build_request_body(
        signature,
        sample_ms,
        timestamp_ms,
        &timezone,
        json!({}),
        json!({}),
        None,
    ) {
        Ok(body) => body,
        Err(error) => {
            eprintln!("build Shazam request: {error}");
            return ExitCode::from(1);
        }
    };
    let endpoint = match live_endpoint_from_environment() {
        Ok(endpoint) => endpoint,
        Err(message) => {
            eprintln!("cannot prepare live request: {message}");
            return ExitCode::from(2);
        }
    };

    match shazam_http_lab::post_shazam_json(&endpoint, &body) {
        Ok(response) => {
            eprintln!(
                "LIVE POST completed to allowlisted amp.shazam.com route; IDs are redacted and no AMP token was sent."
            );
            println!("Shazam HTTP status={}", response.status);
            if !(200..300).contains(&response.status) {
                eprintln!("recognition did not succeed; response body withheld from logs");
                return ExitCode::from(1);
            }
            match shazam_http_lab::parse_track_candidates(&response.body) {
                Ok(candidates) => {
                    for (index, candidate) in candidates.iter().enumerate() {
                        println!(
                            "match[{}] title={:?} artist={:?}",
                            index + 1,
                            candidate.title,
                            candidate.artist
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!(
                        "HTTP response had no usable track result: {error}; {}",
                        shazam_http_lab::summarize_response_shape(&response.body)
                    );
                    ExitCode::from(1)
                }
            }
        }
        Err(error) => {
            eprintln!("Shazam request failed: {error}");
            ExitCode::from(1)
        }
    }
}

fn live_endpoint_from_environment() -> Result<String, String> {
    let streaming_provider = env::var("SHAZAM_STREAMING_PROVIDER").unwrap_or_default();
    let tag_id = random_uuid_v4()
        .map_err(|error| format!("could not generate a request tag ID: {error}"))?;
    let template = env::var("SHAZAM_TAG_URL_TEMPLATE").ok();
    let installation_id = env::var("SHAZAM_INID").ok();
    match (template, installation_id) {
        (Some(template), Some(installation_id)) => shazam_http_lab::resolve_shazam_tag_url(
            &template,
            &installation_id,
            &tag_id,
            &streaming_provider,
        )
        .map_err(|error| format!("manual Shazam route settings were invalid: {error}")),
        (None, None) => {
            let profile = config_request_profile_from_host()?;
            let configuration = shazam_http_lab::fetch_shazam_config(&profile)
                .map_err(|error| format!("could not fetch or parse Shazam Config: {error}"))?;
            configuration
                .resolve_tag_url(&tag_id, &streaming_provider)
                .map_err(|error| format!("Config returned an unsupported Mac tag route: {error}"))
        }
        _ => Err("set both manual route values or unset both for automatic Config".into()),
    }
}

fn run_config_check() -> ExitCode {
    let profile = match config_request_profile_from_host() {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("cannot identify this host for Config: {error}");
            return ExitCode::from(1);
        }
    };
    match shazam_http_lab::fetch_shazam_config(&profile) {
        Ok(configuration) => {
            if let Err(error) = configuration.resolve_tag_url("config-check", "") {
                eprintln!("Config was fetched but its tag route could not be resolved: {error}");
                return ExitCode::from(1);
            }
            println!(
                "Config loaded over verified HTTPS; route resolved in memory, INID present (redacted); no Shazam app cache was read or written."
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("could not fetch Shazam Config: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(target_os = "macos")]
fn config_request_profile_from_host() -> Result<shazam_http_lab::ConfigRequestProfile, String> {
    let language = env::var("SHAZAM_CONFIG_LANGUAGE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(mac_preferred_language)
        .unwrap_or_else(|| "en".into());
    let locale = env::var("SHAZAM_CONFIG_LOCALE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(mac_locale_identifier)
        .unwrap_or_else(|| "en_US".into());
    let model = env::var("SHAZAM_CONFIG_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| command_value("sysctl", &["-n", "hw.model"]))
        .ok_or_else(|| {
            "could not read the Mac model (set SHAZAM_CONFIG_MODEL to override)".to_owned()
        })?;
    let os_version = env::var("SHAZAM_CONFIG_OS_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| command_value("sw_vers", &["-productVersion"]))
        .ok_or_else(|| {
            "could not read the macOS version (set SHAZAM_CONFIG_OS_VERSION to override)".to_owned()
        })?;
    let os_build = env::var("SHAZAM_CONFIG_OS_BUILD")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| command_value("sw_vers", &["-buildVersion"]))
        .ok_or_else(|| {
            "could not read the macOS build (set SHAZAM_CONFIG_OS_BUILD to override)".to_owned()
        })?;
    let country = env::var("SHAZAM_CONFIG_COUNTRY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "-".into());

    Ok(shazam_http_lab::ConfigRequestProfile {
        country,
        language,
        locale,
        model,
        os_version,
        os_build,
        app_version: "2.11.0".into(),
    })
}

#[cfg(windows)]
fn config_request_profile_from_host() -> Result<shazam_http_lab::ConfigRequestProfile, String> {
    shazam_http_lab::windows_config_request_profile().map_err(|error| error.to_string())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn config_request_profile_from_host() -> Result<shazam_http_lab::ConfigRequestProfile, String> {
    Err("automatic Config profile detection is available only on Windows and macOS".into())
}

#[cfg(target_os = "macos")]
fn command_value(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

#[cfg(target_os = "macos")]
fn mac_preferred_language() -> Option<String> {
    let raw = command_value("defaults", &["read", "-g", "AppleLanguages"])?;
    let first = first_quoted_value(&raw)?;
    let language = first.split(['-', '_']).next()?.trim();
    (!language.is_empty()).then(|| language.to_owned())
}

#[cfg(target_os = "macos")]
fn mac_locale_identifier() -> Option<String> {
    let raw = command_value("defaults", &["read", "-g", "AppleLocale"])?;
    let locale = raw.trim().split('.').next()?.replace('-', "_");
    (!locale.is_empty()).then_some(locale)
}

#[cfg(target_os = "macos")]
fn first_quoted_value(value: &str) -> Option<String> {
    let start = value.find('"')? + 1;
    let end = start + value[start..].find('"')?;
    Some(value[start..end].to_owned())
}

fn random_uuid_v4() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    ))
}

fn mock_signature(signature: &[u8], sample_ms: u64) -> ExitCode {
    let timestamp_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(error) => {
            eprintln!("system clock is before Unix epoch: {error}");
            return ExitCode::from(1);
        }
    };
    let timezone = local_timezone_name();
    eprintln!("Diagnostic request timezone: {timezone}");
    let body = match shazam_http_lab::build_request_body(
        signature,
        sample_ms,
        timestamp_ms,
        &timezone,
        json!({}),
        json!({}),
        None,
    ) {
        Ok(body) => body,
        Err(error) => {
            eprintln!("build diagnostic request: {error}");
            return ExitCode::from(1);
        }
    };

    match run_mock_roundtrip(&body) {
        Ok((status, response)) => {
            println!("MOCK ONLY — no Shazam service was contacted; HTTP status={status}");
            match shazam_http_lab::parse_track_candidates(&response) {
                Ok(candidates) => {
                    for (index, candidate) in candidates.iter().enumerate() {
                        println!(
                            "candidate[{}] title={:?} artist={:?} trackid={:?}",
                            index + 1,
                            candidate.title,
                            candidate.artist,
                            candidate.track_id
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("decode mock response: {error}");
                    ExitCode::from(1)
                }
            }
        }
        Err(error) => {
            eprintln!("local mock roundtrip failed: {error}");
            ExitCode::from(1)
        }
    }
}

/// Use the host's local IANA timezone for the diagnostic recognition request.
#[cfg(windows)]
fn local_timezone_name() -> String {
    env::var("TZ")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| iana_time_zone::get_timezone().ok())
        .unwrap_or_else(|| "UTC".into())
}

#[cfg(not(windows))]
fn local_timezone_name() -> String {
    fs::read_link("/etc/localtime")
        .ok()
        .and_then(|path| timezone_from_zoneinfo_path(&path))
        .or_else(|| env::var("TZ").ok().filter(|value| !value.trim().is_empty()))
        .unwrap_or_else(|| "UTC".into())
}

#[cfg(any(test, not(windows)))]
fn timezone_from_zoneinfo_path(path: &Path) -> Option<String> {
    let path = path.to_str()?;
    ["/zoneinfo/", "/zoneinfo.default/"]
        .into_iter()
        .find_map(|marker| path.rsplit_once(marker).map(|(_, zone)| zone))
        .filter(|zone| !zone.is_empty() && !zone.split('/').any(|part| part == ".."))
        .map(str::to_owned)
}

fn run_mock_roundtrip(
    body: &[u8],
) -> Result<(u16, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let server = thread::spawn(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let (stream, _) = listener.accept()?;
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line)?;
            if request_line != "POST /mock HTTP/1.1\r\n" {
                return Err(format!("unexpected local mock request line: {request_line:?}").into());
            }
            let mut content_length = None;
            let mut platform = None;
            let mut app_version = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line)?;
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.trim_end().split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        content_length = Some(value.trim().parse::<usize>()?);
                    }
                    if name.eq_ignore_ascii_case("x-shazam-platform") {
                        platform = Some(value.trim().to_owned());
                    }
                    if name.eq_ignore_ascii_case("x-shazam-appversion") {
                        app_version = Some(value.trim().to_owned());
                    }
                }
            }
            if platform.as_deref() != Some("MAC") || app_version.as_deref() != Some("2.11.0") {
                return Err(
                    "local request omitted the observed Shazam platform/version headers".into(),
                );
            }
            let mut request_body = vec![0; content_length.ok_or("missing Content-Length")?];
            reader.read_exact(&mut request_body)?;
            let parsed: Value = serde_json::from_slice(&request_body)?;
            if parsed.get("signature").is_none() {
                return Err("mock request did not contain a signature".into());
            }

            let response = br#"{"matches":[{"trackid":"mock-track-001","title":"Mock Result Only","artist":"Local Test Fixture"}]}"#;
            let mut stream = reader.into_inner();
            write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response.len()
        )?;
            stream.write_all(response)?;
            Ok(())
        },
    );

    let endpoint = format!("http://{address}/mock");
    let response = shazam_http_lab::post_loopback_json(&endpoint, body)?;
    server
        .join()
        .map_err(|_| io::Error::other("local mock server thread panicked"))??;
    Ok((response.status, response.body))
}

fn usage() {
    eprintln!("usage: shazam-http-lab mock-roundtrip <signature.bin> <sample-duration-ms>");
    eprintln!("   or: shazam-http-lab pcm-mock <mono-i16le.pcm> <sample-rate-hz>");
    eprintln!("   or: shazam-http-lab audio-mock <audio-file> [seconds]");
    eprintln!("   or: shazam-http-lab signature-live <signature.bin> <sample-duration-ms>");
    eprintln!("   or: shazam-http-lab audio-live <audio-file> [seconds]");
    eprintln!("   or: shazam-http-lab config-check");
    eprintln!(
        "Live commands fetch Config automatically unless both manual route overrides are set."
    );
    eprintln!("Config profile overrides: SHAZAM_CONFIG_COUNTRY, _LANGUAGE, _LOCALE, _MODEL, _OS_VERSION, _OS_BUILD.");
    eprintln!("The live tag request sends no AMP token; use only when authorized.");
}

#[cfg(test)]
mod tests {
    use super::timezone_from_zoneinfo_path;
    use std::path::Path;

    #[test]
    fn creates_uuid_v4_from_the_platform_random_source() {
        let id = super::random_uuid_v4().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn extracts_iana_timezone_from_macos_zoneinfo_symlink() {
        assert_eq!(
            timezone_from_zoneinfo_path(Path::new("/var/db/timezone/zoneinfo/Asia/Tokyo")),
            Some("Asia/Tokyo".into())
        );
    }

    #[test]
    fn extracts_iana_timezone_from_default_zoneinfo_symlink() {
        assert_eq!(
            timezone_from_zoneinfo_path(Path::new(
                "/var/db/timezone/zoneinfo.default/Europe/Paris"
            )),
            Some("Europe/Paris".into())
        );
    }

    #[test]
    fn rejects_paths_outside_zoneinfo_tree() {
        assert_eq!(
            timezone_from_zoneinfo_path(Path::new("/etc/localtime")),
            None
        );
    }
}
