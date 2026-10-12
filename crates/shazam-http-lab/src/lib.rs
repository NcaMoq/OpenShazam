//! Offline request-shape, loopback mock, and opt-in live HTTP laboratory.
//!
//! The request body is a partial static reconstruction. Mock requests use a
//! loopback-only transport; the separate live path fetches public Config over
//! verified HTTPS and restricts recognition to the resulting Mac manual-tag
//! route. This is an experimental
//! client for a non-public route, not an official API implementation. The
//! request carries the observed Shazam 2.11.0 platform/version headers and
//! omits the conditional AMP authentication header.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    process::{Command, Stdio},
    time::Duration,
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};

const SIGNATURE_DATA_URI: &str = "data:audio/vnd.shazam.sig;base64,";
const SHAZAM_MAC_PLATFORM: &str = "MAC";
const SHAZAM_MAC_APP_VERSION: &str = "2.11.0";
const MAX_RESPONSE_BYTES: usize = 1_048_576;
const MAX_HEADER_BYTES: usize = 16_384;
const CURL_STATUS_MARKER: &[u8] = b"\n__OPENSHZAM_HTTP_STATUS__";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackCandidate {
    pub track_id: Option<String>,
    pub title: String,
    pub artist: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Non-secret host profile sent to the Config endpoint. The installation ID
/// is deliberately not part of this type; it is parsed from the HTTPS response
/// and is never persisted by this crate.
#[derive(Clone, PartialEq, Eq)]
pub struct ConfigRequestProfile {
    pub country: String,
    pub language: String,
    pub locale: String,
    pub model: String,
    pub os_version: String,
    pub os_build: String,
    pub app_version: String,
}

/// The two values required to construct a Mac manual-tag route. No `Debug`
/// implementation is provided so accidental logging cannot expose INID.
pub struct ShazamConfiguration {
    tag_url_template: String,
    installation_id: String,
}

impl ShazamConfiguration {
    pub fn resolve_tag_url(&self, tag_id: &str, streaming_provider: &str) -> Result<String, Error> {
        resolve_shazam_tag_url(
            &self.tag_url_template,
            &self.installation_id,
            tag_id,
            streaming_provider,
        )
    }

    pub fn tag_url_template(&self) -> &str {
        &self.tag_url_template
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidSignature(String),
    InvalidEndpoint(String),
    InvalidRequest(String),
    Transport(String),
    BadResponse(String),
    NoMatches,
}

/// Replace the three placeholders used by the Mac tagging endpoint builder.
///
/// The installed binary calls `stringByReplacingOccurrencesOfString` on the
/// configured URL string for these values. This mirrors that literal
/// replacement behavior; it does not invent a URL, add query parameters, or
/// URL-encode replacement values.
pub fn substitute_endpoint_tokens(
    template: &str,
    tag_id: &str,
    streaming_provider: &str,
    installation_id: &str,
) -> String {
    template
        .replace("{tagid}", tag_id)
        .replace("{streamingprovider}", streaming_provider)
        .replace("{inid}", installation_id)
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSignature(message)
            | Self::InvalidEndpoint(message)
            | Self::InvalidRequest(message)
            | Self::Transport(message)
            | Self::BadResponse(message) => f.write_str(message),
            Self::NoMatches => f.write_str("response contained no usable track candidates"),
        }
    }
}

impl std::error::Error for Error {}

/// Build the observed Config bootstrap URL. The response supplies both the
/// `apis.tag.href` template and its INID; this function does not embed the
/// recognition endpoint.
pub fn build_config_url(profile: &ConfigRequestProfile) -> Result<String, Error> {
    if profile.country.is_empty()
        || !profile
            .country
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(Error::InvalidRequest(
            "Config country must be a safe country token or `-`".into(),
        ));
    }
    for (name, value) in [
        ("language", profile.language.as_str()),
        ("locale", profile.locale.as_str()),
        ("model", profile.model.as_str()),
        ("OS version", profile.os_version.as_str()),
        ("OS build", profile.os_build.as_str()),
        ("app version", profile.app_version.as_str()),
    ] {
        if value.is_empty()
            || value.len() > 128
            || value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(Error::InvalidRequest(format!(
                "Config {name} is empty, too long, or contains control characters"
            )));
        }
    }

    let pairs = [
        ("app", "SHAZAM"),
        ("l", profile.language.as_str()),
        ("locale", profile.locale.as_str()),
        ("model", profile.model.as_str()),
        ("platform", SHAZAM_MAC_PLATFORM),
        ("os", "MACOS"),
        ("osVersion", profile.os_version.as_str()),
        ("appVersion", profile.app_version.as_str()),
        ("osBuild", profile.os_build.as_str()),
    ];
    let query = pairs
        .iter()
        .map(|(key, value)| format!("{key}={}", percent_encode_query_component(value)))
        .collect::<Vec<_>>()
        .join("&");
    Ok(format!(
        "https://config.shazam.com/configuration/v1/{}/bag?{query}",
        profile.country
    ))
}

