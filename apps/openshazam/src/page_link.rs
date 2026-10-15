//! Safe resolution of track pages from live Shazam Config, never from a
//! guessed hard-coded fallback URL.
pub fn resolve(template: Option<&str>, track_key: Option<&str>) -> Option<String> {
    let template = template?;
    let key = track_key?;
    if key.is_empty() || key.len() > 1024 || !valid_page_url(template) ||
       template.matches("{trackid}").count() != 1 {
        return None;
    }
    let mut encoded = String::with_capacity(key.len());
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    let url = template.replacen("{trackid}", &encoded, 1);
    if url.contains('{') || url.contains('}') || !valid_page_url(&url) {
        return None;
    }
    Some(url)
}

pub fn valid_page_url(value: &str) -> bool {
    if value.len() > 4096 || !value.starts_with("https://") ||
       value.chars().any(char::is_control) ||
       value.contains(['#', '@', '\\']) {
        return false;
    }
    let authority = value[8..].split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.strip_suffix(":443").unwrap_or(authority).to_ascii_lowercase();
    (host == "shazam.com" || host.ends_with(".shazam.com")) &&
        host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
}

#[cfg(test)]
mod tests {
    use super::{resolve, valid_page_url};

    #[test]
    fn resolves_only_live_config_trackid_token() {
        assert_eq!(
            resolve(Some("https://www.shazam.com/track/{trackid}"), Some("a/b")),
            Some("https://www.shazam.com/track/a%2Fb".into())
        );
        assert_eq!(resolve(None, Some("abc")), None);
        assert_eq!(resolve(Some("https://www.shazam.com/track/{trackid}"), None), None);
        assert_eq!(resolve(Some("https://www.shazam.com/track/abc"), Some("abc")), None);
    }

    #[test]
    fn disallows_untrusted_hosts_or_url_injection() {
        for template in [
            "http://www.shazam.com/track/{trackid}",
            "https://shazam.com.evil.test/track/{trackid}",
            "https://shazam.com@evil.test/{trackid}",
            "https://shazam.com:444/{trackid}",
            "https://www.shazam.com/{trackid}#{trackid}",
        ] {
            assert_eq!(resolve(Some(template), Some("test")), None);
        }
        assert!(!valid_page_url("https://evil.test/track/1"));
    }
}
