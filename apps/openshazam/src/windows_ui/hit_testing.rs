#![deny(unsafe_op_in_unsafe_fn)]

use super::{
    history_row_top, native_icon_layout, resolve_track_page_url, result_header_hit_test_contains,
    result_panel_top_at, AppState, ArrowEdge, HistoryMutationTransition, MainPhase,
    ResultCardStage, CONTENT_WIDTH, HISTORY_HEIGHT, HISTORY_ROW_HEIGHT, POPUP_ARROW_HEIGHT,
    TOP_HEIGHT,
};
use crate::ui_model::{BUTTON_SIZE, BUTTON_TOP, BUTTON_X};
use std::time::Instant;
use windows::Win32::{
    Foundation::POINT,
    Graphics::Gdi::ScreenToClient,
    UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::GetCursorPos},
};

pub(super) fn point_in_rect(point_x: f32, point_y: f32, bounds: [f32; 4]) -> bool {
    (bounds[0]..=bounds[2]).contains(&point_x) && (bounds[1]..=bounds[3]).contains(&point_y)
}

pub(super) fn result_card_link_hit(x: f32, y: f32, card_top: f32, close: [f32; 4]) -> bool {
    point_in_rect(
        x,
        y,
        [
            0.0,
            card_top,
            CONTENT_WIDTH as f32,
            card_top + TOP_HEIGHT as f32,
        ],
    ) && !point_in_rect(x, y, close)
}

pub(super) fn visible_history_row_hit(
    y: f32,
    row_top: f32,
    history_top: f32,
    history_bottom: f32,
) -> bool {
    let visible_top = row_top.max(history_top);
    let visible_bottom = (row_top + HISTORY_ROW_HEIGHT).min(history_bottom);
    visible_top < visible_bottom && visible_top <= y && y < visible_bottom
}

