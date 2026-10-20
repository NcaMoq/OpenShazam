use crate::recognition::{
    self, NativeContextState, ProgressiveCaptureMode, WindowsProgressiveControl,
};
use openshazam_controller::{
    EventEmitter, RecognitionBackend, RecognitionContext, RecognitionErrorKind,
    RecognitionEventKind, RecognitionMode, SessionEndReason, SessionId,
};
use shazam_http_lab::NativeTaggingRequestContext;
use shazam_progressive::NativeTaggingType;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
};

struct SessionWorker {
    cancelled: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
    accepted_control: Arc<std::sync::atomic::AtomicU8>,
    shutting_down: Arc<AtomicBool>,
    context: NativeContextState,
    mode: RecognitionMode,
    worker: JoinHandle<()>,
}

/// Windows loopback backend backed by the same capture, ProgressiveSession,
/// Config, and HTTP submission path used by the `recognize-loopback` CLI.
///
/// A GUI host can pass it directly to the typed controller API:
///
/// ```no_run
/// use openshazam::{WindowsRecognitionBackend};
/// use openshazam_controller::ApplicationController;
///
/// let controller = ApplicationController::spawn(
///     WindowsRecognitionBackend::new(),
///     16,
///     64,
/// )?;
/// let ticket = controller.commands.start_manual().expect("manual command was accepted");
/// // Poll controller.events for SignatureReady, RequestStarted,
/// // ManualUiEvent and SessionEnded; inspect ticket.command_id for startup.
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// Each session runs on one named worker so WASAPI and its COM objects are
/// initialized, used, and dropped on the same thread. HTTP requests continue
/// on the existing per-attempt workers. Controller command methods only set
/// atomics and return; `shutdown` performs joins on the controller worker.
pub struct WindowsRecognitionBackend {
    device_id: Option<String>,
    sessions: HashMap<SessionId, SessionWorker>,
    faulted: Arc<AtomicBool>,
}

