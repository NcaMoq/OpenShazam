#![deny(unsafe_op_in_unsafe_fn)]

use std::sync::atomic::{AtomicU32, Ordering};

use windows::{
    core::PWSTR,
    Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum UiLanguage {
    Japanese,
    English,
}

impl UiLanguage {
    pub(super) fn id(self) -> u32 {
        match self {
            Self::Japanese => 1,
            Self::English => 2,
        }
    }

    pub(super) fn from_id(id: u32) -> Option<Self> {
        match id {
            1 => Some(Self::Japanese),
            2 => Some(Self::English),
            _ => None,
        }
    }
}

pub(super) fn directwrite_locale(language: UiLanguage) -> &'static str {
    match language {
        UiLanguage::Japanese => "ja-JP",
        UiLanguage::English => "en-US",
    }
}

static UI_LANGUAGE: AtomicU32 = AtomicU32::new(0);

pub(super) fn ui_language() -> UiLanguage {
    UiLanguage::from_id(UI_LANGUAGE.load(Ordering::Relaxed)).unwrap_or_else(|| {
        let detected = detect_ui_language();
        UI_LANGUAGE.store(detected.id(), Ordering::Relaxed);
        detected
    })
}

pub(super) fn set_ui_language(language: UiLanguage) {
    UI_LANGUAGE.store(language.id(), Ordering::Relaxed);
}

fn select_ui_language_from_multisz(buffer: &[u16]) -> UiLanguage {
    buffer
        .split(|unit| *unit == 0)
        .filter(|tag| !tag.is_empty())
        .filter_map(|tag| String::from_utf16(tag).ok())
        .find_map(|tag| {
            let primary = tag.split(['-', '_']).next()?.to_ascii_lowercase();
            match primary.as_str() {
                "ja" => Some(UiLanguage::Japanese),
                "en" => Some(UiLanguage::English),
                _ => None,
            }
        })
        .unwrap_or(UiLanguage::English)
}

pub(super) fn detect_ui_language() -> UiLanguage {
    let mut language_count = 0u32;
    let mut character_count = 0u32;
    // Windows reports the required MULTI_SZ size on the initial null-buffer
    // call. The Result is expected to be an insufficient-buffer error here.
    let _ = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut language_count,
            None,
            &mut character_count,
        )
    };
    if character_count == 0 || character_count > 65_536 {
        return UiLanguage::English;
    }

    let mut languages = vec![0u16; character_count as usize];
    if unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut language_count,
            Some(PWSTR(languages.as_mut_ptr())),
            &mut character_count,
        )
    }
    .is_err()
    {
        return UiLanguage::English;
    }
    select_ui_language_from_multisz(&languages[..character_count as usize])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum UiTextKey {
    Ready,
    AutoStartStatus,
    AutoOn,
    AutoTurnOff,
    Starting,
    Cancelling,
    Listening,
    NoMatchTitle,
    NoMatchBody,
    OfflineBody,
    TryAgain,
    TimeoutDescription,
    TimeoutDismiss,
    TimeoutRestart,
}

