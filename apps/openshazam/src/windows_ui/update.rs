#![deny(unsafe_op_in_unsafe_fn)]

use super::recognition::{request_capture_cancel, start_capture};
use super::{
    capture_mode_in_progress, ui_language, wide, AppState, UiLanguage, TIMER_UPDATE_COMPLETION,
    UPDATE_COMPLETION_POLL_MS, WM_UPDATE_INSTALL_CONFIRM, WM_UPDATE_WORKER_COMPLETED,
};
use crate::windows_update::{try_begin_update_operation, UpdateCandidate};
use std::{
    ffi::c_void,
    path::{Path, PathBuf},
    sync::{
        atomic::Ordering,
        mpsc::{self, Sender},
        Arc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        UI::WindowsAndMessaging::{KillTimer, PostMessageW, PostQuitMessage, SetTimer},
    },
};

pub(super) type UpdateCheckPayload = (bool, Result<Option<UpdateCandidate>, String>);

pub(super) struct QueuedUpdateCompletion {
    payload: UpdateWorkerCompletion,
}

enum UpdateWorkerCompletion {
    Check(UpdateCheckPayload),
    Download(UpdateDownloadPayload),
    Install(UpdateInstallPayload),
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum UpdateStatus {
    #[default]
    Idle = 0,
    Checking = 1,
    Current = 2,
    Available = 3,
    Downloading = 4,
    Installing = 5,
    Error = 6,
    Updated = 7,
}

impl UpdateStatus {
    pub(super) const fn id(self) -> u32 {
        self as u32
    }
}

pub(super) struct StagedInstallerPath {
    path: Option<PathBuf>,
    expected_name: String,
}

impl StagedInstallerPath {
    fn new(path: PathBuf, expected_name: String) -> Self {
        Self {
            path: Some(path),
            expected_name,
        }
    }

    fn as_path(&self) -> &Path {
        self.path
            .as_deref()
            .expect("staged installer ownership was already transferred")
    }

    fn keep(&mut self) {
        self.path = None;
    }
}

impl Drop for StagedInstallerPath {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        if let Err(error) =
            crate::windows_update::remove_staged_installer(&path, &self.expected_name)
        {
            eprintln!("OpenShazam could not remove a staged update installer: {error}");
        }
    }
}

pub(super) struct UpdateDownloadPayload {
    automatic: bool,
    candidate: UpdateCandidate,
    result: Result<StagedInstallerPath, String>,
}

pub(super) struct UpdateInstallConfirmationPayload {
    candidate: UpdateCandidate,
    installer: StagedInstallerPath,
}

pub(super) struct PendingUpdateInstall {
    pub(super) candidate: UpdateCandidate,
    pub(super) installer: StagedInstallerPath,
    pub(super) automatic: bool,
    pub(super) queued_at: Instant,
}

pub(super) struct UpdateInstallPayload {
    version: String,
    installer: StagedInstallerPath,
    result: Result<bool, String>,
}

const RECOGNITION_STOP_TIMEOUT: Duration = Duration::from_secs(30);

fn should_begin_update_check(interactive: bool, automatic_updates_enabled: bool) -> bool {
    interactive || automatic_updates_enabled
}