impl WindowsRecognitionBackend {
    /// Uses the current default Windows playback endpoint for loopback capture.
    pub fn new() -> Self {
        Self {
            device_id: None,
            sessions: HashMap::new(),
            faulted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Uses a previously enumerated Windows render-device ID.
    pub fn with_device_id(device_id: impl Into<String>) -> Self {
        Self {
            device_id: Some(device_id.into()),
            sessions: HashMap::new(),
            faulted: Arc::new(AtomicBool::new(false)),
        }
    }

    fn reap_prior_sessions(&mut self) -> Result<(), String> {
        let prior = std::mem::take(&mut self.sessions);
        let mut panicked = false;
        for (_, session) in prior {
            if session.worker.join().is_err() {
                panicked = true;
            }
        }
        if panicked {
            self.faulted.store(true, Ordering::Release);
            return Err("previous Windows recognition worker panicked; backend is faulted".into());
        }
        Ok(())
    }
}

impl Default for WindowsRecognitionBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RecognitionBackend for WindowsRecognitionBackend {
    fn start(
        &mut self,
        session_id: SessionId,
        mode: RecognitionMode,
        context: Option<RecognitionContext>,
        events: EventEmitter,
    ) -> Result<(), String> {
        self.reap_prior_sessions()?;
        if self.faulted.load(Ordering::Acquire) {
            return Err(
                "Windows recognition backend is faulted after an incomplete worker cleanup".into(),
            );
        }
        let request_context = match (mode, context) {
            (RecognitionMode::Manual, _) => None,
            (RecognitionMode::Auto, Some(RecognitionContext::Foreground)) => {
                Some(NativeTaggingRequestContext::Foreground)
            }
            (RecognitionMode::Auto, Some(RecognitionContext::Background)) => {
                Some(NativeTaggingRequestContext::Background)
            }
            (RecognitionMode::Auto, None) => {
                return Err("Auto recognition requires a foreground/background context".into())
            }
        };
        let tagging_type = match mode {
            RecognitionMode::Manual => NativeTaggingType::Manual,
            RecognitionMode::Auto => NativeTaggingType::Auto,
        };
        let context_state = NativeContextState::new(request_context);
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let accepted_control = Arc::new(std::sync::atomic::AtomicU8::new(0));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let config_generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let config_fetching = Arc::new(AtomicBool::new(false));
        let cleanup_failed = Arc::new(AtomicBool::new(false));
        let run_cancelled = Arc::clone(&cancelled);
        let run_stop_requested = Arc::clone(&stop_requested);
        let run_accepted_control = Arc::clone(&accepted_control);
        let run_config_generation = Arc::clone(&config_generation);
        let run_config_fetching = Arc::clone(&config_fetching);
        let run_cleanup_failed = Arc::clone(&cleanup_failed);
        let run_faulted = Arc::clone(&self.faulted);
        let run_context = context_state.clone();
        let device_id = self.device_id.clone();
        let worker_events = events.clone();
        let worker = thread::Builder::new()
            .name(format!("openshazam-wasapi-{}", session_id.0))
            .spawn(move || {
                let control = WindowsProgressiveControl {
                    cancelled: run_cancelled.clone(),
                    stop_requested: run_stop_requested.clone(),
                    config_generation: run_config_generation.clone(),
                    config_fetching: run_config_fetching.clone(),
                    accepted_control: run_accepted_control.clone(),
                    cleanup_failed: run_cleanup_failed.clone(),
                    manual_trigger_receiver: if mode == RecognitionMode::Manual {
                        let (sender, receiver) = std::sync::mpsc::channel();
                        let _ = sender.send(recognition::ManualTriggerSignal::Enter);
                        Some(receiver)
                    } else {
                        None
                    },
                    events: Some((session_id, mode, worker_events.clone())),
                };
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    recognition::recognize_loopback_progressive_controlled(
                        device_id.as_deref(),
                        ProgressiveCaptureMode {
                            tagging_type,
                            request_context,
                        },
                        run_context,
                        false,
                        control,
                    )
                }));
                let reason = match run {
                    Ok(Ok(reason)) => reason,
                    Ok(Err(error)) => {
                        if run_cleanup_failed.load(Ordering::Acquire) {
                            run_faulted.store(true, Ordering::Release);
                        }
                        let message = if run_cleanup_failed.load(Ordering::Acquire) {
                            format!("{error}; recognition worker cleanup was incomplete")
                        } else {
                            error.to_string()
                        };
                        let _ = worker_events.emit(
                            Some(session_id),
                            Some(mode),
                            RecognitionEventKind::RecognitionError {
                                kind: if run_config_fetching.load(Ordering::Acquire) {
                                    RecognitionErrorKind::Configuration
                                } else {
                                    RecognitionErrorKind::Capture
                                },
                                message,
                            },
                        );
                        SessionEndReason::Failed
                    }
                    Err(payload) => {
                        run_faulted.store(true, Ordering::Release);
                        let panic_message = payload
                            .downcast_ref::<&str>()
                            .copied()
                            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                            .unwrap_or("recognition worker panicked");
                        let cleanup_note = if run_cleanup_failed.load(Ordering::Acquire) {
                            "; an HTTP worker also failed during cleanup"
                        } else {
                            "; capture resources unwound on their owner thread"
                        };
                        let _ = worker_events.emit(
                            Some(session_id),
                            Some(mode),
                            RecognitionEventKind::RecognitionError {
                                kind: RecognitionErrorKind::Internal,
                                message: format!("{panic_message}{cleanup_note}"),
                            },
                        );
                        SessionEndReason::Failed
                    }
                };
                let generation = run_config_generation.load(Ordering::Acquire);
                let finished_events = if generation == 0 {
                    worker_events
                } else {
                    worker_events.with_config_generation(generation)
                };
                let _ = finished_events.finish(reason);
            })
            .map_err(|error| format!("could not start Windows recognition worker: {error}"))?;
        self.sessions.insert(
            session_id,
            SessionWorker {
                cancelled,
                stop_requested,
                accepted_control,
                shutting_down,
                context: context_state,
                mode,
                worker,
            },
        );
        Ok(())
    }

    fn stop(&mut self, session_id: SessionId) -> Result<(), String> {
        let session = self
            .sessions
            .get(&session_id)
            .ok_or_else(|| format!("no Windows recognition worker for {session_id:?}"))?;
        session.stop_requested.store(true, Ordering::Release);
        let _ =
            session
                .accepted_control
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
        Ok(())
    }

    fn cancel(&mut self, session_id: SessionId) -> Result<(), String> {
        let session = self
            .sessions
            .get(&session_id)
            .ok_or_else(|| format!("no Windows recognition worker for {session_id:?}"))?;
        session.cancelled.store(true, Ordering::Release);
        let _ =
            session
                .accepted_control
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
        Ok(())
    }

    fn set_context(
        &mut self,
        session_id: SessionId,
        context: RecognitionContext,
    ) -> Result<(), String> {
        let session = self
            .sessions
            .get(&session_id)
            .filter(|session| session.mode == RecognitionMode::Auto)
            .ok_or_else(|| format!("no Auto recognition worker for {session_id:?}"))?;
        session.context.set(match context {
            RecognitionContext::Foreground => NativeTaggingRequestContext::Foreground,
            RecognitionContext::Background => NativeTaggingRequestContext::Background,
        });
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), String> {
        let sessions = std::mem::take(&mut self.sessions);
        for session in sessions.values() {
            session.shutting_down.store(true, Ordering::Release);
            session.cancelled.store(true, Ordering::Release);
            let _ = session.accepted_control.compare_exchange(
                0,
                3,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        let mut panicked = false;
        for (_, session) in sessions {
            if session.worker.join().is_err() {
                panicked = true;
            }
        }
        if panicked {
            self.faulted.store(true, Ordering::Release);
            return Err(
                "Windows recognition worker panicked during shutdown; backend is faulted".into(),
            );
        }
        Ok(())
    }
}
