#![deny(unsafe_op_in_unsafe_fn)]

use super::{
    app_state::{AppState, RecognitionSessionUi},
    artwork_pipeline::start_artwork_load,
    artwork_state::finish_active_artwork_request,
    current_success_wave_frame,
    history::{format_history_date_label, resolve_history_timestamp, Track},
    history_offset_after_insert,
    history_sync::{
        invalidate_orphaned_artwork_requests, register_artwork_request, update_settings_history,
    },
    hovered_history_row_for_cursor, is_offline_failure,
    preferences::save_auto_shazam_preference,
    status_text::StatusTextAlternator,
    track_navigation::{refresh_track_page_urls, resolve_page_link_for_generation},
    tray::queue_hidden_match_notification,
    update::abort_pending_update_after_cancel_failure,
    CaptureMode, HistoryMutationEvent, HistoryMutationTransition, MainPhase, ResultCardStage,
    AUTO_CANDIDATE_DEDUPE_LIMIT, HISTORY_HEIGHT,
};
use crate::{
    ui_model,
    windows_history_store::{PersistedTrack, RecognitionSource},
};
use openshazam::WindowsRecognitionBackend;
use openshazam_controller::{
    ApplicationController, CommandResultLookup, CommandStatus, NativeAutoUiEvent,
    NativeManualResultStage, NativeManualUiEvent, RecognitionCommand, RecognitionContext,
    RecognitionEvent, RecognitionEventCategory, RecognitionEventKind, RecognitionSessionSnapshot,
    SessionEndReason, SessionPhase, TrackCandidate, UiRecognitionEvent,
};
use std::collections::VecDeque;
use std::time::{Instant, SystemTime};
use windows::Win32::UI::HiDpi::GetDpiForWindow;

pub(super) fn start_capture(app: &mut AppState, mode: CaptureMode) {
    if app.recognition_session.is_some() || matches!(app.phase, MainPhase::Listening { .. }) {
        return;
    }
    app.status_alternator = None;
    if app.recognition_controller.is_none() {
        match ApplicationController::spawn(WindowsRecognitionBackend::new(), 16, 64) {
            Ok(controller) => app.recognition_controller = Some(controller),
            Err(error) => {
                app.phase = MainPhase::Error {
                    message: format!("Recognition controller failed to start: {error}"),
                    entered_at: Instant::now(),
                };
                return;
            }
        }
    }
    let controller = app
        .recognition_controller
        .as_ref()
        .expect("controller created");
    let context = (mode == CaptureMode::Auto).then_some(if app.visible {
        RecognitionContext::Foreground
    } else {
        RecognitionContext::Background
    });
    let ticket = match mode {
        CaptureMode::Manual => controller.commands.start_manual(),
        CaptureMode::Auto => controller
            .commands
            .start_auto(context.expect("Auto recognition has a foreground/background context")),
    };
    match ticket {
        Ok(ticket) => {
            if mode == CaptureMode::Auto {
                save_auto_shazam_preference(true);
            }
            app.auto_spring =
                (mode == CaptureMode::Auto).then(ui_model::auto_tagging_button_spring_profile);
            app.success_wave_frame = None;
            if mode == CaptureMode::Auto {
                app.auto_candidate_dedupe.clear();
            }
            let entered_at = Instant::now();
            app.status_alternator =
                (mode == CaptureMode::Auto).then(|| StatusTextAlternator::auto(entered_at));
            app.recognition_session = Some(RecognitionSessionUi {
                ticket,
                mode,
                context,
                cancel_command_id: None,
                last_sequence: 0,
                result_presented: false,
            });
            app.phase = MainPhase::Starting { mode, entered_at };
        }
        Err(error) => {
            app.phase = MainPhase::Error {
                message: format!("Recognition request was not accepted: {error:?}"),
                entered_at: Instant::now(),
            };
        }
    }
}
pub(super) fn request_capture_cancel(
    app: &mut AppState,
    disable_auto_shazam: bool,
) -> Result<(), String> {
    let Some(session) = app.recognition_session else {
        return Ok(());
    };
    if session.cancel_command_id.is_some() {
        return Ok(());
    }
    let controller = app
        .recognition_controller
        .as_ref()
        .ok_or_else(|| "recognition controller is unavailable".to_owned())?;
    let command_id = controller
        .commands
        .submit(RecognitionCommand::CancelRecognition {
            session_id: session.ticket.session_id,
        })
        .map_err(|error| format!("cancel request was not accepted: {error:?}"))?;
    if disable_auto_shazam && session.mode == CaptureMode::Auto {
        save_auto_shazam_preference(false);
    }
    if let Some(active) = app.recognition_session.as_mut() {
        active.cancel_command_id = Some(command_id);
    }
    app.phase = MainPhase::Cancelling {
        mode: session.mode,
        entered_at: Instant::now(),
    };
    Ok(())
}

