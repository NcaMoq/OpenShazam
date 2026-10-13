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
pub const OUTER_SECOND_PHASE: f64 = 0.208_333_35;
pub const OUTER_DIAMETERS: [f64; 2] = [604.8, 514.08];

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
    pub opacity: f64,
    pub stroke_width: f64,
    /// Local child animation time is retained for deterministic inspection.
    pub local_time: f64,
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
    pub outer: [CircleFrame; 2],
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
        let diameter = INNER_DIAMETERS[index]
            * (inner_container_factor / 3.0)
            * bass_scale
            * core_scale
            * core_child_scale
            * null_scale;
        let opacity = INNER_BASE_OPACITY[index] * bass_opacity * inner_core_opacity(core_local);
        CircleFrame {
            diameter,
            opacity: opacity.clamp(0.0, 1.0),
            stroke_width: 0.0,
            local_time: core_local,
        }
    });

    let outer_phase_values = [0.0, OUTER_SECOND_PHASE];
    let outer = std::array::from_fn(|index| {
        let native_local = outer_phase - outer_phase_values[index];
        let local = native_local.clamp(0.0, OUTER_CORE_DURATION);
        CircleFrame {
            diameter: OUTER_DIAMETERS[index]
                * outer_container_scale(local)
                * treble_scale
                * outer_size(local),
            opacity: treble_opacity,
            stroke_width: outer_border_width(local),
            local_time: local,
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
        assert_eq!(low.button_child_scale, 0.75);
        assert_eq!(high.button_child_scale, 1.1);
        assert_eq!(low.outer_audio_scale, 0.9);
        assert_eq!(high.outer_audio_scale, 1.1);
        assert_eq!(low.inner_container_factor, 3.0);
        assert_eq!(low.outer_container_factor, 6.3);
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
}
