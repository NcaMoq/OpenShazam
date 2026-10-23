//! Windows-native UI host: Win32 tray/popup and Direct2D/DirectWrite renderer.
//!
//! Win32 owns the tray, popup and input. Product recognition enters only
//! through the typed controller API and WASAPI loopback.
#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    cell::RefCell,
    collections::HashMap,
    mem::size_of,
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(test)]
use crate::windows_history_store::{PersistedTrack, RecognitionSource};
use crate::windows_update::PendingUpdateResult;
mod lifecycle;
pub(crate) use lifecycle::run;
mod global_shortcut;
use global_shortcut::{
    register_global_shortcut, set_global_shortcut_capture_after_wndproc,
    update_global_shortcut_after_wndproc,
};
mod motion_timing;
mod recognition;
mod result_overlay;
use motion_timing::*;
use recognition::{cancel_capture, set_auto_recognition_context, start_capture};
#[cfg(test)]
use result_overlay::RESULT_CARD_HOLD;
use result_overlay::{
    advance_result_card_stage, animation_needs_d2d_redraw, result_overlay_failure_matches,
    result_overlay_fallback_needs_frame, result_overlay_identity_matches_phase,
    result_overlay_intersects_header, result_panel_draw_top, result_panel_top_at,
    ResultCardAdvance, ResultCardStage, ResultOverlayContentKey, ResultOverlayIdentity,
    ResultOverlayMotionAnimation, ResultOverlayMotionKey, RESULT_CARD_TRANSITION,
    SUCCESS_SETTLE_TOTAL,
};
mod artwork_network;
mod artwork_state;
mod layered_surface;
#[cfg(test)]
use layered_surface::layered_surface_pixel_len;
use layered_surface::LayeredSurface;
mod hit_testing;
use hit_testing::{
    clear_history_hover, history_row_index_at, hovered_history_row_for_cursor, main_hit_test,
    MainHit,
};
#[cfg(test)]
use hit_testing::{point_in_rect, result_card_link_hit, visible_history_row_hit};
mod appearance;
#[cfg(test)]
use appearance::theme_from_foreground_rgb;
use appearance::{
    foreground_for_theme, history_title_color, popup_base_color, result_gradient_colors,
    theme_from_ui_settings, Theme,
};
mod renderer;
mod text_layout;
#[cfg(test)]
use artwork_state::{artwork_request_is_current, artwork_request_urls_match_track};
use artwork_state::{
    artwork_request_matches_track, finish_active_artwork_request, should_replace_cached_artwork,
    ArtworkLoadFailure, ArtworkPixels, ArtworkReady, ArtworkRequest, ArtworkRequestIdentity,
    ArtworkSource, CachedArtworkBitmap, TrackIdentity,
};
#[cfg(test)]
use renderer::measure_directwrite_text_layout;
use renderer::Renderer;

mod artwork_pipeline;
#[cfg(test)]
use artwork_pipeline::artwork_pixel_fingerprint;

mod message_payload;
mod notifications;

mod tray;
use tray::{
    add_tray_icon, apply_tray_menu_action, delete_tray_icon, is_tray_activation_event,
    restore_tray_icon, track_tray_context_menu,
};
#[cfg(test)]
use tray::{tray_menu_action, TrayMenuAction};

