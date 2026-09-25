//! The remote half of reading usage: one POSIX script run over SSH that reads
//! the host's own agent sign-ins and prints only the services' answers.

use super::{fetch::output, model::Provider, parse::Raw};
use crate::{Error, Result};
use std::time::Duration;

/// SSH connects within 10 seconds and each request is capped at 10 more.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(40);
const MARKER: &str = "\n@@herdr-usage ";
const STATUS: &str = "\n@@herdr-status ";
const MISSING_CURL: &str = "@@herdr-usage-missing-curl";

/// Tokens are read with `sed` so the host needs nothing beyond POSIX tools and
/// curl, and they reach curl as headers on stdin, never in an argument list
/// another user could read.
const SCRIPT: &str = r#"if ! command -v curl >/dev/null 2>&1; then
    printf '%s\n' '@@herdr-usage-missing-curl'
    exit 0
fi
field() { sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" | head -n 1; }
request() {
    printf '\n@@herdr-usage %s\n' "$1"
    shift
    curl -sS --max-time 10 -H @- -w '\n@@herdr-status %{http_code}\n' "$@" 2>/dev/null
}
claude_dir=${CLAUDE_CONFIG_DIR:-$HOME/.claude}
credentials=
if [ -r "$claude_dir/.credentials.json" ]; then
    credentials=$(cat "$claude_dir/.credentials.json")
elif command -v security >/dev/null 2>&1; then
    credentials=$(security find-generic-password -s 'Claude Code-credentials' -w 2>/dev/null)
fi
token=$(printf '%s\n' "$credentials" | field accessToken)
credentials=
if [ -n "$token" ]; then
    printf 'Authorization: Bearer %s\n' "$token" | request claude \
        -H 'anthropic-beta: oauth-2025-04-20' -H 'User-Agent: claude-code/2.1.0' \
        https://api.anthropic.com/api/oauth/usage
fi
codex_home=${CODEX_HOME:-$HOME/.codex}
if [ -r "$codex_home/auth.json" ]; then
    token=$(field access_token < "$codex_home/auth.json")
    account=$(field account_id < "$codex_home/auth.json")
    if [ -n "$token" ]; then
        {
            printf 'Authorization: Bearer %s\n' "$token"
            if [ -n "$account" ]; then printf 'ChatGPT-Account-Id: %s\n' "$account"; fi
        } | request codex -H 'User-Agent: codex-cli' -H 'OpenAI-Beta: codex-1' \
            -H 'originator: Codex Desktop' https://chatgpt.com/backend-api/wham/usage
    fi
fi
exit 0"#;

pub(super) fn fetch(target: &str) -> Result<Vec<Raw>> {
    let mut command = herdr_client::script_command(target, SCRIPT)?;
    let (success, bytes) = output(&mut command, REMOTE_TIMEOUT, "run ssh")?;
    let text = String::from_utf8_lossy(&bytes);
    let raws = sections(&text)?;
    // ssh exits 255 when it cannot connect; the script itself always exits 0.
    if !success && raws.is_empty() {
        return Err(Error::UsageUnreachable);
    }
    Ok(raws)
}

/// Splits the script's output into one answer per provider. Anything a login
/// shell printed before the first marker is ignored.
pub(super) fn sections(output: &str) -> Result<Vec<Raw>> {
    if output.lines().any(|line| line == MISSING_CURL) {
        return Err(Error::UsageMissingCurl);
    }
    Ok(output
        .split(MARKER)
        .skip(1)
        .filter_map(|section| {
            let (key, rest) = section.split_once('\n').unwrap_or((section, ""));
            let provider = Provider::from_key(key.trim())?;
            let (body, status) = rest
                .rsplit_once(STATUS)
                .map(|(body, status)| (body, status.trim().parse().unwrap_or(0)))
                .unwrap_or((rest, 0));
            Some(Raw {
                provider,
                status,
                body: body.to_owned(),
            })
        })
        .collect())
}

#[cfg(test)]
pub(super) const SCRIPT_FOR_TESTS: &str = SCRIPT;

/// The script repeats the endpoints and markers the local path and parser use.
#[cfg(test)]
pub(super) const SHARED_WITH_SCRIPT: [&str; 7] = [
    super::fetch::CLAUDE_URL,
    super::fetch::CLAUDE_BETA,
    super::fetch::CLAUDE_AGENT,
    super::fetch::CLAUDE_KEYCHAIN,
    super::fetch::CODEX_URL,
    super::fetch::CODEX_AGENT,
    MISSING_CURL,
];
