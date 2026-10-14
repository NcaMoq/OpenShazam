//! Developer-only command line frontend. CLI parsing, stdin, Ctrl+C, output,
//! and exit-code handling live here; recognition itself is shared from the
//! platform-independent engine module.

#[cfg(any(windows, target_os = "macos"))]
use crate::recognition;
#[cfg(windows)]
use crate::recognition::ManualTriggerSignal;
#[cfg(any(windows, target_os = "macos"))]
use crate::recognition::NativeContextState;
#[cfg(any(windows, target_os = "macos", test))]
use crate::recognition::ProgressiveCaptureMode;
#[cfg(any(windows, target_os = "macos", test))]
use shazam_http_lab::NativeTaggingRequestContext;
#[cfg(any(windows, target_os = "macos", test))]
use shazam_progressive::NativeTaggingType;
use std::{io, process::ExitCode};

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
enum ManualTriggerRead {
    Enter,
    EndOfInput,
}

#[cfg(any(windows, test))]
fn read_manual_trigger<R: io::BufRead>(reader: &mut R) -> io::Result<ManualTriggerRead> {
    let mut line = String::new();
    let bytes_read = reader.read_line(&mut line)?;
    if bytes_read > 0 && line.ends_with('\n') {
        Ok(ManualTriggerRead::Enter)
    } else {
        Ok(ManualTriggerRead::EndOfInput)
    }
}

#[cfg(windows)]
fn spawn_manual_trigger_receiver() -> std::sync::mpsc::Receiver<ManualTriggerSignal> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut stdin = stdin.lock();
        let signal = match read_manual_trigger(&mut stdin) {
            Ok(ManualTriggerRead::Enter) => ManualTriggerSignal::Enter,
            Ok(ManualTriggerRead::EndOfInput) => ManualTriggerSignal::EndOfInput,
            Err(error) => ManualTriggerSignal::InputError(error.to_string()),
        };
        let _ = sender.send(signal);
    });
    receiver
}

pub fn run_exit_code() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("OpenShazam failed: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    let result = run_windows_command(&args);
    #[cfg(target_os = "macos")]
    let result = run_macos_command(&args);
    #[cfg(not(any(windows, target_os = "macos")))]
    let result: Result<(), Box<dyn std::error::Error>> = {
        let _ = args;
        Err(Box::new(io::Error::new(
            io::ErrorKind::Unsupported,
            "audio capture is available only on Windows and macOS",
        )))
    };
    result
}

#[cfg(any(windows, target_os = "macos", test))]
fn parse_progressive_capture_options(
    args: &[String],
    allow_device_id: bool,
) -> Result<(ProgressiveCaptureMode, bool, Option<String>), io::Error> {
    let mut tagging_type = None;
    let mut request_context = None;
    let mut jsonl_output = false;
    let mut device_id = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--events-jsonl" if !jsonl_output => jsonl_output = true,
            "--events-jsonl" => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "duplicate --events-jsonl option",
                ));
            }
            "--tagging-type" => {
                if tagging_type.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "duplicate --tagging-type option",
                    ));
                }
                index += 1;
                tagging_type = Some(
                    args.get(index)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "--tagging-type requires manual or auto",
                            )
                        })?
                        .as_str(),
                );
            }
            "--native-context" => {
                if request_context.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "duplicate --native-context option",
                    ));
                }
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--native-context requires foreground or background",
                    )
                })?;
                request_context = Some(match value.as_str() {
                    "foreground" => NativeTaggingRequestContext::Foreground,
                    "background" => NativeTaggingRequestContext::Background,
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--native-context requires foreground or background",
                        ))
                    }
                });
            }
            value if !value.starts_with('-') && allow_device_id && device_id.is_none() => {
                device_id = Some(value.to_owned());
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "unexpected progressive-recognition argument: {}",
                        args[index]
                    ),
                ))
            }
        }
        index += 1;
    }

    let tagging_type = match tagging_type {
        None | Some("manual") => NativeTaggingType::Manual,
        Some("auto") => NativeTaggingType::Auto,
        Some(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--tagging-type requires manual or auto",
            ))
        }
    };
    match (tagging_type, request_context) {
        (NativeTaggingType::Manual, Some(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--native-context is only valid with --tagging-type auto",
        )),
        (NativeTaggingType::Auto, None) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Auto recognition requires --native-context foreground or background",
        )),
        _ => Ok((
            ProgressiveCaptureMode {
                tagging_type,
                request_context,
            },
            jsonl_output,
            device_id,
        )),
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn watch_stdin_context_updates(
    initial: Option<NativeTaggingRequestContext>,
) -> Result<NativeContextState, io::Error> {
    use std::io::BufRead;
    let state = NativeContextState::new(initial);
    if initial.is_some() {
        let updates = state.clone();
        std::thread::Builder::new()
            .name("openshazam-context-input".into())
            .spawn(move || {
                for line in io::stdin().lock().lines() {
                    match line {
                        Ok(line) => match line.trim() {
                            "foreground" => {
                                updates.set(NativeTaggingRequestContext::Foreground);
                                eprintln!("recognition context updated: foreground");
                            }
                            "background" => {
                                updates.set(NativeTaggingRequestContext::Background);
                                eprintln!("recognition context updated: background");
                            }
                            "" => {}
                            _ => eprintln!(
                                "ignored context input; expected foreground or background"
                            ),
                        },
                        Err(error) => {
                            eprintln!("context input stopped: {error}");
                            break;
                        }
                    }
                }
            })?;
    }
    Ok(state)
}