pub(super) fn main_hit_test(app: &AppState, x: f32, y: f32, body_top: f32) -> MainHit {
    let result_accepts_input = result_header_hit_test_contains(y, body_top)
        && match &app.phase {
            MainPhase::Matched { stage, .. } => !matches!(stage, ResultCardStage::Exiting { .. }),
            MainPhase::NoMatch {
                returned_to_ready_at,
                ..
            }
            | MainPhase::Offline {
                returned_to_ready_at,
                ..
            } => returned_to_ready_at.is_none(),
            _ => false,
        };
    if result_accepts_input {
        let top = result_panel_top_at(&app.phase, body_top, Instant::now()).unwrap_or(body_top);
        let offset = top - body_top;
        let close = native_icon_layout(body_top, app.arrow_edge).result_close_hit;
        let local_y = y - offset;
        if point_in_rect(x, local_y, close) {
            return MainHit::CloseResult;
        }
        if let MainPhase::Matched { track, .. } = &app.phase {
            if resolve_track_page_url(track, &app.ui_configs).is_some()
                && result_card_link_hit(x, local_y, body_top, close)
            {
                return MainHit::OpenTrack(track.display_id);
            }
        }
        if matches!(
            app.phase,
            MainPhase::NoMatch { .. } | MainPhase::Offline { .. }
        ) && (90.0..=290.0).contains(&x)
            && (top + 115.0..=top + 155.0).contains(&y)
        {
            return MainHit::TryAgain;
        }
    }
    if let MainPhase::Timeout { .. } = app.phase {
        if (70.0..=190.0).contains(&x) && (body_top + 126.0..=body_top + 160.0).contains(&y) {
            return MainHit::TimeoutRestart;
        }
        if (200.0..=310.0).contains(&x) && (body_top + 126.0..=body_top + 160.0).contains(&y) {
            return MainHit::TimeoutDismiss;
        }
        return MainHit::Other;
    }
    let history_top = body_top + TOP_HEIGHT as f32;
    let now = Instant::now();
    for (index, track) in app.history.iter().enumerate() {
        if resolve_track_page_url(track, &app.ui_configs).is_none() {
            continue;
        }
        let row_top = history_row_top(
            index,
            app.history_transition,
            history_top,
            app.scroll_offset,
            now,
        );
        let history_bottom = history_top + HISTORY_HEIGHT as f32;
        if (0.0..=CONTENT_WIDTH as f32).contains(&x)
            && visible_history_row_hit(y, row_top, history_top, history_bottom)
        {
            return MainHit::OpenTrack(track.display_id);
        }
    }
    let icons = native_icon_layout(body_top, app.arrow_edge);
    if matches!(
        app.phase,
        MainPhase::Listening { .. } | MainPhase::Starting { .. }
    ) && point_in_rect(x, y, icons.cancel_hit)
    {
        return MainHit::Cancel;
    }
    if point_in_rect(x, y, icons.settings_hit)
        && matches!(app.phase, MainPhase::Ready | MainPhase::Error { .. })
    {
        return MainHit::Settings;
    }
    let button_x = BUTTON_X as f32..=(BUTTON_X + BUTTON_SIZE) as f32;
    let button_y = body_top + BUTTON_TOP as f32..=body_top + (BUTTON_TOP + BUTTON_SIZE) as f32;
    if button_x.contains(&x)
        && button_y.contains(&y)
        && app.recognition_session.is_none()
        && !matches!(
            app.phase,
            MainPhase::Matched { .. } | MainPhase::NoMatch { .. } | MainPhase::Offline { .. }
        )
    {
        return MainHit::CenterButton;
    }
    MainHit::Other
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MainHit {
    CenterButton,
    Settings,
    Cancel,
    TimeoutRestart,
    TimeoutDismiss,
    TryAgain,
    CloseResult,
    OpenTrack(u64),
    Other,
}

pub(super) fn history_row_index_at(
    y: f32,
    history_top: f32,
    history_bottom: f32,
    item_count: usize,
    transition: Option<HistoryMutationTransition>,
    scroll_offset: f32,
    now: Instant,
) -> Option<usize> {
    if y < history_top || y >= history_bottom {
        return None;
    }
    (0..item_count).find(|&index| {
        let top = history_row_top(index, transition, history_top, scroll_offset, now);
        visible_history_row_hit(y, top, history_top, history_bottom)
    })
}

pub(super) fn clear_history_hover(hovered_row: &mut Option<usize>) -> bool {
    hovered_row.take().is_some()
}

#[deny(unsafe_op_in_unsafe_fn)]
/// # Safety
/// Must run on the UI thread while `app` owns its live main window.
pub(super) unsafe fn hovered_history_row_for_cursor(app: &AppState) -> Option<usize> {
    if !app.visible {
        return None;
    }
    let mut cursor = POINT::default();
    // SAFETY: `cursor` is writable and the caller's live AppState owns the
    // main HWND for this synchronous hit test.
    unsafe { GetCursorPos(&mut cursor) }.ok()?;
    // SAFETY: The cursor point and main client HWND are valid Win32 inputs.
    if !unsafe { ScreenToClient(app.main_hwnd, &mut cursor) }.as_bool() {
        return None;
    }
    // SAFETY: The main HWND remains live for the duration of this callback.
    let scale = 96.0 / unsafe { GetDpiForWindow(app.main_hwnd) }.max(96) as f32;
    let x = cursor.x as f32 * scale;
    if !(0.0..CONTENT_WIDTH as f32).contains(&x) {
        return None;
    }
    let y = cursor.y as f32 * scale;
    let body_top = if app.arrow_edge == ArrowEdge::Top {
        POPUP_ARROW_HEIGHT as f32
    } else {
        0.0
    };
    let history_top = body_top + TOP_HEIGHT as f32;
    history_row_index_at(
        y,
        history_top,
        history_top + HISTORY_HEIGHT as f32,
        app.history.len(),
        app.history_transition,
        app.scroll_offset,
        Instant::now(),
    )
}
