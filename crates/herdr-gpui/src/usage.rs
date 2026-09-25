//! Plan usage of the coding agents signed in on the selected host, for the
//! status bar. One worker thread reads one host at a time; the UI thread only
//! queues a host and takes the answer on a later tick. Each host keeps its last
//! answer, so switching back shows it at once and a failed refresh keeps the
//! numbers it had, marked stale, rather than blanking them.

mod fetch;
mod model;
mod parse;
#[cfg(unix)]
mod remote;
mod render;

#[cfg(test)]
mod tests;

pub(crate) use model::Host;
use model::{Provider, Report};

use std::{
    collections::HashMap,
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime},
};

/// The services rate limit these endpoints, and the numbers move slowly.
const REFRESH: Duration = Duration::from_secs(10 * 60);
const RATE_LIMITED: Duration = Duration::from_secs(30 * 60);
/// A host that could not be reached is retried sooner than a full refresh.
const ERROR_BACKOFF: Duration = Duration::from_secs(5 * 60);
/// A click cannot queue reads back to back.
const MANUAL_SPACING: Duration = Duration::from_secs(10);
/// Hosts are few; an unbounded catalog still cannot grow the cache past this.
const HOST_LIMIT: usize = 16;

type Answers = crate::Result<Vec<(Provider, crate::Result<Report>)>>;

/// One provider on the shown host: its last good report, and why the latest
/// refresh failed if it did.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reading {
    pub provider: Provider,
    pub report: Option<Report>,
    pub error: Option<String>,
}

#[derive(Default)]
pub(crate) struct Entry {
    pub readings: Vec<Reading>,
    /// Why the host itself could not be read, such as SSH failing.
    pub error: Option<String>,
    pub updated: Option<SystemTime>,
    due: Option<Instant>,
    requested: Option<Instant>,
}

struct Worker {
    requests: mpsc::SyncSender<Host>,
    results: mpsc::Receiver<(Host, Answers)>,
}

#[derive(Default)]
pub(crate) struct Usage {
    host: Option<Host>,
    entries: HashMap<Host, Entry>,
    worker: Option<Worker>,
    busy: Option<Host>,
    minute: u64,
}

impl Usage {
    /// What the status bar shows for the tracked host.
    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.host.as_ref()?)
    }

    /// Whether the tracked host is being read right now.
    pub fn busy(&self) -> bool {
        self.host.is_some() && self.busy == self.host
    }

    /// Reads the tracked host on the next poll, unless it was just read.
    pub fn refresh(&mut self, now: Instant) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let entry = self.entries.entry(host).or_default();
        if entry
            .requested
            .is_none_or(|requested| now.duration_since(requested) >= MANUAL_SPACING)
        {
            entry.due = Some(now);
        }
    }

    /// Follows `host` (None hides usage), takes a finished read, and starts
    /// the next one when it is due. Reads only start while `active`, so a
    /// background window costs no requests. Returns whether anything shown
    /// changed, including the minute the reset countdowns count from.
    pub fn poll(&mut self, host: Option<Host>, active: bool, now: Instant) -> bool {
        let mut changed = self.host != host;
        self.host = host;
        if let Some(worker) = &self.worker {
            match worker.results.try_recv() {
                Ok((host, answers)) => {
                    self.busy = None;
                    self.apply(host, answers, now);
                    changed = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.busy = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let minute = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / 60;
        if minute != self.minute {
            self.minute = minute;
            changed |= self
                .current()
                .is_some_and(|entry| !entry.readings.is_empty());
        }
        if active && self.busy.is_none() {
            changed |= self.dispatch(now);
        }
        changed
    }

    fn dispatch(&mut self, now: Instant) -> bool {
        let Some(host) = self.host.clone() else {
            return false;
        };
        if self
            .entries
            .get(&host)
            .and_then(|entry| entry.due)
            .is_some_and(|due| due > now)
        {
            return false;
        }
        if self.worker.is_none() {
            self.worker = spawn();
        }
        let Some(worker) = &self.worker else {
            self.entries.entry(host).or_default().due = Some(now + ERROR_BACKOFF);
            return false;
        };
        match worker.requests.try_send(host.clone()) {
            Ok(()) => {
                let entry = self.entries.entry(host.clone()).or_default();
                entry.requested = Some(now);
                // Due again only once this read answers.
                entry.due = Some(now + REFRESH);
                self.busy = Some(host);
                true
            }
            Err(mpsc::TrySendError::Full(_)) => false,
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.worker = None;
                false
            }
        }
    }

    fn apply(&mut self, host: Host, answers: Answers, now: Instant) {
        if self.entries.len() >= HOST_LIMIT && !self.entries.contains_key(&host) {
            let shown = self.host.clone();
            self.entries.retain(|host, _| Some(host) == shown.as_ref());
        }
        let entry = self.entries.entry(host).or_default();
        match answers {
            Ok(answers) => {
                let mut due = REFRESH;
                entry.error = None;
                entry.updated = Some(SystemTime::now());
                // A provider missing from the answer has been signed out.
                entry.readings = answers
                    .into_iter()
                    .map(|(provider, report)| match report {
                        Ok(report) => Reading {
                            provider,
                            report: Some(report),
                            error: None,
                        },
                        Err(error) => {
                            if matches!(error, crate::Error::UsageRateLimited) {
                                due = RATE_LIMITED;
                            }
                            Reading {
                                provider,
                                report: entry
                                    .readings
                                    .iter()
                                    .find(|reading| reading.provider == provider)
                                    .and_then(|reading| reading.report.clone()),
                                error: Some(error.to_string()),
                            }
                        }
                    })
                    .collect();
                entry.due = Some(now + due);
            }
            Err(error) => {
                entry.error = Some(error.to_string());
                entry.due = Some(now + ERROR_BACKOFF);
            }
        }
    }
}

fn spawn() -> Option<Worker> {
    let (requests, incoming) = mpsc::sync_channel::<Host>(1);
    let (outgoing, results) = mpsc::sync_channel(1);
    let spawned = thread::Builder::new()
        .name("herdr-usage".into())
        .spawn(move || {
            for host in incoming {
                let answers = fetch::fetch(&host).map(|answers| {
                    answers
                        .into_iter()
                        .map(|(provider, raw)| (provider, raw.and_then(|raw| parse::report(&raw))))
                        .collect()
                });
                if outgoing.send((host, answers)).is_err() {
                    break;
                }
            }
        });
    match spawned {
        Ok(_) => Some(Worker { requests, results }),
        Err(error) => {
            tracing::warn!(category = "usage", %error, "could not start the usage worker");
            None
        }
    }
}
