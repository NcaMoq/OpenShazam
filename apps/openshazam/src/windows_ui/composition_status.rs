#![deny(unsafe_op_in_unsafe_fn)]

use super::{
    composition_scale_for_dpi, create_text_format_with_locale, d2d_color, directwrite_locale, rect,
    status_text_linear_progress, ui_language, StatusTextSnapshot, UiLanguage, CONTENT_WIDTH,
    STATUS_TEXT_HEIGHT, STATUS_TEXT_TOP, STATUS_TEXT_TRANSITION_DURATION,
};
use std::time::Instant;
use windows::{
    core::{Interface, HSTRING},
    Foundation::TimeSpan,
    Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat},
    Win32::{
        Foundation::{POINT, SIZE},
        Graphics::{
            Direct2D::{
                Common::D2D_SIZE_U, ID2D1DeviceContext, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
            },
            DirectWrite::{
                IDWriteFactory, IDWriteTextFormat, DWRITE_FONT_WEIGHT_BOLD,
                DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_ALIGNMENT_CENTER,
            },
        },
        System::WinRT::Composition::ICompositionDrawingSurfaceInterop,
    },
    UI::Composition::{
        AnimationController, CompositionDrawingSurface, CompositionGraphicsDevice,
        CompositionStretch, CompositionSurfaceBrush, Compositor, ContainerVisual,
        CubicBezierEasingFunction, SpriteVisual,
    },
};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

pub(super) struct CompositionStatusLabel {
    _surface: CompositionDrawingSurface,
    interop: ICompositionDrawingSurfaceInterop,
    _brush: CompositionSurfaceBrush,
    visual: SpriteVisual,
    pub(super) size: D2D_SIZE_U,
    pub(super) text: Option<String>,
}

impl CompositionStatusLabel {
    /// # Safety
    /// `compositor` and `graphics_device` must be live Composition interfaces
    /// usable on the current UI STA while this label's surface is created.
    pub(super) unsafe fn new(
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

    /// # Safety
    /// Must run on the UI STA that owns this label's drawing surface.
    pub(super) unsafe fn resize(&mut self, dpi: u32) -> windows::core::Result<()> {
        let size = status_text_surface_size(dpi);
        if size == self.size {
            return Ok(());
        }
        // SAFETY: This label owns the live surface and all interop operations
        // are confined to its creating UI STA.
        unsafe {
            self.interop.Resize(SIZE {
                cx: size.width as i32,
                cy: size.height as i32,
            })?;
        }
        self.size = size;
        self.text = None;
        Ok(())
    }

    /// # Safety
    /// Must run on the UI STA that owns the drawing surface; the DirectWrite
    /// format must remain live throughout the synchronous draw.
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
        // SAFETY: This label owns the surface and uses it only on the creating
        // UI STA; the matching EndDraw follows the draw closure.
        let context = unsafe {
            self.interop
                .BeginDraw::<ID2D1DeviceContext>(None, &mut offset)?
        };
        let draw_result = (|| -> windows::core::Result<()> {
            // SAFETY: `context` came from BeginDraw above. `content`, `format`,
            // brush, and draw bounds remain alive for the synchronous Direct2D
            // calls; text length is provided explicitly by the wrapper.
            unsafe {
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
            }
        })();
        // SAFETY: Pair with the successful BeginDraw above even when drawing
        // returned an error.
        let end_result = unsafe { self.interop.EndDraw() };
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

pub(super) struct CompositionStatusText {
    compositor: Compositor,
    pub(super) root: ContainerVisual,
    pub(super) labels: [CompositionStatusLabel; 2],
    write_factory: IDWriteFactory,
    format: IDWriteTextFormat,
    easing: CubicBezierEasingFunction,
    pub(super) controllers: [Option<AnimationController>; 2],
    pub(super) dpi: u32,
    pub(super) language: UiLanguage,
    body_top: f32,
    last_state: Option<StatusTextSnapshot>,
}

impl CompositionStatusText {
    /// # Safety
    /// The compositor, graphics device, and DirectWrite factory must be live
    /// and usable on the current UI STA for the lifetime of the created text.
    pub(super) unsafe fn new(
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
            // SAFETY: Both labels are created on the UI STA that owns the
            // compositor and graphics device passed to this constructor.
            unsafe { CompositionStatusLabel::new(compositor, graphics_device, dpi)? },
            // SAFETY: Same UI-STA and interface lifetime conditions as above.
            unsafe { CompositionStatusLabel::new(compositor, graphics_device, dpi)? },
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

    /// # Safety
    /// Must be called on the UI STA that owns this text's Composition surfaces.
    pub(super) unsafe fn update(
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
                // SAFETY: Each label's drawing surface is owned by this UI STA.
                unsafe { label.resize(dpi)? };
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
            // SAFETY: Status text surfaces and format are live on this UI STA.
            unsafe { self.labels[0].set_text(previous.text(language), &self.format, dpi)? };
            // SAFETY: Same surface and format lifetime conditions as above.
            unsafe { self.labels[1].set_text(current.text(language), &self.format, dpi)? };
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
            // SAFETY: The status label and text format are live on this UI STA.
            unsafe { self.labels[0].set_text(current.text(language), &self.format, dpi)? };
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

    /// # Safety
    /// Must be called on the UI STA that owns the Composition drawing surfaces.
    pub(super) unsafe fn resize(&mut self, dpi: u32) -> windows::core::Result<()> {
        for label in &mut self.labels {
            // SAFETY: Each label belongs to this text compositor on this UI STA.
            unsafe { label.resize(dpi)? };
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
