#![deny(unsafe_op_in_unsafe_fn)]

//! Composition geometry, Shazam logo, and button visual primitives.
use super::{composition_color, rect, ArrowEdge, Theme};
use crate::ui_model::{
    self, CubicBezier, BUTTON_SIZE, BUTTON_TOP, BUTTON_X, CONTENT_HEIGHT, CONTENT_WIDTH,
    POPUP_ARROW_HEIGHT, POPUP_CORNER_RADIUS, TOP_HEIGHT,
};
use std::time::{Duration, Instant};
use windows::{
    core::{implement, Error, Interface, Ref, HSTRING},
    Foundation::TimeSpan,
    Graphics::IGeometrySource2D,
    Win32::{
        Graphics::Direct2D::{
            Common::{
                D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW,
                D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN,
            },
            ID2D1Factory, ID2D1Geometry, D2D1_COMBINE_MODE_EXCLUDE, D2D1_COMBINE_MODE_UNION,
            D2D1_ELLIPSE,
        },
        System::WinRT::Graphics::Direct2D::{
            IGeometrySource2DInterop, IGeometrySource2DInterop_Impl,
        },
    },
    UI::{
        Color,
        Composition::{
            CompositionBitmapInterpolationMode, CompositionBorderMode, CompositionColorBrush,
            CompositionEllipseGeometry, CompositionGeometricClip, CompositionPath,
            CompositionPathGeometry, CompositionStretch, CompositionSurfaceBrush,
            CompositionVisualSurface, Compositor, ContainerVisual, CubicBezierEasingFunction,
            DropShadow, ScalarKeyFrameAnimation, ShapeVisual, SpriteVisual,
            Vector2KeyFrameAnimation, Vector3KeyFrameAnimation,
        },
    },
};
use windows_numerics::{Vector2, Vector3};

#[implement(IGeometrySource2D, IGeometrySource2DInterop)]
pub(super) struct D2DCompositionGeometrySource {
    geometry: ID2D1Geometry,
}

