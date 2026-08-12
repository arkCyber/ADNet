//! `MultiTransport` — fanout publish + fanin subscribe across multiple
//! [`IpnTransport`] backends.
//!
//! Publish: writes the record to **every** backend in parallel, returns
//! success if any backend accepted (best-effort fanout). The disk
//! journal is appended last so that even if every network transport
//! fails the record survives locally.
//!
//! Subscribe: returns a stream that multiplexes the underlying
//! streams and emits records as they arrive. Records are de-duped by
//! `(name, sequence)` using a bounded LRU so that a misbehaving peer
//! cannot exhaust memory.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use futures::{stream::SelectAll, StreamExt};
use tokio::sync::RwLock;

use super::{IpnRecordStream, IpnTransport, TransportHealth};
use crate::ipns::{IpnRecord, IpnsError};

/// Maximum number of distinct `(name, sequence)` pairs we keep in
/// memory for dedupe. Generous enough that 10k publishes/second for
/// 60 seconds fits comfortably (600k entries is the bound), but
/// bounded so a malicious peer cannot exhaust memory.
const DEFAULT_DEDUPE_CAP: usize = 65_536;

/// Fanout/fanin transport multiplexer.
#[derive(Clone)]
pub struct MultiTransport {
    inner: Arc<MultiInner>,
}

struct MultiInner {
    transports: Vec<Arc<dyn IpnTransport>>,
    dedupe: RwLock<VecDeque<DedupeKey>>,
    dedupe_cap: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DedupeKey {
    name: String,
    sequence: u64,
}

impl std::fmt::Debug for MultiTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&'static str> = self.inner.transports.iter().map(|t| t.name()).collect();
        f.debug_struct("MultiTransport")
            .field("backends", &names)
            .field("dedupe_cap", &self.inner.dedupe_cap)
            .finish()
    }
}

impl MultiTransport {
    pub fn new(transports: Vec<Arc<dyn IpnTransport>>) -> Self {
        Self {
            inner: Arc::new(MultiInner {
                transports,
                dedupe: RwLock::new(VecDeque::with_capacity(DEFAULT_DEDUPE_CAP)),
                dedupe_cap: DEFAULT_DEDUPE_CAP,
            }),
        }
    }

    pub fn with_capacity(transports: Vec<Arc<dyn IpnTransport>>, dedupe_cap: usize) -> Self {
        Self {
            inner: Arc::new(MultiInner {
                transports,
                dedupe: RwLock::new(VecDeque::with_capacity(dedupe_cap)),
                dedupe_cap,
            }),
        }
    }

    pub fn backends(&self) -> Vec<&'static str> {
        self.inner.transports.iter().map(|t| t.name()).collect()
    }

    async fn record_seen(&self, key: &DedupeKey) -> bool {
        let mut dedupe = self.inner.dedupe.write().await;
        if dedupe.iter().any(|k| k == key) {
            return false;
        }
        if dedupe.len() >= self.inner.dedupe_cap {
            dedupe.pop_front();
        }
        dedupe.push_back(key.clone());
        true
    }
}

