#![deny(unsafe_op_in_unsafe_fn)]

use super::notifications::{copy_notification_text, scale_notification_artwork_to_size};
use super::recognition::{capture_animation_start, remember_auto_candidate_key};
use super::StatusTextAlternator;
use super::{
    advance_result_card_stage, animation_needs_frame, animation_timer_needed,
    artwork_pixel_fingerprint, artwork_request_is_current, artwork_request_matches_track,
    artwork_request_urls_match_track, auto_shadow_host_center, bgra_buffer_layout,
    button_border_arc_points, capture_mode_in_progress, composition_button_pixel_snap_offset,
    composition_header_viewport_offset, composition_history_viewport_offset,
    composition_pixel_size, composition_root_layer_order, composition_scale_for_dpi,
    composition_scene_layer_order, copy_panel_pixels_into, create_text_format_with_locale,
    d2d_color_from_system, directwrite_locale, fallback_match_artwork_rect,
    finish_active_artwork_request, gradient_origin_transform, header_fill_scope,
    history_artwork_frame, history_composition_offset, history_first_insert_progress,
    history_row_top, history_text_rects, history_title_color,
    history_transition_requires_d2d_frame, inverse_circle_clip_radii, is_offline_failure,
    is_tray_activation_event, layered_surface_pixel_len, native_icon_layout, no_match_icon_rect,
    page_url_for_display_id, phase_animation_state, phase_needs_frame_at, phase_needs_timeout,
    point_in_rect, popup_base_color, resolve_page_link, resolve_page_link_checked,
    resolve_page_link_for_generation, result_artwork_rect, result_card_link_hit,
    result_details_rects, result_gradient_colors, result_header_hit_test_contains,
    result_header_visible_height, result_overlay_child_offset, result_overlay_failure_matches,
    result_overlay_intersects_header, result_panel_draw_top, result_panel_top_at,
    should_defer_hide_for_tray_activation, should_hide_popup_on_deactivation,
    should_replace_cached_artwork, should_start_startup_logo, status_text_snapshot,
    theme_from_foreground_rgb, tray_menu_action, ui_icon_row_stride, ui_model, valid_page_template,
    visible_history_row_hit, ArrowEdge, ArtworkPixels, ArtworkRequest, ArtworkRequestIdentity,
    ArtworkSource, CaptureMode, CompositionRootLayer, CompositionSceneLayer, HeaderFillScope,
    HistoryMutationEvent, HistoryMutationTransition, HistoryTimestampSource, MainPhase,
    NoMatchIconCommand, PageLinkFailure, ResultCardAdvance, ResultCardStage,
    ResultOverlayContentKey, ResultOverlayIdentity, Theme, Track, TrackIdentity, TrayMenuAction,
    UiConfigSnapshot, UiLanguage, ValidatedWebTrackUrl, AUTO_SHADOW_DISPLAY_BOUNDS,
    AUTO_START_SPRING_DURATION, BUTTON_CLEAR_DIAMETER, BUTTON_SIZE, BUTTON_TOP, BUTTON_X,
    CLOSE_BUTTON_GA_PIXELS, FALLBACK_MATCH_DARK_PIXELS, FALLBACK_MATCH_LIGHT_PIXELS,
    FALLBACK_MATCH_LOGICAL_SIDE, HISTORY_HEIGHT, HISTORY_INSERT_TRANSITION, HISTORY_ROW_HEIGHT,
    INNER_MASK_BOUNDS, NO_MATCH_ICON_PATH, OUTER_BASE_OPACITY, POPUP_ACTIVATION_GRACE,
    POPUP_ARROW_HEIGHT, RESULT_ARTWORK_CORNER_RADIUS, RESULT_CARD_HOLD, RESULT_CARD_TRANSITION,
    RESULT_CLOSE_ICON_PIXELS, RESULT_DETAILS_LEFT, RESULT_DETAILS_RIGHT, SETTINGS_ICON_PIXELS,
    TOP_HEIGHT, WHITE_CLOSE_ICON_PIXELS,
};
use super::{
    take_window_data_after_create_failure, WINDOW_DATA_CREATION_ATTACHED,
    WINDOW_DATA_CREATION_DESTROYED, WINDOW_DATA_CREATION_UNATTACHED,
};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::time::SystemTime;
use std::time::{Duration, Instant, UNIX_EPOCH};

#[test]
fn window_proc_rejects_missing_nonclient_creation_parameters() {
    let result = unsafe {
        super::window_proc(
            super::HWND::default(),
            super::WM_NCCREATE,
            super::WPARAM(0),
            super::LPARAM(0),
        )
    };
    assert_eq!(result.0, 0);
}

#[test]
fn failed_window_creation_drops_only_data_not_already_destroyed_by_wndproc() {
    use std::sync::atomic::AtomicU8;

    let unattached = AtomicU8::new(WINDOW_DATA_CREATION_UNATTACHED);
    assert!(take_window_data_after_create_failure(&unattached));
    assert_eq!(
        unattached.load(std::sync::atomic::Ordering::Acquire),
        WINDOW_DATA_CREATION_DESTROYED
    );

    let attached = AtomicU8::new(WINDOW_DATA_CREATION_ATTACHED);
    assert!(take_window_data_after_create_failure(&attached));
    assert_eq!(
        attached.load(std::sync::atomic::Ordering::Acquire),
        WINDOW_DATA_CREATION_DESTROYED
    );

    let already_destroyed = AtomicU8::new(WINDOW_DATA_CREATION_DESTROYED);
    assert!(!take_window_data_after_create_failure(&already_destroyed));
}

#[test]
fn ui229_history_keeps_controller_event_time_when_ui_delivery_is_delayed() {
    let emitted = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let received = emitted + Duration::from_secs(3_600);
    assert_eq!(
        super::resolve_history_timestamp(Some(emitted), received),
        (emitted, HistoryTimestampSource::ControllerEvent)
    );
    assert_eq!(
        super::resolve_history_timestamp(None, received),
        (received, HistoryTimestampSource::HostReceivedFallback)
    );
}

#[test]
fn ui229_history_date_uses_absolute_utc_timestamp_and_localized_formatter() {
    assert_eq!(
        super::history::winrt_datetime_from_system_time(UNIX_EPOCH).UniversalTime,
        super::history::WINDOWS_EPOCH_OFFSET_100NS
    );
    assert_eq!(
        super::history::winrt_datetime_from_system_time(UNIX_EPOCH + Duration::from_millis(1_000))
            .UniversalTime,
        super::history::WINDOWS_EPOCH_OFFSET_100NS + 10_000_000
    );
    assert_eq!(
        super::history::WINDOWS_DATETIME_TEMPLATE,
        "day month.abbreviated year hour minute"
    );

    unsafe {
        super::CoInitializeEx(None, super::COINIT_APARTMENTTHREADED).unwrap();
        let label = super::format_history_date_label(UNIX_EPOCH);
        super::CoUninitialize();
        assert!(!label.is_empty());
        assert_ne!(label, "たった今");
    }
}

#[test]
fn result_card_composition_uses_standalone_pixels_under_shared_popup_clip() {
    let body_top = 10.0;
    assert_eq!(
        result_panel_draw_top(Some(body_top - TOP_HEIGHT as f32), false),
        Some(body_top - TOP_HEIGHT as f32)
    );
    assert_eq!(result_panel_draw_top(Some(body_top + 90.0), true), None);
    assert_eq!(
        result_panel_draw_top(Some(body_top + 90.0), false),
        Some(body_top + 90.0)
    );
    assert_eq!(result_panel_draw_top(None, true), None);
}

#[test]
fn ui236_result_card_uses_candidate_a_and_residual_curve_without_restarting_time() {
    let curve = super::result_card_easing();
    for (time, expected) in [(0.25, 0.4085), (0.5, 0.8024), (0.75, 0.9605)] {
        assert!(
            (super::sample_cubic_timing_curve(time, curve) - expected).abs() < 0.001,
            "progress at {time:.2} should match UI-236 candidate A"
        );
    }

    for start in [0.0, 0.1, 0.25, 0.5, 0.75, 0.9] {
        let residual = super::remaining_cubic_easing(start, curve).unwrap();
        let y_start = super::sample_cubic_timing_curve(start, curve);
        for remaining in [0.0, 0.2, 0.5, 0.8, 1.0] {
            let original =
                super::sample_cubic_timing_curve(start + (1.0 - start) * remaining, curve);
            let resumed =
                y_start + (1.0 - y_start) * super::sample_cubic_timing_curve(remaining, residual);
            assert!(
                    (original - resumed).abs() < 1e-8,
                    "curve restart drift at start={start} remaining={remaining}: {original} vs {resumed}"
                );
        }
    }

    let started_at = Instant::now();
    assert!(
        (super::result_card_progress(started_at, started_at + Duration::from_millis(75)) - 0.4085)
            .abs()
            < 0.001
    );
    assert!(
        (super::result_card_progress(started_at, started_at + Duration::from_millis(150)) - 0.8024)
            .abs()
            < 0.001
    );
    assert!(
        (super::result_card_progress(started_at, started_at + Duration::from_millis(225)) - 0.9605)
            .abs()
            < 0.001
    );
}

#[test]
fn ui224_ready_and_auto_status_cycles_keep_native_delays() {
    let start = Instant::now();
    let mut ready = StatusTextAlternator::ready(start);
    assert!(ready.first);
    assert_eq!(ready.next_at, start + Duration::from_secs(3));

    assert!(ready.advance(start + Duration::from_secs(3)));
    assert!(!ready.first);
    assert_eq!(ready.next_at, start + Duration::from_secs(5));
    assert_eq!(
        ready.transition_started_at,
        Some(start + Duration::from_secs(3))
    );

    ready.advance(start + Duration::from_secs(5));
    assert!(ready.first);
    assert_eq!(ready.next_at, start + Duration::from_secs(12));
    ready.advance(start + Duration::from_secs(12));
    assert!(!ready.first);
    assert_eq!(ready.next_at, start + Duration::from_secs(14));

    let mut auto = StatusTextAlternator::auto(start);
    assert!(auto.first);
    auto.advance(start + Duration::from_secs(3));
    assert!(!auto.first);
    assert_eq!(auto.next_at, start + Duration::from_secs(6));
    auto.advance(start + Duration::from_secs(6));
    assert!(auto.first);
    assert_eq!(auto.next_at, start + Duration::from_secs(9));
}

#[test]
fn ui200_composition_status_snapshot_keeps_current_and_previous_text_identity() {
    let start = Instant::now();
    let mut auto = StatusTextAlternator::auto(start);
    auto.advance(start + Duration::from_secs(3));
    let listening_auto = MainPhase::Listening {
        mode: CaptureMode::Auto,
        started_at: start,
    };
    assert_eq!(
        super::status_text_snapshot(&listening_auto, Some(auto)),
        super::StatusTextSnapshot {
            current: Some(super::StatusTextLabel::AutoTurnOff),
            previous: Some(super::StatusTextLabel::AutoOn),
            transition_started_at: Some(start + Duration::from_secs(3)),
        }
    );

    let listening_manual = MainPhase::Listening {
        mode: CaptureMode::Manual,
        started_at: start,
    };
    assert_eq!(
        super::status_text_snapshot(&listening_manual, Some(auto)),
        super::StatusTextSnapshot {
            current: Some(super::StatusTextLabel::Listening),
            previous: None,
            transition_started_at: None,
        }
    );

    let no_match = MainPhase::NoMatch {
        entered_at: start,
        returned_to_ready_at: None,
    };
    assert_eq!(
        super::status_text_snapshot(&no_match, Some(auto)),
        super::StatusTextSnapshot::default()
    );
}

#[test]
fn recognition_timeout_states_do_not_share_result_dismiss_timers() {
    let now = Instant::now();
    assert!(phase_needs_timeout(&MainPhase::NoMatch {
        entered_at: now,
        returned_to_ready_at: None,
    }));
    assert!(phase_needs_timeout(&MainPhase::Error {
        message: "error".to_owned(),
        entered_at: now,
    }));
    assert!(!phase_needs_timeout(&MainPhase::Timeout {
        mode: CaptureMode::Manual,
        entered_at: now,
    }));
    assert!(!phase_needs_timeout(&MainPhase::Listening {
        mode: CaptureMode::Manual,
        started_at: now,
    }));
}

#[test]
fn popup_activation_handoff_does_not_close_on_its_initial_focus_loss() {
    let shown_at = Instant::now();
    let grace_until = shown_at + POPUP_ACTIVATION_GRACE;
    let can_close =
        |now| should_hide_popup_on_deactivation(true, true, true, Some(grace_until), now);
    assert!(!can_close(shown_at));
    assert!(!can_close(shown_at + Duration::from_millis(200)));
    assert!(can_close(grace_until));
    assert!(!should_hide_popup_on_deactivation(
        true,
        false,
        true,
        None,
        grace_until + Duration::from_secs(1)
    ));
    assert!(!should_hide_popup_on_deactivation(
        true,
        true,
        false,
        None,
        grace_until + Duration::from_secs(1)
    ));
}

#[test]
fn tray_click_activation_wins_over_the_focus_loss_it_causes() {
    assert!(should_defer_hide_for_tray_activation(Some(true)));
    assert!(should_defer_hide_for_tray_activation(None));
    assert!(!should_defer_hide_for_tray_activation(Some(false)));
}

#[test]
fn tray_menu_selection_maps_to_deferred_actions() {
    assert_eq!(
        tray_menu_action(super::TRAY_MENU_SETTINGS),
        TrayMenuAction::Settings
    );
    assert_eq!(
        tray_menu_action(super::TRAY_MENU_EXIT),
        TrayMenuAction::Exit
    );
    assert_eq!(tray_menu_action(0), TrayMenuAction::None);
}

#[test]
fn empty_history_skeleton_matches_nib_rows_after_coordinate_flip() {
    let nib_bottom_origin_y = [-39.0_f32, 55.0, 149.0, 243.0, 336.0];
    let mut expected_top_origin_y = nib_bottom_origin_y.map(|y| 430.0 - (y + 94.0));
    expected_top_origin_y.sort_by(f32::total_cmp);
    assert_eq!(super::EMPTY_HISTORY_ROW_TOPS, expected_top_origin_y);
}

#[test]
fn auto_candidate_dedupe_is_bounded_and_suppresses_repeated_candidates() {
    let mut recent = VecDeque::new();
    assert!(remember_auto_candidate_key(
        &mut recent,
        "track-key:a".to_owned()
    ));
    assert!(!remember_auto_candidate_key(
        &mut recent,
        "track-key:a".to_owned()
    ));
    assert!(remember_auto_candidate_key(
        &mut recent,
        "track-key:b".to_owned()
    ));

    for index in 0..super::AUTO_CANDIDATE_DEDUPE_LIMIT {
        assert!(remember_auto_candidate_key(
            &mut recent,
            format!("track-key:{index}")
        ));
    }
    assert_eq!(recent.len(), super::AUTO_CANDIDATE_DEDUPE_LIMIT);
    assert!(remember_auto_candidate_key(
        &mut recent,
        "track-key:a".to_owned()
    ));
}

