#![deny(unsafe_op_in_unsafe_fn)]

//! Windows tray notifications and DPI-sized artwork icons.

use std::{collections::VecDeque, mem::size_of};

use super::{
    ui_language, AppState, ArtworkPixels, PendingUpdateResult, Track, UiLanguage, TRAY_ICON_GUID,
    TRAY_ICON_ID,
};
use windows::{
    core::{Error, BOOL},
    Win32::{
        Foundation::HWND,
        Graphics::Gdi::{
            CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, BITMAPINFO,
            BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, RGBQUAD,
        },
        UI::{
            HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi},
            Shell::{
                Shell_NotifyIconW, NIF_GUID, NIF_INFO, NIIF_INFO, NIIF_LARGE_ICON, NIIF_USER,
                NIM_MODIFY, NOTIFYICONDATAW,
            },
            WindowsAndMessaging::{
                CreateIconIndirect, DestroyIcon, ICONINFO, SM_CXICON, SM_CYICON,
            },
        },
    },
};

pub(super) fn notify_hidden_match(
    app: &mut AppState,
    track: &Track,
    artwork: Option<&ArtworkPixels>,
) {
    let body = format!("{} — {}", track.title, track.artist);
    let notification_dpi = unsafe { GetDpiForWindow(app.main_hwnd) }.max(96);
    let balloon_icon =
        artwork.and_then(
            |pixels| match create_notification_icon(pixels, notification_dpi) {
                Ok(icon) => Some(icon),
                Err(error) => {
                    eprintln!("OpenShazam could not create notification artwork icon: {error}");
                    None
                }
            },
        );
    if artwork.is_none() {
        eprintln!("OpenShazam notification artwork unavailable; using the system information icon");
    }
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: app.main_hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_INFO | NIF_GUID,
        dwInfoFlags: if balloon_icon.is_some() {
            NIIF_USER | NIIF_LARGE_ICON
        } else {
            NIIF_INFO
        },
        hBalloonIcon: balloon_icon.unwrap_or_default(),
        guidItem: TRAY_ICON_GUID,
        ..Default::default()
    };
    copy_notification_text(&mut data.szInfoTitle, "Shazam");
    copy_notification_text(&mut data.szInfo, &body);
    if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
        // Explorer sends the matching end event after the balloon is no longer
        // using this custom icon. Keep None entries to preserve event ordering.
        queue_balloon_icon(&mut app.owned_balloon_icons, balloon_icon);
    } else if let Some(icon) = balloon_icon {
        let _ = unsafe { DestroyIcon(icon) };
    }
}

pub(super) fn notify_update_result(
    hwnd: HWND,
    result: &PendingUpdateResult,
    owned_balloon_icons: &mut VecDeque<Option<windows::Win32::UI::WindowsAndMessaging::HICON>>,
) {
    let language = ui_language();
    let (title, body) = match (language, result) {
        (UiLanguage::Japanese, PendingUpdateResult::Installed { version }) => (
            "OpenShazam",
            format!("OpenShazam をバージョン {version} に更新しました。"),
        ),
        (UiLanguage::Japanese, PendingUpdateResult::Failed { expected_version }) => (
            "OpenShazam",
            format!(
                "バージョン {expected_version} への更新を確認できませんでした。設定から更新を確認してください。"
            ),
        ),
        (UiLanguage::English, PendingUpdateResult::Installed { version }) => (
            "OpenShazam",
            format!("OpenShazam was updated to version {version}."),
        ),
        (UiLanguage::English, PendingUpdateResult::Failed { expected_version }) => (
            "OpenShazam",
            format!(
                "The update to version {expected_version} did not complete. Check for updates in Settings."
            ),
        ),
    };
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_INFO | NIF_GUID,
        dwInfoFlags: NIIF_INFO,
        guidItem: TRAY_ICON_GUID,
        ..Default::default()
    };
    copy_notification_text(&mut data.szInfoTitle, title);
    copy_notification_text(&mut data.szInfo, &body);
    if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
        // Even a system-icon balloon sends a balloon end event. Reserve a
        // matching queue slot so it cannot release a later artwork icon.
        queue_balloon_icon(owned_balloon_icons, None);
    } else {
        eprintln!("OpenShazam could not show the update result notification.");
    }
}

fn queue_balloon_icon<T>(owned: &mut VecDeque<Option<T>>, icon: Option<T>) {
    owned.push_back(icon);
}

