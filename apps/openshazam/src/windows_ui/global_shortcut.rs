//! Global keyboard shortcut registration and capture suspension.
#![deny(unsafe_op_in_unsafe_fn)]

use super::preferences::{save_global_shortcut, GlobalShortcut};
use super::{AppState, GLOBAL_SHORTCUT_ID_PRIMARY, GLOBAL_SHORTCUT_ID_SECONDARY};
use windows::Win32::{
    Foundation::{HWND, LRESULT},
    Graphics::Gdi::InvalidateRect,
    UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
    },
};

pub(super) fn register_global_shortcut(
    hwnd: HWND,
    registration_id: i32,
    shortcut: GlobalShortcut,
) -> Result<(), i32> {
    if !shortcut.is_valid() {
        return Err(87); // ERROR_INVALID_PARAMETER
    }
    // SAFETY: Callers pass the live main HWND owned by the UI thread's AppState.
    unsafe {
        RegisterHotKey(
            Some(hwnd),
            registration_id,
            HOT_KEY_MODIFIERS(shortcut.modifiers | MOD_NOREPEAT.0),
            shortcut.key,
        )
    }
    .map_err(|error| error.code().0)
}

fn unregister_global_shortcut(hwnd: HWND, registration_id: i32) -> Result<(), i32> {
    // SAFETY: Callers pass an ID previously registered for the live main HWND.
    unsafe { UnregisterHotKey(Some(hwnd), registration_id) }.map_err(|error| error.code().0)
}

fn update_global_shortcut(app: &mut AppState, shortcut: GlobalShortcut) -> Result<(), i32> {
    if !shortcut.is_valid() {
        return Err(87);
    }
    if app.global_shortcut == shortcut && app.global_shortcut_registration_id.is_some() {
        return Ok(());
    }

    // Register with the alternate ID first. If Windows reports a conflict,
    // the old binding stays active and the user can continue using it.
    let next_id = match app.global_shortcut_registration_id {
        Some(GLOBAL_SHORTCUT_ID_PRIMARY) => GLOBAL_SHORTCUT_ID_SECONDARY,
        _ => GLOBAL_SHORTCUT_ID_PRIMARY,
    };
    register_global_shortcut(app.main_hwnd, next_id, shortcut)?;
    if let Err(error) = save_global_shortcut(shortcut) {
        let _ = unregister_global_shortcut(app.main_hwnd, next_id);
        return Err(error);
    }

    if let Some(previous_id) = app.global_shortcut_registration_id {
        if let Err(error) = unregister_global_shortcut(app.main_hwnd, previous_id) {
            let _ = unregister_global_shortcut(app.main_hwnd, next_id);
            let _ = save_global_shortcut(app.global_shortcut);
            return Err(error);
        }
    }
    app.global_shortcut = shortcut;
    app.global_shortcut_registration_id = Some(next_id);
    Ok(())
}

fn set_global_shortcut_capture(app: &mut AppState, suspended: bool) -> Result<(), i32> {
    if suspended {
        if app.global_shortcut_capture_suspended {
            return Ok(());
        }
        if let Some(registration_id) = app.global_shortcut_registration_id {
            unregister_global_shortcut(app.main_hwnd, registration_id)?;
            app.global_shortcut_registration_id = None;
        }
        app.global_shortcut_capture_suspended = true;
        return Ok(());
    }

    if !app.global_shortcut_capture_suspended && app.global_shortcut_registration_id.is_some() {
        return Ok(());
    }
    app.global_shortcut_capture_suspended = false;
    if app.global_shortcut_registration_id.is_none() {
        register_global_shortcut(
            app.main_hwnd,
            GLOBAL_SHORTCUT_ID_PRIMARY,
            app.global_shortcut,
        )?;
        app.global_shortcut_registration_id = Some(GLOBAL_SHORTCUT_ID_PRIMARY);
    }
    Ok(())
}

/// # Safety
/// `app_ptr` must be null or point to the live state of the currently-dispatching window.
pub(super) unsafe fn update_global_shortcut_after_wndproc(
    app_ptr: *mut AppState,
    modifiers: u32,
    key: u32,
) -> LRESULT {
    // SAFETY: The WndProc contract guarantees that a non-null pointer remains
    // live for the duration of this synchronous dispatch.
    let Some(app) = (unsafe { app_ptr.as_mut() }) else {
        return LRESULT(0);
    };
    match update_global_shortcut(app, GlobalShortcut { modifiers, key }) {
        Ok(()) => LRESULT(1),
        Err(error) => {
            app.message = Some(format!(
                "Windows could not register this shortcut (error {error})."
            ));
            if app.visible {
                // SAFETY: The main HWND is owned by the live AppState being
                // handled synchronously on its UI thread.
                let _ = unsafe { InvalidateRect(Some(app.main_hwnd), None, false) };
            }
            LRESULT(0)
        }
    }
}

/// # Safety
/// `app_ptr` must be null or point to the live state of the currently-dispatching window.
pub(super) unsafe fn set_global_shortcut_capture_after_wndproc(
    app_ptr: *mut AppState,
    suspended: bool,
) -> LRESULT {
    // SAFETY: The WndProc contract guarantees that a non-null pointer remains
    // live for the duration of this synchronous dispatch.
    let Some(app) = (unsafe { app_ptr.as_mut() }) else {
        return LRESULT(0);
    };
    match set_global_shortcut_capture(app, suspended) {
        Ok(()) => LRESULT(1),
        Err(error) => {
            eprintln!("Could not update global shortcut capture state (error {error}).");
            LRESULT(0)
        }
    }
}
