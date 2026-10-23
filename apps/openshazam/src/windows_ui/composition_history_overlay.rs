#![deny(unsafe_op_in_unsafe_fn)]

use super::{
    bgra_buffer_layout, composition_button_pixel_snap_offset, composition_pixel_size,
    composition_scale_for_dpi, copy_panel_pixels_into, d2d_color, history_composition_offset,
    overlay_surface_error, rect, HistoryCompositionKey, HistoryMutationEvent,
    HistoryMutationTransition, LayeredSurface, CONTENT_WIDTH, HISTORY_HEIGHT,
    HISTORY_INSERT_TRANSITION, HISTORY_ROW_PITCH, TOP_HEIGHT,
};
use std::time::Instant;
use windows::{
    core::{Error, Interface, HSTRING},
    Foundation::TimeSpan,
    Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat},
    Win32::{
        Foundation::{POINT, SIZE},
        Graphics::{
            Direct2D::{
                Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U},
                ID2D1Bitmap1, ID2D1DeviceContext, D2D1_BITMAP_OPTIONS_NONE,
                D2D1_BITMAP_PROPERTIES1, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
        },
        System::WinRT::Composition::ICompositionDrawingSurfaceInterop,
    },
    UI::Composition::{
        CompositionBitmapInterpolationMode, CompositionBorderMode, CompositionDrawingSurface,
        CompositionGraphicsDevice, CompositionStretch, CompositionSurfaceBrush, Compositor,
        ContainerVisual,
    },
};

use windows_numerics::{Matrix3x2, Vector2, Vector3};
// Hold the old list and the new first row in one premultiplied surface. The
// compositor moves that cached image for the 250 ms insert, avoiding repeated
// full layered-window D2D redraws on the UI timer.
pub(super) struct CompositionHistoryOverlay {
    _graphics_device: CompositionGraphicsDevice,
    _surface: CompositionDrawingSurface,
    interop: ICompositionDrawingSurfaceInterop,
    _brush: CompositionSurfaceBrush,
    pub(super) viewport: ContainerVisual,
    group: ContainerVisual,
    bitmap: Option<ID2D1Bitmap1>,
    pub(super) size: D2D_SIZE_U,
    dpi: u32,
    pub(super) pixels: Vec<u8>,
    scratch: Vec<u8>,
    key: Option<HistoryCompositionKey>,
}

