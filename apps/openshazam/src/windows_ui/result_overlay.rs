//! Result card identity, transition state, and fallback drawing decisions.

use super::{
    artwork_state::ArtworkSource, localization::UiLanguage, motion_timing::result_card_progress,
    ArrowEdge, MainPhase, TOP_HEIGHT,
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub(super) enum ResultCardStage {
    Entering {
        started_at: Instant,
    },
    Holding {
        started_at: Instant,
    },
    Exiting {
        started_at: Instant,
        from_offset: f32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ResultOverlayIdentity {
    Matched {
        display_id: u64,
        title: String,
        artist: String,
        artwork_url: Option<String>,
        hq_artwork_url: Option<String>,
        artwork: Option<(ArtworkSource, usize, u128)>,
    },
    NoMatch {
        entered_at: Instant,
        offline: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ResultOverlayContentKey {
    pub(super) identity: ResultOverlayIdentity,
    pub(super) theme_generation: u64,
    pub(super) dpi: u32,
    pub(super) edge: ArrowEdge,
    pub(super) language: UiLanguage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ResultOverlayMotionKey {
    MatchEnter {
        display_id: u64,
        started_at: Instant,
    },
    MatchExit {
        display_id: u64,
        started_at: Instant,
        from_offset_bits: u32,
    },
    NoMatchEnter {
        entered_at: Instant,
        offline: bool,
    },
    NoMatchExit {
        entered_at: Instant,
        returned_at: Instant,
        offline: bool,
        from_offset_bits: u32,
    },
}

#[derive(Clone, Debug)]
pub(super) struct ResultOverlayMotionAnimation {
    pub(super) key: ResultOverlayMotionKey,
    pub(super) from_offset: f32,
    pub(super) to_offset: f32,
    pub(super) remaining: Duration,
    pub(super) easing: super::CubicBezier,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum ResultCardAdvance {
    Unchanged,
    Stage(ResultCardStage),
    Ready,
}

pub(super) const RESULT_CARD_TRANSITION: Duration = Duration::from_millis(300);
pub(super) const RESULT_CARD_HOLD: Duration = Duration::from_secs(10);
pub(super) const SUCCESS_SETTLE_TOTAL: Duration = Duration::from_secs(1);

pub(super) fn result_panel_draw_top(
    panel_top: Option<f32>,
    composition_cache_ready: bool,
) -> Option<f32> {
    panel_top.and_then(|top| (!composition_cache_ready).then_some(top))
}

pub(super) fn result_overlay_failure_matches(
    failure_key: Option<&ResultOverlayContentKey>,
    current_key: Option<&ResultOverlayContentKey>,
) -> bool {
    matches!((failure_key, current_key), (Some(failure), Some(current)) if failure == current)
}

pub(super) fn result_overlay_identity_matches_phase(
    identity: &ResultOverlayIdentity,
    phase: &MainPhase,
    current_artwork: Option<(ArtworkSource, usize, u128)>,
) -> bool {
    match (identity, phase) {
        (
            ResultOverlayIdentity::Matched {
                display_id,
                title,
                artist,
                artwork_url,
                hq_artwork_url,
                artwork,
            },
            MainPhase::Matched { track, .. },
        ) => {
            track.display_id == *display_id
                && track.title.as_str() == title
                && track.artist.as_str() == artist
                && track.artwork_url.as_deref() == artwork_url.as_deref()
                && track.hq_artwork_url.as_deref() == hq_artwork_url.as_deref()
                && current_artwork == *artwork
        }
        (
            ResultOverlayIdentity::NoMatch {
                entered_at,
                offline,
            },
            MainPhase::NoMatch {
                entered_at: current_entered_at,
                ..
            },
        ) => !offline && entered_at == current_entered_at,
        (
            ResultOverlayIdentity::NoMatch {
                entered_at,
                offline,
            },
            MainPhase::Offline {
                entered_at: current_entered_at,
                ..
            },
        ) => *offline && entered_at == current_entered_at,
        _ => false,
    }
}

pub(super) fn result_overlay_intersects_header(panel_top: Option<f32>, body_top: f32) -> bool {
    let Some(panel_top) = panel_top else {
        return false;
    };
    if !panel_top.is_finite() || !body_top.is_finite() {
        return false;
    }
    let panel_bottom = panel_top + TOP_HEIGHT as f32;
    let header_bottom = body_top + TOP_HEIGHT as f32;
    panel_top < header_bottom && panel_bottom > body_top
}

pub(super) fn result_panel_top_at(phase: &MainPhase, body_top: f32, now: Instant) -> Option<f32> {
    if let MainPhase::Matched { stage, .. } = phase {
        return Some(match stage {
            ResultCardStage::Entering { started_at } => {
                let progress = result_card_progress(*started_at, now);
                body_top - (1.0 - progress) * TOP_HEIGHT as f32
            }
            ResultCardStage::Holding { .. } => body_top,
            ResultCardStage::Exiting {
                started_at,
                from_offset,
            } => {
                let progress = result_card_progress(*started_at, now);
                body_top + *from_offset * (1.0 - progress) + TOP_HEIGHT as f32 * progress
            }
        });
    }
    let (entered_at, returned_to_ready_at) = match phase {
        MainPhase::NoMatch {
            entered_at,
            returned_to_ready_at,
        }
        | MainPhase::Offline {
            entered_at,
            returned_to_ready_at,
        } => (*entered_at, *returned_to_ready_at),
        _ => return None,
    };
    if let Some(returned_at) = returned_to_ready_at {
        // Freeze the entry position at the close event. Advancing the entry
        // clock during exit makes the D2D fallback drift away from the
        // Composition track when a NoMatch card is closed mid-entry.
        let from_offset =
            -(1.0 - result_card_progress(entered_at, returned_at)) * TOP_HEIGHT as f32;
        let exit = result_card_progress(returned_at, now);
        Some(body_top + from_offset + (TOP_HEIGHT as f32 - from_offset) * exit)
    } else {
        let entrance = result_card_progress(entered_at, now);
        Some(body_top - (1.0 - entrance) * TOP_HEIGHT as f32)
    }
}

pub(super) fn advance_result_card_stage(stage: ResultCardStage, now: Instant) -> ResultCardAdvance {
    match stage {
        ResultCardStage::Entering { started_at } => {
            let holding_started_at = started_at + RESULT_CARD_TRANSITION;
            let exiting_started_at = holding_started_at + RESULT_CARD_HOLD;
            if now >= exiting_started_at {
                if now.saturating_duration_since(exiting_started_at) >= RESULT_CARD_TRANSITION {
                    ResultCardAdvance::Ready
                } else {
                    ResultCardAdvance::Stage(ResultCardStage::Exiting {
                        started_at: exiting_started_at,
                        from_offset: 0.0,
                    })
                }
            } else if now >= holding_started_at {
                ResultCardAdvance::Stage(ResultCardStage::Holding {
                    started_at: holding_started_at,
                })
            } else {
                ResultCardAdvance::Unchanged
            }
        }
        ResultCardStage::Holding { started_at } => {
            let exiting_started_at = started_at + RESULT_CARD_HOLD;
            if now >= exiting_started_at {
                if now.saturating_duration_since(exiting_started_at) >= RESULT_CARD_TRANSITION {
                    ResultCardAdvance::Ready
                } else {
                    ResultCardAdvance::Stage(ResultCardStage::Exiting {
                        started_at: exiting_started_at,
                        from_offset: 0.0,
                    })
                }
            } else {
                ResultCardAdvance::Unchanged
            }
        }
        ResultCardStage::Exiting { started_at, .. }
            if now.saturating_duration_since(started_at) >= RESULT_CARD_TRANSITION =>
        {
            ResultCardAdvance::Ready
        }
        _ => ResultCardAdvance::Unchanged,
    }
}

pub(super) fn result_overlay_fallback_needs_frame(
    phase: &MainPhase,
    composition_ready: bool,
) -> bool {
    !composition_ready
        && matches!(
            phase,
            MainPhase::Matched {
                stage: ResultCardStage::Entering { .. } | ResultCardStage::Exiting { .. },
                ..
            }
        )
}

pub(super) fn animation_needs_d2d_redraw(
    phase: &MainPhase,
    status_text_animating: bool,
    status_text_composition_ready: bool,
    result_overlay_composition_ready: bool,
) -> bool {
    (status_text_animating && !status_text_composition_ready)
        || (!result_overlay_composition_ready
            && matches!(
                phase,
                MainPhase::Matched { .. } | MainPhase::NoMatch { .. } | MainPhase::Offline { .. }
            ))
}
