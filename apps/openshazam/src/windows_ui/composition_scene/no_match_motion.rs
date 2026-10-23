use super::{
    ui_model, Compositor, CubicBezierEasingFunction, ScalarKeyFrameAnimation,
    Vector3KeyFrameAnimation,
};
use windows::{core::Error, Foundation::TimeSpan};
use windows_numerics::{Vector2, Vector3};

pub(super) fn create_no_match_scale_animation(
    compositor: &Compositor,
    from: f32,
    to: f32,
    duration_seconds: f64,
    easing: &CubicBezierEasingFunction,
) -> windows::core::Result<Vector3KeyFrameAnimation> {
    let animation = compositor.CreateVector3KeyFrameAnimation()?;
    animation.SetDuration(TimeSpan {
        Duration: (duration_seconds * 10_000_000.0).round() as i64,
    })?;
    animation.InsertKeyFrame(
        0.0,
        Vector3 {
            X: from,
            Y: from,
            Z: 1.0,
        },
    )?;
    animation.InsertKeyFrameWithEasingFunction(
        1.0,
        Vector3 {
            X: to,
            Y: to,
            Z: 1.0,
        },
        easing,
    )?;
    Ok(animation)
}

pub(super) fn create_no_match_opacity_sequence(
    compositor: &Compositor,
    elapsed_seconds: f64,
    first: ((f64, f64), f64, f64),
    second: Option<((f64, f64), f64, f64)>,
    before_first: f64,
) -> windows::core::Result<(f32, Option<ScalarKeyFrameAnimation>)> {
    let elapsed = if elapsed_seconds.is_finite() {
        elapsed_seconds.max(0.0)
    } else {
        0.0
    };
    let sample = |at: f64| -> f32 {
        let (first_timing, first_from, first_to) = first;
        let first_end = first_timing.0 + first_timing.1;
        if at < first_timing.0 {
            return before_first as f32;
        }
        if at < first_end {
            return (first_from + (first_to - first_from) * ((at - first_timing.0) / first_timing.1))
                as f32;
        }
        if let Some((second_timing, second_from, second_to)) = second {
            if at < second_timing.0 {
                return first_to as f32;
            }
            if at < second_timing.0 + second_timing.1 {
                return (second_from
                    + (second_to - second_from) * ((at - second_timing.0) / second_timing.1))
                    as f32;
            }
            return second_to as f32;
        }
        first_to as f32
    };
    let end_time = second
        .map(|(timing, _, _)| timing.0 + timing.1)
        .unwrap_or(first.0 .0 + first.0 .1)
        .max(first.0 .0 + first.0 .1);
    let value = sample(elapsed);
    if elapsed >= end_time {
        return Ok((value, None));
    }

    let animation = compositor.CreateScalarKeyFrameAnimation()?;
    animation.SetDuration(TimeSpan {
        Duration: ((end_time - elapsed) * 10_000_000.0).round() as i64,
    })?;
    animation.InsertKeyFrame(0.0, value)?;
    let mut events = vec![first.0 .0, first.0 .0 + first.0 .1];
    if let Some((timing, _, _)) = second {
        events.push(timing.0);
        events.push(timing.0 + timing.1);
    }
    events.sort_by(f64::total_cmp);
    events.dedup_by(|left, right| (*left - *right).abs() <= 1e-9);
    for at in events
        .into_iter()
        .filter(|at| *at > elapsed && *at <= end_time)
    {
        let progress = ((at - elapsed) / (end_time - elapsed)) as f32;
        animation.InsertKeyFrame(progress, sample(at))?;
    }
    Ok((value, Some(animation)))
}

#[derive(Clone, Copy)]
pub(super) enum NoMatchScaleUp {
    EaseOut { from: f32, to: f32 },
    Spring(ui_model::SpringProfile),
}