#[cfg(windows)]
fn run_windows_command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use std::{fs::OpenOptions, io::Write, path::Path};
    match args.first().map(String::as_str) {
        Some("list-devices") if args.len() == 1 => recognition::list_render_devices(),
        Some("capture-signature-loopback") if (3..=4).contains(&args.len()) => {
            let output = Path::new(&args[1]);
            refuse_overwrite(output)?;
            let capture_seconds = parse_capture_seconds(&args[2])?;
            let signature = recognition::capture_loopback_signature(
                args.get(3).map(String::as_str),
                capture_seconds,
            )?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output)?;
            file.write_all(&signature)?;
            println!(
                "Wrote {} bytes to {} (local only; no network request was made)",
                signature.len(),
                output.display()
            );
            Ok(())
        }
        Some("recognize-loopback" | "recognize-loopback-progressive") => {
            let (mode, jsonl_output, selected_device_id) =
                parse_progressive_capture_options(&args[1..], true)?;
            let context_state = watch_stdin_context_updates(mode.request_context)?;
            let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let signal_cancelled = std::sync::Arc::clone(&cancelled);
            let accepted_control = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0));
            let signal_control = std::sync::Arc::clone(&accepted_control);
            ctrlc::set_handler(move || {
                signal_cancelled.store(true, std::sync::atomic::Ordering::Release);
                let _ = signal_control.compare_exchange(
                    0,
                    2,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                );
            })?;
            let manual_trigger_receiver = (mode.tagging_type == NativeTaggingType::Manual)
                .then(spawn_manual_trigger_receiver);
            let control = recognition::WindowsProgressiveControl {
                cancelled,
                stop_requested: Default::default(),
                accepted_control,
                config_generation: Default::default(),
                config_fetching: Default::default(),
                cleanup_failed: Default::default(),
                manual_trigger_receiver,
                events: None,
            };
            let reason = recognition::recognize_loopback_progressive_controlled(
                selected_device_id.as_deref(),
                mode,
                context_state,
                jsonl_output,
                control,
            )?;
            match reason {
                openshazam_controller::SessionEndReason::Matched
                | openshazam_controller::SessionEndReason::NoMatch
                | openshazam_controller::SessionEndReason::Stopped => Ok(()),
                openshazam_controller::SessionEndReason::Cancelled
                | openshazam_controller::SessionEndReason::Shutdown => Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "progressive recognition was cancelled",
                )
                .into()),
                openshazam_controller::SessionEndReason::TimedOut => {
                    Err(io::Error::other("progressive recognition timed out").into())
                }
                openshazam_controller::SessionEndReason::Exhausted
                | openshazam_controller::SessionEndReason::Failed => {
                    Err(io::Error::other("progressive recognition ended without a match").into())
                }
            }
        }
        _ => {
            print_windows_usage();
            Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid command or arguments").into())
        }
    }
}

