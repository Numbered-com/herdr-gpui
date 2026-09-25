//! The remote half of reading usage: one POSIX script run over SSH that reads
//! the host's own agent sign-ins and prints only the services' answers. Each
//! provider contributes its fragment through [`Service::remote`].

use super::{fetch::output, model::Provider, parse::Raw, service::Meta};
use crate::{Error, Result};
use std::time::Duration;

/// SSH connects within 10 seconds and each request is capped at 10 more.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(60);
const MARKER: &str = "\n@@herdr-usage ";
const STATUS: &str = "\n@@herdr-status ";
const MISSING_CURL: &str = "@@herdr-usage-missing-curl";

/// Tokens are read with `sed` so the host needs nothing beyond POSIX tools and
/// curl, and they reach curl as headers on stdin, never in an argument list
/// another user could read.
const PRELUDE: &str = r#"if ! command -v curl >/dev/null 2>&1; then
    printf '%s\n' '@@herdr-usage-missing-curl'
    exit 0
fi
field() { sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" | head -n 1; }
request() {
    printf '\n@@herdr-usage %s %s\n' "$1" "$2"
    shift 2
    curl -sS --max-time 10 -H @- -w '\n@@herdr-status %{http_code}\n' "$@" 2>/dev/null
}
"#;

pub(super) fn script() -> String {
    Provider::ALL
        .into_iter()
        .map(|provider| provider.service().remote())
        .fold(PRELUDE.to_owned(), |script, fragment| script + fragment)
        + "exit 0\n"
}

pub(super) fn fetch(target: &str) -> Result<Vec<Raw>> {
    let mut command = herdr_client::script_command(target, &script())?;
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
            let (header, rest) = section.split_once('\n').unwrap_or((section, ""));
            let (key, meta) = header.split_once(' ').unwrap_or((header, ""));
            let provider = Provider::from_key(key.trim())?;
            let (body, status) = rest
                .rsplit_once(STATUS)
                .map(|(body, status)| (body, status.trim().parse().unwrap_or(0)))
                .unwrap_or((rest, 0));
            Some(Raw {
                provider,
                status,
                body: body.to_owned(),
                meta: Meta::parse(meta, provider.service().meta()),
            })
        })
        .collect())
}

/// The script repeats the endpoints and markers the local path and parser use.
#[cfg(test)]
pub(super) const SHARED_WITH_SCRIPT: [&str; 7] = [
    super::claude::URL,
    super::claude::BETA,
    super::claude::AGENT,
    super::claude::KEYCHAIN,
    super::codex::URL,
    super::codex::AGENT,
    MISSING_CURL,
];
