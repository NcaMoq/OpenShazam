#![deny(unsafe_op_in_unsafe_fn)]

use super::{
    bgra_buffer_layout, composition_history_viewport_offset, d2d_color, history_scroll_easing,
    history_scroll_offset_at, overlay_surface_error, rect, remaining_cubic_easing,
    HistoryScrollAnimation, CONTENT_WIDTH, HISTORY_HEIGHT, HISTORY_SCROLL_TRANSITION,
};
use std::time::Instant;
use windows::{
    core::{Error, Interface, HSTRING},
    Foundation::TimeSpan,
    Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat},
    Win32::{
        Foundation::POINT,
        Graphics::{
            Direct2D::{
                Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U},
                ID2D1DeviceContext, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1,
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
        },
        System::WinRT::Composition::ICompositionDrawingSurfaceInterop,
    },
    UI::Composition::{
        CompositionBitmapInterpolationMode, CompositionBorderMode, CompositionDrawingSurface,
        CompositionGraphicsDevice, CompositionStretch, CompositionSurfaceBrush, Compositor,
        ContainerVisual, SpriteVisual,
    },
};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

/// A per-scroll snapshot is small enough for a regular Composition drawing
/// surface. Direct2D rasterizes the visible span once, then the compositor
/// moves one sprite at display refresh rate. This avoids the missing tile and
/// per-row alignment issues seen with virtual drawing surfaces.
pub(super) struct CompositionHistoryScrollOverlay {
    viewport: ContainerVisual,
    group: ContainerVisual,
    sprite: SpriteVisual,
    surface: Option<CompositionDrawingSurface>,
    interop: Option<ICompositionDrawingSurfaceInterop>,
    brush: Option<CompositionSurfaceBrush>,
    active_started_at: Option<Instant>,
    visible: bool,
}

