//! Typed subscription slots (docs/design/operation-seam-adr.md, "Decision
//! 4"): the TUI's only handle onto live data. Data crosses in the slot, never
//! on the event queue.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// A subscription's identity: routes `RuntimeEvent::Updated` to the view
/// holding the matching [`Subscription`], and lets `Drop` retract its own
/// registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionKey(u64);

impl SubscriptionKey {
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// A live view's data: `take` consumes the latest result if a newer one has
/// arrived since the last call -- last-write-wins of the whole result, so a
/// late or duplicate wake is an idempotent re-read of current truth.
pub struct Subscription<T> {
    pub(crate) key: SubscriptionKey,
    pub(crate) latest: Arc<Mutex<Option<T>>>,
    /// Retracts this subscription's registry entry; boxed so the registry's
    /// entry type (private to `crate::runtime`) never needs naming here.
    pub(crate) retract: Box<dyn Fn(SubscriptionKey) + Send + Sync>,
}

impl<T> Subscription<T> {
    /// Consume the latest result, if a newer one has arrived.
    pub fn take(&self) -> Option<T> {
        self.latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    pub fn key(&self) -> SubscriptionKey {
        self.key
    }
}

impl<T> Drop for Subscription<T> {
    fn drop(&mut self) {
        (self.retract)(self.key);
    }
}