pub(super) fn scale_notification_artwork_to_size(
    pixels: &ArtworkPixels,
    target_width: u32,
    target_height: u32,
) -> Option<Vec<u8>> {
    let source_width = usize::try_from(pixels.width).ok()?;
    let source_height = usize::try_from(pixels.height).ok()?;
    let target_width = usize::try_from(target_width).ok()?;
    let target_height = usize::try_from(target_height).ok()?;
    let expected_len = source_width.checked_mul(source_height)?.checked_mul(4)?;
    if target_width == 0
        || target_height == 0
        || source_width == 0
        || source_height == 0
        || pixels.bgra.len() != expected_len
    {
        return None;
    }
    let output_len = target_width.checked_mul(target_height)?.checked_mul(4)?;
    let target_aspect = target_width as f64 / target_height as f64;
    let source_aspect = source_width as f64 / source_height as f64;
    let (crop_width, crop_height) = if source_aspect > target_aspect {
        (source_height as f64 * target_aspect, source_height as f64)
    } else {
        (source_width as f64, source_width as f64 / target_aspect)
    };
    let crop_x = (source_width as f64 - crop_width) * 0.5;
    let crop_y = (source_height as f64 - crop_height) * 0.5;
    let mut output = vec![0u8; output_len];
    let corner_radius = target_width.min(target_height) as f64 / 8.0;

    for y in 0..target_height {
        let source_y = (crop_y + (y as f64 + 0.5) * crop_height / target_height as f64 - 0.5)
            .clamp(0.0, source_height.saturating_sub(1) as f64);
        let y0 = source_y.floor() as usize;
        let y1 = (y0 + 1).min(source_height - 1);
        let fy = source_y - y0 as f64;
        for x in 0..target_width {
            let source_x = (crop_x + (x as f64 + 0.5) * crop_width / target_width as f64 - 0.5)
                .clamp(0.0, source_width.saturating_sub(1) as f64);
            let x0 = source_x.floor() as usize;
            let x1 = (x0 + 1).min(source_width - 1);
            let fx = source_x - x0 as f64;
            let samples = [
                (y0 * source_width + x0) * 4,
                (y0 * source_width + x1) * 4,
                (y1 * source_width + x0) * 4,
                (y1 * source_width + x1) * 4,
            ];
            let destination = (y * target_width + x) * 4;
            for channel in 0..4 {
                let top = f64::from(pixels.bgra[samples[0] + channel]) * (1.0 - fx)
                    + f64::from(pixels.bgra[samples[1] + channel]) * fx;
                let bottom = f64::from(pixels.bgra[samples[2] + channel]) * (1.0 - fx)
                    + f64::from(pixels.bgra[samples[3] + channel]) * fx;
                output[destination + channel] =
                    (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
            }
            if target_width >= 8 && target_height >= 8 {
                let coverage =
                    rounded_corner_coverage(x, y, target_width, target_height, corner_radius);
                for channel in 0..4 {
                    let index = destination + channel;
                    output[index] = (f64::from(output[index]) * coverage)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    Some(output)
}

fn rounded_corner_coverage(x: usize, y: usize, width: usize, height: usize, radius: f64) -> f64 {
    // Signed-distance coverage produces a soft alpha edge at the four corners
    // without changing the square artwork edges or relying on Explorer masks.
    let half_width = width as f64 * 0.5;
    let half_height = height as f64 * 0.5;
    let qx = ((x as f64 + 0.5) - half_width).abs() - (half_width - radius);
    let qy = ((y as f64 + 0.5) - half_height).abs() - (half_height - radius);
    let outside_x = qx.max(0.0);
    let outside_y = qy.max(0.0);
    let signed_distance = outside_x.hypot(outside_y) + qx.max(qy).min(0.0) - radius;
    (0.5 - signed_distance).clamp(0.0, 1.0)
}

pub(super) fn create_notification_icon(
    pixels: &ArtworkPixels,
    dpi: u32,
) -> windows::core::Result<windows::Win32::UI::WindowsAndMessaging::HICON> {
    // NIIF_LARGE_ICON requires hBalloonIcon to use the large system-icon size.
    // This process is per-monitor DPI aware, so query the metrics for the
    // notification window's current monitor instead of using the system-DPI
    // values. A mismatched icon size can make Explorer silently use NIIF_INFO.
    // SAFETY: These system metric queries have no pointer arguments and accept
    // a DPI value clamped to Windows' supported minimum.
    let (width, height) = unsafe {
        (
            GetSystemMetricsForDpi(SM_CXICON, dpi.max(96)),
            GetSystemMetricsForDpi(SM_CYICON, dpi.max(96)),
        )
    };
    if !(1..=512).contains(&width) || !(1..=512).contains(&height) {
        return Err(Error::new(
            windows::core::HRESULT(0x8007_0057u32 as i32),
            "system notification icon dimensions are invalid",
        ));
    }
    let width = width as u32;
    let height = height as u32;
    let scaled = scale_notification_artwork_to_size(pixels, width, height).ok_or_else(|| {
        Error::new(
            windows::core::HRESULT(0x8007_0057u32 as i32),
            "notification artwork has invalid pixel dimensions",
        )
    })?;
    // SAFETY: Creating a memory DC with a null source creates an independent
    // compatible DC, which is released on every exit after this point.
    let dc = unsafe { CreateCompatibleDC(None) };
    if dc.0.is_null() {
        return Err(Error::from_thread());
    }

    let color_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut color_bits = std::ptr::null_mut();
    // SAFETY: `color_info` describes a bounded positive-size 32-bit DIB and
    // the returned pixel pointer is checked before it is read or written.
    let color_bitmap = match unsafe {
        CreateDIBSection(
            Some(dc),
            &color_info,
            DIB_RGB_COLORS,
            &mut color_bits,
            None,
            0,
        )
    } {
        Ok(bitmap) if !color_bits.is_null() => bitmap,
        Ok(bitmap) => {
            // SAFETY: These handles were created above and are still owned here.
            let _ = unsafe { DeleteObject(bitmap.into()) };
            let _ = unsafe { DeleteDC(dc) };
            return Err(Error::from_thread());
        }
        Err(error) => {
            // SAFETY: This is the live DC created above; no bitmap was returned.
            let _ = unsafe { DeleteDC(dc) };
            return Err(error);
        }
    };
    // SAFETY: The DIB has `width * height * 4` bytes, equal to `scaled.len()`;
    // the source and destination do not overlap and `color_bits` was validated.
    unsafe {
        std::ptr::copy_nonoverlapping(scaled.as_ptr(), color_bits.cast::<u8>(), scaled.len())
    };

    #[repr(C)]
    struct MonochromeBitmapInfo {
        header: BITMAPINFOHEADER,
        colors: [RGBQUAD; 2],
    }
    let mask_info = MonochromeBitmapInfo {
        header: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 1,
            biCompression: BI_RGB.0,
            biClrUsed: 2,
            ..Default::default()
        },
        colors: [
            RGBQUAD::default(),
            RGBQUAD {
                rgbBlue: 255,
                rgbGreen: 255,
                rgbRed: 255,
                rgbReserved: 0,
            },
        ],
    };
    let mut mask_bits = std::ptr::null_mut();
    // SAFETY: `mask_info` has the BITMAPINFO header layout followed by its two
    // color entries, and describes a bounded 1-bit monochrome DIB.
    let mask_bitmap = match unsafe {
        CreateDIBSection(
            Some(dc),
            (&mask_info as *const MonochromeBitmapInfo).cast::<BITMAPINFO>(),
            DIB_RGB_COLORS,
            &mut mask_bits,
            None,
            0,
        )
    } {
        Ok(bitmap) if !mask_bits.is_null() => bitmap,
        Ok(bitmap) => {
            // SAFETY: Both bitmap handles and the DC are owned by this function.
            let _ = unsafe { DeleteObject(bitmap.into()) };
            let _ = unsafe { DeleteObject(color_bitmap.into()) };
            let _ = unsafe { DeleteDC(dc) };
            return Err(Error::from_thread());
        }
        Err(error) => {
            // SAFETY: These handles were created above and are still owned here.
            let _ = unsafe { DeleteObject(color_bitmap.into()) };
            let _ = unsafe { DeleteDC(dc) };
            return Err(error);
        }
    };
    let mask_stride = (width as usize).div_ceil(32) * 4;
    // SAFETY: The 1-bit DIB provides `mask_stride * height` writable bytes;
    // its pointer was checked above and the stride uses DWORD row alignment.
    let mask = unsafe {
        std::ptr::write_bytes(mask_bits.cast::<u8>(), 0, mask_stride * height as usize);
        std::slice::from_raw_parts_mut(mask_bits.cast::<u8>(), mask_stride * height as usize)
    };
    for y in 0..height as usize {
        for x in 0..width as usize {
            if scaled[(y * width as usize + x) * 4 + 3] == 0 {
                let byte = y * mask_stride + x / 8;
                mask[byte] |= 0x80 >> (x % 8);
            }
        }
    }

    let icon_info = ICONINFO {
        fIcon: BOOL(1),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask_bitmap,
        hbmColor: color_bitmap,
    };
    // SAFETY: Both bitmap handles remain live through icon creation, and each
    // GDI resource is destroyed exactly once after CreateIconIndirect returns.
    let icon = unsafe { CreateIconIndirect(&icon_info) };
    let _ = unsafe { DeleteObject(mask_bitmap.into()) };
    let _ = unsafe { DeleteObject(color_bitmap.into()) };
    let _ = unsafe { DeleteDC(dc) };
    icon
}
pub(super) fn copy_notification_text(destination: &mut [u16], value: &str) {
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

#[cfg(test)]
mod tests {
    use super::queue_balloon_icon;
    use std::collections::VecDeque;

    #[test]
    fn system_icon_balloons_keep_artwork_icon_event_order() {
        let mut owned = VecDeque::new();
        queue_balloon_icon(&mut owned, Some(11));
        queue_balloon_icon(&mut owned, None);
        queue_balloon_icon(&mut owned, Some(29));

        assert_eq!(
            owned.into_iter().collect::<Vec<_>>(),
            [Some(11), None, Some(29)]
        );
    }
}