pub(super) fn localized_ui_text(key: UiTextKey, language: UiLanguage) -> &'static str {
    match (key, language) {
        (UiTextKey::Ready, UiLanguage::Japanese) => "クリックしてShazamする",
        (UiTextKey::Ready, UiLanguage::English) => "Click to Shazam",
        (UiTextKey::AutoStartStatus, UiLanguage::Japanese) => {
            "\u{30c0}\u{30d6}\u{30eb}\u{30af}\u{30ea}\u{30c3}\u{30af}\u{3057}\u{3066}\u{81ea}\u{52d5}Shazam\u{3059}\u{308b}"
        }
        (UiTextKey::AutoStartStatus, UiLanguage::English) => "Double click to Auto Shazam",
        (UiTextKey::AutoOn, UiLanguage::Japanese) => {
            "Auto Shazam\u{304c}\u{30aa}\u{30f3}\u{306b}\u{306a}\u{3063}\u{3066}\u{3044}\u{307e}\u{3059}"
        }
        (UiTextKey::AutoOn, UiLanguage::English) => "Auto Shazam is on",
        (UiTextKey::AutoTurnOff, UiLanguage::Japanese) => {
            "\u{30af}\u{30ea}\u{30c3}\u{30af}\u{3057}\u{3066}\u{30aa}\u{30d5}\u{306b}\u{3059}\u{308b}"
        }
        (UiTextKey::AutoTurnOff, UiLanguage::English) => "Click to turn off",
        (UiTextKey::Starting, UiLanguage::Japanese) => "\u{958b}\u{59cb}\u{4e2d}",
        (UiTextKey::Starting, UiLanguage::English) => "Starting",
        (UiTextKey::Cancelling, UiLanguage::Japanese) => "\u{505c}\u{6b62}\u{4e2d}",
        (UiTextKey::Cancelling, UiLanguage::English) => "Stopping",
        (UiTextKey::Listening, UiLanguage::Japanese) => "聞き取り中",
        (UiTextKey::Listening, UiLanguage::English) => "Listening",
        (UiTextKey::NoMatchTitle, UiLanguage::Japanese) => "結果なし",
        (UiTextKey::NoMatchTitle, UiLanguage::English) => "No Result",
        (UiTextKey::NoMatchBody, UiLanguage::Japanese) => {
            "認識できませんでした。もう一度お試しください。"
        }
        (UiTextKey::NoMatchBody, UiLanguage::English) => {
            "We didn’t quite catch that. Please try again."
        }
        (UiTextKey::OfflineBody, UiLanguage::Japanese) => "接続を確認してください",
        (UiTextKey::OfflineBody, UiLanguage::English) => "Please check your connection",
        (UiTextKey::TryAgain, UiLanguage::Japanese) => "再度試す",
        (UiTextKey::TryAgain, UiLanguage::English) => "Try Again",
        (UiTextKey::TimeoutDescription, UiLanguage::Japanese) => {
            "引き続き聴きたい？「再起動」をクリックして Shazam を続けましょう！"
        }
        (UiTextKey::TimeoutDescription, UiLanguage::English) => {
            "Still listening? Click ‘Restart’ to keep Shazaming!"
        }
        (UiTextKey::TimeoutDismiss, UiLanguage::Japanese) => "閉じる",
        (UiTextKey::TimeoutDismiss, UiLanguage::English) => "Dismiss",
        (UiTextKey::TimeoutRestart, UiLanguage::Japanese) => "再起動",
        (UiTextKey::TimeoutRestart, UiLanguage::English) => "Restart",
    }
}

pub(super) fn no_match_copy(offline: bool, language: UiLanguage) -> (&'static str, &'static str) {
    let title = localized_ui_text(UiTextKey::NoMatchTitle, language);
    let body = localized_ui_text(
        if offline {
            UiTextKey::OfflineBody
        } else {
            UiTextKey::NoMatchBody
        },
        language,
    );
    (title, body)
}

pub(super) fn timeout_copy(language: UiLanguage) -> (&'static str, &'static str, &'static str) {
    (
        localized_ui_text(UiTextKey::TimeoutDescription, language),
        localized_ui_text(UiTextKey::TimeoutDismiss, language),
        localized_ui_text(UiTextKey::TimeoutRestart, language),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        directwrite_locale, localized_ui_text, no_match_copy, select_ui_language_from_multisz,
        timeout_copy, UiLanguage, UiTextKey,
    };

    #[test]
    fn preferred_ui_languages_choose_first_supported_language_and_fallback_to_english() {
        let preferred = "fr-FR\0ja-JP\0en-US\0\0";
        let encoded: Vec<u16> = preferred.encode_utf16().collect();
        assert_eq!(
            select_ui_language_from_multisz(&encoded),
            UiLanguage::Japanese
        );
        assert_eq!(directwrite_locale(UiLanguage::Japanese), "ja-JP");
        let english: Vec<u16> = "de-DE\0en-US\0\0".encode_utf16().collect();
        assert_eq!(
            select_ui_language_from_multisz(&english),
            UiLanguage::English
        );
        assert_eq!(directwrite_locale(UiLanguage::English), "en-US");
        let unsupported: Vec<u16> = "fr-FR\0de-DE\0\0".encode_utf16().collect();
        assert_eq!(
            select_ui_language_from_multisz(&unsupported),
            UiLanguage::English
        );
    }

    #[test]
    fn localized_recognition_copy_uses_expected_english_product_strings() {
        assert_eq!(
            localized_ui_text(UiTextKey::Ready, UiLanguage::English),
            "Click to Shazam"
        );
        assert_eq!(
            localized_ui_text(UiTextKey::AutoStartStatus, UiLanguage::English),
            "Double click to Auto Shazam"
        );
        assert_eq!(
            localized_ui_text(UiTextKey::AutoOn, UiLanguage::English),
            "Auto Shazam is on"
        );
        assert_eq!(
            localized_ui_text(UiTextKey::AutoTurnOff, UiLanguage::English),
            "Click to turn off"
        );
        assert_eq!(no_match_copy(false, UiLanguage::English).0, "No Result");
        assert_eq!(
            no_match_copy(true, UiLanguage::English).1,
            "Please check your connection"
        );
        assert_eq!(timeout_copy(UiLanguage::English).1, "Dismiss");
        assert_eq!(timeout_copy(UiLanguage::English).2, "Restart");
    }
}
