//! Session state shared by adapter operations: the operation gate and the
//! cancellation token.
//!
//! The gate has two kinds of claim. A write (anything that moves, deletes,
//! installs, saves a rule or builds a gated preview) holds it alone. A scan
//! holds one [`ScanSlot`]; scans in different slots hold the gate together.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tiny_core::processes::{MachineSampler, Sampler};

use crate::cask::CaskState;
use crate::clean::CleanState;
use crate::large_files::LargeFilesState;
use crate::uninstall::UninstallState;
use crate::FfiError;

/// What a read-only scan covers. One scan per slot at a time; each replaces
/// only its own feature's stored result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanSlot {
    /// `clean_discover`.
    Clean,
    /// `storage_scan`.
    Storage,
    /// `large_files_scan`.
    LargeFiles,
    /// `uninstall_list_apps`.
    Apps,
    /// `cask_catalogue` and `cask_outdated`: both run `brew`.
    Casks,
}

impl ScanSlot {
    fn bit(self) -> u8 {
        match self {
            Self::Clean => 1,
            Self::Storage => 1 << 1,
            Self::LargeFiles => 1 << 2,
            Self::Apps => 1 << 3,
            Self::Casks => 1 << 4,
        }
    }
}

/// The gate's bit for a write. Never set together with a scan bit.
const WRITE: u8 = 1 << 7;

/// One per app session. A write overlaps nothing; scans in different slots
/// overlap each other. An overlap that is not allowed is rejected, never
/// queued or merged. Process queries and `process_terminate` bypass the
/// gate, so a scan never holds up Quit.
#[derive(Default, uniffi::Object)]
pub struct TinySession {
    /// `WRITE`, or one bit per held [`ScanSlot`], or zero.
    gate: AtomicU8,
    sampler: Mutex<Sampler>,
    machine: Mutex<MachineSampler>,
    pub(crate) clean: Mutex<CleanState>,
    pub(crate) uninstall: Mutex<UninstallState>,
    pub(crate) large_files: Mutex<LargeFilesState>,
    pub(crate) cask: Mutex<CaskState>,
}

#[uniffi::export]
impl TinySession {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Whether a write or any scan currently holds the gate.
    pub fn is_busy(&self) -> bool {
        self.gate.load(Ordering::Acquire) != 0
    }
}

impl TinySession {
    /// Claims the gate for a write: `Busy` while another write or any scan
    /// holds it. The returned guard releases it on drop, including while
    /// unwinding from a panic.
    pub fn begin(&self) -> Result<OperationGuard<'_>, FfiError> {
        self.gate
            .compare_exchange(0, WRITE, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| FfiError::Busy)?;
        Ok(OperationGuard {
            gate: &self.gate,
            held: WRITE,
        })
    }

    /// Claims `slot` for a read-only scan: `Busy` while a write holds the
    /// gate or the same slot is held. Scans in other slots do not refuse it.
    /// The guard releases only this slot, on drop.
    pub(crate) fn begin_scan(&self, slot: ScanSlot) -> Result<OperationGuard<'_>, FfiError> {
        let bit = slot.bit();
        let mut held = self.gate.load(Ordering::Acquire);
        loop {
            if held & (WRITE | bit) != 0 {
                return Err(FfiError::Busy);
            }
            // Another scan may claim or release its own slot in between.
            match self.gate.compare_exchange_weak(
                held,
                held | bit,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(now) => held = now,
            }
        }
        Ok(OperationGuard {
            gate: &self.gate,
            held: bit,
        })
    }

    /// The sampler holds no invariant a panic could break, so a poisoned lock
    /// is recovered rather than failing every later query.
    pub(crate) fn sampler(&self) -> MutexGuard<'_, Sampler> {
        self.sampler.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The whole-machine sampler, kept apart so its reads never touch process CPU intervals.
    pub(crate) fn machine(&self) -> MutexGuard<'_, MachineSampler> {
        self.machine.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A held claim on the gate: the write bit or one scan slot's bit.
pub struct OperationGuard<'a> {
    gate: &'a AtomicU8,
    held: u8,
}

impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        self.gate.fetch_and(!self.held, Ordering::AcqRel);
    }
}

/// Cooperative cancellation shared between Swift and a running operation.
#[derive(Debug, Default, uniffi::Object)]
pub struct CancellationToken {
    cancelled: AtomicBool,
}

#[uniffi::export]
impl CancellationToken {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl CancellationToken {
    /// The flag core operations poll between and inside work units.
    pub(crate) fn flag(&self) -> &AtomicBool {
        &self.cancelled
    }