pub(super) fn cancel_capture(app: &mut AppState) {
    if let Err(error) = request_capture_cancel(app, true) {
        app.message = Some(error);
    }
}

pub(super) fn set_auto_recognition_context(app: &mut AppState, context: RecognitionContext) {
    let Some(session) = app.recognition_session else {
        return;
    };
    if session.mode != CaptureMode::Auto
        || session.context == Some(context)
        || session.cancel_command_id.is_some()
    {
        return;
    }
    let Some(controller) = app.recognition_controller.as_ref() else {
        return;
    };
    match controller
        .commands
        .submit(RecognitionCommand::SetRecognitionContext {
            session_id: session.ticket.session_id,
            context,
        }) {
        Ok(_) => {
            if let Some(active) = app.recognition_session.as_mut() {
                active.context = Some(context);
            }
        }
        Err(error) => {
            eprintln!("OpenShazam could not update Auto Shazam context: {error:?}");
        }
    }
}

pub(super) fn poll_recognition_events(app: &mut AppState) -> bool {
    let Some(controller) = app.recognition_controller.as_ref() else {
        return false;
    };
    let active = app.recognition_session;
    let mut sequence_gap = false;
    if let Some(session) = active {
        if let CommandResultLookup::Retained(result) = controller
            .commands
            .command_result(session.ticket.command_id)
        {
            if matches!(result.status, CommandStatus::Failed { .. }) {
                sequence_gap = true;
            }
            let failure = match result.status {
                CommandStatus::Rejected { reason } | CommandStatus::StartFailed { reason } => {
                    Some(reason)
                }
                _ => None,
            };
            if let Some(reason) = failure {
                app.recognition_session = None;
                app.phase = MainPhase::Error {
                    message: format!("Recognition did not start: {reason}"),
                    entered_at: Instant::now(),
                };
                return true;
            }
        }
        if let Some(cancel_id) = session.cancel_command_id {
            if let CommandResultLookup::Retained(result) =
                controller.commands.command_result(cancel_id)
            {
                let failure = match result.status {
                    CommandStatus::Rejected { reason }
                    | CommandStatus::StartFailed { reason }
                    | CommandStatus::Failed { reason } => Some(reason),
                    _ => None,
                };
                if let Some(reason) = failure {
                    if let Some(current) = app.recognition_session.as_mut() {
                        current.cancel_command_id = None;
                    }
                    app.message = Some(format!("Cancel request failed: {reason}"));
                    let snapshot = controller.events.snapshot();
                    let _ = reconcile_controller_snapshot(app, &snapshot);
                    abort_pending_update_after_cancel_failure(app, &reason);
                    return true;
                }
            }
        }
    }
    let (events, snapshot) = {
        let mut events = Vec::new();
        while let Ok(event) = controller.events.try_recv() {
            events.push(event);
        }
        (events, controller.events.snapshot())
    };

    let mut changed = false;
    for event in events {
        if event.sequence > app.controller_sequence.saturating_add(1) {
            sequence_gap = true;
        }
        app.controller_sequence = app.controller_sequence.max(event.sequence);
        let Some(session) = active else { continue };
        if event.session_id != Some(session.ticket.session_id) {
            continue;
        }
        let last_processed = app
            .recognition_session
            .map_or(0, |current| current.last_sequence);
        if event.sequence <= last_processed {
            continue;
        }
        if event.sequence > session.last_sequence.saturating_add(1) {
            sequence_gap = true;
        }
        if let Some(current) = app.recognition_session.as_mut() {
            current.last_sequence = current.last_sequence.max(event.sequence);
        }
        sequence_gap |= matches!(
            event.kind,
            RecognitionEventKind::SessionEnded { .. }
                | RecognitionEventKind::SessionCancelled
                | RecognitionEventKind::CommandStatusChanged { .. }
        );
        changed |= apply_recognition_event(app, event, session.mode);
    }

    let notifications_dropped = snapshot.dropped_notifications > app.dropped_notifications;
    if sequence_gap || notifications_dropped || snapshot.last_sequence > app.controller_sequence {
        app.controller_sequence = snapshot.last_sequence;
        app.dropped_notifications = snapshot.dropped_notifications;
        if let Some(current) = app.recognition_session.as_mut() {
            current.last_sequence = snapshot.last_sequence;
        }
        changed |= reconcile_controller_snapshot(app, &snapshot);
    }
    changed
}

