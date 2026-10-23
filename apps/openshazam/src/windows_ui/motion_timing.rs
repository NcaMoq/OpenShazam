use super::{
    CubicBezier, HistoryMutationEvent, HistoryMutationTransition, MainPhase, ResultCardStage,
    ResultOverlayMotionAnimation, ResultOverlayMotionKey, ANIMATION_TICK_MS,
    HISTORY_INSERT_TRANSITION, HISTORY_ROW_PITCH, RESULT_CARD_TRANSITION, TOP_HEIGHT,
};
use std::time::{Duration, Instant};
pub(super) fn result_card_easing() -> CubicBezier {
    // UI-236 candidate A: Apple's standard Core Animation timing function.
    // It is the first Windows comparison candidate, not a measured native
    // Shazam result; the macOS presentation curve remains unresolved.
    CubicBezier {
        x1: 0.25,
        y1: 0.1,
        x2: 0.25,
        y2: 1.0,
    }
}

pub(super) fn cubic_coordinate(parameter: f64, first: f64, second: f64) -> f64 {
    let inverse = 1.0 - parameter;
    3.0 * inverse * inverse * parameter * first
        + 3.0 * inverse * parameter * parameter * second
        + parameter * parameter * parameter
}

pub(super) fn cubic_derivative(parameter: f64, first: f64, second: f64) -> f64 {
    let inverse = 1.0 - parameter;
    3.0 * inverse * inverse * first
        + 6.0 * inverse * parameter * (second - first)
        + 3.0 * parameter * parameter * (1.0 - second)
}

pub(super) fn cubic_parameter_for_x(progress: f64, curve: CubicBezier) -> f64 {
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..48 {
        let mid = (low + high) * 0.5;
        if cubic_coordinate(mid, curve.x1, curve.x2) < progress {
            low = mid;
        } else {
            high = mid;
        }
    }
    (low + high) * 0.5
}

pub(super) fn sample_cubic_timing_curve(progress: f64, curve: CubicBezier) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    if progress <= 0.0 || progress >= 1.0 {
        return progress;
    }
    let parameter = cubic_parameter_for_x(progress, curve);
    cubic_coordinate(parameter, curve.y1, curve.y2)
}

pub(super) fn result_card_progress(started_at: Instant, now: Instant) -> f32 {
    let linear = (now.saturating_duration_since(started_at).as_secs_f64()
        / RESULT_CARD_TRANSITION.as_secs_f64())
    .clamp(0.0, 1.0);
    sample_cubic_timing_curve(linear, result_card_easing()) as f32
}

pub(super) fn remaining_cubic_easing(progress: f64, curve: CubicBezier) -> Option<CubicBezier> {
    if progress >= 1.0 {
        return None;
    }
    if progress <= 0.0 {
        return Some(curve);
    }
    let parameter = cubic_parameter_for_x(progress, curve);
    let remaining_parameter = 1.0 - parameter;
    let start_x = cubic_coordinate(parameter, curve.x1, curve.x2);
    let start_y = cubic_coordinate(parameter, curve.y1, curve.y2);
    let start_dx = cubic_derivative(parameter, curve.x1, curve.x2);
    let start_dy = cubic_derivative(parameter, curve.y1, curve.y2);
    let first_x = start_x + remaining_parameter * start_dx / 3.0;
    let first_y = start_y + remaining_parameter * start_dy / 3.0;
    let second_x = 1.0 - remaining_parameter * (1.0 - curve.x2);
    let second_y = 1.0 - remaining_parameter * (1.0 - curve.y2);
    let x_scale = 1.0 - start_x;
    let y_scale = 1.0 - start_y;
    if x_scale <= f64::EPSILON || y_scale <= f64::EPSILON {
        return None;
    }
    Some(CubicBezier {
        x1: ((first_x - start_x) / x_scale).clamp(0.0, 1.0),
        y1: ((first_y - start_y) / y_scale).clamp(0.0, 1.0),
        x2: ((second_x - start_x) / x_scale).clamp(0.0, 1.0),
        y2: ((second_y - start_y) / y_scale).clamp(0.0, 1.0),
    })
}

