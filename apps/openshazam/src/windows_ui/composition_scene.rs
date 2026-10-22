#![deny(unsafe_op_in_unsafe_fn)]

mod no_match_motion;

use super::{
    auto_shadow_host_center, composition_auto_shadow_color, composition_button_pixel_snap_offset,
    composition_color, composition_outer_ripples_visible, composition_root_layer_order,
    composition_scale_for_dpi, composition_scene_layer_order,
    create_composition_clip_from_d2d_geometry, create_popup_outline_geometry,
    create_scalar_transition, create_scale_transition, create_vector2_scale_transition,
    make_auto_shadow_circle_visuals, make_auto_shadow_outline_mask,
    make_button_border_arc_geometry, make_inverse_circle_clip, make_inverse_circle_clip_at,
    make_no_match_icon_geometry, no_match_icon_rect, overlay_surface_error, popup_outline_center,
    result_overlay_intersects_header, ui_model, AnimationIterationBehavior, ArrowEdge,
    CompositionBorderMode, CompositionColorBrush, CompositionEllipseGeometry,
    CompositionGeometricClip, CompositionHistoryOverlay, CompositionHistoryScrollOverlay,
    CompositionLogo, CompositionResultOverlay, CompositionRootLayer, CompositionSceneLayer,
    CompositionSpriteShape, CompositionStatusText, CompositionSurfaceBrush,
    CompositionVisualSurface, Compositor, ContainerVisual, CubicBezierEasingFunction,
    DesktopWindowTarget, DropShadow, InsetClip, ResultOverlayMotionAnimation,
    ScalarKeyFrameAnimation, ShapeVisual, SpriteVisual, StatusTextSnapshot, TaggingMode, Theme,
    UiLevels, Vector2KeyFrameAnimation, Vector3KeyFrameAnimation, AUTO_SHADOW_DISPLAY_BOUNDS,
    BUTTON_CLEAR_DIAMETER, BUTTON_SIZE, BUTTON_TOP, BUTTON_X, CONTENT_WIDTH, INNER_MASK_BOUNDS,
    MAIN_WINDOW_HEIGHT, OUTER_BASE_OPACITY, POPUP_ARROW_HEIGHT, SUCCESS_SETTLE_TOTAL, TOP_HEIGHT,
};
use no_match_motion::{
    create_no_match_opacity_sequence, create_no_match_position_animation,
    create_no_match_scale_animation, create_no_match_scale_timeline, NoMatchScaleUp,
};
use std::time::{Duration, Instant};
use windows::{
    core::{Error, Interface, HSTRING},
    Foundation::TimeSpan,
    Win32::{
        Foundation::HWND,
        Graphics::{
            Direct2D::{ID2D1Factory, ID2D1Geometry},
            DirectWrite::IDWriteFactory,
        },
        System::WinRT::Composition::ICompositorDesktopInterop,
    },
    UI::Color,
};
use windows_numerics::{Vector2, Vector3};

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

