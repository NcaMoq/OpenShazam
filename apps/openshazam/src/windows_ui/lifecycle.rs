#![deny(unsafe_op_in_unsafe_fn)]

use super::recognition::start_capture;
use super::{
    add_tray_icon, begin_update_check, create_window, foreground_for_theme, load_application_icon,
    register_global_shortcut, request_history_artwork_at_offset, restore_history_tracks,
    set_ui_language, theme_from_ui_settings, wide, AppState, ArrowEdge, CaptureMode, MainPhase,
    StatusTextAlternator, ThemePreference, UiLanguage, UiLevels, UpdateStatus, WindowKind,
    APP_ICON_RESOURCE_ID, APP_USER_MODEL_ID, CLASS_NAME, CONTENT_WIDTH, GLOBAL_SHORTCUT_ID_PRIMARY,
    MAIN_WINDOW_HEIGHT, MAIN_WINDOW_WIDTH, TIMER_UPDATE_CHECK, TRAY_TASKBAR_CREATED_NAME,
    UPDATE_CHECK_INTERVAL_MS, WM_OS_THEME_CHANGED, WM_SHOW_INITIAL_POPUP,
};
use super::{notifications, single_instance, ui_model, window_proc::window_proc};
use super::{
    preferences::{
        load_auto_shazam_preference, load_auto_update_preference, load_global_shortcut,
        load_theme_preference, load_ui_language_preference,
    },
    window_proc::next_message_generation,
};
use crate::{windows_history_store::HistoryStore, windows_update::PendingUpdateResult};
use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    mem::size_of,
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::Instant,
};
use windows::{
    core::{Error, Interface, PCWSTR, PWSTR},
    Foundation::TypedEventHandler,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, WPARAM},
        Graphics::{
            Direct2D::{D2D1CreateFactory, ID2D1Factory, D2D1_FACTORY_TYPE_SINGLE_THREADED},
            DirectWrite::{DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED},
        },
        Storage::EnhancedStorage::{PKEY_AppUserModel_ID, PKEY_AppUserModel_RelaunchIconResource},
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemAlloc, CoTaskMemFree, CoUninitialize,
                IPersistFile,
                StructuredStorage::{
                    PropVariantClear, PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0,
                    PROPVARIANT_0_0_0,
                },
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
            },
            LibraryLoader::GetModuleHandleW,
            Variant::VT_LPWSTR,
            WinRT::{
                CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_STA,
                DQTYPE_THREAD_CURRENT,
            },
        },
        UI::{
            HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
            Input::KeyboardAndMouse::UnregisterHotKey,
            Shell::PropertiesSystem::IPropertyStore,
            Shell::{
                FOLDERID_Programs, IShellLinkW, SHGetKnownFolderPath,
                SetCurrentProcessExplicitAppUserModelID, ShellLink, KNOWN_FOLDER_FLAG,
            },
            WindowsAndMessaging::{
                DestroyWindow, DispatchMessageW, LoadCursorW, PeekMessageW, PostMessageW,
                RegisterClassW, RegisterWindowMessageW, SetTimer, TranslateMessage, WaitMessage,
                CS_DBLCLKS, IDC_ARROW, MSG, PM_REMOVE, WNDCLASSW, WNDPROC,
            },
        },
    },
    UI::{Composition::Compositor, ViewManagement::UISettings},
};

struct StaApartmentGuard;

impl StaApartmentGuard {
    fn initialize() -> windows::core::Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        Ok(Self)
    }
}

impl Drop for StaApartmentGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// # Safety
/// `app_ptr` must be the live `Box<AppState>` allocation owned by the UI loop.
/// No Rust reference to that allocation may be held across `DestroyWindow`,
/// because destroying a window can reenter its WndProc.
unsafe fn destroy_owned_window(app_ptr: *mut AppState, kind: WindowKind) -> bool {
    // SAFETY: The caller retains the unique Box allocation until every owned
    // HWND has been destroyed; this short raw read does not create a reference
    // that could overlap a reentrant WndProc call.
    let hwnd = unsafe {
        match kind {
            WindowKind::Main => (*app_ptr).main_hwnd,
        }
    };
    if hwnd.0.is_null() {
        return true;
    }

    // SAFETY: `hwnd` was read from the still-owned AppState immediately above;
    // WndProc may reenter, but the allocation remains alive for that callback.
    if let Err(error) = unsafe { DestroyWindow(hwnd) } {
        eprintln!("OpenShazam could not destroy its {kind:?} window: {error}");
    }
    // SAFETY: The unique AppState box remains allocated, and no mutable Rust
    // reference was kept across the potentially reentrant DestroyWindow call.
    let remaining = unsafe {
        match kind {
            WindowKind::Main => (*app_ptr).main_hwnd,
        }
    };
    remaining.0.is_null()
}