fn should_auto_install_verified_update(interactive: bool, automatic_updates_enabled: bool) -> bool {
    !interactive && automatic_updates_enabled
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingInstallAction {
    Discard,
    Wait,
    Start,
    Timeout,
}

fn pending_install_action(
    automatic: bool,
    automatic_updates_enabled: bool,
    recognition_active: bool,
    queued_at: Instant,
    now: Instant,
) -> PendingInstallAction {
    if automatic && !automatic_updates_enabled {
        PendingInstallAction::Discard
    } else if !recognition_active {
        PendingInstallAction::Start
    } else if now.saturating_duration_since(queued_at) >= RECOGNITION_STOP_TIMEOUT {
        PendingInstallAction::Timeout
    } else {
        PendingInstallAction::Wait
    }
}

pub(super) fn set_update_status(
    app: &mut AppState,
    status: UpdateStatus,
    version: &str,
    detail: &str,
) {
    app.update_status = status;
    app.update_version = version.to_owned();
    app.update_detail = detail.to_owned();
    sync_update_completion_timer(app);
    if let Some(host) = app.settings_host.as_ref() {
        host.update_status(
            app.update_auto_enabled,
            status.id(),
            version,
            detail,
            app.update_last_checked,
        );
    }
}

fn sync_update_completion_timer(app: &AppState) {
    unsafe {
        if app.update_operation_active {
            if SetTimer(
                Some(app.main_hwnd),
                TIMER_UPDATE_COMPLETION,
                UPDATE_COMPLETION_POLL_MS,
                None,
            ) == 0
            {
                eprintln!("Could not schedule the update completion fallback timer.");
            }
        } else {
            let _ = KillTimer(Some(app.main_hwnd), TIMER_UPDATE_COMPLETION);
        }
    }
}

pub(super) unsafe fn begin_update_check(app: &mut AppState, interactive: bool) {
    if !should_begin_update_check(interactive, app.update_auto_enabled) {
        return;
    }
    if !try_begin_update_operation(&mut app.update_operation_active) {
        return;
    }
    reap_finished_update_workers(app);
    app.update_candidate = None;
    set_update_status(app, UpdateStatus::Checking, "", "");
    let hwnd = app.main_hwnd;
    let thread_hwnd = hwnd.0 as isize;
    let worker_cancelled = Arc::clone(&app.update_cancelled);
    let completion_sender = app.update_completion_sender.clone();
    let spawned = thread::Builder::new()
        .name("openshazam-update-check".to_owned())
        .spawn(move || {
            let result = run_update_worker(|| {
                crate::windows_update::check_for_update(
                    env!("CARGO_PKG_VERSION"),
                    &worker_cancelled,
                )
            });
            if !worker_cancelled.load(Ordering::Acquire) {
                post_update_completion(
                    &completion_sender,
                    HWND(thread_hwnd as *mut c_void),
                    UpdateWorkerCompletion::Check((interactive, result)),
                );
            }
        });
    match spawned {
        Ok(worker) => app.update_workers.push(worker),
        Err(error) => {
            app.update_operation_active = false;
            set_update_status(app, UpdateStatus::Error, "", &error.to_string());
            eprintln!("Could not start the Shazam update check: {error}");
        }
    }
}

fn reap_finished_update_workers(app: &mut AppState) {
    let mut unfinished = Vec::with_capacity(app.update_workers.len());
    for worker in app.update_workers.drain(..) {
        if worker.is_finished() {
            if worker.join().is_err() {
                eprintln!("An OpenShazam update worker stopped unexpectedly.");
            }
        } else {
            unfinished.push(worker);
        }
    }
    app.update_workers = unfinished;
}

pub(super) fn cancel_and_join_update_workers(app: &mut AppState) {
    {
        let _gate = app.update_auto_install_gate.lock();
        app.update_cancelled.store(true, Ordering::Release);
    }
    for worker in app.update_workers.drain(..) {
        if worker.join().is_err() {
            eprintln!("An OpenShazam update worker stopped unexpectedly during shutdown.");
        }
    }
}

fn enqueue_then_wake<T, E>(
    sender: &Sender<T>,
    payload: T,
    wake: impl FnOnce() -> Result<(), E>,
) -> Result<Result<(), E>, mpsc::SendError<T>> {
    sender.send(payload)?;
    Ok(wake())
}

fn run_update_worker<T>(work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .unwrap_or_else(|_| Err("The update worker stopped unexpectedly.".to_owned()))
}

fn post_update_completion(
    sender: &Sender<QueuedUpdateCompletion>,
    hwnd: HWND,
    payload: UpdateWorkerCompletion,
) {
    let completion = QueuedUpdateCompletion { payload };
    match enqueue_then_wake(sender, completion, || unsafe {
        PostMessageW(Some(hwnd), WM_UPDATE_WORKER_COMPLETED, WPARAM(0), LPARAM(0))
    }) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!(
                "Could not wake the OpenShazam UI for an update completion; the fallback timer will drain it: {error}"
            );
        }
        Err(_) => {
            eprintln!("The OpenShazam UI closed before it could receive an update completion.");
        }
    }
}