impl CompositionHistoryOverlay {
    /// # Safety
    /// `compositor` and `graphics_device` must be live Composition interfaces
    /// usable from the current UI STA for the lifetime of this overlay.
    pub(super) unsafe fn new(
        compositor: &Compositor,
        graphics_device: &CompositionGraphicsDevice,
        dpi: u32,
        body_top: f32,
    ) -> windows::core::Result<Self> {
        let dpi = dpi.max(96);
        let size = composition_history_surface_size(dpi).ok_or_else(|| {
            Error::new(
                windows::core::HRESULT(0x8007_0057u32 as i32),
                "Composition history surface dimensions are invalid or too large",
            )
        })?;
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

        let viewport = compositor.CreateContainerVisual()?;
        viewport.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: HISTORY_HEIGHT as f32,
        })?;
        viewport.SetBorderMode(CompositionBorderMode::Soft)?;
        let clip = compositor.CreateInsetClipWithInsets(0.0, 0.0, 0.0, 0.0)?;
        viewport.SetClip(&clip)?;
        viewport.SetOffset(composition_history_viewport_offset(dpi, body_top))?;

        let group = compositor.CreateContainerVisual()?;
        group.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: HISTORY_HEIGHT as f32 + HISTORY_ROW_PITCH,
        })?;
        group.SetBorderMode(CompositionBorderMode::Soft)?;
        group.SetOpacity(0.0)?;
        let sprite = compositor.CreateSpriteVisual()?;
        sprite.SetSize(Vector2 {
            X: CONTENT_WIDTH as f32,
            Y: HISTORY_HEIGHT as f32 + HISTORY_ROW_PITCH,
        })?;
        sprite.SetBorderMode(CompositionBorderMode::Soft)?;
        sprite.SetBrush(&brush)?;
        group.Children()?.InsertAtTop(&sprite)?;
        viewport.Children()?.InsertAtTop(&group)?;

        Ok(Self {
            _graphics_device: graphics_device.clone(),
            _surface: surface,
            interop,
            _brush: brush,
            viewport,
            group,
            bitmap: None,
            size,
            dpi,
            pixels: Vec::new(),
            scratch: Vec::new(),
            key: None,
        })
    }

    pub(super) fn matches_key(&self, key: HistoryCompositionKey) -> bool {
        self.key == Some(key)
    }

    pub(super) fn has_transition(&self, started_at: Instant) -> bool {
        self.key.is_some_and(|key| key.started_at == started_at)
    }

    pub(super) fn set_viewport(&self, dpi: u32, body_top: f32) -> windows::core::Result<()> {
        self.viewport
            .SetOffset(composition_history_viewport_offset(dpi, body_top))
    }

    /// # Safety
    /// Must run on the UI STA that owns this overlay's drawing surface.
    pub(super) unsafe fn resize(&mut self, dpi: u32) -> windows::core::Result<()> {
        let dpi = dpi.max(96);
        if self.dpi == dpi {
            return Ok(());
        }
        let old_size = self.size;
        let new_size = composition_history_surface_size(dpi).ok_or_else(|| {
            Error::new(
                windows::core::HRESULT(0x8007_0057u32 as i32),
                "Composition history surface dimensions are invalid or too large",
            )
        })?;
        // SAFETY: This object owns the live drawing surface and resize is
        // confined to the UI STA that created it.
        unsafe {
            self.interop.Resize(SIZE {
                cx: new_size.width as i32,
                cy: new_size.height as i32,
            })
        }
        .map_err(|error| overlay_surface_error("History.Resize", error))?;
        self.size = new_size;
        self.dpi = dpi;
        self.bitmap = None;
        if !self.pixels.is_empty() {
            let resized =
                resize_bgra_nearest(&self.pixels, old_size, new_size).ok_or_else(|| {
                    Error::new(
                        windows::core::HRESULT(0x8007_0057u32 as i32),
                        "Composition history pixel buffers have invalid dimensions",
                    )
                })?;
            self.pixels.clear();
            // SAFETY: Resize is called on the owning UI STA, and `resized` is
            // a complete validated BGRA image for the new surface dimensions.
            unsafe { self.upload_pixels(&resized)? };
        }
        Ok(())
    }

    /// # Safety
    /// Must run on the UI STA that owns this overlay's drawing surface.
    unsafe fn upload_pixels(&mut self, pixels: &[u8]) -> windows::core::Result<()> {
        let (row_stride, expected_len) = bgra_buffer_layout(self.size.width, self.size.height)
            .ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x8007_0057u32 as i32),
                    "Composition history surface dimensions are invalid or too large",
                )
            })?;
        if pixels.len() != expected_len {
            return Err(Error::new(
                windows::core::HRESULT(0x80070057_u32 as i32),
                "Composition history surface received an unexpected pixel buffer size",
            ));
        }
        if self.pixels == pixels {
            return Ok(());
        }
        self.scratch
            .try_reserve_exact(expected_len.saturating_sub(self.scratch.len()))
            .map_err(|_| {
                Error::new(
                    windows::core::HRESULT(0x8007_000eu32 as i32),
                    "Unable to allocate the Composition history pixel buffer",
                )
            })?;
        self.scratch.clear();
        self.scratch.extend_from_slice(pixels);
        let mut offset = POINT::default();
        // SAFETY: The surface is owned by this overlay and all drawing is
        // serialized on its creating UI STA; `offset` remains live for the call.
        let context = unsafe {
            self.interop
                .BeginDraw::<ID2D1DeviceContext>(None, &mut offset)
        }
        .map_err(|error| overlay_surface_error("History.BeginDraw", error))?;
        let draw_result = (|| -> windows::core::Result<()> {
            // SAFETY: `context` is live for this BeginDraw/EndDraw pair. The
            // validated, owned scratch buffer is large enough for the exact
            // dimensions and stride passed to Direct2D below.
            unsafe {
                context.SetDpi(96.0, 96.0);
                context.SetTransform(&Matrix3x2 {
                    M11: 1.0,
                    M22: 1.0,
                    M31: offset.x as f32,
                    M32: offset.y as f32,
                    ..Default::default()
                });
            }
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
                // SAFETY: `scratch` is owned and its length was checked against
                // the bitmap dimensions; `row_stride` is the validated BGRA stride.
                let bitmap = unsafe {
                    context.CreateBitmap(
                        self.size,
                        Some(self.scratch.as_ptr().cast()),
                        row_stride,
                        &properties,
                    )
                }
                .map_err(|error| overlay_surface_error("History.CreateBitmap", error))?;
                self.bitmap = Some(bitmap.clone());
                bitmap
            };
            if self.bitmap.is_some() && self.pixels.len() == expected_len {
                // SAFETY: The scratch buffer remains stable and covers the
                // complete destination bitmap at the validated row stride.
                unsafe { bitmap.CopyFromMemory(None, self.scratch.as_ptr().cast(), row_stride) }
                    .map_err(|error| overlay_surface_error("History.CopyFromMemory", error))?;
            }
            // SAFETY: The context is valid for the active draw session and
            // the borrowed color/bounds live through their synchronous calls.
            unsafe { context.Clear(Some(&d2d_color(0, 0, 0, 0.0))) };
            let destination = rect(0.0, 0.0, self.size.width as f32, self.size.height as f32);
            // SAFETY: The bitmap belongs to this draw context and all geometry
            // and resources remain alive for the synchronous draw call.
            unsafe {
                context.DrawBitmap(
                    &bitmap,
                    Some(&destination),
                    1.0,
                    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                    None,
                )
            };
            Ok(())
        })();
        // SAFETY: Every successful BeginDraw is paired with EndDraw, including
        // when the drawing closure returns an error.
        let end_result = unsafe { self.interop.EndDraw() };
        draw_result?;
        end_result.map_err(|error| overlay_surface_error("History.EndDraw", error))?;
        std::mem::swap(&mut self.pixels, &mut self.scratch);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    /// # Safety
    /// Must run on the UI STA that owns this overlay and `previous_frame`'s
    /// drawing resources. Pixel buffers are dimension-checked before upload.
    pub(super) unsafe fn capture_and_animate(
        &mut self,
        compositor: &Compositor,
        previous_frame: &LayeredSurface,
        row_pixels: &[u8],
        history_top: f32,
        body_top: f32,
        dpi: u32,
        transition: HistoryMutationTransition,
        key: HistoryCompositionKey,
        now: Instant,
    ) -> windows::core::Result<()> {
        // SAFETY: This operation and the surface resize are performed on the
        // same UI STA that created the overlay.
        unsafe { self.resize(dpi)? };
        self.set_viewport(dpi, body_top)?;
        let scale = composition_scale_for_dpi(self.dpi);
        let row_height = (HISTORY_ROW_PITCH * scale).ceil() as u32;
        let previous_height = (HISTORY_HEIGHT as f32 * scale).ceil() as u32;
        let (_, row_len) = bgra_buffer_layout(self.size.width, row_height).ok_or_else(|| {
            Error::new(
                windows::core::HRESULT(0x8007_0057u32 as i32),
                "Composition history row dimensions are invalid or too large",
            )
        })?;
        let (_, previous_len) =
            bgra_buffer_layout(self.size.width, previous_height).ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x8007_0057u32 as i32),
                    "Composition history snapshot dimensions are invalid or too large",
                )
            })?;
        let (_, combined_len) =
            bgra_buffer_layout(self.size.width, self.size.height).ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x8007_0057u32 as i32),
                    "Composition history surface dimensions are invalid or too large",
                )
            })?;
        if row_pixels.len() != row_len {
            return Err(Error::new(
                windows::core::HRESULT(0x80070057_u32 as i32),
                "Composition history row buffer has an unexpected size",
            ));
        }
        let mut previous = Vec::new();
        let previous_bytes = previous_frame.pixel_bytes();
        let initial_insert = matches!(
            transition.event,
            HistoryMutationEvent::Insert {
                index: 0,
                previous_count: 0,
            }
        );
        if !initial_insert {
            copy_panel_pixels_into(
                previous_bytes,
                previous_frame.size.width,
                previous_frame.size.height,
                (history_top * self.dpi as f32 / 96.0).round() as i32,
                self.size.width,
                previous_height,
                &mut previous,
            )?;
        }

        // The new row sits one pitch above the prior list. A single +91pt
        // Composition track then moves the inserted row into place while
        // shifting every existing row down by the same amount. For the first
        // row there is no prior list: leave the rest transparent so the empty
        // skeleton can fade away beneath the independently moving row.
        if row_len > combined_len {
            return Err(Error::new(
                windows::core::HRESULT(0x8007_0057u32 as i32),
                "Composition history row exceeds its target surface",
            ));
        }
        let mut combined = Vec::new();
        combined.try_reserve_exact(combined_len).map_err(|_| {
            Error::new(
                windows::core::HRESULT(0x8007_000eu32 as i32),
                "Unable to allocate the Composition history surface buffer",
            )
        })?;
        combined.resize(combined_len, 0);
        combined[..row_len].copy_from_slice(row_pixels);
        let destination_start = row_len;
        let copy_len = previous_len.min(combined.len().saturating_sub(destination_start));
        if !initial_insert {
            combined[destination_start..destination_start + copy_len]
                .copy_from_slice(&previous[..copy_len]);
        }
        // SAFETY: `combined` is a complete surface-sized BGRA buffer and this
        // caller is running on the overlay's owning UI STA.
        unsafe { self.upload_pixels(&combined)? };

        self.group
            .StopAnimation(&HSTRING::from("Offset.Y"))
            .map_err(|error| overlay_surface_error("History.StopAnimation.Offset.Y", error))?;
        let from_offset = history_composition_offset(transition.started_at, now);
        self.group
            .SetOffset(Vector3 {
                X: 0.0,
                Y: from_offset,
                Z: 0.0,
            })
            .map_err(|error| overlay_surface_error("History.SetOffset", error))?;
        let remaining = HISTORY_INSERT_TRANSITION
            .saturating_sub(now.saturating_duration_since(transition.started_at));
        if !remaining.is_zero() {
            let animation = compositor
                .CreateScalarKeyFrameAnimation()
                .map_err(|error| overlay_surface_error("History.CreateAnimation", error))?;
            animation
                .SetDuration(TimeSpan {
                    Duration: (remaining.as_nanos() / 100).min(i64::MAX as u128) as i64,
                })
                .map_err(|error| overlay_surface_error("History.SetDuration", error))?;
            animation
                .InsertKeyFrame(1.0, 0.0)
                .map_err(|error| overlay_surface_error("History.InsertKeyFrame", error))?;
            self.group
                .StartAnimation(&HSTRING::from("Offset.Y"), &animation)
                .map_err(|error| overlay_surface_error("History.StartAnimation", error))?;
        }
        self.group
            .SetOpacity(1.0)
            .map_err(|error| overlay_surface_error("History.SetOpacity", error))?;
        self.key = Some(key);
        Ok(())
    }

    pub(super) fn hide(&mut self) -> windows::core::Result<()> {
        if self.key.is_none() {
            return Ok(());
        }
        self.group
            .StopAnimation(&HSTRING::from("Offset.Y"))
            .map_err(|error| overlay_surface_error("History.Hide.StopAnimation", error))?;
        self.group
            .SetOpacity(0.0)
            .map_err(|error| overlay_surface_error("History.Hide.SetOpacity", error))?;
        self.key = None;
        Ok(())
    }
}

