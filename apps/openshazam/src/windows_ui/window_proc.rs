#![deny(unsafe_op_in_unsafe_fn)]

use super::{
    advance_history_scroll, advance_pending_update_install, animation_needs_d2d_redraw,
    animation_needs_frame, animation_snapshot, apply_tray_menu_action,
    artwork_request_matches_track, begin_update_check, begin_update_download,
    cancel_and_join_update_workers, cancel_capture, cancel_deferred_deactivation_hide,
    cancel_pending_automatic_update_install, capture_mode_in_progress, clear_history_hover,
    client_dips, defer_or_hide_main_after_deactivation, delete_tray_icon,
    drain_update_worker_completions, finish_active_artwork_request,
    handle_update_install_confirmation, hide_main, high_word, history_row_index_at,
    history_scroll_offset_after_wheel, history_scroll_offset_at,
    history_transition_requires_d2d_frame, hovered_history_row_for_cursor,
    is_tray_activation_event, low_word, main_hit_test, open_settings_after_wndproc,
    open_track_page, process_main_hit, refresh_history_date_labels, refresh_os_theme,
    request_history_artwork_at_offset, restore_tray_icon, resume_auto_shazam_after_update,
    sample_history_scroll, save_auto_update_preference, save_theme_preference,
    save_ui_language_preference, set_global_shortcut_capture_after_wndproc, set_ui_language,
    set_update_status, should_hide_main_after_deactivation, should_replace_cached_artwork,
    should_start_startup_logo, show_main, start_capture, sync_main_timers,
    system_wheel_scroll_lines, track_tray_context_menu, update_global_shortcut_after_wndproc,
    update_phase_timers, update_visible_windows, valid_shazam_page_url, AppState, ArrowEdge,
    ArtworkLoadFailure, ArtworkReady, ArtworkRequestIdentity, BeginPaint, CaptureMode,
    DefWindowProcW, DestroyIcon, EndPaint, GetCapture, GetDoubleClickTime, GetDpiForWindow,
    GetWindowLongPtrW, HistoryScrollAnimation, InvalidateRect, KillTimer, MainHit, MainPhase,
    PopupWindowPlacement, PostMessageW, PostQuitMessage, ReleaseCapture, Renderer, ScreenToClient,
    SetCapture, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, ThemePreference,
    TrackMouseEvent, UiLanguage, UpdateInstallConfirmationPayload, UpdateStatus,
    ValidatedWebTrackUrl, WindowKind, CONTENT_WIDTH, GWLP_USERDATA, HISTORY_HEIGHT,
    HISTORY_SCROLL_TRANSITION, HWND, LONG_PRESS_TIMEOUT_MS, LPARAM, LRESULT, NIN_BALLOONHIDE,
    NIN_BALLOONTIMEOUT, NIN_BALLOONUSERCLICK, PHASE_TICK_MS, POINT, POPUP_ARROW_HEIGHT, RECT,
    SINGLE_CLICK_TIMEOUT_MS, SUCCESS_SETTLE_TOTAL, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW,
    SW_HIDE, SW_SHOWNORMAL, TIMER_ANIMATION, TIMER_DEFERRED_DEACTIVATION_HIDE,
    TIMER_HISTORY_SCROLL_SETTLE, TIMER_LONG_PRESS, TIMER_PHASE, TIMER_SINGLE_CLICK,
    TIMER_UPDATE_CHECK, TIMER_UPDATE_COMPLETION, TME_LEAVE, TOP_HEIGHT, TRACKMOUSEEVENT, VK_ESCAPE,
    WM_ACTIVATE, WM_ACTIVATEAPP, WM_ACTIVATE_MAIN_POPUP, WM_APPEARANCE_CHANGED,
    WM_APPLY_POPUP_POSITION, WM_APPLY_WINDOW_VISIBILITY, WM_ARTWORK_LOAD_FINISHED,
    WM_ARTWORK_READY, WM_CAPTURECHANGED, WM_CAPTURE_LOST, WM_CLOSE, WM_CREATE, WM_DESTROY,
    WM_DPICHANGED, WM_GLOBAL_SHORTCUT_CAPTURE, WM_GLOBAL_SHORTCUT_CHANGED, WM_HOTKEY, WM_KEYDOWN,
    WM_KILLFOCUS, WM_LANGUAGE_CHANGED, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSELEAVE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_MOVE, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCCREATE,
    WM_NCDESTROY, WM_OPEN_SETTINGS, WM_OPEN_TRACK_PAGE, WM_OS_THEME_CHANGED, WM_PAINT,
    WM_RBUTTONUP, WM_REFRESH_RENDER_SIZE, WM_SETFOCUS, WM_SETTINGCHANGE, WM_SHOWWINDOW,
    WM_SHOW_INITIAL_POPUP, WM_SIZE, WM_THEMECHANGED, WM_TIMECHANGE, WM_TIMER, WM_TRAYICON_MESSAGE,
    WM_UPDATE_ACTION, WM_UPDATE_AUTO_CHANGED, WM_UPDATE_INSTALL_CONFIRM,
    WM_UPDATE_WORKER_COMPLETED, WM_WINDOWPOSCHANGED, WM_WINDOWPOSCHANGING, WPARAM,
};
use super::{message_payload, recognition::poll_recognition_events};
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicU64, AtomicU8, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use windows::Win32::System::{Diagnostics::Debug::ReadProcessMemory, Threading::GetCurrentProcess};
use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;

struct WindowData {
    app: *mut AppState,
    generation: u64,
    kind: WindowKind,
    creation_state: Arc<AtomicU8>,
    renderer: Option<Renderer>,
}

pub(super) const WINDOW_DATA_CREATION_UNATTACHED: u8 = 0;
pub(super) const WINDOW_DATA_CREATION_ATTACHED: u8 = 1;
pub(super) const WINDOW_DATA_CREATION_DESTROYED: u8 = 2;
static NEXT_MESSAGE_GENERATION: AtomicU64 = AtomicU64::new(1);
const MAX_DPI_SUGGESTED_WINDOW_DIMENSION: i32 = 32_768;

fn start_shutdown_waiter_with_fallback(
    start_waiter: impl FnOnce() -> std::io::Result<()>,
    request_shutdown: impl FnOnce(),
) -> std::io::Result<()> {
    let result = start_waiter();
    if result.is_err() {
        request_shutdown();
    }
    result
}

pub(super) fn next_message_generation() -> u64 {
    NEXT_MESSAGE_GENERATION.fetch_add(1, Ordering::Relaxed)
}

// Creation and nonclient destruction run synchronously inside CreateWindowExW.
// This shared state leaves exactly one side responsible for reclaiming userdata.
pub(super) fn take_window_data_after_create_failure(creation_state: &AtomicU8) -> bool {
    creation_state.swap(WINDOW_DATA_CREATION_DESTROYED, Ordering::AcqRel)
        != WINDOW_DATA_CREATION_DESTROYED
}

