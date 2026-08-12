//! Background decay task.
//!
//! Reputation scores pull toward zero at the configured rate. The
//! [`DecayLoop`] is a thin wrapper around a `tokio::time::interval`
//! that ticks the [`crate::score::PeerScoreTable`] on every
//! interval. It is **opt-in** — many callers prefer to drive decay
//! from their own scheduler (e.g. a quic-heartbeat task) — but
//! [`crate::store::ReputationStore::run_decay`] and
//! [`crate::reporter::ReputationReporter`] use this implementation
//! directly.

use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::interval;

use crate::score::PeerScoreTable;

/// Handle to a running [`DecayLoop`]. Drop the handle (or call
/// [`DecayLoop::shutdown`]) to stop the task.
#[derive(Debug)]
pub struct DecayLoop {
    handle: JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
}

impl DecayLoop {
    /// Spawn a new decay loop. The loop ticks every
    /// `interval_secs` seconds (default
    /// `ReputationParams::decay_interval_secs`).
    pub fn spawn(table: PeerScoreTable, interval_secs: u64) -> Self {
        assert!(interval_secs > 0, "decay interval must be > 0");
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(interval_secs));
            // Skip the immediate first tick; tokio::time::interval
            // fires immediately on first .tick() which would mean
            // we decay the same instant we just (re)loaded the
            // table.
            tick.tick().await;
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            return;
                        }
                    }
                    _ = tick.tick() => {
                        table.decay_tick();
                    }
                }
            }
        });
        Self { handle, shutdown_tx }
    }

    /// Spawn with the default [`crate::params::DEFAULT_DECAY_INTERVAL_SECS`].
    pub fn spawn_default(table: PeerScoreTable) -> Self {
        let secs = table.params().decay_interval_secs;
        Self::spawn(table, secs)
    }

    /// Signal the loop to stop. The associated task exits after at
    /// most one more tick.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Await the loop's termination. Used in tests; production
    /// callers typically just drop the handle.
    pub async fn join(self) {
        let _ = self.handle.await;
    }
}

/// Tick the decay loop manually (synchronous). Useful in tests or
/// for embedding into a non-`tokio` scheduler. Returns the number
/// of peers decayed.
pub fn tick_once(table: &PeerScoreTable) -> usize {
    table.decay_tick()
}

/// Trait-object-style adapter for schedulers that don't use
/// `tokio`. The default implementation simply ticks at the
/// configured interval; embedders can replace it with their own.
pub trait DecayDriver: Send + Sync + 'static {
    /// Called once per tick.
    fn tick(&self) -> usize;
}

/// Default [`DecayDriver`] — wraps a [`PeerScoreTable`] directly.
#[derive(Debug, Clone)]
pub struct TableDecayDriver {
    table: PeerScoreTable,
}

impl TableDecayDriver {
    /// Construct from an existing table.
    pub fn new(table: PeerScoreTable) -> Self {
        Self { table }
    }
}

impl DecayDriver for TableDecayDriver {
    fn tick(&self) -> usize {
        tick_once(&self.table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ReputationEvent;
    use crate::params::ReputationParams;
    use adnet_types::NodeId;

    #[tokio::test(flavor = "current_thread")]
    async fn loop_decays_after_a_few_ticks() {
        let mut p = ReputationParams::default();
        p.decay_interval_secs = 1;
        p.decay_rate = 0.1;
        let t = PeerScoreTable::new(p);
        let peer = NodeId::random();
        t.apply(ReputationEvent::ValidMessage {
            peer: peer.clone(),
            topic: None,
            size_bytes: 1024,
        })
        .unwrap();
        let before = t.score(&peer).unwrap();
        let loop_ = DecayLoop::spawn(t.clone(), 1);
        // Wait long enough for several ticks to fire (real time).
        tokio::time::sleep(Duration::from_millis(1500)).await;
        loop_.shutdown();
        loop_.join().await;
        let after = t.score(&peer).unwrap_or(0.0);
        assert!(after < before, "score should have decayed (was {before}, now {after})");
    }

    #[test]
    fn manual_tick_returns_count() {
        let t = PeerScoreTable::new(ReputationParams::default());
        let peer = NodeId::random();
        t.apply(ReputationEvent::ValidMessage {
            peer,
            topic: None,
            size_bytes: 1024,
        })
        .unwrap();
        assert_eq!(tick_once(&t), 1);
    }

    #[test]
    fn driver_calls_tick() {
        let t = PeerScoreTable::new(ReputationParams::default());
        let driver = TableDecayDriver::new(t.clone());
        let peer = NodeId::random();
        t.apply(ReputationEvent::ValidMessage {
            peer,
            topic: None,
            size_bytes: 1024,
        })
        .unwrap();
        let n = driver.tick();
        assert_eq!(n, 1);
    }
}
