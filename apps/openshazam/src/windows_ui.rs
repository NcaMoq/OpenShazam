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
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use windows::{
    core::{implement, Error, Interface, Ref, HSTRING, PCWSTR},
    Foundation::TimeSpan,
    Graphics::IGeometrySource2D,
    System::DispatcherQueueController,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Direct2D::{
                Common::{
                    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BEZIER_SEGMENT,
                    D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW,
                    D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_GRADIENT_STOP,
                    D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
                },
                D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1Geometry,
                ID2D1HwndRenderTarget, ID2D1LinearGradientBrush, ID2D1RenderTarget,
                ID2D1SolidColorBrush, D2D1_COMBINE_MODE_UNION, D2D1_ELLIPSE,
                D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2,
                D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
                D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
                D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
                D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
            },
            DirectWrite::{
                DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
                DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_BOLD,
                DWRITE_FONT_WEIGHT_DEMI_BOLD, DWRITE_FONT_WEIGHT_HEAVY, DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
                DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_WRAP,
            },
            Dwm::DwmFlush,
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
                Shell_NotifyIconGetRect, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP,
                NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION, NOTIFYICONDATAW,
                NOTIFYICONIDENTIFIER, NOTIFYICON_VERSION_4,
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
            CompositionPath, CompositionPathGeometry, CompositionSpriteShape, Compositor,
            ContainerVisual, Desktop::DesktopWindowTarget, DropShadow, ScalarKeyFrameAnimation,
            ShapeVisual, Vector2KeyFrameAnimation, Vector3KeyFrameAnimation,
        },
    },
};

#[cfg(test)]
use windows::{
    Foundation::Size,
    Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat},
    Win32::Graphics::Direct2D::{
        D2D1CreateDevice, ID2D1Device, ID2D1DeviceContext, D2D1_PRIMITIVE_BLEND_COPY,
        D2D1_PRIMITIVE_BLEND_SOURCE_OVER,
    },
    Win32::Graphics::{
        Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0},
        Direct3D11::{
            D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
        },
        Dxgi::IDXGIDevice,
    },
    Win32::System::WinRT::Composition::{ICompositionDrawingSurfaceInterop, ICompositorInterop},
    UI::Composition::{CompositionDrawingSurface, CompositionMaskBrush, CompositionSurfaceBrush},
};

use windows_numerics::{Vector2, Vector3};

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
// [I] The spec confirms four inner CAShape layers and their colors/opacity,
// but does not identify a line width. Keep this preview width isolated so a
// measured value can replace it without changing the shape or timeline.
const INNER_STROKE_WIDTH_PREVIEW: f32 = 2.0;
const MAIN_WINDOW_WIDTH: f64 = CONTENT_WIDTH;
const MAIN_WINDOW_HEIGHT: f64 = CONTENT_HEIGHT + POPUP_ARROW_HEIGHT;
const SETTINGS_WIDTH: f64 = 550.0;
const SETTINGS_HEIGHT: f64 = 400.0;
const ABOUT_WIDTH: f64 = 600.0;
const ABOUT_HEIGHT: f64 = 320.0;
const MIC_INFO_WIDTH: f64 = CONTENT_WIDTH;
const MIC_INFO_HEIGHT: f64 = 190.0;

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

#[derive(Debug)]
enum MainPhase {
    Ready,
    Listening {
        mode: CaptureMode,
        started_at: Instant,
    },
    Matched {
        track: Track,
        entered_at: Instant,
    },
    NoMatch {
        entered_at: Instant,
        returned_to_ready_at: Option<Instant>,
    },
    Error {
        message: String,
        entered_at: Instant,
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
    black: ID2D1SolidColorBrush,
    title: ID2D1SolidColorBrush,
    subtitle: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
    button: ID2D1SolidColorBrush,
    separator: ID2D1SolidColorBrush,
    skeleton: ID2D1SolidColorBrush,
    result_start: ID2D1LinearGradientBrush,
    listening: ID2D1LinearGradientBrush,
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
    icon: IDWriteTextFormat,
    logo: IDWriteTextFormat,
}

struct Renderer {
    factory: ID2D1Factory,
    target: ID2D1RenderTarget,
    hwnd_target: Option<ID2D1HwndRenderTarget>,
    dc_target: Option<ID2D1DCRenderTarget>,
    layered_surface: Option<LayeredSurface>,
    palette: Palette,
    text: TextStyles,
    dpi: u32,
    theme_generation: u64,
    composition: Option<CompositionScene>,
    popup_outline: RefCell<Option<(ArrowEdge, u32, ID2D1Geometry)>>,
    popup_header: RefCell<Option<(ArrowEdge, u32, ID2D1Geometry)>>,
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
    geometry: CompositionEllipseGeometry,
    shape: CompositionSpriteShape,
    core_container: Option<ContainerVisual>,
    sublayer_container: Option<ContainerVisual>,
    inner_animations: Option<InnerRingAnimations>,
    core_scale_animation: Option<Vector3KeyFrameAnimation>,
    geometry_animation: Option<Vector2KeyFrameAnimation>,
    border_animation: Option<ScalarKeyFrameAnimation>,
    diameter: f32,
    base_stroke_width: f32,
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

/// Cached GPU alpha mask backed by a Composition drawing surface. The mask is
/// opaque outside a centered circular cutout; the surface is authored once and
/// can be sampled by independent CompositionMaskBrush instances/transforms.
#[cfg(test)]
#[derive(Debug)]
struct CompositionAlphaMask {
    _surface: CompositionDrawingSurface,
    _surface_brush: CompositionSurfaceBrush,
    brush: CompositionMaskBrush,
}

#[derive(Debug)]
struct CompositionLogo {
    visual: ContainerVisual,
    _transparent_visual: ShapeVisual,
    long_visual: ShapeVisual,
    final_visual: ShapeVisual,
    _transparent_geometries: [CompositionPathGeometry; 2],
    _long_geometries: [CompositionPathGeometry; 2],
    transparent_brush: CompositionColorBrush,
    _solid_brush: CompositionColorBrush,
    shadow: DropShadow,
    transparent_trim_animation: ScalarKeyFrameAnimation,
    solid_trim_animation: ScalarKeyFrameAnimation,
    long_fade_animation: ScalarKeyFrameAnimation,
    final_fade_animation: ScalarKeyFrameAnimation,
    started_at: Option<Instant>,
    stage: ui_model::StartupLogoStage,
}

const SHZ_LOGO_SOURCE_SIZE: f32 = 210.0;
const SHZ_LOGO_REFERENCE_SIZE: f32 = 367.5;
const SHZ_LOGO_STROKE_WIDTH: f32 = 38.0;

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
        offset_x: center_x - SHZ_LOGO_SOURCE_SIZE * 0.5,
        offset_y: center_y - SHZ_LOGO_SOURCE_SIZE * 0.5,
        scale_x: button_width / SHZ_LOGO_REFERENCE_SIZE,
        scale_y: button_height / SHZ_LOGO_REFERENCE_SIZE,
    }
}

