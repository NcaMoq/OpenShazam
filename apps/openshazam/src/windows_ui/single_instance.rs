//! Win32 single-instance ownership and activation for the native UI host.
#![deny(unsafe_op_in_unsafe_fn)]

use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HANDLE, LPARAM, WPARAM},
        System::Threading::CreateMutexW,
        UI::WindowsAndMessaging::{FindWindowW, PostMessageW},
    },
};

use std::{thread, time::Duration};
use windows::Win32::Foundation::{CloseHandle, GetLastError};

const SINGLE_INSTANCE_MUTEX: &str = "Local\\OpenShazam.WindowsUi.Singleton";
const ERROR_ALREADY_EXISTS: u32 = 183;

/// Owns the kernel mutex handle for the full lifetime of the UI process.
pub(super) struct SingleInstanceMutex(HANDLE);

impl Drop for SingleInstanceMutex {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub(super) fn acquire_single_instance_mutex() -> windows::core::Result<(SingleInstanceMutex, bool)>
{
    acquire_named_single_instance_mutex(SINGLE_INSTANCE_MUTEX)
}

fn acquire_named_single_instance_mutex(
    name: &str,
) -> windows::core::Result<(SingleInstanceMutex, bool)> {
    let name = wide(name);
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }?;
    let already_running = unsafe { GetLastError().0 } == ERROR_ALREADY_EXISTS;
    Ok((SingleInstanceMutex(handle), already_running))
}

/// Ask the existing UI host to show its main popup after the mutex confirms
/// that another process owns the singleton instance.
pub(super) fn activate_existing_instance(class_name: &str, show_message: u32) -> bool {
    let class_name = wide(class_name);
    for _ in 0..50 {
        let hwnd = unsafe {
            // The main popup is an undecorated WS_POPUP tool window. Windows
            // does not consistently expose its creation text through
            // FindWindow's title filter, so locate the host by its private
            // window class instead.
            FindWindowW(PCWSTR(class_name.as_ptr()), PCWSTR::null()).unwrap_or_default()
        };
        if !hwnd.0.is_null() {
            return unsafe { PostMessageW(Some(hwnd), show_message, WPARAM(0), LPARAM(0)).is_ok() };
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::acquire_named_single_instance_mutex;

    #[test]
    fn named_single_instance_mutex_tracks_live_handle_lifetime() {
        let name = format!("Local\\OpenShazam.WindowsUi.Test.{}", std::process::id());
        let (first, already_running) = acquire_named_single_instance_mutex(&name).unwrap();
        assert!(!already_running);

        let (second, already_running) = acquire_named_single_instance_mutex(&name).unwrap();
        assert!(already_running);
        drop(second);

        let (while_first_is_alive, already_running) =
            acquire_named_single_instance_mutex(&name).unwrap();
        assert!(already_running);
        drop(while_first_is_alive);
        drop(first);

        let (_after_last_handle_is_closed, already_running) =
            acquire_named_single_instance_mutex(&name).unwrap();
        assert!(!already_running);
    }
}
