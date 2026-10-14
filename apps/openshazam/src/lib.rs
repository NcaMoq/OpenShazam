//! Platform recognition backends reusable by native UI hosts.

#[cfg(windows)]
#[path = "main.rs"]
#[allow(dead_code)]
mod cli;

#[cfg(windows)]
mod windows_backend;

#[cfg(windows)]
pub use windows_backend::WindowsRecognitionBackend;
