//! Validation and encoding for Shazam track-page links.
//!
//! Keeping this boundary independent from window and rendering code makes it
//! harder for untrusted service metadata to reach ShellExecute without the
//! same HTTPS-host and track-key checks.

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ValidatedWebTrackUrl(String);

impl ValidatedWebTrackUrl {
    pub(super) fn from_url(url: String) -> Option<Self> {
        valid_shazam_page_url(&url).then_some(Self(url))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PageLinkFailure {
    MissingConfigGeneration,
    MissingConfigSnapshot,
    MissingTemplate,
    InvalidTemplate,
    MissingTrackKey,
    InvalidTrackKey,
    InvalidResolvedUrl,
}

impl PageLinkFailure {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::MissingConfigGeneration => "missing_config_generation",
            Self::MissingConfigSnapshot => "missing_config_snapshot",
            Self::MissingTemplate => "missing_webtrackurl",
            Self::InvalidTemplate => "invalid_webtrackurl_template",
            Self::MissingTrackKey => "missing_track_key",
            Self::InvalidTrackKey => "invalid_track_key",
            Self::InvalidResolvedUrl => "invalid_resolved_url",
        }
    }
}

pub(super) fn valid_shazam_page_url(url: &str) -> bool {
    if url.len() > 4_096
        || !url.starts_with("https://")
        || url.chars().any(char::is_control)
        || url.contains('@')
        || url.contains('\\')
    {
        return false;
    }
    let authority = url[8..].split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .strip_suffix(":443")
        .unwrap_or(authority)
        .to_ascii_lowercase();
    (host == "shazam.com" || host.ends_with(".shazam.com"))
        && host
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-'))
}

pub(super) fn valid_page_template(template: &str) -> bool {
    valid_shazam_page_url(template) && template.matches("{trackid}").count() == 1
}

pub(super) fn resolve_page_link(
    template: Option<&str>,
    track_key: Option<&str>,
) -> Option<ValidatedWebTrackUrl> {
    resolve_page_link_checked(template, track_key).ok()
}

pub(super) fn resolve_page_link_checked(
    template: Option<&str>,
    track_key: Option<&str>,
) -> Result<ValidatedWebTrackUrl, PageLinkFailure> {
    let template = template.ok_or(PageLinkFailure::MissingTemplate)?;
    let track_key = track_key.ok_or(PageLinkFailure::MissingTrackKey)?;
    const TOKEN: &str = "{trackid}";
    if !valid_page_template(template) {
        return Err(PageLinkFailure::InvalidTemplate);
    }
    if track_key.is_empty() || track_key.len() > 1_024 {
        return Err(PageLinkFailure::InvalidTrackKey);
    }
    let mut encoded = String::with_capacity(track_key.len());
    for byte in track_key.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    let resolved = template.replacen(TOKEN, &encoded, 1);
    if resolved.contains('{') || resolved.contains('}') || !valid_shazam_page_url(&resolved) {
        return Err(PageLinkFailure::InvalidResolvedUrl);
    }
    Ok(ValidatedWebTrackUrl(resolved))
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_page_link_checked, valid_page_template, PageLinkFailure, ValidatedWebTrackUrl,
    };

    #[test]
    fn accepts_only_https_shazam_track_templates_and_encodes_keys() {
        let template = "https://www.shazam.com/track/{trackid}#referrer=shazamapp";
        assert!(valid_page_template(template));
        assert_eq!(
            resolve_page_link_checked(Some(template), Some("track/キー")),
            Ok(ValidatedWebTrackUrl::from_url(
                "https://www.shazam.com/track/track%2F%E3%82%AD%E3%83%BC#referrer=shazamapp"
                    .to_owned(),
            )
            .unwrap())
        );
    }

    #[test]
    fn rejects_untrusted_hosts_and_ambiguous_templates() {
        for template in [
            "http://www.shazam.com/track/{trackid}",
            "https://shazam.com.evil.test/track/{trackid}",
            "https://user@www.shazam.com/track/{trackid}",
            "https://www.shazam.com:444/track/{trackid}",
            "https://www.shazam.com/track/{trackid}/{trackid}",
        ] {
            assert!(!valid_page_template(template), "accepted {template}");
        }
        assert!(
            ValidatedWebTrackUrl::from_url("https://attacker.example/track/1".to_owned()).is_none()
        );
    }

    #[test]
    fn reports_missing_and_invalid_track_inputs_without_returning_them() {
        let template = "https://www.shazam.com/track/{trackid}";
        assert_eq!(
            resolve_page_link_checked(None, Some("private-track-key")),
            Err(PageLinkFailure::MissingTemplate)
        );
        assert_eq!(
            resolve_page_link_checked(Some(template), None),
            Err(PageLinkFailure::MissingTrackKey)
        );
        assert_eq!(
            resolve_page_link_checked(Some(template), Some("")),
            Err(PageLinkFailure::InvalidTrackKey)
        );
        assert_eq!(
            resolve_page_link_checked(Some(template), Some(&"k".repeat(1_025))),
            Err(PageLinkFailure::InvalidTrackKey)
        );
    }
}
