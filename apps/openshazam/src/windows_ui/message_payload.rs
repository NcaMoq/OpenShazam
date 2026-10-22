#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    any::Any,
    collections::HashMap,
    sync::{Mutex, MutexGuard, OnceLock},
};

use windows::{
    core::{Error, Result, HRESULT},
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging::PostMessageW,
    },
};

struct Entry {
    hwnd: isize,
    generation: u64,
    message: u32,
    payload: Box<dyn Any + Send>,
}

#[derive(Default)]
struct RegistryState {
    next_token: isize,
    active_windows: HashMap<isize, u64>,
    entries: HashMap<isize, Entry>,
}

#[derive(Default)]
struct Registry {
    state: Mutex<RegistryState>,
}

impl Registry {
    fn lock(&self) -> MutexGuard<'_, RegistryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn register_window(&self, hwnd: HWND, generation: u64) {
        let hwnd = hwnd.0 as isize;
        let stale_entries = {
            let mut state = self.lock();
            state.active_windows.insert(hwnd, generation);
            let tokens = state
                .entries
                .iter()
                .filter_map(|(token, entry)| {
                    (entry.hwnd == hwnd && entry.generation != generation).then_some(*token)
                })
                .collect::<Vec<_>>();
            tokens
                .into_iter()
                .filter_map(|token| state.entries.remove(&token))
                .collect::<Vec<_>>()
        };
        drop(stale_entries);
    }

    fn insert<T: Any + Send + 'static>(
        &self,
        hwnd: HWND,
        generation: u64,
        message: u32,
        payload: T,
    ) -> Option<isize> {
        let mut state = self.lock();
        let hwnd = hwnd.0 as isize;
        if state.active_windows.get(&hwnd) != Some(&generation) {
            return None;
        }
        let token = loop {
            let candidate = state.next_token.max(1);
            state.next_token = candidate.wrapping_add(1);
            if !state.entries.contains_key(&candidate) {
                break candidate;
            }
        };
        state.entries.insert(
            token,
            Entry {
                hwnd,
                generation,
                message,
                payload: Box::new(payload),
            },
        );
        Some(token)
    }

    fn take<T: Any + Send + 'static>(
        &self,
        hwnd: HWND,
        generation: u64,
        message: u32,
        token: isize,
    ) -> Option<T> {
        if token <= 0 {
            return None;
        }
        let entry = {
            let mut state = self.lock();
            let entry = state.entries.get(&token)?;
            if entry.hwnd != hwnd.0 as isize || entry.message != message {
                return None;
            }
            // This exact window/message dispatch consumes the token. If its
            // handler expects the wrong type, the payload is dropped below
            // after releasing the registry lock instead of lingering forever.
            state.entries.remove(&token)?
        };
        if entry.generation != generation {
            drop(entry);
            return None;
        }
        entry.payload.downcast::<T>().ok().map(|payload| *payload)
    }

    fn close_window(&self, hwnd: HWND, generation: Option<u64>) -> usize {
        let hwnd = hwnd.0 as isize;
        let discarded = {
            let mut state = self.lock();
            let generation_is_current = generation
                .map(|generation| state.active_windows.get(&hwnd) == Some(&generation))
                .unwrap_or(true);
            if generation_is_current {
                state.active_windows.remove(&hwnd);
            }
            let tokens = state
                .entries
                .iter()
                .filter_map(|(token, entry)| {
                    (entry.hwnd == hwnd
                        && generation
                            .map(|generation| entry.generation == generation)
                            .unwrap_or(true))
                    .then_some(*token)
                })
                .collect::<Vec<_>>();
            tokens
                .into_iter()
                .filter_map(|token| state.entries.remove(&token))
                .collect::<Vec<_>>()
        };
        let count = discarded.len();
        drop(discarded);
        count
    }

    fn remove(&self, token: isize) -> Option<Entry> {
        self.lock().entries.remove(&token)
    }
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::default)
}