impl D2DCompositionGeometrySource {
    pub(super) fn new(geometry: ID2D1Geometry) -> Self {
        Self { geometry }
    }
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
pub(super) struct CompositionLogo {
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

pub(super) const SHZ_LOGO_SOURCE_SIZE: f32 = 210.0;
pub(super) const SHZ_LOGO_CANVAS_PADDING: f32 = 36.0;
pub(super) const SHZ_LOGO_CANVAS_SIZE: f32 = SHZ_LOGO_SOURCE_SIZE + SHZ_LOGO_CANVAS_PADDING * 2.0;
pub(super) const SHZ_LOGO_REFERENCE_SIZE: f32 = 367.5;
pub(super) const SHZ_LOGO_STROKE_WIDTH: f32 = 38.0;

pub(super) fn composition_scale_for_dpi(dpi: u32) -> f32 {
    dpi.max(96) as f32 / 96.0
}

fn composition_pixel_phase(dip: f32, scale: f32) -> f32 {
    ((dip * scale).round() - dip * scale) / scale
}

pub(super) fn composition_button_pixel_snap_offset(dpi: u32, body_top: f32) -> Vector3 {
    let scale = composition_scale_for_dpi(dpi);
    Vector3 {
        X: composition_pixel_phase((BUTTON_X + BUTTON_SIZE * 0.5) as f32, scale),
        Y: composition_pixel_phase(body_top + (BUTTON_TOP + BUTTON_SIZE * 0.5) as f32, scale),
        Z: 0.0,
    }
}

pub(super) fn composition_header_viewport_offset(dpi: u32, body_top: f32) -> Vector3 {
    let root_snap = composition_button_pixel_snap_offset(dpi, body_top);
    Vector3 {
        X: -root_snap.X,
        Y: body_top - root_snap.Y,
        Z: 0.0,
    }
}

pub(super) fn result_overlay_child_offset(panel_top: f32, body_top: f32) -> Vector3 {
    Vector3 {
        X: 0.0,
        Y: panel_top - body_top,
        Z: 0.0,
    }
}

#[cfg(test)]
pub(super) fn result_header_visible_height(panel_top: f32, body_top: f32) -> f32 {
    let header_top = body_top;
    let header_bottom = body_top + TOP_HEIGHT as f32;
    let card_top = panel_top.max(header_top);
    let card_bottom = (panel_top + TOP_HEIGHT as f32).min(header_bottom);
    (card_bottom - card_top).max(0.0)
}

pub(super) fn result_header_hit_test_contains(y: f32, body_top: f32) -> bool {
    (body_top..body_top + TOP_HEIGHT as f32).contains(&y)
}

pub(super) fn create_scale_transition(
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

pub(super) fn create_scalar_transition(
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

pub(super) fn create_vector2_scale_transition(
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
pub(super) struct ShzLogoLayout {
    pub(super) center_x: f32,
    pub(super) center_y: f32,
    pub(super) offset_x: f32,
    pub(super) offset_y: f32,
    pub(super) scale_x: f32,
    pub(super) scale_y: f32,
}

pub(super) fn shz_logo_layout(button_width: f32, button_height: f32) -> ShzLogoLayout {
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
pub(super) fn shz_logo_point_in_button(layout: ShzLogoLayout, point: (f32, f32)) -> (f32, f32) {
    (
        layout.center_x
            + (point.0 + SHZ_LOGO_CANVAS_PADDING - SHZ_LOGO_CANVAS_SIZE * 0.5) * layout.scale_x,
        layout.center_y
            + (point.1 + SHZ_LOGO_CANVAS_PADDING - SHZ_LOGO_CANVAS_SIZE * 0.5) * layout.scale_y,
    )
}

impl CompositionLogo {
    pub(super) fn visual(&self) -> &ContainerVisual {
        &self.visual
    }

    pub(super) fn is_complete(&self) -> bool {
        self.stage == ui_model::StartupLogoStage::Complete
    }

    /// # Safety
    /// The Direct2D factory and Composition compositor must be live on the
    /// current UI thread for the lifetime of the generated geometries.
    pub(super) unsafe fn new(
        d2d_factory: &ID2D1Factory,
        compositor: &Compositor,
        theme: Theme,
        animate_on_create: bool,
    ) -> windows::core::Result<Self> {
        let transparent_brush =
            compositor.CreateColorBrushWithColor(logo_transparent_color(theme))?;
        let solid_brush =
            compositor.CreateColorBrushWithColor(composition_color(255, 255, 255, 255))?;
        // SAFETY: Both native factories are live and confined to this UI thread.
        let (transparent_visual, transparent_geometries) = unsafe {
            make_logo_visual(
                compositor,
                d2d_factory,
                [SHZ_LOGO_TOP_PATH, SHZ_LOGO_BOTTOM_PATH],
                &transparent_brush,
                LogoTrim::Start,
            )?
        };
        // SAFETY: Same factory and UI-thread lifetime conditions as above.
        let (long_visual, long_geometries) = unsafe {
            make_logo_visual(
                compositor,
                d2d_factory,
                [SHZ_LOGO_TOP_LONG_PATH, SHZ_LOGO_BOTTOM_LONG_PATH],
                &solid_brush,
                LogoTrim::StartAndEnd,
            )?
        };
        // SAFETY: Same factory and UI-thread lifetime conditions as above.
        let (final_visual, _) = unsafe {
            make_logo_visual(
                compositor,
                d2d_factory,
                [SHZ_LOGO_TOP_PATH, SHZ_LOGO_BOTTOM_PATH],
                &solid_brush,
                LogoTrim::None,
            )?
        };
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

    pub(super) fn start(&mut self) -> windows::core::Result<()> {
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

    pub(super) fn advance(&mut self) -> windows::core::Result<bool> {
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

    pub(super) fn set_theme(&self, theme: Theme) -> windows::core::Result<()> {
        self.transparent_brush
            .SetColor(logo_transparent_color(theme))?;
        self.transparent_shadow.SetColor(logo_shadow_color(theme))
    }
}

#[derive(Clone, Copy)]
pub(super) struct LogoBezier {
    control1: (f32, f32),
    control2: (f32, f32),
    end: (f32, f32),
}

#[derive(Clone, Copy)]
pub(super) struct LogoPath<const N: usize> {
    pub(super) start: (f32, f32),
    pub(super) curves: [LogoBezier; N],
}

pub(super) const SHZ_LOGO_TOP_PATH: LogoPath<4> = LogoPath {
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

pub(super) const SHZ_LOGO_BOTTOM_PATH: LogoPath<4> = LogoPath {
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

pub(super) const SHZ_LOGO_TOP_LONG_PATH: LogoPath<8> = LogoPath {
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

pub(super) const SHZ_LOGO_BOTTOM_LONG_PATH: LogoPath<8> = LogoPath {
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
    // SAFETY: The supplied Direct2D factory is live on this UI thread and each
    // fixed-size path contains finite coordinates owned for the call.
    let geometries = unsafe {
        [
            make_logo_path_geometry(compositor, d2d_factory, paths[0], trim)?,
            make_logo_path_geometry(compositor, d2d_factory, paths[1], trim)?,
        ]
    };
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

pub(super) fn button_border_arc_points() -> [(f32, f32); 4] {
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

pub(super) unsafe fn make_button_border_arc_geometry(
    compositor: &Compositor,
    d2d_factory: &ID2D1Factory,
) -> windows::core::Result<CompositionPathGeometry> {
    let [start, control1, control2, end] = button_border_arc_points();
    // SAFETY: The factory is live on the caller's UI thread; the path geometry
    // and sink are owned COM interfaces and all coordinates are finite constants.
    let geometry = unsafe { d2d_factory.CreatePathGeometry()? };
    let sink = unsafe { geometry.Open()? };
    unsafe {
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
    }
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
    // SAFETY: The factory is live on the UI thread; `path` owns its fixed-size
    // curve data for this call and the geometry/sink COM interfaces stay alive.
    let geometry = unsafe { d2d_factory.CreatePathGeometry()? };
    let sink = unsafe { geometry.Open()? };
    unsafe {
        sink.BeginFigure(
            Vector2 {
                X: path.start.0,
                Y: path.start.1,
            },
            D2D1_FIGURE_BEGIN_HOLLOW,
        );
    }
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
    unsafe {
        sink.AddBeziers(&beziers);
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        sink.Close()?;
    }

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

pub(super) fn composition_auto_shadow_color(theme: Theme) -> Color {
    // The native AutoShadow background uses tagButtonBackgroundTransparent.
    logo_transparent_color(theme)
}

fn logo_shadow_color(_theme: Theme) -> Color {
    // Native uses the tagButtonTopBorder color. The dark appearance rendition
    // is still unmeasured; the current UI uses the confirmed light alpha/color.
    composition_color(51, 255, 255, 255)
}

pub(super) const INNER_MASK_BOUNDS: f32 = 336.0;
pub(super) const BUTTON_CLEAR_DIAMETER: f32 = 96.0;
pub(super) const AUTO_SHADOW_DISPLAY_BOUNDS: f32 = BUTTON_CLEAR_DIAMETER;
pub(super) const INNER_MASK_GEOMETRY_TOLERANCE: f32 = 0.01;

pub(super) fn auto_shadow_host_center() -> Vector2 {
    let center = AUTO_SHADOW_DISPLAY_BOUNDS * 0.5;
    Vector2 {
        X: center,
        Y: center,
    }
}

pub(super) fn inverse_circle_clip_radii(size: f32, hole: f32) -> Option<(f32, f32)> {
    (size.is_finite() && hole.is_finite() && size > 0.0 && hole >= 0.0 && hole < size)
        .then_some((size * 0.5, hole * 0.5))
}

pub(super) unsafe fn make_inverse_circle_clip(
    compositor: &Compositor,
    d2d_factory: &ID2D1Factory,
    size: f32,
    hole: f32,
) -> windows::core::Result<CompositionGeometricClip> {
    // SAFETY: The caller supplies the live UI-thread factories and this helper
    // builds its center from the validated finite `size` value.
    unsafe {
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
}

pub(super) unsafe fn make_inverse_circle_clip_at(
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
    // SAFETY: The live factory creates owned geometry objects from finite,
    // bounded radii and center values validated above.
    let outer = unsafe {
        d2d_factory.CreateEllipseGeometry(&D2D1_ELLIPSE {
            point: Vector2 {
                X: center.X,
                Y: center.Y,
            },
            radiusX: outer_radius,
            radiusY: outer_radius,
        })?
    };
    // SAFETY: Same validated geometry bounds and factory lifetime as `outer`.
    let inner = unsafe {
        d2d_factory.CreateEllipseGeometry(&D2D1_ELLIPSE {
            point: Vector2 {
                X: center.X,
                Y: center.Y,
            },
            radiusX: inner_radius,
            radiusY: inner_radius,
        })?
    };
    // SAFETY: The Direct2D factory remains live on the caller's UI thread.
    let geometry = unsafe { d2d_factory.CreatePathGeometry()? };
    // SAFETY: The path geometry is newly created and exclusively opened here.
    let sink = unsafe { geometry.Open()? };
    // This mask cuts a circle exactly at the Shazam button edge. The default
    // 0.25-DIP flattening tolerance can leave visible facets at 125% DPI when
    // the translucent listening ripples meet the button's translucent fill.
    // SAFETY: Both source geometries and the open sink are live COM interfaces;
    // the finite tolerance is fixed and the sink remains alive through Close.
    unsafe {
        outer.CombineWithGeometry(
            &inner,
            D2D1_COMBINE_MODE_EXCLUDE,
            None,
            INNER_MASK_GEOMETRY_TOLERANCE,
            &sink,
        )?;
        sink.Close()?;
    }

    let geometry: ID2D1Geometry = geometry.cast()?;
    let source: IGeometrySource2D = D2DCompositionGeometrySource { geometry }.into();
    let path = CompositionPath::Create(&source)?;
    let geometry = compositor.CreatePathGeometryWithPath(&path)?;
    compositor.CreateGeometricClipWithGeometry(&geometry)
}

pub(super) fn make_auto_shadow_outline_mask(
    compositor: &Compositor,
    bounds: f32,
    stroke_width: f32,
) -> windows::core::Result<(
    ShapeVisual,
    CompositionVisualSurface,
    CompositionSurfaceBrush,
)> {
    // This off-tree shape is the alpha source for the native clear outer
    // stroke's shadow. It is not drawn as a visible white outline.
    let source = compositor.CreateShapeVisual()?;
    source.SetBorderMode(CompositionBorderMode::Soft)?;
    source.SetSize(Vector2 {
        X: bounds,
        Y: bounds,
    })?;
    let geometry: CompositionEllipseGeometry = compositor.CreateEllipseGeometry()?;
    geometry.SetCenter(Vector2 {
        X: bounds * 0.5,
        Y: bounds * 0.5,
    })?;
    geometry.SetRadius(Vector2 {
        X: bounds * 0.5,
        Y: bounds * 0.5,
    })?;
    let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
    let mask_color = compositor.CreateColorBrushWithColor(composition_color(255, 255, 255, 255))?;
    shape.SetStrokeBrush(&mask_color)?;
    shape.SetStrokeThickness(stroke_width)?;
    source.Shapes()?.Append(&shape)?;

    let surface = compositor.CreateVisualSurface()?;
    surface.SetSourceVisual(&source)?;
    surface.SetSourceOffset(Vector2 { X: 0.0, Y: 0.0 })?;
    surface.SetSourceSize(Vector2 {
        X: bounds,
        Y: bounds,
    })?;
    let mask_brush = compositor.CreateSurfaceBrushWithSurface(&surface)?;
    mask_brush.SetStretch(CompositionStretch::Fill)?;
    mask_brush.SetBitmapInterpolationMode(CompositionBitmapInterpolationMode::Linear)?;
    Ok((source, surface, mask_brush))
}

pub(super) fn popup_outline_center(arrow_center: f32) -> f32 {
    arrow_center.clamp(14.0, CONTENT_WIDTH as f32 - 14.0)
}

pub(super) unsafe fn create_popup_arrow_geometry(
    d2d_factory: &ID2D1Factory,
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
    // SAFETY: The caller provides a live factory; `center` and the popup
    // dimensions are finite layout values used only to build owned geometry.
    let arrow = unsafe { d2d_factory.CreatePathGeometry()? };
    let sink = unsafe { arrow.Open()? };
    unsafe {
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
    }
    arrow.cast()
}

pub(super) unsafe fn create_popup_outline_geometry(
    d2d_factory: &ID2D1Factory,
    edge: ArrowEdge,
    center: f32,
) -> windows::core::Result<ID2D1Geometry> {
    let body_top = if edge == ArrowEdge::Top {
        POPUP_ARROW_HEIGHT as f32
    } else {
        0.0
    };
    let body_bottom = body_top + CONTENT_HEIGHT as f32;
    // SAFETY: The caller provides a live factory and this rectangle uses the
    // bounded fixed popup dimensions and corner radius.
    let rounded = unsafe {
        d2d_factory.CreateRoundedRectangleGeometry(
            &windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                rect: rect(0.0, body_top, CONTENT_WIDTH as f32, body_bottom),
                radiusX: POPUP_CORNER_RADIUS as f32,
                radiusY: POPUP_CORNER_RADIUS as f32,
            },
        )?
    };
    // SAFETY: The factory remains live and the arrow geometry uses the same
    // validated popup center.
    let arrow = unsafe { create_popup_arrow_geometry(d2d_factory, edge, center)? };
    let union = unsafe { d2d_factory.CreatePathGeometry()? };
    let sink = unsafe { union.Open()? };
    // SAFETY: Both inputs and the open destination sink remain live throughout
    // the synchronous boolean geometry operation.
    unsafe {
        rounded.CombineWithGeometry(&arrow, D2D1_COMBINE_MODE_UNION, None, 0.25, &sink)?;
        sink.Close()?;
    }
    union.cast()
}

pub(super) unsafe fn create_composition_clip_from_d2d_geometry(
    compositor: &Compositor,
    geometry: &ID2D1Geometry,
) -> windows::core::Result<CompositionGeometricClip> {
    let source: IGeometrySource2D = D2DCompositionGeometrySource {
        geometry: geometry.clone(),
    }
    .into();
    let path = CompositionPath::Create(&source)?;
    let path_geometry = compositor.CreatePathGeometryWithPath(&path)?;
    compositor.CreateGeometricClipWithGeometry(&path_geometry)
}

pub(super) struct AutoShadowCircleVisuals {
    pub(super) clipped_layer: ContainerVisual,
    pub(super) background: SpriteVisual,
    pub(super) shadow: SpriteVisual,
    pub(super) circle_clip: CompositionGeometricClip,
}

pub(super) fn make_auto_shadow_circle_visuals(
    compositor: &Compositor,
    bounds: f32,
    background_brush: &CompositionColorBrush,
    shadow_brush: &CompositionColorBrush,
    shadow: &DropShadow,
    outer_mask: &CompositionSurfaceBrush,
) -> windows::core::Result<AutoShadowCircleVisuals> {
    // The native background layer clips both its fill and outer-shadow child
    // to its circular bounds. Keep the fill vector-clipped to avoid bitmap
    // minification on the high-contrast edge around the button.
    let geometry: CompositionEllipseGeometry = compositor.CreateEllipseGeometry()?;
    geometry.SetCenter(Vector2 {
        X: bounds * 0.5,
        Y: bounds * 0.5,
    })?;
    geometry.SetRadius(Vector2 {
        X: bounds * 0.5,
        Y: bounds * 0.5,
    })?;
    let circle_clip = compositor.CreateGeometricClipWithGeometry(&geometry)?;
    let clipped_layer = compositor.CreateContainerVisual()?;
    clipped_layer.SetSize(Vector2 {
        X: bounds,
        Y: bounds,
    })?;
    clipped_layer.SetBorderMode(CompositionBorderMode::Soft)?;
    clipped_layer.SetClip(&circle_clip)?;

    shadow.SetMask(outer_mask)?;
    let background = compositor.CreateSpriteVisual()?;
    background.SetSize(Vector2 {
        X: bounds,
        Y: bounds,
    })?;
    background.SetBorderMode(CompositionBorderMode::Soft)?;
    background.SetBrush(background_brush)?;
    clipped_layer.Children()?.InsertAtTop(&background)?;

    // The native outer shape is a sublayer of the background and therefore
    // composes over its fill. Its SpriteVisual carries only the custom shadow
    // mask, not a visible stroke.
    let shadow_visual = compositor.CreateSpriteVisual()?;
    shadow_visual.SetSize(Vector2 {
        X: bounds,
        Y: bounds,
    })?;
    shadow_visual.SetBorderMode(CompositionBorderMode::Soft)?;
    shadow_visual.SetBrush(shadow_brush)?;
    shadow_visual.SetShadow(shadow)?;
    clipped_layer.Children()?.InsertAtTop(&shadow_visual)?;

    Ok(AutoShadowCircleVisuals {
        clipped_layer,
        background,
        shadow: shadow_visual,
        circle_clip,
    })
}
