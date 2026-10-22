#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::Instant,
};

use super::artwork_network::{
    curl_artwork_request, curl_resolve_entry, is_public_artwork_address,
    resolve_public_artwork_addresses_with_timeout, resource_host_port,
    supported_artwork_content_type, valid_https_resource_url, ARTWORK_FETCH_TIMEOUT,
    MAX_ARTWORK_REDIRECTS,
};
use super::{
    ArtworkPixels, ArtworkReady, ArtworkRequest, ArtworkRequestIdentity, ArtworkSource,
    WM_ARTWORK_LOAD_FINISHED, WM_ARTWORK_READY,
};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
        WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
    },
    System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    },
};

// A request may run standard and high-quality downloads in parallel. Keep
// rapid history scrolling from spawning an unbounded number of curl processes,
// while reserving one group for the newest recognition result.
const MAX_ACTIVE_ARTWORK_LOADS: usize = 4;
const MAX_ACTIVE_HISTORY_ARTWORK_LOADS: usize = MAX_ACTIVE_ARTWORK_LOADS - 1;
static ACTIVE_ARTWORK_LOADS: AtomicUsize = AtomicUsize::new(0);

struct ComInitializationGuard;

impl Drop for ComInitializationGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn initialize_artwork_com() -> windows::core::Result<ComInitializationGuard> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.map(|| ComInitializationGuard)
}

struct ArtworkLoadPermit<'a> {
    active: &'a AtomicUsize,
}

impl Drop for ArtworkLoadPermit<'_> {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn try_acquire_artwork_load_slot(
    active: &AtomicUsize,
    limit: usize,
) -> Option<ArtworkLoadPermit<'_>> {
    let mut current = active.load(Ordering::Acquire);
    loop {
        if current >= limit {
            return None;
        }
        match active.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Some(ArtworkLoadPermit { active }),
            Err(observed) => current = observed,
        }
    }
}

pub(super) fn start_artwork_load(
    hwnd: HWND,
    generation: u64,
    request: ArtworkRequest,
    _dpi: u32,
) -> bool {
    start_artwork_load_with_limit(hwnd, generation, request, MAX_ACTIVE_ARTWORK_LOADS)
}

pub(super) fn start_history_artwork_load(
    hwnd: HWND,
    generation: u64,
    request: ArtworkRequest,
    _dpi: u32,
) -> bool {
    start_artwork_load_with_limit(hwnd, generation, request, MAX_ACTIVE_HISTORY_ARTWORK_LOADS)
}

fn start_artwork_load_with_limit(
    hwnd: HWND,
    generation: u64,
    request: ArtworkRequest,
    limit: usize,
) -> bool {
    let hwnd_value = hwnd.0 as isize;
    let identity = request.identity;
    let mut jobs = Vec::with_capacity(2);
    if let Some(url) = request
        .standard_url
        .as_deref()
        .filter(|url| valid_https_resource_url(url))
    {
        jobs.push((ArtworkSource::Standard, url.to_owned()));
    }
    if let Some(url) = request
        .high_quality_url
        .as_deref()
        .filter(|url| valid_https_resource_url(url))
    {
        if !jobs.iter().any(|(_, existing)| existing == url) {
            jobs.push((ArtworkSource::HighQuality, url.to_owned()));
        }
    }
    if jobs.is_empty() {
        return false;
    }
    let Some(permit) = try_acquire_artwork_load_slot(&ACTIVE_ARTWORK_LOADS, limit) else {
        return false;
    };
    let spawned = thread::Builder::new()
        .name("openshazam-artwork-load".to_owned())
        .spawn(move || {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                thread::scope(|scope| {
                    let (sender, receiver) = std::sync::mpsc::channel();
                    for (source, url) in jobs {
                        let sender = sender.clone();
                        scope.spawn(move || {
                            let result = fetch_and_decode_artwork(&url);
                            let _ = sender.send((source, result));
                        });
                    }
                    drop(sender);

                    // Apply each result as soon as its download and decode finish. In
                    // particular, a slow standard endpoint must not hold back the
                    // high-resolution endpoint (or vice versa).
                    for (source, result) in receiver {
                        if let Ok(pixels) = result {
                            post_artwork_ready(hwnd_value, generation, identity, source, pixels);
                        }
                    }
                });
            }));
            // Free the slot before notifying the UI so its next visible artwork
            // request can start as soon as this completion is dispatched.
            drop(permit);
            post_artwork_load_finished(hwnd_value, generation, identity);
        });
    match spawned {
        Ok(_) => true,
        Err(error) => {
            eprintln!("OpenShazam could not start an artwork worker: {error}");
            false
        }
    }
}

