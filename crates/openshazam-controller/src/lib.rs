//! UI-independent, bounded command and event boundary for recognition apps.
//!
//! UI callers enqueue commands with `try_send` and poll events without waiting.
//! The backend runs on a dedicated controller thread. A backend must hand audio
//! capture and HTTP work to their own workers and return promptly from command
//! methods so the controller remains responsive.

use std::{
    collections::VecDeque,
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

/// Monotonic identity for one submitted controller command. It is distinct
/// from `SessionId`: one recognition session may receive several commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandId(pub u64);

/// A nonblocking command submission receipt. `session_id` is allocated for a
/// Start command but does not mean the backend has accepted or started it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandTicket {
    pub command_id: CommandId,
    pub session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandStatus {
    /// The bounded command queue accepted the request; the backend has not
    /// necessarily inspected it yet.
    Submitted,
    /// The controller worker dequeued the command and accepted its intent.
    Accepted,
    /// `RecognitionBackend::start` is in progress.
    Starting,
    /// Backend startup returned successfully and its session worker is live.
    Running,
    Completed,
    Rejected {
        reason: String,
    },
    StartFailed {
        reason: String,
    },
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub command_id: CommandId,
    pub session_id: Option<SessionId>,
    pub status: CommandStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandResultLookup {
    Retained(CommandResult),
    /// This ticket was previously retained but fell outside the bounded ledger.
    Expired {
        evicted_through: CommandId,
    },
    /// No retained submission with this ID is known (including queue-full IDs).
    Unknown,
}

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
    /// The controller allocated the session slot. Check its command ticket to
    /// know whether backend startup later succeeded.
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
    CommandStatusChanged {
        command_id: CommandId,
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
    /// Present for events directly associated with an engine attempt.
    pub attempt_id: Option<u64>,
    /// Monotonic time since this controller started. This is event creation
    /// time, not the later time at which the UI dequeues the event.
    pub elapsed: Duration,
    pub kind: RecognitionEventKind,
}

impl RecognitionEvent {
    /// Borrowed UI event view for rendering without cloning Track or Artwork
    /// strings. The event envelope still supplies session and attempt IDs.
    pub fn ui_recognition_input_ref(&self) -> Option<UiRecognitionInputRef<'_>> {
        let config_generation = self.config_generation?;
        let event = match &self.kind {
            RecognitionEventKind::ManualUiEvent(event) => UiRecognitionEventRef::Manual(event),
            RecognitionEventKind::AutoUiEvent(event) => UiRecognitionEventRef::Auto(event),
            _ => return None,
        };
        Some(UiRecognitionInputRef {
            config_generation,
            event,
        })
    }

    /// Returns the existing typed UI result paired with the generation carried
    /// by this session event. This owned convenience form clones UI metadata;
    /// use `ui_recognition_input_ref` for rendering without those copies.
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

#[derive(Debug, Clone, Copy)]
pub struct UiRecognitionInputRef<'a> {
    pub config_generation: u64,
    pub event: UiRecognitionEventRef<'a>,
}

#[derive(Debug, Clone, Copy)]
pub enum UiRecognitionEventRef<'a> {
    Manual(&'a NativeManualUiEvent),
    Auto(&'a NativeAutoUiEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitError {
    QueueFull,
    ControllerStopped,
    CommandIdExhausted,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecognitionEventCategory {
    SessionStarted,
    CaptureStarted,
    SignatureReady,
    RequestStarted,
    RetryScheduled,
    MatchFound,
    ManualUiEvent,
    AutoUiEvent,
    UiConfigSnapshot,
    NoMatch,
    RecognitionError,
    SessionTimedOut,
    SessionCancelled,
    SessionEnded,
    CommandRejected,
    CommandStatusChanged,
}

/// Compact metadata for the latest session notification. UI payloads are
/// retained separately in `last_result`, avoiding a second copy in snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecognitionEventSummary {
    pub sequence: u64,
    pub attempt_id: Option<u64>,
    pub elapsed: Duration,
    pub category: RecognitionEventCategory,
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
    pub last_event: Option<RecognitionEventSummary>,
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
    /// Bounded command lifecycle records, oldest first.
    pub command_results: VecDeque<CommandResult>,
    pub command_results_capacity: usize,
    /// IDs at or below this point may have been evicted and cannot be queried.
    pub command_results_evicted_through: Option<CommandId>,
    pub command_results_revision: u64,
}

impl ControllerSnapshot {
    pub fn command_result(&self, command_id: CommandId) -> CommandResultLookup {
        // Zero is reserved for the controller's untracked shutdown wakeup and
        // can never represent a successfully submitted user command.
        if command_id.0 == 0 {
            return CommandResultLookup::Unknown;
        }
        if let Some(result) = self
            .command_results
            .iter()
            .find(|result| result.command_id == command_id)
        {
            CommandResultLookup::Retained(result.clone())
        } else if let Some(evicted_through) = self.command_results_evicted_through {
            if command_id <= evicted_through {
                return CommandResultLookup::Expired { evicted_through };
            }
            CommandResultLookup::Unknown
        } else {
            CommandResultLookup::Unknown
        }
    }
}

const SESSION_OPEN: u8 = 0;
const SESSION_CLOSING: u8 = 1;
const SESSION_ENDED: u8 = 2;
const MAX_COMMAND_REASON_BYTES: usize = 1_024;

fn bounded_reason(mut reason: String) -> String {
    if reason.len() > MAX_COMMAND_REASON_BYTES {
        let mut boundary = MAX_COMMAND_REASON_BYTES - 3;
        while !reason.is_char_boundary(boundary) {
            boundary -= 1;
        }
        reason.truncate(boundary);
        reason.push_str("...");
    }
    reason
}

/// Platform adapter contract. Implementations own WASAPI/CoreAudio and network
/// workers. `finish` must be emitted only after the capture and request workers
/// for that session have released their resources. Never call `EventEmitter`
/// from a real-time audio callback; forward events from the backend worker.
/// `start`, `stop`, `cancel`, and `set_context` must submit work and return
/// promptly. Stop/Cancel close result delivery immediately; after their audio
/// and HTTP workers have actually stopped, the backend calls the session's
/// `EventEmitter::finish`. `start` must return an error only after cleaning up
/// any partially created resources. `shutdown` may wait for workers because
/// the controller invokes it off the UI thread.
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
    Command {
        command_id: CommandId,
        command: RecognitionCommand,
    },
}

#[derive(Clone)]
pub struct CommandHandle {
    sender: SyncSender<WorkerMessage>,
    next_session_id: Arc<AtomicU64>,
    next_command_id: Arc<AtomicU64>,
    shutdown_requested: Arc<AtomicBool>,
    snapshot: Arc<Mutex<ControllerSnapshot>>,
}

impl CommandHandle {
    /// Submits a Manual Start and returns the command and session identities.
    /// The ticket proves queue submission only; query its command result to
    /// observe backend acceptance and startup.
    pub fn start_manual(&self) -> Result<CommandTicket, SubmitError> {
        self.start_manual_tracked()
    }

    pub fn start_manual_tracked(&self) -> Result<CommandTicket, SubmitError> {
        let id = self.allocate_session_id();
        let command_id =
            self.submit_tracked(RecognitionCommand::StartManualRecognition { session_id: id })?;
        Ok(CommandTicket {
            command_id,
            session_id: id,
        })
    }

    pub fn start_auto(&self, context: RecognitionContext) -> Result<CommandTicket, SubmitError> {
        self.start_auto_tracked(context)
    }

    /// Submits an Auto Start and returns its command receipt. The ticket only
    /// proves queue submission; inspect `command_result(ticket.command_id)` to
    /// observe backend acceptance and startup.
    pub fn start_auto_tracked(
        &self,
        context: RecognitionContext,
    ) -> Result<CommandTicket, SubmitError> {
        let id = self.allocate_session_id();
        let command_id = self.submit_tracked(RecognitionCommand::StartAutoRecognition {
            session_id: id,
            context,
        })?;
        Ok(CommandTicket {
            command_id,
            session_id: id,
        })
    }

    /// Starts a session bound to the caller-owned Config generation. The
    /// snapshot is the UI-safe subset (`generation` and optional page template)
    /// and is not the recognition engine's internal Shazam Config. Backends
    /// must obtain endpoint, INID, timing, and retry settings through their
    /// existing internal Config owner.
    pub fn start_manual_with_config(
        &self,
        config: UiConfigSnapshot,
    ) -> Result<CommandTicket, SubmitError> {
        self.start_manual_with_config_tracked(config)
    }

    pub fn start_manual_with_config_tracked(
        &self,
        config: UiConfigSnapshot,
    ) -> Result<CommandTicket, SubmitError> {
        let id = self.allocate_session_id();
        let command_id =
            self.submit_tracked(RecognitionCommand::StartManualRecognitionWithConfig {
                session_id: id,
                config,
            })?;
        Ok(CommandTicket {
            command_id,
            session_id: id,
        })
    }

    pub fn start_auto_with_config(
        &self,
        context: RecognitionContext,
        config: UiConfigSnapshot,
    ) -> Result<CommandTicket, SubmitError> {
        self.start_auto_with_config_tracked(context, config)
    }

    pub fn start_auto_with_config_tracked(
        &self,
        context: RecognitionContext,
        config: UiConfigSnapshot,
    ) -> Result<CommandTicket, SubmitError> {
        let id = self.allocate_session_id();
        let command_id =
            self.submit_tracked(RecognitionCommand::StartAutoRecognitionWithConfig {
                session_id: id,
                context,
                config,
            })?;
        Ok(CommandTicket {
            command_id,
            session_id: id,
        })
    }

    pub fn submit(&self, command: RecognitionCommand) -> Result<CommandId, SubmitError> {
        self.submit_tracked(command)
    }

    /// Enqueues without waiting and returns an ID that can be polled through
    /// `command_result`. Success means only that the bounded queue accepted it.
    pub fn submit_tracked(&self, command: RecognitionCommand) -> Result<CommandId, SubmitError> {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return Err(SubmitError::ControllerStopped);
        }
        let session_id = command_session_id(&command);
        let mut snapshot = self.snapshot.lock().expect("snapshot lock poisoned");
        if self.shutdown_requested.load(Ordering::Acquire) {
            return Err(SubmitError::ControllerStopped);
        }
        // All CommandHandle clones share this Snapshot mutex, so keeping the
        // candidate uncommitted until try_send succeeds gives concurrent
        // submitters unique IDs without consuming IDs for QueueFull failures.
        let raw_command_id = self.next_command_id.load(Ordering::Relaxed);
        if raw_command_id == 0 {
            return Err(SubmitError::CommandIdExhausted);
        }
        let command_id = CommandId(raw_command_id);
        match self.sender.try_send(WorkerMessage::Command {
            command_id,
            command,
        }) {
            Ok(()) => {
                retain_command_result(
                    &mut snapshot,
                    CommandResult {
                        command_id,
                        session_id,
                        status: CommandStatus::Submitted,
                    },
                );
                self.next_command_id.store(
                    raw_command_id.checked_add(1).unwrap_or(0),
                    Ordering::Relaxed,
                );
                Ok(command_id)
            }
            Err(TrySendError::Full(_)) => Err(SubmitError::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(SubmitError::ControllerStopped),
        }
    }

    pub fn command_result(&self, command_id: CommandId) -> CommandResultLookup {
        self.snapshot
            .lock()
            .expect("snapshot lock poisoned")
            .command_result(command_id)
    }

    /// Requests shutdown without waiting for the worker or active devices.
    pub fn request_shutdown(&self) {
        // Serialize shutdown with submissions using the same short-lived lock:
        // a command either enters the queue before shutdown and gets drained,
        // or observes the shutdown flag and is not accepted.
        let _snapshot = self.snapshot.lock().expect("snapshot lock poisoned");
        if !self.shutdown_requested.swap(true, Ordering::AcqRel) {
            let _ = self.sender.try_send(WorkerMessage::Command {
                command_id: CommandId(0),
                command: RecognitionCommand::Shutdown,
            });
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
        let Some((session_id, mode, gate)) = &self.session_gate else {
            return Err(EventError::ControllerStopped);
        };
        let _order = self.order_lock.lock().expect("event order lock poisoned");
        match gate.load(Ordering::Acquire) {
            SESSION_OPEN | SESSION_CLOSING => {}
            _ => return Err(EventError::ControllerStopped),
        }
        let previous = gate.swap(SESSION_ENDED, Ordering::AcqRel);
        if !matches!(previous, SESSION_OPEN | SESSION_CLOSING) {
            return Err(EventError::ControllerStopped);
        }
        self.send_event(
            Some(*session_id),
            Some(*mode),
            None,
            RecognitionEventKind::SessionEnded { reason },
        )
    }

    fn send_event(
        &self,
        session_id: Option<SessionId>,
        mode: Option<RecognitionMode>,
        attempt_id: Option<u64>,
        kind: RecognitionEventKind,
    ) -> Result<(), EventError> {
        let event = RecognitionEvent {
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            session_id,
            mode,
            config_generation: self.config_generation,
            attempt_id,
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

    pub fn emit(
        &self,
        session_id: Option<SessionId>,
        mode: Option<RecognitionMode>,
        kind: RecognitionEventKind,
    ) -> Result<(), EventError> {
        self.emit_attempt(session_id, mode, None, kind)
    }

    pub fn emit_attempt(
        &self,
        session_id: Option<SessionId>,
        mode: Option<RecognitionMode>,
        attempt_id: Option<u64>,
        kind: RecognitionEventKind,
    ) -> Result<(), EventError> {
        let kind = match kind {
            RecognitionEventKind::RecognitionError { kind, message } => {
                RecognitionEventKind::RecognitionError {
                    kind,
                    message: bounded_reason(message),
                }
            }
            RecognitionEventKind::CommandRejected { reason } => {
                RecognitionEventKind::CommandRejected {
                    reason: bounded_reason(reason),
                }
            }
            kind => kind,
        };
        if let RecognitionEventKind::SessionEnded { reason } = &kind {
            if let Some((allowed_session, allowed_mode, _)) = &self.session_gate {
                if session_id != Some(*allowed_session) || mode != Some(*allowed_mode) {
                    return Err(EventError::ControllerStopped);
                }
                return self.finish(*reason);
            }
        }
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
        self.send_event(session_id, mode, attempt_id, kind)
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
    active.last_event = Some(RecognitionEventSummary {
        sequence: event.sequence,
        attempt_id: event.attempt_id,
        elapsed: event.elapsed,
        category: match &event.kind {
            RecognitionEventKind::SessionStarted => RecognitionEventCategory::SessionStarted,
            RecognitionEventKind::CaptureStarted => RecognitionEventCategory::CaptureStarted,
            RecognitionEventKind::SignatureReady { .. } => RecognitionEventCategory::SignatureReady,
            RecognitionEventKind::RequestStarted { .. } => RecognitionEventCategory::RequestStarted,
            RecognitionEventKind::RetryScheduled { .. } => RecognitionEventCategory::RetryScheduled,
            RecognitionEventKind::MatchFound { .. } => RecognitionEventCategory::MatchFound,
            RecognitionEventKind::ManualUiEvent(_) => RecognitionEventCategory::ManualUiEvent,
            RecognitionEventKind::AutoUiEvent(_) => RecognitionEventCategory::AutoUiEvent,
            RecognitionEventKind::UiConfigSnapshot(_) => RecognitionEventCategory::UiConfigSnapshot,
            RecognitionEventKind::NoMatch { .. } => RecognitionEventCategory::NoMatch,
            RecognitionEventKind::RecognitionError { .. } => {
                RecognitionEventCategory::RecognitionError
            }
            RecognitionEventKind::SessionTimedOut => RecognitionEventCategory::SessionTimedOut,
            RecognitionEventKind::SessionCancelled => RecognitionEventCategory::SessionCancelled,
            RecognitionEventKind::SessionEnded { .. } => RecognitionEventCategory::SessionEnded,
            RecognitionEventKind::CommandRejected { .. } => {
                RecognitionEventCategory::CommandRejected
            }
            RecognitionEventKind::CommandStatusChanged { .. } => {
                RecognitionEventCategory::CommandStatusChanged
            }
        },
    });
    match &event.kind {
        RecognitionEventKind::SessionStarted => {}
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

fn command_session_id(command: &RecognitionCommand) -> Option<SessionId> {
    match command {
        RecognitionCommand::StartManualRecognition { session_id }
        | RecognitionCommand::StartManualRecognitionWithConfig { session_id, .. }
        | RecognitionCommand::StopRecognition { session_id }
        | RecognitionCommand::CancelRecognition { session_id }
        | RecognitionCommand::SetRecognitionContext { session_id, .. }
        | RecognitionCommand::StartAutoRecognition { session_id, .. }
        | RecognitionCommand::StartAutoRecognitionWithConfig { session_id, .. } => {
            Some(*session_id)
        }
        RecognitionCommand::Shutdown => None,
    }
}

fn retain_command_result(snapshot: &mut ControllerSnapshot, result: CommandResult) {
    if snapshot.command_results_capacity == 0 {
        return;
    }
    while snapshot.command_results.len() >= snapshot.command_results_capacity {
        if let Some(evicted) = snapshot.command_results.pop_front() {
            snapshot.command_results_evicted_through = Some(evicted.command_id);
        }
    }
    snapshot.command_results.push_back(result);
    snapshot.command_results_revision = snapshot.command_results_revision.saturating_add(1);
}

fn set_command_status(
    snapshot: &Mutex<ControllerSnapshot>,
    command_id: CommandId,
    status: CommandStatus,
) -> bool {
    let status = match status {
        CommandStatus::Rejected { reason } => CommandStatus::Rejected {
            reason: bounded_reason(reason),
        },
        CommandStatus::StartFailed { reason } => CommandStatus::StartFailed {
            reason: bounded_reason(reason),
        },
        CommandStatus::Failed { reason } => CommandStatus::Failed {
            reason: bounded_reason(reason),
        },
        status => status,
    };
    let mut snapshot = snapshot.lock().expect("snapshot lock poisoned");
    let Some(result) = snapshot
        .command_results
        .iter_mut()
        .find(|result| result.command_id == command_id)
    else {
        return false;
    };
    if matches!(
        &result.status,
        CommandStatus::Completed
            | CommandStatus::Rejected { .. }
            | CommandStatus::StartFailed { .. }
            | CommandStatus::Failed { .. }
    ) {
        return false;
    }
    result.status = status;
    snapshot.command_results_revision = snapshot.command_results_revision.saturating_add(1);
    true
}

fn update_command_status(
    events: &EventEmitter,
    snapshot: &Mutex<ControllerSnapshot>,
    command_id: CommandId,
    status: CommandStatus,
) {
    if set_command_status(snapshot, command_id, status) {
        emit_nonfatal(
            events,
            None,
            None,
            RecognitionEventKind::CommandStatusChanged { command_id },
        );
    }
}

fn complete_command_if_pending(
    events: &EventEmitter,
    snapshot: &Mutex<ControllerSnapshot>,
    command_id: CommandId,
) {
    let should_complete = {
        let state = snapshot.lock().expect("snapshot lock poisoned");
        matches!(
            state.command_result(command_id),
            CommandResultLookup::Retained(CommandResult {
                status: CommandStatus::Accepted | CommandStatus::Starting | CommandStatus::Running,
                ..
            })
        )
    };
    if should_complete {
        update_command_status(events, snapshot, command_id, CommandStatus::Completed);
    }
}

fn complete_ended_session(
    active: &mut Option<ActiveSession>,
    events: &EventEmitter,
    snapshot: &Mutex<ControllerSnapshot>,
) {
    if let Some(session) = active.take() {
        complete_command_if_pending(events, snapshot, session.start_command_id);
        if let Some(command_id) = session.terminal_command_id {
            complete_command_if_pending(events, snapshot, command_id);
        }
    }
}

fn reject_cancel_after_session_end(
    active: &mut Option<ActiveSession>,
    events: &EventEmitter,
    snapshot: &Mutex<ControllerSnapshot>,
    command_id: CommandId,
) {
    update_command_status(
        events,
        snapshot,
        command_id,
        CommandStatus::Rejected {
            reason: "session ended before Cancel was dispatched".into(),
        },
    );
    complete_ended_session(active, events, snapshot);
}

fn complete_snapshot_session_commands(
    events: &EventEmitter,
    snapshot: &Mutex<ControllerSnapshot>,
    session_id: SessionId,
    status: CommandStatus,
) {
    let command_ids = {
        let state = snapshot.lock().expect("snapshot lock poisoned");
        state
            .command_results
            .iter()
            .filter(|result| {
                result.session_id == Some(session_id)
                    && matches!(
                        &result.status,
                        CommandStatus::Accepted | CommandStatus::Starting | CommandStatus::Running
                    )
            })
            .map(|result| result.command_id)
            .collect::<Vec<_>>()
    };
    for command_id in command_ids {
        update_command_status(events, snapshot, command_id, status.clone());
    }
}

fn update_pending_shutdown_commands(
    events: &EventEmitter,
    snapshot: &Mutex<ControllerSnapshot>,
    status: CommandStatus,
) {
    let command_ids = {
        let state = snapshot.lock().expect("snapshot lock poisoned");
        state
            .command_results
            .iter()
            .filter(|result| {
                result.session_id.is_none() && result.status == CommandStatus::Accepted
            })
            .map(|result| result.command_id)
            .collect::<Vec<_>>()
    };
    for command_id in command_ids {
        update_command_status(events, snapshot, command_id, status.clone());
    }
}

fn reject_queued_commands(
    commands: &Receiver<WorkerMessage>,
    events: &EventEmitter,
    snapshot: &Mutex<ControllerSnapshot>,
) {
    while let Ok(WorkerMessage::Command {
        command_id,
        command: _,
    }) = commands.try_recv()
    {
        if command_id.0 != 0 {
            update_command_status(
                events,
                snapshot,
                command_id,
                CommandStatus::Rejected {
                    reason: "controller is shutting down".into(),
                },
            );
        }
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
        let command_result_capacity = command_capacity.saturating_add(1).max(64);
        let snapshot = Arc::new(Mutex::new(ControllerSnapshot {
            command_results_capacity: command_result_capacity,
            ..ControllerSnapshot::default()
        }));
        let commands = CommandHandle {
            sender: command_sender,
            next_session_id: Arc::new(AtomicU64::new(1)),
            next_command_id: Arc::new(AtomicU64::new(1)),
            shutdown_requested: Arc::clone(&shutdown_requested),
            snapshot: Arc::clone(&snapshot),
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
                match backend.shutdown() {
                    Ok(()) => {
                        finish_after_shutdown(&events, &worker_snapshot);
                        update_pending_shutdown_commands(
                            &events,
                            &worker_snapshot,
                            CommandStatus::Completed,
                        );
                        result
                    }
                    Err(message) => {
                        update_pending_shutdown_commands(
                            &events,
                            &worker_snapshot,
                            CommandStatus::Failed {
                                reason: message.clone(),
                            },
                        );
                        let active = worker_snapshot
                            .lock()
                            .expect("snapshot lock poisoned")
                            .active_session
                            .clone();
                        if let Some(session) = active {
                            set_session_phase(
                                &worker_snapshot,
                                session.session_id,
                                SessionPhase::Faulted,
                            );
                            emit_nonfatal(
                                &events.for_lifecycle(
                                    session.config.as_ref().map(|config| config.generation),
                                ),
                                Some(session.session_id),
                                Some(session.mode),
                                RecognitionEventKind::RecognitionError {
                                    kind: RecognitionErrorKind::Internal,
                                    message: format!("backend shutdown failed: {message}"),
                                },
                            );
                        }
                        result.and(Err(message))
                    }
                }
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
    start_command_id: CommandId,
    terminal_command_id: Option<CommandId>,
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
            complete_ended_session(&mut active, &events, &snapshot);
        }
        if shutdown_requested.load(Ordering::Acquire) {
            break;
        }
        let (command_id, command) = match commands.recv_timeout(Duration::from_millis(20)) {
            Ok(WorkerMessage::Command {
                command_id,
                command,
            }) => (command_id, command),
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
            complete_ended_session(&mut active, &events, &snapshot);
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
                let reason = format!("session {:?} is already active", existing.id);
                update_command_status(
                    &events,
                    &snapshot,
                    command_id,
                    CommandStatus::Rejected {
                        reason: reason.clone(),
                    },
                );
                emit_nonfatal(
                    &events.for_lifecycle(config_generation),
                    Some(session_id),
                    Some(mode),
                    RecognitionEventKind::CommandRejected { reason },
                );
                continue;
            }
            update_command_status(&events, &snapshot, command_id, CommandStatus::Accepted);
            update_command_status(&events, &snapshot, command_id, CommandStatus::Starting);
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
                start_command_id: command_id,
                terminal_command_id: None,
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
                Ok(()) => {
                    if gate.load(Ordering::Acquire) == SESSION_OPEN {
                        set_session_phase(&snapshot, session_id, SessionPhase::Running);
                        update_command_status(
                            &events,
                            &snapshot,
                            command_id,
                            CommandStatus::Running,
                        );
                    } else {
                        update_command_status(
                            &events,
                            &snapshot,
                            command_id,
                            CommandStatus::Completed,
                        );
                    }
                }
                Err(message) => {
                    update_command_status(
                        &events,
                        &snapshot,
                        command_id,
                        CommandStatus::StartFailed {
                            reason: message.clone(),
                        },
                    );
                    if gate
                        .compare_exchange(
                            SESSION_OPEN,
                            SESSION_CLOSING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        let lifecycle = events.for_lifecycle(config_generation);
                        emit_nonfatal(
                            &lifecycle,
                            Some(session_id),
                            Some(mode),
                            RecognitionEventKind::RecognitionError {
                                kind: RecognitionErrorKind::Internal,
                                message: message.clone(),
                            },
                        );
                        end_session(
                            &events,
                            &snapshot,
                            session_id,
                            mode,
                            config_generation,
                            SessionEndReason::Failed,
                        );
                        active = None;
                    }
                }
            }
            continue;
        }

        match command {
            RecognitionCommand::StopRecognition { session_id } => {
                if let Some(session) = active.as_mut().filter(|session| session.id == session_id) {
                    let config_generation = session.config.as_ref().map(|config| config.generation);
                    if session.terminal_command_id.is_some() {
                        update_command_status(
                            &events,
                            &snapshot,
                            command_id,
                            CommandStatus::Rejected {
                                reason: "a Stop or Cancel request is already pending".into(),
                            },
                        );
                        continue;
                    }
                    if session
                        .gate
                        .compare_exchange(
                            SESSION_OPEN,
                            SESSION_CLOSING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        if session.gate.load(Ordering::Acquire) == SESSION_ENDED {
                            update_command_status(
                                &events,
                                &snapshot,
                                command_id,
                                CommandStatus::Rejected {
                                    reason: "session ended before Stop was dispatched".into(),
                                },
                            );
                            complete_ended_session(&mut active, &events, &snapshot);
                            continue;
                        }
                        update_command_status(
                            &events,
                            &snapshot,
                            command_id,
                            CommandStatus::Accepted,
                        );
                        set_session_phase(&snapshot, session_id, SessionPhase::Stopping);
                        session.terminal_command_id = Some(command_id);
                        if let Err(message) = backend.stop(session_id) {
                            update_command_status(
                                &events,
                                &snapshot,
                                command_id,
                                CommandStatus::Failed {
                                    reason: message.clone(),
                                },
                            );
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
                                    set_session_phase(
                                        &snapshot,
                                        session_id,
                                        SessionPhase::Cancelling,
                                    );
                                }
                                Err(cleanup) => {
                                    set_session_phase(&snapshot, session_id, SessionPhase::Faulted);
                                    shutdown_error.get_or_insert_with(|| {
                                        format!("stop failed: {message}; cancel failed: {cleanup}")
                                    });
                                }
                            }
                        }
                    } else {
                        update_command_status(
                            &events,
                            &snapshot,
                            command_id,
                            CommandStatus::Rejected {
                                reason: "session is already ended".into(),
                            },
                        );
                    }
                } else {
                    update_command_status(
                        &events,
                        &snapshot,
                        command_id,
                        CommandStatus::Rejected {
                            reason: "no matching active session".into(),
                        },
                    );
                }
            }
            RecognitionCommand::CancelRecognition { session_id } => {
                if let Some(session) = active.as_mut().filter(|session| session.id == session_id) {
                    let config_generation = session.config.as_ref().map(|config| config.generation);
                    if session.terminal_command_id.is_some() {
                        update_command_status(
                            &events,
                            &snapshot,
                            command_id,
                            CommandStatus::Rejected {
                                reason: "a Stop or Cancel request is already pending".into(),
                            },
                        );
                        continue;
                    }
                    match session.gate.compare_exchange(
                        SESSION_OPEN,
                        SESSION_CLOSING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Err(SESSION_ENDED) => {
                            reject_cancel_after_session_end(
                                &mut active,
                                &events,
                                &snapshot,
                                command_id,
                            );
                            continue;
                        }
                        Ok(_) => {}
                        Err(SESSION_CLOSING) => {
                            update_command_status(
                                &events,
                                &snapshot,
                                command_id,
                                CommandStatus::Rejected {
                                    reason: "session is already closing".into(),
                                },
                            );
                            continue;
                        }
                        Err(_) => unreachable!("unknown session gate state"),
                    }
                    if session.gate.load(Ordering::Acquire) == SESSION_ENDED {
                        reject_cancel_after_session_end(
                            &mut active,
                            &events,
                            &snapshot,
                            command_id,
                        );
                        continue;
                    }
                    set_session_phase(&snapshot, session_id, SessionPhase::Cancelling);
                    session.terminal_command_id = Some(command_id);
                    update_command_status(&events, &snapshot, command_id, CommandStatus::Accepted);
                    match backend.cancel(session_id) {
                        Ok(()) => {
                            if session.gate.load(Ordering::Acquire) != SESSION_ENDED {
                                emit_nonfatal(
                                    &events.for_lifecycle(config_generation),
                                    Some(session_id),
                                    Some(session.mode),
                                    RecognitionEventKind::SessionCancelled,
                                );
                            }
                        }
                        Err(message) => {
                            update_command_status(
                                &events,
                                &snapshot,
                                command_id,
                                CommandStatus::Failed {
                                    reason: message.clone(),
                                },
                            );
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
                } else {
                    update_command_status(
                        &events,
                        &snapshot,
                        command_id,
                        CommandStatus::Rejected {
                            reason: "no matching active session".into(),
                        },
                    );
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
                        update_command_status(
                            &events,
                            &snapshot,
                            command_id,
                            CommandStatus::Accepted,
                        );
                        match backend.set_context(session_id, context) {
                            Ok(()) => {
                                update_command_status(
                                    &events,
                                    &snapshot,
                                    command_id,
                                    CommandStatus::Completed,
                                );
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
                            Err(message) => {
                                update_command_status(
                                    &events,
                                    &snapshot,
                                    command_id,
                                    CommandStatus::Failed {
                                        reason: message.clone(),
                                    },
                                );
                                emit_nonfatal(
                                    &events.for_lifecycle(config_generation),
                                    Some(session_id),
                                    Some(session.mode),
                                    RecognitionEventKind::RecognitionError {
                                        kind: RecognitionErrorKind::Internal,
                                        message,
                                    },
                                );
                            }
                        }
                    } else {
                        update_command_status(
                            &events,
                            &snapshot,
                            command_id,
                            CommandStatus::Rejected {
                                reason: "session is not an active Auto session".into(),
                            },
                        );
                    }
                } else {
                    update_command_status(
                        &events,
                        &snapshot,
                        command_id,
                        CommandStatus::Rejected {
                            reason: "no matching active Auto session".into(),
                        },
                    );
                }
            }
            RecognitionCommand::Shutdown => {
                if command_id.0 != 0 {
                    update_command_status(&events, &snapshot, command_id, CommandStatus::Accepted);
                }
                break;
            }
            RecognitionCommand::StartManualRecognition { .. }
            | RecognitionCommand::StartAutoRecognition { .. }
            | RecognitionCommand::StartManualRecognitionWithConfig { .. }
            | RecognitionCommand::StartAutoRecognitionWithConfig { .. } => unreachable!(),
        }
    }

    reject_queued_commands(&commands, &events, &snapshot);
    if let Some((session_id, gate)) = active
        .as_ref()
        .map(|session| (session.id, Arc::clone(&session.gate)))
    {
        match gate.compare_exchange(
            SESSION_OPEN,
            SESSION_CLOSING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Err(SESSION_ENDED) => complete_ended_session(&mut active, &events, &snapshot),
            Ok(_) | Err(SESSION_CLOSING) => {}
            Err(_) => unreachable!("unknown session gate state"),
        }
        if gate.load(Ordering::Acquire) != SESSION_ENDED {
            set_session_phase(&snapshot, session_id, SessionPhase::Cancelling);
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
        let session_id = session.session_id;
        end_session(
            events,
            snapshot,
            session.session_id,
            session.mode,
            session.config.as_ref().map(|config| config.generation),
            SessionEndReason::Shutdown,
        );
        complete_snapshot_session_commands(events, snapshot, session_id, CommandStatus::Completed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashMap,
        sync::{mpsc::SyncSender, Barrier, Condvar, Mutex},
    };

    #[derive(Clone, Default)]
    struct MockBackend {
        calls: Arc<Mutex<Vec<String>>>,
        session_events: Arc<Mutex<HashMap<SessionId, EventEmitter>>>,
        fail_start: bool,
        fail_stop: bool,
        fail_cancel: bool,
        fail_shutdown: bool,
        start_entered: Option<SyncSender<()>>,
        start_gate: Option<Arc<(Mutex<bool>, Condvar)>>,
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
            if self.fail_start {
                return Err("injected start failure".into());
            }
            if let Some(entered) = self.start_entered.take() {
                let _ = entered.send(());
            }
            if let Some(gate) = &self.start_gate {
                let (released, ready) = &**gate;
                let mut released = released.lock().unwrap();
                while !*released {
                    released = ready.wait(released).unwrap();
                }
            }
            Ok(())
        }

        fn stop(&mut self, session_id: SessionId) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("stop:{session_id:?}"));
            if self.fail_stop {
                return Err("injected stop failure".into());
            }
            if let Some(emitter) = self
                .session_events
                .lock()
                .unwrap()
                .get(&session_id)
                .cloned()
            {
                emitter.finish(SessionEndReason::Stopped).unwrap();
            }
            Ok(())
        }

        fn cancel(&mut self, session_id: SessionId) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("cancel:{session_id:?}"));
            if self.fail_cancel {
                return Err("injected cancel failure".into());
            }
            if let Some(emitter) = self
                .session_events
                .lock()
                .unwrap()
                .get(&session_id)
                .cloned()
            {
                emitter.finish(SessionEndReason::Cancelled).unwrap();
            }
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
            if self.fail_shutdown {
                Err("injected shutdown failure".into())
            } else {
                Ok(())
            }
        }
    }

    fn next_event(controller: &ApplicationController) -> RecognitionEvent {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let event = controller
                .events
                .0
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("controller event should arrive");
            if !matches!(
                event.kind,
                RecognitionEventKind::CommandStatusChanged { .. }
            ) {
                return event;
            }
        }
    }

    fn standalone_emitter(
        snapshot: Arc<Mutex<ControllerSnapshot>>,
    ) -> (EventEmitter, Receiver<RecognitionEvent>) {
        let (sender, receiver) = mpsc::sync_channel(8);
        (
            EventEmitter {
                sender,
                sequence: Arc::new(AtomicU64::new(1)),
                origin: Instant::now(),
                order_lock: Arc::new(Mutex::new(())),
                snapshot,
                session_gate: None,
                config_generation: None,
            },
            receiver,
        )
    }

    #[test]
    fn ended_cancel_helper_terminates_ticket_and_terminal_status_cannot_regress() {
        let command_id = CommandId(19);
        let mut initial = ControllerSnapshot {
            command_results_capacity: 4,
            ..ControllerSnapshot::default()
        };
        retain_command_result(
            &mut initial,
            CommandResult {
                command_id,
                session_id: Some(SessionId(5)),
                status: CommandStatus::Submitted,
            },
        );
        let snapshot = Arc::new(Mutex::new(initial));
        let (events, _receiver) = standalone_emitter(Arc::clone(&snapshot));
        let mut active = None;
        reject_cancel_after_session_end(&mut active, &events, &snapshot, command_id);
        assert!(matches!(
            snapshot.lock().unwrap().command_result(command_id),
            CommandResultLookup::Retained(CommandResult {
                status: CommandStatus::Rejected { .. },
                ..
            })
        ));
        assert!(!set_command_status(
            &snapshot,
            command_id,
            CommandStatus::Accepted
        ));
        assert!(matches!(
            snapshot.lock().unwrap().command_result(command_id),
            CommandResultLookup::Retained(CommandResult {
                status: CommandStatus::Rejected { .. },
                ..
            })
        ));
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

    fn wait_for_command_status(
        controller: &ApplicationController,
        command_id: CommandId,
        predicate: impl Fn(&CommandStatus) -> bool,
    ) -> CommandResult {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match controller.commands.command_result(command_id) {
                CommandResultLookup::Retained(result) if predicate(&result.status) => {
                    return result
                }
                CommandResultLookup::Expired { .. } => panic!("command result expired too early"),
                CommandResultLookup::Unknown => panic!("submitted command result was not retained"),
                _ => {}
            }
            assert!(Instant::now() < deadline, "command status did not advance");
            thread::yield_now();
        }
    }

    fn wait_for_snapshot(
        controller: &ApplicationController,
        predicate: impl Fn(&ControllerSnapshot) -> bool,
    ) -> ControllerSnapshot {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let snapshot = controller.events.snapshot();
            if predicate(&snapshot) {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "controller snapshot did not advance"
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
        let manual = controller.commands.start_manual().unwrap().session_id;
        let started = next_event(&controller);
        assert_eq!(started.session_id, Some(manual));
        assert_eq!(started.mode, Some(RecognitionMode::Manual));
        assert_eq!(started.kind, RecognitionEventKind::SessionStarted);

        let duplicate = controller
            .commands
            .start_auto(RecognitionContext::Background)
            .unwrap()
            .session_id;
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
            .unwrap()
            .session_id;
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
        let manual = controller.commands.start_manual().unwrap().session_id;
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
            .unwrap()
            .session_id;
        assert_eq!(next_event(&controller).session_id, Some(auto));
        assert!(controller.events.snapshot().active_session.is_some());
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn cancel_submitted_after_natural_end_is_rejected_and_new_session_is_untouched() {
        let backend = MockBackend::default();
        let emitters = Arc::clone(&backend.session_events);
        let controller = ApplicationController::spawn(backend, 8, 16).unwrap();
        let old = controller.commands.start_manual().unwrap();
        wait_for_command_status(&controller, old.command_id, |status| {
            *status == CommandStatus::Running
        });
        session_emitter(&emitters, old.session_id)
            .finish(SessionEndReason::TimedOut)
            .unwrap();
        let cancel = controller
            .commands
            .submit_tracked(RecognitionCommand::CancelRecognition {
                session_id: old.session_id,
            })
            .unwrap();
        assert!(matches!(
            wait_for_command_status(&controller, cancel, |status| {
                matches!(status, CommandStatus::Rejected { .. })
            })
            .status,
            CommandStatus::Rejected { .. }
        ));

        let new = controller.commands.start_manual().unwrap();
        assert_eq!(new.session_id, SessionId(old.session_id.0 + 1));
        wait_for_command_status(&controller, new.command_id, |status| {
            *status == CommandStatus::Running
        });
        assert_eq!(
            controller
                .events
                .snapshot()
                .active_session
                .unwrap()
                .session_id,
            new.session_id
        );
        controller.commands.request_shutdown();
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn stale_backend_notifications_cannot_mutate_a_new_session() {
        let backend = MockBackend::default();
        let emitters = Arc::clone(&backend.session_events);
        let controller = ApplicationController::spawn(backend, 8, 8).unwrap();
        let first = controller.commands.start_manual().unwrap();
        next_event(&controller);
        let old_emitter = session_emitter(&emitters, first.session_id);
        old_emitter.finish(SessionEndReason::NoMatch).unwrap();
        next_event(&controller);

        let second = controller
            .commands
            .start_auto(RecognitionContext::Background)
            .unwrap();
        next_event(&controller);
        assert_eq!(
            old_emitter.emit(
                Some(first.session_id),
                Some(RecognitionMode::Manual),
                RecognitionEventKind::CaptureStarted,
            ),
            Err(EventError::ControllerStopped)
        );
        assert_eq!(
            old_emitter.finish(SessionEndReason::NoMatch),
            Err(EventError::ControllerStopped)
        );
        let snapshot = controller.events.snapshot();
        assert_eq!(
            snapshot.active_session.as_ref().unwrap().session_id,
            second.session_id
        );
        assert_eq!(
            snapshot.last_ended_session.as_ref().unwrap().session_id,
            first.session_id
        );
        controller.commands.request_shutdown();
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn matched_and_timed_out_manual_sessions_can_be_restarted() {
        let backend = MockBackend::default();
        let emitters = Arc::clone(&backend.session_events);
        let controller = ApplicationController::spawn(backend, 8, 16).unwrap();
        let matched = controller.commands.start_manual().unwrap().session_id;
        next_event(&controller);
        let old = session_emitter(&emitters, matched);
        old.finish(SessionEndReason::Matched).unwrap();
        assert_eq!(
            next_event(&controller).kind,
            RecognitionEventKind::SessionEnded {
                reason: SessionEndReason::Matched
            }
        );
        let timed = controller.commands.start_manual().unwrap().session_id;
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
        let next = controller.commands.start_manual().unwrap().session_id;
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
            .unwrap()
            .session_id;
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
        let manual = controller.commands.start_manual().unwrap().session_id;
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
            .unwrap()
            .session_id;
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
            web_track_url_template: None,
        };
        let session = controller
            .commands
            .start_manual_with_config(config.clone())
            .unwrap()
            .session_id;
        next_event(&controller);
        next_event(&controller);
        let mut primary_track = candidate();
        primary_track.ui_metadata.as_mut().unwrap().track_key = None;
        let event = NativeManualUiEvent::Result(NativeManualResultEvent {
            stage: NativeManualResultStage::TaggingDidFinish,
            primary_track,
            alternative_track: Some(Box::new(candidate())),
        });
        session_emitter(&emitters, session)
            .emit_attempt(
                Some(session),
                Some(RecognitionMode::Manual),
                Some(7),
                RecognitionEventKind::ManualUiEvent(event.clone()),
            )
            .unwrap();
        let result = next_event(&controller);
        assert_eq!(result.config_generation, Some(44));
        assert_eq!(result.attempt_id, Some(7));
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
            .unwrap()
            .session_id;
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
        let active = controller.commands.start_manual().unwrap().session_id;
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

    #[test]
    fn tracked_start_separates_queue_submission_from_backend_acceptance() {
        let backend = MockBackend::default();
        let controller = ApplicationController::spawn(backend, 8, 8).unwrap();
        let first = controller.commands.start_manual_tracked().unwrap();
        assert_eq!(first.session_id, SessionId(1));
        assert!(matches!(
            controller.commands.command_result(first.command_id),
            CommandResultLookup::Retained(CommandResult {
                command_id,
                session_id: Some(session_id),
                status: CommandStatus::Submitted
                    | CommandStatus::Accepted
                    | CommandStatus::Starting
                    | CommandStatus::Running,
            }) if command_id == first.command_id && session_id == first.session_id
        ));
        assert_eq!(
            next_event(&controller).kind,
            RecognitionEventKind::SessionStarted
        );
        assert_eq!(
            wait_for_command_status(&controller, first.command_id, |status| {
                *status == CommandStatus::Running
            })
            .status,
            CommandStatus::Running
        );

        let duplicate = controller
            .commands
            .start_auto_tracked(RecognitionContext::Foreground)
            .unwrap();
        assert!(matches!(
            next_event(&controller).kind,
            RecognitionEventKind::CommandRejected { .. }
        ));
        let rejected = wait_for_command_status(&controller, duplicate.command_id, |status| {
            matches!(status, CommandStatus::Rejected { .. })
        });
        assert_eq!(rejected.session_id, Some(duplicate.session_id));
        assert!(matches!(rejected.status, CommandStatus::Rejected { .. }));
        assert_eq!(
            controller.commands.command_result(first.command_id),
            CommandResultLookup::Retained(CommandResult {
                command_id: first.command_id,
                session_id: Some(first.session_id),
                status: CommandStatus::Running,
            })
        );
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn command_results_are_bounded_and_report_expiration_after_queue_overflow() {
        let backend = MockBackend::default();
        let controller = ApplicationController::spawn(backend, 1, 1).unwrap();
        let mut tickets = Vec::new();
        for index in 0..70 {
            let command_id = controller
                .commands
                .submit_tracked(RecognitionCommand::StopRecognition {
                    session_id: SessionId(10_000 + index),
                })
                .unwrap();
            wait_for_command_status(&controller, command_id, |status| {
                matches!(status, CommandStatus::Rejected { .. })
            });
            tickets.push(command_id);
        }
        let snapshot = controller.events.snapshot();
        assert_eq!(snapshot.command_results_capacity, 64);
        assert_eq!(snapshot.command_results.len(), 64);
        assert!(snapshot.dropped_notifications > 0);
        assert!(matches!(
            controller.commands.command_result(tickets[0]),
            CommandResultLookup::Expired {
                evicted_through: CommandId(6)
            }
        ));
        assert!(matches!(
            controller.commands.command_result(*tickets.last().unwrap()),
            CommandResultLookup::Retained(CommandResult {
                status: CommandStatus::Rejected { .. },
                ..
            })
        ));
        let commands = controller.commands.clone();
        assert!(controller.join().unwrap().is_ok());
        assert_eq!(
            commands.submit_tracked(RecognitionCommand::StopRecognition {
                session_id: SessionId(1),
            }),
            Err(SubmitError::ControllerStopped)
        );
    }

    #[test]
    fn reserved_command_id_zero_stays_unknown_beyond_eviction_watermark() {
        let snapshot = ControllerSnapshot {
            command_results_evicted_through: Some(CommandId(u64::MAX)),
            ..ControllerSnapshot::default()
        };
        assert_eq!(
            snapshot.command_result(CommandId(0)),
            CommandResultLookup::Unknown
        );
        assert_eq!(
            snapshot.command_result(CommandId(u64::MAX)),
            CommandResultLookup::Expired {
                evicted_through: CommandId(u64::MAX)
            }
        );
    }

    #[test]
    fn queue_full_does_not_consume_command_id_or_make_unknown_ids_expired() {
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let backend = MockBackend {
            start_entered: Some(entered_sender),
            start_gate: Some(Arc::clone(&gate)),
            ..MockBackend::default()
        };
        let controller = ApplicationController::spawn(backend, 1, 32).unwrap();
        let start = controller.commands.start_manual().unwrap();
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("backend start should be in progress");
        let queued_stop = controller
            .commands
            .submit_tracked(RecognitionCommand::StopRecognition {
                session_id: start.session_id,
            })
            .unwrap();
        assert_eq!(queued_stop, CommandId(2));
        assert_eq!(
            controller
                .commands
                .start_auto_tracked(RecognitionContext::Foreground),
            Err(SubmitError::QueueFull)
        );
        {
            let (released, ready) = &*gate;
            *released.lock().unwrap() = true;
            ready.notify_all();
        }
        assert_eq!(
            wait_for_command_status(&controller, queued_stop, |status| {
                *status == CommandStatus::Completed
            })
            .status,
            CommandStatus::Completed
        );

        let next = controller
            .commands
            .submit_tracked(RecognitionCommand::StopRecognition {
                session_id: start.session_id,
            })
            .unwrap();
        assert_eq!(next, CommandId(3));
        assert!(matches!(
            wait_for_command_status(&controller, next, |status| {
                matches!(status, CommandStatus::Rejected { .. })
            })
            .status,
            CommandStatus::Rejected { .. }
        ));
        assert_eq!(
            controller.commands.command_result(CommandId(4)),
            CommandResultLookup::Unknown
        );
        controller.commands.request_shutdown();
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn concurrent_handles_allocate_unique_monotonic_command_ids() {
        let controller = ApplicationController::spawn(MockBackend::default(), 128, 8).unwrap();
        let mut submitters = Vec::new();
        for worker in 0..8 {
            let commands = controller.commands.clone();
            submitters.push(thread::spawn(move || {
                (0..8)
                    .map(|index| {
                        commands
                            .submit_tracked(RecognitionCommand::StopRecognition {
                                session_id: SessionId(10_000 + worker * 8 + index),
                            })
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            }));
        }
        let mut ids = submitters
            .into_iter()
            .flat_map(|submitter| submitter.join().unwrap())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, (1..=64).map(CommandId).collect::<Vec<_>>());
        controller.commands.request_shutdown();
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn command_id_zero_remains_reserved_when_counter_exhausts() {
        let controller = ApplicationController::spawn(MockBackend::default(), 4, 4).unwrap();
        controller
            .commands
            .next_command_id
            .store(u64::MAX, Ordering::Relaxed);
        let last = controller
            .commands
            .submit_tracked(RecognitionCommand::StopRecognition {
                session_id: SessionId(123),
            })
            .unwrap();
        assert_eq!(last, CommandId(u64::MAX));
        assert_eq!(
            controller
                .commands
                .submit_tracked(RecognitionCommand::StopRecognition {
                    session_id: SessionId(124),
                }),
            Err(SubmitError::CommandIdExhausted)
        );
        assert_eq!(
            controller.commands.command_result(CommandId(0)),
            CommandResultLookup::Unknown
        );
        controller.commands.request_shutdown();
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn concurrent_shutdown_and_submit_never_leave_an_accepted_ticket_pending() {
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let backend = MockBackend {
            start_entered: Some(entered_sender),
            start_gate: Some(Arc::clone(&gate)),
            ..MockBackend::default()
        };
        let controller = ApplicationController::spawn(backend, 4, 8).unwrap();
        let start = controller.commands.start_manual().unwrap();
        let result_commands = controller.commands.clone();
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("backend start should be in progress");

        let barrier = Arc::new(Barrier::new(3));
        let submit_commands = controller.commands.clone();
        let submit_barrier = Arc::clone(&barrier);
        let submitter = thread::spawn(move || {
            submit_barrier.wait();
            submit_commands.submit_tracked(RecognitionCommand::CancelRecognition {
                session_id: start.session_id,
            })
        });
        let shutdown_commands = controller.commands.clone();
        let shutdown_barrier = Arc::clone(&barrier);
        let shutdown = thread::spawn(move || {
            shutdown_barrier.wait();
            shutdown_commands.request_shutdown();
        });
        barrier.wait();
        let submitted = submitter.join().unwrap();
        shutdown.join().unwrap();
        {
            let (released, ready) = &*gate;
            *released.lock().unwrap() = true;
            ready.notify_all();
        }
        assert!(controller.join().unwrap().is_ok());

        match submitted {
            Ok(command_id) => assert!(matches!(
                result_commands.command_result(command_id),
                CommandResultLookup::Retained(CommandResult {
                    status: CommandStatus::Rejected { .. },
                    ..
                })
            )),
            Err(SubmitError::ControllerStopped) => {}
            other => panic!("unexpected submit result: {other:?}"),
        }
    }

    #[test]
    fn lost_rejection_notification_is_recoverable_from_command_result_snapshot() {
        let backend = MockBackend::default();
        let controller = ApplicationController::spawn(backend, 4, 1).unwrap();
        let first = controller.commands.start_manual_tracked().unwrap();
        wait_for_command_status(&controller, first.command_id, |status| {
            *status == CommandStatus::Running
        });
        // Leave SessionStarted queued so later notifications hit QueueFull.
        let duplicate = controller
            .commands
            .start_auto_tracked(RecognitionContext::Background)
            .unwrap();
        let result = wait_for_command_status(&controller, duplicate.command_id, |status| {
            matches!(status, CommandStatus::Rejected { .. })
        });
        assert_eq!(result.session_id, Some(duplicate.session_id));
        assert!(matches!(result.status, CommandStatus::Rejected { .. }));
        let snapshot = controller.events.snapshot();
        assert!(snapshot.dropped_notifications > 0);
        assert!(matches!(
            snapshot.command_result(duplicate.command_id),
            CommandResultLookup::Retained(CommandResult {
                status: CommandStatus::Rejected { .. },
                ..
            })
        ));
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn large_progress_event_burst_keeps_queue_and_snapshot_bounded() {
        let backend = MockBackend::default();
        let emitters = Arc::clone(&backend.session_events);
        let controller = ApplicationController::spawn(backend, 4, 2).unwrap();
        let start = controller
            .commands
            .start_auto(RecognitionContext::Background)
            .unwrap();
        wait_for_command_status(&controller, start.command_id, |status| {
            *status == CommandStatus::Running
        });
        let emitter = session_emitter(&emitters, start.session_id);
        for _ in 0..10_000 {
            let _ = emitter.emit(
                Some(start.session_id),
                Some(RecognitionMode::Auto),
                RecognitionEventKind::CaptureStarted,
            );
        }
        let snapshot = controller.events.snapshot();
        assert_eq!(
            snapshot.active_session.as_ref().unwrap().session_id,
            start.session_id
        );
        assert!(snapshot.dropped_notifications >= 9_990);
        assert_eq!(
            snapshot
                .active_session
                .as_ref()
                .unwrap()
                .last_event
                .as_ref()
                .unwrap()
                .category,
            RecognitionEventCategory::CaptureStarted
        );
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn full_command_queue_returns_immediately_and_shutdown_rejects_pending_work() {
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let backend = MockBackend {
            start_entered: Some(entered_sender),
            start_gate: Some(Arc::clone(&gate)),
            ..MockBackend::default()
        };
        let controller = ApplicationController::spawn(backend, 1, 4).unwrap();
        let start = controller.commands.start_manual_tracked().unwrap();
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("backend start should be in progress");
        assert!(matches!(
            controller.commands.command_result(start.command_id),
            CommandResultLookup::Retained(CommandResult {
                status: CommandStatus::Starting,
                ..
            })
        ));

        let queued_stop = controller
            .commands
            .submit_tracked(RecognitionCommand::StopRecognition {
                session_id: start.session_id,
            })
            .unwrap();
        assert_eq!(
            controller
                .commands
                .start_auto_tracked(RecognitionContext::Foreground),
            Err(SubmitError::QueueFull)
        );
        let commands = controller.commands.clone();
        controller.commands.request_shutdown();
        {
            let (released, ready) = &*gate;
            *released.lock().unwrap() = true;
            ready.notify_all();
        }
        assert!(controller.join().unwrap().is_ok());
        assert!(matches!(
            commands.command_result(queued_stop),
            CommandResultLookup::Retained(CommandResult {
                status: CommandStatus::Rejected { .. },
                ..
            })
        ));
        assert!(matches!(
            commands.command_result(start.command_id),
            CommandResultLookup::Retained(CommandResult {
                status: CommandStatus::Completed,
                ..
            })
        ));
        assert_eq!(
            commands.submit_tracked(RecognitionCommand::StopRecognition {
                session_id: start.session_id,
            }),
            Err(SubmitError::ControllerStopped)
        );
    }

    #[test]
    fn cancel_queued_during_start_closes_the_started_session_without_waiting() {
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let backend = MockBackend {
            start_entered: Some(entered_sender),
            start_gate: Some(Arc::clone(&gate)),
            ..MockBackend::default()
        };
        let controller = ApplicationController::spawn(backend, 4, 32).unwrap();
        let start = controller.commands.start_manual().unwrap();
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("backend start should be in progress");
        let cancel = controller
            .commands
            .submit(RecognitionCommand::CancelRecognition {
                session_id: start.session_id,
            })
            .unwrap();
        {
            let (released, ready) = &*gate;
            *released.lock().unwrap() = true;
            ready.notify_all();
        }
        let snapshot = wait_for_snapshot(&controller, |snapshot| {
            snapshot
                .last_ended_session
                .as_ref()
                .is_some_and(|session| session.session_id == start.session_id)
        });
        assert_eq!(
            snapshot.last_ended_session.unwrap().phase,
            SessionPhase::Ended(SessionEndReason::Cancelled)
        );
        let cancel_result = wait_for_command_status(&controller, cancel, |status| {
            *status == CommandStatus::Completed
        });
        assert_eq!(cancel_result.status, CommandStatus::Completed);
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn duplicate_cancel_commands_complete_once_and_reject_the_stale_request() {
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let backend = MockBackend {
            start_entered: Some(entered_sender),
            start_gate: Some(Arc::clone(&gate)),
            ..MockBackend::default()
        };
        let observed = Arc::clone(&backend.calls);
        let controller = ApplicationController::spawn(backend, 4, 16).unwrap();
        let start = controller.commands.start_manual().unwrap();
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("backend start should be in progress");
        let first = controller
            .commands
            .submit_tracked(RecognitionCommand::CancelRecognition {
                session_id: start.session_id,
            })
            .unwrap();
        let duplicate = controller
            .commands
            .submit_tracked(RecognitionCommand::CancelRecognition {
                session_id: start.session_id,
            })
            .unwrap();
        {
            let (released, ready) = &*gate;
            *released.lock().unwrap() = true;
            ready.notify_all();
        }
        assert_eq!(
            wait_for_command_status(&controller, first, |status| {
                *status == CommandStatus::Completed
            })
            .status,
            CommandStatus::Completed
        );
        assert!(matches!(
            wait_for_command_status(&controller, duplicate, |status| {
                matches!(status, CommandStatus::Rejected { .. })
            })
            .status,
            CommandStatus::Rejected { .. }
        ));
        assert_eq!(
            observed
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call.starts_with("cancel:"))
                .count(),
            1
        );
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn shutdown_during_start_releases_backend_before_terminal_snapshot_even_if_queue_is_full() {
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let backend = MockBackend {
            start_entered: Some(entered_sender),
            start_gate: Some(Arc::clone(&gate)),
            ..MockBackend::default()
        };
        let controller = ApplicationController::spawn(backend, 4, 1).unwrap();
        let start = controller.commands.start_manual().unwrap();
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("backend start should be in progress");
        controller.commands.request_shutdown();
        {
            let (released, ready) = &*gate;
            *released.lock().unwrap() = true;
            ready.notify_all();
        }

        let snapshot = wait_for_snapshot(&controller, |snapshot| {
            snapshot
                .last_ended_session
                .as_ref()
                .is_some_and(|session| session.session_id == start.session_id)
        });
        assert_eq!(
            snapshot.last_ended_session.unwrap().phase,
            SessionPhase::Ended(SessionEndReason::Shutdown)
        );
        assert!(snapshot.dropped_notifications > 0);
        assert_eq!(
            wait_for_command_status(&controller, start.command_id, |status| {
                *status == CommandStatus::Completed
            })
            .status,
            CommandStatus::Completed
        );
        assert!(controller.join().unwrap().is_ok());
    }

    #[test]
    fn backend_start_and_stop_failures_are_reported_in_command_results() {
        let mut backend = MockBackend {
            fail_start: true,
            ..MockBackend::default()
        };
        let controller = ApplicationController::spawn(backend.clone(), 8, 8).unwrap();
        let start = controller.commands.start_manual_tracked().unwrap();
        let failed = wait_for_command_status(&controller, start.command_id, |status| {
            matches!(status, CommandStatus::StartFailed { .. })
        });
        assert!(matches!(failed.status, CommandStatus::StartFailed { .. }));
        assert_eq!(
            wait_for_snapshot(&controller, |snapshot| snapshot
                .last_ended_session
                .is_some())
            .last_ended_session
            .unwrap()
            .phase,
            SessionPhase::Ended(SessionEndReason::Failed)
        );
        assert!(controller.join().unwrap().is_ok());

        backend = MockBackend {
            fail_stop: true,
            fail_cancel: true,
            ..MockBackend::default()
        };
        let controller = ApplicationController::spawn(backend, 8, 8).unwrap();
        let start = controller.commands.start_manual_tracked().unwrap();
        wait_for_command_status(&controller, start.command_id, |status| {
            *status == CommandStatus::Running
        });
        let stop = controller
            .commands
            .submit_tracked(RecognitionCommand::StopRecognition {
                session_id: start.session_id,
            })
            .unwrap();
        assert!(matches!(
            wait_for_command_status(&controller, stop, |status| {
                matches!(status, CommandStatus::Failed { .. })
            })
            .status,
            CommandStatus::Failed { .. }
        ));
        assert_eq!(
            wait_for_snapshot(&controller, |snapshot| snapshot
                .active_session
                .as_ref()
                .is_some_and(|session| session.phase == SessionPhase::Faulted))
            .active_session
            .unwrap()
            .phase,
            SessionPhase::Faulted
        );
        assert!(controller.join().unwrap().unwrap_err().contains(
            "stop failed: injected stop failure; cancel failed: injected cancel failure"
        ));
    }

    #[test]
    fn cancel_and_shutdown_failures_leave_faulted_state_visible() {
        let backend = MockBackend {
            fail_cancel: true,
            ..MockBackend::default()
        };
        let controller = ApplicationController::spawn(backend, 4, 4).unwrap();
        let start = controller.commands.start_manual().unwrap();
        wait_for_command_status(&controller, start.command_id, |status| {
            *status == CommandStatus::Running
        });
        let cancel = controller
            .commands
            .submit_tracked(RecognitionCommand::CancelRecognition {
                session_id: start.session_id,
            })
            .unwrap();
        assert!(matches!(
            wait_for_command_status(&controller, cancel, |status| {
                matches!(status, CommandStatus::Failed { .. })
            })
            .status,
            CommandStatus::Failed { .. }
        ));
        assert_eq!(
            wait_for_snapshot(&controller, |snapshot| snapshot
                .active_session
                .as_ref()
                .is_some_and(|session| session.phase == SessionPhase::Faulted))
            .active_session
            .unwrap()
            .phase,
            SessionPhase::Faulted
        );
        assert!(controller.join().unwrap().is_err());

        let backend = MockBackend {
            fail_shutdown: true,
            ..MockBackend::default()
        };
        let controller = ApplicationController::spawn(backend, 4, 4).unwrap();
        let snapshot_handle = Arc::clone(&controller.events.1);
        let start = controller.commands.start_manual().unwrap();
        wait_for_command_status(&controller, start.command_id, |status| {
            *status == CommandStatus::Running
        });
        assert!(controller
            .join()
            .unwrap()
            .unwrap_err()
            .contains("shutdown failure"));
        assert_eq!(
            snapshot_handle
                .lock()
                .unwrap()
                .active_session
                .as_ref()
                .unwrap()
                .phase,
            SessionPhase::Faulted
        );
    }

    #[test]
    fn explicit_shutdown_command_has_a_terminal_command_result() {
        let controller = ApplicationController::spawn(MockBackend::default(), 4, 4).unwrap();
        let commands = controller.commands.clone();
        let shutdown = controller
            .commands
            .submit(RecognitionCommand::Shutdown)
            .unwrap();
        assert!(matches!(
            wait_for_command_status(&controller, shutdown, |status| {
                *status == CommandStatus::Completed
            })
            .status,
            CommandStatus::Completed
        ));
        assert!(controller.join().unwrap().is_ok());
        assert!(matches!(
            commands.command_result(shutdown),
            CommandResultLookup::Retained(CommandResult {
                status: CommandStatus::Completed,
                ..
            })
        ));
    }
}