fn percent_encode_query_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if is_url_unreserved(byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// Parse the Config response without retaining any unrelated user/config data.
/// The root INID and `apis.tag.href` are the only extracted values.
pub fn parse_shazam_config(body: &[u8]) -> Result<ShazamConfiguration, Error> {
    let root: Value = serde_json::from_slice(body)
        .map_err(|_| Error::BadResponse("Config response is not valid JSON".into()))?;
    let installation_id = root
        .get("inid")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= 128 && value.bytes().all(is_url_unreserved)
        })
        .ok_or_else(|| {
            Error::BadResponse("Config response has no usable root `inid` value".into())
        })?;
    let tag_url_template = root
        .get("apis")
        .and_then(|apis| apis.get("tag"))
        .and_then(|tag| tag.get("href"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::BadResponse("Config response has no `apis.tag.href` value".into()))?;
    validate_shazam_tag_template(tag_url_template)?;
    Ok(ShazamConfiguration {
        tag_url_template: tag_url_template.to_owned(),
        installation_id: installation_id.to_owned(),
    })
}

/// Resolve a Config-provided Mac tag template while keeping the route
/// allowlist and token validation in force.
pub fn resolve_shazam_tag_url(
    template: &str,
    installation_id: &str,
    tag_id: &str,
    streaming_provider: &str,
) -> Result<String, Error> {
    validate_shazam_tag_template(template)?;
    for (name, value) in [("INID", installation_id), ("tag ID", tag_id)] {
        if value.is_empty() || !value.bytes().all(is_url_unreserved) {
            return Err(Error::InvalidEndpoint(format!(
                "Config {name} is not a safe URL path token"
            )));
        }
    }
    if !streaming_provider.bytes().all(is_url_unreserved) {
        return Err(Error::InvalidEndpoint(
            "streaming provider is not a safe URL query token".into(),
        ));
    }
    let endpoint =
        substitute_endpoint_tokens(template, tag_id, streaming_provider, installation_id);
    validate_shazam_endpoint(&endpoint)?;
    Ok(endpoint)
}

fn validate_shazam_tag_template(template: &str) -> Result<(), Error> {
    if template.matches("{inid}").count() != 1
        || template.matches("{tagid}").count() != 1
        || template.matches("{streamingprovider}").count() > 1
    {
        return Err(Error::InvalidEndpoint(
            "Config tag template has missing or duplicate placeholders".into(),
        ));
    }
    let sanitized = template
        .replace("{inid}", "installation-id")
        .replace("{tagid}", "tag-id")
        .replace("{streamingprovider}", "");
    if sanitized.contains(['{', '}']) {
        return Err(Error::InvalidEndpoint(
            "Config tag template contains an unknown placeholder".into(),
        ));
    }
    validate_shazam_endpoint(&sanitized)
}

/// Construct the statically observed Mac tagging body shape.
///
/// The required fields and data-URI prefix were found in the installed Mac
/// app's request builder. Optional context/origin values and the service's
/// acceptance of this independently constructed body remain unverified.
pub fn build_request_body(
    signature: &[u8],
    sample_ms: u64,
    timestamp_ms: u64,
    timezone: &str,
    context: Value,
    geolocation: Value,
    third_party: Option<Value>,
) -> Result<Vec<u8>, Error> {
    sigx_format::Signature::parse(signature)
        .map_err(|error| Error::InvalidSignature(error.to_string()))?;
    if timezone.is_empty() {
        return Err(Error::InvalidRequest("timezone must not be empty".into()));
    }

    let mut request = json!({
        "signature": {
            "uri": format!("{SIGNATURE_DATA_URI}{}", STANDARD.encode(signature)),
            "samplems": sample_ms,
        },
        "context": context,
        "geolocation": geolocation,
        "timestamp": timestamp_ms,
        "timezone": timezone,
    });
    if let Some(origin) = third_party {
        request["thirdparty"] = origin;
    }
    serde_json::to_vec(&request)
        .map_err(|error| Error::InvalidRequest(format!("serialize request body: {error}")))
}

