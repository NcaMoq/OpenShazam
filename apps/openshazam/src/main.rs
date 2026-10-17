use std::process::ExitCode;

#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod ui_model;
#[cfg(windows)]
mod windows_settings;
#[cfg(windows)]
mod windows_ui;

#[cfg(windows)]
fn run_ui(start_hidden: bool) -> ExitCode {
    match windows_ui::run(start_hidden) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("OpenShazam UI failed: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
enum UiLaunchArguments {
    NotUi,
    Invalid,
    Launch { start_hidden: bool },
}

#[cfg(windows)]
fn parse_ui_launch_arguments(args: &[String]) -> UiLaunchArguments {
    match args {
        [command] if command == "ui" => UiLaunchArguments::Launch {
            start_hidden: false,
        },
        [command, flag] if command == "ui" && flag == "--startup" => {
            UiLaunchArguments::Launch { start_hidden: true }
        }
        [command, ..] if command == "ui" => UiLaunchArguments::Invalid,
        _ => UiLaunchArguments::NotUi,
    }
}

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match parse_ui_launch_arguments(&args) {
            UiLaunchArguments::Launch { start_hidden } => return run_ui(start_hidden),
            UiLaunchArguments::Invalid => {
                eprintln!("usage: openshazam ui [--startup]");
                return ExitCode::from(2);
            }
            UiLaunchArguments::NotUi => {}
        }
        // The product-only MSIX build excludes the development CLI. Windows
        // launches its startup task without command-line arguments, so that
        // build enters the tray UI directly and never starts recognition.
        if args.is_empty() && !cfg!(feature = "dev-cli") {
            return run_ui(false);
        }
    }

    #[cfg(feature = "dev-cli")]
    {
        openshazam::cli::run_exit_code()
    }

    #[cfg(not(feature = "dev-cli"))]
    {
        eprintln!("usage: openshazam ui");
        ExitCode::from(2)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::{parse_ui_launch_arguments, UiLaunchArguments};

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn startup_launch_is_hidden_but_explicit_ui_launch_is_visible() {
        assert_eq!(
            parse_ui_launch_arguments(&args(&["ui"])),
            UiLaunchArguments::Launch {
                start_hidden: false
            }
        );
        assert_eq!(
            parse_ui_launch_arguments(&args(&["ui", "--startup"])),
            UiLaunchArguments::Launch { start_hidden: true }
        );
    }

    #[test]
    fn unknown_ui_flags_are_rejected_without_changing_other_commands() {
        assert_eq!(
            parse_ui_launch_arguments(&args(&["ui", "--unknown"])),
            UiLaunchArguments::Invalid
        );
        assert_eq!(
            parse_ui_launch_arguments(&args(&["recognize-file", "song.mp3"])),
            UiLaunchArguments::NotUi
        );
    }
}