pub(super) unsafe fn drain_update_worker_completions(app: &mut AppState) {
    loop {
        let completion = match app.update_completion_receiver.try_recv() {
            Ok(completion) => completion,
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
        };
        // SAFETY: Completion draining runs synchronously on the UI thread with
        // the live AppState owned by the currently-dispatching window.
        unsafe {
            match completion.payload {
                UpdateWorkerCompletion::Check(payload) => {
                    handle_update_check_completed(app, payload)
                }
                UpdateWorkerCompletion::Download(payload) => {
                    handle_update_download_completed(app, payload)
                }
                UpdateWorkerCompletion::Install(payload) => {
                    handle_update_install_completed(app, payload)
                }
            }
        }
    }
}

unsafe fn show_update_message(hwnd: HWND, message: &str, flags: u32) -> i32 {
    let title = wide("Shazam");
    let body = wide(message);
    // SAFETY: The caller supplies the live owner HWND on the UI thread; both
    // NUL-terminated UTF-16 buffers outlive the synchronous modal call.
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
            Some(hwnd),
            PCWSTR(body.as_ptr()),
            PCWSTR(title.as_ptr()),
            windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE(flags),
        )
        .0
    }
}

pub(super) unsafe fn handle_update_check_completed(
    app: &mut AppState,
    payload: UpdateCheckPayload,
) {
    app.update_operation_active = false;
    let (interactive, result) = payload;
    match result {
        Err(error) => {
            set_update_status(app, UpdateStatus::Error, "", &error);
            eprintln!("Shazam could not check for updates: {error}");
        }
        Ok(None) => {
            app.update_last_checked = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|elapsed| elapsed.as_secs());
            set_update_status(app, UpdateStatus::Current, "", "");
        }
        Ok(Some(candidate)) => {
            app.update_last_checked = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|elapsed| elapsed.as_secs());
            let version = candidate.manifest.version.clone();
            app.update_candidate = Some(candidate.clone());
            set_update_status(app, UpdateStatus::Available, &version, "");
            if should_auto_install_verified_update(interactive, app.update_auto_enabled) {
                // SAFETY: This handler runs on the UI thread with the live
                // AppState borrowed from its owning WndProc dispatch.
                unsafe { begin_update_download(app, candidate, true) };
            }
        }
    }
}

pub(super) unsafe fn begin_update_download(
    app: &mut AppState,
    candidate: UpdateCandidate,
    automatic: bool,
) {
    if !try_begin_update_operation(&mut app.update_operation_active) {
        return;
    }
    reap_finished_update_workers(app);
    set_update_status(
        app,
        UpdateStatus::Downloading,
        &candidate.manifest.version,
        "",
    );
    let hwnd = app.main_hwnd;
    let thread_hwnd = hwnd.0 as isize;
    let worker_candidate = candidate.clone();
    let worker_cancelled = Arc::clone(&app.update_cancelled);
    let completion_sender = app.update_completion_sender.clone();
    let spawned = thread::Builder::new()
        .name("openshazam-update-download".to_owned())
        .spawn(move || {
            let result = run_update_worker(|| {
                crate::windows_update::download_verified_installer(
                    &worker_candidate,
                    &worker_cancelled,
                )
            })
            .map(|path| {
                StagedInstallerPath::new(path, worker_candidate.manifest.installer_name.clone())
            });
            if !worker_cancelled.load(Ordering::Acquire) {
                post_update_completion(
                    &completion_sender,
                    HWND(thread_hwnd as *mut _),
                    UpdateWorkerCompletion::Download(UpdateDownloadPayload {
                        automatic,
                        candidate: worker_candidate,
                        result,
                    }),
                );
            } else if let Ok(installer) = result {
                drop(installer);
            }
        });
    match spawned {
        Ok(worker) => app.update_workers.push(worker),
        Err(error) => {
            app.update_operation_active = false;
            set_update_status(app, UpdateStatus::Error, "", &error.to_string());
        }
    }
}

