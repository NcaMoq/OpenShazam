//! Deterministic, platform-independent UI geometry and animation evaluation.
//!
//! Values marked `DIRECT` below are directly confirmed by the supplied
//! Shazam 2.11.0 UI specification. Any OS-dependent Core Animation behavior
//! remains an input or an explicitly named implementation estimate.

pub const CONTENT_WIDTH: f64 = 380.0;
pub const CONTENT_HEIGHT: f64 = 610.0;
pub const TOP_HEIGHT: f64 = 180.0;
pub const HISTORY_HEIGHT: f64 = 430.0;
pub const POPUP_ARROW_HEIGHT: f64 = 10.0;
pub const POPUP_CORNER_RADIUS: f64 = 10.0;
pub const BUTTON_SIZE: f64 = 96.0;
pub const BUTTON_X: f64 = 142.0;
pub const BUTTON_TOP: f64 = 30.0;

pub const INNER_CORE_DURATION: f64 = 2.924_999_827_839_61;
pub const INNER_NULL_DURATION: f64 = 2.329_166_529_575_985_6;
pub const INNER_OUTER_LOOP: f64 = 1.909_916_537_592_810_9;
pub const INNER_COMMON_OFFSET: f64 = 0.209_624_995_991_587_32;
pub const INNER_PHASE_OFFSETS: [f64; 4] = [
    0.649_999_966_585_012_6,
    0.514_583_301_431_564_9,
    0.352_083_320_681_785_3,
    0.216_666_655_528_337_55,
];
pub const INNER_DIAMETERS: [f64; 4] = [288.0, 222.545, 170.182, 130.909];
pub const INNER_BASE_OPACITY: [f64; 4] = [0.4, 0.5, 0.7, 0.75];

pub const OUTER_CORE_DURATION: f64 = 1.500_000_037_645_038_9;
pub const OUTER_LOOP_DURATION: f64 = 1.583_333_373;
pub const OUTER_SCALE_NULL_DURATION: f64 = 1.583_333_373_069_763_2;
pub const OUTER_SECOND_PHASE: f64 = 0.208_333_35;
pub const OUTER_DIAMETERS: [f64; 2] = [604.8, 514.08];

pub const STARTUP_LOGO_TRANSPARENT_DURATION: f64 = 0.300_000_034;
pub const STARTUP_LOGO_SOLID_DURATION: f64 = 0.540_000_018;
pub const STARTUP_LOGO_SOLID_DELAY: f64 = STARTUP_LOGO_TRANSPARENT_DURATION;
/// The CAAnimationGroup runs for the delayed second track's full group time.
pub const STARTUP_LOGO_TOTAL_DURATION: f64 = STARTUP_LOGO_SOLID_DELAY + STARTUP_LOGO_SOLID_DURATION;
/// UI-169's strokeStart endpoint for each of the two LongPath layers.
pub const STARTUP_LOGO_SOLID_TRIM_START_END: f64 = 0.453_125;

const STARTUP_LOGO_TRANSPARENT_BEZIER: CubicBezier = CubicBezier {
    x1: 1.0,
    y1: 0.0,
    x2: 1.0,
    y2: 1.0,
};
const STARTUP_LOGO_SOLID_BEZIER: CubicBezier = CubicBezier {
    x1: 0.4,
    y1: 1.0,
    x2: 0.41,
    y2: 1.0,
};