#[cfg(test)]
fn shz_logo_point_in_button(layout: ShzLogoLayout, point: (f32, f32)) -> (f32, f32) {
    (
        layout.center_x + (point.0 - SHZ_LOGO_SOURCE_SIZE * 0.5) * layout.scale_x,
        layout.center_y + (point.1 - SHZ_LOGO_SOURCE_SIZE * 0.5) * layout.scale_y,
    )
}

impl CompositionLogo {
    unsafe fn new(
        d2d_factory: &ID2D1Factory,
        compositor: &Compositor,
        theme: Theme,
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
            LogoTrim::End,
        )?;
        let (final_visual, _) = make_logo_visual(
            compositor,
            d2d_factory,
            [SHZ_LOGO_TOP_PATH, SHZ_LOGO_BOTTOM_PATH],
            &solid_brush,
            LogoTrim::None,
        )?;
        transparent_visual.SetOpacity(1.0)?;
        long_visual.SetOpacity(1.0)?;
        final_visual.SetOpacity(0.0)?;

        let shadow = compositor.CreateDropShadow()?;
        shadow.SetBlurRadius(1.0)?;
        shadow.SetOffset(Vector3 {
            X: 0.5,
            Y: 1.0,
            Z: 0.0,
        })?;
        shadow.SetOpacity(1.0)?;
        shadow.SetColor(logo_shadow_color(theme))?;
        let transparent_root = compositor.CreateLayerVisual()?;
        transparent_root.SetSize(Vector2 {
            X: SHZ_LOGO_SOURCE_SIZE,
            Y: SHZ_LOGO_SOURCE_SIZE,
        })?;
        transparent_root
            .Children()?
            .InsertAtTop(&transparent_visual)?;
        transparent_root.SetShadow(&shadow)?;

