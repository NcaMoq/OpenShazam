#![deny(unsafe_op_in_unsafe_fn)]

use super::artwork_pipeline::artwork_pixels_row_stride;
use super::composition_scene::{AnimationSnapshot, CompositionScene};
#[cfg(test)]
pub(super) use super::text_layout::measure_directwrite_text_layout;
use super::text_layout::TextLayout;
use super::{
    aspect_fit_rect, bgra_buffer_layout, composition_scale_for_dpi, create_popup_arrow_geometry,
    create_popup_outline_geometry, create_ui_icon_bitmap, d2d_color, d2d_color_from_system,
    fallback_match_artwork_rect, gradient_origin_transform, header_fill_scope,
    history_artwork_frame, history_first_insert_progress, history_row_top, history_text_rects,
    history_title_color, localized_ui_text, native_icon_layout, no_match_copy,
    overlay_surface_error, popup_base_color, popup_outline_center, rect, result_artwork_rect,
    result_details_rects, result_gradient_colors, result_overlay_failure_matches,
    result_overlay_identity_matches_phase, result_overlay_motion_animation, result_panel_draw_top,
    result_panel_top_at, should_replace_cached_artwork, status_text_progress, timeout_copy,
    ui_language, AppState, Arc, ArrowEdge, CachedArtworkBitmap, CaptureMode, Color, Compositor,
    Error, GetClientRect, HashMap, HeaderFillScope, HistoryCompositionKey, HistoryMutationEvent,
    ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory, ID2D1Geometry, ID2D1HwndRenderTarget,
    ID2D1Layer, ID2D1LinearGradientBrush, ID2D1RenderTarget, ID2D1SolidColorBrush, IDWriteFactory,
    IDWriteTextFormat, Instant, Interface, LayeredSurface, MainPhase, Matrix3x2, RefCell,
    ResultOverlayContentKey, ResultOverlayIdentity, ResultOverlayMotionKey, StatusCycle, Theme,
    Track, UiLanguage, UiTextKey, Vector2, WindowKind, COMPOSITION_RESULT_CARD_ENABLED,
    CONTENT_HEIGHT, CONTENT_WIDTH, D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_BITMAP_PROPERTIES, D2D1_COMBINE_MODE_UNION, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_END_CLOSED, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LAYER_OPTIONS_NONE,
    D2D1_PIXEL_FORMAT, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D_RECT_F, D2D_SIZE_U, DWRITE_MEASURING_MODE_NATURAL,
    DXGI_FORMAT_B8G8R8A8_UNORM, EMPTY_HISTORY_ROW_TOPS, FALLBACK_MATCH_DARK_PIXELS,
    FALLBACK_MATCH_LIGHT_PIXELS, HISTORY_HEIGHT, HISTORY_INSERT_TRANSITION, HISTORY_ROW_HEIGHT,
    HISTORY_ROW_PITCH, HWND, MAIN_WINDOW_HEIGHT, POPUP_ARROW_HEIGHT, RECT,
    RESULT_ARTWORK_CORNER_RADIUS, RESULT_CLOSE_ICON_PIXELS, SETTINGS_ICON_PIXELS, TOP_HEIGHT,
    WHITE_CLOSE_ICON_PIXELS,
};

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

struct CachedResultPanelBitmap {
    key: ResultOverlayContentKey,
    bitmap: ID2D1Bitmap,
    pixels: Arc<[u8]>,
}

fn solid(
    target: &ID2D1RenderTarget,
    color: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F,
) -> windows::core::Result<ID2D1SolidColorBrush> {
    // SAFETY: `color` lives for the synchronous call and `target` is the live
    // render target owned by this renderer.
    unsafe { target.CreateSolidColorBrush(&color, None) }
}

fn gradient(
    target: &ID2D1RenderTarget,
    colors: [windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F; 2],
    start: Vector2,
    end: Vector2,
) -> windows::core::Result<ID2D1LinearGradientBrush> {
    use windows::Win32::Graphics::Direct2D::Common::D2D1_GRADIENT_STOP;
    use windows::Win32::Graphics::Direct2D::{
        D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    };

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
    // SAFETY: The complete two-stop array remains live through this call.
    let collection = unsafe {
        target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?
    };
    let properties = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
        startPoint: start,
        endPoint: end,
    };
    // SAFETY: The properties and stop collection remain alive for the call.
    unsafe { target.CreateLinearGradientBrush(&properties, None, &collection) }
}

