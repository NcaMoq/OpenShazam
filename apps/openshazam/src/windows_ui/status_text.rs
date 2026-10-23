use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StatusCycle {
    Ready,
    Auto,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct StatusTextAlternator {
    pub(super) cycle: StatusCycle,
    pub(super) first: bool,
    pub(super) next_at: Instant,
    pub(super) transition_started_at: Option<Instant>,
}

impl StatusTextAlternator {
    pub(super) fn ready(now: Instant) -> Self {
        Self::new(StatusCycle::Ready, now)
    }

    pub(super) fn auto(now: Instant) -> Self {
        Self::new(StatusCycle::Auto, now)
    }

    pub(super) fn after_success(now: Instant) -> Self {
        Self::ready(now)
    }

    fn new(cycle: StatusCycle, now: Instant) -> Self {
        Self {
            cycle,
            first: true,
            next_at: now + Duration::from_secs(3),
            transition_started_at: None,
        }
    }

    pub(super) fn advance(&mut self, now: Instant) -> bool {
        let mut changed = false;
        while now >= self.next_at {
            self.first = !self.first;
            self.transition_started_at = Some(self.next_at);
            self.next_at += Duration::from_secs(match self.cycle {
                StatusCycle::Ready if self.first => 7,
                StatusCycle::Ready => 2,
                StatusCycle::Auto => 3,
            });
            changed = true;
        }
        if self.transition_started_at.is_some_and(|started| {
            now.saturating_duration_since(started) >= Duration::from_millis(400)
        }) {
            self.transition_started_at = None;
            changed = true;
        }
        changed
    }

    pub(super) fn animating(self, now: Instant) -> bool {
        self.transition_started_at.is_some_and(|started| {
            now.saturating_duration_since(started) < Duration::from_millis(400)
        })
    }
}

pub(super) fn status_text_progress(started_at: Instant, now: Instant) -> f32 {
    super::ui_model::ease_in_out(status_text_linear_progress(started_at, now) as f64) as f32
}

pub(super) fn status_text_linear_progress(started_at: Instant, now: Instant) -> f32 {
    (now.saturating_duration_since(started_at).as_secs_f64()
        / super::STATUS_TEXT_TRANSITION_DURATION.as_secs_f64())
    .clamp(0.0, 1.0) as f32
}