        let visual = compositor.CreateContainerVisual()?;
        visual.SetSize(Vector2 {
            X: SHZ_LOGO_SOURCE_SIZE,
            Y: SHZ_LOGO_SOURCE_SIZE,
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
        let solid_trim = make_logo_trim_animation(
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
        let long_fade = make_logo_stage_switch_animation(
            compositor,
            ui_model::STARTUP_LOGO_TOTAL_DURATION,
            1.0,
            0.0,
        )?;
        let final_fade = make_logo_stage_switch_animation(
            compositor,
            ui_model::STARTUP_LOGO_TOTAL_DURATION,
            0.0,
            1.0,
        )?;
        Ok(Self {
            transparent_brush,
            _solid_brush: solid_brush,
            shadow,
            visual,
            _transparent_visual: transparent_visual,
            long_visual,
            final_visual,
            _transparent_geometries: transparent_geometries,
            _long_geometries: long_geometries,
            transparent_trim_animation: transparent_trim,
            solid_trim_animation: solid_trim,
            long_fade_animation: long_fade,
            final_fade_animation: final_fade,
            started_at: None,
            stage: ui_model::StartupLogoStage::TransparentStroke,
        })
    }

    fn start(&mut self) -> windows::core::Result<()> {
        if self.started_at.is_some() {
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
            geometry.StartAnimation(&HSTRING::from("TrimEnd"), &self.solid_trim_animation)?;
        }
        self.long_visual
            .StartAnimation(&HSTRING::from("Opacity"), &self.long_fade_animation)?;
        self.final_visual
            .StartAnimation(&HSTRING::from("Opacity"), &self.final_fade_animation)?;
        self.started_at = Some(started_at);
        Ok(())
    }

    fn advance(&mut self) -> windows::core::Result<bool> {
        let Some(started_at) = self.started_at else {
            return Ok(false);
        };
        let frame = ui_model::startup_logo_frame_at(started_at.elapsed().as_secs_f64());
        if frame.stage != self.stage {
            match frame.stage {
                ui_model::StartupLogoStage::TransparentStroke => {}
                ui_model::StartupLogoStage::SolidLongStroke
                | ui_model::StartupLogoStage::Complete => {}
            }
            self.stage = frame.stage;
        }
        Ok(self.stage == ui_model::StartupLogoStage::Complete)
    }

    fn set_theme(&self, theme: Theme) -> windows::core::Result<()> {
        self.transparent_brush
            .SetColor(logo_transparent_color(theme))?;
        self.shadow.SetColor(logo_shadow_color(theme))
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
    End,
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
        X: SHZ_LOGO_SOURCE_SIZE,
        Y: SHZ_LOGO_SOURCE_SIZE,
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
        visual.Shapes()?.Append(&shape)?;
    }
    Ok((visual, geometries))
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
        LogoTrim::End => geometry.SetTrimEnd(0.0)?,
        LogoTrim::None => {}
    }
    Ok(geometry)
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

fn make_logo_stage_switch_animation(
    compositor: &Compositor,
    delay_seconds: f64,
    from: f64,
    to: f64,
) -> windows::core::Result<ScalarKeyFrameAnimation> {
    let animation = compositor.CreateScalarKeyFrameAnimation()?;
    // Composition rejects animation durations below one millisecond even for
    // a delayed, effectively instantaneous opacity switch.
    animation.SetDuration(TimeSpan { Duration: 10_000 })?;
    animation.SetDelayTime(TimeSpan {
        Duration: (delay_seconds * 10_000_000.0).round() as i64,
    })?;
    animation.InsertKeyFrame(0.0, from as f32)?;
    animation.InsertKeyFrame(1.0, to as f32)?;
    Ok(animation)
}

fn logo_transparent_color(theme: Theme) -> Color {
    match theme {
        Theme::Light => composition_color(255, 27, 148, 250),
        Theme::Dark => composition_color(255, 9, 9, 9),
    }
}

fn logo_shadow_color(theme: Theme) -> Color {
    match theme {
        Theme::Light => composition_color(51, 255, 255, 255),
        Theme::Dark => composition_color(255, 0, 0, 0),
    }
}

#[cfg(test)]
#[derive(Debug)]
struct CompositionMaskFactory {
    _compositor: Compositor,
    _d3d_device: ID3D11Device,
    _d2d_device: ID2D1Device,
    graphics_device: windows::UI::Composition::CompositionGraphicsDevice,
}

#[cfg(test)]
impl CompositionMaskFactory {
    unsafe fn new(compositor: &Compositor) -> windows::core::Result<Self> {
        let d3d_device =
            create_mask_d3d_device().map_err(|error| mask_error(error, "D3D11 mask device"))?;
        let dxgi_device: IDXGIDevice = d3d_device
            .cast()
            .map_err(|error| mask_error(error, "DXGI mask device interface"))?;
        let d2d_device: ID2D1Device = D2D1CreateDevice(&dxgi_device, None)
            .map_err(|error| mask_error(error, "Direct2D mask device"))?;
        let interop: ICompositorInterop = compositor
            .cast()
            .map_err(|error| mask_error(error, "Composition graphics interop"))?;
        let graphics_device = interop
            .CreateGraphicsDevice(&d2d_device)
            .map_err(|error| mask_error(error, "Composition graphics device"))?;
        Ok(Self {
            _compositor: compositor.clone(),
            _d3d_device: d3d_device,
            _d2d_device: d2d_device,
            graphics_device,
        })
    }

    #[cfg(test)]
    unsafe fn inverse_circle(
        &self,
        size_dips: f32,
        hole_diameter_dips: f32,
        dpi: u32,
    ) -> windows::core::Result<CompositionAlphaMask> {
        CompositionAlphaMask::inverse_circle(self, size_dips, hole_diameter_dips, dpi)
    }
}

#[cfg(test)]
impl CompositionAlphaMask {
    unsafe fn inverse_circle(
        factory: &CompositionMaskFactory,
        size_dips: f32,
        hole_diameter_dips: f32,
        dpi: u32,
    ) -> windows::core::Result<Self> {
        let pixel_size = ui_model::physical_pixels(size_dips as f64, dpi.max(96));
        let surface = factory
            .graphics_device
            .CreateDrawingSurface(
                Size {
                    Width: pixel_size as f32,
                    Height: pixel_size as f32,
                },
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                DirectXAlphaMode::Premultiplied,
            )
            .map_err(|error| mask_error(error, "Composition drawing surface"))?;
        let surface_interop: ICompositionDrawingSurfaceInterop = surface
            .cast()
            .map_err(|error| mask_error(error, "Drawing surface interop"))?;
        let mut update_offset = POINT::default();
        let draw_context: ID2D1DeviceContext = surface_interop
            .BeginDraw(None, &mut update_offset)
            .map_err(|error| mask_error(error, "Drawing surface BeginDraw"))?;
        let draw_result = (|| -> windows::core::Result<()> {
            draw_context.SetDpi(dpi.max(96) as f32, dpi.max(96) as f32);
            draw_context.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));
            let opaque = draw_context.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                None,
            )?;
            let transparent = draw_context.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                },
                None,
            )?;
            let bounds = D2D_RECT_F {
                left: update_offset.x as f32 * 96.0 / dpi.max(96) as f32,
                top: update_offset.y as f32 * 96.0 / dpi.max(96) as f32,
                right: update_offset.x as f32 * 96.0 / dpi.max(96) as f32 + size_dips,
                bottom: update_offset.y as f32 * 96.0 / dpi.max(96) as f32 + size_dips,
            };
            draw_context.FillRectangle(&bounds, &opaque);
            draw_context.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_COPY);
            let radius = hole_diameter_dips.max(0.0) * 0.5;
            draw_context.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 {
                        X: (bounds.left + bounds.right) * 0.5,
                        Y: (bounds.top + bounds.bottom) * 0.5,
                    },
                    radiusX: radius,
                    radiusY: radius,
                },
                &transparent,
            );
            draw_context.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_SOURCE_OVER);
            Ok(())
        })();
        let end_draw = surface_interop.EndDraw();
        draw_result?;
        end_draw?;

        let surface_brush = factory
            ._compositor
            .CreateSurfaceBrushWithSurface(&surface)
            .map_err(|error| mask_error(error, "Composition surface brush"))?;
        let brush = factory
            ._compositor
            .CreateMaskBrush()
            .map_err(|error| mask_error(error, "Composition mask brush"))?;
        brush
            .SetMask(&surface_brush)
            .map_err(|error| mask_error(error, "Composition mask input"))?;
        let source = factory
            ._compositor
            .CreateColorBrushWithColor(composition_color(255, 255, 255, 255))
            .map_err(|error| mask_error(error, "Composition mask source brush"))?;
        brush
            .SetSource(&source)
            .map_err(|error| mask_error(error, "Composition mask content source"))?;
        Ok(Self {
            _surface: surface,
            _surface_brush: surface_brush,
            brush,
        })
    }

    fn brush(&self) -> &CompositionMaskBrush {
        &self.brush
    }
}