#[test]
fn no_match_icon_path_preserves_native_command_topology_and_unclamped_controls() {
    let moves = NO_MATCH_ICON_PATH
        .iter()
        .filter(|command| matches!(command, NoMatchIconCommand::Move(_)))
        .count();
    let curves = NO_MATCH_ICON_PATH
        .iter()
        .filter(|command| matches!(command, NoMatchIconCommand::Cubic(_, _, _)))
        .count();
    let lines = NO_MATCH_ICON_PATH
        .iter()
        .filter(|command| matches!(command, NoMatchIconCommand::Line(_)))
        .count();
    let closes = NO_MATCH_ICON_PATH
        .iter()
        .filter(|command| matches!(command, NoMatchIconCommand::Close))
        .count();
    assert_eq!((moves, curves, lines, closes), (2, 15, 15, 2));

    assert_eq!(
        NO_MATCH_ICON_PATH[8], NO_MATCH_ICON_PATH[9],
        "the exact repeated UI-168 line must remain in the vector path"
    );
    assert!(NO_MATCH_ICON_PATH.iter().any(|command| matches!(
        command,
        NoMatchIconCommand::Cubic((_, cp1_y), (_, _), _)
            if *cp1_y < 0.0
    )));
    assert!(NO_MATCH_ICON_PATH.iter().any(|command| matches!(
        command,
        NoMatchIconCommand::Cubic((_, _), (cp2_x, _), _)
            if *cp2_x > 1.0
    )));
}

#[test]
fn no_match_icon_uses_the_native_portrait_mask_rect() {
    let [left, top, right, bottom] = no_match_icon_rect(96.0);
    assert!((left - 32.0).abs() < 1e-5);
    assert!((top - 1.2).abs() < 1e-5);
    assert!((right - 64.0).abs() < 1e-5);
    assert!((bottom - 94.8).abs() < 1e-4);
    assert!((right - left - 32.0).abs() < 1e-5);
    assert!((bottom - top - 93.6).abs() < 1e-4);
}

#[test]
fn ui_icon_bitmaps_require_an_exact_bgra_buffer_for_the_declared_dimensions() {
    assert_eq!(ui_icon_row_stride(32, 32, 32 * 32 * 4), Some(32 * 4));
    assert_eq!(ui_icon_row_stride(24, 24, 24 * 24 * 4), Some(24 * 4));
    for (pixels, width, height) in [
        (SETTINGS_ICON_PIXELS.as_slice(), 32, 32),
        (WHITE_CLOSE_ICON_PIXELS.as_slice(), 24, 24),
        (RESULT_CLOSE_ICON_PIXELS.as_slice(), 24, 24),
        (FALLBACK_MATCH_LIGHT_PIXELS.as_slice(), 262, 262),
        (FALLBACK_MATCH_DARK_PIXELS.as_slice(), 262, 262),
    ] {
        assert_eq!(
            ui_icon_row_stride(width, height, pixels.len()),
            Some(width * 4)
        );
    }
    assert_eq!(ui_icon_row_stride(32, 32, 32 * 32 * 4 - 1), None);
    assert_eq!(ui_icon_row_stride(32, 32, 32 * 32 * 4 + 1), None);
    assert_eq!(ui_icon_row_stride(0, 32, 0), None);
    assert_eq!(ui_icon_row_stride(32, 0, 0), None);
    assert_eq!(ui_icon_row_stride(u32::MAX, 1, 0), None);
}

#[test]
fn layered_surface_byte_length_is_checked_before_creating_a_dib() {
    assert_eq!(layered_surface_pixel_len(1, 1), Some(4));
    assert_eq!(layered_surface_pixel_len(380, 620), Some(380 * 620 * 4));
    assert_eq!(layered_surface_pixel_len(0, 1), None);
    assert_eq!(layered_surface_pixel_len(1, 0), None);
    assert_eq!(layered_surface_pixel_len(i32::MAX as u32, 1), None);
}

#[test]
fn notification_text_is_nul_terminated_without_splitting_surrogate_pairs() {
    let mut buffer = [0xFFFF; 5];
    copy_notification_text(&mut buffer, "A😀B");
    assert_eq!(buffer, [b'A' as u16, 0xD83D, 0xDE00, b'B' as u16, 0]);

    let mut truncated = [0; 3];
    copy_notification_text(&mut truncated, "A😀B");
    assert_eq!(truncated, [b'A' as u16, 0, 0]);
}

#[test]
fn premultiplied_d2d_popup_union_keeps_antialiased_alpha_edges() {
    use super::*;

    unsafe {
        let surface = LayeredSurface::new(32, 32).unwrap();
        let factory: ID2D1Factory =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
        let properties = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: Default::default(),
        };
        let target = factory.CreateDCRenderTarget(&properties).unwrap();
        let area = RECT {
            left: 0,
            top: 0,
            right: 32,
            bottom: 32,
        };
        target.BindDC(surface.dc, &area).unwrap();
        target.BeginDraw();
        target.Clear(Some(&D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        }));
        let brush = target
            .CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                None,
            )
            .unwrap();
        let rounded = factory
            .CreateRoundedRectangleGeometry(
                &windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 0.0,
                        top: 6.0,
                        right: 32.0,
                        bottom: 32.0,
                    },
                    radiusX: 8.0,
                    radiusY: 8.0,
                },
            )
            .unwrap();
        let arrow = factory.CreatePathGeometry().unwrap();
        let arrow_sink = arrow.Open().unwrap();
        arrow_sink.BeginFigure(Vector2 { X: 12.0, Y: 6.0 }, D2D1_FIGURE_BEGIN_FILLED);
        arrow_sink.AddLine(Vector2 { X: 16.0, Y: 0.0 });
        arrow_sink.AddLine(Vector2 { X: 20.0, Y: 6.0 });
        arrow_sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        arrow_sink.Close().unwrap();
        let popup = factory.CreatePathGeometry().unwrap();
        let popup_sink = popup.Open().unwrap();
        rounded
            .CombineWithGeometry(&arrow, D2D1_COMBINE_MODE_UNION, None, 0.25, &popup_sink)
            .unwrap();
        popup_sink.Close().unwrap();
        let layer = target.CreateLayer(None).unwrap();
        let layer_parameters = windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS {
            contentBounds: D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: 32.0,
                bottom: 32.0,
            },
            geometricMask: std::mem::ManuallyDrop::new(Some(popup.into())),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: windows_numerics::Matrix3x2 {
                M11: 1.0,
                M22: 1.0,
                ..Default::default()
            },
            opacity: 1.0,
            opacityBrush: std::mem::ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS_NONE,
        };
        target.PushLayer(&layer_parameters, &layer);
        target.FillRectangle(
            &D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: 32.0,
                bottom: 32.0,
            },
            &brush,
        );
        target.PopLayer();
        target.EndDraw(None, None).unwrap();

        let pixels = surface.pixel_bytes();
        let alpha_values = (0..32usize * 32)
            .map(|pixel| pixels[pixel * 4 + 3])
            .collect::<Vec<_>>();
        assert!(
            alpha_values.contains(&0),
            "transparent corner pixels are retained"
        );
        assert!(
            alpha_values.iter().any(|alpha| (1..255).contains(alpha)),
            "rounded contour retains fractional coverage for smooth composition"
        );
    }
}

#[test]
fn native_button_logo_uses_specified_geometry_scale_and_stroke() {
    use super::*;

    let layout = shz_logo_layout(96.0, 96.0);
    assert!((layout.scale_x - (96.0 / 367.5)).abs() < f32::EPSILON);
    assert_eq!(layout.scale_x, layout.scale_y);
    assert_eq!((layout.offset_x, layout.offset_y), (-93.0, -93.0));
    assert_eq!(
        (
            layout.center_x - layout.offset_x,
            layout.center_y - layout.offset_y
        ),
        (141.0, 141.0)
    );
    assert!((SHZ_LOGO_STROKE_WIDTH * layout.scale_x - 9.926_531).abs() < 0.000_01);
    let top_start = shz_logo_point_in_button(layout, SHZ_LOGO_TOP_PATH.start);
    assert!((top_start.0 - 40.474_12).abs() < 0.000_02);
    assert!((top_start.1 - 23.282_94).abs() < 0.000_02);
    let bottom_start = shz_logo_point_in_button(layout, SHZ_LOGO_BOTTOM_PATH.start);
    assert!((bottom_start.0 - 55.528_49).abs() < 0.000_02);
    assert!((bottom_start.1 - 73.082_78).abs() < 0.000_02);
    assert_eq!(SHZ_LOGO_SOURCE_SIZE, 210.0);
    assert_eq!(SHZ_LOGO_CANVAS_PADDING, 36.0);
    assert_eq!(SHZ_LOGO_CANVAS_SIZE, 282.0);
    let long_path_y_min = -13.49;
    let long_path_y_max = 224.57;
    let cap_radius = SHZ_LOGO_STROKE_WIDTH * 0.5;
    assert!(long_path_y_min + SHZ_LOGO_CANVAS_PADDING - cap_radius > 0.0);
    assert!(long_path_y_max + SHZ_LOGO_CANVAS_PADDING + cap_radius < SHZ_LOGO_CANVAS_SIZE);
    assert_eq!(SHZ_LOGO_TOP_PATH.curves.len(), 4);
    assert_eq!(SHZ_LOGO_BOTTOM_PATH.curves.len(), 4);
    assert_eq!(SHZ_LOGO_TOP_PATH.start, (76.19, 10.38));
    assert_eq!(SHZ_LOGO_BOTTOM_PATH.start, (133.82, 201.02));
}

#[test]
fn composition_and_direct2d_share_logical_button_coordinates_at_each_dpi() {
    use super::*;

    for dpi in [96, 120, 144, 168, 192, 240, 288] {
        let scale = composition_scale_for_dpi(dpi);
        let d2d_center_x = ui_model::physical_pixels(BUTTON_X + BUTTON_SIZE * 0.5, dpi);
        let composition_center_x =
            ((BUTTON_X as f32 + BUTTON_SIZE as f32 * 0.5) * scale).round() as i32;
        let button_center_y = POPUP_ARROW_HEIGHT + BUTTON_TOP + BUTTON_SIZE * 0.5;
        let d2d_center_y = ui_model::physical_pixels(button_center_y, dpi);
        let composition_center_y = (button_center_y as f32 * scale).round() as i32;
        assert_eq!(composition_center_x, d2d_center_x, "{dpi} DPI x");
        assert_eq!(composition_center_y, d2d_center_y, "{dpi} DPI y");
        assert!((scale - ui_model::dpi_scale(dpi) as f32).abs() < f32::EPSILON);
    }
}

#[test]
fn composition_pixel_dimensions_preserve_fractional_dpi_and_reject_unrepresentable_sizes() {
    assert_eq!(composition_pixel_size(380.0, 96), Some(380));
    assert_eq!(composition_pixel_size(380.0, 120), Some(475));
    assert_eq!(composition_pixel_size(0.0, 120), None);
    assert_eq!(composition_pixel_size(380.0, u32::MAX), None);
}

#[test]
fn composition_pixel_phase_aligns_button_and_ripples_at_fractional_dpi() {
    for dpi in [96, 120, 144, 168, 192, 240, 288] {
        let scale = composition_scale_for_dpi(dpi);
        for body_top in [0.0, POPUP_ARROW_HEIGHT as f32] {
            let offset = composition_button_pixel_snap_offset(dpi, body_top);
            let logical_x = (BUTTON_X + BUTTON_SIZE * 0.5) as f32;
            let logical_y = body_top + (BUTTON_TOP + BUTTON_SIZE * 0.5) as f32;
            let physical_x = (logical_x + offset.X) * scale;
            let physical_y = (logical_y + offset.Y) * scale;
            assert!(
                (physical_x - physical_x.round()).abs() < 1e-5,
                "{dpi} DPI x"
            );
            assert!(
                (physical_y - physical_y.round()).abs() < 1e-5,
                "{dpi} DPI y"
            );
            assert!(offset.X.abs() * scale <= 0.5 + 1e-5, "{dpi} DPI x offset");
            assert!(offset.Y.abs() * scale <= 0.5 + 1e-5, "{dpi} DPI y offset");
        }
    }
}

#[test]
fn button_arc_preserves_native_degree_value_and_geometry() {
    let [start, _, _, end] = button_border_arc_points();
    assert_eq!(start, (97.0, 50.0));
    assert!((end.0 - 96.926_4).abs() < 0.001);
    assert!((end.1 - 52.685_3).abs() < 0.001);
    assert!(end.1 > start.1);
}

#[test]
fn native_icon_visual_frames_and_hit_targets_are_independent() {
    // Check canonical TopRegion-local values independently of popup edge.
    let layout = native_icon_layout(0.0, ArrowEdge::Top);
    assert_eq!(layout.settings_visual, [349.0, 25.0, 365.0, 41.0]);
    assert_eq!(layout.settings_hit, layout.settings_visual);
    assert_eq!(layout.cancel_visual, [351.0, 27.0, 363.0, 39.0]);
    assert_eq!(layout.cancel_hit, layout.cancel_visual);
    assert_eq!(
        (
            (layout.settings_visual[0] + layout.settings_visual[2]) * 0.5,
            (layout.settings_visual[1] + layout.settings_visual[3]) * 0.5,
        ),
        (357.0, 33.0)
    );
    assert_eq!(
        (
            (layout.cancel_visual[0] + layout.cancel_visual[2]) * 0.5,
            (layout.cancel_visual[1] + layout.cancel_visual[3]) * 0.5,
        ),
        (357.0, 33.0)
    );
    assert_eq!(layout.result_close_visual, [353.0, 25.0, 365.0, 37.0]);
    assert_eq!(layout.result_close_hit, layout.result_close_visual);
    assert_eq!(
        (
            (layout.result_close_visual[0] + layout.result_close_visual[2]) * 0.5,
            (layout.result_close_visual[1] + layout.result_close_visual[3]) * 0.5,
        ),
        (359.0, 31.0)
    );
    let bottom_anchored = native_icon_layout(0.0, ArrowEdge::Bottom);
    let top_anchored = native_icon_layout(POPUP_ARROW_HEIGHT as f32, ArrowEdge::Top);
    assert_eq!(
        top_anchored.settings_visual[1] - bottom_anchored.settings_visual[1],
        POPUP_ARROW_HEIGHT as f32 * 2.0
    );
    assert_eq!(
        top_anchored.cancel_hit[1] - bottom_anchored.cancel_hit[1],
        POPUP_ARROW_HEIGHT as f32 * 2.0
    );
    assert!(point_in_rect(349.0, 25.0, layout.settings_hit));
    assert!(!point_in_rect(348.9, 26.0, layout.settings_hit));
    assert!(point_in_rect(351.0, 27.0, layout.cancel_hit));
    assert!(!point_in_rect(350.9, 28.0, layout.cancel_hit));
    assert!(point_in_rect(353.0, 25.0, layout.result_close_hit));
    assert!(!point_in_rect(352.9, 25.0, layout.result_close_hit));
    assert!(!point_in_rect(349.0, 25.0, layout.result_close_hit));
}