fn fetch_and_decode_artwork(url: &str) -> Result<ArtworkPixels, String> {
    if !valid_https_resource_url(url) {
        return Err("artwork URL was not a public HTTPS resource".to_owned());
    }
    let started_at = Instant::now();
    let mut current_url = url.to_owned();
    for redirect_count in 0..=MAX_ARTWORK_REDIRECTS {
        let (host, port) = resource_host_port(&current_url)
            .ok_or_else(|| "artwork URL authority was invalid".to_owned())?;
        let remaining = ARTWORK_FETCH_TIMEOUT.saturating_sub(started_at.elapsed());
        if remaining.is_zero() {
            return Err("artwork download timed out".to_owned());
        }
        let addresses = resolve_public_artwork_addresses_with_timeout(&host, port, remaining)?;
        let resolve = curl_resolve_entry(&host, port, &addresses);
        let remaining = ARTWORK_FETCH_TIMEOUT.saturating_sub(started_at.elapsed());
        if remaining.is_zero() {
            return Err("artwork download timed out".to_owned());
        }
        let response = curl_artwork_request(&current_url, resolve.as_deref(), remaining)?;
        if !addresses.contains(&response.remote_ip)
            || !is_public_artwork_address(response.remote_ip)
        {
            return Err("artwork connection did not use its validated public address".to_owned());
        }
        if let Some(target) =
            validate_artwork_redirect(response.status, &response.redirect_url, redirect_count)?
        {
            current_url = target.to_owned();
            continue;
        }
        if response.status != 200 {
            return Err(format!("artwork server returned HTTP {}", response.status));
        }
        if !supported_artwork_content_type(&response.content_type) {
            return Err("artwork response MIME type was unsupported".to_owned());
        }
        return decode_artwork_wic(&response.body);
    }
    Err("artwork redirect was invalid or exceeded its limit".to_owned())
}

fn validate_artwork_redirect(
    status: u16,
    redirect_url: &str,
    redirects_followed: usize,
) -> Result<Option<&str>, String> {
    if !(300..400).contains(&status) || redirect_url.is_empty() {
        return Ok(None);
    }
    if !matches!(status, 301 | 302 | 303 | 307 | 308)
        || redirects_followed >= MAX_ARTWORK_REDIRECTS
        || !valid_https_resource_url(redirect_url)
    {
        return Err("artwork redirect was invalid or exceeded its limit".to_owned());
    }
    Ok(Some(redirect_url))
}

