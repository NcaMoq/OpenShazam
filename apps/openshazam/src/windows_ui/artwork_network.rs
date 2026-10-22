use std::os::windows::process::CommandExt;
use std::{
    io::Read,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs},
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

const MAX_ACTIVE_ARTWORK_DNS_RESOLUTIONS: usize = 4;
static ACTIVE_ARTWORK_DNS_RESOLUTIONS: AtomicUsize = AtomicUsize::new(0);

struct ArtworkDnsPermit;

impl Drop for ArtworkDnsPermit {
    fn drop(&mut self) {
        ACTIVE_ARTWORK_DNS_RESOLUTIONS.fetch_sub(1, Ordering::AcqRel);
    }
}

fn try_acquire_artwork_dns_slot() -> Option<ArtworkDnsPermit> {
    let mut active = ACTIVE_ARTWORK_DNS_RESOLUTIONS.load(Ordering::Acquire);
    loop {
        if active >= MAX_ACTIVE_ARTWORK_DNS_RESOLUTIONS {
            return None;
        }
        match ACTIVE_ARTWORK_DNS_RESOLUTIONS.compare_exchange_weak(
            active,
            active + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Some(ArtworkDnsPermit),
            Err(observed) => active = observed,
        }
    }
}

fn artwork_dns_lookup_with_timeout(
    timeout: Duration,
    lookup: impl FnOnce() -> Result<Vec<IpAddr>, String> + Send + 'static,
) -> Result<Vec<IpAddr>, String> {
    let started_at = Instant::now();
    let permit = try_acquire_artwork_dns_slot()
        .ok_or_else(|| "too many artwork host lookups are still pending".to_owned())?;
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("openshazam-artwork-dns".to_owned())
        .spawn(move || {
            let result = lookup();
            drop(permit);
            let _ = sender.send(result);
        })
        .map_err(|error| format!("could not start artwork host lookup: {error}"))?;

    let remaining = timeout.saturating_sub(started_at.elapsed());
    match receiver.recv_timeout(remaining) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err("artwork host lookup timed out".to_owned()),
        Err(RecvTimeoutError::Disconnected) => {
            Err("artwork host lookup worker stopped unexpectedly".to_owned())
        }
    }
}

pub(super) fn valid_https_resource_url(url: &str) -> bool {
    if url.len() > 4_096
        || !url.starts_with("https://")
        || url.contains('#')
        || url.chars().any(char::is_control)
    {
        return false;
    }
    resource_host_port(url).is_some_and(|(host, _)| {
        host.parse::<IpAddr>()
            .map(is_public_artwork_address)
            .unwrap_or(true)
    })
}

pub(super) fn resource_host_port(url: &str) -> Option<(String, u16)> {
    let remainder = url.strip_prefix("https://")?;
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if authority.is_empty() || authority.contains(['@', '\\']) {
        return None;
    }

    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let close = bracketed.find(']')?;
        let host = &bracketed[..close];
        host.parse::<Ipv6Addr>().ok()?;
        let suffix = &bracketed[close + 1..];
        let port = if suffix.is_empty() {
            443
        } else {
            suffix.strip_prefix(':')?.parse::<u16>().ok()?
        };
        (host, port)
    } else {
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host, port.parse::<u16>().ok()?),
            Some(_) => return None,
            None => (authority, 443),
        };
        if host.is_empty() || host.contains(['[', ']']) {
            return None;
        }
        if host.parse::<Ipv4Addr>().is_err() && !valid_dns_host(host) {
            return None;
        }
        (host, port)
    };
    (port == 443).then(|| (host.to_ascii_lowercase(), port))
}

fn valid_dns_host(host: &str) -> bool {
    if host.is_empty()
        || host.len() > 253
        || host.ends_with('.')
        || looks_like_legacy_ipv4_literal(host)
    {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && label
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn looks_like_legacy_ipv4_literal(host: &str) -> bool {
    if host.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
        return true;
    }
    let labels = host.split('.').collect::<Vec<_>>();
    if labels.len() > 4 {
        return false;
    }

    let mut uses_legacy_notation = false;
    for label in labels {
        if label.is_empty() {
            return false;
        }
        if let Some(hex) = label
            .strip_prefix("0x")
            .or_else(|| label.strip_prefix("0X"))
        {
            if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return false;
            }
            uses_legacy_notation = true;
        } else if label.len() > 1 && label.starts_with('0') {
            if !label.bytes().all(|byte| matches!(byte, b'0'..=b'7')) {
                return false;
            }
            uses_legacy_notation = true;
        } else if !label.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
    }

    // Windows' legacy inet_addr accepts one to four components and allows
    // octal/hexadecimal components, so these strings must not be treated as
    // ordinary DNS names even when Rust's strict IPv4 parser rejects them.
    uses_legacy_notation || host.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
}

