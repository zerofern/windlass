//! Keyed one-shot timers with replace semantics, for shells.
//!
//! Every shell implements `ScheduleTimer` as "spawn a sleep, then
//! inject `TimerFired`".  Done naively, each re-arm — retry paths,
//! periodic chains, operator commands racing them — stacks another
//! self-perpetuating chain on top of the existing one; observed in
//! the wild as N duplicate timer events per tick, growing over time.
//! [`KeyedTimers`] enforces the invariant the machines actually
//! assume: at most one pending sleep per timer id.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use tokio::task::AbortHandle;

use crate::machine::{ExternalCause, Timed};

/// Per-shell registry of pending timer sleeps, keyed by the core's
/// timer enum.
///
/// Scheduling a key that is already pending aborts the previous
/// sleep (replace semantics).  Aborting a task that already fired is
/// a no-op, so the race with an in-flight `TimerFired` is harmless.
pub struct KeyedTimers<K> {
    pending: HashMap<K, AbortHandle>,
}

impl<K> Default for KeyedTimers<K> {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash> KeyedTimers<K> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sleeps `after`, then sends `event` causally tagged with the
    /// timer's `name`, replacing any pending sleep for `key`.
    pub fn schedule<E: Send + 'static>(
        &mut self,
        key: K,
        name: &'static str,
        after: Duration,
        tx: &UnboundedSender<Timed<E>>,
        event: E,
    ) {
        let tx = tx.clone();
        let handle = crate::causal::spawn(async move {
            let scheduled_at = std::time::Instant::now() + after;
            tokio::time::sleep(after).await;
            let _ = tx.send(Timed::external(
                scheduled_at,
                ExternalCause::Timer { name },
                event,
            ));
        });
        if let Some(prev) = self.pending.insert(key, handle.abort_handle()) {
            prev.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum Timer {
        Tick,
    }

    #[tokio::test]
    async fn distinct_keys_do_not_replace_each_other() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        enum T {
            A,
            B,
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<Timed<&'static str>>();
        let mut timers = KeyedTimers::new();
        timers.schedule(T::A, "a", Duration::from_millis(10), &tx, "a");
        timers.schedule(T::B, "b", Duration::from_millis(20), &tx, "b");
        let mut got = vec![
            rx.recv().await.expect("first fires").inner,
            rx.recv().await.expect("second fires").inner,
        ];
        got.sort_unstable();
        assert_eq!(got, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn rescheduling_replaces_the_pending_sleep() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Timed<&'static str>>();
        let mut timers = KeyedTimers::new();
        // First schedule would fire "old" — but the second schedule
        // for the same key must replace it before it can.
        timers.schedule(Timer::Tick, "tick", Duration::from_millis(50), &tx, "old");
        timers.schedule(Timer::Tick, "tick", Duration::from_millis(10), &tx, "new");
        let first = rx.recv().await.expect("replacement fires");
        assert_eq!(first.inner, "new");
        // The replaced sleep must never fire.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(rx.try_recv().is_err(), "aborted timer must not fire");
    }
}
