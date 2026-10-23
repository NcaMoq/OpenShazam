#![deny(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;

use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::HWND,
        UI::{
            Shell::{ShellExecuteExW, SEE_MASK_DEFAULT, SHELLEXECUTEINFOW},
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
};

use super::{
    page_link::{resolve_page_link, resolve_page_link_checked, PageLinkFailure},
    wide, AppState, MainPhase, Track, UiConfigSnapshot, ValidatedWebTrackUrl,
};

/// Launch a validated Shazam URL using the current window as the Shell owner.
///
/// # Safety
/// `hwnd` must be a live window handle owned by the current UI thread.
pub(super) unsafe fn open_track_page(
    hwnd: HWND,
    url: &ValidatedWebTrackUrl,
) -> windows::core::Result<()> {
    let verb = wide("open");
    let target = wide(url.as_str());
    let mut execute = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_DEFAULT,
        hwnd,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(target.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `hwnd` is guaranteed live by the caller contract, and both
    // command strings remain NUL-terminated and alive for this synchronous call.
    unsafe { ShellExecuteExW(&mut execute) }
}

fn track_for_display_id<'a>(
    history: &'a [Track],
    current_result: Option<&'a Track>,
    display_id: u64,
) -> Option<&'a Track> {
    history
        .iter()
        .find(|track| track.display_id == display_id)
        .or_else(|| current_result.filter(|track| track.display_id == display_id))
}

pub(super) fn resolve_track_page_url(
    track: &Track,
    ui_configs: &HashMap<u64, UiConfigSnapshot>,
) -> Option<ValidatedWebTrackUrl> {
    resolve_track_page_url_checked(track, ui_configs).ok()
}

fn resolve_track_page_url_checked(
    track: &Track,
    ui_configs: &HashMap<u64, UiConfigSnapshot>,
) -> Result<ValidatedWebTrackUrl, PageLinkFailure> {
    if let Some(url) = track.page_url.as_ref() {
        return Ok(url.clone());
    }
    resolve_page_link_for_generation(
        track.config_generation,
        ui_configs,
        track.track_key.as_deref(),
    )
}

pub(super) fn resolve_page_link_for_generation(
    config_generation: Option<u64>,
    ui_configs: &HashMap<u64, UiConfigSnapshot>,
    track_key: Option<&str>,
) -> Result<ValidatedWebTrackUrl, PageLinkFailure> {
    let generation = config_generation.ok_or(PageLinkFailure::MissingConfigGeneration)?;
    let config = ui_configs
        .get(&generation)
        .ok_or(PageLinkFailure::MissingConfigSnapshot)?;
    resolve_page_link_checked(config.web_track_url_template.as_deref(), track_key)
}

pub(super) fn refresh_track_page_urls(app: &mut AppState) {
    for track in &mut app.history {
        if track.page_url.is_none() {
            track.page_url = track
                .config_generation
                .and_then(|generation| app.ui_configs.get(&generation))
                .and_then(|config| {
                    resolve_page_link(
                        config.web_track_url_template.as_deref(),
                        track.track_key.as_deref(),
                    )
                });
        }
    }
    if let MainPhase::Matched { track, .. } = &mut app.phase {
        if track.page_url.is_none() {
            track.page_url = track
                .config_generation
                .and_then(|generation| app.ui_configs.get(&generation))
                .and_then(|config| {
                    resolve_page_link(
                        config.web_track_url_template.as_deref(),
                        track.track_key.as_deref(),
                    )
                });
        }
    }
}

pub(super) fn page_url_for_display_id(
    history: &[Track],
    current_result: Option<&Track>,
    display_id: u64,
    ui_configs: &HashMap<u64, UiConfigSnapshot>,
) -> Option<ValidatedWebTrackUrl> {
    track_for_display_id(history, current_result, display_id)
        .and_then(|track| resolve_track_page_url(track, ui_configs))
}