pub(super) fn is_public_artwork_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_private()
                && !ip.is_link_local()
                && !ip.is_broadcast()
                && !ip.is_multicast()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 192 && b == 0)
                && !(a == 192 && b == 88 && c == 99)
                && !(a == 192 && b == 0 && c == 2)
                && !(a == 198 && (b == 18 || b == 19))
                && !(a == 198 && b == 51 && c == 100)
                && !(a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public_artwork_address(IpAddr::V4(mapped));
            }
            let segments = ip.segments();
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && !ip.is_unique_local()
                && !ip.is_unicast_link_local()
                && segments[0] & 0xe000 == 0x2000 // Global unicast 2000::/3.
                && !(segments[0] == 0x2001 && segments[1] & 0xfe00 == 0) // IETF assignments.
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8) // Documentation.
                && !(segments[0] == 0x2001 && segments[1] & 0xfff0 == 0x0010) // ORCHID.
                && !(segments[0] == 0x2001 && segments[1] == 0x0002 && segments[2] == 0)
                && segments[0] != 0x2002 // 6to4 can encode private IPv4 destinations.
                && segments[0] != 0x3fff // Documentation prefix.
        }
    }
}

pub(super) fn resolve_public_artwork_addresses_with_timeout(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<Vec<IpAddr>, String> {
    let addresses = if let Ok(address) = host.parse::<IpAddr>() {
        vec![address]
    } else {
        let host = host.to_owned();
        artwork_dns_lookup_with_timeout(timeout, move || lookup_artwork_addresses(&host, port))?
    };
    validate_public_artwork_addresses(addresses)
}

fn lookup_artwork_addresses(host: &str, port: u16) -> Result<Vec<IpAddr>, String> {
    if let Ok(address) = host.parse::<IpAddr>() {
        return Ok(vec![address]);
    }
    (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("artwork host lookup failed: {error}"))
        .map(|addresses| addresses.map(|socket| socket.ip()).collect())
}

fn validate_public_artwork_addresses(addresses: Vec<IpAddr>) -> Result<Vec<IpAddr>, String> {
    if addresses.is_empty() || addresses.iter().any(|ip| !is_public_artwork_address(*ip)) {
        return Err("artwork host resolved to a non-public address".to_owned());
    }
    let mut unique = Vec::with_capacity(addresses.len());
    for address in addresses {
        if !unique.contains(&address) {
            unique.push(address);
        }
    }
    Ok(unique)
}

pub(super) fn curl_resolve_entry(host: &str, port: u16, addresses: &[IpAddr]) -> Option<String> {
    if host.parse::<IpAddr>().is_ok() {
        return None;
    }
    let addresses = addresses
        .iter()
        .map(|address| match address {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        })
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("{host}:{port}:{addresses}"))
}

pub(super) fn supported_artwork_content_type(content_type: &str) -> bool {
    matches!(
        content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "image/jpeg" | "image/png" | "image/webp"
    )
}

const MAX_ARTWORK_BYTES: usize = 8 * 1024 * 1024;
const MAX_ARTWORK_METADATA_BYTES: usize = 8 * 1024;
pub(super) const MAX_ARTWORK_REDIRECTS: usize = 4;
pub(super) const ARTWORK_FETCH_TIMEOUT: Duration = Duration::from_secs(12);
const ARTWORK_RESPONSE_MARKER: &[u8] = b"\nOPEN SHAZAM IMAGE RESPONSE:";

pub(super) struct CurlArtworkResponse {
    pub(super) status: u16,
    pub(super) redirect_url: String,
    pub(super) content_type: String,
    pub(super) remote_ip: IpAddr,
    pub(super) body: Vec<u8>,
}

fn parse_curl_artwork_output(output: &[u8]) -> Result<CurlArtworkResponse, String> {
    if output.len() > MAX_ARTWORK_BYTES + MAX_ARTWORK_METADATA_BYTES {
        return Err("artwork response exceeded the byte limit".to_owned());
    }
    let marker = output
        .windows(ARTWORK_RESPONSE_MARKER.len())
        .rposition(|window| window == ARTWORK_RESPONSE_MARKER)
        .ok_or_else(|| "artwork response metadata was missing".to_owned())?;
    let metadata = std::str::from_utf8(&output[marker + ARTWORK_RESPONSE_MARKER.len()..])
        .map_err(|_| "artwork response metadata was not valid text".to_owned())?;
    let mut fields = metadata.splitn(4, '\n');
    let status = fields
        .next()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .ok_or_else(|| "artwork response status was invalid".to_owned())?;
    let redirect_url = fields.next().unwrap_or_default().trim().to_owned();
    let content_type = fields.next().unwrap_or_default().trim().to_owned();
    let remote_ip = fields
        .next()
        .and_then(|value| value.trim().parse::<IpAddr>().ok())
        .ok_or_else(|| "artwork server address was missing or invalid".to_owned())?;
    if marker > MAX_ARTWORK_BYTES {
        return Err("artwork response exceeded the byte limit".to_owned());
    }
    Ok(CurlArtworkResponse {
        status,
        redirect_url,
        content_type,
        remote_ip,
        body: output[..marker].to_vec(),
    })
}