#[cfg(test)]
fn mask_error(error: Error, context: &str) -> Error {
    Error::new(error.code(), format!("{context}: {error}"))
}

#[cfg(test)]
unsafe fn create_mask_d3d_device() -> windows::core::Result<ID3D11Device> {
    let feature_levels = [D3D_FEATURE_LEVEL_11_0];
    let mut device = None;
    if D3D11CreateDevice(
        None,
        D3D_DRIVER_TYPE_HARDWARE,
        windows::Win32::Foundation::HMODULE::default(),
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        Some(&feature_levels),
        D3D11_SDK_VERSION,
        Some(&mut device),
        None,
        None,
    )
    .is_err()
    {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_WARP,
            windows::Win32::Foundation::HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    device.ok_or_else(Error::from_thread)
}

struct CompositionScene {
    compositor: Compositor,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    inner_audio_root: ContainerVisual,
    outer_audio_root: ContainerVisual,
    rings: [CompositionRing; 6],
    button: ContainerVisual,
    button_shape: CompositionSpriteShape,
    button_fill: CompositionColorBrush,
    logo: CompositionLogo,
    ring_brushes: [CompositionColorBrush; 6],
    idle_animation: Vector3KeyFrameAnimation,
    outer_scale_null_animation: Vector3KeyFrameAnimation,
    idle_active: bool,
    ripples_active: bool,
    theme: Theme,
    body_top: f32,
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
}

impl CompositionScene {
    fn new(
        hwnd: HWND,
        d2d_factory: &ID2D1Factory,
        compositor: &Compositor,
        theme: Theme,
        arrow_edge: ArrowEdge,
    ) -> windows::core::Result<Self> {
        let interop: ICompositorDesktopInterop = compositor.cast()?;
        let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, true)? };
        let root = compositor.CreateContainerVisual()?;
        root.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: MAIN_WINDOW_HEIGHT as f32,
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
        let mut ring_brushes = Vec::with_capacity(6);
        let center_x = (BUTTON_X + BUTTON_SIZE * 0.5) as f32;
        let center_y = body_top + (BUTTON_TOP + BUTTON_SIZE * 0.5) as f32;
        let inner_bounds = ui_model::INNER_DIAMETERS[0] as f32;
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
        for index in 0..6 {
            let diameter = if index < 4 {
                ui_model::INNER_DIAMETERS[index] as f32
            } else {
                ui_model::OUTER_DIAMETERS[index - 4] as f32
            };
            let base_stroke_width = if index < 4 {
                INNER_STROKE_WIDTH_PREVIEW
            } else {
                4.0
            };
            let brush = compositor.CreateColorBrushWithColor(ring_color)?;
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
            let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
            shape.SetStrokeBrush(&brush)?;
            shape.SetStrokeThickness(base_stroke_width)?;
            shape_visual.Shapes()?.Append(&shape)?;
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
                sublayer.Children()?.InsertAtTop(&shape_visual)?;
                core.Children()?.InsertAtTop(&sublayer)?;
                visual.Children()?.InsertAtTop(&core)?;
                inner_audio_root.Children()?.InsertAtTop(&visual)?;
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
            ring_brushes.push(brush);
            rings.push(CompositionRing {
                visual,
                geometry,
                shape,
                core_container,
                sublayer_container,
                inner_animations,
                core_scale_animation,
                geometry_animation,
                border_animation,
                diameter,
                base_stroke_width,
            });
        }
        let rings: [CompositionRing; 6] = rings.try_into().expect("six composition ring visuals");
        let ring_brushes: [CompositionColorBrush; 6] = ring_brushes
            .try_into()
            .expect("six composition ring brushes");

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
            compositor.CreateColorBrushWithColor(composition_color(255, 0, 136, 255))?;
        let button_edge =
            compositor.CreateColorBrushWithColor(composition_color(255, 255, 255, 255))?;
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
        button_shape.SetStrokeBrush(&button_edge)?;
        button_shape.SetStrokeThickness(if theme == Theme::Light { 0.75 } else { 0.0 })?;
        button_shape_visual.Shapes()?.Append(&button_shape)?;
        let mut logo = unsafe { CompositionLogo::new(d2d_factory, compositor, theme)? };
        button.Children()?.InsertAtTop(&button_shape_visual)?;
        button.Children()?.InsertAtTop(&logo.visual)?;

        let children = root.Children()?;
        children.InsertAtTop(&inner_audio_root)?;
        children.InsertAtTop(&button)?;
        children.InsertAtTop(&outer_audio_root)?;

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
        logo.start()?;

        Ok(Self {
            compositor: compositor.clone(),
            _target: target,
            root,
            inner_audio_root,
            outer_audio_root,
            rings,
            button,
            button_shape,
            button_fill,
            logo,
            ring_brushes,
            idle_animation,
            outer_scale_null_animation,
            idle_active: false,
            ripples_active: false,
            theme,
            body_top,
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
                .SetColor(composition_color(255, 0, 136, 255))?;
            self.button_shape
                .SetStrokeThickness(if self.theme == Theme::Light {
                    0.75
                } else {
                    0.0
                })?;
        }

        if self.body_top != snapshot.body_top {
            self.body_top = snapshot.body_top;
            let bottom_inset = MAIN_WINDOW_HEIGHT as f32 - (snapshot.body_top + TOP_HEIGHT as f32);
            let clip =
                self.compositor
                    .CreateInsetClipWithInsets(0.0, 0.0, 0.0, bottom_inset.max(0.0))?;
            self.root.SetClip(&clip)?;
            let inner_bounds = ui_model::INNER_DIAMETERS[0] as f32;
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

        self.button.SetOffset(Vector3 {
            X: BUTTON_X as f32,
            Y: snapshot.body_top + BUTTON_TOP as f32,
            Z: 0.0,
        })?;
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
                ring.visual.SetOpacity(1.0)?;
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
                        .StartAnimation(&HSTRING::from("Radius"), size)?;
                    ring.shape
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
                ring.geometry.StopAnimation(&HSTRING::from("Radius"))?;
                ring.shape
                    .StopAnimation(&HSTRING::from("StrokeThickness"))?;
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
            self.inner_audio_root.SetScale(Vector3 {
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
                ring.shape.SetStrokeThickness(ring.base_stroke_width)?;
                ring.geometry.SetRadius(Vector2 {
                    X: ring.diameter * 0.5,
                    Y: ring.diameter * 0.5,
                })?;
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
            }
            let frame = ui_model::wave_frame_at_with_auto_spring(
                snapshot.mode,
                snapshot.elapsed,
                snapshot.levels,
                snapshot.auto_spring,
            );
            self.inner_audio_root.SetScale(Vector3 {
                X: (frame.inner_container_factor / 3.0 * frame.inner_audio_scale) as f32,
                Y: (frame.inner_container_factor / 3.0 * frame.inner_audio_scale) as f32,
                Z: 1.0,
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
            } else if (!snapshot.show_button || !logo_finished) && self.idle_active {
                self.button.StopAnimation(&HSTRING::from("Scale"))?;
                self.idle_active = false;
            }
        } else {
            if self.idle_active {
                self.button.StopAnimation(&HSTRING::from("Scale"))?;
                self.idle_active = false;
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
        }
        Ok(())
    }
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
        let palette = create_palette(&target, theme)?;
        let text = create_text_styles(&write_factory)?;
        let composition = if kind == WindowKind::Main {
            Some(CompositionScene::new(
                hwnd,
                &factory,
                &compositor,
                theme,
                ArrowEdge::Top,
            )?)
        } else {
            None
        };
        Ok(Self {
            factory,
            target,
            hwnd_target,
            dc_target,
            layered_surface,
            palette,
            text,
            dpi,
            theme_generation,
            composition,
            popup_outline: RefCell::new(None),
            popup_header: RefCell::new(None),
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

    unsafe fn on_dpi_changed(&mut self, dpi: u32, hwnd: HWND, app: &AppState) {
        self.dpi = dpi.max(96);
        self.target.SetDpi(self.dpi as f32, self.dpi as f32);
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
        match kind {
            WindowKind::Main => self.draw_main(app)?,
            WindowKind::Settings => self.draw_settings(app)?,
            WindowKind::About => self.draw_about(app)?,
            WindowKind::MicrophoneInfo => self.draw_microphone_info(app)?,
        }
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

    unsafe fn fill_rect(&self, area: D2D_RECT_F, brush: &ID2D1SolidColorBrush) {
        self.target.FillRectangle(&area, brush);
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

        let popup = self.popup_outline(app)?;
        self.target.FillGeometry(&popup, bg, None);
        match app.phase {
            MainPhase::Listening { .. } => {
                self.target
                    .FillGeometry(&self.popup_header(app)?, &self.palette.listening, None);
            }
            MainPhase::Matched { .. } | MainPhase::NoMatch { .. } => {
                self.target.FillGeometry(
                    &self.popup_header(app)?,
                    &self.palette.result_start,
                    None,
                );
            }
            MainPhase::Ready | MainPhase::Error { .. } => {}
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
        if app.history.is_empty() {
            self.draw_skeleton(history_rect);
        } else {
            self.draw_history(app, history_top);
        }
        self.target.PopAxisAlignedClip();

        match &app.phase {
            MainPhase::Ready => self.draw_ready(app, body_top),
            MainPhase::Listening { mode, .. } => {
                self.draw_listening_status(body_top, *mode);
                self.draw_cancel_button(body_top);
            }
            MainPhase::Matched {
                ref track,
                entered_at,
            } => {
                self.draw_result_panel(track, body_top, entered_at.elapsed().as_secs_f64());
            }
            MainPhase::NoMatch {
                entered_at,
                returned_to_ready_at,
            } => {
                self.draw_no_match_panel(
                    body_top,
                    entered_at.elapsed().as_secs_f64(),
                    *returned_to_ready_at,
                );
            }
            MainPhase::Error { ref message, .. } => {
                self.draw_ready(app, body_top);
                self.draw_error_banner(message, body_top);
            }
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

        // Merge the colored header and arrow before clipping to the popup's
        // outer contour so their shared edge is rasterized only once.
        let arrow = self.popup_arrow_geometry(app.arrow_edge, center)?;
        let header_with_arrow = self.combine_geometry(&header, &arrow, D2D1_COMBINE_MODE_UNION)?;
        let outline = self.popup_outline(app)?;
        let clipped = self.combine_geometry(
            &header_with_arrow,
            &outline,
            windows::Win32::Graphics::Direct2D::D2D1_COMBINE_MODE_INTERSECT,
        )?;
        *self.popup_header.borrow_mut() = Some((key.0, key.1, clipped.clone()));
        Ok(clipped)
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
        self.text(
            "曲をタップして認識",
            rect(
                10.0,
                body_top + 144.0,
                CONTENT_WIDTH as f32 - 10.0,
                body_top + 168.0,
            ),
            &self.text.centered_title,
            if app.theme == Theme::Light {
                &self.palette.title
            } else {
                &self.palette.white
            },
        );
        self.text(
            "⚙",
            rect(343.0, body_top + 20.0, 373.0, body_top + 48.0),
            &self.text.icon,
            &self.palette.subtitle,
        );
    }

    unsafe fn draw_listening_status(&self, body_top: f32, mode: CaptureMode) {
        self.text(
            match mode {
                CaptureMode::Manual => "聴き取り中…",
                CaptureMode::Auto => "Auto Shazam",
            },
            rect(
                10.0,
                body_top + 142.0,
                CONTENT_WIDTH as f32 - 10.0,
                body_top + 170.0,
            ),
            &self.text.centered_title,
            &self.palette.white,
        );
    }

    unsafe fn draw_cancel_button(&self, body_top: f32) {
        self.text(
            "×",
            rect(345.0, body_top + 18.0, 374.0, body_top + 48.0),
            &self.text.icon,
            &self.palette.white,
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

    unsafe fn draw_history(&self, app: &AppState, history_top: f32) {
        let row_pitch = 91.0f32;
        for (index, track) in app.history.iter().enumerate() {
            let top = history_top + index as f32 * row_pitch - app.scroll_offset;
            if top + 89.0 < history_top || top > history_top + HISTORY_HEIGHT as f32 {
                continue;
            }
            let row = rect(0.0, top, CONTENT_WIDTH as f32, top + 89.0);
            if app.hovered_row == Some(index) {
                self.fill_rect(row, &self.palette.skeleton);
            }
            let artwork = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                rect: rect(20.0, top + 12.0, 85.0, top + 77.0),
                radiusX: 4.0,
                radiusY: 4.0,
            };
            self.target
                .FillRoundedRectangle(&artwork, &self.palette.result_start);
            self.text(
                "♫",
                rect(20.0, top + 12.0, 85.0, top + 77.0),
                &self.text.centered_title,
                &self.palette.white,
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

    unsafe fn draw_result_panel(&self, track: &Track, body_top: f32, elapsed: f64) {
        let entrance = ui_model::native_bezier((elapsed / 0.3).clamp(0.0, 1.0)) as f32;
        let y_offset = (1.0 - entrance) * -TOP_HEIGHT as f32;
        let top = body_top + y_offset;
        let panel = rect(0.0, top, CONTENT_WIDTH as f32, top + TOP_HEIGHT as f32);
        self.target
            .FillRectangle(&panel, &self.palette.result_start);
        let art = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
            rect: rect(10.0, top + 20.0, 160.0, top + 170.0),
            radiusX: 5.0,
            radiusY: 5.0,
        };
        self.target.FillRoundedRectangle(&art, &self.palette.accent);
        self.text(
            "♫",
            rect(10.0, top + 20.0, 160.0, top + 170.0),
            &self.text.centered_heavy,
            &self.palette.white,
        );
        self.text(
            &track.title,
            rect(172.0, top + 20.0, 355.0, top + 88.0),
            &self.text.result_title,
            &self.palette.title,
        );
        self.text(
            &track.artist,
            rect(172.0, top + 91.0, 355.0, top + 137.0),
            &self.text.result_artist,
            &self.palette.subtitle,
        );
        let pill = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
            rect: rect(172.0, top + 145.0, 242.0, top + 165.0),
            radiusX: 10.0,
            radiusY: 10.0,
        };
        self.target
            .FillRoundedRectangle(&pill, &self.palette.accent);
        self.text(
            "再生",
            rect(172.0, top + 145.0, 242.0, top + 165.0),
            &self.text.date,
            &self.palette.white,
        );
        self.text(
            "×",
            rect(342.0, top + 16.0, 372.0, top + 48.0),
            &self.text.icon,
            &self.palette.subtitle,
        );
    }

    unsafe fn draw_no_match_panel(
        &self,
        body_top: f32,
        elapsed: f64,
        returned_to_ready_at: Option<Instant>,
    ) {
        let entrance = ui_model::native_bezier((elapsed / 0.3).clamp(0.0, 1.0)) as f32;
        let closing = returned_to_ready_at
            .map(|start| {
                1.0 - ui_model::native_bezier((start.elapsed().as_secs_f64() / 0.3).clamp(0.0, 1.0))
                    as f32
            })
            .unwrap_or(1.0);
        let y_offset = (1.0 - entrance) * -TOP_HEIGHT as f32 + (1.0 - closing) * TOP_HEIGHT as f32;
        let top = body_top + y_offset;
        let panel = rect(0.0, top, CONTENT_WIDTH as f32, top + TOP_HEIGHT as f32);
        self.target
            .FillRectangle(&panel, &self.palette.result_start);
        self.text(
            "見つかりませんでした",
            rect(15.0, top + 55.0, 365.0, top + 84.0),
            &self.text.centered_heavy,
            &self.palette.title,
        );
        self.text(
            "音楽がはっきり聞こえる場所でもう一度お試しください",
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
            "もう一度試す",
            rect(90.0, top + 120.0, 290.0, top + 150.0),
            &self.text.button,
            &self.palette.white,
        );
        self.text(
            "×",
            rect(342.0, top + 16.0, 372.0, top + 48.0),
            &self.text.icon,
            &self.palette.subtitle,
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
            &self.palette.white,
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
        black,
        title,
        subtitle,
        accent,
        button,
        separator,
        skeleton,
        result_start,
        listening,
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
        icon: create_text_format(
            factory,
            20.0,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_CENTER,
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
        MainPhase::Matched { entered_at, .. } => *entered_at,
        MainPhase::NoMatch { entered_at, .. } => *entered_at,
        MainPhase::Error { entered_at, .. } => *entered_at,
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
        MainPhase::Matched { .. } | MainPhase::NoMatch { .. }
    ) && x >= 340.0
        && y <= body_top + 50.0
    {
        return MainHit::CloseResult;
    }
    if let MainPhase::NoMatch { .. } = app.phase {
        if (90.0..=290.0).contains(&x) && y >= body_top + 115.0 && y <= body_top + 155.0 {
            return MainHit::TryAgain;
        }
    }
    let file_hint_top = body_top + CONTENT_HEIGHT as f32 - 30.0;
    if (10.0..=220.0).contains(&x) && y >= file_hint_top {
        return MainHit::OpenFile;
    }
    if x >= 340.0 && y >= body_top + 15.0 && y <= body_top + 53.0 {
        if matches!(app.phase, MainPhase::Listening { .. }) {
            return MainHit::Cancel;
        }
        if matches!(app.phase, MainPhase::Ready | MainPhase::Error { .. }) {
            return MainHit::Settings;
        }
    }
    let button_x = BUTTON_X as f32..=(BUTTON_X + BUTTON_SIZE) as f32;
    let button_y = body_top + BUTTON_TOP as f32..=body_top + (BUTTON_TOP + BUTTON_SIZE) as f32;
    if button_x.contains(&x)
        && button_y.contains(&y)
        && !matches!(
            app.phase,
            MainPhase::Matched { .. } | MainPhase::NoMatch { .. }
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
                app.history.insert(0, track.clone());
                app.history.truncate(40);
                app.phase = MainPhase::Matched {
                    track,
                    entered_at: Instant::now(),
                };
            } else {
                app.phase = MainPhase::NoMatch {
                    entered_at: Instant::now(),
                    returned_to_ready_at: None,
                };
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            app.phase = MainPhase::Error {
                message: if stderr.is_empty() {
                    format!("認識処理が終了コード {} で失敗しました", output.status)
                } else {
                    stderr.chars().take(100).collect()
                },
                entered_at: Instant::now(),
            };
        }
        Err(error) => {
            app.phase = MainPhase::Error {
                message: error,
                entered_at: Instant::now(),
            };
        }
    }
    let _mode = finished.captured_mode;
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
        MainPhase::Matched { .. } | MainPhase::NoMatch { .. } => (TaggingMode::Ready, false, false),
        MainPhase::Ready | MainPhase::Error { .. } => (TaggingMode::Ready, true, false),
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
    }
}

unsafe fn sync_main_timers(app: &mut AppState) {
    let hwnd = app.main_hwnd;
    if app.visible && !hwnd.0.is_null() {
        let track_phase = phase_needs_timeout(&app.phase);
        if track_phase && !app.phase_timer_active {
            let _ = SetTimer(Some(hwnd), TIMER_PHASE, PHASE_TICK_MS, None);
            app.phase_timer_active = true;
        } else if !track_phase && app.phase_timer_active {
            let _ = KillTimer(Some(hwnd), TIMER_PHASE);
            app.phase_timer_active = false;
        }
        let animate = phase_needs_frame(&app.phase);
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
        MainPhase::Matched { .. } | MainPhase::NoMatch { .. } | MainPhase::Error { .. }
    )
}

fn phase_needs_frame(phase: &MainPhase) -> bool {
    match phase {
        MainPhase::Listening { .. } => true,
        MainPhase::Matched { entered_at, .. } => entered_at.elapsed() < Duration::from_millis(350),
        MainPhase::NoMatch {
            entered_at,
            returned_to_ready_at,
        } => {
            entered_at.elapsed() < Duration::from_millis(350)
                || returned_to_ready_at
                    .is_some_and(|start| start.elapsed() < Duration::from_millis(350))
        }
        MainPhase::Ready | MainPhase::Error { .. } => false,
    }
}

unsafe fn show_main(app: &mut AppState) {
    let changed = update_phase_timers(app);
    anchor_popup(app);
    if changed {
        app.message = None;
    }
    update_visible_windows(app);
}

unsafe fn process_main_hit(app: &mut AppState, hit: MainHit) {
    match hit {
        MainHit::Settings => open_settings(app),
        MainHit::Cancel => cancel_capture(app),
        MainHit::TryAgain => start_capture(app, CaptureMode::Manual, None),
        MainHit::CloseResult => {
            app.phase = MainPhase::Ready;
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
            show_main(app);
            LRESULT(0)
        }
        WM_CREATE => {
            let dpi = GetDpiForWindow(hwnd).max(96);
            match Renderer::new(
                hwnd,
                app.d2d_factory.clone(),
                app.dwrite_factory.clone(),
                app.compositor.clone(),
                dpi,
                app.theme_generation,
                app.theme,
                data.kind,
            ) {
                Ok(renderer) => data.renderer = Some(renderer),
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
                    show_main(app);
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
                    let _ = renderer.update_composition(animation_snapshot(app));
                }
                if matches!(
                    app.phase,
                    MainPhase::Matched { .. } | MainPhase::NoMatch { .. }
                ) {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                let _ = DwmFlush();
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
        MainPhase::Matched { entered_at, .. }
            if now.duration_since(*entered_at) >= Duration::from_secs(10) =>
        {
            app.phase = MainPhase::Ready;
            changed = true;
        }
        MainPhase::NoMatch {
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
    changed
}

fn _assert_only_ui_dependencies_are_used(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::{parse_debug_string, parse_match};

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
            target.FillGeometry(&popup, &brush, None);
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
        assert_eq!((layout.offset_x, layout.offset_y), (-57.0, -57.0));
        assert_eq!(
            (
                layout.center_x - layout.offset_x,
                layout.center_y - layout.offset_y
            ),
            (105.0, 105.0)
        );
        assert!((SHZ_LOGO_STROKE_WIDTH * layout.scale_x - 9.926_531).abs() < 0.000_01);
        let top_start = shz_logo_point_in_button(layout, SHZ_LOGO_TOP_PATH.start);
        assert!((top_start.0 - 40.474_12).abs() < 0.000_02);
        assert!((top_start.1 - 23.282_94).abs() < 0.000_02);
        let bottom_start = shz_logo_point_in_button(layout, SHZ_LOGO_BOTTOM_PATH.start);
        assert!((bottom_start.0 - 55.528_49).abs() < 0.000_02);
        assert!((bottom_start.1 - 73.082_78).abs() < 0.000_02);
        assert_eq!(SHZ_LOGO_SOURCE_SIZE, 210.0);
        assert_eq!(SHZ_LOGO_TOP_PATH.curves.len(), 4);
        assert_eq!(SHZ_LOGO_BOTTOM_PATH.curves.len(), 4);
        assert_eq!(SHZ_LOGO_TOP_PATH.start, (76.19, 10.38));
        assert_eq!(SHZ_LOGO_BOTTOM_PATH.start, (133.82, 201.02));
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
    #[ignore = "requires a Windows Composition dispatcher and D3D device; run explicitly to verify alpha-mask surface interop"]
    fn composition_drawing_surface_builds_a_cached_inverse_alpha_mask() {
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
                let factory = CompositionMaskFactory::new(&compositor)?;
                let d2d_factory: ID2D1Factory =
                    D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
                let _masks = [
                    factory.inverse_circle(336.0, 96.0, 144)?,
                    factory.inverse_circle(336.0, 96.0, 144)?,
                    factory.inverse_circle(336.0, 96.0, 144)?,
                ];
                let mut logo = CompositionLogo::new(&d2d_factory, &compositor, Theme::Light)?;
                logo.start()?;
                for dpi in [96, 120, 144, 168, 192, 240, 288] {
                    let mask = factory.inverse_circle(336.0, 96.0, dpi)?;
                    let _brush = mask.brush();
                }
                Ok(())
            })();
            drop(dispatcher);
            CoUninitialize();
            result.unwrap();
        }
    }
}
