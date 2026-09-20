//! Process-local diagnostics only: no files, uploads, environment filters, or stderr.
//! The ring owns at most 5000 records of 4096 serialized JSON bytes each (plus
//! bounded collection overhead). Snapshot holders should replace
//! old snapshots rather than retaining history. Span context includes names, not
//! fields; arbitrary field Debug implementations must cooperate with fmt errors.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    cell::Cell,
    collections::VecDeque,
    fmt::{self, Write},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

const CAPACITY: usize = 5000;
const MAX_BYTES: usize = 4096;
const TIMESTAMP_FORMAT: &str = "[%Y-%m-%d %H:%M:%S]";
static STORE: OnceLock<Arc<Store>> = OnceLock::new();
thread_local! { static FORMATTING: Cell<bool> = const { Cell::new(false) }; }

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "event")]
pub(crate) struct Record {
    #[serde(with = "level_json")]
    pub(crate) level: Level,
    /// Local wall time at capture, retained unchanged for display and export.
    pub(crate) timestamp: String,
    pub(crate) target: String,
    pub(crate) namespace: String,
    pub(crate) message: String,
    pub(crate) fields: Map<String, Value>,
    /// Leaf first, names only. Never retain raw span fields.
    pub(crate) spans: Vec<String>,
    pub(crate) truncated: bool,
}

impl Record {
    pub(crate) fn body(&self) -> String {
        let mut text = self.target.clone();
        for span in &self.spans {
            let _ = write!(text, " [{span}]");
        }
        if !self.message.is_empty() {
            let _ = write!(text, " {}", self.message);
        }
        for (key, value) in &self.fields {
            let _ = write!(text, " {key}={value}");
        }
        if self.truncated {
            text.push_str(" [truncated]");
        }
        text
    }

    pub(crate) fn line(&self) -> String {
        format!(
            "{} {:<5} {}",
            self.timestamp,
            self.level.as_str(),
            self.body()
        )
    }

    #[cfg(test)]
    pub(crate) fn fixture(level: Level, message: impl Into<String>) -> Self {
        Self {
            level,
            timestamp: "[2025-09-26 15:03:45]".into(),
            target: String::new(),
            namespace: String::new(),
            message: message.into(),
            fields: Map::new(),
            spans: Vec::new(),
            truncated: false,
        }
    }
}

mod level_json {
    use super::*;

    pub(super) fn serialize<S: serde::Serializer>(
        level: &Level,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(level.as_str())
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Level, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
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
        let timestamp = chrono::Local::now().format(TIMESTAMP_FORMAT).to_string();
        let meta = event.metadata();
        let mut target = Bounded::new(256);
        let _ = target.write_str(meta.target());
        let mut record = Record {
            level: *meta.level(),
            timestamp,
            target: target.text,
            namespace: meta.target().split("::").next().unwrap_or_default().into(),
            message: String::new(),
            fields: Map::new(),
            spans: Vec::new(),
            truncated: target.truncated,
        };
        if let Some(scope) = ctx.event_scope(event) {
            // Leaf first avoids allocating/reversing an arbitrarily deep scope.
            for (index, span) in scope.take(17).enumerate() {
                if index == 16 {
                    record.truncated = true;
                    break;
                }
                let mut name = Bounded::new(64);
                let _ = name.write_str(span.name());
                record.truncated |= name.truncated;
                record.spans.push(name.text);
            }
        }
        let Ok(base) = serde_json::to_vec(&record) else {
            self.0.lose();
            return;
        };
        let mut visitor = Fields {
            record: &mut record,
            remaining: MAX_BYTES.saturating_sub(base.len()),
            count: 0,
        };
        event.record(&mut visitor);
        self.0.push(record);
    }
}

struct Bounded {
    text: String,
    truncated: bool,
    remaining: usize,
}