fn mark_window_data_attached(creation_state: &AtomicU8) -> bool {
    creation_state
        .compare_exchange(
            WINDOW_DATA_CREATION_UNATTACHED,
            WINDOW_DATA_CREATION_ATTACHED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

// Owns the allocation until CreateWindowExW either installs it as
// GWLP_USERDATA or fails. After installation, WM_NCDESTROY owns the Box.
pub(super) struct WindowDataCreation {
    pointer: *mut WindowData,
    creation_state: Arc<AtomicU8>,
}

impl WindowDataCreation {
    /// # Safety
    /// `app` must point to a live `AppState` that outlives the created HWND.
    pub(super) unsafe fn new(app: *mut AppState, kind: WindowKind, generation: u64) -> Self {
        let creation_state = Arc::new(AtomicU8::new(WINDOW_DATA_CREATION_UNATTACHED));
        let window_data = Box::new(WindowData {
            app,
            generation,
            kind,
            creation_state: Arc::clone(&creation_state),
            renderer: None,
        });
        Self {
            pointer: Box::into_raw(window_data),
            creation_state,
        }
    }

    pub(super) fn create_param(&self) -> *mut c_void {
        self.pointer.cast()
    }

    pub(super) fn finish(self, result: windows::core::Result<HWND>) -> windows::core::Result<HWND> {
        if result.is_err() && take_window_data_after_create_failure(&self.creation_state) {
            // SAFETY: `pointer` came from `Box::into_raw` in `new`. The atomic
            // state arbitrates with WM_NCDESTROY, so only one side reconstructs
            // and drops the Box on a failed CreateWindowExW call.
            unsafe { drop(Box::from_raw(self.pointer)) };
        }
        result
    }
}
/// # Safety
/// `hwnd` must be the window currently dispatching this WndProc on its owning
/// thread. The caller must not retain the pointer past message dispatch.
#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn window_data(hwnd: HWND) -> Option<*mut WindowData> {
    // SAFETY: The caller guarantees a live HWND in its WndProc callback; the
    // value is only converted back to a pointer and is not dereferenced here.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowData };
    (!ptr.is_null()).then_some(ptr)
}

/// Safely read and bound the suggested window rectangle from `WM_DPICHANGED`.
/// `ReadProcessMemory` probes the supplied address so a malformed message
/// cannot make the application dereference it directly.
fn dpi_suggested_bounds(lparam: LPARAM) -> Option<(RECT, i32, i32)> {
    if lparam.0 == 0 {
        return None;
    }
    let mut rect = RECT::default();
    let mut bytes_read = 0usize;
    let rect_size = std::mem::size_of::<RECT>();
    // SAFETY: The pseudo handle always denotes this process; ReadProcessMemory
    // probes the untrusted source address in the kernel, while both output
    // buffers are live, writable stack values sized for the fixed RECT length.
    if unsafe {
        ReadProcessMemory(
            GetCurrentProcess(),
            lparam.0 as *const c_void,
            (&mut rect as *mut RECT).cast(),
            rect_size,
            Some(&mut bytes_read),
        )
    }
    .is_err()
        || bytes_read != rect_size
    {
        return None;
    }
    let width = rect.right.checked_sub(rect.left)?;
    let height = rect.bottom.checked_sub(rect.top)?;
    if width <= 0
        || height <= 0
        || width > MAX_DPI_SUGGESTED_WINDOW_DIMENSION
        || height > MAX_DPI_SUGGESTED_WINDOW_DIMENSION
    {
        return None;
    }
    Some((rect, width, height))
}

/// Handle painting without keeping mutable app state borrowed while GDI may
/// synchronously dispatch `WM_NCPAINT` or `WM_ERASEBKGND` from `BeginPaint`.
///
/// # Safety
/// `hwnd` must be the window currently dispatching `WM_PAINT` on its owning
/// thread, and its associated `AppState` must remain alive through dispatch.
#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn paint_window(hwnd: HWND) -> LRESULT {
    let mut paint = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
    // SAFETY: The caller dispatches WM_PAINT for this live window and `paint`
    // is writable for the duration of BeginPaint.
    let _ = unsafe { BeginPaint(hwnd, &mut paint) };

    if let Some(data_ptr) = unsafe { window_data(hwnd) } {
        // SAFETY: GWLP_USERDATA is installed by WM_NCCREATE and remains owned
        // until WM_NCDESTROY; AppState outlives all owned HWNDs. The mutable
        // references are confined to the UI-thread dispatch. The renderer has
        // no message pump, and BeginPaint/EndPaint stay outside this scope
        // because they can dispatch synchronous paint-related messages.
        let data = unsafe { &mut *data_ptr };
        let app = unsafe { &mut *data.app };
        if let Some(renderer) = data.renderer.as_mut() {
            let result_overlay_matches =
                data.kind == WindowKind::Main && renderer.result_overlay_matches_current(app);
            if data.kind == WindowKind::Main
                && (!result_overlay_matches
                    || animation_needs_frame(&app.phase, app.startup_animation_pending))
            {
                match renderer.update_composition(animation_snapshot(app)) {
                    Ok(()) => {
                        app.status_text_composition_ready =
                            renderer.status_text_composition_enabled();
                    }
                    Err(error) => {
                        app.status_text_composition_ready = false;
                        app.message = Some(format!("Windows Composition update failed: {error}"));
                    }
                }
            }
            // SAFETY: Renderer resources are used on the owning UI thread;
            // `app` and renderer remain live throughout the synchronous draw.
            let draw_result = unsafe { renderer.draw(app, data.kind) };
            app.history_composition_ready = draw_result.as_ref().copied().unwrap_or(false);
            if let Err(error) = draw_result {
                app.message = Some(format!("描画エラー: {error}"));
            }
        }
        if data.kind == WindowKind::Main {
            app.result_overlay_composition_ready = data
                .renderer
                .as_ref()
                .is_some_and(|renderer| renderer.result_overlay_matches_current(app));
            app.history_scroll_composition_ready = data
                .renderer
                .as_ref()
                .is_some_and(|renderer| renderer.history_scroll_composition_active());
            // SAFETY: `app` belongs to this live UI callback and `main_hwnd`
            // remains valid until WM_NCDESTROY on the same thread.
            unsafe { sync_main_timers(app) };
        }
    }

    // EndPaint is kept outside the AppState/WindowData borrow scope as well.
    // SAFETY: Pair with the BeginPaint above before returning from WM_PAINT.
    let _ = unsafe { EndPaint(hwnd, &paint) };
    LRESULT(0)
}

/// Prepare state for releasing the main popup's mouse capture.
///
/// # Safety
/// `hwnd` and `app` must belong to the currently-dispatching main window.
#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn prepare_main_button_capture_release(hwnd: HWND, app: &mut AppState) {
    // SAFETY: `hwnd` is the live main window currently dispatching this input.
    let _ = unsafe { KillTimer(Some(hwnd), TIMER_LONG_PRESS) };
    sample_history_scroll(app, Instant::now());
    let was_long_press_consumed = app.long_press_consumed;
    app.mouse_down_center = false;
    app.long_press_consumed = false;
    if !was_long_press_consumed {
        app.single_click_pending = true;
        // SAFETY: These timer queries and updates use the live owner HWND on
        // its UI thread; their integer parameters are bounded by the app policy.
        let timeout = unsafe { GetDoubleClickTime() }.clamp(180, SINGLE_CLICK_TIMEOUT_MS);
        let _ = unsafe { SetTimer(Some(hwnd), TIMER_SINGLE_CLICK, timeout, None) };
    }
    if app.visible {
        // SAFETY: The main window remains live for this active WndProc call.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    }
}

/// Attach the CreateWindowExW payload and register the matching window generation.
///
/// # Safety
/// Must be called only while dispatching `WM_NCCREATE` with the `CREATESTRUCTW`
/// pointer supplied by Windows. Its `lpCreateParams` must be the live allocation
/// owned by `WindowDataCreation` until attachment or creation failure is resolved.
#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn attach_window_data(hwnd: HWND, lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return false;
    }
    // SAFETY: Windows supplies a readable CREATESTRUCTW for WM_NCCREATE and
    // keeps it valid through this callback.
    let create =
        unsafe { &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW) };
    let data = create.lpCreateParams as *mut WindowData;
    if data.is_null() {
        return false;
    }
    // SAFETY: The caller retains the unique WindowData allocation through this
    // WM_NCCREATE callback; clone the handshake state before publishing the raw
    // pointer so no later work needs to dereference it after a possible reentry.
    let (generation, creation_state) =
        unsafe { ((*data).generation, Arc::clone(&(*data).creation_state)) };
    // SAFETY: `hwnd` is the newly created window dispatching WM_NCCREATE, and
    // `data` points to its still-owned, properly aligned WindowData allocation.
    let _ = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, data as isize) };
    // SAFETY: The same live HWND is queried immediately after storing userdata.
    if unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } != data as isize {
        return false;
    }

    message_payload::register_window(hwnd, generation);
    // Do not overwrite DESTROYED if a nested teardown claimed the allocation
    // while the payload registry discarded stale entries.
    mark_window_data_attached(&creation_state)
}