pub(super) fn post<T: Any + Send + 'static>(
    hwnd: HWND,
    generation: u64,
    message: u32,
    payload: T,
) -> Result<()> {
    let token = registry()
        .insert(hwnd, generation, message, payload)
        .ok_or_else(|| {
            Error::new(
                HRESULT(0x8007_0006u32 as i32),
                "The target window is no longer accepting messages.",
            )
        })?;
    let result = unsafe { PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(token)) };
    if result.is_err() {
        let discarded = registry().remove(token);
        drop(discarded);
    }
    result
}

pub(super) fn take<T: Any + Send + 'static>(
    hwnd: HWND,
    generation: u64,
    message: u32,
    token: isize,
) -> Option<T> {
    registry().take(hwnd, generation, message, token)
}

pub(super) fn register_window(hwnd: HWND, generation: u64) {
    registry().register_window(hwnd, generation);
}

pub(super) fn close_window(hwnd: HWND, generation: Option<u64>) -> usize {
    registry().close_window(hwnd, generation)
}

#[cfg(test)]
mod tests {
    use super::Registry;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use windows::Win32::Foundation::HWND;

    struct ReentrantDrop {
        registry: Arc<Registry>,
        lock_was_available: Arc<AtomicBool>,
    }

    impl Drop for ReentrantDrop {
        fn drop(&mut self) {
            let lock_was_available = self.registry.state.try_lock().is_ok();
            self.lock_was_available
                .store(lock_was_available, Ordering::SeqCst);
        }
    }

    #[test]
    fn payload_is_taken_once_for_its_window_and_message_and_mismatch_is_discarded() {
        let registry = Registry::default();
        let hwnd = HWND(0x1234usize as *mut _);
        registry.register_window(hwnd, 7);
        let message = 0x8001;
        let generation = 7;
        let token = registry
            .insert(hwnd, generation, message, String::from("owned payload"))
            .unwrap();
        let stale_token = registry
            .insert(hwnd, generation, message, String::from("stale payload"))
            .unwrap();

        assert!(registry
            .take::<String>(hwnd, generation, message + 1, token)
            .is_none());
        assert!(registry
            .take::<String>(HWND(0x5678usize as *mut _), generation, message, token)
            .is_none());
        assert!(registry
            .take::<String>(hwnd, generation + 1, message, stale_token)
            .is_none());
        assert!(registry
            .take::<String>(hwnd, generation, message, stale_token)
            .is_none());
        assert!(registry
            .take::<u64>(hwnd, generation, message, token)
            .is_none());
        assert!(!registry.lock().entries.contains_key(&token));
        assert!(registry
            .take::<String>(hwnd, generation, message, 0)
            .is_none());
        assert!(registry
            .take::<String>(hwnd, generation, message, -1)
            .is_none());
        assert!(registry
            .take::<String>(hwnd, generation, message, token)
            .is_none());
    }

    #[test]
    fn mismatched_payload_destructor_runs_after_releasing_registry_lock() {
        let registry = Arc::new(Registry::default());
        let lock_was_available = Arc::new(AtomicBool::new(false));
        let hwnd = HWND(0x1234usize as *mut _);
        registry.register_window(hwnd, 7);
        let token = registry
            .insert(
                hwnd,
                7,
                0x8001,
                ReentrantDrop {
                    registry: Arc::clone(&registry),
                    lock_was_available: Arc::clone(&lock_was_available),
                },
            )
            .unwrap();

        assert!(registry.take::<()>(hwnd, 7, 0x8001, token).is_none());
        assert!(lock_was_available.load(Ordering::SeqCst));
        assert!(!registry.lock().entries.contains_key(&token));
    }