/// POST JSON to a loopback-only HTTP endpoint for local mock testing.
///
/// HTTPS, hostnames, proxies, redirects, and non-loopback destinations are
/// intentionally unsupported. This is not suitable for a live Shazam request.
pub fn post_loopback_json(endpoint: &str, body: &[u8]) -> Result<HttpResponse, Error> {
    let (address, path, authority) = parse_loopback_endpoint(endpoint)?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .map_err(|error| Error::Transport(format!("connect to local mock: {error}")))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| Error::Transport(format!("set mock read timeout: {error}")))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| Error::Transport(format!("set mock write timeout: {error}")))?;

    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nAccept: application/json\r\nX-Shazam-Platform: {SHAZAM_MAC_PLATFORM}\r\nX-Shazam-AppVersion: {SHAZAM_MAC_APP_VERSION}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .and_then(|()| stream.write_all(body))
    .map_err(|error| Error::Transport(format!("write request to local mock: {error}")))?;

    let mut response = Vec::new();
    stream
        .take((MAX_RESPONSE_BYTES + MAX_HEADER_BYTES + 1) as u64)
        .read_to_end(&mut response)
        .map_err(|error| Error::Transport(format!("read local mock response: {error}")))?;
    parse_http_response(&response)
}

/// POST to the observed Mac manual-tagging route over verified HTTPS.
///
/// This intentionally allows only the Shazam `amp.shazam.com` manual-tag
/// route and its observed query keys. It sends no cookies or AMP token, does
/// not use a proxy or follow redirects, and requires a locally installed curl.
/// Call only when you are authorized to use this first-party endpoint.
pub fn post_shazam_json(endpoint: &str, body: &[u8]) -> Result<HttpResponse, Error> {
    validate_shazam_endpoint(endpoint)?;

    let mut child = Command::new("/usr/bin/curl")
        .args([
            "--disable",
            "--noproxy",
            "*",
            "--silent",
            "--show-error",
            "--max-time",
            "20",
            "--proto",
            "=https",
            "--http1.1",
            "--header",
            "Content-Type: application/json",
            "--header",
            "Accept: application/json",
            "--header",
            "X-Shazam-Platform: MAC",
            "--header",
            "X-Shazam-AppVersion: 2.11.0",
            "--data-binary",
            "@-",
            "--write-out",
            "\n__OPENSHZAM_HTTP_STATUS__%{http_code}",
            "--url",
        ])
        .arg(endpoint)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| Error::Transport("could not start the system HTTPS client".into()))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Transport("HTTPS client stdin was unavailable".into()))?;
    if stdin.write_all(body).is_err() {
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::Transport(
            "could not write the JSON request body".into(),
        ));
    }
    drop(stdin);

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Transport("HTTPS client stdout was unavailable".into()))?;
    let output_limit = MAX_RESPONSE_BYTES + CURL_STATUS_MARKER.len() + 8;
    let mut combined = Vec::new();
    stdout
        .by_ref()
        .take(output_limit as u64 + 1)
        .read_to_end(&mut combined)
        .map_err(|_| Error::Transport("could not read the HTTPS response".into()))?;
    if combined.len() > output_limit {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::BadResponse(
            "HTTPS response exceeded the local size limit".into(),
        ));
    }
    let output = child
        .wait_with_output()
        .map_err(|_| Error::Transport("could not wait for the HTTPS client".into()))?;
    if !output.status.success() {
        return Err(Error::Transport(format!(
            "HTTPS request failed (curl exit {})",
            output.status.code().unwrap_or(-1)
        )));
    }

    let marker_index = combined
        .windows(CURL_STATUS_MARKER.len())
        .rposition(|window| window == CURL_STATUS_MARKER)
        .ok_or_else(|| Error::BadResponse("HTTPS client did not return a status code".into()))?;
    let status_start = marker_index + CURL_STATUS_MARKER.len();
    let status = std::str::from_utf8(&combined[status_start..])
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|value| *value != 0)
        .ok_or_else(|| Error::BadResponse("HTTPS client returned an invalid status code".into()))?;
    let response_body = combined[..marker_index].to_vec();
    if response_body.len() > MAX_RESPONSE_BYTES {
        return Err(Error::BadResponse(
            "HTTPS response exceeded the local size limit".into(),
        ));
    }
    Ok(HttpResponse {
        status,
        body: response_body,
    })
}