mod history;
#[cfg(test)]
use history::{format_history_date_label, resolve_history_timestamp};
mod history_motion;
use history_motion::{
    advance_history_scroll, history_offset_after_insert, history_scroll_easing,
    history_scroll_offset_after_wheel, history_scroll_offset_at, sample_history_scroll,
    HistoryScrollAnimation, HISTORY_SCROLL_TRANSITION,
};
#[cfg(test)]
use history_motion::{history_content_height, history_max_scroll_offset};
mod history_scroll_overlay;
use history_scroll_overlay::CompositionHistoryScrollOverlay;
mod history_sync;
use history_sync::{
    request_history_artwork_at_offset, restore_history_tracks, update_settings_history,
};
mod composition_result_overlay;
use composition_result_overlay::{
    copy_panel_pixels_into, overlay_surface_error, CompositionResultOverlay,
};
mod composition_history_overlay;
use composition_history_overlay::{composition_history_viewport_offset, CompositionHistoryOverlay};
mod no_match_icon;
use no_match_icon::{make_no_match_icon_geometry, no_match_icon_rect};
#[cfg(test)]
use no_match_icon::{NoMatchIconCommand, NO_MATCH_ICON_PATH};
mod composition_primitives;
mod composition_scene;
mod composition_status;
use composition_primitives::{
    auto_shadow_host_center, composition_auto_shadow_color, composition_button_pixel_snap_offset,
    composition_header_viewport_offset, composition_scale_for_dpi,
    create_composition_clip_from_d2d_geometry, create_popup_arrow_geometry,
    create_popup_outline_geometry, create_scalar_transition, create_scale_transition,
    create_vector2_scale_transition, make_auto_shadow_circle_visuals,
    make_auto_shadow_outline_mask, make_button_border_arc_geometry, make_inverse_circle_clip,
    make_inverse_circle_clip_at, popup_outline_center, result_header_hit_test_contains,
    result_overlay_child_offset, CompositionLogo, D2DCompositionGeometrySource,
    AUTO_SHADOW_DISPLAY_BOUNDS, BUTTON_CLEAR_DIAMETER, INNER_MASK_BOUNDS,
};
#[cfg(test)]
use composition_primitives::{
    button_border_arc_points, inverse_circle_clip_radii, result_header_visible_height,
    shz_logo_layout, shz_logo_point_in_button, SHZ_LOGO_BOTTOM_PATH, SHZ_LOGO_CANVAS_PADDING,
    SHZ_LOGO_CANVAS_SIZE, SHZ_LOGO_SOURCE_SIZE, SHZ_LOGO_STROKE_WIDTH, SHZ_LOGO_TOP_PATH,
};
use composition_status::CompositionStatusText;
mod localization;
#[cfg(test)]
use history::HistoryTimestampSource;
use history::{refresh_history_date_labels, Track};
use localization::{
    directwrite_locale, localized_ui_text, no_match_copy, set_ui_language, timeout_copy,
    ui_language, UiLanguage, UiTextKey,
};

mod page_link;
mod preferences;
#[cfg(test)]
use page_link::valid_page_template;
use page_link::valid_shazam_page_url;
use page_link::ValidatedWebTrackUrl;
#[cfg(test)]
use page_link::{resolve_page_link, resolve_page_link_checked, PageLinkFailure};
mod track_navigation;
#[cfg(test)]
use track_navigation::resolve_page_link_for_generation;
use track_navigation::{open_track_page, page_url_for_display_id, resolve_track_page_url};

use preferences::{
    save_auto_update_preference, save_theme_preference, save_ui_language_preference,
    ThemePreference,
};
mod single_instance;

mod update;
use update::{
    advance_pending_update_install, begin_update_check, begin_update_download,
    cancel_and_join_update_workers, cancel_pending_automatic_update_install,
    drain_update_worker_completions, handle_update_install_confirmation,
    resume_auto_shazam_after_update, set_update_status, UpdateInstallConfirmationPayload,
    UpdateStatus,
};
mod status_text;
use status_text::{
    status_text_linear_progress, status_text_progress, StatusCycle, StatusTextAlternator,
};
mod window_proc;
#[cfg(test)]
use window_proc::window_proc;
use window_proc::WindowDataCreation;
#[cfg(test)]
use window_proc::{
    take_window_data_after_create_failure, WINDOW_DATA_CREATION_ATTACHED,
    WINDOW_DATA_CREATION_DESTROYED, WINDOW_DATA_CREATION_UNATTACHED,
};

use windows::{
    core::{Error, Interface, PCWSTR},
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Direct2D::{
                Common::{
                    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
                    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_PIXEL_FORMAT,
                    D2D_RECT_F, D2D_SIZE_U,
                },
                ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory, ID2D1Geometry,
                ID2D1HwndRenderTarget, ID2D1Layer, ID2D1LinearGradientBrush, ID2D1RenderTarget,
                ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
                D2D1_COMBINE_MODE_UNION, D2D1_HWND_RENDER_TARGET_PROPERTIES,
                D2D1_LAYER_OPTIONS_NONE, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
                D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
                D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
            },
            DirectWrite::{
                IDWriteFactory, IDWriteTextFormat, DWRITE_FONT_STRETCH_NORMAL,
                DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_WORD_WRAPPING_WRAP,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::{
                BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromRect,
                ScreenToClient, MONITORINFO, MONITOR_DEFAULTTONEAREST,
            },
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{
                GetCapture, GetDoubleClickTime, ReleaseCapture, SetCapture, TrackMouseEvent,
                TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE,
            },
            Shell::{
                Shell_NotifyIconGetRect, NIN_BALLOONHIDE, NIN_BALLOONTIMEOUT, NIN_BALLOONUSERCLICK,
                NOTIFYICONIDENTIFIER,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyIcon, GetClientRect, GetCursorPos,
                GetWindowLongPtrW, KillTimer, LoadIconW, PostMessageW, PostQuitMessage, SetTimer,
                SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW, CW_USEDEFAULT,
                GWLP_USERDATA, HICON, IDI_APPLICATION, SPI_GETWHEELSCROLLLINES, SWP_NOACTIVATE,
                SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNORMAL,
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WM_ACTIVATE, WM_ACTIVATEAPP, WM_APP,
                WM_CAPTURECHANGED, WM_CLOSE, WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_HOTKEY,
                WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
                WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_MOVE, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCCREATE,
                WM_NCDESTROY, WM_PAINT, WM_RBUTTONUP, WM_SETFOCUS, WM_SETTINGCHANGE, WM_SHOWWINDOW,
                WM_SIZE, WM_THEMECHANGED, WM_TIMECHANGE, WM_TIMER, WM_WINDOWPOSCHANGED,
                WM_WINDOWPOSCHANGING, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
            },
        },
    },
    UI::{
        Color,
        Composition::{
            AnimationIterationBehavior, CompositionBorderMode, CompositionColorBrush,
            CompositionEllipseGeometry, CompositionGeometricClip, CompositionSpriteShape,
            CompositionSurfaceBrush, CompositionVisualSurface, Compositor, ContainerVisual,
            CubicBezierEasingFunction, Desktop::DesktopWindowTarget, DropShadow, InsetClip,
            ScalarKeyFrameAnimation, ShapeVisual, SpriteVisual, Vector2KeyFrameAnimation,
            Vector3KeyFrameAnimation,
        },
    },
};