    /// Checkpoint between work units and before every mutation.
    pub fn check(&self) -> Result<(), FfiError> {
        if self.is_cancelled() {
            Err(FfiError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Test hook: panics while holding the gate so Swift tests can prove panics
/// surface as failures and release the gate. Never built into the shipped app.
#[cfg(feature = "test-hooks")]
#[uniffi::export]
pub fn debug_panic(session: Arc<TinySession>) -> Result<(), FfiError> {
    let _gate = session.begin()?;
    panic!("debug_panic test hook");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    const SLOTS: [ScanSlot; 5] = [
        ScanSlot::Clean,
        ScanSlot::Storage,
        ScanSlot::LargeFiles,
        ScanSlot::Apps,
        ScanSlot::Casks,
    ];

    #[test]
    fn overlapping_operation_is_rejected() {
        let session = TinySession::new();
        let first = session.begin().expect("first operation starts");
        assert!(session.is_busy());
        assert!(matches!(session.begin(), Err(FfiError::Busy)));
        drop(first);
        assert!(!session.is_busy());
        assert!(session.begin().is_ok());
    }

    #[test]
    fn scans_in_different_slots_hold_the_gate_together() {
        let session = TinySession::new();
        let held: Vec<_> = SLOTS
            .iter()
            .map(|slot| session.begin_scan(*slot).expect("each slot starts"))
            .collect();
        assert!(session.is_busy());
        drop(held);
        assert!(!session.is_busy());
    }

    #[test]
    fn a_slot_refuses_a_second_scan_of_its_own() {
        let session = TinySession::new();
        for slot in SLOTS {
            let _held = session.begin_scan(slot).expect("the slot starts");
            assert!(matches!(session.begin_scan(slot), Err(FfiError::Busy)));
        }
    }

    #[test]
    fn a_write_is_refused_while_any_scan_runs() {
        let session = TinySession::new();
        for slot in SLOTS {
            let scan = session.begin_scan(slot).expect("the slot starts");
            assert!(matches!(session.begin(), Err(FfiError::Busy)));
            drop(scan);
            assert!(session.begin().is_ok());
        }
    }

    #[test]
    fn every_scan_is_refused_while_a_write_runs() {
        let session = TinySession::new();
        let write = session.begin().expect("the write starts");
        for slot in SLOTS {
            assert!(matches!(session.begin_scan(slot), Err(FfiError::Busy)));
        }
        drop(write);
        assert!(!session.is_busy(), "a refused scan leaves nothing held");
        for slot in SLOTS {
            assert!(session.begin_scan(slot).is_ok());
        }
    }

    #[test]
    fn ending_one_scan_leaves_the_others_held() {
        let session = TinySession::new();
        let clean = session.begin_scan(ScanSlot::Clean).expect("clean starts");
        let storage = session
            .begin_scan(ScanSlot::Storage)
            .expect("storage starts");
        drop(clean);
        assert!(session.is_busy());
        assert!(matches!(session.begin(), Err(FfiError::Busy)));
        assert!(matches!(
            session.begin_scan(ScanSlot::Storage),
            Err(FfiError::Busy)
        ));
        assert!(session.begin_scan(ScanSlot::Clean).is_ok());
        drop(storage);
        assert!(!session.is_busy());
        assert!(session.begin().is_ok());
    }

    #[test]
    fn a_scan_slot_is_released_after_panic() {
        let session = TinySession::new();
        let other = session
            .begin_scan(ScanSlot::Storage)
            .expect("storage starts");
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let _slot = session.begin_scan(ScanSlot::Clean).expect("clean starts");
            panic!("simulated failure mid-scan");
        }));
        assert!(outcome.is_err());
        assert!(session.begin_scan(ScanSlot::Clean).is_ok());
        drop(other);
        assert!(!session.is_busy());
    }

    /// Two threads racing for one slot, and a write racing scans: exactly
    /// the allowed claims succeed, whatever the interleaving.
    #[test]
    fn racing_claims_never_overlap_a_write() {
        use std::sync::atomic::AtomicUsize;
        let session = TinySession::new();
        let writes = AtomicUsize::new(0);
        let scans = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for index in 0..8 {
                let (session, writes, scans) = (&session, &writes, &scans);
                scope.spawn(move || {
                    for _ in 0..2_000 {
                        if index % 2 == 0 {
                            if let Ok(_gate) = session.begin() {
                                writes.fetch_add(1, Ordering::SeqCst);
                                assert_eq!(scans.load(Ordering::SeqCst), 0);
                                assert_eq!(writes.load(Ordering::SeqCst), 1);
                                writes.fetch_sub(1, Ordering::SeqCst);
                            }
                        } else if let Ok(_gate) = session.begin_scan(SLOTS[index % SLOTS.len()]) {
                            scans.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(writes.load(Ordering::SeqCst), 0);
                            scans.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                });
            }
        });
        assert!(!session.is_busy());
    }

    #[test]
    fn gate_is_released_after_panic() {
        let session = TinySession::new();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let _gate = session.begin().expect("operation starts");
            panic!("simulated failure mid-operation");
        }));
        assert!(outcome.is_err());
        assert!(!session.is_busy());
        assert!(session.begin().is_ok());
    }

    #[test]
    fn cancellation_is_observed_at_checkpoints() {
        let token = CancellationToken::new();
        assert!(token.check().is_ok());
        token.cancel();
        assert!(token.is_cancelled());
        assert!(matches!(token.check(), Err(FfiError::Cancelled)));
    }
}