fn apply_recognition_event(app: &mut AppState, event: RecognitionEvent, mode: CaptureMode) -> bool {
    let event_time_utc = event.emitted_at_utc;
    let config_generation = event.config_generation;
    let session_id = event.session_id;
    match event.kind {
        RecognitionEventKind::UiConfigSnapshot(config) => {
            if config_generation == Some(config.generation) {
                app.ui_configs.insert(config.generation, config);
                while app.ui_configs.len() > 8 {
                    let Some(oldest) = app.ui_configs.keys().copied().min() else {
                        break;
                    };
                    app.ui_configs.remove(&oldest);
                }
                refresh_track_page_urls(app);
            }
            true
        }
        RecognitionEventKind::CaptureStarted
        | RecognitionEventKind::SignatureReady { .. }
        | RecognitionEventKind::RequestStarted { .. }
        | RecognitionEventKind::RetryScheduled { .. } => {
            if matches!(app.phase, MainPhase::Starting { .. }) {
                enter_listening(app, mode, Instant::now());
            }
            true
        }
        RecognitionEventKind::ManualUiEvent(NativeManualUiEvent::Result(result))
            if mode == CaptureMode::Manual
                && result.stage == NativeManualResultStage::TaggingDidFinish =>
        {
            let already_presented = app
                .recognition_session
                .is_some_and(|session| session.result_presented);
            if already_presented {
                return false;
            }
            if let Some(session) = app.recognition_session.as_mut() {
                session.result_presented = true;
            }
            show_controller_match(
                app,
                result.primary_track,
                mode,
                config_generation,
                Some(event_time_utc),
            );
            true
        }
        RecognitionEventKind::AutoUiEvent(NativeAutoUiEvent::ContinuousMatchCandidate(result))
            if mode == CaptureMode::Auto =>
        {
            apply_auto_match_candidate(
                app,
                result.primary_track,
                config_generation,
                Some(event_time_utc),
            )
        }
        RecognitionEventKind::ManualUiEvent(NativeManualUiEvent::NoMatch)
        | RecognitionEventKind::NoMatch { .. } => {
            app.phase = MainPhase::NoMatch {
                entered_at: Instant::now(),
                returned_to_ready_at: None,
            };
            true
        }
        RecognitionEventKind::RecognitionError { message, .. } => {
            let entered_at = Instant::now();
            app.phase = if is_offline_failure(&message) {
                MainPhase::Offline {
                    entered_at,
                    returned_to_ready_at: None,
                }
            } else {
                MainPhase::Error {
                    message,
                    entered_at,
                }
            };
            true
        }
        RecognitionEventKind::SessionTimedOut => {
            app.phase = MainPhase::Timeout {
                mode,
                entered_at: Instant::now(),
            };
            true
        }
        RecognitionEventKind::SessionCancelled | RecognitionEventKind::SessionEnded { .. } => {
            let _ended_session_id = session_id;
            // The following authoritative snapshot distinguishes a true terminal
            // state from an event notification that raced a newer session update.
            true
        }
        _ => false,
    }
}