fn create_palette(
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
    // The taskbar-facing lower arrow uses the native popup orientation reversed.
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

pub(super) struct Renderer {
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
    text: TextLayout,
    language: UiLanguage,
    dpi: u32,
    theme_generation: u64,
    composition: Option<CompositionScene>,
    status_text_composition_enabled: bool,
    popup_layer: Option<ID2D1Layer>,
    history_skeleton_layer: Option<ID2D1Layer>,
    artwork_layer: Option<ID2D1Layer>,
    artwork_geometries: [(u32, ID2D1Geometry); 2],
    artwork_bitmaps: RefCell<HashMap<u64, CachedArtworkBitmap>>,
    popup_outline: RefCell<Option<(ArrowEdge, u32, ID2D1Geometry)>>,
    popup_header: RefCell<Option<(ArrowEdge, u32, ID2D1Geometry)>>,
    result_panel_bitmap: Option<CachedResultPanelBitmap>,
    result_panel_bitmap_failure_key: Option<ResultOverlayContentKey>,
    history_overlay_failure_transition: Option<Instant>,
    history_scroll_overlay_failure: Option<Instant>,
    result_overlay_content_key: Option<ResultOverlayContentKey>,
    result_overlay_failure_key: Option<ResultOverlayContentKey>,
    result_overlay_motion_key: Option<ResultOverlayMotionKey>,
    result_overlay_failure_reported: bool,
}

impl Renderer {
    pub(super) fn status_text_composition_enabled(&self) -> bool {
        self.status_text_composition_enabled
    }

    #[allow(clippy::too_many_arguments)] // A window render target needs all three existing native factories and the target state.
    /// # Safety
    /// `hwnd` must be a live window on the calling UI thread, and each supplied
    /// Direct2D/DirectWrite/Composition factory must be valid for that thread.
    #[deny(unsafe_op_in_unsafe_fn)]
    pub(super) unsafe fn new(
        hwnd: HWND,
        factory: ID2D1Factory,
        write_factory: IDWriteFactory,
        compositor: Compositor,
        dpi: u32,
        arrow_edge: ArrowEdge,
        arrow_center: f32,
        theme_generation: u64,
        theme: Theme,
        system_foreground: Color,
        kind: WindowKind,
        animate_startup_logo: bool,
    ) -> windows::core::Result<Self> {
        // SAFETY: The caller establishes a live UI-thread HWND and valid COM
        // factories. All COM out-parameters and backing pixel buffers below
        // remain alive for their synchronous calls; returned interfaces are
        // owned by the resulting Renderer.
        unsafe {
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
                let target =
                    factory.CreateHwndRenderTarget(&target_properties, &hwnd_properties)?;
                let render_target: ID2D1RenderTarget = target.cast()?;
                (render_target, Some(target), None, None)
            };
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let settings_icon = create_ui_icon_bitmap(&target, SETTINGS_ICON_PIXELS, 32, 32)?;
            let white_close_icon = create_ui_icon_bitmap(&target, WHITE_CLOSE_ICON_PIXELS, 24, 24)?;
            let result_close_icon =
                create_ui_icon_bitmap(&target, &RESULT_CLOSE_ICON_PIXELS, 24, 24)?;
            let fallback_match_artwork = if kind == WindowKind::Main {
                Some((
                    create_ui_icon_bitmap(&target, FALLBACK_MATCH_LIGHT_PIXELS, 262, 262)?,
                    create_ui_icon_bitmap(&target, FALLBACK_MATCH_DARK_PIXELS, 262, 262)?,
                ))
            } else {
                None
            };
            let palette = create_palette(&target, theme, system_foreground)?;
            let text = TextLayout::new(&write_factory)?;
            let composition = if kind == WindowKind::Main {
                Some(CompositionScene::new(
                    hwnd,
                    &factory,
                    &write_factory,
                    &compositor,
                    theme,
                    arrow_edge,
                    arrow_center,
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
            let history_skeleton_layer = if kind == WindowKind::Main {
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
                language: ui_language(),
                dpi,
                theme_generation,
                composition,
                status_text_composition_enabled: false,
                popup_layer,
                history_skeleton_layer,
                artwork_layer: Some(artwork_layer),
                artwork_geometries,
                artwork_bitmaps: RefCell::new(HashMap::new()),
                popup_outline: RefCell::new(None),
                popup_header: RefCell::new(None),
                result_panel_bitmap: None,
                result_panel_bitmap_failure_key: None,
                history_overlay_failure_transition: None,
                history_scroll_overlay_failure: None,
                result_overlay_content_key: None,
                result_overlay_failure_key: None,
                result_overlay_motion_key: None,
                result_overlay_failure_reported: false,
            })
        }
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

    pub(super) fn result_overlay_matches_current(&self, app: &AppState) -> bool {
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

    fn cached_result_panel_bitmap(&self, key: &ResultOverlayContentKey) -> Option<&ID2D1Bitmap> {
        self.result_panel_bitmap
            .as_ref()
            .filter(|cached| cached.key == *key)
            .map(|cached| &cached.bitmap)
    }

    /// # Safety
    /// This must run on the renderer's owning UI thread while its D2D target is
    /// alive; the source pixels and properties passed to CreateBitmap stay
    /// readable until that synchronous COM call returns.
    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn ensure_result_panel_bitmap(
        &mut self,
        app: &AppState,
        key: &ResultOverlayContentKey,
    ) -> windows::core::Result<()> {
        // SAFETY: Renderer targets are thread-affine and the bitmap byte layout
        // is validated before its pointer is passed to Direct2D.
        unsafe {
            if self.cached_result_panel_bitmap(key).is_some() {
                return Ok(());
            }

            let (pixels, width, height) = self.rasterize_result_panel(app)?;
            let (row_stride, expected_len) =
                bgra_buffer_layout(width, height).ok_or_else(|| {
                    Error::new(
                        windows::core::HRESULT(0x8007_0057u32 as i32),
                        "Result card bitmap dimensions are invalid or too large",
                    )
                })?;
            if pixels.len() != expected_len {
                return Err(Error::new(
                    windows::core::HRESULT(0x8007_0057u32 as i32),
                    "Result card bitmap buffer length does not match its dimensions",
                ));
            }
            let properties = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: self.dpi as f32,
                dpiY: self.dpi as f32,
            };
            let bitmap = self.target.CreateBitmap(
                D2D_SIZE_U { width, height },
                Some(pixels.as_ptr().cast()),
                row_stride,
                &properties,
            )?;
            self.result_panel_bitmap = Some(CachedResultPanelBitmap {
                key: key.clone(),
                bitmap,
                pixels: Arc::from(pixels),
            });
            Ok(())
        }
    }

    /// # Safety
    /// This must run on the renderer's owning UI thread with a live DC target
    /// and layered surface; the helper restores the main surface before return.
    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn rasterize_result_panel(
        &self,
        app: &AppState,
    ) -> windows::core::Result<(Vec<u8>, u32, u32)> {
        // SAFETY: All Direct2D interfaces belong to this renderer and the
        // temporary offscreen surface remains alive through BindDC/draw/read.
        unsafe {
            let dc_target = self.dc_target.as_ref().ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "The result card requires a Direct2D DC render target",
                )
            })?;
            let main_surface = self.layered_surface.as_ref().ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "The result card requires a layered main-window surface",
                )
            })?;
            let scale = composition_scale_for_dpi(self.dpi);
            let width = (CONTENT_WIDTH as f32 * scale).ceil() as u32;
            let height = (TOP_HEIGHT as f32 * scale).ceil() as u32;
            let offscreen = LayeredSurface::new(width, height)?;
            let offscreen_bounds = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            dc_target.BindDC(offscreen.dc, &offscreen_bounds)?;
            self.target.BeginDraw();
            self.target.Clear(Some(&d2d_color(0, 0, 0, 0.0)));
            let draw_result = match &app.phase {
                MainPhase::Matched { track, .. } => {
                    self.draw_result_panel(app, track, 0.0, app.arrow_edge)
                }
                MainPhase::NoMatch { .. } => self.draw_no_match_panel(0.0, false, app.arrow_edge),
                MainPhase::Offline { .. } => self.draw_no_match_panel(0.0, true, app.arrow_edge),
                _ => Err(Error::new(
                    windows::core::HRESULT(0x80070057_u32 as i32),
                    "Cannot rasterize a result card outside a result phase",
                )),
            };
            let end_result = self.target.EndDraw(None, None);
            let main_bounds = RECT {
                left: 0,
                top: 0,
                right: main_surface.size.width as i32,
                bottom: main_surface.size.height as i32,
            };
            let restore_result = dc_target.BindDC(main_surface.dc, &main_bounds);
            draw_result?;
            end_result?;
            restore_result?;

            let pixels = offscreen.pixel_bytes();
            Ok((pixels.to_vec(), width, height))
        }
    }

    /// # Safety
    /// `hwnd` must be the live owner of this renderer and this call must run on
    /// the renderer's UI thread.
    #[deny(unsafe_op_in_unsafe_fn)]
    pub(super) unsafe fn on_size(&mut self, hwnd: HWND) {
        // SAFETY: The HWND belongs to this renderer and all D2D interfaces and
        // layered DC handles are owned by `self` on this UI thread.
        unsafe {
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
    }

    /// # Safety
    /// `hwnd` must own this renderer and `app` must be the live UI state on the
    /// same thread; renderer COM resources are thread-affine.
    #[deny(unsafe_op_in_unsafe_fn)]
    pub(super) unsafe fn on_dpi_changed(&mut self, dpi: u32, hwnd: HWND, app: &mut AppState) {
        // SAFETY: The caller guarantees the HWND, AppState, and renderer belong
        // to the current UI thread; D2D/Composition operations use those live
        // objects and do not pump the window message loop.
        unsafe {
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
    }

    unsafe fn refresh_theme_if_needed(&mut self, app: &AppState) {
        if self.theme_generation != app.theme_generation {
            if let Ok(palette) = create_palette(&self.target, app.theme, app.system_foreground) {
                self.palette = palette;
                self.theme_generation = app.theme_generation;
            }
        }
    }

    unsafe fn refresh_language_if_needed(&mut self) {
        let language = ui_language();
        if self.language == language {
            return;
        }
        if self.text.refresh(&self.write_factory).is_ok() {
            self.language = language;
        }
    }

    unsafe fn prepare_history_scroll_overlay(&mut self, app: &AppState, body_top: f32) -> bool {
        // SAFETY: This renderer and its Composition/Direct2D resources are
        // confined to the UI thread for the duration of the offscreen pass.
        unsafe {
            let Some(animation) = app.history_scroll_animation else {
                if let Some(scene) = self.composition.as_mut() {
                    let _ = scene.history_scroll_overlay.hide();
                }
                self.history_scroll_overlay_failure = None;
                return false;
            };
            if app.history.is_empty() || app.history_transition.is_some() {
                if let Some(scene) = self.composition.as_mut() {
                    let _ = scene.history_scroll_overlay.hide();
                }
                self.history_scroll_overlay_failure = None;
                return false;
            }
            if self.history_scroll_overlay_failure == Some(animation.started_at) {
                return false;
            }
            if let Some(scene) = self.composition.as_mut() {
                if scene.history_scroll_overlay.matches(animation.started_at) {
                    return scene
                        .history_scroll_overlay
                        .set_viewport(self.dpi, body_top)
                        .is_ok();
                }
            }

            let now = Instant::now();
            let low = animation.from_offset.min(animation.to_offset).max(0.0);
            let high = animation.from_offset.max(animation.to_offset).max(0.0);
            let content_height = HISTORY_HEIGHT as f32 + (high - low);
            // Normal wheel gestures need at most a little more than one viewport.
            // Bound allocation for unusual system wheel settings; the Direct2D
            // path remains the safe fallback for unusually large jumps.
            if !content_height.is_finite() || content_height > HISTORY_HEIGHT as f32 * 4.0 {
                self.history_scroll_overlay_failure = Some(animation.started_at);
                return false;
            }

            let result = (|| -> windows::core::Result<()> {
                let (pixels, size) =
                    self.rasterize_history_scroll_snapshot(app, low, content_height)?;
                let (compositor, graphics_device) = {
                    let scene = self.composition.as_ref().ok_or_else(|| {
                        Error::new(
                            windows::core::HRESULT(0x80004005_u32 as i32),
                            "Windows Composition is unavailable for history scrolling",
                        )
                    })?;
                    (
                        scene.compositor.clone(),
                        scene.result_overlay._graphics_device.clone(),
                    )
                };
                let scene = self.composition.as_mut().ok_or_else(|| {
                    Error::new(
                        windows::core::HRESULT(0x80004005_u32 as i32),
                        "Windows Composition was released before history animation",
                    )
                })?;
                scene
                    .history_scroll_overlay
                    .set_viewport(self.dpi, body_top)?;
                scene.history_scroll_overlay.upload_and_start(
                    &compositor,
                    &graphics_device,
                    &pixels,
                    size,
                    content_height,
                    low,
                    animation,
                    now,
                )
            })();

            match result {
                Ok(()) => {
                    self.history_scroll_overlay_failure = None;
                    true
                }
                Err(error) => {
                    if let Some(scene) = self.composition.as_mut() {
                        let _ = scene.history_scroll_overlay.hide();
                    }
                    self.history_scroll_overlay_failure = Some(animation.started_at);
                    eprintln!("Windows UI history scroll Composition failed; using Direct2D fallback: {error}");
                    false
                }
            }
        }
    }
    pub(super) fn history_scroll_composition_active(&self) -> bool {
        self.composition
            .as_ref()
            .is_some_and(|scene| scene.history_scroll_overlay.active())
    }

    unsafe fn prepare_history_overlay(
        &mut self,
        app: &AppState,
        body_top: f32,
        now: Instant,
    ) -> bool {
        let transition = app.history_transition.filter(|transition| {
            matches!(
                transition.event,
                HistoryMutationEvent::Insert { index: 0, .. }
            ) && now.saturating_duration_since(transition.started_at) < HISTORY_INSERT_TRANSITION
        });
        let Some(transition) = transition else {
            if let Some(scene) = self.composition.as_mut() {
                let _ = scene.history_overlay.hide();
            }
            self.history_overlay_failure_transition = None;
            return false;
        };
        let key = HistoryCompositionKey {
            started_at: transition.started_at,
            scroll_offset_bits: app.scroll_offset.to_bits(),
            hovered_row: app.hovered_row,
            theme_generation: app.theme_generation,
            edge: app.arrow_edge,
        };
        let (matches, stale_transition) = self
            .composition
            .as_ref()
            .map(|scene| {
                (
                    scene.history_overlay.matches_key(key),
                    scene.history_overlay.has_transition(transition.started_at),
                )
            })
            .unwrap_or_default();
        if matches {
            return true;
        }
        if stale_transition {
            if let Some(scene) = self.composition.as_mut() {
                let _ = scene.history_overlay.hide();
            }
            self.history_overlay_failure_transition = Some(transition.started_at);
            return false;
        }
        if self.history_overlay_failure_transition == Some(transition.started_at) {
            return false;
        }

        let result = (|| -> windows::core::Result<()> {
            let row_pixels = unsafe { self.rasterize_history_insert_row(app)? };
            let previous_frame = self.layered_surface.as_ref().ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "The history transition requires the previous layered-window frame",
                )
            })?;
            let scene = self.composition.as_mut().ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "The history transition requires Windows Composition",
                )
            })?;
            unsafe {
                scene.history_overlay.capture_and_animate(
                    &scene.compositor,
                    previous_frame,
                    &row_pixels,
                    body_top + TOP_HEIGHT as f32,
                    body_top,
                    self.dpi,
                    transition,
                    key,
                    now,
                )
            }
        })();
        match result {
            Ok(()) => {
                self.history_overlay_failure_transition = None;
                true
            }
            Err(error) => {
                if let Some(scene) = self.composition.as_mut() {
                    let _ = scene.history_overlay.hide();
                }
                self.history_overlay_failure_transition = Some(transition.started_at);
                eprintln!(
                    "Windows UI history composition update failed; using Direct2D fallback: {error}"
                );
                false
            }
        }
    }

    /// # Safety
    /// The renderer's COM resources must remain valid on their owning UI
    /// thread, and no other thread may draw through this renderer concurrently.
    pub(super) unsafe fn draw(
        &mut self,
        app: &AppState,
        kind: WindowKind,
    ) -> windows::core::Result<bool> {
        // SAFETY: Renderer construction and the window dispatch path preserve
        // COM thread affinity and resource lifetime across this synchronous frame.
        unsafe {
            self.refresh_theme_if_needed(app);
            self.refresh_language_if_needed();
            let body_top = if app.arrow_edge == ArrowEdge::Top {
                POPUP_ARROW_HEIGHT as f32
            } else {
                0.0
            };
            let popup_outline = if kind == WindowKind::Main {
                Some(self.popup_outline(app)?)
            } else {
                None
            };
            if let (Some(scene), Some(outline)) =
                (self.composition.as_mut(), popup_outline.as_ref())
            {
                scene.update_popup_clip(outline, app.arrow_edge, app.arrow_center)?;
            }
            let history_insert_composition_active = kind == WindowKind::Main
                && self.prepare_history_overlay(app, body_top, Instant::now());
            let history_scroll_composition_active =
                kind == WindowKind::Main && self.prepare_history_scroll_overlay(app, body_top);
            let history_composition_active =
                history_insert_composition_active || history_scroll_composition_active;
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
            if panel_top.is_some() {
                if let Some(key) = overlay_key.as_ref() {
                    if self.result_panel_bitmap_failure_key.as_ref() != Some(key)
                        && self.cached_result_panel_bitmap(key).is_none()
                    {
                        match self.ensure_result_panel_bitmap(app, key) {
                            Ok(()) => self.result_panel_bitmap_failure_key = None,
                            Err(error) => {
                                self.result_panel_bitmap_failure_key = Some(key.clone());
                                eprintln!("Windows UI result card bitmap cache failed: {error}");
                            }
                        }
                    }
                }
            }
            let result_panel_cache = overlay_key.as_ref().and_then(|key| {
                self.result_panel_bitmap
                    .as_ref()
                    .filter(|cached| cached.key == *key)
            });
            let result_panel_bitmap = result_panel_cache.map(|cached| cached.bitmap.clone());
            let result_panel_pixels = result_panel_cache.map(|cached| Arc::clone(&cached.pixels));
            let overlay_motion = (kind == WindowKind::Main)
                .then(|| result_overlay_motion_animation(&app.phase, Instant::now()))
                .flatten();
            let overlay_motion_key = overlay_motion.as_ref().map(|motion| motion.key.clone());
            let overlay_motion_changed = COMPOSITION_RESULT_CARD_ENABLED
                && self.result_overlay_motion_key != overlay_motion_key;
            if kind == WindowKind::Main
                && self.result_overlay_failure_key.is_some()
                && (panel_top.is_none()
                    || self.composition.is_none()
                    || self.layered_surface.is_none()
                    || !result_overlay_failure_matches(
                        self.result_overlay_failure_key.as_ref(),
                        overlay_key.as_ref(),
                    ))
            {
                self.result_overlay_failure_key = None;
                self.result_overlay_failure_reported = false;
            }
            let overlay_failure_latched = kind == WindowKind::Main
                && result_overlay_failure_matches(
                    self.result_overlay_failure_key.as_ref(),
                    overlay_key.as_ref(),
                );
            let composition_cache_ready = COMPOSITION_RESULT_CARD_ENABLED
                && self.composition.is_some()
                && self.layered_surface.is_some()
                && result_panel_pixels.is_some()
                && panel_top.is_some()
                && !overlay_failure_latched;
            let composition_cache_hit = composition_cache_ready
                && overlay_key
                    .as_ref()
                    .is_some_and(|key| self.result_overlay_content_key.as_ref() == Some(key));
            // The Composition surface contains the unmasked card pixels; the
            // shared scene root supplies the popup's outer antialiased clip.
            let result_panel_draw_top = result_panel_draw_top(panel_top, composition_cache_ready);
            self.target.BeginDraw();
            let clear = d2d_color(0, 0, 0, 0.0);
            self.target.Clear(Some(&clear));
            let popup_layer_parameters = if kind == WindowKind::Main {
                Some(windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS {
                    contentBounds: rect(0.0, 0.0, CONTENT_WIDTH as f32, MAIN_WINDOW_HEIGHT as f32),
                    geometricMask: std::mem::ManuallyDrop::new(popup_outline.clone()),
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
                WindowKind::Main => self.draw_main(
                    app,
                    result_panel_draw_top,
                    history_insert_composition_active,
                    history_scroll_composition_active,
                    result_panel_bitmap.as_ref(),
                ),
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
                    let overlay_update = if !COMPOSITION_RESULT_CARD_ENABLED {
                        if self.result_overlay_content_key.is_some()
                            || self.result_overlay_motion_key.is_some()
                        {
                            scene.result_overlay.hide()
                        } else {
                            Ok(())
                        }
                    } else if overlay_failure_latched {
                        Ok(())
                    } else if result_panel_pixels.is_some() {
                        scene.update_result_overlay(
                            result_panel_pixels.as_deref(),
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
                            if overlay_failure_latched {
                                self.result_overlay_content_key = None;
                                self.result_overlay_motion_key = None;
                            } else {
                                self.result_overlay_failure_key = None;
                                self.result_overlay_content_key = if composition_cache_ready {
                                    panel_top.and(overlay_key.clone())
                                } else {
                                    None
                                };
                                self.result_overlay_motion_key = if COMPOSITION_RESULT_CARD_ENABLED
                                {
                                    overlay_motion_key.clone()
                                } else {
                                    None
                                };
                            }
                        }
                        Err(error) => {
                            // The cached Composition surface failed to update.
                            // Redraw the current frame into the layered D2D target
                            // below so the card stays visible while the fallback
                            // path is active.
                            let _ = scene.result_overlay.hide();
                            self.result_overlay_content_key = None;
                            self.result_overlay_failure_key = overlay_key.clone();
                            self.result_overlay_motion_key = None;
                            overlay_update_failed = true;
                            overlay_error = Some(error);
                        }
                    }
                    // The layered D2D fallback is behind the desktop Composition
                    // tree. Clip tagging visuals at the card's moving top edge;
                    // hiding the full group made the gear and button disappear
                    // until the card had completely left the header.
                    let fallback_panel_top = if COMPOSITION_RESULT_CARD_ENABLED {
                        result_overlay_failure_matches(
                            self.result_overlay_failure_key.as_ref(),
                            overlay_key.as_ref(),
                        )
                        .then_some(panel_top)
                        .flatten()
                    } else {
                        panel_top
                    };
                    if let Err(error) =
                        scene.set_result_fallback_tagging_clip(fallback_panel_top, body_top)
                    {
                        if overlay_error.is_none() {
                            overlay_error =
                                Some(overlay_surface_error("FallbackTaggingClip", error));
                        }
                    }
                } else {
                    self.result_overlay_content_key = None;
                    self.result_overlay_failure_key = None;
                    self.result_overlay_motion_key = None;
                }
                if overlay_update_failed && self.layered_surface.is_some() {
                    self.redraw_main_dynamic_fallback(
                        app,
                        panel_top,
                        history_insert_composition_active,
                        history_scroll_composition_active,
                    )?;
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
            Ok(history_composition_active)
        }
    }

    unsafe fn redraw_main_dynamic_fallback(
        &self,
        app: &AppState,
        panel_top: Option<f32>,
        history_insert_composition_active: bool,
        history_scroll_composition_active: bool,
    ) -> windows::core::Result<()> {
        // SAFETY: Called synchronously while the layered Direct2D target is
        // owned by this renderer on the UI thread.
        unsafe {
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
            let result_panel_bitmap = self
                .result_overlay_content_key(app, app.arrow_edge)
                .and_then(|key| self.cached_result_panel_bitmap(&key))
                .cloned();
            let draw_result = self.draw_main(
                app,
                panel_top,
                history_insert_composition_active,
                history_scroll_composition_active,
                result_panel_bitmap.as_ref(),
            );
            if self.popup_layer.is_some() {
                self.target.PopLayer();
            }
            draw_result?;
            self.target.EndDraw(None, None)?;
            Ok(())
        }
    }

    pub(super) fn set_popup_opacity(&mut self, opacity: u8) -> windows::core::Result<()> {
        if let Some(scene) = self.composition.as_mut() {
            scene.root.SetOpacity(f32::from(opacity) / 255.0)?;
        }
        Ok(())
    }

    pub(super) fn update_composition(
        &mut self,
        snapshot: AnimationSnapshot,
    ) -> windows::core::Result<()> {
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

    pub(super) fn start_startup_logo(&mut self) -> windows::core::Result<bool> {
        let Some(scene) = self.composition.as_mut() else {
            return Ok(true);
        };
        scene.start_startup_logo()?;
        Ok(scene.startup_logo_complete())
    }

    pub(super) fn startup_logo_incomplete(&self) -> bool {
        self.composition
            .as_ref()
            .is_some_and(|scene| !scene.startup_logo_complete())
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn fill_rect(&self, area: D2D_RECT_F, brush: &ID2D1SolidColorBrush) {
        // SAFETY: The target and brush are owned COM interfaces used only on
        // this renderer's UI thread; `area` remains live through the call.
        unsafe { self.target.FillRectangle(&area, brush) };
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn fill_gradient_rect(
        &self,
        area: &D2D_RECT_F,
        brush: &ID2D1LinearGradientBrush,
        top: f32,
    ) -> windows::core::Result<()> {
        // SAFETY: The brush/target belong to this renderer and the borrowed
        // rectangle/transform values stay alive for each synchronous COM call.
        unsafe {
            brush.SetTransform(&gradient_origin_transform(top));
            self.target.FillRectangle(area, brush);
            brush.SetTransform(&gradient_origin_transform(0.0));
        }
        Ok(())
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn fill_gradient_geometry(
        &self,
        geometry: &ID2D1Geometry,
        brush: &ID2D1LinearGradientBrush,
        top: f32,
    ) -> windows::core::Result<()> {
        // SAFETY: The geometry, brush, and target are live COM references owned
        // by this renderer and are used synchronously on its UI thread.
        unsafe {
            brush.SetTransform(&gradient_origin_transform(top));
            self.target.FillGeometry(geometry, brush, None);
            brush.SetTransform(&gradient_origin_transform(0.0));
        }
        Ok(())
    }

    #[deny(unsafe_op_in_unsafe_fn)]
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
        // SAFETY: UTF-16 storage, text format, rectangle, brush, and target all
        // remain valid for the synchronous Direct2D DrawText call.
        unsafe {
            self.target.DrawText(
                &content,
                format,
                &area,
                brush,
                windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn with_result_header_clip(
        &self,
        body_top: f32,
        draw: impl FnOnce() -> windows::core::Result<()>,
    ) -> windows::core::Result<()> {
        // SAFETY: The renderer's target is inside an active draw session. The
        // clip rectangle is stack-owned through Push, and Pop balances Push even
        // when the drawing closure reports an error.
        unsafe {
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
    }

    unsafe fn draw_main(
        &self,
        app: &AppState,
        result_panel_draw_top: Option<f32>,
        history_insert_composition_active: bool,
        history_scroll_composition_active: bool,
        result_panel_bitmap: Option<&ID2D1Bitmap>,
    ) -> windows::core::Result<()> {
        // SAFETY: The caller has begun a draw pass on this renderer's UI-thread
        // target; every brush, geometry, bitmap, and layer is renderer-owned or
        // borrowed from AppState for the duration of this synchronous pass.
        unsafe {
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
            if app.history.is_empty() {
                self.draw_skeleton(history_rect);
            } else if let Some(progress) =
                history_first_insert_progress(app.history_transition, now)
            {
                self.draw_skeleton_with_opacity(history_rect, 1.0 - progress);
            }
            self.draw_history(
                app,
                history_top,
                now,
                history_insert_composition_active,
                history_scroll_composition_active,
            )?;
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
                    // `taggingDidFinish` calls stopTagging before presenting the
                    // independent result card. The ready-state settings control
                    // therefore remains in the background while the card enters,
                    // holds, and slides away; the result overlay covers only the
                    // portion currently occupied by the card.
                    self.draw_ready(app, body_top);
                    if let Some(top) = result_panel_draw_top {
                        self.with_result_header_clip(body_top, || {
                            if let Some(bitmap) = result_panel_bitmap {
                                self.draw_result_panel_bitmap(bitmap, top);
                                Ok(())
                            } else {
                                self.draw_result_panel(app, track, top, app.arrow_edge)
                            }
                        })?;
                    }
                }
                MainPhase::NoMatch { .. } => {
                    if let Some(top) = result_panel_draw_top {
                        self.with_result_header_clip(body_top, || {
                            if let Some(bitmap) = result_panel_bitmap {
                                self.draw_result_panel_bitmap(bitmap, top);
                                Ok(())
                            } else {
                                self.draw_no_match_panel(top, false, app.arrow_edge)
                            }
                        })?;
                    }
                }
                MainPhase::Offline { .. } => {
                    if let Some(top) = result_panel_draw_top {
                        self.with_result_header_clip(body_top, || {
                            if let Some(bitmap) = result_panel_bitmap {
                                self.draw_result_panel_bitmap(bitmap, top);
                                Ok(())
                            } else {
                                self.draw_no_match_panel(top, true, app.arrow_edge)
                            }
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
    }

    /// Returns one cached union geometry for the rounded panel and its tray
    /// arrow. A single outer contour keeps the translucent DWM edge continuous
    /// and avoids independently rasterized edges where the arrow meets panel.
    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn popup_outline(&self, app: &AppState) -> windows::core::Result<ID2D1Geometry> {
        // SAFETY: The Direct2D factory is owned by this UI-thread renderer and
        // the resulting geometry is retained as a reference-counted COM object.
        unsafe {
            let center = popup_outline_center(app.arrow_center);
            let key = (app.arrow_edge, center.to_bits());
            if let Some((edge, center_bits, geometry)) = self.popup_outline.borrow().as_ref() {
                if *edge == key.0 && *center_bits == key.1 {
                    return Ok(geometry.clone());
                }
            }
            let geometry = create_popup_outline_geometry(&self.factory, app.arrow_edge, center)?;
            *self.popup_outline.borrow_mut() = Some((key.0, key.1, geometry.clone()));
            Ok(geometry)
        }
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn popup_header(&self, app: &AppState) -> windows::core::Result<ID2D1Geometry> {
        // SAFETY: Path geometry/sink objects are created from this renderer's
        // live UI-thread factory, and every borrowed point remains live through
        // its synchronous COM call; the sink is closed before return.
        unsafe {
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
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn popup_arrow_geometry(
        &self,
        edge: ArrowEdge,
        center: f32,
    ) -> windows::core::Result<ID2D1Geometry> {
        // SAFETY: The factory belongs to this renderer and the helper closes
        // its geometry sink before returning an owned COM geometry.
        unsafe { create_popup_arrow_geometry(&self.factory, edge, center) }
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn combine_geometry(
        &self,
        first: &ID2D1Geometry,
        second: &ID2D1Geometry,
        mode: windows::Win32::Graphics::Direct2D::D2D1_COMBINE_MODE,
    ) -> windows::core::Result<ID2D1Geometry> {
        // SAFETY: All geometries and the factory are live COM references on the
        // same UI thread; the combine sink is closed before its geometry escapes.
        unsafe {
            let combined = self.factory.CreatePathGeometry()?;
            let sink = combined.Open()?;
            first.CombineWithGeometry(second, mode, None, 0.25, &sink)?;
            sink.Close()?;
            combined.cast()
        }
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn draw_ready(&self, app: &AppState, body_top: f32) {
        // SAFETY: These helpers draw into the active renderer target using
        // resources owned by this renderer on its UI thread.
        unsafe {
            self.draw_status_text(app, body_top);
            self.draw_settings_icon(body_top, app.arrow_edge);
        }
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn draw_status_text(&self, app: &AppState, body_top: f32) {
        // SAFETY: The renderer target, brushes, and text formats remain alive
        // for these synchronous drawing calls inside the active D2D draw pass.
        unsafe {
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
                    &self.text.styles.centered_title,
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
                    &self.text.styles.centered_title,
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
                &self.text.styles.centered_title,
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
                &self.text.styles.centered_title,
                &self.palette.white_ink,
            );
            self.target.PopAxisAlignedClip();
        }
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn draw_listening_status(&self, app: &AppState, body_top: f32, mode: CaptureMode) {
        // SAFETY: The active renderer target, brush, and text formats are owned
        // by this renderer and remain live during this UI-thread draw pass.
        unsafe {
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
                &self.text.styles.centered_title,
                &self.palette.white_ink,
            );
        }
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn draw_capture_pending(&self, body_top: f32, mode: CaptureMode, cancelling: bool) {
        // SAFETY: Draw into this renderer's active target with owned text and brush resources.
        unsafe {
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
                &self.text.styles.centered_title,
                &self.palette.white_ink,
            );
        }
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn draw_cancel_button(&self, body_top: f32, edge: ArrowEdge) {
        // SAFETY: The target and cached close icon are live COM resources owned
        // by this renderer and used on its UI thread.
        unsafe {
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
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn draw_settings_icon(&self, body_top: f32, edge: ArrowEdge) {
        // SAFETY: The target and cached settings icon are live COM resources
        // owned by this renderer and used on its UI thread.
        unsafe {
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
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn draw_skeleton(&self, history: D2D_RECT_F) {
        // SAFETY: Brushes and target belong to this renderer; each local
        // geometry value is borrowed only for its synchronous drawing call.
        unsafe {
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
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn draw_skeleton_with_opacity(&self, history: D2D_RECT_F, opacity: f32) {
        // SAFETY: The renderer owns the target/layer and runs this helper on
        // the serialized UI-thread drawing pass.
        unsafe {
            let opacity = opacity.clamp(0.0, 1.0);
            if opacity >= 1.0 {
                self.draw_skeleton(history);
                return;
            }
            if opacity <= 0.0 {
                return;
            }
            let Some(layer) = self.history_skeleton_layer.as_ref() else {
                self.draw_skeleton(history);
                return;
            };
            let parameters = windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS {
                contentBounds: history,
                geometricMask: std::mem::ManuallyDrop::new(None),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2 {
                    M11: 1.0,
                    M22: 1.0,
                    ..Default::default()
                },
                opacity,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS_NONE,
            };
            self.target.PushLayer(&parameters, layer);
            self.draw_skeleton(history);
            self.target.PopLayer();
        }
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn draw_track_artwork(
        &self,
        app: &AppState,
        track: &Track,
        destination: D2D_RECT_F,
        side: f32,
    ) -> bool {
        // SAFETY: Artwork bytes remain owned by AppState through this call and
        // their pixel dimensions/stride are validated before CreateBitmap.
        // The target and bitmap cache are used on the renderer's UI thread.
        unsafe {
            let Some((source, pixels)) = app.artwork_resources.get(&track.display_id) else {
                return false;
            };
            let Some(row_stride) = artwork_pixels_row_stride(pixels) else {
                return false;
            };
            let mut bitmaps = self.artwork_bitmaps.borrow_mut();
            let replace_cached = bitmaps.get(&track.display_id).is_none_or(|cached| {
                should_replace_cached_artwork(
                    Some((
                        cached.source,
                        cached.width,
                        cached.height,
                        cached.fingerprint,
                    )),
                    *source,
                    pixels.width,
                    pixels.height,
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
                    row_stride,
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
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn draw_fallback_match_artwork(&self, theme: Theme, frame: D2D_RECT_F) {
        // SAFETY: Both fallback bitmaps and the target are owned by this
        // renderer and remain alive for their synchronous draw call.
        unsafe {
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
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn draw_artwork_bitmap(
        &self,
        bitmap: &ID2D1Bitmap,
        frame: D2D_RECT_F,
        image_rect: D2D_RECT_F,
        side: f32,
    ) -> bool {
        // SAFETY: The bitmap, mask, layer, and target are reference-counted COM
        // objects owned by this renderer and used on its UI thread; local layer
        // parameters and rectangles outlive each synchronous call.
        unsafe {
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
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn draw_history(
        &self,
        app: &AppState,
        history_top: f32,
        now: Instant,
        insert_composition_active: bool,
        scroll_composition_active: bool,
    ) -> windows::core::Result<()> {
        // SAFETY: The active D2D target and all row resources belong to this
        // UI-thread renderer; AppState/history are borrowed for the draw only.
        unsafe {
            // Render the history rows through exactly one path at a time. Drawing
            // Direct2D rows under the moving Composition rows creates doubled,
            // vertically offset text/artwork while the two clocks are sampled at
            // different frame times. The base history background remains in the
            // D2D frame; Composition owns the rows whenever its scroll surface is
            // active. If surface preparation fails, the caller leaves this false
            // and Direct2D renders the complete list as the fallback.
            if scroll_composition_active
                || (insert_composition_active && app.history_transition.is_some())
            {
                return Ok(());
            }
            for (index, track) in app.history.iter().enumerate() {
                let top = history_row_top(
                    index,
                    app.history_transition,
                    history_top,
                    app.scroll_offset,
                    now,
                );
                if top + HISTORY_ROW_HEIGHT < history_top
                    || top > history_top + HISTORY_HEIGHT as f32
                {
                    continue;
                }
                self.draw_history_row(app, index, track, top)?;
            }
            Ok(())
        }
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn draw_history_row(
        &self,
        app: &AppState,
        index: usize,
        track: &Track,
        top: f32,
    ) -> windows::core::Result<()> {
        // SAFETY: Track/AppState strings and artwork remain live during the
        // synchronous D2D calls, and all target/brush resources are renderer-owned.
        unsafe {
            let row = rect(0.0, top, CONTENT_WIDTH as f32, top + HISTORY_ROW_HEIGHT);
            let text_metrics = self.text.history_metrics(&self.write_factory, track)?;
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
                    &self.text.styles.centered_title,
                    &self.palette.white_ink,
                );
            }
            self.text(
                &track.title,
                text_rects.title,
                &self.text.styles.title,
                &self.palette.title,
            );
            self.text(
                &track.artist,
                text_rects.artist,
                &self.text.styles.subtitle,
                &self.palette.subtitle,
            );
            self.text(
                &track.recognized_date_label,
                text_rects.date,
                &self.text.styles.date,
                &self.palette.subtitle,
            );
            self.fill_rect(
                rect(10.0, top + 88.0, 370.0, top + HISTORY_ROW_HEIGHT),
                &self.palette.separator,
            );
            let _recognized_age = track.recognized_at.elapsed();
            Ok(())
        }
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn rasterize_history_insert_row(
        &mut self,
        app: &AppState,
    ) -> windows::core::Result<Vec<u8>> {
        // SAFETY: The renderer target is UI-thread-affine and the first track
        // remains borrowed for the complete synchronous offscreen raster pass.
        unsafe {
            let track = app.history.first().ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x80070057_u32 as i32),
                    "Cannot rasterize an empty history row",
                )
            })?;
            self.rasterize_history_row(app, 0, track)
        }
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn rasterize_history_row(
        &mut self,
        app: &AppState,
        index: usize,
        track: &Track,
    ) -> windows::core::Result<Vec<u8>> {
        // SAFETY: Both DC surfaces and the D2D target are owned by this renderer;
        // the temporary surface remains alive through rendering and pixel copy.
        unsafe {
            let dc_target = self.dc_target.as_ref().ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "The history row requires a Direct2D DC render target",
                )
            })?;
            let main_surface = self.layered_surface.as_ref().ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "The history row requires a layered main-window surface",
                )
            })?;
            let scale = composition_scale_for_dpi(self.dpi);
            let width = (CONTENT_WIDTH as f32 * scale).ceil() as u32;
            let height = (HISTORY_ROW_PITCH * scale).ceil() as u32;
            let offscreen = LayeredSurface::new(width, height)?;
            let offscreen_bounds = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            dc_target.BindDC(offscreen.dc, &offscreen_bounds)?;
            self.target.BeginDraw();
            self.target.Clear(Some(&d2d_color(0, 0, 0, 0.0)));
            let draw_result = self.draw_history_row(app, index, track, 0.0);
            let end_result = self.target.EndDraw(None, None);
            let main_bounds = RECT {
                left: 0,
                top: 0,
                right: main_surface.size.width as i32,
                bottom: main_surface.size.height as i32,
            };
            let restore_result = dc_target.BindDC(main_surface.dc, &main_bounds);
            draw_result?;
            end_result?;
            restore_result?;

            let pixels = offscreen.pixel_bytes();
            Ok(pixels.to_vec())
        }
    }

    #[deny(unsafe_op_in_unsafe_fn)]
    unsafe fn rasterize_history_scroll_snapshot(
        &mut self,
        app: &AppState,
        origin_offset: f32,
        content_height: f32,
    ) -> windows::core::Result<(Vec<u8>, D2D_SIZE_U)> {
        // SAFETY: The target and layered surfaces belong to this UI-thread
        // renderer. The temporary buffer remains alive while its pixels are read.
        unsafe {
            let dc_target = self.dc_target.as_ref().ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "The history scroll snapshot requires a Direct2D DC render target",
                )
            })?;
            let main_surface = self.layered_surface.as_ref().ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "The history scroll snapshot requires the main layered surface",
                )
            })?;
            let scale = composition_scale_for_dpi(self.dpi);
            let size = D2D_SIZE_U {
                width: (CONTENT_WIDTH as f32 * scale).ceil().max(1.0) as u32,
                height: (content_height * scale).ceil().max(1.0) as u32,
            };
            let offscreen = LayeredSurface::new(size.width, size.height)?;
            let offscreen_bounds = RECT {
                left: 0,
                top: 0,
                right: size.width as i32,
                bottom: size.height as i32,
            };
            dc_target.BindDC(offscreen.dc, &offscreen_bounds)?;
            self.target.BeginDraw();
            self.target.Clear(Some(&d2d_color(0, 0, 0, 0.0)));
            let visible_end = origin_offset + content_height;
            let first = (origin_offset.max(0.0) / HISTORY_ROW_PITCH).floor() as usize;
            let last = (visible_end.max(0.0) / HISTORY_ROW_PITCH)
                .ceil()
                .min(app.history.len() as f32) as usize;
            let draw_result = (|| -> windows::core::Result<()> {
                for index in first..last {
                    let track = &app.history[index];
                    let row_top = index as f32 * HISTORY_ROW_PITCH - origin_offset;
                    if row_top + HISTORY_ROW_HEIGHT < 0.0 || row_top > content_height {
                        continue;
                    }
                    self.draw_history_row(app, index, track, row_top)?;
                }
                Ok(())
            })();
            let end_result = self.target.EndDraw(None, None);
            let main_bounds = RECT {
                left: 0,
                top: 0,
                right: main_surface.size.width as i32,
                bottom: main_surface.size.height as i32,
            };
            let restore_result = dc_target.BindDC(main_surface.dc, &main_bounds);
            draw_result?;
            end_result?;
            restore_result?;

            let pixels = offscreen.pixel_bytes();
            Ok((pixels.to_vec(), size))
        }
    }

    unsafe fn draw_result_panel(
        &self,
        app: &AppState,
        track: &Track,
        top: f32,
        edge: ArrowEdge,
    ) -> windows::core::Result<()> {
        // SAFETY: The caller is inside an active UI-thread draw pass and all
        // drawing resources are owned by this renderer or borrowed for this pass.
        unsafe {
            let panel = rect(0.0, top, CONTENT_WIDTH as f32, top + TOP_HEIGHT as f32);
            let gradient = if edge == ArrowEdge::Top {
                &self.palette.result_top
            } else {
                &self.palette.result_bottom
            };
            self.fill_gradient_rect(&panel, gradient, top)?;
            let details = self
                .text
                .result_details_metrics(&self.write_factory, track)?;
            let detail_rects =
                result_details_rects(top, details.title_height, details.artist_height);
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
                &self.text.styles.result_title,
                &self.palette.result_label,
            );
            self.text(
                &track.artist,
                detail_rects.artist,
                &self.text.styles.result_artist,
                &self.palette.result_label,
            );
            self.draw_result_close_icon(top, edge);
            Ok(())
        }
    }

    unsafe fn draw_result_panel_bitmap(&self, bitmap: &ID2D1Bitmap, top: f32) {
        // SAFETY: The bitmap and target are live Direct2D objects owned by or
        // borrowed into the current renderer draw pass on its UI thread.
        unsafe {
            let destination = rect(0.0, top, CONTENT_WIDTH as f32, top + TOP_HEIGHT as f32);
            self.target.DrawBitmap(
                bitmap,
                Some(&destination),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
    }

    unsafe fn draw_no_match_panel(
        &self,
        top: f32,
        offline: bool,
        edge: ArrowEdge,
    ) -> windows::core::Result<()> {
        // SAFETY: The caller is inside an active UI-thread draw pass and the
        // target's brushes/text formats remain owned by this renderer.
        unsafe {
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
                &self.text.styles.centered_heavy,
                &self.palette.title,
            );
            self.text(
                body,
                rect(15.0, top + 84.0, 365.0, top + 110.0),
                &self.text.styles.centered_body,
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
                &self.text.styles.button,
                &self.palette.white_ink,
            );
            self.draw_result_close_icon(top, edge);
            Ok(())
        }
    }

    unsafe fn draw_result_close_icon(&self, body_top: f32, edge: ArrowEdge) {
        // SAFETY: The close bitmap and Direct2D target remain valid for this
        // synchronous call on the renderer's owning UI thread.
        unsafe {
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
    }

    unsafe fn draw_error_banner(&self, message: &str, body_top: f32) {
        // SAFETY: The target and all drawing resources are renderer-owned and
        // the message is borrowed only for this synchronous UI-thread pass.
        unsafe {
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
                &self.text.styles.centered_body,
                &self.palette.white_ink,
            );
        }
    }

    unsafe fn draw_timeout_dialog(&self, body_top: f32) {
        // SAFETY: All COM drawing resources are valid and UI-thread-affine for
        // the duration of this synchronous dialog render.
        unsafe {
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
                &self.text.styles.centered_body,
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
                &self.text.styles.button,
                &self.palette.white_ink,
            );
            self.text(
                dismiss,
                rect(200.0, body_top + 126.0, 310.0, body_top + 160.0),
                &self.text.styles.button,
                &self.palette.title,
            );
        }
    }
}