/// Fetch the current Shazam Config using an ordinary verified HTTPS GET.
/// Redirects, cookies, proxies, AMP credentials, and Shazam app storage are
/// not used. The response is parsed in memory and only `inid` plus
/// `apis.tag.href` are retained for the current process.
pub fn fetch_shazam_config(profile: &ConfigRequestProfile) -> Result<ShazamConfiguration, Error> {
    let url = build_config_url(profile)?;
    let app_version_header = format!("X-Shazam-AppVersion: {}", profile.app_version);
    let mut child = Command::new("/usr/bin/curl")
        .args([
            "--disable",
            "--noproxy",
            "*",
            "--silent",
            "--show-error",
            "--max-time",
            "20",
            "--proto",
            "=https",
            "--http1.1",
            "--header",
            "Accept: application/json",
            "--header",
            "Content-Type: application/json",
            "--header",
            "X-Shazam-Platform: MAC",
            "--header",
            app_version_header.as_str(),
            "--write-out",
            "\n__OPENSHZAM_CONFIG_STATUS__%{http_code}",
            "--url",
        ])
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Error::Transport("could not start the system HTTPS client".into()))?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Transport("HTTPS client stdout was unavailable".into()))?;
    let output_limit = MAX_RESPONSE_BYTES + b"\n__OPENSHZAM_CONFIG_STATUS__".len() + 8;
    let mut combined = Vec::new();
    stdout
        .by_ref()
        .take(output_limit as u64 + 1)
        .read_to_end(&mut combined)
        .map_err(|_| Error::Transport("could not read the Config response".into()))?;
    if combined.len() > output_limit {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::BadResponse(
            "Config response exceeded the local size limit".into(),
        ));
    }
    let output = child
        .wait_with_output()
        .map_err(|_| Error::Transport("could not wait for the HTTPS client".into()))?;
    if !output.status.success() {
        return Err(Error::Transport(format!(
            "Config HTTPS request failed (curl exit {})",
            output.status.code().unwrap_or(-1)
        )));
    }

    const MARKER: &[u8] = b"\n__OPENSHZAM_CONFIG_STATUS__";
    let marker_index = combined
        .windows(MARKER.len())
        .rposition(|window| window == MARKER)
        .ok_or_else(|| Error::BadResponse("HTTPS client did not return a Config status".into()))?;
    let status = std::str::from_utf8(&combined[marker_index + MARKER.len()..])
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|value| *value != 0)
        .ok_or_else(|| {
            Error::BadResponse("HTTPS client returned an invalid Config status".into())
        })?;
    if !(200..300).contains(&status) {
        return Err(Error::BadResponse(format!(
            "Config endpoint returned HTTP {status}"
        )));
    }
    parse_shazam_config(&combined[..marker_index])
}