fn reconcile_controller_snapshot(
    app: &mut AppState,
    snapshot: &openshazam_controller::ControllerSnapshot,
) -> bool {
    let active_id = app
        .recognition_session
        .map(|session| session.ticket.session_id);
    let session_snapshot = snapshot
        .active_session
        .as_ref()
        .filter(|session| Some(session.session_id) == active_id)
        .or_else(|| {
            snapshot
                .last_ended_session
                .as_ref()
                .filter(|session| Some(session.session_id) == active_id)
        });
    let Some(session_snapshot) = session_snapshot else {
        return false;
    };
    reconcile_session_snapshot(app, session_snapshot)
}

fn enter_listening(app: &mut AppState, mode: CaptureMode, started_at: Instant) {
    // Native Auto changes the tagging layer to state 2 and starts its spring
    // before the recorder reports CaptureStarted. Keep the animation clock
    // anchored to the UI command so the spring and ripple phase do not restart
    // when the asynchronous capture event arrives.
    let animation_started_at = capture_animation_start(&app.phase, mode, started_at);
    app.phase = MainPhase::Listening {
        mode,
        started_at: animation_started_at,
    };
    app.status_alternator =
        (mode == CaptureMode::Auto).then(|| StatusTextAlternator::auto(animation_started_at));
}

pub(super) fn capture_animation_start(
    phase: &MainPhase,
    mode: CaptureMode,
    fallback: Instant,
) -> Instant {
    match (phase, mode) {
        (
            MainPhase::Starting {
                mode: CaptureMode::Auto,
                entered_at,
            },
            CaptureMode::Auto,
        ) => *entered_at,
        _ => fallback,
    }
}

fn reconcile_session_snapshot(app: &mut AppState, session: &RecognitionSessionSnapshot) -> bool {
    let Some(ui_session) = app.recognition_session else {
        return false;
    };
    if session.session_id != ui_session.ticket.session_id {
        return false;
    }
    let mut changed = false;
    if let Some(config) = session.config.as_ref() {
        app.ui_configs.insert(config.generation, config.clone());
        refresh_track_page_urls(app);
    }
    if !ui_session.result_presented && ui_session.mode == CaptureMode::Manual {
        if let Some(UiRecognitionEvent::Manual(NativeManualUiEvent::Result(result))) =
            session.last_result.as_ref()
        {
            if result.stage == NativeManualResultStage::TaggingDidFinish {
                if let Some(current) = app.recognition_session.as_mut() {
                    current.result_presented = true;
                }
                show_controller_match(
                    app,
                    result.primary_track.clone(),
                    ui_session.mode,
                    session.config.as_ref().map(|config| config.generation),
                    session.last_result_at_utc,
                );
                changed = true;
            }
        }
    }
    if ui_session.mode == CaptureMode::Auto {
        if let Some(UiRecognitionEvent::Auto(NativeAutoUiEvent::ContinuousMatchCandidate(result))) =
            session.last_result.as_ref()
        {
            changed |= apply_auto_match_candidate(
                app,
                result.primary_track.clone(),
                session.config.as_ref().map(|config| config.generation),
                session.last_result_at_utc,
            );
        }
    }
    match session.phase {
        SessionPhase::Starting => {}
        SessionPhase::Running => {
            let capture_is_live = session.last_event.is_some_and(|event| {
                matches!(
                    event.category,
                    RecognitionEventCategory::CaptureStarted
                        | RecognitionEventCategory::SignatureReady
                        | RecognitionEventCategory::RequestStarted
                        | RecognitionEventCategory::RetryScheduled
                        | RecognitionEventCategory::ManualUiEvent
                        | RecognitionEventCategory::AutoUiEvent
                )
            });
            if capture_is_live
                && (matches!(app.phase, MainPhase::Starting { .. })
                    || (matches!(app.phase, MainPhase::Cancelling { .. })
                        && app
                            .recognition_session
                            .is_some_and(|current| current.cancel_command_id.is_none())))
            {
                enter_listening(app, ui_session.mode, Instant::now());
                changed = true;
            }
        }
        SessionPhase::Stopping | SessionPhase::Cancelling => {
            if !matches!(app.phase, MainPhase::Cancelling { .. }) {
                app.phase = MainPhase::Cancelling {
                    mode: ui_session.mode,
                    entered_at: Instant::now(),
                };
                changed = true;
            }
        }
        SessionPhase::Faulted => {
            app.recognition_session = None;
            app.phase = MainPhase::Error {
                message: "Recognition session failed and its resources may not have been released"
                    .to_owned(),
                entered_at: Instant::now(),
            };
            changed = true;
        }
        SessionPhase::Ended(reason) => {
            app.recognition_session = None;
            if !matches!(
                app.phase,
                MainPhase::Matched { .. }
                    | MainPhase::NoMatch { .. }
                    | MainPhase::Timeout { .. }
                    | MainPhase::Error { .. }
            ) {
                app.phase = match reason {
                    SessionEndReason::NoMatch => MainPhase::NoMatch {
                        entered_at: Instant::now(),
                        returned_to_ready_at: None,
                    },
                    SessionEndReason::TimedOut => MainPhase::Timeout {
                        mode: ui_session.mode,
                        entered_at: Instant::now(),
                    },
                    _ => MainPhase::Ready,
                };
            }
            changed = true;
        }
    }
    changed
}