const BEZIER_X1: f64 = 0.44;
const BEZIER_Y1: f64 = 0.05;
const BEZIER_X2: f64 = 0.55;
const BEZIER_Y2: f64 = 0.95;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaggingMode {
    Ready,
    Manual,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiLevels {
    pub bass: f64,
    pub treble: f64,
}

impl Default for UiLevels {
    fn default() -> Self {
        Self {
            // Keep the unconnected UI at unit scale until a measured meter
            // adapter supplies values. Bass=5/7 is the neutral point of the
            // documented 0.75..1.10 button range; treble=0.5 is unit scale.
            bass: 5.0 / 7.0,
            treble: 0.5,
        }
    }
}

impl UiLevels {
    /// UI-only value adapter from [0, 1] to the directly observed layer
    /// transform ranges. The audio-to-bass/treble source is unresolved in the
    /// specification, so callers must inject values explicitly.
    pub fn normalized(self) -> Self {
        Self {
            bass: finite_unit(self.bass),
            treble: finite_unit(self.treble),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CircleFrame {
    pub diameter: f64,
    /// Static base geometry diameter before container, animation and Bass transforms.
    pub shape_diameter: f64,
    pub core_scale: f64,
    pub sublayer_scale: f64,
    pub scale_null: f64,
    pub core_opacity: f64,
    pub base_opacity: f64,
    pub audio_scale: f64,
    pub opacity: f64,
    pub stroke_width: f64,
    /// Local child animation time is retained for deterministic inspection.
    pub local_time: f64,
}

/// Cubic timing-function controls for a Composition keyframe segment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CubicBezier {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupLogoStage {
    TransparentStroke,
    SolidLongStroke,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StartupLogoFrame {
    pub stage: StartupLogoStage,
    pub transparent_trim_start: f64,
    pub solid_long_trim_start: f64,
    pub solid_long_trim_end: f64,
}

/// One keyframe on a shifted Core Animation child track. `easing` applies
/// between this keyframe and the preceding one; `None` means linear.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedTrackKeyframe {
    pub progress: f64,
    pub value: f64,
    pub easing: Option<CubicBezier>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InnerTrack {
    CoreScale,
    CoreOpacity,
    SublayerScale,
    ScaleNull,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OuterTrack {
    CoreSize,
    BorderWidth,
}

/// Outer-circle values retain the three independently animated native scale
/// tracks. `diameter` is their composed logical result for the current frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OuterCircleFrame {
    pub diameter: f64,
    /// Circle geometry size; container, ScaleNull and audio scales stay apart.
    pub shape_diameter: f64,
    pub opacity: f64,
    pub stroke_width: f64,
    pub core_size_scale: f64,
    pub container_scale: f64,
    pub scale_null: f64,
    pub audio_scale: f64,
    pub local_time: f64,
    pub scale_null_local_time: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveFrame {
    pub button_scale: f64,
    pub button_child_scale: f64,
    pub inner_audio_scale: f64,
    pub inner_audio_opacity: f64,
    pub outer_audio_scale: f64,
    pub outer_audio_opacity: f64,
    pub inner: [CircleFrame; 4],
    pub outer: [OuterCircleFrame; 2],
    pub auto_shadow_scale: f64,
    pub inner_container_factor: f64,
    pub outer_container_factor: f64,
}

/// Runtime parameters that can replace the currently unmeasured Auto Shazam
/// button spring. `None` leaves the transform at its neutral scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringProfile {
    pub mass: f64,
    pub damping: f64,
    pub stiffness: f64,
    pub initial_velocity: f64,
    pub duration_seconds: f64,
    pub from: f64,
    pub to: f64,
}

/// Cubic Bezier timing function (0.44, 0.05, 0.55, 0.95), solving x(t)=time.
/// This is the curve used by both native methods named in the source spec.
pub fn native_bezier(progress: f64) -> f64 {
    let x = finite_unit(progress);
    if x == 0.0 || x == 1.0 {
        return x;
    }

    let mut low = 0.0;
    let mut high = 1.0;
    let mut t = x;
    for _ in 0..8 {
        let estimated = cubic_component(t, BEZIER_X1, BEZIER_X2);
        let slope = cubic_component_derivative(t, BEZIER_X1, BEZIER_X2);
        if slope.abs() > 1e-8 {
            let candidate = t - (estimated - x) / slope;
            if (low..=high).contains(&candidate) {
                t = candidate;
            } else {
                t = (low + high) * 0.5;
            }
        } else {
            t = (low + high) * 0.5;
        }
        let current_x = cubic_component(t, BEZIER_X1, BEZIER_X2);
        if current_x < x {
            low = t;
        } else {
            high = t;
        }
    }
    cubic_component(t, BEZIER_Y1, BEZIER_Y2)
}

/// Deterministic presentation values for the UI-169 one-shot logo drawing.
/// The transparent stroke and the delayed two-track LongPath group use
/// separate clocks; both LongPath tracks share one group-local clock.
pub fn startup_logo_frame_at(elapsed: f64) -> StartupLogoFrame {
    let elapsed = if elapsed.is_finite() {
        elapsed.max(0.0)
    } else {
        0.0
    };
    if elapsed < STARTUP_LOGO_TRANSPARENT_DURATION {
        StartupLogoFrame {
            stage: StartupLogoStage::TransparentStroke,
            transparent_trim_start: bezier_timing(
                elapsed / STARTUP_LOGO_TRANSPARENT_DURATION,
                STARTUP_LOGO_TRANSPARENT_BEZIER,
            ),
            solid_long_trim_start: 0.0,
            solid_long_trim_end: 0.0,
        }
    } else if elapsed < STARTUP_LOGO_TOTAL_DURATION {
        let solid_elapsed = (elapsed - STARTUP_LOGO_SOLID_DELAY).max(0.0);
        StartupLogoFrame {
            stage: StartupLogoStage::SolidLongStroke,
            transparent_trim_start: 1.0,
            solid_long_trim_start: STARTUP_LOGO_SOLID_TRIM_START_END
                * bezier_timing(
                    solid_elapsed / STARTUP_LOGO_SOLID_DURATION,
                    STARTUP_LOGO_SOLID_BEZIER,
                ),
            solid_long_trim_end: bezier_timing(
                solid_elapsed / STARTUP_LOGO_SOLID_DURATION,
                STARTUP_LOGO_SOLID_BEZIER,
            ),
        }
    } else {
        StartupLogoFrame {
            stage: StartupLogoStage::Complete,
            transparent_trim_start: 1.0,
            solid_long_trim_start: STARTUP_LOGO_SOLID_TRIM_START_END,
            solid_long_trim_end: 1.0,
        }
    }
}

fn bezier_timing(progress: f64, curve: CubicBezier) -> f64 {
    let x = finite_unit(progress);
    if x == 0.0 || x == 1.0 {
        return x;
    }
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..48 {
        let t = (low + high) * 0.5;
        if cubic_component(t, curve.x1, curve.x2) < x {
            low = t;
        } else {
            high = t;
        }
    }
    cubic_component((low + high) * 0.5, curve.y1, curve.y2)
}

fn cubic_component(t: f64, p1: f64, p2: f64) -> f64 {
    let one_minus_t = 1.0 - t;
    3.0 * one_minus_t * one_minus_t * t * p1 + 3.0 * one_minus_t * t * t * p2 + t * t * t
}

fn cubic_component_derivative(t: f64, p1: f64, p2: f64) -> f64 {
    let one_minus_t = 1.0 - t;
    3.0 * one_minus_t * one_minus_t * p1
        + 6.0 * one_minus_t * t * (p2 - p1)
        + 3.0 * t * t * (1.0 - p2)
}

fn linear(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * finite_unit(t)
}

fn sample_track(values: &[f64], key_times: &[f64], progress: f64, bezier_segments: &[bool]) -> f64 {
    debug_assert_eq!(values.len(), key_times.len());
    debug_assert_eq!(bezier_segments.len() + 1, values.len());
    let t = finite_unit(progress);
    if t <= key_times[0] {
        return values[0];
    }
    for segment in 0..key_times.len() - 1 {
        let start = key_times[segment];
        let end = key_times[segment + 1];
        if t <= end || segment == key_times.len() - 2 {
            let local = if end <= start {
                1.0
            } else {
                (t - start) / (end - start)
            };
            let eased = if bezier_segments[segment] {
                native_bezier(local)
            } else {
                finite_unit(local)
            };
            return linear(values[segment], values[segment + 1], eased);
        }
    }
    *values.last().unwrap_or(&0.0)
}

/// Translate a child track into the current evaluator's parent-loop time
/// coordinate. Native Core Animation's presentation at the child/parent
/// duration boundary remains unverified. Clipped Bezier segments are split so
/// the remaining curve keeps the same timing-function shape.
fn shifted_track_keyframes(
    values: &[f64],
    key_times: &[f64],
    bezier_segments: &[bool],
    duration: f64,
    phase_offset: f64,
    loop_duration: f64,
    value_scale: f64,
) -> Vec<TimedTrackKeyframe> {
    debug_assert_eq!(values.len(), key_times.len());
    debug_assert_eq!(bezier_segments.len() + 1, values.len());
    debug_assert!(duration > 0.0 && loop_duration > 0.0);

    let loop_end = phase_offset + loop_duration;
    let mut keyframes = vec![TimedTrackKeyframe {
        progress: 0.0,
        value: sample_track(values, key_times, phase_offset / duration, bezier_segments)
            * value_scale,
        easing: None,
    }];

    for segment in 0..bezier_segments.len() {
        let source_start = key_times[segment] * duration;
        let source_end = key_times[segment + 1] * duration;
        if source_end <= source_start {
            continue;
        }
        let clipped_start = phase_offset.max(source_start);
        let clipped_end = loop_end.min(source_end);
        if clipped_end <= clipped_start {
            continue;
        }

        let local_start = (clipped_start - source_start) / (source_end - source_start);
        let local_end = (clipped_end - source_start) / (source_end - source_start);
        let progress = ((clipped_end - phase_offset) / loop_duration).clamp(0.0, 1.0);
        if progress <= 1e-12 {
            continue;
        }
        let easing = bezier_segments[segment].then(|| split_native_bezier(local_start, local_end));
        let value =
            sample_track(values, key_times, clipped_end / duration, bezier_segments) * value_scale;
        if (progress - 1.0).abs() <= 1e-12 {
            keyframes.push(TimedTrackKeyframe {
                progress: 1.0,
                value,
                easing,
            });
            return keyframes;
        }
        keyframes.push(TimedTrackKeyframe {
            progress,
            value,
            easing,
        });
    }

    if keyframes
        .last()
        .is_some_and(|keyframe| keyframe.progress < 1.0 - 1e-12)
    {
        keyframes.push(TimedTrackKeyframe {
            progress: 1.0,
            value: sample_track(values, key_times, loop_end / duration, bezier_segments)
                * value_scale,
            easing: None,
        });
    }
    keyframes
}

/// Translate a delayed outer-circle child track into the parent's repeating
/// time coordinate. Before the child starts, the track holds its first value.
fn delayed_track_keyframes(
    values: &[f64],
    key_times: &[f64],
    bezier_segments: &[bool],
    duration: f64,
    delay: f64,
    loop_duration: f64,
) -> Vec<TimedTrackKeyframe> {
    debug_assert_eq!(values.len(), key_times.len());
    debug_assert_eq!(bezier_segments.len() + 1, values.len());
    debug_assert!(duration > 0.0 && loop_duration > 0.0 && delay >= 0.0);

    let mut keyframes = vec![TimedTrackKeyframe {
        progress: 0.0,
        value: values[0],
        easing: None,
    }];
    if delay > 0.0 && delay < loop_duration {
        keyframes.push(TimedTrackKeyframe {
            progress: delay / loop_duration,
            value: values[0],
            easing: None,
        });
    }

    for segment in 0..bezier_segments.len() {
        let source_start = delay + key_times[segment] * duration;
        let source_end = delay + key_times[segment + 1] * duration;
        if source_end <= source_start {
            continue;
        }
        let clipped_start = source_start.max(delay);
        let clipped_end = loop_duration.min(source_end);
        if clipped_end <= clipped_start {
            continue;
        }

        let local_start = (clipped_start - source_start) / (source_end - source_start);
        let local_end = (clipped_end - source_start) / (source_end - source_start);
        let progress = (clipped_end / loop_duration).clamp(0.0, 1.0);
        if progress <= 1e-12 {
            continue;
        }
        let easing = bezier_segments[segment].then(|| split_native_bezier(local_start, local_end));
        let source_progress = ((clipped_end - delay) / duration).max(0.0);
        let value = sample_track(values, key_times, source_progress, bezier_segments);
        if (progress - 1.0).abs() <= 1e-12 {
            keyframes.push(TimedTrackKeyframe {
                progress: 1.0,
                value,
                easing,
            });
            return keyframes;
        }
        keyframes.push(TimedTrackKeyframe {
            progress,
            value,
            easing,
        });
    }

    if keyframes
        .last()
        .is_some_and(|keyframe| keyframe.progress < 1.0 - 1e-12)
    {
        let local_time = (loop_duration - delay).max(0.0);
        keyframes.push(TimedTrackKeyframe {
            progress: 1.0,
            value: sample_track(values, key_times, local_time / duration, bezier_segments),
            easing: None,
        });
    }
    keyframes
}

/// Return an outer geometry track aligned to the two circle core offsets.
pub fn delayed_outer_track(track: OuterTrack, circle_index: usize) -> Vec<TimedTrackKeyframe> {
    assert!(circle_index < OUTER_DIAMETERS.len());
    let delay = if circle_index == 0 {
        0.0
    } else {
        OUTER_SECOND_PHASE
    };
    let (values, key_times, bezier) = match track {
        OuterTrack::CoreSize => (
            &[0.615_384_6, 1.0, 1.0][..],
            &[0.0, 0.75, 1.0][..],
            &[true, false][..],
        ),
        OuterTrack::BorderWidth => (
            &[0.0, 4.0, 4.0, 0.0, 0.0][..],
            &[
                0.0,
                0.138_888_895_511_627_2,
                0.416_666_656_732_559_2,
                0.888_888_895_511_627_2,
                1.0,
            ][..],
            &[true, false, true, false][..],
        ),
    };
    delayed_track_keyframes(
        values,
        key_times,
        bezier,
        OUTER_CORE_DURATION,
        delay,
        OUTER_LOOP_DURATION,
    )
}

/// Return one inner-circle track shifted into the 1.9099-second parent loop
/// represented by the deterministic evaluator. The native runtime's boundary
/// presentation is still an unresolved [U] input.
pub fn shifted_inner_track(track: InnerTrack, circle_index: usize) -> Vec<TimedTrackKeyframe> {
    assert!(circle_index < INNER_DIAMETERS.len());
    let (values, key_times, duration, bezier, offset, value_scale) = match track {
        InnerTrack::CoreScale => (
            &[0.0, 1.0, 1.0][..],
            &[0.0, 0.740_740_716_457_366_9, 1.0][..],
            INNER_CORE_DURATION,
            &[true, false][..],
            INNER_COMMON_OFFSET + INNER_PHASE_OFFSETS[circle_index],
            1.0,
        ),
        InnerTrack::CoreOpacity => (
            &[0.0, 1.0, 0.0][..],
            &[0.0, 0.333_333_3, 1.0][..],
            INNER_CORE_DURATION,
            &[false, false][..],
            INNER_COMMON_OFFSET + INNER_PHASE_OFFSETS[circle_index],
            INNER_BASE_OPACITY[circle_index],
        ),
        InnerTrack::SublayerScale => (
            &[0.0, 1.0, 0.7][..],
            &[0.0, 0.333_333_3, 1.0][..],
            INNER_CORE_DURATION,
            &[false, false][..],
            INNER_COMMON_OFFSET + INNER_PHASE_OFFSETS[circle_index],
            1.0,
        ),
        InnerTrack::ScaleNull => (
            &[1.0, 1.0, 0.9][..],
            &[0.0, 0.244_186_043_739_318_85, 1.0][..],
            INNER_NULL_DURATION,
            &[false, true][..],
            INNER_COMMON_OFFSET,
            1.0,
        ),
    };
    shifted_track_keyframes(
        values,
        key_times,
        bezier,
        duration,
        offset,
        INNER_OUTER_LOOP,
        value_scale,
    )
}

fn native_bezier_point(t: f64) -> (f64, f64) {
    (
        cubic_component(t, BEZIER_X1, BEZIER_X2),
        cubic_component(t, BEZIER_Y1, BEZIER_Y2),
    )
}

fn native_bezier_derivative(t: f64) -> (f64, f64) {
    (
        cubic_component_derivative(t, BEZIER_X1, BEZIER_X2),
        cubic_component_derivative(t, BEZIER_Y1, BEZIER_Y2),
    )
}

fn native_bezier_parameter(x: f64) -> f64 {
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..48 {
        let middle = (low + high) * 0.5;
        if cubic_component(middle, BEZIER_X1, BEZIER_X2) < x {
            low = middle;
        } else {
            high = middle;
        }
    }
    (low + high) * 0.5
}

fn split_native_bezier(start_x: f64, end_x: f64) -> CubicBezier {
    let start_t = native_bezier_parameter(start_x.clamp(0.0, 1.0));
    let end_t = native_bezier_parameter(end_x.clamp(0.0, 1.0));
    let span = (end_t - start_t).max(f64::EPSILON);
    let start = native_bezier_point(start_t);
    let end = native_bezier_point(end_t);
    let start_derivative = native_bezier_derivative(start_t);
    let end_derivative = native_bezier_derivative(end_t);
    let width = (end.0 - start.0).max(f64::EPSILON);
    let height = (end.1 - start.1).max(f64::EPSILON);
    let first = (
        (start.0 + start_derivative.0 * span / 3.0 - start.0) / width,
        (start.1 + start_derivative.1 * span / 3.0 - start.1) / height,
    );
    let second = (
        (end.0 - end_derivative.0 * span / 3.0 - start.0) / width,
        (end.1 - end_derivative.1 * span / 3.0 - start.1) / height,
    );
    CubicBezier {
        x1: first.0.clamp(0.0, 1.0),
        y1: first.1.clamp(0.0, 1.0),
        x2: second.0.clamp(0.0, 1.0),
        y2: second.1.clamp(0.0, 1.0),
    }
}

#[cfg(test)]
fn sample_bezier_curve(progress: f64, curve: CubicBezier) -> f64 {
    let x = finite_unit(progress);
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..48 {
        let middle = (low + high) * 0.5;
        if cubic_component(middle, curve.x1, curve.x2) < x {
            low = middle;
        } else {
            high = middle;
        }
    }
    cubic_component((low + high) * 0.5, curve.y1, curve.y2)
}

#[cfg(test)]
fn evaluate_track_keyframes(keyframes: &[TimedTrackKeyframe], progress: f64) -> f64 {
    let progress = finite_unit(progress);
    for pair in keyframes.windows(2) {
        let start = pair[0];
        let end = pair[1];
        if progress <= end.progress {
            let local = if end.progress <= start.progress {
                1.0
            } else {
                (progress - start.progress) / (end.progress - start.progress)
            };
            let eased = end.easing.map_or_else(
                || finite_unit(local),
                |curve| sample_bezier_curve(local, curve),
            );
            return linear(start.value, end.value, eased);
        }
    }
    keyframes.last().map_or(0.0, |keyframe| keyframe.value)
}

fn inner_core_scale(local_time: f64) -> f64 {
    sample_track(
        &[0.0, 1.0, 1.0],
        &[0.0, 0.740_740_716_457_366_9, 1.0],
        local_time / INNER_CORE_DURATION,
        &[true, false],
    )
}

fn inner_core_opacity(local_time: f64) -> f64 {
    sample_track(
        &[0.0, 1.0, 0.0],
        &[0.0, 0.333_333_3, 1.0],
        local_time / INNER_CORE_DURATION,
        &[false, false],
    )
}

fn inner_core_sublayer_scale(local_time: f64) -> f64 {
    sample_track(
        &[0.0, 1.0, 0.7],
        &[0.0, 0.333_333_3, 1.0],
        local_time / INNER_CORE_DURATION,
        &[false, false],
    )
}

fn inner_scale_null(local_time: f64) -> f64 {
    sample_track(
        &[1.0, 1.0, 0.9],
        &[0.0, 0.244_186_043_739_318_85, 1.0],
        local_time / INNER_NULL_DURATION,
        &[false, true],
    )
}

fn outer_size(local_time: f64) -> f64 {
    sample_track(
        &[0.615_384_6, 1.0, 1.0],
        &[0.0, 0.75, 1.0],
        local_time / OUTER_CORE_DURATION,
        &[true, false],
    )
}

fn outer_container_scale(local_time: f64) -> f64 {
    sample_track(
        &[0.85, 0.9, 0.9],
        &[0.0, 0.75, 1.0],
        local_time / OUTER_CORE_DURATION,
        &[true, false],
    )
}

fn outer_border_width(local_time: f64) -> f64 {
    // Native supplies three timing functions for four segments. The last
    // segment holds zero at both ends, so a linear fallback has no numeric
    // effect; the missing native segment rule remains documented as [U].
    sample_track(
        &[0.0, 4.0, 4.0, 0.0, 0.0],
        &[
            0.0,
            0.138_888_895_511_627_2,
            0.416_666_656_732_559_2,
            0.888_888_895_511_627_2,
            1.0,
        ],
        local_time / OUTER_CORE_DURATION,
        &[true, false, true, false],
    )
}

fn outer_scale_null(local_time: f64) -> f64 {
    sample_track(
        &[1.0, 1.0, 0.930_000_007_152_557_4],
        &[0.0, 0.657_894_730_567_932_1, 1.0],
        local_time / OUTER_SCALE_NULL_DURATION,
        &[false, true],
    )
}

/// Evaluate the manual or Auto Shazam wave scene at an arbitrary monotonic
/// elapsed time. No allocation or frame-history accumulation is used.
#[cfg(test)]
pub fn wave_frame_at(mode: TaggingMode, elapsed_seconds: f64, levels: UiLevels) -> WaveFrame {
    wave_frame_at_with_auto_spring(mode, elapsed_seconds, levels, None)
}

pub fn wave_frame_at_with_auto_spring(
    mode: TaggingMode,
    elapsed_seconds: f64,
    levels: UiLevels,
    auto_spring: Option<SpringProfile>,
) -> WaveFrame {
    let elapsed = if elapsed_seconds.is_finite() {
        elapsed_seconds.max(0.0)
    } else {
        0.0
    };
    let levels = levels.normalized();
    let inner_phase = elapsed.rem_euclid(INNER_OUTER_LOOP);
    let outer_phase = elapsed.rem_euclid(OUTER_LOOP_DURATION);
    let manual = mode == TaggingMode::Manual;
    let bass_scale = if manual {
        linear(0.7, 1.3, levels.bass)
    } else {
        1.0
    };
    let bass_opacity = if manual {
        linear(0.5, 0.9, levels.bass)
    } else {
        1.0
    };
    let button_child_scale = if manual {
        linear(0.75, 1.1, levels.bass)
    } else if mode == TaggingMode::Auto {
        auto_spring.map_or(1.0, |spring| {
            spring_value(
                elapsed / spring.duration_seconds.max(f64::MIN_POSITIVE),
                spring,
            )
        })
    } else {
        1.0
    };
    let treble_scale = if manual {
        linear(0.9, 1.1, levels.treble)
    } else {
        1.0
    };
    let treble_opacity = if manual {
        linear(0.3, 0.7, levels.treble)
    } else {
        1.0
    };

    let idle_pulse = if mode != TaggingMode::Ready {
        1.0
    } else {
        idle_pulse_at(elapsed)
    };

    let inner_container_factor = if mode == TaggingMode::Manual {
        3.0
    } else {
        2.7
    };
    let inner = std::array::from_fn(|index| {
        // Preserve the two nested native group times. The outer group repeats
        // every 1.9099165s; its child begins at -0.209625s, and the core child
        // begins at the per-layer negative phase offset.
        let group_local = inner_phase + INNER_COMMON_OFFSET;
        let core_local = group_local + INNER_PHASE_OFFSETS[index];
        let null_local = group_local;
        let core_scale = inner_core_scale(core_local);
        let core_child_scale = inner_core_sublayer_scale(core_local);
        let null_scale = inner_scale_null(null_local);
        let core_opacity = inner_core_opacity(core_local);
        let base_opacity = INNER_BASE_OPACITY[index];
        let shape_diameter = INNER_DIAMETERS[index];
        let diameter = shape_diameter
            * (inner_container_factor / 3.0)
            * core_scale
            * core_child_scale
            * null_scale
            * bass_scale;
        let opacity = base_opacity * bass_opacity * core_opacity;
        CircleFrame {
            diameter,
            shape_diameter,
            core_scale,
            sublayer_scale: core_child_scale,
            scale_null: null_scale,
            core_opacity,
            base_opacity,
            audio_scale: bass_scale,
            opacity: opacity.clamp(0.0, 1.0),
            stroke_width: 0.0,
            local_time: core_local,
        }
    });

    let outer_phase_values = [0.0, OUTER_SECOND_PHASE];
    let outer = std::array::from_fn(|index| {
        let native_local = outer_phase - outer_phase_values[index];
        let local = native_local.clamp(0.0, OUTER_CORE_DURATION);
        // UI-150 registers this keyframe directly on each outerCircleLayer,
        // without the core helper's second-layer phase offset. Keep this
        // clock and track independent from circleContainer.scale and
        // circleLayer.size even though the current renderer composes scales.
        let scale_null_local = elapsed.rem_euclid(OUTER_SCALE_NULL_DURATION);
        let container_scale = outer_container_scale(local);
        let core_size_scale = outer_size(local);
        let scale_null = outer_scale_null(scale_null_local);
        OuterCircleFrame {
            diameter: OUTER_DIAMETERS[index]
                * container_scale
                * core_size_scale
                * scale_null
                * treble_scale,
            shape_diameter: OUTER_DIAMETERS[index] * core_size_scale,
            opacity: treble_opacity,
            stroke_width: outer_border_width(local),
            core_size_scale,
            container_scale,
            scale_null,
            audio_scale: treble_scale,
            local_time: local,
            scale_null_local_time: scale_null_local,
        }
    });

    let auto_shadow_scale = 1.0;

    WaveFrame {
        button_scale: idle_pulse,
        button_child_scale,
        inner_audio_scale: bass_scale,
        inner_audio_opacity: bass_opacity,
        outer_audio_scale: treble_scale,
        outer_audio_opacity: treble_opacity,
        inner,
        outer,
        auto_shadow_scale,
        inner_container_factor,
        outer_container_factor: 6.3,
    }
}

pub fn idle_pulse_at(elapsed_seconds: f64) -> f64 {
    let elapsed = if elapsed_seconds.is_finite() {
        elapsed_seconds.max(0.0)
    } else {
        0.0
    };
    let cycle = elapsed.rem_euclid(2.0);
    if cycle <= 1.0 {
        linear(1.0, 1.06, native_bezier(cycle))
    } else {
        linear(1.06, 1.0, native_bezier(cycle - 1.0))
    }
}

/// Native spring coefficients with an analytic under-damped step response.
/// `progress` is normalized to the explicit animation duration; `settlingDuration`
/// remains a separate, injectable OS value for formulas that depend on it.
pub fn spring_value(progress: f64, profile: SpringProfile) -> f64 {
    let p = finite_unit(progress);
    let elapsed = p * profile.duration_seconds.max(0.0);
    if elapsed <= 0.0 || profile.mass <= 0.0 || profile.stiffness <= 0.0 {
        return profile.from;
    }
    let natural = (profile.stiffness / profile.mass).sqrt();
    let decay = profile.damping / (2.0 * profile.mass);
    let discriminant = (natural * natural - decay * decay).max(0.0);
    let damped = discriminant.sqrt();
    if damped < 1e-9 {
        let displacement = profile.from - profile.to;
        return profile.to
            + (displacement + (profile.initial_velocity + decay * displacement) * elapsed)
                * (-decay * elapsed).exp();
    }
    let displacement = profile.from - profile.to;
    let sine_coefficient = (profile.initial_velocity + decay * displacement) / damped;
    profile.to
        + (-decay * elapsed).exp()
            * (displacement * (damped * elapsed).cos()
                + sine_coefficient * (damped * elapsed).sin())
}

/// [I] A replaceable preview estimate for Core Animation's runtime
/// `CASpringAnimation.settlingDuration`. The native value is not statically
/// determined; the app can inject a measured value without changing the
/// transition formulas. `settling_seconds` intentionally defaults to `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SettlingDuration {
    pub settling_seconds: Option<f64>,
}

impl SettlingDuration {
    #[allow(dead_code)] // The native runtime settling duration is still an injected [U] input.
    pub fn resolved(self) -> Option<f64> {
        self.settling_seconds
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
    }
}

pub fn dpi_scale(dpi: u32) -> f64 {
    f64::from(dpi.max(1)) / 96.0
}

pub fn physical_pixels(logical_units: f64, dpi: u32) -> i32 {
    (logical_units * dpi_scale(dpi)).round() as i32
}

#[allow(dead_code)] // Retained until measured spring settling is available to the visual transition.
pub fn no_match_timing(settling: SettlingDuration) -> Option<NoMatchTiming> {
    let spring_settle = settling.resolved()?;
    Some(NoMatchTiming {
        tag_button_center_begin: 0.3,
        tagging_layer_left_fraction: -0.15,
        button_layer_left_fraction: -0.07,
        button_layer_center_spring_duration: 1.1,
        tagging_layer_center_spring_duration: 0.5,
        button_scale_down: (0.0, 0.3),
        button_scale_up_begin: spring_settle - 0.35,
        button_scale_up: 0.3,
        icon_mask_initial_fade: (0.1, 0.15),
        icon_mask_secondary_fade: (spring_settle - 0.3, 0.2),
        logo_fade_in: (spring_settle - 0.25, 0.35),
        logo_scale_in: (spring_settle - 0.25, 0.5),
        logo_scale_spring: [1.0, 14.0, 100.0, 23.0],
    })
}

pub fn no_match_position_profile(
    button_width: f64,
    left_fraction: f64,
    initial_velocity: f64,
    duration_seconds: f64,
) -> SpringProfile {
    SpringProfile {
        from: button_width * left_fraction,
        to: 0.0,
        mass: 1.0,
        damping: 10.0,
        stiffness: 250.0,
        initial_velocity,
        duration_seconds,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoMatchTiming {
    pub tag_button_center_begin: f64,
    pub tagging_layer_left_fraction: f64,
    pub button_layer_left_fraction: f64,
    pub button_layer_center_spring_duration: f64,
    pub tagging_layer_center_spring_duration: f64,
    pub button_scale_down: (f64, f64),
    pub button_scale_up_begin: f64,
    pub button_scale_up: f64,
    pub icon_mask_initial_fade: (f64, f64),
    pub icon_mask_secondary_fade: (f64, f64),
    pub logo_fade_in: (f64, f64),
    pub logo_scale_in: (f64, f64),
    pub logo_scale_spring: [f64; 4],
}

fn finite_unit(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(left: f64, right: f64, tolerance: f64) {
        assert!(
            (left - right).abs() <= tolerance,
            "{left} differs from {right} by more than {tolerance}"
        );
    }

    #[test]
    fn native_bezier_reference_samples_match_spec() {
        for (x, expected) in [
            (0.125, 0.039_998_5),
            (0.25, 0.138_717_5),
            (0.5, 0.506_418_1),
            (0.75, 0.866_529_0),
            (0.875, 0.961_445_1),
        ] {
            close(native_bezier(x), expected, 0.000_003);
        }
        assert_eq!(native_bezier(0.0), 0.0);
        assert_eq!(native_bezier(1.0), 1.0);
    }

    #[test]
    fn startup_logo_tracks_keep_the_two_ui154_clocks_and_easings_separate() {
        let first = startup_logo_frame_at(0.15);
        let expected_first = bezier_timing(
            0.15 / STARTUP_LOGO_TRANSPARENT_DURATION,
            STARTUP_LOGO_TRANSPARENT_BEZIER,
        );
        assert_eq!(first.stage, StartupLogoStage::TransparentStroke);
        close(first.transparent_trim_start, expected_first, 1e-12);
        assert_eq!(first.solid_long_trim_end, 0.0);

        let boundary = startup_logo_frame_at(STARTUP_LOGO_TRANSPARENT_DURATION);
        assert_eq!(boundary.stage, StartupLogoStage::SolidLongStroke);
        assert_eq!(boundary.transparent_trim_start, 1.0);
        assert_eq!(boundary.solid_long_trim_end, 0.0);

        let second =
            startup_logo_frame_at(STARTUP_LOGO_SOLID_DELAY + STARTUP_LOGO_SOLID_DURATION * 0.5);
        let expected_second = bezier_timing(0.5, STARTUP_LOGO_SOLID_BEZIER);
        assert_eq!(second.stage, StartupLogoStage::SolidLongStroke);
        close(
            second.solid_long_trim_start,
            STARTUP_LOGO_SOLID_TRIM_START_END * expected_second,
            1e-12,
        );
        close(second.solid_long_trim_end, expected_second, 1e-12);
        assert_eq!(second.transparent_trim_start, 1.0);

        let complete = startup_logo_frame_at(STARTUP_LOGO_TOTAL_DURATION);
        assert_eq!(complete.stage, StartupLogoStage::Complete);
        assert_eq!(complete.solid_long_trim_start, 0.453_125);
        assert_eq!(complete.solid_long_trim_end, 1.0);
        assert!((STARTUP_LOGO_TOTAL_DURATION - 0.84).abs() < 1e-6);
        assert_eq!(startup_logo_frame_at(f64::NAN).transparent_trim_start, 0.0);
    }

    #[test]
    fn root_and_regions_use_design_logical_dimensions() {
        assert_eq!(CONTENT_WIDTH, 380.0);
        assert_eq!(CONTENT_HEIGHT, TOP_HEIGHT + HISTORY_HEIGHT);
        assert_eq!(BUTTON_X + BUTTON_SIZE, 238.0);
        assert_eq!(BUTTON_X, (CONTENT_WIDTH - BUTTON_SIZE) / 2.0);
    }

    #[test]
    fn nested_inner_phase_and_audio_transforms_are_independent() {
        let low = wave_frame_at(
            TaggingMode::Manual,
            0.4,
            UiLevels {
                bass: 0.0,
                treble: 0.0,
            },
        );
        let high = wave_frame_at(
            TaggingMode::Manual,
            0.4,
            UiLevels {
                bass: 1.0,
                treble: 1.0,
            },
        );
        assert_eq!(low.inner[0].local_time, high.inner[0].local_time);
        assert_eq!(low.inner_audio_scale, 0.7);
        assert_eq!(high.inner_audio_scale, 1.3);
        assert_eq!(low.inner_audio_opacity, 0.5);
        assert_eq!(high.inner_audio_opacity, 0.9);
        close(
            low.inner[0].shape_diameter,
            high.inner[0].shape_diameter,
            1e-12,
        );
        assert_eq!(low.inner[0].audio_scale, 0.7);
        assert_eq!(high.inner[0].audio_scale, 1.3);
        assert_eq!(low.button_child_scale, 0.75);
        assert_eq!(high.button_child_scale, 1.1);
        assert_eq!(low.outer_audio_scale, 0.9);
        assert_eq!(high.outer_audio_scale, 1.1);
        close(
            low.outer[0].shape_diameter,
            high.outer[0].shape_diameter,
            1e-12,
        );
        assert_eq!(low.outer[0].audio_scale, 0.9);
        assert_eq!(high.outer[0].audio_scale, 1.1);
        assert_eq!(low.inner_container_factor, 3.0);
        assert_eq!(low.outer_container_factor, 6.3);
    }

    #[test]
    fn ui150_outer_scale_null_has_its_own_unshifted_loop() {
        let eased_half = OUTER_SCALE_NULL_DURATION * (0.657_894_730_567_932_1 + 1.0) * 0.5;
        let frame = wave_frame_at(TaggingMode::Manual, eased_half, UiLevels::default());
        let expected_null = 1.0 - 0.07 * native_bezier(0.5);

        close(frame.outer[0].scale_null, expected_null, 1e-8);
        close(frame.outer[1].scale_null, expected_null, 1e-8);
        close(
            frame.outer[0].scale_null_local_time,
            frame.outer[1].scale_null_local_time,
            1e-12,
        );
        close(
            frame.outer[0].local_time - frame.outer[1].local_time,
            OUTER_SECOND_PHASE,
            1e-8,
        );

        let neutral = wave_frame_at(TaggingMode::Ready, 0.0, UiLevels::default());
        assert_eq!(neutral.outer[0].scale_null, 1.0);
        assert_eq!(neutral.outer[1].scale_null, 1.0);
    }

    #[test]
    fn outer_core_tracks_preserve_second_phase_and_shape_separation() {
        let initial = wave_frame_at(TaggingMode::Manual, 0.0, UiLevels::default());
        assert_eq!(initial.outer[0].container_scale, 0.85);
        assert_eq!(initial.outer[1].container_scale, 0.85);

        let second_starts =
            wave_frame_at(TaggingMode::Manual, OUTER_SECOND_PHASE, UiLevels::default());
        assert_eq!(second_starts.outer[1].container_scale, 0.85);
        assert_eq!(second_starts.outer[1].core_size_scale, 0.615_384_6);

        let second_peak = wave_frame_at(
            TaggingMode::Manual,
            OUTER_SECOND_PHASE + 0.75 * OUTER_CORE_DURATION,
            UiLevels::default(),
        );
        assert_eq!(second_peak.outer[1].container_scale, 0.9);
        assert_eq!(second_peak.outer[1].core_size_scale, 1.0);
        assert_eq!(second_peak.outer[1].shape_diameter, OUTER_DIAMETERS[1]);
        assert_ne!(
            second_peak.outer[1].diameter,
            second_peak.outer[1].shape_diameter
        );
    }

    #[test]
    fn shifted_outer_geometry_tracks_match_delayed_model_tracks() {
        for index in 0..OUTER_DIAMETERS.len() {
            for track in [OuterTrack::CoreSize, OuterTrack::BorderWidth] {
                let keyframes = delayed_outer_track(track, index);
                for step in 0..1_000 {
                    let elapsed = OUTER_LOOP_DURATION * f64::from(step) / 1_000.0;
                    let frame = wave_frame_at(TaggingMode::Manual, elapsed, UiLevels::default())
                        .outer[index];
                    let expected = match track {
                        OuterTrack::CoreSize => frame.core_size_scale,
                        OuterTrack::BorderWidth => frame.stroke_width,
                    };
                    let actual =
                        evaluate_track_keyframes(&keyframes, elapsed / OUTER_LOOP_DURATION);
                    close(actual, expected, 2e-8);
                }
            }
        }
    }

    #[test]
    fn shifted_inner_composition_tracks_match_nested_time_evaluator() {
        for index in 0..INNER_DIAMETERS.len() {
            for track in [
                InnerTrack::CoreScale,
                InnerTrack::CoreOpacity,
                InnerTrack::SublayerScale,
                InnerTrack::ScaleNull,
            ] {
                let keyframes = shifted_inner_track(track, index);
                for step in 0..1_000 {
                    let elapsed = INNER_OUTER_LOOP * f64::from(step) / 1_000.0;
                    let frame = wave_frame_at(TaggingMode::Manual, elapsed, UiLevels::default())
                        .inner[index];
                    let expected = match track {
                        InnerTrack::CoreScale => frame.core_scale,
                        InnerTrack::CoreOpacity => frame.base_opacity * frame.core_opacity,
                        InnerTrack::SublayerScale => frame.sublayer_scale,
                        InnerTrack::ScaleNull => frame.scale_null,
                    };
                    let actual = evaluate_track_keyframes(&keyframes, elapsed / INNER_OUTER_LOOP);
                    close(actual, expected, 2e-8);
                }
            }
        }
    }

    #[test]
    fn arbitrary_times_repeat_without_accumulated_frame_error() {
        let a = wave_frame_at(TaggingMode::Manual, 0.123, UiLevels::default());
        let b = wave_frame_at(
            TaggingMode::Manual,
            0.123 + INNER_OUTER_LOOP * 10_000.0,
            UiLevels::default(),
        );
        for index in 0..4 {
            close(a.inner[index].diameter, b.inner[index].diameter, 0.000_02);
            close(a.inner[index].opacity, b.inner[index].opacity, 0.000_02);
        }
    }

    #[test]
    fn ready_pulse_is_one_second_each_way_and_auto_has_separate_geometry() {
        assert_eq!(idle_pulse_at(0.0), 1.0);
        close(idle_pulse_at(1.0), 1.06, 1e-12);
        let manual = wave_frame_at(
            TaggingMode::Manual,
            0.2,
            UiLevels {
                bass: 0.5,
                treble: 0.5,
            },
        );
        let auto = wave_frame_at(TaggingMode::Auto, 0.2, UiLevels::default());
        assert_eq!(manual.inner_container_factor, 3.0);
        assert_eq!(auto.inner_container_factor, 2.7);
        close(
            auto.inner_container_factor / manual.inner_container_factor,
            0.9,
            1e-12,
        );
        close(
            manual.inner[0].diameter / auto.inner[0].diameter,
            1.0 / 0.9,
            1e-6,
        );
        assert_eq!(auto.button_child_scale, 1.0);
        let injected = wave_frame_at_with_auto_spring(
            TaggingMode::Auto,
            0.2,
            UiLevels::default(),
            Some(SpringProfile {
                from: 0.95,
                to: 0.85,
                mass: 1.0,
                damping: 10.0,
                stiffness: 100.0,
                initial_velocity: 0.0,
                duration_seconds: 0.2,
            }),
        );
        assert_ne!(injected.button_child_scale, auto.button_child_scale);
    }

    #[test]
    fn dpi_conversion_covers_requested_scale_factors_without_clock_reset() {
        for (dpi, scale) in [
            (96, 1.0),
            (120, 1.25),
            (144, 1.5),
            (168, 1.75),
            (192, 2.0),
            (240, 2.5),
            (288, 3.0),
        ] {
            close(dpi_scale(dpi), scale, 1e-12);
            assert_eq!(physical_pixels(380.0, dpi), (380.0 * scale).round() as i32);
        }
    }

    #[test]
    fn no_match_native_settling_is_unresolved_but_formula_is_replaceable() {
        assert_eq!(SettlingDuration::default().settling_seconds, None);
        let timing = no_match_timing(SettlingDuration {
            settling_seconds: Some(0.77),
        })
        .unwrap();
        close(timing.button_scale_up_begin, 0.42, 1e-12);
        close(timing.icon_mask_secondary_fade.0, 0.47, 1e-12);
        close(timing.logo_fade_in.0, 0.52, 1e-12);
        close(timing.tagging_layer_left_fraction, -0.15, 1e-12);
        close(timing.button_layer_left_fraction, -0.07, 1e-12);
        assert!(no_match_timing(SettlingDuration::default()).is_none());
        assert!(no_match_timing(SettlingDuration {
            settling_seconds: Some(f64::NAN)
        })
        .is_none());
    }

    #[test]
    fn no_match_keeps_tagging_and_button_springs_distinct() {
        let tagging = no_match_position_profile(96.0, -0.15, 8.449_999_809_265_137, 0.5);
        let button =
            no_match_position_profile(96.0, -0.07, 3.940_000_057_220_459, 1.100_000_023_841_858);
        close(tagging.from, -14.4, 1e-6);
        close(button.from, -6.72, 1e-6);
        assert_ne!(tagging.duration_seconds, button.duration_seconds);
        assert_ne!(tagging.initial_velocity, button.initial_velocity);
        assert_eq!(spring_value(0.0, tagging), tagging.from);
        assert_eq!(spring_value(0.0, button), button.from);
        assert_ne!(spring_value(0.5, tagging), spring_value(0.5, button));
    }
}