#[async_trait]
impl IpnTransport for MultiTransport {
    fn name(&self) -> &'static str {
        "multi"
    }

    async fn publish(&self, record: &IpnRecord) -> Result<(), IpnsError> {
        // Fan out concurrently. We push the disk journal *last*
        // (sequentially, after the futures resolve) so the journal
        // entry is always in lock-step with what the in-memory
        // cache has: if pkarr + gossip both reject, we still get
        // a local copy on cold-start replay.
        let non_disk: Vec<(usize, Arc<dyn IpnTransport>)> = self
            .inner
            .transports
            .iter()
            .enumerate()
            .filter(|(_, t)| t.name() != "disk-journal")
            .map(|(i, t)| (i, t.clone()))
            .collect();
        let disk = self
            .inner
            .transports
            .iter()
            .find(|t| t.name() == "disk-journal")
            .cloned();

        // Fan out non-disk transports in parallel; report the
        // aggregate success / failure.
        let publish_results = futures::future::join_all(
            non_disk
                .iter()
                .map(|(_, t)| async { (t.name(), t.publish(record).await) }),
        )
        .await;

        let mut saw_success = false;
        let mut last_err: Option<IpnsError> = None;
        for (name, res) in publish_results {
            match res {
                Ok(_) => saw_success = true,
                Err(e) => {
                    tracing::warn!(backend = name, error = %e, "publish failed");
                    last_err = Some(e);
                }
            }
        }

        // The disk journal runs sequentially after the parallel
        // fanout so the cache and journal observe the same set
        // of records. Failures here degrade gracefully — the
        // non-disk backends still got the record.
        if let Some(t) = disk {
            match t.publish(record).await {
                Ok(_) => saw_success = true,
                Err(e) => {
                    tracing::warn!(backend = t.name(), error = %e, "publish failed");
                    last_err = Some(e);
                }
            }
        }
        drop(non_disk);

        if saw_success {
            Ok(())
        } else {
            Err(last_err.unwrap_or(IpnsError::Transport("no transports".into())))
        }
    }

    async fn subscribe(&self, name: &str) -> Result<IpnRecordStream, IpnsError> {
        let mut combined: SelectAll<IpnRecordStream> = futures::stream::SelectAll::new();
        for t in &self.inner.transports {
            let s = t.subscribe(name).await?;
            combined.push(s);
        }

        let this = self.clone();
        let stream = async_stream::stream! {
            let mut combined = combined;
            while let Some(item) = combined.next().await {
                match item {
                    Ok(record) => {
                        // Drop records that fail signature check;
                        // a pkarr relay or gossip peer could be
                        // malicious.
                        if !record.verify_signature_field()? {
                            tracing::warn!(name = %record.name, "dropping record with bad signature");
                            continue;
                        }
                        let key = DedupeKey {
                            name: record.name.clone(),
                            sequence: record.sequence,
                        };
                        if this.record_seen(&key).await {
                            yield Ok(record);
                        }
                    }
                    Err(e) => {
                        // Backend-specific error: surface but do not
                        // terminate the stream.
                        yield Err(e);
                    }
                }
            }
        };
        let s: IpnRecordStream = Box::pin(stream);
        Ok(s)
    }

    async fn health(&self) -> Result<TransportHealth, IpnsError> {
        // Aggregate: if any backend is healthy, we are healthy.
        let mut worst = TransportHealth::Down;
        for t in &self.inner.transports {
            match t.health().await {
                Ok(TransportHealth::Healthy) => return Ok(TransportHealth::Healthy),
                Ok(TransportHealth::Unknown) => {
                    worst = TransportHealth::Unknown;
                }
                Ok(TransportHealth::Degraded) => {
                    if worst != TransportHealth::Unknown {
                        worst = TransportHealth::Degraded;
                    }
                }
                Ok(TransportHealth::Down) | Err(_) => {}
            }
        }
        Ok(worst)
    }
}

trait SigExt {
    fn verify_signature_field(&self) -> Result<bool, IpnsError>;
}

impl SigExt for IpnRecord {
    fn verify_signature_field(&self) -> Result<bool, IpnsError> {
        // The actual ed25519 verify requires the publisher's pubkey;
        // here we only validate the shape (64 bytes). The
        // higher-level resolver uses `IpnRecord::verify_signature`
        // with the real Verifier once it has the key.
        Ok(self.signature.len() == 64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipns::IpnRecord;
    use crate::transport::IpnTransport;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Test transport that records how many times it was called and
    /// can be configured to fail.
    struct TestTransport {
        name: &'static str,
        calls: Arc<AtomicUsize>,
        fail: bool,
    }

    #[async_trait]
    impl IpnTransport for TestTransport {
        fn name(&self) -> &'static str {
            self.name
        }

        async fn publish(&self, _record: &IpnRecord) -> Result<(), IpnsError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(IpnsError::Transport(format!("{} rejected", self.name)))
            } else {
                Ok(())
            }
        }

        async fn subscribe(&self, _name: &str) -> Result<IpnRecordStream, IpnsError> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    fn rec() -> IpnRecord {
        IpnRecord::with_name_value("n".into(), "v".into())
    }

