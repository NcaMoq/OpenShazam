use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
};

/// Owns per-attempt workers and guarantees they are joined before the owner
/// leaves scope. The cancellation flag is internal cleanup state, separate
/// from any user-visible session control request.
pub(crate) struct WorkerSet {
    workers: BTreeMap<u64, JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
    cleanup_failed: Arc<AtomicBool>,
}

impl WorkerSet {
    pub(crate) fn new(cancel: Arc<AtomicBool>, cleanup_failed: Arc<AtomicBool>) -> Self {
        Self {
            workers: BTreeMap::new(),
            cancel,
            cleanup_failed,
        }
    }

    pub(crate) fn join_all(&mut self) -> Result<(), std::io::Error> {
        let mut first_error = None;
        for (_, worker) in std::mem::take(&mut self.workers) {
            if worker.join().is_err() {
                self.cleanup_failed.store(true, Ordering::Release);
                first_error.get_or_insert_with(|| "recognition HTTP worker panicked".to_owned());
            }
        }
        match first_error {
            Some(error) => Err(std::io::Error::other(error)),
            None => Ok(()),
        }
    }
}

impl Deref for WorkerSet {
    type Target = BTreeMap<u64, JoinHandle<()>>;

    fn deref(&self) -> &Self::Target {
        &self.workers
    }
}

impl DerefMut for WorkerSet {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.workers
    }
}

impl Drop for WorkerSet {
    fn drop(&mut self) {
        if self.workers.is_empty() {
            return;
        }
        self.cancel.store(true, Ordering::Release);
        let _ = self.join_all();
    }
}

#[cfg(test)]
mod tests {
    use super::WorkerSet;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[test]
    fn joins_all_workers_on_success() {
        let cleaned = Arc::new(AtomicBool::new(false));
        let mut workers = WorkerSet::new(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let worker_cleaned = Arc::clone(&cleaned);
        workers.insert(
            1,
            std::thread::spawn(move || worker_cleaned.store(true, Ordering::Release)),
        );

        workers.join_all().unwrap();
        assert!(cleaned.load(Ordering::Acquire));
        assert!(workers.is_empty());
    }

    #[test]
    fn reports_worker_panic_after_joining_remaining_workers() {
        let remaining_joined = Arc::new(AtomicBool::new(false));
        let cleanup_failed = Arc::new(AtomicBool::new(false));
        let mut workers = WorkerSet::new(
            Arc::new(AtomicBool::new(false)),
            Arc::clone(&cleanup_failed),
        );
        workers.insert(1, std::thread::spawn(|| panic!("synthetic worker panic")));
        let joined = Arc::clone(&remaining_joined);
        workers.insert(
            2,
            std::thread::spawn(move || joined.store(true, Ordering::Release)),
        );

        assert!(workers.join_all().is_err());
        assert!(remaining_joined.load(Ordering::Acquire));
        assert!(cleanup_failed.load(Ordering::Acquire));
        assert!(workers.is_empty());
    }

    #[test]
    fn drop_cancels_and_joins_workers() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let observed = Arc::new(AtomicBool::new(false));
        let mut workers = WorkerSet::new(Arc::clone(&cancelled), Arc::new(AtomicBool::new(false)));
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_observed = Arc::clone(&observed);
        workers.insert(
            1,
            std::thread::spawn(move || {
                while !worker_cancelled.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                worker_observed.store(true, Ordering::Release);
            }),
        );

        drop(workers);
        assert!(cancelled.load(Ordering::Acquire));
        assert!(observed.load(Ordering::Acquire));
    }
}
