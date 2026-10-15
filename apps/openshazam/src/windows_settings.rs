//! Lazy, same-process C ABI to the C++/WinRT WinUI 3 Settings island.
//! The DLL is loaded by its absolute path next to the product executable.
//! Neither the recognition controller nor audio buffers ever cross this ABI.
use std::{
    ffi::{c_char, c_void, OsStr},
    os::windows::ffi::OsStrExt,
    path::PathBuf,
};

type Show = unsafe extern "system" fn(*mut c_void) -> i32;
type Translate = unsafe extern "system" fn(*mut c_void) -> i32;
type IsVisible = unsafe extern "system" fn() -> i32;
type Shutdown = unsafe extern "system" fn();

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
}

impl SettingsHost {
    pub fn load() -> Result<Self, String> {
        let executable = std::env::current_exe()
            .map_err(|error| format!("Cannot locate OpenShazam: {error}"))?;
        let path: PathBuf = executable
            .parent()
            .ok_or("Cannot locate OpenShazam's installation directory")?
            .join("OpenShazam.SettingsHost.dll");
        if !path.is_file() {
            return Err(format!(
                "WinUI 3 Settings component is missing: {}. \
                 Build and bundle SettingsHost.vcxproj alongside openshazam.exe.",
                path.display()
            ));
        }
        let mut name: Vec<u16> = OsStr::new(path.as_os_str()).encode_wide().collect();
        name.push(0);
        // SAFETY: The path is absolute, NUL-terminated UTF-16 and only points
        // to the dedicated binary shipped alongside our product executable.
        let module = unsafe { LoadLibraryW(name.as_ptr()) };
        if module.is_null() {
            return Err(format!(
                "Windows could not load WinUI 3 Settings from {} (OS error {}). \
                 Check the Windows App SDK runtime.",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: The exports use a C ABI and are resolved from our own DLL.
        // No function is invoked unless every symbol is present.
        let result = unsafe {
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
            })
        };
        if result.is_err() {
            unsafe { FreeLibrary(module) };
        }
        result
    }

    pub fn show(&self, parent: windows::Win32::Foundation::HWND) -> Result<(), String> {
        // SAFETY: Called by the Win32 UI thread on its own HWND.
        let hr = unsafe { (self.show)(parent.0) };
        if hr < 0 {
            Err(format!(
                "WinUI 3 Settings failed to open (HRESULT 0x{:08X})",
                hr as u32
            ))
        } else {
            Ok(())
        }
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
        }
    }
}
