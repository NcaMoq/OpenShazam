//! UI-independent, bounded command and event boundary for recognition apps.
//!
//! UI callers enqueue commands with `try_send` and poll events without waiting.
//! The backend runs on a dedicated controller thread. A backend must hand audio
//! capture and HTTP work to their own workers and return promptly from command
//! methods so the controller remains responsive.

use std::{
    io,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub use shazam_http_lab::{
    NativeAutoResultEvent, NativeAutoUiEvent, NativeManualResultEvent, NativeManualResultStage,
    NativeManualUiEvent, NativeTrackUiMetadata, TrackCandidate, TrackCandidateOrigin,
    UiConfigSnapshot, UiRecognitionEvent, UiRecognitionInput,
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
    StartManualRecognitionWithConfig {
        session_id: SessionId,
        config: UiConfigSnapshot,
    },
    StartAutoRecognitionWithConfig {
        session_id: SessionId,
        context: RecognitionContext,
        config: UiConfigSnapshot,
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
    /// Existing Manual callback-stage data, including Track Key and artwork.
    ManualUiEvent(NativeManualUiEvent),
    /// Auto's callback candidate; this does not claim Native duplicate filtering.
    AutoUiEvent(NativeAutoUiEvent),
    /// Session-scoped Config for UI PageLinkResolver use. URL resolution stays
    /// in the UI layer.
    UiConfigSnapshot(UiConfigSnapshot),
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
    NoMatch,
    TimedOut,
    Exhausted,
    Failed,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecognitionEvent {
    pub sequence: u64,
    pub session_id: Option<SessionId>,
    pub mode: Option<RecognitionMode>,
    /// Copied from the immutable Config attached to this session at Start.
    pub config_generation: Option<u64>,
    /// Monotonic time since this controller started. This is event creation
    /// time, not the later time at which the UI dequeues the event.
    pub elapsed: Duration,
    pub kind: RecognitionEventKind,
}

impl RecognitionEvent {
    /// Returns the existing typed UI result paired with the generation carried
    /// by this session event. Session identity remains on this envelope.
    pub fn ui_recognition_input(&self) -> Option<UiRecognitionInput> {
        let config_generation = self.config_generation?;
        let event = match &self.kind {
            RecognitionEventKind::ManualUiEvent(event) => UiRecognitionEvent::Manual(event.clone()),
            RecognitionEventKind::AutoUiEvent(event) => UiRecognitionEvent::Auto(event.clone()),
            _ => return None,
        };
        Some(UiRecognitionInput {
            config_generation,
            event,
        })
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Starting,
    Running,
    Stopping,
    Cancelling,
    Faulted,
    Ended(SessionEndReason),
}

/// Authoritative bounded state retained independently of the UI notification
/// queue. UI hosts poll this after a sequence gap or QueueFull report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecognitionSessionSnapshot {
    pub session_id: SessionId,
    pub mode: RecognitionMode,
    pub context: Option<RecognitionContext>,
    pub config: Option<UiConfigSnapshot>,
    pub phase: SessionPhase,
    pub last_result: Option<UiRecognitionEvent>,
    pub last_event: Option<RecognitionEvent>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControllerSnapshot {
    /// Increments for every event attempt, including a notification dropped
    /// because the bounded UI queue was full.
    pub last_sequence: u64,
    pub dropped_notifications: u64,
    pub active_session: Option<RecognitionSessionSnapshot>,
    /// Most recent ended session, retained until another one ends.
    pub last_ended_session: Option<RecognitionSessionSnapshot>,
}

const SESSION_OPEN: u8 = 0;
const SESSION_CLOSING: u8 = 1;
const SESSION_ENDED: u8 = 2;

/// Platform adapter contract. Implementations own WASAPI/CoreAudio and network
/// workers. `finish` must be emitted only after the capture and request workers
/// for that session have released their resources. Never call `EventEmitter`
/// from a real-time audio callback; forward events from the backend worker.
/// `stop` drains requests, while `cancel` invalidates the session and aborts
/// requests where supported. Methods run on the controller worker, so start
/// long-running work on dedicated workers and reserve waits for stop/shutdown.
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

    /// Starts a session bound to the caller-owned Config generation. The
    /// snapshot value is immutable for this session even when another Config
    /// is fetched later.
    pub fn start_manual_with_config(
        &self,
        config: UiConfigSnapshot,
    ) -> Result<SessionId, SubmitError> {
        let id = self.allocate_session_id();
        self.submit(RecognitionCommand::StartManualRecognitionWithConfig {
            session_id: id,
            config,
        })?;
        Ok(id)
    }

    pub fn start_auto_with_config(
        &self,
        context: RecognitionContext,
        config: UiConfigSnapshot,
    ) -> Result<SessionId, SubmitError> {
        let id = self.allocate_session_id();
        self.submit(RecognitionCommand::StartAutoRecognitionWithConfig {
            session_id: id,
            context,
            config,
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
    snapshot: Arc<Mutex<ControllerSnapshot>>,
    session_gate: Option<(SessionId, RecognitionMode, Arc<AtomicU8>)>,
    config_generation: Option<u64>,
}

impl EventEmitter {
    fn for_session(
        &self,
        session_id: SessionId,
        mode: RecognitionMode,
        gate: Arc<AtomicU8>,
        config_generation: Option<u64>,
    ) -> Self {
        let mut scoped = self.clone();
        scoped.session_gate = Some((session_id, mode, gate));
        scoped.config_generation = config_generation;
        scoped
    }

    fn for_lifecycle(&self, config_generation: Option<u64>) -> Self {
        let mut scoped = self.clone();
        scoped.session_gate = None;
        scoped.config_generation = config_generation;
        // Lifecycle emitters are used only by the controller worker after the
        // backend gate has been closed or after backend cleanup has completed.
        scoped
    }

    /// Completes a session after the backend has stopped capture and released
    /// its session resources. Calling this twice or after Stop/Cancel is safe:
    /// only the first terminal transition can be published.
    pub fn finish(&self, reason: SessionEndReason) -> Result<(), EventError> {
        let Some((session_id, mode, _)) = &self.session_gate else {
            return Err(EventError::ControllerStopped);
        };
        self.emit(
            Some(*session_id),
            Some(*mode),
            RecognitionEventKind::SessionEnded { reason },
        )
    }

    pub fn emit(
        &self,
        session_id: Option<SessionId>,
        mode: Option<RecognitionMode>,
        kind: RecognitionEventKind,
    ) -> Result<(), EventError> {
        if let Some((allowed_session, allowed_mode, gate)) = &self.session_gate {
            if gate.load(Ordering::Acquire) != SESSION_OPEN
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
            .is_some_and(|(_, _, gate)| gate.load(Ordering::Acquire) != SESSION_OPEN)
        {
            return Err(EventError::ControllerStopped);
        }
        if matches!(kind, RecognitionEventKind::SessionEnded { .. }) {
            if let Some((_, _, gate)) = &self.session_gate {
                gate.compare_exchange(
                    SESSION_OPEN,
                    SESSION_ENDED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .map_err(|_| EventError::ControllerStopped)?;
            }
        }
        let event = RecognitionEvent {
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            session_id,
            mode,
            config_generation: self.config_generation,
            elapsed: self.origin.elapsed(),
            kind,
        };
        update_snapshot(&self.snapshot, &event);
        match self.sender.try_send(event) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                let mut snapshot = self.snapshot.lock().expect("snapshot lock poisoned");
                snapshot.dropped_notifications = snapshot.dropped_notifications.saturating_add(1);
                Err(EventError::QueueFull)
            }
            Err(TrySendError::Disconnected(_)) => Err(EventError::ControllerStopped),
        }
    }
}

fn update_snapshot(snapshot: &Mutex<ControllerSnapshot>, event: &RecognitionEvent) {
    let mut snapshot = snapshot.lock().expect("snapshot lock poisoned");
    snapshot.last_sequence = event.sequence;
    let Some(active) = snapshot.active_session.as_mut() else {
        return;
    };
    if Some(active.session_id) != event.session_id {
        return;
    }
    active.last_event = Some(event.clone());
    match &event.kind {
        RecognitionEventKind::SessionStarted => active.phase = SessionPhase::Running,
        RecognitionEventKind::ManualUiEvent(result) => {
            active.last_result = Some(UiRecognitionEvent::Manual(result.clone()));
        }
        RecognitionEventKind::AutoUiEvent(result) => {
            active.last_result = Some(UiRecognitionEvent::Auto(result.clone()));
        }
        RecognitionEventKind::MatchFound { .. } => {}
        RecognitionEventKind::SessionEnded { reason } => {
            active.phase = SessionPhase::Ended(*reason);
            snapshot.last_ended_session = snapshot.active_session.take();
        }
        _ => {}
    }
}

pub struct ApplicationController {
    pub commands: CommandHandle,
    pub events: EventReceiver,
    worker: Option<JoinHandle<Result<(), String>>>,
}

pub struct EventReceiver(Receiver<RecognitionEvent>, Arc<Mutex<ControllerSnapshot>>);

impl EventReceiver {
    /// Polls without blocking the UI thread.
    pub fn try_recv(&self) -> Result<RecognitionEvent, TryRecvError> {
        self.0.try_recv()
    }

    /// Returns the authoritative bounded session state. Use this to recover
    /// after a sequence gap or `EventError::QueueFull`; it does not consume
    /// notifications.
    pub fn snapshot(&self) -> ControllerSnapshot {
        self.1.lock().expect("snapshot lock poisoned").clone()
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
        let snapshot = Arc::new(Mutex::new(ControllerSnapshot::default()));
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
            snapshot: Arc::clone(&snapshot),
            session_gate: None,
            config_generation: None,
        };
        let worker_snapshot = Arc::clone(&snapshot);
        let worker = thread::Builder::new()
            .name("openshazam-controller".into())
            .spawn(move || {
                let result = run_controller(
                    &mut backend,
                    command_receiver,
                    events.clone(),
                    shutdown_requested,
                    Arc::clone(&worker_snapshot),
                );
                let shutdown_result = backend.shutdown();
                if shutdown_result.is_ok() {
                    finish_after_shutdown(&events, &worker_snapshot);
                }
                result.and(shutdown_result)
            })?;
        Ok(Self {
            commands,
            events: EventReceiver(event_receiver, snapshot),
            worker: Some(worker),
        })
    }

    /// Joins after a shutdown request. Call this from a background thread.
    pub fn join(mut self) -> thread::Result<Result<(), String>> {
        self.commands.request_shutdown();
        self.worker.take().expect("controller worker exists").join()
    }
}

struct ActiveSession {
    id: SessionId,
    mode: RecognitionMode,
    context: Option<RecognitionContext>,
    config: Option<UiConfigSnapshot>,
    gate: Arc<AtomicU8>,
}

fn run_controller<B: RecognitionBackend>(
    backend: &mut B,
    commands: Receiver<WorkerMessage>,
    events: EventEmitter,
    shutdown_requested: Arc<AtomicBool>,
    snapshot: Arc<Mutex<ControllerSnapshot>>,
) -> Result<(), String> {
    let mut active: Option<ActiveSession> = None;
    let mut shutdown_error = None;
    loop {
        if active
            .as_ref()
            .is_some_and(|session| session.gate.load(Ordering::Acquire) == SESSION_ENDED)
        {
            active = None;
        }
        if shutdown_requested.load(Ordering::Acquire) {
            break;
        }
        let command = match commands.recv_timeout(Duration::from_millis(20)) {
            Ok(WorkerMessage::Command(command)) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        // A backend can finish while `recv_timeout` is blocked. Reconcile the
        // gate again after receiving so a queued next Start is not rejected
        // merely because the worker woke for that command before its timer.
        if active
            .as_ref()
            .is_some_and(|session| session.gate.load(Ordering::Acquire) == SESSION_ENDED)
        {
            active = None;
        }

        let start = match &command {
            RecognitionCommand::StartManualRecognition { session_id } => {
                Some((*session_id, RecognitionMode::Manual, None, None))
            }
            RecognitionCommand::StartAutoRecognition {
                session_id,
                context,
            } => Some((*session_id, RecognitionMode::Auto, Some(*context), None)),
            RecognitionCommand::StartManualRecognitionWithConfig { session_id, config } => Some((
                *session_id,
                RecognitionMode::Manual,
                None,
                Some(config.clone()),
            )),
            RecognitionCommand::StartAutoRecognitionWithConfig {
                session_id,
                context,
                config,
            } => Some((
                *session_id,
                RecognitionMode::Auto,
                Some(*context),
                Some(config.clone()),
            )),
            _ => None,
        };

        if let Some((session_id, mode, context, config)) = start {
            let config_generation = config.as_ref().map(|config| config.generation);
            if let Some(existing) = active.as_ref() {
                emit_nonfatal(
                    &events.for_lifecycle(config_generation),
                    Some(session_id),
                    Some(mode),
                    RecognitionEventKind::CommandRejected {
                        reason: format!("session {:?} is already active", existing.id),
                    },
                );
                continue;
            }
            let gate = Arc::new(AtomicU8::new(SESSION_OPEN));
            let session_events =
                events.for_session(session_id, mode, Arc::clone(&gate), config_generation);
            {
                let mut state = snapshot.lock().expect("snapshot lock poisoned");
                state.active_session = Some(RecognitionSessionSnapshot {
                    session_id,
                    mode,
                    context,
                    config: config.clone(),
                    phase: SessionPhase::Starting,
                    last_result: None,
                    last_event: None,
                });
            }
            active = Some(ActiveSession {
                id: session_id,
                mode,
                context,
                config: config.clone(),
                gate: Arc::clone(&gate),
            });
            emit_nonfatal(
                &session_events,
                Some(session_id),
                Some(mode),
                RecognitionEventKind::SessionStarted,
            );
            if let Some(config) = &config {
                emit_nonfatal(
                    &session_events,
                    Some(session_id),
                    Some(mode),
                    RecognitionEventKind::UiConfigSnapshot(config.clone()),
                );
            }
            match backend.start(session_id, mode, context, session_events) {
                Ok(()) => {}
                Err(message) => {
                    if gate
                        .compare_exchange(
                            SESSION_OPEN,
                            SESSION_CLOSING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        let cancel_error = backend.cancel(session_id).err();
                        let error_message = match cancel_error.as_deref() {
                            Some(cleanup) => format!("{message}; cleanup failed: {cleanup}"),
                            None => message,
                        };
                        let lifecycle = events.for_lifecycle(config_generation);
                        emit_nonfatal(
                            &lifecycle,
                            Some(session_id),
                            Some(mode),
                            RecognitionEventKind::RecognitionError {
                                kind: RecognitionErrorKind::Internal,
                                message: error_message,
                            },
                        );
                        if cancel_error.is_none() {
                            end_session(
                                &events,
                                &snapshot,
                                session_id,
                                mode,
                                config_generation,
                                SessionEndReason::Failed,
                            );
                            active = None;
                        } else {
                            set_session_phase(&snapshot, session_id, SessionPhase::Faulted);
                        }
                    }
                }
            }
            continue;
        }

        match command {
            RecognitionCommand::StopRecognition { session_id } => {
                if let Some(session) = active.as_mut().filter(|session| session.id == session_id) {
                    let config_generation = session.config.as_ref().map(|config| config.generation);
                    if session
                        .gate
                        .compare_exchange(
                            SESSION_OPEN,
                            SESSION_CLOSING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                        || session.gate.load(Ordering::Acquire) == SESSION_CLOSING
                    {
                        set_session_phase(&snapshot, session_id, SessionPhase::Stopping);
                        match backend.stop(session_id) {
                            Ok(()) => {
                                end_session(
                                    &events,
                                    &snapshot,
                                    session_id,
                                    session.mode,
                                    config_generation,
                                    SessionEndReason::Stopped,
                                );
                                active = None;
                            }
                            Err(message) => {
                                emit_nonfatal(
                                    &events.for_lifecycle(config_generation),
                                    Some(session_id),
                                    Some(session.mode),
                                    RecognitionEventKind::RecognitionError {
                                        kind: RecognitionErrorKind::Internal,
                                        message: message.clone(),
                                    },
                                );
                                match backend.cancel(session_id) {
                                    Ok(()) => {
                                        end_session(
                                            &events,
                                            &snapshot,
                                            session_id,
                                            session.mode,
                                            config_generation,
                                            SessionEndReason::Failed,
                                        );
                                        active = None;
                                    }
                                    Err(cleanup) => {
                                        set_session_phase(
                                            &snapshot,
                                            session_id,
                                            SessionPhase::Faulted,
                                        );
                                        shutdown_error.get_or_insert_with(|| {
                                            format!(
                                                "stop failed: {message}; cancel failed: {cleanup}"
                                            )
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            RecognitionCommand::CancelRecognition { session_id } => {
                if let Some(session) = active.as_mut().filter(|session| session.id == session_id) {
                    let config_generation = session.config.as_ref().map(|config| config.generation);
                    match session.gate.compare_exchange(
                        SESSION_OPEN,
                        SESSION_CLOSING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Err(SESSION_ENDED) => {
                            active = None;
                            continue;
                        }
                        Ok(_) | Err(SESSION_CLOSING) => {}
                        Err(_) => unreachable!("unknown session gate state"),
                    }
                    if session.gate.load(Ordering::Acquire) == SESSION_ENDED {
                        active = None;
                        continue;
                    }
                    set_session_phase(&snapshot, session_id, SessionPhase::Cancelling);
                    match backend.cancel(session_id) {
                        Ok(()) => {
                            emit_nonfatal(
                                &events.for_lifecycle(config_generation),
                                Some(session_id),
                                Some(session.mode),
                                RecognitionEventKind::SessionCancelled,
                            );
                            end_session(
                                &events,
                                &snapshot,
                                session_id,
                                session.mode,
                                config_generation,
                                SessionEndReason::Cancelled,
                            );
                            active = None;
                        }
                        Err(message) => {
                            emit_nonfatal(
                                &events.for_lifecycle(config_generation),
                                Some(session_id),
                                Some(session.mode),
                                RecognitionEventKind::RecognitionError {
                                    kind: RecognitionErrorKind::Internal,
                                    message: message.clone(),
                                },
                            );
                            set_session_phase(&snapshot, session_id, SessionPhase::Faulted);
                            shutdown_error.get_or_insert(message);
                        }
                    }
                }
            }
            RecognitionCommand::SetRecognitionContext {
                session_id,
                context,
            } => {
                if let Some(session) = active.as_mut().filter(|session| {
                    session.id == session_id && session.mode == RecognitionMode::Auto
                }) {
                    if session.gate.load(Ordering::Acquire) == SESSION_OPEN {
                        let config_generation =
                            session.config.as_ref().map(|config| config.generation);
                        match backend.set_context(session_id, context) {
                            Ok(()) => {
                                session.context = Some(context);
                                if let Some(state) = snapshot
                                    .lock()
                                    .expect("snapshot lock poisoned")
                                    .active_session
                                    .as_mut()
                                {
                                    if state.session_id == session_id {
                                        state.context = Some(context);
                                    }
                                }
                            }
                            Err(message) => emit_nonfatal(
                                &events.for_lifecycle(config_generation),
                                Some(session_id),
                                Some(session.mode),
                                RecognitionEventKind::RecognitionError {
                                    kind: RecognitionErrorKind::Internal,
                                    message,
                                },
                            ),
                        }
                    }
                }
            }
            RecognitionCommand::Shutdown => break,
            RecognitionCommand::StartManualRecognition { .. }
            | RecognitionCommand::StartAutoRecognition { .. }
            | RecognitionCommand::StartManualRecognitionWithConfig { .. }
            | RecognitionCommand::StartAutoRecognitionWithConfig { .. } => unreachable!(),
        }
    }

    if let Some(session) = active {
        match session.gate.compare_exchange(
            SESSION_OPEN,
            SESSION_CLOSING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Err(SESSION_ENDED) => return shutdown_error.map_or(Ok(()), Err),
            Ok(_) | Err(SESSION_CLOSING) => {}
            Err(_) => unreachable!("unknown session gate state"),
        }
        set_session_phase(&snapshot, session.id, SessionPhase::Cancelling);
        match backend.cancel(session.id) {
            Ok(()) => end_session(
                &events,
                &snapshot,
                session.id,
                session.mode,
                session.config.as_ref().map(|config| config.generation),
                SessionEndReason::Shutdown,
            ),
            Err(message) => {
                set_session_phase(&snapshot, session.id, SessionPhase::Faulted);
                emit_nonfatal(
                    &events.for_lifecycle(session.config.as_ref().map(|config| config.generation)),
                    Some(session.id),
                    Some(session.mode),
                    RecognitionEventKind::RecognitionError {
                        kind: RecognitionErrorKind::Internal,
                        message: message.clone(),
                    },
                );
                shutdown_error.get_or_insert(message);
            }
        }
    }
    shutdown_error.map_or(Ok(()), Err)
}

fn emit_nonfatal(
    events: &EventEmitter,
    session_id: Option<SessionId>,
    mode: Option<RecognitionMode>,
    kind: RecognitionEventKind,
) {
    // QueueFull means the event is represented by the authoritative snapshot
    // and the incremented sequence/dropped counter. It is not a controller
    // failure. A stale session gate is also an expected race outcome.
    let _ = events.emit(session_id, mode, kind);
}

fn set_session_phase(
    snapshot: &Mutex<ControllerSnapshot>,
    session_id: SessionId,
    phase: SessionPhase,
) {
    if let Some(session) = snapshot
        .lock()
        .expect("snapshot lock poisoned")
        .active_session
        .as_mut()
        .filter(|session| session.session_id == session_id)
    {
        session.phase = phase;
    }
}

fn end_session(
    events: &EventEmitter,
    snapshot: &Mutex<ControllerSnapshot>,
    session_id: SessionId,
    mode: RecognitionMode,
    config_generation: Option<u64>,
    reason: SessionEndReason,
) {
    emit_nonfatal(
        &events.for_lifecycle(config_generation),
        Some(session_id),
        Some(mode),
        RecognitionEventKind::SessionEnded { reason },
    );
    // `update_snapshot` moves the active state to `last_ended_session` before
    // enqueueing; if a duplicate/stale lifecycle notification was rejected,
    // avoid manufacturing a second terminal state here.
    let mut state = snapshot.lock().expect("snapshot lock poisoned");
    if state
        .active_session
        .as_ref()
        .is_some_and(|session| session.session_id == session_id)
    {
        if let Some(mut ended) = state.active_session.take() {
            ended.phase = SessionPhase::Ended(reason);
            state.last_ended_session = Some(ended);
        }
    }
}

fn finish_after_shutdown(events: &EventEmitter, snapshot: &Mutex<ControllerSnapshot>) {
    let active = snapshot
        .lock()
        .expect("snapshot lock poisoned")
        .active_session
        .clone();
    if let Some(session) = active {
        end_session(
            events,
            snapshot,
            session.session_id,
            session.mode,
            session.config.as_ref().map(|config| config.generation),
            SessionEndReason::Shutdown,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, sync::Mutex};

    #[derive(Clone, Default)]
    struct MockBackend {
        calls: Arc<Mutex<Vec<String>>>,
        session_events: Arc<Mutex<HashMap<SessionId, EventEmitter>>>,
    }

    impl RecognitionBackend for MockBackend {
        fn start(
            &mut self,
            session_id: SessionId,
            mode: RecognitionMode,
            _context: Option<RecognitionContext>,
            events: EventEmitter,
        ) -> Result<(), String> {
            self.session_events
                .lock()
                .unwrap()
                .insert(session_id, events);
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

    fn candidate() -> TrackCandidate {
        TrackCandidate {
            track_id: Some("opaque-id".into()),
            title: "Title".into(),
            artist: "Artist".into(),
            ui_metadata: Some(NativeTrackUiMetadata {
                track_key: Some("track-key".into()),
                album_adam_id: None,
                isrc: None,
                date: None,
                artwork_url: Some("artwork".into()),
                hq_artwork_url: None,
                background_image_url: None,
            }),
            alternative_track: None,
            origin: TrackCandidateOrigin::Synthetic,
        }
    }

    fn session_emitter(
        emitters: &Arc<Mutex<HashMap<SessionId, EventEmitter>>>,
        session_id: SessionId,
    ) -> EventEmitter {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(emitter) = emitters.lock().unwrap().get(&session_id).cloned() {
                return emitter;
            }
            assert!(
                Instant::now() < deadline,
                "backend did not start the session"
            );
            thread::yield_now();
        }
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
            session_events.lock().unwrap()[&automatic].emit(
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
    fn natural_terminal_event_releases_controller_for_a_new_mode() {
        let backend = MockBackend::default();
        let emitters = Arc::clone(&backend.session_events);
        let controller = ApplicationController::spawn(backend, 8, 8).unwrap();
        let manual = controller.commands.start_manual().unwrap();
        assert_eq!(
            next_event(&controller).kind,
            RecognitionEventKind::SessionStarted
        );
        session_emitter(&emitters, manual)
            .finish(SessionEndReason::NoMatch)
            .unwrap();
        assert_eq!(
            next_event(&controller).kind,
            RecognitionEventKind::SessionEnded {
                reason: SessionEndReason::NoMatch
            }
        );
        let auto = controller
            .commands
            .start_auto(RecognitionContext::Foreground)
            .unwrap();
        assert_eq!(next_event(&controller).session_id, Some(auto));
        assert!(controller.events.snapshot().active_session.is_some());
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn matched_and_timed_out_manual_sessions_can_be_restarted() {
        let backend = MockBackend::default();
        let emitters = Arc::clone(&backend.session_events);
        let controller = ApplicationController::spawn(backend, 8, 16).unwrap();
        let matched = controller.commands.start_manual().unwrap();
        next_event(&controller);
        let old = session_emitter(&emitters, matched);
        old.finish(SessionEndReason::Matched).unwrap();
        assert_eq!(
            next_event(&controller).kind,
            RecognitionEventKind::SessionEnded {
                reason: SessionEndReason::Matched
            }
        );
        let timed = controller.commands.start_manual().unwrap();
        assert_eq!(next_event(&controller).session_id, Some(timed));
        session_emitter(&emitters, timed)
            .finish(SessionEndReason::TimedOut)
            .unwrap();
        assert_eq!(
            next_event(&controller).kind,
            RecognitionEventKind::SessionEnded {
                reason: SessionEndReason::TimedOut
            }
        );
        let next = controller.commands.start_manual().unwrap();
        assert_eq!(next_event(&controller).session_id, Some(next));
        assert_eq!(
            old.finish(SessionEndReason::Failed),
            Err(EventError::ControllerStopped)
        );
        assert_eq!(
            controller
                .events
                .snapshot()
                .active_session
                .as_ref()
                .unwrap()
                .session_id,
            next
        );
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn auto_ui_match_is_nonterminal_and_explicit_stop_ends_once() {
        let backend = MockBackend::default();
        let emitters = Arc::clone(&backend.session_events);
        let controller = ApplicationController::spawn(backend, 8, 8).unwrap();
        let auto = controller
            .commands
            .start_auto(RecognitionContext::Background)
            .unwrap();
        next_event(&controller);
        let emitter = session_emitter(&emitters, auto);
        emitter
            .emit(
                Some(auto),
                Some(RecognitionMode::Auto),
                RecognitionEventKind::AutoUiEvent(NativeAutoUiEvent::ContinuousMatchCandidate(
                    NativeAutoResultEvent {
                        primary_track: candidate(),
                        alternative_track: None,
                    },
                )),
            )
            .unwrap();
        assert!(matches!(
            next_event(&controller).kind,
            RecognitionEventKind::AutoUiEvent(_)
        ));
        assert_eq!(
            controller
                .events
                .snapshot()
                .active_session
                .as_ref()
                .unwrap()
                .session_id,
            auto
        );
        controller
            .commands
            .submit(RecognitionCommand::StopRecognition { session_id: auto })
            .unwrap();
        assert_eq!(
            next_event(&controller).kind,
            RecognitionEventKind::SessionEnded {
                reason: SessionEndReason::Stopped
            }
        );
        assert_eq!(
            emitter.finish(SessionEndReason::Matched),
            Err(EventError::ControllerStopped)
        );
        let manual = controller.commands.start_manual().unwrap();
        assert_eq!(next_event(&controller).session_id, Some(manual));
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn queue_full_does_not_kill_controller_and_snapshot_recovers_terminal_result() {
        let backend = MockBackend::default();
        let emitters = Arc::clone(&backend.session_events);
        let controller = ApplicationController::spawn(backend, 8, 1).unwrap();
        let session = controller
            .commands
            .start_manual_with_config(UiConfigSnapshot {
                generation: 9,
                web_track_url_template: Some("https://example.test/{track_key}".into()),
            })
            .unwrap();
        let emitter = session_emitter(&emitters, session);
        // Do not drain the one-slot queue: backend notifications are nonfatal
        // and the authoritative snapshot retains the latest result and finish.
        let result = NativeManualUiEvent::NoMatch;
        let _ = emitter.emit(
            Some(session),
            Some(RecognitionMode::Manual),
            RecognitionEventKind::ManualUiEvent(result),
        );
        let _ = emitter.finish(SessionEndReason::NoMatch);
        let snapshot = controller.events.snapshot();
        assert!(snapshot.dropped_notifications >= 1);
        assert_eq!(
            snapshot.last_ended_session.as_ref().unwrap().session_id,
            session
        );
        assert_eq!(
            snapshot.last_ended_session.as_ref().unwrap().phase,
            SessionPhase::Ended(SessionEndReason::NoMatch)
        );
        assert_eq!(
            snapshot.last_ended_session.as_ref().unwrap().last_result,
            Some(UiRecognitionEvent::Manual(NativeManualUiEvent::NoMatch))
        );
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn session_config_generation_and_track_metadata_are_bound_to_event() {
        let backend = MockBackend::default();
        let emitters = Arc::clone(&backend.session_events);
        let controller = ApplicationController::spawn(backend, 8, 8).unwrap();
        let config = UiConfigSnapshot {
            generation: 44,
            web_track_url_template: Some("https://example.test/{track_key}".into()),
        };
        let session = controller
            .commands
            .start_manual_with_config(config.clone())
            .unwrap();
        next_event(&controller);
        next_event(&controller);
        let event = NativeManualUiEvent::Result(NativeManualResultEvent {
            stage: NativeManualResultStage::TaggingDidFinish,
            primary_track: candidate(),
            alternative_track: None,
        });
        session_emitter(&emitters, session)
            .emit(
                Some(session),
                Some(RecognitionMode::Manual),
                RecognitionEventKind::ManualUiEvent(event.clone()),
            )
            .unwrap();
        let result = next_event(&controller);
        assert_eq!(result.config_generation, Some(44));
        assert_eq!(
            result.ui_recognition_input(),
            Some(UiRecognitionInput {
                config_generation: 44,
                event: UiRecognitionEvent::Manual(event)
            })
        );
        assert_eq!(
            controller.events.snapshot().active_session.unwrap().config,
            Some(config)
        );
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn backend_failure_and_shutdown_have_terminal_snapshot_state() {
        let backend = MockBackend::default();
        let emitters = Arc::clone(&backend.session_events);
        let controller = ApplicationController::spawn(backend, 8, 8).unwrap();
        let snapshot_handle = Arc::clone(&controller.events.1);
        let session = controller
            .commands
            .start_auto(RecognitionContext::Foreground)
            .unwrap();
        next_event(&controller);
        let events = session_emitter(&emitters, session);
        events
            .emit(
                Some(session),
                Some(RecognitionMode::Auto),
                RecognitionEventKind::RecognitionError {
                    kind: RecognitionErrorKind::Transport,
                    message: "offline".into(),
                },
            )
            .unwrap();
        next_event(&controller);
        events.finish(SessionEndReason::Failed).unwrap();
        assert_eq!(
            next_event(&controller).kind,
            RecognitionEventKind::SessionEnded {
                reason: SessionEndReason::Failed
            }
        );
        let active = controller.commands.start_manual().unwrap();
        next_event(&controller);
        assert_eq!(
            controller
                .events
                .snapshot()
                .active_session
                .as_ref()
                .unwrap()
                .session_id,
            active
        );
        assert!(controller.join().unwrap().is_ok());
        let snapshot = snapshot_handle.lock().unwrap().clone();
        assert_eq!(
            snapshot.last_ended_session.as_ref().unwrap().session_id,
            active
        );
        assert_eq!(
            snapshot.last_ended_session.as_ref().unwrap().phase,
            SessionPhase::Ended(SessionEndReason::Shutdown)
        );
    }
}