fn show_controller_match(
    app: &mut AppState,
    candidate: TrackCandidate,
    mode: CaptureMode,
    config_generation: Option<u64>,
    event_time_utc: Option<SystemTime>,
) {
    let track = record_controller_track(app, candidate, mode, config_generation, event_time_utc);
    app.status_alternator = Some(StatusTextAlternator::after_success(track.recognized_at));
    if app.visible {
        app.success_wave_frame = current_success_wave_frame(app);
        app.phase = MainPhase::Matched {
            track,
            stage: ResultCardStage::Entering {
                started_at: Instant::now(),
            },
        };
    } else {
        app.success_wave_frame = None;
        app.phase = MainPhase::Ready;
        if mode == CaptureMode::Manual {
            queue_hidden_match_notification(app, &track);
        }
    }
}

fn apply_auto_match_candidate(
    app: &mut AppState,
    candidate: TrackCandidate,
    config_generation: Option<u64>,
    event_time_utc: Option<SystemTime>,
) -> bool {
    if !matches!(
        app.phase,
        MainPhase::Starting {
            mode: CaptureMode::Auto,
            ..
        } | MainPhase::Listening {
            mode: CaptureMode::Auto,
            ..
        }
    ) {
        return false;
    }
    if auto_candidate_already_seen(app, &candidate) {
        return false;
    }
    if let MainPhase::Starting { entered_at, .. } = &app.phase {
        enter_listening(app, CaptureMode::Auto, *entered_at);
    }
    let track = record_controller_track(
        app,
        candidate,
        CaptureMode::Auto,
        config_generation,
        event_time_utc,
    );
    if !app.visible {
        queue_hidden_match_notification(app, &track);
    }
    true
}

fn auto_candidate_already_seen(app: &mut AppState, candidate: &TrackCandidate) -> bool {
    let Some(identity) = auto_candidate_identity(candidate) else {
        return false;
    };
    // A track already present in past history is still a valid new Auto
    // Shazam result. Suppress only repeats from this active Auto session.
    !remember_auto_candidate_key(&mut app.auto_candidate_dedupe, identity)
}

pub(super) fn remember_auto_candidate_key(recent: &mut VecDeque<String>, identity: String) -> bool {
    if recent.contains(&identity) {
        return false;
    }
    recent.push_back(identity);
    while recent.len() > AUTO_CANDIDATE_DEDUPE_LIMIT {
        recent.pop_front();
    }
    true
}

fn auto_candidate_identity(candidate: &TrackCandidate) -> Option<String> {
    if let Some(key) = candidate
        .ui_metadata
        .as_ref()
        .and_then(|metadata| metadata.track_key.as_deref())
        .filter(|key| !key.trim().is_empty())
    {
        return Some(format!("track-key:{key}"));
    }
    if let Some(id) = candidate
        .track_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    {
        return Some(format!("candidate-id:{id}"));
    }
    let title = candidate.title.trim().to_lowercase();
    let artist = candidate.artist.trim().to_lowercase();
    (!title.is_empty() || !artist.is_empty()).then(|| format!("display:{title}\u{001f}{artist}"))
}