impl Bounded {
    fn new(remaining: usize) -> Self {
        Self {
            text: String::new(),
            truncated: false,
            remaining,
        }
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
            let bytes = if ch == '"' || ch == '\\' {
                2
            } else {
                ch.len_utf8()
            };
            if bytes > self.remaining {
                self.truncated = true;
                return Err(fmt::Error);
            }
            self.text.push(ch);
            self.remaining -= bytes;
        }
        Ok(())
    }
}

struct Fields<'a> {
    record: &'a mut Record,
    remaining: usize,
    count: usize,
}

impl Fields<'_> {
    fn budget(&mut self, field: &Field) -> Option<usize> {
        // Field names are static but may still be very long. Reject rather than
        // truncate keys, which could merge unrelated fields.
        let overhead = field.name().len().saturating_mul(6).saturating_add(8);
        if self.count >= 32 || field.name().len() > 128 || overhead >= self.remaining {
            self.record.truncated = true;
            return None;
        }
        self.count += 1;
        self.remaining -= overhead;
        Some(self.remaining)
    }

    fn insert(&mut self, field: &Field, value: Value) {
        let Ok(encoded) = serde_json::to_vec(&value) else {
            return;
        };
        if encoded.len() > self.remaining {
            self.record.truncated = true;
            return;
        }
        self.remaining -= encoded.len();
        if field.name() == "message" {
            self.record.message = match value {
                Value::String(text) => text,
                other => other.to_string(),
            };
        } else {
            self.record.fields.insert(field.name().into(), value);
        }
    }

    fn scalar(&mut self, field: &Field, value: Value) {
        if self.budget(field).is_some() {
            self.insert(field, value);
        }
    }
}

impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let Some(budget) = self.budget(field) else {
            return;
        };
        let mut text = Bounded::new(budget.saturating_sub(2));
        let _ = write!(text, "{value:?}");
        self.record.truncated |= text.truncated;
        self.insert(field, Value::String(text.text));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        let Some(budget) = self.budget(field) else {
            return;
        };
        let mut text = Bounded::new(budget.saturating_sub(2));
        let _ = text.write_str(value);
        self.record.truncated |= text.truncated;
        self.insert(field, Value::String(text.text));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.scalar(field, value.into());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.scalar(field, value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.scalar(field, value.into());
    }
    fn record_i128(&mut self, field: &Field, value: i128) {
        if let Some(number) = serde_json::Number::from_i128(value) {
            self.scalar(field, Value::Number(number));
        } else {
            self.record_debug(field, &value);
        }
    }
    fn record_u128(&mut self, field: &Field, value: u128) {
        if let Some(number) = serde_json::Number::from_u128(value) {
            self.scalar(field, Value::Number(number));
        } else {
            self.record_debug(field, &value);
        }
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        if value.is_finite() {
            self.scalar(field, value.into());
        } else {
            self.record_debug(field, &value);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_and_left_aligned_levels_have_exact_columns() {
        let time = chrono::DateTime::parse_from_rfc3339("2025-09-26T15:03:45-07:00").unwrap();
        let timestamp = time.format(TIMESTAMP_FORMAT).to_string();
        assert_eq!(timestamp, "[2025-09-26 15:03:45]");
        for (level, padded) in [
            (Level::TRACE, "TRACE"),
            (Level::DEBUG, "DEBUG"),
            (Level::INFO, "INFO "),
            (Level::WARN, "WARN "),
            (Level::ERROR, "ERROR"),
        ] {
            let mut record = Record::fixture(level, "message");
            record.target = "target".into();
            assert_eq!(
                record.line(),
                format!("[2025-09-26 15:03:45] {padded} target message")
            );
            assert_eq!(&record.line()[28..], "target message");
        }
    }

    #[test]
    fn public_api_is_available_without_installing_a_global_subscriber() {
        let _init = init;
        let _ = generation();
        assert!(snapshot().is_some());
    }

    #[test]
    fn json_schema_preserves_typed_fields_and_roundtrips() {
        let store = Arc::new(Store::default());
        tracing::subscriber::with_default(subscriber(store.clone()), || {
            let span = tracing::info_span!(target: "herdr_gpui", "paint", password = "secret");
            let _entered = span.enter();
            tracing::info!(target: "herdr_gpui::terminal_painter",
                ready = true, signed = -7_i64, unsigned = u64::MAX, ratio = 1.25_f64,
                text = "quoted \"text\"", debug = ?[1, 2], large = u128::MAX,
                nonfinite = f64::INFINITY, "Paint complete");
        });
        let (_, rows, _) = store.snapshot().unwrap();
        let row = &*rows[0];
        let encoded = serde_json::to_string(row).unwrap();
        let json: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(json["type"], "event");
        assert_eq!(json["level"], "INFO");
        assert_eq!(json["namespace"], "herdr_gpui");
        assert_eq!(json["target"], "herdr_gpui::terminal_painter");
        assert_eq!(json["message"], "Paint complete");
        assert_eq!(json["fields"]["ready"], true);
        assert_eq!(json["fields"]["signed"], -7);
        assert_eq!(json["fields"]["unsigned"], u64::MAX);
        assert_eq!(json["fields"]["ratio"], 1.25);
        assert_eq!(json["fields"]["text"], "quoted \"text\"");
        assert_eq!(json["fields"]["debug"], "[1, 2]");
        assert_eq!(json["fields"]["large"], u128::MAX.to_string());
        assert_eq!(json["fields"]["nonfinite"], "inf");
        assert_eq!(json["spans"], serde_json::json!(["paint"]));
        assert!(!encoded.contains("secret"));
        assert_eq!(serde_json::from_str::<Record>(&encoded).unwrap(), *row);
        assert!(serde_json::from_str::<Record>(&encoded.replace("INFO", "INVALID")).is_err());
    }

    #[test]
    fn json_escaping_and_many_fields_share_one_record_budget() {
        let store = Arc::new(Store::default());
        let huge = "\"\\\n\u{1f980}".repeat(100_000);
        tracing::subscriber::with_default(subscriber(store.clone()), || {
            tracing::info!(target: "herdr_client", a = huge.as_str(), b = huge.as_str(), "{}", huge);
            tracing::info!(target: "herdr_client",
                a01=1,a02=2,a03=3,a04=4,a05=5,a06=6,a07=7,a08=8,a09=9,a10=10,
                a11=1,a12=2,a13=3,a14=4,a15=5,a16=6,a17=7,a18=8,a19=9,a20=10,
                a21=1,a22=2,a23=3,a24=4,a25=5,a26=6,a27=7,a28=8,a29=9,a30=10,
                a31=1,a32=2,a33=3,a34=4);
        });
        let (_, rows, _) = store.snapshot().unwrap();
        for row in &rows {
            let encoded = serde_json::to_vec(row.as_ref()).unwrap();
            assert!(encoded.len() <= MAX_BYTES, "{}", encoded.len());
            assert!(row.truncated);
            assert!(row.fields.len() <= 32);
            assert_eq!(serde_json::from_slice::<Record>(&encoded).unwrap(), **row);
        }
        assert_eq!(rows[1].fields.len(), 32);
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
            assert_eq!(record.spans, ["connection"]);
            assert!(!record.body().contains("not retained"));
            assert!(
                chrono::NaiveDateTime::parse_from_str(&record.timestamp, "[%Y-%m-%d %H:%M:%S]")
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
        assert!(serde_json::to_vec(&*records[0]).unwrap().len() <= MAX_BYTES);
        assert!(records[0].truncated);
        assert!(!records[0].body().chars().any(char::is_control));
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
            store.push(Record::fixture(Level::INFO, i.to_string()));
        }
        let (generation, records, dropped) = store.snapshot().unwrap();
        assert_eq!(
            (generation, records.len(), dropped),
            ((CAPACITY + 2) as u64, CAPACITY, 2)
        );
        assert_eq!(records[0].message, "2");
        let lock = store.records.lock().unwrap();
        assert!(store.snapshot().is_none());
        store.push(Record::fixture(Level::INFO, "lost"));
        drop(lock);
        assert_eq!(store.snapshot().unwrap().2, 3);
        assert_eq!(records[0].message, "2");
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