fn owned_window_handles_are_clear(main: HWND) -> bool {
    main.0.is_null()
}

/// # Safety
/// `app_ptr` must be the unique raw pointer obtained from `Box::into_raw`, and
/// this must be the final shutdown path so the allocation is never reclaimed.
unsafe fn retain_app_until_process_exit(app_ptr: *mut AppState, apartment: StaApartmentGuard) {
    // A failed DestroyWindow leaves WndProc's raw AppState pointer live. Leak
    // the allocation and apartment only on this fatal shutdown path so that a
    // late window message cannot dereference freed state before process exit.
    // SAFETY: `app_ptr` is the still-live unique Box allocation transferred to
    // the UI loop; leaking reconstructs its ownership without freeing it.
    let _ = unsafe { Box::leak(Box::from_raw(app_ptr)) };
    std::mem::forget(apartment);
}

pub(crate) fn run(start_hidden: bool) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _sta_apartment = StaApartmentGuard::initialize()?;
        let app_user_model_id = wide(APP_USER_MODEL_ID);
        if let Err(error) =
            SetCurrentProcessExplicitAppUserModelID(PCWSTR(app_user_model_id.as_ptr()))
        {
            eprintln!("OpenShazam could not set its Windows notification identity: {error}");
        }
        let silent_startup = start_hidden;
        let (instance_mutex, already_running) = single_instance::acquire_single_instance_mutex()?;
        if already_running {
            // A second explicit UI launch reuses the resident tray process.
            // A duplicate logon activation exits quietly and never foregrounds
            // the existing popup.
            if !silent_startup
                && !single_instance::activate_existing_instance(CLASS_NAME, WM_SHOW_INITIAL_POPUP)
            {
                eprintln!("OpenShazam is already running, but its window could not be activated.");
            }
            drop(instance_mutex);
            return Ok(());
        }
        let _instance_mutex = instance_mutex;
        if let Err(error) = register_notification_application() {
            eprintln!("OpenShazam could not register its Windows notification icon: {error}");
        }
        let instance = GetModuleHandleW(None)?;
        let class_name = wide(CLASS_NAME);
        let class = window_class(
            instance.into(),
            Some(window_proc),
            PCWSTR(class_name.as_ptr()),
        );
        if RegisterClassW(&class) == 0 {
            return Err(Box::new(Error::from_thread()));
        }
        let d2d_factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let dwrite_factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let dispatcher_queue = CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })?;
        let compositor = Compositor::new()?;
        let theme_settings = UISettings::new()?;
        let theme_preference = load_theme_preference();
        let language = load_ui_language_preference();
        set_ui_language(language);
        let global_shortcut = load_global_shortcut();
        let update_auto_enabled = load_auto_update_preference();
        let update_auto_enabled_shared = Arc::new(AtomicBool::new(update_auto_enabled));
        let update_auto_install_gate = Arc::new(Mutex::new(()));
        let update_cancelled = Arc::new(AtomicBool::new(false));
        let (update_completion_sender, update_completion_receiver) = std::sync::mpsc::channel();
        let pending_update_read =
            crate::windows_update::consume_pending_update_result(env!("CARGO_PKG_VERSION"));
        if let Err(error) = crate::windows_update::cleanup_obsolete_update_artifacts() {
            eprintln!("OpenShazam could not clean old update downloads: {error}");
        }
        let (
            initial_update_status,
            initial_update_version,
            initial_update_detail,
            startup_update_result,
        ) = match pending_update_read {
            Ok(Some(result)) => match &result {
                PendingUpdateResult::Installed { version } => (
                    UpdateStatus::Updated,
                    version.clone(),
                    String::new(),
                    Some(result),
                ),
                PendingUpdateResult::Failed { expected_version } => (
                    UpdateStatus::Error,
                    expected_version.clone(),
                    failed_update_status_detail(language, expected_version),
                    Some(result),
                ),
            },
            Ok(None) => (UpdateStatus::Idle, String::new(), String::new(), None),
            Err(error) => {
                eprintln!("OpenShazam could not verify the previous update result: {error}");
                (
                    UpdateStatus::Error,
                    String::new(),
                    update_result_read_error_detail(language).to_owned(),
                    None,
                )
            }
        };
        let (history_store, history) = match HistoryStore::open() {
            Ok((store, entries)) => (Some(store), restore_history_tracks(entries)),
            Err(error) => {
                eprintln!(
                    "OpenShazam could not restore persistent history; new results remain in memory only: {error}"
                );
                (None, Vec::new())
            }
        };
        let next_display_id = history.len() as u64 + 1;
        let (system_theme, os_foreground) = theme_from_ui_settings(&theme_settings)?;
        let theme = theme_preference.resolve(system_theme);
        let system_foreground = match theme_preference {
            ThemePreference::System => os_foreground,
            ThemePreference::Light | ThemePreference::Dark => foreground_for_theme(theme),
        };
        let taskbar_created_name = wide(TRAY_TASKBAR_CREATED_NAME);
        let taskbar_created_message = RegisterWindowMessageW(PCWSTR(taskbar_created_name.as_ptr()));
        let app = Box::new(AppState {
            main_hwnd: HWND::default(),
            message_generation: next_message_generation(),
            taskbar_created_message,
            global_shortcut,
            global_shortcut_registration_id: None,
            global_shortcut_capture_suspended: false,
            settings_host: None,
            settings_opening: false,
            update_operation_active: false,
            pending_update_install: None,
            resume_auto_shazam_after_update: false,
            update_cancelled,
            update_workers: Vec::new(),
            update_completion_sender,
            update_completion_receiver,
            update_auto_enabled,
            update_auto_enabled_shared,
            update_auto_install_gate,
            update_candidate: None,
            update_status: initial_update_status,
            update_detail: initial_update_detail,
            update_version: initial_update_version,
            update_last_checked: None,
            d2d_factory,
            dwrite_factory,
            compositor,
            _dispatcher_queue: dispatcher_queue,
            theme_settings,
            theme_changed_token: None,
            system_foreground,
            phase: MainPhase::Ready,
            history,
            history_transition: None,
            history_composition_ready: false,
            scroll_offset: 0.0,
            history_scroll_animation: None,
            history_scroll_composition_ready: false,
            hovered_row: None,
            mouse_leave_tracking: false,
            ui_levels: UiLevels::default(),
            auto_spring: None,
            theme,
            theme_preference,
            theme_generation: 0,
            visible: false,
            popup_opacity: 0,
            main_activated: false,
            popup_activation_grace_until: None,
            deactivation_hide_pending: false,
            phase_timer_active: false,
            animation_timer_active: false,
            result_overlay_composition_ready: false,
            status_text_composition_ready: false,
            startup_animation_pending: false,
            startup_logo_consumed: false,
            arrow_edge: ArrowEdge::Top,
            arrow_center: (CONTENT_WIDTH * 0.5) as f32,
            mouse_down_center: false,
            long_press_consumed: false,
            single_click_pending: false,
            recognition_controller: None,
            recognition_session: None,
            history_store,
            auto_candidate_dedupe: VecDeque::new(),
            controller_sequence: 0,
            dropped_notifications: 0,
            status_alternator: Some(StatusTextAlternator::ready(Instant::now())),
            success_wave_frame: None,
            next_display_id,
            next_artwork_request_id: 1,
            artwork_generations: HashMap::new(),
            active_artwork_requests: HashMap::new(),
            artwork_failures: HashMap::new(),
            artwork_resources: HashMap::new(),
            pending_match_notifications: HashMap::new(),
            owned_balloon_icons: VecDeque::new(),
            ui_configs: HashMap::new(),
            message: None,
        });
        let app_ptr = Box::into_raw(app);
        let dpi = 96u32;
        let hwnd = match create_window(
            app_ptr,
            WindowKind::Main,
            "Shazam",
            ui_model::physical_pixels(MAIN_WINDOW_WIDTH, dpi),
            ui_model::physical_pixels(MAIN_WINDOW_HEIGHT, dpi),
            false,
        ) {
            Ok(hwnd) => hwnd,
            Err(error) => {
                drop(Box::from_raw(app_ptr));
                return Err(Box::new(error));
            }
        };
        (*app_ptr).main_hwnd = hwnd;
        request_history_artwork_at_offset(&mut *app_ptr, 0.0);
        let callback_hwnd = hwnd.0 as isize;
        let theme_changed_handler =
            TypedEventHandler::<UISettings, windows::core::IInspectable>::new(move |_, _| {
                let callback_hwnd = HWND(callback_hwnd as *mut c_void);
                let _ = PostMessageW(
                    Some(callback_hwnd),
                    WM_OS_THEME_CHANGED,
                    WPARAM(0),
                    LPARAM(0),
                );
                Ok(())
            });
        match (*app_ptr)
            .theme_settings
            .ColorValuesChanged(&theme_changed_handler)
        {
            Ok(token) => (*app_ptr).theme_changed_token = Some(token),
            Err(error) => {
                if !destroy_owned_window(app_ptr, WindowKind::Main) {
                    retain_app_until_process_exit(app_ptr, _sta_apartment);
                    std::mem::forget(_instance_mutex);
                    return Err(Box::new(error));
                }
                drop(Box::from_raw(app_ptr));
                return Err(Box::new(error));
            }
        }
        if let Err(error) = add_tray_icon(hwnd) {
            if let Some(token) = (*app_ptr).theme_changed_token.take() {
                let _ = (*app_ptr).theme_settings.RemoveColorValuesChanged(token);
            }
            if !destroy_owned_window(app_ptr, WindowKind::Main) {
                retain_app_until_process_exit(app_ptr, _sta_apartment);
                std::mem::forget(_instance_mutex);
                return Err(Box::new(error));
            }
            drop(Box::from_raw(app_ptr));
            return Err(Box::new(error));
        }
        if let Some(result) = startup_update_result.as_ref() {
            notifications::notify_update_result(hwnd, result, &mut (*app_ptr).owned_balloon_icons);
        }
        // Check in the background on launch and then once a day. When automatic
        // updates are enabled, verified installers are downloaded and installed
        // without blocking the UI thread.
        if SetTimer(
            Some(hwnd),
            TIMER_UPDATE_CHECK,
            UPDATE_CHECK_INTERVAL_MS,
            None,
        ) == 0
        {
            eprintln!("Could not schedule the daily Shazam update check.");
        }
        if startup_update_result.is_none() {
            begin_update_check(&mut *app_ptr, false);
        }
        match register_global_shortcut(hwnd, GLOBAL_SHORTCUT_ID_PRIMARY, (*app_ptr).global_shortcut)
        {
            Ok(()) => {
                (*app_ptr).global_shortcut_registration_id = Some(GLOBAL_SHORTCUT_ID_PRIMARY);
            }
            Err(error) => {
                eprintln!("Could not register global Shazam shortcut (error {error}).");
                (*app_ptr).message = Some(format!(
                    "Could not register global Shazam shortcut (Windows error {error})."
                ));
            }
        }
        // Let Explorer finish publishing the notification icon before asking
        // Shell_NotifyIconGetRect for its anchor. `ui` opens the popup on
        // launch; the tray icon remains available to hide/show it afterward.
        if !silent_startup {
            let _ = PostMessageW(Some(hwnd), WM_SHOW_INITIAL_POPUP, WPARAM(0), LPARAM(0));
        }
        // Mirror Shazam's persisted Auto Shazam state: once the user has
        // enabled continuous recognition, launch it again with the popup
        // hidden. Showing the popup later only changes its request context.
        if load_auto_shazam_preference() {
            start_capture(&mut *app_ptr, CaptureMode::Auto);
        }
        let mut message = MSG::default();
        let mut running = true;
        while running {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                if message.message == windows::Win32::UI::WindowsAndMessaging::WM_QUIT {
                    running = false;
                    break;
                }
                if (*app_ptr)
                    .settings_host
                    .as_ref()
                    .is_some_and(|host| host.pre_translate(&mut message))
                {
                    continue;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            if !running {
                break;
            }
            let _ = WaitMessage();
        }
        if let Some(token) = (*app_ptr).theme_changed_token.take() {
            let _ = (*app_ptr).theme_settings.RemoveColorValuesChanged(token);
        }
        // Shutdown WinUI XAML before releasing the STA and compositor.
        (*app_ptr).settings_host.take();
        if let Some(registration_id) = (*app_ptr).global_shortcut_registration_id.take() {
            let _ = UnregisterHotKey(Some((*app_ptr).main_hwnd), registration_id);
        }
        let _ = destroy_owned_window(app_ptr, WindowKind::Main);
        if !owned_window_handles_are_clear((*app_ptr).main_hwnd) {
            eprintln!(
                "OpenShazam is retaining UI state because a window could not be destroyed safely."
            );
            retain_app_until_process_exit(app_ptr, _sta_apartment);
            std::mem::forget(_instance_mutex);
            return Err(Box::new(std::io::Error::other(
                "OpenShazam could not destroy every owned window during shutdown.",
            )));
        }
        drop(Box::from_raw(app_ptr));
    }
    Ok(())
}

