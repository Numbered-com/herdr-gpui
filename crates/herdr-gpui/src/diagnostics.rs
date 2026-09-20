//! Process-local diagnostics only: no files, uploads, environment filters, or stderr.
//! The ring owns at most 5000 records of 4096 bytes. Snapshot holders should replace
//! old snapshots rather than retaining history. Span context includes names, not
//! fields; arbitrary field Debug implementations must cooperate with fmt errors.
use std::{
    cell::Cell,
    collections::VecDeque,
    fmt::{self, Write},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

const CAPACITY: usize = 5000;
const MAX_BYTES: usize = 4096;
const TRUNCATED: &str = " [truncated]";
static STORE: OnceLock<Arc<Store>> = OnceLock::new();
thread_local! { static FORMATTING: Cell<bool> = const { Cell::new(false) }; }

#[derive(Debug)]
pub(crate) struct Record {
    pub(crate) level: Level,
    pub(crate) line: String,
}

/// Install once, before starting workers. An existing global subscriber is an
/// error, never silently replaced. No tracing-log bridge is installed.
pub(crate) fn init() -> Result<(), tracing::subscriber::SetGlobalDefaultError> {
    tracing::subscriber::set_global_default(subscriber(store().clone()))
}

fn store() -> &'static Arc<Store> {
    STORE.get_or_init(|| Arc::new(Store::default()))
}

/// A change hint, including eviction/contention losses. On snapshot contention,
/// keep the previous UI snapshot AND generation and retry on a later tick.
pub(crate) fn generation() -> u64 {
    store().generation.load(Ordering::Acquire)
}

/// Oldest first. None means busy (or poisoned), not an empty log. The dropped
/// count includes evictions and rejected events, not failed snapshot attempts.
pub(crate) fn snapshot() -> Option<(u64, Vec<Arc<Record>>, u64)> {
    store().snapshot()
}

#[derive(Default)]
struct Store {
    records: Mutex<VecDeque<Arc<Record>>>,
    generation: AtomicU64,
    dropped: AtomicU64,
}

impl Store {
    fn lose(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
        self.generation.fetch_add(1, Ordering::Release);
    }

    fn push(&self, record: Record) {
        let Ok(mut records) = self.records.try_lock() else {
            self.lose();
            return;
        };
        if records.len() == CAPACITY {
            records.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        records.push_back(Arc::new(record));
        self.generation.fetch_add(1, Ordering::Release);
    }

    fn snapshot(&self) -> Option<(u64, Vec<Arc<Record>>, u64)> {
        let records = self.records.try_lock().ok()?;
        // Read the hint before the loss count so a concurrent loss cannot be
        // hidden by returning a newer hint alongside an older count.
        let generation = self.generation.load(Ordering::Acquire);
        let dropped = self.dropped.load(Ordering::Relaxed);
        Some((generation, records.iter().cloned().collect(), dropped))
    }
}

fn app_target(target: &str) -> bool {
    ["herdr_gpui", "herdr_client", "herdr_protocol"]
        .iter()
        .any(|root| {
            target == *root
                || target
                    .strip_prefix(root)
                    .is_some_and(|rest| rest.starts_with("::"))
        })
}

fn subscriber(store: Arc<Store>) -> impl Subscriber + Send + Sync {
    tracing_subscriber::registry().with(Capture(store).with_filter(
        tracing_subscriber::filter::filter_fn(|meta| app_target(meta.target())),
    ))
}

struct Capture(Arc<Store>);
struct FormattingGuard;
impl Drop for FormattingGuard {
    fn drop(&mut self) {
        FORMATTING.set(false);
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if FORMATTING.replace(true) {
            self.0.lose();
            return;
        }
        let _guard = FormattingGuard;
        let mut line = Bounded::default();
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let meta = event.metadata();
        let _ = write!(
            line,
            "{}.{:03} {} {}",
            time.as_secs(),
            time.subsec_millis(),
            meta.level(),
            meta.target()
        );
        if let Some(scope) = ctx.event_scope(event) {
            // Leaf first avoids allocating/reversing an arbitrarily deep scope.
            for span in scope.take(16) {
                if write!(line, " [{}]", span.name()).is_err() {
                    break;
                }
            }
        }
        event.record(&mut line);
        self.0.push(Record {
            level: *meta.level(),
            line: line.finish(),
        });
    }
}

#[derive(Default)]
struct Bounded {
    text: String,
    truncated: bool,
}

impl Bounded {
    fn finish(mut self) -> String {
        if self.truncated {
            self.text.push_str(TRUNCATED);
        }
        self.text
    }
}

impl Write for Bounded {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.truncated {
            return Err(fmt::Error);
        }
        // Sanitize while copying, never allocate a full formatted field. Stop
        // scanning once full, even for megabytes of untrusted UTF-8/control data.
        for ch in value.chars() {
            let ch = if ch.is_control() { ' ' } else { ch };
            if self.text.len() + ch.len_utf8() > MAX_BYTES - TRUNCATED.len() {
                self.truncated = true;
                return Err(fmt::Error);
            }
            self.text.push(ch);
        }
        Ok(())
    }
}

