//! Lazy, same-process C ABI to the C++/WinRT WinUI 3 Settings island.
//! The DLL is loaded by its absolute path next to the product executable.
//! Neither the recognition controller nor audio buffers ever cross this ABI.
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

use std::{
    ffi::{c_char, c_void, OsStr},
    os::windows::ffi::OsStrExt,
    path::PathBuf,
};

pub(crate) type Show =
    unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32, *const u16) -> i32;
type UpdateHistory = unsafe extern "system" fn(*const SettingsHistoryItem, usize);
type UpdateStatus = unsafe extern "system" fn(i32, u32, *const u16, *const u16, u64);
type Translate = unsafe extern "system" fn(*mut c_void) -> i32;
type IsVisible = unsafe extern "system" fn() -> i32;
type Shutdown = unsafe extern "system" fn();
type BootstrapInitialize = unsafe extern "system" fn(u32, *const u16, u64) -> i32;
type BootstrapShutdown = unsafe extern "system" fn();

#[repr(C)]
pub(crate) struct SettingsHistoryItem {
    pub title: *const u16,
    pub artist: *const u16,
    pub recognized_at: *const u16,
    pub page_url: *const u16,
}

// Keep these values aligned with the Windows App SDK 1.8 package pinned by
// settings-host/packages.config and its WindowsAppSDK-VersionInfo.h.
const WINDOWSAPPSDK_RELEASE_MAJORMINOR: u32 = 0x0001_0008;
const WINDOWSAPPSDK_RUNTIME_MIN_VERSION: u64 = 0x1F40_0326_08CC_0000;
const WINDOWSAPPSDK_RELEASE_VERSION_TAG: [u16; 1] = [0];
const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;
const PRODUCT_DLL_SEARCH_FLAGS: u32 =
    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}

pub struct SettingsHost {
    library: *mut c_void,
    show: Show,
    update_history: UpdateHistory,
    update_status: UpdateStatus,
    pre_translate: Translate,
    is_visible: IsVisible,
    shutdown: Shutdown,
    bootstrap_library: *mut c_void,
    bootstrap_shutdown: Option<BootstrapShutdown>,
}