#[test]
fn ui232_bottom_arrow_moves_header_icons_and_hit_targets_up_ten_dip() {
    let layout = native_icon_layout(0.0, ArrowEdge::Bottom);
    assert_eq!(layout.settings_visual, [349.0, 15.0, 365.0, 31.0]);
    assert_eq!(layout.settings_hit, layout.settings_visual);
    assert_eq!(layout.result_close_visual, [353.0, 15.0, 365.0, 27.0]);
    assert_eq!(layout.result_close_hit, layout.result_close_visual);
    assert_eq!(layout.cancel_visual, [351.0, 17.0, 363.0, 29.0]);
    assert_eq!(layout.cancel_hit, layout.cancel_visual);
    assert!(point_in_rect(357.0, 15.0, layout.settings_hit));
    assert!(point_in_rect(359.0, 21.0, layout.result_close_hit));
    assert!(point_in_rect(357.0, 23.0, layout.cancel_hit));
    assert!(!point_in_rect(357.0, 14.9, layout.settings_hit));
}

#[test]
fn os_foreground_color_uses_the_documented_windows_theme_threshold() {
    assert_eq!(theme_from_foreground_rgb(0, 0, 0), Theme::Light);
    assert_eq!(theme_from_foreground_rgb(255, 255, 255), Theme::Dark);
    assert_eq!(theme_from_foreground_rgb(128, 128, 128), Theme::Light);
    assert_eq!(theme_from_foreground_rgb(0, 255, 0), Theme::Dark);
}

#[test]
fn ui223_history_title_uses_theme_specific_ink_with_documented_alpha() {
    let light = history_title_color(Theme::Light);
    assert_eq!((light.r, light.g, light.b), (0.0, 0.0, 0.0));
    assert!((light.a - 0.847).abs() < f32::EPSILON);

    let dark = history_title_color(Theme::Dark);
    assert_eq!((dark.r, dark.g, dark.b), (1.0, 1.0, 1.0));
    assert!((dark.a - 0.847).abs() < f32::EPSILON);
}

#[test]
fn ui222_result_card_link_excludes_close_button() {
    let top = 42.0;
    let close = [353.0, top + 25.0, 365.0, top + 37.0];
    assert!(result_card_link_hit(30.0, top + 80.0, top, close));
    assert!(!result_card_link_hit(359.0, top + 31.0, top, close));
    assert!(!result_card_link_hit(30.0, top - 0.1, top, close));
    assert!(!result_card_link_hit(
        30.0,
        top + TOP_HEIGHT as f32 + 0.1,
        top,
        close
    ));
}

#[test]
fn ui222_history_link_hit_respects_viewport_clipping() {
    let top = 200.0;
    let bottom = top + HISTORY_HEIGHT as f32;
    assert!(visible_history_row_hit(top + 1.0, top, top, bottom));
    assert!(!visible_history_row_hit(top - 1.0, top - 20.0, top, bottom));
    assert!(!visible_history_row_hit(
        bottom + 1.0,
        bottom - 10.0,
        top,
        bottom
    ));
    assert!(visible_history_row_hit(
        bottom - 1.0,
        bottom - 80.0,
        top,
        bottom
    ));
}

#[test]
fn ui222_page_template_requires_one_track_key_token() {
    assert!(valid_page_template(
        "https://www.shazam.com/track/{trackid}"
    ));
    let referrer_template = "https://www.shazam.com/track/{trackid}#referrer=shazamapp";
    assert!(valid_page_template(referrer_template));
    assert_eq!(
        resolve_page_link_checked(Some(referrer_template), Some("abc123")),
        Ok(ValidatedWebTrackUrl::from_url(
            "https://www.shazam.com/track/abc123#referrer=shazamapp".to_owned(),
        )
        .unwrap())
    );
    assert!(!valid_page_template(
        "https://www.shazam.com/track/{trackid}/{trackid}"
    ));
    assert!(resolve_page_link(
        Some("https://www.shazam.com/track/{trackid}"),
        Some("abc/123")
    )
    .is_some_and(|url| url.as_str() == "https://www.shazam.com/track/abc%2F123"));
    assert!(resolve_page_link(Some("https://www.shazam.com/track/{trackid}"), None).is_none());
}

#[test]
fn ui222_page_link_failure_identifies_missing_or_invalid_inputs_without_logging_values() {
    let configs = std::collections::HashMap::from([(
        10,
        UiConfigSnapshot {
            generation: 10,
            web_track_url_template: None,
        },
    )]);

    assert_eq!(
        resolve_page_link_for_generation(None, &configs, Some("key-1")),
        Err(PageLinkFailure::MissingConfigGeneration)
    );
    assert_eq!(
        resolve_page_link_for_generation(Some(11), &configs, Some("key-1")),
        Err(PageLinkFailure::MissingConfigSnapshot)
    );
    assert_eq!(
        resolve_page_link_for_generation(Some(10), &configs, Some("key-1")),
        Err(PageLinkFailure::MissingTemplate)
    );
    assert_eq!(
        resolve_page_link_checked(
            Some("https://www.shazam.com/track/{trackid}/{trackid}"),
            Some("key-1")
        ),
        Err(PageLinkFailure::InvalidTemplate)
    );
    assert_eq!(
        resolve_page_link_checked(Some("https://www.shazam.com/track/{trackid}"), None),
        Err(PageLinkFailure::MissingTrackKey)
    );
    assert_eq!(
        resolve_page_link_checked(Some("https://www.shazam.com/track/{trackid}"), Some("")),
        Err(PageLinkFailure::InvalidTrackKey)
    );
    assert_eq!(
        PageLinkFailure::MissingTemplate.as_str(),
        "missing_webtrackurl"
    );
}

#[test]
fn ui222_result_and_history_page_lookup_uses_the_clicked_track_identity() {
    let recognized_at = Instant::now();
    let make_track = |display_id, url: &str| Track {
        display_id,
        title: format!("track-{display_id}"),
        artist: "artist".to_owned(),
        recognized_at,
        recognized_at_utc: UNIX_EPOCH,
        recognized_date_label: String::new(),
        timestamp_source: HistoryTimestampSource::HostReceivedFallback,
        artwork_url: None,
        hq_artwork_url: None,
        background_artwork_url: None,
        track_key: None,
        config_generation: None,
        page_url: Some(ValidatedWebTrackUrl::from_url(url.to_owned()).unwrap()),
    };
    let history_track = make_track(41, "https://www.shazam.com/track/history-41");
    let result_track = make_track(42, "https://www.shazam.com/track/result-42");
    let history = [history_track];

    let configs = std::collections::HashMap::new();
    assert_eq!(
        page_url_for_display_id(&history, Some(&result_track), 41, &configs)
            .map(|url| url.as_str().to_owned()),
        Some("https://www.shazam.com/track/history-41".to_owned())
    );
    assert_eq!(
        page_url_for_display_id(&history, Some(&result_track), 42, &configs)
            .map(|url| url.as_str().to_owned()),
        Some("https://www.shazam.com/track/result-42".to_owned())
    );
    assert!(page_url_for_display_id(&history, Some(&result_track), 99, &configs).is_none());
}

#[test]
fn ui222_clicked_track_resolves_page_url_from_its_config_generation() {
    let recognized_at = Instant::now();
    let track = Track {
        display_id: 51,
        title: "track-51".to_owned(),
        artist: "artist".to_owned(),
        recognized_at,
        recognized_at_utc: UNIX_EPOCH,
        recognized_date_label: String::new(),
        timestamp_source: HistoryTimestampSource::HostReceivedFallback,
        artwork_url: None,
        hq_artwork_url: None,
        background_artwork_url: None,
        track_key: Some("key-51".to_owned()),
        config_generation: Some(91),
        page_url: None,
    };
    let configs = std::collections::HashMap::from([(
        91,
        UiConfigSnapshot {
            generation: 91,
            web_track_url_template: Some("https://www.shazam.com/track/{trackid}".to_owned()),
        },
    )]);

    assert_eq!(
        page_url_for_display_id(&[], Some(&track), 51, &configs).map(|url| url.as_str().to_owned()),
        Some("https://www.shazam.com/track/key-51".to_owned())
    );
}

#[test]
fn restoring_history_revalidates_saved_shazam_page_urls() {
    let make_entry = |url: &str| super::PersistedTrack {
        title: "title".to_owned(),
        artist: "artist".to_owned(),
        recognized_at_utc: UNIX_EPOCH,
        track_key: None,
        artwork_url: None,
        hq_artwork_url: None,
        background_artwork_url: None,
        shazam_page_url: Some(url.to_owned()),
        source: super::RecognitionSource::Manual,
    };
    let restored = super::restore_history_tracks(vec![
        make_entry("https://www.shazam.com/track/valid-key"),
        make_entry("https://attacker.example/track/forged"),
    ]);

    assert_eq!(
        restored[0]
            .page_url
            .as_ref()
            .map(|url| url.as_str().to_owned()),
        Some("https://www.shazam.com/track/valid-key".to_owned())
    );
    assert!(restored[1].page_url.is_none());
}

#[test]
fn artwork_bitmap_cache_replaces_changed_pixels_without_downgrading_quality() {
    let first = artwork_pixel_fingerprint(1, 1, &[1, 2, 3, 4]);
    let refreshed = artwork_pixel_fingerprint(1, 1, &[5, 6, 7, 8]);
    assert_ne!(first, refreshed);
    assert!(should_replace_cached_artwork(
        None,
        ArtworkSource::Standard,
        1,
        1,
        first
    ));
    assert!(!should_replace_cached_artwork(
        Some((ArtworkSource::Standard, 1, 1, first)),
        ArtworkSource::Standard,
        1,
        1,
        first,
    ));
    assert!(should_replace_cached_artwork(
        Some((ArtworkSource::Standard, 1, 1, first)),
        ArtworkSource::Standard,
        1,
        1,
        refreshed,
    ));
    assert!(should_replace_cached_artwork(
        Some((ArtworkSource::Standard, 1, 1, first)),
        ArtworkSource::HighQuality,
        1,
        1,
        first,
    ));
    assert!(!should_replace_cached_artwork(
        Some((ArtworkSource::HighQuality, 2, 2, refreshed)),
        ArtworkSource::Standard,
        1,
        1,
        first,
    ));
}

#[test]
fn notification_artwork_uses_bilinear_center_crop_and_rejects_bad_buffers() {
    let mut bgra = Vec::new();
    for pixel in [
        [0, 0, 0, 255],
        [10, 20, 30, 255],
        [30, 40, 50, 255],
        [100, 100, 100, 255],
        [200, 200, 200, 255],
        [30, 40, 50, 255],
        [50, 60, 70, 255],
        [200, 200, 200, 255],
    ] {
        bgra.extend_from_slice(&pixel);
    }
    let pixels = ArtworkPixels {
        width: 4,
        height: 2,
        fingerprint: 0,
        bgra,
    };

    assert_eq!(
        scale_notification_artwork_to_size(&pixels, 1, 1),
        Some(vec![30, 40, 50, 255])
    );
    assert_eq!(
        scale_notification_artwork_to_size(&pixels, 1, 2),
        Some(vec![20, 30, 40, 255, 40, 50, 60, 255])
    );

    let solid_artwork = ArtworkPixels {
        width: 2,
        height: 2,
        fingerprint: 0,
        bgra: vec![255; 2 * 2 * 4],
    };
    let rounded = scale_notification_artwork_to_size(&solid_artwork, 32, 32).unwrap();
    let alpha_at = |x: usize, y: usize| rounded[(y * 32 + x) * 4 + 3];
    assert_eq!(alpha_at(0, 0), 0);
    assert!((1..255).contains(&alpha_at(0, 1)));
    assert_eq!(alpha_at(16, 16), 255);

    assert_eq!(scale_notification_artwork_to_size(&pixels, 0, 0), None);
    let mut malformed = pixels;
    malformed.bgra.pop();
    assert_eq!(scale_notification_artwork_to_size(&malformed, 1, 1), None);

    let one_pixel = ArtworkPixels {
        width: 1,
        height: 1,
        fingerprint: 0,
        bgra: vec![0, 0, 0, 255],
    };
    assert_eq!(
        scale_notification_artwork_to_size(&one_pixel, u32::MAX, u32::MAX),
        None,
        "overflowing target buffers must be rejected before allocation"
    );
    let overflowing_source = ArtworkPixels {
        width: u32::MAX,
        height: u32::MAX,
        fingerprint: 0,
        bgra: Vec::new(),
    };
    assert_eq!(
        scale_notification_artwork_to_size(&overflowing_source, 1, 1),
        None,
        "overflowing source buffers must be rejected before indexing"
    );
}

#[test]
fn custom_notification_artwork_creates_native_icons_at_supported_dpi_values() {
    let pixels = ArtworkPixels {
        width: 1,
        height: 1,
        fingerprint: 0,
        bgra: vec![40, 80, 120, 255],
    };
    unsafe {
        for dpi in [96, 120, 144, 168, 192, 240, 288] {
            let icon = super::notifications::create_notification_icon(&pixels, dpi)
                .unwrap_or_else(|error| panic!("failed to create icon at {dpi} DPI: {error}"));
            assert!(
                !icon.0.is_null(),
                "icon handle at {dpi} DPI must be non-null"
            );
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(icon);
        }
    }
}

