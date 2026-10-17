//! Windows-native UI host: Win32 tray/popup and Direct2D/DirectWrite renderer.
//!
//! Win32 owns the tray, popup and input. Product recognition enters only
//! through the typed controller API and WASAPI loopback.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    ffi::c_void,
    io::Read,
    mem::size_of,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs},
    process::{Command, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use windows::{
    core::{implement, Error, Interface, Ref, BOOL, HSTRING, PCWSTR, PWSTR},
    ApplicationModel::{Activation::ActivationKind, AppInstance},
    Foundation::{DateTime, TimeSpan, TypedEventHandler},
    Globalization::DateTimeFormatting::DateTimeFormatter,
    Graphics::{
        DirectX::{DirectXAlphaMode, DirectXPixelFormat},
        IGeometrySource2D,
    },
    System::DispatcherQueueController,
    Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
        WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
    },
    Win32::{
        Foundation::{
            HANDLE, HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
        },
        Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME},
        Graphics::{
            Direct2D::{
                Common::{
                    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BEZIER_SEGMENT,
                    D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW,
                    D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_GRADIENT_STOP,
                    D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
                },
                D2D1CreateDevice, D2D1CreateFactory, ID2D1Bitmap, ID2D1Bitmap1,
                ID2D1DCRenderTarget, ID2D1Device, ID2D1DeviceContext, ID2D1Factory, ID2D1Geometry,
                ID2D1HwndRenderTarget, ID2D1Layer, ID2D1LinearGradientBrush, ID2D1RenderTarget,
                ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_OPTIONS_NONE,
                D2D1_BITMAP_PROPERTIES, D2D1_BITMAP_PROPERTIES1, D2D1_COMBINE_MODE_EXCLUDE,
                D2D1_COMBINE_MODE_UNION, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP,
                D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2,
                D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                D2D1_LAYER_OPTIONS_NONE, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
                D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
                D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
                D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
            },
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP},
            Direct3D11::{
                D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_SDK_VERSION,
            },
            DirectWrite::{
                DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
                DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_BOLD,
                DWRITE_FONT_WEIGHT_DEMI_BOLD, DWRITE_FONT_WEIGHT_HEAVY, DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
                DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_WRAP,
            },
            Dxgi::{Common::DXGI_FORMAT_B8G8R8A8_UNORM, IDXGIDevice},
            Gdi::{
                BeginPaint, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint,
                GetMonitorInfoW, InvalidateRect, MonitorFromRect, ScreenToClient, SelectObject,
                AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
                DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST,
                RGBQUAD,
            },
        },
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED,
            },
            LibraryLoader::GetModuleHandleW,
            Registry::{
                RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
                HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD,
                REG_OPTION_NON_VOLATILE,
            },
            WinRT::Composition::{
                ICompositionDrawingSurfaceInterop, ICompositorDesktopInterop, ICompositorInterop,
            },
            WinRT::Graphics::Direct2D::{IGeometrySource2DInterop, IGeometrySource2DInterop_Impl},
            WinRT::{
                CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_STA,
                DQTYPE_THREAD_CURRENT,
            },
        },
        UI::{
            HiDpi::{
                GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            },
            Input::KeyboardAndMouse::{GetDoubleClickTime, ReleaseCapture, SetCapture, VK_ESCAPE},
            Shell::{
                ShellExecuteExW, Shell_NotifyIconGetRect, Shell_NotifyIconW, NIF_GUID, NIF_ICON,
                NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_LARGE_ICON, NIIF_USER,
                NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETFOCUS, NIM_SETVERSION, NIN_BALLOONHIDE,
                NIN_BALLOONTIMEOUT, NIN_BALLOONUSERCLICK, NOTIFYICONDATAW, NOTIFYICONIDENTIFIER,
                NOTIFYICON_VERSION_4, SEE_MASK_DEFAULT, SHELLEXECUTEINFOW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
                DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, FindWindowW,
                GetClientRect, GetCursorPos, GetWindowLongPtrW, GetWindowRect, KillTimer,
                LoadCursorW, LoadIconW, PeekMessageW, PostMessageW, PostQuitMessage,
                RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetTimer,
                SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW, TrackPopupMenu,
                TranslateMessage, UpdateLayeredWindow, WaitMessage, CS_DBLCLKS, CW_USEDEFAULT,
                GWLP_USERDATA, ICONINFO, IDC_ARROW, IDI_APPLICATION, MF_SEPARATOR, MF_STRING, MSG,
                PM_REMOVE, SM_CXICON, SM_CYICON, SPI_GETWHEELSCROLLLINES, SWP_NOACTIVATE,
                SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE, SW_SHOWNORMAL,
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, TPM_RETURNCMD, TPM_RIGHTBUTTON, ULW_ALPHA,
                WM_ACTIVATE, WM_APP, WM_CLOSE, WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_KEYDOWN,
                WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
                WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_NULL, WM_PAINT, WM_RBUTTONUP,
                WM_SETFOCUS, WM_SETTINGCHANGE, WM_THEMECHANGED, WM_TIMECHANGE, WM_TIMER, WNDCLASSW,
                WNDPROC, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
            },
        },
    },
    UI::ViewManagement::{UIColorType, UISettings},
    UI::{
        Color,
        Composition::{
            AnimationController, AnimationIterationBehavior, CompositionBitmapInterpolationMode,
            CompositionBorderMode, CompositionColorBrush, CompositionDrawingSurface,
            CompositionEllipseGeometry, CompositionGeometricClip, CompositionGraphicsDevice,
            CompositionPath, CompositionPathGeometry, CompositionSpriteShape, CompositionStretch,
            CompositionSurfaceBrush, CompositionVisualSurface, Compositor, ContainerVisual,
            CubicBezierEasingFunction, Desktop::DesktopWindowTarget, DropShadow,
            ScalarKeyFrameAnimation, ShapeVisual, SpriteVisual, Vector2KeyFrameAnimation,
            Vector3KeyFrameAnimation,
        },
    },
};

#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(attributes: *mut c_void, initial_owner: i32, name: *const u16) -> HANDLE;
    fn GetLastError() -> u32;
    fn CloseHandle(handle: HANDLE) -> i32;
}

use windows_numerics::{Matrix3x2, Vector2, Vector3};

use openshazam::WindowsRecognitionBackend;
use openshazam_controller::{
    ApplicationController, CommandId, CommandResultLookup, CommandStatus, NativeAutoUiEvent,
    NativeManualResultStage, NativeManualUiEvent, RecognitionCommand, RecognitionContext,
    RecognitionEvent, RecognitionEventCategory, RecognitionEventKind, RecognitionSessionSnapshot,
    SessionEndReason, SessionPhase, TrackCandidate, UiConfigSnapshot, UiRecognitionEvent,
};

use crate::ui_model::{
    self, CubicBezier, TaggingMode, UiLevels, BUTTON_SIZE, BUTTON_TOP, BUTTON_X, CONTENT_HEIGHT,
    CONTENT_WIDTH, HISTORY_HEIGHT, POPUP_ARROW_HEIGHT, POPUP_CORNER_RADIUS, TOP_HEIGHT,
};

const CLASS_NAME: &str = "OpenShazamNativeUiWindow";
const SINGLE_INSTANCE_MUTEX: &str = "Local\\OpenShazam.WindowsUi.Singleton";
const ERROR_ALREADY_EXISTS: u32 = 183;
const WM_TRAYICON_MESSAGE: u32 = WM_APP + 0x41;
const WM_SHOW_INITIAL_POPUP: u32 = WM_APP + 0x43;
const WM_OS_THEME_CHANGED: u32 = WM_APP + 0x44;
const WM_ARTWORK_READY: u32 = WM_APP + 0x45;
const WM_ACTIVATE_MAIN_POPUP: u32 = WM_APP + 0x46;
const WM_OPEN_SETTINGS: u32 = WM_APP + 0x47;
const WM_APPEARANCE_CHANGED: u32 = WM_APP + 0x48;
const WM_ARTWORK_LOAD_FINISHED: u32 = WM_APP + 0x49;
const TRAY_ICON_ID: u32 = 1;
const TRAY_ICON_GUID: windows::core::GUID = windows::core::GUID {
    data1: 0x6a3d77b1,
    data2: 0x8e9c,
    data3: 0x4e7d,
    data4: [0x92, 0x01, 0x5d, 0x34, 0xc1, 0x7a, 0x0f, 0x62],
};
const TRAY_TASKBAR_CREATED_NAME: &str = "TaskbarCreated";
const TRAY_MENU_SETTINGS: usize = 1;
const TRAY_MENU_EXIT: usize = 2;
const POPUP_FADE_DURATION: Duration = Duration::from_millis(150);
const POPUP_ACTIVATION_GRACE: Duration = Duration::from_millis(350);
const TIMER_SINGLE_CLICK: usize = 0x4f53;
const TIMER_LONG_PRESS: usize = 0x4f54;
const TIMER_PHASE: usize = 0x4f55;
const TIMER_ANIMATION: usize = 0x4f56;
const TIMER_DEFERRED_DEACTIVATION_HIDE: usize = 0x4f57;
const SINGLE_CLICK_TIMEOUT_MS: u32 = 280;
const LONG_PRESS_TIMEOUT_MS: u32 = 500;
const DEACTIVATION_HIDE_DELAY_MS: u32 = 250;
const PHASE_TICK_MS: u32 = 100;
const ANIMATION_TICK_MS: u32 = 16;
const AUTO_START_SPRING_DURATION: Duration = Duration::from_millis(200);
const AUTO_CANDIDATE_DEDUPE_LIMIT: usize = 40;
const MAIN_WINDOW_WIDTH: f64 = CONTENT_WIDTH;
const MAIN_WINDOW_HEIGHT: f64 = CONTENT_HEIGHT + POPUP_ARROW_HEIGHT;
const ABOUT_WIDTH: f64 = 600.0;
const ABOUT_HEIGHT: f64 = 320.0;
const MIC_INFO_WIDTH: f64 = CONTENT_WIDTH;
const MIC_INFO_HEIGHT: f64 = 190.0;
const OUTER_BASE_OPACITY: [f32; 2] = [0.4, 0.6];

#[derive(Clone, Copy, Debug, PartialEq)]
struct NativeIconLayout {
    settings_visual: [f32; 4],
    settings_hit: [f32; 4],
    cancel_visual: [f32; 4],
    cancel_hit: [f32; 4],
    result_close_visual: [f32; 4],
    result_close_hit: [f32; 4],
}

fn bottom_arrow_visual_top_adjustment(edge: ArrowEdge) -> f32 {
    // UI-232's Windows Bottom projection is user-requested while the Mac
    // runtime geometry remains unmeasured. Keep the 10-DIP adjustment in one
    // place so the visible rectangles and their hit targets move together.
    if edge == ArrowEdge::Bottom {
        POPUP_ARROW_HEIGHT as f32
    } else {
        0.0
    }
}

fn native_icon_layout(body_top: f32, edge: ArrowEdge) -> NativeIconLayout {
    let top_adjustment = bottom_arrow_visual_top_adjustment(edge);
    NativeIconLayout {
        settings_visual: [
            349.0,
            body_top + 25.0 - top_adjustment,
            365.0,
            body_top + 41.0 - top_adjustment,
        ],
        // The popup's stored 30x28 frame is constrained to the 16x16 gear.
        settings_hit: [
            349.0,
            body_top + 25.0 - top_adjustment,
            365.0,
            body_top + 41.0 - top_adjustment,
        ],
        cancel_visual: [
            351.0,
            body_top + 27.0 - top_adjustment,
            363.0,
            body_top + 39.0 - top_adjustment,
        ],
        cancel_hit: [
            351.0,
            body_top + 27.0 - top_adjustment,
            363.0,
            body_top + 39.0 - top_adjustment,
        ],
        result_close_visual: [
            353.0,
            body_top + 25.0 - top_adjustment,
            365.0,
            body_top + 37.0 - top_adjustment,
        ],
        result_close_hit: [
            353.0,
            body_top + 25.0 - top_adjustment,
            365.0,
            body_top + 37.0 - top_adjustment,
        ],
    }
}

fn point_in_rect(point_x: f32, point_y: f32, bounds: [f32; 4]) -> bool {
    (bounds[0]..=bounds[2]).contains(&point_x) && (bounds[1]..=bounds[3]).contains(&point_y)
}

fn result_card_link_hit(
    x: f32,
    y: f32,
    card_top: f32,
    close: [f32; 4],
    store_action: Option<[f32; 4]>,
) -> bool {
    point_in_rect(
        x,
        y,
        [
            0.0,
            card_top,
            CONTENT_WIDTH as f32,
            card_top + TOP_HEIGHT as f32,
        ],
    ) && !point_in_rect(x, y, close)
        && !store_action.is_some_and(|action| point_in_rect(x, y, action))
}

fn visible_history_row_hit(y: f32, row_top: f32, history_top: f32, history_bottom: f32) -> bool {
    let visible_top = row_top.max(history_top);
    let visible_bottom = (row_top + HISTORY_ROW_HEIGHT).min(history_bottom);
    visible_top < visible_bottom && visible_top <= y && y < visible_bottom
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowKind {
    Main,
    About,
    MicrophoneInfo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Theme {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u32)]
enum ThemePreference {
    #[default]
    System = 0,
    Light = 1,
    Dark = 2,
}

impl ThemePreference {
    fn from_raw(value: u32) -> Self {
        match value {
            1 => Self::Light,
            2 => Self::Dark,
            _ => Self::System,
        }
    }

    fn resolve(self, system_theme: Theme) -> Theme {
        match self {
            Self::System => system_theme,
            Self::Light => Theme::Light,
            Self::Dark => Theme::Dark,
        }
    }

    fn id(self) -> u32 {
        self as u32
    }
}

fn load_theme_preference() -> ThemePreference {
    let key_path = wide("Software\\OpenShazam");
    let value_name = wide("AppearanceMode");
    let mut key = HKEY::default();
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            None,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    if opened.0 != 0 {
        return ThemePreference::System;
    }

    let mut value = 0u32;
    let mut value_type = Default::default();
    let mut byte_count = size_of::<u32>() as u32;
    let queried = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut value_type),
            Some((&mut value as *mut u32).cast()),
            Some(&mut byte_count),
        )
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if queried.0 == 0 && value_type == REG_DWORD && byte_count == size_of::<u32>() as u32 {
        ThemePreference::from_raw(value)
    } else {
        ThemePreference::System
    }
}

fn save_theme_preference(preference: ThemePreference) {
    let key_path = wide("Software\\OpenShazam");
    let value_name = wide("AppearanceMode");
    let mut key = HKEY::default();
    let created = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            Some(0),
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    };
    if created.0 != 0 {
        return;
    }

    let value = preference.id();
    let bytes = unsafe {
        std::slice::from_raw_parts((&value as *const u32).cast::<u8>(), size_of::<u32>())
    };
    unsafe {
        let _ = RegSetValueExW(
            key,
            PCWSTR(value_name.as_ptr()),
            Some(0),
            REG_DWORD,
            Some(bytes),
        );
        let _ = RegCloseKey(key);
    }
}

fn load_auto_shazam_preference() -> bool {
    let key_path = wide("Software\\OpenShazam");
    let value_name = wide("AutoShazamEnabled");
    let mut key = HKEY::default();
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            None,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    if opened.0 != 0 {
        return false;
    }

    let mut value = 0u32;
    let mut value_type = Default::default();
    let mut byte_count = size_of::<u32>() as u32;
    let queried = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut value_type),
            Some((&mut value as *mut u32).cast()),
            Some(&mut byte_count),
        )
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    queried.0 == 0 && value_type == REG_DWORD && byte_count == size_of::<u32>() as u32 && value != 0
}

fn save_auto_shazam_preference(enabled: bool) {
    let key_path = wide("Software\\OpenShazam");
    let value_name = wide("AutoShazamEnabled");
    let mut key = HKEY::default();
    let created = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            Some(0),
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    };
    if created.0 != 0 {
        return;
    }

    let value = u32::from(enabled);
    let bytes = unsafe {
        std::slice::from_raw_parts((&value as *const u32).cast::<u8>(), size_of::<u32>())
    };
    unsafe {
        let _ = RegSetValueExW(
            key,
            PCWSTR(value_name.as_ptr()),
            Some(0),
            REG_DWORD,
            Some(bytes),
        );
        let _ = RegCloseKey(key);
    }
}

fn foreground_for_theme(theme: Theme) -> Color {
    match theme {
        Theme::Light => Color {
            A: 255,
            R: 0,
            G: 0,
            B: 0,
        },
        Theme::Dark => Color {
            A: 255,
            R: 255,
            G: 255,
            B: 255,
        },
    }
}

fn theme_from_foreground_rgb(red: u8, green: u8, blue: u8) -> Theme {
    if 5u32 * u32::from(green) + 2u32 * u32::from(red) + u32::from(blue) > 8 * 128 {
        Theme::Dark
    } else {
        Theme::Light
    }
}

fn theme_from_ui_settings(settings: &UISettings) -> windows::core::Result<(Theme, Color)> {
    let foreground = settings.GetColorValue(UIColorType::Foreground)?;
    Ok((
        theme_from_foreground_rgb(foreground.R, foreground.G, foreground.B),
        foreground,
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrowEdge {
    Top,
    Bottom,
}

#[derive(Clone, Debug)]
struct Track {
    display_id: u64,
    title: String,
    artist: String,
    recognized_at: Instant,
    recognized_at_utc: SystemTime,
    recognized_date_label: String,
    #[allow(dead_code)]
    timestamp_source: HistoryTimestampSource,
    artwork_url: Option<String>,
    hq_artwork_url: Option<String>,
    #[allow(dead_code)] // Kept distinct from the album-art rendition by the UI data contract.
    background_artwork_url: Option<String>,
    #[allow(dead_code)]
    // Retained alongside the Config-resolved page URL for the presentation model.
    track_key: Option<String>,
    config_generation: Option<u64>,
    page_url: Option<ValidatedWebTrackUrl>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryTimestampSource {
    ControllerEvent,
    HostReceivedFallback,
}

fn resolve_history_timestamp(
    event_time_utc: Option<SystemTime>,
    received_at_utc: SystemTime,
) -> (SystemTime, HistoryTimestampSource) {
    match event_time_utc {
        Some(when) => (when, HistoryTimestampSource::ControllerEvent),
        None => (
            received_at_utc,
            HistoryTimestampSource::HostReceivedFallback,
        ),
    }
}

const WINDOWS_EPOCH_OFFSET_100NS: i64 = 116_444_736_000_000_000;
const WINDOWS_DATETIME_TEMPLATE: &str = "day month.abbreviated year hour minute";

fn winrt_datetime_from_system_time(value: SystemTime) -> DateTime {
    let elapsed = value.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let unix_100ns =
        (elapsed.as_secs() as i128) * 10_000_000 + i128::from(elapsed.subsec_nanos() / 100);
    DateTime {
        UniversalTime: (unix_100ns + i128::from(WINDOWS_EPOCH_OFFSET_100NS))
            .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64,
    }
}

fn format_history_date_label(value: SystemTime) -> String {
    let template = HSTRING::from(WINDOWS_DATETIME_TEMPLATE);
    DateTimeFormatter::CreateDateTimeFormatter(&template)
        .and_then(|formatter| formatter.Format(winrt_datetime_from_system_time(value)))
        .map(|formatted| formatted.to_string())
        .unwrap_or_default()
}

fn refresh_history_date_labels(history: &mut [Track]) {
    for track in history {
        track.recognized_date_label = format_history_date_label(track.recognized_at_utc);
    }
}

fn result_panel_draw_top(
    panel_top: Option<f32>,
    body_top: f32,
    composition_cache_ready: bool,
    composition_cache_hit: bool,
) -> Option<f32> {
    panel_top.and_then(|top| {
        if composition_cache_ready {
            (!composition_cache_hit).then_some(body_top)
        } else {
            Some(top)
        }
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ValidatedWebTrackUrl(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PageLinkFailure {
    MissingConfigGeneration,
    MissingConfigSnapshot,
    MissingTemplate,
    InvalidTemplate,
    MissingTrackKey,
    InvalidTrackKey,
    InvalidResolvedUrl,
}

impl PageLinkFailure {
    fn as_str(self) -> &'static str {
        match self {
            Self::MissingConfigGeneration => "missing_config_generation",
            Self::MissingConfigSnapshot => "missing_config_snapshot",
            Self::MissingTemplate => "missing_webtrackurl",
            Self::InvalidTemplate => "invalid_webtrackurl_template",
            Self::MissingTrackKey => "missing_track_key",
            Self::InvalidTrackKey => "invalid_track_key",
            Self::InvalidResolvedUrl => "invalid_resolved_url",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArtworkSource {
    Standard,
    HighQuality,
}

fn should_replace_cached_artwork(
    cached: Option<(ArtworkSource, u128)>,
    incoming_source: ArtworkSource,
    incoming_fingerprint: u128,
) -> bool {
    match cached {
        None => true,
        Some((ArtworkSource::HighQuality, _)) if incoming_source == ArtworkSource::Standard => {
            false
        }
        Some((cached_source, cached_fingerprint)) => {
            cached_source != incoming_source || cached_fingerprint != incoming_fingerprint
        }
    }
}

#[derive(Clone, Debug)]
struct ArtworkPixels {
    width: u32,
    height: u32,
    fingerprint: u128,
    bgra: Vec<u8>,
}

struct CachedArtworkBitmap {
    source: ArtworkSource,
    width: u32,
    height: u32,
    byte_len: usize,
    fingerprint: u128,
    bitmap: ID2D1Bitmap,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct TrackIdentity {
    display_id: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ArtworkRequestIdentity {
    track: TrackIdentity,
    generation: u64,
    request_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ArtworkRequest {
    identity: ArtworkRequestIdentity,
    standard_url: Option<String>,
    high_quality_url: Option<String>,
}

fn artwork_request_is_current(
    current: Option<&ArtworkRequest>,
    incoming: ArtworkRequestIdentity,
) -> bool {
    current.is_some_and(|request| request.identity == incoming)
}

fn artwork_request_urls_match_track(
    request_standard: Option<&str>,
    request_high_quality: Option<&str>,
    track_standard: Option<&str>,
    track_high_quality: Option<&str>,
) -> bool {
    request_standard == track_standard && request_high_quality == track_high_quality
}

fn artwork_request_matches_track(
    current: Option<&ArtworkRequest>,
    incoming: ArtworkRequestIdentity,
    track: Option<&Track>,
) -> bool {
    if !artwork_request_is_current(current, incoming) {
        return false;
    }
    let Some(track) = track else {
        return false;
    };
    track.display_id == incoming.track.display_id
        && current.is_some_and(|request| {
            artwork_request_urls_match_track(
                request.standard_url.as_deref(),
                request.high_quality_url.as_deref(),
                track.artwork_url.as_deref(),
                track.hq_artwork_url.as_deref(),
            )
        })
}

fn finish_active_artwork_request(
    active_requests: &mut HashMap<TrackIdentity, ArtworkRequest>,
    identity: ArtworkRequestIdentity,
) -> bool {
    if artwork_request_is_current(active_requests.get(&identity.track), identity) {
        active_requests.remove(&identity.track);
        true
    } else {
        false
    }
}

#[derive(Debug)]
struct ArtworkReady {
    identity: ArtworkRequestIdentity,
    source: ArtworkSource,
    pixels: ArtworkPixels,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StatusCycle {
    Ready,
    Auto,
}

#[derive(Clone, Copy, Debug)]
struct StatusTextAlternator {
    cycle: StatusCycle,
    first: bool,
    next_at: Instant,
    transition_started_at: Option<Instant>,
}

impl StatusTextAlternator {
    fn ready(now: Instant) -> Self {
        Self::new(StatusCycle::Ready, now)
    }

    fn auto(now: Instant) -> Self {
        Self::new(StatusCycle::Auto, now)
    }

    fn after_success(now: Instant) -> Self {
        Self::ready(now)
    }

    fn new(cycle: StatusCycle, now: Instant) -> Self {
        Self {
            cycle,
            first: true,
            next_at: now + Duration::from_secs(3),
            transition_started_at: None,
        }
    }

    fn advance(&mut self, now: Instant) -> bool {
        let mut changed = false;
        while now >= self.next_at {
            self.first = !self.first;
            self.transition_started_at = Some(self.next_at);
            self.next_at += Duration::from_secs(match self.cycle {
                StatusCycle::Ready if self.first => 7,
                StatusCycle::Ready => 2,
                StatusCycle::Auto => 3,
            });
            changed = true;
        }
        if self.transition_started_at.is_some_and(|started| {
            now.saturating_duration_since(started) >= Duration::from_millis(400)
        }) {
            self.transition_started_at = None;
            changed = true;
        }
        changed
    }

    fn animating(self, now: Instant) -> bool {
        self.transition_started_at.is_some_and(|started| {
            now.saturating_duration_since(started) < Duration::from_millis(400)
        })
    }
}

fn status_text_progress(started_at: Instant, now: Instant) -> f32 {
    ui_model::ease_in_out(status_text_linear_progress(started_at, now) as f64) as f32
}

fn status_text_linear_progress(started_at: Instant, now: Instant) -> f32 {
    (now.saturating_duration_since(started_at).as_secs_f64()
        / STATUS_TEXT_TRANSITION_DURATION.as_secs_f64())
    .clamp(0.0, 1.0) as f32
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureMode {
    Manual,
    Auto,
}

#[derive(Clone, Copy, Debug)]
enum ResultCardStage {
    Entering {
        started_at: Instant,
    },
    Holding {
        started_at: Instant,
    },
    Exiting {
        started_at: Instant,
        from_offset: f32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ResultOverlayIdentity {
    Matched {
        display_id: u64,
        title: String,
        artist: String,
        artwork_url: Option<String>,
        hq_artwork_url: Option<String>,
        artwork: Option<(ArtworkSource, usize, u128)>,
    },
    NoMatch {
        entered_at: Instant,
        offline: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResultOverlayContentKey {
    identity: ResultOverlayIdentity,
    theme_generation: u64,
    dpi: u32,
    edge: ArrowEdge,
    language: UiLanguage,
}

fn result_overlay_identity_matches_phase(
    identity: &ResultOverlayIdentity,
    phase: &MainPhase,
    current_artwork: Option<(ArtworkSource, usize, u128)>,
) -> bool {
    match (identity, phase) {
        (
            ResultOverlayIdentity::Matched {
                display_id,
                title,
                artist,
                artwork_url,
                hq_artwork_url,
                artwork,
            },
            MainPhase::Matched { track, .. },
        ) => {
            track.display_id == *display_id
                && track.title.as_str() == title
                && track.artist.as_str() == artist
                && track.artwork_url.as_deref() == artwork_url.as_deref()
                && track.hq_artwork_url.as_deref() == hq_artwork_url.as_deref()
                && current_artwork == *artwork
        }
        (
            ResultOverlayIdentity::NoMatch {
                entered_at,
                offline,
            },
            MainPhase::NoMatch {
                entered_at: current_entered_at,
                ..
            },
        ) => !offline && entered_at == current_entered_at,
        (
            ResultOverlayIdentity::NoMatch {
                entered_at,
                offline,
            },
            MainPhase::Offline {
                entered_at: current_entered_at,
                ..
            },
        ) => *offline && entered_at == current_entered_at,
        _ => false,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ResultOverlayMotionKey {
    MatchEnter {
        display_id: u64,
        started_at: Instant,
    },
    MatchExit {
        display_id: u64,
        started_at: Instant,
        from_offset_bits: u32,
    },
    NoMatchEnter {
        entered_at: Instant,
        offline: bool,
    },
    NoMatchExit {
        entered_at: Instant,
        returned_at: Instant,
        offline: bool,
        from_offset_bits: u32,
    },
}

#[derive(Clone, Debug)]
struct ResultOverlayMotionAnimation {
    key: ResultOverlayMotionKey,
    from_offset: f32,
    to_offset: f32,
    remaining: Duration,
    easing: CubicBezier,
}

// Native history delivers these independently through Core Data change types.
// This UI currently receives inserts only; updates/deletes stay available for
// the UI adapter without widening service or persistence APIs.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
enum HistoryMutationEvent {
    Insert { index: usize, previous_count: usize },
    Update { index: usize },
    Delete { index: usize },
}

#[derive(Clone, Copy, Debug)]
struct HistoryMutationTransition {
    started_at: Instant,
    event: HistoryMutationEvent,
}

#[derive(Clone, Copy, Debug)]
enum ResultCardAdvance {
    Unchanged,
    Stage(ResultCardStage),
    Ready,
}

const RESULT_CARD_TRANSITION: Duration = Duration::from_millis(300);
const RESULT_CARD_HOLD: Duration = Duration::from_secs(10);
const SUCCESS_SETTLE_TOTAL: Duration = Duration::from_secs(1);
// [I/U] Native NSTableView slide duration was not recovered. Keep this
// implementation value replaceable rather than presenting it as native data.
const HISTORY_INSERT_TRANSITION: Duration = Duration::from_millis(250);
// Nib frame y values (-39,55,149,243,336) use a bottom-left origin. Convert
// each 94-DIP row into the history's top-left coordinate space (0..430).
const EMPTY_HISTORY_ROW_TOPS: [f32; 5] = [0.0, 93.0, 187.0, 281.0, 375.0];

// Keeping Track inline avoids a heap allocation on the recognition-to-popup
// fast path; this state machine is not copied on every animation frame.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum MainPhase {
    Ready,
    Starting {
        mode: CaptureMode,
        entered_at: Instant,
    },
    Listening {
        mode: CaptureMode,
        started_at: Instant,
    },
    Cancelling {
        mode: CaptureMode,
        entered_at: Instant,
    },
    Matched {
        track: Track,
        stage: ResultCardStage,
    },
    NoMatch {
        entered_at: Instant,
        returned_to_ready_at: Option<Instant>,
    },
    Error {
        message: String,
        entered_at: Instant,
    },
    Timeout {
        mode: CaptureMode,
        entered_at: Instant,
    },
    Offline {
        entered_at: Instant,
        returned_to_ready_at: Option<Instant>,
    },
}

#[derive(Clone, Copy, Debug)]
struct RecognitionSessionUi {
    ticket: openshazam_controller::CommandTicket,
    mode: CaptureMode,
    context: Option<RecognitionContext>,
    cancel_command_id: Option<CommandId>,
    last_sequence: u64,
    result_presented: bool,
}

struct AppState {
    main_hwnd: HWND,
    taskbar_created_message: u32,
    settings_host: Option<crate::windows_settings::SettingsHost>,
    settings_opening: bool,
    about_hwnd: HWND,
    microphone_hwnd: HWND,
    d2d_factory: ID2D1Factory,
    dwrite_factory: IDWriteFactory,
    compositor: Compositor,
    _dispatcher_queue: DispatcherQueueController,
    theme_settings: UISettings,
    theme_changed_token: Option<i64>,
    system_foreground: Color,
    phase: MainPhase,
    history: Vec<Track>,
    history_transition: Option<HistoryMutationTransition>,
    scroll_offset: f32,
    hovered_row: Option<usize>,
    // UI-only adapter points. No service meter output is assumed or inferred.
    ui_levels: UiLevels,
    // Core Animation default spring coefficients and the explicit 0.2s track
    // are supplied when Auto starts. Keeping this optional lets UI tests inject
    // an alternate measured profile without changing the recognition engine.
    auto_spring: Option<ui_model::SpringProfile>,
    theme: Theme,
    theme_preference: ThemePreference,
    theme_generation: u64,
    visible: bool,
    popup_opacity: u8,
    popup_transition: Option<PopupOpacityTransition>,
    main_activated: bool,
    popup_activation_grace_until: Option<Instant>,
    deactivation_hide_pending: bool,
    phase_timer_active: bool,
    animation_timer_active: bool,
    result_overlay_composition_ready: bool,
    status_text_composition_ready: bool,
    startup_animation_pending: bool,
    startup_logo_consumed: bool,
    about_visible: bool,
    microphone_visible: bool,
    arrow_edge: ArrowEdge,
    arrow_center: f32,
    mouse_down_center: bool,
    long_press_consumed: bool,
    single_click_pending: bool,
    recognition_controller: Option<ApplicationController>,
    recognition_session: Option<RecognitionSessionUi>,
    auto_candidate_dedupe: VecDeque<String>,
    controller_sequence: u64,
    dropped_notifications: u64,
    status_alternator: Option<StatusTextAlternator>,
    success_wave_frame: Option<ui_model::WaveFrame>,
    next_display_id: u64,
    next_artwork_request_id: u64,
    artwork_generations: HashMap<TrackIdentity, u64>,
    active_artwork_requests: HashMap<TrackIdentity, ArtworkRequest>,
    artwork_resources: HashMap<u64, (ArtworkSource, ArtworkPixels)>,
    pending_match_notifications: HashMap<u64, Track>,
    owned_balloon_icons: VecDeque<Option<windows::Win32::UI::WindowsAndMessaging::HICON>>,
    ui_configs: HashMap<u64, UiConfigSnapshot>,
    message: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct PopupOpacityTransition {
    started_at: Instant,
    from: u8,
    to: u8,
    hide_when_complete: bool,
}

struct WindowData {
    app: *mut AppState,
    hwnd: HWND,
    kind: WindowKind,
    renderer: Option<Renderer>,
}

struct Palette {
    white: ID2D1SolidColorBrush,
    white_ink: ID2D1SolidColorBrush,
    black: ID2D1SolidColorBrush,
    title: ID2D1SolidColorBrush,
    subtitle: ID2D1SolidColorBrush,
    result_label: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
    button: ID2D1SolidColorBrush,
    separator: ID2D1SolidColorBrush,
    skeleton: ID2D1SolidColorBrush,
    result_top: ID2D1LinearGradientBrush,
    result_bottom: ID2D1LinearGradientBrush,
    listening: ID2D1LinearGradientBrush,
    listening_bottom: ID2D1LinearGradientBrush,
    empty_overlay: ID2D1LinearGradientBrush,
}

struct TextStyles {
    title: IDWriteTextFormat,
    subtitle: IDWriteTextFormat,
    date: IDWriteTextFormat,
    result_title: IDWriteTextFormat,
    result_artist: IDWriteTextFormat,
    result_action: IDWriteTextFormat,
    button: IDWriteTextFormat,
    centered_body: IDWriteTextFormat,
    centered_title: IDWriteTextFormat,
    centered_heavy: IDWriteTextFormat,
    trailing_small: IDWriteTextFormat,
    logo: IDWriteTextFormat,
}

#[derive(Clone, Copy, Debug)]
struct ResultDetailsRects {
    title: D2D_RECT_F,
    artist: D2D_RECT_F,
    action: D2D_RECT_F,
}

#[derive(Clone, Copy, Debug)]
struct ResultDetailsMetrics {
    title_height: f32,
    artist_height: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct MeasuredTextLayout {
    height: f32,
    #[allow(dead_code)] // Retained separately from TextFrame for native baseline comparison.
    first_baseline: f32,
    #[allow(dead_code)] // Retained separately from TextFrame for native baseline comparison.
    last_baseline: f32,
    #[allow(dead_code)] // DirectWrite overhang is a layout metric, not a pixel-exact ink mask.
    overhang_left: f32,
    #[allow(dead_code)]
    overhang_top: f32,
    #[allow(dead_code)]
    overhang_right: f32,
    #[allow(dead_code)]
    overhang_bottom: f32,
}

#[derive(Clone, Copy, Debug)]
struct HistoryTextMetrics {
    title: MeasuredTextLayout,
    artist: MeasuredTextLayout,
    date: MeasuredTextLayout,
}

#[derive(Clone, Debug)]
struct CachedHistoryTextMetrics {
    title: String,
    artist: String,
    date: String,
    metrics: HistoryTextMetrics,
}

#[derive(Clone, Copy, Debug)]
struct HistoryTextRects {
    title: D2D_RECT_F,
    artist: D2D_RECT_F,
    date: D2D_RECT_F,
}

struct CachedResultDetailsMetrics {
    title: String,
    artist: String,
    metrics: ResultDetailsMetrics,
}

struct Renderer {
    factory: ID2D1Factory,
    write_factory: IDWriteFactory,
    target: ID2D1RenderTarget,
    settings_icon: ID2D1Bitmap,
    white_close_icon: ID2D1Bitmap,
    result_close_icon: ID2D1Bitmap,
    fallback_match_artwork: Option<(ID2D1Bitmap, ID2D1Bitmap)>,
    hwnd_target: Option<ID2D1HwndRenderTarget>,
    dc_target: Option<ID2D1DCRenderTarget>,
    layered_surface: Option<LayeredSurface>,
    palette: Palette,
    text: TextStyles,
    dpi: u32,
    theme_generation: u64,
    composition: Option<CompositionScene>,
    status_text_composition_enabled: bool,
    popup_layer: Option<ID2D1Layer>,
    artwork_layer: Option<ID2D1Layer>,
    artwork_geometries: [(u32, ID2D1Geometry); 2],
    artwork_bitmaps: RefCell<HashMap<u64, CachedArtworkBitmap>>,
    popup_outline: RefCell<Option<(ArrowEdge, u32, ID2D1Geometry)>>,
    popup_header: RefCell<Option<(ArrowEdge, u32, ID2D1Geometry)>>,
    result_details_cache: RefCell<Option<CachedResultDetailsMetrics>>,
    history_text_metrics_cache: RefCell<HashMap<u64, CachedHistoryTextMetrics>>,
    result_overlay_content_key: Option<ResultOverlayContentKey>,
    result_overlay_motion_key: Option<ResultOverlayMotionKey>,
    result_overlay_failure_reported: bool,
}

const SETTINGS_ICON_PIXELS: &[u8; 4096] = include_bytes!("../assets/settings-2x.bgra");
const WHITE_CLOSE_ICON_PIXELS: &[u8; 2304] = include_bytes!("../assets/WhiteClose-2x.bgra");
// ShazamUI's native closeButton @2x GA8 rendition, extracted from its
// bottom-origin CoreUI atlas and stored inline so normal builds need no
// generated files outside dist.
const CLOSE_BUTTON_GA_PIXELS: &[u8; 1152] = &[
    0, 0, 16, 39, 71, 179, 93, 237, 88, 223, 57, 143, 5, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 5, 12, 57, 144, 88, 224, 94, 238, 71, 180, 16, 39, 0, 0, 16, 39, 97, 245,
    100, 255, 100, 255, 100, 255, 100, 255, 84, 214, 10, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 10, 24, 84, 214, 100, 255, 100, 255, 100, 255, 100, 255, 97, 245, 16, 39, 71, 180,
    100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 85, 215, 10, 24, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 10, 24, 85, 215, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255,
    71, 179, 93, 237, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 85,
    215, 10, 24, 0, 0, 0, 0, 0, 0, 0, 0, 10, 24, 85, 215, 100, 255, 100, 255, 100, 255, 100, 255,
    100, 255, 100, 255, 100, 255, 93, 237, 88, 223, 100, 255, 100, 255, 100, 255, 100, 255, 100,
    255, 100, 255, 100, 255, 100, 255, 85, 215, 10, 24, 0, 0, 0, 0, 10, 24, 85, 215, 100, 255, 100,
    255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 88, 223, 57, 143, 100, 255,
    100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 85, 215, 10,
    24, 10, 24, 85, 215, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100,
    255, 100, 255, 57, 143, 5, 11, 84, 214, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100,
    255, 100, 255, 100, 255, 100, 255, 85, 215, 85, 215, 100, 255, 100, 255, 100, 255, 100, 255,
    100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 84, 214, 5, 12, 0, 0, 10, 24, 85, 215, 100,
    255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255,
    100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 85, 215, 10,
    24, 0, 0, 0, 0, 0, 0, 10, 24, 85, 215, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100,
    255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255,
    100, 255, 85, 215, 10, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 24, 85, 215, 100, 255, 100, 255,
    100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100,
    255, 100, 255, 100, 255, 85, 215, 10, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 24, 85,
    215, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255,
    100, 255, 100, 255, 100, 255, 85, 215, 10, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 10, 24, 85, 215, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255,
    100, 255, 100, 255, 100, 255, 85, 215, 10, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 10, 24, 85, 215, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100,
    255, 100, 255, 100, 255, 100, 255, 85, 215, 10, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 10, 24, 85, 215, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100,
    255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 85, 215, 10, 24, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 10, 24, 85, 215, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255,
    100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 85, 215, 10,
    24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 24, 85, 215, 100, 255, 100, 255, 100, 255, 100, 255, 100,
    255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255,
    100, 255, 100, 255, 85, 215, 10, 24, 0, 0, 0, 0, 0, 0, 10, 24, 85, 215, 100, 255, 100, 255,
    100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100,
    255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 85, 215, 10, 24, 0, 0, 5, 12,
    84, 214, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100,
    255, 85, 215, 85, 215, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255,
    100, 255, 100, 255, 84, 214, 5, 12, 57, 143, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255,
    100, 255, 100, 255, 100, 255, 100, 255, 85, 215, 10, 24, 10, 24, 85, 215, 100, 255, 100, 255,
    100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 57, 144, 88, 223, 100,
    255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 85, 215, 10, 24, 0,
    0, 0, 0, 10, 24, 85, 215, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255,
    100, 255, 88, 224, 93, 237, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100,
    255, 85, 215, 10, 24, 0, 0, 0, 0, 0, 0, 0, 0, 10, 24, 85, 215, 100, 255, 100, 255, 100, 255,
    100, 255, 100, 255, 100, 255, 100, 255, 94, 238, 71, 179, 100, 255, 100, 255, 100, 255, 100,
    255, 100, 255, 100, 255, 85, 215, 10, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 24, 85, 215,
    100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 100, 255, 71, 180, 16, 39, 97, 245, 100, 255,
    100, 255, 100, 255, 100, 255, 84, 214, 10, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    10, 24, 84, 214, 100, 255, 100, 255, 100, 255, 100, 255, 97, 245, 16, 39, 0, 0, 16, 39, 71,
    180, 94, 238, 88, 224, 57, 144, 5, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 5, 12, 57, 144, 88, 224, 94, 238, 71, 180, 16, 39, 0, 0,
];
const fn close_button_ga_to_bgra(pixels: &[u8; 1152]) -> [u8; 2304] {
    let mut output = [0u8; 2304];
    let mut index = 0;
    while index < 24 * 24 {
        let gray = pixels[index * 2] as u16;
        let alpha = pixels[index * 2 + 1];
        let premultiplied = ((gray * alpha as u16 + 127) / 255) as u8;
        let offset = index * 4;
        output[offset] = premultiplied;
        output[offset + 1] = premultiplied;
        output[offset + 2] = premultiplied;
        output[offset + 3] = alpha;
        index += 1;
    }
    output
}
const RESULT_CLOSE_ICON_PIXELS: [u8; 2304] = close_button_ga_to_bgra(CLOSE_BUTTON_GA_PIXELS);
const FALLBACK_MATCH_LIGHT_PIXELS: &[u8; 274_576] =
    include_bytes!("../assets/fallbackArtworkMatch-light-2x.bgra");
const FALLBACK_MATCH_DARK_PIXELS: &[u8; 274_576] =
    include_bytes!("../assets/fallbackArtworkMatch-dark-2x.bgra");
const FALLBACK_MATCH_LOGICAL_SIDE: f32 = 131.0;

unsafe fn create_ui_icon_bitmap(
    target: &ID2D1RenderTarget,
    pixels: &[u8],
    width: u32,
    height: u32,
) -> windows::core::Result<ID2D1Bitmap> {
    let properties = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        // Source pixels are the 2x renditions; destination coordinates stay in DIPs.
        dpiX: 192.0,
        dpiY: 192.0,
    };
    target.CreateBitmap(
        D2D_SIZE_U { width, height },
        Some(pixels.as_ptr().cast()),
        width * 4,
        &properties,
    )
}

/// Premultiplied BGRA backing for the popup's per-pixel alpha presentation.
/// The popup includes a non-rectangular tray arrow, so a GDI window region
/// would reintroduce aliased edges even when Direct2D anti-aliases the paths.
struct LayeredSurface {
    dc: HDC,
    bitmap: HBITMAP,
    previous_bitmap: HGDIOBJ,
    size: D2D_SIZE_U,
    _pixels: *mut c_void,
}

impl LayeredSurface {
    unsafe fn new(width: u32, height: u32) -> windows::core::Result<Self> {
        let dc = CreateCompatibleDC(None);
        if dc.0.is_null() {
            return Err(Error::from_thread());
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width.max(1) as i32,
                biHeight: -(height.max(1) as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut pixels, None, 0) {
            Ok(bitmap) => bitmap,
            Err(error) => {
                let _ = DeleteDC(dc);
                return Err(error);
            }
        };
        let previous_bitmap = SelectObject(dc, bitmap.into());
        if pixels.is_null() || previous_bitmap.0.is_null() || previous_bitmap.0 as isize == -1 {
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(dc);
            return Err(Error::from_thread());
        }
        Ok(Self {
            dc,
            bitmap,
            previous_bitmap,
            size: D2D_SIZE_U {
                width: width.max(1),
                height: height.max(1),
            },
            _pixels: pixels,
        })
    }

    unsafe fn present(&self, hwnd: HWND, opacity: u8) -> windows::core::Result<()> {
        let mut window = RECT::default();
        GetWindowRect(hwnd, &mut window)?;
        let destination = POINT {
            x: window.left,
            y: window.top,
        };
        let source = POINT::default();
        let size = windows::Win32::Foundation::SIZE {
            cx: window.right - window.left,
            cy: window.bottom - window.top,
        };
        if size.cx.max(0) as u32 != self.size.width || size.cy.max(0) as u32 != self.size.height {
            return Err(Error::new(
                windows::core::HRESULT(0x80070057_u32 as i32),
                "Layered popup surface size does not match the window client size",
            ));
        }
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: opacity,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        UpdateLayeredWindow(
            hwnd,
            None,
            Some(&destination),
            Some(&size),
            Some(self.dc),
            Some(&source),
            Default::default(),
            Some(&blend),
            ULW_ALPHA,
        )
    }
}

impl Drop for LayeredSurface {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.dc, self.previous_bitmap);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

// Copy only the current result panel from the premultiplied layered-window
// surface. The cached bytes become the Composition surface; the original
// Direct2D panel remains the pixel source so text, artwork, and popup masking
// stay identical while the result is above the Composition button/ripples.
fn copy_panel_pixels_into(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    panel_top: i32,
    output_width: u32,
    output_height: u32,
    output: &mut Vec<u8>,
) {
    let output_len = output_width as usize * output_height as usize * 4;
    output.resize(output_len, 0);
    output.fill(0);
    let copy_width = source_width.min(output_width) as usize * 4;
    let source_stride = source_width as usize * 4;
    let output_stride = output_width as usize * 4;

    for output_y in 0..output_height as usize {
        let source_y = panel_top + output_y as i32;
        if !(0..source_height as i32).contains(&source_y) {
            continue;
        }
        let source_start = source_y as usize * source_stride;
        let output_start = output_y * output_stride;
        let source_end = source_start + copy_width;
        let output_end = output_start + copy_width;
        if source_end <= source.len() && output_end <= output.len() {
            output[output_start..output_end].copy_from_slice(&source[source_start..source_end]);
        }
    }
}

struct CompositionResultOverlay {
    _graphics_device: CompositionGraphicsDevice,
    _rendering_device: ID2D1Device,
    _surface: CompositionDrawingSurface,
    surface_interop: ICompositionDrawingSurfaceInterop,
    _surface_brush: CompositionSurfaceBrush,
    viewport: ContainerVisual,
    group: ContainerVisual,
    bitmap: Option<ID2D1Bitmap1>,
    size: D2D_SIZE_U,
    dpi: u32,
    pixels: Vec<u8>,
    scratch: Vec<u8>,
    has_contents: bool,
}

fn overlay_surface_error(operation: &str, error: windows::core::Error) -> windows::core::Error {
    Error::new(
        error.code(),
        format!("Composition result surface {operation}: {error}"),
    )
}

impl CompositionResultOverlay {
    unsafe fn new(compositor: &Compositor, dpi: u32, body_top: f32) -> windows::core::Result<Self> {
        let mut device = None;
        let hardware = D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        );
        if hardware.is_err() {
            device = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )?;
        }
        let device: ID3D11Device = device.ok_or_else(Error::from_thread)?;
        let dxgi_device: IDXGIDevice = device.cast()?;
        // CompositionDrawingSurfaceInterop::BeginDraw is requested as an
        // ID2D1DeviceContext below, so back its graphics device with the D2D
        // device created from this D3D11 adapter. Keep the rendering-device
        // interface consistent with the requested drawing interface.
        let rendering_device = D2D1CreateDevice(&dxgi_device, None)?;
        let compositor_interop: ICompositorInterop = compositor.cast()?;
        let graphics_device = compositor_interop.CreateGraphicsDevice(&rendering_device)?;
        let dpi = dpi.max(96);
        let scale = composition_scale_for_dpi(dpi);
        let size = D2D_SIZE_U {
            width: (CONTENT_WIDTH as f32 * scale).ceil() as u32,
            height: (TOP_HEIGHT as f32 * scale).ceil() as u32,
        };
        let surface = graphics_device.CreateDrawingSurface(
            windows::Foundation::Size {
                Width: size.width as f32,
                Height: size.height as f32,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        let surface_interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
        let surface_brush = compositor.CreateSurfaceBrushWithSurface(&surface)?;
        surface_brush.SetStretch(CompositionStretch::Fill)?;

        // Keep the clip in a fixed header-sized parent. Only the card group
        // below this viewport moves during entry/exit, so the transparent
        // Composition target cannot reveal a card over the D2D history.
        let viewport = compositor.CreateContainerVisual()?;
        viewport.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: TOP_HEIGHT as f32,
        })?;
        viewport.SetBorderMode(CompositionBorderMode::Soft)?;
        let clip = compositor.CreateInsetClipWithInsets(0.0, 0.0, 0.0, 0.0)?;
        viewport.SetClip(&clip)?;

        let group = compositor.CreateContainerVisual()?;
        group.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: TOP_HEIGHT as f32,
        })?;
        group.SetOpacity(0.0)?;
        group.SetBorderMode(CompositionBorderMode::Soft)?;
        let sprite = compositor.CreateSpriteVisual()?;
        sprite.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: TOP_HEIGHT as f32,
        })?;
        sprite.SetBrush(&surface_brush)?;
        group.Children()?.InsertAtTop(&sprite)?;
        viewport.Children()?.InsertAtTop(&group)?;
        viewport.SetOffset(composition_header_viewport_offset(dpi, body_top))?;

        Ok(Self {
            _graphics_device: graphics_device,
            _rendering_device: rendering_device,
            _surface: surface,
            surface_interop,
            _surface_brush: surface_brush,
            viewport,
            group,
            bitmap: None,
            size,
            dpi,
            pixels: Vec::new(),
            scratch: Vec::new(),
            has_contents: false,
        })
    }

    unsafe fn resize(&mut self, dpi: u32) -> windows::core::Result<()> {
        let dpi = dpi.max(96);
        if self.dpi == dpi {
            return Ok(());
        }
        let scale = composition_scale_for_dpi(dpi);
        let size = D2D_SIZE_U {
            width: (CONTENT_WIDTH as f32 * scale).ceil() as u32,
            height: (TOP_HEIGHT as f32 * scale).ceil() as u32,
        };
        self.surface_interop
            .Resize(SIZE {
                cx: size.width as i32,
                cy: size.height as i32,
            })
            .map_err(|error| overlay_surface_error("Resize", error))?;
        self.size = size;
        self.dpi = dpi;
        self.bitmap = None;
        self.pixels.clear();
        self.scratch.clear();
        self.has_contents = false;
        Ok(())
    }

    fn set_viewport(&self, dpi: u32, body_top: f32) -> windows::core::Result<()> {
        self.viewport
            .SetOffset(composition_header_viewport_offset(dpi, body_top))
    }

    unsafe fn update_from_layered(
        &mut self,
        source: &LayeredSurface,
        capture_top: f32,
        body_top: f32,
        dpi: u32,
    ) -> windows::core::Result<()> {
        self.resize(dpi)?;
        self.set_viewport(dpi, body_top)?;
        let source_len = source.size.width as usize * source.size.height as usize * 4;
        let source_bytes = std::slice::from_raw_parts(source._pixels.cast::<u8>(), source_len);
        let capture_top_px = (capture_top * self.dpi as f32 / 96.0).round() as i32;
        copy_panel_pixels_into(
            source_bytes,
            source.size.width,
            source.size.height,
            capture_top_px,
            self.size.width,
            self.size.height,
            &mut self.scratch,
        );

        if !self.has_contents || self.scratch != self.pixels {
            let mut offset = POINT::default();
            let context = self
                .surface_interop
                .BeginDraw::<ID2D1DeviceContext>(None, &mut offset)
                .map_err(|error| overlay_surface_error("BeginDraw", error))?;
            let draw_result = (|| -> windows::core::Result<()> {
                context.SetDpi(96.0, 96.0);
                context.SetTransform(&Matrix3x2 {
                    M11: 1.0,
                    M22: 1.0,
                    M31: offset.x as f32,
                    M32: offset.y as f32,
                    ..Default::default()
                });
                let bitmap = if let Some(bitmap) = self.bitmap.as_ref() {
                    bitmap.clone()
                } else {
                    let properties = D2D1_BITMAP_PROPERTIES1 {
                        pixelFormat: D2D1_PIXEL_FORMAT {
                            format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                        },
                        dpiX: 96.0,
                        dpiY: 96.0,
                        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                        colorContext: std::mem::ManuallyDrop::new(None),
                    };
                    let bitmap = context
                        .CreateBitmap(
                            self.size,
                            Some(self.scratch.as_ptr().cast()),
                            self.size.width * 4,
                            &properties,
                        )
                        .map_err(|error| overlay_surface_error("CreateBitmap", error))?;
                    self.bitmap = Some(bitmap.clone());
                    bitmap
                };
                if self.bitmap.is_some() && self.has_contents {
                    bitmap
                        .CopyFromMemory(None, self.scratch.as_ptr().cast(), self.size.width * 4)
                        .map_err(|error| overlay_surface_error("CopyFromMemory", error))?;
                }
                let transparent = d2d_color(0, 0, 0, 0.0);
                context.Clear(Some(&transparent));
                let destination = rect(0.0, 0.0, self.size.width as f32, self.size.height as f32);
                context.DrawBitmap(
                    &bitmap,
                    Some(&destination),
                    1.0,
                    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                    None,
                );
                Ok(())
            })();
            let end_result = self.surface_interop.EndDraw();
            draw_result?;
            end_result.map_err(|error| overlay_surface_error("EndDraw", error))?;
            std::mem::swap(&mut self.pixels, &mut self.scratch);
            self.has_contents = true;
        }

        Ok(())
    }

    fn set_position(&self, panel_top: f32, body_top: f32, dpi: u32) -> windows::core::Result<()> {
        self.set_viewport(dpi, body_top)
            .map_err(|error| overlay_surface_error("Viewport.SetOffset", error))?;
        self.group
            .StopAnimation(&HSTRING::from("Offset.Y"))
            .map_err(|error| overlay_surface_error("StopAnimation.Offset.Y", error))?;
        self.group
            .SetOffset(result_overlay_child_offset(panel_top, body_top))
            .map_err(|error| overlay_surface_error("SetOffset", error))?;
        self.group
            .SetOpacity(1.0)
            .map_err(|error| overlay_surface_error("SetOpacity", error))
    }

    fn animate_position(
        &self,
        compositor: &Compositor,
        motion: &ResultOverlayMotionAnimation,
        body_top: f32,
        dpi: u32,
    ) -> windows::core::Result<()> {
        self.set_viewport(dpi, body_top)
            .map_err(|error| overlay_surface_error("Viewport.SetOffset", error))?;
        self.group
            .StopAnimation(&HSTRING::from("Offset.Y"))
            .map_err(|error| overlay_surface_error("StopAnimation.Offset.Y", error))?;
        self.group
            .SetOffset(Vector3 {
                X: 0.0,
                Y: motion.from_offset,
                Z: 0.0,
            })
            .map_err(|error| overlay_surface_error("SetOffset.Start", error))?;
        let easing = compositor
            .CreateCubicBezierEasingFunction(
                Vector2 {
                    X: motion.easing.x1 as f32,
                    Y: motion.easing.y1 as f32,
                },
                Vector2 {
                    X: motion.easing.x2 as f32,
                    Y: motion.easing.y2 as f32,
                },
            )
            .map_err(|error| overlay_surface_error("CreateCubicBezierEasingFunction", error))?;
        let animation = create_scalar_transition(
            compositor,
            motion.from_offset,
            motion.to_offset,
            motion.remaining,
            &easing,
        )
        .map_err(|error| overlay_surface_error("CreateOffsetYAnimation", error))?;
        self.group
            .StartAnimation(&HSTRING::from("Offset.Y"), &animation)
            .map_err(|error| overlay_surface_error("StartAnimation.Offset.Y", error))?;
        self.group
            .SetOpacity(1.0)
            .map_err(|error| overlay_surface_error("SetOpacity", error))
    }

    fn hide(&self) -> windows::core::Result<()> {
        self.group
            .StopAnimation(&HSTRING::from("Offset.Y"))
            .map_err(|error| overlay_surface_error("Hide.StopAnimation.Offset.Y", error))?;
        self.group
            .SetOpacity(0.0)
            .map_err(|error| overlay_surface_error("Hide.SetOpacity", error))
    }
}

#[derive(Debug)]
struct CompositionRing {
    visual: ContainerVisual,
    geometry: Option<CompositionEllipseGeometry>,
    shape: Option<CompositionSpriteShape>,
    _inner_fill_visual: Option<ShapeVisual>,
    core_container: Option<ContainerVisual>,
    sublayer_container: Option<ContainerVisual>,
    inner_animations: Option<InnerRingAnimations>,
    core_scale_animation: Option<Vector3KeyFrameAnimation>,
    geometry_animation: Option<Vector2KeyFrameAnimation>,
    border_animation: Option<ScalarKeyFrameAnimation>,
    diameter: f32,
    base_stroke_width: f32,
    base_opacity: f32,
}

#[derive(Debug)]
struct InnerRingAnimations {
    core_scale: Vector3KeyFrameAnimation,
    core_opacity: ScalarKeyFrameAnimation,
    sublayer_scale: Vector3KeyFrameAnimation,
    scale_null: Vector3KeyFrameAnimation,
}

#[implement(IGeometrySource2D, IGeometrySource2DInterop)]
struct D2DCompositionGeometrySource {
    geometry: ID2D1Geometry,
}

impl windows::Graphics::IGeometrySource2D_Impl for D2DCompositionGeometrySource_Impl {}

impl IGeometrySource2DInterop_Impl for D2DCompositionGeometrySource_Impl {
    fn GetGeometry(&self) -> windows::core::Result<ID2D1Geometry> {
        Ok(self.geometry.clone())
    }

    fn TryGetGeometryUsingFactory(
        &self,
        _factory: Ref<ID2D1Factory>,
    ) -> windows::core::Result<ID2D1Geometry> {
        Ok(self.geometry.clone())
    }
}

#[derive(Debug)]
struct CompositionLogo {
    visual: ContainerVisual,
    _transparent_visual: ShapeVisual,
    _transparent_shadow_visual: SpriteVisual,
    _transparent_shadow_surface: CompositionVisualSurface,
    transparent_shadow: DropShadow,
    long_visual: ShapeVisual,
    final_visual: ShapeVisual,
    _transparent_geometries: [CompositionPathGeometry; 2],
    _long_geometries: [CompositionPathGeometry; 2],
    transparent_brush: CompositionColorBrush,
    _solid_brush: CompositionColorBrush,
    transparent_trim_animation: ScalarKeyFrameAnimation,
    solid_trim_start_animation: ScalarKeyFrameAnimation,
    solid_trim_end_animation: ScalarKeyFrameAnimation,
    started_at: Option<Instant>,
    stage: ui_model::StartupLogoStage,
}

const SHZ_LOGO_SOURCE_SIZE: f32 = 210.0;
const SHZ_LOGO_CANVAS_PADDING: f32 = 36.0;
const SHZ_LOGO_CANVAS_SIZE: f32 = SHZ_LOGO_SOURCE_SIZE + SHZ_LOGO_CANVAS_PADDING * 2.0;
const SHZ_LOGO_REFERENCE_SIZE: f32 = 367.5;
const SHZ_LOGO_STROKE_WIDTH: f32 = 38.0;

fn composition_scale_for_dpi(dpi: u32) -> f32 {
    dpi.max(96) as f32 / 96.0
}

fn composition_pixel_phase(dip: f32, scale: f32) -> f32 {
    ((dip * scale).round() - dip * scale) / scale
}

fn composition_button_pixel_snap_offset(dpi: u32, body_top: f32) -> Vector3 {
    let scale = composition_scale_for_dpi(dpi);
    Vector3 {
        X: composition_pixel_phase((BUTTON_X + BUTTON_SIZE * 0.5) as f32, scale),
        Y: composition_pixel_phase(body_top + (BUTTON_TOP + BUTTON_SIZE * 0.5) as f32, scale),
        Z: 0.0,
    }
}

fn composition_header_viewport_offset(dpi: u32, body_top: f32) -> Vector3 {
    let root_snap = composition_button_pixel_snap_offset(dpi, body_top);
    Vector3 {
        X: -root_snap.X,
        Y: body_top - root_snap.Y,
        Z: 0.0,
    }
}

fn result_overlay_child_offset(panel_top: f32, body_top: f32) -> Vector3 {
    Vector3 {
        X: 0.0,
        Y: panel_top - body_top,
        Z: 0.0,
    }
}

#[cfg(test)]
fn result_header_visible_height(panel_top: f32, body_top: f32) -> f32 {
    let header_top = body_top;
    let header_bottom = body_top + TOP_HEIGHT as f32;
    let card_top = panel_top.max(header_top);
    let card_bottom = (panel_top + TOP_HEIGHT as f32).min(header_bottom);
    (card_bottom - card_top).max(0.0)
}

fn result_header_hit_test_contains(y: f32, body_top: f32) -> bool {
    (body_top..body_top + TOP_HEIGHT as f32).contains(&y)
}

fn create_scale_transition(
    compositor: &Compositor,
    from: f32,
    to: f32,
    duration: Duration,
    easing: &CubicBezierEasingFunction,
) -> windows::core::Result<Vector3KeyFrameAnimation> {
    let animation = compositor.CreateVector3KeyFrameAnimation()?;
    animation.SetDuration(TimeSpan {
        Duration: (duration.as_nanos() / 100).min(i64::MAX as u128) as i64,
    })?;
    animation.InsertKeyFrame(
        0.0,
        Vector3 {
            X: from,
            Y: from,
            Z: 1.0,
        },
    )?;
    animation.InsertKeyFrameWithEasingFunction(
        1.0,
        Vector3 {
            X: to,
            Y: to,
            Z: 1.0,
        },
        easing,
    )?;
    Ok(animation)
}

fn create_scalar_transition(
    compositor: &Compositor,
    from: f32,
    to: f32,
    duration: Duration,
    easing: &CubicBezierEasingFunction,
) -> windows::core::Result<ScalarKeyFrameAnimation> {
    let animation = compositor.CreateScalarKeyFrameAnimation()?;
    animation.SetDuration(TimeSpan {
        Duration: (duration.as_nanos() / 100).min(i64::MAX as u128) as i64,
    })?;
    animation.InsertKeyFrame(0.0, from)?;
    animation.InsertKeyFrameWithEasingFunction(1.0, to, easing)?;
    Ok(animation)
}

fn create_vector2_scale_transition(
    compositor: &Compositor,
    from: f32,
    to: f32,
    duration: Duration,
    easing: &CubicBezierEasingFunction,
) -> windows::core::Result<Vector2KeyFrameAnimation> {
    let animation = compositor.CreateVector2KeyFrameAnimation()?;
    animation.SetDuration(TimeSpan {
        Duration: (duration.as_nanos() / 100).min(i64::MAX as u128) as i64,
    })?;
    animation.InsertKeyFrame(0.0, Vector2 { X: from, Y: from })?;
    animation.InsertKeyFrameWithEasingFunction(1.0, Vector2 { X: to, Y: to }, easing)?;
    Ok(animation)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ShzLogoLayout {
    center_x: f32,
    center_y: f32,
    offset_x: f32,
    offset_y: f32,
    scale_x: f32,
    scale_y: f32,
}

fn shz_logo_layout(button_width: f32, button_height: f32) -> ShzLogoLayout {
    let center_x = button_width * 0.5;
    let center_y = button_height * 0.5;
    ShzLogoLayout {
        center_x,
        center_y,
        offset_x: center_x - SHZ_LOGO_CANVAS_SIZE * 0.5,
        offset_y: center_y - SHZ_LOGO_CANVAS_SIZE * 0.5,
        scale_x: button_width / SHZ_LOGO_REFERENCE_SIZE,
        scale_y: button_height / SHZ_LOGO_REFERENCE_SIZE,
    }
}

#[cfg(test)]
fn shz_logo_point_in_button(layout: ShzLogoLayout, point: (f32, f32)) -> (f32, f32) {
    (
        layout.center_x
            + (point.0 + SHZ_LOGO_CANVAS_PADDING - SHZ_LOGO_CANVAS_SIZE * 0.5) * layout.scale_x,
        layout.center_y
            + (point.1 + SHZ_LOGO_CANVAS_PADDING - SHZ_LOGO_CANVAS_SIZE * 0.5) * layout.scale_y,
    )
}

impl CompositionLogo {
    unsafe fn new(
        d2d_factory: &ID2D1Factory,
        compositor: &Compositor,
        theme: Theme,
        animate_on_create: bool,
    ) -> windows::core::Result<Self> {
        let transparent_brush =
            compositor.CreateColorBrushWithColor(logo_transparent_color(theme))?;
        let solid_brush =
            compositor.CreateColorBrushWithColor(composition_color(255, 255, 255, 255))?;
        let (transparent_visual, transparent_geometries) = make_logo_visual(
            compositor,
            d2d_factory,
            [SHZ_LOGO_TOP_PATH, SHZ_LOGO_BOTTOM_PATH],
            &transparent_brush,
            LogoTrim::Start,
        )?;
        let (long_visual, long_geometries) = make_logo_visual(
            compositor,
            d2d_factory,
            [SHZ_LOGO_TOP_LONG_PATH, SHZ_LOGO_BOTTOM_LONG_PATH],
            &solid_brush,
            LogoTrim::StartAndEnd,
        )?;
        let (final_visual, _) = make_logo_visual(
            compositor,
            d2d_factory,
            [SHZ_LOGO_TOP_PATH, SHZ_LOGO_BOTTOM_PATH],
            &solid_brush,
            LogoTrim::None,
        )?;
        transparent_visual.SetOpacity(if animate_on_create { 1.0 } else { 0.0 })?;
        long_visual.SetOpacity(if animate_on_create { 1.0 } else { 0.0 })?;
        final_visual.SetOpacity(if animate_on_create { 0.0 } else { 1.0 })?;

        let transparent_root = compositor.CreateLayerVisual()?;
        transparent_root.SetSize(Vector2 {
            X: SHZ_LOGO_CANVAS_SIZE,
            Y: SHZ_LOGO_CANVAS_SIZE,
        })?;
        transparent_root
            .Children()?
            .InsertAtTop(&transparent_visual)?;

        // Capture the transparent stroke's alpha, not its padded rectangular
        // canvas, so the startup shadow follows the logo outline.
        let transparent_shadow_surface = compositor.CreateVisualSurface()?;
        transparent_shadow_surface.SetSourceVisual(&transparent_visual)?;
        transparent_shadow_surface.SetSourceOffset(Vector2 { X: 0.0, Y: 0.0 })?;
        transparent_shadow_surface.SetSourceSize(Vector2 {
            X: SHZ_LOGO_CANVAS_SIZE,
            Y: SHZ_LOGO_CANVAS_SIZE,
        })?;
        let transparent_shadow_brush =
            compositor.CreateSurfaceBrushWithSurface(&transparent_shadow_surface)?;
        let transparent_shadow = compositor.CreateDropShadow()?;
        transparent_shadow.SetBlurRadius(1.0)?;
        transparent_shadow.SetOffset(Vector3 {
            X: 0.5,
            Y: 1.0,
            Z: 0.0,
        })?;
        transparent_shadow.SetOpacity(1.0)?;
        transparent_shadow.SetColor(logo_shadow_color(theme))?;
        transparent_shadow.SetSourcePolicy(
            windows::UI::Composition::CompositionDropShadowSourcePolicy::InheritFromVisualContent,
        )?;
        let transparent_shadow_visual = compositor.CreateSpriteVisual()?;
        transparent_shadow_visual.SetSize(Vector2 {
            X: SHZ_LOGO_CANVAS_SIZE,
            Y: SHZ_LOGO_CANVAS_SIZE,
        })?;
        transparent_shadow_visual.SetBrush(&transparent_shadow_brush)?;
        transparent_shadow_visual.SetShadow(&transparent_shadow)?;

        let visual = compositor.CreateContainerVisual()?;
        visual.SetSize(Vector2 {
            X: SHZ_LOGO_CANVAS_SIZE,
            Y: SHZ_LOGO_CANVAS_SIZE,
        })?;
        let layout = shz_logo_layout(BUTTON_SIZE as f32, BUTTON_SIZE as f32);
        visual.SetCenterPoint(Vector3 {
            X: layout.center_x - layout.offset_x,
            Y: layout.center_y - layout.offset_y,
            Z: 0.0,
        })?;
        visual.SetOffset(Vector3 {
            X: layout.offset_x,
            Y: layout.offset_y,
            Z: 0.0,
        })?;
        visual.SetScale(Vector3 {
            X: layout.scale_x,
            Y: layout.scale_y,
            Z: 1.0,
        })?;
        visual.Children()?.InsertAtTop(&transparent_shadow_visual)?;
        visual.Children()?.InsertAtTop(&transparent_root)?;
        visual.Children()?.InsertAtTop(&long_visual)?;
        visual.Children()?.InsertAtTop(&final_visual)?;

        let transparent_trim = make_logo_trim_animation(
            compositor,
            ui_model::STARTUP_LOGO_TRANSPARENT_DURATION,
            0.0,
            1.0,
            CubicBezier {
                x1: 1.0,
                y1: 0.0,
                x2: 1.0,
                y2: 1.0,
            },
            0.0,
        )?;
        let solid_trim_start = make_logo_trim_animation(
            compositor,
            ui_model::STARTUP_LOGO_SOLID_DURATION,
            0.0,
            ui_model::STARTUP_LOGO_SOLID_TRIM_START_END,
            CubicBezier {
                x1: 0.4,
                y1: 1.0,
                x2: 0.41,
                y2: 1.0,
            },
            ui_model::STARTUP_LOGO_SOLID_DELAY,
        )?;
        let solid_trim_end = make_logo_trim_animation(
            compositor,
            ui_model::STARTUP_LOGO_SOLID_DURATION,
            0.0,
            1.0,
            CubicBezier {
                x1: 0.4,
                y1: 1.0,
                x2: 0.41,
                y2: 1.0,
            },
            ui_model::STARTUP_LOGO_SOLID_DELAY,
        )?;
        Ok(Self {
            transparent_brush,
            _solid_brush: solid_brush,
            visual,
            _transparent_visual: transparent_visual,
            _transparent_shadow_visual: transparent_shadow_visual,
            _transparent_shadow_surface: transparent_shadow_surface,
            transparent_shadow,
            long_visual,
            final_visual,
            _transparent_geometries: transparent_geometries,
            _long_geometries: long_geometries,
            transparent_trim_animation: transparent_trim,
            solid_trim_start_animation: solid_trim_start,
            solid_trim_end_animation: solid_trim_end,
            started_at: None,
            stage: if animate_on_create {
                ui_model::StartupLogoStage::TransparentStroke
            } else {
                ui_model::StartupLogoStage::Complete
            },
        })
    }

    fn start(&mut self) -> windows::core::Result<()> {
        if self.started_at.is_some() || self.stage == ui_model::StartupLogoStage::Complete {
            return Ok(());
        }
        let started_at = Instant::now();
        for geometry in &self._transparent_geometries {
            geometry.StartAnimation(
                &HSTRING::from("TrimStart"),
                &self.transparent_trim_animation,
            )?;
        }
        for geometry in &self._long_geometries {
            geometry.StartAnimation(
                &HSTRING::from("TrimStart"),
                &self.solid_trim_start_animation,
            )?;
            geometry.StartAnimation(&HSTRING::from("TrimEnd"), &self.solid_trim_end_animation)?;
        }
        self.started_at = Some(started_at);
        Ok(())
    }

    fn advance(&mut self) -> windows::core::Result<bool> {
        let Some(started_at) = self.started_at else {
            return Ok(self.stage == ui_model::StartupLogoStage::Complete);
        };
        let frame = ui_model::startup_logo_frame_at(started_at.elapsed().as_secs_f64());
        if frame.stage != self.stage {
            match frame.stage {
                ui_model::StartupLogoStage::TransparentStroke => {}
                ui_model::StartupLogoStage::SolidLongStroke => {}
                ui_model::StartupLogoStage::Complete => {
                    for geometry in &self._transparent_geometries {
                        geometry.StopAnimation(&HSTRING::from("TrimStart"))?;
                    }
                    for geometry in &self._long_geometries {
                        geometry.StopAnimation(&HSTRING::from("TrimStart"))?;
                        geometry.StopAnimation(&HSTRING::from("TrimEnd"))?;
                    }
                    // The native implementation removes its temporary
                    // translucent strokes and their outline shadow when the
                    // solid logo takes over. Leaving either visual visible
                    // underneath the final paths adds a ghost edge/pattern
                    // that becomes especially noticeable while listening.
                    self._transparent_visual.SetOpacity(0.0)?;
                    self._transparent_shadow_visual.SetOpacity(0.0)?;
                    self.long_visual.SetOpacity(0.0)?;
                    self.final_visual.SetOpacity(1.0)?;
                }
            }
            self.stage = frame.stage;
        }
        Ok(self.stage == ui_model::StartupLogoStage::Complete)
    }

    fn set_theme(&self, theme: Theme) -> windows::core::Result<()> {
        self.transparent_brush
            .SetColor(logo_transparent_color(theme))?;
        self.transparent_shadow.SetColor(logo_shadow_color(theme))
    }
}

#[derive(Clone, Copy)]
struct LogoBezier {
    control1: (f32, f32),
    control2: (f32, f32),
    end: (f32, f32),
}

#[derive(Clone, Copy)]
struct LogoPath<const N: usize> {
    start: (f32, f32),
    curves: [LogoBezier; N],
}

const SHZ_LOGO_TOP_PATH: LogoPath<4> = LogoPath {
    start: (76.19, 10.38),
    curves: [
        LogoBezier {
            control1: (62.4, 24.48),
            control2: (41.85, 44.71),
            end: (32.49, 54.68),
        },
        LogoBezier {
            control1: (11.96, 76.54),
            control2: (12.8, 113.87),
            end: (34.29, 134.68),
        },
        LogoBezier {
            control1: (56.8, 157.87),
            control2: (93.03, 156.21),
            end: (113.29, 138.21),
        },
        LogoBezier {
            control1: (119.79, 132.0),
            control2: (133.69, 117.88),
            end: (133.69, 117.88),
        },
    ],
};

const SHZ_LOGO_BOTTOM_PATH: LogoPath<4> = LogoPath {
    start: (133.82, 201.02),
    curves: [
        LogoBezier {
            control1: (147.6, 186.93),
            control2: (168.16, 166.7),
            end: (177.52, 156.72),
        },
        LogoBezier {
            control1: (198.04, 134.86),
            control2: (197.21, 97.53),
            end: (175.72, 76.72),
        },
        LogoBezier {
            control1: (153.21, 53.53),
            control2: (116.98, 55.2),
            end: (96.72, 73.2),
        },
        LogoBezier {
            control1: (90.22, 79.4),
            control2: (76.32, 93.52),
            end: (76.32, 93.52),
        },
    ],
};

const SHZ_LOGO_TOP_LONG_PATH: LogoPath<8> = LogoPath {
    start: (133.6, 117.9),
    curves: [
        LogoBezier {
            control1: (144.05, 107.41),
            control2: (155.21, 96.08),
            end: (161.2, 89.7),
        },
        LogoBezier {
            control1: (181.72, 67.84),
            control2: (180.89, 30.51),
            end: (159.4, 9.7),
        },
        LogoBezier {
            control1: (136.89, -13.49),
            control2: (100.97, -11.49),
            end: (80.7, 6.17),
        },
        LogoBezier {
            control1: (80.7, 6.17),
            control2: (77.6, 9.12),
            end: (76.3, 10.4),
        },
        LogoBezier {
            control1: (62.4, 24.48),
            control2: (41.85, 44.71),
            end: (32.49, 54.68),
        },
        LogoBezier {
            control1: (11.96, 76.54),
            control2: (12.8, 113.87),
            end: (34.29, 134.68),
        },
        LogoBezier {
            control1: (56.8, 157.87),
            control2: (93.03, 156.21),
            end: (113.29, 138.21),
        },
        LogoBezier {
            control1: (119.79, 132.0),
            control2: (133.69, 117.88),
            end: (133.69, 117.88),
        },
    ],
};

const SHZ_LOGO_BOTTOM_LONG_PATH: LogoPath<8> = LogoPath {
    start: (76.4, 93.5),
    curves: [
        LogoBezier {
            control1: (65.95, 103.99),
            control2: (55.21, 114.99),
            end: (49.22, 121.37),
        },
        LogoBezier {
            control1: (28.69, 143.24),
            control2: (29.53, 180.57),
            end: (51.02, 201.37),
        },
        LogoBezier {
            control1: (73.53, 224.57),
            control2: (109.45, 222.57),
            end: (129.72, 204.9),
        },
        LogoBezier {
            control1: (129.72, 204.9),
            control2: (132.31, 202.48),
            end: (133.62, 201.2),
        },
        LogoBezier {
            control1: (147.6, 186.93),
            control2: (168.16, 166.7),
            end: (177.52, 156.72),
        },
        LogoBezier {
            control1: (198.04, 134.86),
            control2: (197.21, 97.53),
            end: (175.72, 76.72),
        },
        LogoBezier {
            control1: (153.21, 53.53),
            control2: (116.98, 55.2),
            end: (96.72, 73.2),
        },
        LogoBezier {
            control1: (90.22, 79.4),
            control2: (76.32, 93.52),
            end: (76.32, 93.52),
        },
    ],
};

#[derive(Clone, Copy)]
enum LogoTrim {
    Start,
    StartAndEnd,
    None,
}

unsafe fn make_logo_visual<const N: usize>(
    compositor: &Compositor,
    d2d_factory: &ID2D1Factory,
    paths: [LogoPath<N>; 2],
    brush: &CompositionColorBrush,
    trim: LogoTrim,
) -> windows::core::Result<(ShapeVisual, [CompositionPathGeometry; 2])> {
    let visual = compositor.CreateShapeVisual()?;
    visual.SetSize(Vector2 {
        X: SHZ_LOGO_CANVAS_SIZE,
        Y: SHZ_LOGO_CANVAS_SIZE,
    })?;
    let geometries = [
        make_logo_path_geometry(compositor, d2d_factory, paths[0], trim)?,
        make_logo_path_geometry(compositor, d2d_factory, paths[1], trim)?,
    ];
    for geometry in &geometries {
        let shape = compositor.CreateSpriteShapeWithGeometry(geometry)?;
        shape.SetStrokeBrush(brush)?;
        shape.SetStrokeThickness(SHZ_LOGO_STROKE_WIDTH)?;
        shape.SetStrokeStartCap(windows::UI::Composition::CompositionStrokeCap::Round)?;
        shape.SetStrokeEndCap(windows::UI::Composition::CompositionStrokeCap::Round)?;
        shape.SetOffset(Vector2 {
            X: SHZ_LOGO_CANVAS_PADDING,
            Y: SHZ_LOGO_CANVAS_PADDING,
        })?;
        visual.Shapes()?.Append(&shape)?;
    }
    Ok((visual, geometries))
}

fn button_border_arc_points() -> [(f32, f32); 4] {
    let center_x = BUTTON_SIZE * 0.5;
    let center_y = BUTTON_SIZE * 0.5 + 2.0;
    let radius = BUTTON_SIZE * 0.5 + 1.0;
    // NSBezierPath's appendArc selector uses degrees. The native value π is
    // therefore a 3.14159-degree arc, not a half-circle.
    let start_angle = 0.0f64;
    let end_angle = std::f64::consts::PI.to_radians();
    let tangent = (end_angle - start_angle) * 0.25;
    let control_scale = 4.0 / 3.0 * tangent.tan();
    let point = |angle: f64| {
        (
            (center_x + radius * angle.cos()) as f32,
            (center_y + radius * angle.sin()) as f32,
        )
    };
    let derivative = |angle: f64| (-radius * angle.sin(), radius * angle.cos());
    let p0 = point(start_angle);
    let p3 = point(end_angle);
    let d0 = derivative(start_angle);
    let d1 = derivative(end_angle);
    [
        p0,
        (
            (p0.0 as f64 + control_scale * d0.0) as f32,
            (p0.1 as f64 + control_scale * d0.1) as f32,
        ),
        (
            (p3.0 as f64 - control_scale * d1.0) as f32,
            (p3.1 as f64 - control_scale * d1.1) as f32,
        ),
        p3,
    ]
}

unsafe fn make_button_border_arc_geometry(
    compositor: &Compositor,
    d2d_factory: &ID2D1Factory,
) -> windows::core::Result<CompositionPathGeometry> {
    let [start, control1, control2, end] = button_border_arc_points();
    let geometry = d2d_factory.CreatePathGeometry()?;
    let sink = geometry.Open()?;
    sink.BeginFigure(
        Vector2 {
            X: start.0,
            Y: start.1,
        },
        D2D1_FIGURE_BEGIN_HOLLOW,
    );
    sink.AddBezier(&D2D1_BEZIER_SEGMENT {
        point1: Vector2 {
            X: control1.0,
            Y: control1.1,
        },
        point2: Vector2 {
            X: control2.0,
            Y: control2.1,
        },
        point3: Vector2 { X: end.0, Y: end.1 },
    });
    sink.EndFigure(D2D1_FIGURE_END_OPEN);
    sink.Close()?;
    let geometry: ID2D1Geometry = geometry.cast()?;
    let source: IGeometrySource2D = D2DCompositionGeometrySource { geometry }.into();
    compositor.CreatePathGeometryWithPath(&CompositionPath::Create(&source)?)
}

unsafe fn make_logo_path_geometry<const N: usize>(
    compositor: &Compositor,
    d2d_factory: &ID2D1Factory,
    path: LogoPath<N>,
    trim: LogoTrim,
) -> windows::core::Result<CompositionPathGeometry> {
    let geometry = d2d_factory.CreatePathGeometry()?;
    let sink = geometry.Open()?;
    sink.BeginFigure(
        Vector2 {
            X: path.start.0,
            Y: path.start.1,
        },
        D2D1_FIGURE_BEGIN_HOLLOW,
    );
    let beziers = path.curves.map(|curve| D2D1_BEZIER_SEGMENT {
        point1: Vector2 {
            X: curve.control1.0,
            Y: curve.control1.1,
        },
        point2: Vector2 {
            X: curve.control2.0,
            Y: curve.control2.1,
        },
        point3: Vector2 {
            X: curve.end.0,
            Y: curve.end.1,
        },
    });
    sink.AddBeziers(&beziers);
    sink.EndFigure(D2D1_FIGURE_END_OPEN);
    sink.Close()?;

    let geometry: ID2D1Geometry = geometry.cast()?;
    let source: IGeometrySource2D = D2DCompositionGeometrySource { geometry }.into();
    let path = CompositionPath::Create(&source)?;
    let geometry = compositor.CreatePathGeometryWithPath(&path)?;
    match trim {
        LogoTrim::Start => geometry.SetTrimStart(0.0)?,
        LogoTrim::StartAndEnd => {
            geometry.SetTrimStart(0.0)?;
            geometry.SetTrimEnd(0.0)?;
        }
        LogoTrim::None => {}
    }
    Ok(geometry)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum NoMatchIconCommand {
    Move((f64, f64)),
    Line((f64, f64)),
    Cubic((f64, f64), (f64, f64), (f64, f64)),
    Close,
}

// UI-168: ARM64 and x86_64 constants agree for all 34 path commands. Keep
// duplicate line segments and out-of-range control points as recorded.
const NO_MATCH_ICON_PATH: [NoMatchIconCommand; 34] = [
    NoMatchIconCommand::Move((0.499499989, 0.804914556)),
    NoMatchIconCommand::Cubic(
        (0.657000017, 0.804914556),
        (0.784749985, 0.848632486),
        (0.784749985, 0.902521378),
    ),
    NoMatchIconCommand::Cubic(
        (0.784749985, 0.956367493),
        (0.657000017, 1.000085488),
        (0.499499989, 1.000085488),
    ),
    NoMatchIconCommand::Cubic(
        (0.341875005, 1.000085488),
        (0.214000010, 0.956367493),
        (0.214000010, 0.902521378),
    ),
    NoMatchIconCommand::Cubic(
        (0.214000010, 0.848632486),
        (0.341875005, 0.804914556),
        (0.499499989, 0.804914556),
    ),
    NoMatchIconCommand::Close,
    NoMatchIconCommand::Move((0.686124992, 0.624957288)),
    NoMatchIconCommand::Line((0.686124992, 0.625042712)),
    NoMatchIconCommand::Line((0.686124992, 0.625085488)),
    NoMatchIconCommand::Line((0.686124992, 0.625085488)),
    NoMatchIconCommand::Line((0.686000013, 0.625170911)),
    NoMatchIconCommand::Line((0.686000013, 0.625085488)),
    NoMatchIconCommand::Line((0.686000013, 0.625170911)),
    NoMatchIconCommand::Line((0.686000013, 0.625213688)),
    NoMatchIconCommand::Line((0.685874987, 0.625299177)),
    NoMatchIconCommand::Line((0.685874987, 0.625384600)),
    NoMatchIconCommand::Line((0.685750008, 0.625427377)),
    NoMatchIconCommand::Line((0.684375000, 0.627307664)),
    NoMatchIconCommand::Cubic(
        (0.676875019, 0.636923048),
        (0.666625023, 0.645897433),
        (0.651000023, 0.653931642),
    ),
    NoMatchIconCommand::Line((0.647249985, 0.655726506)),
    NoMatchIconCommand::Cubic(
        (0.585750008, 0.685256436),
        (0.473124981, 0.691794893),
        (0.392374992, 0.670341883),
    ),
    NoMatchIconCommand::Cubic(
        (0.317499995, 0.650341849),
        (0.301749992, 0.611923087),
        (0.278125000, 0.580128205),
    ),
    NoMatchIconCommand::Cubic(
        (0.225500011, 0.518247849),
        (0.178999996, 0.455170949),
        (0.132000005, 0.392606849),
    ),
    NoMatchIconCommand::Cubic(
        (0.105999994, 0.361495711),
        (0.082249999, 0.329957261),
        (0.058499998, 0.298589755),
    ),
    NoMatchIconCommand::Cubic(
        (0.027750000, 0.261025649),
        (-0.005125000, 0.223290598),
        (0.000625000, 0.183247868),
    ),
    NoMatchIconCommand::Cubic(
        (0.011125000, 0.111623935),
        (0.125999999, 0.046581195),
        (0.293875003, 0.016965812),
    ),
    NoMatchIconCommand::Cubic(
        (0.464624977, -0.013076923),
        (0.669999981, -0.002606838),
        (0.814999962, 0.043504275),
    ),
    NoMatchIconCommand::Cubic(
        (0.968750000, 0.092350430),
        (1.034624958, 0.174017099),
        (0.979749966, 0.248376064),
    ),
    NoMatchIconCommand::Cubic(
        (0.932999992, 0.311666668),
        (0.883874989, 0.375042744),
        (0.831999969, 0.437692300),
    ),
    NoMatchIconCommand::Cubic(
        (0.784250021, 0.500128200),
        (0.738875008, 0.563247876),
        (0.686124992, 0.625085488),
    ),
    NoMatchIconCommand::Line((0.686124992, 0.625042712)),
    NoMatchIconCommand::Line((0.686124992, 0.624957288)),
    NoMatchIconCommand::Line((0.686124992, 0.624957288)),
    NoMatchIconCommand::Close,
];

const NO_MATCH_ICON_BASE_WIDTH: f32 = 40.0;
const NO_MATCH_ICON_BASE_HEIGHT: f32 = 117.0;
const NO_MATCH_ICON_SCALE: f32 = 0.8;
fn no_match_icon_rect(button_size: f32) -> [f32; 4] {
    let width = NO_MATCH_ICON_BASE_WIDTH * NO_MATCH_ICON_SCALE;
    let height = NO_MATCH_ICON_BASE_HEIGHT * NO_MATCH_ICON_SCALE;
    let left = (button_size - width) * 0.5;
    let top = (button_size - height) * 0.5;
    [left, top, left + width, top + height]
}

fn no_match_icon_point(
    point: (f64, f64),
    width: f32,
    height: f32,
    origin_x: f32,
    origin_y: f32,
) -> Vector2 {
    Vector2 {
        X: (f64::from(origin_x) + point.0 * f64::from(width)) as f32,
        Y: (f64::from(origin_y) + point.1 * f64::from(height)) as f32,
    }
}

unsafe fn make_no_match_icon_geometry(
    compositor: &Compositor,
    d2d_factory: &ID2D1Factory,
    icon_rect: [f32; 4],
) -> windows::core::Result<CompositionPathGeometry> {
    let origin_x = icon_rect[0];
    let origin_y = icon_rect[1];
    let width = icon_rect[2] - icon_rect[0];
    let height = icon_rect[3] - icon_rect[1];
    let source = d2d_factory.CreatePathGeometry()?;
    let sink = source.Open()?;
    for command in NO_MATCH_ICON_PATH {
        match command {
            NoMatchIconCommand::Move(point) => {
                sink.BeginFigure(
                    no_match_icon_point(point, width, height, origin_x, origin_y),
                    D2D1_FIGURE_BEGIN_FILLED,
                );
            }
            NoMatchIconCommand::Line(point) => {
                sink.AddLine(no_match_icon_point(
                    point, width, height, origin_x, origin_y,
                ));
            }
            NoMatchIconCommand::Cubic(cp1, cp2, end) => {
                sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                    point1: no_match_icon_point(cp1, width, height, origin_x, origin_y),
                    point2: no_match_icon_point(cp2, width, height, origin_x, origin_y),
                    point3: no_match_icon_point(end, width, height, origin_x, origin_y),
                });
            }
            NoMatchIconCommand::Close => sink.EndFigure(D2D1_FIGURE_END_CLOSED),
        }
    }
    sink.Close()?;

    // UI-168 finalIconPathFromIconPath:inRect: rotates around the path's own
    // bounds center, then subtracts the supplied rect origin. Apply those
    // operations to the full source coordinates before creating the cached
    // Composition geometry.
    let bounds = source.GetBounds(None)?;
    let center_x = (bounds.left + bounds.right) * 0.5;
    let center_y = (bounds.top + bounds.bottom) * 0.5;
    let geometry = d2d_factory.CreatePathGeometry()?;
    let sink = geometry.Open()?;
    for command in NO_MATCH_ICON_PATH {
        let transform = |point: (f64, f64)| {
            let point = (
                f64::from(origin_x) + point.0 * f64::from(width),
                f64::from(origin_y) + point.1 * f64::from(height),
            );
            Vector2 {
                X: (2.0 * f64::from(center_x) - point.0 - f64::from(origin_x)) as f32,
                Y: (2.0 * f64::from(center_y) - point.1 - f64::from(origin_y)) as f32,
            }
        };
        match command {
            NoMatchIconCommand::Move(point) => {
                sink.BeginFigure(transform(point), D2D1_FIGURE_BEGIN_FILLED);
            }
            NoMatchIconCommand::Line(point) => sink.AddLine(transform(point)),
            NoMatchIconCommand::Cubic(cp1, cp2, end) => {
                sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                    point1: transform(cp1),
                    point2: transform(cp2),
                    point3: transform(end),
                });
            }
            NoMatchIconCommand::Close => sink.EndFigure(D2D1_FIGURE_END_CLOSED),
        }
    }
    sink.Close()?;
    let geometry: ID2D1Geometry = geometry.cast()?;
    let source: IGeometrySource2D = D2DCompositionGeometrySource { geometry }.into();
    compositor.CreatePathGeometryWithPath(&CompositionPath::Create(&source)?)
}

fn make_logo_trim_animation(
    compositor: &Compositor,
    duration_seconds: f64,
    from: f64,
    to: f64,
    curve: CubicBezier,
    delay_seconds: f64,
) -> windows::core::Result<ScalarKeyFrameAnimation> {
    let animation = compositor.CreateScalarKeyFrameAnimation()?;
    animation.SetDuration(TimeSpan {
        Duration: (duration_seconds * 10_000_000.0).round() as i64,
    })?;
    if delay_seconds > 0.0 {
        animation.SetDelayTime(TimeSpan {
            Duration: (delay_seconds * 10_000_000.0).round() as i64,
        })?;
    }
    let easing = compositor.CreateCubicBezierEasingFunction(
        Vector2 {
            X: curve.x1 as f32,
            Y: curve.y1 as f32,
        },
        Vector2 {
            X: curve.x2 as f32,
            Y: curve.y2 as f32,
        },
    )?;
    animation.InsertKeyFrame(0.0, from as f32)?;
    animation.InsertKeyFrameWithEasingFunction(1.0, to as f32, &easing)?;
    Ok(animation)
}

fn logo_transparent_color(theme: Theme) -> Color {
    match theme {
        Theme::Light => composition_color(255, 27, 148, 250),
        Theme::Dark => composition_color(255, 9, 9, 9),
    }
}

fn composition_auto_shadow_color(theme: Theme) -> Color {
    // The native AutoShadow background uses tagButtonBackgroundTransparent.
    logo_transparent_color(theme)
}

fn logo_shadow_color(_theme: Theme) -> Color {
    // Native uses the tagButtonTopBorder color. The dark appearance rendition
    // is still unmeasured; the current UI uses the confirmed light alpha/color.
    composition_color(51, 255, 255, 255)
}

const INNER_MASK_BOUNDS: f32 = 336.0;
const BUTTON_CLEAR_DIAMETER: f32 = 96.0;
const AUTO_SHADOW_DISPLAY_BOUNDS: f32 = BUTTON_CLEAR_DIAMETER;
// AutoShadow is captured to a CompositionVisualSurface before it is scaled
// with the popup root. Keep that source at 4x so the circle stays smooth at
// fractional and high-DPI scales (including the supported 300% range).
const AUTO_SHADOW_RASTER_SCALE: f32 = 4.0;
const INNER_MASK_GEOMETRY_TOLERANCE: f32 = 0.01;

fn auto_shadow_host_center() -> Vector2 {
    let center = AUTO_SHADOW_DISPLAY_BOUNDS * 0.5;
    Vector2 {
        X: center,
        Y: center,
    }
}

fn inverse_circle_clip_radii(size: f32, hole: f32) -> Option<(f32, f32)> {
    (size.is_finite() && hole.is_finite() && size > 0.0 && hole >= 0.0 && hole < size)
        .then_some((size * 0.5, hole * 0.5))
}

unsafe fn make_inverse_circle_clip(
    compositor: &Compositor,
    d2d_factory: &ID2D1Factory,
    size: f32,
    hole: f32,
) -> windows::core::Result<CompositionGeometricClip> {
    make_inverse_circle_clip_at(
        compositor,
        d2d_factory,
        size,
        hole,
        Vector2 {
            X: size * 0.5,
            Y: size * 0.5,
        },
    )
}

unsafe fn make_inverse_circle_clip_at(
    compositor: &Compositor,
    d2d_factory: &ID2D1Factory,
    size: f32,
    hole: f32,
    center: Vector2,
) -> windows::core::Result<CompositionGeometricClip> {
    let (outer_radius, inner_radius) = inverse_circle_clip_radii(size, hole).ok_or_else(|| {
        Error::new(
            windows::core::HRESULT(0x80070057u32 as i32),
            "invalid ripple mask dimensions",
        )
    })?;
    let outer = d2d_factory.CreateEllipseGeometry(&D2D1_ELLIPSE {
        point: Vector2 {
            X: center.X,
            Y: center.Y,
        },
        radiusX: outer_radius,
        radiusY: outer_radius,
    })?;
    let inner = d2d_factory.CreateEllipseGeometry(&D2D1_ELLIPSE {
        point: Vector2 {
            X: center.X,
            Y: center.Y,
        },
        radiusX: inner_radius,
        radiusY: inner_radius,
    })?;
    let geometry = d2d_factory.CreatePathGeometry()?;
    let sink = geometry.Open()?;
    // This mask cuts a circle exactly at the Shazam button edge. The default
    // 0.25-DIP flattening tolerance can leave visible facets at 125% DPI when
    // the translucent listening ripples meet the button's translucent fill.
    outer.CombineWithGeometry(
        &inner,
        D2D1_COMBINE_MODE_EXCLUDE,
        None,
        INNER_MASK_GEOMETRY_TOLERANCE,
        &sink,
    )?;
    sink.Close()?;

    let geometry: ID2D1Geometry = geometry.cast()?;
    let source: IGeometrySource2D = D2DCompositionGeometrySource { geometry }.into();
    let path = CompositionPath::Create(&source)?;
    let geometry = compositor.CreatePathGeometryWithPath(&path)?;
    compositor.CreateGeometricClipWithGeometry(&geometry)
}

unsafe fn make_auto_shadow_surface(
    compositor: &Compositor,
    bounds: f32,
    brush: &CompositionColorBrush,
) -> windows::core::Result<(
    ShapeVisual,
    CompositionVisualSurface,
    CompositionSurfaceBrush,
)> {
    let raster_bounds = bounds * AUTO_SHADOW_RASTER_SCALE;
    let source = compositor.CreateShapeVisual()?;
    source.SetBorderMode(CompositionBorderMode::Soft)?;
    source.SetSize(Vector2 {
        X: raster_bounds,
        Y: raster_bounds,
    })?;
    let geometry: CompositionEllipseGeometry = compositor.CreateEllipseGeometry()?;
    geometry.SetCenter(Vector2 {
        X: raster_bounds * 0.5,
        Y: raster_bounds * 0.5,
    })?;
    geometry.SetRadius(Vector2 {
        X: raster_bounds * 0.5,
        Y: raster_bounds * 0.5,
    })?;
    let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
    shape.SetFillBrush(brush)?;
    source.Shapes()?.Append(&shape)?;

    let surface = compositor.CreateVisualSurface()?;
    surface.SetSourceVisual(&source)?;
    surface.SetSourceOffset(Vector2 { X: 0.0, Y: 0.0 })?;
    surface.SetSourceSize(Vector2 {
        X: raster_bounds,
        Y: raster_bounds,
    })?;
    let surface_brush = compositor.CreateSurfaceBrushWithSurface(&surface)?;
    surface_brush.SetStretch(CompositionStretch::Fill)?;
    surface_brush.SetBitmapInterpolationMode(CompositionBitmapInterpolationMode::Linear)?;
    Ok((source, surface, surface_brush))
}

struct CompositionScene {
    compositor: Compositor,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    tagging_root: ContainerVisual,
    result_overlay: CompositionResultOverlay,
    status_text: CompositionStatusText,
    auto_shadow_root: ContainerVisual,
    auto_shadow_mask: CompositionGeometricClip,
    auto_shadow_brush: CompositionColorBrush,
    _auto_shadow_source: ShapeVisual,
    _auto_shadow_surface: CompositionVisualSurface,
    auto_shadow: DropShadow,
    inner_audio_root: ContainerVisual,
    inner_wave_root: ContainerVisual,
    inner_inverse_clip: CompositionGeometricClip,
    outer_audio_root: ContainerVisual,
    rings: [CompositionRing; 6],
    button: ContainerVisual,
    button_visible: bool,
    result_fallback_occludes_button: bool,
    button_content: ContainerVisual,
    button_fill: CompositionColorBrush,
    logo: CompositionLogo,
    no_match_logo_transform: ContainerVisual,
    no_match_icon_transform: ContainerVisual,
    ring_brushes: [CompositionColorBrush; 3],
    idle_animation: Vector3KeyFrameAnimation,
    outer_scale_null_animation: Vector3KeyFrameAnimation,
    idle_active: bool,
    idle_started_at: Option<Instant>,
    last_button_scale: f32,
    ripples_active: bool,
    success_settle_active: bool,
    success_settle_completed: bool,
    success_track_id: Option<u64>,
    theme: Theme,
    body_top: f32,
    dpi: u32,
    no_match_active: bool,
    no_match_restore_active: bool,
}

#[derive(Clone, Copy)]
struct AnimationSnapshot {
    mode: TaggingMode,
    elapsed: f64,
    now: Instant,
    theme: Theme,
    body_top: f32,
    status_text: StatusTextSnapshot,
    show_button: bool,
    show_ripples: bool,
    levels: UiLevels,
    auto_spring: Option<ui_model::SpringProfile>,
    no_match_elapsed: Option<f64>,
    success_elapsed: Option<f64>,
    success_track_id: Option<u64>,
    success_wave_frame: Option<ui_model::WaveFrame>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StatusTextLabel {
    Ready,
    AutoStart,
    AutoOn,
    AutoTurnOff,
    Listening,
}

impl StatusTextLabel {
    fn text(self, language: UiLanguage) -> &'static str {
        let key = match self {
            Self::Ready => UiTextKey::Ready,
            Self::AutoStart => UiTextKey::AutoStartStatus,
            Self::AutoOn => UiTextKey::AutoOn,
            Self::AutoTurnOff => UiTextKey::AutoTurnOff,
            Self::Listening => UiTextKey::Listening,
        };
        localized_ui_text(key, language)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct StatusTextSnapshot {
    current: Option<StatusTextLabel>,
    previous: Option<StatusTextLabel>,
    transition_started_at: Option<Instant>,
}

fn status_text_snapshot(
    phase: &MainPhase,
    alternator: Option<StatusTextAlternator>,
) -> StatusTextSnapshot {
    let cycle = match phase {
        MainPhase::Ready | MainPhase::Error { .. } | MainPhase::Matched { .. } => {
            Some(StatusCycle::Ready)
        }
        MainPhase::Listening {
            mode: CaptureMode::Auto,
            ..
        }
        | MainPhase::Starting {
            mode: CaptureMode::Auto,
            ..
        } => Some(StatusCycle::Auto),
        MainPhase::Listening {
            mode: CaptureMode::Manual,
            ..
        } => {
            return StatusTextSnapshot {
                current: Some(StatusTextLabel::Listening),
                previous: None,
                transition_started_at: None,
            };
        }
        MainPhase::Starting {
            mode: CaptureMode::Manual,
            ..
        }
        | MainPhase::Cancelling { .. }
        | MainPhase::NoMatch { .. }
        | MainPhase::Offline { .. }
        | MainPhase::Timeout { .. } => None,
    };
    let Some(cycle) = cycle else {
        return StatusTextSnapshot::default();
    };
    let status = alternator.filter(|status| status.cycle == cycle);
    let first = status.is_none_or(|status| status.first);
    let (first_label, second_label) = match cycle {
        StatusCycle::Ready => (StatusTextLabel::Ready, StatusTextLabel::AutoStart),
        StatusCycle::Auto => (StatusTextLabel::AutoOn, StatusTextLabel::AutoTurnOff),
    };
    let current = if first { first_label } else { second_label };
    let transition_started_at = status.and_then(|status| status.transition_started_at);
    let previous = transition_started_at.map(|_| if first { second_label } else { first_label });
    StatusTextSnapshot {
        current: Some(current),
        previous,
        transition_started_at,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompositionRootLayer {
    AutoShadow,
    InnerRipples,
    Button,
    OuterRipples,
}

fn composition_root_layer_order() -> [CompositionRootLayer; 4] {
    // Insert back-to-front to match SHUTaggingLayer: the inner ripple group
    // sits below the Shazam button and the outer group is added above it.
    // The inner inverse clip follows the button scale so its fill cannot paint
    // over the button; the larger outer rings remain clear of its bounds.
    [
        CompositionRootLayer::AutoShadow,
        CompositionRootLayer::InnerRipples,
        CompositionRootLayer::Button,
        CompositionRootLayer::OuterRipples,
    ]
}

fn composition_outer_ripples_visible(mode: TaggingMode) -> bool {
    // Native Auto start adds its shadow and inner circles, but does not add
    // the outer circle group used by manual recognition.
    mode == TaggingMode::Manual
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompositionSceneLayer {
    Tagging,
    ResultOverlay,
}

fn composition_scene_layer_order() -> [CompositionSceneLayer; 2] {
    [
        CompositionSceneLayer::Tagging,
        CompositionSceneLayer::ResultOverlay,
    ]
}

const STATUS_TEXT_TOP: f32 = 141.0;
const STATUS_TEXT_HEIGHT: f32 = 19.0;
const STATUS_TEXT_TRANSITION_DURATION: Duration = Duration::from_millis(400);

struct CompositionStatusLabel {
    _surface: CompositionDrawingSurface,
    interop: ICompositionDrawingSurfaceInterop,
    _brush: CompositionSurfaceBrush,
    visual: SpriteVisual,
    size: D2D_SIZE_U,
    text: Option<String>,
}

impl CompositionStatusLabel {
    unsafe fn new(
        compositor: &Compositor,
        graphics_device: &CompositionGraphicsDevice,
        dpi: u32,
    ) -> windows::core::Result<Self> {
        let size = status_text_surface_size(dpi);
        let surface = graphics_device.CreateDrawingSurface(
            windows::Foundation::Size {
                Width: size.width as f32,
                Height: size.height as f32,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
        let brush = compositor.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        let visual = compositor.CreateSpriteVisual()?;
        visual.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: STATUS_TEXT_HEIGHT,
        })?;
        visual.SetBrush(&brush)?;
        visual.SetOpacity(0.0)?;
        Ok(Self {
            _surface: surface,
            interop,
            _brush: brush,
            visual,
            size,
            text: None,
        })
    }

    unsafe fn resize(&mut self, dpi: u32) -> windows::core::Result<()> {
        let size = status_text_surface_size(dpi);
        if size == self.size {
            return Ok(());
        }
        self.interop.Resize(SIZE {
            cx: size.width as i32,
            cy: size.height as i32,
        })?;
        self.size = size;
        self.text = None;
        Ok(())
    }

    unsafe fn set_text(
        &mut self,
        value: &str,
        format: &IDWriteTextFormat,
        dpi: u32,
    ) -> windows::core::Result<()> {
        if self.text.as_deref() == Some(value) {
            return Ok(());
        }
        let mut offset = POINT::default();
        let context = self
            .interop
            .BeginDraw::<ID2D1DeviceContext>(None, &mut offset)?;
        let draw_result = (|| -> windows::core::Result<()> {
            context.SetDpi(96.0, 96.0);
            context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            context.SetTransform(&Matrix3x2 {
                M11: 1.0,
                M22: 1.0,
                M31: offset.x as f32,
                M32: offset.y as f32,
                ..Default::default()
            });
            let transparent = d2d_color(0, 0, 0, 0.0);
            context.Clear(Some(&transparent));
            let white = d2d_color(255, 255, 255, 1.0);
            let brush = context.CreateSolidColorBrush(&white, None)?;
            let scale = composition_scale_for_dpi(dpi);
            let area = rect(
                8.0 * scale,
                0.0,
                (CONTENT_WIDTH as f32 - 8.0) * scale,
                STATUS_TEXT_HEIGHT * scale,
            );
            let content: Vec<u16> = value.encode_utf16().collect();
            context.DrawText(
                &content,
                format,
                &area,
                &brush,
                windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            Ok(())
        })();
        let end_result = self.interop.EndDraw();
        draw_result?;
        end_result?;
        self.text = Some(value.to_owned());
        Ok(())
    }
}

fn status_text_surface_size(dpi: u32) -> D2D_SIZE_U {
    let scale = composition_scale_for_dpi(dpi.max(96));
    D2D_SIZE_U {
        width: (CONTENT_WIDTH as f32 * scale).ceil() as u32,
        height: (STATUS_TEXT_HEIGHT * scale).ceil() as u32,
    }
}

struct CompositionStatusText {
    compositor: Compositor,
    root: ContainerVisual,
    labels: [CompositionStatusLabel; 2],
    write_factory: IDWriteFactory,
    format: IDWriteTextFormat,
    easing: CubicBezierEasingFunction,
    controllers: [Option<AnimationController>; 2],
    dpi: u32,
    language: UiLanguage,
    body_top: f32,
    last_state: Option<StatusTextSnapshot>,
}

impl CompositionStatusText {
    unsafe fn new(
        compositor: &Compositor,
        graphics_device: &CompositionGraphicsDevice,
        write_factory: IDWriteFactory,
        dpi: u32,
        body_top: f32,
    ) -> windows::core::Result<Self> {
        let dpi = dpi.max(96);
        let language = ui_language();
        let format = status_text_format(&write_factory, dpi, language)?;
        let labels = [
            CompositionStatusLabel::new(compositor, graphics_device, dpi)?,
            CompositionStatusLabel::new(compositor, graphics_device, dpi)?,
        ];
        let root = compositor.CreateContainerVisual()?;
        root.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: STATUS_TEXT_HEIGHT,
        })?;
        root.SetOffset(Vector3 {
            X: 0.0,
            Y: body_top + STATUS_TEXT_TOP,
            Z: 0.0,
        })?;
        // The two labels travel through this viewport during the push. A
        // Composition container does not clip its children to its own bounds
        // by default, so keep the outgoing label from painting above the line.
        let clip = compositor.CreateInsetClipWithInsets(0.0, 0.0, 0.0, 0.0)?;
        root.SetClip(&clip)?;
        root.SetOpacity(0.0)?;
        let children = root.Children()?;
        children.InsertAtTop(&labels[0].visual)?;
        children.InsertAtTop(&labels[1].visual)?;
        let easing = compositor.CreateCubicBezierEasingFunction(
            Vector2 { X: 0.42, Y: 0.0 },
            Vector2 { X: 0.58, Y: 1.0 },
        )?;
        Ok(Self {
            compositor: compositor.clone(),
            root,
            labels,
            write_factory,
            format,
            easing,
            controllers: [None, None],
            dpi,
            language,
            body_top,
            last_state: None,
        })
    }

    unsafe fn update(
        &mut self,
        state: StatusTextSnapshot,
        now: Instant,
        body_top: f32,
        dpi: u32,
    ) -> windows::core::Result<()> {
        let dpi = dpi.max(96);
        let language = ui_language();
        let dpi_changed = self.dpi != dpi;
        let language_changed = self.language != language;
        let configuration_changed = dpi_changed || language_changed;
        if dpi_changed {
            for label in &mut self.labels {
                label.resize(dpi)?;
            }
            self.dpi = dpi;
        }
        if configuration_changed {
            self.format = status_text_format(&self.write_factory, dpi, language)?;
            for label in &mut self.labels {
                label.text = None;
            }
        }
        self.language = language;
        if self.body_top != body_top {
            self.body_top = body_top;
            self.root.SetOffset(Vector3 {
                X: 0.0,
                Y: body_top + STATUS_TEXT_TOP,
                Z: 0.0,
            })?;
        }
        if !configuration_changed && self.last_state == Some(state) {
            return Ok(());
        }
        for label in &self.labels {
            label.visual.StopAnimation(&HSTRING::from("Offset.Y"))?;
        }
        self.controllers = [None, None];

        let Some(current) = state.current else {
            self.root.SetOpacity(0.0)?;
            self.labels[0].visual.SetOpacity(0.0)?;
            self.labels[1].visual.SetOpacity(0.0)?;
            self.labels[0].visual.SetOffset(Vector3::default())?;
            self.labels[1].visual.SetOffset(Vector3::default())?;
            self.last_state = Some(state);
            return Ok(());
        };

        let transition =
            state
                .previous
                .zip(state.transition_started_at)
                .filter(|(previous, started_at)| {
                    *previous != current
                        && now.saturating_duration_since(*started_at)
                            < STATUS_TEXT_TRANSITION_DURATION
                });
        if let Some((previous, started_at)) = transition {
            self.labels[0].set_text(previous.text(language), &self.format, dpi)?;
            self.labels[1].set_text(current.text(language), &self.format, dpi)?;
            self.labels[0].visual.SetOpacity(1.0)?;
            self.labels[1].visual.SetOpacity(1.0)?;
            self.labels[0].visual.SetOffset(Vector3::default())?;
            self.labels[1].visual.SetOffset(Vector3 {
                X: 0.0,
                Y: STATUS_TEXT_HEIGHT,
                Z: 0.0,
            })?;
            let progress = status_text_linear_progress(started_at, now);
            self.controllers = [
                Some(self.start_vertical_animation(0, 0.0, -STATUS_TEXT_HEIGHT, progress)?),
                Some(self.start_vertical_animation(1, STATUS_TEXT_HEIGHT, 0.0, progress)?),
            ];
        } else {
            self.labels[0].set_text(current.text(language), &self.format, dpi)?;
            self.labels[0].visual.SetOpacity(1.0)?;
            self.labels[1].visual.SetOpacity(0.0)?;
            self.labels[0].visual.SetOffset(Vector3::default())?;
            self.labels[1].visual.SetOffset(Vector3::default())?;
        }
        self.root.SetOpacity(1.0)?;
        self.last_state = Some(state);
        Ok(())
    }

    fn start_vertical_animation(
        &self,
        label_index: usize,
        from: f32,
        to: f32,
        progress: f32,
    ) -> windows::core::Result<AnimationController> {
        let animation = self.compositor.CreateScalarKeyFrameAnimation()?;
        animation.SetDuration(TimeSpan {
            Duration: STATUS_TEXT_TRANSITION_DURATION.as_nanos() as i64 / 100,
        })?;
        animation.InsertKeyFrameWithEasingFunction(0.0, from, &self.easing)?;
        animation.InsertKeyFrameWithEasingFunction(1.0, to, &self.easing)?;
        let controller = self.compositor.CreateAnimationController()?;
        self.labels[label_index]
            .visual
            .StartAnimationWithController(&HSTRING::from("Offset.Y"), &animation, &controller)?;
        controller.SetProgress(progress)?;
        Ok(controller)
    }

    unsafe fn resize(&mut self, dpi: u32) -> windows::core::Result<()> {
        for label in &mut self.labels {
            label.resize(dpi)?;
        }
        self.last_state = None;
        Ok(())
    }
}

fn status_text_format(
    factory: &IDWriteFactory,
    dpi: u32,
    language: UiLanguage,
) -> windows::core::Result<IDWriteTextFormat> {
    let scale = composition_scale_for_dpi(dpi.max(96));
    create_text_format_with_locale(
        factory,
        16.0 * scale,
        DWRITE_FONT_WEIGHT_BOLD,
        DWRITE_TEXT_ALIGNMENT_CENTER,
        directwrite_locale(language),
    )
}

fn result_overlay_occludes_shazam_button(panel_top: Option<f32>, body_top: f32) -> bool {
    let Some(panel_top) = panel_top else {
        return false;
    };
    let panel_bottom = panel_top + TOP_HEIGHT as f32;
    let button_top = body_top + BUTTON_TOP as f32;
    let button_bottom = button_top + BUTTON_SIZE as f32;
    panel_top < button_bottom && panel_bottom > button_top
}

fn should_start_startup_logo(kind: WindowKind, already_started: bool) -> bool {
    kind == WindowKind::Main && !already_started
}

impl CompositionScene {
    // Parameters mirror the existing native graphics context and popup
    // lifecycle; grouping them would obscure resource ownership.
    #[allow(clippy::too_many_arguments)]
    fn new(
        hwnd: HWND,
        d2d_factory: &ID2D1Factory,
        write_factory: &IDWriteFactory,
        compositor: &Compositor,
        theme: Theme,
        arrow_edge: ArrowEdge,
        dpi: u32,
        animate_startup_logo: bool,
    ) -> windows::core::Result<Self> {
        let interop: ICompositorDesktopInterop = compositor.cast()?;
        let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, true)? };
        let root = compositor.CreateContainerVisual()?;
        root.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: MAIN_WINDOW_HEIGHT as f32,
        })?;
        // UI-228 keeps a full-size scene root for the card overlay. Tagging
        // visuals retain their own header clip below; the result card is a
        // sibling so it can cover the Composition button and travel over rows.
        // Inherit soft edges through both groups after monitor-DPI scaling.
        root.SetBorderMode(CompositionBorderMode::Soft)?;
        let tagging_root = compositor.CreateContainerVisual()?;
        tagging_root.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: MAIN_WINDOW_HEIGHT as f32,
        })?;
        tagging_root.SetBorderMode(CompositionBorderMode::Soft)?;
        let dpi = dpi.max(96);
        let composition_scale = composition_scale_for_dpi(dpi);
        root.SetScale(Vector3 {
            X: composition_scale,
            Y: composition_scale,
            Z: 1.0,
        })?;
        let body_top = if arrow_edge == ArrowEdge::Top {
            POPUP_ARROW_HEIGHT as f32
        } else {
            0.0
        };
        root.SetOffset(composition_button_pixel_snap_offset(dpi, body_top))?;
        let clip_bottom = MAIN_WINDOW_HEIGHT as f32 - (body_top + TOP_HEIGHT as f32);
        let clip = compositor.CreateInsetClipWithInsets(0.0, 0.0, 0.0, clip_bottom.max(0.0))?;
        tagging_root.SetClip(&clip)?;

        let ring_color = composition_tagging_color(theme);
        let easing = compositor.CreateCubicBezierEasingFunction(
            Vector2 { X: 0.44, Y: 0.05 },
            Vector2 { X: 0.55, Y: 0.95 },
        )?;
        let mut rings = Vec::with_capacity(6);
        let mut ring_brushes = Vec::with_capacity(3);
        ring_brushes.push(compositor.CreateColorBrushWithColor(ring_color)?);
        let center_x = (BUTTON_X + BUTTON_SIZE * 0.5) as f32;
        let center_y = body_top + (BUTTON_TOP + BUTTON_SIZE * 0.5) as f32;
        // The inverse-mask canvas is 336pt, but AutoShadow itself follows the
        // 96pt button host bounds. Keep those coordinate spaces independent:
        // using the mask canvas as the shadow ellipse makes the Auto layer
        // appear 3.5x too large.
        let inner_bounds = INNER_MASK_BOUNDS;
        let auto_shadow_bounds = AUTO_SHADOW_DISPLAY_BOUNDS;
        let auto_shadow_root = compositor.CreateContainerVisual()?;
        auto_shadow_root.SetSize(Vector2 {
            X: auto_shadow_bounds,
            Y: auto_shadow_bounds,
        })?;
        auto_shadow_root.SetBorderMode(CompositionBorderMode::Soft)?;
        auto_shadow_root.SetCenterPoint(Vector3 {
            X: auto_shadow_bounds * 0.5,
            Y: auto_shadow_bounds * 0.5,
            Z: 0.0,
        })?;
        auto_shadow_root.SetOffset(Vector3 {
            X: center_x - auto_shadow_bounds * 0.5,
            Y: center_y - auto_shadow_bounds * 0.5,
            Z: 0.0,
        })?;
        auto_shadow_root.SetOpacity(0.0)?;
        // The mask keeps its native 336pt backing bounds, but its center is
        // aligned to the 96pt AutoShadow host. The host clips the mask's
        // oversized outer area, leaving only the button-scale edge reveal.
        let auto_shadow_mask = unsafe {
            make_inverse_circle_clip_at(
                compositor,
                d2d_factory,
                inner_bounds,
                BUTTON_CLEAR_DIAMETER,
                auto_shadow_host_center(),
            )?
        };
        auto_shadow_mask.SetCenterPoint(auto_shadow_host_center())?;
        auto_shadow_root.SetClip(&auto_shadow_mask)?;
        let auto_shadow_brush =
            compositor.CreateColorBrushWithColor(composition_auto_shadow_color(theme))?;
        let (auto_shadow_source, auto_shadow_surface, auto_shadow_surface_brush) = unsafe {
            make_auto_shadow_surface(compositor, auto_shadow_bounds, &auto_shadow_brush)?
        };
        let auto_shadow = compositor.CreateDropShadow()?;
        auto_shadow.SetBlurRadius(3.0)?;
        auto_shadow.SetOffset(Vector3::default())?;
        auto_shadow.SetOpacity(0.5)?;
        auto_shadow.SetColor(composition_color(255, 36, 36, 36))?;
        auto_shadow.SetSourcePolicy(
            windows::UI::Composition::CompositionDropShadowSourcePolicy::InheritFromVisualContent,
        )?;
        let auto_shadow_sprite = compositor.CreateSpriteVisual()?;
        auto_shadow_sprite.SetSize(Vector2 {
            X: auto_shadow_bounds,
            Y: auto_shadow_bounds,
        })?;
        auto_shadow_sprite.SetBorderMode(CompositionBorderMode::Soft)?;
        auto_shadow_sprite.SetBrush(&auto_shadow_surface_brush)?;
        auto_shadow_sprite.SetShadow(&auto_shadow)?;
        auto_shadow_root
            .Children()?
            .InsertAtTop(&auto_shadow_sprite)?;

        let inner_audio_root = compositor.CreateContainerVisual()?;
        inner_audio_root.SetSize(Vector2 {
            X: inner_bounds,
            Y: inner_bounds,
        })?;
        inner_audio_root.SetCenterPoint(Vector3 {
            X: inner_bounds * 0.5,
            Y: inner_bounds * 0.5,
            Z: 0.0,
        })?;
        inner_audio_root.SetOffset(Vector3 {
            X: center_x - inner_bounds * 0.5,
            Y: center_y - inner_bounds * 0.5,
            Z: 0.0,
        })?;
        inner_audio_root.SetOpacity(0.0)?;
        let inner_inverse_clip = unsafe {
            make_inverse_circle_clip(compositor, d2d_factory, inner_bounds, BUTTON_CLEAR_DIAMETER)?
        };
        // The fill waves follow the audio scale, while the clear hole follows
        // the button scale in the same animation update.
        inner_inverse_clip.SetCenterPoint(Vector2 {
            X: inner_bounds * 0.5,
            Y: inner_bounds * 0.5,
        })?;
        inner_audio_root.SetClip(&inner_inverse_clip)?;
        let inner_wave_root = compositor.CreateContainerVisual()?;
        inner_wave_root.SetSize(Vector2 {
            X: inner_bounds,
            Y: inner_bounds,
        })?;
        inner_wave_root.SetCenterPoint(Vector3 {
            X: inner_bounds * 0.5,
            Y: inner_bounds * 0.5,
            Z: 0.0,
        })?;
        inner_audio_root.Children()?.InsertAtTop(&inner_wave_root)?;
        let outer_bounds = ui_model::OUTER_DIAMETERS[0] as f32;
        let outer_audio_root = compositor.CreateContainerVisual()?;
        outer_audio_root.SetSize(Vector2 {
            X: outer_bounds,
            Y: outer_bounds,
        })?;
        outer_audio_root.SetCenterPoint(Vector3 {
            X: outer_bounds * 0.5,
            Y: outer_bounds * 0.5,
            Z: 0.0,
        })?;
        outer_audio_root.SetOffset(Vector3 {
            X: center_x - outer_bounds * 0.5,
            Y: center_y - outer_bounds * 0.5,
            Z: 0.0,
        })?;
        outer_audio_root.SetOpacity(0.0)?;
        #[allow(clippy::needless_range_loop)]
        // Scene indices also select paired native animation tracks.
        for index in 0..6 {
            let diameter = if index < 4 {
                ui_model::INNER_DIAMETERS[index] as f32
            } else {
                ui_model::OUTER_DIAMETERS[index - 4] as f32
            };
            let base_stroke_width = if index < 4 { 0.0 } else { 4.0 };
            let base_opacity = if index < 4 {
                1.0
            } else {
                OUTER_BASE_OPACITY[index - 4]
            };
            let brush = if index >= 4 {
                Some(compositor.CreateColorBrushWithColor(ring_color)?)
            } else {
                None
            };
            let visual = compositor.CreateContainerVisual()?;
            visual.SetSize(Vector2 {
                X: diameter,
                Y: diameter,
            })?;
            visual.SetCenterPoint(Vector3 {
                X: diameter * 0.5,
                Y: diameter * 0.5,
                Z: 0.0,
            })?;
            visual.SetOffset(Vector3 {
                X: if index < 4 {
                    (inner_bounds - diameter) * 0.5
                } else {
                    (outer_bounds - diameter) * 0.5
                },
                Y: if index < 4 {
                    (inner_bounds - diameter) * 0.5
                } else {
                    (outer_bounds - diameter) * 0.5
                },
                Z: 0.0,
            })?;
            visual.SetOpacity(0.0)?;

            let (geometry, shape, inner_fill_visual) = if index < 4 {
                let fill_geometry: CompositionEllipseGeometry =
                    compositor.CreateEllipseGeometry()?;
                fill_geometry.SetCenter(Vector2 {
                    X: diameter * 0.5,
                    Y: diameter * 0.5,
                })?;
                fill_geometry.SetRadius(Vector2 {
                    X: diameter * 0.5,
                    Y: diameter * 0.5,
                })?;
                let fill_shape = compositor.CreateSpriteShapeWithGeometry(&fill_geometry)?;
                fill_shape.SetFillBrush(&ring_brushes[0])?;
                let fill_visual = compositor.CreateShapeVisual()?;
                fill_visual.SetSize(Vector2 {
                    X: diameter,
                    Y: diameter,
                })?;
                fill_visual.Shapes()?.Append(&fill_shape)?;
                (None, None, Some(fill_visual))
            } else {
                let geometry: CompositionEllipseGeometry = compositor.CreateEllipseGeometry()?;
                geometry.SetCenter(Vector2 {
                    X: diameter * 0.5,
                    Y: diameter * 0.5,
                })?;
                geometry.SetRadius(Vector2 {
                    X: diameter * 0.5,
                    Y: diameter * 0.5,
                })?;
                let shape_visual = compositor.CreateShapeVisual()?;
                shape_visual.SetSize(Vector2 {
                    X: diameter,
                    Y: diameter,
                })?;
                shape_visual.SetCenterPoint(Vector3 {
                    X: diameter * 0.5,
                    Y: diameter * 0.5,
                    Z: 0.0,
                })?;
                let brush = brush.as_ref().expect("outer ring brush");
                let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
                shape.SetStrokeBrush(brush)?;
                shape.SetStrokeThickness(base_stroke_width)?;
                shape_visual.Shapes()?.Append(&shape)?;
                (Some(geometry), Some(shape), None)
            };
            let (core_container, sublayer_container) = if index < 4 {
                let core = compositor.CreateContainerVisual()?;
                core.SetSize(Vector2 {
                    X: diameter,
                    Y: diameter,
                })?;
                core.SetCenterPoint(Vector3 {
                    X: diameter * 0.5,
                    Y: diameter * 0.5,
                    Z: 0.0,
                })?;
                let sublayer = compositor.CreateContainerVisual()?;
                sublayer.SetSize(Vector2 {
                    X: diameter,
                    Y: diameter,
                })?;
                sublayer.SetCenterPoint(Vector3 {
                    X: diameter * 0.5,
                    Y: diameter * 0.5,
                    Z: 0.0,
                })?;
                sublayer
                    .Children()?
                    .InsertAtTop(inner_fill_visual.as_ref().expect("inner fill visual"))?;
                core.Children()?.InsertAtTop(&sublayer)?;
                visual.Children()?.InsertAtTop(&core)?;
                inner_wave_root.Children()?.InsertAtTop(&visual)?;
                (Some(core), Some(sublayer))
            } else {
                let container = compositor.CreateContainerVisual()?;
                container.SetSize(Vector2 {
                    X: diameter,
                    Y: diameter,
                })?;
                container.SetCenterPoint(Vector3 {
                    X: diameter * 0.5,
                    Y: diameter * 0.5,
                    Z: 0.0,
                })?;
                let shape_visual = compositor.CreateShapeVisual()?;
                shape_visual.SetSize(Vector2 {
                    X: diameter,
                    Y: diameter,
                })?;
                shape_visual.SetCenterPoint(Vector3 {
                    X: diameter * 0.5,
                    Y: diameter * 0.5,
                    Z: 0.0,
                })?;
                shape_visual
                    .Shapes()?
                    .Append(shape.as_ref().expect("outer ring shape"))?;
                container.Children()?.InsertAtTop(&shape_visual)?;
                visual.Children()?.InsertAtTop(&container)?;
                outer_audio_root.Children()?.InsertAtTop(&visual)?;
                (Some(container), None)
            };
            let inner_animations = if index < 4 {
                Some(make_inner_ring_animations(compositor, index)?)
            } else {
                None
            };
            let core_scale_animation = if index >= 4 {
                let delay = if index == 4 {
                    0.0
                } else {
                    ui_model::OUTER_SECOND_PHASE
                };
                let loop_duration = ui_model::OUTER_LOOP_DURATION;
                let core_duration = ui_model::OUTER_CORE_DURATION;
                let delay_progress = (delay / loop_duration) as f32;
                let peak_progress = ((delay + 0.75 * core_duration) / loop_duration) as f32;
                let core_end_progress = ((delay + core_duration) / loop_duration) as f32;

                let core_scale = compositor.CreateVector3KeyFrameAnimation()?;
                core_scale.SetDuration(TimeSpan {
                    Duration: 15_833_334,
                })?;
                core_scale.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
                core_scale.InsertKeyFrame(
                    0.0,
                    Vector3 {
                        X: 0.85,
                        Y: 0.85,
                        Z: 1.0,
                    },
                )?;
                if delay > 0.0 {
                    core_scale.InsertKeyFrame(
                        delay_progress,
                        Vector3 {
                            X: 0.85,
                            Y: 0.85,
                            Z: 1.0,
                        },
                    )?;
                }
                core_scale.InsertKeyFrameWithEasingFunction(
                    peak_progress,
                    Vector3 {
                        X: 0.9,
                        Y: 0.9,
                        Z: 1.0,
                    },
                    &easing,
                )?;
                if core_end_progress < 1.0 {
                    core_scale.InsertKeyFrame(
                        core_end_progress,
                        Vector3 {
                            X: 0.9,
                            Y: 0.9,
                            Z: 1.0,
                        },
                    )?;
                }
                core_scale.InsertKeyFrame(
                    1.0,
                    Vector3 {
                        X: 0.9,
                        Y: 0.9,
                        Z: 1.0,
                    },
                )?;
                Some(core_scale)
            } else {
                None
            };
            let geometry_animation = if index >= 4 {
                Some(create_vector2_track_animation(
                    compositor,
                    ui_model::delayed_outer_track(ui_model::OuterTrack::CoreSize, index - 4),
                    diameter * 0.5,
                    ui_model::OUTER_LOOP_DURATION,
                )?)
            } else {
                None
            };
            let border_animation = if index >= 4 {
                Some(create_scalar_track_animation(
                    compositor,
                    ui_model::delayed_outer_track(ui_model::OuterTrack::BorderWidth, index - 4),
                    ui_model::OUTER_LOOP_DURATION,
                )?)
            } else {
                None
            };
            if let Some(brush) = brush {
                ring_brushes.push(brush);
            }
            rings.push(CompositionRing {
                visual,
                geometry,
                shape,
                _inner_fill_visual: inner_fill_visual,
                core_container,
                sublayer_container,
                inner_animations,
                core_scale_animation,
                geometry_animation,
                border_animation,
                diameter,
                base_stroke_width,
                base_opacity,
            });
        }
        let rings: [CompositionRing; 6] = rings.try_into().expect("six composition ring visuals");
        let ring_brushes: [CompositionColorBrush; 3] = ring_brushes
            .try_into()
            .expect("one inner and two outer composition ring brushes");

        let button = compositor.CreateContainerVisual()?;
        button.SetSize(Vector2 {
            X: BUTTON_SIZE as f32,
            Y: BUTTON_SIZE as f32,
        })?;
        button.SetCenterPoint(Vector3 {
            X: BUTTON_SIZE as f32 * 0.5,
            Y: BUTTON_SIZE as f32 * 0.5,
            Z: 0.0,
        })?;
        button.SetOffset(Vector3 {
            X: BUTTON_X as f32,
            Y: body_top + BUTTON_TOP as f32,
            Z: 0.0,
        })?;
        let button_shape_visual = compositor.CreateShapeVisual()?;
        button_shape_visual.SetSize(Vector2 {
            X: BUTTON_SIZE as f32,
            Y: BUTTON_SIZE as f32,
        })?;
        button_shape_visual.SetCenterPoint(Vector3 {
            X: BUTTON_SIZE as f32 * 0.5,
            Y: BUTTON_SIZE as f32 * 0.5,
            Z: 0.0,
        })?;
        let button_fill =
            compositor.CreateColorBrushWithColor(composition_color(77, 255, 255, 255))?;
        let button_edge =
            compositor.CreateColorBrushWithColor(composition_color(51, 255, 255, 255))?;
        let button_geometry: CompositionEllipseGeometry = compositor.CreateEllipseGeometry()?;
        button_geometry.SetCenter(Vector2 {
            X: BUTTON_SIZE as f32 * 0.5,
            Y: BUTTON_SIZE as f32 * 0.5,
        })?;
        button_geometry.SetRadius(Vector2 {
            X: BUTTON_SIZE as f32 * 0.5,
            Y: BUTTON_SIZE as f32 * 0.5,
        })?;
        let button_shape = compositor.CreateSpriteShapeWithGeometry(&button_geometry)?;
        button_shape.SetFillBrush(&button_fill)?;
        button_shape_visual.Shapes()?.Append(&button_shape)?;
        let button_border_geometry =
            unsafe { make_button_border_arc_geometry(compositor, d2d_factory)? };
        let button_border_shape =
            compositor.CreateSpriteShapeWithGeometry(&button_border_geometry)?;
        button_border_shape.SetStrokeBrush(&button_edge)?;
        button_border_shape.SetStrokeThickness(1.0)?;
        button_shape_visual.Shapes()?.Append(&button_border_shape)?;
        let logo =
            unsafe { CompositionLogo::new(d2d_factory, compositor, theme, animate_startup_logo)? };
        let no_match_logo_transform = compositor.CreateContainerVisual()?;
        no_match_logo_transform.SetSize(Vector2 {
            X: BUTTON_SIZE as f32,
            Y: BUTTON_SIZE as f32,
        })?;
        no_match_logo_transform.SetCenterPoint(Vector3 {
            X: BUTTON_SIZE as f32 * 0.5,
            Y: BUTTON_SIZE as f32 * 0.5,
            Z: 0.0,
        })?;
        no_match_logo_transform.SetScale(Vector3 {
            X: 1.0,
            Y: 1.0,
            Z: 1.0,
        })?;
        no_match_logo_transform.SetOpacity(1.0)?;
        no_match_logo_transform
            .Children()?
            .InsertAtTop(&logo.visual)?;
        let button_content = compositor.CreateContainerVisual()?;
        button_content.SetSize(Vector2 {
            X: BUTTON_SIZE as f32,
            Y: BUTTON_SIZE as f32,
        })?;
        button_content.SetCenterPoint(Vector3 {
            X: BUTTON_SIZE as f32 * 0.5,
            Y: BUTTON_SIZE as f32 * 0.5,
            Z: 0.0,
        })?;
        // UI-168 records the exact independent NoMatch mask outline. Keep it
        // as a separate vector shape so its native position spring does not
        // alter the regular Shazam logo geometry.
        let no_match_icon_transform = compositor.CreateContainerVisual()?;
        no_match_icon_transform.SetSize(Vector2 {
            X: BUTTON_SIZE as f32,
            Y: BUTTON_SIZE as f32,
        })?;
        no_match_icon_transform.SetCenterPoint(Vector3 {
            X: BUTTON_SIZE as f32 * 0.5,
            Y: BUTTON_SIZE as f32 * 0.5,
            Z: 0.0,
        })?;
        no_match_icon_transform.SetOpacity(0.0)?;
        let no_match_icon_brush =
            compositor.CreateColorBrushWithColor(composition_color(255, 255, 255, 255))?;
        let icon_rect = no_match_icon_rect(BUTTON_SIZE as f32);
        let no_match_icon_geometry =
            unsafe { make_no_match_icon_geometry(compositor, d2d_factory, icon_rect)? };
        let no_match_icon_shape =
            compositor.CreateSpriteShapeWithGeometry(&no_match_icon_geometry)?;
        no_match_icon_shape.SetFillBrush(&no_match_icon_brush)?;
        let no_match_icon_visual = compositor.CreateShapeVisual()?;
        let no_match_icon_width = icon_rect[2] - icon_rect[0];
        let no_match_icon_height = icon_rect[3] - icon_rect[1];
        no_match_icon_visual.SetSize(Vector2 {
            X: no_match_icon_width,
            Y: no_match_icon_height,
        })?;
        no_match_icon_visual.SetOffset(Vector3 {
            X: icon_rect[0],
            Y: icon_rect[1],
            Z: 0.0,
        })?;
        no_match_icon_visual
            .Shapes()?
            .Append(&no_match_icon_shape)?;
        no_match_icon_transform
            .Children()?
            .InsertAtTop(&no_match_icon_visual)?;

        button_content
            .Children()?
            .InsertAtTop(&button_shape_visual)?;
        button_content
            .Children()?
            .InsertAtTop(&no_match_logo_transform)?;
        button_content
            .Children()?
            .InsertAtTop(&no_match_icon_transform)?;
        button.Children()?.InsertAtTop(&button_content)?;

        let children = tagging_root.Children()?;
        for layer in composition_root_layer_order() {
            match layer {
                CompositionRootLayer::AutoShadow => children.InsertAtTop(&auto_shadow_root)?,
                CompositionRootLayer::InnerRipples => children.InsertAtTop(&inner_audio_root)?,
                CompositionRootLayer::Button => children.InsertAtTop(&button)?,
                CompositionRootLayer::OuterRipples => children.InsertAtTop(&outer_audio_root)?,
            }
        }

        let result_overlay = unsafe { CompositionResultOverlay::new(compositor, dpi, body_top)? };
        let status_text = unsafe {
            CompositionStatusText::new(
                compositor,
                &result_overlay._graphics_device,
                write_factory.clone(),
                dpi,
                body_top,
            )?
        };
        tagging_root.Children()?.InsertAtTop(&status_text.root)?;
        let root_children = root.Children()?;
        for layer in composition_scene_layer_order() {
            match layer {
                CompositionSceneLayer::Tagging => root_children.InsertAtTop(&tagging_root)?,
                CompositionSceneLayer::ResultOverlay => {
                    root_children.InsertAtTop(&result_overlay.viewport)?
                }
            }
        }

        let idle_animation = compositor.CreateVector3KeyFrameAnimation()?;
        idle_animation.SetDuration(TimeSpan {
            Duration: 20_000_000,
        })?;
        idle_animation.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
        idle_animation.InsertKeyFrame(
            0.0,
            Vector3 {
                X: 1.0,
                Y: 1.0,
                Z: 1.0,
            },
        )?;
        idle_animation.InsertKeyFrameWithEasingFunction(
            0.5,
            Vector3 {
                X: 1.06,
                Y: 1.06,
                Z: 1.0,
            },
            &easing,
        )?;
        idle_animation.InsertKeyFrameWithEasingFunction(
            1.0,
            Vector3 {
                X: 1.0,
                Y: 1.0,
                Z: 1.0,
            },
            &easing,
        )?;
        let outer_scale_null_animation = compositor.CreateVector3KeyFrameAnimation()?;
        outer_scale_null_animation.SetDuration(TimeSpan {
            // Windows.Foundation.TimeSpan uses 100ns ticks.
            Duration: 15_833_334,
        })?;
        outer_scale_null_animation.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
        outer_scale_null_animation.InsertKeyFrame(
            0.0,
            Vector3 {
                X: 1.0,
                Y: 1.0,
                Z: 1.0,
            },
        )?;
        outer_scale_null_animation.InsertKeyFrame(
            0.657_894_73,
            Vector3 {
                X: 1.0,
                Y: 1.0,
                Z: 1.0,
            },
        )?;
        outer_scale_null_animation.InsertKeyFrameWithEasingFunction(
            1.0,
            Vector3 {
                X: 0.93,
                Y: 0.93,
                Z: 1.0,
            },
            &easing,
        )?;
        target.SetRoot(&root)?;

        Ok(Self {
            compositor: compositor.clone(),
            _target: target,
            root,
            tagging_root,
            result_overlay,
            status_text,
            auto_shadow_root,
            auto_shadow_mask,
            auto_shadow_brush,
            _auto_shadow_source: auto_shadow_source,
            _auto_shadow_surface: auto_shadow_surface,
            auto_shadow,
            inner_audio_root,
            inner_wave_root,
            inner_inverse_clip,
            outer_audio_root,
            rings,
            button,
            button_visible: true,
            result_fallback_occludes_button: false,
            button_content,
            button_fill,
            logo,
            no_match_logo_transform,
            no_match_icon_transform,
            ring_brushes,
            idle_animation,
            outer_scale_null_animation,
            idle_active: false,
            idle_started_at: None,
            last_button_scale: 1.0,
            ripples_active: false,
            success_settle_active: false,
            success_settle_completed: false,
            success_track_id: None,
            theme,
            body_top,
            dpi,
            no_match_active: false,
            no_match_restore_active: false,
        })
    }

    fn set_dpi(&mut self, dpi: u32) -> windows::core::Result<()> {
        let dpi = dpi.max(96);
        if self.dpi == dpi {
            return Ok(());
        }
        self.dpi = dpi;
        let scale = composition_scale_for_dpi(dpi);
        self.root.SetScale(Vector3 {
            X: scale,
            Y: scale,
            Z: 1.0,
        })?;
        self.root
            .SetOffset(composition_button_pixel_snap_offset(dpi, self.body_top))?;
        unsafe {
            self.result_overlay.resize(dpi)?;
            self.result_overlay.set_viewport(dpi, self.body_top)?;
            self.status_text.resize(dpi)
        }
    }

    // Explicit source/capture/motion parameters keep the GPU overlay
    // updates independent of audio workers and view state.
    #[allow(clippy::too_many_arguments)]
    unsafe fn update_result_overlay(
        &mut self,
        source: Option<&LayeredSurface>,
        capture_top: Option<f32>,
        panel_top: Option<f32>,
        body_top: f32,
        dpi: u32,
        motion: Option<&ResultOverlayMotionAnimation>,
        motion_changed: bool,
    ) -> windows::core::Result<()> {
        if let (Some(source), Some(capture_top)) = (source, capture_top) {
            self.result_overlay
                .update_from_layered(source, capture_top, body_top, dpi)?;
        } else {
            self.result_overlay.resize(dpi)?;
        }
        match (source, panel_top, motion) {
            (_, None, _) | (None, Some(_), _) => self.result_overlay.hide(),
            (Some(_), Some(_), Some(motion)) if motion_changed => self
                .result_overlay
                .animate_position(&self.compositor, motion, body_top, dpi),
            (Some(_), Some(_), Some(_)) => Ok(()),
            (Some(_), Some(panel_top), None) if motion_changed || capture_top.is_some() => {
                self.result_overlay.set_position(panel_top, body_top, dpi)
            }
            (Some(_), Some(_), None) => Ok(()),
        }
    }

    fn set_result_fallback_occlusion(
        &mut self,
        occludes_button: bool,
    ) -> windows::core::Result<()> {
        self.result_fallback_occludes_button = occludes_button;
        self.button
            .SetOpacity(if self.button_visible && !occludes_button {
                1.0
            } else {
                0.0
            })
    }

    fn update(&mut self, snapshot: AnimationSnapshot) -> windows::core::Result<()> {
        if snapshot.success_track_id != self.success_track_id {
            if self.success_settle_active {
                self.stop_success_settle_tracks()?;
            }
            self.success_track_id = snapshot.success_track_id;
            self.success_settle_active = false;
            self.success_settle_completed = false;
        }
        if let Some(elapsed) = snapshot.success_elapsed {
            if elapsed < SUCCESS_SETTLE_TOTAL.as_secs_f64()
                && !self.success_settle_active
                && !self.success_settle_completed
            {
                self.start_success_settle(snapshot.success_wave_frame)?;
            }
            if elapsed >= SUCCESS_SETTLE_TOTAL.as_secs_f64() && !self.success_settle_completed {
                self.finish_success_settle()?;
            }
        } else {
            self.success_settle_active = false;
            self.success_settle_completed = false;
        }

        if self.theme != snapshot.theme {
            self.theme = snapshot.theme;
            self.logo.set_theme(self.theme)?;
            for brush in &self.ring_brushes {
                brush.SetColor(composition_tagging_color(self.theme))?;
            }
            self.auto_shadow_brush
                .SetColor(composition_auto_shadow_color(self.theme))?;
            self.auto_shadow
                .SetColor(composition_color(255, 36, 36, 36))?;
            self.button_fill
                .SetColor(composition_color(77, 255, 255, 255))?;
        }

        if self.body_top != snapshot.body_top {
            self.body_top = snapshot.body_top;
            self.root.SetOffset(composition_button_pixel_snap_offset(
                self.dpi,
                self.body_top,
            ))?;
            let bottom_inset = MAIN_WINDOW_HEIGHT as f32 - (snapshot.body_top + TOP_HEIGHT as f32);
            let clip =
                self.compositor
                    .CreateInsetClipWithInsets(0.0, 0.0, 0.0, bottom_inset.max(0.0))?;
            self.tagging_root.SetClip(&clip)?;
            self.result_overlay.set_viewport(self.dpi, self.body_top)?;
            let inner_bounds = INNER_MASK_BOUNDS;
            let auto_shadow_bounds = AUTO_SHADOW_DISPLAY_BOUNDS;
            let outer_bounds = ui_model::OUTER_DIAMETERS[0] as f32;
            let center_x = (BUTTON_X + BUTTON_SIZE * 0.5) as f32;
            let center_y = snapshot.body_top + (BUTTON_TOP + BUTTON_SIZE * 0.5) as f32;
            self.auto_shadow_root.SetOffset(Vector3 {
                X: center_x - auto_shadow_bounds * 0.5,
                Y: center_y - auto_shadow_bounds * 0.5,
                Z: 0.0,
            })?;
            self.inner_audio_root.SetOffset(Vector3 {
                X: center_x - inner_bounds * 0.5,
                Y: center_y - inner_bounds * 0.5,
                Z: 0.0,
            })?;
            self.outer_audio_root.SetOffset(Vector3 {
                X: center_x - outer_bounds * 0.5,
                Y: center_y - outer_bounds * 0.5,
                Z: 0.0,
            })?;
        }

        if snapshot.no_match_elapsed.is_some() && !self.no_match_restore_active {
            let from_scale = if self.idle_active {
                self.idle_started_at
                    .map(|started_at| {
                        ui_model::idle_pulse_at(started_at.elapsed().as_secs_f64()) as f32
                    })
                    .unwrap_or(self.last_button_scale)
            } else {
                self.last_button_scale
            };
            if self.idle_active {
                self.button.StopAnimation(&HSTRING::from("Scale"))?;
                self.idle_active = false;
                self.idle_started_at = None;
            }
            self.start_no_match_idle_restore(from_scale)?;
            self.no_match_restore_active = true;
        }

        if snapshot
            .no_match_elapsed
            .is_some_and(|elapsed| elapsed >= ui_model::NO_MATCH_DISPATCH_DELAY_SECONDS)
            && !self.no_match_active
        {
            self.start_no_match_motion(snapshot.no_match_elapsed.unwrap_or_default())?;
            self.no_match_active = true;
        } else if snapshot.no_match_elapsed.is_none()
            && (self.no_match_active || self.no_match_restore_active)
        {
            self.button.StopAnimation(&HSTRING::from("Offset"))?;
            self.button.StopAnimation(&HSTRING::from("Scale"))?;
            self.button_content.StopAnimation(&HSTRING::from("Scale"))?;
            self.no_match_logo_transform
                .StopAnimation(&HSTRING::from("Opacity"))?;
            self.no_match_logo_transform
                .StopAnimation(&HSTRING::from("Scale"))?;
            self.no_match_icon_transform
                .StopAnimation(&HSTRING::from("Offset"))?;
            self.no_match_icon_transform
                .StopAnimation(&HSTRING::from("Opacity"))?;
            self.button.SetScale(Vector3 {
                X: 1.0,
                Y: 1.0,
                Z: 1.0,
            })?;
            self.button_content.SetScale(Vector3 {
                X: 1.0,
                Y: 1.0,
                Z: 1.0,
            })?;
            self.no_match_logo_transform.SetOpacity(1.0)?;
            self.no_match_logo_transform.SetScale(Vector3 {
                X: 1.0,
                Y: 1.0,
                Z: 1.0,
            })?;
            self.no_match_icon_transform.SetOffset(Vector3::default())?;
            self.no_match_icon_transform.SetOpacity(0.0)?;
            self.no_match_active = false;
            self.no_match_restore_active = false;
            self.last_button_scale = 1.0;
        }
        if !self.no_match_active {
            self.button.SetOffset(Vector3 {
                X: BUTTON_X as f32,
                Y: snapshot.body_top + BUTTON_TOP as f32,
                Z: 0.0,
            })?;
        }
        self.button_visible = snapshot.show_button;
        self.button.SetOpacity(
            if snapshot.show_button && !self.result_fallback_occludes_button {
                1.0
            } else {
                0.0
            },
        )?;

        if snapshot.show_ripples && !self.ripples_active {
            for ring in &self.rings[..4] {
                ring.visual.SetOpacity(1.0)?;
                ring.visual.StartAnimation(
                    &HSTRING::from("Scale"),
                    &ring
                        .inner_animations
                        .as_ref()
                        .expect("inner ring animation tracks")
                        .scale_null,
                )?;
                let animations = ring
                    .inner_animations
                    .as_ref()
                    .expect("inner ring animation tracks");
                if let Some(container) = &ring.core_container {
                    container.StartAnimation(&HSTRING::from("Scale"), &animations.core_scale)?;
                    container
                        .StartAnimation(&HSTRING::from("Opacity"), &animations.core_opacity)?;
                }
                if let Some(sublayer) = &ring.sublayer_container {
                    sublayer.StartAnimation(&HSTRING::from("Scale"), &animations.sublayer_scale)?;
                }
            }
            if composition_outer_ripples_visible(snapshot.mode) {
                for ring in &self.rings[4..] {
                    ring.visual.SetOpacity(ring.base_opacity)?;
                    ring.visual.StartAnimation(
                        &HSTRING::from("Scale"),
                        &self.outer_scale_null_animation,
                    )?;
                    if let (Some(container), Some(scale)) = (
                        ring.core_container.as_ref(),
                        ring.core_scale_animation.as_ref(),
                    ) {
                        container.StartAnimation(&HSTRING::from("Scale"), scale)?;
                    }
                    if let (Some(size), Some(border)) = (
                        ring.geometry_animation.as_ref(),
                        ring.border_animation.as_ref(),
                    ) {
                        ring.geometry
                            .as_ref()
                            .expect("outer ring geometry")
                            .StartAnimation(&HSTRING::from("Radius"), size)?;
                        ring.shape
                            .as_ref()
                            .expect("outer ring shape")
                            .StartAnimation(&HSTRING::from("StrokeThickness"), border)?;
                    }
                }
            } else {
                self.stop_outer_ripple_tracks()?;
                for ring in &self.rings[4..] {
                    ring.visual.SetOpacity(0.0)?;
                }
            }
            self.ripples_active = true;
        } else if !snapshot.show_ripples
            && self.ripples_active
            && snapshot.success_elapsed.is_none()
        {
            for ring in &self.rings[..4] {
                ring.visual.StopAnimation(&HSTRING::from("Scale"))?;
                if let Some(container) = &ring.core_container {
                    container.StopAnimation(&HSTRING::from("Scale"))?;
                    container.StopAnimation(&HSTRING::from("Opacity"))?;
                }
                if let Some(sublayer) = &ring.sublayer_container {
                    sublayer.StopAnimation(&HSTRING::from("Scale"))?;
                }
                ring.visual.SetScale(Vector3 {
                    X: 1.0,
                    Y: 1.0,
                    Z: 1.0,
                })?;
            }
            self.stop_outer_ripple_tracks()?;
            for ring in &self.rings[4..] {
                ring.visual.SetOpacity(0.0)?;
            }
            self.ripples_active = false;
        }

        if snapshot.show_ripples {
            if self.idle_active {
                self.button.StopAnimation(&HSTRING::from("Scale"))?;
                self.idle_active = false;
                self.idle_started_at = None;
            }
            let frame = ui_model::wave_frame_at_with_auto_spring(
                snapshot.mode,
                snapshot.elapsed,
                snapshot.levels,
                snapshot.auto_spring,
            );
            self.auto_shadow_root
                .SetOpacity(if snapshot.mode == TaggingMode::Auto {
                    1.0
                } else {
                    0.0
                })?;
            self.auto_shadow_root.SetScale(Vector3 {
                X: frame.auto_shadow_scale as f32,
                Y: frame.auto_shadow_scale as f32,
                Z: 1.0,
            })?;
            self.auto_shadow_mask.SetScale(Vector2 {
                X: frame.button_child_scale as f32,
                Y: frame.button_child_scale as f32,
            })?;
            self.button.SetScale(Vector3 {
                X: frame.button_child_scale as f32,
                Y: frame.button_child_scale as f32,
                Z: 1.0,
            })?;
            self.inner_wave_root.SetScale(Vector3 {
                X: (frame.inner_container_factor / 3.0 * frame.inner_audio_scale) as f32,
                Y: (frame.inner_container_factor / 3.0 * frame.inner_audio_scale) as f32,
                Z: 1.0,
            })?;
            self.inner_inverse_clip.SetScale(Vector2 {
                X: frame.button_child_scale as f32,
                Y: frame.button_child_scale as f32,
            })?;
            self.inner_audio_root
                .SetOpacity(frame.inner_audio_opacity as f32)?;
            self.outer_audio_root.SetScale(Vector3 {
                X: frame.outer_audio_scale as f32,
                Y: frame.outer_audio_scale as f32,
                Z: 1.0,
            })?;
            self.outer_audio_root.SetOpacity(
                if composition_outer_ripples_visible(snapshot.mode) {
                    frame.outer_audio_opacity as f32
                } else {
                    0.0
                },
            )?;
        } else if snapshot.success_elapsed.is_none() {
            // Do not write static values over the active success tracks. Once
            // the result leaves this state, restore the ready scene in one
            // update and release the Auto shadow layer.
            self.auto_shadow_root
                .StopAnimation(&HSTRING::from("Scale"))?;
            self.auto_shadow_mask
                .StopAnimation(&HSTRING::from("Scale"))?;
            self.auto_shadow_root.SetScale(Vector3 {
                X: 1.0,
                Y: 1.0,
                Z: 1.0,
            })?;
            self.auto_shadow_mask.SetScale(Vector2 { X: 1.0, Y: 1.0 })?;
            self.auto_shadow_root.SetOpacity(0.0)?;
            self.inner_audio_root.SetOpacity(0.0)?;
            self.inner_wave_root.SetScale(Vector3 {
                X: 1.0,
                Y: 1.0,
                Z: 1.0,
            })?;
            self.inner_inverse_clip
                .SetScale(Vector2 { X: 1.0, Y: 1.0 })?;
            self.button.SetScale(Vector3 {
                X: 1.0,
                Y: 1.0,
                Z: 1.0,
            })?;
            self.outer_audio_root.SetOpacity(0.0)?;
            self.outer_audio_root.SetScale(Vector3 {
                X: 1.0,
                Y: 1.0,
                Z: 1.0,
            })?;
            for ring in &self.rings {
                ring.visual.SetOpacity(0.0)?;
                if let Some(shape) = &ring.shape {
                    shape.SetStrokeThickness(ring.base_stroke_width)?;
                }
                if let Some(geometry) = &ring.geometry {
                    geometry.SetRadius(Vector2 {
                        X: ring.diameter * 0.5,
                        Y: ring.diameter * 0.5,
                    })?;
                }
                if let Some(container) = &ring.core_container {
                    container.SetScale(Vector3 {
                        X: 1.0,
                        Y: 1.0,
                        Z: 1.0,
                    })?;
                    container.SetOpacity(if ring.inner_animations.is_some() {
                        0.0
                    } else {
                        1.0
                    })?;
                }
                if let Some(sublayer) = &ring.sublayer_container {
                    sublayer.SetScale(Vector3 {
                        X: 1.0,
                        Y: 1.0,
                        Z: 1.0,
                    })?;
                }
            }
        }

        let logo_finished = self.logo.advance()?;
        if snapshot.mode == TaggingMode::Ready {
            let success_allows_idle = snapshot
                .success_elapsed
                .is_none_or(|elapsed| elapsed >= SUCCESS_SETTLE_TOTAL.as_secs_f64());
            if snapshot.show_button && logo_finished && success_allows_idle && !self.idle_active {
                self.button.SetScale(Vector3 {
                    X: 1.0,
                    Y: 1.0,
                    Z: 1.0,
                })?;
                self.button
                    .StartAnimation(&HSTRING::from("Scale"), &self.idle_animation)?;
                self.idle_active = true;
                self.idle_started_at = Some(Instant::now());
            } else if (!snapshot.show_button || !logo_finished || !success_allows_idle)
                && self.idle_active
            {
                self.button.StopAnimation(&HSTRING::from("Scale"))?;
                self.idle_active = false;
                self.idle_started_at = None;
            }
        } else {
            if self.idle_active {
                self.button.StopAnimation(&HSTRING::from("Scale"))?;
                self.idle_active = false;
                self.idle_started_at = None;
            }
            let frame = ui_model::wave_frame_at_with_auto_spring(
                snapshot.mode,
                snapshot.elapsed,
                snapshot.levels,
                snapshot.auto_spring,
            );
            self.last_button_scale = frame.button_child_scale as f32;
        }
        unsafe {
            self.status_text.update(
                snapshot.status_text,
                snapshot.now,
                snapshot.body_top,
                self.dpi,
            )?;
        }
        Ok(())
    }

    fn start_success_settle(
        &mut self,
        frame: Option<ui_model::WaveFrame>,
    ) -> windows::core::Result<()> {
        let frame = frame.unwrap_or_else(|| {
            ui_model::wave_frame_at_with_auto_spring(
                TaggingMode::Manual,
                0.0,
                UiLevels::default(),
                None,
            )
        });
        if self.idle_active {
            self.button.StopAnimation(&HSTRING::from("Scale"))?;
            self.idle_active = false;
            self.idle_started_at = None;
        }
        self.stop_ripple_child_tracks()?;

        let ease_out = self.compositor.CreateCubicBezierEasingFunction(
            Vector2 { X: 0.0, Y: 0.0 },
            Vector2 { X: 0.58, Y: 1.0 },
        )?;
        let ease_in = self.compositor.CreateCubicBezierEasingFunction(
            Vector2 { X: 0.42, Y: 0.0 },
            Vector2 { X: 1.0, Y: 1.0 },
        )?;
        let button_scale = create_scale_transition(
            &self.compositor,
            frame.button_child_scale as f32,
            1.0,
            Duration::from_millis(350),
            &ease_out,
        )?;
        let inner_scale = create_scale_transition(
            &self.compositor,
            (frame.inner_container_factor / 3.0 * frame.inner_audio_scale) as f32,
            0.3,
            Duration::from_millis(350),
            &ease_in,
        )?;
        let button_mask_scale = create_vector2_scale_transition(
            &self.compositor,
            frame.button_child_scale as f32,
            1.0,
            Duration::from_millis(350),
            &ease_out,
        )?;
        let auto_shadow_scale = create_scale_transition(
            &self.compositor,
            frame.auto_shadow_scale as f32,
            0.9,
            Duration::from_secs(1),
            &ease_in,
        )?;
        let auto_shadow_mask_scale = create_vector2_scale_transition(
            &self.compositor,
            frame.auto_shadow_scale as f32,
            0.9,
            Duration::from_secs(1),
            &ease_in,
        )?;
        let outer_opacity = create_scalar_transition(
            &self.compositor,
            frame.outer_audio_opacity as f32,
            0.0,
            Duration::from_millis(500),
            &ease_out,
        )?;
        self.button
            .StartAnimation(&HSTRING::from("Scale"), &button_scale)?;
        self.inner_inverse_clip
            .StartAnimation(&HSTRING::from("Scale"), &button_mask_scale)?;
        self.inner_wave_root
            .StartAnimation(&HSTRING::from("Scale"), &inner_scale)?;
        self.outer_audio_root
            .StartAnimation(&HSTRING::from("Opacity"), &outer_opacity)?;
        self.auto_shadow_root
            .StartAnimation(&HSTRING::from("Scale"), &auto_shadow_scale)?;
        self.auto_shadow_mask
            .StartAnimation(&HSTRING::from("Scale"), &auto_shadow_mask_scale)?;
        self.success_settle_active = true;
        Ok(())
    }

    fn stop_ripple_child_tracks(&self) -> windows::core::Result<()> {
        for ring in &self.rings[..4] {
            ring.visual.StopAnimation(&HSTRING::from("Scale"))?;
            if let Some(core) = &ring.core_container {
                core.StopAnimation(&HSTRING::from("Scale"))?;
                core.StopAnimation(&HSTRING::from("Opacity"))?;
            }
            if let Some(sublayer) = &ring.sublayer_container {
                sublayer.StopAnimation(&HSTRING::from("Scale"))?;
            }
        }
        for ring in &self.rings[4..] {
            ring.visual.StopAnimation(&HSTRING::from("Scale"))?;
            if let Some(core) = &ring.core_container {
                core.StopAnimation(&HSTRING::from("Scale"))?;
            }
            if let Some(geometry) = &ring.geometry {
                geometry.StopAnimation(&HSTRING::from("Radius"))?;
            }
            if let Some(shape) = &ring.shape {
                shape.StopAnimation(&HSTRING::from("StrokeThickness"))?;
            }
        }
        Ok(())
    }

    fn stop_outer_ripple_tracks(&self) -> windows::core::Result<()> {
        for ring in &self.rings[4..] {
            ring.visual.StopAnimation(&HSTRING::from("Scale"))?;
            if let Some(container) = &ring.core_container {
                container.StopAnimation(&HSTRING::from("Scale"))?;
            }
            if let Some(geometry) = &ring.geometry {
                geometry.StopAnimation(&HSTRING::from("Radius"))?;
            }
            if let Some(shape) = &ring.shape {
                shape.StopAnimation(&HSTRING::from("StrokeThickness"))?;
            }
            ring.visual.SetScale(Vector3 {
                X: 1.0,
                Y: 1.0,
                Z: 1.0,
            })?;
        }
        Ok(())
    }

    fn stop_success_settle_tracks(&mut self) -> windows::core::Result<()> {
        self.button.StopAnimation(&HSTRING::from("Scale"))?;
        self.inner_inverse_clip
            .StopAnimation(&HSTRING::from("Scale"))?;
        self.inner_wave_root
            .StopAnimation(&HSTRING::from("Scale"))?;
        self.outer_audio_root
            .StopAnimation(&HSTRING::from("Opacity"))?;
        self.auto_shadow_root
            .StopAnimation(&HSTRING::from("Scale"))?;
        self.auto_shadow_mask
            .StopAnimation(&HSTRING::from("Scale"))?;
        self.success_settle_active = false;
        Ok(())
    }

    fn finish_success_settle(&mut self) -> windows::core::Result<()> {
        self.stop_ripple_child_tracks()?;
        self.stop_success_settle_tracks()?;
        self.inner_inverse_clip
            .SetScale(Vector2 { X: 1.0, Y: 1.0 })?;
        self.inner_wave_root.SetScale(Vector3 {
            X: 0.3,
            Y: 0.3,
            Z: 1.0,
        })?;
        self.outer_audio_root.SetOpacity(0.0)?;
        self.button.SetScale(Vector3 {
            X: 1.0,
            Y: 1.0,
            Z: 1.0,
        })?;
        self.auto_shadow_root.SetScale(Vector3 {
            X: 0.9,
            Y: 0.9,
            Z: 1.0,
        })?;
        self.auto_shadow_mask.SetScale(Vector2 { X: 0.9, Y: 0.9 })?;
        self.success_settle_active = false;
        self.success_settle_completed = true;
        self.ripples_active = false;
        Ok(())
    }

    fn startup_logo_complete(&self) -> bool {
        self.logo.stage == ui_model::StartupLogoStage::Complete
    }

    fn start_startup_logo(&mut self) -> windows::core::Result<()> {
        self.logo.start()
    }

    fn start_no_match_idle_restore(&self, from_scale: f32) -> windows::core::Result<()> {
        let ease_out = self.compositor.CreateCubicBezierEasingFunction(
            Vector2 { X: 0.0, Y: 0.0 },
            Vector2 { X: 0.58, Y: 1.0 },
        )?;
        let restore =
            create_no_match_scale_animation(&self.compositor, from_scale, 1.0, 0.245, &ease_out)?;
        self.button.SetScale(Vector3 {
            X: from_scale,
            Y: from_scale,
            Z: 1.0,
        })?;
        self.button
            .StartAnimation(&HSTRING::from("Scale"), &restore)
    }

    fn start_no_match_motion(&mut self, no_match_elapsed: f64) -> windows::core::Result<()> {
        let motion_elapsed =
            (no_match_elapsed - ui_model::NO_MATCH_DISPATCH_DELAY_SECONDS).max(0.0);
        let button_left = BUTTON_X as f32;
        let button_top = self.body_top + BUTTON_TOP as f32;
        let tagging_profile = ui_model::no_match_position_profile(
            BUTTON_SIZE,
            -0.150_000_005_960_464_48,
            8.449_999_809_265_137,
            0.5,
        );
        let icon_profile = ui_model::no_match_icon_center_spring_profile();
        let settling_seconds =
            ui_model::no_match_settling_duration_estimate().ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x80070057_u32 as i32),
                    "NoMatch spring settling estimate could not be evaluated",
                )
            })?;
        let timing = ui_model::no_match_timing(ui_model::SettlingDuration {
            settling_seconds: Some(settling_seconds),
        })
        .ok_or_else(|| {
            Error::new(
                windows::core::HRESULT(0x80070057_u32 as i32),
                "NoMatch timing could not be evaluated",
            )
        })?;
        let (tagging_offset, tagging_position) = create_no_match_position_animation(
            &self.compositor,
            button_left,
            button_top,
            tagging_profile,
            timing.tag_button_center_begin,
            motion_elapsed,
        )?;
        let (icon_offset, icon_position) = create_no_match_position_animation(
            &self.compositor,
            0.0,
            0.0,
            icon_profile,
            timing.tag_button_center_begin,
            motion_elapsed,
        )?;
        self.button.SetOffset(Vector3 {
            X: button_left + tagging_offset,
            Y: button_top,
            Z: 0.0,
        })?;
        self.no_match_icon_transform.SetOffset(Vector3 {
            X: icon_offset,
            Y: 0.0,
            Z: 0.0,
        })?;
        if let Some(animation) = tagging_position {
            self.button
                .StartAnimation(&HSTRING::from("Offset"), &animation)?;
        }
        if let Some(animation) = icon_position {
            self.no_match_icon_transform
                .StartAnimation(&HSTRING::from("Offset"), &animation)?;
        }

        let (icon_opacity, icon_opacity_animation) = create_no_match_opacity_sequence(
            &self.compositor,
            motion_elapsed,
            (timing.icon_mask_initial_fade, 1.0, 0.0),
            Some((timing.icon_mask_secondary_fade, 0.0, 1.0)),
            1.0,
        )?;
        self.no_match_icon_transform.SetOpacity(icon_opacity)?;
        if let Some(animation) = icon_opacity_animation {
            self.no_match_icon_transform
                .StartAnimation(&HSTRING::from("Opacity"), &animation)?;
        }

        let (button_scale, button_scale_animation) = create_no_match_scale_timeline(
            &self.compositor,
            motion_elapsed,
            timing.button_scale_down,
            1.0,
            0.8,
            timing.button_scale_up_begin,
            timing.button_scale_up,
            NoMatchScaleUp::EaseOut { from: 0.8, to: 1.0 },
        )?;
        self.button_content.SetScale(Vector3 {
            X: button_scale,
            Y: button_scale,
            Z: 1.0,
        })?;
        if let Some(animation) = button_scale_animation {
            self.button_content
                .StartAnimation(&HSTRING::from("Scale"), &animation)?;
        }

        let (logo_opacity, logo_opacity_animation) = create_no_match_opacity_sequence(
            &self.compositor,
            motion_elapsed,
            (timing.logo_fade_out, 1.0, 0.0),
            Some((timing.logo_fade_in, 0.0, 1.0)),
            1.0,
        )?;
        self.no_match_logo_transform.SetOpacity(logo_opacity)?;
        if let Some(animation) = logo_opacity_animation {
            self.no_match_logo_transform
                .StartAnimation(&HSTRING::from("Opacity"), &animation)?;
        }

        let logo_scale_profile = ui_model::SpringProfile {
            from: ui_model::NO_MATCH_LOGO_SCALE_OUT_TO,
            to: 1.0,
            mass: timing.logo_scale_spring[0],
            damping: timing.logo_scale_spring[1],
            stiffness: timing.logo_scale_spring[2],
            initial_velocity: timing.logo_scale_spring[3],
            duration_seconds: timing.logo_scale_in.1,
        };
        let (logo_scale, logo_scale_animation) = create_no_match_scale_timeline(
            &self.compositor,
            motion_elapsed,
            timing.logo_scale_out,
            ui_model::NO_MATCH_LOGO_SCALE_OUT_FROM as f32,
            ui_model::NO_MATCH_LOGO_SCALE_OUT_TO as f32,
            timing.logo_scale_in.0,
            timing.logo_scale_in.1,
            NoMatchScaleUp::Spring(logo_scale_profile),
        )?;
        self.no_match_logo_transform.SetScale(Vector3 {
            X: logo_scale,
            Y: logo_scale,
            Z: 1.0,
        })?;
        if let Some(animation) = logo_scale_animation {
            self.no_match_logo_transform
                .StartAnimation(&HSTRING::from("Scale"), &animation)?;
        }
        Ok(())
    }
}

fn create_no_match_scale_animation(
    compositor: &Compositor,
    from: f32,
    to: f32,
    duration_seconds: f64,
    easing: &CubicBezierEasingFunction,
) -> windows::core::Result<Vector3KeyFrameAnimation> {
    let animation = compositor.CreateVector3KeyFrameAnimation()?;
    animation.SetDuration(TimeSpan {
        Duration: (duration_seconds * 10_000_000.0).round() as i64,
    })?;
    animation.InsertKeyFrame(
        0.0,
        Vector3 {
            X: from,
            Y: from,
            Z: 1.0,
        },
    )?;
    animation.InsertKeyFrameWithEasingFunction(
        1.0,
        Vector3 {
            X: to,
            Y: to,
            Z: 1.0,
        },
        easing,
    )?;
    Ok(animation)
}

fn create_no_match_opacity_sequence(
    compositor: &Compositor,
    elapsed_seconds: f64,
    first: ((f64, f64), f64, f64),
    second: Option<((f64, f64), f64, f64)>,
    before_first: f64,
) -> windows::core::Result<(f32, Option<ScalarKeyFrameAnimation>)> {
    let elapsed = if elapsed_seconds.is_finite() {
        elapsed_seconds.max(0.0)
    } else {
        0.0
    };
    let sample = |at: f64| -> f32 {
        let (first_timing, first_from, first_to) = first;
        let first_end = first_timing.0 + first_timing.1;
        if at < first_timing.0 {
            return before_first as f32;
        }
        if at < first_end {
            return (first_from + (first_to - first_from) * ((at - first_timing.0) / first_timing.1))
                as f32;
        }
        if let Some((second_timing, second_from, second_to)) = second {
            if at < second_timing.0 {
                return first_to as f32;
            }
            if at < second_timing.0 + second_timing.1 {
                return (second_from
                    + (second_to - second_from) * ((at - second_timing.0) / second_timing.1))
                    as f32;
            }
            return second_to as f32;
        }
        first_to as f32
    };
    let end_time = second
        .map(|(timing, _, _)| timing.0 + timing.1)
        .unwrap_or(first.0 .0 + first.0 .1)
        .max(first.0 .0 + first.0 .1);
    let value = sample(elapsed);
    if elapsed >= end_time {
        return Ok((value, None));
    }

    let animation = compositor.CreateScalarKeyFrameAnimation()?;
    animation.SetDuration(TimeSpan {
        Duration: ((end_time - elapsed) * 10_000_000.0).round() as i64,
    })?;
    animation.InsertKeyFrame(0.0, value)?;
    let mut events = vec![first.0 .0, first.0 .0 + first.0 .1];
    if let Some((timing, _, _)) = second {
        events.push(timing.0);
        events.push(timing.0 + timing.1);
    }
    events.sort_by(f64::total_cmp);
    events.dedup_by(|left, right| (*left - *right).abs() <= 1e-9);
    for at in events
        .into_iter()
        .filter(|at| *at > elapsed && *at <= end_time)
    {
        let progress = ((at - elapsed) / (end_time - elapsed)) as f32;
        animation.InsertKeyFrame(progress, sample(at))?;
    }
    Ok((value, Some(animation)))
}

#[derive(Clone, Copy)]
enum NoMatchScaleUp {
    EaseOut { from: f32, to: f32 },
    Spring(ui_model::SpringProfile),
}

fn create_no_match_scale_timeline(
    compositor: &Compositor,
    elapsed_seconds: f64,
    down: (f64, f64),
    down_from: f32,
    down_to: f32,
    up_start: f64,
    up_duration: f64,
    up: NoMatchScaleUp,
) -> windows::core::Result<(f32, Option<Vector3KeyFrameAnimation>)> {
    let elapsed = if elapsed_seconds.is_finite() {
        elapsed_seconds.max(0.0)
    } else {
        0.0
    };
    let down_end = down.0 + down.1;
    let up_end = up_start + up_duration;
    let ease_down =
        |progress: f64| down_from + (down_to - down_from) * ui_model::ease_out(progress) as f32;
    let ease_up = |progress: f64, from: f32, to: f32| {
        from + (to - from) * ui_model::ease_out(progress) as f32
    };
    let spring_at = |progress: f64, profile: ui_model::SpringProfile| {
        ui_model::spring_value(progress, profile) as f32
    };
    let final_value = match up {
        NoMatchScaleUp::EaseOut { to, .. } => to,
        NoMatchScaleUp::Spring(profile) => spring_at(1.0, profile),
    };
    let value = if elapsed < down.0 {
        down_from
    } else if elapsed < down_end {
        ease_down((elapsed - down.0) / down.1)
    } else if elapsed < up_start {
        down_to
    } else if elapsed < up_end {
        let progress = (elapsed - up_start) / up_duration;
        match up {
            NoMatchScaleUp::EaseOut { from, to } => ease_up(progress, from, to),
            NoMatchScaleUp::Spring(profile) => spring_at(progress, profile),
        }
    } else {
        final_value
    };
    if elapsed >= up_end {
        return Ok((final_value, None));
    }

    let animation = compositor.CreateVector3KeyFrameAnimation()?;
    let timeline_end = up_end.max(elapsed + f64::EPSILON);
    let timeline_duration = timeline_end - elapsed;
    animation.SetDuration(TimeSpan {
        Duration: (timeline_duration * 10_000_000.0).round().max(1.0) as i64,
    })?;
    animation.InsertKeyFrame(
        0.0,
        Vector3 {
            X: value,
            Y: value,
            Z: 1.0,
        },
    )?;

    if elapsed < down_end {
        let progress = if elapsed < down.0 {
            0.0
        } else {
            (elapsed - down.0) / down.1
        };
        if elapsed < down.0 && down.0 > elapsed {
            animation.InsertKeyFrame(
                ((down.0 - elapsed) / timeline_duration) as f32,
                Vector3 {
                    X: down_from,
                    Y: down_from,
                    Z: 1.0,
                },
            )?;
        }
        let curve = ui_model::ease_out_tail(progress).ok_or_else(|| {
            Error::new(
                windows::core::HRESULT(0x80070057_u32 as i32),
                "NoMatch EaseOut tail could not be evaluated",
            )
        })?;
        let easing = compositor.CreateCubicBezierEasingFunction(
            Vector2 {
                X: curve.x1 as f32,
                Y: curve.y1 as f32,
            },
            Vector2 {
                X: curve.x2 as f32,
                Y: curve.y2 as f32,
            },
        )?;
        animation.InsertKeyFrameWithEasingFunction(
            ((down_end - elapsed) / timeline_duration) as f32,
            Vector3 {
                X: down_to,
                Y: down_to,
                Z: 1.0,
            },
            &easing,
        )?;
    }

    if elapsed < up_start {
        animation.InsertKeyFrame(
            ((up_start - elapsed) / timeline_duration) as f32,
            Vector3 {
                X: down_to,
                Y: down_to,
                Z: 1.0,
            },
        )?;
    }

    match up {
        NoMatchScaleUp::EaseOut { from, to } => {
            let progress = if elapsed < up_start {
                0.0
            } else {
                (elapsed - up_start) / up_duration
            };
            let curve = ui_model::ease_out_tail(progress).ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x80070057_u32 as i32),
                    "NoMatch scale-up EaseOut tail could not be evaluated",
                )
            })?;
            let easing = compositor.CreateCubicBezierEasingFunction(
                Vector2 {
                    X: curve.x1 as f32,
                    Y: curve.y1 as f32,
                },
                Vector2 {
                    X: curve.x2 as f32,
                    Y: curve.y2 as f32,
                },
            )?;
            animation.InsertKeyFrameWithEasingFunction(
                ((up_end - elapsed) / timeline_duration) as f32,
                Vector3 {
                    X: to,
                    Y: to,
                    Z: 1.0,
                },
                &easing,
            )?;
            let _ = from;
        }
        NoMatchScaleUp::Spring(profile) => {
            let current_progress = if elapsed < up_start {
                0.0
            } else {
                ((elapsed - up_start) / up_duration).clamp(0.0, 1.0)
            };
            for index in 1..=256 {
                let local = index as f64 / 256.0;
                let progress = current_progress + (1.0 - current_progress) * local;
                let at = up_start + progress * up_duration;
                if at <= elapsed + 1e-9 {
                    continue;
                }
                let scale = spring_at(progress, profile);
                animation.InsertKeyFrame(
                    ((at - elapsed) / timeline_duration) as f32,
                    Vector3 {
                        X: scale,
                        Y: scale,
                        Z: 1.0,
                    },
                )?;
            }
        }
    }
    Ok((value, Some(animation)))
}

fn create_no_match_position_animation(
    compositor: &Compositor,
    center_x: f32,
    center_y: f32,
    profile: ui_model::SpringProfile,
    start_seconds: f64,
    elapsed_seconds: f64,
) -> windows::core::Result<(f32, Option<Vector3KeyFrameAnimation>)> {
    let elapsed = if elapsed_seconds.is_finite() {
        elapsed_seconds.max(0.0)
    } else {
        0.0
    };
    let progress = if elapsed < start_seconds {
        0.0
    } else {
        ((elapsed - start_seconds) / profile.duration_seconds).clamp(0.0, 1.0)
    };
    let offset = ui_model::spring_value(progress, profile) as f32;
    if elapsed >= start_seconds + profile.duration_seconds {
        return Ok((0.0, None));
    }
    let animation = compositor.CreateVector3KeyFrameAnimation()?;
    let remaining = profile.duration_seconds * (1.0 - progress);
    animation.SetDuration(TimeSpan {
        Duration: (remaining * 10_000_000.0).round().max(1.0) as i64,
    })?;
    if elapsed < start_seconds {
        animation.SetDelayTime(TimeSpan {
            Duration: ((start_seconds - elapsed) * 10_000_000.0).round() as i64,
        })?;
    }
    for index in 0..=256 {
        let local = index as f64 / 256.0;
        let sample_progress = progress + (1.0 - progress) * local;
        let sample_offset = ui_model::spring_value(sample_progress, profile) as f32;
        animation.InsertKeyFrame(
            local as f32,
            Vector3 {
                X: center_x + sample_offset,
                Y: center_y,
                Z: 0.0,
            },
        )?;
    }
    Ok((offset, Some(animation)))
}

fn make_inner_ring_animations(
    compositor: &Compositor,
    circle_index: usize,
) -> windows::core::Result<InnerRingAnimations> {
    let core_scale = create_vector_track_animation(
        compositor,
        ui_model::shifted_inner_track(ui_model::InnerTrack::CoreScale, circle_index),
    )?;
    let core_opacity = create_scalar_track_animation(
        compositor,
        ui_model::shifted_inner_track(ui_model::InnerTrack::CoreOpacity, circle_index),
        ui_model::INNER_OUTER_LOOP,
    )?;
    let sublayer_scale = create_vector_track_animation(
        compositor,
        ui_model::shifted_inner_track(ui_model::InnerTrack::SublayerScale, circle_index),
    )?;
    let scale_null = create_vector_track_animation(
        compositor,
        ui_model::shifted_inner_track(ui_model::InnerTrack::ScaleNull, circle_index),
    )?;
    Ok(InnerRingAnimations {
        core_scale,
        core_opacity,
        sublayer_scale,
        scale_null,
    })
}

fn create_vector_track_animation(
    compositor: &Compositor,
    keyframes: Vec<ui_model::TimedTrackKeyframe>,
) -> windows::core::Result<Vector3KeyFrameAnimation> {
    let animation = compositor.CreateVector3KeyFrameAnimation()?;
    animation.SetDuration(TimeSpan {
        Duration: (ui_model::INNER_OUTER_LOOP * 10_000_000.0).round() as i64,
    })?;
    animation.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
    for keyframe in keyframes {
        let value = Vector3 {
            X: keyframe.value as f32,
            Y: keyframe.value as f32,
            Z: 1.0,
        };
        if let Some(curve) = keyframe.easing {
            let easing = compositor.CreateCubicBezierEasingFunction(
                Vector2 {
                    X: curve.x1 as f32,
                    Y: curve.y1 as f32,
                },
                Vector2 {
                    X: curve.x2 as f32,
                    Y: curve.y2 as f32,
                },
            )?;
            animation.InsertKeyFrameWithEasingFunction(keyframe.progress as f32, value, &easing)?;
        } else {
            animation.InsertKeyFrame(keyframe.progress as f32, value)?;
        }
    }
    Ok(animation)
}

fn create_scalar_track_animation(
    compositor: &Compositor,
    keyframes: Vec<ui_model::TimedTrackKeyframe>,
    duration_seconds: f64,
) -> windows::core::Result<ScalarKeyFrameAnimation> {
    let animation = compositor.CreateScalarKeyFrameAnimation()?;
    animation.SetDuration(TimeSpan {
        Duration: (duration_seconds * 10_000_000.0).round() as i64,
    })?;
    animation.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
    for keyframe in keyframes {
        if let Some(curve) = keyframe.easing {
            let easing = compositor.CreateCubicBezierEasingFunction(
                Vector2 {
                    X: curve.x1 as f32,
                    Y: curve.y1 as f32,
                },
                Vector2 {
                    X: curve.x2 as f32,
                    Y: curve.y2 as f32,
                },
            )?;
            animation.InsertKeyFrameWithEasingFunction(
                keyframe.progress as f32,
                keyframe.value as f32,
                &easing,
            )?;
        } else {
            animation.InsertKeyFrame(keyframe.progress as f32, keyframe.value as f32)?;
        }
    }
    Ok(animation)
}

fn create_vector2_track_animation(
    compositor: &Compositor,
    keyframes: Vec<ui_model::TimedTrackKeyframe>,
    base_radius: f32,
    duration_seconds: f64,
) -> windows::core::Result<Vector2KeyFrameAnimation> {
    let animation = compositor.CreateVector2KeyFrameAnimation()?;
    animation.SetDuration(TimeSpan {
        Duration: (duration_seconds * 10_000_000.0).round() as i64,
    })?;
    animation.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
    for keyframe in keyframes {
        let value = Vector2 {
            X: base_radius * keyframe.value as f32,
            Y: base_radius * keyframe.value as f32,
        };
        if let Some(curve) = keyframe.easing {
            let easing = compositor.CreateCubicBezierEasingFunction(
                Vector2 {
                    X: curve.x1 as f32,
                    Y: curve.y1 as f32,
                },
                Vector2 {
                    X: curve.x2 as f32,
                    Y: curve.y2 as f32,
                },
            )?;
            animation.InsertKeyFrameWithEasingFunction(keyframe.progress as f32, value, &easing)?;
        } else {
            animation.InsertKeyFrame(keyframe.progress as f32, value)?;
        }
    }
    Ok(animation)
}

fn composition_color(a: u8, r: u8, g: u8, b: u8) -> Color {
    Color {
        A: a,
        R: r,
        G: g,
        B: b,
    }
}

fn composition_tagging_color(theme: Theme) -> Color {
    match theme {
        Theme::Light => composition_color(255, 255, 255, 255),
        Theme::Dark => composition_color(255, 148, 148, 148),
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn d2d_color(r: u8, g: u8, b: u8, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: alpha,
    }
}

fn history_title_color(theme: Theme) -> D2D1_COLOR_F {
    match theme {
        Theme::Light => d2d_color(0, 0, 0, 0.847),
        Theme::Dark => d2d_color(255, 255, 255, 0.847),
    }
}

fn d2d_color_from_system(color: Color) -> D2D1_COLOR_F {
    d2d_color(color.R, color.G, color.B, f32::from(color.A) / 255.0)
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

fn result_artwork_rect(panel_top: f32, edge: ArrowEdge) -> D2D_RECT_F {
    let top = panel_top + 20.0 - bottom_arrow_visual_top_adjustment(edge);
    rect(10.0, top, 160.0, top + 150.0)
}

fn aspect_fit_rect(destination: D2D_RECT_F, source_width: u32, source_height: u32) -> D2D_RECT_F {
    // UI-184 records NSImageScale=3: keep the image-view frame as the clip,
    // but center the proportionally scaled image inside it.
    let destination_width = destination.right - destination.left;
    let destination_height = destination.bottom - destination.top;
    if source_width == 0
        || source_height == 0
        || destination_width <= 0.0
        || destination_height <= 0.0
    {
        return destination;
    }

    let scale =
        (destination_width / source_width as f32).min(destination_height / source_height as f32);
    let width = source_width as f32 * scale;
    let height = source_height as f32 * scale;
    let left = destination.left + (destination_width - width) * 0.5;
    let top = destination.top + (destination_height - height) * 0.5;
    rect(left, top, left + width, top + height)
}

fn fallback_match_artwork_rect(frame: D2D_RECT_F) -> D2D_RECT_F {
    let width = frame.right - frame.left;
    let height = frame.bottom - frame.top;
    let left = frame.left + (width - FALLBACK_MATCH_LOGICAL_SIDE) * 0.5;
    let top = frame.top + (height - FALLBACK_MATCH_LOGICAL_SIDE) * 0.5;
    rect(
        left,
        top,
        left + FALLBACK_MATCH_LOGICAL_SIDE,
        top + FALLBACK_MATCH_LOGICAL_SIDE,
    )
}

// [R] DetailsColumn 204pt wide with 20pt insets yields a 164pt text column
// when preserving its derived x=172 leading edge (UI-3.2).
const RESULT_DETAILS_LEFT: f32 = 172.0;
const RESULT_DETAILS_RIGHT: f32 = 336.0;
const RESULT_DETAILS_CENTER_Y: f32 = 95.0;
const RESULT_TITLE_MIN_HEIGHT: f32 = 24.0;
const RESULT_TITLE_MAX_LINES: f32 = 3.0;
const RESULT_ARTIST_MIN_HEIGHT: f32 = 18.0;
const RESULT_ARTIST_MAX_LINES: f32 = 4.0;
const RESULT_ARTWORK_CORNER_RADIUS: f32 = 5.0;
const RESULT_DETAILS_TITLE_ARTIST_GAP: f32 = 5.0;
const RESULT_DETAILS_ARTIST_ACTION_GAP: f32 = 10.0;
const RESULT_ACTION_HEIGHT: f32 = 20.0;

fn result_details_rects(
    panel_top: f32,
    title_height: f32,
    artist_height: f32,
    action_visible: bool,
) -> ResultDetailsRects {
    let action_height = if action_visible {
        RESULT_DETAILS_ARTIST_ACTION_GAP + RESULT_ACTION_HEIGHT
    } else {
        0.0
    };
    let total_height =
        title_height + RESULT_DETAILS_TITLE_ARTIST_GAP + artist_height + action_height;
    let title_top = panel_top + RESULT_DETAILS_CENTER_Y - total_height * 0.5;
    let title_bottom = title_top + title_height;
    let artist_top = title_bottom + RESULT_DETAILS_TITLE_ARTIST_GAP;
    let artist_bottom = artist_top + artist_height;
    let action_top = if action_visible {
        artist_bottom + RESULT_DETAILS_ARTIST_ACTION_GAP
    } else {
        artist_bottom
    };
    ResultDetailsRects {
        title: rect(
            RESULT_DETAILS_LEFT,
            title_top,
            RESULT_DETAILS_RIGHT,
            title_bottom,
        ),
        artist: rect(
            RESULT_DETAILS_LEFT,
            artist_top,
            RESULT_DETAILS_RIGHT,
            artist_bottom,
        ),
        action: if action_visible {
            rect(
                RESULT_DETAILS_LEFT,
                action_top,
                RESULT_DETAILS_RIGHT,
                action_top + RESULT_ACTION_HEIGHT,
            )
        } else {
            rect(0.0, 0.0, 0.0, 0.0)
        },
    }
}

const HISTORY_ROW_HEIGHT: f32 = 89.0;
const HISTORY_ROW_SPACING: f32 = 2.0;
const HISTORY_ROW_PITCH: f32 = HISTORY_ROW_HEIGHT + HISTORY_ROW_SPACING;
// One native wheel line follows the history text's approximate DirectWrite line height.
// The number of lines per detent comes from SPI_GETWHEELSCROLLLINES.
const HISTORY_SCROLL_LINE_DIP: f32 = 18.0;
const HISTORY_TEXT_LEFT: f32 = 95.0;
const HISTORY_TEXT_RIGHT: f32 = 360.0;
const HISTORY_ARTWORK_SIZE: f32 = 65.0;
const HISTORY_ARTWORK_LEFT: f32 = 20.0;
const HISTORY_ARTWORK_TOP: f32 = 11.0;
const HISTORY_ARTWORK_CENTER_Y: f32 = HISTORY_ARTWORK_TOP + HISTORY_ARTWORK_SIZE * 0.5;
const HISTORY_TEXT_GAP: f32 = 5.0;

fn history_content_height(item_count: usize) -> f32 {
    if item_count == 0 {
        0.0
    } else {
        item_count as f32 * HISTORY_ROW_PITCH - HISTORY_ROW_SPACING
    }
}

fn history_max_scroll_offset(item_count: usize, viewport_height: f32) -> f32 {
    (history_content_height(item_count) - viewport_height.max(0.0)).max(0.0)
}

fn history_scroll_offset_after_wheel(
    current_offset: f32,
    delta: f32,
    wheel_lines: u32,
    item_count: usize,
    viewport_height: f32,
) -> f32 {
    let maximum = history_max_scroll_offset(item_count, viewport_height);
    let current = if current_offset.is_finite() {
        current_offset
    } else {
        0.0
    };
    if !delta.is_finite() || wheel_lines == 0 {
        return current.clamp(0.0, maximum);
    }

    let distance = if wheel_lines == u32::MAX {
        delta / 120.0 * viewport_height
    } else {
        delta / 120.0 * wheel_lines as f32 * HISTORY_SCROLL_LINE_DIP
    };
    (current - distance).clamp(0.0, maximum)
}

fn history_offset_after_insert(
    current_offset: f32,
    new_item_count: usize,
    viewport_height: f32,
) -> (f32, bool) {
    if current_offset > 0.0 {
        (
            (current_offset + HISTORY_ROW_PITCH)
                .min(history_max_scroll_offset(new_item_count, viewport_height)),
            false,
        )
    } else {
        (0.0, true)
    }
}

fn history_text_rects(
    row_top: f32,
    title_height: f32,
    artist_height: f32,
    date_height: f32,
) -> HistoryTextRects {
    let total_height = title_height + artist_height + date_height + HISTORY_TEXT_GAP * 2.0;
    let title_top = row_top + HISTORY_ARTWORK_CENTER_Y - total_height * 0.5;
    let title_bottom = title_top + title_height;
    let artist_top = title_bottom + HISTORY_TEXT_GAP;
    let artist_bottom = artist_top + artist_height;
    let date_top = artist_bottom + HISTORY_TEXT_GAP;
    HistoryTextRects {
        title: rect(
            HISTORY_TEXT_LEFT,
            title_top,
            HISTORY_TEXT_RIGHT,
            title_bottom,
        ),
        artist: rect(
            HISTORY_TEXT_LEFT,
            artist_top,
            HISTORY_TEXT_RIGHT,
            artist_top + artist_height,
        ),
        date: rect(
            HISTORY_TEXT_LEFT,
            date_top,
            HISTORY_TEXT_RIGHT,
            date_top + date_height,
        ),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeaderFillScope {
    HeaderAndArrow,
    HeaderRectangle,
}

fn header_fill_scope(edge: ArrowEdge) -> HeaderFillScope {
    match edge {
        ArrowEdge::Top => HeaderFillScope::HeaderAndArrow,
        ArrowEdge::Bottom => HeaderFillScope::HeaderRectangle,
    }
}

fn popup_base_color(theme: Theme) -> (u8, u8, u8) {
    match theme {
        Theme::Light => (255, 255, 255),
        Theme::Dark => (30, 30, 30),
    }
}

fn result_gradient_colors(theme: Theme, edge: ArrowEdge) -> [(u8, u8, u8); 2] {
    let (top, bottom) = match theme {
        Theme::Light => ((244, 247, 249), (188, 205, 216)),
        Theme::Dark => ((0, 0, 0), (55, 66, 73)),
    };
    match edge {
        ArrowEdge::Top => [top, bottom],
        ArrowEdge::Bottom => [bottom, top],
    }
}

fn history_artwork_frame(row_top: f32) -> D2D_RECT_F {
    rect(
        HISTORY_ARTWORK_LEFT,
        row_top + HISTORY_ARTWORK_TOP,
        HISTORY_ARTWORK_LEFT + HISTORY_ARTWORK_SIZE,
        row_top + HISTORY_ARTWORK_TOP + HISTORY_ARTWORK_SIZE,
    )
}

fn gradient_origin_transform(top: f32) -> Matrix3x2 {
    Matrix3x2 {
        M11: 1.0,
        M22: 1.0,
        M32: top,
        ..Default::default()
    }
}

fn create_text_format(
    factory: &IDWriteFactory,
    size: f32,
    weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT,
    alignment: windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT,
) -> windows::core::Result<IDWriteTextFormat> {
    create_text_format_with_locale(
        factory,
        size,
        weight,
        alignment,
        directwrite_locale(ui_language()),
    )
}

fn create_text_format_with_locale(
    factory: &IDWriteFactory,
    size: f32,
    weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT,
    alignment: windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT,
    locale: &str,
) -> windows::core::Result<IDWriteTextFormat> {
    let family = wide("Segoe UI");
    let locale = wide(locale);
    unsafe {
        let format = factory.CreateTextFormat(
            PCWSTR(family.as_ptr()),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            PCWSTR(locale.as_ptr()),
        )?;
        format.SetTextAlignment(alignment)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
        // The source does not establish the native ellipsis policy. Use a
        // replaceable Windows candidate at Unicode character-cluster
        // boundaries so constrained labels do not leave a partly drawn line.
        let trimming = windows::Win32::Graphics::DirectWrite::DWRITE_TRIMMING {
            granularity:
                windows::Win32::Graphics::DirectWrite::DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: 0,
            delimiterCount: 0,
        };
        let trimming_sign = factory.CreateEllipsisTrimmingSign(&format)?;
        format.SetTrimming(&trimming, &trimming_sign)?;
        Ok(format)
    }
}

unsafe fn measure_directwrite_text_layout(
    factory: &IDWriteFactory,
    value: &str,
    format: &IDWriteTextFormat,
    max_width: f32,
    min_height: f32,
    max_lines: f32,
) -> windows::core::Result<MeasuredTextLayout> {
    if value.is_empty() {
        return Ok(MeasuredTextLayout {
            height: min_height,
            ..Default::default()
        });
    }
    let content: Vec<u16> = value.encode_utf16().collect();
    let layout = factory.CreateTextLayout(&content, format, max_width, 512.0)?;
    let mut text_metrics = DWRITE_TEXT_METRICS::default();
    layout.GetMetrics(&mut text_metrics)?;
    if text_metrics.lineCount == 0 {
        return Ok(MeasuredTextLayout {
            height: min_height,
            ..Default::default()
        });
    }
    let mut lines = vec![
        windows::Win32::Graphics::DirectWrite::DWRITE_LINE_METRICS::default();
        text_metrics.lineCount as usize
    ];
    let mut actual_line_count = 0;
    layout.GetLineMetrics(Some(&mut lines), &mut actual_line_count)?;

    let mut height = 0.0;
    let mut first_baseline = 0.0;
    let mut last_baseline = 0.0;
    for (index, line) in lines
        .iter()
        .take(actual_line_count as usize)
        .take(max_lines.ceil() as usize)
        .enumerate()
    {
        if index == 0 {
            first_baseline = line.baseline;
        }
        last_baseline = height + line.baseline;
        height += line.height;
    }
    let overhang = layout.GetOverhangMetrics().unwrap_or_default();
    Ok(MeasuredTextLayout {
        height: height.max(min_height),
        first_baseline,
        last_baseline,
        overhang_left: overhang.left,
        overhang_top: overhang.top,
        overhang_right: overhang.right,
        overhang_bottom: overhang.bottom,
    })
}

impl Renderer {
    #[allow(clippy::too_many_arguments)] // A window render target needs all three existing native factories and the target state.
    unsafe fn new(
        hwnd: HWND,
        factory: ID2D1Factory,
        write_factory: IDWriteFactory,
        compositor: Compositor,
        dpi: u32,
        theme_generation: u64,
        theme: Theme,
        system_foreground: Color,
        kind: WindowKind,
        animate_startup_logo: bool,
    ) -> windows::core::Result<Self> {
        let mut client = RECT::default();
        GetClientRect(hwnd, &mut client)?;
        let dpi = dpi.max(96);
        let size = D2D_SIZE_U {
            width: (client.right - client.left).max(1) as u32,
            height: (client.bottom - client.top).max(1) as u32,
        };
        let target_properties = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: if kind == WindowKind::Main {
                    D2D1_ALPHA_MODE_PREMULTIPLIED
                } else {
                    D2D1_ALPHA_MODE_IGNORE
                },
            },
            dpiX: dpi as f32,
            dpiY: dpi as f32,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: Default::default(),
        };
        let (target, hwnd_target, dc_target, layered_surface) = if kind == WindowKind::Main {
            let surface = LayeredSurface::new(size.width, size.height)?;
            let dc_target = factory.CreateDCRenderTarget(&target_properties)?;
            dc_target.BindDC(surface.dc, &client)?;
            let target: ID2D1RenderTarget = dc_target.cast()?;
            (target, None, Some(dc_target), Some(surface))
        } else {
            let hwnd_properties = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: size,
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let target = factory.CreateHwndRenderTarget(&target_properties, &hwnd_properties)?;
            let render_target: ID2D1RenderTarget = target.cast()?;
            (render_target, Some(target), None, None)
        };
        target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let settings_icon = create_ui_icon_bitmap(&target, SETTINGS_ICON_PIXELS, 32, 32)?;
        let white_close_icon = create_ui_icon_bitmap(&target, WHITE_CLOSE_ICON_PIXELS, 24, 24)?;
        let result_close_icon = create_ui_icon_bitmap(&target, &RESULT_CLOSE_ICON_PIXELS, 24, 24)?;
        let fallback_match_artwork = if kind == WindowKind::Main {
            Some((
                create_ui_icon_bitmap(&target, FALLBACK_MATCH_LIGHT_PIXELS, 262, 262)?,
                create_ui_icon_bitmap(&target, FALLBACK_MATCH_DARK_PIXELS, 262, 262)?,
            ))
        } else {
            None
        };
        let palette = create_palette(&target, theme, system_foreground)?;
        let text = create_text_styles(&write_factory)?;
        let composition = if kind == WindowKind::Main {
            Some(CompositionScene::new(
                hwnd,
                &factory,
                &write_factory,
                &compositor,
                theme,
                ArrowEdge::Top,
                dpi,
                animate_startup_logo,
            )?)
        } else {
            None
        };
        let popup_layer = if kind == WindowKind::Main {
            Some(target.CreateLayer(None)?)
        } else {
            None
        };
        let artwork_layer = target.CreateLayer(None)?;
        let artwork_geometries = [
            (
                150,
                factory
                    .CreateRoundedRectangleGeometry(
                        &windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                            rect: rect(0.0, 0.0, 150.0, 150.0),
                            radiusX: RESULT_ARTWORK_CORNER_RADIUS,
                            radiusY: RESULT_ARTWORK_CORNER_RADIUS,
                        },
                    )?
                    .cast()?,
            ),
            (
                65,
                factory
                    .CreateRoundedRectangleGeometry(
                        &windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                            rect: rect(0.0, 0.0, 65.0, 65.0),
                            radiusX: 4.0,
                            radiusY: 4.0,
                        },
                    )?
                    .cast()?,
            ),
        ];
        Ok(Self {
            factory,
            write_factory,
            target,
            settings_icon,
            white_close_icon,
            result_close_icon,
            fallback_match_artwork,
            hwnd_target,
            dc_target,
            layered_surface,
            palette,
            text,
            dpi,
            theme_generation,
            composition,
            status_text_composition_enabled: false,
            popup_layer,
            artwork_layer: Some(artwork_layer),
            artwork_geometries,
            artwork_bitmaps: RefCell::new(HashMap::new()),
            popup_outline: RefCell::new(None),
            popup_header: RefCell::new(None),
            result_details_cache: RefCell::new(None),
            history_text_metrics_cache: RefCell::new(HashMap::new()),
            result_overlay_content_key: None,
            result_overlay_motion_key: None,
            result_overlay_failure_reported: false,
        })
    }

    fn result_overlay_content_key(
        &self,
        app: &AppState,
        edge: ArrowEdge,
    ) -> Option<ResultOverlayContentKey> {
        let identity = match &app.phase {
            MainPhase::Matched { track, .. } => ResultOverlayIdentity::Matched {
                display_id: track.display_id,
                title: track.title.clone(),
                artist: track.artist.clone(),
                artwork_url: track.artwork_url.clone(),
                hq_artwork_url: track.hq_artwork_url.clone(),
                artwork: app
                    .artwork_resources
                    .get(&track.display_id)
                    .map(|(source, pixels)| (*source, pixels.bgra.len(), pixels.fingerprint)),
            },
            MainPhase::NoMatch { entered_at, .. } => ResultOverlayIdentity::NoMatch {
                entered_at: *entered_at,
                offline: false,
            },
            MainPhase::Offline { entered_at, .. } => ResultOverlayIdentity::NoMatch {
                entered_at: *entered_at,
                offline: true,
            },
            _ => return None,
        };
        Some(ResultOverlayContentKey {
            identity,
            theme_generation: app.theme_generation,
            dpi: self.dpi,
            edge,
            language: ui_language(),
        })
    }

    fn result_overlay_matches_current(&self, app: &AppState) -> bool {
        let Some(key) = self.result_overlay_content_key.as_ref() else {
            return false;
        };
        if key.theme_generation != app.theme_generation
            || key.dpi != self.dpi
            || key.edge != app.arrow_edge
            || key.language != ui_language()
        {
            return false;
        }
        let current_artwork = match &app.phase {
            MainPhase::Matched { track, .. } => app
                .artwork_resources
                .get(&track.display_id)
                .map(|(source, pixels)| (*source, pixels.bgra.len(), pixels.fingerprint)),
            _ => None,
        };
        result_overlay_identity_matches_phase(&key.identity, &app.phase, current_artwork)
    }

    unsafe fn on_size(&mut self, hwnd: HWND) {
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client).is_ok() {
            let size = D2D_SIZE_U {
                width: (client.right - client.left).max(1) as u32,
                height: (client.bottom - client.top).max(1) as u32,
            };
            if let Some(target) = self.hwnd_target.as_ref() {
                let _ = target.Resize(&size);
            }
            if let (Some(surface), Some(target)) =
                (self.layered_surface.as_mut(), self.dc_target.as_ref())
            {
                let width = size.width.max(1);
                let height = size.height.max(1);
                if surface.size.width != width || surface.size.height != height {
                    if let Ok(replacement) = LayeredSurface::new(width, height) {
                        if target.BindDC(replacement.dc, &client).is_ok() {
                            *surface = replacement;
                        }
                    }
                } else {
                    let _ = target.BindDC(surface.dc, &client);
                }
            }
        }
    }

    unsafe fn on_dpi_changed(&mut self, dpi: u32, hwnd: HWND, app: &mut AppState) {
        self.dpi = dpi.max(96);
        self.target.SetDpi(self.dpi as f32, self.dpi as f32);
        if let Some(composition) = self.composition.as_mut() {
            if let Err(error) = composition.set_dpi(self.dpi) {
                app.message = Some(format!("Composition DPI update failed: {error}"));
            }
        }
        self.on_size(hwnd);
        if let Ok(palette) = create_palette(&self.target, app.theme, app.system_foreground) {
            self.palette = palette;
            self.theme_generation = app.theme_generation;
        }
    }

    unsafe fn refresh_theme_if_needed(&mut self, app: &AppState) {
        if self.theme_generation != app.theme_generation {
            if let Ok(palette) = create_palette(&self.target, app.theme, app.system_foreground) {
                self.palette = palette;
                self.theme_generation = app.theme_generation;
            }
        }
    }

    unsafe fn draw(&mut self, app: &AppState, kind: WindowKind) -> windows::core::Result<()> {
        self.refresh_theme_if_needed(app);
        let body_top = if app.arrow_edge == ArrowEdge::Top {
            POPUP_ARROW_HEIGHT as f32
        } else {
            0.0
        };
        let panel_top = if kind == WindowKind::Main {
            match &app.phase {
                MainPhase::Matched { .. }
                | MainPhase::NoMatch { .. }
                | MainPhase::Offline { .. } => {
                    result_panel_top_at(&app.phase, body_top, Instant::now())
                }
                _ => None,
            }
        } else {
            None
        };
        let overlay_key = (kind == WindowKind::Main)
            .then(|| self.result_overlay_content_key(app, app.arrow_edge))
            .flatten();
        let overlay_motion = (kind == WindowKind::Main)
            .then(|| result_overlay_motion_animation(&app.phase, Instant::now()))
            .flatten();
        let overlay_motion_key = overlay_motion.as_ref().map(|motion| motion.key.clone());
        let overlay_motion_changed = self.result_overlay_motion_key != overlay_motion_key;
        let composition_cache_ready =
            self.composition.is_some() && self.layered_surface.is_some() && panel_top.is_some();
        let composition_cache_hit = composition_cache_ready
            && overlay_key
                .as_ref()
                .is_some_and(|key| self.result_overlay_content_key.as_ref() == Some(key));
        // Rasterize the card once at the fixed header origin. Its animated
        // offset is applied by the Composition visual.
        let result_panel_draw_top = result_panel_draw_top(
            panel_top,
            body_top,
            composition_cache_ready,
            composition_cache_hit,
        );
        self.target.BeginDraw();
        let clear = d2d_color(0, 0, 0, 0.0);
        self.target.Clear(Some(&clear));
        let popup_layer_parameters = if kind == WindowKind::Main {
            Some(windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS {
                contentBounds: rect(0.0, 0.0, CONTENT_WIDTH as f32, MAIN_WINDOW_HEIGHT as f32),
                geometricMask: std::mem::ManuallyDrop::new(Some(self.popup_outline(app)?)),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: windows_numerics::Matrix3x2 {
                    M11: 1.0,
                    M22: 1.0,
                    ..Default::default()
                },
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS_NONE,
            })
        } else {
            None
        };
        if let (Some(parameters), Some(layer)) =
            (popup_layer_parameters.as_ref(), self.popup_layer.as_ref())
        {
            self.target.PushLayer(parameters, layer);
        }
        let draw_result = match kind {
            WindowKind::Main => self.draw_main(app, result_panel_draw_top),
            WindowKind::About => self.draw_about(app),
            WindowKind::MicrophoneInfo => self.draw_microphone_info(app),
        };
        if popup_layer_parameters.is_some() {
            self.target.PopLayer();
        }
        draw_result?;
        self.target.EndDraw(None, None)?;
        if kind == WindowKind::Main {
            let mut overlay_error = None;
            let mut overlay_update_failed = false;
            if let Some(scene) = self.composition.as_mut() {
                let overlay_update = if self.layered_surface.is_some() {
                    scene.update_result_overlay(
                        self.layered_surface.as_ref(),
                        (composition_cache_ready && !composition_cache_hit).then_some(body_top),
                        panel_top,
                        body_top,
                        self.dpi,
                        overlay_motion.as_ref(),
                        overlay_motion_changed,
                    )
                } else {
                    scene.update_result_overlay(
                        None,
                        None,
                        None,
                        body_top,
                        self.dpi,
                        None,
                        overlay_motion_changed,
                    )
                };
                match overlay_update {
                    Ok(()) => {
                        self.result_overlay_content_key = if composition_cache_ready {
                            panel_top.and(overlay_key.clone())
                        } else {
                            None
                        };
                        self.result_overlay_motion_key = overlay_motion_key.clone();
                    }
                    Err(error) => {
                        // The cached Composition surface failed to update.
                        // Redraw the current frame into the layered D2D target
                        // below so the card stays visible while the fallback
                        // path is active.
                        let _ = scene.result_overlay.hide();
                        self.result_overlay_content_key = None;
                        self.result_overlay_motion_key = None;
                        overlay_update_failed = true;
                        overlay_error = Some(error);
                    }
                }
                // A failed Composition copy leaves the card in the layered
                // D2D frame, which is behind the desktop Composition tree.
                // Hide only the Shazam button while the fallback card crosses
                // its bounds; restore it as soon as the card clears the area.
                let fallback_occludes_button = overlay_error.is_some()
                    && result_overlay_occludes_shazam_button(panel_top, body_top);
                if let Err(error) = scene.set_result_fallback_occlusion(fallback_occludes_button) {
                    if overlay_error.is_none() {
                        overlay_error = Some(overlay_surface_error("FallbackButtonOpacity", error));
                    }
                }
            } else {
                self.result_overlay_content_key = None;
                self.result_overlay_motion_key = None;
            }
            if overlay_update_failed && self.layered_surface.is_some() {
                self.redraw_main_dynamic_fallback(app, panel_top)?;
            }
            if let Some(error) = overlay_error {
                if !self.result_overlay_failure_reported {
                    eprintln!(
                        "Windows UI result overlay update failed; using D2D fallback: {error}"
                    );
                    self.result_overlay_failure_reported = true;
                }
            } else {
                self.result_overlay_failure_reported = false;
            }
        }
        if let Some(surface) = self.layered_surface.as_ref() {
            surface.present(app.main_hwnd, app.popup_opacity)?;
        }
        Ok(())
    }

    unsafe fn redraw_main_dynamic_fallback(
        &self,
        app: &AppState,
        panel_top: Option<f32>,
    ) -> windows::core::Result<()> {
        self.target.BeginDraw();
        let clear = d2d_color(0, 0, 0, 0.0);
        self.target.Clear(Some(&clear));
        let parameters = windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS {
            contentBounds: rect(0.0, 0.0, CONTENT_WIDTH as f32, MAIN_WINDOW_HEIGHT as f32),
            geometricMask: std::mem::ManuallyDrop::new(Some(self.popup_outline(app)?)),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: windows_numerics::Matrix3x2 {
                M11: 1.0,
                M22: 1.0,
                ..Default::default()
            },
            opacity: 1.0,
            opacityBrush: std::mem::ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS_NONE,
        };
        if let Some(layer) = self.popup_layer.as_ref() {
            self.target.PushLayer(&parameters, layer);
        }
        let draw_result = self.draw_main(app, panel_top);
        if self.popup_layer.is_some() {
            self.target.PopLayer();
        }
        draw_result?;
        self.target.EndDraw(None, None)?;
        Ok(())
    }

    fn set_popup_opacity(&mut self, opacity: u8) -> windows::core::Result<()> {
        if let Some(scene) = self.composition.as_mut() {
            scene.root.SetOpacity(f32::from(opacity) / 255.0)?;
        }
        Ok(())
    }

    fn update_composition(&mut self, snapshot: AnimationSnapshot) -> windows::core::Result<()> {
        if let Some(scene) = self.composition.as_mut() {
            match scene.update(snapshot) {
                Ok(()) => self.status_text_composition_enabled = true,
                Err(error) => {
                    self.status_text_composition_enabled = false;
                    return Err(error);
                }
            }
        } else {
            self.status_text_composition_enabled = false;
        }
        Ok(())
    }

    fn start_startup_logo(&mut self) -> windows::core::Result<bool> {
        let Some(scene) = self.composition.as_mut() else {
            return Ok(true);
        };
        scene.start_startup_logo()?;
        Ok(scene.startup_logo_complete())
    }

    unsafe fn fill_rect(&self, area: D2D_RECT_F, brush: &ID2D1SolidColorBrush) {
        self.target.FillRectangle(&area, brush);
    }

    unsafe fn fill_gradient_rect(
        &self,
        area: &D2D_RECT_F,
        brush: &ID2D1LinearGradientBrush,
        top: f32,
    ) -> windows::core::Result<()> {
        brush.SetTransform(&gradient_origin_transform(top));
        self.target.FillRectangle(area, brush);
        brush.SetTransform(&gradient_origin_transform(0.0));
        Ok(())
    }

    unsafe fn fill_gradient_geometry(
        &self,
        geometry: &ID2D1Geometry,
        brush: &ID2D1LinearGradientBrush,
        top: f32,
    ) -> windows::core::Result<()> {
        brush.SetTransform(&gradient_origin_transform(top));
        self.target.FillGeometry(geometry, brush, None);
        brush.SetTransform(&gradient_origin_transform(0.0));
        Ok(())
    }

    unsafe fn text(
        &self,
        value: &str,
        area: D2D_RECT_F,
        format: &IDWriteTextFormat,
        brush: &ID2D1SolidColorBrush,
    ) {
        if value.is_empty() {
            return;
        }
        let content: Vec<u16> = value.encode_utf16().collect();
        self.target.DrawText(
            &content,
            format,
            &area,
            brush,
            windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }

    unsafe fn measured_text_height(
        &self,
        value: &str,
        format: &IDWriteTextFormat,
        min_height: f32,
        max_lines: f32,
    ) -> windows::core::Result<f32> {
        self.measured_text_height_at_width(
            value,
            format,
            RESULT_DETAILS_RIGHT - RESULT_DETAILS_LEFT,
            min_height,
            max_lines,
        )
    }

    unsafe fn measured_text_height_at_width(
        &self,
        value: &str,
        format: &IDWriteTextFormat,
        max_width: f32,
        min_height: f32,
        max_lines: f32,
    ) -> windows::core::Result<f32> {
        Ok(measure_directwrite_text_layout(
            &self.write_factory,
            value,
            format,
            max_width,
            min_height,
            max_lines,
        )?
        .height)
    }

    unsafe fn result_details_metrics(
        &self,
        track: &Track,
    ) -> windows::core::Result<ResultDetailsMetrics> {
        if let Some(cached) = self.result_details_cache.borrow().as_ref() {
            if cached.title == track.title && cached.artist == track.artist {
                return Ok(cached.metrics);
            }
        }
        let metrics = ResultDetailsMetrics {
            title_height: self.measured_text_height(
                &track.title,
                &self.text.result_title,
                RESULT_TITLE_MIN_HEIGHT,
                RESULT_TITLE_MAX_LINES,
            )?,
            artist_height: self.measured_text_height(
                &track.artist,
                &self.text.result_artist,
                RESULT_ARTIST_MIN_HEIGHT,
                RESULT_ARTIST_MAX_LINES,
            )?,
        };
        *self.result_details_cache.borrow_mut() = Some(CachedResultDetailsMetrics {
            title: track.title.clone(),
            artist: track.artist.clone(),
            metrics,
        });
        Ok(metrics)
    }

    unsafe fn history_text_metrics(
        &self,
        track: &Track,
    ) -> windows::core::Result<HistoryTextMetrics> {
        if let Some(cached) = self
            .history_text_metrics_cache
            .borrow()
            .get(&track.display_id)
        {
            if cached.title == track.title
                && cached.artist == track.artist
                && cached.date == track.recognized_date_label
            {
                return Ok(cached.metrics);
            }
        }
        let width = HISTORY_TEXT_RIGHT - HISTORY_TEXT_LEFT;
        let metrics = HistoryTextMetrics {
            title: measure_directwrite_text_layout(
                &self.write_factory,
                &track.title,
                &self.text.title,
                width,
                0.0,
                2.0,
            )?,
            artist: measure_directwrite_text_layout(
                &self.write_factory,
                &track.artist,
                &self.text.subtitle,
                width,
                0.0,
                2.0,
            )?,
            date: measure_directwrite_text_layout(
                &self.write_factory,
                &track.recognized_date_label,
                &self.text.date,
                width,
                0.0,
                1.0,
            )?,
        };
        let mut cache = self.history_text_metrics_cache.borrow_mut();
        cache.insert(
            track.display_id,
            CachedHistoryTextMetrics {
                title: track.title.clone(),
                artist: track.artist.clone(),
                date: track.recognized_date_label.clone(),
                metrics,
            },
        );
        if cache.len() > 64 {
            if let Some(oldest_id) = cache.keys().copied().min() {
                cache.remove(&oldest_id);
            }
        }
        Ok(metrics)
    }

    unsafe fn with_result_header_clip(
        &self,
        body_top: f32,
        draw: impl FnOnce() -> windows::core::Result<()>,
    ) -> windows::core::Result<()> {
        self.target.PushAxisAlignedClip(
            &rect(
                0.0,
                body_top,
                CONTENT_WIDTH as f32,
                body_top + TOP_HEIGHT as f32,
            ),
            windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        );
        let result = draw();
        self.target.PopAxisAlignedClip();
        result
    }

    unsafe fn draw_main(
        &self,
        app: &AppState,
        result_panel_draw_top: Option<f32>,
    ) -> windows::core::Result<()> {
        let arrow_top = app.arrow_edge == ArrowEdge::Top;
        let body_top = if arrow_top {
            POPUP_ARROW_HEIGHT as f32
        } else {
            0.0
        };
        let body_bottom = body_top + CONTENT_HEIGHT as f32;
        let bg = if app.theme == Theme::Light {
            &self.palette.white
        } else {
            &self.palette.black
        };

        self.target.FillRectangle(
            &rect(0.0, 0.0, CONTENT_WIDTH as f32, MAIN_WINDOW_HEIGHT as f32),
            bg,
        );
        match app.phase {
            // UI-156: the tagging view owns this gradient from its initial
            // Ready state onward. Listening and Auto use the same colors;
            // result and NoMatch panels are separate overlays below.
            MainPhase::Ready
            | MainPhase::Starting { .. }
            | MainPhase::Cancelling { .. }
            | MainPhase::Error { .. }
            | MainPhase::Timeout { .. }
            | MainPhase::Matched { .. } => {
                if header_fill_scope(app.arrow_edge) == HeaderFillScope::HeaderAndArrow {
                    self.fill_gradient_geometry(
                        &self.popup_header(app)?,
                        &self.palette.listening,
                        body_top,
                    )?;
                } else {
                    self.fill_gradient_rect(
                        &rect(
                            0.0,
                            body_top,
                            CONTENT_WIDTH as f32,
                            body_top + TOP_HEIGHT as f32,
                        ),
                        &self.palette.listening_bottom,
                        body_top,
                    )?;
                }
            }
            MainPhase::Listening { .. } => {
                if header_fill_scope(app.arrow_edge) == HeaderFillScope::HeaderAndArrow {
                    self.fill_gradient_geometry(
                        &self.popup_header(app)?,
                        &self.palette.listening,
                        body_top,
                    )?;
                } else {
                    self.fill_gradient_rect(
                        &rect(
                            0.0,
                            body_top,
                            CONTENT_WIDTH as f32,
                            body_top + TOP_HEIGHT as f32,
                        ),
                        &self.palette.listening_bottom,
                        body_top,
                    )?;
                }
            }
            MainPhase::NoMatch { .. } | MainPhase::Offline { .. } => {
                let result_gradient = if app.arrow_edge == ArrowEdge::Top {
                    &self.palette.result_top
                } else {
                    &self.palette.result_bottom
                };
                if header_fill_scope(app.arrow_edge) == HeaderFillScope::HeaderAndArrow {
                    self.fill_gradient_geometry(
                        &self.popup_header(app)?,
                        result_gradient,
                        body_top,
                    )?;
                } else {
                    self.fill_gradient_rect(
                        &rect(
                            0.0,
                            body_top,
                            CONTENT_WIDTH as f32,
                            body_top + TOP_HEIGHT as f32,
                        ),
                        result_gradient,
                        body_top,
                    )?;
                }
            }
        }

        let history_top = body_top + TOP_HEIGHT as f32;
        let history_rect = rect(
            0.0,
            history_top,
            CONTENT_WIDTH as f32,
            history_top + HISTORY_HEIGHT as f32,
        );
        let clip = rect(0.0, history_top, CONTENT_WIDTH as f32, body_bottom);
        self.target.PushAxisAlignedClip(
            &clip,
            windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        );
        let now = Instant::now();
        if app.history.is_empty()
            || app.history_transition.is_some_and(|transition| {
                matches!(
                    transition.event,
                    HistoryMutationEvent::Insert {
                        previous_count: 0,
                        ..
                    }
                )
            })
        {
            self.draw_skeleton(history_rect);
        }
        self.draw_history(app, history_top, now)?;
        self.target.PopAxisAlignedClip();

        match &app.phase {
            MainPhase::Ready => self.draw_ready(app, body_top),
            MainPhase::Starting { mode, .. } => {
                self.draw_capture_pending(body_top, *mode, false);
                self.draw_cancel_button(body_top, app.arrow_edge);
            }
            MainPhase::Cancelling { mode, .. } => {
                self.draw_capture_pending(body_top, *mode, true);
                self.draw_cancel_button(body_top, app.arrow_edge);
            }
            MainPhase::Listening { mode, .. } => {
                self.draw_listening_status(app, body_top, *mode);
                self.draw_cancel_button(body_top, app.arrow_edge);
            }
            MainPhase::Matched { ref track, .. } => {
                self.draw_status_text(app, body_top);
                if let Some(top) = result_panel_draw_top {
                    self.with_result_header_clip(body_top, || {
                        self.draw_result_panel(app, track, top, app.arrow_edge)
                    })?;
                }
            }
            MainPhase::NoMatch { .. } => {
                if let Some(top) = result_panel_draw_top {
                    self.with_result_header_clip(body_top, || {
                        self.draw_no_match_panel(top, false, app.arrow_edge)
                    })?;
                }
            }
            MainPhase::Offline { .. } => {
                if let Some(top) = result_panel_draw_top {
                    self.with_result_header_clip(body_top, || {
                        self.draw_no_match_panel(top, true, app.arrow_edge)
                    })?;
                }
            }
            MainPhase::Error { ref message, .. } => {
                self.draw_ready(app, body_top);
                self.draw_error_banner(message, body_top);
            }
            MainPhase::Timeout { .. } => self.draw_timeout_dialog(body_top),
        }
        Ok(())
    }

    /// Returns one cached union geometry for the rounded panel and its tray
    /// arrow. A single outer contour keeps the translucent DWM edge continuous
    /// and avoids independently rasterized edges where the arrow meets panel.
    unsafe fn popup_outline(&self, app: &AppState) -> windows::core::Result<ID2D1Geometry> {
        let half = 10.0f32;
        let center = app
            .arrow_center
            .clamp(half + 4.0, CONTENT_WIDTH as f32 - half - 4.0);
        let key = (app.arrow_edge, center.to_bits());
        if let Some((edge, center_bits, geometry)) = self.popup_outline.borrow().as_ref() {
            if *edge == key.0 && *center_bits == key.1 {
                return Ok(geometry.clone());
            }
        }

        let body_top = if app.arrow_edge == ArrowEdge::Top {
            POPUP_ARROW_HEIGHT as f32
        } else {
            0.0
        };
        let body_bottom = body_top + CONTENT_HEIGHT as f32;
        let rounded = self.factory.CreateRoundedRectangleGeometry(
            &windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                rect: rect(0.0, body_top, CONTENT_WIDTH as f32, body_bottom),
                radiusX: POPUP_CORNER_RADIUS as f32,
                radiusY: POPUP_CORNER_RADIUS as f32,
            },
        )?;
        let arrow = self.popup_arrow_geometry(app.arrow_edge, center)?;

        let union = self.factory.CreatePathGeometry()?;
        let union_sink = union.Open()?;
        rounded.CombineWithGeometry(&arrow, D2D1_COMBINE_MODE_UNION, None, 0.25, &union_sink)?;
        union_sink.Close()?;
        let geometry: ID2D1Geometry = union.cast()?;
        *self.popup_outline.borrow_mut() = Some((key.0, key.1, geometry.clone()));
        Ok(geometry)
    }

    unsafe fn popup_header(&self, app: &AppState) -> windows::core::Result<ID2D1Geometry> {
        let half = 10.0f32;
        let center = app
            .arrow_center
            .clamp(half + 4.0, CONTENT_WIDTH as f32 - half - 4.0);
        let key = (app.arrow_edge, center.to_bits());
        if let Some((edge, center_bits, geometry)) = self.popup_header.borrow().as_ref() {
            if *edge == key.0 && *center_bits == key.1 {
                return Ok(geometry.clone());
            }
        }

        let body_top = if app.arrow_edge == ArrowEdge::Top {
            POPUP_ARROW_HEIGHT as f32
        } else {
            0.0
        };
        let header = self.factory.CreatePathGeometry()?;
        let header_sink = header.Open()?;
        header_sink.BeginFigure(
            Vector2 {
                X: 0.0,
                Y: body_top,
            },
            D2D1_FIGURE_BEGIN_FILLED,
        );
        header_sink.AddLine(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: body_top,
        });
        header_sink.AddLine(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: body_top + TOP_HEIGHT as f32,
        });
        header_sink.AddLine(Vector2 {
            X: 0.0,
            Y: body_top + TOP_HEIGHT as f32,
        });
        header_sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        header_sink.Close()?;

        // Merge the colored header and arrow so their shared edge is
        // rasterized only once. The popup layer applies the outer contour once.
        let header_with_arrow =
            if header_fill_scope(app.arrow_edge) == HeaderFillScope::HeaderAndArrow {
                let arrow = self.popup_arrow_geometry(app.arrow_edge, center)?;
                self.combine_geometry(&header, &arrow, D2D1_COMBINE_MODE_UNION)?
            } else {
                header.cast()?
            };
        *self.popup_header.borrow_mut() = Some((key.0, key.1, header_with_arrow.clone()));
        Ok(header_with_arrow)
    }

    unsafe fn popup_arrow_geometry(
        &self,
        edge: ArrowEdge,
        center: f32,
    ) -> windows::core::Result<ID2D1Geometry> {
        let half = 10.0f32;
        let (tip_y, base_y) = if edge == ArrowEdge::Top {
            (0.0, POPUP_ARROW_HEIGHT as f32)
        } else {
            (
                CONTENT_HEIGHT as f32 + POPUP_ARROW_HEIGHT as f32,
                CONTENT_HEIGHT as f32,
            )
        };
        let arrow = self.factory.CreatePathGeometry()?;
        let sink = arrow.Open()?;
        sink.BeginFigure(
            Vector2 {
                X: center - half,
                Y: base_y,
            },
            D2D1_FIGURE_BEGIN_FILLED,
        );
        sink.AddLine(Vector2 {
            X: center,
            Y: tip_y,
        });
        sink.AddLine(Vector2 {
            X: center + half,
            Y: base_y,
        });
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        arrow.cast()
    }

    unsafe fn combine_geometry(
        &self,
        first: &ID2D1Geometry,
        second: &ID2D1Geometry,
        mode: windows::Win32::Graphics::Direct2D::D2D1_COMBINE_MODE,
    ) -> windows::core::Result<ID2D1Geometry> {
        let combined = self.factory.CreatePathGeometry()?;
        let sink = combined.Open()?;
        first.CombineWithGeometry(second, mode, None, 0.25, &sink)?;
        sink.Close()?;
        combined.cast()
    }

    unsafe fn draw_ready(&self, app: &AppState, body_top: f32) {
        self.draw_status_text(app, body_top);
        self.draw_settings_icon(body_top, app.arrow_edge);
    }

    unsafe fn draw_status_text(&self, app: &AppState, body_top: f32) {
        if self.status_text_composition_enabled {
            return;
        }
        let area = rect(
            8.0,
            body_top + 141.0,
            CONTENT_WIDTH as f32 - 8.0,
            body_top + 160.0,
        );
        let Some(status) = app.status_alternator else {
            self.text(
                localized_ui_text(UiTextKey::Ready, ui_language()),
                area,
                &self.text.centered_title,
                &self.palette.white_ink,
            );
            return;
        };
        let language = ui_language();
        let (first, second) = match status.cycle {
            StatusCycle::Ready => (
                localized_ui_text(UiTextKey::Ready, language),
                localized_ui_text(UiTextKey::AutoStartStatus, language),
            ),
            StatusCycle::Auto => (
                localized_ui_text(UiTextKey::AutoOn, language),
                localized_ui_text(UiTextKey::AutoTurnOff, language),
            ),
        };
        let current = if status.first { first } else { second };
        let Some(started_at) = status.transition_started_at else {
            self.text(
                current,
                area,
                &self.text.centered_title,
                &self.palette.white_ink,
            );
            return;
        };
        let progress = status_text_progress(started_at, Instant::now());
        let previous = if status.first { second } else { first };
        let line_height = area.bottom - area.top;
        self.target.PushAxisAlignedClip(
            &area,
            windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        );
        self.text(
            previous,
            rect(
                area.left,
                area.top - line_height * progress,
                area.right,
                area.bottom - line_height * progress,
            ),
            &self.text.centered_title,
            &self.palette.white_ink,
        );
        self.text(
            current,
            rect(
                area.left,
                area.top + line_height * (1.0 - progress),
                area.right,
                area.bottom + line_height * (1.0 - progress),
            ),
            &self.text.centered_title,
            &self.palette.white_ink,
        );
        self.target.PopAxisAlignedClip();
    }

    unsafe fn draw_listening_status(&self, app: &AppState, body_top: f32, mode: CaptureMode) {
        if mode == CaptureMode::Auto {
            self.draw_status_text(app, body_top);
            return;
        }
        if self.status_text_composition_enabled {
            return;
        }
        self.text(
            localized_ui_text(UiTextKey::Listening, ui_language()),
            rect(
                8.0,
                body_top + 141.0,
                CONTENT_WIDTH as f32 - 8.0,
                body_top + 160.0,
            ),
            &self.text.centered_title,
            &self.palette.white_ink,
        );
    }

    unsafe fn draw_capture_pending(&self, body_top: f32, mode: CaptureMode, cancelling: bool) {
        let key = if cancelling {
            UiTextKey::Cancelling
        } else {
            UiTextKey::Starting
        };
        let label = if mode == CaptureMode::Auto && !cancelling {
            "Auto Shazam"
        } else {
            localized_ui_text(key, ui_language())
        };
        self.text(
            label,
            rect(
                10.0,
                body_top + 142.0,
                CONTENT_WIDTH as f32 - 10.0,
                body_top + 170.0,
            ),
            &self.text.centered_title,
            &self.palette.white_ink,
        );
    }

    unsafe fn draw_cancel_button(&self, body_top: f32, edge: ArrowEdge) {
        let [left, top, right, bottom] = native_icon_layout(body_top, edge).cancel_visual;
        let destination = rect(left, top, right, bottom);
        self.target.DrawBitmap(
            &self.white_close_icon,
            Some(&destination),
            1.0,
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            None,
        );
    }

    unsafe fn draw_settings_icon(&self, body_top: f32, edge: ArrowEdge) {
        let [left, top, right, bottom] = native_icon_layout(body_top, edge).settings_visual;
        let destination = rect(left, top, right, bottom);
        self.target.DrawBitmap(
            &self.settings_icon,
            Some(&destination),
            1.0,
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            None,
        );
    }

    unsafe fn draw_skeleton(&self, history: D2D_RECT_F) {
        for top in EMPTY_HISTORY_ROW_TOPS {
            let row_top = history.top + top;
            let art = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                rect: rect(10.0, row_top + 13.0, 75.0, row_top + 78.0),
                radiusX: 4.0,
                radiusY: 4.0,
            };
            self.target
                .FillRoundedRectangle(&art, &self.palette.skeleton);
            self.fill_rect(
                rect(85.0, row_top + 25.0, 257.0, row_top + 36.0),
                &self.palette.skeleton,
            );
            self.fill_rect(
                rect(85.0, row_top + 45.0, 224.0, row_top + 54.0),
                &self.palette.skeleton,
            );
            self.fill_rect(
                rect(85.0, row_top + 63.0, 161.0, row_top + 70.0),
                &self.palette.skeleton,
            );
            self.fill_rect(
                rect(
                    10.0,
                    row_top + 92.0,
                    CONTENT_WIDTH as f32 - 10.0,
                    row_top + 93.0,
                ),
                &self.palette.separator,
            );
        }
        self.target
            .FillRectangle(&history, &self.palette.empty_overlay);
    }

    unsafe fn draw_track_artwork(
        &self,
        app: &AppState,
        track: &Track,
        destination: D2D_RECT_F,
        side: f32,
    ) -> bool {
        let Some((source, pixels)) = app.artwork_resources.get(&track.display_id) else {
            return false;
        };
        let mut bitmaps = self.artwork_bitmaps.borrow_mut();
        let replace_cached = bitmaps.get(&track.display_id).is_none_or(|cached| {
            should_replace_cached_artwork(
                Some((cached.source, cached.fingerprint)),
                *source,
                pixels.fingerprint,
            )
        });
        if replace_cached {
            let properties = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let Ok(bitmap) = self.target.CreateBitmap(
                D2D_SIZE_U {
                    width: pixels.width,
                    height: pixels.height,
                },
                Some(pixels.bgra.as_ptr().cast()),
                pixels.width.saturating_mul(4),
                &properties,
            ) else {
                return false;
            };
            let bytes = pixels.bgra.len();
            bitmaps.insert(
                track.display_id,
                CachedArtworkBitmap {
                    source: *source,
                    width: pixels.width,
                    height: pixels.height,
                    byte_len: bytes,
                    fingerprint: pixels.fingerprint,
                    bitmap,
                },
            );
            const BITMAP_CACHE_LIMIT: usize = 24 * 1024 * 1024;
            while bitmaps
                .values()
                .map(|cached| cached.byte_len)
                .sum::<usize>()
                > BITMAP_CACHE_LIMIT
            {
                let oldest = bitmaps
                    .keys()
                    .copied()
                    .filter(|display_id| *display_id != track.display_id)
                    .min();
                let Some(oldest) = oldest else { break };
                bitmaps.remove(&oldest);
            }
        }
        let Some(cached) = bitmaps.get(&track.display_id) else {
            return false;
        };
        let image_rect = aspect_fit_rect(destination, cached.width, cached.height);
        self.draw_artwork_bitmap(&cached.bitmap, destination, image_rect, side)
    }

    unsafe fn draw_fallback_match_artwork(&self, theme: Theme, frame: D2D_RECT_F) {
        let Some((light, dark)) = self.fallback_match_artwork.as_ref() else {
            return;
        };
        let bitmap = match theme {
            Theme::Light => light,
            Theme::Dark => dark,
        };
        // CoreUI stores this match placeholder as a 262px @2x image (131pt).
        // NSImageCell uses AspectFit and centers that intrinsic size in the
        // 150pt album-art view; retain the transparent margin and alpha mask.
        let image_rect = fallback_match_artwork_rect(frame);
        let _ = self.draw_artwork_bitmap(bitmap, frame, image_rect, 150.0);
    }

    unsafe fn draw_artwork_bitmap(
        &self,
        bitmap: &ID2D1Bitmap,
        frame: D2D_RECT_F,
        image_rect: D2D_RECT_F,
        side: f32,
    ) -> bool {
        let Some(layer) = self.artwork_layer.as_ref() else {
            return false;
        };
        let mask = &self.artwork_geometries[if side > 100.0 { 0 } else { 1 }].1;
        let parameters = windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS {
            contentBounds: frame,
            geometricMask: std::mem::ManuallyDrop::new(Some(mask.clone())),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2 {
                M11: 1.0,
                M22: 1.0,
                M31: frame.left,
                M32: frame.top,
                ..Default::default()
            },
            opacity: 1.0,
            opacityBrush: std::mem::ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS_NONE,
        };
        self.target.PushLayer(&parameters, layer);
        self.target.DrawBitmap(
            bitmap,
            Some(&image_rect),
            1.0,
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            None,
        );
        self.target.PopLayer();
        true
    }

    unsafe fn draw_history(
        &self,
        app: &AppState,
        history_top: f32,
        now: Instant,
    ) -> windows::core::Result<()> {
        for (index, track) in app.history.iter().enumerate() {
            let top = history_row_top(
                index,
                app.history_transition,
                history_top,
                app.scroll_offset,
                now,
            );
            if top + HISTORY_ROW_HEIGHT < history_top || top > history_top + HISTORY_HEIGHT as f32 {
                continue;
            }
            let row = rect(0.0, top, CONTENT_WIDTH as f32, top + HISTORY_ROW_HEIGHT);
            let text_metrics = self.history_text_metrics(track)?;
            let text_rects = history_text_rects(
                top,
                text_metrics.title.height,
                text_metrics.artist.height,
                text_metrics.date.height,
            );
            if app.hovered_row == Some(index) {
                self.fill_rect(row, &self.palette.skeleton);
            }
            let artwork = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                // UI-144 image padding belongs to primary-action icons, not album art.
                rect: history_artwork_frame(top),
                radiusX: 4.0,
                radiusY: 4.0,
            };
            self.target
                .FillRoundedRectangle(&artwork, &self.palette.result_top);
            if !self.draw_track_artwork(app, track, history_artwork_frame(top), 65.0) {
                self.text(
                    "\u{266b}",
                    history_artwork_frame(top),
                    &self.text.centered_title,
                    &self.palette.white_ink,
                );
            }
            self.text(
                &track.title,
                text_rects.title,
                &self.text.title,
                &self.palette.title,
            );
            self.text(
                &track.artist,
                text_rects.artist,
                &self.text.subtitle,
                &self.palette.subtitle,
            );
            self.text(
                &track.recognized_date_label,
                text_rects.date,
                &self.text.date,
                &self.palette.subtitle,
            );
            self.fill_rect(
                rect(10.0, top + 88.0, 370.0, top + HISTORY_ROW_HEIGHT),
                &self.palette.separator,
            );
            let _recognized_age = track.recognized_at.elapsed();
        }
        Ok(())
    }

    unsafe fn draw_result_panel(
        &self,
        app: &AppState,
        track: &Track,
        top: f32,
        edge: ArrowEdge,
    ) -> windows::core::Result<()> {
        let panel = rect(0.0, top, CONTENT_WIDTH as f32, top + TOP_HEIGHT as f32);
        let gradient = if edge == ArrowEdge::Top {
            &self.palette.result_top
        } else {
            &self.palette.result_bottom
        };
        self.fill_gradient_rect(&panel, gradient, top)?;
        let details = self.result_details_metrics(track)?;
        let detail_rects =
            result_details_rects(top, details.title_height, details.artist_height, true);
        // The image view is 150×150pt and clips at radius5. The native
        // placeholder has its own centered 131pt intrinsic image size and
        // alpha, so do not paint an opaque square under it.
        let artwork_frame = result_artwork_rect(top, edge);
        if !self.draw_track_artwork(app, track, artwork_frame, 150.0) {
            self.draw_fallback_match_artwork(app.theme, artwork_frame);
        }
        self.text(
            &track.title,
            detail_rects.title,
            &self.text.result_title,
            &self.palette.result_label,
        );
        self.text(
            &track.artist,
            detail_rects.artist,
            &self.text.result_artist,
            &self.palette.result_label,
        );
        // UI-233 keeps a fixed Apple Music CTA visible. The current UI model
        // has no verified destination, so show it disabled and keep its hit
        // rectangle separate from the Shazam page link.
        self.text(
            localized_ui_text(UiTextKey::AppleMusicAction, ui_language()),
            detail_rects.action,
            &self.text.result_action,
            &self.palette.subtitle,
        );
        self.draw_result_close_icon(top, edge);
        Ok(())
    }

    unsafe fn draw_no_match_panel(
        &self,
        top: f32,
        offline: bool,
        edge: ArrowEdge,
    ) -> windows::core::Result<()> {
        let panel = rect(0.0, top, CONTENT_WIDTH as f32, top + TOP_HEIGHT as f32);
        let gradient = if edge == ArrowEdge::Top {
            &self.palette.result_top
        } else {
            &self.palette.result_bottom
        };
        self.fill_gradient_rect(&panel, gradient, top)?;
        let (title, body) = no_match_copy(offline, ui_language());
        self.text(
            title,
            rect(15.0, top + 55.0, 365.0, top + 84.0),
            &self.text.centered_heavy,
            &self.palette.title,
        );
        self.text(
            body,
            rect(15.0, top + 84.0, 365.0, top + 110.0),
            &self.text.centered_body,
            &self.palette.subtitle,
        );
        let button = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
            rect: rect(90.0, top + 120.0, 290.0, top + 150.0),
            radiusX: 16.0,
            radiusY: 16.0,
        };
        self.target
            .FillRoundedRectangle(&button, &self.palette.accent);
        self.text(
            localized_ui_text(UiTextKey::TryAgain, ui_language()),
            rect(90.0, top + 120.0, 290.0, top + 150.0),
            &self.text.button,
            &self.palette.white_ink,
        );
        self.draw_result_close_icon(top, edge);
        Ok(())
    }

    unsafe fn draw_result_close_icon(&self, body_top: f32, edge: ArrowEdge) {
        let [left, top, right, bottom] = native_icon_layout(body_top, edge).result_close_visual;
        let destination = rect(left, top, right, bottom);
        self.target.DrawBitmap(
            &self.result_close_icon,
            Some(&destination),
            1.0,
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            None,
        );
    }

    unsafe fn draw_error_banner(&self, message: &str, body_top: f32) {
        let banner = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
            rect: rect(40.0, body_top + 72.0, 340.0, body_top + 105.0),
            radiusX: 8.0,
            radiusY: 8.0,
        };
        self.target
            .FillRoundedRectangle(&banner, &self.palette.button);
        self.text(
            message,
            rect(50.0, body_top + 73.0, 330.0, body_top + 104.0),
            &self.text.centered_body,
            &self.palette.white_ink,
        );
    }

    unsafe fn draw_timeout_dialog(&self, body_top: f32) {
        let (description, dismiss, restart) = timeout_copy(ui_language());
        let dialog = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
            rect: rect(24.0, body_top + 48.0, 356.0, body_top + 168.0),
            radiusX: 10.0,
            radiusY: 10.0,
        };
        self.target
            .FillRoundedRectangle(&dialog, &self.palette.white);
        self.text(
            description,
            rect(42.0, body_top + 60.0, 338.0, body_top + 116.0),
            &self.text.centered_body,
            &self.palette.title,
        );
        let restart_button = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
            rect: rect(70.0, body_top + 126.0, 190.0, body_top + 160.0),
            radiusX: 7.0,
            radiusY: 7.0,
        };
        self.target
            .FillRoundedRectangle(&restart_button, &self.palette.accent);
        let dismiss_button = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
            rect: rect(200.0, body_top + 126.0, 310.0, body_top + 160.0),
            radiusX: 7.0,
            radiusY: 7.0,
        };
        self.target
            .DrawRoundedRectangle(&dismiss_button, &self.palette.subtitle, 1.0, None);
        self.text(
            restart,
            rect(70.0, body_top + 126.0, 190.0, body_top + 160.0),
            &self.text.button,
            &self.palette.white_ink,
        );
        self.text(
            dismiss,
            rect(200.0, body_top + 126.0, 310.0, body_top + 160.0),
            &self.text.button,
            &self.palette.title,
        );
    }

    unsafe fn draw_about(&self, app: &AppState) -> windows::core::Result<()> {
        let background = if app.theme == Theme::Light {
            &self.palette.white
        } else {
            &self.palette.black
        };
        let outer = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
            rect: rect(0.0, 0.0, ABOUT_WIDTH as f32, ABOUT_HEIGHT as f32),
            radiusX: 12.0,
            radiusY: 12.0,
        };
        self.target.FillRoundedRectangle(&outer, background);
        let logo = D2D1_ELLIPSE {
            point: Vector2 {
                X: ABOUT_WIDTH as f32 * 0.5,
                Y: 105.0,
            },
            radiusX: 46.0,
            radiusY: 46.0,
        };
        self.target.FillEllipse(&logo, &self.palette.button);
        self.text(
            "S",
            rect(
                ABOUT_WIDTH as f32 * 0.5 - 30.0,
                70.0,
                ABOUT_WIDTH as f32 * 0.5 + 30.0,
                140.0,
            ),
            &self.text.logo,
            &self.palette.white,
        );
        self.text(
            "OpenShazam",
            rect(40.0, 160.0, ABOUT_WIDTH as f32 - 40.0, 205.0),
            &self.text.centered_title,
            &self.palette.title,
        );
        self.text(
            "Windows ネイティブUI / Rust",
            rect(40.0, 205.0, ABOUT_WIDTH as f32 - 40.0, 235.0),
            &self.text.centered_body,
            &self.palette.subtitle,
        );
        self.text(
            "研究用の独立実装。Shazam Inc.とは無関係です。",
            rect(40.0, 238.0, ABOUT_WIDTH as f32 - 40.0, 276.0),
            &self.text.centered_body,
            &self.palette.subtitle,
        );
        Ok(())
    }

    unsafe fn draw_microphone_info(&self, app: &AppState) -> windows::core::Result<()> {
        let background = if app.theme == Theme::Light {
            &self.palette.result_top
        } else {
            &self.palette.listening
        };
        self.target.FillRectangle(
            &rect(0.0, 0.0, MIC_INFO_WIDTH as f32, MIC_INFO_HEIGHT as f32),
            background,
        );
        self.text(
            "マイク入力",
            rect(15.0, 20.0, 365.0, 55.0),
            &self.text.centered_heavy,
            &self.palette.title,
        );
        self.text(
            "現在のWindows版はシステム再生音声のloopbackに対応しています。",
            rect(15.0, 60.0, 365.0, 116.0),
            &self.text.centered_body,
            &self.palette.subtitle,
        );
        self.text(
            "クリックで戻る",
            rect(15.0, 150.0, 365.0, 178.0),
            &self.text.centered_body,
            &self.palette.title,
        );
        Ok(())
    }
}

unsafe fn create_palette(
    target: &ID2D1RenderTarget,
    theme: Theme,
    system_foreground: Color,
) -> windows::core::Result<Palette> {
    let (subtitle, skeleton, separator) = match theme {
        Theme::Light => (
            d2d_color(148, 148, 148, 1.0),
            d2d_color(230, 234, 237, 0.75),
            d2d_color(0, 0, 0, 0.098),
        ),
        Theme::Dark => (
            d2d_color(212, 212, 212, 1.0),
            d2d_color(55, 66, 73, 0.8),
            d2d_color(255, 255, 255, 0.098),
        ),
    };
    let (background_red, background_green, background_blue) = popup_base_color(theme);
    let background = d2d_color(background_red, background_green, background_blue, 1.0);
    let white = solid(target, background)?;
    let white_ink = solid(target, d2d_color(255, 255, 255, 1.0))?;
    let black = solid(target, d2d_color(30, 30, 30, 1.0))?;
    let title = solid(target, history_title_color(theme))?;
    let subtitle = solid(target, subtitle)?;
    // UI-223: Result title and artist follow the active system foreground;
    // history title/subtitle brushes remain independent appearance tokens.
    let result_label = solid(target, d2d_color_from_system(system_foreground))?;
    let accent_color = if theme == Theme::Light {
        d2d_color(0, 136, 255, 1.0)
    } else {
        d2d_color(80, 80, 80, 1.0)
    };
    let accent = solid(target, accent_color)?;
    let button = solid(target, d2d_color(0, 136, 255, 1.0))?;
    let separator = solid(target, separator)?;
    let skeleton = solid(target, skeleton)?;
    let result_top_colors =
        result_gradient_colors(theme, ArrowEdge::Top).map(|(r, g, b)| d2d_color(r, g, b, 1.0));
    let result_bottom_colors =
        result_gradient_colors(theme, ArrowEdge::Bottom).map(|(r, g, b)| d2d_color(r, g, b, 1.0));
    let result_top = gradient(
        target,
        result_top_colors,
        Vector2 { X: 190.0, Y: 0.0 },
        Vector2 {
            X: 190.0,
            Y: TOP_HEIGHT as f32,
        },
    )?;
    let result_bottom = gradient(
        target,
        result_bottom_colors,
        Vector2 { X: 190.0, Y: 0.0 },
        Vector2 {
            X: 190.0,
            Y: TOP_HEIGHT as f32,
        },
    )?;
    let listening = match theme {
        Theme::Light => gradient(
            target,
            [d2d_color(0, 103, 255, 1.0), d2d_color(3, 187, 255, 1.0)],
            Vector2 { X: 190.0, Y: 0.0 },
            Vector2 {
                X: 190.0,
                Y: TOP_HEIGHT as f32,
            },
        )?,
        Theme::Dark => gradient(
            target,
            [d2d_color(0, 0, 0, 1.0), d2d_color(55, 66, 73, 1.0)],
            Vector2 { X: 190.0, Y: 0.0 },
            Vector2 {
                X: 190.0,
                Y: TOP_HEIGHT as f32,
            },
        )?,
    };
    // The taskbar-facing lower arrow has the native popup orientation reversed:
    // the cyan stop begins at the top of the header and blue ends at the history edge.
    let listening_bottom = match theme {
        Theme::Light => gradient(
            target,
            [d2d_color(3, 187, 255, 1.0), d2d_color(0, 103, 255, 1.0)],
            Vector2 { X: 190.0, Y: 0.0 },
            Vector2 {
                X: 190.0,
                Y: TOP_HEIGHT as f32,
            },
        )?,
        Theme::Dark => gradient(
            target,
            [d2d_color(55, 66, 73, 1.0), d2d_color(0, 0, 0, 1.0)],
            Vector2 { X: 190.0, Y: 0.0 },
            Vector2 {
                X: 190.0,
                Y: TOP_HEIGHT as f32,
            },
        )?,
    };
    let overlay_colors = match theme {
        Theme::Light => [
            d2d_color(255, 255, 255, 0.90),
            d2d_color(255, 255, 255, 0.0),
        ],
        Theme::Dark => [d2d_color(0, 0, 0, 0.90), d2d_color(0, 0, 0, 0.0)],
    };
    let empty_overlay = gradient(
        target,
        overlay_colors,
        Vector2 {
            X: 190.0,
            Y: TOP_HEIGHT as f32,
        },
        Vector2 {
            X: 190.0,
            Y: TOP_HEIGHT as f32 + HISTORY_HEIGHT as f32,
        },
    )?;
    Ok(Palette {
        white,
        white_ink,
        black,
        title,
        subtitle,
        result_label,
        accent,
        button,
        separator,
        skeleton,
        result_top,
        result_bottom,
        listening,
        listening_bottom,
        empty_overlay,
    })
}

unsafe fn solid(
    target: &ID2D1RenderTarget,
    color: D2D1_COLOR_F,
) -> windows::core::Result<ID2D1SolidColorBrush> {
    target.CreateSolidColorBrush(&color, None)
}

unsafe fn gradient(
    target: &ID2D1RenderTarget,
    colors: [D2D1_COLOR_F; 2],
    start: Vector2,
    end: Vector2,
) -> windows::core::Result<ID2D1LinearGradientBrush> {
    let stops = [
        D2D1_GRADIENT_STOP {
            position: 0.0,
            color: colors[0],
        },
        D2D1_GRADIENT_STOP {
            position: 1.0,
            color: colors[1],
        },
    ];
    let collection =
        target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
    let properties = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
        startPoint: start,
        endPoint: end,
    };
    target.CreateLinearGradientBrush(&properties, None, &collection)
}

unsafe fn create_text_styles(factory: &IDWriteFactory) -> windows::core::Result<TextStyles> {
    Ok(TextStyles {
        title: create_text_format(
            factory,
            14.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?,
        subtitle: create_text_format(
            factory,
            12.0,
            DWRITE_FONT_WEIGHT_DEMI_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?,
        date: create_text_format(
            factory,
            10.0,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?,
        result_title: create_text_format(
            factory,
            20.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?,
        result_artist: create_text_format(
            factory,
            14.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?,
        result_action: create_text_format(
            factory,
            12.0,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?,
        button: create_text_format(
            factory,
            13.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?,
        centered_body: create_text_format(
            factory,
            12.0,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?,
        centered_title: create_text_format(
            factory,
            16.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?,
        centered_heavy: create_text_format(
            factory,
            18.0,
            DWRITE_FONT_WEIGHT_HEAVY,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?,
        trailing_small: create_text_format(
            factory,
            12.0,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_TRAILING,
        )?,
        logo: create_text_format(
            factory,
            58.0,
            DWRITE_FONT_WEIGHT_HEAVY,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?,
    })
}

fn phase_elapsed(phase: &MainPhase) -> f64 {
    let start = match phase {
        MainPhase::Ready => return 0.0,
        MainPhase::Starting { entered_at, .. } | MainPhase::Cancelling { entered_at, .. } => {
            *entered_at
        }
        MainPhase::Listening { started_at, .. } => *started_at,
        MainPhase::Matched { stage, .. } => match stage {
            ResultCardStage::Entering { started_at }
            | ResultCardStage::Holding { started_at }
            | ResultCardStage::Exiting { started_at, .. } => *started_at,
        },
        MainPhase::NoMatch { entered_at, .. } | MainPhase::Offline { entered_at, .. } => {
            *entered_at
        }
        MainPhase::Error { entered_at, .. } => *entered_at,
        MainPhase::Timeout { entered_at, .. } => *entered_at,
    };
    start.elapsed().as_secs_f64()
}

fn is_tray_activation_event(event: u16) -> bool {
    // With NOTIFYICON_VERSION_4 the shell reports pointer messages (including
    // WM_LBUTTONUP) as well as the semantic selection notification. Treating
    // both as toggles makes one physical click show and immediately hide the
    // popup. NIN_SELECT/NIN_KEYSELECT are the single activation events.
    event == windows::Win32::UI::Shell::NIN_SELECT as u16
        || event
            == (windows::Win32::UI::Shell::NIN_SELECT | windows::Win32::UI::Shell::NINF_KEY) as u16
}

fn should_show_initial_popup(startup_task_activation: bool) -> bool {
    !startup_task_activation
}

fn is_startup_task_activation() -> bool {
    AppInstance::GetActivatedEventArgs()
        .ok()
        .and_then(|args| args.Kind().ok())
        .is_some_and(|kind| kind == ActivationKind::StartupTask)
}

struct SingleInstanceMutex(HANDLE);

impl Drop for SingleInstanceMutex {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn acquire_single_instance_mutex() -> windows::core::Result<(SingleInstanceMutex, bool)> {
    acquire_named_single_instance_mutex(SINGLE_INSTANCE_MUTEX)
}

fn acquire_named_single_instance_mutex(
    name: &str,
) -> windows::core::Result<(SingleInstanceMutex, bool)> {
    let name = wide(name);
    let handle = unsafe { CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr()) };
    if handle.0.is_null() {
        return Err(Error::from_thread());
    }
    let already_running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    Ok((SingleInstanceMutex(handle), already_running))
}

fn activate_existing_instance() -> bool {
    let class_name = wide(CLASS_NAME);
    let title = wide("OpenShazam");
    for _ in 0..50 {
        let hwnd = unsafe {
            FindWindowW(PCWSTR(class_name.as_ptr()), PCWSTR(title.as_ptr())).unwrap_or_default()
        };
        if !hwnd.0.is_null() {
            return unsafe {
                PostMessageW(Some(hwnd), WM_SHOW_INITIAL_POPUP, WPARAM(0), LPARAM(0)).is_ok()
            };
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

unsafe fn create_window(
    app: *mut AppState,
    kind: WindowKind,
    title: &str,
    width: i32,
    height: i32,
    visible: bool,
) -> windows::core::Result<HWND> {
    let instance = GetModuleHandleW(None)?;
    let class_name = wide(CLASS_NAME);
    let title = wide(title);
    let window_data = Box::new(WindowData {
        app,
        hwnd: HWND::default(),
        kind,
        renderer: None,
    });
    let window_data_ptr = Box::into_raw(window_data);
    let mut style = WS_POPUP;
    if visible {
        style |= WS_VISIBLE;
    }
    let extended = match kind {
        WindowKind::Main => {
            WS_EX_TOOLWINDOW
                | WS_EX_LAYERED
                | windows::Win32::UI::WindowsAndMessaging::WS_EX_TOPMOST
        }
        _ => WS_EX_TOOLWINDOW,
    };
    let result = CreateWindowExW(
        extended,
        PCWSTR(class_name.as_ptr()),
        PCWSTR(title.as_ptr()),
        style,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        width,
        height,
        None,
        None,
        Some(instance.into()),
        Some(window_data_ptr.cast::<c_void>()),
    );
    match result {
        Ok(hwnd) => {
            (*window_data_ptr).hwnd = hwnd;
            Ok(hwnd)
        }
        Err(error) => {
            drop(Box::from_raw(window_data_ptr));
            Err(error)
        }
    }
}

unsafe fn add_tray_icon(hwnd: HWND) -> windows::core::Result<()> {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP | NIF_GUID,
        uCallbackMessage: WM_TRAYICON_MESSAGE,
        hIcon: LoadIconW(None, IDI_APPLICATION)?,
        guidItem: TRAY_ICON_GUID,
        ..Default::default()
    };
    let tip = wide("OpenShazam");
    for (dst, src) in data.szTip.iter_mut().zip(tip) {
        *dst = src;
    }
    if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
        return Err(Error::from_thread());
    }
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    if !Shell_NotifyIconW(NIM_SETVERSION, &data).as_bool() {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        return Err(Error::from_thread());
    }
    Ok(())
}

unsafe fn restore_tray_icon(hwnd: HWND) {
    // Explorer drops notification-area registrations when its taskbar process
    // restarts. Re-add the stable GUID and restore version 4 callback semantics.
    let _ = add_tray_icon(hwnd);
}

unsafe fn show_tray_context_menu(app: &mut AppState) {
    let hwnd = app.main_hwnd;
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    let settings = wide("Settings");
    let exit = wide("Exit OpenShazam");
    let _ = AppendMenuW(
        menu,
        MF_STRING,
        TRAY_MENU_SETTINGS,
        PCWSTR(settings.as_ptr()),
    );
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, TRAY_MENU_EXIT, PCWSTR(exit.as_ptr()));
    let _ = SetForegroundWindow(hwnd);
    let mut point = POINT::default();
    let _ = GetCursorPos(&mut point);
    let selected = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON,
        point.x,
        point.y,
        None,
        hwnd,
        None,
    );
    let _ = DestroyMenu(menu);
    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    let data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_GUID,
        guidItem: TRAY_ICON_GUID,
        ..Default::default()
    };
    let _ = Shell_NotifyIconW(NIM_SETFOCUS, &data);
    match selected.0 as usize {
        TRAY_MENU_SETTINGS => open_settings(app),
        TRAY_MENU_EXIT => {
            let _ = DestroyWindow(hwnd);
        }
        _ => {}
    }
}

fn queue_hidden_match_notification(app: &mut AppState, track: &Track) {
    let display_id = track.display_id;
    if let Some(pixels) = app
        .artwork_resources
        .get(&display_id)
        .map(|(_, pixels)| pixels.clone())
    {
        unsafe { notify_hidden_match(app, track, Some(&pixels)) };
    } else if app
        .active_artwork_requests
        .contains_key(&TrackIdentity { display_id })
    {
        // Wait for the worker so the first notification can use the album cover.
        app.pending_match_notifications
            .insert(display_id, track.clone());
    } else {
        unsafe { notify_hidden_match(app, track, None) };
    }
}

unsafe fn notify_hidden_match(app: &mut AppState, track: &Track, artwork: Option<&ArtworkPixels>) {
    let body = format!("{} — {}", track.title, track.artist);
    let notification_dpi = GetDpiForWindow(app.main_hwnd).max(96);
    let balloon_icon =
        artwork.and_then(
            |pixels| match create_notification_icon(pixels, notification_dpi) {
                Ok(icon) => Some(icon),
                Err(error) => {
                    eprintln!("OpenShazam could not create notification artwork icon: {error}");
                    None
                }
            },
        );
    if artwork.is_none() {
        eprintln!("OpenShazam notification artwork unavailable; using the system information icon");
    }
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: app.main_hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_INFO | NIF_GUID,
        dwInfoFlags: if balloon_icon.is_some() {
            NIIF_USER | NIIF_LARGE_ICON
        } else {
            NIIF_INFO
        },
        hBalloonIcon: balloon_icon.unwrap_or_default(),
        guidItem: TRAY_ICON_GUID,
        ..Default::default()
    };
    copy_notification_text(&mut data.szInfoTitle, "Shazam");
    copy_notification_text(&mut data.szInfo, &body);
    if Shell_NotifyIconW(NIM_MODIFY, &data).as_bool() {
        // Explorer sends the matching end event after the balloon is no longer
        // using this custom icon. Keep None entries to preserve event ordering.
        app.owned_balloon_icons.push_back(balloon_icon);
    } else if let Some(icon) = balloon_icon {
        let _ = DestroyIcon(icon);
    }
}

fn scale_notification_artwork_to_size(
    pixels: &ArtworkPixels,
    target_width: u32,
    target_height: u32,
) -> Option<Vec<u8>> {
    let source_width = usize::try_from(pixels.width).ok()?;
    let source_height = usize::try_from(pixels.height).ok()?;
    let target_width = usize::try_from(target_width).ok()?;
    let target_height = usize::try_from(target_height).ok()?;
    let expected_len = source_width.checked_mul(source_height)?.checked_mul(4)?;
    if target_width == 0
        || target_height == 0
        || source_width == 0
        || source_height == 0
        || pixels.bgra.len() != expected_len
    {
        return None;
    }
    let output_len = target_width.checked_mul(target_height)?.checked_mul(4)?;
    let target_aspect = target_width as f64 / target_height as f64;
    let source_aspect = source_width as f64 / source_height as f64;
    let (crop_width, crop_height) = if source_aspect > target_aspect {
        (source_height as f64 * target_aspect, source_height as f64)
    } else {
        (source_width as f64, source_width as f64 / target_aspect)
    };
    let crop_x = (source_width as f64 - crop_width) * 0.5;
    let crop_y = (source_height as f64 - crop_height) * 0.5;
    let mut output = vec![0u8; output_len];

    for y in 0..target_height {
        let source_y = (crop_y + (y as f64 + 0.5) * crop_height / target_height as f64 - 0.5)
            .clamp(0.0, source_height.saturating_sub(1) as f64);
        let y0 = source_y.floor() as usize;
        let y1 = (y0 + 1).min(source_height - 1);
        let fy = source_y - y0 as f64;
        for x in 0..target_width {
            let source_x = (crop_x + (x as f64 + 0.5) * crop_width / target_width as f64 - 0.5)
                .clamp(0.0, source_width.saturating_sub(1) as f64);
            let x0 = source_x.floor() as usize;
            let x1 = (x0 + 1).min(source_width - 1);
            let fx = source_x - x0 as f64;
            let samples = [
                (y0 * source_width + x0) * 4,
                (y0 * source_width + x1) * 4,
                (y1 * source_width + x0) * 4,
                (y1 * source_width + x1) * 4,
            ];
            let destination = (y * target_width + x) * 4;
            for channel in 0..4 {
                let top = f64::from(pixels.bgra[samples[0] + channel]) * (1.0 - fx)
                    + f64::from(pixels.bgra[samples[1] + channel]) * fx;
                let bottom = f64::from(pixels.bgra[samples[2] + channel]) * (1.0 - fx)
                    + f64::from(pixels.bgra[samples[3] + channel]) * fx;
                output[destination + channel] =
                    (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Some(output)
}

unsafe fn create_notification_icon(
    pixels: &ArtworkPixels,
    dpi: u32,
) -> windows::core::Result<windows::Win32::UI::WindowsAndMessaging::HICON> {
    // NIIF_LARGE_ICON requires hBalloonIcon to use the large system-icon size.
    // This process is per-monitor DPI aware, so query the metrics for the
    // notification window's current monitor instead of using the system-DPI
    // values. A mismatched icon size can make Explorer silently use NIIF_INFO.
    let width = GetSystemMetricsForDpi(SM_CXICON, dpi.max(96));
    let height = GetSystemMetricsForDpi(SM_CYICON, dpi.max(96));
    if !(1..=512).contains(&width) || !(1..=512).contains(&height) {
        return Err(Error::new(
            windows::core::HRESULT(0x8007_0057u32 as i32),
            "system notification icon dimensions are invalid",
        ));
    }
    let width = width as u32;
    let height = height as u32;
    let scaled = scale_notification_artwork_to_size(pixels, width, height).ok_or_else(|| {
        Error::new(
            windows::core::HRESULT(0x8007_0057u32 as i32),
            "notification artwork has invalid pixel dimensions",
        )
    })?;
    let dc = CreateCompatibleDC(None);
    if dc.0.is_null() {
        return Err(Error::from_thread());
    }

    let color_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut color_bits = std::ptr::null_mut();
    let color_bitmap = match CreateDIBSection(
        Some(dc),
        &color_info,
        DIB_RGB_COLORS,
        &mut color_bits,
        None,
        0,
    ) {
        Ok(bitmap) if !color_bits.is_null() => bitmap,
        Ok(bitmap) => {
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(dc);
            return Err(Error::from_thread());
        }
        Err(error) => {
            let _ = DeleteDC(dc);
            return Err(error);
        }
    };
    std::ptr::copy_nonoverlapping(scaled.as_ptr(), color_bits.cast::<u8>(), scaled.len());

    #[repr(C)]
    struct MonochromeBitmapInfo {
        header: BITMAPINFOHEADER,
        colors: [RGBQUAD; 2],
    }
    let mask_info = MonochromeBitmapInfo {
        header: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 1,
            biCompression: BI_RGB.0,
            biClrUsed: 2,
            ..Default::default()
        },
        colors: [
            RGBQUAD::default(),
            RGBQUAD {
                rgbBlue: 255,
                rgbGreen: 255,
                rgbRed: 255,
                rgbReserved: 0,
            },
        ],
    };
    let mut mask_bits = std::ptr::null_mut();
    let mask_bitmap = match CreateDIBSection(
        Some(dc),
        (&mask_info as *const MonochromeBitmapInfo).cast::<BITMAPINFO>(),
        DIB_RGB_COLORS,
        &mut mask_bits,
        None,
        0,
    ) {
        Ok(bitmap) if !mask_bits.is_null() => bitmap,
        Ok(bitmap) => {
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteObject(color_bitmap.into());
            let _ = DeleteDC(dc);
            return Err(Error::from_thread());
        }
        Err(error) => {
            let _ = DeleteObject(color_bitmap.into());
            let _ = DeleteDC(dc);
            return Err(error);
        }
    };
    let mask_stride = (width as usize).div_ceil(32) * 4;
    std::ptr::write_bytes(mask_bits.cast::<u8>(), 0, mask_stride * height as usize);
    let mask =
        std::slice::from_raw_parts_mut(mask_bits.cast::<u8>(), mask_stride * height as usize);
    for y in 0..height as usize {
        for x in 0..width as usize {
            if scaled[(y * width as usize + x) * 4 + 3] == 0 {
                let byte = y * mask_stride + x / 8;
                mask[byte] |= 0x80 >> (x % 8);
            }
        }
    }

    let icon_info = ICONINFO {
        fIcon: BOOL(1),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask_bitmap,
        hbmColor: color_bitmap,
    };
    let icon = CreateIconIndirect(&icon_info);
    let _ = DeleteObject(mask_bitmap.into());
    let _ = DeleteObject(color_bitmap.into());
    let _ = DeleteDC(dc);
    icon
}
fn copy_notification_text(destination: &mut [u16], value: &str) {
    destination.fill(0);
    let capacity = destination.len().saturating_sub(1);
    let mut encoded: Vec<u16> = value.encode_utf16().take(capacity).collect();
    if encoded
        .last()
        .is_some_and(|unit| (0xD800..=0xDBFF).contains(unit))
    {
        encoded.pop();
    }
    destination[..encoded.len()].copy_from_slice(&encoded);
}

unsafe fn delete_tray_icon(hwnd: HWND) {
    let data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_GUID,
        guidItem: TRAY_ICON_GUID,
        ..Default::default()
    };
    let _ = Shell_NotifyIconW(NIM_DELETE, &data);
}

unsafe fn anchor_popup(app: &mut AppState) {
    let hwnd = app.main_hwnd;
    let identifier = NOTIFYICONIDENTIFIER {
        cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        guidItem: TRAY_ICON_GUID,
    };
    let tray = Shell_NotifyIconGetRect(&identifier).ok();
    let dpi = GetDpiForWindow(hwnd).max(96);
    let scale = f64::from(dpi) / 96.0;
    let width = (MAIN_WINDOW_WIDTH * scale).round() as i32;
    let height = (MAIN_WINDOW_HEIGHT * scale).round() as i32;
    if let Some(tray_rect) = tray {
        let tray_center_x = (tray_rect.left + tray_rect.right) / 2;
        let monitor = MonitorFromRect(&tray_rect, MONITOR_DEFAULTTONEAREST);
        let mut monitor_info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let work_area = if GetMonitorInfoW(monitor, &mut monitor_info).as_bool() {
            monitor_info.rcWork
        } else {
            RECT {
                left: tray_rect.left,
                top: 0,
                right: tray_rect.right,
                bottom: tray_rect.bottom + height,
            }
        };
        let room_above = tray_rect.top - work_area.top;
        let room_below = work_area.bottom - tray_rect.bottom;
        let below = if room_below >= height {
            true
        } else if room_above >= height {
            false
        } else {
            room_below >= room_above
        };
        app.arrow_edge = if below {
            ArrowEdge::Top
        } else {
            ArrowEdge::Bottom
        };
        let raw_x = tray_center_x - width / 2;
        let raw_y = if below {
            tray_rect.bottom
        } else {
            tray_rect.top - height
        };
        let x_max = (work_area.right - width).max(work_area.left);
        let y_max = (work_area.bottom - height).max(work_area.top);
        let x = raw_x.clamp(work_area.left, x_max);
        let y = raw_y.clamp(work_area.top, y_max);
        app.arrow_center =
            ((tray_center_x - x) as f64 / scale).clamp(14.0, CONTENT_WIDTH - 14.0) as f32;
        let flags = SWP_SHOWWINDOW | SWP_NOACTIVATE;
        let _ = SetWindowPos(hwnd, None, x, y, width, height, flags);
    } else {
        // The tray rectangle can be unavailable for an overflow icon on some
        // shell versions. Anchor to the actual click point and its monitor,
        // rather than placing a guessed arrow at a fixed screen coordinate.
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let click_rect = RECT {
            left: cursor.x,
            top: cursor.y,
            right: cursor.x.saturating_add(1),
            bottom: cursor.y.saturating_add(1),
        };
        let monitor = MonitorFromRect(&click_rect, MONITOR_DEFAULTTONEAREST);
        let mut monitor_info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let work_area = if GetMonitorInfoW(monitor, &mut monitor_info).as_bool() {
            monitor_info.rcWork
        } else {
            RECT {
                left: cursor.x - width / 2,
                top: cursor.y - height,
                right: cursor.x + width / 2,
                bottom: cursor.y + height,
            }
        };
        let below = cursor.y < (work_area.top + work_area.bottom) / 2;
        app.arrow_edge = if below {
            ArrowEdge::Top
        } else {
            ArrowEdge::Bottom
        };
        let raw_x = cursor.x - width / 2;
        let raw_y = if below { cursor.y } else { cursor.y - height };
        let x = raw_x.clamp(
            work_area.left,
            (work_area.right - width).max(work_area.left),
        );
        let y = raw_y.clamp(
            work_area.top,
            (work_area.bottom - height).max(work_area.top),
        );
        app.arrow_center = ((cursor.x - x) as f64 / scale).clamp(14.0, CONTENT_WIDTH - 14.0) as f32;
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width,
            height,
            SWP_SHOWWINDOW | SWP_NOACTIVATE,
        );
    }
    app.visible = true;
    // SetWindowPos above already shows the popup with SWP_NOACTIVATE. Calling
    // ShowWindow(SW_SHOWNORMAL) here immediately steals activation while the
    // tray callback is still unwinding, which can make Windows send a focus
    // loss before the deferred popup activation has settled.
    let _ = InvalidateRect(Some(hwnd), None, false);
}

unsafe fn window_data(hwnd: HWND) -> Option<&'static mut WindowData> {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowData;
    ptr.as_mut()
}

unsafe fn main_hit_test(
    app: &AppState,
    x: f32,
    y: f32,
    body_top: f32,
    renderer: Option<&Renderer>,
) -> MainHit {
    let result_accepts_input = result_header_hit_test_contains(y, body_top)
        && match &app.phase {
            MainPhase::Matched { stage, .. } => !matches!(stage, ResultCardStage::Exiting { .. }),
            MainPhase::NoMatch {
                returned_to_ready_at,
                ..
            }
            | MainPhase::Offline {
                returned_to_ready_at,
                ..
            } => returned_to_ready_at.is_none(),
            _ => false,
        };
    if result_accepts_input {
        let top = result_panel_top_at(&app.phase, body_top, Instant::now()).unwrap_or(body_top);
        let offset = top - body_top;
        let close = native_icon_layout(body_top, app.arrow_edge).result_close_hit;
        let local_y = y - offset;
        if point_in_rect(x, local_y, close) {
            return MainHit::CloseResult;
        }
        if let MainPhase::Matched { track, .. } = &app.phase {
            let action = renderer
                .and_then(|renderer| renderer.result_details_metrics(track).ok())
                .map(|metrics| {
                    result_details_rects(
                        body_top,
                        metrics.title_height,
                        metrics.artist_height,
                        true,
                    )
                    .action
                })
                .unwrap_or_else(|| {
                    // If DirectWrite metrics are unavailable, conservatively
                    // keep the whole details column out of the Shazam link.
                    rect(
                        RESULT_DETAILS_LEFT,
                        body_top,
                        RESULT_DETAILS_RIGHT,
                        body_top + TOP_HEIGHT as f32,
                    )
                });
            let action_bounds = [action.left, action.top, action.right, action.bottom];
            if resolve_track_page_url(track, &app.ui_configs).is_some()
                && result_card_link_hit(x, local_y, body_top, close, Some(action_bounds))
            {
                return MainHit::OpenTrack(track.display_id);
            }
        }
        if matches!(
            app.phase,
            MainPhase::NoMatch { .. } | MainPhase::Offline { .. }
        ) && (90.0..=290.0).contains(&x)
            && (top + 115.0..=top + 155.0).contains(&y)
        {
            return MainHit::TryAgain;
        }
    }
    if let MainPhase::Timeout { .. } = app.phase {
        if (70.0..=190.0).contains(&x) && (body_top + 126.0..=body_top + 160.0).contains(&y) {
            return MainHit::TimeoutRestart;
        }
        if (200.0..=310.0).contains(&x) && (body_top + 126.0..=body_top + 160.0).contains(&y) {
            return MainHit::TimeoutDismiss;
        }
        return MainHit::Other;
    }
    let history_top = body_top + TOP_HEIGHT as f32;
    let now = Instant::now();
    for (index, track) in app.history.iter().enumerate() {
        if resolve_track_page_url(track, &app.ui_configs).is_none() {
            continue;
        }
        let row_top = history_row_top(
            index,
            app.history_transition,
            history_top,
            app.scroll_offset,
            now,
        );
        let history_bottom = history_top + HISTORY_HEIGHT as f32;
        if (0.0..=CONTENT_WIDTH as f32).contains(&x)
            && visible_history_row_hit(y, row_top, history_top, history_bottom)
        {
            return MainHit::OpenTrack(track.display_id);
        }
    }
    let icons = native_icon_layout(body_top, app.arrow_edge);
    if matches!(
        app.phase,
        MainPhase::Listening { .. } | MainPhase::Starting { .. }
    ) && point_in_rect(x, y, icons.cancel_hit)
    {
        return MainHit::Cancel;
    }
    if point_in_rect(x, y, icons.settings_hit)
        && matches!(app.phase, MainPhase::Ready | MainPhase::Error { .. })
    {
        return MainHit::Settings;
    }
    let button_x = BUTTON_X as f32..=(BUTTON_X + BUTTON_SIZE) as f32;
    let button_y = body_top + BUTTON_TOP as f32..=body_top + (BUTTON_TOP + BUTTON_SIZE) as f32;
    if button_x.contains(&x)
        && button_y.contains(&y)
        && app.recognition_session.is_none()
        && !matches!(
            app.phase,
            MainPhase::Matched { .. } | MainPhase::NoMatch { .. } | MainPhase::Offline { .. }
        )
    {
        return MainHit::CenterButton;
    }
    MainHit::Other
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MainHit {
    CenterButton,
    Settings,
    Cancel,
    TimeoutRestart,
    TimeoutDismiss,
    TryAgain,
    CloseResult,
    OpenTrack(u64),
    Other,
}

unsafe fn open_settings(app: &mut AppState) {
    // Do not initialize or show WinUI from inside the popup's WndProc. Showing
    // a top-level window synchronously sends activation messages to its owner;
    // defer it until the current callback has returned to avoid re-entering
    // this function while it holds &mut AppState.
    if PostMessageW(Some(app.main_hwnd), WM_OPEN_SETTINGS, WPARAM(0), LPARAM(0)).is_err() {
        app.message = Some(format!(
            "Could not schedule Settings: {}",
            Error::from_thread()
        ));
    }
}

unsafe fn open_settings_after_wndproc(app_ptr: *mut AppState) -> LRESULT {
    if app_ptr.is_null() {
        return LRESULT(0);
    }

    // ShowWindow/activation can synchronously re-enter the main WndProc. Mark
    // the host as active before loading or calling into WinUI so that the
    // re-entrant activation path does not call back into the same DLL to query
    // its visibility while its Show export is still on the stack.
    (*app_ptr).settings_opening = true;

    // Loading the host can initialize WinUI and the Windows App SDK. Keep this
    // outside any AppState borrow as well, even though it normally does not
    // dispatch owner-window messages.
    if (*app_ptr).settings_host.is_none() {
        match crate::windows_settings::SettingsHost::load() {
            Ok(host) => (*app_ptr).settings_host = Some(host),
            Err(error) => {
                (*app_ptr).settings_opening = false;
                (*app_ptr).message = Some(error);
                let owner = (*app_ptr).main_hwnd;
                let _ = InvalidateRect(Some(owner), None, false);
                return LRESULT(0);
            }
        }
    }

    // Copy only plain handles/function pointers out. Drop all Rust references
    // before calling WinUI, whose ShowWindow/activation path re-enters WndProc.
    let (show, owner, theme_preference) = {
        let app = &mut *app_ptr;
        let Some(host) = app.settings_host.as_ref() else {
            app.settings_opening = false;
            return LRESULT(0);
        };
        (
            host.show_callback(),
            app.main_hwnd,
            app.theme_preference.id(),
        )
    };

    let hr = show(owner.0, theme_preference);
    (*app_ptr).settings_opening = false;
    if hr < 0 {
        (*app_ptr).message = Some(format!(
            "WinUI 3 Settings failed to open (HRESULT 0x{:08X})",
            hr as u32
        ));
        let _ = InvalidateRect(Some(owner), None, false);
    }
    LRESULT(0)
}

// Legacy About panel is retained for source compatibility until the
// About entry moves into the WinUI 3 Settings host.
#[allow(dead_code)]
unsafe fn open_about(app: &mut AppState) {
    if app.about_hwnd.0.is_null() {
        let dpi = GetDpiForWindow(app.main_hwnd).max(96);
        let factor = f64::from(dpi) / 96.0;
        if let Ok(hwnd) = create_window(
            app as *mut AppState,
            WindowKind::About,
            "About OpenShazam",
            (ABOUT_WIDTH * factor).round() as i32,
            (ABOUT_HEIGHT * factor).round() as i32,
            false,
        ) {
            app.about_hwnd = hwnd;
        }
    }
    if !app.about_hwnd.0.is_null() {
        app.about_visible = true;
        let _ = ShowWindow(app.about_hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(app.about_hwnd);
        let _ = InvalidateRect(Some(app.about_hwnd), None, false);
    }
}

#[allow(dead_code)]
unsafe fn open_microphone_info(app: &mut AppState) {
    if app.microphone_hwnd.0.is_null() {
        let dpi = GetDpiForWindow(app.main_hwnd).max(96);
        let factor = f64::from(dpi) / 96.0;
        if let Ok(hwnd) = create_window(
            app as *mut AppState,
            WindowKind::MicrophoneInfo,
            "Microphone input",
            (MIC_INFO_WIDTH * factor).round() as i32,
            (MIC_INFO_HEIGHT * factor).round() as i32,
            false,
        ) {
            app.microphone_hwnd = hwnd;
        }
    }
    if !app.microphone_hwnd.0.is_null() {
        app.microphone_visible = true;
        let _ = ShowWindow(app.microphone_hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(app.microphone_hwnd);
        let _ = InvalidateRect(Some(app.microphone_hwnd), None, false);
    }
}

fn hide_main(app: &mut AppState) {
    if !app.visible && app.popup_opacity == 0 && app.popup_transition.is_none() {
        return;
    }
    unsafe { cancel_deferred_deactivation_hide(app) };
    if app
        .popup_transition
        .is_some_and(|transition| !app.visible && transition.hide_when_complete)
    {
        return;
    }
    let now = Instant::now();
    let from = popup_opacity_at(app, now);
    app.popup_opacity = from;
    app.popup_transition = Some(PopupOpacityTransition {
        started_at: now,
        from,
        to: 0,
        hide_when_complete: true,
    });
    app.visible = false;
    app.main_activated = false;
    app.popup_activation_grace_until = None;
    set_auto_recognition_context(app, RecognitionContext::Background);
    unsafe {
        sync_main_timers(app);
        if !app.main_hwnd.0.is_null() {
            let _ = InvalidateRect(Some(app.main_hwnd), None, false);
        }
    }
}

fn popup_opacity_at(app: &AppState, now: Instant) -> u8 {
    let Some(transition) = app.popup_transition else {
        return app.popup_opacity;
    };
    let elapsed = now.saturating_duration_since(transition.started_at);
    let progress = (elapsed.as_secs_f64() / POPUP_FADE_DURATION.as_secs_f64()).clamp(0.0, 1.0);
    let opacity = f64::from(transition.from)
        + (f64::from(transition.to) - f64::from(transition.from)) * progress;
    opacity.round().clamp(0.0, 255.0) as u8
}

unsafe fn advance_popup_transition(app: &mut AppState, now: Instant) -> bool {
    let Some(transition) = app.popup_transition else {
        return false;
    };
    let elapsed = now.saturating_duration_since(transition.started_at);
    let complete = elapsed >= POPUP_FADE_DURATION;
    app.popup_opacity = if complete {
        transition.to
    } else {
        popup_opacity_at(app, now)
    };
    if complete {
        app.popup_transition = None;
        if transition.hide_when_complete && !app.visible && !app.main_hwnd.0.is_null() {
            let _ = ShowWindow(app.main_hwnd, SW_HIDE);
        }
    }
    true
}

fn start_capture(app: &mut AppState, mode: CaptureMode) {
    if app.recognition_session.is_some() || matches!(app.phase, MainPhase::Listening { .. }) {
        return;
    }
    app.status_alternator = None;
    if app.recognition_controller.is_none() {
        match ApplicationController::spawn(WindowsRecognitionBackend::new(), 16, 64) {
            Ok(controller) => app.recognition_controller = Some(controller),
            Err(error) => {
                app.phase = MainPhase::Error {
                    message: format!("Recognition controller failed to start: {error}"),
                    entered_at: Instant::now(),
                };
                return;
            }
        }
    }
    let controller = app
        .recognition_controller
        .as_ref()
        .expect("controller created");
    let context = (mode == CaptureMode::Auto).then_some(if app.visible {
        RecognitionContext::Foreground
    } else {
        RecognitionContext::Background
    });
    let ticket = match mode {
        CaptureMode::Manual => controller.commands.start_manual(),
        CaptureMode::Auto => controller
            .commands
            .start_auto(context.expect("Auto recognition has a foreground/background context")),
    };
    match ticket {
        Ok(ticket) => {
            if mode == CaptureMode::Auto {
                save_auto_shazam_preference(true);
            }
            app.auto_spring =
                (mode == CaptureMode::Auto).then(ui_model::auto_tagging_button_spring_profile);
            app.success_wave_frame = None;
            if mode == CaptureMode::Auto {
                app.auto_candidate_dedupe.clear();
            }
            let entered_at = Instant::now();
            app.status_alternator =
                (mode == CaptureMode::Auto).then(|| StatusTextAlternator::auto(entered_at));
            app.recognition_session = Some(RecognitionSessionUi {
                ticket,
                mode,
                context,
                cancel_command_id: None,
                last_sequence: 0,
                result_presented: false,
            });
            app.phase = MainPhase::Starting { mode, entered_at };
        }
        Err(error) => {
            app.phase = MainPhase::Error {
                message: format!("Recognition request was not accepted: {error:?}"),
                entered_at: Instant::now(),
            };
        }
    }
}
fn cancel_capture(app: &mut AppState) {
    if let (Some(controller), Some(session)) =
        (app.recognition_controller.as_ref(), app.recognition_session)
    {
        if session.cancel_command_id.is_some() {
            return;
        }
        match controller
            .commands
            .submit(RecognitionCommand::CancelRecognition {
                session_id: session.ticket.session_id,
            }) {
            Ok(command_id) => {
                if session.mode == CaptureMode::Auto {
                    save_auto_shazam_preference(false);
                }
                if let Some(active) = app.recognition_session.as_mut() {
                    active.cancel_command_id = Some(command_id);
                }
                app.phase = MainPhase::Cancelling {
                    mode: session.mode,
                    entered_at: Instant::now(),
                };
            }
            Err(error) => {
                app.message = Some(format!("Cancel request was not accepted: {error:?}"));
            }
        }
    }
}

fn set_auto_recognition_context(app: &mut AppState, context: RecognitionContext) {
    let Some(session) = app.recognition_session else {
        return;
    };
    if session.mode != CaptureMode::Auto
        || session.context == Some(context)
        || session.cancel_command_id.is_some()
    {
        return;
    }
    let Some(controller) = app.recognition_controller.as_ref() else {
        return;
    };
    match controller
        .commands
        .submit(RecognitionCommand::SetRecognitionContext {
            session_id: session.ticket.session_id,
            context,
        }) {
        Ok(_) => {
            if let Some(active) = app.recognition_session.as_mut() {
                active.context = Some(context);
            }
        }
        Err(error) => {
            eprintln!("OpenShazam could not update Auto Shazam context: {error:?}");
        }
    }
}

fn poll_recognition_events(app: &mut AppState) -> bool {
    let Some(controller) = app.recognition_controller.as_ref() else {
        return false;
    };
    let active = app.recognition_session;
    let mut sequence_gap = false;
    if let Some(session) = active {
        if let CommandResultLookup::Retained(result) = controller
            .commands
            .command_result(session.ticket.command_id)
        {
            if matches!(result.status, CommandStatus::Failed { .. }) {
                sequence_gap = true;
            }
            let failure = match result.status {
                CommandStatus::Rejected { reason } | CommandStatus::StartFailed { reason } => {
                    Some(reason)
                }
                _ => None,
            };
            if let Some(reason) = failure {
                app.recognition_session = None;
                app.phase = MainPhase::Error {
                    message: format!("Recognition did not start: {reason}"),
                    entered_at: Instant::now(),
                };
                return true;
            }
        }
        if let Some(cancel_id) = session.cancel_command_id {
            if let CommandResultLookup::Retained(result) =
                controller.commands.command_result(cancel_id)
            {
                let failure = match result.status {
                    CommandStatus::Rejected { reason }
                    | CommandStatus::StartFailed { reason }
                    | CommandStatus::Failed { reason } => Some(reason),
                    _ => None,
                };
                if let Some(reason) = failure {
                    if let Some(current) = app.recognition_session.as_mut() {
                        current.cancel_command_id = None;
                    }
                    app.message = Some(format!("Cancel request failed: {reason}"));
                    let snapshot = controller.events.snapshot();
                    let _ = reconcile_controller_snapshot(app, &snapshot);
                    return true;
                }
            }
        }
    }
    let (events, snapshot) = {
        let mut events = Vec::new();
        while let Ok(event) = controller.events.try_recv() {
            events.push(event);
        }
        (events, controller.events.snapshot())
    };

    let mut changed = false;
    for event in events {
        if event.sequence > app.controller_sequence.saturating_add(1) {
            sequence_gap = true;
        }
        app.controller_sequence = app.controller_sequence.max(event.sequence);
        let Some(session) = active else { continue };
        if event.session_id != Some(session.ticket.session_id) {
            continue;
        }
        let last_processed = app
            .recognition_session
            .map_or(0, |current| current.last_sequence);
        if event.sequence <= last_processed {
            continue;
        }
        if event.sequence > session.last_sequence.saturating_add(1) {
            sequence_gap = true;
        }
        if let Some(current) = app.recognition_session.as_mut() {
            current.last_sequence = current.last_sequence.max(event.sequence);
        }
        sequence_gap |= matches!(
            event.kind,
            RecognitionEventKind::SessionEnded { .. }
                | RecognitionEventKind::SessionCancelled
                | RecognitionEventKind::CommandStatusChanged { .. }
        );
        changed |= apply_recognition_event(app, event, session.mode);
    }

    let notifications_dropped = snapshot.dropped_notifications > app.dropped_notifications;
    if sequence_gap || notifications_dropped || snapshot.last_sequence > app.controller_sequence {
        app.controller_sequence = snapshot.last_sequence;
        app.dropped_notifications = snapshot.dropped_notifications;
        if let Some(current) = app.recognition_session.as_mut() {
            current.last_sequence = snapshot.last_sequence;
        }
        changed |= reconcile_controller_snapshot(app, &snapshot);
    }
    changed
}

fn apply_recognition_event(app: &mut AppState, event: RecognitionEvent, mode: CaptureMode) -> bool {
    let event_time_utc = event.emitted_at_utc;
    let config_generation = event.config_generation;
    let session_id = event.session_id;
    match event.kind {
        RecognitionEventKind::UiConfigSnapshot(config) => {
            if config_generation == Some(config.generation) {
                app.ui_configs.insert(config.generation, config);
                while app.ui_configs.len() > 8 {
                    let Some(oldest) = app.ui_configs.keys().copied().min() else {
                        break;
                    };
                    app.ui_configs.remove(&oldest);
                }
                refresh_track_page_urls(app);
            }
            true
        }
        RecognitionEventKind::CaptureStarted
        | RecognitionEventKind::SignatureReady { .. }
        | RecognitionEventKind::RequestStarted { .. }
        | RecognitionEventKind::RetryScheduled { .. } => {
            if matches!(app.phase, MainPhase::Starting { .. }) {
                enter_listening(app, mode, Instant::now());
            }
            true
        }
        RecognitionEventKind::ManualUiEvent(NativeManualUiEvent::Result(result))
            if mode == CaptureMode::Manual
                && result.stage == NativeManualResultStage::TaggingDidFinish =>
        {
            let already_presented = app
                .recognition_session
                .is_some_and(|session| session.result_presented);
            if already_presented {
                return false;
            }
            if let Some(session) = app.recognition_session.as_mut() {
                session.result_presented = true;
            }
            show_controller_match(
                app,
                result.primary_track,
                mode,
                config_generation,
                Some(event_time_utc),
            );
            true
        }
        RecognitionEventKind::AutoUiEvent(NativeAutoUiEvent::ContinuousMatchCandidate(result))
            if mode == CaptureMode::Auto =>
        {
            apply_auto_match_candidate(
                app,
                result.primary_track,
                config_generation,
                Some(event_time_utc),
            )
        }
        RecognitionEventKind::ManualUiEvent(NativeManualUiEvent::NoMatch)
        | RecognitionEventKind::NoMatch { .. } => {
            app.phase = MainPhase::NoMatch {
                entered_at: Instant::now(),
                returned_to_ready_at: None,
            };
            true
        }
        RecognitionEventKind::RecognitionError { message, .. } => {
            let entered_at = Instant::now();
            app.phase = if is_offline_failure(&message) {
                MainPhase::Offline {
                    entered_at,
                    returned_to_ready_at: None,
                }
            } else {
                MainPhase::Error {
                    message,
                    entered_at,
                }
            };
            true
        }
        RecognitionEventKind::SessionTimedOut => {
            app.phase = MainPhase::Timeout {
                mode,
                entered_at: Instant::now(),
            };
            true
        }
        RecognitionEventKind::SessionCancelled | RecognitionEventKind::SessionEnded { .. } => {
            let _ended_session_id = session_id;
            // The following authoritative snapshot distinguishes a true terminal
            // state from an event notification that raced a newer session update.
            true
        }
        _ => false,
    }
}

fn reconcile_controller_snapshot(
    app: &mut AppState,
    snapshot: &openshazam_controller::ControllerSnapshot,
) -> bool {
    let active_id = app
        .recognition_session
        .map(|session| session.ticket.session_id);
    let session_snapshot = snapshot
        .active_session
        .as_ref()
        .filter(|session| Some(session.session_id) == active_id)
        .or_else(|| {
            snapshot
                .last_ended_session
                .as_ref()
                .filter(|session| Some(session.session_id) == active_id)
        });
    let Some(session_snapshot) = session_snapshot else {
        return false;
    };
    reconcile_session_snapshot(app, session_snapshot)
}

fn enter_listening(app: &mut AppState, mode: CaptureMode, started_at: Instant) {
    // Native Auto changes the tagging layer to state 2 and starts its spring
    // before the recorder reports CaptureStarted. Keep the animation clock
    // anchored to the UI command so the spring and ripple phase do not restart
    // when the asynchronous capture event arrives.
    let animation_started_at = capture_animation_start(&app.phase, mode, started_at);
    app.phase = MainPhase::Listening {
        mode,
        started_at: animation_started_at,
    };
    app.status_alternator =
        (mode == CaptureMode::Auto).then(|| StatusTextAlternator::auto(animation_started_at));
}

fn capture_animation_start(phase: &MainPhase, mode: CaptureMode, fallback: Instant) -> Instant {
    match (phase, mode) {
        (
            MainPhase::Starting {
                mode: CaptureMode::Auto,
                entered_at,
            },
            CaptureMode::Auto,
        ) => *entered_at,
        _ => fallback,
    }
}

fn reconcile_session_snapshot(app: &mut AppState, session: &RecognitionSessionSnapshot) -> bool {
    let Some(ui_session) = app.recognition_session else {
        return false;
    };
    if session.session_id != ui_session.ticket.session_id {
        return false;
    }
    let mut changed = false;
    if let Some(config) = session.config.as_ref() {
        app.ui_configs.insert(config.generation, config.clone());
        refresh_track_page_urls(app);
    }
    if !ui_session.result_presented && ui_session.mode == CaptureMode::Manual {
        if let Some(UiRecognitionEvent::Manual(NativeManualUiEvent::Result(result))) =
            session.last_result.as_ref()
        {
            if result.stage == NativeManualResultStage::TaggingDidFinish {
                if let Some(current) = app.recognition_session.as_mut() {
                    current.result_presented = true;
                }
                show_controller_match(
                    app,
                    result.primary_track.clone(),
                    ui_session.mode,
                    session.config.as_ref().map(|config| config.generation),
                    session.last_result_at_utc,
                );
                changed = true;
            }
        }
    }
    if ui_session.mode == CaptureMode::Auto {
        if let Some(UiRecognitionEvent::Auto(NativeAutoUiEvent::ContinuousMatchCandidate(result))) =
            session.last_result.as_ref()
        {
            changed |= apply_auto_match_candidate(
                app,
                result.primary_track.clone(),
                session.config.as_ref().map(|config| config.generation),
                session.last_result_at_utc,
            );
        }
    }
    match session.phase {
        SessionPhase::Starting => {}
        SessionPhase::Running => {
            let capture_is_live = session.last_event.is_some_and(|event| {
                matches!(
                    event.category,
                    RecognitionEventCategory::CaptureStarted
                        | RecognitionEventCategory::SignatureReady
                        | RecognitionEventCategory::RequestStarted
                        | RecognitionEventCategory::RetryScheduled
                        | RecognitionEventCategory::ManualUiEvent
                        | RecognitionEventCategory::AutoUiEvent
                )
            });
            if capture_is_live
                && (matches!(app.phase, MainPhase::Starting { .. })
                    || (matches!(app.phase, MainPhase::Cancelling { .. })
                        && app
                            .recognition_session
                            .is_some_and(|current| current.cancel_command_id.is_none())))
            {
                enter_listening(app, ui_session.mode, Instant::now());
                changed = true;
            }
        }
        SessionPhase::Stopping | SessionPhase::Cancelling => {
            if !matches!(app.phase, MainPhase::Cancelling { .. }) {
                app.phase = MainPhase::Cancelling {
                    mode: ui_session.mode,
                    entered_at: Instant::now(),
                };
                changed = true;
            }
        }
        SessionPhase::Faulted => {
            app.recognition_session = None;
            app.phase = MainPhase::Error {
                message: "Recognition session failed and its resources may not have been released"
                    .to_owned(),
                entered_at: Instant::now(),
            };
            changed = true;
        }
        SessionPhase::Ended(reason) => {
            app.recognition_session = None;
            if !matches!(
                app.phase,
                MainPhase::Matched { .. }
                    | MainPhase::NoMatch { .. }
                    | MainPhase::Timeout { .. }
                    | MainPhase::Error { .. }
            ) {
                app.phase = match reason {
                    SessionEndReason::NoMatch => MainPhase::NoMatch {
                        entered_at: Instant::now(),
                        returned_to_ready_at: None,
                    },
                    SessionEndReason::TimedOut => MainPhase::Timeout {
                        mode: ui_session.mode,
                        entered_at: Instant::now(),
                    },
                    _ => MainPhase::Ready,
                };
            }
            changed = true;
        }
    }
    changed
}

fn show_controller_match(
    app: &mut AppState,
    candidate: TrackCandidate,
    mode: CaptureMode,
    config_generation: Option<u64>,
    event_time_utc: Option<SystemTime>,
) {
    let track = record_controller_track(app, candidate, config_generation, event_time_utc);
    app.status_alternator = Some(StatusTextAlternator::after_success(track.recognized_at));
    if app.visible {
        app.success_wave_frame = current_success_wave_frame(app);
        app.phase = MainPhase::Matched {
            track,
            stage: ResultCardStage::Entering {
                started_at: Instant::now(),
            },
        };
    } else {
        app.success_wave_frame = None;
        app.phase = MainPhase::Ready;
        if mode == CaptureMode::Manual {
            queue_hidden_match_notification(app, &track);
        }
    }
}

fn apply_auto_match_candidate(
    app: &mut AppState,
    candidate: TrackCandidate,
    config_generation: Option<u64>,
    event_time_utc: Option<SystemTime>,
) -> bool {
    if !matches!(
        app.phase,
        MainPhase::Starting {
            mode: CaptureMode::Auto,
            ..
        } | MainPhase::Listening {
            mode: CaptureMode::Auto,
            ..
        }
    ) {
        return false;
    }
    if auto_candidate_already_seen(app, &candidate) {
        return false;
    }
    if let MainPhase::Starting { entered_at, .. } = &app.phase {
        enter_listening(app, CaptureMode::Auto, *entered_at);
    }
    let track = record_controller_track(app, candidate, config_generation, event_time_utc);
    if !app.visible {
        queue_hidden_match_notification(app, &track);
    }
    true
}

fn auto_candidate_already_seen(app: &mut AppState, candidate: &TrackCandidate) -> bool {
    let Some(identity) = auto_candidate_identity(candidate) else {
        return false;
    };
    // A track already present in past history is still a valid new Auto
    // Shazam result. Suppress only repeats from this active Auto session.
    !remember_auto_candidate_key(&mut app.auto_candidate_dedupe, identity)
}

fn remember_auto_candidate_key(recent: &mut VecDeque<String>, identity: String) -> bool {
    if recent.contains(&identity) {
        return false;
    }
    recent.push_back(identity);
    while recent.len() > AUTO_CANDIDATE_DEDUPE_LIMIT {
        recent.pop_front();
    }
    true
}

fn auto_candidate_identity(candidate: &TrackCandidate) -> Option<String> {
    if let Some(key) = candidate
        .ui_metadata
        .as_ref()
        .and_then(|metadata| metadata.track_key.as_deref())
        .filter(|key| !key.trim().is_empty())
    {
        return Some(format!("track-key:{key}"));
    }
    if let Some(id) = candidate
        .track_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    {
        return Some(format!("candidate-id:{id}"));
    }
    let title = candidate.title.trim().to_lowercase();
    let artist = candidate.artist.trim().to_lowercase();
    (!title.is_empty() || !artist.is_empty()).then(|| format!("display:{title}\u{001f}{artist}"))
}

fn record_controller_track(
    app: &mut AppState,
    candidate: TrackCandidate,
    config_generation: Option<u64>,
    event_time_utc: Option<SystemTime>,
) -> Track {
    let metadata = candidate.ui_metadata.as_ref();
    let display_id = app.next_display_id;
    app.next_display_id = app.next_display_id.wrapping_add(1).max(1);
    let track_key = metadata.and_then(|metadata| metadata.track_key.clone());
    let page_link =
        resolve_page_link_for_generation(config_generation, &app.ui_configs, track_key.as_deref());
    let page_url = page_link.as_ref().ok().cloned();
    if let Err(reason) = page_link {
        let config = config_generation.and_then(|generation| app.ui_configs.get(&generation));
        eprintln!(
            "OpenShazam Shazam page link unavailable: config_generation={}, config_snapshot={}, webtrackurl={}, track_key={}, reason={}",
            config_generation.is_some(),
            config.is_some(),
            config.is_some_and(|config| config.web_track_url_template.is_some()),
            track_key.is_some(),
            reason.as_str(),
        );
    }
    let (recognized_at_utc, timestamp_source) =
        resolve_history_timestamp(event_time_utc, SystemTime::now());
    let track = Track {
        display_id,
        title: candidate.title,
        artist: candidate.artist,
        recognized_at: Instant::now(),
        recognized_at_utc,
        recognized_date_label: format_history_date_label(recognized_at_utc),
        timestamp_source,
        artwork_url: metadata.and_then(|metadata| metadata.artwork_url.clone()),
        hq_artwork_url: metadata.and_then(|metadata| metadata.hq_artwork_url.clone()),
        background_artwork_url: metadata.and_then(|metadata| metadata.background_image_url.clone()),
        track_key,
        config_generation,
        page_url,
    };
    let artwork_request = register_artwork_request(app, &track);
    let previous_count = app.history.len();
    let previous_scroll_offset = app.scroll_offset;
    app.history.insert(0, track.clone());
    app.history.truncate(40);
    let (next_scroll_offset, animate_insert) = history_offset_after_insert(
        previous_scroll_offset,
        app.history.len(),
        HISTORY_HEIGHT as f32,
    );
    app.scroll_offset = next_scroll_offset;
    app.history_transition = animate_insert.then(|| HistoryMutationTransition {
        started_at: Instant::now(),
        event: HistoryMutationEvent::Insert {
            index: 0,
            previous_count,
        },
    });
    // The stationary pointer may now be over a different visible row.
    let next_hover = unsafe { hovered_history_row_for_cursor(app) };
    app.hovered_row = next_hover;
    invalidate_orphaned_artwork_requests(app);
    if let Some(request) = artwork_request {
        start_artwork_load(
            app.main_hwnd,
            request,
            unsafe { GetDpiForWindow(app.main_hwnd) }.max(96),
        );
    }
    track
}

fn current_success_wave_frame(app: &AppState) -> Option<ui_model::WaveFrame> {
    let MainPhase::Listening { mode, started_at } = &app.phase else {
        return None;
    };
    Some(ui_model::wave_frame_at_with_auto_spring(
        match *mode {
            CaptureMode::Manual => TaggingMode::Manual,
            CaptureMode::Auto => TaggingMode::Auto,
        },
        started_at.elapsed().as_secs_f64(),
        app.ui_levels,
        app.auto_spring,
    ))
}

fn register_artwork_request(app: &mut AppState, track: &Track) -> Option<ArtworkRequest> {
    if track.artwork_url.is_none() && track.hq_artwork_url.is_none() {
        return None;
    }
    let identity = TrackIdentity {
        display_id: track.display_id,
    };
    let generation = app
        .artwork_generations
        .get(&identity)
        .copied()
        .unwrap_or_default()
        .wrapping_add(1)
        .max(1);
    app.artwork_generations.insert(identity, generation);
    let request_id = app.next_artwork_request_id.max(1);
    app.next_artwork_request_id = request_id.wrapping_add(1).max(1);
    let request = ArtworkRequest {
        identity: ArtworkRequestIdentity {
            track: identity,
            generation,
            request_id,
        },
        standard_url: track.artwork_url.clone(),
        high_quality_url: track.hq_artwork_url.clone(),
    };
    app.active_artwork_requests
        .insert(identity, request.clone());
    Some(request)
}

fn invalidate_orphaned_artwork_requests(app: &mut AppState) {
    let live_tracks: HashSet<_> = app
        .history
        .iter()
        .map(|track| TrackIdentity {
            display_id: track.display_id,
        })
        .collect();
    app.active_artwork_requests
        .retain(|identity, _| live_tracks.contains(identity));
    app.artwork_generations
        .retain(|identity, _| live_tracks.contains(identity));
    app.artwork_resources.retain(|display_id, _| {
        live_tracks.contains(&TrackIdentity {
            display_id: *display_id,
        })
    });
    let orphaned_notifications: Vec<_> = app
        .pending_match_notifications
        .keys()
        .filter(|display_id| {
            !live_tracks.contains(&TrackIdentity {
                display_id: **display_id,
            })
        })
        .copied()
        .collect();
    for display_id in orphaned_notifications {
        if let Some(track) = app.pending_match_notifications.remove(&display_id) {
            unsafe { notify_hidden_match(app, &track, None) };
        }
    }
}

fn start_artwork_load(hwnd: HWND, request: ArtworkRequest, dpi: u32) {
    let hwnd_value = hwnd.0 as isize;
    let required_pixels = 150u32.saturating_mul(dpi.max(96)).saturating_add(95) / 96;
    thread::spawn(move || {
        let mut standard_size = None;
        if let Some(url) = request
            .standard_url
            .as_deref()
            .filter(|url| valid_https_resource_url(url))
        {
            if let Ok(pixels) = fetch_and_decode_artwork(&url) {
                standard_size = Some(pixels.width.min(pixels.height));
                post_artwork_ready(
                    hwnd_value,
                    request.identity,
                    ArtworkSource::Standard,
                    pixels,
                );
            }
        }
        if standard_size.is_none_or(|size| size < required_pixels) {
            if let Some(url) = request
                .high_quality_url
                .as_deref()
                .filter(|url| valid_https_resource_url(url))
            {
                if let Ok(pixels) = fetch_and_decode_artwork(&url) {
                    post_artwork_ready(
                        hwnd_value,
                        request.identity,
                        ArtworkSource::HighQuality,
                        pixels,
                    );
                }
            }
        }
        post_artwork_load_finished(hwnd_value, request.identity);
    });
}

fn valid_https_resource_url(url: &str) -> bool {
    if url.len() > 4_096
        || !url.starts_with("https://")
        || url.contains('#')
        || url.chars().any(char::is_control)
    {
        return false;
    }
    resource_host_port(url).is_some_and(|(host, _)| {
        host.parse::<IpAddr>()
            .map(is_public_artwork_address)
            .unwrap_or(true)
    })
}

fn resource_host_port(url: &str) -> Option<(String, u16)> {
    let remainder = url.strip_prefix("https://")?;
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if authority.is_empty() || authority.contains(['@', '\\']) {
        return None;
    }

    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let close = bracketed.find(']')?;
        let host = &bracketed[..close];
        host.parse::<Ipv6Addr>().ok()?;
        let suffix = &bracketed[close + 1..];
        let port = if suffix.is_empty() {
            443
        } else {
            suffix.strip_prefix(':')?.parse::<u16>().ok()?
        };
        (host, port)
    } else {
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host, port.parse::<u16>().ok()?),
            Some(_) => return None,
            None => (authority, 443),
        };
        if host.is_empty() || host.contains(['[', ']']) {
            return None;
        }
        if host.parse::<Ipv4Addr>().is_err() && !valid_dns_host(host) {
            return None;
        }
        (host, port)
    };
    (port == 443).then(|| (host.to_ascii_lowercase(), port))
}

fn valid_dns_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || host.ends_with('.') {
        return false;
    }
    // Reject shortened or otherwise ambiguous numeric IPv4 spellings such as
    // 127.1; curl and platform resolvers may interpret those as IP literals.
    if host.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && label
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn is_public_artwork_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_private()
                && !ip.is_link_local()
                && !ip.is_broadcast()
                && !ip.is_multicast()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 192 && b == 0)
                && !(a == 192 && b == 88 && c == 99)
                && !(a == 192 && b == 0 && c == 2)
                && !(a == 198 && (b == 18 || b == 19))
                && !(a == 198 && b == 51 && c == 100)
                && !(a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public_artwork_address(IpAddr::V4(mapped));
            }
            let segments = ip.segments();
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && !ip.is_unique_local()
                && !ip.is_unicast_link_local()
                && segments[0] & 0xe000 == 0x2000 // Global unicast 2000::/3.
                && !(segments[0] == 0x2001 && segments[1] & 0xfe00 == 0) // IETF assignments.
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8) // Documentation.
                && !(segments[0] == 0x2001 && segments[1] & 0xfff0 == 0x0010) // ORCHID.
                && !(segments[0] == 0x2001 && segments[1] == 0x0002 && segments[2] == 0)
                && segments[0] != 0x2002 // 6to4 can encode private IPv4 destinations.
                && segments[0] != 0x3fff // Documentation prefix.
        }
    }
}

fn resolve_public_artwork_addresses(host: &str, port: u16) -> Result<Vec<IpAddr>, String> {
    let addresses = if let Ok(address) = host.parse::<IpAddr>() {
        vec![address]
    } else {
        (host, port)
            .to_socket_addrs()
            .map_err(|error| format!("artwork host lookup failed: {error}"))?
            .map(|socket| socket.ip())
            .collect::<Vec<_>>()
    };
    if addresses.is_empty() || addresses.iter().any(|ip| !is_public_artwork_address(*ip)) {
        return Err("artwork host resolved to a non-public address".to_owned());
    }
    let mut unique = Vec::with_capacity(addresses.len());
    for address in addresses {
        if !unique.contains(&address) {
            unique.push(address);
        }
    }
    Ok(unique)
}

fn curl_resolve_entry(host: &str, port: u16, addresses: &[IpAddr]) -> Option<String> {
    if host.parse::<IpAddr>().is_ok() {
        return None;
    }
    let addresses = addresses
        .iter()
        .map(|address| match address {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        })
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("{host}:{port}:{addresses}"))
}

fn supported_artwork_content_type(content_type: &str) -> bool {
    matches!(
        content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "image/jpeg" | "image/png" | "image/webp"
    )
}

const MAX_ARTWORK_BYTES: usize = 8 * 1024 * 1024;
const MAX_ARTWORK_METADATA_BYTES: usize = 8 * 1024;
const MAX_ARTWORK_REDIRECTS: usize = 4;
const ARTWORK_FETCH_TIMEOUT: Duration = Duration::from_secs(12);
const ARTWORK_RESPONSE_MARKER: &[u8] = b"\nOPEN SHAZAM IMAGE RESPONSE:";

struct CurlArtworkResponse {
    status: u16,
    redirect_url: String,
    content_type: String,
    remote_ip: IpAddr,
    body: Vec<u8>,
}

fn parse_curl_artwork_output(output: &[u8]) -> Result<CurlArtworkResponse, String> {
    if output.len() > MAX_ARTWORK_BYTES + MAX_ARTWORK_METADATA_BYTES {
        return Err("artwork response exceeded the byte limit".to_owned());
    }
    let marker = output
        .windows(ARTWORK_RESPONSE_MARKER.len())
        .rposition(|window| window == ARTWORK_RESPONSE_MARKER)
        .ok_or_else(|| "artwork response metadata was missing".to_owned())?;
    let metadata = std::str::from_utf8(&output[marker + ARTWORK_RESPONSE_MARKER.len()..])
        .map_err(|_| "artwork response metadata was not valid text".to_owned())?;
    let mut fields = metadata.splitn(4, '\n');
    let status = fields
        .next()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .ok_or_else(|| "artwork response status was invalid".to_owned())?;
    let redirect_url = fields.next().unwrap_or_default().trim().to_owned();
    let content_type = fields.next().unwrap_or_default().trim().to_owned();
    let remote_ip = fields
        .next()
        .and_then(|value| value.trim().parse::<IpAddr>().ok())
        .ok_or_else(|| "artwork server address was missing or invalid".to_owned())?;
    if marker > MAX_ARTWORK_BYTES {
        return Err("artwork response exceeded the byte limit".to_owned());
    }
    Ok(CurlArtworkResponse {
        status,
        redirect_url,
        content_type,
        remote_ip,
        body: output[..marker].to_vec(),
    })
}

fn curl_artwork_request(
    url: &str,
    resolve: Option<&str>,
    remaining: Duration,
) -> Result<CurlArtworkResponse, String> {
    let timeout = format!("{:.3}", remaining.as_secs_f64().max(0.1));
    let max_bytes = MAX_ARTWORK_BYTES.to_string();
    let write_out =
        "\nOPEN SHAZAM IMAGE RESPONSE:%{http_code}\n%{redirect_url}\n%{content_type}\n%{remote_ip}"
            .to_string();
    let mut command = Command::new("curl.exe");
    command.args([
        "--disable",
        "--fail",
        "--proto",
        "=https",
        "--max-time",
        &timeout,
        "--max-filesize",
        &max_bytes,
        "--silent",
        "--show-error",
        "--proxy",
        "",
        "--output",
        "-",
    ]);
    if let Some(resolve) = resolve {
        command.args(["--resolve", resolve]);
    }
    let mut child = command
        .args(["--write-out", &write_out, url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("artwork downloader unavailable: {error}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "artwork downloader output was unavailable".to_owned())?;
    let output_limit = MAX_ARTWORK_BYTES + MAX_ARTWORK_METADATA_BYTES + 1;
    let mut output = Vec::with_capacity(64 * 1024);
    let read_result = stdout
        .by_ref()
        .take(output_limit as u64)
        .read_to_end(&mut output);
    drop(stdout);
    if let Err(error) = read_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("artwork response read failed: {error}"));
    }
    if output.len() > MAX_ARTWORK_BYTES + MAX_ARTWORK_METADATA_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return Err("artwork response exceeded the byte limit".to_owned());
    }
    let status = child
        .wait()
        .map_err(|error| format!("artwork downloader wait failed: {error}"))?;
    if !status.success() {
        return Err("artwork download failed or exceeded its transfer limits".to_owned());
    }

    parse_curl_artwork_output(&output)
}

fn valid_shazam_page_url(url: &str) -> bool {
    if url.len() > 4_096
        || !url.starts_with("https://")
        || url.chars().any(char::is_control)
        || url.contains('@')
        || url.contains('\\')
    {
        return false;
    }
    let authority = url[8..].split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .strip_suffix(":443")
        .unwrap_or(authority)
        .to_ascii_lowercase();
    (host == "shazam.com" || host.ends_with(".shazam.com"))
        && host
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-'))
}

fn valid_page_template(template: &str) -> bool {
    valid_shazam_page_url(template) && template.matches("{trackid}").count() == 1
}

fn resolve_page_link(
    template: Option<&str>,
    track_key: Option<&str>,
) -> Option<ValidatedWebTrackUrl> {
    resolve_page_link_checked(template, track_key).ok()
}

fn resolve_page_link_checked(
    template: Option<&str>,
    track_key: Option<&str>,
) -> Result<ValidatedWebTrackUrl, PageLinkFailure> {
    let template = template.ok_or(PageLinkFailure::MissingTemplate)?;
    let track_key = track_key.ok_or(PageLinkFailure::MissingTrackKey)?;
    const TOKEN: &str = "{trackid}";
    if !valid_page_template(template) {
        return Err(PageLinkFailure::InvalidTemplate);
    }
    if track_key.is_empty() || track_key.len() > 1_024 {
        return Err(PageLinkFailure::InvalidTrackKey);
    }
    let mut encoded = String::with_capacity(track_key.len());
    for byte in track_key.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    let resolved = template.replacen(TOKEN, &encoded, 1);
    if resolved.contains('{') || resolved.contains('}') || !valid_shazam_page_url(&resolved) {
        return Err(PageLinkFailure::InvalidResolvedUrl);
    }
    Ok(ValidatedWebTrackUrl(resolved))
}

unsafe fn open_track_page(hwnd: HWND, url: &ValidatedWebTrackUrl) -> windows::core::Result<()> {
    let verb = wide("open");
    let target = wide(&url.0);
    let mut execute = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_DEFAULT,
        hwnd,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(target.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    ShellExecuteExW(&mut execute)
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

fn resolve_track_page_url(
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

fn resolve_page_link_for_generation(
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

fn refresh_track_page_urls(app: &mut AppState) {
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

fn page_url_for_display_id(
    history: &[Track],
    current_result: Option<&Track>,
    display_id: u64,
    ui_configs: &HashMap<u64, UiConfigSnapshot>,
) -> Option<ValidatedWebTrackUrl> {
    track_for_display_id(history, current_result, display_id)
        .and_then(|track| resolve_track_page_url(track, ui_configs))
}

fn fetch_and_decode_artwork(url: &str) -> Result<ArtworkPixels, String> {
    if !valid_https_resource_url(url) {
        return Err("artwork URL was not a public HTTPS resource".to_owned());
    }
    let started_at = Instant::now();
    let mut current_url = url.to_owned();
    for redirect_count in 0..=MAX_ARTWORK_REDIRECTS {
        let (host, port) = resource_host_port(&current_url)
            .ok_or_else(|| "artwork URL authority was invalid".to_owned())?;
        let addresses = resolve_public_artwork_addresses(&host, port)?;
        let resolve = curl_resolve_entry(&host, port, &addresses);
        let remaining = ARTWORK_FETCH_TIMEOUT.saturating_sub(started_at.elapsed());
        if remaining.is_zero() {
            return Err("artwork download timed out".to_owned());
        }
        let response = curl_artwork_request(&current_url, resolve.as_deref(), remaining)?;
        if !addresses.contains(&response.remote_ip)
            || !is_public_artwork_address(response.remote_ip)
        {
            return Err("artwork connection did not use its validated public address".to_owned());
        }
        if (300..400).contains(&response.status) && !response.redirect_url.is_empty() {
            if redirect_count == MAX_ARTWORK_REDIRECTS
                || !valid_https_resource_url(&response.redirect_url)
            {
                return Err("artwork redirect was invalid or exceeded its limit".to_owned());
            }
            current_url = response.redirect_url;
            continue;
        }
        if response.status != 200 {
            return Err(format!("artwork server returned HTTP {}", response.status));
        }
        if !supported_artwork_content_type(&response.content_type) {
            return Err("artwork response MIME type was unsupported".to_owned());
        }
        return decode_artwork_wic(&response.body);
    }
    Err("artwork redirect was invalid or exceeded its limit".to_owned())
}

fn artwork_pixel_fingerprint(width: u32, height: u32, bgra: &[u8]) -> u128 {
    // A small content key lets the renderer replace a bitmap when a track's
    // artwork bytes are refreshed under the same display identity. It is only
    // a process-local cache key, not a security or file-integrity hash.
    let bytes = width
        .to_le_bytes()
        .into_iter()
        .chain(height.to_le_bytes())
        .chain(bgra.iter().copied());
    let mut left = 0xcbf2_9ce4_8422_2325u64;
    let mut right = 0x8422_2325_cbf2_9ce4u64;
    for byte in bytes {
        left = (left ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        right = (right ^ u64::from(byte).wrapping_add(0x9d)).wrapping_mul(0x9e37_79b1_85eb_ca87);
    }
    (u128::from(left) << 64) | u128::from(right)
}

fn decode_artwork_wic(encoded: &[u8]) -> Result<ArtworkPixels, String> {
    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let result = unsafe {
        (|| -> windows::core::Result<ArtworkPixels> {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let stream = factory.CreateStream()?;
            stream.InitializeFromMemory(encoded)?;
            let decoder = factory.CreateDecoderFromStream(
                &stream,
                std::ptr::null(),
                WICDecodeMetadataCacheOnDemand,
            )?;
            let frame = decoder.GetFrame(0)?;
            let mut width = 0u32;
            let mut height = 0u32;
            frame.GetSize(&mut width, &mut height)?;
            if width == 0 || height == 0 || width > 2_048 || height > 2_048 {
                return Err(windows::core::Error::new(
                    windows::core::HRESULT(0x8007_0057u32 as i32),
                    "artwork dimensions are outside the supported range",
                ));
            }
            let stride = width.checked_mul(4).ok_or_else(|| {
                windows::core::Error::new(
                    windows::core::HRESULT(0x8007_0057u32 as i32),
                    "artwork stride overflow",
                )
            })?;
            let byte_len = (stride as usize)
                .checked_mul(height as usize)
                .ok_or_else(|| {
                    windows::core::Error::new(
                        windows::core::HRESULT(0x8007_0057u32 as i32),
                        "artwork buffer size overflow",
                    )
                })?;
            let mut bgra = vec![0u8; byte_len];
            let converter = factory.CreateFormatConverter()?;
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            converter.CopyPixels(std::ptr::null(), stride, &mut bgra)?;
            let fingerprint = artwork_pixel_fingerprint(width, height, &bgra);
            Ok(ArtworkPixels {
                width,
                height,
                fingerprint,
                bgra,
            })
        })()
    };
    if initialized {
        unsafe { CoUninitialize() };
    }
    result.map_err(|error| format!("artwork decode failed: {error}"))
}

fn post_artwork_ready(
    hwnd: isize,
    identity: ArtworkRequestIdentity,
    source: ArtworkSource,
    pixels: ArtworkPixels,
) {
    let pointer = Box::into_raw(Box::new(ArtworkReady {
        identity,
        source,
        pixels,
    })) as isize;
    if unsafe {
        PostMessageW(
            Some(HWND(hwnd as *mut c_void)),
            WM_ARTWORK_READY,
            WPARAM(0),
            LPARAM(pointer),
        )
    }
    .is_err()
    {
        unsafe { drop(Box::from_raw(pointer as *mut ArtworkReady)) };
    }
}

fn post_artwork_load_finished(hwnd: isize, identity: ArtworkRequestIdentity) {
    let pointer = Box::into_raw(Box::new(identity)) as isize;
    if unsafe {
        PostMessageW(
            Some(HWND(hwnd as *mut c_void)),
            WM_ARTWORK_LOAD_FINISHED,
            WPARAM(0),
            LPARAM(pointer),
        )
    }
    .is_err()
    {
        unsafe { drop(Box::from_raw(pointer as *mut ArtworkRequestIdentity)) };
    }
}

fn is_offline_failure(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    [
        "offline",
        "no internet",
        "network is unreachable",
        "connection refused",
        "could not resolve",
        "dns error",
        "接続できません",
        "インターネットに接続",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiLanguage {
    Japanese,
    English,
}

fn directwrite_locale(language: UiLanguage) -> &'static str {
    match language {
        UiLanguage::Japanese => "ja-JP",
        UiLanguage::English => "en-US",
    }
}

static UI_LANGUAGE: OnceLock<UiLanguage> = OnceLock::new();

fn ui_language() -> UiLanguage {
    *UI_LANGUAGE.get_or_init(detect_ui_language)
}

fn select_ui_language_from_multisz(buffer: &[u16]) -> UiLanguage {
    buffer
        .split(|unit| *unit == 0)
        .filter(|tag| !tag.is_empty())
        .filter_map(|tag| String::from_utf16(tag).ok())
        .find_map(|tag| {
            let primary = tag.split(['-', '_']).next()?.to_ascii_lowercase();
            match primary.as_str() {
                "ja" => Some(UiLanguage::Japanese),
                "en" => Some(UiLanguage::English),
                _ => None,
            }
        })
        .unwrap_or(UiLanguage::English)
}

fn detect_ui_language() -> UiLanguage {
    let mut language_count = 0u32;
    let mut character_count = 0u32;
    // Windows reports the required MULTI_SZ size on the initial null-buffer
    // call. The Result is expected to be an insufficient-buffer error here.
    let _ = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut language_count,
            None,
            &mut character_count,
        )
    };
    if character_count == 0 || character_count > 65_536 {
        return UiLanguage::English;
    }

    let mut languages = vec![0u16; character_count as usize];
    if unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut language_count,
            Some(PWSTR(languages.as_mut_ptr())),
            &mut character_count,
        )
    }
    .is_err()
    {
        return UiLanguage::English;
    }
    select_ui_language_from_multisz(&languages[..character_count as usize])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiTextKey {
    Ready,
    AutoStartStatus,
    AutoOn,
    AutoTurnOff,
    Starting,
    Cancelling,
    Listening,
    NoMatchTitle,
    NoMatchBody,
    OfflineBody,
    TryAgain,
    AppleMusicAction,
    TimeoutDescription,
    TimeoutDismiss,
    TimeoutRestart,
}

fn localized_ui_text(key: UiTextKey, language: UiLanguage) -> &'static str {
    match (key, language) {
        (UiTextKey::Ready, UiLanguage::Japanese) => "クリックしてShazamする",
        (UiTextKey::Ready, UiLanguage::English) => "Click to Shazam",
        (UiTextKey::AutoStartStatus, UiLanguage::Japanese) => {
            "\u{30c0}\u{30d6}\u{30eb}\u{30af}\u{30ea}\u{30c3}\u{30af}\u{3057}\u{3066}\u{81ea}\u{52d5}Shazam\u{3059}\u{308b}"
        }
        (UiTextKey::AutoStartStatus, UiLanguage::English) => "Double click to Auto Shazam",
        (UiTextKey::AutoOn, UiLanguage::Japanese) => {
            "Auto Shazam\u{304c}\u{30aa}\u{30f3}\u{306b}\u{306a}\u{3063}\u{3066}\u{3044}\u{307e}\u{3059}"
        }
        (UiTextKey::AutoOn, UiLanguage::English) => "Auto Shazam is on",
        (UiTextKey::AutoTurnOff, UiLanguage::Japanese) => {
            "\u{30af}\u{30ea}\u{30c3}\u{30af}\u{3057}\u{3066}\u{30aa}\u{30d5}\u{306b}\u{3059}\u{308b}"
        }
        (UiTextKey::AutoTurnOff, UiLanguage::English) => "Click to turn off",
        (UiTextKey::Starting, UiLanguage::Japanese) => "\u{958b}\u{59cb}\u{4e2d}",
        (UiTextKey::Starting, UiLanguage::English) => "Starting",
        (UiTextKey::Cancelling, UiLanguage::Japanese) => "\u{505c}\u{6b62}\u{4e2d}",
        (UiTextKey::Cancelling, UiLanguage::English) => "Stopping",
        (UiTextKey::Listening, UiLanguage::Japanese) => "聞き取り中",
        (UiTextKey::Listening, UiLanguage::English) => "Listening",
        (UiTextKey::NoMatchTitle, UiLanguage::Japanese) => "結果なし",
        (UiTextKey::NoMatchTitle, UiLanguage::English) => "No Result",
        (UiTextKey::NoMatchBody, UiLanguage::Japanese) => {
            "認識できませんでした。もう一度お試しください。"
        }
        (UiTextKey::NoMatchBody, UiLanguage::English) => {
            "We didn’t quite catch that. Please try again."
        }
        (UiTextKey::OfflineBody, UiLanguage::Japanese) => "接続を確認してください",
        (UiTextKey::OfflineBody, UiLanguage::English) => "Please check your connection",
        (UiTextKey::TryAgain, UiLanguage::Japanese) => "再度試す",
        (UiTextKey::TryAgain, UiLanguage::English) => "Try Again",
        (UiTextKey::AppleMusicAction, UiLanguage::Japanese) => {
            "Apple Music\u{3067}\u{8074}\u{304f}"
        }
        (UiTextKey::AppleMusicAction, UiLanguage::English) => "Listen on Apple Music",
        (UiTextKey::TimeoutDescription, UiLanguage::Japanese) => {
            "引き続き聴きたい？「再起動」をクリックして Shazam を続けましょう！"
        }
        (UiTextKey::TimeoutDescription, UiLanguage::English) => {
            "Still listening? Click ‘Restart’ to keep Shazaming!"
        }
        (UiTextKey::TimeoutDismiss, UiLanguage::Japanese) => "閉じる",
        (UiTextKey::TimeoutDismiss, UiLanguage::English) => "Dismiss",
        (UiTextKey::TimeoutRestart, UiLanguage::Japanese) => "再起動",
        (UiTextKey::TimeoutRestart, UiLanguage::English) => "Restart",
    }
}

fn no_match_copy(offline: bool, language: UiLanguage) -> (&'static str, &'static str) {
    let title = localized_ui_text(UiTextKey::NoMatchTitle, language);
    let body = localized_ui_text(
        if offline {
            UiTextKey::OfflineBody
        } else {
            UiTextKey::NoMatchBody
        },
        language,
    );
    (title, body)
}

fn timeout_copy(language: UiLanguage) -> (&'static str, &'static str, &'static str) {
    (
        localized_ui_text(UiTextKey::TimeoutDescription, language),
        localized_ui_text(UiTextKey::TimeoutDismiss, language),
        localized_ui_text(UiTextKey::TimeoutRestart, language),
    )
}

unsafe fn update_visible_windows(app: &mut AppState) {
    sync_main_timers(app);
    if app.visible || app.popup_transition.is_some() {
        let _ = InvalidateRect(Some(app.main_hwnd), None, false);
    }
    if app.about_visible {
        let _ = InvalidateRect(Some(app.about_hwnd), None, false);
    }
    if app.microphone_visible {
        let _ = InvalidateRect(Some(app.microphone_hwnd), None, false);
    }
}

fn phase_animation_state(phase: &MainPhase) -> (TaggingMode, bool, bool) {
    match phase {
        MainPhase::Listening {
            mode: CaptureMode::Manual,
            ..
        } => (TaggingMode::Manual, true, true),
        MainPhase::Listening {
            mode: CaptureMode::Auto,
            ..
        }
        | MainPhase::Starting {
            mode: CaptureMode::Auto,
            ..
        } => (TaggingMode::Auto, true, true),
        MainPhase::Matched { .. } => (TaggingMode::Ready, true, false),
        MainPhase::NoMatch { .. } | MainPhase::Offline { .. } => (TaggingMode::Ready, false, false),
        MainPhase::Ready
        | MainPhase::Starting {
            mode: CaptureMode::Manual,
            ..
        }
        | MainPhase::Cancelling { .. }
        | MainPhase::Error { .. } => (TaggingMode::Ready, true, false),
        MainPhase::Timeout { .. } => (TaggingMode::Ready, false, false),
    }
}

fn animation_snapshot(app: &AppState) -> AnimationSnapshot {
    let now = Instant::now();
    let (mode, show_button, show_ripples) = phase_animation_state(&app.phase);
    AnimationSnapshot {
        mode,
        elapsed: phase_elapsed(&app.phase),
        now,
        theme: app.theme,
        body_top: if app.arrow_edge == ArrowEdge::Top {
            POPUP_ARROW_HEIGHT as f32
        } else {
            0.0
        },
        status_text: status_text_snapshot(&app.phase, app.status_alternator),
        show_button,
        show_ripples,
        levels: app.ui_levels,
        auto_spring: app.auto_spring,
        no_match_elapsed: match &app.phase {
            MainPhase::NoMatch { entered_at, .. } | MainPhase::Offline { entered_at, .. } => {
                Some(entered_at.elapsed().as_secs_f64())
            }
            _ => None,
        },
        success_elapsed: match &app.phase {
            MainPhase::Matched { track, .. } => Some(track.recognized_at.elapsed().as_secs_f64()),
            _ => None,
        },
        success_track_id: match &app.phase {
            MainPhase::Matched { track, .. } => Some(track.display_id),
            _ => None,
        },
        success_wave_frame: app.success_wave_frame,
    }
}

unsafe fn sync_main_timers(app: &mut AppState) {
    let hwnd = app.main_hwnd;
    if (app.visible || app.recognition_session.is_some() || app.popup_transition.is_some())
        && !hwnd.0.is_null()
    {
        let track_phase = phase_needs_timeout(&app.phase)
            || app.history_transition.is_some()
            || app.recognition_session.is_some()
            || app.status_alternator.is_some();
        if track_phase && !app.phase_timer_active {
            let _ = SetTimer(Some(hwnd), TIMER_PHASE, PHASE_TICK_MS, None);
            app.phase_timer_active = true;
        } else if !track_phase && app.phase_timer_active {
            let _ = KillTimer(Some(hwnd), TIMER_PHASE);
            app.phase_timer_active = false;
        }
        let animate = app.popup_transition.is_some()
            || animation_needs_frame(&app.phase, app.startup_animation_pending)
            || result_overlay_fallback_needs_frame(
                &app.phase,
                app.result_overlay_composition_ready,
            )
            || (!app.status_text_composition_ready
                && app
                    .status_alternator
                    .is_some_and(|status| status.animating(Instant::now())))
            || app.history_transition.is_some_and(|transition| {
                Instant::now().saturating_duration_since(transition.started_at)
                    < HISTORY_INSERT_TRANSITION
            });
        if animate && !app.animation_timer_active {
            let _ = SetTimer(Some(hwnd), TIMER_ANIMATION, ANIMATION_TICK_MS, None);
            app.animation_timer_active = true;
        } else if !animate && app.animation_timer_active {
            let _ = KillTimer(Some(hwnd), TIMER_ANIMATION);
            app.animation_timer_active = false;
        }
    } else if !hwnd.0.is_null() {
        if app.phase_timer_active {
            let _ = KillTimer(Some(hwnd), TIMER_PHASE);
            app.phase_timer_active = false;
        }
        if app.animation_timer_active {
            let _ = KillTimer(Some(hwnd), TIMER_ANIMATION);
            app.animation_timer_active = false;
        }
    }
}

fn phase_needs_timeout(phase: &MainPhase) -> bool {
    matches!(
        phase,
        MainPhase::Matched { .. }
            | MainPhase::NoMatch { .. }
            | MainPhase::Offline { .. }
            | MainPhase::Error { .. }
    )
}

fn phase_needs_frame(phase: &MainPhase) -> bool {
    phase_needs_frame_at(phase, Instant::now())
}

fn phase_needs_frame_at(phase: &MainPhase, now: Instant) -> bool {
    match phase {
        // Listening ripples are continuous Windows Composition keyframes.
        // They do not need a UI-thread timer; future meter values enter
        // through a change event and update the audio transforms once per value.
        MainPhase::Starting {
            mode: CaptureMode::Auto,
            entered_at,
        }
        | MainPhase::Listening {
            mode: CaptureMode::Auto,
            started_at: entered_at,
        } => now.saturating_duration_since(*entered_at) < AUTO_START_SPRING_DURATION,
        MainPhase::Listening {
            mode: CaptureMode::Manual,
            ..
        } => false,
        // Result cards animate on their Composition visual. Only the D2D
        // layered-surface fallback needs per-frame UI-thread work.
        MainPhase::Matched { .. } => false,
        MainPhase::NoMatch {
            entered_at,
            returned_to_ready_at,
        }
        | MainPhase::Offline {
            entered_at,
            returned_to_ready_at,
        } => {
            // Keep UI-thread redraws only for the 300 ms panel transition and
            // the 180 ms deferred spring kickoff. The position/scale tracks
            // continue on the Composition clock once they have started.
            now.saturating_duration_since(*entered_at) < Duration::from_millis(350)
                || returned_to_ready_at.is_some_and(|start| {
                    now.saturating_duration_since(start) < Duration::from_millis(350)
                })
        }
        MainPhase::Ready
        | MainPhase::Starting { .. }
        | MainPhase::Cancelling { .. }
        | MainPhase::Error { .. }
        | MainPhase::Timeout { .. } => false,
    }
}

fn result_panel_top_at(phase: &MainPhase, body_top: f32, now: Instant) -> Option<f32> {
    if let MainPhase::Matched { stage, .. } = phase {
        return Some(match stage {
            ResultCardStage::Entering { started_at } => {
                let progress = result_card_progress(*started_at, now);
                body_top - (1.0 - progress) * TOP_HEIGHT as f32
            }
            ResultCardStage::Holding { .. } => body_top,
            ResultCardStage::Exiting {
                started_at,
                from_offset,
            } => {
                let progress = result_card_progress(*started_at, now);
                body_top + *from_offset * (1.0 - progress) + TOP_HEIGHT as f32 * progress
            }
        });
    }
    let (entered_at, returned_to_ready_at) = match phase {
        MainPhase::NoMatch {
            entered_at,
            returned_to_ready_at,
        }
        | MainPhase::Offline {
            entered_at,
            returned_to_ready_at,
        } => (*entered_at, *returned_to_ready_at),
        _ => return None,
    };
    if let Some(returned_at) = returned_to_ready_at {
        // Freeze the entry position at the close event. Advancing the entry
        // clock during exit makes the D2D fallback drift away from the
        // Composition track when a NoMatch card is closed mid-entry.
        let from_offset =
            -(1.0 - result_card_progress(entered_at, returned_at)) * TOP_HEIGHT as f32;
        let exit = result_card_progress(returned_at, now);
        Some(body_top + from_offset + (TOP_HEIGHT as f32 - from_offset) * exit)
    } else {
        let entrance = result_card_progress(entered_at, now);
        Some(body_top - (1.0 - entrance) * TOP_HEIGHT as f32)
    }
}

fn result_card_easing() -> CubicBezier {
    // UI-236 candidate A: Apple's standard Core Animation timing function.
    // It is the first Windows comparison candidate, not a measured native
    // Shazam result; the macOS presentation curve remains unresolved.
    CubicBezier {
        x1: 0.25,
        y1: 0.1,
        x2: 0.25,
        y2: 1.0,
    }
}

fn cubic_coordinate(parameter: f64, first: f64, second: f64) -> f64 {
    let inverse = 1.0 - parameter;
    3.0 * inverse * inverse * parameter * first
        + 3.0 * inverse * parameter * parameter * second
        + parameter * parameter * parameter
}

fn cubic_derivative(parameter: f64, first: f64, second: f64) -> f64 {
    let inverse = 1.0 - parameter;
    3.0 * inverse * inverse * first
        + 6.0 * inverse * parameter * (second - first)
        + 3.0 * parameter * parameter * (1.0 - second)
}

fn cubic_parameter_for_x(progress: f64, curve: CubicBezier) -> f64 {
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..48 {
        let mid = (low + high) * 0.5;
        if cubic_coordinate(mid, curve.x1, curve.x2) < progress {
            low = mid;
        } else {
            high = mid;
        }
    }
    (low + high) * 0.5
}

fn sample_cubic_timing_curve(progress: f64, curve: CubicBezier) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    if progress <= 0.0 || progress >= 1.0 {
        return progress;
    }
    let parameter = cubic_parameter_for_x(progress, curve);
    cubic_coordinate(parameter, curve.y1, curve.y2)
}

fn result_card_progress(started_at: Instant, now: Instant) -> f32 {
    let linear = (now.saturating_duration_since(started_at).as_secs_f64()
        / RESULT_CARD_TRANSITION.as_secs_f64())
    .clamp(0.0, 1.0);
    sample_cubic_timing_curve(linear, result_card_easing()) as f32
}

fn remaining_cubic_easing(progress: f64, curve: CubicBezier) -> Option<CubicBezier> {
    if progress >= 1.0 {
        return None;
    }
    if progress <= 0.0 {
        return Some(curve);
    }
    let parameter = cubic_parameter_for_x(progress, curve);
    let remaining_parameter = 1.0 - parameter;
    let start_x = cubic_coordinate(parameter, curve.x1, curve.x2);
    let start_y = cubic_coordinate(parameter, curve.y1, curve.y2);
    let start_dx = cubic_derivative(parameter, curve.x1, curve.x2);
    let start_dy = cubic_derivative(parameter, curve.y1, curve.y2);
    let first_x = start_x + remaining_parameter * start_dx / 3.0;
    let first_y = start_y + remaining_parameter * start_dy / 3.0;
    let second_x = 1.0 - remaining_parameter * (1.0 - curve.x2);
    let second_y = 1.0 - remaining_parameter * (1.0 - curve.y2);
    let x_scale = 1.0 - start_x;
    let y_scale = 1.0 - start_y;
    if x_scale <= f64::EPSILON || y_scale <= f64::EPSILON {
        return None;
    }
    Some(CubicBezier {
        x1: ((first_x - start_x) / x_scale).clamp(0.0, 1.0),
        y1: ((first_y - start_y) / y_scale).clamp(0.0, 1.0),
        x2: ((second_x - start_x) / x_scale).clamp(0.0, 1.0),
        y2: ((second_y - start_y) / y_scale).clamp(0.0, 1.0),
    })
}

fn result_overlay_motion_animation(
    phase: &MainPhase,
    now: Instant,
) -> Option<ResultOverlayMotionAnimation> {
    let curve = result_card_easing();
    let (key, from, to, started_at) = match phase {
        MainPhase::Matched { track, stage } => match stage {
            ResultCardStage::Entering { started_at } => (
                ResultOverlayMotionKey::MatchEnter {
                    display_id: track.display_id,
                    started_at: *started_at,
                },
                -(TOP_HEIGHT as f32),
                0.0,
                *started_at,
            ),
            ResultCardStage::Exiting {
                started_at,
                from_offset,
            } => (
                ResultOverlayMotionKey::MatchExit {
                    display_id: track.display_id,
                    started_at: *started_at,
                    from_offset_bits: from_offset.to_bits(),
                },
                *from_offset,
                TOP_HEIGHT as f32,
                *started_at,
            ),
            ResultCardStage::Holding { .. } => return None,
        },
        MainPhase::NoMatch {
            entered_at,
            returned_to_ready_at,
        } => match returned_to_ready_at {
            Some(returned_at) => {
                let entrance = result_card_progress(*entered_at, *returned_at);
                let from_offset = -(1.0 - entrance) * TOP_HEIGHT as f32;
                (
                    ResultOverlayMotionKey::NoMatchExit {
                        entered_at: *entered_at,
                        returned_at: *returned_at,
                        offline: false,
                        from_offset_bits: from_offset.to_bits(),
                    },
                    from_offset,
                    TOP_HEIGHT as f32,
                    *returned_at,
                )
            }
            None => (
                ResultOverlayMotionKey::NoMatchEnter {
                    entered_at: *entered_at,
                    offline: false,
                },
                -(TOP_HEIGHT as f32),
                0.0,
                *entered_at,
            ),
        },
        MainPhase::Offline {
            entered_at,
            returned_to_ready_at,
        } => match returned_to_ready_at {
            Some(returned_at) => {
                let entrance = result_card_progress(*entered_at, *returned_at);
                let from_offset = -(1.0 - entrance) * TOP_HEIGHT as f32;
                (
                    ResultOverlayMotionKey::NoMatchExit {
                        entered_at: *entered_at,
                        returned_at: *returned_at,
                        offline: true,
                        from_offset_bits: from_offset.to_bits(),
                    },
                    from_offset,
                    TOP_HEIGHT as f32,
                    *returned_at,
                )
            }
            None => (
                ResultOverlayMotionKey::NoMatchEnter {
                    entered_at: *entered_at,
                    offline: true,
                },
                -(TOP_HEIGHT as f32),
                0.0,
                *entered_at,
            ),
        },
        _ => return None,
    };

    let elapsed = now.saturating_duration_since(started_at);
    if elapsed >= RESULT_CARD_TRANSITION {
        return None;
    }
    let linear = (elapsed.as_secs_f64() / RESULT_CARD_TRANSITION.as_secs_f64()).clamp(0.0, 1.0);
    let progress = sample_cubic_timing_curve(linear, curve) as f32;
    Some(ResultOverlayMotionAnimation {
        key,
        from_offset: from + (to - from) * progress,
        to_offset: to,
        remaining: RESULT_CARD_TRANSITION.saturating_sub(elapsed),
        easing: remaining_cubic_easing(linear, curve).unwrap_or(curve),
    })
}

fn linear_transition_progress(started_at: Instant, now: Instant, duration: Duration) -> f32 {
    // UI-226 confirms the spatial endpoints and the 0.3s duration; the native
    // time-easing curve remains unresolved. Keep the linear time evaluator
    // explicitly replaceable rather than borrowing an unrelated measured curve.
    (now.saturating_duration_since(started_at).as_secs_f32() / duration.as_secs_f32())
        .clamp(0.0, 1.0)
}

fn advance_result_card_stage(stage: ResultCardStage, now: Instant) -> ResultCardAdvance {
    match stage {
        ResultCardStage::Entering { started_at } => {
            let holding_started_at = started_at + RESULT_CARD_TRANSITION;
            let exiting_started_at = holding_started_at + RESULT_CARD_HOLD;
            if now >= exiting_started_at {
                if now.saturating_duration_since(exiting_started_at) >= RESULT_CARD_TRANSITION {
                    ResultCardAdvance::Ready
                } else {
                    ResultCardAdvance::Stage(ResultCardStage::Exiting {
                        started_at: exiting_started_at,
                        from_offset: 0.0,
                    })
                }
            } else if now >= holding_started_at {
                ResultCardAdvance::Stage(ResultCardStage::Holding {
                    started_at: holding_started_at,
                })
            } else {
                ResultCardAdvance::Unchanged
            }
        }
        ResultCardStage::Holding { started_at } => {
            let exiting_started_at = started_at + RESULT_CARD_HOLD;
            if now >= exiting_started_at {
                if now.saturating_duration_since(exiting_started_at) >= RESULT_CARD_TRANSITION {
                    ResultCardAdvance::Ready
                } else {
                    ResultCardAdvance::Stage(ResultCardStage::Exiting {
                        started_at: exiting_started_at,
                        from_offset: 0.0,
                    })
                }
            } else {
                ResultCardAdvance::Unchanged
            }
        }
        ResultCardStage::Exiting { started_at, .. }
            if now.saturating_duration_since(started_at) >= RESULT_CARD_TRANSITION =>
        {
            ResultCardAdvance::Ready
        }
        _ => ResultCardAdvance::Unchanged,
    }
}

fn animation_needs_frame(phase: &MainPhase, startup_animation_pending: bool) -> bool {
    startup_animation_pending || phase_needs_frame(phase)
}

fn result_overlay_fallback_needs_frame(phase: &MainPhase, composition_ready: bool) -> bool {
    !composition_ready
        && matches!(
            phase,
            MainPhase::Matched {
                stage: ResultCardStage::Entering { .. } | ResultCardStage::Exiting { .. },
                ..
            }
        )
}

fn animation_needs_d2d_redraw(
    phase: &MainPhase,
    status_text_animating: bool,
    status_text_composition_ready: bool,
    result_overlay_composition_ready: bool,
) -> bool {
    (status_text_animating && !status_text_composition_ready)
        || (!result_overlay_composition_ready
            && matches!(
                phase,
                MainPhase::Matched { .. } | MainPhase::NoMatch { .. } | MainPhase::Offline { .. }
            ))
}

unsafe fn show_main(app: &mut AppState, mut renderer: Option<&mut Renderer>) {
    cancel_deferred_deactivation_hide(app);
    refresh_os_theme(app);
    let now = Instant::now();
    // Showing/positioning the popup can produce focus notifications before
    // the deferred SetForegroundWindow call has completed. Do not interpret
    // that activation handoff as an outside click that should close the UI.
    app.main_activated = false;
    app.popup_activation_grace_until = Some(now + POPUP_ACTIVATION_GRACE);
    let from = popup_opacity_at(app, now);
    app.popup_opacity = from;
    app.popup_transition = Some(PopupOpacityTransition {
        started_at: now,
        from,
        to: 255,
        hide_when_complete: false,
    });
    app.visible = true;
    set_auto_recognition_context(app, RecognitionContext::Foreground);
    if let Some(renderer) = renderer.as_deref_mut() {
        if let Err(error) = renderer.set_popup_opacity(from) {
            app.message = Some(format!("Popup opacity setup failed: {error}"));
        }
        match renderer.start_startup_logo() {
            Ok(complete) => app.startup_animation_pending = !complete,
            Err(error) => {
                app.startup_animation_pending = false;
                app.message = Some(format!("Startup logo animation failed: {error}"));
            }
        }
    }
    let changed = update_phase_timers(app);
    anchor_popup(app);
    if changed {
        app.message = None;
    }
    // WM_CREATE builds the scene before the tray anchor determines whether
    // the popup arrow belongs above or below the panel. Apply the resolved
    // body origin before showing the HWND so Composition never presents the
    // initial 10-DIP placeholder position for a bottom-anchored popup.
    if let Some(renderer) = renderer {
        match renderer.update_composition(animation_snapshot(app)) {
            Ok(()) => {
                app.status_text_composition_ready = renderer.status_text_composition_enabled;
            }
            Err(error) => {
                app.status_text_composition_ready = false;
                app.message = Some(format!("Composition初期配置に失敗: {error}"));
            }
        }
    }
    // The main HWND is created hidden so only the tray icon is visible during
    // initialization. Invalidating it does not make it visible; explicitly
    // show it after its anchor and initial Composition state are ready.
    let _ = ShowWindow(app.main_hwnd, SW_SHOWNOACTIVATE);
    // Explorer may restore focus to the notification area as the tray
    // callback returns. Defer activation until the callback has unwound.
    let _ = PostMessageW(
        Some(app.main_hwnd),
        WM_ACTIVATE_MAIN_POPUP,
        WPARAM(0),
        LPARAM(0),
    );
    update_visible_windows(app);
}

unsafe fn cancel_deferred_deactivation_hide(app: &mut AppState) {
    if app.deactivation_hide_pending {
        let _ = KillTimer(Some(app.main_hwnd), TIMER_DEFERRED_DEACTIVATION_HIDE);
        app.deactivation_hide_pending = false;
    }
}

unsafe fn defer_or_hide_main_after_deactivation(app: &mut AppState) {
    if !should_hide_main_after_deactivation(app, Instant::now()) {
        return;
    }
    let cursor_over_tray = cursor_over_tray_icon(app.main_hwnd);
    if !should_defer_hide_for_tray_activation(cursor_over_tray) {
        hide_main(app);
        return;
    }
    if app.deactivation_hide_pending {
        return;
    }
    app.deactivation_hide_pending = true;
    if SetTimer(
        Some(app.main_hwnd),
        TIMER_DEFERRED_DEACTIVATION_HIDE,
        DEACTIVATION_HIDE_DELAY_MS,
        None,
    ) == 0
    {
        app.deactivation_hide_pending = false;
        hide_main(app);
    }
}

unsafe fn cursor_over_tray_icon(hwnd: HWND) -> Option<bool> {
    let mut cursor = POINT::default();
    GetCursorPos(&mut cursor).ok()?;
    let identifier = NOTIFYICONIDENTIFIER {
        cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        guidItem: TRAY_ICON_GUID,
    };
    let tray = Shell_NotifyIconGetRect(&identifier).ok()?;
    Some(
        cursor.x >= tray.left
            && cursor.x < tray.right
            && cursor.y >= tray.top
            && cursor.y < tray.bottom,
    )
}

fn should_defer_hide_for_tray_activation(cursor_over_tray: Option<bool>) -> bool {
    // Explorer can report activation loss before delivering the icon's own
    // selection callback. When the pointer is on the icon (or the shell cannot
    // resolve its overflow-area rectangle), let that callback decide whether
    // the already-open popup should close. Ordinary outside clicks still hide
    // immediately.
    cursor_over_tray != Some(false)
}

fn refresh_os_theme(app: &mut AppState) -> bool {
    let Ok((system_theme, os_foreground)) = theme_from_ui_settings(&app.theme_settings) else {
        return false;
    };
    let theme = app.theme_preference.resolve(system_theme);
    let system_foreground = match app.theme_preference {
        ThemePreference::System => os_foreground,
        ThemePreference::Light | ThemePreference::Dark => foreground_for_theme(theme),
    };
    if app.theme == theme && app.system_foreground == system_foreground {
        return false;
    }
    app.theme = theme;
    app.system_foreground = system_foreground;
    app.theme_generation = app.theme_generation.wrapping_add(1);
    true
}

unsafe fn process_main_hit(app: &mut AppState, hit: MainHit) {
    match hit {
        MainHit::Settings => open_settings(app),
        MainHit::Cancel => cancel_capture(app),
        MainHit::TimeoutRestart => {
            if let MainPhase::Timeout { mode, .. } = app.phase {
                start_capture(app, mode);
            }
        }
        MainHit::TimeoutDismiss => app.phase = MainPhase::Ready,
        MainHit::TryAgain => start_capture(app, CaptureMode::Manual),
        MainHit::CloseResult => {
            let now = Instant::now();
            let body_top = if app.arrow_edge == ArrowEdge::Top {
                POPUP_ARROW_HEIGHT as f32
            } else {
                0.0
            };
            let from_offset = result_panel_top_at(&app.phase, body_top, now)
                .map(|top| top - body_top)
                .unwrap_or(0.0);
            match &mut app.phase {
                MainPhase::Matched { stage, .. } => {
                    *stage = ResultCardStage::Exiting {
                        started_at: now,
                        from_offset,
                    };
                }
                MainPhase::NoMatch {
                    returned_to_ready_at,
                    ..
                }
                | MainPhase::Offline {
                    returned_to_ready_at,
                    ..
                } if returned_to_ready_at.is_none() => *returned_to_ready_at = Some(now),
                _ => {}
            }
            app.message = None;
        }
        MainHit::OpenTrack(display_id) => {
            let current_result = match &app.phase {
                MainPhase::Matched { track, .. } => Some(track),
                _ => None,
            };
            if let Some(url) =
                page_url_for_display_id(&app.history, current_result, display_id, &app.ui_configs)
            {
                if let Err(error) = unsafe { open_track_page(app.main_hwnd, &url) } {
                    eprintln!("OpenShazam could not launch the Shazam page: {error}");
                    app.message = Some("Could not open the track page".to_owned());
                }
            }
        }
        MainHit::CenterButton => {
            if capture_mode_in_progress(&app.phase).is_some() {
                cancel_capture(app);
            } else {
                start_capture(app, CaptureMode::Manual);
            }
        }
        MainHit::Other => {}
    }
}

fn capture_mode_in_progress(phase: &MainPhase) -> Option<CaptureMode> {
    match phase {
        MainPhase::Starting { mode, .. }
        | MainPhase::Listening { mode, .. }
        | MainPhase::Cancelling { mode, .. } => Some(*mode),
        _ => None,
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
        let data = create.lpCreateParams as *mut WindowData;
        (*data).hwnd = hwnd;
        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, data as isize);
        return LRESULT(1);
    }

    // This must run before `window_data()` creates an `&mut WindowData` and
    // before taking `&mut AppState`: showing WinUI synchronously re-enters the
    // owner window's procedure while activation changes.
    if message == WM_OPEN_SETTINGS {
        let data_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowData;
        if data_ptr.is_null() {
            return LRESULT(0);
        }
        return open_settings_after_wndproc((*data_ptr).app);
    }

    let Some(data) = window_data(hwnd) else {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    };
    let app = &mut *data.app;

    match message {
        _ if message == app.taskbar_created_message && app.taskbar_created_message != 0 => {
            restore_tray_icon(hwnd);
            LRESULT(0)
        }
        WM_SHOW_INITIAL_POPUP if data.kind == WindowKind::Main => {
            show_main(app, data.renderer.as_mut());
            LRESULT(0)
        }
        WM_ACTIVATE_MAIN_POPUP if data.kind == WindowKind::Main => {
            if app.visible {
                let _ = SetForegroundWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_ARTWORK_READY if data.kind == WindowKind::Main => {
            let pointer = lparam.0 as *mut ArtworkReady;
            if !pointer.is_null() {
                let ready = Box::from_raw(pointer);
                let identity = ready.identity;
                let bound_track = app
                    .history
                    .iter()
                    .find(|track| track.display_id == identity.track.display_id);
                let current_request = app.active_artwork_requests.get(&identity.track);
                if artwork_request_matches_track(current_request, identity, bound_track) {
                    let pixels = ready.pixels;
                    let existing_is_better = app
                        .artwork_resources
                        .get(&identity.track.display_id)
                        .is_some_and(|(source, cached)| {
                            !should_replace_cached_artwork(
                                Some((*source, cached.fingerprint)),
                                ready.source,
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
                            let Some(oldest) = app.artwork_resources.keys().copied().min() else {
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
                        unsafe { notify_hidden_match(app, &track, pixels.as_ref()) };
                    }
                    if app.visible {
                        let _ = InvalidateRect(Some(app.main_hwnd), None, false);
                    }
                }
            }
            LRESULT(0)
        }
        WM_ARTWORK_LOAD_FINISHED if data.kind == WindowKind::Main => {
            let pointer = lparam.0 as *mut ArtworkRequestIdentity;
            if !pointer.is_null() {
                let identity = *Box::from_raw(pointer);
                if finish_active_artwork_request(&mut app.active_artwork_requests, identity) {
                    if let Some(track) = app
                        .pending_match_notifications
                        .remove(&identity.track.display_id)
                    {
                        let pixels = app
                            .artwork_resources
                            .get(&identity.track.display_id)
                            .map(|(_, pixels)| pixels.clone());
                        unsafe { notify_hidden_match(app, &track, pixels.as_ref()) };
                    }
                    let track_is_retained = app
                        .history
                        .iter()
                        .any(|track| track.display_id == identity.track.display_id);
                    if !track_is_retained {
                        app.artwork_generations.remove(&identity.track);
                        app.artwork_resources.remove(&identity.track.display_id);
                    }
                }
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
        WM_CREATE => {
            let dpi = GetDpiForWindow(hwnd).max(96);
            let animate_startup_logo =
                should_start_startup_logo(data.kind, app.startup_logo_consumed);
            match Renderer::new(
                hwnd,
                app.d2d_factory.clone(),
                app.dwrite_factory.clone(),
                app.compositor.clone(),
                dpi,
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
                        app.startup_animation_pending = renderer
                            .composition
                            .as_ref()
                            .is_some_and(|scene| !scene.startup_logo_complete());
                    }
                    data.renderer = Some(renderer);
                }
                Err(error) => app.message = Some(format!("Direct2D初期化失敗: {error}")),
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut paint = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut paint);
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
                                renderer.status_text_composition_enabled;
                        }
                        Err(error) => {
                            app.status_text_composition_ready = false;
                            app.message =
                                Some(format!("Windows Composition update failed: {error}"));
                        }
                    }
                }
                if let Err(error) = renderer.draw(app, data.kind) {
                    app.message = Some(format!("描画エラー: {error}"));
                }
            }
            if data.kind == WindowKind::Main {
                app.result_overlay_composition_ready = data
                    .renderer
                    .as_ref()
                    .is_some_and(|renderer| renderer.result_overlay_matches_current(app));
                sync_main_timers(app);
            }
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_SIZE => {
            if let Some(renderer) = data.renderer.as_mut() {
                renderer.on_size(hwnd);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let new_dpi = ((wparam.0 >> 16) & 0xffff) as u32;
            let suggested = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            if let Some(renderer) = data.renderer.as_mut() {
                renderer.on_dpi_changed(new_dpi, hwnd, app);
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
            } else if event == windows::Win32::UI::WindowsAndMessaging::WM_CONTEXTMENU as u16 {
                show_tray_context_menu(app);
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
        WM_LBUTTONDOWN if data.kind == WindowKind::Main => {
            let (x, y) = client_dips(hwnd, lparam);
            let body_top = if app.arrow_edge == ArrowEdge::Top {
                POPUP_ARROW_HEIGHT as f32
            } else {
                0.0
            };
            app.mouse_down_center =
                main_hit_test(app, x, y, body_top, data.renderer.as_ref()) == MainHit::CenterButton;
            app.long_press_consumed = false;
            if app.mouse_down_center {
                let _ = SetCapture(hwnd);
                let _ = SetTimer(Some(hwnd), TIMER_LONG_PRESS, LONG_PRESS_TIMEOUT_MS, None);
            }
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK if data.kind == WindowKind::Main => {
            let _ = KillTimer(Some(hwnd), TIMER_SINGLE_CLICK);
            let _ = KillTimer(Some(hwnd), TIMER_LONG_PRESS);
            app.single_click_pending = false;
            let (x, y) = client_dips(hwnd, lparam);
            let body_top = if app.arrow_edge == ArrowEdge::Top {
                POPUP_ARROW_HEIGHT as f32
            } else {
                0.0
            };
            if main_hit_test(app, x, y, body_top, data.renderer.as_ref()) == MainHit::CenterButton {
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
                process_main_hit(
                    app,
                    main_hit_test(app, x, y, body_top, data.renderer.as_ref()),
                );
            }
            update_visible_windows(app);
            LRESULT(0)
        }
        WM_LBUTTONUP if data.kind == WindowKind::Main => {
            let _ = KillTimer(Some(hwnd), TIMER_LONG_PRESS);
            if app.mouse_down_center {
                let _ = ReleaseCapture();
                app.mouse_down_center = false;
                if !app.long_press_consumed {
                    app.single_click_pending = true;
                    let timeout = GetDoubleClickTime().clamp(180, SINGLE_CLICK_TIMEOUT_MS);
                    let _ = SetTimer(Some(hwnd), TIMER_SINGLE_CLICK, timeout, None);
                }
                app.long_press_consumed = false;
            } else {
                let (x, y) = client_dips(hwnd, lparam);
                let body_top = if app.arrow_edge == ArrowEdge::Top {
                    POPUP_ARROW_HEIGHT as f32
                } else {
                    0.0
                };
                let hit = main_hit_test(app, x, y, body_top, data.renderer.as_ref());
                process_main_hit(app, hit);
                update_visible_windows(app);
            }
            LRESULT(0)
        }
        WM_TIMER if data.kind == WindowKind::Main && wparam.0 == TIMER_PHASE => {
            let recognition_changed = poll_recognition_events(app);
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
                                renderer.status_text_composition_enabled;
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
            if data.kind == WindowKind::Main && wparam.0 == TIMER_DEFERRED_DEACTIVATION_HIDE =>
        {
            cancel_deferred_deactivation_hide(app);
            if should_hide_main_after_deactivation(app, Instant::now()) {
                hide_main(app);
                update_visible_windows(app);
            }
            LRESULT(0)
        }
        WM_TIMER if data.kind == WindowKind::Main && wparam.0 == TIMER_ANIMATION => {
            if app.visible || app.popup_transition.is_some() {
                let now = Instant::now();
                let fading = app.popup_transition.is_some();
                if fading {
                    advance_popup_transition(app, now);
                    if let Some(renderer) = data.renderer.as_mut() {
                        if let Err(error) = renderer.set_popup_opacity(app.popup_opacity) {
                            app.message = Some(format!("Popup opacity update failed: {error}"));
                        }
                    }
                    if app.visible || app.popup_transition.is_some() {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                if app.visible {
                    let composition_frame_needed =
                        animation_needs_frame(&app.phase, app.startup_animation_pending);
                    if composition_frame_needed {
                        if let Some(renderer) = data.renderer.as_mut() {
                            match renderer.update_composition(animation_snapshot(app)) {
                                Ok(()) => {
                                    app.status_text_composition_ready =
                                        renderer.status_text_composition_enabled;
                                    if app.startup_animation_pending {
                                        app.startup_animation_pending = renderer
                                            .composition
                                            .as_ref()
                                            .is_some_and(|scene| !scene.startup_logo_complete());
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
                    let history_row_animating = app.history_transition.is_some_and(|transition| {
                        now.saturating_duration_since(transition.started_at)
                            < HISTORY_INSERT_TRANSITION
                    });
                    if history_row_animating
                        || animation_needs_d2d_redraw(
                            &app.phase,
                            status_text_animating,
                            app.status_text_composition_ready,
                            result_overlay_composition_ready,
                        )
                    {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
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
            let (x, y) = client_dips(hwnd, lparam);
            let body_top = if app.arrow_edge == ArrowEdge::Top {
                POPUP_ARROW_HEIGHT as f32
            } else {
                0.0
            };
            let history_top = body_top + TOP_HEIGHT as f32;
            let next = if (0.0..CONTENT_WIDTH as f32).contains(&x) {
                history_row_index_at(
                    y,
                    history_top,
                    history_top + HISTORY_HEIGHT as f32,
                    app.history.len(),
                    app.history_transition,
                    app.scroll_offset,
                    Instant::now(),
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
        WM_MOUSEWHEEL if data.kind == WindowKind::Main => {
            let mut cursor = POINT {
                x: low_word(lparam.0) as i16 as i32,
                y: high_word(lparam.0) as i16 as i32,
            };
            if !ScreenToClient(hwnd, &mut cursor).as_bool() {
                return DefWindowProcW(hwnd, message, wparam, lparam);
            }
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
                return DefWindowProcW(hwnd, message, wparam, lparam);
            }

            let delta = (high_word(wparam.0 as isize) as i16) as f32;
            let next = history_scroll_offset_after_wheel(
                app.scroll_offset,
                delta,
                system_wheel_scroll_lines(),
                app.history.len(),
                HISTORY_HEIGHT as f32,
            );
            if next != app.scroll_offset {
                app.scroll_offset = next;
                app.hovered_row = history_row_index_at(
                    y,
                    history_top,
                    history_top + HISTORY_HEIGHT as f32,
                    app.history.len(),
                    app.history_transition,
                    app.scroll_offset,
                    Instant::now(),
                );
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 == VK_ESCAPE.0 as usize {
                match data.kind {
                    WindowKind::Main => hide_main(app),
                    WindowKind::About => {
                        app.about_visible = false;
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                    WindowKind::MicrophoneInfo => {
                        app.microphone_visible = false;
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                }
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
            if main_hit_test(app, x, y, body_top, data.renderer.as_ref()) == MainHit::CenterButton {
                if capture_mode_in_progress(&app.phase) == Some(CaptureMode::Auto) {
                    cancel_capture(app);
                } else {
                    start_capture(app, CaptureMode::Auto);
                }
                update_visible_windows(app);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP if data.kind == WindowKind::About => {
            app.about_visible = false;
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        WM_LBUTTONUP if data.kind == WindowKind::MicrophoneInfo => {
            app.microphone_visible = false;
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        WM_CLOSE => {
            match data.kind {
                WindowKind::Main => hide_main(app),
                WindowKind::About => {
                    app.about_visible = false;
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                WindowKind::MicrophoneInfo => {
                    app.microphone_visible = false;
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
            LRESULT(0)
        }
        WM_SETFOCUS if data.kind == WindowKind::Main => {
            cancel_deferred_deactivation_hide(app);
            app.main_activated = true;
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_KILLFOCUS if data.kind == WindowKind::Main => {
            defer_or_hide_main_after_deactivation(app);
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_ACTIVATE if data.kind == WindowKind::Main => {
            let activation = wparam.0 & 0xffff;
            if activation == 0 {
                defer_or_hide_main_after_deactivation(app);
            } else if activation != 0 {
                cancel_deferred_deactivation_hide(app);
                app.main_activated = true;
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_NCDESTROY => {
            let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowData;
            let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if !pointer.is_null() {
                let data = Box::from_raw(pointer);
                match data.kind {
                    WindowKind::Main => app.main_hwnd = HWND::default(),
                    WindowKind::About => app.about_hwnd = HWND::default(),
                    WindowKind::MicrophoneInfo => app.microphone_hwnd = HWND::default(),
                }
                drop(data);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_DESTROY if data.kind == WindowKind::Main => {
            delete_tray_icon(hwnd);
            for icon in app.owned_balloon_icons.drain(..).flatten() {
                let _ = DestroyIcon(icon);
            }
            if let Some(controller) = app.recognition_controller.take() {
                thread::spawn(move || {
                    let _ = controller.join();
                });
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn low_word(value: isize) -> u16 {
    value as u16
}

fn high_word(value: isize) -> u16 {
    ((value as usize >> 16) & 0xffff) as u16
}

fn system_wheel_scroll_lines() -> u32 {
    let mut lines = 3u32;
    let result = unsafe {
        SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            Some((&mut lines as *mut u32).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if result.is_ok() {
        lines
    } else {
        3
    }
}

fn no_auxiliary_window_visible(app: &AppState) -> bool {
    !app.settings_opening
        && !app
            .settings_host
            .as_ref()
            .is_some_and(|host| host.visible())
        && !app.about_visible
        && !app.microphone_visible
}

fn should_hide_main_after_deactivation(app: &AppState, now: Instant) -> bool {
    should_hide_popup_on_deactivation(
        app.visible,
        app.main_activated,
        no_auxiliary_window_visible(app),
        app.popup_activation_grace_until,
        now,
    )
}

fn should_hide_popup_on_deactivation(
    visible: bool,
    main_activated: bool,
    no_auxiliary_window: bool,
    activation_grace_until: Option<Instant>,
    now: Instant,
) -> bool {
    visible
        && main_activated
        && no_auxiliary_window
        && activation_grace_until.is_none_or(|deadline| now >= deadline)
}

unsafe fn client_dips(hwnd: HWND, lparam: LPARAM) -> (f32, f32) {
    let scale = 96.0 / GetDpiForWindow(hwnd).max(96) as f32;
    let x = (low_word(lparam.0) as i16 as f32) * scale;
    let y = (high_word(lparam.0) as i16 as f32) * scale;
    (x, y)
}

fn window_class(instance: HINSTANCE, proc: WNDPROC, class_name: PCWSTR) -> WNDCLASSW {
    WNDCLASSW {
        style: CS_DBLCLKS,
        lpfnWndProc: proc,
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        lpszClassName: class_name,
        ..Default::default()
    }
}

/// Start the resident Windows UI. The hidden notification-area host is kept
/// alive when the popup is closed; its draw path sleeps until an event arrives.
pub fn run(start_hidden: bool) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let silent_startup = start_hidden || is_startup_task_activation();
        let (instance_mutex, already_running) = acquire_single_instance_mutex()?;
        if already_running {
            // A second explicit UI launch reuses the resident tray process.
            // A duplicate logon activation exits quietly and never foregrounds
            // the existing popup.
            if !silent_startup && !activate_existing_instance() {
                eprintln!("OpenShazam is already running, but its window could not be activated.");
            }
            drop(instance_mutex);
            CoUninitialize();
            return Ok(());
        }
        let _instance_mutex = instance_mutex;
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
            taskbar_created_message,
            settings_host: None,
            settings_opening: false,
            about_hwnd: HWND::default(),
            microphone_hwnd: HWND::default(),
            d2d_factory,
            dwrite_factory,
            compositor,
            _dispatcher_queue: dispatcher_queue,
            theme_settings,
            theme_changed_token: None,
            system_foreground,
            phase: MainPhase::Ready,
            history: Vec::new(),
            history_transition: None,
            scroll_offset: 0.0,
            hovered_row: None,
            ui_levels: UiLevels::default(),
            auto_spring: None,
            theme,
            theme_preference,
            theme_generation: 0,
            visible: false,
            popup_opacity: 0,
            popup_transition: None,
            main_activated: false,
            popup_activation_grace_until: None,
            deactivation_hide_pending: false,
            phase_timer_active: false,
            animation_timer_active: false,
            result_overlay_composition_ready: false,
            status_text_composition_ready: false,
            startup_animation_pending: false,
            startup_logo_consumed: false,
            about_visible: false,
            microphone_visible: false,
            arrow_edge: ArrowEdge::Top,
            arrow_center: (CONTENT_WIDTH * 0.5) as f32,
            mouse_down_center: false,
            long_press_consumed: false,
            single_click_pending: false,
            recognition_controller: None,
            recognition_session: None,
            auto_candidate_dedupe: VecDeque::new(),
            controller_sequence: 0,
            dropped_notifications: 0,
            status_alternator: Some(StatusTextAlternator::ready(Instant::now())),
            success_wave_frame: None,
            next_display_id: 1,
            next_artwork_request_id: 1,
            artwork_generations: HashMap::new(),
            active_artwork_requests: HashMap::new(),
            artwork_resources: HashMap::new(),
            pending_match_notifications: HashMap::new(),
            owned_balloon_icons: VecDeque::new(),
            ui_configs: HashMap::new(),
            message: None,
        });
        let app_ptr = Box::into_raw(app);
        let dpi = 96u32;
        let hwnd = create_window(
            app_ptr,
            WindowKind::Main,
            "OpenShazam",
            ui_model::physical_pixels(MAIN_WINDOW_WIDTH, dpi),
            ui_model::physical_pixels(MAIN_WINDOW_HEIGHT, dpi),
            false,
        )?;
        (*app_ptr).main_hwnd = hwnd;
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
                let _ = DestroyWindow(hwnd);
                drop(Box::from_raw(app_ptr));
                return Err(Box::new(error));
            }
        }
        if let Err(error) = add_tray_icon(hwnd) {
            if let Some(token) = (*app_ptr).theme_changed_token.take() {
                let _ = (*app_ptr).theme_settings.RemoveColorValuesChanged(token);
            }
            let _ = DestroyWindow(hwnd);
            drop(Box::from_raw(app_ptr));
            return Err(Box::new(error));
        }
        // Let Explorer finish publishing the notification icon before asking
        // Shell_NotifyIconGetRect for its anchor. `ui` opens the popup on
        // launch; the tray icon remains available to hide/show it afterward.
        if should_show_initial_popup(silent_startup) {
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
        if !(*app_ptr).about_hwnd.0.is_null() {
            let _ = DestroyWindow((*app_ptr).about_hwnd);
        }
        if !(*app_ptr).microphone_hwnd.0.is_null() {
            let _ = DestroyWindow((*app_ptr).microphone_hwnd);
        }
        if !(*app_ptr).main_hwnd.0.is_null() {
            let _ = DestroyWindow((*app_ptr).main_hwnd);
        }
        drop(Box::from_raw(app_ptr));
        CoUninitialize();
    }
    Ok(())
}

fn update_phase_timers(app: &mut AppState) -> bool {
    let now = Instant::now();
    let mut changed = false;
    match &mut app.phase {
        MainPhase::Matched { stage, .. } => match advance_result_card_stage(*stage, now) {
            ResultCardAdvance::Unchanged => {}
            ResultCardAdvance::Stage(next) => {
                *stage = next;
                changed = true;
            }
            ResultCardAdvance::Ready => {
                app.phase = MainPhase::Ready;
                changed = true;
            }
        },
        MainPhase::NoMatch {
            entered_at,
            returned_to_ready_at,
        }
        | MainPhase::Offline {
            entered_at,
            returned_to_ready_at,
        } => {
            if returned_to_ready_at.is_none()
                && now.duration_since(*entered_at) >= Duration::from_secs(10)
            {
                *returned_to_ready_at = Some(now);
                changed = true;
            } else if returned_to_ready_at
                .is_some_and(|start| now.duration_since(start) >= Duration::from_millis(300))
            {
                app.phase = MainPhase::Ready;
                changed = true;
            }
        }
        MainPhase::Error { entered_at, .. }
            if now.duration_since(*entered_at) >= Duration::from_secs(8) =>
        {
            app.phase = MainPhase::Ready;
            changed = true;
        }
        _ => {}
    }
    let desired_status_cycle = match &app.phase {
        MainPhase::Ready | MainPhase::Matched { .. } => Some(StatusCycle::Ready),
        MainPhase::Listening {
            mode: CaptureMode::Auto,
            ..
        } => Some(StatusCycle::Auto),
        MainPhase::Starting {
            mode: CaptureMode::Auto,
            ..
        } => Some(StatusCycle::Auto),
        _ => None,
    };
    match desired_status_cycle {
        Some(cycle)
            if app
                .status_alternator
                .is_none_or(|status| status.cycle != cycle) =>
        {
            app.status_alternator = Some(match cycle {
                StatusCycle::Ready => StatusTextAlternator::ready(now),
                StatusCycle::Auto => StatusTextAlternator::auto(now),
            });
            changed = true;
        }
        None if app.status_alternator.is_some() => {
            app.status_alternator = None;
            changed = true;
        }
        _ => {}
    }
    if app.history_transition.is_some_and(|transition| {
        now.saturating_duration_since(transition.started_at) >= HISTORY_INSERT_TRANSITION
    }) {
        app.history_transition = None;
        changed = true;
    }
    if let Some(status) = app.status_alternator.as_mut() {
        changed |= status.advance(now);
    }
    changed
}

fn history_row_top(
    index: usize,
    transition: Option<HistoryMutationTransition>,
    history_top: f32,
    scroll_offset: f32,
    now: Instant,
) -> f32 {
    let base = history_top + index as f32 * HISTORY_ROW_PITCH - scroll_offset;
    let Some(transition) = transition else {
        return base;
    };
    let progress =
        linear_transition_progress(transition.started_at, now, HISTORY_INSERT_TRANSITION);
    match transition.event {
        HistoryMutationEvent::Insert {
            index: inserted, ..
        } if index >= inserted => base - HISTORY_ROW_PITCH * (1.0 - progress),
        HistoryMutationEvent::Delete { index: deleted } if index >= deleted => {
            base + HISTORY_ROW_PITCH * (1.0 - progress)
        }
        HistoryMutationEvent::Insert { .. }
        | HistoryMutationEvent::Update { .. }
        | HistoryMutationEvent::Delete { .. } => base,
    }
}

fn history_row_index_at(
    y: f32,
    history_top: f32,
    history_bottom: f32,
    item_count: usize,
    transition: Option<HistoryMutationTransition>,
    scroll_offset: f32,
    now: Instant,
) -> Option<usize> {
    if y < history_top || y >= history_bottom {
        return None;
    }
    (0..item_count).find(|&index| {
        let top = history_row_top(index, transition, history_top, scroll_offset, now);
        visible_history_row_hit(y, top, history_top, history_bottom)
    })
}

unsafe fn hovered_history_row_for_cursor(app: &AppState) -> Option<usize> {
    if !app.visible {
        return None;
    }
    let mut cursor = POINT::default();
    GetCursorPos(&mut cursor).ok()?;
    if !ScreenToClient(app.main_hwnd, &mut cursor).as_bool() {
        return None;
    }
    let scale = 96.0 / GetDpiForWindow(app.main_hwnd).max(96) as f32;
    let x = cursor.x as f32 * scale;
    if !(0.0..CONTENT_WIDTH as f32).contains(&x) {
        return None;
    }
    let y = cursor.y as f32 * scale;
    let body_top = if app.arrow_edge == ArrowEdge::Top {
        POPUP_ARROW_HEIGHT as f32
    } else {
        0.0
    };
    let history_top = body_top + TOP_HEIGHT as f32;
    history_row_index_at(
        y,
        history_top,
        history_top + HISTORY_HEIGHT as f32,
        app.history.len(),
        app.history_transition,
        app.scroll_offset,
        Instant::now(),
    )
}

#[cfg(test)]
mod tests {
    use super::StatusTextAlternator;
    use super::{
        advance_result_card_stage, animation_needs_frame, artwork_pixel_fingerprint,
        artwork_request_is_current, artwork_request_matches_track,
        artwork_request_urls_match_track, auto_shadow_host_center, button_border_arc_points,
        capture_animation_start, capture_mode_in_progress, composition_button_pixel_snap_offset,
        composition_header_viewport_offset, composition_root_layer_order,
        composition_scale_for_dpi, composition_scene_layer_order, copy_notification_text,
        copy_panel_pixels_into, create_text_format_with_locale, d2d_color_from_system,
        directwrite_locale, fallback_match_artwork_rect, finish_active_artwork_request,
        gradient_origin_transform, header_fill_scope, history_artwork_frame, history_row_top,
        history_text_rects, history_title_color, inverse_circle_clip_radii, is_offline_failure,
        is_tray_activation_event, localized_ui_text, native_icon_layout, no_match_copy,
        no_match_icon_rect, page_url_for_display_id, phase_animation_state, phase_needs_frame_at,
        phase_needs_timeout, point_in_rect, popup_base_color, remember_auto_candidate_key,
        resolve_page_link, resolve_page_link_checked, resolve_page_link_for_generation,
        result_artwork_rect, result_card_link_hit, result_details_rects, result_gradient_colors,
        result_header_hit_test_contains, result_header_visible_height, result_overlay_child_offset,
        result_overlay_occludes_shazam_button, result_panel_draw_top, result_panel_top_at,
        scale_notification_artwork_to_size, select_ui_language_from_multisz,
        should_defer_hide_for_tray_activation, should_hide_popup_on_deactivation,
        should_replace_cached_artwork, should_show_initial_popup, should_start_startup_logo,
        status_text_snapshot, theme_from_foreground_rgb, timeout_copy, ui_model,
        valid_page_template, visible_history_row_hit, ArrowEdge, ArtworkPixels, ArtworkRequest,
        ArtworkRequestIdentity, ArtworkSource, CaptureMode, CompositionRootLayer,
        CompositionSceneLayer, HeaderFillScope, HistoryMutationEvent, HistoryMutationTransition,
        HistoryTimestampSource, MainPhase, NoMatchIconCommand, PageLinkFailure, ResultCardAdvance,
        ResultCardStage, Theme, ThemePreference, Track, TrackIdentity, UiConfigSnapshot,
        UiLanguage, UiTextKey, ValidatedWebTrackUrl, WindowKind, AUTO_SHADOW_DISPLAY_BOUNDS,
        AUTO_START_SPRING_DURATION, BUTTON_CLEAR_DIAMETER, BUTTON_SIZE, BUTTON_TOP, BUTTON_X,
        CLOSE_BUTTON_GA_PIXELS, FALLBACK_MATCH_DARK_PIXELS, FALLBACK_MATCH_LIGHT_PIXELS,
        FALLBACK_MATCH_LOGICAL_SIDE, HISTORY_HEIGHT, HISTORY_ROW_HEIGHT, INNER_MASK_BOUNDS,
        NO_MATCH_ICON_PATH, OUTER_BASE_OPACITY, POPUP_ACTIVATION_GRACE, POPUP_ARROW_HEIGHT,
        RESULT_ACTION_HEIGHT, RESULT_ARTWORK_CORNER_RADIUS, RESULT_CARD_HOLD,
        RESULT_CARD_TRANSITION, RESULT_CLOSE_ICON_PIXELS, RESULT_DETAILS_LEFT,
        RESULT_DETAILS_RIGHT, SETTINGS_ICON_PIXELS, TOP_HEIGHT, WHITE_CLOSE_ICON_PIXELS,
    };
    use std::collections::HashMap;
    use std::collections::VecDeque;
    use std::time::SystemTime;
    use std::time::{Duration, Instant, UNIX_EPOCH};

    #[test]
    fn named_single_instance_mutex_tracks_live_handle_lifetime() {
        let name = format!("Local\\OpenShazam.WindowsUi.Test.{}", std::process::id());
        let (first, already_running) = super::acquire_named_single_instance_mutex(&name).unwrap();
        assert!(!already_running);

        let (second, already_running) = super::acquire_named_single_instance_mutex(&name).unwrap();
        assert!(already_running);
        drop(second);

        let (while_first_is_alive, already_running) =
            super::acquire_named_single_instance_mutex(&name).unwrap();
        assert!(already_running);
        drop(while_first_is_alive);
        drop(first);

        let (_after_last_handle_is_closed, already_running) =
            super::acquire_named_single_instance_mutex(&name).unwrap();
        assert!(!already_running);
    }

    #[test]
    fn ui229_history_keeps_controller_event_time_when_ui_delivery_is_delayed() {
        let emitted = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let received = emitted + Duration::from_secs(3_600);
        assert_eq!(
            super::resolve_history_timestamp(Some(emitted), received),
            (emitted, HistoryTimestampSource::ControllerEvent)
        );
        assert_eq!(
            super::resolve_history_timestamp(None, received),
            (received, HistoryTimestampSource::HostReceivedFallback)
        );
    }

    #[test]
    fn ui229_history_date_uses_absolute_utc_timestamp_and_localized_formatter() {
        assert_eq!(
            super::winrt_datetime_from_system_time(UNIX_EPOCH).UniversalTime,
            super::WINDOWS_EPOCH_OFFSET_100NS
        );
        assert_eq!(
            super::winrt_datetime_from_system_time(UNIX_EPOCH + Duration::from_millis(1_000))
                .UniversalTime,
            super::WINDOWS_EPOCH_OFFSET_100NS + 10_000_000
        );
        assert_eq!(
            super::WINDOWS_DATETIME_TEMPLATE,
            "day month.abbreviated year hour minute"
        );

        unsafe {
            super::CoInitializeEx(None, super::COINIT_APARTMENTTHREADED).unwrap();
            let label = super::format_history_date_label(UNIX_EPOCH);
            super::CoUninitialize();
            assert!(!label.is_empty());
            assert_ne!(label, "たった今");
        }
    }

    #[test]
    fn ui234_result_card_is_rasterized_once_then_composition_moves_only_its_visual() {
        let body_top = 10.0;
        assert_eq!(
            result_panel_draw_top(Some(body_top - TOP_HEIGHT as f32), body_top, true, false),
            Some(body_top)
        );
        assert_eq!(
            result_panel_draw_top(Some(body_top + 90.0), body_top, true, true),
            None
        );
        assert_eq!(
            result_panel_draw_top(Some(body_top + 90.0), body_top, false, false),
            Some(body_top + 90.0)
        );
        assert_eq!(result_panel_draw_top(None, body_top, true, false), None);
    }

    #[test]
    fn ui236_result_card_uses_candidate_a_and_residual_curve_without_restarting_time() {
        let curve = super::result_card_easing();
        for (time, expected) in [(0.25, 0.4085), (0.5, 0.8024), (0.75, 0.9605)] {
            assert!(
                (super::sample_cubic_timing_curve(time, curve) - expected).abs() < 0.001,
                "progress at {time:.2} should match UI-236 candidate A"
            );
        }

        for start in [0.0, 0.1, 0.25, 0.5, 0.75, 0.9] {
            let residual = super::remaining_cubic_easing(start, curve).unwrap();
            let y_start = super::sample_cubic_timing_curve(start, curve);
            for remaining in [0.0, 0.2, 0.5, 0.8, 1.0] {
                let original =
                    super::sample_cubic_timing_curve(start + (1.0 - start) * remaining, curve);
                let resumed = y_start
                    + (1.0 - y_start) * super::sample_cubic_timing_curve(remaining, residual);
                assert!(
                    (original - resumed).abs() < 1e-8,
                    "curve restart drift at start={start} remaining={remaining}: {original} vs {resumed}"
                );
            }
        }

        let started_at = Instant::now();
        assert!(
            (super::result_card_progress(started_at, started_at + Duration::from_millis(75))
                - 0.4085)
                .abs()
                < 0.001
        );
        assert!(
            (super::result_card_progress(started_at, started_at + Duration::from_millis(150))
                - 0.8024)
                .abs()
                < 0.001
        );
        assert!(
            (super::result_card_progress(started_at, started_at + Duration::from_millis(225))
                - 0.9605)
                .abs()
                < 0.001
        );
    }

    #[test]
    fn ui224_ready_and_auto_status_cycles_keep_native_delays() {
        let start = Instant::now();
        let mut ready = StatusTextAlternator::ready(start);
        assert!(ready.first);
        assert_eq!(ready.next_at, start + Duration::from_secs(3));

        assert!(ready.advance(start + Duration::from_secs(3)));
        assert!(!ready.first);
        assert_eq!(ready.next_at, start + Duration::from_secs(5));
        assert_eq!(
            ready.transition_started_at,
            Some(start + Duration::from_secs(3))
        );

        ready.advance(start + Duration::from_secs(5));
        assert!(ready.first);
        assert_eq!(ready.next_at, start + Duration::from_secs(12));
        ready.advance(start + Duration::from_secs(12));
        assert!(!ready.first);
        assert_eq!(ready.next_at, start + Duration::from_secs(14));

        let mut auto = StatusTextAlternator::auto(start);
        assert!(auto.first);
        auto.advance(start + Duration::from_secs(3));
        assert!(!auto.first);
        assert_eq!(auto.next_at, start + Duration::from_secs(6));
        auto.advance(start + Duration::from_secs(6));
        assert!(auto.first);
        assert_eq!(auto.next_at, start + Duration::from_secs(9));
    }

    #[test]
    fn ui200_composition_status_snapshot_keeps_current_and_previous_text_identity() {
        let start = Instant::now();
        let mut auto = StatusTextAlternator::auto(start);
        auto.advance(start + Duration::from_secs(3));
        let listening_auto = MainPhase::Listening {
            mode: CaptureMode::Auto,
            started_at: start,
        };
        assert_eq!(
            super::status_text_snapshot(&listening_auto, Some(auto)),
            super::StatusTextSnapshot {
                current: Some(super::StatusTextLabel::AutoTurnOff),
                previous: Some(super::StatusTextLabel::AutoOn),
                transition_started_at: Some(start + Duration::from_secs(3)),
            }
        );

        let listening_manual = MainPhase::Listening {
            mode: CaptureMode::Manual,
            started_at: start,
        };
        assert_eq!(
            super::status_text_snapshot(&listening_manual, Some(auto)),
            super::StatusTextSnapshot {
                current: Some(super::StatusTextLabel::Listening),
                previous: None,
                transition_started_at: None,
            }
        );

        let no_match = MainPhase::NoMatch {
            entered_at: start,
            returned_to_ready_at: None,
        };
        assert_eq!(
            super::status_text_snapshot(&no_match, Some(auto)),
            super::StatusTextSnapshot::default()
        );
    }

    #[test]
    fn preferred_ui_languages_choose_first_supported_language_and_fallback_to_english() {
        let preferred = "fr-FR\0ja-JP\0en-US\0\0";
        let encoded: Vec<u16> = preferred.encode_utf16().collect();
        assert_eq!(
            select_ui_language_from_multisz(&encoded),
            UiLanguage::Japanese
        );
        assert_eq!(directwrite_locale(UiLanguage::Japanese), "ja-JP");
        let english: Vec<u16> = "de-DE\0en-US\0\0".encode_utf16().collect();
        assert_eq!(
            select_ui_language_from_multisz(&english),
            UiLanguage::English
        );
        assert_eq!(directwrite_locale(UiLanguage::English), "en-US");
        let unsupported: Vec<u16> = "fr-FR\0de-DE\0\0".encode_utf16().collect();
        assert_eq!(
            select_ui_language_from_multisz(&unsupported),
            UiLanguage::English
        );
    }

    #[test]
    fn localized_recognition_copy_uses_expected_english_product_strings() {
        assert_eq!(
            localized_ui_text(UiTextKey::Ready, UiLanguage::English),
            "Click to Shazam"
        );
        assert_eq!(
            localized_ui_text(UiTextKey::AutoStartStatus, UiLanguage::English),
            "Double click to Auto Shazam"
        );
        assert_eq!(
            localized_ui_text(UiTextKey::AutoOn, UiLanguage::English),
            "Auto Shazam is on"
        );
        assert_eq!(
            localized_ui_text(UiTextKey::AutoTurnOff, UiLanguage::English),
            "Click to turn off"
        );
        assert_eq!(no_match_copy(false, UiLanguage::English).0, "No Result");
        assert_eq!(
            no_match_copy(true, UiLanguage::English).1,
            "Please check your connection"
        );
        assert_eq!(timeout_copy(UiLanguage::English).1, "Dismiss");
        assert_eq!(timeout_copy(UiLanguage::English).2, "Restart");
    }

    #[test]
    fn recognition_timeout_states_do_not_share_result_dismiss_timers() {
        let now = Instant::now();
        assert!(phase_needs_timeout(&MainPhase::NoMatch {
            entered_at: now,
            returned_to_ready_at: None,
        }));
        assert!(phase_needs_timeout(&MainPhase::Error {
            message: "error".to_owned(),
            entered_at: now,
        }));
        assert!(!phase_needs_timeout(&MainPhase::Timeout {
            mode: CaptureMode::Manual,
            entered_at: now,
        }));
        assert!(!phase_needs_timeout(&MainPhase::Listening {
            mode: CaptureMode::Manual,
            started_at: now,
        }));
    }

    #[test]
    fn popup_activation_handoff_does_not_close_on_its_initial_focus_loss() {
        let shown_at = Instant::now();
        let grace_until = shown_at + POPUP_ACTIVATION_GRACE;
        let can_close =
            |now| should_hide_popup_on_deactivation(true, true, true, Some(grace_until), now);
        assert!(!can_close(shown_at));
        assert!(!can_close(shown_at + Duration::from_millis(200)));
        assert!(can_close(grace_until));
        assert!(!should_hide_popup_on_deactivation(
            true,
            false,
            true,
            None,
            grace_until + Duration::from_secs(1)
        ));
        assert!(!should_hide_popup_on_deactivation(
            true,
            true,
            false,
            None,
            grace_until + Duration::from_secs(1)
        ));
    }

    #[test]
    fn tray_click_activation_wins_over_the_focus_loss_it_causes() {
        assert!(should_defer_hide_for_tray_activation(Some(true)));
        assert!(should_defer_hide_for_tray_activation(None));
        assert!(!should_defer_hide_for_tray_activation(Some(false)));
    }

    #[test]
    fn empty_history_skeleton_matches_nib_rows_after_coordinate_flip() {
        let nib_bottom_origin_y = [-39.0_f32, 55.0, 149.0, 243.0, 336.0];
        let mut expected_top_origin_y = nib_bottom_origin_y.map(|y| 430.0 - (y + 94.0));
        expected_top_origin_y.sort_by(f32::total_cmp);
        assert_eq!(super::EMPTY_HISTORY_ROW_TOPS, expected_top_origin_y);
    }

    #[test]
    fn auto_candidate_dedupe_is_bounded_and_suppresses_repeated_candidates() {
        let mut recent = VecDeque::new();
        assert!(remember_auto_candidate_key(
            &mut recent,
            "track-key:a".to_owned()
        ));
        assert!(!remember_auto_candidate_key(
            &mut recent,
            "track-key:a".to_owned()
        ));
        assert!(remember_auto_candidate_key(
            &mut recent,
            "track-key:b".to_owned()
        ));

        for index in 0..super::AUTO_CANDIDATE_DEDUPE_LIMIT {
            assert!(remember_auto_candidate_key(
                &mut recent,
                format!("track-key:{index}")
            ));
        }
        assert_eq!(recent.len(), super::AUTO_CANDIDATE_DEDUPE_LIMIT);
        assert!(remember_auto_candidate_key(
            &mut recent,
            "track-key:a".to_owned()
        ));
    }

    #[test]
    fn startup_task_activation_keeps_the_main_popup_hidden() {
        assert!(should_show_initial_popup(false));
        assert!(!should_show_initial_popup(true));
    }

    #[test]
    fn no_match_icon_path_preserves_native_command_topology_and_unclamped_controls() {
        let moves = NO_MATCH_ICON_PATH
            .iter()
            .filter(|command| matches!(command, NoMatchIconCommand::Move(_)))
            .count();
        let curves = NO_MATCH_ICON_PATH
            .iter()
            .filter(|command| matches!(command, NoMatchIconCommand::Cubic(_, _, _)))
            .count();
        let lines = NO_MATCH_ICON_PATH
            .iter()
            .filter(|command| matches!(command, NoMatchIconCommand::Line(_)))
            .count();
        let closes = NO_MATCH_ICON_PATH
            .iter()
            .filter(|command| matches!(command, NoMatchIconCommand::Close))
            .count();
        assert_eq!((moves, curves, lines, closes), (2, 15, 15, 2));

        assert_eq!(
            NO_MATCH_ICON_PATH[8], NO_MATCH_ICON_PATH[9],
            "the exact repeated UI-168 line must remain in the vector path"
        );
        assert!(NO_MATCH_ICON_PATH.iter().any(|command| matches!(
            command,
            NoMatchIconCommand::Cubic((_, cp1_y), (_, _), _)
                if *cp1_y < 0.0
        )));
        assert!(NO_MATCH_ICON_PATH.iter().any(|command| matches!(
            command,
            NoMatchIconCommand::Cubic((_, _), (cp2_x, _), _)
                if *cp2_x > 1.0
        )));
    }

    #[test]
    fn no_match_icon_uses_the_native_portrait_mask_rect() {
        let [left, top, right, bottom] = no_match_icon_rect(96.0);
        assert!((left - 32.0).abs() < 1e-5);
        assert!((top - 1.2).abs() < 1e-5);
        assert!((right - 64.0).abs() < 1e-5);
        assert!((bottom - 94.8).abs() < 1e-4);
        assert!((right - left - 32.0).abs() < 1e-5);
        assert!((bottom - top - 93.6).abs() < 1e-4);
    }

    #[test]
    fn notification_text_is_nul_terminated_without_splitting_surrogate_pairs() {
        let mut buffer = [0xFFFF; 5];
        copy_notification_text(&mut buffer, "A😀B");
        assert_eq!(buffer, [b'A' as u16, 0xD83D, 0xDE00, b'B' as u16, 0]);

        let mut truncated = [0; 3];
        copy_notification_text(&mut truncated, "A😀B");
        assert_eq!(truncated, [b'A' as u16, 0, 0]);
    }

    #[test]
    fn premultiplied_d2d_popup_union_keeps_antialiased_alpha_edges() {
        use super::*;

        unsafe {
            let surface = LayeredSurface::new(32, 32).unwrap();
            let factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
            let properties = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: Default::default(),
            };
            let target = factory.CreateDCRenderTarget(&properties).unwrap();
            let area = RECT {
                left: 0,
                top: 0,
                right: 32,
                bottom: 32,
            };
            target.BindDC(surface.dc, &area).unwrap();
            target.BeginDraw();
            target.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));
            let brush = target
                .CreateSolidColorBrush(
                    &D2D1_COLOR_F {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: 1.0,
                    },
                    None,
                )
                .unwrap();
            let rounded = factory
                .CreateRoundedRectangleGeometry(
                    &windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: 0.0,
                            top: 6.0,
                            right: 32.0,
                            bottom: 32.0,
                        },
                        radiusX: 8.0,
                        radiusY: 8.0,
                    },
                )
                .unwrap();
            let arrow = factory.CreatePathGeometry().unwrap();
            let arrow_sink = arrow.Open().unwrap();
            arrow_sink.BeginFigure(Vector2 { X: 12.0, Y: 6.0 }, D2D1_FIGURE_BEGIN_FILLED);
            arrow_sink.AddLine(Vector2 { X: 16.0, Y: 0.0 });
            arrow_sink.AddLine(Vector2 { X: 20.0, Y: 6.0 });
            arrow_sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            arrow_sink.Close().unwrap();
            let popup = factory.CreatePathGeometry().unwrap();
            let popup_sink = popup.Open().unwrap();
            rounded
                .CombineWithGeometry(&arrow, D2D1_COMBINE_MODE_UNION, None, 0.25, &popup_sink)
                .unwrap();
            popup_sink.Close().unwrap();
            let layer = target.CreateLayer(None).unwrap();
            let layer_parameters = windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS {
                contentBounds: D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: 32.0,
                    bottom: 32.0,
                },
                geometricMask: std::mem::ManuallyDrop::new(Some(popup.into())),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: windows_numerics::Matrix3x2 {
                    M11: 1.0,
                    M22: 1.0,
                    ..Default::default()
                },
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS_NONE,
            };
            target.PushLayer(&layer_parameters, &layer);
            target.FillRectangle(
                &D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: 32.0,
                    bottom: 32.0,
                },
                &brush,
            );
            target.PopLayer();
            target.EndDraw(None, None).unwrap();

            let pixels = surface._pixels.cast::<u8>();
            let alpha_values = (0..32usize * 32)
                .map(|pixel| *pixels.add(pixel * 4 + 3))
                .collect::<Vec<_>>();
            assert!(
                alpha_values.contains(&0),
                "transparent corner pixels are retained"
            );
            assert!(
                alpha_values.iter().any(|alpha| (1..255).contains(alpha)),
                "rounded contour retains fractional coverage for smooth composition"
            );
        }
    }

    #[test]
    fn native_button_logo_uses_specified_geometry_scale_and_stroke() {
        use super::*;

        let layout = shz_logo_layout(96.0, 96.0);
        assert!((layout.scale_x - (96.0 / 367.5)).abs() < f32::EPSILON);
        assert_eq!(layout.scale_x, layout.scale_y);
        assert_eq!((layout.offset_x, layout.offset_y), (-93.0, -93.0));
        assert_eq!(
            (
                layout.center_x - layout.offset_x,
                layout.center_y - layout.offset_y
            ),
            (141.0, 141.0)
        );
        assert!((SHZ_LOGO_STROKE_WIDTH * layout.scale_x - 9.926_531).abs() < 0.000_01);
        let top_start = shz_logo_point_in_button(layout, SHZ_LOGO_TOP_PATH.start);
        assert!((top_start.0 - 40.474_12).abs() < 0.000_02);
        assert!((top_start.1 - 23.282_94).abs() < 0.000_02);
        let bottom_start = shz_logo_point_in_button(layout, SHZ_LOGO_BOTTOM_PATH.start);
        assert!((bottom_start.0 - 55.528_49).abs() < 0.000_02);
        assert!((bottom_start.1 - 73.082_78).abs() < 0.000_02);
        assert_eq!(SHZ_LOGO_SOURCE_SIZE, 210.0);
        assert_eq!(SHZ_LOGO_CANVAS_PADDING, 36.0);
        assert_eq!(SHZ_LOGO_CANVAS_SIZE, 282.0);
        let long_path_y_min = -13.49;
        let long_path_y_max = 224.57;
        let cap_radius = SHZ_LOGO_STROKE_WIDTH * 0.5;
        assert!(long_path_y_min + SHZ_LOGO_CANVAS_PADDING - cap_radius > 0.0);
        assert!(long_path_y_max + SHZ_LOGO_CANVAS_PADDING + cap_radius < SHZ_LOGO_CANVAS_SIZE);
        assert_eq!(SHZ_LOGO_TOP_PATH.curves.len(), 4);
        assert_eq!(SHZ_LOGO_BOTTOM_PATH.curves.len(), 4);
        assert_eq!(SHZ_LOGO_TOP_PATH.start, (76.19, 10.38));
        assert_eq!(SHZ_LOGO_BOTTOM_PATH.start, (133.82, 201.02));
    }

    #[test]
    fn composition_and_direct2d_share_logical_button_coordinates_at_each_dpi() {
        use super::*;

        for dpi in [96, 120, 144, 168, 192, 240, 288] {
            let scale = composition_scale_for_dpi(dpi);
            let d2d_center_x = ui_model::physical_pixels(BUTTON_X + BUTTON_SIZE * 0.5, dpi);
            let composition_center_x =
                ((BUTTON_X as f32 + BUTTON_SIZE as f32 * 0.5) * scale).round() as i32;
            let button_center_y = POPUP_ARROW_HEIGHT + BUTTON_TOP + BUTTON_SIZE * 0.5;
            let d2d_center_y = ui_model::physical_pixels(button_center_y, dpi);
            let composition_center_y = (button_center_y as f32 * scale).round() as i32;
            assert_eq!(composition_center_x, d2d_center_x, "{dpi} DPI x");
            assert_eq!(composition_center_y, d2d_center_y, "{dpi} DPI y");
            assert!((scale - ui_model::dpi_scale(dpi) as f32).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn composition_pixel_phase_aligns_button_and_ripples_at_fractional_dpi() {
        for dpi in [96, 120, 144, 168, 192, 240, 288] {
            let scale = composition_scale_for_dpi(dpi);
            for body_top in [0.0, POPUP_ARROW_HEIGHT as f32] {
                let offset = composition_button_pixel_snap_offset(dpi, body_top);
                let logical_x = (BUTTON_X + BUTTON_SIZE * 0.5) as f32;
                let logical_y = body_top + (BUTTON_TOP + BUTTON_SIZE * 0.5) as f32;
                let physical_x = (logical_x + offset.X) * scale;
                let physical_y = (logical_y + offset.Y) * scale;
                assert!(
                    (physical_x - physical_x.round()).abs() < 1e-5,
                    "{dpi} DPI x"
                );
                assert!(
                    (physical_y - physical_y.round()).abs() < 1e-5,
                    "{dpi} DPI y"
                );
                assert!(offset.X.abs() * scale <= 0.5 + 1e-5, "{dpi} DPI x offset");
                assert!(offset.Y.abs() * scale <= 0.5 + 1e-5, "{dpi} DPI y offset");
            }
        }
    }

    #[test]
    fn button_arc_preserves_native_degree_value_and_geometry() {
        let [start, _, _, end] = button_border_arc_points();
        assert_eq!(start, (97.0, 50.0));
        assert!((end.0 - 96.926_4).abs() < 0.001);
        assert!((end.1 - 52.685_3).abs() < 0.001);
        assert!(end.1 > start.1);
    }

    #[test]
    fn native_icon_visual_frames_and_hit_targets_are_independent() {
        // Check canonical TopRegion-local values independently of popup edge.
        let layout = native_icon_layout(0.0, ArrowEdge::Top);
        assert_eq!(layout.settings_visual, [349.0, 25.0, 365.0, 41.0]);
        assert_eq!(layout.settings_hit, layout.settings_visual);
        assert_eq!(layout.cancel_visual, [351.0, 27.0, 363.0, 39.0]);
        assert_eq!(layout.cancel_hit, layout.cancel_visual);
        assert_eq!(
            (
                (layout.settings_visual[0] + layout.settings_visual[2]) * 0.5,
                (layout.settings_visual[1] + layout.settings_visual[3]) * 0.5,
            ),
            (357.0, 33.0)
        );
        assert_eq!(
            (
                (layout.cancel_visual[0] + layout.cancel_visual[2]) * 0.5,
                (layout.cancel_visual[1] + layout.cancel_visual[3]) * 0.5,
            ),
            (357.0, 33.0)
        );
        assert_eq!(layout.result_close_visual, [353.0, 25.0, 365.0, 37.0]);
        assert_eq!(layout.result_close_hit, layout.result_close_visual);
        assert_eq!(
            (
                (layout.result_close_visual[0] + layout.result_close_visual[2]) * 0.5,
                (layout.result_close_visual[1] + layout.result_close_visual[3]) * 0.5,
            ),
            (359.0, 31.0)
        );
        let bottom_anchored = native_icon_layout(0.0, ArrowEdge::Bottom);
        let top_anchored = native_icon_layout(POPUP_ARROW_HEIGHT as f32, ArrowEdge::Top);
        assert_eq!(
            top_anchored.settings_visual[1] - bottom_anchored.settings_visual[1],
            POPUP_ARROW_HEIGHT as f32 * 2.0
        );
        assert_eq!(
            top_anchored.cancel_hit[1] - bottom_anchored.cancel_hit[1],
            POPUP_ARROW_HEIGHT as f32 * 2.0
        );
        assert!(point_in_rect(349.0, 25.0, layout.settings_hit));
        assert!(!point_in_rect(348.9, 26.0, layout.settings_hit));
        assert!(point_in_rect(351.0, 27.0, layout.cancel_hit));
        assert!(!point_in_rect(350.9, 28.0, layout.cancel_hit));
        assert!(point_in_rect(353.0, 25.0, layout.result_close_hit));
        assert!(!point_in_rect(352.9, 25.0, layout.result_close_hit));
        assert!(!point_in_rect(349.0, 25.0, layout.result_close_hit));
    }

    #[test]
    fn ui232_bottom_arrow_moves_header_icons_and_hit_targets_up_ten_dip() {
        let layout = native_icon_layout(0.0, ArrowEdge::Bottom);
        assert_eq!(layout.settings_visual, [349.0, 15.0, 365.0, 31.0]);
        assert_eq!(layout.settings_hit, layout.settings_visual);
        assert_eq!(layout.result_close_visual, [353.0, 15.0, 365.0, 27.0]);
        assert_eq!(layout.result_close_hit, layout.result_close_visual);
        assert_eq!(layout.cancel_visual, [351.0, 17.0, 363.0, 29.0]);
        assert_eq!(layout.cancel_hit, layout.cancel_visual);
        assert!(point_in_rect(357.0, 15.0, layout.settings_hit));
        assert!(point_in_rect(359.0, 21.0, layout.result_close_hit));
        assert!(point_in_rect(357.0, 23.0, layout.cancel_hit));
        assert!(!point_in_rect(357.0, 14.9, layout.settings_hit));
    }

    #[test]
    fn os_foreground_color_uses_the_documented_windows_theme_threshold() {
        assert_eq!(theme_from_foreground_rgb(0, 0, 0), Theme::Light);
        assert_eq!(theme_from_foreground_rgb(255, 255, 255), Theme::Dark);
        assert_eq!(theme_from_foreground_rgb(128, 128, 128), Theme::Light);
        assert_eq!(theme_from_foreground_rgb(0, 255, 0), Theme::Dark);
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
    fn ui223_history_title_uses_theme_specific_ink_with_documented_alpha() {
        let light = history_title_color(Theme::Light);
        assert_eq!((light.r, light.g, light.b), (0.0, 0.0, 0.0));
        assert!((light.a - 0.847).abs() < f32::EPSILON);

        let dark = history_title_color(Theme::Dark);
        assert_eq!((dark.r, dark.g, dark.b), (1.0, 1.0, 1.0));
        assert!((dark.a - 0.847).abs() < f32::EPSILON);
    }

    #[test]
    fn ui222_result_card_link_excludes_close_and_visible_store_action() {
        let top = 42.0;
        let close = [353.0, top + 25.0, 365.0, top + 37.0];
        let action = result_details_rects(top, 24.0, 18.0, true).action;
        let action_bounds = [action.left, action.top, action.right, action.bottom];

        assert!(result_card_link_hit(
            30.0,
            top + 80.0,
            top,
            close,
            Some(action_bounds)
        ));
        assert!(!result_card_link_hit(
            359.0,
            top + 31.0,
            top,
            close,
            Some(action_bounds)
        ));
        assert!(!result_card_link_hit(
            (action.left + action.right) * 0.5,
            (action.top + action.bottom) * 0.5,
            top,
            close,
            Some(action_bounds),
        ));
        assert!(!result_card_link_hit(
            30.0,
            top - 0.1,
            top,
            close,
            Some(action_bounds)
        ));
        assert!(!result_card_link_hit(
            30.0,
            top + TOP_HEIGHT as f32 + 0.1,
            top,
            close,
            Some(action_bounds),
        ));
    }

    #[test]
    fn ui233_fixed_store_cta_is_visible_but_separate_from_shazam_page_hit() {
        let top = 42.0;
        let close = [353.0, top + 25.0, 365.0, top + 37.0];
        let action = result_details_rects(top, 24.0, 18.0, true).action;
        assert_eq!(action.left, RESULT_DETAILS_LEFT);
        assert_eq!(action.right, RESULT_DETAILS_RIGHT);
        assert_eq!(action.bottom - action.top, RESULT_ACTION_HEIGHT);
        let action_center = (
            (action.left + action.right) * 0.5,
            (action.top + action.bottom) * 0.5,
        );

        assert!(!result_card_link_hit(
            action_center.0,
            action_center.1,
            top,
            close,
            Some([action.left, action.top, action.right, action.bottom]),
        ));
        assert!(result_card_link_hit(
            30.0,
            top + 80.0,
            top,
            close,
            Some([action.left, action.top, action.right, action.bottom]),
        ));
        assert_eq!(
            localized_ui_text(UiTextKey::AppleMusicAction, UiLanguage::Japanese),
            "Apple Musicで聴く"
        );
        assert_eq!(
            localized_ui_text(UiTextKey::AppleMusicAction, UiLanguage::English),
            "Listen on Apple Music"
        );
    }

    #[test]
    fn ui222_history_link_hit_respects_viewport_clipping() {
        let top = 200.0;
        let bottom = top + HISTORY_HEIGHT as f32;
        assert!(visible_history_row_hit(top + 1.0, top, top, bottom));
        assert!(!visible_history_row_hit(top - 1.0, top - 20.0, top, bottom));
        assert!(!visible_history_row_hit(
            bottom + 1.0,
            bottom - 10.0,
            top,
            bottom
        ));
        assert!(visible_history_row_hit(
            bottom - 1.0,
            bottom - 80.0,
            top,
            bottom
        ));
    }

    #[test]
    fn ui209_artwork_urls_reject_unsafe_authorities_and_private_ip_literals() {
        assert!(super::valid_https_resource_url(
            "https://images.example.test/cover.webp?size=300"
        ));
        assert!(super::valid_https_resource_url(
            "https://[2001:4860:4860::8888]/cover.webp"
        ));
        assert!(!super::valid_https_resource_url(
            "http://images.example.test/cover.jpg"
        ));
        assert!(!super::valid_https_resource_url(
            "https://images.example.test:8443/cover.jpg"
        ));
        assert!(!super::valid_https_resource_url(
            "https://user@images.example.test/cover.jpg"
        ));
        assert!(!super::valid_https_resource_url(
            "https://images.example.test/cover.jpg#frag"
        ));
        assert!(!super::valid_https_resource_url(
            "https://127.0.0.1/cover.jpg"
        ));
        assert!(!super::valid_https_resource_url("https://127.1/cover.jpg"));
        assert!(!super::valid_https_resource_url("https://[::1]/cover.jpg"));
    }

    #[test]
    fn ui209_artwork_address_policy_rejects_private_reserved_and_tunnel_ranges() {
        for value in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.0.1",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2001:2::1",
            "2001:10::1",
            "2002::1",
            "3fff::1",
        ] {
            let address = value
                .parse()
                .unwrap_or_else(|_| panic!("invalid test IP {value}"));
            assert!(
                !super::is_public_artwork_address(address),
                "{value} must not be a public artwork destination"
            );
        }
        for value in ["1.1.1.1", "8.8.8.8", "2001:4860:4860::8888"] {
            let address = value
                .parse()
                .unwrap_or_else(|_| panic!("invalid test IP {value}"));
            assert!(
                super::is_public_artwork_address(address),
                "{value} should remain available for public artwork"
            );
        }
    }

    #[test]
    fn ui209_artwork_host_resolution_is_pinned_and_mime_allowlist_is_narrow() {
        let addresses = [
            "8.8.8.8".parse().unwrap(),
            "2001:4860:4860::8888".parse().unwrap(),
        ];
        assert_eq!(
            super::curl_resolve_entry("images.example.test", 443, &addresses),
            Some("images.example.test:443:8.8.8.8,[2001:4860:4860::8888]".to_owned())
        );
        assert!(super::curl_resolve_entry("8.8.8.8", 443, &addresses).is_none());
        assert!(super::supported_artwork_content_type("image/jpeg"));
        assert!(super::supported_artwork_content_type(
            "IMAGE/PNG; charset=binary"
        ));
        assert!(super::supported_artwork_content_type("image/webp"));
        assert!(!super::supported_artwork_content_type("text/html"));
        assert!(!super::supported_artwork_content_type("image/svg+xml"));
    }

    #[test]
    fn ui209_curl_artwork_output_is_bounded_and_extracts_response_metadata() {
        let output = b"image-bytes\nOPEN SHAZAM IMAGE RESPONSE:302\nhttps://cdn.example.test/cover.png\nimage/png\n8.8.8.8";
        let response = super::parse_curl_artwork_output(output).unwrap();
        assert_eq!(response.status, 302);
        assert_eq!(response.redirect_url, "https://cdn.example.test/cover.png");
        assert_eq!(response.content_type, "image/png");
        assert_eq!(
            response.remote_ip,
            "8.8.8.8".parse::<std::net::IpAddr>().unwrap()
        );
        assert_eq!(response.body, b"image-bytes");

        let oversized =
            vec![b'x'; super::MAX_ARTWORK_BYTES + super::MAX_ARTWORK_METADATA_BYTES + 1];
        assert!(super::parse_curl_artwork_output(&oversized).is_err());
        let oversized_body = format!(
            "{}\nOPEN SHAZAM IMAGE RESPONSE:200\n\nimage/jpeg\n8.8.8.8",
            "x".repeat(super::MAX_ARTWORK_BYTES + 1)
        );
        assert!(super::parse_curl_artwork_output(oversized_body.as_bytes()).is_err());
    }

    #[test]
    fn ui222_page_template_requires_one_track_key_token() {
        assert!(valid_page_template(
            "https://www.shazam.com/track/{trackid}"
        ));
        let referrer_template = "https://www.shazam.com/track/{trackid}#referrer=shazamapp";
        assert!(valid_page_template(referrer_template));
        assert_eq!(
            resolve_page_link_checked(Some(referrer_template), Some("abc123")),
            Ok(ValidatedWebTrackUrl(
                "https://www.shazam.com/track/abc123#referrer=shazamapp".to_owned()
            ))
        );
        assert!(!valid_page_template(
            "https://www.shazam.com/track/{trackid}/{trackid}"
        ));
        assert!(resolve_page_link(
            Some("https://www.shazam.com/track/{trackid}"),
            Some("abc/123")
        )
        .is_some_and(|url| url.0 == "https://www.shazam.com/track/abc%2F123"));
        assert!(resolve_page_link(Some("https://www.shazam.com/track/{trackid}"), None).is_none());
    }

    #[test]
    fn ui222_page_link_failure_identifies_missing_or_invalid_inputs_without_logging_values() {
        let configs = std::collections::HashMap::from([(
            10,
            UiConfigSnapshot {
                generation: 10,
                web_track_url_template: None,
            },
        )]);

        assert_eq!(
            resolve_page_link_for_generation(None, &configs, Some("key-1")),
            Err(PageLinkFailure::MissingConfigGeneration)
        );
        assert_eq!(
            resolve_page_link_for_generation(Some(11), &configs, Some("key-1")),
            Err(PageLinkFailure::MissingConfigSnapshot)
        );
        assert_eq!(
            resolve_page_link_for_generation(Some(10), &configs, Some("key-1")),
            Err(PageLinkFailure::MissingTemplate)
        );
        assert_eq!(
            resolve_page_link_checked(
                Some("https://www.shazam.com/track/{trackid}/{trackid}"),
                Some("key-1")
            ),
            Err(PageLinkFailure::InvalidTemplate)
        );
        assert_eq!(
            resolve_page_link_checked(Some("https://www.shazam.com/track/{trackid}"), None),
            Err(PageLinkFailure::MissingTrackKey)
        );
        assert_eq!(
            resolve_page_link_checked(Some("https://www.shazam.com/track/{trackid}"), Some("")),
            Err(PageLinkFailure::InvalidTrackKey)
        );
        assert_eq!(
            PageLinkFailure::MissingTemplate.as_str(),
            "missing_webtrackurl"
        );
    }

    #[test]
    fn ui222_result_and_history_page_lookup_uses_the_clicked_track_identity() {
        let recognized_at = Instant::now();
        let make_track = |display_id, url: &str| Track {
            display_id,
            title: format!("track-{display_id}"),
            artist: "artist".to_owned(),
            recognized_at,
            recognized_at_utc: UNIX_EPOCH,
            recognized_date_label: String::new(),
            timestamp_source: HistoryTimestampSource::HostReceivedFallback,
            artwork_url: None,
            hq_artwork_url: None,
            background_artwork_url: None,
            track_key: None,
            config_generation: None,
            page_url: Some(ValidatedWebTrackUrl(url.to_owned())),
        };
        let history_track = make_track(41, "https://www.shazam.com/track/history-41");
        let result_track = make_track(42, "https://www.shazam.com/track/result-42");
        let history = [history_track];

        let configs = std::collections::HashMap::new();
        assert_eq!(
            page_url_for_display_id(&history, Some(&result_track), 41, &configs).map(|url| url.0),
            Some("https://www.shazam.com/track/history-41".to_owned())
        );
        assert_eq!(
            page_url_for_display_id(&history, Some(&result_track), 42, &configs).map(|url| url.0),
            Some("https://www.shazam.com/track/result-42".to_owned())
        );
        assert!(page_url_for_display_id(&history, Some(&result_track), 99, &configs).is_none());
    }

    #[test]
    fn ui222_clicked_track_resolves_page_url_from_its_config_generation() {
        let recognized_at = Instant::now();
        let track = Track {
            display_id: 51,
            title: "track-51".to_owned(),
            artist: "artist".to_owned(),
            recognized_at,
            recognized_at_utc: UNIX_EPOCH,
            recognized_date_label: String::new(),
            timestamp_source: HistoryTimestampSource::HostReceivedFallback,
            artwork_url: None,
            hq_artwork_url: None,
            background_artwork_url: None,
            track_key: Some("key-51".to_owned()),
            config_generation: Some(91),
            page_url: None,
        };
        let configs = std::collections::HashMap::from([(
            91,
            UiConfigSnapshot {
                generation: 91,
                web_track_url_template: Some("https://www.shazam.com/track/{trackid}".to_owned()),
            },
        )]);

        assert_eq!(
            page_url_for_display_id(&[], Some(&track), 51, &configs).map(|url| url.0),
            Some("https://www.shazam.com/track/key-51".to_owned())
        );
    }

    #[test]
    fn artwork_bitmap_cache_replaces_changed_pixels_without_downgrading_quality() {
        let first = artwork_pixel_fingerprint(1, 1, &[1, 2, 3, 4]);
        let refreshed = artwork_pixel_fingerprint(1, 1, &[5, 6, 7, 8]);
        assert_ne!(first, refreshed);
        assert!(should_replace_cached_artwork(
            None,
            ArtworkSource::Standard,
            first
        ));
        assert!(!should_replace_cached_artwork(
            Some((ArtworkSource::Standard, first)),
            ArtworkSource::Standard,
            first,
        ));
        assert!(should_replace_cached_artwork(
            Some((ArtworkSource::Standard, first)),
            ArtworkSource::Standard,
            refreshed,
        ));
        assert!(should_replace_cached_artwork(
            Some((ArtworkSource::Standard, first)),
            ArtworkSource::HighQuality,
            first,
        ));
        assert!(!should_replace_cached_artwork(
            Some((ArtworkSource::HighQuality, refreshed)),
            ArtworkSource::Standard,
            first,
        ));
    }

    #[test]
    fn notification_artwork_uses_bilinear_center_crop_and_rejects_bad_buffers() {
        let mut bgra = Vec::new();
        for pixel in [
            [0, 0, 0, 255],
            [10, 20, 30, 255],
            [30, 40, 50, 255],
            [100, 100, 100, 255],
            [200, 200, 200, 255],
            [30, 40, 50, 255],
            [50, 60, 70, 255],
            [200, 200, 200, 255],
        ] {
            bgra.extend_from_slice(&pixel);
        }
        let pixels = ArtworkPixels {
            width: 4,
            height: 2,
            fingerprint: 0,
            bgra,
        };

        assert_eq!(
            scale_notification_artwork_to_size(&pixels, 1, 1),
            Some(vec![30, 40, 50, 255])
        );
        assert_eq!(
            scale_notification_artwork_to_size(&pixels, 1, 2),
            Some(vec![20, 30, 40, 255, 40, 50, 60, 255])
        );
        assert_eq!(scale_notification_artwork_to_size(&pixels, 0, 0), None);
        let mut malformed = pixels;
        malformed.bgra.pop();
        assert_eq!(scale_notification_artwork_to_size(&malformed, 1, 1), None);
    }

    #[test]
    fn custom_notification_artwork_creates_native_icons_at_supported_dpi_values() {
        let pixels = ArtworkPixels {
            width: 1,
            height: 1,
            fingerprint: 0,
            bgra: vec![40, 80, 120, 255],
        };
        unsafe {
            for dpi in [96, 120, 144, 168, 192, 240, 288] {
                let icon = super::create_notification_icon(&pixels, dpi)
                    .unwrap_or_else(|error| panic!("failed to create icon at {dpi} DPI: {error}"));
                assert!(
                    !icon.0.is_null(),
                    "icon handle at {dpi} DPI must be non-null"
                );
                let _ = super::DestroyIcon(icon);
            }
        }
    }

    #[test]
    fn ui240_artwork_reuse_rejects_stale_request_generation_and_url_binding() {
        let track = TrackIdentity { display_id: 41 };
        let old = ArtworkRequest {
            identity: ArtworkRequestIdentity {
                track,
                generation: 1,
                request_id: 10,
            },
            standard_url: Some("https://cdn.example.test/a.jpg".to_owned()),
            high_quality_url: None,
        };
        let current = ArtworkRequest {
            identity: ArtworkRequestIdentity {
                track,
                generation: 2,
                request_id: 11,
            },
            standard_url: Some("https://cdn.example.test/b.jpg".to_owned()),
            high_quality_url: Some("https://cdn.example.test/b@2x.jpg".to_owned()),
        };

        assert!(artwork_request_is_current(Some(&current), current.identity));
        assert!(!artwork_request_is_current(Some(&current), old.identity));
        assert!(!artwork_request_is_current(
            Some(&current),
            ArtworkRequestIdentity {
                track,
                generation: current.identity.generation,
                request_id: old.identity.request_id,
            },
        ));
        assert!(!artwork_request_is_current(
            Some(&current),
            ArtworkRequestIdentity {
                track,
                generation: old.identity.generation,
                request_id: current.identity.request_id,
            },
        ));
        assert!(artwork_request_urls_match_track(
            current.standard_url.as_deref(),
            current.high_quality_url.as_deref(),
            Some("https://cdn.example.test/b.jpg"),
            Some("https://cdn.example.test/b@2x.jpg"),
        ));
        assert!(!artwork_request_urls_match_track(
            old.standard_url.as_deref(),
            old.high_quality_url.as_deref(),
            Some("https://cdn.example.test/b.jpg"),
            Some("https://cdn.example.test/b@2x.jpg"),
        ));

        let make_track = |standard: Option<&str>, high_quality: Option<&str>| Track {
            display_id: track.display_id,
            title: "Title".to_owned(),
            artist: "Artist".to_owned(),
            recognized_at: Instant::now(),
            recognized_at_utc: SystemTime::now(),
            recognized_date_label: String::new(),
            timestamp_source: HistoryTimestampSource::ControllerEvent,
            artwork_url: standard.map(str::to_owned),
            hq_artwork_url: high_quality.map(str::to_owned),
            background_artwork_url: None,
            track_key: None,
            config_generation: None,
            page_url: None,
        };
        let track_a = make_track(Some("https://cdn.example.test/a.jpg"), None);
        let track_b = make_track(
            Some("https://cdn.example.test/b.jpg"),
            Some("https://cdn.example.test/b@2x.jpg"),
        );
        let track_c = make_track(
            Some("https://cdn.example.test/c.jpg"),
            Some("https://cdn.example.test/c@2x.jpg"),
        );
        let request_c = ArtworkRequest {
            identity: ArtworkRequestIdentity {
                track,
                generation: 3,
                request_id: 12,
            },
            standard_url: track_c.artwork_url.clone(),
            high_quality_url: track_c.hq_artwork_url.clone(),
        };
        let mut active = HashMap::from([(track, request_c.clone())]);

        // Simulate A→B→C rebinding before any worker result reaches the UI.
        // A/B successes and their completion events must not replace or clear C.
        assert!(!artwork_request_matches_track(
            active.get(&track),
            old.identity,
            Some(&track_a)
        ));
        assert!(!artwork_request_matches_track(
            active.get(&track),
            current.identity,
            Some(&track_b)
        ));
        assert!(artwork_request_matches_track(
            active.get(&track),
            request_c.identity,
            Some(&track_c)
        ));
        assert!(!artwork_request_matches_track(
            active.get(&track),
            request_c.identity,
            None
        ));
        assert!(!artwork_request_matches_track(
            active.get(&track),
            request_c.identity,
            Some(&make_track(None, None)),
        ));

        assert!(!finish_active_artwork_request(&mut active, old.identity));
        assert!(!finish_active_artwork_request(
            &mut active,
            current.identity
        ));
        assert_eq!(active.get(&track), Some(&request_c));
        assert!(finish_active_artwork_request(
            &mut active,
            request_c.identity
        ));
        assert!(active.is_empty());
        assert!(!finish_active_artwork_request(
            &mut active,
            request_c.identity
        ));
    }

    #[test]
    fn result_label_color_preserves_system_foreground_rgba() {
        let mapped = d2d_color_from_system(super::Color {
            A: 128,
            R: 36,
            G: 72,
            B: 144,
        });
        assert!((mapped.r - 36.0 / 255.0).abs() < 1e-6);
        assert!((mapped.g - 72.0 / 255.0).abs() < 1e-6);
        assert!((mapped.b - 144.0 / 255.0).abs() < 1e-6);
        assert!((mapped.a - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn native_header_icons_embed_the_high_resolution_catalog_renditions() {
        assert_eq!(SETTINGS_ICON_PIXELS.len(), 32 * 32 * 4);
        assert_eq!(WHITE_CLOSE_ICON_PIXELS.len(), 24 * 24 * 4);
        assert_eq!(CLOSE_BUTTON_GA_PIXELS.len(), 24 * 24 * 2);
        assert_eq!(RESULT_CLOSE_ICON_PIXELS.len(), 24 * 24 * 4);
        let close_button_fingerprint = CLOSE_BUTTON_GA_PIXELS
            .iter()
            .fold(0xcbf29ce484222325u64, |hash, &byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
            });
        assert_eq!(close_button_fingerprint, 0x40e3261b8f1b5655);
        // The catalog packs these images in a bottom-origin atlas. Preserve
        // their recognizable silhouettes so another valid but wrong crop
        // (for example a social icon) cannot pass as the settings/close art.
        assert_eq!(SETTINGS_ICON_PIXELS[(16 * 32 + 16) * 4 + 3], 0);
        assert!(WHITE_CLOSE_ICON_PIXELS[(12 * 24 + 12) * 4 + 3] > 0);
        assert!(RESULT_CLOSE_ICON_PIXELS[(12 * 24 + 12) * 4 + 3] > 0);
        assert_eq!(RESULT_CLOSE_ICON_PIXELS[3], 0);
    }

    #[test]
    fn result_panel_position_is_reproducible_at_entry_and_exit_times() {
        let entered_at = std::time::Instant::now();
        let returned_at = entered_at + std::time::Duration::from_secs(1);
        let phase = MainPhase::Matched {
            track: super::Track {
                display_id: 1,
                title: String::new(),
                artist: String::new(),
                recognized_at: entered_at,
                recognized_at_utc: UNIX_EPOCH,
                recognized_date_label: String::new(),
                timestamp_source: super::HistoryTimestampSource::HostReceivedFallback,
                artwork_url: None,
                hq_artwork_url: None,
                background_artwork_url: None,
                track_key: None,
                config_generation: None,
                page_url: None,
            },
            stage: ResultCardStage::Entering {
                started_at: entered_at,
            },
        };
        let body_top = 42.0;

        assert_eq!(
            result_panel_top_at(&phase, body_top, entered_at),
            Some(body_top - super::TOP_HEIGHT as f32)
        );
        assert_eq!(
            result_panel_top_at(
                &phase,
                body_top,
                entered_at + std::time::Duration::from_millis(300)
            ),
            Some(body_top)
        );
        let exiting = MainPhase::Matched {
            track: super::Track {
                display_id: 1,
                title: String::new(),
                artist: String::new(),
                recognized_at: entered_at,
                recognized_at_utc: UNIX_EPOCH,
                recognized_date_label: String::new(),
                timestamp_source: super::HistoryTimestampSource::HostReceivedFallback,
                artwork_url: None,
                hq_artwork_url: None,
                background_artwork_url: None,
                track_key: None,
                config_generation: None,
                page_url: None,
            },
            stage: ResultCardStage::Exiting {
                started_at: returned_at,
                from_offset: 0.0,
            },
        };
        assert_eq!(
            result_panel_top_at(&exiting, body_top, returned_at),
            Some(body_top)
        );
        assert_eq!(
            result_panel_top_at(
                &exiting,
                body_top,
                returned_at + std::time::Duration::from_millis(300)
            ),
            Some(body_top + super::TOP_HEIGHT as f32)
        );
    }

    #[test]
    fn ui236_no_match_fallback_exit_freezes_mid_entry_position_like_composition() {
        let entered_at = Instant::now();
        let returned_at = entered_at + Duration::from_millis(75);
        let body_top = 42.0;
        let phases = [
            MainPhase::NoMatch {
                entered_at,
                returned_to_ready_at: Some(returned_at),
            },
            MainPhase::Offline {
                entered_at,
                returned_to_ready_at: Some(returned_at),
            },
        ];
        let ingress = super::result_card_progress(entered_at, returned_at);
        let from_offset = -(1.0 - ingress) * super::TOP_HEIGHT as f32;

        for phase in phases {
            let at_close = result_panel_top_at(&phase, body_top, returned_at).unwrap();
            assert!((at_close - (body_top + from_offset)).abs() < 1e-4);

            for (elapsed_ms, normalized) in [(75, 0.4085), (150, 0.8024), (225, 0.9605)] {
                let now = returned_at + Duration::from_millis(elapsed_ms);
                let expected =
                    body_top + from_offset + (super::TOP_HEIGHT as f32 - from_offset) * normalized;
                let fallback = result_panel_top_at(&phase, body_top, now).unwrap();
                let composition = super::result_overlay_motion_animation(&phase, now).unwrap();
                assert!((fallback - expected).abs() < 0.15);
                assert!((fallback - (body_top + composition.from_offset)).abs() < 1e-4);
            }

            let finished =
                result_panel_top_at(&phase, body_top, returned_at + Duration::from_millis(300))
                    .unwrap();
            assert!((finished - (body_top + super::TOP_HEIGHT as f32)).abs() < 1e-4);
        }
    }

    #[test]
    fn ui234_fixed_header_clip_contains_every_result_exit_frame() {
        for body_top in [0.0, POPUP_ARROW_HEIGHT as f32] {
            for offset in [0.0, 45.0, 90.0, 135.0, 180.0] {
                assert_eq!(
                    result_header_visible_height(body_top + offset, body_top),
                    180.0 - offset
                );
            }
            assert_eq!(
                result_header_visible_height(body_top - TOP_HEIGHT as f32, body_top),
                0.0
            );
            assert!(!result_header_hit_test_contains(body_top - 0.01, body_top));
            assert!(result_header_hit_test_contains(body_top, body_top));
            assert!(result_header_hit_test_contains(body_top + 179.99, body_top));
            assert!(!result_header_hit_test_contains(body_top + 180.0, body_top));
        }
    }

    #[test]
    fn ui234_composition_header_viewport_stays_fixed_across_dpi_and_arrow_edges() {
        for dpi in [96, 120, 144, 192, 240, 288] {
            for body_top in [0.0, POPUP_ARROW_HEIGHT as f32] {
                let root_snap = composition_button_pixel_snap_offset(dpi, body_top);
                let viewport = composition_header_viewport_offset(dpi, body_top);
                assert!((root_snap.X + viewport.X).abs() < 1e-6);
                assert!((root_snap.Y + viewport.Y - body_top).abs() < 1e-6);

                for exit_offset in [0.0, 45.0, 90.0, 135.0, 180.0] {
                    let child = result_overlay_child_offset(body_top + exit_offset, body_top);
                    assert!((child.Y - exit_offset).abs() < 1e-6);
                    assert_eq!(
                        result_header_visible_height(body_top + exit_offset, body_top),
                        180.0 - exit_offset
                    );
                }
            }
        }
    }

    #[test]
    fn tray_activation_uses_one_semantic_event_per_v4_selection() {
        assert!(!is_tray_activation_event(
            windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONUP as u16
        ));
        assert!(is_tray_activation_event(
            windows::Win32::UI::Shell::NIN_SELECT as u16
        ));
        assert!(is_tray_activation_event(
            (windows::Win32::UI::Shell::NIN_SELECT | windows::Win32::UI::Shell::NINF_KEY) as u16
        ));
        assert!(!is_tray_activation_event(
            windows::Win32::UI::WindowsAndMessaging::WM_RBUTTONUP as u16
        ));
    }

    #[test]
    fn result_card_holds_ten_seconds_after_entrance_then_exits() {
        let start = std::time::Instant::now();
        let entrance_finished = start + RESULT_CARD_TRANSITION;
        let hold_timer_fired = entrance_finished + RESULT_CARD_HOLD;
        assert!(matches!(
            advance_result_card_stage(
                ResultCardStage::Entering { started_at: start },
                entrance_finished
            ),
            ResultCardAdvance::Stage(ResultCardStage::Holding { started_at }) if started_at == entrance_finished
        ));
        assert!(matches!(
            advance_result_card_stage(
                ResultCardStage::Holding { started_at: entrance_finished },
                hold_timer_fired
            ),
            ResultCardAdvance::Stage(ResultCardStage::Exiting { started_at, from_offset: 0.0 }) if started_at == hold_timer_fired
        ));
        assert!(matches!(
            advance_result_card_stage(
                ResultCardStage::Exiting {
                    started_at: hold_timer_fired,
                    from_offset: 0.0
                },
                hold_timer_fired + RESULT_CARD_TRANSITION
            ),
            ResultCardAdvance::Ready
        ));
    }

    #[test]
    fn result_card_deadlines_keep_running_while_popup_is_hidden() {
        let start = Instant::now();
        let entrance_finished = start + RESULT_CARD_TRANSITION;
        let hold_finished = entrance_finished + RESULT_CARD_HOLD;
        let exit_finished = hold_finished + RESULT_CARD_TRANSITION;

        assert!(matches!(
            advance_result_card_stage(
                ResultCardStage::Entering { started_at: start },
                start + Duration::from_secs(5)
            ),
            ResultCardAdvance::Stage(ResultCardStage::Holding { started_at })
                if started_at == entrance_finished
        ));
        assert!(matches!(
            advance_result_card_stage(
                ResultCardStage::Entering { started_at: start },
                hold_finished + Duration::from_millis(150)
            ),
            ResultCardAdvance::Stage(ResultCardStage::Exiting { started_at, from_offset: 0.0 })
                if started_at == hold_finished
        ));
        assert!(matches!(
            advance_result_card_stage(
                ResultCardStage::Entering { started_at: start },
                exit_finished
            ),
            ResultCardAdvance::Ready
        ));
    }

    #[test]
    fn history_insert_rows_slide_independently_of_result_card() {
        let started_at = std::time::Instant::now();
        let transition = Some(HistoryMutationTransition {
            started_at,
            event: HistoryMutationEvent::Insert {
                index: 0,
                previous_count: 2,
            },
        });
        assert_eq!(history_row_top(0, transition, 100.0, 0.0, started_at), 9.0);
        assert_eq!(
            history_row_top(1, transition, 100.0, 0.0, started_at),
            100.0
        );
        assert_eq!(
            history_row_top(
                0,
                transition,
                100.0,
                0.0,
                started_at + std::time::Duration::from_millis(250)
            ),
            100.0
        );
        let first_row = Some(HistoryMutationTransition {
            started_at,
            event: HistoryMutationEvent::Insert {
                index: 0,
                previous_count: 0,
            },
        });
        assert_eq!(history_row_top(0, first_row, 100.0, 0.0, started_at), 9.0);
        let deletion = Some(HistoryMutationTransition {
            started_at,
            event: HistoryMutationEvent::Delete { index: 1 },
        });
        assert_eq!(history_row_top(1, deletion, 100.0, 0.0, started_at), 282.0);
        let update = Some(HistoryMutationTransition {
            started_at,
            event: HistoryMutationEvent::Update { index: 1 },
        });
        assert_eq!(history_row_top(1, update, 100.0, 0.0, started_at), 191.0);
    }

    #[test]
    fn windows_history_scroll_range_uses_content_height_not_row_count() {
        assert_eq!(super::history_content_height(0), 0.0);
        assert_eq!(super::history_max_scroll_offset(0, 430.0), 0.0);
        assert_eq!(super::history_max_scroll_offset(1, 430.0), 0.0);
        assert_eq!(super::history_max_scroll_offset(4, 430.0), 0.0);
        assert_eq!(super::history_max_scroll_offset(5, 430.0), 23.0);
        assert_eq!(super::history_max_scroll_offset(6, 430.0), 114.0);
        assert_eq!(super::history_max_scroll_offset(40, 430.0), 3_208.0);
    }

    #[test]
    fn windows_history_wheel_respects_system_lines_page_mode_and_fractional_delta() {
        let count = 40;
        assert_eq!(
            super::history_scroll_offset_after_wheel(100.0, 120.0, 3, count, 430.0),
            46.0
        );
        assert_eq!(
            super::history_scroll_offset_after_wheel(100.0, -120.0, 1, count, 430.0),
            118.0
        );
        assert_eq!(
            super::history_scroll_offset_after_wheel(100.0, -120.0, 3, count, 430.0),
            154.0
        );
        let first_half_detent =
            super::history_scroll_offset_after_wheel(0.0, -30.0, 3, count, 430.0);
        let second_half_detent =
            super::history_scroll_offset_after_wheel(first_half_detent, -30.0, 3, count, 430.0);
        assert_eq!(first_half_detent, 13.5);
        assert_eq!(second_half_detent, 27.0);
        assert_eq!(
            super::history_scroll_offset_after_wheel(100.0, -120.0, u32::MAX, count, 430.0),
            530.0
        );
        assert_eq!(
            super::history_scroll_offset_after_wheel(100.0, -120.0, 0, count, 430.0),
            100.0
        );
        assert_eq!(
            super::history_scroll_offset_after_wheel(100.0, f32::NAN, 3, count, 430.0),
            100.0
        );
        assert_eq!(
            super::history_scroll_offset_after_wheel(100.0, 120.0, 3, 4, 430.0),
            0.0
        );
    }

    #[test]
    fn windows_history_rows_hit_with_the_same_clip_spacing_and_scroll_geometry() {
        let now = Instant::now();
        assert_eq!(
            super::history_row_index_at(280.0, 200.0, 630.0, 5, None, 0.0, now),
            Some(0)
        );
        assert_eq!(
            super::history_row_index_at(289.5, 200.0, 630.0, 5, None, 0.0, now),
            None
        );
        assert_eq!(
            super::history_row_index_at(291.0, 200.0, 630.0, 5, None, 0.0, now),
            Some(1)
        );
        assert_eq!(
            super::history_row_index_at(630.0, 200.0, 630.0, 5, None, 0.0, now),
            None
        );
        assert_eq!(
            super::history_row_index_at(200.0, 200.0, 630.0, 5, None, 91.0, now),
            Some(1)
        );
    }

    #[test]
    fn windows_history_insert_preserves_scrolled_content_and_only_animates_at_top() {
        assert_eq!(
            super::history_offset_after_insert(0.0, 5, 430.0),
            (0.0, true)
        );
        assert_eq!(
            super::history_offset_after_insert(23.0, 6, 430.0),
            (114.0, false)
        );
        assert_eq!(
            super::history_offset_after_insert(3_208.0, 40, 430.0),
            (3_208.0, false)
        );
    }

    #[test]
    fn offline_markers_are_separated_from_timeout_and_no_match() {
        assert!(is_offline_failure("network is unreachable"));
        assert!(is_offline_failure("could not resolve host"));
        assert!(!is_offline_failure("request timed out"));
        assert!(!is_offline_failure("no match"));
        assert!(!is_offline_failure("unexpected parser failure"));
    }

    #[test]
    fn outer_circle_children_keep_native_base_opacities() {
        assert_eq!(OUTER_BASE_OPACITY, [0.4, 0.6]);
    }

    #[test]
    fn listening_layers_match_native_z_order_without_covering_the_button() {
        assert_eq!(
            composition_root_layer_order(),
            [
                CompositionRootLayer::AutoShadow,
                CompositionRootLayer::InnerRipples,
                CompositionRootLayer::Button,
                CompositionRootLayer::OuterRipples,
            ]
        );

        let levels = ui_model::UiLevels {
            bass: 1.0,
            treble: 0.0,
        };
        let max_button_diameter = BUTTON_SIZE as f64
            * ui_model::wave_frame_at(ui_model::TaggingMode::Manual, 0.0, levels)
                .button_child_scale;
        for step in 0..=500 {
            let elapsed = ui_model::OUTER_LOOP_DURATION * f64::from(step) / 500.0;
            let frame = ui_model::wave_frame_at(ui_model::TaggingMode::Manual, elapsed, levels);
            for (index, ring) in frame.outer.iter().enumerate() {
                assert!(
                    ring.diameter - ring.stroke_width > max_button_diameter,
                    "outer ring {index} overlaps the maximum button diameter at {elapsed:.4}s"
                );
            }
        }
        assert_eq!(
            composition_scene_layer_order(),
            [
                CompositionSceneLayer::Tagging,
                CompositionSceneLayer::ResultOverlay,
            ]
        );
    }

    #[test]
    fn result_fallback_hides_shazam_button_only_while_the_card_crosses_it() {
        let body_top = POPUP_ARROW_HEIGHT as f32;
        let button_top = body_top + BUTTON_TOP as f32;
        let button_bottom = button_top + BUTTON_SIZE as f32;

        assert!(!result_overlay_occludes_shazam_button(None, body_top));
        assert!(!result_overlay_occludes_shazam_button(
            Some(button_top - TOP_HEIGHT as f32),
            body_top
        ));
        assert!(result_overlay_occludes_shazam_button(
            Some(button_top - TOP_HEIGHT as f32 + 0.01),
            body_top
        ));
        assert!(result_overlay_occludes_shazam_button(
            Some(body_top),
            body_top
        ));
        assert!(!result_overlay_occludes_shazam_button(
            Some(button_bottom),
            body_top
        ));
    }

    #[test]
    fn ui228_result_overlay_pixel_copy_preserves_alpha_and_clips_panel_edges() {
        let source = [
            1, 2, 3, 4, 5, 6, 7, 8, // first source row
            9, 10, 11, 12, 13, 14, 15, 16, // second source row
        ];
        let mut output = Vec::new();
        copy_panel_pixels_into(&source, 2, 2, -1, 2, 3, &mut output);
        assert_eq!(
            output,
            [
                0, 0, 0, 0, 0, 0, 0, 0, // above the host window is transparent
                1, 2, 3, 4, 5, 6, 7, 8, // alpha and color channels are retained
                9, 10, 11, 12, 13, 14, 15, 16,
            ]
        );

        copy_panel_pixels_into(&source, 2, 2, 1, 2, 2, &mut output);
        assert_eq!(
            output,
            [9, 10, 11, 12, 13, 14, 15, 16, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    #[ignore = "requires the Windows Composition graphics device; run explicitly on Windows"]
    fn composition_result_overlay_uploads_pixels_and_resizes_at_fractional_dpi() {
        use super::{
            result_card_easing, CompositionResultOverlay, Interface, LayeredSurface,
            ResultOverlayMotionAnimation, ResultOverlayMotionKey, RESULT_CARD_TRANSITION,
        };

        unsafe {
            super::CoInitializeEx(None, super::COINIT_APARTMENTTHREADED).unwrap();
            let dispatcher =
                super::CreateDispatcherQueueController(super::DispatcherQueueOptions {
                    dwSize: std::mem::size_of::<super::DispatcherQueueOptions>() as u32,
                    threadType: super::DQTYPE_THREAD_CURRENT,
                    apartmentType: super::DQTAT_COM_STA,
                })
                .unwrap();
            let instance = super::GetModuleHandleW(None).unwrap();
            let class_name = super::wide("STATIC");
            let title = super::wide("OpenShazam Composition Surface Test");
            let hwnd = super::CreateWindowExW(
                super::WS_EX_LAYERED,
                super::PCWSTR(class_name.as_ptr()),
                super::PCWSTR(title.as_ptr()),
                super::WS_POPUP,
                0,
                0,
                380,
                610,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .unwrap();
            let result = (|| -> windows::core::Result<()> {
                let compositor = super::Compositor::new()?;
                let mut overlay = CompositionResultOverlay::new(&compositor, 96, 10.0)?;
                let desktop_interop: super::ICompositorDesktopInterop = compositor.cast()?;
                let target = desktop_interop.CreateDesktopWindowTarget(hwnd, true)?;
                let root = compositor.CreateContainerVisual()?;
                root.SetSize(super::Vector2 { X: 380.0, Y: 610.0 })?;
                root.Children()?.InsertAtTop(&overlay.viewport)?;
                target.SetRoot(&root)?;
                let source_96 = LayeredSurface::new(380, 610)?;
                let bytes_96 = std::slice::from_raw_parts_mut(
                    source_96._pixels.cast::<u8>(),
                    source_96.size.width as usize * source_96.size.height as usize * 4,
                );
                bytes_96.fill(0);
                let first_content_pixel_96 = 10 * 380 * 4;
                bytes_96[first_content_pixel_96..first_content_pixel_96 + 4]
                    .copy_from_slice(&[0, 0, 0, 255]);
                overlay.update_from_layered(&source_96, 10.0, 10.0, 96)?;

                bytes_96[first_content_pixel_96..first_content_pixel_96 + 4]
                    .copy_from_slice(&[32, 64, 128, 255]);
                overlay.update_from_layered(&source_96, 10.0, 10.0, 96)?;
                source_96.present(hwnd, 255)?;

                let source_120 = LayeredSurface::new(475, 280)?;
                let bytes_120 = std::slice::from_raw_parts_mut(
                    source_120._pixels.cast::<u8>(),
                    source_120.size.width as usize * source_120.size.height as usize * 4,
                );
                bytes_120.fill(0);
                let first_content_pixel_120 = 16 * 475 * 4;
                bytes_120[first_content_pixel_120..first_content_pixel_120 + 4]
                    .copy_from_slice(&[16, 32, 64, 255]);
                overlay.update_from_layered(&source_120, 10.0, 10.0, 120)?;
                overlay.animate_position(
                    &compositor,
                    &ResultOverlayMotionAnimation {
                        key: ResultOverlayMotionKey::NoMatchEnter {
                            entered_at: Instant::now(),
                            offline: false,
                        },
                        from_offset: -180.0,
                        to_offset: 0.0,
                        remaining: RESULT_CARD_TRANSITION,
                        easing: result_card_easing(),
                    },
                    10.0,
                    120,
                )?;
                Ok(())
            })();
            let _ = super::DestroyWindow(hwnd);
            drop(dispatcher);
            super::CoUninitialize();
            result.unwrap();
        }
    }

    #[test]
    #[ignore = "requires a Windows Composition dispatcher and graphics device; run explicitly on Windows"]
    fn composition_status_text_renders_transitions_and_resizes_at_fractional_dpi() {
        use super::{
            CompositionResultOverlay, CompositionStatusText, StatusTextLabel, StatusTextSnapshot,
            DWRITE_FACTORY_TYPE_SHARED,
        };

        unsafe {
            super::CoInitializeEx(None, super::COINIT_APARTMENTTHREADED).unwrap();
            let dispatcher =
                super::CreateDispatcherQueueController(super::DispatcherQueueOptions {
                    dwSize: std::mem::size_of::<super::DispatcherQueueOptions>() as u32,
                    threadType: super::DQTYPE_THREAD_CURRENT,
                    apartmentType: super::DQTAT_COM_STA,
                })
                .unwrap();
            let result = (|| -> windows::core::Result<()> {
                let compositor = super::Compositor::new()?;
                let overlay = CompositionResultOverlay::new(&compositor, 96, 10.0)?;
                let write_factory: super::IDWriteFactory =
                    super::DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
                let mut status = CompositionStatusText::new(
                    &compositor,
                    &overlay._graphics_device,
                    write_factory,
                    96,
                    10.0,
                )?;
                let started_at = super::Instant::now();
                status.update(
                    StatusTextSnapshot {
                        current: Some(StatusTextLabel::Ready),
                        previous: None,
                        transition_started_at: None,
                    },
                    started_at,
                    10.0,
                    96,
                )?;
                assert_eq!(
                    status.labels[0].text.as_deref(),
                    Some(StatusTextLabel::Ready.text(status.language))
                );
                assert!(status.controllers.iter().all(Option::is_none));

                let transition_started_at = started_at + super::Duration::from_secs(3);
                status.update(
                    StatusTextSnapshot {
                        current: Some(StatusTextLabel::AutoTurnOff),
                        previous: Some(StatusTextLabel::AutoOn),
                        transition_started_at: Some(transition_started_at),
                    },
                    transition_started_at + super::Duration::from_millis(200),
                    10.0,
                    96,
                )?;
                assert_eq!(
                    status.labels[0].text.as_deref(),
                    Some(StatusTextLabel::AutoOn.text(status.language))
                );
                assert_eq!(
                    status.labels[1].text.as_deref(),
                    Some(StatusTextLabel::AutoTurnOff.text(status.language))
                );
                assert!(status.controllers.iter().all(Option::is_some));

                status.update(
                    StatusTextSnapshot {
                        current: Some(StatusTextLabel::AutoTurnOff),
                        previous: None,
                        transition_started_at: None,
                    },
                    transition_started_at + super::Duration::from_millis(500),
                    10.0,
                    120,
                )?;
                assert_eq!(
                    status.labels[0].text.as_deref(),
                    Some(StatusTextLabel::AutoTurnOff.text(status.language))
                );
                assert!(status.controllers.iter().all(Option::is_none));
                assert_eq!(status.labels[0].size.width, 475);
                assert_eq!(status.labels[0].size.height, 24);
                assert_eq!(status.dpi, 120);
                Ok(())
            })();
            drop(dispatcher);
            super::CoUninitialize();
            result.unwrap();
        }
    }

    #[test]
    fn button_keeps_its_specified_absolute_position() {
        for body_top in [0.0, POPUP_ARROW_HEIGHT as f32] {
            let button_left = BUTTON_X as f32;
            let button_top = body_top + BUTTON_TOP as f32;
            let mask_left = button_left + BUTTON_SIZE as f32 * 0.5 - INNER_MASK_BOUNDS * 0.5;
            let mask_top = button_top + BUTTON_SIZE as f32 * 0.5 - INNER_MASK_BOUNDS * 0.5;
            let button_radius = BUTTON_SIZE as f32 * 0.5;
            assert!(
                (mask_left + INNER_MASK_BOUNDS * 0.5 - (button_left + button_radius)).abs() < 1e-5
            );
            assert!(
                (mask_top + INNER_MASK_BOUNDS * 0.5 - (button_top + button_radius)).abs() < 1e-5
            );
        }
    }

    #[test]
    fn inner_ripple_clip_keeps_a_button_sized_hole_independent_of_child_rings() {
        assert_eq!(
            INNER_MASK_BOUNDS,
            ui_model::INNER_DIAMETERS[0] as f32 + 48.0
        );
        assert_eq!(
            inverse_circle_clip_radii(INNER_MASK_BOUNDS, BUTTON_CLEAR_DIAMETER),
            Some((168.0, 48.0))
        );
        assert_eq!(inverse_circle_clip_radii(96.0, 96.0), None);
        assert_eq!(inverse_circle_clip_radii(f32::NAN, 48.0), None);
    }

    #[test]
    fn auto_shadow_display_host_stays_independent_of_its_mask_canvas() {
        assert_eq!(AUTO_SHADOW_DISPLAY_BOUNDS, BUTTON_CLEAR_DIAMETER);
        assert_eq!(AUTO_SHADOW_DISPLAY_BOUNDS, 96.0);
        assert_eq!(INNER_MASK_BOUNDS, BUTTON_CLEAR_DIAMETER * 3.5);

        let center = auto_shadow_host_center();
        assert_eq!(center.X, 48.0);
        assert_eq!(center.Y, 48.0);

        // At the native 0.85 button pulse the 336pt mask still exceeds the
        // display host; the visible edge is limited by the 96pt host bounds.
        let pulse_scale = 0.85_f32;
        let mask_outer_diameter = INNER_MASK_BOUNDS * pulse_scale;
        let mask_hole_diameter = BUTTON_CLEAR_DIAMETER * pulse_scale;
        let visible_host_diameter = AUTO_SHADOW_DISPLAY_BOUNDS;
        let visible_edge_width = (visible_host_diameter - mask_hole_diameter) * 0.5;
        assert!(mask_outer_diameter > visible_host_diameter);
        assert!((mask_hole_diameter - 81.6).abs() < 1e-4);
        assert!((visible_edge_width - 7.2).abs() < 1e-4);
    }

    #[test]
    fn auto_shazam_omits_manual_outer_ripple_layers() {
        assert!(super::composition_outer_ripples_visible(
            ui_model::TaggingMode::Manual
        ));
        assert!(!super::composition_outer_ripples_visible(
            ui_model::TaggingMode::Auto
        ));
        assert!(!super::composition_outer_ripples_visible(
            ui_model::TaggingMode::Ready
        ));
    }

    #[test]
    fn inner_ripple_mask_tracks_button_scale_separately_from_audio_fill() {
        for bass in [0.0, 0.2, 5.0 / 7.0, 1.0] {
            let frame = ui_model::wave_frame_at(
                ui_model::TaggingMode::Manual,
                0.0,
                ui_model::UiLevels { bass, treble: 0.5 },
            );
            let hole_diameter = BUTTON_CLEAR_DIAMETER as f64 * frame.button_child_scale;
            let fill_scale = frame.inner_container_factor / 3.0 * frame.inner_audio_scale;
            let compensated_fill_scale = fill_scale / frame.button_child_scale;
            let expected_button_scale = 0.75 + 0.35 * bass;
            let expected_fill_scale = 0.7 + 0.6 * bass;
            assert!((frame.button_child_scale - expected_button_scale).abs() < 1e-9);
            assert!((fill_scale - expected_fill_scale).abs() < 1e-9);
            assert!((hole_diameter - 96.0 * expected_button_scale).abs() < 1e-9);
            assert!((frame.button_child_scale * compensated_fill_scale - fill_scale).abs() < 1e-9);
            assert!(
                (48.0 * (expected_fill_scale - expected_button_scale) - (-2.4 + 12.0 * bass)).abs()
                    < 1e-9
            );
            assert!(frame.button_child_scale > 0.0);
            assert!(fill_scale > 0.0);
        }
    }

    #[test]
    fn logo_startup_animation_runs_once_for_the_main_scene_only() {
        assert!(should_start_startup_logo(WindowKind::Main, false));
        assert!(!should_start_startup_logo(WindowKind::Main, true));
        assert!(!should_start_startup_logo(WindowKind::About, false));
    }

    #[test]
    fn album_art_frames_keep_native_view_sizes_without_button_padding() {
        let result = result_artwork_rect(10.0, ArrowEdge::Top);
        assert_eq!(
            (result.left, result.top, result.right, result.bottom),
            (10.0, 30.0, 160.0, 180.0)
        );
        assert_eq!(result.right - result.left, 150.0);
        assert_eq!(result.bottom - result.top, 150.0);
        assert_eq!(RESULT_ARTWORK_CORNER_RADIUS, 5.0);

        let bottom_arrow_result = result_artwork_rect(0.0, ArrowEdge::Bottom);
        assert_eq!(
            (
                bottom_arrow_result.left,
                bottom_arrow_result.top,
                bottom_arrow_result.right,
                bottom_arrow_result.bottom,
            ),
            (10.0, 10.0, 160.0, 160.0)
        );

        let history = history_artwork_frame(200.0);
        assert_eq!(
            (history.left, history.top, history.right, history.bottom),
            (20.0, 211.0, 85.0, 276.0)
        );
        assert_eq!(history.right - history.left, 65.0);
        assert_eq!(history.bottom - history.top, 65.0);
    }

    #[test]
    fn match_fallback_uses_native_intrinsic_size_and_centering() {
        let frame = result_artwork_rect(10.0, ArrowEdge::Top);
        let image = fallback_match_artwork_rect(frame);
        assert_eq!(FALLBACK_MATCH_LOGICAL_SIDE, 131.0);
        assert_eq!(
            (image.left, image.top, image.right, image.bottom),
            (19.5, 39.5, 150.5, 170.5)
        );

        let bottom_arrow_frame = result_artwork_rect(0.0, ArrowEdge::Bottom);
        let bottom_arrow_image = fallback_match_artwork_rect(bottom_arrow_frame);
        assert_eq!(
            (
                bottom_arrow_image.left,
                bottom_arrow_image.top,
                bottom_arrow_image.right,
                bottom_arrow_image.bottom,
            ),
            (19.5, 19.5, 150.5, 150.5)
        );
    }

    #[test]
    fn match_fallback_theme_assets_preserve_premultiplied_alpha() {
        fn assert_white_alpha_mask(pixels: &[u8], max_alpha: u8) {
            assert_eq!(pixels.len(), 262 * 262 * 4);
            for pixel in pixels.chunks_exact(4) {
                assert_eq!(pixel[0], pixel[1]);
                assert_eq!(pixel[1], pixel[2]);
                assert_eq!(pixel[2], pixel[3]);
            }
            let center = (131 * 262 + 131) * 4;
            assert_eq!(&pixels[center..center + 4], &[0, 0, 0, 0]);
            assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == max_alpha));
        }

        assert_white_alpha_mask(FALLBACK_MATCH_LIGHT_PIXELS, 179);
        assert_white_alpha_mask(FALLBACK_MATCH_DARK_PIXELS, 77);
        assert_ne!(FALLBACK_MATCH_LIGHT_PIXELS, FALLBACK_MATCH_DARK_PIXELS);
    }

    #[test]
    fn bottom_arrow_is_excluded_from_the_header_gradient_union() {
        assert_eq!(
            header_fill_scope(ArrowEdge::Top),
            HeaderFillScope::HeaderAndArrow
        );
        assert_eq!(
            header_fill_scope(ArrowEdge::Bottom),
            HeaderFillScope::HeaderRectangle
        );
        assert_eq!(popup_base_color(Theme::Light), (255, 255, 255));
        assert_eq!(popup_base_color(Theme::Dark), (30, 30, 30));
    }

    #[test]
    fn ui183_result_gradient_reverses_for_each_theme_and_result_surface() {
        let surfaces = ["header", "matched", "no-match/offline"];
        let cases = [
            (
                Theme::Light,
                [(244, 247, 249), (188, 205, 216)],
                [(188, 205, 216), (244, 247, 249)],
            ),
            (
                Theme::Dark,
                [(0, 0, 0), (55, 66, 73)],
                [(55, 66, 73), (0, 0, 0)],
            ),
        ];
        for (theme, top, bottom) in cases {
            for (edge, expected) in [(ArrowEdge::Top, top), (ArrowEdge::Bottom, bottom)] {
                for surface in surfaces {
                    assert_eq!(
                        result_gradient_colors(theme, edge),
                        expected,
                        "{surface} gradient for {theme:?}/{edge:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn ui184_result_artwork_keeps_native_insets_while_panel_moves_at_hidpi() {
        let dpi_sizes = [(96, 150), (120, 188), (144, 225), (192, 300)];
        for (dpi, expected_pixel_size) in dpi_sizes {
            let scale = composition_scale_for_dpi(dpi);
            for panel_top in [-180.0, -170.0, -90.0, 0.0, 10.0, 180.0, 190.0] {
                for (edge, expected_top_inset) in
                    [(ArrowEdge::Top, 20.0), (ArrowEdge::Bottom, 10.0)]
                {
                    let art = result_artwork_rect(panel_top, edge);
                    assert_eq!(art.left, 10.0);
                    assert_eq!(art.top - panel_top, expected_top_inset);
                    assert_eq!(art.right, 160.0);
                    assert_eq!(art.bottom - panel_top, expected_top_inset + 150.0);
                    assert_eq!(art.right - art.left, 150.0);
                    assert_eq!(art.bottom - art.top, 150.0);
                    assert_eq!((150.0 * scale).round() as i32, expected_pixel_size);
                }
            }
        }
    }

    #[test]
    fn result_details_keep_the_derived_164_point_text_column() {
        assert_eq!(RESULT_DETAILS_LEFT, 172.0);
        assert_eq!(RESULT_DETAILS_RIGHT - RESULT_DETAILS_LEFT, 164.0);
    }

    #[test]
    fn result_details_stack_tracks_artwork_center_and_expands_for_wrapping() {
        let single_line = result_details_rects(10.0, 24.0, 18.0, true);
        assert_eq!(single_line.title.top, 66.5);
        assert_eq!(single_line.title.bottom, 90.5);
        assert_eq!(single_line.artist.top, 95.5);
        assert_eq!(single_line.action.top, 123.5);
        assert_eq!(
            (single_line.title.top + single_line.action.bottom) * 0.5,
            105.0
        );

        let no_action = result_details_rects(10.0, 24.0, 18.0, false);
        assert_eq!(no_action.title.top, 81.5);
        assert_eq!(no_action.artist.bottom, 128.5);
        assert_eq!(no_action.action.left, 0.0);
        assert_eq!((no_action.title.top + no_action.artist.bottom) * 0.5, 105.0);

        let wrapped = result_details_rects(10.0, 48.0, 36.0, true);
        assert_eq!(wrapped.title.top, 45.5);
        assert_eq!(wrapped.action.bottom, 164.5);
        assert_eq!((wrapped.title.top + wrapped.action.bottom) * 0.5, 105.0);
    }

    #[test]
    fn ui235_history_text_stack_uses_measured_lines_and_stays_inside_89_dip_row() {
        let top = 120.0;
        let text = history_text_rects(top, 34.0, 28.0, 12.0);
        assert_eq!(text.title.bottom + 5.0, text.artist.top);
        assert_eq!(text.artist.bottom + 5.0, text.date.top);
        assert!(((text.title.top + text.date.bottom) * 0.5 - top - 43.5).abs() < f32::EPSILON);
        assert!(text.title.top >= top);
        assert!(text.date.bottom <= top + HISTORY_ROW_HEIGHT);

        // Result text uses measured DirectWrite line heights too. With the
        // unavailable store action omitted, the maximum title/artist stack
        // remains centered and does not escape the fixed 180-DIP header.
        let result = result_details_rects(0.0, 80.0, 74.0, false);
        assert!(result.title.top >= 0.0);
        assert!(result.artist.bottom <= TOP_HEIGHT as f32);
    }

    #[test]
    fn ui237_history_artwork_and_text_stack_share_the_constraint_resolved_center() {
        for row_top in [0.0, 11.0, 200.0] {
            let artwork = history_artwork_frame(row_top);
            let artwork_center_y = (artwork.top + artwork.bottom) * 0.5;
            assert_eq!(artwork.left, 20.0);
            assert_eq!(artwork.top, row_top + 11.0);
            assert_eq!(artwork.right - artwork.left, 65.0);
            assert!((artwork_center_y - (row_top + 43.5)).abs() < f32::EPSILON);

            let text = history_text_rects(row_top, 34.0, 28.0, 12.0);
            let text_center_y = (text.title.top + text.date.bottom) * 0.5;
            assert_eq!(text.title.left, artwork.right + 10.0);
            assert!((text_center_y - artwork_center_y).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn directwrite_result_and_history_layouts_keep_unicode_clusters_inside_fixed_frames() {
        let factory: super::IDWriteFactory =
            unsafe { super::DWriteCreateFactory(super::DWRITE_FACTORY_TYPE_SHARED).unwrap() };
        let long_unbroken = "OpenShazam-Unbroken-Title-".repeat(18);
        let cases = [
            (
                UiLanguage::Japanese,
                "カレンダーガール ありがとう こんにちは".to_owned(),
            ),
            (UiLanguage::English, long_unbroken),
            (
                UiLanguage::Japanese,
                "女性ボーカル👩🏻‍🎤の曲 e\u{301} と結合文字".to_owned(),
            ),
            (
                UiLanguage::English,
                "العربية עברית mixed RTL text\r\nnext line".to_owned(),
            ),
            (UiLanguage::English, String::new()),
        ];
        let mut trimmed_cases = 0usize;

        for (language, value) in cases {
            let locale = directwrite_locale(language);
            for (font_size, width, height) in [
                (20.0, 164.0, 72.0),
                (14.0, 164.0, 72.0),
                (14.0, 265.0, 30.0),
                (12.0, 265.0, 25.0),
                (10.0, 265.0, 21.0),
            ] {
                let format = create_text_format_with_locale(
                    &factory,
                    font_size,
                    super::DWRITE_FONT_WEIGHT_NORMAL,
                    super::DWRITE_TEXT_ALIGNMENT_LEADING,
                    locale,
                )
                .unwrap();
                let content: Vec<u16> = value.encode_utf16().collect();
                if content.is_empty() {
                    continue;
                }
                let layout = unsafe {
                    factory
                        .CreateTextLayout(&content, &format, width, height)
                        .unwrap()
                };
                let mut metrics = super::DWRITE_TEXT_METRICS::default();
                unsafe { layout.GetMetrics(&mut metrics).unwrap() };
                assert_eq!(metrics.layoutWidth, width);
                assert_eq!(metrics.layoutHeight, height);
                assert!(metrics.width.is_finite() && metrics.width >= 0.0);
                assert!(metrics.height.is_finite() && metrics.height <= height + 0.01);

                let mut line_metrics = vec![
                    windows::Win32::Graphics::DirectWrite::DWRITE_LINE_METRICS::default();
                    metrics.lineCount as usize
                ];
                let mut actual_line_count = 0;
                unsafe {
                    layout
                        .GetLineMetrics(Some(&mut line_metrics), &mut actual_line_count)
                        .unwrap()
                };
                assert_eq!(actual_line_count, metrics.lineCount);
                for line in line_metrics.iter().take(actual_line_count as usize) {
                    assert!(line.height.is_finite() && line.height >= 0.0);
                    assert!(line.baseline.is_finite() && line.baseline >= 0.0);
                }
                let measured = unsafe {
                    super::measure_directwrite_text_layout(
                        &factory,
                        &value,
                        &format,
                        width,
                        0.0,
                        if width == 164.0 {
                            4.0
                        } else if font_size == 10.0 {
                            1.0
                        } else {
                            2.0
                        },
                    )
                    .unwrap()
                };
                assert!(measured.height.is_finite() && measured.height >= 0.0);
                assert!(measured.first_baseline.is_finite());
                assert!(measured.last_baseline.is_finite());
                assert!(measured.last_baseline <= measured.height + 0.01);
                assert!(measured.overhang_left.is_finite());
                assert!(measured.overhang_top.is_finite());
                assert!(measured.overhang_right.is_finite());
                assert!(measured.overhang_bottom.is_finite());
                if line_metrics
                    .iter()
                    .take(actual_line_count as usize)
                    .any(|line| line.isTrimmed.as_bool())
                {
                    trimmed_cases += 1;
                }

                let mut clusters = vec![
                    windows::Win32::Graphics::DirectWrite::DWRITE_CLUSTER_METRICS::default();
                    content.len()
                ];
                let mut actual_cluster_count = 0;
                unsafe {
                    layout
                        .GetClusterMetrics(Some(&mut clusters), &mut actual_cluster_count)
                        .unwrap()
                };
                let mut utf16_end = 0usize;
                let mut cluster_boundaries = Vec::with_capacity(actual_cluster_count as usize);
                for cluster in clusters.iter().take(actual_cluster_count as usize) {
                    utf16_end += usize::from(cluster.length);
                    assert!(
                        String::from_utf16(&content[..utf16_end]).is_ok(),
                        "DirectWrite split a UTF-16 surrogate pair in {value:?}"
                    );
                    cluster_boundaries.push(utf16_end);
                }
                assert_eq!(utf16_end, content.len(), "text clusters lost UTF-16 units");
                let combining_sequence: Vec<u16> = "e\u{301}".encode_utf16().collect();
                if let Some(start) = content
                    .windows(combining_sequence.len())
                    .position(|window| window == combining_sequence)
                {
                    assert!(
                        !cluster_boundaries.contains(&(start + 1)),
                        "DirectWrite split a base character from its combining mark"
                    );
                }
                let emoji_sequence: Vec<u16> = "👩🏻‍🎤".encode_utf16().collect();
                if let Some(start) = content
                    .windows(emoji_sequence.len())
                    .position(|window| window == emoji_sequence)
                {
                    for boundary in start + 1..start + emoji_sequence.len() {
                        assert!(
                            !cluster_boundaries.contains(&boundary),
                            "DirectWrite split an emoji ZWJ grapheme cluster"
                        );
                    }
                }
            }
        }
        assert!(trimmed_cases > 0, "matrix did not exercise text trimming");
    }

    #[test]
    fn gradient_origin_translation_preserves_dip_scale_and_tracks_panel_top() {
        for top in [0.0, 10.0, 27.5, 180.0] {
            let transform = gradient_origin_transform(top);
            assert_eq!(transform.M11, 1.0);
            assert_eq!(transform.M12, 0.0);
            assert_eq!(transform.M21, 0.0);
            assert_eq!(transform.M22, 1.0);
            assert_eq!(transform.M31, 0.0);
            assert_eq!(transform.M32, top);
        }
    }

    #[test]
    fn startup_logo_keeps_the_ready_timer_until_idle_pulse_can_start() {
        let ready = MainPhase::Ready;
        assert!(!animation_needs_frame(&ready, false));
        assert!(animation_needs_frame(&ready, true));

        let listening = MainPhase::Listening {
            mode: CaptureMode::Manual,
            started_at: std::time::Instant::now(),
        };
        assert!(!animation_needs_frame(&listening, false));

        let auto_started = Instant::now();
        let auto_listening = MainPhase::Listening {
            mode: CaptureMode::Auto,
            started_at: auto_started,
        };
        assert!(phase_needs_frame_at(&auto_listening, auto_started));
        assert!(phase_needs_frame_at(
            &auto_listening,
            auto_started + Duration::from_millis(199)
        ));
        assert!(!phase_needs_frame_at(
            &auto_listening,
            auto_started + Duration::from_millis(200)
        ));
    }

    #[test]
    fn auto_wave_and_button_spring_start_before_capture_and_keep_the_same_clock() {
        let auto_started = Instant::now();
        let capture_event = auto_started + Duration::from_millis(120);
        let starting = MainPhase::Starting {
            mode: CaptureMode::Auto,
            entered_at: auto_started,
        };

        assert_eq!(
            phase_animation_state(&starting),
            (ui_model::TaggingMode::Auto, true, true)
        );
        assert_eq!(
            status_text_snapshot(&starting, None).current,
            Some(super::StatusTextLabel::AutoOn)
        );
        assert!(phase_needs_frame_at(&starting, auto_started));
        assert!(phase_needs_frame_at(
            &starting,
            auto_started + Duration::from_millis(199)
        ));
        assert!(!phase_needs_frame_at(
            &starting,
            auto_started + AUTO_START_SPRING_DURATION
        ));
        assert_eq!(
            capture_animation_start(&starting, CaptureMode::Auto, capture_event),
            auto_started,
            "CaptureStarted must not restart the native Auto pulse"
        );

        let manual_starting = MainPhase::Starting {
            mode: CaptureMode::Manual,
            entered_at: auto_started,
        };
        assert_eq!(
            phase_animation_state(&manual_starting),
            (ui_model::TaggingMode::Ready, true, false)
        );
        assert_eq!(status_text_snapshot(&manual_starting, None).current, None);
        assert_eq!(
            capture_animation_start(&manual_starting, CaptureMode::Manual, capture_event),
            capture_event
        );
    }

    #[test]
    fn auto_capture_can_be_cancelled_during_start_listen_and_cancel_phases() {
        let now = Instant::now();
        for phase in [
            MainPhase::Starting {
                mode: CaptureMode::Auto,
                entered_at: now,
            },
            MainPhase::Listening {
                mode: CaptureMode::Auto,
                started_at: now,
            },
            MainPhase::Cancelling {
                mode: CaptureMode::Auto,
                entered_at: now,
            },
        ] {
            assert_eq!(capture_mode_in_progress(&phase), Some(CaptureMode::Auto));
        }

        let manual_starting = MainPhase::Starting {
            mode: CaptureMode::Manual,
            entered_at: now,
        };
        assert_eq!(
            capture_mode_in_progress(&manual_starting),
            Some(CaptureMode::Manual)
        );
        assert_eq!(capture_mode_in_progress(&MainPhase::Ready), None);
    }

    #[test]
    fn status_text_push_uses_composition_and_keeps_d2d_fallback_deterministic() {
        let start = std::time::Instant::now();
        let ready = MainPhase::Ready;
        assert!(!super::animation_needs_d2d_redraw(
            &ready, false, false, false
        ));
        assert!(super::animation_needs_d2d_redraw(
            &ready, true, false, false
        ));
        assert!(!super::animation_needs_d2d_redraw(
            &ready, true, true, false
        ));
        assert_eq!(super::status_text_progress(start, start), 0.0);
        assert_eq!(super::status_text_linear_progress(start, start), 0.0);
        assert!(
            (super::status_text_progress(start, start + std::time::Duration::from_millis(200))
                - 0.5)
                .abs()
                < 0.001
        );
        assert_eq!(
            super::status_text_linear_progress(
                start,
                start + std::time::Duration::from_millis(200)
            ),
            0.5
        );
        assert_eq!(
            super::status_text_linear_progress(
                start,
                start + std::time::Duration::from_millis(400)
            ),
            1.0
        );
        assert_eq!(
            super::status_text_progress(start, start + std::time::Duration::from_millis(400)),
            1.0
        );
        assert_eq!(
            super::status_text_progress(start, start - std::time::Duration::from_millis(1)),
            0.0
        );
    }

    #[test]
    fn composition_result_overlay_suppresses_redundant_d2d_animation_redraws() {
        let now = Instant::now();
        let matched = MainPhase::Matched {
            track: Track {
                display_id: 1,
                title: String::new(),
                artist: String::new(),
                recognized_at: now,
                recognized_at_utc: SystemTime::UNIX_EPOCH,
                recognized_date_label: String::new(),
                timestamp_source: HistoryTimestampSource::ControllerEvent,
                artwork_url: None,
                hq_artwork_url: None,
                background_artwork_url: None,
                track_key: None,
                config_generation: None,
                page_url: None,
            },
            stage: ResultCardStage::Entering { started_at: now },
        };
        let no_match = MainPhase::NoMatch {
            entered_at: now,
            returned_to_ready_at: None,
        };

        assert!(super::animation_needs_d2d_redraw(
            &matched, false, true, false
        ));
        assert!(!super::animation_needs_d2d_redraw(
            &matched, false, true, true
        ));
        assert!(super::animation_needs_d2d_redraw(
            &no_match, false, true, false
        ));
        assert!(!super::animation_needs_d2d_redraw(
            &no_match, false, true, true
        ));
        // Composition failure must keep the D2D fallback repaint active.
        assert!(super::animation_needs_d2d_redraw(
            &matched, false, false, false
        ));
    }

    #[test]
    fn composition_overlay_readiness_is_bound_to_current_phase_and_artwork() {
        let now = Instant::now();
        let track = Track {
            display_id: 5,
            title: "Title".to_owned(),
            artist: "Artist".to_owned(),
            recognized_at: now,
            recognized_at_utc: SystemTime::UNIX_EPOCH,
            recognized_date_label: String::new(),
            timestamp_source: HistoryTimestampSource::ControllerEvent,
            artwork_url: Some("https://cdn.example.test/cover.jpg".to_owned()),
            hq_artwork_url: None,
            background_artwork_url: None,
            track_key: None,
            config_generation: None,
            page_url: None,
        };
        let phase = MainPhase::Matched {
            track: track.clone(),
            stage: ResultCardStage::Holding { started_at: now },
        };
        let fingerprint = (ArtworkSource::Standard, 1024, 0xAABB_CCDD);
        let identity = super::ResultOverlayIdentity::Matched {
            display_id: track.display_id,
            title: track.title.clone(),
            artist: track.artist.clone(),
            artwork_url: track.artwork_url.clone(),
            hq_artwork_url: track.hq_artwork_url.clone(),
            artwork: Some(fingerprint),
        };
        assert!(super::result_overlay_identity_matches_phase(
            &identity,
            &phase,
            Some(fingerprint),
        ));
        assert!(!super::result_overlay_identity_matches_phase(
            &identity,
            &phase,
            Some((ArtworkSource::Standard, 1024, 0x1122_3344)),
        ));

        let no_match = MainPhase::NoMatch {
            entered_at: now,
            returned_to_ready_at: None,
        };
        let no_match_identity = super::ResultOverlayIdentity::NoMatch {
            entered_at: now,
            offline: false,
        };
        assert!(super::result_overlay_identity_matches_phase(
            &no_match_identity,
            &no_match,
            None,
        ));
        let offline = MainPhase::Offline {
            entered_at: now,
            returned_to_ready_at: None,
        };
        assert!(!super::result_overlay_identity_matches_phase(
            &no_match_identity,
            &offline,
            None,
        ));
    }

    #[test]
    fn no_match_keeps_high_resolution_ticks_through_both_position_springs() {
        let now = std::time::Instant::now();
        let transition = MainPhase::NoMatch {
            entered_at: now - std::time::Duration::from_millis(200),
            returned_to_ready_at: None,
        };
        assert!(animation_needs_frame(&transition, false));
        let composition_only = MainPhase::NoMatch {
            entered_at: now - std::time::Duration::from_millis(900),
            returned_to_ready_at: None,
        };
        assert!(!animation_needs_frame(&composition_only, false));
    }

    #[test]
    #[ignore = "requires a Windows Composition dispatcher; run explicitly to verify native property support"]
    fn composition_animates_ellipse_radius_and_sprite_stroke_without_a_window() {
        use super::*;

        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).unwrap();
            let dispatcher = CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })
            .unwrap();
            let result = (|| -> windows::core::Result<()> {
                let compositor = Compositor::new()?;
                let root = compositor.CreateContainerVisual()?;
                root.SetBorderMode(CompositionBorderMode::Soft)?;
                assert_eq!(root.BorderMode()?, CompositionBorderMode::Soft);

                let geometry: CompositionEllipseGeometry = compositor.CreateEllipseGeometry()?;
                let radius = compositor.CreateVector2KeyFrameAnimation()?;
                radius.SetDuration(TimeSpan {
                    Duration: 10_000_000,
                })?;
                radius.InsertKeyFrame(0.0, Vector2 { X: 48.0, Y: 48.0 })?;
                radius.InsertKeyFrame(1.0, Vector2 { X: 64.0, Y: 64.0 })?;
                geometry.StartAnimation(&HSTRING::from("Radius"), &radius)?;

                let brush =
                    compositor.CreateColorBrushWithColor(composition_color(255, 255, 255, 255))?;
                let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
                shape.SetStrokeBrush(&brush)?;
                let stroke = compositor.CreateScalarKeyFrameAnimation()?;
                stroke.SetDuration(TimeSpan {
                    Duration: 10_000_000,
                })?;
                stroke.InsertKeyFrame(0.0, 0.0)?;
                stroke.InsertKeyFrame(1.0, 4.0)?;
                shape.StartAnimation(&HSTRING::from("StrokeThickness"), &stroke)?;
                // Stop clocked animations before tearing down the dispatcher.
                // Leaving them active can race COM/Composition cleanup when
                // another native Composition test starts in the same process.
                shape.StopAnimation(&HSTRING::from("StrokeThickness"))?;
                geometry.StopAnimation(&HSTRING::from("Radius"))?;
                Ok(())
            })();
            drop(dispatcher);
            CoUninitialize();
            result.unwrap();
        }
    }

    #[test]
    #[ignore = "requires a Windows Composition dispatcher; run explicitly to verify NoMatch path interop"]
    fn composition_builds_the_native_no_match_icon_path() {
        use super::*;

        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).unwrap();
            let dispatcher = CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })
            .unwrap();
            let result = (|| -> windows::core::Result<()> {
                let compositor = Compositor::new()?;
                let d2d_factory: ID2D1Factory =
                    D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
                let geometry = make_no_match_icon_geometry(
                    &compositor,
                    &d2d_factory,
                    no_match_icon_rect(BUTTON_SIZE as f32),
                )?;
                let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
                let visual = compositor.CreateShapeVisual()?;
                visual.Shapes()?.Append(&shape)?;
                Ok(())
            })();
            drop(dispatcher);
            CoUninitialize();
            result.unwrap();
        }
    }

    #[test]
    #[ignore = "requires a Windows Composition dispatcher; run explicitly to verify group clip interop"]
    fn composition_inner_ripple_group_accepts_the_inverse_circle_clip() {
        use super::*;

        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).unwrap();
            let dispatcher = CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })
            .unwrap();
            let result = (|| -> windows::core::Result<()> {
                let compositor = Compositor::new()?;
                let d2d_factory: ID2D1Factory =
                    D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
                let clip = make_inverse_circle_clip(
                    &compositor,
                    &d2d_factory,
                    INNER_MASK_BOUNDS,
                    BUTTON_CLEAR_DIAMETER,
                )?;
                assert_eq!(
                    inverse_circle_clip_radii(INNER_MASK_BOUNDS, BUTTON_CLEAR_DIAMETER),
                    Some((168.0, 48.0))
                );
                let group = compositor.CreateContainerVisual()?;
                group.SetSize(Vector2 {
                    X: INNER_MASK_BOUNDS,
                    Y: INNER_MASK_BOUNDS,
                })?;
                group.SetClip(&clip)?;
                clip.SetScale(Vector2 { X: 0.75, Y: 0.75 })?;
                assert_eq!(clip.Scale()?, Vector2 { X: 0.75, Y: 0.75 });
                let ring = compositor.CreateSpriteVisual()?;
                ring.SetSize(Vector2 {
                    X: ui_model::INNER_DIAMETERS[0] as f32,
                    Y: ui_model::INNER_DIAMETERS[0] as f32,
                })?;
                group.Children()?.InsertAtTop(&ring)?;
                let mut logo = CompositionLogo::new(&d2d_factory, &compositor, Theme::Light, true)?;
                logo.start()?;
                Ok(())
            })();
            drop(dispatcher);
            CoUninitialize();
            result.unwrap();
        }
    }

    #[test]
    #[ignore = "requires a Windows Composition dispatcher; run explicitly to verify AutoShadow mask interop"]
    fn composition_auto_shadow_uses_button_viewport_for_336pt_mask() {
        use super::*;

        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).unwrap();
            let dispatcher = CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })
            .unwrap();
            let result = (|| -> windows::core::Result<()> {
                let compositor = Compositor::new()?;
                let d2d_factory: ID2D1Factory =
                    D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
                let clip = make_inverse_circle_clip_at(
                    &compositor,
                    &d2d_factory,
                    INNER_MASK_BOUNDS,
                    BUTTON_CLEAR_DIAMETER,
                    auto_shadow_host_center(),
                )?;
                clip.SetCenterPoint(auto_shadow_host_center())?;
                clip.SetScale(Vector2 { X: 0.85, Y: 0.85 })?;

                let host = compositor.CreateContainerVisual()?;
                host.SetSize(Vector2 {
                    X: AUTO_SHADOW_DISPLAY_BOUNDS,
                    Y: AUTO_SHADOW_DISPLAY_BOUNDS,
                })?;
                host.SetBorderMode(CompositionBorderMode::Soft)?;
                host.SetCenterPoint(Vector3 {
                    X: AUTO_SHADOW_DISPLAY_BOUNDS * 0.5,
                    Y: AUTO_SHADOW_DISPLAY_BOUNDS * 0.5,
                    Z: 0.0,
                })?;
                host.SetClip(&clip)?;

                let color_brush =
                    compositor.CreateColorBrushWithColor(composition_color(255, 9, 9, 9))?;
                let (source, surface, surface_brush) = make_auto_shadow_surface(
                    &compositor,
                    AUTO_SHADOW_DISPLAY_BOUNDS,
                    &color_brush,
                )?;
                let sprite = compositor.CreateSpriteVisual()?;
                sprite.SetSize(Vector2 {
                    X: AUTO_SHADOW_DISPLAY_BOUNDS,
                    Y: AUTO_SHADOW_DISPLAY_BOUNDS,
                })?;
                sprite.SetBorderMode(CompositionBorderMode::Soft)?;
                sprite.SetBrush(&surface_brush)?;
                host.Children()?.InsertAtTop(&sprite)?;

                assert_eq!(host.Size()?, Vector2 { X: 96.0, Y: 96.0 });
                assert_eq!(source.Size()?, Vector2 { X: 384.0, Y: 384.0 });
                assert_eq!(surface.SourceSize()?, Vector2 { X: 384.0, Y: 384.0 });
                assert_eq!(source.BorderMode()?, CompositionBorderMode::Soft);
                assert_eq!(surface_brush.Stretch()?, CompositionStretch::Fill);
                assert_eq!(
                    surface_brush.BitmapInterpolationMode()?,
                    CompositionBitmapInterpolationMode::Linear
                );
                assert_eq!(sprite.Size()?, Vector2 { X: 96.0, Y: 96.0 });
                assert_eq!(sprite.BorderMode()?, CompositionBorderMode::Soft);
                assert_eq!(clip.CenterPoint()?, auto_shadow_host_center());
                assert_eq!(clip.Scale()?, Vector2 { X: 0.85, Y: 0.85 });
                drop(color_brush);
                Ok(())
            })();
            drop(dispatcher);
            CoUninitialize();
            result.unwrap();
        }
    }
}
