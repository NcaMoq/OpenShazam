#![deny(unsafe_op_in_unsafe_fn)]

use super::{
    load_application_icon, notifications, open_settings, ui_language, wide, AppState, Track,
    TrackIdentity, UiLanguage, TRAY_ICON_GUID, TRAY_ICON_ID, TRAY_MENU_EXIT, TRAY_MENU_SETTINGS,
    WM_TRAYICON_MESSAGE,
};
use std::mem::size_of;
use windows::{
    core::{Error, PCWSTR},
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{
                Shell_NotifyIconW, NIF_GUID, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD,
                NIM_DELETE, NIM_SETFOCUS, NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos, PostMessageW,
                PostQuitMessage, SetForegroundWindow, TrackPopupMenu, MF_SEPARATOR, MF_STRING,
                TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
            },
        },
    },
};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TrayMenuAction {
    None,
    Settings,
    Exit,
}

pub(super) fn tray_menu_action(selected: usize) -> TrayMenuAction {
    match selected {
        TRAY_MENU_SETTINGS => TrayMenuAction::Settings,
        TRAY_MENU_EXIT => TrayMenuAction::Exit,
        _ => TrayMenuAction::None,
    }
}

pub(super) fn is_tray_activation_event(event: u16) -> bool {
    // With NOTIFYICON_VERSION_4 the shell reports pointer messages (including
    // WM_LBUTTONUP) as well as the semantic selection notification. Treating
    // both as toggles makes one physical click show and immediately hide the
    // popup. NIN_SELECT/NIN_KEYSELECT are the single activation events.
    event == windows::Win32::UI::Shell::NIN_SELECT as u16
        || event
            == (windows::Win32::UI::Shell::NIN_SELECT | windows::Win32::UI::Shell::NINF_KEY) as u16
}

/// # Safety
/// `hwnd` must be a live window owned by this process for the registration.
pub(super) unsafe fn add_tray_icon(hwnd: HWND) -> windows::core::Result<()> {
    // SAFETY: The current process module handle is queried without retaining
    // pointers; the caller guarantees the owner window remains live.
    let instance = unsafe { GetModuleHandleW(None)? };
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP | NIF_GUID,
        uCallbackMessage: WM_TRAYICON_MESSAGE,
        hIcon: unsafe { load_application_icon(instance.into()) },
        guidItem: TRAY_ICON_GUID,
        ..Default::default()
    };
    let tip = wide("Shazam");
    for (dst, src) in data.szTip.iter_mut().zip(tip) {
        *dst = src;
    }
    // SAFETY: `data` is fully initialized, `szTip` is NUL-terminated, and
    // `hwnd` is live for the duration required by the shell registration.
    if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
        return Err(Error::from_thread());
    }
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    // SAFETY: This updates the registration created immediately above; on
    // failure the same stable GUID/data are used to remove that registration.
    if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) }.as_bool() {
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        return Err(Error::from_thread());
    }
    Ok(())
}

/// # Safety
/// Must run on the UI thread while `app.main_hwnd` remains a live window.
pub(super) unsafe fn restore_tray_icon(app: &mut AppState) {
    // Explorer drops notification-area registrations when its taskbar process
    // restarts. Its old balloon notifications are gone too, so release their
    // icons before re-adding the stable GUID with version 4 callback semantics.
    for icon in take_queued_balloon_icons(&mut app.owned_balloon_icons) {
        // SAFETY: Each queued handle is still owned by AppState and Explorer
        // has restarted, so no outstanding balloon can reference these icons.
        let _ = unsafe { DestroyIcon(icon) };
    }
    // SAFETY: The app's main window remains live while Explorer's icon is restored.
    let _ = unsafe { add_tray_icon(app.main_hwnd) };
}

fn take_queued_balloon_icons<T>(
    icons: &mut std::collections::VecDeque<Option<T>>,
) -> impl Iterator<Item = T> + '_ {
    icons.drain(..).flatten()
}