#[test]
fn ui240_artwork_reuse_rejects_stale_request_generation_and_url_binding() {
    let track = TrackIdentity { display_id: 41 };
    let old = ArtworkRequest {
        identity: ArtworkRequestIdentity {
            track,
            generation: 1,
            request_id: 10,
        },
        standard_url: Some("https://cdn.example.test/a.jpg".to_owned()),
        high_quality_url: None,
    };
    let current = ArtworkRequest {
        identity: ArtworkRequestIdentity {
            track,
            generation: 2,
            request_id: 11,
        },
        standard_url: Some("https://cdn.example.test/b.jpg".to_owned()),
        high_quality_url: Some("https://cdn.example.test/b@2x.jpg".to_owned()),
    };

    assert!(artwork_request_is_current(Some(&current), current.identity));
    assert!(!artwork_request_is_current(Some(&current), old.identity));
    assert!(!artwork_request_is_current(
        Some(&current),
        ArtworkRequestIdentity {
            track,
            generation: current.identity.generation,
            request_id: old.identity.request_id,
        },
    ));
    assert!(!artwork_request_is_current(
        Some(&current),
        ArtworkRequestIdentity {
            track,
            generation: old.identity.generation,
            request_id: current.identity.request_id,
        },
    ));
    assert!(artwork_request_urls_match_track(
        current.standard_url.as_deref(),
        current.high_quality_url.as_deref(),
        Some("https://cdn.example.test/b.jpg"),
        Some("https://cdn.example.test/b@2x.jpg"),
    ));
    assert!(!artwork_request_urls_match_track(
        old.standard_url.as_deref(),
        old.high_quality_url.as_deref(),
        Some("https://cdn.example.test/b.jpg"),
        Some("https://cdn.example.test/b@2x.jpg"),
    ));

    let make_track = |standard: Option<&str>, high_quality: Option<&str>| Track {
        display_id: track.display_id,
        title: "Title".to_owned(),
        artist: "Artist".to_owned(),
        recognized_at: Instant::now(),
        recognized_at_utc: SystemTime::now(),
        recognized_date_label: String::new(),
        timestamp_source: HistoryTimestampSource::ControllerEvent,
        artwork_url: standard.map(str::to_owned),
        hq_artwork_url: high_quality.map(str::to_owned),
        background_artwork_url: None,
        track_key: None,
        config_generation: None,
        page_url: None,
    };
    let track_a = make_track(Some("https://cdn.example.test/a.jpg"), None);
    let track_b = make_track(
        Some("https://cdn.example.test/b.jpg"),
        Some("https://cdn.example.test/b@2x.jpg"),
    );
    let track_c = make_track(
        Some("https://cdn.example.test/c.jpg"),
        Some("https://cdn.example.test/c@2x.jpg"),
    );
    let request_c = ArtworkRequest {
        identity: ArtworkRequestIdentity {
            track,
            generation: 3,
            request_id: 12,
        },
        standard_url: track_c.artwork_url.clone(),
        high_quality_url: track_c.hq_artwork_url.clone(),
    };
    let mut active = HashMap::from([(track, request_c.clone())]);

    // Simulate A→B→C rebinding before any worker result reaches the UI.
    // A/B successes and their completion events must not replace or clear C.
    assert!(!artwork_request_matches_track(
        active.get(&track),
        old.identity,
        Some(&track_a)
    ));
    assert!(!artwork_request_matches_track(
        active.get(&track),
        current.identity,
        Some(&track_b)
    ));
    assert!(artwork_request_matches_track(
        active.get(&track),
        request_c.identity,
        Some(&track_c)
    ));
    assert!(!artwork_request_matches_track(
        active.get(&track),
        request_c.identity,
        None
    ));
    assert!(!artwork_request_matches_track(
        active.get(&track),
        request_c.identity,
        Some(&make_track(None, None)),
    ));

    assert!(!finish_active_artwork_request(&mut active, old.identity));
    assert!(!finish_active_artwork_request(
        &mut active,
        current.identity
    ));
    assert_eq!(active.get(&track), Some(&request_c));
    assert!(finish_active_artwork_request(
        &mut active,
        request_c.identity
    ));
    assert!(active.is_empty());
    assert!(!finish_active_artwork_request(
        &mut active,
        request_c.identity
    ));
}

#[test]
fn result_label_color_preserves_system_foreground_rgba() {
    let mapped = d2d_color_from_system(super::Color {
        A: 128,
        R: 36,
        G: 72,
        B: 144,
    });
    assert!((mapped.r - 36.0 / 255.0).abs() < 1e-6);
    assert!((mapped.g - 72.0 / 255.0).abs() < 1e-6);
    assert!((mapped.b - 144.0 / 255.0).abs() < 1e-6);
    assert!((mapped.a - 128.0 / 255.0).abs() < 1e-6);
}