fn record_controller_track(
    app: &mut AppState,
    candidate: TrackCandidate,
    mode: CaptureMode,
    config_generation: Option<u64>,
    event_time_utc: Option<SystemTime>,
) -> Track {
    let metadata = candidate.ui_metadata.as_ref();
    let display_id = app.next_display_id;
    app.next_display_id = app.next_display_id.wrapping_add(1).max(1);
    let track_key = metadata.and_then(|metadata| metadata.track_key.clone());
    let page_link =
        resolve_page_link_for_generation(config_generation, &app.ui_configs, track_key.as_deref());
    let page_url = page_link.as_ref().ok().cloned();
    if let Err(reason) = page_link {
        let config = config_generation.and_then(|generation| app.ui_configs.get(&generation));
        eprintln!(
            "OpenShazam Shazam page link unavailable: config_generation={}, config_snapshot={}, webtrackurl={}, track_key={}, reason={}",
            config_generation.is_some(),
            config.is_some(),
            config.is_some_and(|config| config.web_track_url_template.is_some()),
            track_key.is_some(),
            reason.as_str(),
        );
    }
    let (recognized_at_utc, timestamp_source) =
        resolve_history_timestamp(event_time_utc, SystemTime::now());
    let track = Track {
        display_id,
        title: candidate.title,
        artist: candidate.artist,
        recognized_at: Instant::now(),
        recognized_at_utc,
        recognized_date_label: format_history_date_label(recognized_at_utc),
        timestamp_source,
        artwork_url: metadata.and_then(|metadata| metadata.artwork_url.clone()),
        hq_artwork_url: metadata.and_then(|metadata| metadata.hq_artwork_url.clone()),
        background_artwork_url: metadata.and_then(|metadata| metadata.background_image_url.clone()),
        track_key,
        config_generation,
        page_url,
    };
    if let Some(store) = app.history_store.as_ref() {
        store.save(PersistedTrack {
            title: track.title.clone(),
            artist: track.artist.clone(),
            recognized_at_utc: track.recognized_at_utc,
            track_key: track.track_key.clone(),
            artwork_url: track.artwork_url.clone(),
            hq_artwork_url: track.hq_artwork_url.clone(),
            background_artwork_url: track.background_artwork_url.clone(),
            shazam_page_url: track.page_url.as_ref().map(|url| url.as_str().to_owned()),
            source: match mode {
                CaptureMode::Manual => RecognitionSource::Manual,
                CaptureMode::Auto => RecognitionSource::Auto,
            },
        });
    }
    let artwork_request = register_artwork_request(app, &track);
    let previous_count = app.history.len();
    let previous_scroll_offset = app.scroll_offset;
    app.history.insert(0, track.clone());
    app.history.truncate(40);
    update_settings_history(app);
    let (next_scroll_offset, animate_insert) = history_offset_after_insert(
        previous_scroll_offset,
        app.history.len(),
        HISTORY_HEIGHT as f32,
    );
    app.scroll_offset = next_scroll_offset;
    app.history_scroll_animation = None;
    app.history_transition = animate_insert.then(|| HistoryMutationTransition {
        started_at: Instant::now(),
        event: HistoryMutationEvent::Insert {
            index: 0,
            previous_count,
        },
    });
    // The stationary pointer may now be over a different visible row.
    let next_hover = unsafe { hovered_history_row_for_cursor(app) };
    app.hovered_row = next_hover;
    invalidate_orphaned_artwork_requests(app);
    if let Some(request) = artwork_request {
        let identity = request.identity;
        if !start_artwork_load(
            app.main_hwnd,
            app.message_generation,
            request,
            unsafe { GetDpiForWindow(app.main_hwnd) }.max(96),
        ) {
            finish_active_artwork_request(&mut app.active_artwork_requests, identity);
        }
    }
    track
}