fn validate_shazam_endpoint(endpoint: &str) -> Result<(), Error> {
    let rest = endpoint.strip_prefix("https://").ok_or_else(|| {
        Error::InvalidEndpoint("Shazam live transport requires verified HTTPS".into())
    })?;
    let (authority, path_and_query) = rest.split_once('/').ok_or_else(|| {
        Error::InvalidEndpoint("Shazam endpoint must include the manual-tag route".into())
    })?;
    if authority != "amp.shazam.com" || path_and_query.contains(['#', '@']) {
        return Err(Error::InvalidEndpoint(
            "only the configured amp.shazam.com host is accepted".into(),
        ));
    }
    if path_and_query
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b' ' || byte == b'{' || byte == b'}')
    {
        return Err(Error::InvalidEndpoint(
            "Shazam endpoint contains unsafe or unresolved characters".into(),
        ));
    }

    let (path, query) = path_and_query.split_once('?').ok_or_else(|| {
        Error::InvalidEndpoint("Shazam manual-tag route must include its configured query".into())
    })?;
    let path_parts: Vec<&str> = path.split('/').collect();
    if path_parts.len() != 9
        || path_parts[0] != "discovery"
        || path_parts[1] != "v5"
        || path_parts[2].is_empty()
        || !path_parts[2].bytes().all(is_url_unreserved)
        || path_parts[3].is_empty()
        || !path_parts[3].bytes().all(is_url_unreserved)
        || path_parts[4] != "mac"
        || path_parts[5] != "-"
        || path_parts[6] != "tag"
        || path_parts[7].is_empty()
        || !path_parts[7].bytes().all(is_url_unreserved)
        || path_parts[8].is_empty()
        || !path_parts[8].bytes().all(is_url_unreserved)
    {
        return Err(Error::InvalidEndpoint(
            "only a safe configured Mac manual-tag route is accepted".into(),
        ));
    }

    let mut seen = std::collections::BTreeMap::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(Error::InvalidEndpoint(
                "malformed Shazam query string".into(),
            ));
        };
        if !matches!(
            key,
            "sync" | "webv3" | "sampling" | "connected" | "sharehub" | "hidelb"
        ) || !value.bytes().all(is_url_unreserved)
            || seen.insert(key, value).is_some()
        {
            return Err(Error::InvalidEndpoint(
                "Shazam query contains an unexpected or duplicate parameter".into(),
            ));
        }
    }
    for (key, value) in [
        ("sync", "true"),
        ("webv3", "true"),
        ("sampling", "true"),
        ("sharehub", "false"),
        ("hidelb", "true"),
    ] {
        if seen.get(key) != Some(&value) {
            return Err(Error::InvalidEndpoint(
                "Shazam query does not match the observed manual-tag template".into(),
            ));
        }
    }
    if !seen.contains_key("connected") {
        return Err(Error::InvalidEndpoint(
            "Shazam query is missing `connected`".into(),
        ));
    }
    Ok(())
}

fn is_url_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// Decode the currently observed response keys into displayable candidates.
///
/// The `matches`, `trackid`, `title`, and `artist` keys exist in the installed
/// binary. The complete live response envelope has not been captured; this
/// parser supports the common array/object forms only as a diagnostic.
pub fn parse_track_candidates(response_body: &[u8]) -> Result<Vec<TrackCandidate>, Error> {
    let root: Value = serde_json::from_slice(response_body)
        .map_err(|error| Error::BadResponse(format!("response is not valid JSON: {error}")))?;

    if let Some(track) = root.get("track").filter(|value| value.is_object()) {
        if let Some(candidate) = track_candidate(track) {
            return Ok(vec![candidate]);
        }
    }

    let matches = root
        .get("matches")
        .ok_or_else(|| Error::BadResponse("response has no `matches` field".into()))?;
    let entries: Vec<&Value> = match matches {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![matches],
        _ => {
            return Err(Error::BadResponse(
                "response `matches` must be an object or array".into(),
            ));
        }
    };

    let mut candidates = Vec::new();
    for entry in entries {
        let track = entry
            .get("track")
            .filter(|value| value.is_object())
            .unwrap_or(entry);
        if let Some(candidate) = track_candidate(track) {
            candidates.push(candidate);
        }
    }
    if candidates.is_empty() {
        return Err(Error::NoMatches);
    }
    Ok(candidates)
}

