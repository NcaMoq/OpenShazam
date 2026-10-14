//! UI-independent, bounded command and event boundary for recognition apps.
//!
//! UI callers enqueue commands with `try_send` and poll events without waiting.
//! The backend runs on a dedicated controller thread. A backend must hand audio
//! capture and HTTP work to their own workers and return promptly from command
//! methods so the controller remains responsive.

use std::{
    io,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub use shazam_progressive::{RecognitionOutcome, RetryDirective};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecognitionMode {
    Manual,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecognitionContext {
    Foreground,
    Background,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecognitionCommand {
    StartManualRecognition {
        session_id: SessionId,
    },
    StartAutoRecognition {
        session_id: SessionId,
        context: RecognitionContext,
    },
    StopRecognition {
        session_id: SessionId,
    },
    CancelRecognition {
        session_id: SessionId,
    },
    SetRecognitionContext {
        session_id: SessionId,
        context: RecognitionContext,
    },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecognitionErrorKind {
    Capture,
    Transport,
    Signature,
    Configuration,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecognitionEventKind {
    SessionStarted,
    CaptureStarted,
    SignatureReady {
        attempt_id: u64,
        sample_ms: u64,
        signature_bytes: usize,
    },
    RequestStarted {
        attempt_id: u64,
    },
    RetryScheduled {
        attempt_id: u64,
        retry_duration_ms: Option<u64>,
        interval_ms: Option<u64>,
    },
    MatchFound {
        attempt_id: u64,
        title: Option<String>,
        artist: Option<String>,
        track_id: Option<String>,
    },
    NoMatch {
        attempt_id: u64,
    },
    RecognitionError {
        kind: RecognitionErrorKind,
        message: String,
    },
    SessionTimedOut,
    SessionCancelled,
    SessionEnded {
        reason: SessionEndReason,
    },
    CommandRejected {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEndReason {
    Stopped,
    Cancelled,
    Matched,
    Exhausted,
    Failed,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecognitionEvent {
    pub sequence: u64,
    pub session_id: Option<SessionId>,
    pub mode: Option<RecognitionMode>,
    /// Monotonic time since this controller started. This is event creation
    /// time, not the later time at which the UI dequeues the event.
    pub elapsed: Duration,
    pub kind: RecognitionEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitError {
    QueueFull,
    ControllerStopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventError {
    QueueFull,
    ControllerStopped,
}

/// Platform adapter contract. Implementations own WASAPI/CoreAudio and network
/// workers. `stop` stops capture and drains in-flight requests before returning;
/// `cancel` invalidates the session and aborts requests where supported.
/// Every method runs on the controller worker, so implementations should start
/// long-running work on dedicated workers and reserve blocking waits for stop
/// or shutdown.
pub trait RecognitionBackend: Send + 'static {
    fn start(
        &mut self,
        session_id: SessionId,
        mode: RecognitionMode,
        context: Option<RecognitionContext>,
        events: EventEmitter,
    ) -> Result<(), String>;
    fn stop(&mut self, session_id: SessionId) -> Result<(), String>;
    fn cancel(&mut self, session_id: SessionId) -> Result<(), String>;
    fn set_context(
        &mut self,
        session_id: SessionId,
        context: RecognitionContext,
    ) -> Result<(), String>;
    fn shutdown(&mut self) -> Result<(), String>;
}

enum WorkerMessage {
    Command(RecognitionCommand),
}

#[derive(Clone)]
pub struct CommandHandle {
    sender: SyncSender<WorkerMessage>,
    next_session_id: Arc<AtomicU64>,
    shutdown_requested: Arc<AtomicBool>,
}

impl CommandHandle {
    pub fn start_manual(&self) -> Result<SessionId, SubmitError> {
        let id = self.allocate_session_id();
        self.submit(RecognitionCommand::StartManualRecognition { session_id: id })?;
        Ok(id)
    }

    pub fn start_auto(&self, context: RecognitionContext) -> Result<SessionId, SubmitError> {
        let id = self.allocate_session_id();
        self.submit(RecognitionCommand::StartAutoRecognition {
            session_id: id,
            context,
        })?;
        Ok(id)
    }

    pub fn submit(&self, command: RecognitionCommand) -> Result<(), SubmitError> {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return Err(SubmitError::ControllerStopped);
        }
        match self.sender.try_send(WorkerMessage::Command(command)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(SubmitError::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(SubmitError::ControllerStopped),
        }
    }

    /// Requests shutdown without waiting for the worker or active devices.
    pub fn request_shutdown(&self) {
        if !self.shutdown_requested.swap(true, Ordering::AcqRel) {
            let _ = self
                .sender
                .try_send(WorkerMessage::Command(RecognitionCommand::Shutdown));
        }
    }

    fn allocate_session_id(&self) -> SessionId {
        SessionId(self.next_session_id.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Clone)]
pub struct EventEmitter {
    sender: SyncSender<RecognitionEvent>,
    sequence: Arc<AtomicU64>,
    origin: Instant,
    order_lock: Arc<Mutex<()>>,
    session_gate: Option<(SessionId, RecognitionMode, Arc<AtomicBool>)>,
}

impl EventEmitter {
    fn for_session(
        &self,
        session_id: SessionId,
        mode: RecognitionMode,
        gate: Arc<AtomicBool>,
    ) -> Self {
        let mut scoped = self.clone();
        scoped.session_gate = Some((session_id, mode, gate));
        scoped
    }

    pub fn emit(
        &self,
        session_id: Option<SessionId>,
        mode: Option<RecognitionMode>,
        kind: RecognitionEventKind,
    ) -> Result<(), EventError> {
        if let Some((allowed_session, allowed_mode, gate)) = &self.session_gate {
            if !gate.load(Ordering::Acquire)
                || session_id != Some(*allowed_session)
                || mode != Some(*allowed_mode)
            {
                return Err(EventError::ControllerStopped);
            }
        }
        let _order = self.order_lock.lock().expect("event order lock poisoned");
        if self
            .session_gate
            .as_ref()
            .is_some_and(|(_, _, gate)| !gate.load(Ordering::Acquire))
        {
            return Err(EventError::ControllerStopped);
        }
        let event = RecognitionEvent {
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            session_id,
            mode,
            elapsed: self.origin.elapsed(),
            kind,
        };
        match self.sender.try_send(event) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(EventError::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(EventError::ControllerStopped),
        }
    }
}

pub struct ApplicationController {
    pub commands: CommandHandle,
    pub events: EventReceiver,
    worker: Option<JoinHandle<Result<(), String>>>,
}

pub struct EventReceiver(Receiver<RecognitionEvent>);

impl EventReceiver {
    /// Polls without blocking the UI thread.
    pub fn try_recv(&self) -> Result<RecognitionEvent, TryRecvError> {
        self.0.try_recv()
    }
}

impl ApplicationController {
    pub fn spawn<B: RecognitionBackend>(
        mut backend: B,
        command_capacity: usize,
        event_capacity: usize,
    ) -> io::Result<Self> {
        if command_capacity == 0 || event_capacity == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "controller queue capacities must be nonzero",
            ));
        }
        let (command_sender, command_receiver) = mpsc::sync_channel(command_capacity);
        let (event_sender, event_receiver) = mpsc::sync_channel(event_capacity);
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let commands = CommandHandle {
            sender: command_sender,
            next_session_id: Arc::new(AtomicU64::new(1)),
            shutdown_requested: Arc::clone(&shutdown_requested),
        };
        let events = EventEmitter {
            sender: event_sender,
            sequence: Arc::new(AtomicU64::new(1)),
            origin: Instant::now(),
            order_lock: Arc::new(Mutex::new(())),
            session_gate: None,
        };
        let worker = thread::Builder::new()
            .name("openshazam-controller".into())
            .spawn(move || {
                let result =
                    run_controller(&mut backend, command_receiver, events, shutdown_requested);
                let shutdown_result = backend.shutdown();
                result.and(shutdown_result)
            })?;
        Ok(Self {
            commands,
            events: EventReceiver(event_receiver),
            worker: Some(worker),
        })
    }

    /// Joins after a shutdown request. Call this from a background thread.
    pub fn join(mut self) -> thread::Result<Result<(), String>> {
        self.commands.request_shutdown();
        self.worker.take().expect("controller worker exists").join()
    }
}

fn run_controller<B: RecognitionBackend>(
    backend: &mut B,
    commands: Receiver<WorkerMessage>,
    events: EventEmitter,
    shutdown_requested: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut active: Option<(
        SessionId,
        RecognitionMode,
        Option<RecognitionContext>,
        Arc<AtomicBool>,
    )> = None;
    loop {
        if shutdown_requested.load(Ordering::Acquire) {
            break;
        }
        let command = match commands.recv_timeout(Duration::from_millis(20)) {
            Ok(WorkerMessage::Command(command)) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match command {
            RecognitionCommand::StartManualRecognition { session_id }
            | RecognitionCommand::StartAutoRecognition { session_id, .. } => {
                let (mode, context) = match command {
                    RecognitionCommand::StartManualRecognition { .. } => {
                        (RecognitionMode::Manual, None)
                    }
                    RecognitionCommand::StartAutoRecognition { context, .. } => {
                        (RecognitionMode::Auto, Some(context))
                    }
                    _ => unreachable!(),
                };
                if let Some((active_id, _, _, _)) = &active {
                    events
                        .emit(
                            Some(session_id),
                            Some(mode),
                            RecognitionEventKind::CommandRejected {
                                reason: format!("session {active_id:?} is already active"),
                            },
                        )
                        .map_err(|error| format!("event queue failed: {error:?}"))?;
                    continue;
                }
                let session_gate = Arc::new(AtomicBool::new(true));
                active = Some((session_id, mode, context, Arc::clone(&session_gate)));
                events
                    .emit(
                        Some(session_id),
                        Some(mode),
                        RecognitionEventKind::SessionStarted,
                    )
                    .map_err(|error| format!("event queue failed: {error:?}"))?;
                if let Err(message) = backend.start(
                    session_id,
                    mode,
                    context,
                    events.for_session(session_id, mode, session_gate.clone()),
                ) {
                    session_gate.store(false, Ordering::Release);
                    let _ = backend.cancel(session_id);
                    events
                        .emit(
                            Some(session_id),
                            Some(mode),
                            RecognitionEventKind::RecognitionError {
                                kind: RecognitionErrorKind::Internal,
                                message,
                            },
                        )
                        .and_then(|()| {
                            events.emit(
                                Some(session_id),
                                Some(mode),
                                RecognitionEventKind::SessionEnded {
                                    reason: SessionEndReason::Failed,
                                },
                            )
                        })
                        .map_err(|error| format!("event queue failed: {error:?}"))?;
                    active = None;
                }
            }
            RecognitionCommand::StopRecognition { session_id } => {
                if let Some((active_id, mode, _, session_gate)) = active.as_ref() {
                    if session_id == *active_id {
                        let mode = *mode;
                        if let Err(message) = backend.stop(session_id) {
                            events
                                .emit(
                                    Some(session_id),
                                    Some(mode),
                                    RecognitionEventKind::RecognitionError {
                                        kind: RecognitionErrorKind::Internal,
                                        message,
                                    },
                                )
                                .map_err(|error| format!("event queue failed: {error:?}"))?;
                        }
                        session_gate.store(false, Ordering::Release);
                        events
                            .emit(
                                Some(session_id),
                                Some(mode),
                                RecognitionEventKind::SessionEnded {
                                    reason: SessionEndReason::Stopped,
                                },
                            )
                            .map_err(|error| format!("event queue failed: {error:?}"))?;
                        active = None;
                    }
                }
            }
            RecognitionCommand::CancelRecognition { session_id } => {
                if let Some((active_id, mode, _, session_gate)) = active.as_ref() {
                    if session_id == *active_id {
                        let mode = *mode;
                        session_gate.store(false, Ordering::Release);
                        if let Err(message) = backend.cancel(session_id) {
                            events
                                .emit(
                                    Some(session_id),
                                    Some(mode),
                                    RecognitionEventKind::RecognitionError {
                                        kind: RecognitionErrorKind::Internal,
                                        message,
                                    },
                                )
                                .map_err(|error| format!("event queue failed: {error:?}"))?;
                        }
                        events
                            .emit(
                                Some(session_id),
                                Some(mode),
                                RecognitionEventKind::SessionCancelled,
                            )
                            .and_then(|()| {
                                events.emit(
                                    Some(session_id),
                                    Some(mode),
                                    RecognitionEventKind::SessionEnded {
                                        reason: SessionEndReason::Cancelled,
                                    },
                                )
                            })
                            .map_err(|error| format!("event queue failed: {error:?}"))?;
                        active = None;
                    }
                }
            }
            RecognitionCommand::SetRecognitionContext {
                session_id,
                context,
            } => {
                if let Some((active_id, mode, _, session_gate)) = active.as_ref() {
                    if session_id == *active_id && *mode == RecognitionMode::Auto {
                        let session_gate = Arc::clone(session_gate);
                        if let Err(message) = backend.set_context(session_id, context) {
                            events
                                .emit(
                                    Some(session_id),
                                    Some(*mode),
                                    RecognitionEventKind::RecognitionError {
                                        kind: RecognitionErrorKind::Internal,
                                        message,
                                    },
                                )
                                .map_err(|error| format!("event queue failed: {error:?}"))?;
                        } else {
                            active = Some((
                                session_id,
                                RecognitionMode::Auto,
                                Some(context),
                                session_gate,
                            ));
                        }
                    }
                }
            }
            RecognitionCommand::Shutdown => break,
        }
    }
    if let Some((session_id, _, _, session_gate)) = active {
        session_gate.store(false, Ordering::Release);
        backend.cancel(session_id)?;
        events
            .emit(
                Some(session_id),
                None,
                RecognitionEventKind::SessionEnded {
                    reason: SessionEndReason::Shutdown,
                },
            )
            .map_err(|error| format!("event queue failed: {error:?}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct MockBackend {
        calls: Arc<Mutex<Vec<String>>>,
        session_events: Arc<Mutex<Option<EventEmitter>>>,
    }

    impl RecognitionBackend for MockBackend {
        fn start(
            &mut self,
            session_id: SessionId,
            mode: RecognitionMode,
            _context: Option<RecognitionContext>,
            events: EventEmitter,
        ) -> Result<(), String> {
            *self.session_events.lock().unwrap() = Some(events);
            self.calls
                .lock()
                .unwrap()
                .push(format!("start:{session_id:?}:{mode:?}"));
            Ok(())
        }

        fn stop(&mut self, session_id: SessionId) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("stop:{session_id:?}"));
            Ok(())
        }

        fn cancel(&mut self, session_id: SessionId) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("cancel:{session_id:?}"));
            Ok(())
        }

        fn set_context(
            &mut self,
            session_id: SessionId,
            context: RecognitionContext,
        ) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("context:{session_id:?}:{context:?}"));
            Ok(())
        }

        fn shutdown(&mut self) -> Result<(), String> {
            self.calls.lock().unwrap().push("shutdown".into());
            Ok(())
        }
    }

    fn next_event(controller: &ApplicationController) -> RecognitionEvent {
        controller
            .events
            .0
            .recv_timeout(Duration::from_secs(1))
            .expect("controller event should arrive")
    }

    #[test]
    fn commands_are_serialized_and_stop_cancel_have_distinct_effects() {
        let backend = MockBackend::default();
        let observed = Arc::clone(&backend.calls);
        let session_events = Arc::clone(&backend.session_events);
        let controller = ApplicationController::spawn(backend, 8, 8).unwrap();
        let manual = controller.commands.start_manual().unwrap();
        let started = next_event(&controller);
        assert_eq!(started.session_id, Some(manual));
        assert_eq!(started.mode, Some(RecognitionMode::Manual));
        assert_eq!(started.kind, RecognitionEventKind::SessionStarted);

        let duplicate = controller
            .commands
            .start_auto(RecognitionContext::Background)
            .unwrap();
        let rejected = next_event(&controller);
        assert_eq!(rejected.session_id, Some(duplicate));
        assert!(matches!(
            rejected.kind,
            RecognitionEventKind::CommandRejected { .. }
        ));

        controller
            .commands
            .submit(RecognitionCommand::StopRecognition { session_id: manual })
            .unwrap();
        let stopped = next_event(&controller);
        assert_eq!(
            stopped.kind,
            RecognitionEventKind::SessionEnded {
                reason: SessionEndReason::Stopped
            }
        );

        let automatic = controller
            .commands
            .start_auto(RecognitionContext::Foreground)
            .unwrap();
        assert_eq!(next_event(&controller).session_id, Some(automatic));
        controller
            .commands
            .submit(RecognitionCommand::SetRecognitionContext {
                session_id: automatic,
                context: RecognitionContext::Background,
            })
            .unwrap();
        controller
            .commands
            .submit(RecognitionCommand::CancelRecognition {
                session_id: automatic,
            })
            .unwrap();
        assert_eq!(
            next_event(&controller).kind,
            RecognitionEventKind::SessionCancelled
        );
        assert_eq!(
            next_event(&controller).kind,
            RecognitionEventKind::SessionEnded {
                reason: SessionEndReason::Cancelled
            }
        );
        assert_eq!(
            session_events.lock().unwrap().as_ref().unwrap().emit(
                Some(automatic),
                Some(RecognitionMode::Auto),
                RecognitionEventKind::MatchFound {
                    attempt_id: 1,
                    title: Some("stale".into()),
                    artist: None,
                    track_id: None,
                },
            ),
            Err(EventError::ControllerStopped)
        );

        assert!(controller.join().unwrap().is_ok());
        let calls = observed.lock().unwrap();
        assert!(calls.iter().any(|call| call.starts_with("stop:")));
        assert!(calls.iter().any(|call| call.starts_with("cancel:")));
        assert!(calls.iter().any(|call| call.contains("Background")));
        assert_eq!(calls.last().map(String::as_str), Some("shutdown"));
    }

    #[test]
    fn event_emitter_is_bounded_and_sequences_successful_events() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let emitter = EventEmitter {
            sender,
            sequence: Arc::new(AtomicU64::new(1)),
            origin: Instant::now(),
            order_lock: Arc::new(Mutex::new(())),
            session_gate: None,
        };
        emitter
            .emit(None, None, RecognitionEventKind::SessionStarted)
            .unwrap();
        assert_eq!(
            emitter.emit(None, None, RecognitionEventKind::CaptureStarted),
            Err(EventError::QueueFull)
        );
        assert_eq!(receiver.recv().unwrap().sequence, 1);
        assert!(emitter
            .emit(None, None, RecognitionEventKind::CaptureStarted)
            .is_ok());
        assert_eq!(receiver.recv().unwrap().sequence, 3);
    }
}
