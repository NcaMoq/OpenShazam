//! Platform recognition backends reusable by native UI hosts.

pub mod page_link;
pub mod recognition;
pub mod session_end;
#[cfg(any(windows, test))]
mod worker_lifecycle;

#[cfg(feature = "dev-cli")]
pub mod cli;

#[cfg(windows)]
mod windows_backend;

#[cfg(windows)]
pub use windows_backend::WindowsRecognitionBackend;
