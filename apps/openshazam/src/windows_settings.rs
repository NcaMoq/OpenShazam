//! Lazy, same-process C ABI to the C++/WinRT WinUI 3 Settings island.
//! The DLL is loaded by its absolute path next to the product executable.
//! Neither the recognition controller nor audio buffers ever cross this ABI.
use std::{
    ffi::{c_char, c_void, OsStr},
    os::windows::ffi::OsStrExt,
    path::PathBuf,
};

pub(crate) type Show = unsafe extern "system" fn(*mut c_void, u32) -> i32;
type Translate = unsafe extern "system" fn(*mut c_void) -> i32;
type IsVisible = unsafe extern "system" fn() -> i32;
type Shutdown = unsafe extern "system" fn();
type BootstrapInitialize = unsafe extern "system" fn(u32, *const u16, u64) -> i32;
type BootstrapShutdown = unsafe extern "system" fn();

// Keep these values aligned with the Windows App SDK 1.8 package pinned by
// settings-host/packages.config and its WindowsAppSDK-VersionInfo.h.
const WINDOWSAPPSDK_RELEASE_MAJORMINOR: u32 = 0x0001_0008;
const WINDOWSAPPSDK_RUNTIME_MIN_VERSION: u64 = 0x1F40_0326_08CC_0000;
const WINDOWSAPPSDK_RELEASE_VERSION_TAG: [u16; 1] = [0];

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}

pub struct SettingsHost {
    library: *mut c_void,
    show: Show,
    pre_translate: Translate,
    is_visible: IsVisible,
    shutdown: Shutdown,
    bootstrap_library: *mut c_void,
    bootstrap_shutdown: BootstrapShutdown,
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
            return Err(format!(
                "WinUI 3 Settings component is missing: {}. \
                 Build and bundle SettingsHost.vcxproj alongside openshazam.exe.",
                path.display()
            ));
        }
        if !bootstrap_path.is_file() {
            return Err(format!(
                "Windows App SDK bootstrapper is missing: {}. \
                 Build and bundle Microsoft.WindowsAppRuntime.Bootstrap.dll alongside openshazam.exe.",
                bootstrap_path.display()
            ));
        }

        let mut bootstrap_name: Vec<u16> = OsStr::new(bootstrap_path.as_os_str())
            .encode_wide()
            .collect();
        bootstrap_name.push(0);
        // SAFETY: The absolute, NUL-terminated path points to the app-local
        // Windows App SDK bootstrapper produced alongside the Settings host.
        let bootstrap_library = unsafe { LoadLibraryW(bootstrap_name.as_ptr()) };
        if bootstrap_library.is_null() {
            return Err(format!(
                "Windows could not load the Windows App SDK bootstrapper from {} (OS error {}).",
                bootstrap_path.display(),
                std::io::Error::last_os_error()
            ));
        }

        // SAFETY: These exports are the documented C ABI of the app-local
        // Windows App SDK bootstrapper. Initialize the framework package graph
        // before loading SettingsHost, whose imports include WinUI runtime DLLs.
        let bootstrap_functions = unsafe {
            let initialize = GetProcAddress(
                bootstrap_library,
                b"MddBootstrapInitialize\0".as_ptr().cast(),
            );
            let shutdown =
                GetProcAddress(bootstrap_library, b"MddBootstrapShutdown\0".as_ptr().cast());
            if initialize.is_null() || shutdown.is_null() {
                FreeLibrary(bootstrap_library);
                return Err("Windows App SDK bootstrapper is missing a required export".to_owned());
            }
            (
                std::mem::transmute::<*mut c_void, BootstrapInitialize>(initialize),
                std::mem::transmute::<*mut c_void, BootstrapShutdown>(shutdown),
            )
        };
        let bootstrap_hr = unsafe {
            (bootstrap_functions.0)(
                WINDOWSAPPSDK_RELEASE_MAJORMINOR,
                WINDOWSAPPSDK_RELEASE_VERSION_TAG.as_ptr(),
                WINDOWSAPPSDK_RUNTIME_MIN_VERSION,
            )
        };
        if bootstrap_hr < 0 {
            unsafe { FreeLibrary(bootstrap_library) };
            return Err(format!(
                "Windows App SDK 1.8 initialization failed (HRESULT 0x{:08X}).",
                bootstrap_hr as u32
            ));
        }

        let mut name: Vec<u16> = OsStr::new(path.as_os_str()).encode_wide().collect();
        name.push(0);
        // SAFETY: The path is absolute, NUL-terminated UTF-16 and only points
        // to the dedicated binary shipped alongside our product executable.
        // The Windows App SDK framework dependency is active at this point.
        let module = unsafe { LoadLibraryW(name.as_ptr()) };
        if module.is_null() {
            unsafe {
                (bootstrap_functions.1)();
                FreeLibrary(bootstrap_library);
            }
            return Err(format!(
                "Windows could not load WinUI 3 Settings from {} (OS error {}). \
                 Check the Windows App SDK runtime.",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: The exports use a C ABI and are resolved from our own DLL.
        // No function is invoked unless every symbol is present.
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
                bootstrap_shutdown: bootstrap_functions.1,
            })
        })();
        if result.is_err() {
            unsafe {
                FreeLibrary(module);
                (bootstrap_functions.1)();
                FreeLibrary(bootstrap_library);
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

    pub fn visible(&self) -> bool {
        unsafe { (self.is_visible)() != 0 }
    }

    pub fn pre_translate(
        &self,
        message: &mut windows::Win32::UI::WindowsAndMessaging::MSG,
    ) -> bool {
        // WinUI focus navigation must be processed before TranslateMessage.
        unsafe { (self.pre_translate)(message as *mut _ as *mut c_void) != 0 }
    }
}

impl Drop for SettingsHost {
    fn drop(&mut self) {
        // SAFETY: The Rust host always owns its DLL on the same UI thread.
        unsafe {
            (self.shutdown)();
            FreeLibrary(self.library);
            (self.bootstrap_shutdown)();
            FreeLibrary(self.bootstrap_library);
        }
    }
}