fn window_class(instance: HINSTANCE, proc: WNDPROC, class_name: PCWSTR) -> WNDCLASSW {
    WNDCLASSW {
        style: CS_DBLCLKS,
        lpfnWndProc: proc,
        hInstance: instance,
        hIcon: unsafe { load_application_icon(instance) },
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        lpszClassName: class_name,
        ..Default::default()
    }
}

/// # Safety
/// COM must be initialized on the current thread, and `property_store` must be
/// a live interface accepting the property key and a NUL-terminated string.
unsafe fn set_shortcut_string_property(
    property_store: &IPropertyStore,
    key: &windows::Win32::Foundation::PROPERTYKEY,
    value: &str,
) -> windows::core::Result<()> {
    let value = wide(value);
    let allocation_size = value.len().checked_mul(size_of::<u16>()).ok_or_else(|| {
        Error::new(
            windows::core::HRESULT(0x8007_0057u32 as i32),
            "shortcut property string size overflowed",
        )
    })?;
    // SAFETY: `value` is a NUL-terminated UTF-16 buffer and the checked byte
    // length allocates enough CoTaskMem for the complete string.
    let allocation = unsafe { CoTaskMemAlloc(allocation_size).cast::<u16>() };
    if allocation.is_null() {
        return Err(Error::from_thread());
    }
    // SAFETY: `allocation` covers `value.len()` UTF-16 units and does not
    // overlap the Rust-owned source buffer.
    unsafe { allocation.copy_from_nonoverlapping(value.as_ptr(), value.len()) };

    let mut variant = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_LPWSTR,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    pwszVal: PWSTR(allocation),
                },
            }),
        },
    };
    // SAFETY: `variant` owns a valid CoTaskMem UTF-16 string for the duration
    // of both synchronous property-store calls.
    let result = unsafe {
        property_store
            .SetValue(key, &variant)
            .and_then(|()| property_store.Commit())
    };
    // PROPVARIANT owns its VT_LPWSTR allocation. Let the Windows API clear
    // the full variant so its allocator and nested union layout stay paired.
    // SAFETY: `variant` was initialized above with a valid VT_LPWSTR union arm.
    let clear_result = unsafe { PropVariantClear(&mut variant) };
    result?;
    clear_result
}