impl SettingsHost {
    pub fn load() -> Result<Self, String> {
        let executable = std::env::current_exe()
            .map_err(|error| format!("Cannot locate OpenShazam: {error}"))?;
        let path: PathBuf = executable
            .parent()
            .ok_or("Cannot locate OpenShazam's installation directory")?
            .join("OpenShazam.SettingsHost.dll");
        let bootstrap_path = executable
            .parent()
            .ok_or("Cannot locate OpenShazam's installation directory")?
            .join("Microsoft.WindowsAppRuntime.Bootstrap.dll");
        if !path.is_file() {
            return Err("WinUI 3 Settings component is missing. \
                 Build and bundle SettingsHost.vcxproj alongside openshazam.exe."
                .to_owned());
        }
        if !bootstrap_path.is_file() {
            return Err(
                "Windows App SDK bootstrapper is missing. \
                 Build and bundle Microsoft.WindowsAppRuntime.Bootstrap.dll alongside openshazam.exe."
                    .to_owned(),
            );
        }

        // This product is always unpackaged. Initialize the app-local Windows
        // App SDK framework before loading the WinUI 3 settings host.
        let mut bootstrap_name: Vec<u16> = OsStr::new(bootstrap_path.as_os_str())
            .encode_wide()
            .collect();
        bootstrap_name.push(0);
        // The executable and both product DLLs live side by side. Search that
        // DLL's own directory for its dependencies and System32 for Windows
        // libraries; do not let the working directory or PATH supply a DLL.
        // SAFETY: `bootstrap_name` is an absolute, NUL-terminated UTF-16 path
        // whose buffer remains live for the call; the flags restrict DLL lookup.
        let bootstrap_library = unsafe {
            LoadLibraryExW(
                bootstrap_name.as_ptr(),
                std::ptr::null_mut(),
                PRODUCT_DLL_SEARCH_FLAGS,
            )
        };
        if bootstrap_library.is_null() {
            return Err(format!(
                "Windows could not load the Windows App SDK bootstrapper (OS error {}).",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: `bootstrap_library` is the live module returned above and
        // both export names are static NUL-terminated strings. The Windows App
        // SDK defines these exports with the declared system ABI and signatures.
        let bootstrap_functions = unsafe {
            let initialize =
                GetProcAddress(bootstrap_library, c"MddBootstrapInitialize".as_ptr().cast());
            let shutdown =
                GetProcAddress(bootstrap_library, c"MddBootstrapShutdown".as_ptr().cast());
            if initialize.is_null() || shutdown.is_null() {
                FreeLibrary(bootstrap_library);
                return Err("Windows App SDK bootstrapper is missing a required export".to_owned());
            }
            (
                std::mem::transmute::<*mut c_void, BootstrapInitialize>(initialize),
                std::mem::transmute::<*mut c_void, BootstrapShutdown>(shutdown),
            )
        };
        // SAFETY: `initialize` was resolved from the live Windows App SDK
        // bootstrapper; the version-tag pointer refers to static storage.
        let bootstrap_hr = unsafe {
            (bootstrap_functions.0)(
                WINDOWSAPPSDK_RELEASE_MAJORMINOR,
                WINDOWSAPPSDK_RELEASE_VERSION_TAG.as_ptr(),
                WINDOWSAPPSDK_RUNTIME_MIN_VERSION,
            )
        };
        if bootstrap_hr < 0 {
            // SAFETY: this is the still-live module reference acquired above;
            // initialization failed, so no initialized framework needs shutdown.
            unsafe { FreeLibrary(bootstrap_library) };
            return Err(format!(
                "Windows App SDK 1.8 initialization failed (HRESULT 0x{:08X}).",
                bootstrap_hr as u32
            ));
        }
        let (bootstrap_library, bootstrap_shutdown) =
            (bootstrap_library, Some(bootstrap_functions.1));

        let mut name: Vec<u16> = OsStr::new(path.as_os_str()).encode_wide().collect();
        name.push(0);
        // SAFETY: The path is absolute, NUL-terminated UTF-16 and only points
        // to the dedicated binary shipped alongside our product executable.
        // The Windows App SDK framework dependency is active at this point.
        let module = unsafe {
            LoadLibraryExW(
                name.as_ptr(),
                std::ptr::null_mut(),
                PRODUCT_DLL_SEARCH_FLAGS,
            )
        };
        if module.is_null() {
            if let Some(bootstrap_shutdown) = bootstrap_shutdown {
                // SAFETY: initialization succeeded on this thread and the
                // function and module pointers are retained from that call.
                unsafe {
                    bootstrap_shutdown();
                    FreeLibrary(bootstrap_library);
                }
            }
            return Err(format!(
                "Windows could not load WinUI 3 Settings (OS error {}). \
                 Check the Windows App SDK runtime.",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: Every export is resolved from this live product DLL using a
        // static NUL-terminated name. The signatures match the exported C ABI;
        // no function is invoked unless all required symbols are present.
        let result = (|| unsafe {
            let get = |symbol: &'static [u8]| -> Result<*mut c_void, String> {
                let address = GetProcAddress(module, symbol.as_ptr().cast());
                if address.is_null() {
                    return Err(format!(
                        "Missing WinUI 3 Settings ABI export {}",
                        String::from_utf8_lossy(&symbol[..symbol.len() - 1])
                    ));
                }
                Ok(address)
            };
            Ok(Self {
                library: module,
                show: std::mem::transmute::<*mut c_void, Show>(get(b"OpenShazamSettingsShow\0")?),
                update_history: std::mem::transmute::<*mut c_void, UpdateHistory>(get(
                    b"OpenShazamSettingsSetHistory\0",
                )?),
                update_status: std::mem::transmute::<*mut c_void, UpdateStatus>(get(
                    b"OpenShazamSettingsSetUpdateStatus\0",
                )?),
                pre_translate: std::mem::transmute::<*mut c_void, Translate>(get(
                    b"OpenShazamSettingsPreTranslate\0",
                )?),
                is_visible: std::mem::transmute::<*mut c_void, IsVisible>(get(
                    b"OpenShazamSettingsIsVisible\0",
                )?),
                shutdown: std::mem::transmute::<*mut c_void, Shutdown>(get(
                    b"OpenShazamSettingsShutdown\0",
                )?),
                bootstrap_library,
                bootstrap_shutdown,
            })
        })();
        if result.is_err() {
            // SAFETY: no `SettingsHost` was returned, so these module
            // references are still owned here and no host callback is active.
            unsafe {
                FreeLibrary(module);
                if let Some(bootstrap_shutdown) = bootstrap_shutdown {
                    bootstrap_shutdown();
                    FreeLibrary(bootstrap_library);
                }
            }
        }
        result
    }

    /// Copy the C ABI callback so the caller can invoke WinUI after ending all
    /// Rust borrows of AppState. Showing a Win32 window synchronously sends
    /// activation messages to its owner, which re-enters the app window proc.
    pub(crate) fn show_callback(&self) -> Show {
        self.show
    }

    /// Copy the supplied rows into the WinUI history snapshot synchronously.
    ///
    /// # Safety
    ///
    /// Every non-null string pointer in `items` must address a readable,
    /// NUL-terminated UTF-16 string for the duration of this call. The C++
    /// export copies these strings before returning and does not retain them.
    pub(crate) unsafe fn update_history(&self, items: &[SettingsHistoryItem]) {
        // The WinUI host copies every string before returning. The string
        // buffers and descriptor slice therefore only need to live for this
        // synchronous call.
        // SAFETY: the slice pointer is valid for `items.len()` entries, and
        // the C++ export copies every pointed-to string before returning.
        unsafe { (self.update_history)(items.as_ptr(), items.len()) };
    }

    pub(crate) fn update_status(
        &self,
        enabled: bool,
        status: u32,
        version: &str,
        detail: &str,
        last_checked: Option<u64>,
    ) {
        let version: Vec<u16> = OsStr::new(version).encode_wide().chain(Some(0)).collect();
        let detail: Vec<u16> = OsStr::new(detail).encode_wide().chain(Some(0)).collect();
        // SAFETY: both UTF-16 buffers are NUL-terminated and remain alive for
        // the synchronous C ABI call, whose implementation copies their text.
        unsafe {
            (self.update_status)(
                enabled as i32,
                status,
                version.as_ptr(),
                detail.as_ptr(),
                last_checked.unwrap_or(0),
            );
        }
    }

    pub fn visible(&self) -> bool {
        // SAFETY: the function pointer belongs to the loaded SettingsHost DLL,
        // which remains owned by `self` for this synchronous call.
        unsafe { (self.is_visible)() != 0 }
    }

    pub fn pre_translate(
        &self,
        message: &mut windows::Win32::UI::WindowsAndMessaging::MSG,
    ) -> bool {
        // WinUI focus navigation must be processed before TranslateMessage.
        // SAFETY: `message` is a live MSG supplied by the current UI message
        // loop, and the C++ export reads it synchronously without retaining it.
        unsafe { (self.pre_translate)(message as *mut _ as *mut c_void) != 0 }
    }
}

impl Drop for SettingsHost {
    fn drop(&mut self) {
        // SAFETY: The Rust host always owns its DLL on the same UI thread.
        unsafe {
            (self.shutdown)();
            FreeLibrary(self.library);
            if let Some(bootstrap_shutdown) = self.bootstrap_shutdown {
                bootstrap_shutdown();
                FreeLibrary(self.bootstrap_library);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SettingsHistoryItem, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
        PRODUCT_DLL_SEARCH_FLAGS,
    };

    #[test]
    fn settings_history_item_matches_the_native_x64_abi_layout() {
        use std::mem::{align_of, offset_of, size_of};

        let pointer_size = size_of::<*const u16>();
        assert_eq!(offset_of!(SettingsHistoryItem, title), 0);
        assert_eq!(offset_of!(SettingsHistoryItem, artist), pointer_size);
        assert_eq!(
            offset_of!(SettingsHistoryItem, recognized_at),
            2 * pointer_size
        );
        assert_eq!(offset_of!(SettingsHistoryItem, page_url), 3 * pointer_size);
        assert_eq!(size_of::<SettingsHistoryItem>(), 4 * pointer_size);
        assert_eq!(align_of::<SettingsHistoryItem>(), align_of::<*const u16>());
    }

    #[test]
    fn settings_host_dll_search_is_limited_to_product_and_system_directories() {
        assert_eq!(LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, 0x0000_0100);
        assert_eq!(LOAD_LIBRARY_SEARCH_SYSTEM32, 0x0000_0800);
        assert_eq!(
            PRODUCT_DLL_SEARCH_FLAGS,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32
        );
    }
}