pub(super) struct CompositionScene {
    pub(super) compositor: Compositor,
    _target: DesktopWindowTarget,
    pub(super) root: ContainerVisual,
    _popup_clip: CompositionGeometricClip,
    popup_clip_key: (ArrowEdge, u32),
    tagging_clip: InsetClip,
    tagging_clip_bottom: f32,
    pub(super) history_overlay: CompositionHistoryOverlay,
    pub(super) history_scroll_overlay: CompositionHistoryScrollOverlay,
    pub(super) result_overlay: CompositionResultOverlay,
    status_text: CompositionStatusText,
    auto_shadow_root: ContainerVisual,
    auto_shadow_mask: CompositionGeometricClip,
    auto_shadow_brush: CompositionColorBrush,
    _auto_shadow_circle_layer: ContainerVisual,
    _auto_shadow_circle_clip: CompositionGeometricClip,
    _auto_shadow_sprite: SpriteVisual,
    _auto_shadow_shadow_sprite: SpriteVisual,
    _auto_shadow_outline_source: ShapeVisual,
    _auto_shadow_outline_surface: CompositionVisualSurface,
    _auto_shadow_outline_mask: CompositionSurfaceBrush,
    auto_shadow: DropShadow,
    inner_audio_root: ContainerVisual,
    inner_wave_root: ContainerVisual,
    inner_inverse_clip: CompositionGeometricClip,
    outer_audio_root: ContainerVisual,
    rings: [CompositionRing; 6],
    button: ContainerVisual,
    button_visible: bool,
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
pub(super) struct AnimationSnapshot {
    pub(super) mode: TaggingMode,
    pub(super) elapsed: f64,
    pub(super) now: Instant,
    pub(super) theme: Theme,
    pub(super) body_top: f32,
    pub(super) status_text: StatusTextSnapshot,
    pub(super) show_button: bool,
    pub(super) show_ripples: bool,
    pub(super) levels: UiLevels,
    pub(super) auto_spring: Option<ui_model::SpringProfile>,
    pub(super) no_match_elapsed: Option<f64>,
    pub(super) success_elapsed: Option<f64>,
    pub(super) success_track_id: Option<u64>,
    pub(super) success_wave_frame: Option<ui_model::WaveFrame>,
}

impl CompositionScene {
    // Parameters mirror the existing native graphics context and popup
    // lifecycle; grouping them would obscure resource ownership.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        hwnd: HWND,
        d2d_factory: &ID2D1Factory,
        write_factory: &IDWriteFactory,
        compositor: &Compositor,
        theme: Theme,
        arrow_edge: ArrowEdge,
        arrow_center: f32,
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
        let arrow_center = popup_outline_center(arrow_center);
        let popup_outline =
            unsafe { create_popup_outline_geometry(d2d_factory, arrow_edge, arrow_center)? };
        let popup_clip =
            unsafe { create_composition_clip_from_d2d_geometry(compositor, &popup_outline)? };
        root.SetClip(&popup_clip)?;
        let popup_clip_key = (arrow_edge, arrow_center.to_bits());
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
        let (auto_shadow_outline_source, auto_shadow_outline_surface, auto_shadow_outline_mask) =
            make_auto_shadow_outline_mask(compositor, auto_shadow_bounds, 2.0)?;
        let auto_shadow = compositor.CreateDropShadow()?;
        auto_shadow.SetBlurRadius(3.0)?;
        auto_shadow.SetOffset(Vector3::default())?;
        auto_shadow.SetOpacity(0.5)?;
        auto_shadow.SetColor(composition_color(255, 36, 36, 36))?;
        auto_shadow.SetSourcePolicy(
            windows::UI::Composition::CompositionDropShadowSourcePolicy::Default,
        )?;
        let auto_shadow_shadow_brush =
            compositor.CreateColorBrushWithColor(composition_color(0, 0, 0, 0))?;
        let auto_shadow_circle = make_auto_shadow_circle_visuals(
            compositor,
            auto_shadow_bounds,
            &auto_shadow_brush,
            &auto_shadow_shadow_brush,
            &auto_shadow,
            &auto_shadow_outline_mask,
        )?;
        auto_shadow_root
            .Children()?
            .InsertAtTop(&auto_shadow_circle.clipped_layer)?;

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
            .InsertAtTop(logo.visual())?;
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
        let history_overlay = unsafe {
            CompositionHistoryOverlay::new(
                compositor,
                &result_overlay._graphics_device,
                dpi,
                body_top,
            )?
        };
        let history_scroll_overlay =
            CompositionHistoryScrollOverlay::new(compositor, dpi, body_top)?;
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
                CompositionSceneLayer::HistoryScrollOverlay => {
                    root_children.InsertAtTop(history_scroll_overlay.viewport())?
                }
                CompositionSceneLayer::HistoryOverlay => {
                    root_children.InsertAtTop(&history_overlay.viewport)?
                }
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
            _popup_clip: popup_clip,
            popup_clip_key,
            tagging_clip: clip,
            tagging_clip_bottom: clip_bottom.max(0.0),
            history_overlay,
            history_scroll_overlay,
            result_overlay,
            status_text,
            auto_shadow_root,
            auto_shadow_mask,
            auto_shadow_brush,
            _auto_shadow_circle_layer: auto_shadow_circle.clipped_layer,
            _auto_shadow_circle_clip: auto_shadow_circle.circle_clip,
            _auto_shadow_sprite: auto_shadow_circle.background,
            _auto_shadow_shadow_sprite: auto_shadow_circle.shadow,
            _auto_shadow_outline_source: auto_shadow_outline_source,
            _auto_shadow_outline_surface: auto_shadow_outline_surface,
            _auto_shadow_outline_mask: auto_shadow_outline_mask,
            auto_shadow,
            inner_audio_root,
            inner_wave_root,
            inner_inverse_clip,
            outer_audio_root,
            rings,
            button,
            button_visible: true,
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

    pub(super) unsafe fn update_popup_clip(
        &mut self,
        outline: &ID2D1Geometry,
        edge: ArrowEdge,
        arrow_center: f32,
    ) -> windows::core::Result<()> {
        let center = popup_outline_center(arrow_center);
        let key = (edge, center.to_bits());
        if self.popup_clip_key == key {
            return Ok(());
        }
        // SAFETY: This scene's compositor and the supplied geometry are used
        // only on the UI STA where the scene was created.
        let clip = unsafe { create_composition_clip_from_d2d_geometry(&self.compositor, outline)? };
        self.root.SetClip(&clip)?;
        self._popup_clip = clip;
        self.popup_clip_key = key;
        Ok(())
    }

