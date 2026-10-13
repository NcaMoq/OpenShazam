//! Windows-native UI host: Win32 tray/popup and Direct2D/DirectWrite renderer.
//!
//! Audio, signature generation and recognition remain in the existing CLI
//! commands. This module only starts those existing commands and adapts their
//! output to UI state.

use std::{
    cell::RefCell,
    ffi::c_void,
    mem::size_of,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

use windows::{
    core::{implement, Error, Interface, Ref, HSTRING, PCWSTR, PWSTR},
    Foundation::TimeSpan,
    Graphics::IGeometrySource2D,
    System::DispatcherQueueController,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME},
        Graphics::{
            Direct2D::{
                Common::{
                    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BEZIER_SEGMENT,
                    D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW,
                    D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_GRADIENT_STOP,
                    D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
                },
                D2D1CreateFactory, ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory, ID2D1Geometry,
                ID2D1HwndRenderTarget, ID2D1Layer, ID2D1LinearGradientBrush, ID2D1RenderTarget,
                ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
                D2D1_COMBINE_MODE_EXCLUDE, D2D1_COMBINE_MODE_UNION, D2D1_ELLIPSE,
                D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2,
                D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LAYER_OPTIONS_NONE,
                D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE,
                D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
                D2D1_RENDER_TARGET_USAGE_NONE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
            },
            DirectWrite::{
                DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
                DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_BOLD,
                DWRITE_FONT_WEIGHT_DEMI_BOLD, DWRITE_FONT_WEIGHT_HEAVY, DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
                DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_WRAP,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::{
                BeginPaint, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint,
                GetMonitorInfoW, InvalidateRect, MonitorFromRect, SelectObject, AC_SRC_ALPHA,
                AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
                HBITMAP, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST,
            },
        },
        System::{
            Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED},
            LibraryLoader::GetModuleHandleW,
            WinRT::Composition::ICompositorDesktopInterop,
            WinRT::Graphics::Direct2D::{IGeometrySource2DInterop, IGeometrySource2DInterop_Impl},
            WinRT::{
                CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_STA,
                DQTYPE_THREAD_CURRENT,
            },
        },
        UI::{
            Controls::Dialogs::{
                GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW,
            },
            HiDpi::{
                GetDpiForWindow, SetProcessDpiAwarenessContext,
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            },
            Input::KeyboardAndMouse::{
                GetDoubleClickTime, GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_ESCAPE,
            },
            Shell::{
                Shell_NotifyIconGetRect, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE,
                NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
                NOTIFYICONDATAW, NOTIFYICONIDENTIFIER, NOTIFYICON_VERSION_4,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
                GetWindowLongPtrW, GetWindowRect, KillTimer, LoadCursorW, LoadIconW, PeekMessageW,
                PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer,
                SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, UpdateLayeredWindow,
                WaitMessage, CS_DBLCLKS, CW_USEDEFAULT, GWLP_USERDATA, IDC_ARROW, IDI_APPLICATION,
                MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE,
                SW_SHOWNORMAL, ULW_ALPHA, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_CREATE, WM_DESTROY,
                WM_DPICHANGED, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
                WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT,
                WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WNDPROC, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
                WS_POPUP, WS_VISIBLE,
            },
        },
    },
    UI::{
        Color,
        Composition::{
            AnimationIterationBehavior, CompositionColorBrush, CompositionEllipseGeometry,
            CompositionGeometricClip, CompositionPath, CompositionPathGeometry,
            CompositionSpriteShape, CompositionVisualSurface, Compositor, ContainerVisual,
            CubicBezierEasingFunction, Desktop::DesktopWindowTarget, DropShadow,
            ScalarKeyFrameAnimation, ShapeVisual, SpriteVisual, Vector2KeyFrameAnimation,
            Vector3KeyFrameAnimation,
        },
    },
};

use windows_numerics::{Matrix3x2, Vector2, Vector3};

use crate::ui_model::{
    self, CubicBezier, TaggingMode, UiLevels, BUTTON_SIZE, BUTTON_TOP, BUTTON_X, CONTENT_HEIGHT,
    CONTENT_WIDTH, HISTORY_HEIGHT, POPUP_ARROW_HEIGHT, POPUP_CORNER_RADIUS, TOP_HEIGHT,
};

const CLASS_NAME: &str = "OpenShazamNativeUiWindow";
const WM_TRAYICON_MESSAGE: u32 = WM_APP + 0x41;
const WM_CAPTURE_FINISHED: u32 = WM_APP + 0x42;
const WM_SHOW_INITIAL_POPUP: u32 = WM_APP + 0x43;
const TRAY_ICON_ID: u32 = 1;
const TIMER_SINGLE_CLICK: usize = 0x4f53;
const TIMER_LONG_PRESS: usize = 0x4f54;
const TIMER_PHASE: usize = 0x4f55;
const TIMER_ANIMATION: usize = 0x4f56;
const SINGLE_CLICK_TIMEOUT_MS: u32 = 280;
const LONG_PRESS_TIMEOUT_MS: u32 = 500;
const PHASE_TICK_MS: u32 = 100;
const ANIMATION_TICK_MS: u32 = 8;
const MAIN_WINDOW_WIDTH: f64 = CONTENT_WIDTH;
const MAIN_WINDOW_HEIGHT: f64 = CONTENT_HEIGHT + POPUP_ARROW_HEIGHT;
const SETTINGS_WIDTH: f64 = 550.0;
const SETTINGS_HEIGHT: f64 = 400.0;
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

fn native_icon_layout(body_top: f32) -> NativeIconLayout {
    NativeIconLayout {
        settings_visual: [349.0, body_top + 25.0, 365.0, body_top + 41.0],
        // The popup's stored 30x28 frame is constrained to the 16x16 gear.
        settings_hit: [349.0, body_top + 25.0, 365.0, body_top + 41.0],
        cancel_visual: [351.0, body_top + 27.0, 363.0, body_top + 39.0],
        cancel_hit: [351.0, body_top + 27.0, 363.0, body_top + 39.0],
        result_close_visual: [353.0, body_top + 25.0, 365.0, body_top + 37.0],
        result_close_hit: [353.0, body_top + 25.0, 365.0, body_top + 37.0],
    }
}