#[test]
fn native_header_icons_embed_the_high_resolution_catalog_renditions() {
    assert_eq!(SETTINGS_ICON_PIXELS.len(), 32 * 32 * 4);
    assert_eq!(WHITE_CLOSE_ICON_PIXELS.len(), 24 * 24 * 4);
    assert_eq!(CLOSE_BUTTON_GA_PIXELS.len(), 24 * 24 * 2);
    assert_eq!(RESULT_CLOSE_ICON_PIXELS.len(), 24 * 24 * 4);
    let close_button_fingerprint = CLOSE_BUTTON_GA_PIXELS
        .iter()
        .fold(0xcbf29ce484222325u64, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    assert_eq!(close_button_fingerprint, 0x40e3261b8f1b5655);
    // The catalog packs these images in a bottom-origin atlas. Preserve
    // their recognizable silhouettes so another valid but wrong crop
    // (for example a social icon) cannot pass as the settings/close art.
    assert_eq!(SETTINGS_ICON_PIXELS[(16 * 32 + 16) * 4 + 3], 0);
    assert!(WHITE_CLOSE_ICON_PIXELS[(12 * 24 + 12) * 4 + 3] > 0);
    assert!(RESULT_CLOSE_ICON_PIXELS[(12 * 24 + 12) * 4 + 3] > 0);
    assert_eq!(RESULT_CLOSE_ICON_PIXELS[3], 0);
}

#[test]
fn result_panel_position_is_reproducible_at_entry_and_exit_times() {
    let entered_at = std::time::Instant::now();
    let returned_at = entered_at + std::time::Duration::from_secs(1);
    let phase = MainPhase::Matched {
        track: super::Track {
            display_id: 1,
            title: String::new(),
            artist: String::new(),
            recognized_at: entered_at,
            recognized_at_utc: UNIX_EPOCH,
            recognized_date_label: String::new(),
            timestamp_source: super::HistoryTimestampSource::HostReceivedFallback,
            artwork_url: None,
            hq_artwork_url: None,
            background_artwork_url: None,
            track_key: None,
            config_generation: None,
            page_url: None,
        },
        stage: ResultCardStage::Entering {
            started_at: entered_at,
        },
    };
    let body_top = 42.0;

    assert_eq!(
        result_panel_top_at(&phase, body_top, entered_at),
        Some(body_top - super::TOP_HEIGHT as f32)
    );
    assert_eq!(
        result_panel_top_at(
            &phase,
            body_top,
            entered_at + std::time::Duration::from_millis(300)
        ),
        Some(body_top)
    );
    let exiting = MainPhase::Matched {
        track: super::Track {
            display_id: 1,
            title: String::new(),
            artist: String::new(),
            recognized_at: entered_at,
            recognized_at_utc: UNIX_EPOCH,
            recognized_date_label: String::new(),
            timestamp_source: super::HistoryTimestampSource::HostReceivedFallback,
            artwork_url: None,
            hq_artwork_url: None,
            background_artwork_url: None,
            track_key: None,
            config_generation: None,
            page_url: None,
        },
        stage: ResultCardStage::Exiting {
            started_at: returned_at,
            from_offset: 0.0,
        },
    };
    assert_eq!(
        result_panel_top_at(&exiting, body_top, returned_at),
        Some(body_top)
    );
    assert_eq!(
        result_panel_top_at(
            &exiting,
            body_top,
            returned_at + std::time::Duration::from_millis(300)
        ),
        Some(body_top + super::TOP_HEIGHT as f32)
    );
}

#[test]
fn ui236_no_match_fallback_exit_freezes_mid_entry_position_like_composition() {
    let entered_at = Instant::now();
    let returned_at = entered_at + Duration::from_millis(75);
    let body_top = 42.0;
    let phases = [
        MainPhase::NoMatch {
            entered_at,
            returned_to_ready_at: Some(returned_at),
        },
        MainPhase::Offline {
            entered_at,
            returned_to_ready_at: Some(returned_at),
        },
    ];
    let ingress = super::result_card_progress(entered_at, returned_at);
    let from_offset = -(1.0 - ingress) * super::TOP_HEIGHT as f32;

    for phase in phases {
        let at_close = result_panel_top_at(&phase, body_top, returned_at).unwrap();
        assert!((at_close - (body_top + from_offset)).abs() < 1e-4);

        for (elapsed_ms, normalized) in [(75, 0.4085), (150, 0.8024), (225, 0.9605)] {
            let now = returned_at + Duration::from_millis(elapsed_ms);
            let expected =
                body_top + from_offset + (super::TOP_HEIGHT as f32 - from_offset) * normalized;
            let fallback = result_panel_top_at(&phase, body_top, now).unwrap();
            let composition = super::result_overlay_motion_animation(&phase, now).unwrap();
            assert!((fallback - expected).abs() < 0.15);
            assert!((fallback - (body_top + composition.from_offset)).abs() < 1e-4);
        }

        let finished =
            result_panel_top_at(&phase, body_top, returned_at + Duration::from_millis(300))
                .unwrap();
        assert!((finished - (body_top + super::TOP_HEIGHT as f32)).abs() < 1e-4);
    }
}

#[test]
fn ui234_fixed_header_clip_contains_every_result_exit_frame() {
    for body_top in [0.0, POPUP_ARROW_HEIGHT as f32] {
        for offset in [0.0, 45.0, 90.0, 135.0, 180.0] {
            assert_eq!(
                result_header_visible_height(body_top + offset, body_top),
                180.0 - offset
            );
        }
        assert_eq!(
            result_header_visible_height(body_top - TOP_HEIGHT as f32, body_top),
            0.0
        );
        assert!(!result_header_hit_test_contains(body_top - 0.01, body_top));
        assert!(result_header_hit_test_contains(body_top, body_top));
        assert!(result_header_hit_test_contains(body_top + 179.99, body_top));
        assert!(!result_header_hit_test_contains(body_top + 180.0, body_top));
    }
}

#[test]
fn ui234_composition_header_viewport_stays_fixed_across_dpi_and_arrow_edges() {
    for dpi in [96, 120, 144, 192, 240, 288] {
        for body_top in [0.0, POPUP_ARROW_HEIGHT as f32] {
            let root_snap = composition_button_pixel_snap_offset(dpi, body_top);
            let viewport = composition_header_viewport_offset(dpi, body_top);
            assert!((root_snap.X + viewport.X).abs() < 1e-6);
            assert!((root_snap.Y + viewport.Y - body_top).abs() < 1e-6);

            let history_viewport = composition_history_viewport_offset(dpi, body_top);
            assert!((root_snap.X + history_viewport.X).abs() < 1e-6);
            assert!((root_snap.Y + history_viewport.Y - body_top - TOP_HEIGHT as f32).abs() < 1e-6);

            for exit_offset in [0.0, 45.0, 90.0, 135.0, 180.0] {
                let child = result_overlay_child_offset(body_top + exit_offset, body_top);
                assert!((child.Y - exit_offset).abs() < 1e-6);
                assert_eq!(
                    result_header_visible_height(body_top + exit_offset, body_top),
                    180.0 - exit_offset
                );
            }
        }
    }
}

#[test]
fn tray_activation_uses_one_semantic_event_per_v4_selection() {
    assert!(!is_tray_activation_event(
        windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONUP as u16
    ));
    assert!(is_tray_activation_event(
        windows::Win32::UI::Shell::NIN_SELECT as u16
    ));
    assert!(is_tray_activation_event(
        (windows::Win32::UI::Shell::NIN_SELECT | windows::Win32::UI::Shell::NINF_KEY) as u16
    ));
    assert!(!is_tray_activation_event(
        windows::Win32::UI::WindowsAndMessaging::WM_RBUTTONUP as u16
    ));
}

#[test]
fn result_card_holds_ten_seconds_after_entrance_then_exits() {
    let start = std::time::Instant::now();
    let entrance_finished = start + RESULT_CARD_TRANSITION;
    let hold_timer_fired = entrance_finished + RESULT_CARD_HOLD;
    assert!(matches!(
        advance_result_card_stage(
            ResultCardStage::Entering { started_at: start },
            entrance_finished
        ),
        ResultCardAdvance::Stage(ResultCardStage::Holding { started_at }) if started_at == entrance_finished
    ));
    assert!(matches!(
        advance_result_card_stage(
            ResultCardStage::Holding { started_at: entrance_finished },
            hold_timer_fired
        ),
        ResultCardAdvance::Stage(ResultCardStage::Exiting { started_at, from_offset: 0.0 }) if started_at == hold_timer_fired
    ));
    assert!(matches!(
        advance_result_card_stage(
            ResultCardStage::Exiting {
                started_at: hold_timer_fired,
                from_offset: 0.0
            },
            hold_timer_fired + RESULT_CARD_TRANSITION
        ),
        ResultCardAdvance::Ready
    ));
}

#[test]
fn result_card_deadlines_keep_running_while_popup_is_hidden() {
    let start = Instant::now();
    let entrance_finished = start + RESULT_CARD_TRANSITION;
    let hold_finished = entrance_finished + RESULT_CARD_HOLD;
    let exit_finished = hold_finished + RESULT_CARD_TRANSITION;

    assert!(matches!(
        advance_result_card_stage(
            ResultCardStage::Entering { started_at: start },
            start + Duration::from_secs(5)
        ),
        ResultCardAdvance::Stage(ResultCardStage::Holding { started_at })
            if started_at == entrance_finished
    ));
    assert!(matches!(
        advance_result_card_stage(
            ResultCardStage::Entering { started_at: start },
            hold_finished + Duration::from_millis(150)
        ),
        ResultCardAdvance::Stage(ResultCardStage::Exiting { started_at, from_offset: 0.0 })
            if started_at == hold_finished
    ));
    assert!(matches!(
        advance_result_card_stage(
            ResultCardStage::Entering { started_at: start },
            exit_finished
        ),
        ResultCardAdvance::Ready
    ));
}

#[test]
fn history_insert_rows_slide_independently_of_result_card() {
    let started_at = std::time::Instant::now();
    let transition = Some(HistoryMutationTransition {
        started_at,
        event: HistoryMutationEvent::Insert {
            index: 0,
            previous_count: 2,
        },
    });
    assert_eq!(history_row_top(0, transition, 100.0, 0.0, started_at), 9.0);
    assert_eq!(
        history_row_top(1, transition, 100.0, 0.0, started_at),
        100.0
    );
    assert_eq!(
        history_row_top(
            0,
            transition,
            100.0,
            0.0,
            started_at + std::time::Duration::from_millis(250)
        ),
        100.0
    );
    let first_row = Some(HistoryMutationTransition {
        started_at,
        event: HistoryMutationEvent::Insert {
            index: 0,
            previous_count: 0,
        },
    });
    assert_eq!(history_row_top(0, first_row, 100.0, 0.0, started_at), 9.0);
    let deletion = Some(HistoryMutationTransition {
        started_at,
        event: HistoryMutationEvent::Delete { index: 1 },
    });
    assert_eq!(history_row_top(1, deletion, 100.0, 0.0, started_at), 282.0);
    let update = Some(HistoryMutationTransition {
        started_at,
        event: HistoryMutationEvent::Update { index: 1 },
    });
    assert_eq!(history_row_top(1, update, 100.0, 0.0, started_at), 191.0);
}

#[test]
fn first_history_insert_slides_while_the_empty_skeleton_fades() {
    let started_at = std::time::Instant::now();
    let first_row = Some(HistoryMutationTransition {
        started_at,
        event: HistoryMutationEvent::Insert {
            index: 0,
            previous_count: 0,
        },
    });

    assert_eq!(
        history_first_insert_progress(first_row, started_at),
        Some(0.0)
    );
    assert_eq!(
        history_first_insert_progress(
            first_row,
            started_at + std::time::Duration::from_millis(125)
        ),
        Some(0.5)
    );
    assert_eq!(
        history_first_insert_progress(
            first_row,
            started_at + std::time::Duration::from_millis(250)
        ),
        Some(1.0)
    );
    assert!(history_transition_requires_d2d_frame(
        first_row, started_at, true
    ));
    assert!(history_transition_requires_d2d_frame(
        first_row,
        started_at + std::time::Duration::from_millis(250),
        true
    ));
    assert!(!history_transition_requires_d2d_frame(
        first_row,
        started_at + std::time::Duration::from_millis(266),
        true
    ));

    let later_insert = Some(HistoryMutationTransition {
        started_at,
        event: HistoryMutationEvent::Insert {
            index: 0,
            previous_count: 1,
        },
    });
    assert!(!history_transition_requires_d2d_frame(
        later_insert,
        started_at,
        true
    ));
}

#[test]
fn windows_history_scroll_range_uses_content_height_not_row_count() {
    assert_eq!(super::history_content_height(0), 0.0);
    assert_eq!(super::history_max_scroll_offset(0, 430.0), 0.0);
    assert_eq!(super::history_max_scroll_offset(1, 430.0), 0.0);
    assert_eq!(super::history_max_scroll_offset(4, 430.0), 0.0);
    assert_eq!(super::history_max_scroll_offset(5, 430.0), 23.0);
    assert_eq!(super::history_max_scroll_offset(6, 430.0), 114.0);
    assert_eq!(super::history_max_scroll_offset(40, 430.0), 3_208.0);
}

#[test]
fn windows_history_wheel_respects_system_lines_page_mode_and_fractional_delta() {
    let count = 40;
    assert_eq!(
        super::history_scroll_offset_after_wheel(100.0, 120.0, 3, count, 430.0),
        46.0
    );
    assert_eq!(
        super::history_scroll_offset_after_wheel(100.0, -120.0, 1, count, 430.0),
        118.0
    );
    assert_eq!(
        super::history_scroll_offset_after_wheel(100.0, -120.0, 3, count, 430.0),
        154.0
    );
    let first_half_detent = super::history_scroll_offset_after_wheel(0.0, -30.0, 3, count, 430.0);
    let second_half_detent =
        super::history_scroll_offset_after_wheel(first_half_detent, -30.0, 3, count, 430.0);
    assert_eq!(first_half_detent, 13.5);
    assert_eq!(second_half_detent, 27.0);
    assert_eq!(
        super::history_scroll_offset_after_wheel(100.0, -120.0, u32::MAX, count, 430.0),
        530.0
    );
    assert_eq!(
        super::history_scroll_offset_after_wheel(100.0, -120.0, 0, count, 430.0),
        100.0
    );
    assert_eq!(
        super::history_scroll_offset_after_wheel(100.0, f32::NAN, 3, count, 430.0),
        100.0
    );
    assert_eq!(
        super::history_scroll_offset_after_wheel(100.0, 120.0, 3, 4, 430.0),
        0.0
    );
}

#[test]
fn windows_history_rows_hit_with_the_same_clip_spacing_and_scroll_geometry() {
    let now = Instant::now();
    assert_eq!(
        super::history_row_index_at(280.0, 200.0, 630.0, 5, None, 0.0, now),
        Some(0)
    );
    assert_eq!(
        super::history_row_index_at(289.5, 200.0, 630.0, 5, None, 0.0, now),
        None
    );
    assert_eq!(
        super::history_row_index_at(291.0, 200.0, 630.0, 5, None, 0.0, now),
        Some(1)
    );
    assert_eq!(
        super::history_row_index_at(630.0, 200.0, 630.0, 5, None, 0.0, now),
        None
    );
    assert_eq!(
        super::history_row_index_at(200.0, 200.0, 630.0, 5, None, 91.0, now),
        Some(1)
    );
}

#[test]
fn history_hover_clears_when_pointer_leaves_the_popup() {
    let mut hovered_row = Some(3);
    assert!(super::clear_history_hover(&mut hovered_row));
    assert_eq!(hovered_row, None);
    assert!(!super::clear_history_hover(&mut hovered_row));
}

#[test]
fn windows_history_insert_preserves_scrolled_content_and_only_animates_at_top() {
    assert_eq!(
        super::history_offset_after_insert(0.0, 5, 430.0),
        (0.0, true)
    );
    assert_eq!(
        super::history_offset_after_insert(23.0, 6, 430.0),
        (114.0, false)
    );
    assert_eq!(
        super::history_offset_after_insert(3_208.0, 40, 430.0),
        (3_208.0, false)
    );
}

#[test]
fn offline_markers_are_separated_from_timeout_and_no_match() {
    assert!(is_offline_failure("network is unreachable"));
    assert!(is_offline_failure("could not resolve host"));
    assert!(!is_offline_failure("request timed out"));
    assert!(!is_offline_failure("no match"));
    assert!(!is_offline_failure("unexpected parser failure"));
}

#[test]
fn outer_circle_children_keep_native_base_opacities() {
    assert_eq!(OUTER_BASE_OPACITY, [0.4, 0.6]);
}

#[test]
fn listening_layers_match_native_z_order_without_covering_the_button() {
    assert_eq!(
        composition_root_layer_order(),
        [
            CompositionRootLayer::AutoShadow,
            CompositionRootLayer::InnerRipples,
            CompositionRootLayer::Button,
            CompositionRootLayer::OuterRipples,
        ]
    );

    let levels = ui_model::UiLevels {
        bass: 1.0,
        treble: 0.0,
    };
    let max_button_diameter = BUTTON_SIZE
        * ui_model::wave_frame_at(ui_model::TaggingMode::Manual, 0.0, levels).button_child_scale;
    for step in 0..=500 {
        let elapsed = ui_model::OUTER_LOOP_DURATION * f64::from(step) / 500.0;
        let frame = ui_model::wave_frame_at(ui_model::TaggingMode::Manual, elapsed, levels);
        for (index, ring) in frame.outer.iter().enumerate() {
            assert!(
                ring.diameter - ring.stroke_width > max_button_diameter,
                "outer ring {index} overlaps the maximum button diameter at {elapsed:.4}s"
            );
        }
    }
    assert_eq!(
        composition_scene_layer_order(),
        [
            CompositionSceneLayer::Tagging,
            CompositionSceneLayer::HistoryScrollOverlay,
            CompositionSceneLayer::HistoryOverlay,
            CompositionSceneLayer::ResultOverlay,
        ]
    );
}

#[test]
fn composition_history_insert_offset_tracks_the_d2d_row_endpoints() {
    let started_at = Instant::now();
    assert_eq!(history_composition_offset(started_at, started_at), -91.0);
    assert_eq!(
        history_composition_offset(started_at, started_at + HISTORY_INSERT_TRANSITION / 2),
        -45.5
    );
    assert_eq!(
        history_composition_offset(started_at, started_at + HISTORY_INSERT_TRANSITION),
        0.0
    );
}

#[test]
fn result_fallback_suppresses_tagging_for_the_full_header_intersection() {
    let body_top = POPUP_ARROW_HEIGHT as f32;

    assert!(!result_overlay_intersects_header(None, body_top));
    assert!(!result_overlay_intersects_header(
        Some(body_top - TOP_HEIGHT as f32),
        body_top
    ));
    assert!(result_overlay_intersects_header(
        Some(body_top - TOP_HEIGHT as f32 + 0.01),
        body_top
    ));
    assert!(result_overlay_intersects_header(Some(body_top), body_top));
    assert!(result_overlay_intersects_header(
        Some(body_top + TOP_HEIGHT as f32 - 0.01),
        body_top
    ));
    assert!(!result_overlay_intersects_header(
        Some(body_top + TOP_HEIGHT as f32),
        body_top
    ));
    assert!(!result_overlay_intersects_header(Some(f32::NAN), body_top));
    assert!(!result_overlay_intersects_header(
        Some(body_top),
        f32::INFINITY
    ));
    assert!(result_overlay_intersects_header(
        Some(TOP_HEIGHT as f32 - 0.01),
        0.0
    ));
    assert!(!result_overlay_intersects_header(
        Some(TOP_HEIGHT as f32),
        0.0
    ));
}

#[test]
fn result_overlay_failure_latch_is_scoped_to_current_content_key() {
    let key = ResultOverlayContentKey {
        identity: ResultOverlayIdentity::NoMatch {
            entered_at: Instant::now(),
            offline: false,
        },
        theme_generation: 1,
        dpi: 96,
        edge: ArrowEdge::Bottom,
        language: UiLanguage::English,
    };
    let changed_dpi = ResultOverlayContentKey {
        dpi: 120,
        ..key.clone()
    };

    assert!(result_overlay_failure_matches(Some(&key), Some(&key)));
    assert!(!result_overlay_failure_matches(
        Some(&key),
        Some(&changed_dpi)
    ));
    assert!(!result_overlay_failure_matches(Some(&key), None));
    assert!(!result_overlay_failure_matches(None, Some(&key)));
}

#[test]
fn ui228_result_overlay_pixel_copy_preserves_alpha_and_clips_panel_edges() {
    let source = [
        1, 2, 3, 4, 5, 6, 7, 8, // first source row
        9, 10, 11, 12, 13, 14, 15, 16, // second source row
    ];
    let mut output = Vec::new();
    copy_panel_pixels_into(&source, 2, 2, -1, 2, 3, &mut output).unwrap();
    assert_eq!(
        output,
        [
            0, 0, 0, 0, 0, 0, 0, 0, // above the host window is transparent
            1, 2, 3, 4, 5, 6, 7, 8, // alpha and color channels are retained
            9, 10, 11, 12, 13, 14, 15, 16,
        ]
    );

    copy_panel_pixels_into(&source, 2, 2, 1, 2, 2, &mut output).unwrap();
    assert_eq!(
        output,
        [9, 10, 11, 12, 13, 14, 15, 16, 0, 0, 0, 0, 0, 0, 0, 0]
    );

    let mut extreme_offset = Vec::new();
    copy_panel_pixels_into(&source, 2, 2, i32::MAX, 2, 2, &mut extreme_offset).unwrap();
    assert_eq!(extreme_offset, [0; 16]);

    let mut unchanged = vec![0xAA];
    assert!(copy_panel_pixels_into(&source, 2, 2, 0, u32::MAX, u32::MAX, &mut unchanged).is_err());
    assert_eq!(unchanged, [0xAA]);
    assert!(copy_panel_pixels_into(&source[..15], 2, 2, 0, 2, 2, &mut unchanged).is_err());
    assert_eq!(unchanged, [0xAA]);
    assert_eq!(bgra_buffer_layout(u32::MAX, u32::MAX), None);
}

#[test]
#[ignore = "requires the Windows Composition dispatcher and graphics device; run explicitly on Windows"]
fn composition_history_overlays_capture_insert_and_scroll_surfaces() {
    use super::{
        CompositionHistoryOverlay, CompositionHistoryScrollOverlay, CompositionResultOverlay,
        HistoryCompositionKey, HistoryMutationEvent, HistoryMutationTransition,
        HistoryScrollAnimation, Interface, LayeredSurface, D2D_SIZE_U, HISTORY_HEIGHT,
        HISTORY_ROW_PITCH, TOP_HEIGHT,
    };

    unsafe {
        super::CoInitializeEx(None, super::COINIT_APARTMENTTHREADED).unwrap();
        let dispatcher = super::CreateDispatcherQueueController(super::DispatcherQueueOptions {
            dwSize: std::mem::size_of::<super::DispatcherQueueOptions>() as u32,
            threadType: super::DQTYPE_THREAD_CURRENT,
            apartmentType: super::DQTAT_COM_STA,
        })
        .unwrap();
        let instance = super::GetModuleHandleW(None).unwrap();
        let class_name = super::wide("STATIC");
        let title = super::wide("OpenShazam History Composition Test");
        let hwnd = super::CreateWindowExW(
            super::WS_EX_LAYERED,
            super::PCWSTR(class_name.as_ptr()),
            super::PCWSTR(title.as_ptr()),
            super::WS_POPUP,
            0,
            0,
            380,
            620,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .unwrap();
        let result = (|| -> windows::core::Result<()> {
            let compositor = super::Compositor::new()?;
            let result_overlay = CompositionResultOverlay::new(&compositor, 96, 10.0)?;
            let mut history = CompositionHistoryOverlay::new(
                &compositor,
                &result_overlay._graphics_device,
                96,
                10.0,
            )?;
            let mut scroll = CompositionHistoryScrollOverlay::new(&compositor, 96, 10.0)?;
            let desktop_interop: super::ICompositorDesktopInterop = compositor.cast()?;
            let target = desktop_interop.CreateDesktopWindowTarget(hwnd, true)?;
            let root = compositor.CreateContainerVisual()?;
            root.SetSize(super::Vector2 { X: 380.0, Y: 620.0 })?;
            root.Children()?.InsertAtTop(&history.viewport)?;
            root.Children()?.InsertAtTop(scroll.viewport())?;
            target.SetRoot(&root)?;

            let mut previous_frame = LayeredSurface::new(380, 620)?;
            let history_top = 10.0 + TOP_HEIGHT as f32;
            {
                let old_pixels = previous_frame.pixel_bytes_mut();
                old_pixels.fill(0);
                let old_marker = (history_top as usize * 380 + 3) * 4;
                old_pixels[old_marker..old_marker + 4].copy_from_slice(&[11, 22, 33, 255]);
            }

            let row_width = 380_usize;
            let row_height = HISTORY_ROW_PITCH as usize;
            let mut row_pixels = vec![0; row_width * row_height * 4];
            row_pixels[..4].copy_from_slice(&[44, 55, 66, 255]);
            let started_at = Instant::now();
            let key = HistoryCompositionKey {
                started_at,
                scroll_offset_bits: 0.0_f32.to_bits(),
                hovered_row: None,
                theme_generation: 0,
                edge: super::ArrowEdge::Top,
            };
            history.capture_and_animate(
                &compositor,
                &previous_frame,
                &row_pixels,
                history_top,
                10.0,
                96,
                HistoryMutationTransition {
                    started_at,
                    event: HistoryMutationEvent::Insert {
                        index: 0,
                        previous_count: 1,
                    },
                },
                key,
                started_at,
            )?;
            assert_eq!(&history.pixels[..4], &[44, 55, 66, 255]);
            let old_row_start = row_width * row_height * 4;
            assert_eq!(
                &history.pixels[old_row_start + 3 * 4..old_row_start + 4 * 4],
                &[11, 22, 33, 255]
            );
            assert!(history.matches_key(key));

            let first_started_at = Instant::now();
            let first_key = HistoryCompositionKey {
                started_at: first_started_at,
                ..key
            };
            history.capture_and_animate(
                &compositor,
                &previous_frame,
                &row_pixels,
                history_top,
                10.0,
                96,
                HistoryMutationTransition {
                    started_at: first_started_at,
                    event: HistoryMutationEvent::Insert {
                        index: 0,
                        previous_count: 0,
                    },
                },
                first_key,
                first_started_at,
            )?;
            assert_eq!(&history.pixels[..4], &[44, 55, 66, 255]);
            assert_eq!(
                &history.pixels[old_row_start + 3 * 4..old_row_start + 4 * 4],
                &[0, 0, 0, 0]
            );
            assert!(history.matches_key(first_key));
            history.resize(120)?;
            history.set_viewport(120, 10.0)?;
            assert!(history.size.width > 380);
            assert!(history.size.height > (HISTORY_HEIGHT as f32 + HISTORY_ROW_PITCH) as u32);
            history.hide()?;
            assert!(!history.matches_key(first_key));

            let scroll_size = D2D_SIZE_U {
                width: 380,
                height: (HISTORY_ROW_PITCH * 2.0) as u32,
            };
            let snapshot = vec![128; scroll_size.width as usize * scroll_size.height as usize * 4];
            let scroll_started_at = Instant::now();
            scroll.upload_and_start(
                &compositor,
                &result_overlay._graphics_device,
                &snapshot,
                scroll_size,
                scroll_size.height as f32,
                0.0,
                HistoryScrollAnimation {
                    started_at: scroll_started_at,
                    from_offset: 0.0,
                    to_offset: HISTORY_ROW_PITCH,
                },
                scroll_started_at,
            )?;
            assert!(scroll.active());
            assert!(scroll.matches(scroll_started_at));
            scroll.set_viewport(120, 10.0)?;
            scroll.hide()?;
            assert!(!scroll.active());
            Ok(())
        })();
        let _ = super::DestroyWindow(hwnd);
        drop(dispatcher);
        super::CoUninitialize();
        result.unwrap();
    }
}

#[test]
#[ignore = "requires the Windows Composition graphics device; run explicitly on Windows"]
fn composition_result_overlay_uploads_pixels_and_resizes_at_fractional_dpi() {
    use super::{
        result_card_easing, CompositionResultOverlay, Interface, LayeredSurface,
        ResultOverlayMotionAnimation, ResultOverlayMotionKey, RESULT_CARD_TRANSITION,
    };

    unsafe {
        super::CoInitializeEx(None, super::COINIT_APARTMENTTHREADED).unwrap();
        let dispatcher = super::CreateDispatcherQueueController(super::DispatcherQueueOptions {
            dwSize: std::mem::size_of::<super::DispatcherQueueOptions>() as u32,
            threadType: super::DQTYPE_THREAD_CURRENT,
            apartmentType: super::DQTAT_COM_STA,
        })
        .unwrap();
        let instance = super::GetModuleHandleW(None).unwrap();
        let class_name = super::wide("STATIC");
        let title = super::wide("OpenShazam Composition Surface Test");
        let hwnd = super::CreateWindowExW(
            super::WS_EX_LAYERED,
            super::PCWSTR(class_name.as_ptr()),
            super::PCWSTR(title.as_ptr()),
            super::WS_POPUP,
            0,
            0,
            380,
            610,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .unwrap();
        let result = (|| -> windows::core::Result<()> {
            let compositor = super::Compositor::new()?;
            let mut overlay = CompositionResultOverlay::new(&compositor, 96, 10.0)?;
            let desktop_interop: super::ICompositorDesktopInterop = compositor.cast()?;
            let target = desktop_interop.CreateDesktopWindowTarget(hwnd, true)?;
            let root = compositor.CreateContainerVisual()?;
            root.SetSize(super::Vector2 { X: 380.0, Y: 610.0 })?;
            root.Children()?.InsertAtTop(&overlay.viewport)?;
            target.SetRoot(&root)?;
            let mut source_96 = LayeredSurface::new(380, 610)?;
            let first_content_pixel_96 = 10 * 380 * 4;
            {
                let bytes_96 = source_96.pixel_bytes_mut();
                bytes_96.fill(0);
                bytes_96[first_content_pixel_96..first_content_pixel_96 + 4]
                    .copy_from_slice(&[0, 0, 0, 255]);
            }
            overlay.update_from_layered(&source_96, 10.0, 10.0, 96)?;

            source_96.pixel_bytes_mut()[first_content_pixel_96..first_content_pixel_96 + 4]
                .copy_from_slice(&[32, 64, 128, 255]);
            overlay.update_from_layered(&source_96, 10.0, 10.0, 96)?;
            source_96.present(hwnd, 255)?;

            let mut source_120 = LayeredSurface::new(475, 280)?;
            let first_content_pixel_120 = 16 * 475 * 4;
            {
                let bytes_120 = source_120.pixel_bytes_mut();
                bytes_120.fill(0);
                bytes_120[first_content_pixel_120..first_content_pixel_120 + 4]
                    .copy_from_slice(&[16, 32, 64, 255]);
            }
            overlay.update_from_layered(&source_120, 10.0, 10.0, 120)?;
            overlay.animate_position(
                &compositor,
                &ResultOverlayMotionAnimation {
                    key: ResultOverlayMotionKey::NoMatchEnter {
                        entered_at: Instant::now(),
                        offline: false,
                    },
                    from_offset: -180.0,
                    to_offset: 0.0,
                    remaining: RESULT_CARD_TRANSITION,
                    easing: result_card_easing(),
                },
                10.0,
                120,
            )?;
            Ok(())
        })();
        let _ = super::DestroyWindow(hwnd);
        drop(dispatcher);
        super::CoUninitialize();
        result.unwrap();
    }
}

#[test]
#[ignore = "requires a Windows Composition dispatcher and graphics device; run explicitly on Windows"]
fn composition_status_text_renders_transitions_and_resizes_at_fractional_dpi() {
    use super::{
        CompositionResultOverlay, CompositionStatusText, StatusTextLabel, StatusTextSnapshot,
        DWRITE_FACTORY_TYPE_SHARED,
    };

    unsafe {
        super::CoInitializeEx(None, super::COINIT_APARTMENTTHREADED).unwrap();
        let dispatcher = super::CreateDispatcherQueueController(super::DispatcherQueueOptions {
            dwSize: std::mem::size_of::<super::DispatcherQueueOptions>() as u32,
            threadType: super::DQTYPE_THREAD_CURRENT,
            apartmentType: super::DQTAT_COM_STA,
        })
        .unwrap();
        let result = (|| -> windows::core::Result<()> {
            let compositor = super::Compositor::new()?;
            let overlay = CompositionResultOverlay::new(&compositor, 96, 10.0)?;
            let write_factory: super::IDWriteFactory =
                super::DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mut status = CompositionStatusText::new(
                &compositor,
                &overlay._graphics_device,
                write_factory,
                96,
                10.0,
            )?;
            let started_at = super::Instant::now();
            status.update(
                StatusTextSnapshot {
                    current: Some(StatusTextLabel::Ready),
                    previous: None,
                    transition_started_at: None,
                },
                started_at,
                10.0,
                96,
            )?;
            assert_eq!(
                status.labels[0].text.as_deref(),
                Some(StatusTextLabel::Ready.text(status.language))
            );
            assert!(status.controllers.iter().all(Option::is_none));

            let transition_started_at = started_at + super::Duration::from_secs(3);
            status.update(
                StatusTextSnapshot {
                    current: Some(StatusTextLabel::AutoTurnOff),
                    previous: Some(StatusTextLabel::AutoOn),
                    transition_started_at: Some(transition_started_at),
                },
                transition_started_at + super::Duration::from_millis(200),
                10.0,
                96,
            )?;
            assert_eq!(
                status.labels[0].text.as_deref(),
                Some(StatusTextLabel::AutoOn.text(status.language))
            );
            assert_eq!(
                status.labels[1].text.as_deref(),
                Some(StatusTextLabel::AutoTurnOff.text(status.language))
            );
            assert!(status.controllers.iter().all(Option::is_some));

            status.update(
                StatusTextSnapshot {
                    current: Some(StatusTextLabel::AutoTurnOff),
                    previous: None,
                    transition_started_at: None,
                },
                transition_started_at + super::Duration::from_millis(500),
                10.0,
                120,
            )?;
            assert_eq!(
                status.labels[0].text.as_deref(),
                Some(StatusTextLabel::AutoTurnOff.text(status.language))
            );
            assert!(status.controllers.iter().all(Option::is_none));
            assert_eq!(status.labels[0].size.width, 475);
            assert_eq!(status.labels[0].size.height, 24);
            assert_eq!(status.dpi, 120);
            Ok(())
        })();
        drop(dispatcher);
        super::CoUninitialize();
        result.unwrap();
    }
}

#[test]
fn button_keeps_its_specified_absolute_position() {
    for body_top in [0.0, POPUP_ARROW_HEIGHT as f32] {
        let button_left = BUTTON_X as f32;
        let button_top = body_top + BUTTON_TOP as f32;
        let mask_left = button_left + BUTTON_SIZE as f32 * 0.5 - INNER_MASK_BOUNDS * 0.5;
        let mask_top = button_top + BUTTON_SIZE as f32 * 0.5 - INNER_MASK_BOUNDS * 0.5;
        let button_radius = BUTTON_SIZE as f32 * 0.5;
        assert!((mask_left + INNER_MASK_BOUNDS * 0.5 - (button_left + button_radius)).abs() < 1e-5);
        assert!((mask_top + INNER_MASK_BOUNDS * 0.5 - (button_top + button_radius)).abs() < 1e-5);
    }
}

#[test]
fn inner_ripple_clip_keeps_a_button_sized_hole_independent_of_child_rings() {
    assert_eq!(
        INNER_MASK_BOUNDS,
        ui_model::INNER_DIAMETERS[0] as f32 + 48.0
    );
    assert_eq!(
        inverse_circle_clip_radii(INNER_MASK_BOUNDS, BUTTON_CLEAR_DIAMETER),
        Some((168.0, 48.0))
    );
    assert_eq!(inverse_circle_clip_radii(96.0, 96.0), None);
    assert_eq!(inverse_circle_clip_radii(f32::NAN, 48.0), None);
}

#[test]
fn auto_shadow_display_host_stays_independent_of_its_mask_canvas() {
    assert_eq!(AUTO_SHADOW_DISPLAY_BOUNDS, BUTTON_CLEAR_DIAMETER);
    assert_eq!(AUTO_SHADOW_DISPLAY_BOUNDS, 96.0);
    assert_eq!(INNER_MASK_BOUNDS, BUTTON_CLEAR_DIAMETER * 3.5);

    let center = auto_shadow_host_center();
    assert_eq!(center.X, 48.0);
    assert_eq!(center.Y, 48.0);

    // At the native 0.85 button pulse the 336pt mask still exceeds the
    // display host; the visible edge is limited by the 96pt host bounds.
    let pulse_scale = 0.85_f32;
    let mask_outer_diameter = INNER_MASK_BOUNDS * pulse_scale;
    let mask_hole_diameter = BUTTON_CLEAR_DIAMETER * pulse_scale;
    let visible_host_diameter = AUTO_SHADOW_DISPLAY_BOUNDS;
    let visible_edge_width = (visible_host_diameter - mask_hole_diameter) * 0.5;
    assert!(mask_outer_diameter > visible_host_diameter);
    assert!((mask_hole_diameter - 81.6).abs() < 1e-4);
    assert!((visible_edge_width - 7.2).abs() < 1e-4);
}

#[test]
fn auto_shazam_omits_manual_outer_ripple_layers() {
    assert!(super::composition_outer_ripples_visible(
        ui_model::TaggingMode::Manual
    ));
    assert!(!super::composition_outer_ripples_visible(
        ui_model::TaggingMode::Auto
    ));
    assert!(!super::composition_outer_ripples_visible(
        ui_model::TaggingMode::Ready
    ));
}

#[test]
fn inner_ripple_mask_tracks_button_scale_separately_from_audio_fill() {
    for bass in [0.0, 0.2, 5.0 / 7.0, 1.0] {
        let frame = ui_model::wave_frame_at(
            ui_model::TaggingMode::Manual,
            0.0,
            ui_model::UiLevels { bass, treble: 0.5 },
        );
        let hole_diameter = BUTTON_CLEAR_DIAMETER as f64 * frame.button_child_scale;
        let fill_scale = frame.inner_container_factor / 3.0 * frame.inner_audio_scale;
        let compensated_fill_scale = fill_scale / frame.button_child_scale;
        let expected_button_scale = 0.75 + 0.35 * bass;
        let expected_fill_scale = 0.7 + 0.6 * bass;
        assert!((frame.button_child_scale - expected_button_scale).abs() < 1e-9);
        assert!((fill_scale - expected_fill_scale).abs() < 1e-9);
        assert!((hole_diameter - 96.0 * expected_button_scale).abs() < 1e-9);
        assert!((frame.button_child_scale * compensated_fill_scale - fill_scale).abs() < 1e-9);
        assert!(
            (48.0 * (expected_fill_scale - expected_button_scale) - (-2.4 + 12.0 * bass)).abs()
                < 1e-9
        );
        assert!(frame.button_child_scale > 0.0);
        assert!(fill_scale > 0.0);
    }
}

#[test]
fn logo_startup_animation_runs_once() {
    assert!(should_start_startup_logo(false));
    assert!(!should_start_startup_logo(true));
}

#[test]
fn album_art_frames_keep_native_view_sizes_without_button_padding() {
    let result = result_artwork_rect(10.0, ArrowEdge::Top);
    assert_eq!(
        (result.left, result.top, result.right, result.bottom),
        (10.0, 30.0, 160.0, 180.0)
    );
    assert_eq!(result.right - result.left, 150.0);
    assert_eq!(result.bottom - result.top, 150.0);
    assert_eq!(RESULT_ARTWORK_CORNER_RADIUS, 5.0);

    let bottom_arrow_result = result_artwork_rect(0.0, ArrowEdge::Bottom);
    assert_eq!(
        (
            bottom_arrow_result.left,
            bottom_arrow_result.top,
            bottom_arrow_result.right,
            bottom_arrow_result.bottom,
        ),
        (10.0, 10.0, 160.0, 160.0)
    );

    let history = history_artwork_frame(200.0);
    assert_eq!(
        (history.left, history.top, history.right, history.bottom),
        (20.0, 211.0, 85.0, 276.0)
    );
    assert_eq!(history.right - history.left, 65.0);
    assert_eq!(history.bottom - history.top, 65.0);
}

#[test]
fn match_fallback_uses_native_intrinsic_size_and_centering() {
    let frame = result_artwork_rect(10.0, ArrowEdge::Top);
    let image = fallback_match_artwork_rect(frame);
    assert_eq!(FALLBACK_MATCH_LOGICAL_SIDE, 131.0);
    assert_eq!(
        (image.left, image.top, image.right, image.bottom),
        (19.5, 39.5, 150.5, 170.5)
    );

    let bottom_arrow_frame = result_artwork_rect(0.0, ArrowEdge::Bottom);
    let bottom_arrow_image = fallback_match_artwork_rect(bottom_arrow_frame);
    assert_eq!(
        (
            bottom_arrow_image.left,
            bottom_arrow_image.top,
            bottom_arrow_image.right,
            bottom_arrow_image.bottom,
        ),
        (19.5, 19.5, 150.5, 150.5)
    );
}

#[test]
fn match_fallback_theme_assets_preserve_premultiplied_alpha() {
    fn assert_white_alpha_mask(pixels: &[u8], max_alpha: u8) {
        assert_eq!(pixels.len(), 262 * 262 * 4);
        for pixel in pixels.chunks_exact(4) {
            assert_eq!(pixel[0], pixel[1]);
            assert_eq!(pixel[1], pixel[2]);
            assert_eq!(pixel[2], pixel[3]);
        }
        let center = (131 * 262 + 131) * 4;
        assert_eq!(&pixels[center..center + 4], &[0, 0, 0, 0]);
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == max_alpha));
    }

    assert_white_alpha_mask(FALLBACK_MATCH_LIGHT_PIXELS, 179);
    assert_white_alpha_mask(FALLBACK_MATCH_DARK_PIXELS, 77);
    assert_ne!(FALLBACK_MATCH_LIGHT_PIXELS, FALLBACK_MATCH_DARK_PIXELS);
}

#[test]
fn bottom_arrow_is_excluded_from_the_header_gradient_union() {
    assert_eq!(
        header_fill_scope(ArrowEdge::Top),
        HeaderFillScope::HeaderAndArrow
    );
    assert_eq!(
        header_fill_scope(ArrowEdge::Bottom),
        HeaderFillScope::HeaderRectangle
    );
    assert_eq!(popup_base_color(Theme::Light), (255, 255, 255));
    assert_eq!(popup_base_color(Theme::Dark), (30, 30, 30));
}

#[test]
fn ui183_result_gradient_reverses_for_each_theme_and_result_surface() {
    let surfaces = ["header", "matched", "no-match/offline"];
    let cases = [
        (
            Theme::Light,
            [(244, 247, 249), (188, 205, 216)],
            [(188, 205, 216), (244, 247, 249)],
        ),
        (
            Theme::Dark,
            [(0, 0, 0), (55, 66, 73)],
            [(55, 66, 73), (0, 0, 0)],
        ),
    ];
    for (theme, top, bottom) in cases {
        for (edge, expected) in [(ArrowEdge::Top, top), (ArrowEdge::Bottom, bottom)] {
            for surface in surfaces {
                assert_eq!(
                    result_gradient_colors(theme, edge),
                    expected,
                    "{surface} gradient for {theme:?}/{edge:?}"
                );
            }
        }
    }
}

#[test]
fn ui184_result_artwork_keeps_native_insets_while_panel_moves_at_hidpi() {
    let dpi_sizes = [(96, 150), (120, 188), (144, 225), (192, 300)];
    for (dpi, expected_pixel_size) in dpi_sizes {
        let scale = composition_scale_for_dpi(dpi);
        for panel_top in [-180.0, -170.0, -90.0, 0.0, 10.0, 180.0, 190.0] {
            for (edge, expected_top_inset) in [(ArrowEdge::Top, 20.0), (ArrowEdge::Bottom, 10.0)] {
                let art = result_artwork_rect(panel_top, edge);
                assert_eq!(art.left, 10.0);
                assert_eq!(art.top - panel_top, expected_top_inset);
                assert_eq!(art.right, 160.0);
                assert_eq!(art.bottom - panel_top, expected_top_inset + 150.0);
                assert_eq!(art.right - art.left, 150.0);
                assert_eq!(art.bottom - art.top, 150.0);
                assert_eq!((150.0 * scale).round() as i32, expected_pixel_size);
            }
        }
    }
}

#[test]
fn result_details_keep_the_derived_164_point_text_column() {
    assert_eq!(RESULT_DETAILS_LEFT, 172.0);
    assert_eq!(RESULT_DETAILS_RIGHT - RESULT_DETAILS_LEFT, 164.0);
}

#[test]
fn result_details_stack_tracks_artwork_center_and_expands_for_wrapping() {
    let single_line = result_details_rects(10.0, 24.0, 18.0);
    assert_eq!(single_line.title.top, 81.5);
    assert_eq!(single_line.title.bottom, 105.5);
    assert_eq!(single_line.artist.top, 110.5);
    assert_eq!(single_line.artist.bottom, 128.5);
    assert_eq!(
        (single_line.title.top + single_line.artist.bottom) * 0.5,
        105.0
    );

    let wrapped = result_details_rects(10.0, 48.0, 36.0);
    assert_eq!(wrapped.title.top, 60.5);
    assert_eq!(wrapped.artist.bottom, 149.5);
    assert_eq!((wrapped.title.top + wrapped.artist.bottom) * 0.5, 105.0);
}

#[test]
fn ui235_history_text_stack_uses_measured_lines_and_stays_inside_89_dip_row() {
    let top = 120.0;
    let text = history_text_rects(top, 34.0, 28.0, 12.0);
    assert_eq!(text.title.bottom + 5.0, text.artist.top);
    assert_eq!(text.artist.bottom + 5.0, text.date.top);
    assert!(((text.title.top + text.date.bottom) * 0.5 - top - 43.5).abs() < f32::EPSILON);
    assert!(text.title.top >= top);
    assert!(text.date.bottom <= top + HISTORY_ROW_HEIGHT);

    // Result text uses measured DirectWrite line heights too. The
    // title/artist stack remains centered inside the fixed 180-DIP header.
    let result = result_details_rects(0.0, 80.0, 74.0);
    assert!(result.title.top >= 0.0);
    assert!(result.artist.bottom <= TOP_HEIGHT as f32);
}

#[test]
fn ui237_history_artwork_and_text_stack_share_the_constraint_resolved_center() {
    for row_top in [0.0, 11.0, 200.0] {
        let artwork = history_artwork_frame(row_top);
        let artwork_center_y = (artwork.top + artwork.bottom) * 0.5;
        assert_eq!(artwork.left, 20.0);
        assert_eq!(artwork.top, row_top + 11.0);
        assert_eq!(artwork.right - artwork.left, 65.0);
        assert!((artwork_center_y - (row_top + 43.5)).abs() < f32::EPSILON);

        let text = history_text_rects(row_top, 34.0, 28.0, 12.0);
        let text_center_y = (text.title.top + text.date.bottom) * 0.5;
        assert_eq!(text.title.left, artwork.right + 10.0);
        assert!((text_center_y - artwork_center_y).abs() < f32::EPSILON);
    }
}

#[test]
fn directwrite_result_and_history_layouts_keep_unicode_clusters_inside_fixed_frames() {
    let factory: super::IDWriteFactory =
        unsafe { super::DWriteCreateFactory(super::DWRITE_FACTORY_TYPE_SHARED).unwrap() };
    let long_unbroken = "OpenShazam-Unbroken-Title-".repeat(18);
    let cases = [
        (
            UiLanguage::Japanese,
            "カレンダーガール ありがとう こんにちは".to_owned(),
        ),
        (UiLanguage::English, long_unbroken),
        (
            UiLanguage::Japanese,
            "女性ボーカル👩🏻‍🎤の曲 e\u{301} と結合文字".to_owned(),
        ),
        (
            UiLanguage::English,
            "العربية עברית mixed RTL text\r\nnext line".to_owned(),
        ),
        (UiLanguage::English, String::new()),
    ];
    let mut trimmed_cases = 0usize;

    for (language, value) in cases {
        let locale = directwrite_locale(language);
        for (font_size, width, height) in [
            (20.0, 164.0, 72.0),
            (14.0, 164.0, 72.0),
            (14.0, 265.0, 30.0),
            (12.0, 265.0, 25.0),
            (10.0, 265.0, 21.0),
        ] {
            let format = create_text_format_with_locale(
                &factory,
                font_size,
                windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL,
                windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_LEADING,
                locale,
            )
            .unwrap();
            let content: Vec<u16> = value.encode_utf16().collect();
            if content.is_empty() {
                continue;
            }
            let layout = unsafe {
                factory
                    .CreateTextLayout(&content, &format, width, height)
                    .unwrap()
            };
            let mut metrics = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
            unsafe { layout.GetMetrics(&mut metrics).unwrap() };
            assert_eq!(metrics.layoutWidth, width);
            assert_eq!(metrics.layoutHeight, height);
            assert!(metrics.width.is_finite() && metrics.width >= 0.0);
            assert!(metrics.height.is_finite() && metrics.height <= height + 0.01);

            let mut line_metrics = vec![
                    windows::Win32::Graphics::DirectWrite::DWRITE_LINE_METRICS::default();
                    metrics.lineCount as usize
                ];
            let mut actual_line_count = 0;
            unsafe {
                layout
                    .GetLineMetrics(Some(&mut line_metrics), &mut actual_line_count)
                    .unwrap()
            };
            assert_eq!(actual_line_count, metrics.lineCount);
            for line in line_metrics.iter().take(actual_line_count as usize) {
                assert!(line.height.is_finite() && line.height >= 0.0);
                assert!(line.baseline.is_finite() && line.baseline >= 0.0);
            }
            let measured = super::measure_directwrite_text_layout(
                &factory,
                &value,
                &format,
                width,
                0.0,
                if width == 164.0 {
                    4.0
                } else if font_size == 10.0 {
                    1.0
                } else {
                    2.0
                },
            )
            .unwrap();
            assert!(measured.height.is_finite() && measured.height >= 0.0);
            assert!(measured.first_baseline.is_finite());
            assert!(measured.last_baseline.is_finite());
            assert!(measured.last_baseline <= measured.height + 0.01);
            assert!(measured.overhang_left.is_finite());
            assert!(measured.overhang_top.is_finite());
            assert!(measured.overhang_right.is_finite());
            assert!(measured.overhang_bottom.is_finite());
            if line_metrics
                .iter()
                .take(actual_line_count as usize)
                .any(|line| line.isTrimmed.as_bool())
            {
                trimmed_cases += 1;
            }

            let mut clusters = vec![
                    windows::Win32::Graphics::DirectWrite::DWRITE_CLUSTER_METRICS::default();
                    content.len()
                ];
            let mut actual_cluster_count = 0;
            unsafe {
                layout
                    .GetClusterMetrics(Some(&mut clusters), &mut actual_cluster_count)
                    .unwrap()
            };
            let mut utf16_end = 0usize;
            let mut cluster_boundaries = Vec::with_capacity(actual_cluster_count as usize);
            for cluster in clusters.iter().take(actual_cluster_count as usize) {
                utf16_end += usize::from(cluster.length);
                assert!(
                    String::from_utf16(&content[..utf16_end]).is_ok(),
                    "DirectWrite split a UTF-16 surrogate pair in {value:?}"
                );
                cluster_boundaries.push(utf16_end);
            }
            assert_eq!(utf16_end, content.len(), "text clusters lost UTF-16 units");
            let combining_sequence: Vec<u16> = "e\u{301}".encode_utf16().collect();
            if let Some(start) = content
                .windows(combining_sequence.len())
                .position(|window| window == combining_sequence)
            {
                assert!(
                    !cluster_boundaries.contains(&(start + 1)),
                    "DirectWrite split a base character from its combining mark"
                );
            }
            let emoji_sequence: Vec<u16> = "👩🏻‍🎤".encode_utf16().collect();
            if let Some(start) = content
                .windows(emoji_sequence.len())
                .position(|window| window == emoji_sequence)
            {
                for boundary in start + 1..start + emoji_sequence.len() {
                    assert!(
                        !cluster_boundaries.contains(&boundary),
                        "DirectWrite split an emoji ZWJ grapheme cluster"
                    );
                }
            }
        }
    }
    assert!(trimmed_cases > 0, "matrix did not exercise text trimming");
}

#[test]
fn gradient_origin_translation_preserves_dip_scale_and_tracks_panel_top() {
    for top in [0.0, 10.0, 27.5, 180.0] {
        let transform = gradient_origin_transform(top);
        assert_eq!(transform.M11, 1.0);
        assert_eq!(transform.M12, 0.0);
        assert_eq!(transform.M21, 0.0);
        assert_eq!(transform.M22, 1.0);
        assert_eq!(transform.M31, 0.0);
        assert_eq!(transform.M32, top);
    }
}

#[test]
fn startup_logo_keeps_the_ready_timer_until_idle_pulse_can_start() {
    let ready = MainPhase::Ready;
    assert!(!animation_needs_frame(&ready, false));
    assert!(animation_needs_frame(&ready, true));

    let listening = MainPhase::Listening {
        mode: CaptureMode::Manual,
        started_at: std::time::Instant::now(),
    };
    assert!(!animation_needs_frame(&listening, false));

    let auto_started = Instant::now();
    let auto_listening = MainPhase::Listening {
        mode: CaptureMode::Auto,
        started_at: auto_started,
    };
    assert!(phase_needs_frame_at(&auto_listening, auto_started));
    assert!(phase_needs_frame_at(
        &auto_listening,
        auto_started + Duration::from_millis(199)
    ));
    assert!(!phase_needs_frame_at(
        &auto_listening,
        auto_started + Duration::from_millis(200)
    ));
}

#[test]
fn popup_visibility_does_not_schedule_opacity_animation_frames() {
    assert!(!animation_timer_needed(false, true));
    assert!(!animation_timer_needed(false, false));
    assert!(animation_timer_needed(true, true));
    assert!(!animation_timer_needed(true, false));
}

#[test]
fn auto_wave_and_button_spring_start_before_capture_and_keep_the_same_clock() {
    let auto_started = Instant::now();
    let capture_event = auto_started + Duration::from_millis(120);
    let starting = MainPhase::Starting {
        mode: CaptureMode::Auto,
        entered_at: auto_started,
    };

    assert_eq!(
        phase_animation_state(&starting),
        (ui_model::TaggingMode::Auto, true, true)
    );
    assert_eq!(
        status_text_snapshot(&starting, None).current,
        Some(super::StatusTextLabel::AutoOn)
    );
    assert!(phase_needs_frame_at(&starting, auto_started));
    assert!(phase_needs_frame_at(
        &starting,
        auto_started + Duration::from_millis(199)
    ));
    assert!(!phase_needs_frame_at(
        &starting,
        auto_started + AUTO_START_SPRING_DURATION
    ));
    assert_eq!(
        capture_animation_start(&starting, CaptureMode::Auto, capture_event),
        auto_started,
        "CaptureStarted must not restart the native Auto pulse"
    );

    let manual_starting = MainPhase::Starting {
        mode: CaptureMode::Manual,
        entered_at: auto_started,
    };
    assert_eq!(
        phase_animation_state(&manual_starting),
        (ui_model::TaggingMode::Ready, true, false)
    );
    assert_eq!(status_text_snapshot(&manual_starting, None).current, None);
    assert_eq!(
        capture_animation_start(&manual_starting, CaptureMode::Manual, capture_event),
        capture_event
    );
}

#[test]
fn auto_capture_can_be_cancelled_during_start_listen_and_cancel_phases() {
    let now = Instant::now();
    for phase in [
        MainPhase::Starting {
            mode: CaptureMode::Auto,
            entered_at: now,
        },
        MainPhase::Listening {
            mode: CaptureMode::Auto,
            started_at: now,
        },
        MainPhase::Cancelling {
            mode: CaptureMode::Auto,
            entered_at: now,
        },
    ] {
        assert_eq!(capture_mode_in_progress(&phase), Some(CaptureMode::Auto));
    }

    let manual_starting = MainPhase::Starting {
        mode: CaptureMode::Manual,
        entered_at: now,
    };
    assert_eq!(
        capture_mode_in_progress(&manual_starting),
        Some(CaptureMode::Manual)
    );
    assert_eq!(capture_mode_in_progress(&MainPhase::Ready), None);
}

#[test]
fn status_text_push_uses_composition_and_keeps_d2d_fallback_deterministic() {
    let start = std::time::Instant::now();
    let ready = MainPhase::Ready;
    assert!(!super::animation_needs_d2d_redraw(
        &ready, false, false, false
    ));
    assert!(super::animation_needs_d2d_redraw(
        &ready, true, false, false
    ));
    assert!(!super::animation_needs_d2d_redraw(
        &ready, true, true, false
    ));
    assert_eq!(super::status_text_progress(start, start), 0.0);
    assert_eq!(super::status_text_linear_progress(start, start), 0.0);
    assert!(
        (super::status_text_progress(start, start + std::time::Duration::from_millis(200)) - 0.5)
            .abs()
            < 0.001
    );
    assert_eq!(
        super::status_text_linear_progress(start, start + std::time::Duration::from_millis(200)),
        0.5
    );
    assert_eq!(
        super::status_text_linear_progress(start, start + std::time::Duration::from_millis(400)),
        1.0
    );
    assert_eq!(
        super::status_text_progress(start, start + std::time::Duration::from_millis(400)),
        1.0
    );
    assert_eq!(
        super::status_text_progress(start, start - std::time::Duration::from_millis(1)),
        0.0
    );
}

#[test]
fn composition_result_overlay_suppresses_redundant_d2d_animation_redraws() {
    let now = Instant::now();
    let matched = MainPhase::Matched {
        track: Track {
            display_id: 1,
            title: String::new(),
            artist: String::new(),
            recognized_at: now,
            recognized_at_utc: SystemTime::UNIX_EPOCH,
            recognized_date_label: String::new(),
            timestamp_source: HistoryTimestampSource::ControllerEvent,
            artwork_url: None,
            hq_artwork_url: None,
            background_artwork_url: None,
            track_key: None,
            config_generation: None,
            page_url: None,
        },
        stage: ResultCardStage::Entering { started_at: now },
    };
    let no_match = MainPhase::NoMatch {
        entered_at: now,
        returned_to_ready_at: None,
    };

    assert!(super::animation_needs_d2d_redraw(
        &matched, false, true, false
    ));
    assert!(!super::animation_needs_d2d_redraw(
        &matched, false, true, true
    ));
    assert!(super::animation_needs_d2d_redraw(
        &no_match, false, true, false
    ));
    assert!(!super::animation_needs_d2d_redraw(
        &no_match, false, true, true
    ));
    // Composition failure must keep the D2D fallback repaint active.
    assert!(super::animation_needs_d2d_redraw(
        &matched, false, false, false
    ));
}

#[test]
fn composition_overlay_readiness_is_bound_to_current_phase_and_artwork() {
    let now = Instant::now();
    let track = Track {
        display_id: 5,
        title: "Title".to_owned(),
        artist: "Artist".to_owned(),
        recognized_at: now,
        recognized_at_utc: SystemTime::UNIX_EPOCH,
        recognized_date_label: String::new(),
        timestamp_source: HistoryTimestampSource::ControllerEvent,
        artwork_url: Some("https://cdn.example.test/cover.jpg".to_owned()),
        hq_artwork_url: None,
        background_artwork_url: None,
        track_key: None,
        config_generation: None,
        page_url: None,
    };
    let phase = MainPhase::Matched {
        track: track.clone(),
        stage: ResultCardStage::Holding { started_at: now },
    };
    let fingerprint = (ArtworkSource::Standard, 1024, 0xAABB_CCDD);
    let identity = super::ResultOverlayIdentity::Matched {
        display_id: track.display_id,
        title: track.title.clone(),
        artist: track.artist.clone(),
        artwork_url: track.artwork_url.clone(),
        hq_artwork_url: track.hq_artwork_url.clone(),
        artwork: Some(fingerprint),
    };
    assert!(super::result_overlay_identity_matches_phase(
        &identity,
        &phase,
        Some(fingerprint),
    ));
    assert!(!super::result_overlay_identity_matches_phase(
        &identity,
        &phase,
        Some((ArtworkSource::Standard, 1024, 0x1122_3344)),
    ));

    let no_match = MainPhase::NoMatch {
        entered_at: now,
        returned_to_ready_at: None,
    };
    let no_match_identity = super::ResultOverlayIdentity::NoMatch {
        entered_at: now,
        offline: false,
    };
    assert!(super::result_overlay_identity_matches_phase(
        &no_match_identity,
        &no_match,
        None,
    ));
    let offline = MainPhase::Offline {
        entered_at: now,
        returned_to_ready_at: None,
    };
    assert!(!super::result_overlay_identity_matches_phase(
        &no_match_identity,
        &offline,
        None,
    ));
}

#[test]
fn no_match_keeps_high_resolution_ticks_through_both_position_springs() {
    let now = std::time::Instant::now();
    let transition = MainPhase::NoMatch {
        entered_at: now - std::time::Duration::from_millis(200),
        returned_to_ready_at: None,
    };
    assert!(animation_needs_frame(&transition, false));
    let composition_only = MainPhase::NoMatch {
        entered_at: now - std::time::Duration::from_millis(900),
        returned_to_ready_at: None,
    };
    assert!(!animation_needs_frame(&composition_only, false));
}

#[test]
#[ignore = "requires a Windows Composition dispatcher; run explicitly to verify native property support"]
fn composition_animates_ellipse_radius_and_sprite_stroke_without_a_window() {
    use super::*;

    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).unwrap();
        let dispatcher = CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })
        .unwrap();
        let result = (|| -> windows::core::Result<()> {
            let compositor = Compositor::new()?;
            let root = compositor.CreateContainerVisual()?;
            root.SetBorderMode(CompositionBorderMode::Soft)?;
            assert_eq!(root.BorderMode()?, CompositionBorderMode::Soft);

            let geometry: CompositionEllipseGeometry = compositor.CreateEllipseGeometry()?;
            let radius = compositor.CreateVector2KeyFrameAnimation()?;
            radius.SetDuration(TimeSpan {
                Duration: 10_000_000,
            })?;
            radius.InsertKeyFrame(0.0, Vector2 { X: 48.0, Y: 48.0 })?;
            radius.InsertKeyFrame(1.0, Vector2 { X: 64.0, Y: 64.0 })?;
            geometry.StartAnimation(&HSTRING::from("Radius"), &radius)?;

            let brush =
                compositor.CreateColorBrushWithColor(composition_color(255, 255, 255, 255))?;
            let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
            shape.SetStrokeBrush(&brush)?;
            let stroke = compositor.CreateScalarKeyFrameAnimation()?;
            stroke.SetDuration(TimeSpan {
                Duration: 10_000_000,
            })?;
            stroke.InsertKeyFrame(0.0, 0.0)?;
            stroke.InsertKeyFrame(1.0, 4.0)?;
            shape.StartAnimation(&HSTRING::from("StrokeThickness"), &stroke)?;
            // Stop clocked animations before tearing down the dispatcher.
            // Leaving them active can race COM/Composition cleanup when
            // another native Composition test starts in the same process.
            shape.StopAnimation(&HSTRING::from("StrokeThickness"))?;
            geometry.StopAnimation(&HSTRING::from("Radius"))?;
            Ok(())
        })();
        drop(dispatcher);
        CoUninitialize();
        result.unwrap();
    }
}

