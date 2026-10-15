//! Developer CLI only. The Windows desktop application is the WinUI 3
//! project in settings-ui, backed by the production openshazam-ui-bridge binary.
use std::process::ExitCode;

fn main() -> ExitCode {
    openshazam::cli::run_exit_code()
}