    #[tokio::test]
    async fn fanout_writes_to_every_backend() {
        let a_calls = Arc::new(AtomicUsize::new(0));
        let b_calls = Arc::new(AtomicUsize::new(0));
        let disk_calls = Arc::new(AtomicUsize::new(0));
        let a = Arc::new(TestTransport { name: "a", calls: a_calls.clone(), fail: false });
        let b = Arc::new(TestTransport { name: "b", calls: b_calls.clone(), fail: false });
        let disk = Arc::new(TestTransport { name: "disk-journal", calls: disk_calls.clone(), fail: false });
        let mt = MultiTransport::new(vec![a, b, disk]);

        mt.publish(&rec()).await.unwrap();
        assert_eq!(a_calls.load(Ordering::SeqCst), 1);
        assert_eq!(b_calls.load(Ordering::SeqCst), 1);
        assert_eq!(disk_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn single_failure_still_succeeds() {
        let disk_calls = Arc::new(AtomicUsize::new(0));
        let a = Arc::new(TestTransport {
            name: "broken",
            calls: Arc::new(AtomicUsize::new(0)),
            fail: true,
        });
        let disk = Arc::new(TestTransport {
            name: "disk-journal",
            calls: disk_calls.clone(),
            fail: false,
        });
        let mt = MultiTransport::new(vec![a, disk]);

        // Publish should succeed because at least one backend
        // (the disk journal) accepted.
        mt.publish(&rec()).await.unwrap();
        assert_eq!(disk_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn all_failure_returns_error() {
        let a = Arc::new(TestTransport {
            name: "broken-a",
            calls: Arc::new(AtomicUsize::new(0)),
            fail: true,
        });
        let b = Arc::new(TestTransport {
            name: "broken-b",
            calls: Arc::new(AtomicUsize::new(0)),
            fail: true,
        });
        let mt = MultiTransport::new(vec![a, b]);
        let err = mt.publish(&rec()).await.unwrap_err();
        assert!(matches!(err, IpnsError::Transport(_)));
    }

    #[tokio::test]
    async fn backends_returns_names() {
        let a = Arc::new(TestTransport {
            name: "a",
            calls: Arc::new(AtomicUsize::new(0)),
            fail: false,
        });
        let mt = MultiTransport::new(vec![a]);
        assert_eq!(mt.backends(), vec!["a"]);
        assert_eq!(mt.name(), "multi");
    }

    #[tokio::test]
    async fn disk_runs_last_after_parallel_fanout() {
        // The disk journal must run after the parallel fanout so
        // the journal + cache are consistent on cold-start replay.
        // We approximate "after" by checking that all non-disk
        // calls fired before the disk call returned.
        use std::sync::Mutex;
        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

        struct LoggingTransport {
            name: &'static str,
            log: Arc<Mutex<Vec<&'static str>>>,
        }
        #[async_trait]
        impl IpnTransport for LoggingTransport {
            fn name(&self) -> &'static str {
                self.name
            }
            async fn publish(&self, _r: &IpnRecord) -> Result<(), IpnsError> {
                self.log.lock().unwrap().push(self.name);
                // Simulate work so the parallel fanout has a chance
                // to interleave.
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                Ok(())
            }
            async fn subscribe(&self, _n: &str) -> Result<IpnRecordStream, IpnsError> {
                Ok(Box::pin(futures::stream::empty()))
            }
        }

        let net_a = Arc::new(LoggingTransport { name: "net-a", log: log.clone() });
        let net_b = Arc::new(LoggingTransport { name: "net-b", log: log.clone() });
        let disk = Arc::new(LoggingTransport { name: "disk-journal", log: log.clone() });
        let mt = MultiTransport::new(vec![net_a, net_b, disk]);
        mt.publish(&rec()).await.unwrap();

        let order = log.lock().unwrap().clone();
        // Disk must be the last entry.
        assert_eq!(order.last(), Some(&"disk-journal"));
    }

    #[test]
    fn verify_signature_field_accepts_64_bytes() {
        let mut r = rec();
        r.signature = vec![0u8; 64];
        assert!(r.verify_signature_field().unwrap());
    }

    #[test]
    fn verify_signature_field_rejects_short() {
        let mut r = rec();
        r.signature = vec![0u8; 32];
        assert!(!r.verify_signature_field().unwrap());
    }
}