impl CompositionHistoryScrollOverlay {
    pub(super) fn new(
        compositor: &Compositor,
        dpi: u32,
        body_top: f32,
    ) -> windows::core::Result<Self> {
        let viewport = compositor.CreateContainerVisual()?;
        viewport.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: HISTORY_HEIGHT as f32,
        })?;
        viewport.SetBorderMode(CompositionBorderMode::Soft)?;
        let clip = compositor.CreateInsetClipWithInsets(0.0, 0.0, 0.0, 0.0)?;
        viewport.SetClip(&clip)?;
        viewport.SetOffset(composition_history_viewport_offset(dpi, body_top))?;
        viewport.SetOpacity(0.0)?;

        let group = compositor.CreateContainerVisual()?;
        group.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: 1.0,
        })?;
        group.SetBorderMode(CompositionBorderMode::Soft)?;
        group.SetOpacity(1.0)?;
        let sprite = compositor.CreateSpriteVisual()?;
        sprite.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: 1.0,
        })?;
        sprite.SetBorderMode(CompositionBorderMode::Soft)?;
        group.Children()?.InsertAtTop(&sprite)?;
        viewport.Children()?.InsertAtTop(&group)?;

        Ok(Self {
            viewport,
            group,
            sprite,
            surface: None,
            interop: None,
            brush: None,
            active_started_at: None,
            visible: false,
        })
    }

    pub(super) fn viewport(&self) -> &ContainerVisual {
        &self.viewport
    }

    pub(super) fn active(&self) -> bool {
        self.visible && self.active_started_at.is_some()
    }

    pub(super) fn matches(&self, started_at: Instant) -> bool {
        self.active() && self.active_started_at == Some(started_at)
    }

    pub(super) fn set_viewport(&self, dpi: u32, body_top: f32) -> windows::core::Result<()> {
        self.viewport
            .SetOffset(composition_history_viewport_offset(dpi, body_top))
    }

    #[allow(clippy::too_many_arguments)]
    /// # Safety
    /// Must be called on the UI STA that owns `compositor` and
    /// `graphics_device`; `pixels` must remain readable for the duration of
    /// the synchronous copy into the Composition surface.
    pub(super) unsafe fn upload_and_start(
        &mut self,
        compositor: &Compositor,
        graphics_device: &CompositionGraphicsDevice,
        pixels: &[u8],
        size: D2D_SIZE_U,
        content_height: f32,
        origin_offset: f32,
        animation: HistoryScrollAnimation,
        now: Instant,
    ) -> windows::core::Result<()> {
        let (row_stride, expected) =
            bgra_buffer_layout(size.width, size.height).ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x8007_0057u32 as i32),
                    "The history scroll snapshot dimensions are invalid or too large",
                )
            })?;
        if pixels.len() != expected {
            return Err(Error::new(
                windows::core::HRESULT(0x80070057_u32 as i32),
                "The history scroll snapshot has an unexpected pixel buffer size",
            ));
        }

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
        brush.SetBitmapInterpolationMode(CompositionBitmapInterpolationMode::Linear)?;

        let mut offset = POINT::default();
        // SAFETY: The drawing surface and its interop interface were created
        // for this UI-STA operation and remain live through the matching EndDraw.
        let context = unsafe { interop.BeginDraw::<ID2D1DeviceContext>(None, &mut offset) }
            .map_err(|error| overlay_surface_error("HistoryScrollSnapshot.BeginDraw", error))?;
        let draw_result = (|| -> windows::core::Result<()> {
            // SAFETY: `BeginDraw` returned the live drawing context for this
            // UI-STA surface. `pixels` has a checked exact BGRA length and its
            // pointer is consumed synchronously by CreateBitmap.
            unsafe {
                context.SetDpi(96.0, 96.0);
                context.SetTransform(&Matrix3x2 {
                    M11: 1.0,
                    M22: 1.0,
                    M31: offset.x as f32,
                    M32: offset.y as f32,
                    ..Default::default()
                });
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
                let bitmap = context.CreateBitmap(
                    size,
                    Some(pixels.as_ptr().cast()),
                    row_stride,
                    &properties,
                )?;
                context.Clear(Some(&d2d_color(0, 0, 0, 0.0)));
                let destination = rect(0.0, 0.0, size.width as f32, size.height as f32);
                context.DrawBitmap(
                    &bitmap,
                    Some(&destination),
                    1.0,
                    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                    None,
                );
                Ok(())
            }
        })();
        // SAFETY: This pairs with the successful BeginDraw above on the same
        // live interop surface, regardless of whether drawing itself failed.
        let end_result = unsafe { interop.EndDraw() };
        draw_result?;
        end_result
            .map_err(|error| overlay_surface_error("HistoryScrollSnapshot.EndDraw", error))?;

        self.group.StopAnimation(&HSTRING::from("Offset.Y"))?;
        self.group.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: content_height,
        })?;
        self.sprite.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: content_height,
        })?;
        self.sprite.SetBrush(&brush)?;
        self.surface = Some(surface);
        self.interop = Some(interop);
        self.brush = Some(brush);

        let current_offset = history_scroll_offset_at(animation, now).0;
        let initial_y = origin_offset - current_offset;
        let target_y = origin_offset - animation.to_offset;
        self.group.SetOffset(Vector3 {
            X: 0.0,
            Y: initial_y,
            Z: 0.0,
        })?;
        self.group.SetOpacity(1.0)?;
        self.viewport.SetOpacity(1.0)?;
        self.visible = true;

        let elapsed = now.saturating_duration_since(animation.started_at);
        let progress =
            (elapsed.as_secs_f64() / HISTORY_SCROLL_TRANSITION.as_secs_f64()).clamp(0.0, 1.0);
        if progress >= 1.0 {
            self.group.SetOffset(Vector3 {
                X: 0.0,
                Y: target_y,
                Z: 0.0,
            })?;
        } else {
            let curve = history_scroll_easing();
            let remaining_easing = remaining_cubic_easing(progress, curve).unwrap_or(curve);
            let easing = compositor.CreateCubicBezierEasingFunction(
                Vector2 {
                    X: remaining_easing.x1 as f32,
                    Y: remaining_easing.y1 as f32,
                },
                Vector2 {
                    X: remaining_easing.x2 as f32,
                    Y: remaining_easing.y2 as f32,
                },
            )?;
            let remaining = HISTORY_SCROLL_TRANSITION.saturating_sub(elapsed);
            let keyframe = compositor.CreateScalarKeyFrameAnimation()?;
            keyframe.SetDuration(TimeSpan {
                Duration: (remaining.as_nanos() / 100).min(i64::MAX as u128) as i64,
            })?;
            keyframe.InsertKeyFrameWithEasingFunction(1.0, target_y, &easing)?;
            self.group
                .StartAnimation(&HSTRING::from("Offset.Y"), &keyframe)?;
        }
        self.active_started_at = Some(animation.started_at);
        Ok(())
    }

    pub(super) fn hide(&mut self) -> windows::core::Result<()> {
        self.group.StopAnimation(&HSTRING::from("Offset.Y"))?;
        self.viewport.SetOpacity(0.0)?;
        self.visible = false;
        self.active_started_at = None;
        Ok(())
    }
}