/// # Safety
/// COM must be initialized on the current UI thread for the lifetime of the
/// ShellLink and property-store interfaces created by this function.
unsafe fn register_notification_application() -> windows::core::Result<()> {
    let executable_path = std::env::current_exe()
        .map_err(|_| Error::from_thread())?
        .to_string_lossy()
        .into_owned();
    let executable = wide(&executable_path);
    let executable_parent = std::path::Path::new(&executable_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_string_lossy()
        .into_owned();
    let working_directory = wide(&executable_parent);
    let arguments = wide("ui");
    let icon_resource = format!("{executable_path},-{APP_ICON_RESOURCE_ID}");

    // SAFETY: The known-folder API returns an allocated, NUL-terminated path
    // when it succeeds; the caller runs with COM initialized on the UI thread.
    let programs = unsafe { SHGetKnownFolderPath(&FOLDERID_Programs, KNOWN_FOLDER_FLAG(0), None)? };
    if programs.0.is_null() {
        return Err(Error::new(
            windows::core::HRESULT(0x8000_4003u32 as i32),
            "Windows returned a null Programs-folder path",
        ));
    }
    // SAFETY: The successful shell call returned a readable NUL-terminated path.
    let programs_path = unsafe { PCWSTR(programs.0).to_string() };
    // SAFETY: The path allocation is released exactly once after conversion.
    unsafe { CoTaskMemFree(Some(programs.0.cast())) };
    let programs_path = programs_path?;
    let shortcut_path_string = format!("{programs_path}\\Shazam.lnk");
    let legacy_shortcut_path = format!("{programs_path}\\OpenShazam.lnk");
    if !std::path::Path::new(&shortcut_path_string).exists() {
        // Rename the shortcut created by older builds so Start and toast
        // surfaces use the public product name after an in-place update.
        let _ = std::fs::rename(&legacy_shortcut_path, &shortcut_path_string);
    }
    let shortcut_path = wide(&shortcut_path_string);

    // SAFETY: COM is initialized on this thread and the requested in-process
    // ShellLink class uses the system COM server.
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? };
    // SAFETY: Every PCWSTR points to an owned NUL-terminated UTF-16 buffer
    // kept alive through the corresponding synchronous ShellLink call.
    unsafe {
        link.SetPath(PCWSTR(executable.as_ptr()))?;
        link.SetArguments(PCWSTR(arguments.as_ptr()))?;
        link.SetWorkingDirectory(PCWSTR(working_directory.as_ptr()))?;
        link.SetIconLocation(PCWSTR(executable.as_ptr()), 0)?;
    }

    let property_store: IPropertyStore = link.cast()?;
    unsafe {
        set_shortcut_string_property(&property_store, &PKEY_AppUserModel_ID, APP_USER_MODEL_ID)?
    };
    unsafe {
        set_shortcut_string_property(
            &property_store,
            &PKEY_AppUserModel_RelaunchIconResource,
            &icon_resource,
        )?
    };

    let persist_file: IPersistFile = link.cast()?;
    // SAFETY: `shortcut_path` is a NUL-terminated owned UTF-16 path and the
    // persist interface remains live until this synchronous save completes.
    unsafe { persist_file.Save(PCWSTR(shortcut_path.as_ptr()), true) }
}