/// Reclaim the per-window userdata after removing its published pointer.
///
/// # Safety
/// Must be called exactly once from `WM_NCDESTROY` for a window that follows
/// the `WindowDataCreation` ownership protocol, on the AppState-owning UI thread.
#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn detach_window_data(hwnd: HWND) -> Option<u64> {
    // SAFETY: WM_NCDESTROY is dispatched for the live window whose userdata was
    // installed by attach_window_data.
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowData };
    // SAFETY: The window is still in its nonclient-destroy callback; clear the
    // slot before reconstructing the unique Box to prevent a second owner.
    let _ = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
    if pointer.is_null() {
        return None;
    }

    // SAFETY: This is the unique Box::into_raw pointer transferred at WM_NCCREATE;
    // clearing the slot above and the one-shot WM_NCDESTROY callback ensure one drop.
    let data = unsafe { Box::from_raw(pointer) };
    let app_ptr = data.app;
    let kind = data.kind;
    let generation = data.generation;
    data.creation_state
        .store(WINDOW_DATA_CREATION_DESTROYED, Ordering::Release);
    drop(data);
    if !app_ptr.is_null() {
        // SAFETY: AppState remains owned by the UI loop until all HWNDs have
        // received WM_NCDESTROY. No WindowData or AppState reference is retained
        // across any window API that can dispatch another message.
        let app = unsafe { &mut *app_ptr };
        match kind {
            WindowKind::Main => app.main_hwnd = HWND::default(),
        }
    }
    Some(generation)
}

