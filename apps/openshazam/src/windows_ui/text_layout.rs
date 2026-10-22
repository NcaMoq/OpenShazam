#![deny(unsafe_op_in_unsafe_fn)]

use std::{cell::RefCell, collections::HashMap};

use super::{
    create_text_format_with_locale, directwrite_locale, ui_language, IDWriteFactory,
    IDWriteTextFormat, Track, HISTORY_TEXT_LEFT, HISTORY_TEXT_RIGHT, RESULT_ARTIST_MAX_LINES,
    RESULT_ARTIST_MIN_HEIGHT, RESULT_DETAILS_LEFT, RESULT_DETAILS_RIGHT, RESULT_TITLE_MAX_LINES,
    RESULT_TITLE_MIN_HEIGHT,
};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct MeasuredTextLayout {
    pub(super) height: f32,
    #[cfg(test)]
    pub(super) first_baseline: f32,
    #[cfg(test)]
    pub(super) last_baseline: f32,
    #[cfg(test)]
    pub(super) overhang_left: f32,
    #[cfg(test)]
    pub(super) overhang_top: f32,
    #[cfg(test)]
    pub(super) overhang_right: f32,
    #[cfg(test)]
    pub(super) overhang_bottom: f32,
}

impl MeasuredTextLayout {
    fn with_height(height: f32) -> Self {
        Self {
            height,
            #[cfg(test)]
            first_baseline: 0.0,
            #[cfg(test)]
            last_baseline: 0.0,
            #[cfg(test)]
            overhang_left: 0.0,
            #[cfg(test)]
            overhang_top: 0.0,
            #[cfg(test)]
            overhang_right: 0.0,
            #[cfg(test)]
            overhang_bottom: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct HistoryTextMetrics {
    pub(super) title: MeasuredTextLayout,
    pub(super) artist: MeasuredTextLayout,
    pub(super) date: MeasuredTextLayout,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ResultDetailsMetrics {
    pub(super) title_height: f32,
    pub(super) artist_height: f32,
}

struct CachedHistoryTextMetrics {
    title: String,
    artist: String,
    date: String,
    metrics: HistoryTextMetrics,
}

struct CachedResultDetailsMetrics {
    title: String,
    artist: String,
    metrics: ResultDetailsMetrics,
}

pub(super) struct TextStyles {
    pub(super) title: IDWriteTextFormat,
    pub(super) subtitle: IDWriteTextFormat,
    pub(super) date: IDWriteTextFormat,
    pub(super) result_title: IDWriteTextFormat,
    pub(super) result_artist: IDWriteTextFormat,
    pub(super) button: IDWriteTextFormat,
    pub(super) centered_body: IDWriteTextFormat,
    pub(super) centered_title: IDWriteTextFormat,
    pub(super) centered_heavy: IDWriteTextFormat,
}

pub(super) struct TextLayout {
    pub(super) styles: TextStyles,
    result_details_cache: RefCell<Option<CachedResultDetailsMetrics>>,
    history_text_metrics_cache: RefCell<HashMap<u64, CachedHistoryTextMetrics>>,
}

impl TextLayout {
    pub(super) fn new(factory: &IDWriteFactory) -> windows::core::Result<Self> {
        Ok(Self {
            styles: create_text_styles(factory)?,
            result_details_cache: RefCell::new(None),
            history_text_metrics_cache: RefCell::new(HashMap::new()),
        })
    }

    pub(super) fn refresh(&mut self, factory: &IDWriteFactory) -> windows::core::Result<()> {
        let styles = create_text_styles(factory)?;
        self.styles = styles;
        *self.result_details_cache.borrow_mut() = None;
        self.history_text_metrics_cache.borrow_mut().clear();
        Ok(())
    }

    pub(super) fn result_details_metrics(
        &self,
        factory: &IDWriteFactory,
        track: &Track,
    ) -> windows::core::Result<ResultDetailsMetrics> {
        if let Some(cached) = self.result_details_cache.borrow().as_ref() {
            if cached.title == track.title && cached.artist == track.artist {
                return Ok(cached.metrics);
            }
        }
        let metrics = ResultDetailsMetrics {
            title_height: measure_text_height(
                factory,
                &track.title,
                &self.styles.result_title,
                RESULT_DETAILS_RIGHT - RESULT_DETAILS_LEFT,
                RESULT_TITLE_MIN_HEIGHT,
                RESULT_TITLE_MAX_LINES,
            )?,
            artist_height: measure_text_height(
                factory,
                &track.artist,
                &self.styles.result_artist,
                RESULT_DETAILS_RIGHT - RESULT_DETAILS_LEFT,
                RESULT_ARTIST_MIN_HEIGHT,
                RESULT_ARTIST_MAX_LINES,
            )?,
        };
        *self.result_details_cache.borrow_mut() = Some(CachedResultDetailsMetrics {
            title: track.title.clone(),
            artist: track.artist.clone(),
            metrics,
        });
        Ok(metrics)
    }

    pub(super) fn history_metrics(
        &self,
        factory: &IDWriteFactory,
        track: &Track,
    ) -> windows::core::Result<HistoryTextMetrics> {
        if let Some(cached) = self
            .history_text_metrics_cache
            .borrow()
            .get(&track.display_id)
        {
            if cached.title == track.title
                && cached.artist == track.artist
                && cached.date == track.recognized_date_label
            {
                return Ok(cached.metrics);
            }
        }
        let width = HISTORY_TEXT_RIGHT - HISTORY_TEXT_LEFT;
        let metrics = HistoryTextMetrics {
            title: measure_directwrite_text_layout(
                factory,
                &track.title,
                &self.styles.title,
                width,
                0.0,
                2.0,
            )?,
            artist: measure_directwrite_text_layout(
                factory,
                &track.artist,
                &self.styles.subtitle,
                width,
                0.0,
                2.0,
            )?,
            date: measure_directwrite_text_layout(
                factory,
                &track.recognized_date_label,
                &self.styles.date,
                width,
                0.0,
                1.0,
            )?,
        };
        let mut cache = self.history_text_metrics_cache.borrow_mut();
        cache.insert(
            track.display_id,
            CachedHistoryTextMetrics {
                title: track.title.clone(),
                artist: track.artist.clone(),
                date: track.recognized_date_label.clone(),
                metrics,
            },
        );
        if cache.len() > 64 {
            if let Some(oldest_id) = cache.keys().copied().min() {
                cache.remove(&oldest_id);
            }
        }
        Ok(metrics)
    }
}

fn create_text_format(
    factory: &IDWriteFactory,
    size: f32,
    weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT,
    alignment: windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT,
) -> windows::core::Result<IDWriteTextFormat> {
    create_text_format_with_locale(
        factory,
        size,
        weight,
        alignment,
        directwrite_locale(ui_language()),
    )
}

fn create_text_styles(factory: &IDWriteFactory) -> windows::core::Result<TextStyles> {
    use windows::Win32::Graphics::DirectWrite::{
        DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_DEMI_BOLD, DWRITE_FONT_WEIGHT_HEAVY,
        DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    };

    Ok(TextStyles {
        title: create_text_format(
            factory,
            14.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?,
        subtitle: create_text_format(
            factory,
            12.0,
            DWRITE_FONT_WEIGHT_DEMI_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?,
        date: create_text_format(
            factory,
            10.0,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?,
        result_title: create_text_format(
            factory,
            20.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?,
        result_artist: create_text_format(
            factory,
            14.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?,
        button: create_text_format(
            factory,
            13.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?,
        centered_body: create_text_format(
            factory,
            12.0,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?,
        centered_title: create_text_format(
            factory,
            16.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?,
        centered_heavy: create_text_format(
            factory,
            18.0,
            DWRITE_FONT_WEIGHT_HEAVY,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?,
    })
}

fn measure_text_height(
    factory: &IDWriteFactory,
    value: &str,
    format: &IDWriteTextFormat,
    max_width: f32,
    min_height: f32,
    max_lines: f32,
) -> windows::core::Result<f32> {
    Ok(
        measure_directwrite_text_layout(factory, value, format, max_width, min_height, max_lines)?
            .height,
    )
}

pub(super) fn measure_directwrite_text_layout(
    factory: &IDWriteFactory,
    value: &str,
    format: &IDWriteTextFormat,
    max_width: f32,
    min_height: f32,
    max_lines: f32,
) -> windows::core::Result<MeasuredTextLayout> {
    if value.is_empty() {
        return Ok(MeasuredTextLayout::with_height(min_height));
    }
    let content: Vec<u16> = value.encode_utf16().collect();
    // SAFETY: `content` is an owned UTF-16 buffer whose allocation outlives
    // the synchronous, length-taking DirectWrite call.
    let layout = unsafe { factory.CreateTextLayout(&content, format, max_width, 512.0)? };
    let mut text_metrics = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
    // SAFETY: `text_metrics` is a writable initialized output value.
    unsafe { layout.GetMetrics(&mut text_metrics)? };
    if text_metrics.lineCount == 0 {
        return Ok(MeasuredTextLayout::with_height(min_height));
    }
    let mut lines = vec![
        windows::Win32::Graphics::DirectWrite::DWRITE_LINE_METRICS::default();
        text_metrics.lineCount as usize
    ];
    let mut actual_line_count = 0;
    // SAFETY: The buffer is sized from DirectWrite's reported line count and
    // remains writable through this synchronous call.
    unsafe { layout.GetLineMetrics(Some(&mut lines), &mut actual_line_count)? };

    let mut height = 0.0;
    #[cfg(test)]
    let mut first_baseline = 0.0;
    #[cfg(test)]
    let mut last_baseline = 0.0;
    #[cfg(test)]
    let mut first_line = true;
    for line in lines
        .iter()
        .take(actual_line_count as usize)
        .take(max_lines.ceil() as usize)
    {
        #[cfg(test)]
        {
            if first_line {
                first_baseline = line.baseline;
                first_line = false;
            }
            last_baseline = height + line.baseline;
        }
        height += line.height;
    }
    #[cfg(test)]
    // SAFETY: `layout` is a live DirectWrite layout owned by this function.
    let overhang = unsafe { layout.GetOverhangMetrics() }.unwrap_or_default();
    Ok(MeasuredTextLayout {
        height: height.max(min_height),
        #[cfg(test)]
        first_baseline,
        #[cfg(test)]
        last_baseline,
        #[cfg(test)]
        overhang_left: overhang.left,
        #[cfg(test)]
        overhang_top: overhang.top,
        #[cfg(test)]
        overhang_right: overhang.right,
        #[cfg(test)]
        overhang_bottom: overhang.bottom,
    })
}
