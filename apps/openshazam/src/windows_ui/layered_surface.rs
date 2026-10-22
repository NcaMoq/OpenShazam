#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;

use windows::{
    core::{Error, HRESULT},
    Win32::{
        Foundation::{HWND, POINT, RECT, SIZE},
        Graphics::{
            Direct2D::Common::D2D_SIZE_U,
            Gdi::{
                CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject,
                AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
                DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
            },
        },
        UI::WindowsAndMessaging::{GetWindowRect, UpdateLayeredWindow, ULW_ALPHA},
    },
};

/// Owns the premultiplied BGRA DIB and GDI handles used to present the popup.
/// Keeping the selected bitmap and its DC together ensures they are restored
/// and released in the correct order when the surface is dropped.
pub(super) struct LayeredSurface {
    pub(super) dc: HDC,
    bitmap: HBITMAP,
    previous_bitmap: HGDIOBJ,
    pub(super) size: D2D_SIZE_U,
    pixel_len: usize,
    pixels: *mut c_void,
}

pub(super) fn layered_surface_pixel_len(width: u32, height: u32) -> Option<usize> {
    super::bgra_buffer_layout(width, height).map(|(_, byte_len)| byte_len)
}

impl LayeredSurface {
    pub(super) fn new(width: u32, height: u32) -> windows::core::Result<Self> {
        let width = width.max(1);
        let height = height.max(1);
        let pixel_len = layered_surface_pixel_len(width, height).ok_or_else(|| {
            Error::new(
                HRESULT(0x80070057_u32 as i32),
                "Layered popup surface dimensions are too large",
            )
        })?;
        // SAFETY: Creates a private memory DC; ownership is transferred to this
        // surface after the DIB is selected successfully.
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.0.is_null() {
            return Err(Error::from_thread());
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = std::ptr::null_mut();
        // SAFETY: `dc`, `info`, and the writable output pointer remain valid
        // for the synchronous GDI call. The returned DIB is owned below.
        let bitmap = match unsafe {
            CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut pixels, None, 0)
        } {
            Ok(bitmap) => bitmap,
            Err(error) => {
                // SAFETY: `dc` was created above and has not escaped.
                let _ = unsafe { DeleteDC(dc) };
                return Err(error);
            }
        };
        // SAFETY: Selects the newly created bitmap into the private memory DC.
        let previous_bitmap = unsafe { SelectObject(dc, bitmap.into()) };
        let previous_bitmap_is_valid =
            !previous_bitmap.0.is_null() && previous_bitmap.0 as isize != -1;
        if pixels.is_null() || !previous_bitmap_is_valid {
            // SAFETY: Handles are private to this constructor. Restore the
            // original bitmap when selection succeeded; otherwise destroy the
            // DC first so the DIB is no longer selected before deleting it.
            unsafe {
                if previous_bitmap_is_valid {
                    let _ = SelectObject(dc, previous_bitmap);
                }
                let _ = DeleteDC(dc);
                let _ = DeleteObject(bitmap.into());
            }
            return Err(Error::from_thread());
        }
        Ok(Self {
            dc,
            bitmap,
            previous_bitmap,
            size: D2D_SIZE_U { width, height },
            pixel_len,
            pixels,
        })
    }

    pub(super) fn pixel_bytes(&self) -> &[u8] {
        // SAFETY: `new` stores the DIB pointer and checked byte length together;
        // the selected bitmap remains owned by this surface until it is dropped.
        unsafe { std::slice::from_raw_parts(self.pixels.cast::<u8>(), self.pixel_len) }
    }

    #[cfg(test)]
    pub(super) fn pixel_bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: The exclusive borrow prevents another Rust caller from using
        // this surface while the slice is alive, and `new` validated the DIB span.
        unsafe { std::slice::from_raw_parts_mut(self.pixels.cast::<u8>(), self.pixel_len) }
    }

    pub(super) fn present(&self, hwnd: HWND, opacity: u8) -> windows::core::Result<()> {
        let mut window = RECT::default();
        // SAFETY: The caller supplies the live popup HWND on the UI thread.
        unsafe { GetWindowRect(hwnd, &mut window)? };
        let destination = POINT {
            x: window.left,
            y: window.top,
        };
        let source = POINT::default();
        let size = SIZE {
            cx: window.right - window.left,
            cy: window.bottom - window.top,
        };
        if size.cx.max(0) as u32 != self.size.width || size.cy.max(0) as u32 != self.size.height {
            return Err(Error::new(
                HRESULT(0x80070057_u32 as i32),
                "Layered popup surface size does not match the window client size",
            ));
        }
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: opacity,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: All input structs and the owned memory DC remain valid during
        // this synchronous composition call.
        unsafe {
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
}

impl Drop for LayeredSurface {
    fn drop(&mut self) {
        // SAFETY: These handles are exclusively owned by the surface. Restore
        // the stock bitmap before deleting the DIB, then release the DC.
        unsafe {
            let _ = SelectObject(self.dc, self.previous_bitmap);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}