    pub(super) fn set_dpi(&mut self, dpi: u32) -> windows::core::Result<()> {
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
            self.history_overlay.resize(dpi)?;
            self.history_overlay.set_viewport(dpi, self.body_top)?;
            self.history_scroll_overlay
                .set_viewport(dpi, self.body_top)?;
            self.history_scroll_overlay.hide()?;
            self.status_text.resize(dpi)
        }
    }

    // Explicit source/capture/motion parameters keep the GPU overlay
    // updates independent of audio workers and view state.
    /// # Safety
    /// Must be called on the UI STA that created this scene and its Composition
    /// surfaces; `card_pixels` must be a live buffer for the duration of the call.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn update_result_overlay(
        &mut self,
        card_pixels: Option<&[u8]>,
        capture_top: Option<f32>,
        panel_top: Option<f32>,
        body_top: f32,
        dpi: u32,
        motion: Option<&ResultOverlayMotionAnimation>,
        motion_changed: bool,
    ) -> windows::core::Result<()> {
        if let (Some(card_pixels), Some(_)) = (card_pixels, capture_top) {
            // Refresh from the standalone premultiplied card raster, not the
            // popup's already masked frame. This lets the Composition root
            // apply its outer antialiased clip once without sampling header
            // color into the card edge.
            self.result_overlay
                .hide()
                .map_err(|error| overlay_surface_error("HideBeforeRefresh", error))?;
            // SAFETY: This scene and its Composition surface are used on their
            // creating UI STA; `card_pixels` is borrowed from the live renderer
            // snapshot for this synchronous surface copy.
            unsafe {
                self.result_overlay
                    .update_from_pixels(card_pixels, body_top, dpi)?;
            }
        } else {
            // SAFETY: The overlay's Composition surface is accessed only on
            // the creating UI STA, matching the scene's threading contract.
            unsafe { self.result_overlay.resize(dpi)? };
        }
        match (card_pixels, panel_top, motion) {
            (_, None, _) | (None, Some(_), _) => self.result_overlay.hide(),
            // A content refresh hides the group while its surface is updated.
            // Resume the sampled in-flight motion even when its identity did
            // not change; otherwise the group stays hidden until the card
            // reaches its final position and appears to pop in.
            (Some(_), Some(_), Some(motion)) if motion_changed || capture_top.is_some() => self
                .result_overlay
                .animate_position(&self.compositor, motion, body_top, dpi),
            (Some(_), Some(_), Some(_)) => Ok(()),
            (Some(_), Some(panel_top), None) if motion_changed || capture_top.is_some() => {
                self.result_overlay.set_position(panel_top, body_top, dpi)
            }
            (Some(_), Some(_), None) => Ok(()),
        }
    }

    pub(super) fn set_result_fallback_tagging_clip(
        &mut self,
        panel_top: Option<f32>,
        body_top: f32,
    ) -> windows::core::Result<()> {
        // The D2D fallback card is below the Composition tree. Keep the
        // controls above its moving top edge visible and clip the rest, so
        // the button and gear reappear progressively as the card exits.
        let header_bottom = body_top + TOP_HEIGHT as f32;
        let visible_bottom = panel_top
            .filter(|top| result_overlay_intersects_header(Some(*top), body_top))
            .map(|top| top.clamp(0.0, header_bottom))
            .unwrap_or(header_bottom);
        let clip_bottom = (MAIN_WINDOW_HEIGHT as f32 - visible_bottom).max(0.0);
        if (self.tagging_clip_bottom - clip_bottom).abs() > f32::EPSILON {
            self.tagging_clip.SetBottomInset(clip_bottom)?;
            self.tagging_clip_bottom = clip_bottom;
        }
        Ok(())
    }

    pub(super) fn update(&mut self, snapshot: AnimationSnapshot) -> windows::core::Result<()> {
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
            let bottom_inset = bottom_inset.max(0.0);
            self.tagging_clip.SetBottomInset(bottom_inset)?;
            self.tagging_clip_bottom = bottom_inset;
            self.result_overlay.set_viewport(self.dpi, self.body_top)?;
            self.history_overlay.set_viewport(self.dpi, self.body_top)?;
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

    pub(super) fn startup_logo_complete(&self) -> bool {
        self.logo.is_complete()
    }

    pub(super) fn start_startup_logo(&mut self) -> windows::core::Result<()> {
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

fn composition_tagging_color(theme: Theme) -> Color {
    match theme {
        Theme::Light => composition_color(255, 255, 255, 255),
        Theme::Dark => composition_color(255, 148, 148, 148),
    }
}
