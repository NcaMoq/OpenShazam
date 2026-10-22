#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(test)]
use super::LayeredSurface;

use super::{
    bgra_buffer_layout, composition_header_viewport_offset, composition_pixel_size,
    create_scalar_transition, d2d_color, rect, result_overlay_child_offset,
    ResultOverlayMotionAnimation, CONTENT_WIDTH, TOP_HEIGHT,
};
use windows::{
    core::{Error, Interface, HSTRING},
    Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat},
    Win32::{
        Foundation::{HMODULE, POINT, SIZE},
        Graphics::{
            Direct2D::{
                Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U},
                D2D1CreateDevice, ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext,
                D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1,
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
            },
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP},
            Direct3D11::{
                D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_SDK_VERSION,
            },
            Dxgi::{Common::DXGI_FORMAT_B8G8R8A8_UNORM, IDXGIDevice},
        },
        System::WinRT::Composition::{ICompositionDrawingSurfaceInterop, ICompositorInterop},
    },
    UI::Composition::{
        CompositionBorderMode, CompositionDrawingSurface, CompositionGraphicsDevice,
        CompositionStretch, CompositionSurfaceBrush, Compositor, ContainerVisual,
    },
};
use windows_numerics::{Matrix3x2, Vector2, Vector3};
// Copy a rectangular pixel range from a premultiplied DIB into a tight buffer.
pub(super) fn copy_panel_pixels_into(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    panel_top: i32,
    output_width: u32,
    output_height: u32,
    output: &mut Vec<u8>,
) -> windows::core::Result<()> {
    let (source_stride, source_len) = bgra_buffer_layout(source_width, source_height)
        .ok_or_else(invalid_bgra_buffer_dimensions)?;
    let (output_stride, output_len) = bgra_buffer_layout(output_width, output_height)
        .ok_or_else(invalid_bgra_buffer_dimensions)?;
    if source.len() != source_len {
        return Err(Error::new(
            windows::core::HRESULT(0x8007_0057u32 as i32),
            "Composition source pixel buffer length does not match its dimensions",
        ));
    }
    let copy_width = source_width
        .min(output_width)
        .checked_mul(4)
        .ok_or_else(invalid_bgra_buffer_dimensions)? as usize;
    output
        .try_reserve_exact(output_len.saturating_sub(output.len()))
        .map_err(|_| {
            Error::new(
                windows::core::HRESULT(0x8007_000eu32 as i32),
                "Unable to allocate the Composition pixel buffer",
            )
        })?;
    output.resize(output_len, 0);
    output.fill(0);

    for output_y in 0..output_height {
        let source_y = i64::from(panel_top) + i64::from(output_y);
        if !(0..i64::from(source_height)).contains(&source_y) {
            continue;
        }
        let source_start = usize::try_from(source_y)
            .ok()
            .and_then(|row| row.checked_mul(source_stride as usize))
            .ok_or_else(invalid_bgra_buffer_dimensions)?;
        let output_start = (output_y as usize)
            .checked_mul(output_stride as usize)
            .ok_or_else(invalid_bgra_buffer_dimensions)?;
        let source_end = source_start
            .checked_add(copy_width)
            .filter(|end| *end <= source.len())
            .ok_or_else(invalid_bgra_buffer_dimensions)?;
        let output_end = output_start
            .checked_add(copy_width)
            .filter(|end| *end <= output.len())
            .ok_or_else(invalid_bgra_buffer_dimensions)?;
        output[output_start..output_end].copy_from_slice(&source[source_start..source_end]);
    }
    Ok(())
}

fn invalid_bgra_buffer_dimensions() -> Error {
    Error::new(
        windows::core::HRESULT(0x8007_0057u32 as i32),
        "Composition BGRA buffer dimensions are invalid or too large",
    )
}

pub(super) struct CompositionResultOverlay {
    pub(super) _graphics_device: CompositionGraphicsDevice,
    _rendering_device: ID2D1Device,
    _surface: CompositionDrawingSurface,
    surface_interop: ICompositionDrawingSurfaceInterop,
    _surface_brush: CompositionSurfaceBrush,
    pub(super) viewport: ContainerVisual,
    group: ContainerVisual,
    bitmap: Option<ID2D1Bitmap1>,
    size: D2D_SIZE_U,
    dpi: u32,
    pixels: Vec<u8>,
    scratch: Vec<u8>,
    has_contents: bool,
}

pub(super) fn overlay_surface_error(
    operation: &str,
    error: windows::core::Error,
) -> windows::core::Error {
    Error::new(
        error.code(),
        format!("Composition result surface {operation}: {error}"),
    )
}