#[cfg(test)]
use windows::core::HSTRING;

#[cfg(test)]
use windows::Foundation::TimeSpan;
#[cfg(test)]
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
#[cfg(test)]
use windows::Win32::{
    Graphics::{
        Direct2D::{D2D1CreateFactory, D2D1_FACTORY_TYPE_SINGLE_THREADED},
        DirectWrite::{DWriteCreateFactory, DWRITE_FACTORY_TYPE_SHARED},
    },
    System::{
        Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED},
        WinRT::{
            CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_STA,
            DQTYPE_THREAD_CURRENT,
        },
    },
    UI::WindowsAndMessaging::DestroyWindow,
};
#[cfg(test)]
use windows::UI::Composition::CompositionStretch;
#[cfg(test)]
use windows_numerics::Vector3;

use windows_numerics::{Matrix3x2, Vector2};

use openshazam_controller::{RecognitionContext, UiConfigSnapshot};

use crate::ui_model::{
    self, CubicBezier, TaggingMode, UiLevels, BUTTON_SIZE, BUTTON_TOP, BUTTON_X, CONTENT_HEIGHT,
    CONTENT_WIDTH, HISTORY_HEIGHT, POPUP_ARROW_HEIGHT, TOP_HEIGHT,
};

const CLASS_NAME: &str = "OpenShazamNativeUiWindow";
const WM_TRAYICON_MESSAGE: u32 = WM_APP + 0x41;
const WM_SHOW_INITIAL_POPUP: u32 = WM_APP + 0x43;
const WM_OS_THEME_CHANGED: u32 = WM_APP + 0x44;
const WM_ARTWORK_READY: u32 = WM_APP + 0x45;
const WM_ACTIVATE_MAIN_POPUP: u32 = WM_APP + 0x46;
const WM_OPEN_SETTINGS: u32 = WM_APP + 0x47;
const WM_APPEARANCE_CHANGED: u32 = WM_APP + 0x48;
const WM_ARTWORK_LOAD_FINISHED: u32 = WM_APP + 0x49;
const WM_GLOBAL_SHORTCUT_CHANGED: u32 = WM_APP + 0x4a;
const WM_GLOBAL_SHORTCUT_CAPTURE: u32 = WM_APP + 0x4b;
const WM_LANGUAGE_CHANGED: u32 = WM_APP + 0x4c;
const WM_UPDATE_AUTO_CHANGED: u32 = WM_APP + 0x4f;
const WM_UPDATE_ACTION: u32 = WM_APP + 0x50;
const WM_UPDATE_INSTALL_CONFIRM: u32 = WM_APP + 0x53;
const WM_APPLY_WINDOW_VISIBILITY: u32 = WM_APP + 0x54;
const WM_APPLY_POPUP_POSITION: u32 = WM_APP + 0x55;
const WM_REFRESH_RENDER_SIZE: u32 = WM_APP + 0x56;
const WM_OPEN_TRACK_PAGE: u32 = WM_APP + 0x57;
const WM_CAPTURE_LOST: u32 = WM_APP + 0x58;
const WM_UPDATE_WORKER_COMPLETED: u32 = WM_APP + 0x59;
const GLOBAL_SHORTCUT_ID_PRIMARY: i32 = 0x5301;
const GLOBAL_SHORTCUT_ID_SECONDARY: i32 = 0x5302;
const WM_MOUSELEAVE: u32 = 0x02A3;
const TRAY_ICON_ID: u32 = 1;
const APP_ICON_RESOURCE_ID: usize = 101;
const APP_USER_MODEL_ID: &str = "OpenShazam.Desktop";
// The Composition card is rasterized into its own unmasked premultiplied
// surface, then inherits the popup's antialiased root clip exactly once.
const COMPOSITION_RESULT_CARD_ENABLED: bool = true;
const TRAY_ICON_GUID: windows::core::GUID = windows::core::GUID {
    data1: 0x6a3d77b1,
    data2: 0x8e9c,
    data3: 0x4e7d,
    data4: [0x92, 0x01, 0x5d, 0x34, 0xc1, 0x7a, 0x0f, 0x62],
};
const TRAY_TASKBAR_CREATED_NAME: &str = "TaskbarCreated";
const TRAY_MENU_SETTINGS: usize = 1;
const TRAY_MENU_EXIT: usize = 2;