#[allow(clippy::too_many_arguments)]
pub(super) fn create_no_match_scale_timeline(
    compositor: &Compositor,
    elapsed_seconds: f64,
    down: (f64, f64),
    down_from: f32,
    down_to: f32,
    up_start: f64,
    up_duration: f64,
    up: NoMatchScaleUp,
) -> windows::core::Result<(f32, Option<Vector3KeyFrameAnimation>)> {
    let elapsed = if elapsed_seconds.is_finite() {
        elapsed_seconds.max(0.0)
    } else {
        0.0
    };
    let down_end = down.0 + down.1;
    let up_end = up_start + up_duration;
    let ease_down =
        |progress: f64| down_from + (down_to - down_from) * ui_model::ease_out(progress) as f32;
    let ease_up = |progress: f64, from: f32, to: f32| {
        from + (to - from) * ui_model::ease_out(progress) as f32
    };
    let spring_at = |progress: f64, profile: ui_model::SpringProfile| {
        ui_model::spring_value(progress, profile) as f32
    };
    let final_value = match up {
        NoMatchScaleUp::EaseOut { to, .. } => to,
        NoMatchScaleUp::Spring(profile) => spring_at(1.0, profile),
    };
    let value = if elapsed < down.0 {
        down_from
    } else if elapsed < down_end {
        ease_down((elapsed - down.0) / down.1)
    } else if elapsed < up_start {
        down_to
    } else if elapsed < up_end {
        let progress = (elapsed - up_start) / up_duration;
        match up {
            NoMatchScaleUp::EaseOut { from, to } => ease_up(progress, from, to),
            NoMatchScaleUp::Spring(profile) => spring_at(progress, profile),
        }
    } else {
        final_value
    };
    if elapsed >= up_end {
        return Ok((final_value, None));
    }

    let animation = compositor.CreateVector3KeyFrameAnimation()?;
    let timeline_end = up_end.max(elapsed + f64::EPSILON);
    let timeline_duration = timeline_end - elapsed;
    animation.SetDuration(TimeSpan {
        Duration: (timeline_duration * 10_000_000.0).round().max(1.0) as i64,
    })?;
    animation.InsertKeyFrame(
        0.0,
        Vector3 {
            X: value,
            Y: value,
            Z: 1.0,
        },
    )?;

    if elapsed < down_end {
        let progress = if elapsed < down.0 {
            0.0
        } else {
            (elapsed - down.0) / down.1
        };
        if elapsed < down.0 {
            animation.InsertKeyFrame(
                ((down.0 - elapsed) / timeline_duration) as f32,
                Vector3 {
                    X: down_from,
                    Y: down_from,
                    Z: 1.0,
                },
            )?;
        }
        let curve = ui_model::ease_out_tail(progress).ok_or_else(|| {
            Error::new(
                windows::core::HRESULT(0x80070057_u32 as i32),
                "NoMatch EaseOut tail could not be evaluated",
            )
        })?;
        let easing = compositor.CreateCubicBezierEasingFunction(
            Vector2 {
                X: curve.x1 as f32,
                Y: curve.y1 as f32,
            },
            Vector2 {
                X: curve.x2 as f32,
                Y: curve.y2 as f32,
            },
        )?;
        animation.InsertKeyFrameWithEasingFunction(
            ((down_end - elapsed) / timeline_duration) as f32,
            Vector3 {
                X: down_to,
                Y: down_to,
                Z: 1.0,
            },
            &easing,
        )?;
    }

    if elapsed < up_start {
        animation.InsertKeyFrame(
            ((up_start - elapsed) / timeline_duration) as f32,
            Vector3 {
                X: down_to,
                Y: down_to,
                Z: 1.0,
            },
        )?;
    }

    match up {
        NoMatchScaleUp::EaseOut { from, to } => {
            let progress = if elapsed < up_start {
                0.0
            } else {
                (elapsed - up_start) / up_duration
            };
            let curve = ui_model::ease_out_tail(progress).ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT(0x80070057_u32 as i32),
                    "NoMatch scale-up EaseOut tail could not be evaluated",
                )
            })?;
            let easing = compositor.CreateCubicBezierEasingFunction(
                Vector2 {
                    X: curve.x1 as f32,
                    Y: curve.y1 as f32,
                },
                Vector2 {
                    X: curve.x2 as f32,
                    Y: curve.y2 as f32,
                },
            )?;
            animation.InsertKeyFrameWithEasingFunction(
                ((up_end - elapsed) / timeline_duration) as f32,
                Vector3 {
                    X: to,
                    Y: to,
                    Z: 1.0,
                },
                &easing,
            )?;
            let _ = from;
        }
        NoMatchScaleUp::Spring(profile) => {
            let current_progress = if elapsed < up_start {
                0.0
            } else {
                ((elapsed - up_start) / up_duration).clamp(0.0, 1.0)
            };
            for index in 1..=256 {
                let local = index as f64 / 256.0;
                let progress = current_progress + (1.0 - current_progress) * local;
                let at = up_start + progress * up_duration;
                if at <= elapsed + 1e-9 {
                    continue;
                }
                let scale = spring_at(progress, profile);
                animation.InsertKeyFrame(
                    ((at - elapsed) / timeline_duration) as f32,
                    Vector3 {
                        X: scale,
                        Y: scale,
                        Z: 1.0,
                    },
                )?;
            }
        }
    }
    Ok((value, Some(animation)))
}

pub(super) fn create_no_match_position_animation(
    compositor: &Compositor,
    center_x: f32,
    center_y: f32,
    profile: ui_model::SpringProfile,
    start_seconds: f64,
    elapsed_seconds: f64,
) -> windows::core::Result<(f32, Option<Vector3KeyFrameAnimation>)> {
    let elapsed = if elapsed_seconds.is_finite() {
        elapsed_seconds.max(0.0)
    } else {
        0.0
    };
    let progress = if elapsed < start_seconds {
        0.0
    } else {
        ((elapsed - start_seconds) / profile.duration_seconds).clamp(0.0, 1.0)
    };
    let offset = ui_model::spring_value(progress, profile) as f32;
    if elapsed >= start_seconds + profile.duration_seconds {
        return Ok((0.0, None));
    }
    let animation = compositor.CreateVector3KeyFrameAnimation()?;
    let remaining = profile.duration_seconds * (1.0 - progress);
    animation.SetDuration(TimeSpan {
        Duration: (remaining * 10_000_000.0).round().max(1.0) as i64,
    })?;
    if elapsed < start_seconds {
        animation.SetDelayTime(TimeSpan {
            Duration: ((start_seconds - elapsed) * 10_000_000.0).round() as i64,
        })?;
    }
    for index in 0..=256 {
        let local = index as f64 / 256.0;
        let sample_progress = progress + (1.0 - progress) * local;
        let sample_offset = ui_model::spring_value(sample_progress, profile) as f32;
        animation.InsertKeyFrame(
            local as f32,
            Vector3 {
                X: center_x + sample_offset,
                Y: center_y,
                Z: 0.0,
            },
        )?;
    }
    Ok((offset, Some(animation)))
}