fn failed_update_status_detail(language: UiLanguage, expected_version: &str) -> String {
    match language {
        UiLanguage::Japanese => format!(
            "バージョン {expected_version} への更新を確認できませんでした。設定から更新を確認してください。"
        ),
        UiLanguage::English => format!(
            "The update to version {expected_version} did not complete. Check for updates in Settings."
        ),
    }
}

fn update_result_read_error_detail(language: UiLanguage) -> &'static str {
    match language {
        UiLanguage::Japanese => {
            "前回の更新結果を確認できませんでした。設定から更新を確認してください。"
        }
        UiLanguage::English => {
            "Could not verify the previous update result. Check for updates in Settings."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        failed_update_status_detail, owned_window_handles_are_clear,
        update_result_read_error_detail, StaApartmentGuard, UiLanguage,
    };
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

    #[test]
    fn ui_sta_apartment_is_uninitialized_when_its_guard_drops() {
        let can_initialize_mta_after_sta_guard_drop = std::thread::spawn(|| {
            drop(StaApartmentGuard::initialize().expect("STA initialization should succeed"));
            let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if initialized.is_ok() {
                unsafe { CoUninitialize() };
                true
            } else {
                false
            }
        })
        .join()
        .expect("COM test thread should complete");

        assert!(can_initialize_mta_after_sta_guard_drop);
    }

    #[test]
    fn app_state_is_releasable_only_after_every_owned_window_is_destroyed() {
        let closed = HWND::default();
        assert!(owned_window_handles_are_clear(closed));
        assert!(!owned_window_handles_are_clear(HWND(1usize as *mut _)));
    }

    #[test]
    fn failed_update_status_matches_the_selected_language() {
        assert_eq!(
            failed_update_status_detail(UiLanguage::Japanese, "0.1.2"),
            "バージョン 0.1.2 への更新を確認できませんでした。設定から更新を確認してください。"
        );
        assert_eq!(
            failed_update_status_detail(UiLanguage::English, "0.1.2"),
            "The update to version 0.1.2 did not complete. Check for updates in Settings."
        );
    }

    #[test]
    fn unreadable_update_result_uses_a_localized_generic_message() {
        assert_eq!(
            update_result_read_error_detail(UiLanguage::Japanese),
            "前回の更新結果を確認できませんでした。設定から更新を確認してください。"
        );
        assert_eq!(
            update_result_read_error_detail(UiLanguage::English),
            "Could not verify the previous update result. Check for updates in Settings."
        );
    }
}
