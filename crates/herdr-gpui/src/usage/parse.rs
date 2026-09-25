//! Service responses to reports. Both local HTTP and the remote script hand
//! over the same status and body, so every host is judged by one parser.

use super::model::{Kind, Provider, Report, Window};
use crate::{Error, Result};
use serde::Deserialize;
use std::time::{Duration, SystemTime};

/// One provider's answer, before it is trusted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Raw {
    pub provider: Provider,
    /// The HTTP status, or 0 when the request never got an answer.
    pub status: u16,
    pub body: String,
}

pub(crate) fn report(raw: &Raw) -> Result<Report> {
    match raw.status {
        200..=299 => {}
        0 => return Err(Error::UsageConnect),
        401 | 403 => return Err(Error::UsageRejected),
        429 => return Err(Error::UsageRateLimited),
        status => return Err(Error::UsageStatus(status)),
    }
    match raw.provider {
        Provider::Claude => claude(&raw.body),
        Provider::Codex => codex(&raw.body),
    }
}

fn json<'a, T: Deserialize<'a>>(body: &'a str) -> Result<T> {
    serde_json::from_str(body).map_err(|error| Error::UsageJson(error.classify()))
}

/// Seconds, milliseconds, or RFC 3339, as the services have each used.
#[derive(Deserialize)]
#[serde(untagged)]
enum Timestamp {
    Number(f64),
    Text(String),
}

impl Timestamp {
    fn time(&self) -> Option<SystemTime> {
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

#[derive(Deserialize)]
struct ClaudeUsage {
    limits: Option<Vec<ClaudeLimit>>,
    five_hour: Option<ClaudeWindow>,
    seven_day: Option<ClaudeWindow>,
}

#[derive(Deserialize)]
struct ClaudeLimit {
    kind: String,
    percent: Option<f64>,
    resets_at: Option<Timestamp>,
    scope: Option<ClaudeScope>,
}

#[derive(Deserialize)]
struct ClaudeScope {
    model: Option<ClaudeModel>,
}

#[derive(Deserialize)]
struct ClaudeModel {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct ClaudeWindow {
    utilization: Option<f64>,
    resets_at: Option<Timestamp>,
}

/// `limits` names every window the account has, model-scoped ones included;
/// the older `five_hour`/`seven_day` pair stands in when it is absent.
fn claude(body: &str) -> Result<Report> {
    let usage: ClaudeUsage = json(body)?;
    let mut windows: Vec<Window> = usage
        .limits
        .unwrap_or_default()
        .into_iter()
        .filter_map(|limit| {
            let kind = match limit.kind.as_str() {
                "session" => Kind::Session,
                "weekly_all" => Kind::Weekly,
                "weekly_scoped" => Kind::Model(
                    limit
                        .scope?
                        .model?
                        .display_name
                        .filter(|name| !name.trim().is_empty())?,
                ),
                _ => return None,
            };
            Some(Window::new(
                kind,
                limit.percent?,
                limit.resets_at.as_ref().and_then(Timestamp::time),
            ))
        })
        .collect();
    if windows.is_empty() {
        windows = [
            (Kind::Session, usage.five_hour),
            (Kind::Weekly, usage.seven_day),
        ]
        .into_iter()
        .filter_map(|(kind, window)| {
            let window = window?;
            Some(Window::new(
                kind,
                window.utilization?,
                window.resets_at.as_ref().and_then(Timestamp::time),
            ))
        })
        .collect();
    }
    Ok(Report::new(Provider::Claude, None, windows))
}

#[derive(Deserialize)]
struct CodexUsage {
    plan_type: Option<String>,
    rate_limit: Option<CodexLimits>,
}

#[derive(Deserialize)]
struct CodexLimits {
    primary_window: Option<CodexWindow>,
    secondary_window: Option<CodexWindow>,
}

#[derive(Deserialize)]
struct CodexWindow {
    used_percent: f64,
    limit_window_seconds: Option<u64>,
    reset_at: Option<Timestamp>,
}

const SESSION: u64 = 5 * 3600;
const WEEK: u64 = 7 * 86_400;

/// A window is known by its length; which slot it arrives in only decides
/// when the length is missing or unfamiliar.
fn codex(body: &str) -> Result<Report> {
    let usage: CodexUsage = json(body)?;
    let limits = usage.rate_limit;
    let windows = limits
        .map(|limits| {
            [
                (Kind::Session, limits.primary_window),
                (Kind::Weekly, limits.secondary_window),
            ]
        })
        .into_iter()
        .flatten()
        .filter_map(|(slot, window)| {
            let window = window?;
            let kind = match window.limit_window_seconds {
                Some(length) if length.abs_diff(SESSION) <= 60 => Kind::Session,
                Some(length) if length.abs_diff(WEEK) <= 60 => Kind::Weekly,
                _ => slot,
            };
            Some(Window::new(
                kind,
                window.used_percent,
                window.reset_at.as_ref().and_then(Timestamp::time),
            ))
        })
        .collect();
    let plan = usage
        .plan_type
        .filter(|plan| !plan.trim().is_empty())
        .map(|plan| {
            let mut chars = plan.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        });
    Ok(Report::new(Provider::Codex, plan, windows))
}