#[test]
#[ignore = "requires a Windows Composition dispatcher; run explicitly to verify NoMatch path interop"]
fn composition_builds_the_native_no_match_icon_path() {
    use super::*;

    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).unwrap();
        let dispatcher = CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })
        .unwrap();
        let result = (|| -> windows::core::Result<()> {
            let compositor = Compositor::new()?;
            let d2d_factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let geometry = make_no_match_icon_geometry(
                &compositor,
                &d2d_factory,
                no_match_icon_rect(BUTTON_SIZE as f32),
            )?;
            let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
            let visual = compositor.CreateShapeVisual()?;
            visual.Shapes()?.Append(&shape)?;
            Ok(())
        })();
        drop(dispatcher);
        CoUninitialize();
        result.unwrap();
    }
}

#[test]
#[ignore = "requires a Windows Composition dispatcher; run explicitly to verify group clip interop"]
fn composition_inner_ripple_group_accepts_the_inverse_circle_clip() {
    use super::*;

    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).unwrap();
        let dispatcher = CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })
        .unwrap();
        let result = (|| -> windows::core::Result<()> {
            let compositor = Compositor::new()?;
            let d2d_factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let clip = make_inverse_circle_clip(
                &compositor,
                &d2d_factory,
                INNER_MASK_BOUNDS,
                BUTTON_CLEAR_DIAMETER,
            )?;
            assert_eq!(
                inverse_circle_clip_radii(INNER_MASK_BOUNDS, BUTTON_CLEAR_DIAMETER),
                Some((168.0, 48.0))
            );
            let group = compositor.CreateContainerVisual()?;
            group.SetSize(Vector2 {
                X: INNER_MASK_BOUNDS,
                Y: INNER_MASK_BOUNDS,
            })?;
            group.SetClip(&clip)?;
            clip.SetScale(Vector2 { X: 0.75, Y: 0.75 })?;
            assert_eq!(clip.Scale()?, Vector2 { X: 0.75, Y: 0.75 });
            let ring = compositor.CreateSpriteVisual()?;
            ring.SetSize(Vector2 {
                X: ui_model::INNER_DIAMETERS[0] as f32,
                Y: ui_model::INNER_DIAMETERS[0] as f32,
            })?;
            group.Children()?.InsertAtTop(&ring)?;
            let mut logo = CompositionLogo::new(&d2d_factory, &compositor, Theme::Light, true)?;
            logo.start()?;
            Ok(())
        })();
        drop(dispatcher);
        CoUninitialize();
        result.unwrap();
    }
}