fn track_candidate(track: &Value) -> Option<TrackCandidate> {
    Some(TrackCandidate {
        track_id: track
            .get("trackid")
            .or_else(|| track.get("key"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        title: track.get("title")?.as_str()?.to_owned(),
        artist: track
            .get("artist")
            .or_else(|| track.get("subtitle"))?
            .as_str()?
            .to_owned(),
    })
}

/// Summarize JSON response structure without exposing any response values.
pub fn summarize_response_shape(response_body: &[u8]) -> String {
    let Ok(root) = serde_json::from_slice::<Value>(response_body) else {
        return format!("non-JSON body ({} bytes)", response_body.len());
    };
    let Some(object) = root.as_object() else {
        return format!("JSON root type={}", json_type(&root));
    };

    let mut summary = format!("root keys={}", joined_keys(object));
    if let Some(track) = object.get("track") {
        if let Some(track) = track.as_object() {
            summary.push_str(&format!("; root-track keys={}", joined_keys(track)));
        } else {
            summary.push_str(&format!("; root-track type={}", json_type(track)));
        }
    }
    if let Some(matches) = object.get("matches") {
        match matches {
            Value::Array(entries) => {
                summary.push_str(&format!("; matches=array[{}]", entries.len()));
                if let Some(first) = entries.first().and_then(Value::as_object) {
                    summary.push_str(&format!("; first-match keys={}", joined_keys(first)));
                    if let Some(track) = first.get("track").and_then(Value::as_object) {
                        summary.push_str(&format!("; first-track keys={}", joined_keys(track)));
                    }
                }
            }
            Value::Object(matches) => {
                summary.push_str(&format!("; matches=object keys={}", joined_keys(matches)));
            }
            other => summary.push_str(&format!("; matches type={}", json_type(other))),
        }
    }
    summary
}

fn joined_keys(object: &serde_json::Map<String, Value>) -> String {
    object.keys().cloned().collect::<Vec<_>>().join(",")
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn parse_loopback_endpoint(endpoint: &str) -> Result<(SocketAddr, &str, &str), Error> {
    let rest = endpoint.strip_prefix("http://").ok_or_else(|| {
        Error::InvalidEndpoint("only http:// literal loopback URLs are accepted".into())
    })?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.contains('@') || authority.contains('?') || authority.contains('#') {
        return Err(Error::InvalidEndpoint(
            "credentials, query strings, and fragments are not accepted".into(),
        ));
    }
    let address: SocketAddr = authority
        .parse()
        .map_err(|_| Error::InvalidEndpoint("endpoint must use an IP literal and port".into()))?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(Error::InvalidEndpoint(
            "only a nonzero port on a literal loopback IP is accepted".into(),
        ));
    }
    let path = if path.is_empty() {
        "/"
    } else {
        &rest[authority.len()..]
    };
    if path
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(Error::InvalidEndpoint(
            "endpoint path contains invalid bytes".into(),
        ));
    }
    Ok((address, path, authority))
}

fn parse_http_response(bytes: &[u8]) -> Result<HttpResponse, Error> {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Err(Error::BadResponse(
            "HTTP response has no header terminator".into(),
        ));
    };
    if header_end > MAX_HEADER_BYTES {
        return Err(Error::BadResponse(
            "HTTP response headers are too large".into(),
        ));
    }
    let headers = std::str::from_utf8(&bytes[..header_end])
        .map_err(|error| Error::BadResponse(format!("HTTP headers are not UTF-8: {error}")))?;
    let mut lines = headers.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| Error::BadResponse("HTTP response has no status line".into()))?;
    let mut status_parts = status_line.split_ascii_whitespace();
    if !status_parts
        .next()
        .is_some_and(|protocol| protocol.starts_with("HTTP/1."))
    {
        return Err(Error::BadResponse("unsupported HTTP status line".into()));
    }
    let status = status_parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| Error::BadResponse("invalid HTTP status code".into()))?;
    let mut content_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(Error::BadResponse("malformed HTTP response header".into()));
        };
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value.trim().eq_ignore_ascii_case("chunked")
        {
            return Err(Error::BadResponse(
                "chunked mock responses are not supported".into(),
            ));
        }
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| Error::BadResponse("invalid HTTP Content-Length".into()))?,
            );
        }
    }
    let body_start = header_end + 4;
    let available = &bytes[body_start..];
    let body_length = content_length.unwrap_or(available.len());
    if body_length > MAX_RESPONSE_BYTES || available.len() < body_length {
        return Err(Error::BadResponse(
            "HTTP response body length is missing data or exceeds the local limit".into(),
        ));
    }
    Ok(HttpResponse {
        status,
        body: available[..body_length].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        thread,
    };

    use super::*;

    fn valid_signature() -> Vec<u8> {
        sigx_format::encode_legacy_signature(&sigx_format::LegacySignatureInput {
            sample_rate: 16_000,
            stream_pos_ticks: 0,
            sample_length_samples: 0,
            density: 15,
            sections: Vec::new(),
        })
        .expect("empty legacy signature is valid")
    }

    #[test]
    fn body_contains_observed_data_uri_and_fields() {
        let body = build_request_body(
            &valid_signature(),
            12_000,
            1_800_000_000_123,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            Some(json!("test-origin")),
        )
        .unwrap();
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        let uri = parsed["signature"]["uri"].as_str().unwrap();
        assert!(uri.starts_with(SIGNATURE_DATA_URI));
        assert_eq!(parsed["signature"]["samplems"], 12_000);
        assert_eq!(parsed["timestamp"], 1_800_000_000_123_u64);
        assert_eq!(parsed["timezone"], "Asia/Tokyo");
        assert_eq!(parsed["thirdparty"], "test-origin");
        assert_eq!(parsed["context"], json!({}));
        assert_eq!(parsed["geolocation"], json!({}));
    }

    #[test]
    fn response_parser_reads_mock_matches_without_claiming_live_schema() {
        let candidates = parse_track_candidates(
            br#"{"matches":[{"trackid":"mock-1","title":"Mock Song","artist":"Test Artist"}]}"#,
        )
        .unwrap();
        assert_eq!(
            candidates,
            vec![TrackCandidate {
                track_id: Some("mock-1".into()),
                title: "Mock Song".into(),
                artist: "Test Artist".into(),
            }]
        );
    }

    #[test]
    fn response_parser_accepts_track_key_and_subtitle_shape() {
        let candidates = parse_track_candidates(
            br#"{"matches":[{"track":{"key":"mock-key","title":"Mock Song","subtitle":"Test Artist"}}]}"#,
        )
        .unwrap();
        assert_eq!(candidates[0].track_id.as_deref(), Some("mock-key"));
        assert_eq!(candidates[0].title, "Mock Song");
        assert_eq!(candidates[0].artist, "Test Artist");
    }

    #[test]
    fn response_parser_accepts_top_level_track_alongside_match_offsets() {
        let candidates = parse_track_candidates(
            br#"{"matches":[{"id":"match-1","offset":0.25,"timeskew":0.0,"frequencyskew":0.0}],"track":{"key":"track-1","title":"Mock Song","subtitle":"Test Artist"}}"#,
        )
        .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].track_id.as_deref(), Some("track-1"));
        assert_eq!(candidates[0].title, "Mock Song");
        assert_eq!(candidates[0].artist, "Test Artist");
    }

    #[test]
    fn response_shape_summary_reports_keys_and_counts_without_values() {
        let summary = summarize_response_shape(
            br#"{"matches":[{"track":{"title":"secret title","subtitle":"secret artist"},"id":"private-id"}],"requestId":"private-request"}"#,
        );
        assert!(summary.contains("root keys=matches,requestId"));
        assert!(summary.contains("matches=array[1]"));
        assert!(summary.contains("first-match keys=id,track"));
        assert!(!summary.contains("secret"));
        assert!(!summary.contains("private"));
    }

    #[test]
    fn endpoint_token_replacement_matches_static_string_replacement() {
        let resolved = substitute_endpoint_tokens(
            "https://fixture.invalid/tag/{tagid}?provider={streamingprovider}&inid={inid}",
            "tag-123",
            "provider-x",
            "install-456",
        );
        assert_eq!(
            resolved,
            "https://fixture.invalid/tag/tag-123?provider=provider-x&inid=install-456"
        );
    }

    fn config_profile() -> ConfigRequestProfile {
        ConfigRequestProfile {
            country: "-".into(),
            language: "ja".into(),
            locale: "ja_JP".into(),
            model: "MacBookPro18,2".into(),
            os_version: "15.7.4".into(),
            os_build: "24G517".into(),
            app_version: SHAZAM_MAC_APP_VERSION.into(),
        }
    }

    #[test]
    fn config_url_encodes_host_profile_and_uses_country_fallback() {
        assert_eq!(
            build_config_url(&config_profile()).unwrap(),
            "https://config.shazam.com/configuration/v1/-/bag?app=SHAZAM&l=ja&locale=ja_JP&model=MacBookPro18%2C2&platform=MAC&os=MACOS&osVersion=15.7.4&appVersion=2.11.0&osBuild=24G517"
        );
        let mut profile = config_profile();
        profile.country = "../amp.shazam.com".into();
        assert!(matches!(
            build_config_url(&profile),
            Err(Error::InvalidRequest(_))
        ));
    }

    #[test]
    fn config_fixture_yields_ephemeral_inid_and_dynamic_tag_route() {
        let fixture = br#"{
            "inid":"fixture-installation-id",
            "apis":{"tag":{"href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true"}},
            "unrelated":"must not be retained"
        }"#;
        let config = parse_shazam_config(fixture).unwrap();
        let endpoint = config.resolve_tag_url("fixture-tag-id", "").unwrap();
        assert_eq!(
            endpoint,
            "https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/fixture-installation-id/fixture-tag-id?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"
        );
        assert_eq!(config.tag_url_template().matches("{inid}").count(), 1);
    }

    #[test]
    fn config_parser_rejects_missing_ids_and_non_allowlisted_tag_routes() {
        assert!(matches!(
            parse_shazam_config(br#"{"apis":{"tag":{"href":"https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"}}}"#),
            Err(Error::BadResponse(_))
        ));
        assert!(matches!(
            parse_shazam_config(br#"{"inid":"fixture-id","apis":{"tag":{"href":"https://example.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"}}}"#),
            Err(Error::InvalidEndpoint(_))
        ));
    }

    #[test]
    fn endpoint_resolver_rejects_unsafe_inid_or_streaming_provider() {
        let template = "https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected={streamingprovider}&sharehub=false&hidelb=true";
        assert!(resolve_shazam_tag_url(template, "id/escape", "tag", "").is_err());
        assert!(resolve_shazam_tag_url(template, "id", "tag", "x&extra=y").is_err());
    }

    #[test]
    fn live_endpoint_allows_only_resolved_manual_tag_template() {
        let endpoint = "https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/installer-1/tag-2?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true";
        assert!(validate_shazam_endpoint(endpoint).is_ok());
        assert!(validate_shazam_endpoint(
            "https://example.com/discovery/v5/ja/JP/mac/-/tag/installer-1/tag-2?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"
        )
        .is_err());
        assert!(validate_shazam_endpoint(
            "https://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/{inid}/{tagid}?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"
        )
        .is_err());
        assert!(validate_shazam_endpoint(
            "http://amp.shazam.com/discovery/v5/ja/JP/mac/-/tag/installer-1/tag-2?sync=true&webv3=true&sampling=true&connected=&sharehub=false&hidelb=true"
        )
        .is_err());
    }

    #[test]
    fn loopback_http_roundtrip_posts_json_and_parses_mock_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            assert_eq!(request_line, "POST /mock HTTP/1.1\r\n");
            let mut content_length = None;
            let mut platform = None;
            let mut app_version = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.trim_end().split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        content_length = Some(value.trim().parse::<usize>().unwrap());
                    }
                    if name.eq_ignore_ascii_case("x-shazam-platform") {
                        platform = Some(value.trim().to_owned());
                    }
                    if name.eq_ignore_ascii_case("x-shazam-appversion") {
                        app_version = Some(value.trim().to_owned());
                    }
                }
            }
            assert_eq!(platform.as_deref(), Some("MAC"));
            assert_eq!(app_version.as_deref(), Some("2.11.0"));
            let mut body = vec![0; content_length.unwrap()];
            reader.read_exact(&mut body).unwrap();
            let parsed: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(parsed["signature"]["samplems"], 12_000);

            let response = br#"{"matches":[{"trackid":"mock-1","title":"Local Mock Song","artist":"Local Test Artist"}]}"#;
            let mut stream = reader.into_inner();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            )
            .unwrap();
            stream.write_all(response).unwrap();
        });

        let endpoint = format!("http://{address}/mock");
        let body = build_request_body(
            &valid_signature(),
            12_000,
            1_800_000_000_123,
            "Asia/Tokyo",
            json!({}),
            json!({}),
            None,
        )
        .unwrap();
        let response = post_loopback_json(&endpoint, &body).unwrap();
        assert_eq!(response.status, 200);
        let candidates = parse_track_candidates(&response.body).unwrap();
        assert_eq!(candidates[0].title, "Local Mock Song");
        assert_eq!(candidates[0].artist, "Local Test Artist");
        server.join().unwrap();
    }

    #[test]
    fn transport_refuses_non_loopback_and_https_destinations() {
        let body = b"{}";
        assert!(matches!(
            post_loopback_json("http://example.com:80/match", body),
            Err(Error::InvalidEndpoint(_))
        ));
        assert!(matches!(
            post_loopback_json("https://127.0.0.1:443/match", body),
            Err(Error::InvalidEndpoint(_))
        ));
    }
}