pub(super) fn result_overlay_motion_animation(
    phase: &MainPhase,
    now: Instant,
) -> Option<ResultOverlayMotionAnimation> {
    let curve = result_card_easing();
    let (key, from, to, started_at) = match phase {
        MainPhase::Matched { track, stage } => match stage {
            ResultCardStage::Entering { started_at } => (
                ResultOverlayMotionKey::MatchEnter {
                    display_id: track.display_id,
                    started_at: *started_at,
                },
                -(TOP_HEIGHT as f32),
                0.0,
                *started_at,
            ),
            ResultCardStage::Exiting {
                started_at,
                from_offset,
            } => (
                ResultOverlayMotionKey::MatchExit {
                    display_id: track.display_id,
                    started_at: *started_at,
                    from_offset_bits: from_offset.to_bits(),
                },
                *from_offset,
                TOP_HEIGHT as f32,
                *started_at,
            ),
            ResultCardStage::Holding { .. } => return None,
        },
        MainPhase::NoMatch {
            entered_at,
            returned_to_ready_at,
        } => match returned_to_ready_at {
            Some(returned_at) => {
                let entrance = result_card_progress(*entered_at, *returned_at);
                let from_offset = -(1.0 - entrance) * TOP_HEIGHT as f32;
                (
                    ResultOverlayMotionKey::NoMatchExit {
                        entered_at: *entered_at,
                        returned_at: *returned_at,
                        offline: false,
                        from_offset_bits: from_offset.to_bits(),
                    },
                    from_offset,
                    TOP_HEIGHT as f32,
                    *returned_at,
                )
            }
            None => (
                ResultOverlayMotionKey::NoMatchEnter {
                    entered_at: *entered_at,
                    offline: false,
                },
                -(TOP_HEIGHT as f32),
                0.0,
                *entered_at,
            ),
        },
        MainPhase::Offline {
            entered_at,
            returned_to_ready_at,
        } => match returned_to_ready_at {
            Some(returned_at) => {
                let entrance = result_card_progress(*entered_at, *returned_at);
                let from_offset = -(1.0 - entrance) * TOP_HEIGHT as f32;
                (
                    ResultOverlayMotionKey::NoMatchExit {
                        entered_at: *entered_at,
                        returned_at: *returned_at,
                        offline: true,
                        from_offset_bits: from_offset.to_bits(),
                    },
                    from_offset,
                    TOP_HEIGHT as f32,
                    *returned_at,
                )
            }
            None => (
                ResultOverlayMotionKey::NoMatchEnter {
                    entered_at: *entered_at,
                    offline: true,
                },
                -(TOP_HEIGHT as f32),
                0.0,
                *entered_at,
            ),
        },
        _ => return None,
    };

    let elapsed = now.saturating_duration_since(started_at);
    if elapsed >= RESULT_CARD_TRANSITION {
        return None;
    }
    let linear = (elapsed.as_secs_f64() / RESULT_CARD_TRANSITION.as_secs_f64()).clamp(0.0, 1.0);
    let progress = sample_cubic_timing_curve(linear, curve) as f32;
    Some(ResultOverlayMotionAnimation {
        key,
        from_offset: from + (to - from) * progress,
        to_offset: to,
        remaining: RESULT_CARD_TRANSITION.saturating_sub(elapsed),
        easing: remaining_cubic_easing(linear, curve).unwrap_or(curve),
    })
}

pub(super) fn linear_transition_progress(
    started_at: Instant,
    now: Instant,
    duration: Duration,
) -> f32 {
    // UI-226 confirms the spatial endpoints and the 0.3s duration; the native
    // time-easing curve remains unresolved. Keep the linear time evaluator
    // explicitly replaceable rather than borrowing an unrelated measured curve.
    (now.saturating_duration_since(started_at).as_secs_f32() / duration.as_secs_f32())
        .clamp(0.0, 1.0)
}

pub(super) fn history_first_insert_progress(
    transition: Option<HistoryMutationTransition>,
    now: Instant,
) -> Option<f32> {
    let transition = transition?;
    matches!(
        transition.event,
        HistoryMutationEvent::Insert {
            index: 0,
            previous_count: 0,
        }
    )
    .then(|| linear_transition_progress(transition.started_at, now, HISTORY_INSERT_TRANSITION))
}

pub(super) fn history_transition_requires_d2d_frame(
    transition: Option<HistoryMutationTransition>,
    now: Instant,
    composition_ready: bool,
) -> bool {
    transition.is_some_and(|transition| {
        let first_insert = history_first_insert_progress(Some(transition), now).is_some();
        let frame_deadline = HISTORY_INSERT_TRANSITION
            + if first_insert {
                Duration::from_millis(u64::from(ANIMATION_TICK_MS))
            } else {
                Duration::ZERO
            };
        now.saturating_duration_since(transition.started_at) < frame_deadline
            && (!composition_ready || first_insert)
    })
}

pub(super) fn history_composition_offset(started_at: Instant, now: Instant) -> f32 {
    let progress = linear_transition_progress(started_at, now, HISTORY_INSERT_TRANSITION);
    -HISTORY_ROW_PITCH * (1.0 - progress)
}
