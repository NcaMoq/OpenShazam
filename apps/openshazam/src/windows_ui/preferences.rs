#![deny(unsafe_op_in_unsafe_fn)]

use std::mem::size_of;

use super::{
    localization::{detect_ui_language, UiLanguage},
    wide, Theme,
};
use windows::{
    core::PCWSTR,
    Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE,
    },
};

const REGISTRY_PATH: &str = "Software\\OpenShazam";
const GLOBAL_SHORTCUT_MODIFIER_MASK: u32 = 0x0f;

pub(super) const DEFAULT_GLOBAL_SHORTCUT: GlobalShortcut = GlobalShortcut {
    modifiers: 0x03, // Ctrl + Alt
    key: 0x53,       // S
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u32)]
pub(super) enum ThemePreference {
    #[default]
    System = 0,
    Light = 1,
    Dark = 2,
}

impl ThemePreference {
    pub(super) fn from_raw(value: u32) -> Self {
        match value {
            1 => Self::Light,
            2 => Self::Dark,
            _ => Self::System,
        }
    }

    pub(super) fn resolve(self, system_theme: Theme) -> Theme {
        match self {
            Self::System => system_theme,
            Self::Light => Theme::Light,
            Self::Dark => Theme::Dark,
        }
    }

    pub(super) fn id(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GlobalShortcut {
    pub(super) modifiers: u32,
    pub(super) key: u32,
}

impl GlobalShortcut {
    pub(super) fn is_valid(self) -> bool {
        let modifier_key = matches!(self.key, 0x10..=0x12 | 0x5b..=0x5c | 0xa0..=0xa5);
        self.modifiers & !GLOBAL_SHORTCUT_MODIFIER_MASK == 0
            && (1..=0xff).contains(&self.key)
            && !modifier_key
            && self.key != 0x7b // F12 is reserved for debuggers by Windows.
    }

    pub(super) fn packed(self) -> u32 {
        (self.key << 16) | self.modifiers
    }

    pub(super) fn from_packed(value: u32) -> Option<Self> {
        let shortcut = Self {
            modifiers: value & GLOBAL_SHORTCUT_MODIFIER_MASK,
            key: (value >> 16) & 0xff,
        };
        shortcut.is_valid().then_some(shortcut)
    }
}

struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn open_for_read_at(path: &str) -> Option<RegistryKey> {
    let path = wide(path);
    let mut key = HKEY::default();
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            None,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    (result.0 == 0).then_some(RegistryKey(key))
}

fn open_for_read() -> Option<RegistryKey> {
    open_for_read_at(REGISTRY_PATH)
}

fn create_for_write_at(path: &str) -> Result<RegistryKey, i32> {
    let path = wide(path);
    let mut key = HKEY::default();
    let result = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            Some(0),
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    };
    if result.0 == 0 {
        Ok(RegistryKey(key))
    } else {
        Err(result.0 as i32)
    }
}

fn create_for_write() -> Result<RegistryKey, i32> {
    create_for_write_at(REGISTRY_PATH)
}

fn read_dword(key: &RegistryKey, name: &str) -> Option<u32> {
    let name = wide(name);
    let mut value = 0u32;
    let mut value_type = Default::default();
    let mut byte_count = size_of::<u32>() as u32;
    let result = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut value_type),
            Some((&mut value as *mut u32).cast()),
            Some(&mut byte_count),
        )
    };
    (result.0 == 0 && value_type == REG_DWORD && byte_count == size_of::<u32>() as u32)
        .then_some(value)
}

fn write_dword(key: &RegistryKey, name: &str, value: u32) -> Result<(), i32> {
    let name = wide(name);
    let bytes = value.to_ne_bytes();
    let result = unsafe {
        RegSetValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            Some(0),
            REG_DWORD,
            Some(&bytes),
        )
    };
    if result.0 == 0 {
        Ok(())
    } else {
        Err(result.0 as i32)
    }
}

pub(super) fn load_global_shortcut() -> GlobalShortcut {
    open_for_read()
        .and_then(|key| read_dword(&key, "GlobalShortcut"))
        .and_then(GlobalShortcut::from_packed)
        .unwrap_or(DEFAULT_GLOBAL_SHORTCUT)
}

pub(super) fn save_global_shortcut(shortcut: GlobalShortcut) -> Result<(), i32> {
    let key = create_for_write()?;
    write_dword(&key, "GlobalShortcut", shortcut.packed())
}

pub(super) fn load_theme_preference() -> ThemePreference {
    open_for_read()
        .and_then(|key| read_dword(&key, "AppearanceMode"))
        .map(ThemePreference::from_raw)
        .unwrap_or_default()
}