/// # Safety
/// Must run on the UI thread with `hwnd` referring to a live owner window.
pub(super) unsafe fn track_tray_context_menu(hwnd: HWND) -> usize {
    // SAFETY: Creates a new menu owned by this call and destroyed before return.
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return 0;
    };
    let language = ui_language();
    let settings = wide(match language {
        UiLanguage::Japanese => "設定",
        UiLanguage::English => "Settings",
    });
    let exit = wide(match language {
        UiLanguage::Japanese => "Shazam を終了",
        UiLanguage::English => "Exit Shazam",
    });
    // SAFETY: Menu and UTF-16 strings remain live until TrackPopupMenu returns.
    let _ = unsafe {
        AppendMenuW(
            menu,
            MF_STRING,
            TRAY_MENU_SETTINGS,
            PCWSTR(settings.as_ptr()),
        )
    };
    // SAFETY: `menu` is valid and all referenced string storage remains alive.
    let _ = unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) };
    let _ = unsafe { AppendMenuW(menu, MF_STRING, TRAY_MENU_EXIT, PCWSTR(exit.as_ptr())) };
    // SAFETY: The caller guarantees a live window owned by this process.
    let _ = unsafe { SetForegroundWindow(hwnd) };
    let mut point = POINT::default();
    // SAFETY: `point` is a writable stack value for GetCursorPos.
    let _ = unsafe { GetCursorPos(&mut point) };
    // SAFETY: `menu` and `hwnd` remain live through this synchronous call.
    let selected = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            None,
            hwnd,
            None,
        )
    };
    // SAFETY: The menu was created above and is owned by this function.
    let _ = unsafe { DestroyMenu(menu) };
    // SAFETY: `hwnd` remains live throughout the current UI callback.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0)) };
    let data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_GUID,
        guidItem: TRAY_ICON_GUID,
        ..Default::default()
    };
    // SAFETY: This references the live tray icon using its stable GUID.
    let _ = unsafe { Shell_NotifyIconW(NIM_SETFOCUS, &data) };
    selected.0 as usize
}

/// # Safety
/// Must run on the UI thread with AppState valid for the active window callback.
pub(super) unsafe fn apply_tray_menu_action(app: &mut AppState, selected: usize) -> LRESULT {
    match tray_menu_action(selected) {
        // SAFETY: The caller provides the live UI AppState and runs on its UI thread.
        TrayMenuAction::Settings => unsafe { open_settings(app) },
        // SAFETY: This function is invoked only on the UI thread's message loop.
        TrayMenuAction::Exit => unsafe { PostQuitMessage(0) },
        TrayMenuAction::None => {}
    }
    LRESULT(0)
}

pub(super) fn queue_hidden_match_notification(app: &mut AppState, track: &Track) {
    let display_id = track.display_id;
    if let Some(pixels) = app
        .artwork_resources
        .get(&display_id)
        .map(|(_, pixels)| pixels.clone())
    {
        notifications::notify_hidden_match(app, track, Some(&pixels));
    } else if app
        .active_artwork_requests
        .contains_key(&TrackIdentity { display_id })
    {
        // Wait for the worker so the first notification can use the album cover.
        app.pending_match_notifications
            .insert(display_id, track.clone());
    } else {
        notifications::notify_hidden_match(app, track, None);
    }
}

/// # Safety
/// `hwnd` must identify the window used when the tray icon was registered.
pub(super) unsafe fn delete_tray_icon(hwnd: HWND) {
    let data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_GUID,
        guidItem: TRAY_ICON_GUID,
        ..Default::default()
    };
    // SAFETY: The fully initialized data identifies the icon registered for
    // this still-valid owner window by its stable GUID.
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
}

#[cfg(test)]
mod tests {
    use super::take_queued_balloon_icons;
    use std::collections::VecDeque;

    #[test]
    fn explorer_restart_releases_queued_notification_icon_ownership_in_order() {
        let mut icons = VecDeque::from([Some(11), None, Some(29)]);

        assert_eq!(
            take_queued_balloon_icons(&mut icons).collect::<Vec<_>>(),
            [11, 29]
        );
        assert!(icons.is_empty());
    }
}