pub(super) fn curl_artwork_request(
    url: &str,
    resolve: Option<&str>,
    remaining: Duration,
) -> Result<CurlArtworkResponse, String> {
    let timeout = format!("{:.3}", remaining.as_secs_f64().max(0.1));
    let max_bytes = MAX_ARTWORK_BYTES.to_string();
    let write_out =
        "\nOPEN SHAZAM IMAGE RESPONSE:%{http_code}\n%{redirect_url}\n%{content_type}\n%{remote_ip}"
            .to_string();
    let curl_program = shazam_http_lab::system_curl_program()
        .map_err(|error| format!("artwork downloader unavailable: {error}"))?;
    let mut command = Command::new(curl_program);
    // curl is a console executable. Do not let artwork requests open a terminal
    // window when Shazam itself is running as a GUI-subsystem application.
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    command.args([
        "--disable",
        "--fail",
        "--proto",
        "=https",
        "--max-time",
        &timeout,
        "--max-filesize",
        &max_bytes,
        "--silent",
        "--show-error",
        "--proxy",
        "",
        "--output",
        "-",
    ]);
    if let Some(resolve) = resolve {
        command.args(["--resolve", resolve]);
    }
    let mut child = command
        .args(["--write-out", &write_out, url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("artwork downloader unavailable: {error}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "artwork downloader output was unavailable".to_owned())?;
    let output_limit = MAX_ARTWORK_BYTES + MAX_ARTWORK_METADATA_BYTES + 1;
    let mut output = Vec::with_capacity(64 * 1024);
    let read_result = stdout
        .by_ref()
        .take(output_limit as u64)
        .read_to_end(&mut output);
    drop(stdout);
    if let Err(error) = read_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("artwork response read failed: {error}"));
    }
    if output.len() > MAX_ARTWORK_BYTES + MAX_ARTWORK_METADATA_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return Err("artwork response exceeded the byte limit".to_owned());
    }
    let status = child
        .wait()
        .map_err(|error| format!("artwork downloader wait failed: {error}"))?;
    if !status.success() {
        return Err("artwork download failed or exceeded its transfer limits".to_owned());
    }

    parse_curl_artwork_output(&output)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn ui209_artwork_urls_reject_unsafe_authorities_and_private_ip_literals() {
        assert!(super::valid_https_resource_url(
            "https://images.example.test/cover.webp?size=300"
        ));
        assert!(super::valid_https_resource_url(
            "https://0x7f.example.test/cover.webp"
        ));
        assert!(super::valid_https_resource_url(
            "https://[2001:4860:4860::8888]/cover.webp"
        ));
        assert!(!super::valid_https_resource_url(
            "http://images.example.test/cover.jpg"
        ));
        assert!(!super::valid_https_resource_url(
            "https://images.example.test:8443/cover.jpg"
        ));
        assert!(!super::valid_https_resource_url(
            "https://user@images.example.test/cover.jpg"
        ));
        assert!(!super::valid_https_resource_url(
            "https://images.example.test/cover.jpg#frag"
        ));
        assert!(!super::valid_https_resource_url(
            "https://127.0.0.1/cover.jpg"
        ));
        assert!(!super::valid_https_resource_url("https://127.1/cover.jpg"));
        assert!(!super::valid_https_resource_url(
            "https://08.0.0.1/cover.jpg"
        ));
        assert!(!super::valid_https_resource_url(
            "https://0x7f.0.0.1/cover.jpg"
        ));
        assert!(!super::valid_https_resource_url(
            "https://0177.0.0.1/cover.jpg"
        ));
        assert!(!super::valid_https_resource_url(
            "https://0x7f000001/cover.jpg"
        ));
        assert!(!super::valid_https_resource_url("https://[::1]/cover.jpg"));
    }

    #[test]
    fn ui209_artwork_address_policy_rejects_private_reserved_and_tunnel_ranges() {
        for value in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.0.1",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2001:2::1",
            "2001:10::1",
            "2002::1",
            "3fff::1",
        ] {
            let address = value
                .parse()
                .unwrap_or_else(|_| panic!("invalid test IP {value}"));
            assert!(
                !super::is_public_artwork_address(address),
                "{value} must not be a public artwork destination"
            );
        }
        for value in ["1.1.1.1", "8.8.8.8", "2001:4860:4860::8888"] {
            let address = value
                .parse()
                .unwrap_or_else(|_| panic!("invalid test IP {value}"));
            assert!(
                super::is_public_artwork_address(address),
                "{value} should remain available for public artwork"
            );
        }
    }

    #[test]
    fn artwork_dns_lookups_timeout_and_bound_resolvers_that_outlive_the_deadline() {
        let (started_sender, started_receiver) = mpsc::channel();
        let mut release_senders = Vec::new();
        let mut result_receivers = Vec::new();
        let mut callers = Vec::new();

        for _ in 0..super::MAX_ACTIVE_ARTWORK_DNS_RESOLUTIONS {
            let started_sender = started_sender.clone();
            let (release_sender, release_receiver) = mpsc::channel();
            let (result_sender, result_receiver) = mpsc::channel();
            callers.push(thread::spawn(move || {
                let result =
                    super::artwork_dns_lookup_with_timeout(Duration::from_millis(40), move || {
                        let _ = started_sender.send(());
                        let _ = release_receiver.recv();
                        Ok(vec!["8.8.8.8".parse().unwrap()])
                    });
                let _ = result_sender.send(result);
            }));
            release_senders.push(release_sender);
            result_receivers.push(result_receiver);
        }
        drop(started_sender);

        for _ in 0..super::MAX_ACTIVE_ARTWORK_DNS_RESOLUTIONS {
            started_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("each bounded resolver worker should start");
        }
        for result_receiver in &result_receivers {
            assert_eq!(
                result_receiver
                    .recv_timeout(Duration::from_secs(2))
                    .expect("DNS lookup must honor its deadline")
                    .unwrap_err(),
                "artwork host lookup timed out"
            );
        }

        let excess_lookup = super::artwork_dns_lookup_with_timeout(Duration::from_secs(1), || {
            Ok(vec!["8.8.8.8".parse().unwrap()])
        });
        assert_eq!(
            excess_lookup.unwrap_err(),
            "too many artwork host lookups are still pending"
        );

        for release_sender in release_senders {
            release_sender.send(()).unwrap();
        }
        for caller in callers {
            caller.join().unwrap();
        }

        let cleanup_deadline = Instant::now() + Duration::from_secs(2);
        while super::ACTIVE_ARTWORK_DNS_RESOLUTIONS.load(std::sync::atomic::Ordering::Acquire) != 0
            && Instant::now() < cleanup_deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            super::ACTIVE_ARTWORK_DNS_RESOLUTIONS.load(std::sync::atomic::Ordering::Acquire),
            0,
            "released resolver workers must return their slots"
        );
    }

    #[test]
    fn ui209_artwork_host_resolution_is_pinned_and_mime_allowlist_is_narrow() {
        let addresses = [
            "8.8.8.8".parse().unwrap(),
            "2001:4860:4860::8888".parse().unwrap(),
        ];
        assert_eq!(
            super::curl_resolve_entry("images.example.test", 443, &addresses),
            Some("images.example.test:443:8.8.8.8,[2001:4860:4860::8888]".to_owned())
        );
        assert!(super::curl_resolve_entry("8.8.8.8", 443, &addresses).is_none());
        assert!(super::supported_artwork_content_type("image/jpeg"));
        assert!(super::supported_artwork_content_type(
            "IMAGE/PNG; charset=binary"
        ));
        assert!(super::supported_artwork_content_type("image/webp"));
        assert!(!super::supported_artwork_content_type("text/html"));
        assert!(!super::supported_artwork_content_type("image/svg+xml"));
    }

    #[test]
    fn ui209_curl_artwork_output_is_bounded_and_extracts_response_metadata() {
        let output = b"image-bytes\nOPEN SHAZAM IMAGE RESPONSE:302\nhttps://cdn.example.test/cover.png\nimage/png\n8.8.8.8";
        let response = super::parse_curl_artwork_output(output).unwrap();
        assert_eq!(response.status, 302);
        assert_eq!(response.redirect_url, "https://cdn.example.test/cover.png");
        assert_eq!(response.content_type, "image/png");
        assert_eq!(
            response.remote_ip,
            "8.8.8.8".parse::<std::net::IpAddr>().unwrap()
        );
        assert_eq!(response.body, b"image-bytes");

        let oversized =
            vec![b'x'; super::MAX_ARTWORK_BYTES + super::MAX_ARTWORK_METADATA_BYTES + 1];
        assert!(super::parse_curl_artwork_output(&oversized).is_err());
        let oversized_body = format!(
            "{}\nOPEN SHAZAM IMAGE RESPONSE:200\n\nimage/jpeg\n8.8.8.8",
            "x".repeat(super::MAX_ARTWORK_BYTES + 1)
        );
        assert!(super::parse_curl_artwork_output(oversized_body.as_bytes()).is_err());
    }
}