pub(super) fn artwork_pixel_fingerprint(width: u32, height: u32, bgra: &[u8]) -> u128 {
    // A small content key lets the renderer replace a bitmap when a track's
    // artwork bytes are refreshed under the same display identity. It is only
    // a process-local cache key, not a security or file-integrity hash.
    let bytes = width
        .to_le_bytes()
        .into_iter()
        .chain(height.to_le_bytes())
        .chain(bgra.iter().copied());
    let mut left = 0xcbf2_9ce4_8422_2325u64;
    let mut right = 0x8422_2325_cbf2_9ce4u64;
    for byte in bytes {
        left = (left ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        right = (right ^ u64::from(byte).wrapping_add(0x9d)).wrapping_mul(0x9e37_79b1_85eb_ca87);
    }
    (u128::from(left) << 64) | u128::from(right)
}

fn decode_artwork_wic(encoded: &[u8]) -> Result<ArtworkPixels, String> {
    let _com = initialize_artwork_com()
        .map_err(|error| format!("artwork COM initialization failed: {error}"))?;
    let result = unsafe {
        (|| -> windows::core::Result<ArtworkPixels> {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let stream = factory.CreateStream()?;
            stream.InitializeFromMemory(encoded)?;
            let decoder = factory.CreateDecoderFromStream(
                &stream,
                std::ptr::null(),
                WICDecodeMetadataCacheOnDemand,
            )?;
            let frame = decoder.GetFrame(0)?;
            let mut width = 0u32;
            let mut height = 0u32;
            frame.GetSize(&mut width, &mut height)?;
            let (stride, byte_len) = artwork_buffer_layout(width, height).ok_or_else(|| {
                windows::core::Error::new(
                    windows::core::HRESULT(0x8007_0057u32 as i32),
                    "artwork dimensions are outside the supported range",
                )
            })?;
            let mut bgra = vec![0u8; byte_len];
            let converter = factory.CreateFormatConverter()?;
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            converter.CopyPixels(std::ptr::null(), stride, &mut bgra)?;
            let fingerprint = artwork_pixel_fingerprint(width, height, &bgra);
            Ok(ArtworkPixels {
                width,
                height,
                fingerprint,
                bgra,
            })
        })()
    };
    result.map_err(|error| format!("artwork decode failed: {error}"))
}

fn artwork_buffer_layout(width: u32, height: u32) -> Option<(u32, usize)> {
    if width == 0 || height == 0 || width > 2_048 || height > 2_048 {
        return None;
    }
    let stride = width.checked_mul(4)?;
    let byte_len = usize::try_from(stride)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?;
    Some((stride, byte_len))
}

pub(super) fn artwork_pixels_row_stride(pixels: &ArtworkPixels) -> Option<u32> {
    let (stride, expected_len) = artwork_buffer_layout(pixels.width, pixels.height)?;
    (pixels.bgra.len() == expected_len).then_some(stride)
}

fn post_artwork_ready(
    hwnd: isize,
    generation: u64,
    identity: ArtworkRequestIdentity,
    source: ArtworkSource,
    pixels: ArtworkPixels,
) {
    let _ = super::message_payload::post(
        HWND(hwnd as *mut _),
        generation,
        WM_ARTWORK_READY,
        ArtworkReady {
            identity,
            source,
            pixels,
        },
    );
}

fn post_artwork_load_finished(hwnd: isize, generation: u64, identity: ArtworkRequestIdentity) {
    let _ = super::message_payload::post(
        HWND(hwnd as *mut _),
        generation,
        WM_ARTWORK_LOAD_FINISHED,
        identity,
    );
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use super::{
        artwork_buffer_layout, artwork_pixels_row_stride, initialize_artwork_com,
        try_acquire_artwork_load_slot, validate_artwork_redirect, MAX_ACTIVE_ARTWORK_LOADS,
        MAX_ACTIVE_HISTORY_ARTWORK_LOADS, MAX_ARTWORK_REDIRECTS,
    };
    use crate::windows_ui::ArtworkPixels;

    #[test]
    fn artwork_com_guard_releases_its_thread_apartment_initialization() {
        let can_initialize_sta_after_guard_drop = std::thread::spawn(|| {
            initialize_artwork_com().expect("MTA initialization should succeed");
            let initialized = unsafe {
                windows::Win32::System::Com::CoInitializeEx(
                    None,
                    windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
                )
            };
            if initialized.is_ok() {
                unsafe { windows::Win32::System::Com::CoUninitialize() };
                true
            } else {
                false
            }
        })
        .join()
        .expect("COM test thread should complete");

        assert!(can_initialize_sta_after_guard_drop);
    }

    #[test]
    fn artwork_load_slots_bound_concurrent_download_groups() {
        let active = AtomicUsize::new(0);
        let mut permits = (0..MAX_ACTIVE_ARTWORK_LOADS)
            .map(|_| {
                try_acquire_artwork_load_slot(&active, MAX_ACTIVE_ARTWORK_LOADS)
                    .expect("slot should be available")
            })
            .collect::<Vec<_>>();
        assert_eq!(active.load(Ordering::Acquire), MAX_ACTIVE_ARTWORK_LOADS);
        assert!(try_acquire_artwork_load_slot(&active, MAX_ACTIVE_ARTWORK_LOADS).is_none());

        drop(permits.pop());
        let replacement = try_acquire_artwork_load_slot(&active, MAX_ACTIVE_ARTWORK_LOADS)
            .expect("a released slot should be reusable");
        assert_eq!(active.load(Ordering::Acquire), MAX_ACTIVE_ARTWORK_LOADS);
        drop(replacement);
        drop(permits);
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn history_artwork_leaves_a_worker_slot_for_new_recognition_results() {
        let active = AtomicUsize::new(0);
        let history_permits = (0..MAX_ACTIVE_HISTORY_ARTWORK_LOADS)
            .map(|_| {
                try_acquire_artwork_load_slot(&active, MAX_ACTIVE_HISTORY_ARTWORK_LOADS)
                    .expect("history slot should be available")
            })
            .collect::<Vec<_>>();
        assert!(try_acquire_artwork_load_slot(&active, MAX_ACTIVE_HISTORY_ARTWORK_LOADS).is_none());
        let recognition = try_acquire_artwork_load_slot(&active, MAX_ACTIVE_ARTWORK_LOADS)
            .expect("a recognition result should use the reserved slot");
        assert_eq!(active.load(Ordering::Acquire), MAX_ACTIVE_ARTWORK_LOADS);
        drop(recognition);
        drop(history_permits);
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn concurrent_artwork_slot_acquisition_never_exceeds_the_limit() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..32 {
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            workers.push(std::thread::spawn(move || {
                if let Some(_permit) =
                    try_acquire_artwork_load_slot(&active, MAX_ACTIVE_ARTWORK_LOADS)
                {
                    let current = active.load(Ordering::Acquire);
                    peak.fetch_max(current, Ordering::AcqRel);
                    std::thread::yield_now();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(peak.load(Ordering::Acquire) <= MAX_ACTIVE_ARTWORK_LOADS);
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn artwork_redirects_accept_only_standard_https_redirects_within_the_limit() {
        let target = "https://cdn.example.test/cover.webp";
        assert_eq!(
            validate_artwork_redirect(302, target, 0).unwrap(),
            Some(target)
        );
        assert_eq!(validate_artwork_redirect(200, target, 0).unwrap(), None);
        assert_eq!(validate_artwork_redirect(302, "", 0).unwrap(), None);
        assert!(validate_artwork_redirect(304, target, 0).is_err());
        assert!(validate_artwork_redirect(302, target, MAX_ARTWORK_REDIRECTS).is_err());
        assert!(validate_artwork_redirect(302, "http://cdn.example.test/cover.webp", 0).is_err());
        assert!(validate_artwork_redirect(302, "https://127.0.0.1/cover.webp", 0).is_err());
    }

    #[test]
    fn artwork_buffer_layout_bounds_decoded_dimensions_and_memory() {
        assert_eq!(artwork_buffer_layout(1, 1), Some((4, 4)));
        assert_eq!(
            artwork_buffer_layout(2_048, 2_048),
            Some((8_192, 16_777_216))
        );
        assert_eq!(artwork_buffer_layout(0, 1), None);
        assert_eq!(artwork_buffer_layout(1, 0), None);
        assert_eq!(artwork_buffer_layout(2_049, 1), None);
        assert_eq!(artwork_buffer_layout(1, 2_049), None);
    }

    #[test]
    fn artwork_pixel_row_stride_requires_a_bounded_exact_bgra_buffer() {
        let valid = ArtworkPixels {
            width: 2,
            height: 2,
            fingerprint: 0,
            bgra: vec![0; 16],
        };
        assert_eq!(artwork_pixels_row_stride(&valid), Some(8));

        let mut truncated = valid.clone();
        truncated.bgra.pop();
        assert_eq!(artwork_pixels_row_stride(&truncated), None);

        let oversized = ArtworkPixels {
            width: 2_049,
            height: 1,
            fingerprint: 0,
            bgra: vec![0; 2_049 * 4],
        };
        assert_eq!(artwork_pixels_row_stride(&oversized), None);
    }
}
