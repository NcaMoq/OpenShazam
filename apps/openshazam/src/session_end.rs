use openshazam_controller::SessionEndReason;
use shazam_progressive::{SessionStatus, StopReason};

/// Resolves a terminal engine state before consulting control requests.
/// A terminal Match/Timeout therefore cannot be overwritten by a later
/// cleanup signal. For a still-active session, Shutdown > Cancel > Stop.
pub fn resolve_session_end(
    status: SessionStatus,
    saw_match: bool,
    saw_no_match: bool,
    stop_requested: bool,
    cancel_requested: bool,
    shutdown_requested: bool,
) -> SessionEndReason {
    match status {
        SessionStatus::Succeeded => {
            return if saw_match {
                SessionEndReason::Matched
            } else {
                SessionEndReason::NoMatch
            };
        }
        SessionStatus::Exhausted if saw_match || saw_no_match => {
            return if saw_match {
                SessionEndReason::Matched
            } else {
                SessionEndReason::NoMatch
            };
        }
        SessionStatus::Exhausted => return SessionEndReason::Exhausted,
        SessionStatus::Failed(StopReason::SessionTimeout) => return SessionEndReason::TimedOut,
        SessionStatus::Cancelled if shutdown_requested => return SessionEndReason::Shutdown,
        SessionStatus::Cancelled => return SessionEndReason::Cancelled,
        SessionStatus::Failed(_) => return SessionEndReason::Failed,
        _ => {}
    }

    if shutdown_requested {
        SessionEndReason::Shutdown
    } else if cancel_requested {
        SessionEndReason::Cancelled
    } else if stop_requested {
        SessionEndReason::Stopped
    } else {
        SessionEndReason::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_session_end;
    use openshazam_controller::SessionEndReason;
    use shazam_progressive::{SessionStatus, StopReason};

    #[test]
    fn resolves_natural_terminal_states_before_control_races() {
        assert_eq!(
            resolve_session_end(SessionStatus::Succeeded, true, false, true, true, true),
            SessionEndReason::Matched
        );
        assert_eq!(
            resolve_session_end(SessionStatus::Succeeded, false, true, false, false, false),
            SessionEndReason::NoMatch
        );
        assert_eq!(
            resolve_session_end(
                SessionStatus::Failed(StopReason::SessionTimeout),
                false,
                false,
                true,
                true,
                true
            ),
            SessionEndReason::TimedOut
        );
        assert_eq!(
            resolve_session_end(SessionStatus::Exhausted, false, false, false, false, false),
            SessionEndReason::Exhausted
        );
        assert_eq!(
            resolve_session_end(SessionStatus::Exhausted, false, true, false, false, false),
            SessionEndReason::NoMatch
        );
    }

    #[test]
    fn resolves_active_control_requests_in_documented_priority_order() {
        let active = SessionStatus::Capturing;
        assert_eq!(
            resolve_session_end(active, false, false, true, true, true),
            SessionEndReason::Shutdown
        );
        assert_eq!(
            resolve_session_end(active, false, false, true, true, false),
            SessionEndReason::Cancelled
        );
        assert_eq!(
            resolve_session_end(active, false, false, true, false, false),
            SessionEndReason::Stopped
        );
        assert_eq!(
            resolve_session_end(SessionStatus::Cancelled, false, false, false, true, true),
            SessionEndReason::Shutdown
        );
        assert_eq!(
            resolve_session_end(active, false, false, false, false, false),
            SessionEndReason::Failed
        );
    }

    #[test]
    fn internal_request_cleanup_is_not_a_user_cancel_input() {
        // The resolver accepts only the external command state. Turning on an
        // internal HTTP cancellation token after success cannot alter this.
        assert_eq!(
            resolve_session_end(SessionStatus::Succeeded, true, false, false, false, false),
            SessionEndReason::Matched
        );
    }
}
