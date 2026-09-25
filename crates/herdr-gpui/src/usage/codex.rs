//! Codex plan usage, read with the Codex CLI's own ChatGPT sign-in.

use super::{
    model::{Account, Kind, Provider, Report, SESSION, Section, WEEK, Window, title_case},
    service::{Meta, Request, Service, Timestamp, agent_home, bearer, json, read_private},
};
use crate::{Error, Result, icons::AgentIcon};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::time::Duration;
use ureq::http::HeaderValue;

pub(super) const URL: &str = "https://chatgpt.com/backend-api/wham/usage";
pub(super) const AGENT: &str = "codex-cli";

pub(super) struct Codex;

impl Service for Codex {
    fn name(&self) -> &'static str {
        "Codex"
    }

    fn key(&self) -> &'static str {
        "codex"
    }

    fn icon(&self) -> AgentIcon {
        AgentIcon::Codex
    }

    fn dashboard(&self) -> &'static str {
        "https://chatgpt.com/codex/settings/usage"
    }

    fn status_page(&self) -> &'static str {
        "https://status.openai.com"
    }

    fn local(&self) -> Option<Result<Request>> {
        let home = agent_home("CODEX_HOME", ".codex")?;
        let auth = credential(&read_private(&home.join("auth.json"))?)?;
        Some(headers(&auth).map(|headers| Request {
            url: URL,
            headers,
            meta: Meta::default(),
        }))
    }

    #[cfg(unix)]
    fn remote(&self) -> &'static str {
        r#"codex_home=${CODEX_HOME:-$HOME/.codex}
if [ -r "$codex_home/auth.json" ]; then
    token=$(field access_token < "$codex_home/auth.json")
    account=$(field account_id < "$codex_home/auth.json")
    if [ -n "$token" ]; then
        {
            printf 'Authorization: Bearer %s\n' "$token"
            if [ -n "$account" ]; then printf 'ChatGPT-Account-Id: %s\n' "$account"; fi
        } | request codex "" -H 'User-Agent: codex-cli' -H 'OpenAI-Beta: codex-1' \
            -H 'originator: Codex Desktop' https://chatgpt.com/backend-api/wham/usage
    fi
    token=
fi
"#
    }

    fn parse(&self, body: &str, _meta: &Meta) -> Result<Report> {
        let usage: Usage = json(body)?;
        let windows = usage.rate_limit.map(Limits::windows).unwrap_or_default();
        let review = usage
            .code_review_rate_limit
            .map(Limits::windows)
            .unwrap_or_default()
            .into_iter()
            .next()
            .map(|window| {
                Section::Limit(Window {
                    kind: Kind::Model("Code review".into()),
                    ..window
                })
            });
        let resets = usage
            .rate_limit_reset_credits
            .map(|credits| Section::Facts {
                title: "Limit reset credits".into(),
                facts: vec![("Available".into(), credits.available_count.to_string())],
            });
        let credits = usage.credits.map(|credits| Section::Facts {
            title: "Credits".into(),
            facts: vec![(
                "Balance".into(),
                if credits.unlimited.unwrap_or(false) {
                    "Unlimited".into()
                } else {
                    credits.balance.unwrap_or_else(|| "0".into())
                },
            )],
        });
        let account = Account {
            email: usage.email.filter(|email| !email.trim().is_empty()),
            plan: usage
                .plan_type
                .map(|plan| title_case(&plan))
                .filter(|plan| !plan.is_empty()),
        };
        Ok(Report::new(Provider::Codex, account, windows)
            .with_sections(review.into_iter().chain(resets).chain(credits)))
    }
}

fn headers(auth: &Auth) -> Result<Vec<(&'static str, HeaderValue)>> {
    let mut headers = vec![
        ("Authorization", bearer(&auth.access_token)?),
        ("User-Agent", HeaderValue::from_static(AGENT)),
        ("OpenAI-Beta", HeaderValue::from_static("codex-1")),
        ("originator", HeaderValue::from_static("Codex Desktop")),
    ];
    if let Some(account) = &auth.account_id {
        headers.push((
            "ChatGPT-Account-Id",
            HeaderValue::from_str(account).map_err(Error::UsageHeader)?,
        ));
    }
    Ok(headers)
}

#[derive(Deserialize)]
struct AuthFile {
    tokens: Option<Auth>,
}

#[derive(Deserialize)]
pub(super) struct Auth {
    access_token: SecretString,
    account_id: Option<String>,
}

/// An API-key-only Codex install has no `tokens` and no plan usage to show.
pub(super) fn credential(bytes: &[u8]) -> Option<Auth> {
    serde_json::from_slice::<AuthFile>(bytes)
        .ok()?
        .tokens
        .filter(|auth| !auth.access_token.expose_secret().trim().is_empty())
}

#[derive(Deserialize)]
struct Usage {
    email: Option<String>,
    plan_type: Option<String>,
    rate_limit: Option<Limits>,
    code_review_rate_limit: Option<Limits>,
    rate_limit_reset_credits: Option<ResetCredits>,
    credits: Option<Credits>,
}

#[derive(Deserialize)]
struct Limits {
    primary_window: Option<LimitWindow>,
    secondary_window: Option<LimitWindow>,
}

impl Limits {
    /// A window is known by its length; which slot it arrives in only decides
    /// when the length is missing or unfamiliar.
    fn windows(self) -> Vec<Window> {
        [
            (Kind::Session, self.primary_window),
            (Kind::Weekly, self.secondary_window),
        ]
        .into_iter()
        .filter_map(|(slot, window)| {
            let window = window?;
            let length = window.limit_window_seconds.map(Duration::from_secs);
            let kind = match length {
                Some(length) if length.abs_diff(SESSION) <= Duration::from_secs(60) => {
                    Kind::Session
                }
                Some(length) if length.abs_diff(WEEK) <= Duration::from_secs(60) => Kind::Weekly,
                _ => slot,
            };
            Some(Window::new(
                kind,
                window.used_percent,
                window.reset_at.as_ref().and_then(Timestamp::time),
                length,
            ))
        })
        .collect()
    }
}

#[derive(Deserialize)]
struct LimitWindow {
    used_percent: f64,
    limit_window_seconds: Option<u64>,
    reset_at: Option<Timestamp>,
}

#[derive(Deserialize)]
struct ResetCredits {
    available_count: u32,
}

#[derive(Deserialize)]
struct Credits {
    unlimited: Option<bool>,
    balance: Option<String>,
}