#[test]
#[ignore = "requires a Windows Composition dispatcher; run explicitly to verify AutoShadow layer and mask interop"]
fn composition_auto_shadow_uses_button_viewport_for_336pt_mask() {
    use super::*;

    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).unwrap();
        let dispatcher = CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })
        .unwrap();
        let result = (|| -> windows::core::Result<()> {
            let compositor = Compositor::new()?;
            let d2d_factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let clip = make_inverse_circle_clip_at(
                &compositor,
                &d2d_factory,
                INNER_MASK_BOUNDS,
                BUTTON_CLEAR_DIAMETER,
                auto_shadow_host_center(),
            )?;
            clip.SetCenterPoint(auto_shadow_host_center())?;
            clip.SetScale(Vector2 { X: 0.85, Y: 0.85 })?;

            let host = compositor.CreateContainerVisual()?;
            host.SetSize(Vector2 {
                X: AUTO_SHADOW_DISPLAY_BOUNDS,
                Y: AUTO_SHADOW_DISPLAY_BOUNDS,
            })?;
            host.SetBorderMode(CompositionBorderMode::Soft)?;
            host.SetCenterPoint(Vector3 {
                X: AUTO_SHADOW_DISPLAY_BOUNDS * 0.5,
                Y: AUTO_SHADOW_DISPLAY_BOUNDS * 0.5,
                Z: 0.0,
            })?;
            host.SetClip(&clip)?;

            let color_brush =
                compositor.CreateColorBrushWithColor(composition_color(255, 9, 9, 9))?;
            let (outline_source, outline_surface, outline_mask) =
                make_auto_shadow_outline_mask(&compositor, AUTO_SHADOW_DISPLAY_BOUNDS, 2.0)?;
            let shadow = compositor.CreateDropShadow()?;
            shadow.SetBlurRadius(3.0)?;
            shadow.SetOffset(Vector3::default())?;
            shadow.SetOpacity(0.5)?;
            shadow.SetColor(composition_color(255, 36, 36, 36))?;
            shadow.SetSourcePolicy(
                windows::UI::Composition::CompositionDropShadowSourcePolicy::Default,
            )?;
            let shadow_brush =
                compositor.CreateColorBrushWithColor(composition_color(0, 0, 0, 0))?;
            let layers = make_auto_shadow_circle_visuals(
                &compositor,
                AUTO_SHADOW_DISPLAY_BOUNDS,
                &color_brush,
                &shadow_brush,
                &shadow,
                &outline_mask,
            )?;
            host.Children()?.InsertAtTop(&layers.clipped_layer)?;

            assert_eq!(host.Size()?, Vector2 { X: 96.0, Y: 96.0 });
            assert_eq!(layers.clipped_layer.Size()?, Vector2 { X: 96.0, Y: 96.0 });
            assert_eq!(
                layers.clipped_layer.BorderMode()?,
                CompositionBorderMode::Soft
            );
            assert_eq!(layers.background.Size()?, Vector2 { X: 96.0, Y: 96.0 });
            assert_eq!(layers.shadow.Size()?, Vector2 { X: 96.0, Y: 96.0 });
            assert_eq!(layers.clipped_layer.Children()?.Count()?, 2);
            let circle_clip: CompositionGeometricClip = layers.clipped_layer.Clip()?.cast()?;
            let circle: CompositionEllipseGeometry = circle_clip.Geometry()?.cast()?;
            assert_eq!(
                circle.Center()?,
                Vector2 {
                    X: AUTO_SHADOW_DISPLAY_BOUNDS * 0.5,
                    Y: AUTO_SHADOW_DISPLAY_BOUNDS * 0.5,
                }
            );
            assert_eq!(
                circle.Radius()?,
                Vector2 {
                    X: AUTO_SHADOW_DISPLAY_BOUNDS * 0.5,
                    Y: AUTO_SHADOW_DISPLAY_BOUNDS * 0.5,
                }
            );
            let applied_shadow = layers.shadow.Shadow()?.cast::<DropShadow>()?;
            assert_eq!(applied_shadow.BlurRadius()?, 3.0);
            assert_eq!(
                applied_shadow.SourcePolicy()?,
                windows::UI::Composition::CompositionDropShadowSourcePolicy::Default
            );
            let applied_mask: CompositionSurfaceBrush = applied_shadow.Mask()?.cast()?;
            assert_eq!(applied_mask.Stretch()?, CompositionStretch::Fill);
            assert_eq!(outline_source.Size()?, Vector2 { X: 96.0, Y: 96.0 });
            assert_eq!(outline_surface.SourceSize()?, Vector2 { X: 96.0, Y: 96.0 });
            assert_eq!(clip.CenterPoint()?, auto_shadow_host_center());
            assert_eq!(clip.Scale()?, Vector2 { X: 0.85, Y: 0.85 });
            drop(color_brush);
            Ok(())
        })();
        drop(dispatcher);
        CoUninitialize();
        result.unwrap();
    }
}