fn point_in_rect(point_x: f32, point_y: f32, bounds: [f32; 4]) -> bool {
    (bounds[0]..=bounds[2]).contains(&point_x) && (bounds[1]..=bounds[3]).contains(&point_y)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowKind {
    Main,
    Settings,
    About,
    MicrophoneInfo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Theme {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrowEdge {
    Top,
    Bottom,
}

#[derive(Clone, Debug)]
struct Track {
    title: String,
    artist: String,
    recognized_at: Instant,
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
// [I/U] Native NSTableView slide duration was not recovered. Keep this
// implementation value replaceable rather than presenting it as native data.
const HISTORY_INSERT_TRANSITION: Duration = Duration::from_millis(250);

#[derive(Debug)]
enum MainPhase {
    Ready,
    Listening {
        mode: CaptureMode,
        started_at: Instant,
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

#[derive(Debug)]
struct CaptureFinished {
    captured_mode: CaptureMode,
    result: Result<Output, String>,
    canceled: bool,
}

struct AppState {
    main_hwnd: HWND,
    settings_hwnd: HWND,
    about_hwnd: HWND,
    microphone_hwnd: HWND,
    d2d_factory: ID2D1Factory,
    dwrite_factory: IDWriteFactory,
    compositor: Compositor,
    _dispatcher_queue: DispatcherQueueController,
    phase: MainPhase,
    history: Vec<Track>,
    history_transition: Option<HistoryMutationTransition>,
    scroll_offset: f32,
    hovered_row: Option<usize>,
    // UI-only adapter points. No service meter output is assumed or inferred.
    ui_levels: UiLevels,
    // Core Animation's Auto spring parameters are unresolved in the source;
    // leave the transform neutral until a measured profile is supplied.
    auto_spring: Option<ui_model::SpringProfile>,
    theme: Theme,
    theme_generation: u64,
    visible: bool,
    main_activated: bool,
    phase_timer_active: bool,
    animation_timer_active: bool,
    startup_animation_pending: bool,
    startup_logo_consumed: bool,
    settings_visible: bool,
    about_visible: bool,
    microphone_visible: bool,
    arrow_edge: ArrowEdge,
    arrow_center: f32,
    mouse_down_center: bool,
    long_press_consumed: bool,
    single_click_pending: bool,
    capture_canceled: Arc<AtomicBool>,
    capture_child: Option<Arc<Mutex<Option<Child>>>>,
    message: Option<String>,
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
    accent: ID2D1SolidColorBrush,
    button: ID2D1SolidColorBrush,
    separator: ID2D1SolidColorBrush,
    skeleton: ID2D1SolidColorBrush,
    result_start: ID2D1LinearGradientBrush,
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
    button: IDWriteTextFormat,
    settings: IDWriteTextFormat,
    settings_small: IDWriteTextFormat,
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
    hwnd_target: Option<ID2D1HwndRenderTarget>,
    dc_target: Option<ID2D1DCRenderTarget>,
    layered_surface: Option<LayeredSurface>,
    palette: Palette,
    text: TextStyles,
    dpi: u32,
    theme_generation: u64,
    composition: Option<CompositionScene>,
    popup_layer: Option<ID2D1Layer>,
    popup_outline: RefCell<Option<(ArrowEdge, u32, ID2D1Geometry)>>,
    popup_header: RefCell<Option<(ArrowEdge, u32, ID2D1Geometry)>>,
    result_details_cache: RefCell<Option<CachedResultDetailsMetrics>>,
}

const SETTINGS_ICON_PIXELS: &[u8; 4096] = include_bytes!("../assets/settings-2x.bgra");
const WHITE_CLOSE_ICON_PIXELS: &[u8; 2304] = include_bytes!("../assets/WhiteClose-2x.bgra");

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

    unsafe fn present(&self, hwnd: HWND) -> windows::core::Result<()> {
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
            SourceConstantAlpha: 255,
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
                    for geometry in &self._long_geometries {
                        geometry.StopAnimation(&HSTRING::from("TrimStart"))?;
                        geometry.StopAnimation(&HSTRING::from("TrimEnd"))?;
                    }
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

fn no_match_icon_point(point: (f64, f64), size: f32, origin: f32) -> Vector2 {
    Vector2 {
        X: (f64::from(origin) + point.0 * f64::from(size)) as f32,
        Y: (f64::from(origin) + point.1 * f64::from(size)) as f32,
    }
}

unsafe fn make_no_match_icon_geometry(
    compositor: &Compositor,
    d2d_factory: &ID2D1Factory,
    size: f32,
) -> windows::core::Result<CompositionPathGeometry> {
    let origin = (BUTTON_SIZE as f32 - size) * 0.5;
    let source = d2d_factory.CreatePathGeometry()?;
    let sink = source.Open()?;
    for command in NO_MATCH_ICON_PATH {
        match command {
            NoMatchIconCommand::Move(point) => {
                sink.BeginFigure(
                    no_match_icon_point(point, size, origin),
                    D2D1_FIGURE_BEGIN_FILLED,
                );
            }
            NoMatchIconCommand::Line(point) => {
                sink.AddLine(no_match_icon_point(point, size, origin));
            }
            NoMatchIconCommand::Cubic(cp1, cp2, end) => {
                sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                    point1: no_match_icon_point(cp1, size, origin),
                    point2: no_match_icon_point(cp2, size, origin),
                    point3: no_match_icon_point(end, size, origin),
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
                f64::from(origin) + point.0 * f64::from(size),
                f64::from(origin) + point.1 * f64::from(size),
            );
            Vector2 {
                X: (2.0 * f64::from(center_x) - point.0 - f64::from(origin)) as f32,
                Y: (2.0 * f64::from(center_y) - point.1 - f64::from(origin)) as f32,
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

fn logo_shadow_color(_theme: Theme) -> Color {
    // Native uses the tagButtonTopBorder color. The dark appearance rendition
    // is still unmeasured; the current UI uses the confirmed light alpha/color.
    composition_color(51, 255, 255, 255)
}

const INNER_MASK_BOUNDS: f32 = 336.0;
const BUTTON_CLEAR_DIAMETER: f32 = 96.0;

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
    let (outer_radius, inner_radius) = inverse_circle_clip_radii(size, hole).ok_or_else(|| {
        Error::new(
            windows::core::HRESULT(0x80070057u32 as i32),
            "invalid ripple mask dimensions",
        )
    })?;
    let center = size * 0.5;
    let outer = d2d_factory.CreateEllipseGeometry(&D2D1_ELLIPSE {
        point: Vector2 {
            X: center,
            Y: center,
        },
        radiusX: outer_radius,
        radiusY: outer_radius,
    })?;
    let inner = d2d_factory.CreateEllipseGeometry(&D2D1_ELLIPSE {
        point: Vector2 {
            X: center,
            Y: center,
        },
        radiusX: inner_radius,
        radiusY: inner_radius,
    })?;
    let geometry = d2d_factory.CreatePathGeometry()?;
    let sink = geometry.Open()?;
    outer.CombineWithGeometry(&inner, D2D1_COMBINE_MODE_EXCLUDE, None, 0.25, &sink)?;
    sink.Close()?;

    let geometry: ID2D1Geometry = geometry.cast()?;
    let source: IGeometrySource2D = D2DCompositionGeometrySource { geometry }.into();
    let path = CompositionPath::Create(&source)?;
    let geometry = compositor.CreatePathGeometryWithPath(&path)?;
    compositor.CreateGeometricClipWithGeometry(&geometry)
}

struct CompositionScene {
    compositor: Compositor,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    inner_audio_root: ContainerVisual,
    inner_wave_root: ContainerVisual,
    inner_inverse_clip: CompositionGeometricClip,
    outer_audio_root: ContainerVisual,
    rings: [CompositionRing; 6],
    button: ContainerVisual,
    button_content: ContainerVisual,
    button_fill: CompositionColorBrush,
    logo: CompositionLogo,
    no_match_icon_transform: ContainerVisual,
    ring_brushes: [CompositionColorBrush; 3],
    idle_animation: Vector3KeyFrameAnimation,
    outer_scale_null_animation: Vector3KeyFrameAnimation,
    idle_active: bool,
    idle_started_at: Option<Instant>,
    last_button_scale: f32,
    ripples_active: bool,
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
    theme: Theme,
    body_top: f32,
    show_button: bool,
    show_ripples: bool,
    levels: UiLevels,
    auto_spring: Option<ui_model::SpringProfile>,
    no_match_elapsed: Option<f64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompositionRootLayer {
    OuterRipples,
    InnerRipples,
    Button,
}

fn composition_root_layer_order() -> [CompositionRootLayer; 3] {
    // Sequential InsertAtTop calls reproduce the native insertion sequence:
    // inner circles below the button, outer circles added last at the front.
    [
        CompositionRootLayer::InnerRipples,
        CompositionRootLayer::Button,
        CompositionRootLayer::OuterRipples,
    ]
}

fn should_start_startup_logo(kind: WindowKind, already_started: bool) -> bool {
    kind == WindowKind::Main && !already_started
}

impl CompositionScene {
    fn new(
        hwnd: HWND,
        d2d_factory: &ID2D1Factory,
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
        let clip_bottom = MAIN_WINDOW_HEIGHT as f32 - (body_top + TOP_HEIGHT as f32);
        let clip = compositor.CreateInsetClipWithInsets(0.0, 0.0, 0.0, clip_bottom.max(0.0))?;
        root.SetClip(&clip)?;

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
        // Native uses a 3.5×96pt inverse mask (336pt) around the button.
        // The largest visible inner fill remains 288pt inside that mask.
        let inner_bounds = INNER_MASK_BOUNDS;
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
        // The fill waves follow the audio-driven inner scale, while the clear
        // hole follows the button scale. Keep the mask in the unscaled parent
        // coordinate space and transform the clip independently.
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
        let no_match_icon_geometry = unsafe {
            make_no_match_icon_geometry(compositor, d2d_factory, BUTTON_SIZE as f32 * 0.8)?
        };
        let no_match_icon_shape =
            compositor.CreateSpriteShapeWithGeometry(&no_match_icon_geometry)?;
        no_match_icon_shape.SetFillBrush(&no_match_icon_brush)?;
        let no_match_icon_visual = compositor.CreateShapeVisual()?;
        let no_match_icon_size = BUTTON_SIZE as f32 * 0.8;
        let no_match_icon_inset = (BUTTON_SIZE as f32 - no_match_icon_size) * 0.5;
        no_match_icon_visual.SetSize(Vector2 {
            X: no_match_icon_size,
            Y: no_match_icon_size,
        })?;
        no_match_icon_visual.SetOffset(Vector3 {
            X: no_match_icon_inset,
            Y: no_match_icon_inset,
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
        button_content.Children()?.InsertAtTop(&logo.visual)?;
        button_content
            .Children()?
            .InsertAtTop(&no_match_icon_transform)?;
        button.Children()?.InsertAtTop(&button_content)?;

        let children = root.Children()?;
        for layer in composition_root_layer_order() {
            match layer {
                CompositionRootLayer::OuterRipples => children.InsertAtTop(&outer_audio_root)?,
                CompositionRootLayer::InnerRipples => children.InsertAtTop(&inner_audio_root)?,
                CompositionRootLayer::Button => children.InsertAtTop(&button)?,
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
            inner_audio_root,
            inner_wave_root,
            inner_inverse_clip,
            outer_audio_root,
            rings,
            button,
            button_content,
            button_fill,
            logo,
            no_match_icon_transform,
            ring_brushes,
            idle_animation,
            outer_scale_null_animation,
            idle_active: false,
            idle_started_at: None,
            last_button_scale: 1.0,
            ripples_active: false,
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
        })
    }

    fn update(&mut self, snapshot: AnimationSnapshot) -> windows::core::Result<()> {
        if self.theme != snapshot.theme {
            self.theme = snapshot.theme;
            self.logo.set_theme(self.theme)?;
            for brush in &self.ring_brushes {
                brush.SetColor(composition_tagging_color(self.theme))?;
            }
            self.button_fill
                .SetColor(composition_color(77, 255, 255, 255))?;
        }

        if self.body_top != snapshot.body_top {
            self.body_top = snapshot.body_top;
            let bottom_inset = MAIN_WINDOW_HEIGHT as f32 - (snapshot.body_top + TOP_HEIGHT as f32);
            let clip =
                self.compositor
                    .CreateInsetClipWithInsets(0.0, 0.0, 0.0, bottom_inset.max(0.0))?;
            self.root.SetClip(&clip)?;
            let inner_bounds = INNER_MASK_BOUNDS;
            let outer_bounds = ui_model::OUTER_DIAMETERS[0] as f32;
            let center_x = (BUTTON_X + BUTTON_SIZE * 0.5) as f32;
            let center_y = snapshot.body_top + (BUTTON_TOP + BUTTON_SIZE * 0.5) as f32;
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
            .is_some_and(|elapsed| elapsed >= 0.18)
            && !self.no_match_active
        {
            self.start_no_match_motion(snapshot.body_top)?;
            self.no_match_active = true;
        } else if snapshot.no_match_elapsed.is_none()
            && (self.no_match_active || self.no_match_restore_active)
        {
            self.button.StopAnimation(&HSTRING::from("Offset"))?;
            self.button.StopAnimation(&HSTRING::from("Scale"))?;
            self.button_content.StopAnimation(&HSTRING::from("Scale"))?;
            self.no_match_icon_transform
                .StopAnimation(&HSTRING::from("Offset"))?;
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
        self.button
            .SetOpacity(if snapshot.show_button { 1.0 } else { 0.0 })?;

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
            for ring in &self.rings[4..] {
                ring.visual.SetOpacity(ring.base_opacity)?;
                ring.visual
                    .StartAnimation(&HSTRING::from("Scale"), &self.outer_scale_null_animation)?;
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
            self.ripples_active = true;
        } else if !snapshot.show_ripples && self.ripples_active {
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
            self.ripples_active = false;
        }

        if !snapshot.show_ripples {
            self.inner_audio_root.SetOpacity(0.0)?;
            self.inner_wave_root.SetScale(Vector3 {
                X: 1.0,
                Y: 1.0,
                Z: 1.0,
            })?;
            self.inner_inverse_clip
                .SetScale(Vector2 { X: 1.0, Y: 1.0 })?;
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
            self.outer_audio_root
                .SetOpacity(frame.outer_audio_opacity as f32)?;
        }

        let logo_finished = self.logo.advance()?;
        if snapshot.mode == TaggingMode::Ready {
            if snapshot.show_button && logo_finished && !self.idle_active {
                self.button.SetScale(Vector3 {
                    X: 1.0,
                    Y: 1.0,
                    Z: 1.0,
                })?;
                self.button
                    .StartAnimation(&HSTRING::from("Scale"), &self.idle_animation)?;
                self.idle_active = true;
                self.idle_started_at = Some(Instant::now());
            } else if (!snapshot.show_button || !logo_finished) && self.idle_active {
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
            self.button.SetScale(Vector3 {
                X: frame.button_child_scale as f32,
                Y: frame.button_child_scale as f32,
                Z: 1.0,
            })?;
            self.last_button_scale = frame.button_child_scale as f32;
        }
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

    fn start_no_match_motion(&mut self, body_top: f32) -> windows::core::Result<()> {
        let button_left = BUTTON_X as f32;
        let button_top = body_top + BUTTON_TOP as f32;
        let tagging_profile = ui_model::no_match_position_profile(
            BUTTON_SIZE,
            -0.150_000_005_960_464_48,
            8.449_999_809_265_137,
            0.5,
        );
        let icon_profile = ui_model::no_match_position_profile(
            BUTTON_SIZE,
            -0.070_000_000_298_023_22,
            3.940_000_057_220_459,
            1.100_000_023_841_858,
        );
        let tagging_position = create_no_match_position_animation(
            &self.compositor,
            button_left,
            button_top,
            tagging_profile,
        )?;
        let icon_position =
            create_no_match_position_animation(&self.compositor, 0.0, 0.0, icon_profile)?;
        self.button.SetOffset(Vector3 {
            X: button_left + tagging_profile.from as f32,
            Y: body_top + BUTTON_TOP as f32,
            Z: 0.0,
        })?;
        self.no_match_icon_transform.SetOffset(Vector3 {
            X: icon_profile.from as f32,
            Y: 0.0,
            Z: 0.0,
        })?;
        self.no_match_icon_transform.SetOpacity(1.0)?;
        self.button
            .StartAnimation(&HSTRING::from("Offset"), &tagging_position)?;
        self.no_match_icon_transform
            .StartAnimation(&HSTRING::from("Offset"), &icon_position)?;

        let ease_out = self.compositor.CreateCubicBezierEasingFunction(
            Vector2 { X: 0.0, Y: 0.0 },
            Vector2 { X: 0.58, Y: 1.0 },
        )?;
        self.button_content.SetScale(Vector3 {
            X: 1.0,
            Y: 1.0,
            Z: 1.0,
        })?;
        let scale = create_no_match_scale_animation(&self.compositor, 1.0, 0.8, 0.3, &ease_out)?;
        self.button_content
            .StartAnimation(&HSTRING::from("Scale"), &scale)
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

fn create_no_match_position_animation(
    compositor: &Compositor,
    center_x: f32,
    center_y: f32,
    profile: ui_model::SpringProfile,
) -> windows::core::Result<Vector3KeyFrameAnimation> {
    let animation = compositor.CreateVector3KeyFrameAnimation()?;
    animation.SetDuration(TimeSpan {
        Duration: (profile.duration_seconds * 10_000_000.0).round() as i64,
    })?;
    animation.SetDelayTime(TimeSpan {
        Duration: 3_000_000,
    })?;
    // Sample the analytic spring densely at creation time; Composition then
    // interpolates on the GPU between knots without an 8 ms Rust frame loop.
    for frame in 0..=256 {
        let progress = frame as f64 / 256.0;
        let offset = ui_model::spring_value(progress, profile) as f32;
        animation.InsertKeyFrame(
            progress as f32,
            Vector3 {
                X: center_x + offset,
                Y: center_y,
                Z: 0.0,
            },
        )?;
    }
    Ok(animation)
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

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

fn result_artwork_frame(panel_top: f32) -> D2D_RECT_F {
    rect(10.0, panel_top + 20.0, 160.0, panel_top + 170.0)
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
const RESULT_DETAILS_TITLE_ARTIST_GAP: f32 = 5.0;
const RESULT_DETAILS_ARTIST_ACTION_GAP: f32 = 10.0;
const RESULT_ACTION_HEIGHT: f32 = 20.0;

fn result_details_rects(
    panel_top: f32,
    title_height: f32,
    artist_height: f32,
) -> ResultDetailsRects {
    let total_height = title_height
        + RESULT_DETAILS_TITLE_ARTIST_GAP
        + artist_height
        + RESULT_DETAILS_ARTIST_ACTION_GAP
        + RESULT_ACTION_HEIGHT;
    let title_top = panel_top + RESULT_DETAILS_CENTER_Y - total_height * 0.5;
    let title_bottom = title_top + title_height;
    let artist_top = title_bottom + RESULT_DETAILS_TITLE_ARTIST_GAP;
    let artist_bottom = artist_top + artist_height;
    let action_top = artist_bottom + RESULT_DETAILS_ARTIST_ACTION_GAP;
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
        action: rect(
            RESULT_DETAILS_LEFT,
            action_top,
            RESULT_DETAILS_LEFT + 70.0,
            action_top + RESULT_ACTION_HEIGHT,
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
    rect(20.0, row_top + 12.0, 85.0, row_top + 77.0)
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
    let family = wide("Segoe UI");
    let locale = wide("ja-JP");
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
        Ok(format)
    }
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
        let palette = create_palette(&target, theme)?;
        let text = create_text_styles(&write_factory)?;
        let composition = if kind == WindowKind::Main {
            Some(CompositionScene::new(
                hwnd,
                &factory,
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
        Ok(Self {
            factory,
            write_factory,
            target,
            settings_icon,
            white_close_icon,
            hwnd_target,
            dc_target,
            layered_surface,
            palette,
            text,
            dpi,
            theme_generation,
            composition,
            popup_layer,
            popup_outline: RefCell::new(None),
            popup_header: RefCell::new(None),
            result_details_cache: RefCell::new(None),
        })
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
        if let Ok(palette) = create_palette(&self.target, app.theme) {
            self.palette = palette;
            self.theme_generation = app.theme_generation;
        }
    }

    unsafe fn refresh_theme_if_needed(&mut self, app: &AppState) {
        if self.theme_generation != app.theme_generation {
            if let Ok(palette) = create_palette(&self.target, app.theme) {
                self.palette = palette;
                self.theme_generation = app.theme_generation;
            }
        }
    }

    unsafe fn draw(&mut self, app: &AppState, kind: WindowKind) -> windows::core::Result<()> {
        self.refresh_theme_if_needed(app);
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
            WindowKind::Main => self.draw_main(app),
            WindowKind::Settings => self.draw_settings(app),
            WindowKind::About => self.draw_about(app),
            WindowKind::MicrophoneInfo => self.draw_microphone_info(app),
        };
        if popup_layer_parameters.is_some() {
            self.target.PopLayer();
        }
        draw_result?;
        self.target.EndDraw(None, None)?;
        if let Some(surface) = self.layered_surface.as_ref() {
            surface.present(app.main_hwnd)?;
        }
        Ok(())
    }

    fn update_composition(&mut self, snapshot: AnimationSnapshot) -> windows::core::Result<()> {
        if let Some(scene) = self.composition.as_mut() {
            scene.update(snapshot)?;
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
        if value.is_empty() {
            return Ok(min_height);
        }
        let content: Vec<u16> = value.encode_utf16().collect();
        let max_height = max_lines * min_height;
        let layout = self.write_factory.CreateTextLayout(
            &content,
            format,
            RESULT_DETAILS_RIGHT - RESULT_DETAILS_LEFT,
            max_height,
        )?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut metrics)?;
        Ok(metrics.height.clamp(min_height, max_height))
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

    unsafe fn draw_main(&self, app: &AppState) -> windows::core::Result<()> {
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
            MainPhase::Ready | MainPhase::Error { .. } | MainPhase::Timeout { .. } => {
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
            MainPhase::Matched { .. } | MainPhase::NoMatch { .. } | MainPhase::Offline { .. } => {
                if header_fill_scope(app.arrow_edge) == HeaderFillScope::HeaderAndArrow {
                    self.fill_gradient_geometry(
                        &self.popup_header(app)?,
                        &self.palette.result_start,
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
                        &self.palette.result_start,
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
        self.draw_history(app, history_top, now);
        self.target.PopAxisAlignedClip();

        match &app.phase {
            MainPhase::Ready => self.draw_ready(app, body_top),
            MainPhase::Listening { mode, .. } => {
                self.draw_listening_status(body_top, *mode);
                self.draw_cancel_button(body_top);
            }
            MainPhase::Matched { ref track, .. } => {
                let top =
                    result_panel_top_at(&app.phase, body_top, Instant::now()).unwrap_or(body_top);
                self.draw_result_panel(track, top)?;
            }
            MainPhase::NoMatch { .. } => {
                let top =
                    result_panel_top_at(&app.phase, body_top, Instant::now()).unwrap_or(body_top);
                self.draw_no_match_panel(top, false)?;
            }
            MainPhase::Offline { .. } => {
                let top =
                    result_panel_top_at(&app.phase, body_top, Instant::now()).unwrap_or(body_top);
                self.draw_no_match_panel(top, true)?;
            }
            MainPhase::Error { ref message, .. } => {
                self.draw_ready(app, body_top);
                self.draw_error_banner(message, body_top);
            }
            MainPhase::Timeout { .. } => self.draw_timeout_dialog(body_top),
        }
        self.draw_file_hint(body_bottom);
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
        let arrow = self.popup_arrow_geometry(app.arrow_edge, center)?;
        let header_with_arrow = self.combine_geometry(&header, &arrow, D2D1_COMBINE_MODE_UNION)?;
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

    unsafe fn draw_ready(&self, _app: &AppState, body_top: f32) {
        self.text(
            localized_ui_text(UiTextKey::Ready, ui_language()),
            rect(
                8.0,
                body_top + 141.0,
                CONTENT_WIDTH as f32 - 8.0,
                body_top + 160.0,
            ),
            &self.text.centered_title,
            &self.palette.white_ink,
        );
        self.draw_settings_icon(body_top);
    }

    unsafe fn draw_listening_status(&self, body_top: f32, mode: CaptureMode) {
        self.text(
            match mode {
                CaptureMode::Manual => localized_ui_text(UiTextKey::Listening, ui_language()),
                CaptureMode::Auto => "Auto Shazam",
            },
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

    unsafe fn draw_cancel_button(&self, body_top: f32) {
        let [left, top, right, bottom] = native_icon_layout(body_top).cancel_visual;
        let destination = rect(left, top, right, bottom);
        self.target.DrawBitmap(
            &self.white_close_icon,
            Some(&destination),
            1.0,
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            None,
        );
    }

    unsafe fn draw_settings_icon(&self, body_top: f32) {
        let [left, top, right, bottom] = native_icon_layout(body_top).settings_visual;
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
        let rows = [0.0f32, 93.0, 187.0, 281.0, 375.0];
        for top in rows {
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

    unsafe fn draw_history(&self, app: &AppState, history_top: f32, now: Instant) {
        for (index, track) in app.history.iter().enumerate() {
            let top = history_row_top(
                index,
                app.history_transition,
                history_top,
                app.scroll_offset,
                now,
            );
            if top + 89.0 < history_top || top > history_top + HISTORY_HEIGHT as f32 {
                continue;
            }
            let row = rect(0.0, top, CONTENT_WIDTH as f32, top + 89.0);
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
                .FillRoundedRectangle(&artwork, &self.palette.result_start);
            // The existing recognition UI contract has no artwork payload; this glyph is a
            // placeholder, and the album-art view must not inherit UI-144 button padding.
            self.text(
                "♫",
                history_artwork_frame(top),
                &self.text.centered_title,
                &self.palette.white_ink,
            );
            let title_color = if app.theme == Theme::Light {
                &self.palette.title
            } else {
                &self.palette.white
            };
            self.text(
                &track.title,
                rect(95.0, top + 8.0, 360.0, top + 38.0),
                &self.text.title,
                title_color,
            );
            self.text(
                &track.artist,
                rect(95.0, top + 38.0, 360.0, top + 63.0),
                &self.text.subtitle,
                &self.palette.subtitle,
            );
            self.text(
                "たった今",
                rect(95.0, top + 63.0, 360.0, top + 84.0),
                &self.text.date,
                &self.palette.subtitle,
            );
            self.fill_rect(
                rect(10.0, top + 88.0, 370.0, top + 89.0),
                &self.palette.separator,
            );
            let _recognized_age = track.recognized_at.elapsed();
        }
    }

    unsafe fn draw_result_panel(&self, track: &Track, top: f32) -> windows::core::Result<()> {
        let panel = rect(0.0, top, CONTENT_WIDTH as f32, top + TOP_HEIGHT as f32);
        self.fill_gradient_rect(&panel, &self.palette.result_start, top)?;
        let details = self.result_details_metrics(track)?;
        let detail_rects = result_details_rects(top, details.title_height, details.artist_height);
        let art = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
            // The image view itself is 150×150pt; do not add UI-144 button-image padding.
            rect: result_artwork_frame(top),
            radiusX: 5.0,
            radiusY: 5.0,
        };
        self.target.FillRoundedRectangle(&art, &self.palette.accent);
        // The current recognition result contains title/artist only; keep this as an explicit
        // placeholder until an existing UI-facing artwork source is available.
        self.text(
            "♫",
            result_artwork_frame(top),
            &self.text.centered_heavy,
            &self.palette.white_ink,
        );
        self.text(
            &track.title,
            detail_rects.title,
            &self.text.result_title,
            &self.palette.title,
        );
        self.text(
            &track.artist,
            detail_rects.artist,
            &self.text.result_artist,
            &self.palette.subtitle,
        );
        let pill = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
            rect: detail_rects.action,
            radiusX: 10.0,
            radiusY: 10.0,
        };
        self.target
            .FillRoundedRectangle(&pill, &self.palette.accent);
        self.text(
            "再生",
            detail_rects.action,
            &self.text.date,
            &self.palette.white_ink,
        );
        self.draw_result_close_icon(top);
        Ok(())
    }

    unsafe fn draw_no_match_panel(&self, top: f32, offline: bool) -> windows::core::Result<()> {
        let panel = rect(0.0, top, CONTENT_WIDTH as f32, top + TOP_HEIGHT as f32);
        self.fill_gradient_rect(&panel, &self.palette.result_start, top)?;
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
        self.draw_result_close_icon(top);
        Ok(())
    }

    unsafe fn draw_result_close_icon(&self, body_top: f32) {
        let [left, top, right, bottom] = native_icon_layout(body_top).result_close_visual;
        self.target.DrawLine(
            Vector2 {
                X: left + 2.0,
                Y: top + 2.0,
            },
            Vector2 {
                X: right - 2.0,
                Y: bottom - 2.0,
            },
            &self.palette.subtitle,
            1.2,
            None,
        );
        self.target.DrawLine(
            Vector2 {
                X: right - 2.0,
                Y: top + 2.0,
            },
            Vector2 {
                X: left + 2.0,
                Y: bottom - 2.0,
            },
            &self.palette.subtitle,
            1.2,
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

    unsafe fn draw_file_hint(&self, body_bottom: f32) {
        self.text(
            "Ctrl+O 音声ファイルを認識",
            rect(
                10.0,
                body_bottom - 24.0,
                CONTENT_WIDTH as f32 - 10.0,
                body_bottom - 5.0,
            ),
            &self.text.date,
            &self.palette.subtitle,
        );
    }

    unsafe fn draw_settings(&self, app: &AppState) -> windows::core::Result<()> {
        let background = if app.theme == Theme::Light {
            &self.palette.white
        } else {
            &self.palette.black
        };
        let outer = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
            rect: rect(0.0, 0.0, SETTINGS_WIDTH as f32, SETTINGS_HEIGHT as f32),
            radiusX: 12.0,
            radiusY: 12.0,
        };
        self.target.FillRoundedRectangle(&outer, background);
        self.text(
            "設定",
            rect(28.0, 12.0, 510.0, 52.0),
            &self.text.centered_title,
            &self.palette.title,
        );
        let rows = [122.0f32, 64.0, 37.0, 64.0];
        let mut top = 58.0;
        for (index, height) in rows.into_iter().enumerate() {
            let row = rect(38.0, top, 512.0, top + height);
            self.fill_rect(
                row,
                if index % 2 == 0 {
                    &self.palette.skeleton
                } else {
                    background
                },
            );
            match index {
                0 => {
                    self.text(
                        "表示",
                        rect(56.0, top + 12.0, 470.0, top + 40.0),
                        &self.text.title,
                        &self.palette.title,
                    );
                    self.text(
                        "テーマ",
                        rect(56.0, top + 56.0, 300.0, top + 90.0),
                        &self.text.settings,
                        &self.palette.subtitle,
                    );
                    self.text(
                        if app.theme == Theme::Light {
                            "ライト"
                        } else {
                            "ダーク"
                        },
                        rect(355.0, top + 56.0, 490.0, top + 90.0),
                        &self.text.trailing_small,
                        &self.palette.accent,
                    );
                }
                1 => {
                    self.text(
                        "ショートカット",
                        rect(56.0, top + 6.0, 300.0, top + 30.0),
                        &self.text.settings,
                        &self.palette.title,
                    );
                    self.text(
                        "Ctrl + O",
                        rect(310.0, top + 6.0, 490.0, top + 30.0),
                        &self.text.trailing_small,
                        &self.palette.subtitle,
                    );
                    self.text(
                        "音声ファイルを開く",
                        rect(56.0, top + 31.0, 490.0, top + 59.0),
                        &self.text.settings_small,
                        &self.palette.subtitle,
                    );
                }
                2 => {
                    self.text(
                        "マイク入力",
                        rect(56.0, top + 2.0, 300.0, top + 35.0),
                        &self.text.settings,
                        &self.palette.title,
                    );
                    self.text(
                        "未対応",
                        rect(355.0, top + 2.0, 490.0, top + 35.0),
                        &self.text.trailing_small,
                        &self.palette.subtitle,
                    );
                }
                _ => {
                    self.text(
                        "このアプリについて",
                        rect(56.0, top + 8.0, 470.0, top + 33.0),
                        &self.text.settings,
                        &self.palette.title,
                    );
                    self.text(
                        "OpenShazam",
                        rect(56.0, top + 34.0, 470.0, top + 58.0),
                        &self.text.settings_small,
                        &self.palette.subtitle,
                    );
                }
            }
            top += height;
        }
        self.text(
            "Escで閉じる",
            rect(
                30.0,
                SETTINGS_HEIGHT as f32 - 30.0,
                520.0,
                SETTINGS_HEIGHT as f32 - 6.0,
            ),
            &self.text.date,
            &self.palette.subtitle,
        );
        Ok(())
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
            &self.palette.result_start
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
) -> windows::core::Result<Palette> {
    let (background, foreground, subtitle, skeleton, separator) = match theme {
        Theme::Light => (
            d2d_color(255, 255, 255, 1.0),
            d2d_color(0, 0, 0, 0.847),
            d2d_color(148, 148, 148, 1.0),
            d2d_color(230, 234, 237, 0.75),
            d2d_color(0, 0, 0, 0.098),
        ),
        Theme::Dark => (
            d2d_color(30, 30, 30, 1.0),
            d2d_color(255, 255, 255, 0.847),
            d2d_color(212, 212, 212, 1.0),
            d2d_color(55, 66, 73, 0.8),
            d2d_color(255, 255, 255, 0.098),
        ),
    };
    let white = solid(target, background)?;
    let white_ink = solid(target, d2d_color(255, 255, 255, 1.0))?;
    let black = solid(target, d2d_color(30, 30, 30, 1.0))?;
    let title = solid(target, foreground)?;
    let subtitle = solid(target, subtitle)?;
    let accent_color = if theme == Theme::Light {
        d2d_color(0, 136, 255, 1.0)
    } else {
        d2d_color(80, 80, 80, 1.0)
    };
    let accent = solid(target, accent_color)?;
    let button = solid(target, d2d_color(0, 136, 255, 1.0))?;
    let separator = solid(target, separator)?;
    let skeleton = solid(target, skeleton)?;
    let result_colors = if theme == Theme::Light {
        [d2d_color(244, 247, 249, 1.0), d2d_color(188, 205, 216, 1.0)]
    } else {
        [d2d_color(0, 0, 0, 1.0), d2d_color(55, 66, 73, 1.0)]
    };
    let result_start = gradient(
        target,
        result_colors,
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
        accent,
        button,
        separator,
        skeleton,
        result_start,
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
        button: create_text_format(
            factory,
            13.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?,
        settings: create_text_format(
            factory,
            13.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?,
        settings_small: create_text_format(
            factory,
            12.0,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_LEADING,
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
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: WM_TRAYICON_MESSAGE,
        hIcon: LoadIconW(None, IDI_APPLICATION)?,
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
    let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
    Ok(())
}

unsafe fn notify_hidden_match(hwnd: HWND, track: &Track) {
    let body = format!("{} — {}", track.title, track.artist);
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_INFO,
        ..Default::default()
    };
    copy_notification_text(&mut data.szInfoTitle, "Shazam");
    copy_notification_text(&mut data.szInfo, &body);
    let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
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
        ..Default::default()
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
        let flags = SWP_SHOWWINDOW;
        let _ = SetWindowPos(hwnd, None, x, y, width, height, flags);
    } else {
        app.arrow_edge = ArrowEdge::Top;
        app.arrow_center = (CONTENT_WIDTH * 0.5) as f32;
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            width,
            height,
            SWP_SHOWWINDOW | SWP_NOZORDER,
        );
    }
    app.visible = true;
    let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
    let _ = SetForegroundWindow(hwnd);
    let _ = InvalidateRect(Some(hwnd), None, false);
}

unsafe fn window_data(hwnd: HWND) -> Option<&'static mut WindowData> {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowData;
    ptr.as_mut()
}

unsafe fn main_hit_test(app: &AppState, x: f32, y: f32, body_top: f32) -> MainHit {
    if matches!(
        app.phase,
        MainPhase::Matched { .. } | MainPhase::NoMatch { .. } | MainPhase::Offline { .. }
    ) {
        let top = result_panel_top_at(&app.phase, body_top, Instant::now()).unwrap_or(body_top);
        let offset = top - body_top;
        let close = native_icon_layout(body_top).result_close_hit;
        if point_in_rect(x, y - offset, close) {
            return MainHit::CloseResult;
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
    let file_hint_top = body_top + CONTENT_HEIGHT as f32 - 30.0;
    if (10.0..=220.0).contains(&x) && y >= file_hint_top {
        return MainHit::OpenFile;
    }
    let icons = native_icon_layout(body_top);
    if matches!(app.phase, MainPhase::Listening { .. }) && point_in_rect(x, y, icons.cancel_hit) {
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
    OpenFile,
    Settings,
    Cancel,
    TimeoutRestart,
    TimeoutDismiss,
    TryAgain,
    CloseResult,
    Other,
}

unsafe fn open_settings(app: &mut AppState) {
    if app.settings_hwnd.0.is_null() {
        let dpi = GetDpiForWindow(app.main_hwnd).max(96);
        let factor = f64::from(dpi) / 96.0;
        if let Ok(hwnd) = create_window(
            app as *mut AppState,
            WindowKind::Settings,
            "OpenShazam Preferences",
            (SETTINGS_WIDTH * factor).round() as i32,
            (SETTINGS_HEIGHT * factor).round() as i32,
            false,
        ) {
            app.settings_hwnd = hwnd;
        }
    }
    if !app.settings_hwnd.0.is_null() {
        app.settings_visible = true;
        let _ = ShowWindow(app.settings_hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(app.settings_hwnd);
        let _ = InvalidateRect(Some(app.settings_hwnd), None, false);
    }
}

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
    unsafe {
        if !app.main_hwnd.0.is_null() {
            let _ = ShowWindow(app.main_hwnd, SW_HIDE);
        }
    }
    app.visible = false;
    unsafe { sync_main_timers(app) };
}

fn start_capture(app: &mut AppState, mode: CaptureMode, file: Option<PathBuf>) {
    if matches!(app.phase, MainPhase::Listening { .. }) {
        return;
    }
    let (program, args) = match file {
        Some(path) => match shazam_http_lab_exe() {
            Some(exe) => (
                exe,
                vec![
                    "audio-live".to_owned(),
                    path.to_string_lossy().into_owned(),
                    "12".to_owned(),
                ],
            ),
            None => {
                app.phase = MainPhase::Error {
                    message: "shazam-http-lab.exe が見つかりません".to_owned(),
                    entered_at: Instant::now(),
                };
                return;
            }
        },
        None => match std::env::current_exe() {
            Ok(exe) => (exe, vec!["recognize-loopback".to_owned()]),
            Err(error) => {
                app.phase = MainPhase::Error {
                    message: error.to_string(),
                    entered_at: Instant::now(),
                };
                return;
            }
        },
    };

    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match command.spawn() {
        Ok(child) => {
            let slot = Arc::new(Mutex::new(Some(child)));
            let slot_for_worker = Arc::clone(&slot);
            let cancel = Arc::new(AtomicBool::new(false));
            let cancel_for_worker = Arc::clone(&cancel);
            app.capture_child = Some(slot);
            app.capture_canceled = cancel;
            app.phase = MainPhase::Listening {
                mode,
                started_at: Instant::now(),
            };
            let hwnd = app.main_hwnd.0 as isize;
            thread::spawn(move || {
                let result = wait_for_capture(slot_for_worker, cancel_for_worker.clone());
                let canceled = cancel_for_worker.load(Ordering::Acquire);
                let finished = Box::new(CaptureFinished {
                    captured_mode: mode,
                    result,
                    canceled,
                });
                let pointer = Box::into_raw(finished) as isize;
                let posted = unsafe {
                    PostMessageW(
                        Some(HWND(hwnd as *mut c_void)),
                        WM_CAPTURE_FINISHED,
                        WPARAM(0),
                        LPARAM(pointer),
                    )
                };
                if posted.is_err() {
                    unsafe {
                        drop(Box::from_raw(pointer as *mut CaptureFinished));
                    }
                }
            });
        }
        Err(error) => {
            app.phase = MainPhase::Error {
                message: format!("認識処理を開始できません: {error}"),
                entered_at: Instant::now(),
            };
        }
    }
}

fn wait_for_capture(
    slot: Arc<Mutex<Option<Child>>>,
    canceled: Arc<AtomicBool>,
) -> Result<Output, String> {
    loop {
        if canceled.load(Ordering::Acquire) {
            if let Ok(mut guard) = slot.lock() {
                if let Some(child) = guard.as_mut() {
                    let _ = child.kill();
                }
            }
        }
        let finished = {
            let mut guard = slot
                .lock()
                .map_err(|_| "認識処理の状態ロックに失敗しました".to_owned())?;
            let child = guard
                .as_mut()
                .ok_or_else(|| "認識処理が終了しました".to_owned())?;
            match child.try_wait() {
                Ok(Some(_)) => true,
                Ok(None) => false,
                Err(error) => return Err(format!("認識処理を確認できません: {error}")),
            }
        };
        if finished {
            let child = slot
                .lock()
                .map_err(|_| "認識処理の状態ロックに失敗しました".to_owned())?
                .take()
                .ok_or_else(|| "認識処理が終了しました".to_owned())?;
            return child
                .wait_with_output()
                .map_err(|error| format!("認識結果を取得できません: {error}"));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn cancel_capture(app: &mut AppState) {
    app.capture_canceled.store(true, Ordering::Release);
    if let Some(slot) = &app.capture_child {
        if let Ok(mut guard) = slot.lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.kill();
            }
        }
    }
}

fn complete_capture(app: &mut AppState, finished: CaptureFinished) {
    app.capture_child = None;
    app.capture_canceled = Arc::new(AtomicBool::new(false));
    if finished.canceled {
        app.phase = MainPhase::Ready;
        return;
    }
    match finished.result {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some((title, artist)) = parse_match(&stdout) {
                let track = Track {
                    title,
                    artist,
                    recognized_at: Instant::now(),
                };
                let history_started_at = Instant::now();
                let previous_count = app.history.len();
                app.history.insert(0, track.clone());
                app.history_transition = Some(HistoryMutationTransition {
                    started_at: history_started_at,
                    event: HistoryMutationEvent::Insert {
                        index: 0,
                        previous_count,
                    },
                });
                app.history.truncate(40);
                if app.visible {
                    app.phase = MainPhase::Matched {
                        track,
                        stage: ResultCardStage::Entering {
                            started_at: Instant::now(),
                        },
                    };
                } else {
                    // Native posts a notification when the popup is hidden;
                    // keep that result out of a popup the user closed.
                    app.phase = MainPhase::Ready;
                    if finished.captured_mode == CaptureMode::Manual {
                        unsafe { notify_hidden_match(app.main_hwnd, &track) };
                    }
                }
            } else {
                app.phase = MainPhase::NoMatch {
                    entered_at: Instant::now(),
                    returned_to_ready_at: None,
                };
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            if is_timeout_failure(&stderr) {
                app.phase = MainPhase::Timeout {
                    mode: finished.captured_mode,
                    entered_at: Instant::now(),
                };
            } else if is_offline_failure(&stderr) {
                app.phase = MainPhase::Offline {
                    entered_at: Instant::now(),
                    returned_to_ready_at: None,
                };
            } else {
                app.phase = MainPhase::Error {
                    message: if stderr.is_empty() {
                        format!("認識処理が終了コード {} で失敗しました", output.status)
                    } else {
                        stderr.chars().take(100).collect()
                    },
                    entered_at: Instant::now(),
                };
            }
        }
        Err(error) => {
            if is_timeout_failure(&error) {
                app.phase = MainPhase::Timeout {
                    mode: finished.captured_mode,
                    entered_at: Instant::now(),
                };
            } else if is_offline_failure(&error) {
                app.phase = MainPhase::Offline {
                    entered_at: Instant::now(),
                    returned_to_ready_at: None,
                };
            } else {
                app.phase = MainPhase::Error {
                    message: error,
                    entered_at: Instant::now(),
                };
            }
        }
    }
    let _mode = finished.captured_mode;
}

fn is_timeout_failure(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    [
        "timeout",
        "timed out",
        "timedout",
        "deadline exceeded",
        "タイムアウト",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
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
    Listening,
    NoMatchTitle,
    NoMatchBody,
    OfflineBody,
    TryAgain,
    TimeoutDescription,
    TimeoutDismiss,
    TimeoutRestart,
}

fn localized_ui_text(key: UiTextKey, language: UiLanguage) -> &'static str {
    match (key, language) {
        (UiTextKey::Ready, UiLanguage::Japanese) => "クリックしてShazamする",
        (UiTextKey::Ready, UiLanguage::English) => "Click to Shazam",
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

fn parse_match(output: &str) -> Option<(String, String)> {
    for line in output.lines() {
        let Some(start) = line.find("title=") else {
            continue;
        };
        let (title, after_title) = parse_debug_string(&line[start + "title=".len()..])?;
        let artist_marker = after_title.find("artist=")?;
        let (artist, _) = parse_debug_string(&after_title[artist_marker + "artist=".len()..])?;
        if !title.trim().is_empty() && !artist.trim().is_empty() {
            return Some((title, artist));
        }
    }
    None
}

fn parse_debug_string(input: &str) -> Option<(String, &str)> {
    let input = input.trim_start();
    if !input.starts_with('"') {
        return None;
    }
    let mut output = String::new();
    let mut chars = input[1..].char_indices().peekable();
    while let Some((offset, ch)) = chars.next() {
        match ch {
            '"' => return Some((output, &input[offset + 2..])),
            '\\' => {
                let (_, escaped) = chars.next()?;
                match escaped {
                    'n' => output.push('\n'),
                    'r' => output.push('\r'),
                    't' => output.push('\t'),
                    '0' => output.push('\0'),
                    '\\' => output.push('\\'),
                    '"' => output.push('"'),
                    _ => output.push(escaped),
                }
            }
            _ => output.push(ch),
        }
    }
    None
}

fn shazam_http_lab_exe() -> Option<PathBuf> {
    let mut path = std::env::current_exe().ok()?;
    path.set_file_name("shazam-http-lab.exe");
    path.exists().then_some(path)
}

unsafe fn choose_audio_file(hwnd: HWND) -> Option<PathBuf> {
    let mut path = [0u16; 32_768];
    let filter = wide("Audio files\0*.mp3;*.wav;*.m4a;*.aac;*.flac;*.ogg\0All files\0*.*\0\0");
    let title = wide("認識する音声ファイルを選択");
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: windows::core::PWSTR(path.as_mut_ptr()),
        nMaxFile: path.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST,
        ..Default::default()
    };
    if !GetOpenFileNameW(&mut dialog).as_bool() {
        return None;
    }
    let end = path
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(path.len());
    Some(PathBuf::from(String::from_utf16_lossy(&path[..end])))
}

unsafe fn update_visible_windows(app: &mut AppState) {
    sync_main_timers(app);
    if app.visible {
        let _ = InvalidateRect(Some(app.main_hwnd), None, false);
    }
    if app.settings_visible {
        let _ = InvalidateRect(Some(app.settings_hwnd), None, false);
    }
    if app.about_visible {
        let _ = InvalidateRect(Some(app.about_hwnd), None, false);
    }
    if app.microphone_visible {
        let _ = InvalidateRect(Some(app.microphone_hwnd), None, false);
    }
}

fn animation_snapshot(app: &AppState) -> AnimationSnapshot {
    let (mode, show_button, show_ripples) = match app.phase {
        MainPhase::Listening {
            mode: CaptureMode::Manual,
            ..
        } => (TaggingMode::Manual, true, true),
        MainPhase::Listening {
            mode: CaptureMode::Auto,
            ..
        } => (TaggingMode::Auto, true, true),
        MainPhase::Matched { .. } | MainPhase::NoMatch { .. } | MainPhase::Offline { .. } => {
            (TaggingMode::Ready, false, false)
        }
        MainPhase::Ready | MainPhase::Error { .. } => (TaggingMode::Ready, true, false),
        MainPhase::Timeout { .. } => (TaggingMode::Ready, false, false),
    };
    AnimationSnapshot {
        mode,
        elapsed: phase_elapsed(&app.phase),
        theme: app.theme,
        body_top: if app.arrow_edge == ArrowEdge::Top {
            POPUP_ARROW_HEIGHT as f32
        } else {
            0.0
        },
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
    }
}

unsafe fn sync_main_timers(app: &mut AppState) {
    let hwnd = app.main_hwnd;
    if app.visible && !hwnd.0.is_null() {
        let track_phase = phase_needs_timeout(&app.phase) || app.history_transition.is_some();
        if track_phase && !app.phase_timer_active {
            let _ = SetTimer(Some(hwnd), TIMER_PHASE, PHASE_TICK_MS, None);
            app.phase_timer_active = true;
        } else if !track_phase && app.phase_timer_active {
            let _ = KillTimer(Some(hwnd), TIMER_PHASE);
            app.phase_timer_active = false;
        }
        let animate = animation_needs_frame(&app.phase, app.startup_animation_pending)
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
    match phase {
        // Listening ripples are continuous Windows Composition keyframes.
        // They do not need an 8 ms UI-thread timer; future meter values enter
        // through a change event and update the audio transforms once per value.
        MainPhase::Listening { .. } => false,
        MainPhase::Matched { stage, .. } => match stage {
            ResultCardStage::Entering { started_at }
            | ResultCardStage::Exiting { started_at, .. } => {
                started_at.elapsed() < RESULT_CARD_TRANSITION
            }
            ResultCardStage::Holding { .. } => false,
        },
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
            entered_at.elapsed() < Duration::from_millis(350)
                || returned_to_ready_at
                    .is_some_and(|start| start.elapsed() < Duration::from_millis(350))
        }
        MainPhase::Ready | MainPhase::Error { .. } | MainPhase::Timeout { .. } => false,
    }
}

fn result_panel_top_at(phase: &MainPhase, body_top: f32, now: Instant) -> Option<f32> {
    if let MainPhase::Matched { stage, .. } = phase {
        return Some(match stage {
            ResultCardStage::Entering { started_at } => {
                let progress = linear_transition_progress(*started_at, now, RESULT_CARD_TRANSITION);
                body_top - (1.0 - progress) * TOP_HEIGHT as f32
            }
            ResultCardStage::Holding { .. } => body_top,
            ResultCardStage::Exiting {
                started_at,
                from_offset,
            } => {
                let progress = linear_transition_progress(*started_at, now, RESULT_CARD_TRANSITION);
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
    let entrance = ui_model::native_bezier(
        (now.saturating_duration_since(entered_at).as_secs_f64() / 0.3).clamp(0.0, 1.0),
    ) as f32;
    let exit = returned_to_ready_at
        .map(|start| {
            ui_model::native_bezier(
                (now.saturating_duration_since(start).as_secs_f64() / 0.3).clamp(0.0, 1.0),
            ) as f32
        })
        .unwrap_or(0.0);
    Some(body_top - (1.0 - entrance) * TOP_HEIGHT as f32 + exit * TOP_HEIGHT as f32)
}

fn linear_transition_progress(started_at: Instant, now: Instant, duration: Duration) -> f32 {
    // Native NSAnimationContext's default timing function is not established in
    // the reference, so keep this Windows interpolation deterministic and
    // replaceable instead of reusing another feature's measured Bezier curve.
    (now.saturating_duration_since(started_at).as_secs_f32() / duration.as_secs_f32())
        .clamp(0.0, 1.0)
}

fn advance_result_card_stage(stage: ResultCardStage, now: Instant) -> ResultCardAdvance {
    match stage {
        ResultCardStage::Entering { started_at }
            if now.saturating_duration_since(started_at) >= RESULT_CARD_TRANSITION =>
        {
            ResultCardAdvance::Stage(ResultCardStage::Holding {
                started_at: started_at + RESULT_CARD_TRANSITION,
            })
        }
        ResultCardStage::Holding { started_at }
            if now.saturating_duration_since(started_at) >= RESULT_CARD_HOLD =>
        {
            ResultCardAdvance::Stage(ResultCardStage::Exiting {
                started_at: started_at + RESULT_CARD_HOLD,
                from_offset: 0.0,
            })
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

unsafe fn show_main(app: &mut AppState, mut renderer: Option<&mut Renderer>) {
    if let Some(renderer) = renderer.as_deref_mut() {
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
        if let Err(error) = renderer.update_composition(animation_snapshot(app)) {
            app.message = Some(format!("Composition初期配置に失敗: {error}"));
        }
    }
    update_visible_windows(app);
}

unsafe fn process_main_hit(app: &mut AppState, hit: MainHit) {
    match hit {
        MainHit::Settings => open_settings(app),
        MainHit::Cancel => cancel_capture(app),
        MainHit::TimeoutRestart => {
            if let MainPhase::Timeout { mode, .. } = app.phase {
                start_capture(app, mode, None);
            }
        }
        MainHit::TimeoutDismiss => app.phase = MainPhase::Ready,
        MainHit::TryAgain => start_capture(app, CaptureMode::Manual, None),
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
        MainHit::CenterButton => {
            if matches!(app.phase, MainPhase::Listening { .. }) {
                cancel_capture(app);
            } else {
                start_capture(app, CaptureMode::Manual, None);
            }
        }
        MainHit::OpenFile => {
            if let Some(path) = choose_audio_file(app.main_hwnd) {
                start_capture(app, CaptureMode::Manual, Some(path));
            }
        }
        MainHit::Other => {}
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

    let Some(data) = window_data(hwnd) else {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    };
    let app = &mut *data.app;

    match message {
        WM_SHOW_INITIAL_POPUP if data.kind == WindowKind::Main => {
            show_main(app, data.renderer.as_mut());
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
                if data.kind == WindowKind::Main {
                    if let Err(error) = renderer.update_composition(animation_snapshot(app)) {
                        app.message = Some(format!("Windows Composition update failed: {error}"));
                    }
                }
                if let Err(error) = renderer.draw(app, data.kind) {
                    app.message = Some(format!("描画エラー: {error}"));
                }
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
            let event = lparam.0 as u32;
            if event == windows::Win32::UI::Shell::NIN_SELECT
                || event == windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONUP
                || event == windows::Win32::UI::WindowsAndMessaging::WM_RBUTTONUP
            {
                if app.visible {
                    hide_main(app);
                } else {
                    show_main(app, data.renderer.as_mut());
                }
            }
            LRESULT(0)
        }
        WM_CAPTURE_FINISHED if data.kind == WindowKind::Main => {
            let finished = Box::from_raw(lparam.0 as *mut CaptureFinished);
            complete_capture(app, *finished);
            update_visible_windows(app);
            LRESULT(0)
        }
        WM_LBUTTONDOWN if data.kind == WindowKind::Main => {
            let (x, y) = client_dips(hwnd, lparam);
            let body_top = if app.arrow_edge == ArrowEdge::Top {
                POPUP_ARROW_HEIGHT as f32
            } else {
                0.0
            };
            app.mouse_down_center = main_hit_test(app, x, y, body_top) == MainHit::CenterButton;
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
            if main_hit_test(app, x, y, body_top) == MainHit::CenterButton {
                app.mouse_down_center = true;
                app.long_press_consumed = true;
                if matches!(
                    app.phase,
                    MainPhase::Listening {
                        mode: CaptureMode::Auto,
                        ..
                    }
                ) {
                    cancel_capture(app);
                } else {
                    start_capture(app, CaptureMode::Auto, None);
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
                let hit = main_hit_test(app, x, y, body_top);
                process_main_hit(app, hit);
                update_visible_windows(app);
            }
            LRESULT(0)
        }
        WM_TIMER if data.kind == WindowKind::Main && wparam.0 == TIMER_PHASE => {
            if update_phase_timers(app) {
                update_visible_windows(app);
            }
            LRESULT(0)
        }
        WM_TIMER if data.kind == WindowKind::Main && wparam.0 == TIMER_ANIMATION => {
            if app.visible {
                if let Some(renderer) = data.renderer.as_mut() {
                    if let Err(error) = renderer.update_composition(animation_snapshot(app)) {
                        app.message = Some(format!("Windows Composition update failed: {error}"));
                        app.startup_animation_pending = false;
                    } else if app.startup_animation_pending {
                        app.startup_animation_pending = renderer
                            .composition
                            .as_ref()
                            .is_some_and(|scene| !scene.startup_logo_complete());
                    }
                }
                if matches!(
                    app.phase,
                    MainPhase::Matched { .. }
                        | MainPhase::NoMatch { .. }
                        | MainPhase::Offline { .. }
                ) {
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
                if matches!(
                    app.phase,
                    MainPhase::Listening {
                        mode: CaptureMode::Auto,
                        ..
                    }
                ) {
                    cancel_capture(app);
                } else {
                    start_capture(app, CaptureMode::Auto, None);
                }
                update_visible_windows(app);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE if data.kind == WindowKind::Main => {
            let (_x, y) = client_dips(hwnd, lparam);
            let body_top = if app.arrow_edge == ArrowEdge::Top {
                POPUP_ARROW_HEIGHT as f32
            } else {
                0.0
            };
            let history_top = body_top + TOP_HEIGHT as f32;
            let next = if y >= history_top {
                let row = ((y - history_top + app.scroll_offset) / 91.0).floor() as usize;
                (row < app.history.len()).then_some(row)
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
            if !app.history.is_empty() {
                let delta = (high_word(wparam.0 as isize) as i16) as f32;
                let extent = (app.history.len().saturating_sub(1) as f32 * 91.0).max(0.0);
                app.scroll_offset = (app.scroll_offset - delta / 120.0 * 91.0).clamp(0.0, extent);
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 == VK_ESCAPE.0 as usize {
                match data.kind {
                    WindowKind::Main => hide_main(app),
                    WindowKind::Settings => {
                        app.settings_visible = false;
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                    WindowKind::About => {
                        app.about_visible = false;
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                    WindowKind::MicrophoneInfo => {
                        app.microphone_visible = false;
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                }
            } else if data.kind == WindowKind::Main
                && wparam.0 == b'O' as usize
                && GetKeyState(VK_CONTROL.0 as i32) < 0
            {
                if let Some(path) = choose_audio_file(hwnd) {
                    start_capture(app, CaptureMode::Manual, Some(path));
                }
                update_visible_windows(app);
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
                if matches!(
                    app.phase,
                    MainPhase::Listening {
                        mode: CaptureMode::Auto,
                        ..
                    }
                ) {
                    cancel_capture(app);
                } else {
                    start_capture(app, CaptureMode::Auto, None);
                }
                update_visible_windows(app);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP if data.kind == WindowKind::Settings => {
            let x = low_word(lparam.0) as f32 * 96.0 / GetDpiForWindow(hwnd).max(96) as f32;
            let y = high_word(lparam.0) as f32 * 96.0 / GetDpiForWindow(hwnd).max(96) as f32;
            if (355.0..=510.0).contains(&x) && (110.0..=190.0).contains(&y) {
                app.theme = if app.theme == Theme::Light {
                    Theme::Dark
                } else {
                    Theme::Light
                };
                app.theme_generation += 1;
                update_visible_windows(app);
            } else if (55.0..=500.0).contains(&x) && (180.0..=244.0).contains(&y) {
                if let Some(path) = choose_audio_file(hwnd) {
                    start_capture(app, CaptureMode::Manual, Some(path));
                }
                update_visible_windows(app);
            } else if (55.0..=500.0).contains(&x) && (244.0..=310.0).contains(&y) {
                open_microphone_info(app);
            } else if (55.0..=500.0).contains(&x) && y >= 310.0 {
                open_about(app);
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
                WindowKind::Settings => {
                    app.settings_visible = false;
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
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
        WM_ACTIVATE if data.kind == WindowKind::Main => {
            let activation = wparam.0 & 0xffff;
            if activation == 0 && app.main_activated && no_auxiliary_window_visible(app) {
                hide_main(app);
            } else if activation != 0 {
                app.main_activated = true;
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_KILLFOCUS if data.kind == WindowKind::Main => {
            if app.main_activated && no_auxiliary_window_visible(app) {
                hide_main(app);
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
                    WindowKind::Settings => app.settings_hwnd = HWND::default(),
                    WindowKind::About => app.about_hwnd = HWND::default(),
                    WindowKind::MicrophoneInfo => app.microphone_hwnd = HWND::default(),
                }
                drop(data);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_DESTROY if data.kind == WindowKind::Main => {
            delete_tray_icon(hwnd);
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

fn no_auxiliary_window_visible(app: &AppState) -> bool {
    !app.settings_visible && !app.about_visible && !app.microphone_visible
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
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
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
        let app = Box::new(AppState {
            main_hwnd: HWND::default(),
            settings_hwnd: HWND::default(),
            about_hwnd: HWND::default(),
            microphone_hwnd: HWND::default(),
            d2d_factory,
            dwrite_factory,
            compositor,
            _dispatcher_queue: dispatcher_queue,
            phase: MainPhase::Ready,
            history: Vec::new(),
            history_transition: None,
            scroll_offset: 0.0,
            hovered_row: None,
            ui_levels: UiLevels::default(),
            auto_spring: None,
            theme: Theme::Light,
            theme_generation: 0,
            visible: false,
            main_activated: false,
            phase_timer_active: false,
            animation_timer_active: false,
            startup_animation_pending: false,
            startup_logo_consumed: false,
            settings_visible: false,
            about_visible: false,
            microphone_visible: false,
            arrow_edge: ArrowEdge::Top,
            arrow_center: (CONTENT_WIDTH * 0.5) as f32,
            mouse_down_center: false,
            long_press_consumed: false,
            single_click_pending: false,
            capture_canceled: Arc::new(AtomicBool::new(false)),
            capture_child: None,
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
        if let Err(error) = add_tray_icon(hwnd) {
            let _ = DestroyWindow(hwnd);
            drop(Box::from_raw(app_ptr));
            return Err(Box::new(error));
        }
        // Let Explorer finish publishing the notification icon before asking
        // Shell_NotifyIconGetRect for its anchor. `ui` opens the popup on
        // launch; the tray icon remains available to hide/show it afterward.
        let _ = PostMessageW(Some(hwnd), WM_SHOW_INITIAL_POPUP, WPARAM(0), LPARAM(0));
        let mut message = MSG::default();
        let mut running = true;
        while running {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                if message.message == windows::Win32::UI::WindowsAndMessaging::WM_QUIT {
                    running = false;
                    break;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            if !running {
                break;
            }
            let _ = WaitMessage();
        }
        if !(*app_ptr).settings_hwnd.0.is_null() {
            let _ = DestroyWindow((*app_ptr).settings_hwnd);
        }
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
    if app.history_transition.is_some_and(|transition| {
        now.saturating_duration_since(transition.started_at) >= HISTORY_INSERT_TRANSITION
    }) {
        app.history_transition = None;
        changed = true;
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
    let base = history_top + index as f32 * 91.0 - scroll_offset;
    let Some(transition) = transition else {
        return base;
    };
    let progress =
        linear_transition_progress(transition.started_at, now, HISTORY_INSERT_TRANSITION);
    match transition.event {
        HistoryMutationEvent::Insert {
            index: inserted, ..
        } if index >= inserted => base - 91.0 * (1.0 - progress),
        HistoryMutationEvent::Delete { index: deleted } if index >= deleted => {
            base + 91.0 * (1.0 - progress)
        }
        HistoryMutationEvent::Insert { .. }
        | HistoryMutationEvent::Update { .. }
        | HistoryMutationEvent::Delete { .. } => base,
    }
}

fn _assert_only_ui_dependencies_are_used(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::{
        advance_result_card_stage, animation_needs_frame, button_border_arc_points,
        composition_root_layer_order, copy_notification_text, gradient_origin_transform,
        header_fill_scope, history_artwork_frame, history_row_top, inverse_circle_clip_radii,
        is_offline_failure, is_timeout_failure, localized_ui_text, native_icon_layout,
        no_match_copy, parse_debug_string, parse_match, phase_needs_timeout, point_in_rect,
        result_artwork_frame, result_details_rects, result_panel_top_at,
        select_ui_language_from_multisz, should_start_startup_logo, timeout_copy, ui_model,
        ArrowEdge, CaptureMode, CompositionRootLayer, HeaderFillScope, HistoryMutationEvent,
        HistoryMutationTransition, MainPhase, NoMatchIconCommand, ResultCardAdvance,
        ResultCardStage, UiLanguage, UiTextKey, WindowKind, BUTTON_CLEAR_DIAMETER,
        INNER_MASK_BOUNDS, NO_MATCH_ICON_PATH, OUTER_BASE_OPACITY, RESULT_DETAILS_LEFT,
        RESULT_DETAILS_RIGHT, SETTINGS_ICON_PIXELS, WHITE_CLOSE_ICON_PIXELS,
    };

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
    fn parses_cli_result_without_changing_recognition_protocol() {
        let output = "Shazam HTTP status=200\nmatch[1] title=\"I Really Like You\" artist=\"カーリー・レイ・ジェプセン\"\n";
        assert_eq!(
            parse_match(output).unwrap(),
            (
                "I Really Like You".to_owned(),
                "カーリー・レイ・ジェプセン".to_owned()
            )
        );
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
    fn parses_escaped_debug_strings() {
        let (decoded, rest) = parse_debug_string("\"Line\\n\\\"One\\\"\" artist=\"A\"").unwrap();
        assert_eq!(decoded, "Line\n\"One\"");
        assert_eq!(rest, " artist=\"A\"");
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
    fn button_arc_preserves_native_degree_value_and_geometry() {
        let [start, _, _, end] = button_border_arc_points();
        assert_eq!(start, (97.0, 50.0));
        assert!((end.0 - 96.926_4).abs() < 0.001);
        assert!((end.1 - 52.685_3).abs() < 0.001);
        assert!(end.1 > start.1);
    }

    #[test]
    fn native_icon_visual_frames_and_hit_targets_are_independent() {
        let layout = native_icon_layout(0.0);
        assert_eq!(layout.settings_visual, [349.0, 25.0, 365.0, 41.0]);
        assert_eq!(layout.settings_hit, layout.settings_visual);
        assert_eq!(layout.cancel_visual, [351.0, 27.0, 363.0, 39.0]);
        assert_eq!(layout.cancel_hit, layout.cancel_visual);
        assert_eq!(
            (layout.settings_visual[1] + layout.settings_visual[3]) * 0.5,
            (layout.settings_visual[1] + layout.settings_visual[3]) * 0.5
        );
        assert_eq!(
            (layout.cancel_visual[1] + layout.cancel_visual[3]) * 0.5,
            (layout.settings_visual[1] + layout.settings_visual[3]) * 0.5
        );
        assert_eq!(layout.result_close_visual, [353.0, 25.0, 365.0, 37.0]);
        assert_eq!(layout.result_close_hit, layout.result_close_visual);
        assert!(point_in_rect(349.0, 25.0, layout.settings_hit));
        assert!(!point_in_rect(348.9, 26.0, layout.settings_hit));
        assert!(point_in_rect(351.0, 27.0, layout.cancel_hit));
        assert!(!point_in_rect(350.9, 28.0, layout.cancel_hit));
        assert!(point_in_rect(353.0, 25.0, layout.result_close_hit));
        assert!(!point_in_rect(352.9, 25.0, layout.result_close_hit));
        assert!(!point_in_rect(349.0, 25.0, layout.result_close_hit));
    }

    #[test]
    fn native_header_icons_embed_the_high_resolution_catalog_renditions() {
        assert_eq!(SETTINGS_ICON_PIXELS.len(), 32 * 32 * 4);
        assert_eq!(WHITE_CLOSE_ICON_PIXELS.len(), 24 * 24 * 4);
        // The catalog packs these images in a bottom-origin atlas. Preserve
        // their recognizable silhouettes so another valid but wrong crop
        // (for example a social icon) cannot pass as the settings/close art.
        assert_eq!(SETTINGS_ICON_PIXELS[(16 * 32 + 16) * 4 + 3], 0);
        assert!(WHITE_CLOSE_ICON_PIXELS[(12 * 24 + 12) * 4 + 3] > 0);
    }

    #[test]
    fn result_panel_position_is_reproducible_at_entry_and_exit_times() {
        let entered_at = std::time::Instant::now();
        let returned_at = entered_at + std::time::Duration::from_secs(1);
        let phase = MainPhase::Matched {
            track: super::Track {
                title: String::new(),
                artist: String::new(),
                recognized_at: entered_at,
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
                title: String::new(),
                artist: String::new(),
                recognized_at: entered_at,
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
    fn result_card_holds_ten_seconds_after_entrance_then_exits() {
        let start = std::time::Instant::now();
        let holding_start = start + std::time::Duration::from_millis(300);
        let exit_start = holding_start + std::time::Duration::from_secs(10);
        assert!(matches!(
            advance_result_card_stage(ResultCardStage::Entering { started_at: start }, holding_start),
            ResultCardAdvance::Stage(ResultCardStage::Holding { started_at }) if started_at == holding_start
        ));
        assert!(matches!(
            advance_result_card_stage(ResultCardStage::Holding { started_at: holding_start }, exit_start),
            ResultCardAdvance::Stage(ResultCardStage::Exiting { started_at, from_offset: 0.0 }) if started_at == exit_start
        ));
        assert!(matches!(
            advance_result_card_stage(
                ResultCardStage::Exiting {
                    started_at: exit_start,
                    from_offset: 0.0
                },
                exit_start + std::time::Duration::from_millis(300)
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
    fn timeout_markers_are_separated_from_generic_failures_and_no_match() {
        assert!(is_timeout_failure("request timed out"));
        assert!(is_timeout_failure("recognition timeout"));
        assert!(is_timeout_failure("タイムアウト"));
        assert!(!is_timeout_failure("network connection failed"));
        assert!(!is_timeout_failure("no match"));
        let timeout = MainPhase::Timeout {
            mode: CaptureMode::Manual,
            entered_at: std::time::Instant::now(),
        };
        assert!(!phase_needs_timeout(&timeout));
        assert!(!animation_needs_frame(&timeout, false));
    }

    #[test]
    fn preferred_ui_languages_choose_first_supported_language_and_fallback_to_english() {
        let to_multisz = |tags: &[&str]| {
            let mut buffer = Vec::new();
            for tag in tags {
                buffer.extend(tag.encode_utf16());
                buffer.push(0);
            }
            buffer.push(0);
            buffer
        };
        assert_eq!(
            select_ui_language_from_multisz(&to_multisz(&["fr-FR", "ja-JP", "en-US"])),
            UiLanguage::Japanese
        );
        assert_eq!(
            select_ui_language_from_multisz(&to_multisz(&["de-DE", "en-GB", "ja-JP"])),
            UiLanguage::English
        );
        assert_eq!(
            select_ui_language_from_multisz(&to_multisz(&["fr-FR", "de-DE"])),
            UiLanguage::English
        );
    }

    #[test]
    fn localized_recognition_copy_uses_source_keys_for_both_languages() {
        assert_eq!(
            localized_ui_text(UiTextKey::Ready, UiLanguage::Japanese),
            "クリックしてShazamする"
        );
        assert_eq!(
            localized_ui_text(UiTextKey::Listening, UiLanguage::English),
            "Listening"
        );
        assert_eq!(
            no_match_copy(false, UiLanguage::Japanese),
            ("結果なし", "認識できませんでした。もう一度お試しください。")
        );
        assert_eq!(
            no_match_copy(false, UiLanguage::English),
            ("No Result", "We didn’t quite catch that. Please try again.")
        );
        assert_eq!(
            no_match_copy(true, UiLanguage::Japanese),
            ("結果なし", "接続を確認してください")
        );
        assert_eq!(timeout_copy(UiLanguage::English).1, "Dismiss");
        assert_eq!(timeout_copy(UiLanguage::Japanese).2, "再起動");
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
    fn native_root_insertion_keeps_inner_behind_button_and_outer_in_front() {
        assert_eq!(
            composition_root_layer_order(),
            [
                CompositionRootLayer::InnerRipples,
                CompositionRootLayer::Button,
                CompositionRootLayer::OuterRipples,
            ]
        );
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
    fn inner_ripple_mask_tracks_button_scale_separately_from_audio_fill() {
        for bass in [0.0, 0.2, 5.0 / 7.0, 1.0] {
            let frame = ui_model::wave_frame_at(
                ui_model::TaggingMode::Manual,
                0.0,
                ui_model::UiLevels { bass, treble: 0.5 },
            );
            let hole_diameter = BUTTON_CLEAR_DIAMETER as f64 * frame.button_child_scale;
            let fill_scale = frame.inner_container_factor / 3.0 * frame.inner_audio_scale;
            let expected_button_scale = 0.75 + 0.35 * bass;
            let expected_fill_scale = 0.7 + 0.6 * bass;
            assert!((frame.button_child_scale - expected_button_scale).abs() < 1e-9);
            assert!((fill_scale - expected_fill_scale).abs() < 1e-9);
            assert!((hole_diameter - 96.0 * expected_button_scale).abs() < 1e-9);
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
        assert!(!should_start_startup_logo(WindowKind::Settings, false));
    }

    #[test]
    fn album_art_frames_keep_native_view_sizes_without_button_padding() {
        let result = result_artwork_frame(10.0);
        assert_eq!(
            (result.left, result.top, result.right, result.bottom),
            (10.0, 30.0, 160.0, 180.0)
        );
        assert_eq!(result.right - result.left, 150.0);
        assert_eq!(result.bottom - result.top, 150.0);

        let history = history_artwork_frame(200.0);
        assert_eq!(
            (history.left, history.top, history.right, history.bottom),
            (20.0, 212.0, 85.0, 277.0)
        );
        assert_eq!(history.right - history.left, 65.0);
        assert_eq!(history.bottom - history.top, 65.0);
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
    }

    #[test]
    fn result_details_keep_the_derived_164_point_text_column() {
        assert_eq!(RESULT_DETAILS_LEFT, 172.0);
        assert_eq!(RESULT_DETAILS_RIGHT - RESULT_DETAILS_LEFT, 164.0);
    }

    #[test]
    fn result_details_stack_tracks_artwork_center_and_expands_for_wrapping() {
        let single_line = result_details_rects(10.0, 24.0, 18.0);
        assert_eq!(single_line.title.top, 66.5);
        assert_eq!(single_line.title.bottom, 90.5);
        assert_eq!(single_line.artist.top, 95.5);
        assert_eq!(single_line.action.top, 123.5);
        assert_eq!(
            (single_line.title.top + single_line.action.bottom) * 0.5,
            105.0
        );

        let wrapped = result_details_rects(10.0, 48.0, 36.0);
        assert_eq!(wrapped.title.top, 45.5);
        assert_eq!(wrapped.action.bottom, 164.5);
        assert_eq!((wrapped.title.top + wrapped.action.bottom) * 0.5, 105.0);
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
                    BUTTON_SIZE as f32 * 0.8,
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
}