    #[test]
    fn destroying_a_window_discards_only_its_pending_payloads() {
        let registry = Registry::default();
        let first = HWND(0x1234usize as *mut _);
        let second = HWND(0x5678usize as *mut _);
        registry.register_window(first, 1);
        registry.register_window(second, 2);
        let first_token = registry
            .insert(first, 1, 0x8001, String::from("first"))
            .unwrap();
        let second_token = registry
            .insert(second, 2, 0x8001, String::from("second"))
            .unwrap();

        assert_eq!(registry.close_window(first, Some(1)), 1);
        assert!(registry
            .take::<String>(first, 1, 0x8001, first_token)
            .is_none());
        assert_eq!(
            registry
                .take::<String>(second, 2, 0x8001, second_token)
                .as_deref(),
            Some("second")
        );
        assert_eq!(registry.close_window(second, Some(2)), 0);
    }

    #[test]
    fn discarded_payload_destructors_run_after_releasing_the_registry_lock() {
        let registry = Arc::new(Registry::default());
        let lock_was_available = Arc::new(AtomicBool::new(false));
        let hwnd = HWND(0x1234usize as *mut _);
        registry.register_window(hwnd, 1);
        registry
            .insert(
                hwnd,
                1,
                0x8001,
                ReentrantDrop {
                    registry: Arc::clone(&registry),
                    lock_was_available: Arc::clone(&lock_was_available),
                },
            )
            .unwrap();

        assert_eq!(registry.close_window(hwnd, Some(1)), 1);
        assert!(lock_was_available.load(Ordering::SeqCst));
    }

    #[test]
    fn stale_payload_destructors_run_after_releasing_the_registry_lock() {
        let registry = Arc::new(Registry::default());
        let lock_was_available = Arc::new(AtomicBool::new(false));
        let hwnd = HWND(0x1234usize as *mut _);
        registry.register_window(hwnd, 1);
        let token = registry
            .insert(
                hwnd,
                1,
                0x8001,
                ReentrantDrop {
                    registry: Arc::clone(&registry),
                    lock_was_available: Arc::clone(&lock_was_available),
                },
            )
            .unwrap();

        assert!(registry
            .take::<ReentrantDrop>(hwnd, 2, 0x8001, token)
            .is_none());
        assert!(lock_was_available.load(Ordering::SeqCst));
    }

    #[test]
    fn removed_payload_destructors_run_after_releasing_the_registry_lock() {
        let registry = Arc::new(Registry::default());
        let lock_was_available = Arc::new(AtomicBool::new(false));
        let hwnd = HWND(0x1234usize as *mut _);
        registry.register_window(hwnd, 1);
        let token = registry
            .insert(
                hwnd,
                1,
                0x8001,
                ReentrantDrop {
                    registry: Arc::clone(&registry),
                    lock_was_available: Arc::clone(&lock_was_available),
                },
            )
            .unwrap();

        drop(registry.remove(token));
        assert!(lock_was_available.load(Ordering::SeqCst));
    }

    #[test]
    fn payload_posts_are_rejected_after_the_window_closes() {
        let registry = Registry::default();
        let hwnd = HWND(0x1234usize as *mut _);
        registry.register_window(hwnd, 9);
        let token = registry
            .insert(hwnd, 9, 0x8001, String::from("queued"))
            .unwrap();

        assert_eq!(registry.close_window(hwnd, Some(9)), 1);
        assert!(registry
            .insert(hwnd, 9, 0x8001, String::from("late"))
            .is_none());
        assert!(registry.take::<String>(hwnd, 9, 0x8001, token).is_none());
    }

    #[test]
    fn an_old_window_generation_cannot_post_to_or_close_a_reused_handle() {
        let registry = Registry::default();
        let hwnd = HWND(0x1234usize as *mut _);
        registry.register_window(hwnd, 10);
        registry.close_window(hwnd, Some(10));
        registry.register_window(hwnd, 11);

        assert!(registry
            .insert(hwnd, 10, 0x8001, String::from("stale"))
            .is_none());
        let current_token = registry
            .insert(hwnd, 11, 0x8001, String::from("current"))
            .unwrap();
        assert_eq!(registry.close_window(hwnd, Some(10)), 0);
        assert_eq!(
            registry
                .take::<String>(hwnd, 11, 0x8001, current_token)
                .as_deref(),
            Some("current")
        );
    }
}