fn composition_history_surface_size(dpi: u32) -> Option<D2D_SIZE_U> {
    let dpi = dpi.max(96);
    let size = D2D_SIZE_U {
        width: composition_pixel_size(CONTENT_WIDTH as f32, dpi)?,
        height: composition_pixel_size(HISTORY_HEIGHT as f32, dpi)?
            .checked_add(composition_pixel_size(HISTORY_ROW_PITCH, dpi)?)?,
    };
    bgra_buffer_layout(size.width, size.height)?;
    Some(size)
}

pub(super) fn composition_history_viewport_offset(dpi: u32, body_top: f32) -> Vector3 {
    let root_snap = composition_button_pixel_snap_offset(dpi, body_top);
    Vector3 {
        X: -root_snap.X,
        Y: body_top + TOP_HEIGHT as f32 - root_snap.Y,
        Z: 0.0,
    }
}

fn resize_bgra_nearest(
    source: &[u8],
    source_size: D2D_SIZE_U,
    target_size: D2D_SIZE_U,
) -> Option<Vec<u8>> {
    let (_, source_len) = bgra_buffer_layout(source_size.width, source_size.height)?;
    let (_, target_len) = bgra_buffer_layout(target_size.width, target_size.height)?;
    if source.len() != source_len {
        return None;
    }
    let mut target = Vec::new();
    target.try_reserve_exact(target_len).ok()?;
    target.resize(target_len, 0);
    for y in 0..target_size.height as usize {
        let source_y = y * source_size.height as usize / target_size.height.max(1) as usize;
        for x in 0..target_size.width as usize {
            let source_x = x * source_size.width as usize / target_size.width.max(1) as usize;
            let source_start = (source_y * source_size.width as usize + source_x) * 4;
            let target_start = (y * target_size.width as usize + x) * 4;
            target[target_start..target_start + 4]
                .copy_from_slice(&source[source_start..source_start + 4]);
        }
    }
    Some(target)
}
