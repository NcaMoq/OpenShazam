//! Shared runtime state for the native Windows UI.

use super::{
    artwork_state::{
        ArtworkLoadFailure, ArtworkPixels, ArtworkRequest, ArtworkSource, TrackIdentity,
    },
    history::Track,
    preferences::{GlobalShortcut, ThemePreference},
    status_text::StatusTextAlternator,
    update::{PendingUpdateInstall, QueuedUpdateCompletion, UpdateStatus},
    ArrowEdge, CaptureMode, HistoryMutationTransition, HistoryScrollAnimation, MainPhase, Theme,
};
use crate::{
    ui_model::{self, UiLevels},
    windows_history_store::HistoryStore,
    windows_update::UpdateCandidate,
};
use openshazam_controller::{
    ApplicationController, CommandId, RecognitionContext, UiConfigSnapshot,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{atomic::AtomicBool, mpsc, Arc, Mutex},
    thread,
    time::Instant,
};
use windows::{
    System::DispatcherQueueController,
    Win32::{
        Foundation::HWND,
        Graphics::{Direct2D::ID2D1Factory, DirectWrite::IDWriteFactory},
    },
    UI::{Color, Composition::Compositor, ViewManagement::UISettings},
};

#[derive(Clone, Copy, Debug)]
pub(super) struct RecognitionSessionUi {
    pub(super) ticket: openshazam_controller::CommandTicket,
    pub(super) mode: CaptureMode,
    pub(super) context: Option<RecognitionContext>,
    pub(super) cancel_command_id: Option<CommandId>,
    pub(super) last_sequence: u64,
    pub(super) result_presented: bool,
}

pub(super) struct AppState {
    pub(super) main_hwnd: HWND,
    pub(super) message_generation: u64,
    pub(super) taskbar_created_message: u32,
    pub(super) global_shortcut: GlobalShortcut,
    pub(super) global_shortcut_registration_id: Option<i32>,
    pub(super) global_shortcut_capture_suspended: bool,
    pub(super) settings_host: Option<crate::windows_settings::SettingsHost>,
    pub(super) settings_opening: bool,
    pub(super) update_operation_active: bool,
    pub(super) pending_update_install: Option<PendingUpdateInstall>,
    pub(super) resume_auto_shazam_after_update: bool,
    pub(super) update_cancelled: Arc<AtomicBool>,
    pub(super) update_workers: Vec<thread::JoinHandle<()>>,
    pub(super) update_completion_sender: mpsc::Sender<QueuedUpdateCompletion>,
    pub(super) update_completion_receiver: mpsc::Receiver<QueuedUpdateCompletion>,
    pub(super) update_auto_enabled: bool,
    pub(super) update_auto_enabled_shared: Arc<AtomicBool>,
    pub(super) update_auto_install_gate: Arc<Mutex<()>>,
    pub(super) update_candidate: Option<UpdateCandidate>,
    pub(super) update_status: UpdateStatus,
    pub(super) update_detail: String,
    pub(super) update_version: String,
    pub(super) update_last_checked: Option<u64>,
    pub(super) d2d_factory: ID2D1Factory,
    pub(super) dwrite_factory: IDWriteFactory,
    pub(super) compositor: Compositor,
    pub(super) _dispatcher_queue: DispatcherQueueController,
    pub(super) theme_settings: UISettings,
    pub(super) theme_changed_token: Option<i64>,
    pub(super) system_foreground: Color,
    pub(super) phase: MainPhase,
    pub(super) history: Vec<Track>,
    pub(super) history_transition: Option<HistoryMutationTransition>,
    pub(super) history_composition_ready: bool,
    pub(super) scroll_offset: f32,
    pub(super) history_scroll_animation: Option<HistoryScrollAnimation>,
    pub(super) history_scroll_composition_ready: bool,
    pub(super) hovered_row: Option<usize>,
    pub(super) mouse_leave_tracking: bool,
    // UI-only adapter points. No service meter output is assumed or inferred.
    pub(super) ui_levels: UiLevels,
    // Core Animation default spring coefficients and the explicit 0.2s track
    // are supplied when Auto starts. Keeping this optional lets UI tests inject
    // an alternate measured profile without changing the recognition engine.
    pub(super) auto_spring: Option<ui_model::SpringProfile>,
    pub(super) theme: Theme,
    pub(super) theme_preference: ThemePreference,
    pub(super) theme_generation: u64,
    pub(super) visible: bool,
    pub(super) popup_opacity: u8,
    pub(super) main_activated: bool,
    pub(super) popup_activation_grace_until: Option<Instant>,
    pub(super) deactivation_hide_pending: bool,
    pub(super) phase_timer_active: bool,
    pub(super) animation_timer_active: bool,
    pub(super) result_overlay_composition_ready: bool,
    pub(super) status_text_composition_ready: bool,
    pub(super) startup_animation_pending: bool,
    pub(super) startup_logo_consumed: bool,
    pub(super) arrow_edge: ArrowEdge,
    pub(super) arrow_center: f32,
    pub(super) mouse_down_center: bool,
    pub(super) long_press_consumed: bool,
    pub(super) single_click_pending: bool,
    pub(super) recognition_controller: Option<ApplicationController>,
    pub(super) recognition_session: Option<RecognitionSessionUi>,
    pub(super) history_store: Option<HistoryStore>,
    pub(super) auto_candidate_dedupe: VecDeque<String>,
    pub(super) controller_sequence: u64,
    pub(super) dropped_notifications: u64,
    pub(super) status_alternator: Option<StatusTextAlternator>,
    pub(super) success_wave_frame: Option<ui_model::WaveFrame>,
    pub(super) next_display_id: u64,
    pub(super) next_artwork_request_id: u64,
    pub(super) artwork_generations: HashMap<TrackIdentity, u64>,
    pub(super) active_artwork_requests: HashMap<TrackIdentity, ArtworkRequest>,
    pub(super) artwork_failures: HashMap<TrackIdentity, ArtworkLoadFailure>,
    pub(super) artwork_resources: HashMap<u64, (ArtworkSource, ArtworkPixels)>,
    pub(super) pending_match_notifications: HashMap<u64, Track>,
    pub(super) owned_balloon_icons:
        VecDeque<Option<windows::Win32::UI::WindowsAndMessaging::HICON>>,
    pub(super) ui_configs: HashMap<u64, UiConfigSnapshot>,
    pub(super) message: Option<String>,
}