pub(super) fn save_theme_preference(preference: ThemePreference) {
    if let Ok(key) = create_for_write() {
        let _ = write_dword(&key, "AppearanceMode", preference.id());
    }
}

pub(super) fn load_auto_update_preference() -> bool {
    auto_update_enabled_from_raw(
        open_for_read().and_then(|key| read_dword(&key, "AutoUpdateEnabled")),
    )
}

fn auto_update_enabled_from_raw(value: Option<u32>) -> bool {
    value.map(|value| value != 0).unwrap_or(true)
}

pub(super) fn save_auto_update_preference(enabled: bool) -> Result<(), i32> {
    let key = create_for_write()?;
    write_dword(&key, "AutoUpdateEnabled", u32::from(enabled))
}

pub(super) fn load_ui_language_preference() -> UiLanguage {
    open_for_read()
        .and_then(|key| read_dword(&key, "Language"))
        .and_then(UiLanguage::from_id)
        .unwrap_or_else(detect_ui_language)
}

pub(super) fn save_ui_language_preference(language: UiLanguage) -> Result<(), i32> {
    let key = create_for_write()?;
    write_dword(&key, "Language", language.id())
}

pub(super) fn load_auto_shazam_preference() -> bool {
    open_for_read()
        .and_then(|key| read_dword(&key, "AutoShazamEnabled"))
        .is_some_and(|value| value != 0)
}

pub(super) fn save_auto_shazam_preference(enabled: bool) {
    if let Ok(key) = create_for_write() {
        let _ = write_dword(&key, "AutoShazamEnabled", u32::from(enabled));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use windows::Win32::System::Registry::RegDeleteKeyW;

    static TEST_KEY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TemporaryRegistryKey(String);

    impl TemporaryRegistryKey {
        fn create() -> Self {
            let sequence = TEST_KEY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = format!("Software\\OpenShazamTest-{}-{sequence}", std::process::id());
            let key = create_for_write_at(&path).expect("create isolated test registry key");
            drop(key);
            Self(path)
        }
    }

    impl Drop for TemporaryRegistryKey {
        fn drop(&mut self) {
            let path = wide(&self.0);
            unsafe {
                let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()));
            }
        }
    }

    #[test]
    fn appearance_preference_overrides_or_follows_the_windows_theme() {
        assert_eq!(ThemePreference::System.resolve(Theme::Light), Theme::Light);
        assert_eq!(ThemePreference::System.resolve(Theme::Dark), Theme::Dark);
        assert_eq!(ThemePreference::Light.resolve(Theme::Dark), Theme::Light);
        assert_eq!(ThemePreference::Dark.resolve(Theme::Light), Theme::Dark);
        assert_eq!(ThemePreference::from_raw(0), ThemePreference::System);
        assert_eq!(ThemePreference::from_raw(1), ThemePreference::Light);
        assert_eq!(ThemePreference::from_raw(2), ThemePreference::Dark);
        assert_eq!(ThemePreference::from_raw(u32::MAX), ThemePreference::System);
    }

    #[test]
    fn automatic_updates_default_to_enabled_and_respect_the_saved_toggle() {
        assert!(auto_update_enabled_from_raw(None));
        assert!(!auto_update_enabled_from_raw(Some(0)));
        assert!(auto_update_enabled_from_raw(Some(1)));
        assert!(auto_update_enabled_from_raw(Some(u32::MAX)));
    }

    #[test]
    fn shortcut_round_trip_preserves_modifiers_and_key() {
        let shortcut = DEFAULT_GLOBAL_SHORTCUT;
        assert!(shortcut.is_valid());
        assert_eq!(
            GlobalShortcut::from_packed(shortcut.packed()),
            Some(shortcut)
        );
    }

    #[test]
    fn shortcut_rejects_reserved_and_modifier_only_keys() {
        for key in [0x10, 0x5b, 0xa5, 0x7b, 0] {
            assert!(!GlobalShortcut {
                modifiers: 0x03,
                key,
            }
            .is_valid());
        }
        assert!(!GlobalShortcut {
            modifiers: 0x10,
            key: b'S' as u32,
        }
        .is_valid());
    }

    #[test]
    fn registry_dword_round_trip_uses_an_isolated_user_key() {
        let path = TemporaryRegistryKey::create();
        {
            let key = create_for_write_at(&path.0).expect("open isolated test registry key");
            write_dword(&key, "Preference", 0x1234_abcd).expect("write test DWORD");
        }

        let key = open_for_read_at(&path.0).expect("read isolated test registry key");
        assert_eq!(read_dword(&key, "Preference"), Some(0x1234_abcd));
        assert_eq!(read_dword(&key, "MissingPreference"), None);
    }
}