impl Visit for Bounded {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if !self.truncated {
            let _ = write!(self, " {}={value:?}", field.name());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn public_api_is_available_without_installing_a_global_subscriber() {
        let _init = init;
        let _ = generation();
        assert!(snapshot().is_some());
    }

    #[test]
    fn levels_targets_and_span_context() {
        let store = Arc::new(Store::default());
        tracing::subscriber::with_default(subscriber(store.clone()), || {
            let span =
                tracing::info_span!(target: "herdr_client", "connection", secret = "not retained");
            let _entered = span.enter();
            tracing::trace!(target: "herdr_client::worker", "trace");
            tracing::debug!(target: "herdr_gpui", "debug");
            tracing::info!(target: "herdr_protocol", "info");
            tracing::warn!(target: "herdr_client", "warn");
            tracing::error!(target: "herdr_gpui", "error");
            tracing::error!(target: "herdr_gpui_impostor", "excluded");
            tracing::error!(target: "gpui", "excluded");
        });
        let (generation, records, dropped) = store.snapshot().unwrap();
        assert_eq!((generation, records.len(), dropped), (5, 5, 0));
        assert_eq!(
            records.iter().map(|r| r.level).collect::<Vec<_>>(),
            [
                Level::TRACE,
                Level::DEBUG,
                Level::INFO,
                Level::WARN,
                Level::ERROR
            ]
        );
        for record in records {
            assert!(record.line.contains("[connection]"));
            assert!(!record.line.contains("not retained"));
            assert!(
                record
                    .line
                    .split(' ')
                    .next()
                    .unwrap()
                    .parse::<f64>()
                    .is_ok()
            );
        }
    }

    #[test]
    fn bounded_utf8_controls_and_debug_stream() {
        struct Huge;
        impl fmt::Debug for Huge {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                for _ in 0..1_000_000 {
                    f.write_str("\u{1f980}\n\0")?;
                }
                panic!("formatter should stop on budget exhaustion")
            }
        }
        let store = Arc::new(Store::default());
        tracing::subscriber::with_default(subscriber(store.clone()), || {
            tracing::info!(target: "herdr_gpui", value = ?Huge);
        });
        let (_, records, _) = store.snapshot().unwrap();
        assert!(records[0].line.len() <= MAX_BYTES);
        assert!(records[0].line.ends_with(TRUNCATED));
        assert!(!records[0].line.chars().any(char::is_control));
    }

    #[test]
    fn filtered_and_reentrant_events_never_format_fields() {
        struct MustNotFormat;
        impl fmt::Debug for MustNotFormat {
            fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
                panic!("filtered or reentrant event formatted a field")
            }
        }
        let store = Arc::new(Store::default());
        tracing::subscriber::with_default(subscriber(store.clone()), || {
            tracing::error!(target: "dependency", value = ?MustNotFormat);
            FORMATTING.set(true);
            let _guard = FormattingGuard;
            tracing::error!(target: "herdr_client", value = ?MustNotFormat);
        });
        let (generation, records, dropped) = store.snapshot().unwrap();
        assert_eq!((generation, records.len(), dropped), (1, 0, 1));
        assert!(!FORMATTING.get());
    }

    #[test]
    fn eviction_contention_and_snapshot_lifetime() {
        let store = Store::default();
        for i in 0..CAPACITY + 2 {
            store.push(Record {
                level: Level::INFO,
                line: i.to_string(),
            });
        }
        let (generation, records, dropped) = store.snapshot().unwrap();
        assert_eq!(
            (generation, records.len(), dropped),
            ((CAPACITY + 2) as u64, CAPACITY, 2)
        );
        assert_eq!(records[0].line, "2");
        let lock = store.records.lock().unwrap();
        assert!(store.snapshot().is_none());
        store.push(Record {
            level: Level::INFO,
            line: "lost".into(),
        });
        drop(lock);
        assert_eq!(store.snapshot().unwrap().2, 3);
        assert_eq!(records[0].line, "2");
    }

    #[test]
    fn concurrent_writers_and_snapshots_account_for_every_event() {
        let store = Arc::new(Store::default());
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let store = store.clone();
                scope.spawn(move || {
                    tracing::subscriber::with_default(subscriber(store.clone()), || {
                        for i in 0..2000 {
                            tracing::trace!(target: "herdr_client", iteration = i, "concurrent");
                            if i % 100 == 0 {
                                let _ = store.snapshot();
                            }
                        }
                    });
                });
            }
        });
        let (generation, records, dropped) = store.snapshot().unwrap();
        assert_eq!(generation, 16000);
        assert!(records.len() <= CAPACITY);
        assert_eq!(records.len() as u64 + dropped, 16000);
    }
}