impl CompositionResultOverlay {
    /// # Safety
    /// `compositor` must remain a valid Composition interface usable on the
    /// current UI STA for the duration of surface and graphics-device creation.
    pub(super) unsafe fn new(
        compositor: &Compositor,
        dpi: u32,
        body_top: f32,
    ) -> windows::core::Result<Self> {
        let dpi = dpi.max(96);
        let size = composition_result_surface_size(dpi)?;
        let mut device = None;
        // SAFETY: The device creation call receives null/default optional
        // inputs and a valid, exclusively borrowed output slot.
        let hardware = unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        };
        if hardware.is_err() {
            device = None;
            // SAFETY: Same valid arguments and output-slot conditions as the
            // hardware attempt, using WARP as a local software fallback.
            unsafe {
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
        }
        let device: ID3D11Device = device.ok_or_else(Error::from_thread)?;
        let dxgi_device: IDXGIDevice = device.cast()?;
        // CompositionDrawingSurfaceInterop::BeginDraw is requested as an
        // ID2D1DeviceContext below, so back its graphics device with the D2D
        // device created from this D3D11 adapter. Keep the rendering-device
        // interface consistent with the requested drawing interface.
        // SAFETY: `dxgi_device` is a live interface returned by the device we
        // just created, and the default creation options are valid.
        let rendering_device = unsafe { D2D1CreateDevice(&dxgi_device, None)? };
        let compositor_interop: ICompositorInterop = compositor.cast()?;
        // SAFETY: Both COM interfaces are live and are used on this UI STA.
        let graphics_device =
            unsafe { compositor_interop.CreateGraphicsDevice(&rendering_device)? };
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

    pub(super) unsafe fn resize(&mut self, dpi: u32) -> windows::core::Result<()> {
        let dpi = dpi.max(96);
        if self.dpi == dpi {
            return Ok(());
        }
        let size = composition_result_surface_size(dpi)?;
        // SAFETY: This overlay owns the live drawing surface and all its
        // interop calls are confined to the surface's creating UI STA.
        unsafe {
            self.surface_interop.Resize(SIZE {
                cx: size.width as i32,
                cy: size.height as i32,
            })
        }
        .map_err(|error| overlay_surface_error("Resize", error))?;
        self.size = size;
        self.dpi = dpi;
        self.bitmap = None;
        self.pixels.clear();
        self.scratch.clear();
        self.has_contents = false;
        Ok(())
    }

    pub(super) fn set_viewport(&self, dpi: u32, body_top: f32) -> windows::core::Result<()> {
        self.viewport
            .SetOffset(composition_header_viewport_offset(dpi, body_top))
    }

    #[cfg(test)]
    /// # Safety
    /// The source and this overlay's Composition surface must remain live on
    /// the UI STA while the pixel copy is performed.
    pub(super) unsafe fn update_from_layered(
        &mut self,
        source: &LayeredSurface,
        capture_top: f32,
        body_top: f32,
        dpi: u32,
    ) -> windows::core::Result<()> {
        // SAFETY: The caller upholds this overlay's UI-STA surface contract.
        unsafe { self.resize(dpi)? };
        self.set_viewport(dpi, body_top)?;
        let source_bytes = source.pixel_bytes();
        let capture_top_px = (capture_top * self.dpi as f32 / 96.0).round() as i32;
        copy_panel_pixels_into(
            source_bytes,
            source.size.width,
            source.size.height,
            capture_top_px,
            self.size.width,
            self.size.height,
            &mut self.scratch,
        )?;

        // SAFETY: Scratch contains the checked panel region and the owned
        // surface is used from its creating STA.
        unsafe { self.refresh_surface_from_scratch() }
    }

    /// # Safety
    /// Must be called on this overlay's creating UI STA. `pixels` remains
    /// borrowed until its validated bytes have been copied into owned storage.
    pub(super) unsafe fn update_from_pixels(
        &mut self,
        pixels: &[u8],
        body_top: f32,
        dpi: u32,
    ) -> windows::core::Result<()> {
        // SAFETY: The caller upholds this overlay's UI-STA surface contract.
        unsafe { self.resize(dpi)? };
        self.set_viewport(dpi, body_top)?;
        let (_, expected_len) =
            bgra_buffer_layout(self.size.width, self.size.height).ok_or_else(|| {
                overlay_surface_error("UpdatePixels", invalid_bgra_buffer_dimensions())
            })?;
        if pixels.len() != expected_len {
            return Err(overlay_surface_error(
                "UpdatePixels",
                Error::new(
                    windows::core::HRESULT(0x80070057_u32 as i32),
                    format!(
                        "premultiplied card image has {} bytes; expected {expected_len}",
                        pixels.len()
                    ),
                ),
            ));
        }
        self.scratch
            .try_reserve_exact(expected_len.saturating_sub(self.scratch.len()))
            .map_err(|_| {
                overlay_surface_error(
                    "UpdatePixels",
                    Error::new(
                        windows::core::HRESULT(0x8007_000eu32 as i32),
                        "Unable to allocate the Composition pixel buffer",
                    ),
                )
            })?;
        self.scratch.clear();
        self.scratch.extend_from_slice(pixels);
        // SAFETY: Scratch owns an exact, checked BGRA buffer for the surface.
        unsafe { self.refresh_surface_from_scratch() }
    }

    /// # Safety
    /// Must run on the UI STA that owns `surface_interop`; the scratch vector
    /// must remain stable through the synchronous drawing calls.
    unsafe fn refresh_surface_from_scratch(&mut self) -> windows::core::Result<()> {
        if !self.has_contents || self.scratch != self.pixels {
            let (row_stride, expected_len) = bgra_buffer_layout(self.size.width, self.size.height)
                .ok_or_else(|| {
                    overlay_surface_error("UpdatePixels", invalid_bgra_buffer_dimensions())
                })?;
            if self.scratch.len() != expected_len {
                return Err(overlay_surface_error(
                    "UpdatePixels",
                    invalid_bgra_buffer_dimensions(),
                ));
            }
            let mut offset = POINT::default();
            // SAFETY: This overlay owns the live surface and accesses it only
            // on the UI STA where its interop interface was created.
            let context = unsafe {
                self.surface_interop
                    .BeginDraw::<ID2D1DeviceContext>(None, &mut offset)
            }
            .map_err(|error| overlay_surface_error("BeginDraw", error))?;
            let bitmap_was_cached = self.bitmap.is_some();
            let draw_result = (|| -> windows::core::Result<()> {
                // SAFETY: `context` came from BeginDraw above. Scratch has an
                // exact validated BGRA length and remains unchanged until the
                // synchronous Direct2D copy completes.
                unsafe {
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
                                row_stride,
                                &properties,
                            )
                            .map_err(|error| overlay_surface_error("CreateBitmap", error))?;
                        self.bitmap = Some(bitmap.clone());
                        bitmap
                    };
                    // A cached bitmap can survive a failed first draw while
                    // `has_contents` is still false. Refresh every reused
                    // bitmap so the next track cannot display stale pixels.
                    if bitmap_was_cached {
                        bitmap
                            .CopyFromMemory(None, self.scratch.as_ptr().cast(), row_stride)
                            .map_err(|error| overlay_surface_error("CopyFromMemory", error))?;
                    }
                    let transparent = d2d_color(0, 0, 0, 0.0);
                    context.Clear(Some(&transparent));
                    let destination =
                        rect(0.0, 0.0, self.size.width as f32, self.size.height as f32);
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
            // SAFETY: Pair with the successful BeginDraw above even when the
            // rendering closure returned an error.
            let end_result = unsafe { self.surface_interop.EndDraw() };
            draw_result?;
            end_result.map_err(|error| overlay_surface_error("EndDraw", error))?;
            std::mem::swap(&mut self.pixels, &mut self.scratch);
            self.has_contents = true;
        }

        Ok(())
    }

    pub(super) fn set_position(
        &self,
        panel_top: f32,
        body_top: f32,
        dpi: u32,
    ) -> windows::core::Result<()> {
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

    pub(super) fn animate_position(
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

    pub(super) fn hide(&self) -> windows::core::Result<()> {
        self.group
            .StopAnimation(&HSTRING::from("Offset.Y"))
            .map_err(|error| overlay_surface_error("Hide.StopAnimation.Offset.Y", error))?;
        self.group
            .SetOpacity(0.0)
            .map_err(|error| overlay_surface_error("Hide.SetOpacity", error))
    }
}

fn composition_result_surface_size(dpi: u32) -> windows::core::Result<D2D_SIZE_U> {
    let size = D2D_SIZE_U {
        width: composition_pixel_size(CONTENT_WIDTH as f32, dpi)
            .ok_or_else(invalid_bgra_buffer_dimensions)?,
        height: composition_pixel_size(TOP_HEIGHT as f32, dpi)
            .ok_or_else(invalid_bgra_buffer_dimensions)?,
    };
    bgra_buffer_layout(size.width, size.height).ok_or_else(invalid_bgra_buffer_dimensions)?;
    Ok(size)
}
