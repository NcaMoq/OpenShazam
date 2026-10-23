//! History scroll bounds, wheel input, and time-based animation state.

use super::{
    sample_cubic_timing_curve, AppState, CubicBezier, HISTORY_ROW_PITCH, HISTORY_ROW_SPACING,
    HISTORY_SCROLL_LINE_DIP,
};
use std::time::{Duration, Instant};

pub(super) const HISTORY_SCROLL_TRANSITION: Duration = Duration::from_millis(180);

#[derive(Clone, Copy, Debug)]
pub(super) struct HistoryScrollAnimation {
    pub(super) started_at: Instant,
    pub(super) from_offset: f32,
    pub(super) to_offset: f32,
}

pub(super) fn history_content_height(item_count: usize) -> f32 {
    if item_count == 0 {
        0.0
    } else {
        item_count as f32 * HISTORY_ROW_PITCH - HISTORY_ROW_SPACING
    }
}

pub(super) fn history_max_scroll_offset(item_count: usize, viewport_height: f32) -> f32 {
    (history_content_height(item_count) - viewport_height.max(0.0)).max(0.0)
}

pub(super) fn history_scroll_offset_after_wheel(
    current_offset: f32,
    delta: f32,
    wheel_lines: u32,
    item_count: usize,
    viewport_height: f32,
) -> f32 {
    let maximum = history_max_scroll_offset(item_count, viewport_height);
    let current = if current_offset.is_finite() {
        current_offset
    } else {
        0.0
    };
    if !delta.is_finite() || wheel_lines == 0 {
        return current.clamp(0.0, maximum);
    }

    let distance = if wheel_lines == u32::MAX {
        delta / 120.0 * viewport_height
    } else {
        delta / 120.0 * wheel_lines as f32 * HISTORY_SCROLL_LINE_DIP
    };
    (current - distance).clamp(0.0, maximum)
}

pub(super) fn history_scroll_offset_at(
    animation: HistoryScrollAnimation,
    now: Instant,
) -> (f32, bool) {
    let progress = (now
        .saturating_duration_since(animation.started_at)
        .as_secs_f32()
        / HISTORY_SCROLL_TRANSITION.as_secs_f32())
    .clamp(0.0, 1.0);
    let eased = sample_cubic_timing_curve(progress as f64, history_scroll_easing()) as f32;
    (
        animation.from_offset + (animation.to_offset - animation.from_offset) * eased,
        progress < 1.0,
    )
}

pub(super) fn history_scroll_easing() -> CubicBezier {
    // x(t)=t and y(t)=1-(1-t)^3, so Composition and the deterministic
    // hit-test clock use the same ease-out cubic curve.
    CubicBezier {
        x1: 1.0 / 3.0,
        y1: 1.0,
        x2: 2.0 / 3.0,
        y2: 1.0,
    }
}

pub(super) fn advance_history_scroll(app: &mut AppState, now: Instant) -> bool {
    let Some(animation) = app.history_scroll_animation else {
        return false;
    };
    let (offset, still_animating) = history_scroll_offset_at(animation, now);
    let offset = if still_animating {
        offset
    } else {
        animation.to_offset
    };
    let changed = (offset - app.scroll_offset).abs() > f32::EPSILON;
    app.scroll_offset = offset;
    if !still_animating {
        app.history_scroll_animation = None;
    }
    changed
}

pub(super) fn sample_history_scroll(app: &mut AppState, now: Instant) {
    if let Some(animation) = app.history_scroll_animation {
        app.scroll_offset = history_scroll_offset_at(animation, now).0;
    }
}

pub(super) fn history_offset_after_insert(
    current_offset: f32,
    new_item_count: usize,
    viewport_height: f32,
) -> (f32, bool) {
    if current_offset > 0.0 {
        (
            (current_offset + HISTORY_ROW_PITCH)
                .min(history_max_scroll_offset(new_item_count, viewport_height)),
            false,
        )
    } else {
        (0.0, true)
    }
}

#[cfg(test)]
mod tests {
    use super::{history_scroll_offset_at, HistoryScrollAnimation};
    use std::time::{Duration, Instant};

    #[test]
    fn scroll_animation_starts_at_its_origin_and_finishes_at_its_target() {
        let started_at = Instant::now();
        let animation = HistoryScrollAnimation {
            started_at,
            from_offset: 120.0,
            to_offset: 300.0,
        };

        assert_eq!(
            history_scroll_offset_at(animation, started_at),
            (120.0, true)
        );

        let (middle, still_animating) =
            history_scroll_offset_at(animation, started_at + Duration::from_millis(90));
        assert!((middle - 277.5).abs() < 0.001);
        assert!(still_animating);

        assert_eq!(
            history_scroll_offset_at(animation, started_at + Duration::from_millis(180)),
            (300.0, false)
        );
    }
}
