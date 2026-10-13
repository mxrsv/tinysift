//! Session state shared by adapter operations: the operation gate and the
//! cancellation token.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tiny_core::processes::Sampler;

use crate::clean::CleanState;
use crate::FfiError;

/// One per app session. Rejects overlapping scans and mutations instead of
/// queueing or merging them; short read-only process queries bypass the gate.
#[derive(Default, uniffi::Object)]
pub struct TinySession {
    busy: AtomicBool,
    sampler: Mutex<Sampler>,
    pub(crate) clean: Mutex<CleanState>,
}

#[uniffi::export]
impl TinySession {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Whether an operation currently holds the gate.
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }
}

impl TinySession {
    /// Claims the operation gate. The returned guard releases it on drop,
    /// including while unwinding from a panic.
    pub fn begin(&self) -> Result<OperationGuard<'_>, FfiError> {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| FfiError::Busy)?;
        Ok(OperationGuard { busy: &self.busy })
    }

    /// The sampler holds no invariant a panic could break, so a poisoned lock
    /// is recovered rather than failing every later query.
    pub(crate) fn sampler(&self) -> MutexGuard<'_, Sampler> {
        self.sampler.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct OperationGuard<'a> {
    busy: &'a AtomicBool,
}

impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        self.busy.store(false, Ordering::Release);
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
