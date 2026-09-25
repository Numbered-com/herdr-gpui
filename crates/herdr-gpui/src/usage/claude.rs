//! Claude plan usage, read with Claude Code's own sign-in.

use super::{
    fetch::output,
    model::{Account, Kind, Provider, Report, SESSION, Section, WEEK, Window, title_case},
    service::{Meta, Request, Service, Timestamp, agent_home, bearer, json, read_private},
};
use crate::{Result, icons::AgentIcon};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::{process::Command, time::Duration};
use ureq::http::HeaderValue;

pub(super) const URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub(super) const BETA: &str = "oauth-2025-04-20";
/// The usage endpoint serves Claude Code's OAuth clients.
pub(super) const AGENT: &str = "claude-code/2.1.0";
pub(super) const KEYCHAIN: &str = "Claude Code-credentials";
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct Claude;

impl Service for Claude {
    fn name(&self) -> &'static str {
        "Claude"
    }

    fn key(&self) -> &'static str {
        "claude"
    }

    fn icon(&self) -> AgentIcon {
        AgentIcon::Claude
    }

    fn dashboard(&self) -> &'static str {
        "https://claude.ai/settings/usage"
    }

    fn status_page(&self) -> &'static str {
        "https://status.anthropic.com"
    }

    #[cfg(unix)]
    fn meta(&self) -> &'static [&'static str] {
        &["plan", "tier", "email"]
    }

    fn local(&self) -> Option<Result<Request>> {
        let oauth = credentials()?;
        let token = oauth.access_token?;
        let email = agent_home("CLAUDE_CONFIG_DIR", ".claude")
            .and_then(|directory| {
                // Claude Code keeps its account beside the directory by default.
                let inside = directory.join(".claude.json");
                read_private(&inside)
                    .or_else(|| read_private(&directory.parent()?.join(".claude.json")))
            })
            .and_then(|bytes| serde_json::from_slice::<Profile>(&bytes).ok())
            .and_then(|profile| profile.account?.email);
        let meta = Meta::default()
            .with("plan", oauth.subscription)
            .with("tier", oauth.tier)
            .with("email", email);
        Some(bearer(&token).map(|authorization| Request {
            url: URL,
            headers: vec![
                ("Authorization", authorization),
                ("anthropic-beta", HeaderValue::from_static(BETA)),
                ("User-Agent", HeaderValue::from_static(AGENT)),
            ],
            meta,
        }))
    }

    #[cfg(unix)]
    fn remote(&self) -> &'static str {
        r#"claude_dir=${CLAUDE_CONFIG_DIR:-$HOME/.claude}
credentials=
if [ -r "$claude_dir/.credentials.json" ]; then
    credentials=$(cat "$claude_dir/.credentials.json")
elif command -v security >/dev/null 2>&1; then
    credentials=$(security find-generic-password -s 'Claude Code-credentials' -w 2>/dev/null)
fi
token=$(printf '%s\n' "$credentials" | field accessToken)
plan=$(printf '%s\n' "$credentials" | field subscriptionType)
tier=$(printf '%s\n' "$credentials" | field rateLimitTier)
credentials=
email=
for profile in "$claude_dir/.claude.json" "$HOME/.claude.json"; do
    if [ -z "$email" ] && [ -r "$profile" ]; then email=$(field emailAddress < "$profile"); fi
done
if [ -n "$token" ]; then
    printf 'Authorization: Bearer %s\n' "$token" | request claude "plan=$plan tier=$tier email=$email" \
        -H 'anthropic-beta: oauth-2025-04-20' -H 'User-Agent: claude-code/2.1.0' \
        https://api.anthropic.com/api/oauth/usage
fi
token=
"#
    }

    fn parse(&self, body: &str, meta: &Meta) -> Result<Report> {
        let usage: Usage = json(body)?;
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
                let length = if kind == Kind::Session { SESSION } else { WEEK };
                Some(Window::new(
                    kind,
                    limit.percent?,
                    limit.resets_at.as_ref().and_then(Timestamp::time),
                    Some(length),
                ))
            })
            .collect();
        if windows.is_empty() {
            windows = [
                (Kind::Session, usage.five_hour, SESSION),
                (Kind::Weekly, usage.seven_day, WEEK),
            ]
            .into_iter()
            .filter_map(|(kind, window, length)| {
                let window = window?;
                Some(Window::new(
                    kind,
                    window.utilization?,
                    window.resets_at.as_ref().and_then(Timestamp::time),
                    Some(length),
                ))
            })
            .collect();
        }
        let account = Account {
            email: meta.get("email").map(str::to_owned),
            plan: plan(meta.get("plan"), meta.get("tier")),
        };
        let shares: Vec<_> = usage
            .seven_day_breakdown
            .map(|breakdown| breakdown.rows)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|row| Some((row.display_name?, row.percent.filter(|p| *p > 0.)? as f32)))
            .collect();
        let spend = usage.spend.map(|spend| Section::Facts {
            title: "Extra usage".into(),
            facts: if spend.enabled.unwrap_or(false) {
                let used = spend.used.as_ref().and_then(Money::text);
                let limit = spend.limit.as_ref().and_then(Money::text);
                vec![(
                    "This month".into(),
                    match (used, limit) {
                        (Some(used), Some(limit)) => format!("{used} of {limit}"),
                        (Some(used), None) => used,
                        _ => "On".into(),
                    },
                )]
            } else {
                vec![("Status".into(), "Off".into())]
            },
        });
        Ok(
            Report::new(Provider::Claude, account, windows).with_sections(
                (!shares.is_empty())
                    .then(|| Section::Shares {
                        title: "This week by surface".into(),
                        shares,
                    })
                    .into_iter()
                    .chain(spend),
            ),
        )
    }
}

