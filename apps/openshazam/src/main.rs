use std::process::ExitCode;

#[cfg(any(windows, test))]
mod ui_model;
#[cfg(windows)]
mod windows_ui;

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) == Some("ui") {
            if args.len() != 1 {
                eprintln!("usage: openshazam ui");
                return ExitCode::from(2);
            }
            return match windows_ui::run() {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("OpenShazam UI failed: {error}");
                    ExitCode::from(1)
                }
            };
        }
    }

    openshazam::cli::run_exit_code()
}