/// # Safety
/// Register this callback only for windows whose `GWLP_USERDATA` follows the
/// `WindowDataCreation` ownership protocol. Windows must invoke it as a WndProc;
/// each associated `AppState` must remain alive until all its HWNDs are destroyed.
#[deny(unsafe_op_in_unsafe_fn)]
pub(super) unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: This branch is entered only for Windows' WM_NCCREATE callback;
        // CreateWindowExW retains the payload owner until this branch returns.
        return LRESULT(unsafe { attach_window_data(hwnd, lparam) } as isize);
    }

    if message == WM_NCDESTROY {
        // SAFETY: The callback's registration contract guarantees the userdata
        // protocol and exactly one WM_NCDESTROY owner for this HWND.
        let generation = unsafe { detach_window_data(hwnd) };
        message_payload::close_window(hwnd, generation);
        // SAFETY: `hwnd` and the original message parameters belong to this
        // active WndProc call; DefWindowProcW is the required forwarding path.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }

    if message == WM_SIZE {
        // SAFETY: The live callback HWND is the target of a bounded internal
        // message; posting defers renderer access until WM_SIZE has unwound.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_REFRESH_RENDER_SIZE, WPARAM(0), LPARAM(0)) };
        return LRESULT(0);
    }
    if message == WM_REFRESH_RENDER_SIZE {
        // SAFETY: Userdata is installed and owned by this live WndProc HWND;
        // the queued message runs on its UI thread. on_size updates render
        // resources and queries client bounds without pumping messages.
        unsafe {
            if let Some(data_ptr) = window_data(hwnd) {
                if let Some(renderer) = (*data_ptr).renderer.as_mut() {
                    renderer.on_size(hwnd);
                }
            }
        }
        return LRESULT(0);
    }
    if message == WM_DPICHANGED {
        let Some((suggested, width, height)) = dpi_suggested_bounds(lparam) else {
            return LRESULT(0);
        };
        let new_dpi = ((wparam.0 >> 16) & 0xffff) as u32;
        // SAFETY: This callback owns the live HWND and the suggested bounds
        // have already been probed and validated for positive bounded sizes.
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                None,
                suggested.left,
                suggested.top,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
        // SAFETY: The window userdata protocol keeps WindowData/AppState live
        // through this same-thread callback. DPI resource updates do not pump
        // messages, and SetWindowPos completed before these mutable references
        // were created so synchronous window notifications cannot alias them.
        unsafe {
            if let Some(data_ptr) = window_data(hwnd) {
                let data = &mut *data_ptr;
                let app = &mut *data.app;
                if let Some(renderer) = data.renderer.as_mut() {
                    renderer.on_dpi_changed(new_dpi, hwnd, app);
                }
            }
        }
        return LRESULT(0);
    }
    if message == WM_APPLY_POPUP_POSITION {
        // SAFETY: The live WndProc HWND owns this userdata until NCDESTROY;
        // the message carries only a registry token and never a borrowed pointer.
        let Some(data_ptr) = (unsafe { window_data(hwnd) }) else {
            return LRESULT(0);
        };
        // SAFETY: WindowData remains live for this dispatch because NCDESTROY
        // cannot run concurrently on another thread for a UI-thread HWND.
        if unsafe { (*data_ptr).kind } != WindowKind::Main {
            return LRESULT(0);
        }
        // SAFETY: Read the generation from the unique WindowData allocation
        // associated with this active HWND before consuming its queued payload.
        if let Some(placement) = message_payload::take::<PopupWindowPlacement>(
            hwnd,
            unsafe { (*data_ptr).generation },
            message,
            lparam.0,
        ) {
            // SAFETY: This is the active target HWND and the registered
            // placement is produced by the app's bounded popup-position logic.
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    placement.x,
                    placement.y,
                    placement.width,
                    placement.height,
                    SWP_SHOWWINDOW | SWP_NOACTIVATE,
                )
            };
        }
        return LRESULT(0);
    }
    if message == WM_APPLY_WINDOW_VISIBILITY {
        if wparam.0 == 0 {
            // SAFETY: The active callback HWND is the window whose visibility
            // command is being dispatched; no AppState borrow is held here.
            let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
        } else {
            // SAFETY: Both APIs operate on the live callback HWND. No Rust
            // references into WindowData or AppState are held across reentry.
            let _ = unsafe { ShowWindow(hwnd, SW_SHOWNORMAL) };
            let _ = unsafe { SetForegroundWindow(hwnd) };
        }
        return LRESULT(0);
    }
    if message == WM_OPEN_TRACK_PAGE {
        // SAFETY: The active WndProc HWND owns WindowData through this dispatch.
        let Some(data_ptr) = (unsafe { window_data(hwnd) }) else {
            return LRESULT(0);
        };
        // SAFETY: This allocation remains live until this UI-thread callback
        // returns; this read does not retain a reference across an API call.
        if unsafe { (*data_ptr).kind } != WindowKind::Main {
            return LRESULT(0);
        }
        // SAFETY: Copy stable fields from WindowData before payload handling
        // can drop entries or invoke their destructors.
        let (app_ptr, generation) = unsafe { ((*data_ptr).app, (*data_ptr).generation) };
        let Some(url) =
            message_payload::take::<ValidatedWebTrackUrl>(hwnd, generation, message, lparam.0)
        else {
            return LRESULT(0);
        };
        if !valid_shazam_page_url(url.as_str()) {
            return LRESULT(0);
        }
        // SAFETY: The active main HWND is passed to the track-navigation helper;
        // no WindowData/AppState reference is retained across shell calls.
        if let Err(error) = unsafe { open_track_page(hwnd, &url) } {
            eprintln!("OpenShazam could not launch the Shazam page: {error}");
            if !app_ptr.is_null() {
                // SAFETY: AppState outlives all owned HWNDs, including a nested
                // shell activation while this callback is on the stack.
                let app = unsafe { &mut *app_ptr };
                app.message = Some("Could not open the track page".to_owned());
                if app.main_hwnd == hwnd {
                    // SAFETY: The HWND is checked against the live app; this
                    // schedules repaint work without borrowing the app state.
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                }
            }
        }
        return LRESULT(0);
    }
    if message == WM_ACTIVATE_MAIN_POPUP {
        // SAFETY: WindowData and its AppState belong to the active main HWND;
        // the short reads finish before SetForegroundWindow may reenter.
        let should_foreground = unsafe {
            window_data(hwnd).is_some_and(|data_ptr| {
                (*data_ptr).kind == WindowKind::Main && (*(*data_ptr).app).visible
            })
        };
        if should_foreground {
            // SAFETY: No WindowData/AppState borrow remains active here.
            let _ = unsafe { SetForegroundWindow(hwnd) };
        }
        return LRESULT(0);
    }
    if matches!(
        message,
        WM_MOVE
            | WM_NCCALCSIZE
            | WM_NCACTIVATE
            | WM_ACTIVATEAPP
            | WM_SHOWWINDOW
            | WM_WINDOWPOSCHANGING
            | WM_WINDOWPOSCHANGED
    ) {
        // SAFETY: Forward the unhandled message for this active WndProc HWND.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    if message == WM_CAPTURECHANGED {
        // SAFETY: WindowData belongs to the active callback HWND and is read
        // before posting the deferred capture-loss event.
        let is_main =
            unsafe { window_data(hwnd).is_some_and(|data| (*data).kind == WindowKind::Main) };
        if is_main {
            // SAFETY: Defer state changes until Windows finishes this callback.
            let _ = unsafe { PostMessageW(Some(hwnd), WM_CAPTURE_LOST, WPARAM(0), LPARAM(0)) };
        }
        // SAFETY: Preserve default capture-change processing for the active HWND.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    if message == WM_CAPTURE_LOST {
        // SAFETY: This deferred private message runs on the owner UI thread;
        // userdata remains live through the callback. These timer and repaint
        // requests do not synchronously dispatch window messages.
        unsafe {
            if let Some(data_ptr) = window_data(hwnd) {
                if (*data_ptr).kind == WindowKind::Main && GetCapture() != hwnd {
                    let app = &mut *(*data_ptr).app;
                    if app.mouse_down_center {
                        let _ = KillTimer(Some(hwnd), TIMER_LONG_PRESS);
                        app.mouse_down_center = false;
                        app.long_press_consumed = false;
                        if app.visible {
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    }
                }
            }
        }
        return LRESULT(0);
    }
    if matches!(message, WM_SETFOCUS | WM_KILLFOCUS | WM_ACTIVATE) {
        // SAFETY: Focus messages run on the owner UI thread with live userdata.
        // The state helpers only update fields and do not call Win32 APIs.
        unsafe {
            if let Some(data_ptr) = window_data(hwnd) {
                if (*data_ptr).kind == WindowKind::Main {
                    let app = &mut *(*data_ptr).app;
                    match message {
                        WM_SETFOCUS => {
                            cancel_deferred_deactivation_hide(app);
                            app.main_activated = true;
                        }
                        WM_KILLFOCUS => defer_or_hide_main_after_deactivation(app),
                        WM_ACTIVATE => {
                            let activation = wparam.0 & 0xffff;
                            if activation == 0 {
                                defer_or_hide_main_after_deactivation(app);
                            } else {
                                cancel_deferred_deactivation_hide(app);
                                app.main_activated = true;
                            }
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }
        // SAFETY: Delegate standard focus and activation behavior to Windows.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }

    if message == WM_TRAYICON_MESSAGE
        && (lparam.0 as u32 & 0xffff) == windows::Win32::UI::WindowsAndMessaging::WM_CONTEXTMENU
    {
        // SAFETY: This tray callback is for the active owner HWND; copy only
        // stable identity fields before entering the menu's nested message loop.
        let Some((generation, is_main)) = (unsafe {
            window_data(hwnd).map(|data| ((*data).generation, (*data).kind == WindowKind::Main))
        }) else {
            return LRESULT(0);
        };
        if !is_main {
            return LRESULT(0);
        }
        // TrackPopupMenu pumps a nested message loop. Run it before borrowing
        // either userdata or AppState: nested messages can destroy the HWND and
        // WM_NCDESTROY reclaims its WindowData allocation synchronously.
        // SAFETY: `hwnd` is live at entry; reacquire userdata after this helper's
        // nested loop because it may destroy the window and free the allocation.
        let selected = unsafe { track_tray_context_menu(hwnd) };
        // SAFETY: Fetch the current pointer after the nested loop completes.
        let Some(data_ptr) = (unsafe { window_data(hwnd) }) else {
            return LRESULT(0);
        };
        // SAFETY: The freshly fetched WindowData remains live through dispatch;
        // this generation check rejects an HWND reused during the menu loop.
        if unsafe { (*data_ptr).kind != WindowKind::Main || (*data_ptr).generation != generation } {
            return LRESULT(0);
        }
        // SAFETY: Copy AppState's stable pointer from this WindowData allocation.
        let app_ptr = unsafe { (*data_ptr).app };
        if app_ptr.is_null() {
            return LRESULT(0);
        }
        // SAFETY: AppState outlives all owned HWNDs. This helper queues menu
        // actions and does not enter a nested message loop while borrowed.
        return unsafe { apply_tray_menu_action(&mut *app_ptr, selected) };
    }

    // This must run before general message dispatch takes mutable references to
    // WindowData or AppState: showing WinUI synchronously re-enters this WndProc.
    if message == WM_OPEN_SETTINGS {
        // SAFETY: The active owner HWND retains its userdata through this dispatch.
        let Some(data_ptr) = (unsafe { window_data(hwnd) }) else {
            return LRESULT(0);
        };
        // SAFETY: The helper queues WinUI work until this WndProc returns.
        return unsafe { open_settings_after_wndproc((*data_ptr).app) };
    }
    if message == WM_GLOBAL_SHORTCUT_CHANGED || message == WM_GLOBAL_SHORTCUT_CAPTURE {
        // SAFETY: The active owner HWND retains its userdata through this dispatch.
        let Some(data_ptr) = (unsafe { window_data(hwnd) }) else {
            return LRESULT(0);
        };
        // SAFETY: The unique WindowData allocation remains live through callback return.
        if unsafe { (*data_ptr).kind } != WindowKind::Main {
            return LRESULT(0);
        }
        // SAFETY: Copy the stable AppState pointer before passing it onward.
        let app_ptr = unsafe { (*data_ptr).app };
        if message == WM_GLOBAL_SHORTCUT_CHANGED {
            // SAFETY: The helper confines pointer access to this UI-thread dispatch.
            return unsafe {
                update_global_shortcut_after_wndproc(app_ptr, wparam.0 as u32, lparam.0 as u32)
            };
        }
        // SAFETY: The helper confines pointer access to this UI-thread dispatch.
        return unsafe { set_global_shortcut_capture_after_wndproc(app_ptr, wparam.0 != 0) };
    }
    if message == WM_UPDATE_INSTALL_CONFIRM {
        // SAFETY: The active main HWND retains userdata throughout this callback.
        let Some(data_ptr) = (unsafe { window_data(hwnd) }) else {
            return LRESULT(0);
        };
        // SAFETY: WindowData remains live until this callback returns.
        if unsafe { (*data_ptr).kind } != WindowKind::Main {
            return LRESULT(0);
        }
        // SAFETY: Copy identity before consuming the message payload because
        // its destructor could re-enter the owning message loop.
        let (generation, app_ptr) = unsafe { ((*data_ptr).generation, (*data_ptr).app) };
        let Some(payload) = message_payload::take::<UpdateInstallConfirmationPayload>(
            hwnd, generation, message, lparam.0,
        ) else {
            return LRESULT(0);
        };
        // SAFETY: AppState outlives all owned HWNDs. This helper keeps only the
        // raw pointer across its modal prompt and borrows it after the loop exits.
        return unsafe { handle_update_install_confirmation(app_ptr, hwnd, payload) };
    }

    // ReleaseCapture synchronously sends WM_CAPTURECHANGED to this window.
    // Prepare state in a short borrow, then release capture after that borrow
    // and all general WndProc references are out of scope.
    if message == WM_LBUTTONUP {
        // SAFETY: WindowData/AppState belong to this live UI-thread callback.
        // The mutable borrow is confined to prepare_main_button_capture_release
        // and ends before ReleaseCapture can synchronously dispatch a message.
        unsafe {
            if let Some(data_ptr) = window_data(hwnd) {
                if (*data_ptr).kind == WindowKind::Main {
                    let app_ptr = (*data_ptr).app;
                    if app_ptr.is_null() {
                        return LRESULT(0);
                    }
                    if (*app_ptr).mouse_down_center {
                        prepare_main_button_capture_release(hwnd, &mut *app_ptr);
                        if GetCapture() == hwnd {
                            let _ = ReleaseCapture();
                        }
                        return LRESULT(0);
                    }
                }
            }
        }
    }

    // SetCapture can synchronously notify the previous capture window. All
    // windows share AppState, so commit the button state before calling it.
    if message == WM_LBUTTONDOWN {
        // SAFETY: This message is dispatched on the owner thread while userdata
        // is live. State is committed in a short borrow before SetCapture can
        // synchronously notify a different window sharing AppState.
        unsafe {
            if let Some(data_ptr) = window_data(hwnd) {
                if (*data_ptr).kind == WindowKind::Main {
                    let app_ptr = (*data_ptr).app;
                    if app_ptr.is_null() {
                        return LRESULT(0);
                    }
                    let (x, y) = client_dips(hwnd, lparam);
                    let body_top = if (*app_ptr).arrow_edge == ArrowEdge::Top {
                        POPUP_ARROW_HEIGHT as f32
                    } else {
                        0.0
                    };
                    let should_capture = {
                        let app = &mut *app_ptr;
                        app.mouse_down_center =
                            main_hit_test(app, x, y, body_top) == MainHit::CenterButton;
                        app.long_press_consumed = false;
                        app.mouse_down_center
                    };
                    if should_capture {
                        let _ = SetCapture(hwnd);
                        let _ = SetTimer(Some(hwnd), TIMER_LONG_PRESS, LONG_PRESS_TIMEOUT_MS, None);
                    }
                    return LRESULT(0);
                }
            }
        }
    }

    if message == WM_PAINT {
        // SAFETY: This message is dispatched for the live HWND whose WindowData
        // and AppState remain owned until WM_NCDESTROY completes.
        return unsafe { paint_window(hwnd) };
    }

    // SAFETY: The active WndProc HWND owns this userdata until the earlier
    // WM_NCDESTROY branch removes it. The callback is serialized on the UI
    // thread, and AppState outlives all of its owned windows.
    let Some(data_ptr) = (unsafe { window_data(hwnd) }) else {
        // SAFETY: Forward an unhandled message for this active WndProc HWND.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    let mut call_default_proc = false;
    // SAFETY: This is the serialized per-window UI dispatch. Messages that
    // enter nested loops or synchronously re-enter the WndProc (Settings,
    // update confirmation, tray menus, capture changes, and popup positioning)
    // are handled above this block. The remaining handlers use state-only
    // operations, asynchronous PostMessage/paint/timer requests, or renderer
    // operations that do not pump the message loop. The userdata pointer is
    // therefore unique for this callback and remains live through this match.
    let result = unsafe {
        let data = &mut *data_ptr;
        let app = &mut *data.app;

        match message {
            WM_UPDATE_WORKER_COMPLETED if data.kind == WindowKind::Main => {
                drain_update_worker_completions(app);
                LRESULT(0)
            }
            WM_UPDATE_AUTO_CHANGED if data.kind == WindowKind::Main => {
                let enabled = wparam.0 != 0;
                let result = match app.update_auto_install_gate.lock() {
                    Ok(_gate) => match save_auto_update_preference(enabled) {
                        Ok(()) => {
                            app.update_auto_enabled_shared
                                .store(enabled, Ordering::Release);
                            Ok(())
                        }
                        Err(error) => Err(format!("Windows error {error}")),
                    },
                    Err(_) => Err("the update installer could not be synchronized".to_owned()),
                };
                match result {
                    Ok(()) => {
                        app.update_auto_enabled = enabled;
                        if !enabled {
                            cancel_pending_automatic_update_install(app);
                        }
                        if enabled && !app.update_operation_active {
                            begin_update_check(app, false);
                        }
                    }
                    Err(error) => {
                        set_update_status(
                            app,
                            UpdateStatus::Error,
                            "",
                            &format!("Could not save automatic update setting: {error}."),
                        );
                    }
                }
                if let Some(host) = app.settings_host.as_ref() {
                    host.update_status(
                        app.update_auto_enabled,
                        app.update_status.id(),
                        &app.update_version,
                        &app.update_detail,
                        app.update_last_checked,
                    );
                }
                LRESULT(0)
            }
            WM_UPDATE_ACTION if data.kind == WindowKind::Main => {
                if !app.update_operation_active {
                    if wparam.0 != 0 && app.update_status == UpdateStatus::Available {
                        if let Some(candidate) = app.update_candidate.clone() {
                            begin_update_download(app, candidate, false);
                        } else {
                            begin_update_check(app, true);
                        }
                    } else {
                        begin_update_check(app, true);
                    }
                }
                LRESULT(0)
            }
            WM_TIMER if data.kind == WindowKind::Main && wparam.0 == TIMER_UPDATE_CHECK => {
                begin_update_check(app, false);
                LRESULT(0)
            }
            WM_TIMER if data.kind == WindowKind::Main && wparam.0 == TIMER_UPDATE_COMPLETION => {
                drain_update_worker_completions(app);
                LRESULT(0)
            }
            WM_HOTKEY
                if data.kind == WindowKind::Main
                    && app.global_shortcut_registration_id == Some(wparam.0 as i32) =>
            {
                if !app.visible {
                    show_main(app, data.renderer.as_mut());
                }
                start_capture(app, CaptureMode::Manual);
                update_visible_windows(app);
                LRESULT(0)
            }
            _ if message == app.taskbar_created_message && app.taskbar_created_message != 0 => {
                restore_tray_icon(app);
                LRESULT(0)
            }
            WM_SHOW_INITIAL_POPUP if data.kind == WindowKind::Main => {
                show_main(app, data.renderer.as_mut());
                LRESULT(0)
            }
            WM_ARTWORK_READY if data.kind == WindowKind::Main => {
                if let Some(ready) =
                    message_payload::take::<ArtworkReady>(hwnd, data.generation, message, lparam.0)
                {
                    let identity = ready.identity;
                    let bound_track = app
                        .history
                        .iter()
                        .find(|track| track.display_id == identity.track.display_id);
                    let current_request = app.active_artwork_requests.get(&identity.track);
                    if artwork_request_matches_track(current_request, identity, bound_track) {
                        app.artwork_failures.remove(&identity.track);
                        let pixels = ready.pixels;
                        let existing_is_better = app
                            .artwork_resources
                            .get(&identity.track.display_id)
                            .is_some_and(|(source, cached)| {
                                !should_replace_cached_artwork(
                                    Some((
                                        *source,
                                        cached.width,
                                        cached.height,
                                        cached.fingerprint,
                                    )),
                                    ready.source,
                                    pixels.width,
                                    pixels.height,
                                    pixels.fingerprint,
                                )
                            });
                        if !existing_is_better {
                            app.artwork_resources
                                .insert(identity.track.display_id, (ready.source, pixels));
                            const ARTWORK_CACHE_LIMIT: usize = 24 * 1024 * 1024;
                            while app
                                .artwork_resources
                                .values()
                                .map(|(_, image)| image.bgra.len())
                                .sum::<usize>()
                                > ARTWORK_CACHE_LIMIT
                            {
                                let Some(oldest) = app.artwork_resources.keys().copied().min()
                                else {
                                    break;
                                };
                                app.artwork_resources.remove(&oldest);
                            }
                        }
                        if let Some(track) = app
                            .pending_match_notifications
                            .remove(&identity.track.display_id)
                        {
                            let pixels = app
                                .artwork_resources
                                .get(&identity.track.display_id)
                                .map(|(_, pixels)| pixels.clone());
                            super::notifications::notify_hidden_match(app, &track, pixels.as_ref());
                        }
                        if app.visible {
                            let _ = InvalidateRect(Some(app.main_hwnd), None, false);
                        }
                    }
                }
                LRESULT(0)
            }
            WM_ARTWORK_LOAD_FINISHED if data.kind == WindowKind::Main => {
                if let Some(identity) = message_payload::take::<ArtworkRequestIdentity>(
                    hwnd,
                    data.generation,
                    message,
                    lparam.0,
                ) {
                    let completed_request = app
                        .active_artwork_requests
                        .get(&identity.track)
                        .filter(|request| request.identity == identity)
                        .cloned();
                    if finish_active_artwork_request(&mut app.active_artwork_requests, identity) {
                        if let Some(track) = app
                            .pending_match_notifications
                            .remove(&identity.track.display_id)
                        {
                            let pixels = app
                                .artwork_resources
                                .get(&identity.track.display_id)
                                .map(|(_, pixels)| pixels.clone());
                            super::notifications::notify_hidden_match(app, &track, pixels.as_ref());
                        }
                        let track_is_retained = app
                            .history
                            .iter()
                            .any(|track| track.display_id == identity.track.display_id);
                        if !track_is_retained {
                            app.artwork_generations.remove(&identity.track);
                            app.artwork_failures.remove(&identity.track);
                            app.artwork_resources.remove(&identity.track.display_id);
                        } else if !app
                            .artwork_resources
                            .contains_key(&identity.track.display_id)
                        {
                            if let Some(request) = completed_request {
                                app.artwork_failures.insert(
                                    identity.track,
                                    ArtworkLoadFailure {
                                        standard_url: request.standard_url,
                                        high_quality_url: request.high_quality_url,
                                        failed_at: Instant::now(),
                                    },
                                );
                            }
                        }
                    }
                    // Scrolling may have changed while the bounded artwork
                    // workers were busy. Start requests for the current
                    // viewport as soon as this worker releases its slot.
                    let scroll_offset = app.scroll_offset;
                    request_history_artwork_at_offset(app, scroll_offset);
                }
                LRESULT(0)
            }
            WM_OS_THEME_CHANGED | WM_SETTINGCHANGE | WM_THEMECHANGED | WM_TIMECHANGE
                if data.kind == WindowKind::Main =>
            {
                refresh_os_theme(app);
                refresh_history_date_labels(&mut app.history);
                update_visible_windows(app);
                LRESULT(0)
            }
            WM_APPEARANCE_CHANGED if data.kind == WindowKind::Main => {
                let preference = ThemePreference::from_raw(wparam.0 as u32);
                if app.theme_preference != preference {
                    app.theme_preference = preference;
                    save_theme_preference(preference);
                    refresh_os_theme(app);
                    update_visible_windows(app);
                }
                LRESULT(0)
            }
            WM_LANGUAGE_CHANGED if data.kind == WindowKind::Main => {
                let Some(language) = UiLanguage::from_id(wparam.0 as u32) else {
                    return LRESULT(0);
                };
                if let Err(error) = save_ui_language_preference(language) {
                    eprintln!(
                        "OpenShazam could not save the display language (Windows error {error})."
                    );
                    return LRESULT(0);
                }
                set_ui_language(language);
                refresh_history_date_labels(&mut app.history);
                if let Some(renderer) = data.renderer.as_mut() {
                    match renderer.update_composition(animation_snapshot(app)) {
                        Ok(()) => {
                            app.status_text_composition_ready =
                                renderer.status_text_composition_enabled()
                        }
                        Err(error) => {
                            app.status_text_composition_ready = false;
                            app.message = Some(format!(
                                "Windows Composition language update failed: {error}"
                            ));
                        }
                    }
                }
                update_visible_windows(app);
                LRESULT(1)
            }
            WM_CREATE => {
                let dpi = GetDpiForWindow(hwnd).max(96);
                let animate_startup_logo = data.kind == WindowKind::Main
                    && should_start_startup_logo(app.startup_logo_consumed);
                match Renderer::new(
                    hwnd,
                    app.d2d_factory.clone(),
                    app.dwrite_factory.clone(),
                    app.compositor.clone(),
                    dpi,
                    app.arrow_edge,
                    app.arrow_center,
                    app.theme_generation,
                    app.theme,
                    app.system_foreground,
                    data.kind,
                    animate_startup_logo,
                ) {
                    Ok(renderer) => {
                        if animate_startup_logo {
                            app.startup_logo_consumed = true;
                        }
                        if data.kind == WindowKind::Main {
                            app.startup_animation_pending = renderer.startup_logo_incomplete();
                        }
                        data.renderer = Some(renderer);
                    }
                    Err(error) => app.message = Some(format!("Direct2D初期化失敗: {error}")),
                }
                LRESULT(0)
            }
            WM_TRAYICON_MESSAGE if data.kind == WindowKind::Main => {
                // NOTIFYICON_VERSION_4 packs the event code into LOWORD(lParam)
                // and the icon ID into HIWORD(lParam). Comparing the full value
                // silently drops clicks because the upper word is nonzero.
                let event = (lparam.0 as u32 & 0xffff) as u16;
                if is_tray_activation_event(event) {
                    cancel_deferred_deactivation_hide(app);
                    if app.visible {
                        hide_main(app);
                    } else {
                        show_main(app, data.renderer.as_mut());
                    }
                } else if matches!(
                    u32::from(event),
                    NIN_BALLOONHIDE | NIN_BALLOONTIMEOUT | NIN_BALLOONUSERCLICK
                ) {
                    if let Some(Some(icon)) = app.owned_balloon_icons.pop_front() {
                        let _ = DestroyIcon(icon);
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK if data.kind == WindowKind::Main => {
                let _ = KillTimer(Some(hwnd), TIMER_SINGLE_CLICK);
                let _ = KillTimer(Some(hwnd), TIMER_LONG_PRESS);
                app.single_click_pending = false;
                sample_history_scroll(app, Instant::now());
                let (x, y) = client_dips(hwnd, lparam);
                let body_top = if app.arrow_edge == ArrowEdge::Top {
                    POPUP_ARROW_HEIGHT as f32
                } else {
                    0.0
                };
                if main_hit_test(app, x, y, body_top) == MainHit::CenterButton {
                    app.mouse_down_center = true;
                    app.long_press_consumed = true;
                    if capture_mode_in_progress(&app.phase) == Some(CaptureMode::Auto) {
                        cancel_capture(app);
                    } else {
                        start_capture(app, CaptureMode::Auto);
                    }
                } else {
                    app.mouse_down_center = false;
                    app.long_press_consumed = false;
                    process_main_hit(app, main_hit_test(app, x, y, body_top));
                }
                update_visible_windows(app);
                LRESULT(0)
            }
            WM_LBUTTONUP if data.kind == WindowKind::Main => {
                let _ = KillTimer(Some(hwnd), TIMER_LONG_PRESS);
                sample_history_scroll(app, Instant::now());
                let (x, y) = client_dips(hwnd, lparam);
                let body_top = if app.arrow_edge == ArrowEdge::Top {
                    POPUP_ARROW_HEIGHT as f32
                } else {
                    0.0
                };
                let hit = main_hit_test(app, x, y, body_top);
                process_main_hit(app, hit);
                update_visible_windows(app);
                LRESULT(0)
            }
            WM_TIMER if data.kind == WindowKind::Main && wparam.0 == TIMER_PHASE => {
                let recognition_changed = poll_recognition_events(app);
                advance_pending_update_install(app);
                resume_auto_shazam_after_update(app);
                let settle_update_due = matches!(
                    &app.phase,
                    MainPhase::Matched { track, .. }
                        if track.recognized_at.elapsed()
                            < SUCCESS_SETTLE_TOTAL + Duration::from_millis(u64::from(PHASE_TICK_MS))
                );
                let phase_changed = update_phase_timers(app);
                if settle_update_due {
                    // The success settle and result-card motion are already
                    // running on Composition's clock. Poll their completion at
                    // the phase cadence without repainting/presenting the full
                    // layered popup every 100 ms.
                    if let Some(renderer) = data.renderer.as_mut() {
                        match renderer.update_composition(animation_snapshot(app)) {
                            Ok(()) => {
                                app.status_text_composition_ready =
                                    renderer.status_text_composition_enabled();
                            }
                            Err(error) => {
                                app.status_text_composition_ready = false;
                                app.message =
                                    Some(format!("Windows Composition update failed: {error}"));
                            }
                        }
                    }
                }
                if phase_changed || recognition_changed {
                    update_visible_windows(app);
                }
                LRESULT(0)
            }
            WM_TIMER
                if data.kind == WindowKind::Main
                    && wparam.0 == TIMER_DEFERRED_DEACTIVATION_HIDE =>
            {
                cancel_deferred_deactivation_hide(app);
                if should_hide_main_after_deactivation(app, Instant::now()) {
                    hide_main(app);
                    update_visible_windows(app);
                }
                LRESULT(0)
            }
            WM_TIMER
                if data.kind == WindowKind::Main && wparam.0 == TIMER_HISTORY_SCROLL_SETTLE =>
            {
                let _ = KillTimer(Some(hwnd), TIMER_HISTORY_SCROLL_SETTLE);
                if let Some(animation) = app.history_scroll_animation {
                    let now = Instant::now();
                    let (offset, still_animating) = history_scroll_offset_at(animation, now);
                    app.scroll_offset = if still_animating {
                        offset
                    } else {
                        animation.to_offset
                    };
                    if still_animating && app.visible {
                        let remaining = HISTORY_SCROLL_TRANSITION
                            .saturating_sub(now.saturating_duration_since(animation.started_at));
                        let timeout = remaining.as_millis().clamp(1, u32::MAX as u128) as u32;
                        let _ = SetTimer(Some(hwnd), TIMER_HISTORY_SCROLL_SETTLE, timeout, None);
                    } else {
                        app.history_scroll_animation = None;
                        app.history_scroll_composition_ready = false;
                        if app.visible {
                            app.hovered_row = hovered_history_row_for_cursor(app);
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                        sync_main_timers(app);
                    }
                }
                LRESULT(0)
            }
            WM_TIMER if data.kind == WindowKind::Main && wparam.0 == TIMER_ANIMATION => {
                if app.visible {
                    let now = Instant::now();
                    let hovered_row_before = app.hovered_row;
                    let history_scroll_composition_active = data
                        .renderer
                        .as_ref()
                        .is_some_and(|renderer| renderer.history_scroll_composition_active());
                    let history_scroll_moved = advance_history_scroll(app, now);
                    if history_scroll_moved {
                        app.hovered_row = if history_scroll_composition_active
                            && app.history_scroll_animation.is_some()
                        {
                            None
                        } else {
                            hovered_history_row_for_cursor(app)
                        };
                    }
                    let history_scroll_finished =
                        history_scroll_moved && app.history_scroll_animation.is_none();
                    if history_scroll_finished {
                        let _ = KillTimer(Some(hwnd), TIMER_HISTORY_SCROLL_SETTLE);
                    }
                    let history_scroll_needs_d2d_redraw = history_scroll_moved
                        && (!history_scroll_composition_active
                            || history_scroll_finished
                            || app.hovered_row != hovered_row_before);
                    let composition_frame_needed =
                        animation_needs_frame(&app.phase, app.startup_animation_pending);
                    if composition_frame_needed {
                        if let Some(renderer) = data.renderer.as_mut() {
                            match renderer.update_composition(animation_snapshot(app)) {
                                Ok(()) => {
                                    app.status_text_composition_ready =
                                        renderer.status_text_composition_enabled();
                                    if app.startup_animation_pending {
                                        app.startup_animation_pending =
                                            renderer.startup_logo_incomplete();
                                    }
                                }
                                Err(error) => {
                                    app.status_text_composition_ready = false;
                                    app.message =
                                        Some(format!("Windows Composition update failed: {error}"));
                                    app.startup_animation_pending = false;
                                }
                            }
                        }
                    }
                    let status_text_animating = app
                        .status_alternator
                        .is_some_and(|status| status.animating(now));
                    let result_overlay_composition_ready = data
                        .renderer
                        .as_ref()
                        .is_some_and(|renderer| renderer.result_overlay_matches_current(app));
                    app.result_overlay_composition_ready = result_overlay_composition_ready;
                    let history_row_animating = history_transition_requires_d2d_frame(
                        app.history_transition,
                        now,
                        app.history_composition_ready,
                    );
                    if history_scroll_needs_d2d_redraw
                        || history_row_animating
                        || animation_needs_d2d_redraw(
                            &app.phase,
                            status_text_animating,
                            app.status_text_composition_ready,
                            result_overlay_composition_ready,
                        )
                    {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    sync_main_timers(app);
                }
                LRESULT(0)
            }
            WM_TIMER if data.kind == WindowKind::Main && wparam.0 == TIMER_SINGLE_CLICK => {
                let _ = KillTimer(Some(hwnd), TIMER_SINGLE_CLICK);
                if app.single_click_pending {
                    app.single_click_pending = false;
                    process_main_hit(app, MainHit::CenterButton);
                    update_visible_windows(app);
                }
                LRESULT(0)
            }
            WM_TIMER if data.kind == WindowKind::Main && wparam.0 == TIMER_LONG_PRESS => {
                let _ = KillTimer(Some(hwnd), TIMER_LONG_PRESS);
                if app.mouse_down_center {
                    app.long_press_consumed = true;
                    app.single_click_pending = false;
                    let _ = KillTimer(Some(hwnd), TIMER_SINGLE_CLICK);
                    if capture_mode_in_progress(&app.phase) == Some(CaptureMode::Auto) {
                        cancel_capture(app);
                    } else {
                        start_capture(app, CaptureMode::Auto);
                    }
                    update_visible_windows(app);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE if data.kind == WindowKind::Main => {
                if !app.mouse_leave_tracking {
                    let mut tracking = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    app.mouse_leave_tracking = TrackMouseEvent(&mut tracking).is_ok();
                }
                let now = Instant::now();
                sample_history_scroll(app, now);
                let (x, y) = client_dips(hwnd, lparam);
                let body_top = if app.arrow_edge == ArrowEdge::Top {
                    POPUP_ARROW_HEIGHT as f32
                } else {
                    0.0
                };
                let history_top = body_top + TOP_HEIGHT as f32;
                let next = if app.history_scroll_animation.is_some() {
                    None
                } else if (0.0..CONTENT_WIDTH as f32).contains(&x) {
                    history_row_index_at(
                        y,
                        history_top,
                        history_top + HISTORY_HEIGHT as f32,
                        app.history.len(),
                        app.history_transition,
                        app.scroll_offset,
                        now,
                    )
                } else {
                    None
                };
                if next != app.hovered_row {
                    app.hovered_row = next;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE if data.kind == WindowKind::Main => {
                app.mouse_leave_tracking = false;
                if clear_history_hover(&mut app.hovered_row) {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL if data.kind == WindowKind::Main => {
                let mut cursor = POINT {
                    x: low_word(lparam.0) as i16 as i32,
                    y: high_word(lparam.0) as i16 as i32,
                };
                if !ScreenToClient(hwnd, &mut cursor).as_bool() {
                    call_default_proc = true;
                    LRESULT(0)
                } else {
                    let scale = 96.0 / GetDpiForWindow(hwnd).max(96) as f32;
                    let x = cursor.x as f32 * scale;
                    let y = cursor.y as f32 * scale;
                    let body_top = if app.arrow_edge == ArrowEdge::Top {
                        POPUP_ARROW_HEIGHT as f32
                    } else {
                        0.0
                    };
                    let history_top = body_top + TOP_HEIGHT as f32;
                    if !(0.0..CONTENT_WIDTH as f32).contains(&x)
                        || y < history_top
                        || y >= history_top + HISTORY_HEIGHT as f32
                    {
                        call_default_proc = true;
                        LRESULT(0)
                    } else {
                        let delta = (high_word(wparam.0 as isize) as i16) as f32;
                        let now = Instant::now();
                        let current_animation = app.history_scroll_animation;
                        let current_offset = current_animation
                            .map_or(app.scroll_offset, |animation| {
                                history_scroll_offset_at(animation, now).0
                            });
                        let current_target = current_animation
                            .map_or(app.scroll_offset, |animation| animation.to_offset);
                        let next = history_scroll_offset_after_wheel(
                            current_target,
                            delta,
                            system_wheel_scroll_lines(),
                            app.history.len(),
                            HISTORY_HEIGHT as f32,
                        );
                        if (next - current_target).abs() > f32::EPSILON {
                            app.scroll_offset = current_offset;
                            app.history_scroll_animation =
                                if (next - current_offset).abs() > f32::EPSILON {
                                    Some(HistoryScrollAnimation {
                                        started_at: now,
                                        from_offset: current_offset,
                                        to_offset: next,
                                    })
                                } else {
                                    None
                                };
                            app.history_scroll_composition_ready = false;
                            app.hovered_row = None;
                            if app.history_scroll_animation.is_some() {
                                request_history_artwork_at_offset(app, next);
                                let _ = SetTimer(
                                    Some(hwnd),
                                    TIMER_HISTORY_SCROLL_SETTLE,
                                    HISTORY_SCROLL_TRANSITION.as_millis() as u32,
                                    None,
                                );
                            } else {
                                let _ = KillTimer(Some(hwnd), TIMER_HISTORY_SCROLL_SETTLE);
                                app.hovered_row = hovered_history_row_for_cursor(app);
                            }
                            sync_main_timers(app);
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                        LRESULT(0)
                    }
                }
            }
            WM_KEYDOWN => {
                if wparam.0 == VK_ESCAPE.0 as usize && data.kind == WindowKind::Main {
                    hide_main(app);
                }
                LRESULT(0)
            }
            WM_RBUTTONUP if data.kind == WindowKind::Main => {
                let (x, y) = client_dips(hwnd, lparam);
                let body_top = if app.arrow_edge == ArrowEdge::Top {
                    POPUP_ARROW_HEIGHT as f32
                } else {
                    0.0
                };
                if main_hit_test(app, x, y, body_top) == MainHit::CenterButton {
                    if capture_mode_in_progress(&app.phase) == Some(CaptureMode::Auto) {
                        cancel_capture(app);
                    } else {
                        start_capture(app, CaptureMode::Auto);
                    }
                    update_visible_windows(app);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                if data.kind == WindowKind::Main {
                    hide_main(app);
                }
                LRESULT(0)
            }
            WM_DESTROY if data.kind == WindowKind::Main => {
                delete_tray_icon(hwnd);
                cancel_and_join_update_workers(app);
                for icon in app.owned_balloon_icons.drain(..).flatten() {
                    let _ = DestroyIcon(icon);
                }
                if let Some(controller) = app.recognition_controller.take() {
                    let commands = controller.commands.clone();
                    let start_result = start_shutdown_waiter_with_fallback(
                        move || {
                            thread::Builder::new()
                                .name("openshazam-controller-shutdown".to_owned())
                                .spawn(move || {
                                    let _ = controller.join();
                                })
                                .map(|_| ())
                        },
                        move || commands.request_shutdown(),
                    );
                    if let Err(error) = start_result {
                        eprintln!(
                            "OpenShazam could not create the controller shutdown waiter; requested shutdown without joining: {error}"
                        );
                    }
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => {
                call_default_proc = true;
                LRESULT(0)
            }
        }
    };

    if call_default_proc {
        // DefWindowProcW may synchronously dispatch more messages. The mutable
        // WindowData/AppState references from the custom handler are out of scope.
        // SAFETY: The active WndProc HWND and original parameters are forwarded
        // only after the custom message handler's references have ended.
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    } else {
        result
    }
}

#[cfg(test)]
mod dpi_tests {
    use super::{dpi_suggested_bounds, MAX_DPI_SUGGESTED_WINDOW_DIMENSION};
    use windows::Win32::Foundation::{LPARAM, RECT};

    #[test]
    fn dpi_change_requires_a_non_null_rectangle() {
        assert!(dpi_suggested_bounds(LPARAM(0)).is_none());
    }

    #[test]
    fn dpi_change_rejects_an_unreadable_rectangle_pointer() {
        assert!(dpi_suggested_bounds(LPARAM(1)).is_none());
    }

    #[test]
    fn dpi_change_accepts_normal_suggested_bounds() {
        let rect = RECT {
            left: 24,
            top: 32,
            right: 824,
            bottom: 632,
        };
        let bounds = dpi_suggested_bounds(LPARAM(&rect as *const RECT as isize));
        assert_eq!(
            bounds.map(|(bounds, width, height)| (
                bounds.left,
                bounds.top,
                bounds.right,
                bounds.bottom,
                width,
                height,
            )),
            Some((24, 32, 824, 632, 800, 600))
        );
    }

    #[test]
    fn dpi_change_rejects_inverted_overflowing_and_unbounded_bounds() {
        for rect in [
            RECT {
                left: 20,
                top: 20,
                right: 19,
                bottom: 21,
            },
            RECT {
                left: i32::MIN,
                top: 0,
                right: i32::MAX,
                bottom: 100,
            },
            RECT {
                left: 0,
                top: 0,
                right: MAX_DPI_SUGGESTED_WINDOW_DIMENSION + 1,
                bottom: 100,
            },
        ] {
            assert!(dpi_suggested_bounds(LPARAM(&rect as *const RECT as isize)).is_none());
        }
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::start_shutdown_waiter_with_fallback;
    use std::cell::Cell;

    #[test]
    fn shutdown_is_requested_when_the_waiter_thread_cannot_start() {
        let requested = Cell::new(false);
        let result = start_shutdown_waiter_with_fallback(
            || Err(std::io::Error::other("simulated thread creation failure")),
            || requested.set(true),
        );

        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Other);
        assert!(requested.get());
    }

    #[test]
    fn successful_waiter_creation_does_not_request_fallback_shutdown() {
        let requested = Cell::new(false);
        let result = start_shutdown_waiter_with_fallback(|| Ok(()), || requested.set(true));

        assert!(result.is_ok());
        assert!(!requested.get());
    }
}

#[cfg(test)]
mod window_data_ownership_tests {
    use super::{
        mark_window_data_attached, take_window_data_after_create_failure,
        WINDOW_DATA_CREATION_ATTACHED, WINDOW_DATA_CREATION_DESTROYED,
        WINDOW_DATA_CREATION_UNATTACHED,
    };
    use std::sync::atomic::AtomicU8;

    #[test]
    fn attachment_only_transitions_an_unattached_window() {
        let state = AtomicU8::new(WINDOW_DATA_CREATION_UNATTACHED);

        assert!(mark_window_data_attached(&state));
        assert_eq!(
            state.load(std::sync::atomic::Ordering::Acquire),
            WINDOW_DATA_CREATION_ATTACHED
        );
        assert!(!mark_window_data_attached(&state));
    }

    #[test]
    fn attachment_does_not_overwrite_a_reentrant_destroy() {
        let state = AtomicU8::new(WINDOW_DATA_CREATION_DESTROYED);

        assert!(!mark_window_data_attached(&state));
        assert_eq!(
            state.load(std::sync::atomic::Ordering::Acquire),
            WINDOW_DATA_CREATION_DESTROYED
        );
        assert!(!take_window_data_after_create_failure(&state));
    }

    #[test]
    fn create_failure_claims_the_allocation_once() {
        let state = AtomicU8::new(WINDOW_DATA_CREATION_UNATTACHED);

        assert!(take_window_data_after_create_failure(&state));
        assert!(!take_window_data_after_create_failure(&state));
        assert_eq!(
            state.load(std::sync::atomic::Ordering::Acquire),
            WINDOW_DATA_CREATION_DESTROYED
        );
    }
}
