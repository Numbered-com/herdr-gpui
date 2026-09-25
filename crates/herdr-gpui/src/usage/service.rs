//! What a provider implements for its usage to be shown. A provider is one
//! module under `providers/` with a unit struct implementing [`Service`], and
//! one line in [`super::registry`]. Everything else, detection, local and
//! remote fetching, caching, the status bar, and the panel, works from the
//! trait: a provider reads through a [`Probe`] and draws through a [`Ui`].

use super::{model::Report, probe::Probe, ui::Ui};
use crate::{Error, Result};
use gpui::{AnyElement, App};
use serde::Deserialize;
use std::time::{Duration, SystemTime};

/// A config value a provider reads, documented where users set it:
/// `[usage.providers.<id>] <name> = "…"`, or one of `env` for this app.
#[derive(Debug)]
pub(crate) struct Setting {
    pub name: &'static str,
    pub env: &'static [&'static str],
    /// What the value is and where to find it, e.g. which cookie to copy
    /// from which site's developer tools.
    pub help: &'static str,
}

impl Setting {
    pub const fn new(name: &'static str, env: &'static [&'static str], help: &'static str) -> Self {
        Self { name, env, help }
    }
}

pub(crate) trait Service: Sync {
    /// Lowercase ASCII, stable: config tables and saved choices use it.
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    /// An embedded SVG asset path.
    fn icon(&self) -> &'static str {
        "icons/agent-generic.svg"
    }
    /// The account's own usage page.
    fn dashboard(&self) -> Option<&'static str> {
        None
    }
    fn status_page(&self) -> Option<&'static str> {
        None
    }
    /// Values this provider reads from config or the environment.
    fn settings(&self) -> &'static [Setting] {
        &[]
    }
    /// Finds the sign-in through `probe` and asks the service. None means
    /// the probed host has no sign-in and the config names none, so the
    /// provider is left out unless the config asks for it.
    fn fetch(&self, probe: &mut Probe) -> Option<Result<Report>>;
    /// The panel body. The default draws the shared fields; a provider with
    /// its own detail draws that too, from [`Report::detail`].
    fn render(&self, report: &Report, ui: &Ui, _cx: &App) -> AnyElement {
        ui.standard(report)
    }
}

pub(super) fn json<'a, T: Deserialize<'a>>(body: &'a str) -> Result<T> {
    serde_json::from_str(body).map_err(|error| Error::UsageJson(error.classify()))
}

/// Seconds, milliseconds, or RFC 3339, as services variously send times.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Timestamp {
    Number(f64),
    Text(String),
}

impl Timestamp {
    pub fn time(&self) -> Option<SystemTime> {
        match self {
            Self::Number(value) if value.is_finite() && *value > 0. => {
                let seconds = if *value > 1e11 { value / 1000. } else { *value };
                SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs_f64(seconds))
            }
            Self::Number(_) => None,
            Self::Text(text) => {
                if let Ok(number) = text.trim().parse::<f64>() {
                    return Self::Number(number).time();
                }
                let at = chrono::DateTime::parse_from_rfc3339(text.trim())
                    .ok()
                    .map(|at| at.to_utc())
                    .or_else(|| {
                        chrono::NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%dT%H:%M:%S%.f")
                            .or_else(|_| {
                                chrono::NaiveDateTime::parse_from_str(
                                    text.trim(),
                                    "%Y-%m-%d %H:%M:%S",
                                )
                            })
                            .ok()
                            .map(|at| at.and_utc())
                    })?;
                let seconds = u64::try_from(at.timestamp()).ok()?;
                SystemTime::UNIX_EPOCH
                    .checked_add(Duration::new(seconds, at.timestamp_subsec_nanos()))
            }
        }
    }
}

/// Deserializes a number that a service may send as a string.
pub(crate) fn number<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Loose {
        Number(f64),
        Text(String),
        Null,
    }
    Ok(match Option::<Loose>::deserialize(deserializer)? {
        Some(Loose::Number(value)) => Some(value),
        Some(Loose::Text(text)) => text.trim().parse().ok(),
        Some(Loose::Null) | None => None,
    })
}