#[cfg(windows)]
fn print_windows_usage() {
    eprintln!("usage: openshazam list-devices");
    eprintln!("   or: openshazam recognize-loopback [device-id] [--tagging-type manual|auto] [--native-context foreground|background] [--events-jsonl]");
    eprintln!("   or: openshazam recognize-loopback-progressive [device-id] [--tagging-type manual|auto] [--native-context foreground|background] [--events-jsonl] (alias)");
    eprintln!(
        "   or: openshazam capture-signature-loopback <new-output.sig> <seconds> [device-id]"
    );
}

#[cfg(windows)]
fn refuse_overwrite(output: &std::path::Path) -> io::Result<()> {
    if output.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "refusing to overwrite existing output: {}",
                output.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_macos_command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use std::path::Path;
    match args.first().map(String::as_str) {
        Some("capture-signature") if args.len() == 3 => {
            let seconds = parse_capture_seconds(&args[2])?;
            recognition::macos_capture::capture_signature(Path::new(&args[1]), seconds)
        }
        Some("recognize-microphone-progressive") => {
            let (mode, jsonl_output, _) = parse_progressive_capture_options(&args[1..], false)?;
            let context_state = watch_stdin_context_updates(mode.request_context)?;
            recognition::macos_capture::recognize_progressive(
                mode.tagging_type,
                context_state,
                jsonl_output,
            )
        }
        _ => {
            eprintln!("usage: openshazam capture-signature <new-output.sig> <seconds>");
            eprintln!("   or: openshazam recognize-microphone-progressive [--tagging-type manual|auto] [--native-context foreground|background] [--events-jsonl]");
            Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid command or arguments").into())
        }
    }
}

#[cfg(any(windows, target_os = "macos", test))]
fn parse_capture_seconds(value: &str) -> Result<usize, io::Error> {
    let seconds = value.parse::<usize>().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "capture duration must be an integer from 1 to 30 seconds",
        )
    })?;
    if !(1..=30).contains(&seconds) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capture duration must be an integer from 1 to 30 seconds",
        ));
    }
    Ok(seconds)
}

#[cfg(test)]
mod tests {
    use super::{
        parse_capture_seconds, parse_progressive_capture_options, read_manual_trigger,
        ManualTriggerRead,
    };
    use shazam_http_lab::NativeTaggingRequestContext;
    use shazam_progressive::NativeTaggingType;

    #[test]
    fn parses_manual_and_auto_cli_options() {
        let (manual, jsonl, device) = parse_progressive_capture_options(&[], false).unwrap();
        assert_eq!(manual.tagging_type, NativeTaggingType::Manual);
        assert!(!jsonl);
        assert_eq!(device, None);
        let args = vec![
            "--tagging-type".into(),
            "auto".into(),
            "--native-context".into(),
            "background".into(),
            "--events-jsonl".into(),
        ];
        let (auto, jsonl, _) = parse_progressive_capture_options(&args, false).unwrap();
        assert_eq!(auto.tagging_type, NativeTaggingType::Auto);
        assert_eq!(
            auto.request_context,
            Some(NativeTaggingRequestContext::Background)
        );
        assert!(jsonl);
    }

    #[test]
    fn rejects_ambiguous_cli_context_and_bounds_capture_duration() {
        let args = vec!["--tagging-type".into(), "auto".into()];
        assert!(parse_progressive_capture_options(&args, false).is_err());
        assert_eq!(parse_capture_seconds("30").unwrap(), 30);
        assert!(parse_capture_seconds("31").is_err());
    }

    #[test]
    fn cli_stdin_trigger_distinguishes_enter_eof_partial_input_and_read_error() {
        assert_eq!(
            read_manual_trigger(&mut std::io::Cursor::new(b"\n".as_slice())).unwrap(),
            ManualTriggerRead::Enter
        );
        assert_eq!(
            read_manual_trigger(&mut std::io::Cursor::new(Vec::<u8>::new())).unwrap(),
            ManualTriggerRead::EndOfInput
        );
        assert_eq!(
            read_manual_trigger(&mut std::io::Cursor::new(b"partial".as_slice())).unwrap(),
            ManualTriggerRead::EndOfInput
        );

        struct FailingReader;
        impl std::io::Read for FailingReader {
            fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("synthetic stdin read error"))
            }
        }
        assert_eq!(
            read_manual_trigger(&mut std::io::BufReader::new(FailingReader))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Other
        );
    }
}
