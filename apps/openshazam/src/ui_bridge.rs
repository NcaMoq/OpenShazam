//! Strict JSONL protocol boundary between the WinUI 3 process and Rust's
//! existing Windows WASAPI/Progressive recognition controller.
//! No XAML thread ever processes audio or blocks on a native request.
#[cfg(windows)]
mod host {
    use openshazam::{page_link, WindowsRecognitionBackend};
    use openshazam_controller::{
        ApplicationController, RecognitionCommand, RecognitionContext, RecognitionEvent,
        RecognitionEventKind, RecognitionMode, SessionId, NativeAutoUiEvent,
        NativeManualResultStage, NativeManualUiEvent, UiConfigSnapshot,
    };
    use serde_json::{json, Value};
    use std::{
        collections::HashMap,
        io::{self, BufRead, Write},
        sync::mpsc::{self, Receiver, TryRecvError},
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    const PROTOCOL: &str = "openshazam.bridge.v1";
    const MAX_COMMAND_BYTES: usize = 4_096;

    enum Command {
        StartManual,
        StartAuto,
        Stop,
        Cancel,
        Quit,
    }

    fn parse_command(line: &str) -> Result<Command, &'static str> {
        if line.len() > MAX_COMMAND_BYTES {
            return Err("command too long");
        }
        let value: Value = serde_json::from_str(line).map_err(|_| "invalid JSON")?;
        match value.get("command").and_then(Value::as_str) {
            Some("start_manual") => Ok(Command::StartManual),
            Some("start_auto") => Ok(Command::StartAuto),
            Some("stop") => Ok(Command::Stop),
            Some("cancel") => Ok(Command::Cancel),
            Some("shutdown") => Ok(Command::Quit),
            _ => Err("unknown command"),
        }
    }