/// `max` on tier `default_claude_max_20x` is `Max 20x`.
pub(super) fn plan(subscription: Option<&str>, tier: Option<&str>) -> Option<String> {
    let name = title_case(subscription?);
    let multiple = tier
        .and_then(|tier| tier.rsplit('_').next())
        .filter(|last| {
            last.strip_suffix('x')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        });
    (!name.is_empty()).then(|| match multiple {
        Some(multiple) => format!("{name} {multiple}"),
        None => name,
    })
}

#[derive(Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OAuth>,
}

#[derive(Deserialize)]
pub(super) struct OAuth {
    #[serde(rename = "accessToken")]
    access_token: Option<SecretString>,
    #[serde(rename = "subscriptionType")]
    subscription: Option<String>,
    #[serde(rename = "rateLimitTier")]
    tier: Option<String>,
}

#[derive(Deserialize)]
struct Profile {
    #[serde(rename = "oauthAccount")]
    account: Option<ProfileAccount>,
}

#[derive(Deserialize)]
struct ProfileAccount {
    #[serde(rename = "emailAddress")]
    email: Option<String>,
}

/// macOS keeps Claude Code's sign-in in the login keychain; elsewhere, and for
/// older installs, it is a file in the config directory.
fn credentials() -> Option<OAuth> {
    let keychain = cfg!(target_os = "macos")
        .then(|| {
            let mut command = Command::new("/usr/bin/security");
            command.args(["find-generic-password", "-s", KEYCHAIN, "-w"]);
            output(
                &mut command,
                KEYCHAIN_TIMEOUT,
                "read the Claude keychain item",
            )
            .ok()
            .filter(|(success, _)| *success)
            .and_then(|(_, bytes)| credential(&bytes))
        })
        .flatten();
    keychain.or_else(|| {
        let directory = agent_home("CLAUDE_CONFIG_DIR", ".claude")?;
        credential(&read_private(&directory.join(".credentials.json"))?)
    })
}

pub(super) fn credential(bytes: &[u8]) -> Option<OAuth> {
    serde_json::from_slice::<Credentials>(bytes)
        .ok()?
        .oauth
        .filter(|oauth| {
            oauth
                .access_token
                .as_ref()
                .is_some_and(|token| !token.expose_secret().trim().is_empty())
        })
}

#[cfg(test)]
impl OAuth {
    pub fn token(&self) -> Option<&str> {
        self.access_token
            .as_ref()
            .map(|token| token.expose_secret())
    }
}

#[derive(Deserialize)]
struct Usage {
    limits: Option<Vec<Limit>>,
    five_hour: Option<FixedWindow>,
    seven_day: Option<FixedWindow>,
    seven_day_breakdown: Option<Breakdown>,
    spend: Option<Spend>,
}

#[derive(Deserialize)]
struct Limit {
    kind: String,
    percent: Option<f64>,
    resets_at: Option<Timestamp>,
    scope: Option<Scope>,
}

#[derive(Deserialize)]
struct Scope {
    model: Option<Model>,
}

#[derive(Deserialize)]
struct Model {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct FixedWindow {
    utilization: Option<f64>,
    resets_at: Option<Timestamp>,
}

#[derive(Deserialize)]
struct Breakdown {
    rows: Vec<BreakdownRow>,
}

#[derive(Deserialize)]
struct BreakdownRow {
    display_name: Option<String>,
    percent: Option<f64>,
}

#[derive(Deserialize)]
struct Spend {
    enabled: Option<bool>,
    used: Option<Money>,
    limit: Option<Money>,
}

#[derive(Deserialize)]
struct Money {
    amount_minor: i64,
    currency: String,
    exponent: u32,
}

impl Money {
    /// `12.34 EUR`, in the currency's own minor units.
    fn text(&self) -> Option<String> {
        let scale = 10_i64.checked_pow(self.exponent.min(6))?;
        let sign = if self.amount_minor < 0 { "-" } else { "" };
        let amount = self.amount_minor.unsigned_abs();
        let scale = scale.unsigned_abs();
        let currency = self.currency.trim();
        Some(if self.exponent == 0 {
            format!("{sign}{amount} {currency}")
        } else {
            format!(
                "{sign}{}.{:0width$} {currency}",
                amount / scale,
                amount % scale,
                width = self.exponent.min(6) as usize
            )
        })
    }
}
