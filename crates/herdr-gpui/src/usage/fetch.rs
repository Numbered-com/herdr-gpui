//! Reading a host's plan usage. Locally, the agents' own saved sign-ins are
//! read and sent straight to their services. A remote host runs one script
//! over SSH that does the same with its own sign-ins and prints only the
//! service responses, so no credential ever leaves the machine it belongs to.
//! Credentials are only read: a stale sign-in is the agent's to refresh.

#[cfg(unix)]
use super::remote;
use super::{
    model::{Host, Provider},
    parse::Raw,
};
use crate::{Error, Result};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};
use zeroize::Zeroizing;

pub(super) const CLAUDE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub(super) const CLAUDE_BETA: &str = "oauth-2025-04-20";
/// The usage endpoint serves Claude Code's OAuth clients.
pub(super) const CLAUDE_AGENT: &str = "claude-code/2.1.0";
pub(super) const CLAUDE_KEYCHAIN: &str = "Claude Code-credentials";
pub(super) const CODEX_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
pub(super) const CODEX_AGENT: &str = "codex-cli";

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(5);
const LIMIT: usize = 256 * 1024;

/// Every provider this host is signed in to, with its service's answer. A
/// provider without a sign-in is absent rather than an error.
pub(super) fn fetch(host: &Host) -> Result<Vec<(Provider, Result<Raw>)>> {
    match host {
        Host::Local => Ok(Provider::ALL
            .into_iter()
            .filter_map(|provider| Some((provider, local(provider)?)))
            .collect()),
        Host::Ssh(target) => Ok(remote::fetch(target)?
            .into_iter()
            .map(|raw| (raw.provider, Ok(raw)))
            .collect()),
    }
}

fn local(provider: Provider) -> Option<Result<Raw>> {
    use ureq::http::HeaderValue;
    Some(match provider {
        Provider::Claude => {
            let token = claude_token()?;
            bearer(&token).and_then(|authorization| {
                get(
                    provider,
                    CLAUDE_URL,
                    vec![
                        ("Authorization", authorization),
                        ("anthropic-beta", HeaderValue::from_static(CLAUDE_BETA)),
                        ("User-Agent", HeaderValue::from_static(CLAUDE_AGENT)),
                    ],
                )
            })
        }
        Provider::Codex => {
            let auth = codex_auth()?;
            codex_headers(&auth).and_then(|headers| get(provider, CODEX_URL, headers))
        }
    })
}

fn codex_headers(auth: &CodexAuth) -> Result<Vec<(&'static str, ureq::http::HeaderValue)>> {
    use ureq::http::HeaderValue;
    let mut headers = vec![
        ("Authorization", bearer(&auth.access_token)?),
        ("User-Agent", HeaderValue::from_static(CODEX_AGENT)),
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

fn bearer(token: &SecretString) -> Result<ureq::http::HeaderValue> {
    let text = Zeroizing::new(format!("Bearer {}", token.expose_secret()));
    let mut header = ureq::http::HeaderValue::from_str(&text).map_err(Error::UsageHeader)?;
    header.set_sensitive(true);
    Ok(header)
}

fn get(
    provider: Provider,
    url: &str,
    headers: Vec<(&'static str, ureq::http::HeaderValue)>,
) -> Result<Raw> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let mut request = agent.get(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let mut response = request.call().map_err(Error::UsageNetwork)?;
    let status = response.status().as_u16();
    let mut body = String::new();
    response
        .body_mut()
        .as_reader()
        .take(LIMIT as u64 + 1)
        .read_to_string(&mut body)
        .map_err(|source| Error::UsageNetwork(ureq::Error::Io(source)))?;
    if body.len() > LIMIT {
        return Err(Error::UsageSize);
    }
    Ok(Raw {
        provider,
        status,
        body,
    })
}

#[derive(Deserialize)]
struct ClaudeCredentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<ClaudeOAuth>,
}

#[derive(Deserialize)]
struct ClaudeOAuth {
    #[serde(rename = "accessToken")]
    access_token: Option<SecretString>,
}

/// macOS keeps Claude Code's sign-in in the login keychain; elsewhere, and for
/// older installs, it is a file in the config directory.
fn claude_token() -> Option<SecretString> {
    let keychain = cfg!(target_os = "macos")
        .then(|| {
            let mut command = Command::new("/usr/bin/security");
            command.args(["find-generic-password", "-s", CLAUDE_KEYCHAIN, "-w"]);
            output(
                &mut command,
                KEYCHAIN_TIMEOUT,
                "read the Claude keychain item",
            )
            .ok()
            .filter(|(success, _)| *success)
            .and_then(|(_, bytes)| claude_credential(&bytes))
        })
        .flatten();
    keychain.or_else(|| {
        let directory = std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| crate::config::home().ok().map(|home| home.join(".claude")))?;
        let bytes = Zeroizing::new(std::fs::read(directory.join(".credentials.json")).ok()?);
        claude_credential(&bytes)
    })
}

pub(super) fn claude_credential(bytes: &[u8]) -> Option<SecretString> {
    serde_json::from_slice::<ClaudeCredentials>(bytes)
        .ok()?
        .oauth?
        .access_token
        .filter(|token| !token.expose_secret().trim().is_empty())
}

#[derive(Deserialize)]
struct CodexAuthFile {
    tokens: Option<CodexAuth>,
}

#[derive(Deserialize)]
pub(super) struct CodexAuth {
    access_token: SecretString,
    account_id: Option<String>,
}

/// An API-key-only Codex install has no `tokens` and no plan usage to show.
fn codex_auth() -> Option<CodexAuth> {
    let home = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| crate::config::home().ok().map(|home| home.join(".codex")))?;
    let bytes = Zeroizing::new(std::fs::read(home.join("auth.json")).ok()?);
    codex_credential(&bytes)
}

pub(super) fn codex_credential(bytes: &[u8]) -> Option<CodexAuth> {
    serde_json::from_slice::<CodexAuthFile>(bytes)
        .ok()?
        .tokens
        .filter(|auth| !auth.access_token.expose_secret().trim().is_empty())
}

/// Runs `command` to completion with a deadline, keeping at most `LIMIT`
/// bytes of its standard output. Standard error is discarded: it may echo
/// what the child was reading.
pub(super) fn output(
    command: &mut Command,
    timeout: Duration,
    operation: &'static str,
) -> Result<(bool, Zeroizing<Vec<u8>>)> {
    let process = |source| Error::UsageProcess { operation, source };
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(process)?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(process(std::io::Error::other("no output pipe")));
    };
    let (sender, reads) = mpsc::sync_channel(1);
    let reader = thread::Builder::new()
        .name("herdr-usage-output".into())
        .spawn(move || {
            let mut bytes = Zeroizing::new(Vec::new());
            let result = stdout
                .take(LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = sender.send(result);
        });
    if let Err(source) = reader {
        let _ = child.kill();
        let _ = child.wait();
        return Err(process(source));
    }
    let result = match reads.recv_timeout(timeout) {
        Ok(Ok(bytes)) if bytes.len() > LIMIT => Err(Error::UsageSize),
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(source)) => Err(process(source)),
        Err(_) => Err(Error::UsageTimeout),
    };
    let bytes = match result {
        Ok(bytes) => bytes,
        Err(error) => {
            // Killing the child closes the pipe, which ends the reader.
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let status = child.wait().map_err(process)?;
    Ok((status.success(), bytes))
}

#[cfg(windows)]
mod remote {
    pub(super) fn fetch(_target: &str) -> crate::Result<Vec<super::Raw>> {
        Err(crate::Error::UsageUnsupported)
    }
}
