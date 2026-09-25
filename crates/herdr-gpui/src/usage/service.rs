//! What an agent must provide for its usage to be shown. Adding one takes a
//! [`Provider`](super::model::Provider) variant, an implementation of
//! [`Service`] in its own module, and the arm that maps one to the other.
//! Everything else, local and remote fetching, caching, the status bar, and
//! the panel, works from the trait.

use super::model::Report;
use crate::{Error, Result, icons::AgentIcon};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use ureq::http::HeaderValue;
use zeroize::Zeroizing;

pub(crate) trait Service: Sync {
    fn name(&self) -> &'static str;
    /// Lowercase ASCII identifier, used as the remote script's section marker.
    fn key(&self) -> &'static str;
    fn icon(&self) -> AgentIcon;
    /// Where the account's own usage page lives.
    fn dashboard(&self) -> &'static str;
    fn status_page(&self) -> &'static str;
    /// The [`Meta`] names the remote fragment may print; others are dropped.
    #[cfg(unix)]
    fn meta(&self) -> &'static [&'static str] {
        &[]
    }
    /// The request this machine's saved sign-in makes, or None when the agent
    /// is not signed in here. Credentials are only read, never refreshed.
    fn local(&self) -> Option<Result<Request>>;
    /// A POSIX `sh` fragment that does what [`Service::local`] does on a
    /// remote host. It may use `field NAME` (the first JSON string value named
    /// NAME on stdin) and must answer, when signed in, by piping its secret
    /// headers into `request KEY "META" CURL_ARGS…`, where META is
    /// space-separated `name=value` pairs for [`Meta`]. Secrets must never
    /// appear in curl's arguments, where other users could read them. Remote
    /// hosts are reached over SSH, which only Unix clients support.
    #[cfg(unix)]
    fn remote(&self) -> &'static str;
    /// A successful response body, with what the sign-in said about itself.
    fn parse(&self, body: &str, meta: &Meta) -> Result<Report>;
}

pub(crate) struct Request {
    pub url: &'static str,
    pub headers: Vec<(&'static str, HeaderValue)>,
    pub meta: Meta,
}

/// Facts about a sign-in that its service's response does not repeat, such as
/// the plan tier, gathered alike on this machine and by the remote script.
/// Values are single bounded words, so the script can print them on one line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Meta(Vec<(&'static str, String)>);

const META_VALUE_LIMIT: usize = 128;

impl Meta {
    pub fn with(mut self, name: &'static str, value: Option<impl AsRef<str>>) -> Self {
        if let Some(value) = value
            .as_ref()
            .map(|value| value.as_ref().trim())
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= META_VALUE_LIMIT
                    && !value.chars().any(|c| c.is_whitespace() || c.is_control())
            })
        {
            self.0.retain(|(existing, _)| *existing != name);
            self.0.push((name, value.to_owned()));
        }
        self
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(existing, _)| *existing == name)
            .map(|(_, value)| value.as_str())
    }

    /// Only names in `known` are kept, so a remote host cannot grow the map.
    #[cfg(unix)]
    pub fn parse(line: &str, known: &[&'static str]) -> Self {
        line.split_whitespace()
            .filter_map(|pair| pair.split_once('='))
            .fold(Self::default(), |meta, (name, value)| {
                match known.iter().find(|known| **known == name) {
                    Some(name) => meta.with(name, Some(value)),
                    None => meta,
                }
            })
    }
}

pub(super) fn bearer(token: &SecretString) -> Result<HeaderValue> {
    let text = Zeroizing::new(format!("Bearer {}", token.expose_secret()));
    let mut header = HeaderValue::from_str(&text).map_err(Error::UsageHeader)?;
    header.set_sensitive(true);
    Ok(header)
}

/// `$VARIABLE` when set, else `default` under the home directory.
pub(super) fn agent_home(variable: &str, default: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::config::home().ok().map(|home| home.join(default)))
}

/// Agent configuration can be large, but not this large.
const FILE_LIMIT: u64 = 16 * 1024 * 1024;

/// A file that may hold secrets, wiped from memory when dropped. Missing,
/// unreadable, and oversized files all read as absent.
pub(super) fn read_private(path: &Path) -> Option<Zeroizing<Vec<u8>>> {
    use std::io::Read;
    let mut bytes = Zeroizing::new(Vec::new());
    std::fs::File::open(path)
        .ok()?
        .take(FILE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= FILE_LIMIT).then_some(bytes)
}

pub(super) fn json<'a, T: Deserialize<'a>>(body: &'a str) -> Result<T> {
    serde_json::from_str(body).map_err(|error| Error::UsageJson(error.classify()))
}

/// Seconds, milliseconds, or RFC 3339, as the services have each used.
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Timestamp {
    Number(f64),
    Text(String),
}

impl Timestamp {
    pub fn time(&self) -> Option<SystemTime> {
        match self {
            Self::Number(value) if value.is_finite() && *value > 0. => {
                let seconds = if *value > 1e10 { value / 1000. } else { *value };
                SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs_f64(seconds))
            }
            Self::Number(_) => None,
            Self::Text(text) => {
                let at = chrono::DateTime::parse_from_rfc3339(text).ok()?;
                let seconds = u64::try_from(at.timestamp()).ok()?;
                SystemTime::UNIX_EPOCH
                    .checked_add(Duration::new(seconds, at.timestamp_subsec_nanos()))
            }
        }
    }
}