    fn command_reader() -> Receiver<Result<Command, &'static str>> {
        let (sender, receiver) = mpsc::sync_channel(16);
        thread::spawn(move || {
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                let command = match line {
                    Ok(line) => parse_command(&line),
                    Err(_) => break,
                };
                if sender.send(command).is_err() {
                    return;
                }
            }
            let _ = sender.send(Ok(Command::Quit));
        });
        receiver
    }

    fn send(out: &mut impl Write, message: Value) -> io::Result<()> {
        serde_json::to_writer(&mut *out, &message).map_err(io::Error::other)?;
        out.write_all(b"\n")?;
        out.flush()
    }

    fn millis(value: SystemTime) -> u64 {
        value.duration_since(UNIX_EPOCH)
            .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }

    fn match_message(
        title: &str,
        artist: &str,
        artwork: Option<&str>,
        track_key: Option<&str>,
        config: Option<&UiConfigSnapshot>,
        occurred_at: SystemTime,
        mode: &str,
    ) -> Value {
        let page_url = page_link::resolve(
            config.and_then(|c| c.web_track_url_template.as_deref()),
            track_key,
        );
        json!({
            "kind": "match",
            "mode": mode,
            "title": title,
            "artist": artist,
            "artworkUrl": artwork,
            "pageUrl": page_url,
            "recognizedAtUnixMs": millis(occurred_at),
        })
    }

    fn emit_event(
        out: &mut impl Write,
        event: RecognitionEvent,
        configs: &mut HashMap<u64, UiConfigSnapshot>,
    ) -> io::Result<()> {
        let generation = event.config_generation;
        let occurred = event.emitted_at_utc;
        match event.kind {
            RecognitionEventKind::UiConfigSnapshot(config) => {
                configs.insert(config.generation, config);
                // Bounded cache: each session owns at most one live generation.
                if configs.len() > 8 {
                    if let Some(oldest) = configs.keys().copied().min() {
                        configs.remove(&oldest);
                    }
                }
            }
            RecognitionEventKind::CaptureStarted => {
                send(out, json!({"kind":"status", "value":"listening"}))?;
            }
            RecognitionEventKind::ManualUiEvent(NativeManualUiEvent::Result(result))
                if result.stage == NativeManualResultStage::TaggingDidFinish =>
            {
                let track = result.primary_track;
                let metadata = track.ui_metadata.as_ref();
                send(out, match_message(
                    &track.title,
                    &track.artist,
                    metadata.and_then(|v| v.artwork_url.as_deref()),
                    metadata.and_then(|v| v.track_key.as_deref()),
                    generation.and_then(|v| configs.get(&v)),
                    occurred,
                    "manual",
                ))?;
            }
            RecognitionEventKind::AutoUiEvent(NativeAutoUiEvent::ContinuousMatchCandidate(result)) => {
                let track = result.primary_track;
                let metadata = track.ui_metadata.as_ref();
                send(out, match_message(
                    &track.title,
                    &track.artist,
                    metadata.and_then(|v| v.artwork_url.as_deref()),
                    metadata.and_then(|v| v.track_key.as_deref()),
                    generation.and_then(|v| configs.get(&v)),
                    occurred,
                    "auto",
                ))?;
            }
            RecognitionEventKind::NoMatch { .. } |
            RecognitionEventKind::ManualUiEvent(NativeManualUiEvent::NoMatch) => {
                send(out, json!({"kind":"status", "value":"no_match"}))?;
            }
            RecognitionEventKind::SessionTimedOut => {
                send(out, json!({"kind":"status", "value":"timeout"}))?;
            }
            RecognitionEventKind::RecognitionError { kind, message } => {
                send(out, json!({"kind":"error", "code":format!("{kind:?}"), "message":message}))?;
            }
            RecognitionEventKind::SessionEnded { reason } => {
                send(out, json!({"kind":"ended", "reason":format!("{reason:?}")}))?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let controller = ApplicationController::spawn(
            WindowsRecognitionBackend::new(), 16, 64,
        )?;
        let input = command_reader();
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let mut active: Option<SessionId> = None;
        let mut configs = HashMap::new();
        send(&mut out, json!({"kind":"ready", "protocol":PROTOCOL}))?;
        'main: loop {
            for _ in 0..16 {
                match input.try_recv() {
                    Ok(Ok(Command::Quit)) | Err(TryRecvError::Disconnected) => break 'main,
                    Ok(Ok(command)) => {
                        let outcome = match command {
                            Command::StartManual if active.is_none() => controller.commands
                                .start_manual().map(|ticket| {
                                    active = Some(ticket.session_id);
                                    ("manual", ticket.session_id.0)
                                }),
                            Command::StartAuto if active.is_none() => controller.commands
                                .start_auto(RecognitionContext::Foreground).map(|ticket| {
                                    active = Some(ticket.session_id);
                                    ("auto", ticket.session_id.0)
                                }),
                            Command::Stop if active.is_some() => {
                                let id = active.expect("checked active session");
                                controller.commands.submit(RecognitionCommand::StopRecognition {
                                    session_id: id,
                                }).map(|_| ("stopping", id.0))
                            }
                            Command::Cancel if active.is_some() => {
                                let id = active.expect("checked active session");
                                controller.commands.submit(RecognitionCommand::CancelRecognition {
                                    session_id: id,
                                }).map(|_| ("cancelling", id.0))
                            }
                            _ => {
                                send(&mut out, json!({"kind":"error", "code":"invalid_state",
                                    "message":"command does not match active session state"}))?;
                                continue;
                            }
                        };
                        match outcome {
                            Ok((mode, session)) => send(&mut out, json!({
                                "kind":"status", "value": mode, "session":session
                            }))?,
                            Err(error) => send(&mut out, json!({
                                "kind":"error", "code":"command_rejected",
                                "message":format!("{error:?}")
                            }))?,
                        }
                    }
                    Ok(Err(error)) => send(&mut out, json!({
                        "kind":"error", "code":"bad_command", "message":error
                    }))?,
                    Err(TryRecvError::Empty) => break,
                }
            }
            for _ in 0..64 {
                match controller.events.try_recv() {
                    Ok(event) => {
                        if event.session_id.is_some() && event.session_id != active {
                            continue;
                        }
                        if matches!(event.kind, RecognitionEventKind::SessionEnded { .. }) {
                            active = None;
                        }
                        emit_event(&mut out, event, &mut configs)?;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break 'main,
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        // The controller joins its capture and HTTP workers before returning.
        match controller.join() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(error.into()),
            Err(_) => Err("controller thread panicked".into()),
        }
    }
}

#[cfg(windows)]
fn main() {
    if let Err(error) = host::run() {
        eprintln!("OpenShazam UI bridge: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("OpenShazam UI bridge is supported on Windows only.");
    std::process::exit(1);
}