const POPUP_ACTIVATION_GRACE: Duration = Duration::from_millis(350);
const TIMER_SINGLE_CLICK: usize = 0x4f53;
const TIMER_LONG_PRESS: usize = 0x4f54;
const TIMER_PHASE: usize = 0x4f55;
const TIMER_ANIMATION: usize = 0x4f56;
const TIMER_DEFERRED_DEACTIVATION_HIDE: usize = 0x4f57;
const TIMER_HISTORY_SCROLL_SETTLE: usize = 0x4f59;
const TIMER_UPDATE_CHECK: usize = 0x4f5a;
const TIMER_UPDATE_COMPLETION: usize = 0x4f5b;
const UPDATE_CHECK_INTERVAL_MS: u32 = 24 * 60 * 60 * 1000;
const UPDATE_COMPLETION_POLL_MS: u32 = 250;
const SINGLE_CLICK_TIMEOUT_MS: u32 = 280;
const LONG_PRESS_TIMEOUT_MS: u32 = 500;
const DEACTIVATION_HIDE_DELAY_MS: u32 = 250;
const PHASE_TICK_MS: u32 = 100;
const ANIMATION_TICK_MS: u32 = 16;
const AUTO_START_SPRING_DURATION: Duration = Duration::from_millis(200);
const AUTO_CANDIDATE_DEDUPE_LIMIT: usize = 40;
const MAIN_WINDOW_WIDTH: f64 = CONTENT_WIDTH;
const MAIN_WINDOW_HEIGHT: f64 = CONTENT_HEIGHT + POPUP_ARROW_HEIGHT;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowKind {
    Main,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrowEdge {
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureMode {
    Manual,
    Auto,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HistoryCompositionKey {
    started_at: Instant,
    scroll_offset_bits: u32,
    hovered_row: Option<usize>,
    theme_generation: u64,
    edge: ArrowEdge,
}

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

mod app_state;
use app_state::AppState;

#[derive(Clone, Copy)]
struct PopupWindowPlacement {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Clone, Copy, Debug)]
struct ResultDetailsRects {
    title: D2D_RECT_F,
    artist: D2D_RECT_F,
}

#[derive(Clone, Copy, Debug)]
struct HistoryTextRects {
    title: D2D_RECT_F,
    artist: D2D_RECT_F,
    date: D2D_RECT_F,
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

fn ui_icon_row_stride(width: u32, height: u32, pixel_len: usize) -> Option<u32> {
    if width == 0 || height == 0 {
        return None;
    }
    let row_stride = width.checked_mul(4)?;
    let expected_len = (row_stride as usize).checked_mul(height as usize)?;
    (pixel_len == expected_len).then_some(row_stride)
}

fn create_ui_icon_bitmap(
    target: &ID2D1RenderTarget,
    pixels: &[u8],
    width: u32,
    height: u32,
) -> windows::core::Result<ID2D1Bitmap> {
    let pitch = ui_icon_row_stride(width, height, pixels.len()).ok_or_else(|| {
        Error::new(
            windows::core::HRESULT(0x80070057_u32 as i32),
            "UI icon pixel data does not match its dimensions",
        )
    })?;
    let properties = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        // Source pixels are the 2x renditions; destination coordinates stay in DIPs.
        dpiX: 192.0,
        dpiY: 192.0,
    };
    // SAFETY: `ui_icon_row_stride` requires an exact width × height × 4-byte
    // buffer, and `pixels` remains borrowed for the duration of this call.
    unsafe {
        target.CreateBitmap(
            D2D_SIZE_U { width, height },
            Some(pixels.as_ptr().cast()),
            pitch,
            &properties,
        )
    }
}

fn bgra_buffer_layout(width: u32, height: u32) -> Option<(u32, usize)> {
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return None;
    }
    let row_stride = width.checked_mul(4)?;
    let byte_len = (row_stride as usize).checked_mul(height as usize)?;
    (byte_len <= isize::MAX as usize).then_some((row_stride, byte_len))
}

fn composition_pixel_size(dip: f32, dpi: u32) -> Option<u32> {
    let pixels = (dip * composition_scale_for_dpi(dpi)).ceil();
    if !pixels.is_finite() || pixels < 1.0 || f64::from(pixels) > f64::from(i32::MAX) {
        return None;
    }
    Some(pixels as u32)
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
    HistoryScrollOverlay,
    HistoryOverlay,
    ResultOverlay,
}

fn composition_scene_layer_order() -> [CompositionSceneLayer; 4] {
    [
        CompositionSceneLayer::Tagging,
        CompositionSceneLayer::HistoryScrollOverlay,
        CompositionSceneLayer::HistoryOverlay,
        CompositionSceneLayer::ResultOverlay,
    ]
}

const STATUS_TEXT_TOP: f32 = 141.0;
const STATUS_TEXT_HEIGHT: f32 = 19.0;
const STATUS_TEXT_TRANSITION_DURATION: Duration = Duration::from_millis(400);

fn should_start_startup_logo(already_started: bool) -> bool {
    !already_started
}

fn composition_color(a: u8, r: u8, g: u8, b: u8) -> Color {
    Color {
        A: a,
        R: r,
        G: g,
        B: b,
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

fn result_details_rects(
    panel_top: f32,
    title_height: f32,
    artist_height: f32,
) -> ResultDetailsRects {
    let total_height = title_height + RESULT_DETAILS_TITLE_ARTIST_GAP + artist_height;
    let title_top = panel_top + RESULT_DETAILS_CENTER_Y - total_height * 0.5;
    let title_bottom = title_top + title_height;
    let artist_top = title_bottom + RESULT_DETAILS_TITLE_ARTIST_GAP;
    let artist_bottom = artist_top + artist_height;
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

/// # Safety
/// `app` must point to the live UI-thread AppState that owns the HWND until its
/// `WM_NCDESTROY` callback completes.
#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn create_window(
    app: *mut AppState,
    kind: WindowKind,
    title: &str,
    width: i32,
    height: i32,
    visible: bool,
) -> windows::core::Result<HWND> {
    // SAFETY: Querying the current process executable module requires no
    // external pointer and is valid throughout application startup.
    let instance = unsafe { GetModuleHandleW(None) }?;
    let class_name = wide(CLASS_NAME);
    let title = wide(title);
    // SAFETY: The caller guarantees the live AppState pointer in the function's
    // contract, and this field is copied before CreateWindowExW can re-enter.
    let generation = unsafe { (*app).message_generation };
    // SAFETY: The caller holds the AppState allocation until all owned HWNDs
    // have received WM_NCDESTROY during shutdown.
    let window_data = unsafe { WindowDataCreation::new(app, kind, generation) };
    let mut style = WS_POPUP;
    if visible {
        style |= WS_VISIBLE;
    }
    let extended =
        WS_EX_TOOLWINDOW | WS_EX_LAYERED | windows::Win32::UI::WindowsAndMessaging::WS_EX_TOPMOST;
    // SAFETY: Class/title strings and CreateWindow payload remain live for the
    // synchronous call. WindowDataCreation arbitrates WM_NCCREATE/WM_NCDESTROY
    // ownership if creation re-enters the WndProc or fails.
    let result = unsafe {
        CreateWindowExW(
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
            Some(window_data.create_param()),
        )
    };
    window_data.finish(result)
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn defer_window_visibility(hwnd: HWND, show: bool) {
    // SAFETY: The caller supplies an app-owned live HWND; posting defers the
    // visibility change and cannot synchronously re-enter this callback.
    if unsafe {
        PostMessageW(
            Some(hwnd),
            WM_APPLY_WINDOW_VISIBILITY,
            WPARAM(usize::from(show)),
            LPARAM(0),
        )
    }
    .is_err()
    {
        eprintln!("OpenShazam could not schedule a window visibility change.");
    }
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn anchor_popup(app: &mut AppState) -> PopupWindowPlacement {
    // SAFETY: This helper is called on the UI thread with the app-owned live
    // main HWND. The shell/monitor APIs below are synchronous queries only;
    // they do not pump messages or retain any pointers passed into them.
    unsafe {
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
        let (x, y, edge, anchor_x) = if let Some(tray_rect) = tray {
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
            let raw_x = tray_center_x - width / 2;
            let raw_y = if below {
                tray_rect.bottom
            } else {
                tray_rect.top - height
            };
            (
                raw_x.clamp(
                    work_area.left,
                    (work_area.right - width).max(work_area.left),
                ),
                raw_y.clamp(
                    work_area.top,
                    (work_area.bottom - height).max(work_area.top),
                ),
                if below {
                    ArrowEdge::Top
                } else {
                    ArrowEdge::Bottom
                },
                tray_center_x,
            )
        } else {
            // Some shell versions cannot provide the rectangle of an overflow
            // notification icon. Anchor to the click point and its monitor.
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
            let raw_x = cursor.x - width / 2;
            let raw_y = if below { cursor.y } else { cursor.y - height };
            (
                raw_x.clamp(
                    work_area.left,
                    (work_area.right - width).max(work_area.left),
                ),
                raw_y.clamp(
                    work_area.top,
                    (work_area.bottom - height).max(work_area.top),
                ),
                if below {
                    ArrowEdge::Top
                } else {
                    ArrowEdge::Bottom
                },
                cursor.x,
            )
        };
        app.arrow_edge = edge;
        app.arrow_center = ((anchor_x - x) as f64 / scale).clamp(14.0, CONTENT_WIDTH - 14.0) as f32;
        app.visible = true;
        let _ = InvalidateRect(Some(hwnd), None, false);
        PopupWindowPlacement {
            x,
            y,
            width,
            height,
        }
    }
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn open_settings(app: &mut AppState) {
    // Do not initialize or show WinUI from inside the popup's WndProc. Showing
    // a top-level window synchronously sends activation messages to its owner;
    // defer it until the current callback has returned to avoid re-entering
    // this function while it holds &mut AppState.
    // SAFETY: `main_hwnd` is owned by the live AppState; the actual WinUI call
    // is deferred until this callback and mutable borrow have ended.
    if unsafe { PostMessageW(Some(app.main_hwnd), WM_OPEN_SETTINGS, WPARAM(0), LPARAM(0)) }.is_err()
    {
        app.message = Some(format!(
            "Could not schedule Settings: {}",
            Error::from_thread()
        ));
    }
}

/// # Safety
/// `app_ptr` must point to the live AppState for the currently-dispatching main
/// window and remain allocated until this synchronous WinUI call returns.
#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn open_settings_after_wndproc(app_ptr: *mut AppState) -> LRESULT {
    if app_ptr.is_null() {
        return LRESULT(0);
    }

    // SAFETY: The WndProc owner keeps AppState allocated through this call.
    // The mutable-reference scope below ends before `show` enters WinUI and can
    // synchronously re-enter WndProc; after it returns, the pointer remains
    // valid because AppState teardown waits for all owned HWNDs to be destroyed.
    unsafe {
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
        let (show, owner, theme_preference, shortcut, language) = {
            let app = &mut *app_ptr;
            let Some(host) = app.settings_host.as_ref() else {
                app.settings_opening = false;
                return LRESULT(0);
            };
            (
                host.show_callback(),
                app.main_hwnd,
                app.theme_preference.id(),
                app.global_shortcut,
                ui_language().id(),
            )
        };

        let product_version: Vec<u16> = env!("CARGO_PKG_VERSION")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let hr = show(
            owner.0,
            theme_preference,
            shortcut.modifiers,
            shortcut.key,
            language,
            product_version.as_ptr(),
        );
        (*app_ptr).settings_opening = false;
        if hr < 0 {
            (*app_ptr).message = Some(format!(
                "WinUI 3 Settings failed to open (HRESULT 0x{:08X})",
                hr as u32
            ));
            let _ = InvalidateRect(Some(owner), None, false);
        } else {
            // Show initializes the WinUI island on the first request; send the
            // current in-memory history after that initialization has completed.
            update_settings_history(&*app_ptr);
            if let Some(host) = (*app_ptr).settings_host.as_ref() {
                host.update_status(
                    (*app_ptr).update_auto_enabled,
                    (*app_ptr).update_status.id(),
                    &(*app_ptr).update_version,
                    &(*app_ptr).update_detail,
                    (*app_ptr).update_last_checked,
                );
            }
            // Settings takes focus without going through the popup's ordinary
            // deactivation path, so dismiss the main popup explicitly on success.
            hide_main(&mut *app_ptr);
        }
        LRESULT(0)
    }
}

fn hide_main(app: &mut AppState) {
    if !app.visible && app.popup_opacity == 0 {
        return;
    }
    unsafe { cancel_deferred_deactivation_hide(app) };
    if let Some(animation) = app.history_scroll_animation.take() {
        app.scroll_offset = history_scroll_offset_at(animation, Instant::now()).0;
    }
    app.history_scroll_composition_ready = false;
    app.popup_opacity = 0;
    app.visible = false;
    clear_history_hover(&mut app.hovered_row);
    app.mouse_leave_tracking = false;
    app.main_activated = false;
    app.popup_activation_grace_until = None;
    set_auto_recognition_context(app, RecognitionContext::Background);
    unsafe {
        let _ = KillTimer(Some(app.main_hwnd), TIMER_HISTORY_SCROLL_SETTLE);
        sync_main_timers(app);
        if !app.main_hwnd.0.is_null() {
            defer_window_visibility(app.main_hwnd, false);
        }
    }
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

#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn update_visible_windows(app: &mut AppState) {
    // SAFETY: AppState owns its timer HWNDs and the caller runs on the UI thread.
    unsafe { sync_main_timers(app) };
    if app.visible {
        // SAFETY: The main HWND belongs to this live AppState; invalidation only
        // schedules painting and does not dispatch WM_PAINT synchronously.
        let _ = unsafe { InvalidateRect(Some(app.main_hwnd), None, false) };
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

fn animation_snapshot(app: &AppState) -> composition_scene::AnimationSnapshot {
    let now = Instant::now();
    let (mode, show_button, show_ripples) = phase_animation_state(&app.phase);
    composition_scene::AnimationSnapshot {
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

#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn sync_main_timers(app: &mut AppState) {
    // SAFETY: Every timer operation targets the live main HWND owned by app.
    // SetTimer/KillTimer update queue state and do not synchronously dispatch.
    unsafe {
        let hwnd = app.main_hwnd;
        if !hwnd.0.is_null() && app.history_scroll_animation.is_none() {
            let _ = KillTimer(Some(hwnd), TIMER_HISTORY_SCROLL_SETTLE);
        }
        if app.visible || app.recognition_session.is_some() && !hwnd.0.is_null() {
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
            let content_animation_needs_frame =
                animation_needs_frame(&app.phase, app.startup_animation_pending)
                    || (app.history_scroll_animation.is_some()
                        && !app.history_scroll_composition_ready)
                    || result_overlay_fallback_needs_frame(
                        &app.phase,
                        app.result_overlay_composition_ready,
                    )
                    || (!app.status_text_composition_ready
                        && app
                            .status_alternator
                            .is_some_and(|status| status.animating(Instant::now())))
                    || history_transition_requires_d2d_frame(
                        app.history_transition,
                        Instant::now(),
                        app.history_composition_ready,
                    );
            let animate = animation_timer_needed(app.visible, content_animation_needs_frame);
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

fn animation_needs_frame(phase: &MainPhase, startup_animation_pending: bool) -> bool {
    startup_animation_pending || phase_needs_frame(phase)
}

fn animation_timer_needed(visible: bool, content_animation_needs_frame: bool) -> bool {
    visible && content_animation_needs_frame
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn show_main(app: &mut AppState, mut renderer: Option<&mut Renderer>) {
    // SAFETY: The main HWND is owned by app; this helper only cancels its timer.
    unsafe { cancel_deferred_deactivation_hide(app) };
    refresh_os_theme(app);
    let now = Instant::now();
    // Showing/positioning the popup can produce focus notifications before
    // the deferred SetForegroundWindow call has completed. Do not interpret
    // that activation handoff as an outside click that should close the UI.
    app.main_activated = false;
    app.popup_activation_grace_until = Some(now + POPUP_ACTIVATION_GRACE);
    app.popup_opacity = 255;
    app.visible = true;
    set_auto_recognition_context(app, RecognitionContext::Foreground);
    if let Some(renderer) = renderer.as_deref_mut() {
        if let Err(error) = renderer.set_popup_opacity(255) {
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
    // SAFETY: app owns a live main HWND; anchor_popup only queries tray/monitor
    // geometry and does not dispatch messages.
    let placement = unsafe { anchor_popup(app) };
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
                app.status_text_composition_ready = renderer.status_text_composition_enabled();
            }
            Err(error) => {
                app.status_text_composition_ready = false;
                app.message = Some(format!("Composition初期配置に失敗: {error}"));
            }
        }
    }
    // Queue the positioning/show operation after this state borrow has ended.
    // SetWindowPos synchronously sends window-position and size messages back
    // to the owner WndProc.
    if let Err(error) = message_payload::post(
        app.main_hwnd,
        app.message_generation,
        WM_APPLY_POPUP_POSITION,
        placement,
    ) {
        app.visible = false;
        set_auto_recognition_context(app, RecognitionContext::Background);
        app.message = Some(format!("Could not schedule the Shazam window: {error}"));
        // SAFETY: Timer synchronization targets HWNDs owned by this AppState.
        unsafe { sync_main_timers(app) };
        return;
    }
    // Explorer may restore focus to the notification area as the tray
    // callback returns. Defer activation until the callback has unwound.
    // SAFETY: Queue activation after this function returns so foreground
    // handling cannot re-enter while the AppState borrow is active.
    let _ = unsafe {
        PostMessageW(
            Some(app.main_hwnd),
            WM_ACTIVATE_MAIN_POPUP,
            WPARAM(0),
            LPARAM(0),
        )
    };
    // SAFETY: AppState owns the HWNDs and these operations only arm timers and
    // schedule asynchronous repaint work.
    unsafe { update_visible_windows(app) };
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn cancel_deferred_deactivation_hide(app: &mut AppState) {
    if app.deactivation_hide_pending {
        // SAFETY: The main HWND is owned by the live AppState; KillTimer only
        // removes a queued timer and does not dispatch window messages.
        let _ = unsafe { KillTimer(Some(app.main_hwnd), TIMER_DEFERRED_DEACTIVATION_HIDE) };
        app.deactivation_hide_pending = false;
    }
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn defer_or_hide_main_after_deactivation(app: &mut AppState) {
    if !should_hide_main_after_deactivation(app, Instant::now()) {
        return;
    }
    // SAFETY: The queried HWND is the app-owned main window on this UI thread.
    let cursor_over_tray = unsafe { cursor_over_tray_icon(app.main_hwnd) };
    if !should_defer_hide_for_tray_activation(cursor_over_tray) {
        hide_main(app);
        return;
    }
    if app.deactivation_hide_pending {
        return;
    }
    app.deactivation_hide_pending = true;
    // SAFETY: SetTimer targets the live app-owned main HWND and does not invoke
    // the timer callback synchronously.
    if unsafe {
        SetTimer(
            Some(app.main_hwnd),
            TIMER_DEFERRED_DEACTIVATION_HIDE,
            DEACTIVATION_HIDE_DELAY_MS,
            None,
        )
    } == 0
    {
        app.deactivation_hide_pending = false;
        hide_main(app);
    }
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn cursor_over_tray_icon(hwnd: HWND) -> Option<bool> {
    let mut cursor = POINT::default();
    // SAFETY: `cursor` is a writable stack value and the caller provides a live
    // app-owned HWND for the tray identifier below.
    unsafe { GetCursorPos(&mut cursor) }.ok()?;
    let identifier = NOTIFYICONIDENTIFIER {
        cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        guidItem: TRAY_ICON_GUID,
    };
    // SAFETY: `identifier` contains the window/GUID used for this process's
    // registered tray icon and is readable for the duration of the call.
    let tray = unsafe { Shell_NotifyIconGetRect(&identifier) }.ok()?;
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

#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn process_main_hit(app: &mut AppState, hit: MainHit) {
    match hit {
        MainHit::Settings => {
            // SAFETY: AppState owns the main HWND; this helper only posts a
            // message so WinUI initialization happens after the event dispatch.
            unsafe { open_settings(app) }
        }
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
                if let Err(error) = message_payload::post(
                    app.main_hwnd,
                    app.message_generation,
                    WM_OPEN_TRACK_PAGE,
                    url,
                ) {
                    eprintln!("OpenShazam could not schedule the Shazam page: {error}");
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

#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn client_dips(hwnd: HWND, lparam: LPARAM) -> (f32, f32) {
    // SAFETY: The caller passes the live HWND currently receiving the input
    // message; GetDpiForWindow only reads its current scale.
    let scale = 96.0 / unsafe { GetDpiForWindow(hwnd) }.max(96) as f32;
    let x = (low_word(lparam.0) as i16 as f32) * scale;
    let y = (high_word(lparam.0) as i16 as f32) * scale;
    (x, y)
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn load_application_icon(instance: HINSTANCE) -> HICON {
    let resource_name = PCWSTR(APP_ICON_RESOURCE_ID as *const u16);
    // SAFETY: `instance` is the loaded executable module and `resource_name`
    // is the compile-time integer resource identifier for its icon.
    unsafe { LoadIconW(Some(instance), resource_name) }
        .or_else(|_| {
            // SAFETY: IDI_APPLICATION is the system's predefined shared icon.
            unsafe { LoadIconW(None, IDI_APPLICATION) }
        })
        .unwrap_or_default()
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

#[cfg(test)]
mod tests;