pub(super) unsafe fn handle_update_download_completed(
    app: &mut AppState,
    payload: UpdateDownloadPayload,
) {
    app.update_operation_active = false;
    let UpdateDownloadPayload {
        automatic,
        candidate,
        result,
    } = payload;
    match result {
        Err(error) => {
            set_update_status(
                app,
                UpdateStatus::Error,
                &candidate.manifest.version,
                &error,
            );
        }
        Ok(installer) => {
            let action = download_completion_action(
                automatic,
                app.update_auto_enabled,
                capture_mode_in_progress(&app.phase).is_some(),
            );
            if action == DownloadCompletionAction::Discard {
                drop(installer);
                set_update_status(
                    app,
                    UpdateStatus::Available,
                    &candidate.manifest.version,
                    "",
                );
                return;
            }
            if !try_begin_update_operation(&mut app.update_operation_active) {
                drop(installer);
                return;
            }
            match action {
                DownloadCompletionAction::Confirm => {
                    let version = candidate.manifest.version.clone();
                    if let Err(error) = super::message_payload::post(
                        app.main_hwnd,
                        app.message_generation,
                        WM_UPDATE_INSTALL_CONFIRM,
                        UpdateInstallConfirmationPayload {
                            candidate,
                            installer,
                        },
                    ) {
                        app.update_operation_active = false;
                        set_update_status(
                            app,
                            UpdateStatus::Error,
                            &version,
                            &format!("Could not request update confirmation: {error}"),
                        );
                    }
                }
                DownloadCompletionAction::Install => {
                    start_verified_update_installer(app, candidate, installer, automatic);
                }
                DownloadCompletionAction::Discard => unreachable!("discarded above"),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DownloadCompletionAction {
    Discard,
    Confirm,
    Install,
}

fn download_completion_action(
    automatic: bool,
    automatic_updates_enabled: bool,
    recognition_active: bool,
) -> DownloadCompletionAction {
    if crate::windows_update::automatic_download_should_be_discarded(
        automatic,
        automatic_updates_enabled,
    ) {
        DownloadCompletionAction::Discard
    } else if !automatic && recognition_active {
        DownloadCompletionAction::Confirm
    } else {
        DownloadCompletionAction::Install
    }
}

/// # Safety
/// `app_ptr` must be the live state for `hwnd`, owned by the current UI thread,
/// and remain allocated until the nested modal message loop returns.
pub(super) unsafe fn handle_update_install_confirmation(
    app_ptr: *mut AppState,
    hwnd: HWND,
    payload: UpdateInstallConfirmationPayload,
) -> LRESULT {
    if app_ptr.is_null() {
        return LRESULT(0);
    }
    let message = match ui_language() {
        UiLanguage::Japanese => "\u{8a8d}\u{8b58}\u{4e2d}\u{3067}\u{3059}\u{3002}\u{8a8d}\u{8b58}\u{3092}\u{505c}\u{6b62}\u{3057}\u{3066}\u{66f4}\u{65b0}\u{30a4}\u{30f3}\u{30b9}\u{30c8}\u{30fc}\u{30e9}\u{30fc}\u{3092}\u{8d77}\u{52d5}\u{3057}\u{307e}\u{3059}\u{304b}\u{ff1f}",
        UiLanguage::English => {
            "Recognition is still active. Stop Shazam and start the verified update installer?"
        }
    };
    // MessageBox runs a nested loop. Keep only the stable AppState pointer live
    // here; do not create an AppState reference until the dialog has returned.
    // SAFETY: The WndProc supplies its live owner HWND. The modal message pump
    // may reenter the WndProc, so no mutable AppState reference is held here.
    let confirmed = unsafe { show_update_message(hwnd, message, 0x4 | 0x20 | 0x100) } == 6;

    // SAFETY: The WndProc contract keeps AppState alive through the synchronous
    // dialog; the null check above excludes a null pointer.
    let app = unsafe { &mut *app_ptr };
    if app.main_hwnd != hwnd
        || !app.update_operation_active
        || app.update_cancelled.load(Ordering::Acquire)
    {
        app.update_operation_active = false;
        return LRESULT(0);
    }
    if !confirmed {
        app.update_operation_active = false;
        set_update_status(
            app,
            UpdateStatus::Available,
            &payload.candidate.manifest.version,
            "",
        );
        return LRESULT(0);
    }
    start_verified_update_installer(app, payload.candidate, payload.installer, false);
    LRESULT(0)
}

pub(super) fn advance_pending_update_install(app: &mut AppState) {
    let Some(pending) = app.pending_update_install.as_ref() else {
        return;
    };
    let action = pending_install_action(
        pending.automatic,
        app.update_auto_enabled,
        app.recognition_session.is_some(),
        pending.queued_at,
        Instant::now(),
    );
    match action {
        PendingInstallAction::Wait => {}
        PendingInstallAction::Discard => cancel_pending_automatic_update_install(app),
        PendingInstallAction::Start => {
            let pending = app
                .pending_update_install
                .take()
                .expect("pending update was checked above");
            launch_verified_update_installer(
                app,
                pending.candidate,
                pending.installer,
                pending.automatic,
            );
        }
        PendingInstallAction::Timeout => {
            let pending = app
                .pending_update_install
                .take()
                .expect("pending update was checked above");
            app.update_operation_active = false;
            let detail = match ui_language() {
                UiLanguage::Japanese => {
                    "認識を停止できなかったため、更新を開始できませんでした。認識を手動で停止してから再試行してください。"
                }
                UiLanguage::English => {
                    "Could not stop recognition in time. Stop it manually and try again."
                }
            };
            set_update_status(
                app,
                UpdateStatus::Error,
                &pending.candidate.manifest.version,
                detail,
            );
            drop(pending);
            resume_auto_shazam_after_update(app);
        }
    }
}

pub(super) fn cancel_pending_automatic_update_install(app: &mut AppState) {
    let should_cancel = app
        .pending_update_install
        .as_ref()
        .is_some_and(|pending| pending.automatic);
    if !should_cancel {
        return;
    }
    let pending = app
        .pending_update_install
        .take()
        .expect("pending update was checked above");
    app.update_operation_active = false;
    set_update_status(
        app,
        UpdateStatus::Available,
        &pending.candidate.manifest.version,
        "",
    );
    drop(pending);
    resume_auto_shazam_after_update(app);
}

pub(super) fn abort_pending_update_after_cancel_failure(app: &mut AppState, reason: &str) {
    let Some(pending) = app.pending_update_install.take() else {
        return;
    };
    app.update_operation_active = false;
    if app.recognition_session.is_some() {
        // The capture did not stop, so Auto Shazam has continued running.
        app.resume_auto_shazam_after_update = false;
    }
    let detail = match ui_language() {
        UiLanguage::Japanese => {
            format!("認識を停止できず、更新を開始できませんでした。{reason}")
        }
        UiLanguage::English => {
            format!("Could not stop recognition before installing the update. {reason}")
        }
    };
    set_update_status(
        app,
        UpdateStatus::Error,
        &pending.candidate.manifest.version,
        &detail,
    );
    drop(pending);
    resume_auto_shazam_after_update(app);
}

pub(super) fn resume_auto_shazam_after_update(app: &mut AppState) {
    if !app.resume_auto_shazam_after_update
        || app.pending_update_install.is_some()
        || app.update_operation_active
        || app.recognition_session.is_some()
    {
        return;
    }
    app.resume_auto_shazam_after_update = false;
    if super::preferences::load_auto_shazam_preference() {
        start_capture(app, super::CaptureMode::Auto);
    }
}

fn start_verified_update_installer(
    app: &mut AppState,
    candidate: UpdateCandidate,
    installer: StagedInstallerPath,
    automatic: bool,
) {
    if let Some(session) = app.recognition_session {
        let resume_auto_shazam = session.mode == super::CaptureMode::Auto
            && super::preferences::load_auto_shazam_preference();
        app.resume_auto_shazam_after_update = resume_auto_shazam;
        let version = candidate.manifest.version.clone();
        app.pending_update_install = Some(PendingUpdateInstall {
            candidate,
            installer,
            automatic,
            queued_at: Instant::now(),
        });
        let wait_detail = match ui_language() {
            UiLanguage::Japanese => "Shazamの認識を停止してから更新をインストールします。",
            UiLanguage::English => "Stopping recognition before installing the update.",
        };
        set_update_status(app, UpdateStatus::Installing, &version, wait_detail);
        if let Err(error) = request_capture_cancel(app, false) {
            abort_pending_update_after_cancel_failure(app, &error);
        }
        return;
    }
    launch_verified_update_installer(app, candidate, installer, automatic);
}

fn launch_verified_update_installer(
    app: &mut AppState,
    candidate: UpdateCandidate,
    installer: StagedInstallerPath,
    automatic: bool,
) {
    set_update_status(
        app,
        UpdateStatus::Installing,
        &candidate.manifest.version,
        "",
    );
    let version = candidate.manifest.version.clone();
    let automatic_setting = Arc::clone(&app.update_auto_enabled_shared);
    let automatic_install_gate = Arc::clone(&app.update_auto_install_gate);
    let worker_cancelled = Arc::clone(&app.update_cancelled);
    let hwnd = app.main_hwnd.0 as isize;
    let completion_sender = app.update_completion_sender.clone();
    let result = thread::Builder::new()
        .name("openshazam-update-install".to_owned())
        .spawn(move || {
            let result = run_update_worker(|| {
                crate::windows_update::start_installer(
                    &candidate,
                    installer.as_path(),
                    automatic,
                    &worker_cancelled,
                    &automatic_setting,
                    &automatic_install_gate,
                )
            });
            let mut installer = installer;
            if matches!(&result, Ok(true)) {
                installer.keep();
            }
            if !worker_cancelled.load(Ordering::Acquire) {
                post_update_completion(
                    &completion_sender,
                    HWND(hwnd as *mut _),
                    UpdateWorkerCompletion::Install(UpdateInstallPayload {
                        version,
                        installer,
                        result,
                    }),
                );
            }
        });
    match result {
        Ok(worker) => app.update_workers.push(worker),
        Err(error) => {
            app.update_operation_active = false;
            set_update_status(app, UpdateStatus::Error, "", &error.to_string());
            resume_auto_shazam_after_update(app);
        }
    }
}

pub(super) unsafe fn handle_update_install_completed(
    app: &mut AppState,
    payload: UpdateInstallPayload,
) {
    app.update_operation_active = false;
    let UpdateInstallPayload {
        version,
        installer: _installer,
        result,
    } = payload;
    match result {
        Ok(true) => {
            // SAFETY: Completion handling runs on the process's UI thread.
            unsafe { PostQuitMessage(0) };
        }
        Ok(false) => {
            set_update_status(app, UpdateStatus::Available, &version, "");
            resume_auto_shazam_after_update(app);
        }
        Err(error) => {
            set_update_status(app, UpdateStatus::Error, &version, &error);
            resume_auto_shazam_after_update(app);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        download_completion_action, enqueue_then_wake, pending_install_action, run_update_worker,
        should_auto_install_verified_update, should_begin_update_check, DownloadCompletionAction,
        PendingInstallAction, UpdateStatus, RECOGNITION_STOP_TIMEOUT,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn background_update_checks_respect_the_automatic_setting_and_manual_checks_remain_available() {
        assert!(!should_begin_update_check(false, false));
        assert!(should_begin_update_check(false, true));
        assert!(should_begin_update_check(true, false));
        assert!(should_begin_update_check(true, true));
    }

    #[test]
    fn only_background_checks_with_automatic_updates_enabled_auto_install_verified_updates() {
        assert!(should_auto_install_verified_update(false, true));
        assert!(!should_auto_install_verified_update(false, false));
        assert!(!should_auto_install_verified_update(true, true));
        assert!(!should_auto_install_verified_update(true, false));
    }

    #[test]
    fn download_completion_covers_automatic_and_manual_install_paths() {
        use DownloadCompletionAction::{Confirm, Discard, Install};

        for recognition_active in [false, true] {
            assert_eq!(
                download_completion_action(true, true, recognition_active),
                Install,
                "an enabled automatic update installs after verification, regardless of recognition state"
            );
            assert_eq!(
                download_completion_action(true, false, recognition_active),
                Discard,
                "turning automatic updates off discards an automatic download"
            );
        }

        assert_eq!(download_completion_action(false, true, false), Install);
        assert_eq!(download_completion_action(false, false, false), Install);
        assert_eq!(download_completion_action(false, true, true), Confirm);
        assert_eq!(download_completion_action(false, false, true), Confirm);
    }

    #[test]
    fn update_install_waits_for_recognition_to_stop_before_launching() {
        let queued_at = Instant::now();
        assert_eq!(
            pending_install_action(
                false,
                false,
                true,
                queued_at,
                queued_at + Duration::from_secs(1),
            ),
            PendingInstallAction::Wait,
        );
        assert_eq!(
            pending_install_action(
                true,
                true,
                false,
                queued_at,
                queued_at + Duration::from_secs(1),
            ),
            PendingInstallAction::Start,
        );
    }

    #[test]
    fn pending_automatic_update_is_discarded_when_automatic_updates_are_disabled() {
        let queued_at = Instant::now();
        assert_eq!(
            pending_install_action(
                true,
                false,
                true,
                queued_at,
                queued_at + Duration::from_secs(1),
            ),
            PendingInstallAction::Discard,
        );
        assert_eq!(
            pending_install_action(
                false,
                false,
                true,
                queued_at,
                queued_at + Duration::from_secs(1),
            ),
            PendingInstallAction::Wait,
            "a manually requested update remains available when automatic updates are off",
        );
    }

    #[test]
    fn recognition_stop_wait_is_bounded() {
        let queued_at = Instant::now();
        assert_eq!(
            pending_install_action(
                true,
                true,
                true,
                queued_at,
                queued_at + RECOGNITION_STOP_TIMEOUT - Duration::from_millis(1),
            ),
            PendingInstallAction::Wait,
        );
        assert_eq!(
            pending_install_action(
                true,
                true,
                true,
                queued_at,
                queued_at + RECOGNITION_STOP_TIMEOUT,
            ),
            PendingInstallAction::Timeout,
        );
    }

    #[test]
    fn update_status_ids_match_the_settings_host_contract() {
        assert_eq!(UpdateStatus::Idle.id(), 0);
        assert_eq!(UpdateStatus::Checking.id(), 1);
        assert_eq!(UpdateStatus::Current.id(), 2);
        assert_eq!(UpdateStatus::Available.id(), 3);
        assert_eq!(UpdateStatus::Downloading.id(), 4);
        assert_eq!(UpdateStatus::Installing.id(), 5);
        assert_eq!(UpdateStatus::Error.id(), 6);
        assert_eq!(UpdateStatus::Updated.id(), 7);
        assert_eq!(UpdateStatus::default(), UpdateStatus::Idle);
    }

    #[test]
    fn update_completion_remains_queued_when_the_ui_wakeup_fails() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut wake_called = false;

        let wake_result = enqueue_then_wake(&sender, 42, || {
            wake_called = true;
            Err("message queue unavailable")
        })
        .expect("the completion must enter the durable queue before waking the UI");

        assert!(wake_called);
        assert_eq!(wake_result, Err("message queue unavailable"));
        assert_eq!(receiver.try_recv(), Ok(42));
    }

    #[test]
    fn update_worker_panics_become_recoverable_errors() {
        let result = run_update_worker(|| -> Result<(), String> {
            panic!("simulated update worker panic");
        });

        assert_eq!(
            result,
            Err("The update worker stopped unexpectedly.".to_owned())
        );
    }
}
