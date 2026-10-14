use openshazam_controller::SessionEndReason;
use shazam_progressive::{SessionStatus, StopReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionControlRequests {
    pub stop_requested: bool,
    pub cancel_requested: bool,
    pub shutdown_requested: bool,
}

impl SessionControlRequests {
    pub fn from_first_request(code: u8) -> Self {
        match code {
            1 => Self {
                stop_requested: true,
                cancel_requested: false,
                shutdown_requested: false,
            },
            2 => Self {
                stop_requested: false,
                cancel_requested: true,
                shutdown_requested: false,
            },
            3 => Self {
                stop_requested: false,
                cancel_requested: false,
                shutdown_requested: true,
            },
            _ => Self {
                stop_requested: false,
                cancel_requested: false,
                shutdown_requested: false,
            },
        }
    }
}

/// Resolves the session's terminal cause. `terminal_before_input_end` is
/// captured before `finish_input()`: if the lifecycle was already terminal,
/// a later Stop cannot rewrite a natural result. If it was active, an empty
/// `Exhausted` produced by Stop-ending the PCM stream means `Stopped`, while
/// delivered Match/NoMatch and Native Timeout outcomes remain authoritative.
pub fn resolve_session_end(
    status: SessionStatus,
    terminal_before_input_end: Option<SessionStatus>,
    saw_match: bool,
    saw_no_match: bool,
    control: SessionControlRequests,
    failure_confirmed: bool,
) -> SessionEndReason {
    if failure_confirmed {
        return SessionEndReason::Failed;
    }

    if let Some(before) = terminal_before_input_end {
        if let Some(reason) = terminal_reason(before, saw_match, saw_no_match, false) {
            return reason;
        }
    }

    if let Some(reason) = terminal_reason(status, saw_match, saw_no_match, true) {
        return reason;
    }

    control_reason(control).unwrap_or(if status == SessionStatus::Exhausted {
        SessionEndReason::Exhausted
    } else {
        SessionEndReason::Failed
    })
}

fn terminal_reason(
    status: SessionStatus,
    saw_match: bool,
    saw_no_match: bool,
    allow_stop_caused_exhaustion: bool,
) -> Option<SessionEndReason> {
    match status {
        SessionStatus::Succeeded => Some(if saw_match {
            SessionEndReason::Matched
        } else {
            SessionEndReason::NoMatch
        }),
        SessionStatus::Exhausted if saw_match => Some(SessionEndReason::Matched),
        SessionStatus::Exhausted if saw_no_match => Some(SessionEndReason::NoMatch),
        SessionStatus::Exhausted if !allow_stop_caused_exhaustion => {
            Some(SessionEndReason::Exhausted)
        }
        SessionStatus::Failed(StopReason::SessionTimeout) => Some(SessionEndReason::TimedOut),
        SessionStatus::Failed(_) => Some(SessionEndReason::Failed),
        _ => None,
    }
}

fn control_reason(control: SessionControlRequests) -> Option<SessionEndReason> {
    if control.shutdown_requested {
        Some(SessionEndReason::Shutdown)
    } else if control.cancel_requested {
        Some(SessionEndReason::Cancelled)
    } else if control.stop_requested {
        Some(SessionEndReason::Stopped)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_session_end, SessionControlRequests};
    use openshazam_controller::SessionEndReason;
    use shazam_progressive::{SessionStatus, StopReason};

    #[test]
    fn resolves_natural_terminal_states_before_control_races() {
        assert_eq!(
            resolve_session_end(
                SessionStatus::Succeeded,
                None,
                true,
                false,
                SessionControlRequests {
                    stop_requested: true,
                    cancel_requested: true,
                    shutdown_requested: true
                },
                false
            ),
            SessionEndReason::Matched
        );
        assert_eq!(
            resolve_session_end(
                SessionStatus::Succeeded,
                None,
                false,
                true,
                SessionControlRequests {
                    stop_requested: false,
                    cancel_requested: false,
                    shutdown_requested: false
                },
                false
            ),
            SessionEndReason::NoMatch
        );
        assert_eq!(
            resolve_session_end(
                SessionStatus::Failed(StopReason::SessionTimeout),
                None,
                false,
                false,
                SessionControlRequests {
                    stop_requested: true,
                    cancel_requested: true,
                    shutdown_requested: true
                },
                false
            ),
            SessionEndReason::TimedOut
        );
        assert_eq!(
            resolve_session_end(
                SessionStatus::Exhausted,
                None,
                false,
                false,
                SessionControlRequests {
                    stop_requested: false,
                    cancel_requested: false,
                    shutdown_requested: false
                },
                false
            ),
            SessionEndReason::Exhausted
        );
        assert_eq!(
            resolve_session_end(
                SessionStatus::Exhausted,
                None,
                false,
                true,
                SessionControlRequests {
                    stop_requested: false,
                    cancel_requested: false,
                    shutdown_requested: false
                },
                false
            ),
            SessionEndReason::NoMatch
        );
    }

    #[test]
    fn resolves_active_control_requests_in_documented_priority_order() {
        let active = SessionStatus::Capturing;
        assert_eq!(
            resolve_session_end(
                active,
                None,
                false,
                false,
                SessionControlRequests {
                    stop_requested: true,
                    cancel_requested: true,
                    shutdown_requested: true
                },
                false
            ),
            SessionEndReason::Shutdown
        );
        assert_eq!(
            resolve_session_end(
                active,
                None,
                false,
                false,
                SessionControlRequests {
                    stop_requested: true,
                    cancel_requested: true,
                    shutdown_requested: false
                },
                false
            ),
            SessionEndReason::Cancelled
        );
        assert_eq!(
            resolve_session_end(
                active,
                None,
                false,
                false,
                SessionControlRequests {
                    stop_requested: true,
                    cancel_requested: false,
                    shutdown_requested: false
                },
                false
            ),
            SessionEndReason::Stopped
        );
        assert_eq!(
            resolve_session_end(
                SessionStatus::Cancelled,
                None,
                false,
                false,
                SessionControlRequests {
                    stop_requested: false,
                    cancel_requested: true,
                    shutdown_requested: true
                },
                false
            ),
            SessionEndReason::Shutdown
        );
        assert_eq!(
            resolve_session_end(
                active,
                None,
                false,
                false,
                SessionControlRequests {
                    stop_requested: false,
                    cancel_requested: false,
                    shutdown_requested: false
                },
                false
            ),
            SessionEndReason::Failed
        );
    }

    #[test]
    fn internal_request_cleanup_is_not_a_user_cancel_input() {
        // The resolver accepts only the external command state. Turning on an
        // internal HTTP cancellation token after success cannot alter this.
        assert_eq!(
            resolve_session_end(
                SessionStatus::Succeeded,
                None,
                true,
                false,
                SessionControlRequests {
                    stop_requested: false,
                    cancel_requested: false,
                    shutdown_requested: false
                },
                false
            ),
            SessionEndReason::Matched
        );
    }

    #[test]
    fn distinguishes_stop_caused_exhaustion_from_natural_exhaustion() {
        let stop_exhausted = resolve_session_end(
            SessionStatus::Exhausted,
            Some(SessionStatus::Capturing),
            false,
            false,
            SessionControlRequests {
                stop_requested: true,
                cancel_requested: false,
                shutdown_requested: false,
            },
            false,
        );
        assert_eq!(stop_exhausted, SessionEndReason::Stopped);

        let stop_after_natural_exhaustion = resolve_session_end(
            SessionStatus::Exhausted,
            Some(SessionStatus::Exhausted),
            false,
            false,
            SessionControlRequests {
                stop_requested: true,
                cancel_requested: false,
                shutdown_requested: false,
            },
            false,
        );
        assert_eq!(stop_after_natural_exhaustion, SessionEndReason::Exhausted);
    }

    #[test]
    fn confirms_success_no_match_timeout_and_failure_before_late_controls() {
        assert_eq!(
            resolve_session_end(
                SessionStatus::Succeeded,
                Some(SessionStatus::Succeeded),
                true,
                false,
                SessionControlRequests {
                    stop_requested: true,
                    cancel_requested: true,
                    shutdown_requested: true
                },
                false
            ),
            SessionEndReason::Matched
        );
        assert_eq!(
            resolve_session_end(
                SessionStatus::Exhausted,
                Some(SessionStatus::Exhausted),
                false,
                true,
                SessionControlRequests {
                    stop_requested: true,
                    cancel_requested: false,
                    shutdown_requested: false
                },
                false
            ),
            SessionEndReason::NoMatch
        );
        assert_eq!(
            resolve_session_end(
                SessionStatus::Failed(StopReason::SessionTimeout),
                Some(SessionStatus::Failed(StopReason::SessionTimeout)),
                false,
                false,
                SessionControlRequests {
                    stop_requested: true,
                    cancel_requested: false,
                    shutdown_requested: false
                },
                false
            ),
            SessionEndReason::TimedOut
        );
        assert_eq!(
            resolve_session_end(
                SessionStatus::Capturing,
                None,
                false,
                false,
                SessionControlRequests {
                    stop_requested: true,
                    cancel_requested: true,
                    shutdown_requested: false
                },
                true
            ),
            SessionEndReason::Failed
        );
    }

    #[test]
    fn stop_cancel_and_shutdown_are_distinct_and_control_order_is_stable() {
        let active = SessionStatus::Capturing;
        assert_eq!(
            resolve_session_end(
                active,
                None,
                false,
                false,
                SessionControlRequests {
                    stop_requested: true,
                    cancel_requested: true,
                    shutdown_requested: false
                },
                false
            ),
            SessionEndReason::Cancelled
        );
        assert_eq!(
            resolve_session_end(
                active,
                None,
                false,
                false,
                SessionControlRequests {
                    stop_requested: true,
                    cancel_requested: true,
                    shutdown_requested: true
                },
                false
            ),
            SessionEndReason::Shutdown
        );
    }

    #[test]
    fn accepted_control_request_is_stable_after_later_requests() {
        for (request, expected) in [
            (1, SessionEndReason::Stopped),
            (2, SessionEndReason::Cancelled),
            (3, SessionEndReason::Shutdown),
        ] {
            let control = SessionControlRequests::from_first_request(request);
            assert_eq!(
                resolve_session_end(SessionStatus::Capturing, None, false, false, control, false,),
                expected
            );
        }
    }
}
