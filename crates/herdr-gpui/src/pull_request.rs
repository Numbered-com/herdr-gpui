//! Read-only PR prefetch. One worker, bounded memory, and no UI-thread I/O.
use serde::Deserialize;
use std::{
    io::Read,
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);
const QUERY: &str = r#"query($owner: String!, $repo: String!, $branch: String!) {
  repository(owner: $owner, name: $repo) {
    pullRequests(first: 2, headRefName: $branch, orderBy: {field: UPDATED_AT, direction: DESC}) {
      nodes {
        number url title state isDraft headRefName baseRefName additions deletions
        changedFiles updatedAt mergeStateStatus reviewDecision headRepositoryOwner { login }
        commits(last: 1) { nodes { commit { statusCheckRollup {
          contexts(first: 100) {
            pageInfo { hasNextPage }
            nodes { __typename ... on CheckRun { status conclusion } ... on StatusContext { state } }
          }
        } } } }
      }
    }
  }
}"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Input {
    pub checkout: Option<String>,
    pub repo_key: String,
    pub branch: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PullRequest {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub state: String,
    pub is_draft: bool,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub updated_at: String,
    pub merge_state_status: String,
    pub review_decision: String,
    #[serde(skip)]
    pub checks_summary: String,
    #[serde(default)]
    status_check_rollup: Option<Vec<Check>>,
    head_repository_owner: Owner,
}

#[derive(Clone, Debug, Deserialize)]
struct Owner {
    login: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Check {
    #[serde(rename = "__typename")]
    kind: String,
    state: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
}

impl PullRequest {
    pub fn lifecycle(&self) -> &'static str {
        match self.state.as_str() {
            "MERGED" => "Merged",
            "CLOSED" => "Closed",
            _ if self.is_draft => "Draft",
            _ => "Open",
        }
    }

    pub fn checks(&self) -> String {
        let mut counts = [0; 4];
        for check in self.status_check_rollup.iter().flatten() {
            let state = if check.kind == "StatusContext" {
                check.state.as_deref()
            } else if check.status.as_deref() == Some("COMPLETED") {
                check.conclusion.as_deref()
            } else {
                None
            };
            counts[match state {
                Some("SUCCESS") => 0,
                Some(
                    "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED"
                    | "STARTUP_FAILURE" | "STALE",
                ) => 1,
                Some("NEUTRAL" | "SKIPPED") => 3,
                _ => 2,
            }] += 1;
        }
        if counts == [0; 4] {
            return "No checks reported".into();
        }
        counts
            .into_iter()
            .zip(["passed", "failed", "pending", "skipped"])
            .filter(|(count, _)| *count > 0)
            .map(|(count, label)| format!("{count} {label}"))
            .collect::<Vec<_>>()
            .join(" / ")
    }

    pub fn merge_status(&self) -> &'static str {
        match self.merge_state_status.as_str() {
            "CLEAN" => "No merge conflicts",
            "DIRTY" => "Merge conflicts",
            "BEHIND" => "Branch behind base",
            "BLOCKED" => "Merge blocked",
            "UNSTABLE" => "Checks need attention",
            "DRAFT" => "Not ready for review",
            "HAS_HOOKS" => "Merge hooks required",
            _ => "Merge status unavailable",
        }
    }

    pub fn review(&self) -> &'static str {
        match self.review_decision.as_str() {
            "APPROVED" => "Approved",
            "CHANGES_REQUESTED" => "Changes requested",
            "REVIEW_REQUIRED" => "Review required",
            _ => "No review decision",
        }
    }
}

pub(super) fn clean(text: &str) -> String {
    text.chars()
        .take(512)
        .map(|c| {
            if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

type Result = crate::Result<Option<PullRequest>>;
use crate::Error;

const CACHE_LIMIT: usize = 128;
const REFRESH: Duration = Duration::from_secs(90);
const ERROR_BACKOFF: Duration = Duration::from_secs(300);

struct Entry {
    input: Input,
    value: Option<PullRequest>,
    message: Option<String>,
    checked: Instant,
    due: Instant,
    used: Instant,
}

/// The scope includes selection epoch, connection generation and daemon boot.
/// Token allocation identity is an additional auth generation, never token text.
#[derive(Default)]
pub(super) struct Cache {
    lookup: Lookup,
    entries: Vec<Entry>,
    queue: std::collections::VecDeque<Input>,
    active: Option<Input>,
    scope: Option<(u64, u64, String)>,
    token: Option<Arc<secrecy::SecretString>>,
    next_scan: Option<Instant>,
    paused_until: Option<Instant>,
    pub cursor: usize,
}

impl Cache {
    #[cfg(any(test, feature = "integration-test"))]
    pub fn seed(&mut self, input: Input, value: PullRequest, now: Instant) {
        self.entries.retain(|entry| entry.input != input);
        if self.entries.len() == CACHE_LIMIT {
            self.entries.remove(0);
        }
        self.entries.push(Entry {
            input,
            value: Some(value),
            message: None,
            checked: now,
            due: now + REFRESH,
            used: now,
        });
    }

    pub fn clear(&mut self) {
        self.lookup.clear();
        self.entries.clear();
        self.queue.clear();
        self.active = None;
        self.scope = None;
        self.token = None;
        self.next_scan = None;
        self.paused_until = None;
        self.cursor = 0;
    }

    pub fn scope(&mut self, scope: (u64, u64, String), token: Arc<secrecy::SecretString>) {
        if self.scope.as_ref() != Some(&scope)
            || self
                .token
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, &token))
        {
            self.clear();
            self.scope = Some(scope);
            self.token = Some(token);
        }
    }

    pub fn retain(&mut self, current: impl Fn(&Input) -> bool) {
        self.entries.retain(|entry| current(&entry.input));
        self.queue.retain(&current);
        if self.active.as_ref().is_some_and(|input| !current(input)) {
            self.lookup.clear();
            self.active = None;
        }
    }

    pub fn scan_due(&self, now: Instant) -> bool {
        self.next_scan.is_none_or(|next| now >= next)
    }

    pub fn schedule(&mut self, inputs: impl IntoIterator<Item = Input>, now: Instant) {
        self.next_scan = Some(now + Duration::from_secs(1));
        // Replace queued metadata, not an unbounded history of snapshot changes.
        self.queue.clear();
        for input in inputs {
            if self.queue.len() == CACHE_LIMIT {
                break;
            }
            if self.active.as_ref() != Some(&input)
                && !self.queue.contains(&input)
                && self
                    .entries
                    .iter()
                    .find(|entry| entry.input == input)
                    .is_none_or(|entry| now >= entry.due)
            {
                self.queue.push_back(input);
            }
        }
    }

    pub fn poll(&mut self, now: Instant) -> bool {
        let mut changed = self.lookup.poll();
        if !self.lookup.loading
            && let Some(input) = self.active.take()
        {
            let failed = self.lookup.message.is_some();
            // Rate/auth failures pause the account; a bad local repo must not
            // prevent the remaining workspaces from being prefetched.
            if let Some(cooldown) = self.lookup.cooldown.take() {
                self.paused_until = Some(now + cooldown);
            }
            if let Some(entry) = self.entries.iter_mut().find(|entry| entry.input == input) {
                if !failed {
                    entry.value = self.lookup.value.take();
                }
                entry.message = self.lookup.message.take();
                entry.checked = now;
                entry.due = now + if failed { ERROR_BACKOFF } else { REFRESH };
            } else {
                if self.entries.len() == CACHE_LIMIT
                    && let Some((index, _)) = self
                        .entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| now >= e.due)
                        .min_by_key(|(_, e)| e.used)
                {
                    self.entries.remove(index);
                }
                self.entries.push(Entry {
                    input,
                    value: self.lookup.value.take(),
                    message: self.lookup.message.take(),
                    checked: now,
                    due: now + if failed { ERROR_BACKOFF } else { REFRESH },
                    used: now,
                });
            }
            changed = true;
        }
        if self.active.is_none()
            && self.paused_until.is_none_or(|until| now >= until)
            && let Some(token) = &self.token
        {
            while let Some(input) = self.queue.pop_front() {
                let existing = self.entries.iter().find(|entry| entry.input == input);
                if existing.is_some_and(|entry| now < entry.due)
                    || (existing.is_none()
                        && self.entries.len() == CACHE_LIMIT
                        && self.entries.iter().all(|entry| now < entry.due))
                {
                    continue;
                }
                self.lookup.clear();
                self.lookup.request(input.clone(), token.clone());
                self.active = Some(input);
                self.cursor = self.cursor.wrapping_add(1);
                self.lookup.poll();
                changed = true;
                break;
            }
        }
        changed
    }

    /// Pure cache read: opening a menu cannot launch Git, HTTPS or daemon requests.
    pub fn present(&mut self, input: &Input, view: &mut Lookup, now: Instant) {
        view.clear();
        if let Some(entry) = self.entries.iter_mut().find(|entry| &entry.input == input) {
            entry.used = now;
            view.value = entry.value.clone();
            view.message = entry.message.clone();
            view.checked = Some(entry.checked);
        } else {
            if self.paused_until.is_some_and(|until| now < until) {
                view.message = Some("GitHub requests paused after an authentication or rate-limit error; retrying automatically.".into());
            } else {
                view.loading = true;
            }
        }
        if view.value.is_some() && view.message.is_some() {
            view.message = Some("Refresh unavailable; retrying automatically.".into());
        }
    }
}

struct Worker {
    requests: mpsc::SyncSender<(u64, Input, Arc<secrecy::SecretString>)>,
    results: mpsc::Receiver<(u64, Result, Option<Duration>)>,
}

#[derive(Default)]
pub(super) struct Lookup {
    worker: Option<Worker>,
    generation: Arc<AtomicU64>,
    busy: bool,
    waiting: Option<(Input, Arc<secrecy::SecretString>)>,
    pub loading: bool,
    pub value: Option<PullRequest>,
    pub message: Option<String>,
    pub checked: Option<Instant>,
    cooldown: Option<Duration>,
}

impl Drop for Lookup {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

impl Lookup {
    pub fn clear(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        // Sign-out also drains already-queued private results without scheduling
        // more work. A still-running cancelled request is drained on a later tick.
        if let Some(worker) = &self.worker
            && worker.results.try_recv().is_ok()
        {
            self.busy = false;
        }
        self.waiting = None;
        self.loading = false;
        self.value = None;
        self.message = None;
        self.checked = None;
        self.cooldown = None;
    }

    pub fn request(&mut self, input: Input, token: Arc<secrecy::SecretString>) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.waiting = Some((input, token));
        self.loading = true;
        self.message = None;
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(worker) = &self.worker {
            match worker.results.try_recv() {
                Ok((generation, result, cooldown)) => {
                    self.busy = false;
                    if generation == self.generation.load(Ordering::Relaxed) {
                        self.loading = false;
                        self.checked = Some(Instant::now());
                        self.cooldown = cooldown;
                        match result {
                            Ok(value) => {
                                self.value = value;
                                self.message = None;
                            }
                            Err(error) => self.message = Some(error.to_string()),
                        }
                        changed = true;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.busy = false;
                    self.loading = false;
                    self.waiting = None;
                    self.message = Some("PR worker stopped; retrying automatically.".into());
                    self.worker = None;
                    return true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if !self.busy
            && let Some((input, token)) = self.waiting.take()
        {
            if self.worker.is_none() {
                let (requests, incoming) =
                    mpsc::sync_channel::<(u64, Input, Arc<secrecy::SecretString>)>(1);
                let (outgoing, results) = mpsc::sync_channel(1);
                let current = self.generation.clone();
                match thread::Builder::new()
                    .name("herdr-pr".into())
                    .spawn(move || {
                        for (generation, input, token) in incoming {
                            let mut cooldown = None;
                            let result = fetch_with_backoff(
                                &input,
                                &token,
                                || current.load(Ordering::Relaxed) != generation,
                                &mut cooldown,
                            );
                            if outgoing.send((generation, result, cooldown)).is_err() {
                                break;
                            }
                        }
                    }) {
                    Ok(_) => self.worker = Some(Worker { requests, results }),
                    Err(_) => {
                        self.loading = false;
                        self.message = Some("Could not start PR worker.".into());
                        return true;
                    }
                }
            }
            if let Some(worker) = &self.worker {
                self.busy = worker
                    .requests
                    .try_send((self.generation.load(Ordering::Relaxed), input, token))
                    .is_ok();
            }
        }
        changed
    }
}

#[cfg(test)]
fn fetch(input: &Input, token: &secrecy::SecretString, cancelled: impl Fn() -> bool) -> Result {
    fetch_with_backoff(input, token, cancelled, &mut None)
}

fn fetch_with_backoff(
    input: &Input,
    token: &secrecy::SecretString,
    cancelled: impl Fn() -> bool,
    cooldown: &mut Option<Duration>,
) -> Result {
    let deadline = Instant::now() + TIMEOUT;
    let (owner, repo) = local_repository(input, deadline, &cancelled)?;
    let timeout = deadline
        .checked_duration_since(Instant::now())
        .ok_or(Error::PrTimeout)?;
    let response = crate::github::graphql(
        token,
        QUERY,
        serde_json::json!({"owner":owner,"repo":repo,"branch":input.branch}),
        timeout,
        cancelled,
        cooldown,
    )?;
    parse_graphql(response, &owner, &repo, &input.branch)
}

pub(super) fn local_repository(
    input: &Input,
    deadline: Instant,
    cancelled: &impl Fn() -> bool,
) -> crate::Result<(String, String)> {
    if input
        .checkout
        .as_ref()
        .is_some_and(|path| !Path::new(path).is_absolute())
        || !Path::new(&input.repo_key).is_absolute()
    {
        return Err(Error::PrAbsolutePath);
    }
    if input.branch.is_empty()
        || input.branch.len() > 1024
        || input.branch.chars().any(char::is_control)
    {
        return Err(Error::PrBranch);
    }
    let checkout = match &input.checkout {
        Some(path) => path.clone(),
        None => {
            // Older daemons lack workspace.get. Use Git's own worktree registry,
            // never pane cwd or the daemon's new-workspace directory policy.
            let mut command = Command::new("git");
            command.args([
                "-c",
                "core.fsmonitor=false",
                "--git-dir",
                &input.repo_key,
                "worktree",
                "list",
                "--porcelain",
                "-z",
            ]);
            let (ok, output) = run(&mut command, deadline, cancelled)?;
            if !ok {
                return Err(Error::PrWorktreeLookup);
            }
            worktree_checkout(&output, &input.branch)?
        }
    };
    let git = |args: &[&str]| {
        let mut command = Command::new("git");
        command
            .args(["-c", "core.fsmonitor=false", "-C", &checkout])
            .args(args);
        run(&mut command, deadline, cancelled).and_then(|(ok, output)| {
            if ok {
                Ok(output.trim_end_matches(['\r', '\n']).to_owned())
            } else {
                Err(Error::PrCheckout)
            }
        })
    };
    // A Git registry candidate still must match both repository and live HEAD.
    let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    if Path::new(&common)
        .canonicalize()
        .ok()
        .zip(Path::new(&input.repo_key).canonicalize().ok())
        .is_none_or(|(actual, expected)| actual != expected)
    {
        return Err(Error::PrRepositoryMismatch);
    }
    if git(&["symbolic-ref", "--quiet", "--short", "HEAD"])? != input.branch {
        return Err(Error::PrBranchChanged);
    }
    let remote = git(&["config", "--get", "remote.origin.url"])?;
    crate::avatars::github_repo(&remote).ok_or(Error::PrOrigin)
}

fn worktree_checkout(output: &str, branch: &str) -> crate::Result<String> {
    let branch = format!("branch refs/heads/{branch}");
    let mut paths = output.split("\0\0").filter_map(|record| {
        let mut fields = record.split('\0');
        let path = fields.next()?.strip_prefix("worktree ")?;
        (Path::new(path).is_absolute() && fields.any(|field| field == branch)).then_some(path)
    });
    let path = paths.next().ok_or(Error::PrMissingWorktree)?;
    if paths.next().is_some() {
        return Err(Error::PrAmbiguousWorktree);
    }
    Ok(path.into())
}

fn parse_graphql(response: serde_json::Value, owner: &str, repo: &str, branch: &str) -> Result {
    let mut nodes = response["data"]["repository"]["pullRequests"]["nodes"]
        .as_array()
        .ok_or(Error::PrRepository)?
        .clone();
    let mut incomplete = false;
    for pr in &mut nodes {
        let contexts = &pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"];
        incomplete |= contexts["pageInfo"]["hasNextPage"] == true;
        let checks = contexts["nodes"].clone();
        pr["statusCheckRollup"] = checks;
        if pr["reviewDecision"].is_null() {
            pr["reviewDecision"] = "".into();
        }
    }
    let mut result = parse(
        &serde_json::to_string(&nodes).map_err(Error::github_json)?,
        owner,
        repo,
        branch,
    )?;
    if incomplete && let Some(pr) = &mut result {
        pr.checks_summary
            .push_str(" (first 100; more checks exist)");
    }
    Ok(result)
}

fn parse(text: &str, owner: &str, repo: &str, branch: &str) -> Result {
    if text.len() > OUTPUT_LIMIT {
        return Err(Error::PrSize);
    }
    let mut values: Vec<PullRequest> = serde_json::from_str(text).map_err(Error::github_json)?;
    if values.is_empty() {
        return Ok(None);
    }
    if values.len() != 1 {
        return Err(Error::PrAmbiguous);
    }
    let Some(mut pr) = values.pop() else {
        return Ok(None);
    };
    let expected = format!("https://github.com/{owner}/{repo}/pull/{}", pr.number);
    if pr.number == 0
        || !matches!(pr.state.as_str(), "OPEN" | "CLOSED" | "MERGED")
        || !pr.url.eq_ignore_ascii_case(&expected)
        || pr.head_ref_name != branch
        || !pr.head_repository_owner.login.eq_ignore_ascii_case(owner)
    {
        return Err(Error::PrIdentity);
    }
    pr.url = expected;
    pr.title = clean(&pr.title);
    pr.head_ref_name = clean(&pr.head_ref_name);
    pr.base_ref_name = clean(&pr.base_ref_name);
    pr.updated_at = clean(&pr.updated_at);
    pr.merge_state_status = clean(&pr.merge_state_status);
    pr.checks_summary = pr.checks();
    Ok(Some(pr))
}

// Nonblocking sockets avoid reader threads that can hang on inherited pipe handles.
// Kill/wait only the exact child we created, and never on the UI thread.
fn run(
    command: &mut Command,
    deadline: Instant,
    cancelled: &impl Fn() -> bool,
) -> crate::Result<(bool, String)> {
    if cancelled() {
        return Err(Error::PrCancelled);
    }
    let (mut reader, writer) = UnixStream::pair().map_err(|source| Error::PrProcess {
        operation: "create process output channel",
        source,
    })?;
    reader
        .set_nonblocking(true)
        .map_err(|source| Error::PrProcess {
            operation: "configure process output",
            source,
        })?;
    let error_writer = writer.try_clone().map_err(|source| Error::PrProcess {
        operation: "configure process errors",
        source,
    })?;
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir("/")
        .env_remove("GH_REPO")
        .env_remove("GH_DEBUG")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env("GH_HOST", "github.com")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::from(OwnedFd::from(error_writer)));
    let mut child = command.spawn().map_err(|source| Error::PrProcess {
        operation: "launch Git (install git on PATH)",
        source,
    })?;
    // Command retains Stdio descriptors after spawn; release them so EOF is observable.
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let result = (|| {
        let mut output = Vec::new();
        let mut buffer = [0; 8192];
        let mut eof = false;
        loop {
            if cancelled() {
                return Err(Error::PrCancelled);
            }
            if Instant::now() >= deadline {
                return Err(Error::PrTimeout);
            }
            match reader.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(n) => {
                    if output.len() + n > OUTPUT_LIMIT {
                        return Err(Error::PrSize);
                    }
                    output.extend_from_slice(&buffer[..n]);
                    continue;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(source) => {
                    return Err(Error::PrProcess {
                        operation: "read process output",
                        source,
                    });
                }
            }
            if let Some(status) = child.try_wait().map_err(|source| Error::PrProcess {
                operation: "wait for process",
                source,
            })? && eof
            {
                return String::from_utf8(output)
                    .map(|text| (status.success(), text))
                    .map_err(|error| Error::PrEncoding(error.utf8_error()));
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}

#[cfg(any(test, feature = "integration-test"))]
pub(super) fn fixture() -> crate::Result<PullRequest> {
    parse(&serde_json::json!([{
        "number": 8, "url": "https://github.com/example/project/pull/8",
        "title": "Improve workspace context menus with a deliberately long PR title for narrow native layouts",
        "state": "OPEN", "isDraft": false, "headRefName": "feature", "baseRefName": "main",
        "additions": 1730, "deletions": 31, "changedFiles": 16,
        "updatedAt": "2026-09-20T12:00:00Z", "mergeStateStatus": "BLOCKED", "reviewDecision": "REVIEW_REQUIRED",
        "headRepositoryOwner": {"login": "example"},
        "statusCheckRollup": [
            {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SUCCESS"},
            {"__typename": "StatusContext", "state": "FAILURE"},
            {"__typename": "CheckRun", "status": "IN_PROGRESS", "conclusion": null}
        ]
    }]).to_string(), "example", "project", "feature")?.ok_or(Error::PrRepository)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    struct Peer {
        cache: Cache,
        incoming: mpsc::Receiver<(u64, Input, Arc<secrecy::SecretString>)>,
        outgoing: mpsc::SyncSender<(u64, Result, Option<Duration>)>,
    }

    impl Peer {
        fn new() -> Self {
            let (requests, incoming) = mpsc::sync_channel(1);
            let (outgoing, results) = mpsc::sync_channel(1);
            let mut cache = Cache::default();
            cache.lookup.worker = Some(Worker { requests, results });
            cache.scope((0, 1, "boot".into()), Arc::new("fixture".into()));
            Self {
                cache,
                incoming,
                outgoing,
            }
        }

        fn complete(&mut self, now: Instant, result: Result, cooldown: Option<Duration>) -> Input {
            let (generation, input, _) = self.incoming.try_recv().unwrap();
            self.outgoing.send((generation, result, cooldown)).unwrap();
            self.cache.poll(now);
            input
        }
    }

    fn input(branch: &str) -> Input {
        Input {
            checkout: None,
            repo_key: "/repo/.git".into(),
            branch: branch.into(),
        }
    }

    #[test]
    fn cache_prefetches_without_menu_and_refreshes_at_ttl_with_stale_data() {
        let mut peer = Peer::new();
        let now = Instant::now();
        let input = input("feature");
        peer.cache.schedule([input.clone(), input.clone()], now);
        assert_eq!(peer.cache.queue.len(), 1);
        peer.cache.poll(now);
        assert_eq!(
            peer.complete(now, Ok(Some(fixture().unwrap())), None),
            input
        );
        let mut view = Lookup::default();
        peer.cache.present(&input, &mut view, now);
        assert_eq!(view.value.as_ref().unwrap().number, 8);
        assert!(!view.loading);
        assert!(peer.incoming.try_recv().is_err(), "menu read is I/O free");
        peer.cache
            .schedule([input.clone()], now + REFRESH - Duration::from_secs(1));
        peer.cache.poll(now + REFRESH - Duration::from_secs(1));
        assert!(peer.incoming.try_recv().is_err());
        let due = now + REFRESH;
        peer.cache.schedule([input.clone()], due);
        peer.cache.poll(due);
        peer.cache.present(&input, &mut view, due);
        assert!(
            view.value.is_some(),
            "refresh does not replace cached data with loading"
        );
        peer.complete(
            due,
            Err(std::io::Error::other("network unavailable").into()),
            None,
        );
        peer.cache.present(&input, &mut view, due);
        assert!(view.value.is_some());
        assert!(view.message.is_some());
        peer.cache.schedule(
            [input.clone()],
            due + ERROR_BACKOFF - Duration::from_secs(1),
        );
        assert!(peer.cache.queue.is_empty());
        peer.cache.schedule([input.clone()], due + ERROR_BACKOFF);
        peer.cache.poll(due + ERROR_BACKOFF);
        peer.complete(due + ERROR_BACKOFF, Ok(None), None);
        peer.cache.present(&input, &mut view, due + ERROR_BACKOFF);
        assert!(view.value.is_none() && view.message.is_none() && !view.loading);
        peer.cache.schedule([input], due + ERROR_BACKOFF);
        assert!(
            peer.cache.queue.is_empty(),
            "negative results also have a TTL"
        );
    }

    #[test]
    fn cache_fences_auth_scope_removed_branch_and_late_results() {
        let now = Instant::now();
        for change in 0..6 {
            let mut peer = Peer::new();
            peer.cache.seed(input("cached"), fixture().unwrap(), now);
            peer.cache.schedule([input("old")], now);
            peer.cache.poll(now);
            let (generation, _, _) = peer.incoming.try_recv().unwrap();
            let token = peer.cache.token.as_ref().unwrap().clone();
            match change {
                0 => peer.cache.clear(), // sign-out/disconnect
                1 => peer
                    .cache
                    .scope((0, 1, "boot".into()), Arc::new("other-account".into())),
                2 => peer.cache.scope((1, 1, "boot".into()), token),
                3 => peer.cache.scope((0, 2, "boot".into()), token),
                4 => peer.cache.scope((0, 1, "new-boot".into()), token),
                _ => peer.cache.retain(|input| input.branch == "new"),
            }
            assert!(peer.cache.entries.is_empty());
            assert!(peer.cache.queue.is_empty());
            peer.outgoing
                .send((generation, Ok(Some(fixture().unwrap())), None))
                .unwrap();
            peer.cache.poll(now);
            assert!(
                peer.cache.entries.is_empty(),
                "late result restored sensitive data: {change}"
            );
            assert!(peer.incoming.try_recv().is_err());
        }
    }

    #[test]
    fn signout_drains_private_results_without_starting_queued_work() {
        let mut peer = Peer::new();
        let now = Instant::now();
        peer.cache.schedule([input("active"), input("queued")], now);
        peer.cache.poll(now);
        let (generation, _, _) = peer.incoming.try_recv().unwrap();
        peer.outgoing
            .send((generation, Ok(Some(fixture().unwrap())), None))
            .unwrap();
        peer.cache.clear();
        assert!(!peer.cache.lookup.busy);
        assert!(
            peer.cache
                .lookup
                .worker
                .as_ref()
                .unwrap()
                .results
                .try_recv()
                .is_err()
        );
        assert!(peer.incoming.try_recv().is_err());
        assert!(peer.cache.token.is_none());
    }

    #[test]
    fn cache_is_bounded_lru_and_does_not_refetch_fresh_entries_under_pressure() {
        let mut peer = Peer::new();
        let now = Instant::now();
        peer.cache
            .schedule((0..CACHE_LIMIT * 2).map(|i| input(&i.to_string())), now);
        assert_eq!(peer.cache.queue.len(), CACHE_LIMIT);
        peer.cache.poll(now);
        for _ in 0..CACHE_LIMIT {
            peer.complete(now, Ok(None), None);
        }
        assert_eq!(peer.cache.entries.len(), CACHE_LIMIT);
        assert!(peer.incoming.try_recv().is_err());
        peer.cache.schedule([input("overflow")], now);
        peer.cache.poll(now);
        assert!(
            peer.incoming.try_recv().is_err(),
            "do not evict fresh data to hammer GitHub"
        );
        let mut view = Lookup::default();
        peer.cache
            .present(&input("0"), &mut view, now + Duration::from_secs(1));
        peer.cache.schedule([input("overflow")], now + REFRESH);
        peer.cache.poll(now + REFRESH);
        peer.complete(now + REFRESH, Ok(None), None);
        assert_eq!(peer.cache.entries.len(), CACHE_LIMIT);
        assert!(peer.cache.entries.iter().any(|e| e.input.branch == "0"));
        assert!(!peer.cache.entries.iter().any(|e| e.input.branch == "1"));
        assert!(
            peer.cache
                .entries
                .iter()
                .any(|e| e.input.branch == "overflow")
        );
    }

    #[test]
    fn local_failures_do_not_starve_other_repos_but_rate_limits_pause_account() {
        let mut peer = Peer::new();
        let now = Instant::now();
        peer.cache.schedule(
            [input("local-error"), input("limited"), input("waiting")],
            now,
        );
        peer.cache.poll(now);
        assert_eq!(
            peer.complete(now, Err(Error::PrOrigin), None).branch,
            "local-error"
        );
        assert_eq!(
            peer.complete(
                now,
                Err(Error::GitHubRateLimit),
                Some(Duration::from_secs(3600))
            )
            .branch,
            "limited"
        );
        peer.cache.poll(now + Duration::from_secs(3599));
        assert!(peer.incoming.try_recv().is_err());
        let mut view = Lookup::default();
        peer.cache.present(&input("waiting"), &mut view, now);
        assert!(view.message.as_deref().unwrap().contains("paused"));
        peer.cache.poll(now + Duration::from_secs(3600));
        assert_eq!(
            peer.complete(now + Duration::from_secs(3600), Ok(None), None)
                .branch,
            "waiting"
        );
    }

    #[test]
    fn parses_identity_lifecycle_and_check_categories() {
        let mut pr = fixture().unwrap();
        assert_eq!(pr.checks(), "1 passed / 1 failed / 1 pending");
        assert_eq!(pr.lifecycle(), "Open");
        assert_eq!(pr.review(), "Review required");
        pr.is_draft = true;
        assert_eq!(pr.lifecycle(), "Draft");
        pr.state = "MERGED".into();
        assert_eq!(pr.lifecycle(), "Merged");
        pr.state = "CLOSED".into();
        assert_eq!(pr.lifecycle(), "Closed");
        pr.status_check_rollup = Some(vec![]);
        assert_eq!(pr.checks(), "No checks reported");
        pr.status_check_rollup = serde_json::from_value(serde_json::json!([
            {"__typename":"CheckRun", "status":"COMPLETED", "conclusion":"SKIPPED"},
            {"__typename":"CheckRun", "status":"COMPLETED", "conclusion":"NEUTRAL"},
            {"__typename":"CheckRun", "status":"COMPLETED", "conclusion":"TIMED_OUT"},
            {"__typename":"CheckRun", "status":"COMPLETED", "conclusion":null}
        ]))
        .unwrap();
        assert_eq!(pr.checks(), "1 failed / 1 pending / 2 skipped");
        for (state, label) in [
            ("CLEAN", "No merge conflicts"),
            ("DIRTY", "Merge conflicts"),
            ("BEHIND", "Branch behind base"),
            ("BLOCKED", "Merge blocked"),
            ("UNSTABLE", "Checks need attention"),
            ("DRAFT", "Not ready for review"),
            ("HAS_HOOKS", "Merge hooks required"),
            ("UNKNOWN", "Merge status unavailable"),
            ("FUTURE_VALUE", "Merge status unavailable"),
        ] {
            pr.merge_state_status = state.into();
            assert_eq!(pr.merge_status(), label);
        }
    }

    fn response() -> serde_json::Value {
        serde_json::json!([{
            "number":8, "url":"https://github.com/example/project/pull/8", "title":"Title\n\u{202e}safe",
            "state":"OPEN", "isDraft":false, "headRefName":"feature", "baseRefName":"main",
            "additions":1, "deletions":0, "changedFiles":1, "updatedAt":"now",
            "mergeStateStatus":"UNKNOWN", "reviewDecision":"", "headRepositoryOwner":{"login":"example"}
        }])
    }

    #[test]
    fn native_graphql_normalizes_nullable_reviews_and_bounded_checks() {
        let mut pr = response()[0].clone();
        pr["reviewDecision"] = serde_json::Value::Null;
        pr["commits"] = serde_json::json!({"nodes":[{"commit":{"statusCheckRollup":{"contexts":{
            "pageInfo":{"hasNextPage":true}, "nodes":[{"__typename":"StatusContext","state":"SUCCESS"}]
        }}}}]});
        let response = serde_json::json!({"data":{"repository":{"pullRequests":{"nodes":[pr]}}}});
        let pr = parse_graphql(response.clone(), "example", "project", "feature")
            .unwrap()
            .unwrap();
        assert_eq!(pr.review(), "No review decision");
        assert!(pr.checks_summary.contains("1 passed"));
        assert!(pr.checks_summary.contains("first 100"));
        assert!(parse_graphql(response, "wrong", "project", "feature").is_err());
        assert!(
            parse_graphql(
                serde_json::json!({"data":{"repository":null}}),
                "a",
                "b",
                "c"
            )
            .is_err()
        );
        assert!(
            parse_graphql(
                serde_json::json!({"data":{"repository":{"pullRequests":{"nodes":[]}}}}),
                "a",
                "b",
                "c"
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn rejects_malformed_ambiguous_oversized_and_mismatched_responses() {
        let parse_value =
            |v: &serde_json::Value| parse(&v.to_string(), "example", "project", "feature");
        assert!(
            parse("[]", "example", "project", "feature")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            parse_value(&response()).unwrap().unwrap().title,
            "Title  safe"
        );
        for (key, value) in [
            (
                "url",
                serde_json::json!("https://github.com.evil.test/example/project/pull/8"),
            ),
            (
                "url",
                serde_json::json!("https://github.com/example/project/pull/9"),
            ),
            (
                "url",
                serde_json::json!("http://github.com/example/project/pull/8"),
            ),
            ("headRefName", serde_json::json!("other")),
            ("headRepositoryOwner", serde_json::json!({"login":"fork"})),
            ("number", serde_json::json!(0)),
            ("additions", serde_json::json!(-1)),
            ("state", serde_json::json!("UNKNOWN")),
            ("isDraft", serde_json::Value::Null),
        ] {
            let mut v = response();
            v[0][key] = value;
            assert!(parse_value(&v).is_err(), "{key}");
        }
        let v = response();
        assert!(parse_value(&serde_json::json!([v[0], v[0]])).is_err());
        assert!(parse("{}", "a", "b", "c").is_err());
        assert!(parse(&" ".repeat(OUTPUT_LIMIT + 1), "a", "b", "c").is_err());
        assert_eq!(clean(&"x".repeat(2000)).len(), 512);
    }

    #[test]
    fn github_origins_are_strictly_validated() {
        assert_eq!(
            crate::avatars::github_repo("git@github.com:Some-Owner/repo.git"),
            Some(("some-owner".into(), "repo".into()))
        );
        for remote in [
            "https://github.com/a/b/c",
            "https://github.com@evil.test/a/b",
            "https://other.test/a/b",
            "https://github.com/a/b?x",
        ] {
            assert!(crate::avatars::github_repo(remote).is_none());
        }
    }

    #[test]
    fn worker_discards_stale_results_and_runs_only_requested_jobs() {
        let (requests, incoming) = mpsc::sync_channel(1);
        let (outgoing, results) = mpsc::sync_channel(1);
        let mut lookup = Lookup::default();
        lookup.worker = Some(Worker { requests, results });
        let input = Input {
            checkout: Some("/fixture".into()),
            repo_key: "/fixture/.git".into(),
            branch: "feature".into(),
        };
        lookup.request(input.clone(), Arc::new("fixture".into()));
        lookup.poll();
        let (old, _, _) = incoming.try_recv().unwrap();
        lookup.clear();
        lookup.request(input, Arc::new("fixture".into()));
        lookup.poll();
        assert!(incoming.try_recv().is_err(), "single in-flight request");
        outgoing
            .send((old, Ok(Some(fixture().unwrap())), None))
            .unwrap();
        lookup.poll();
        assert!(lookup.value.is_none());
        assert!(lookup.loading);
        let (current, _, _) = incoming.try_recv().unwrap();
        outgoing
            .send((current, Ok(Some(fixture().unwrap())), None))
            .unwrap();
        assert!(lookup.poll());
        assert!(!lookup.loading);
        assert_eq!(lookup.value.as_ref().unwrap().number, 8);
        assert!(!lookup.poll());
        assert!(incoming.try_recv().is_err(), "no automatic polling");
        lookup.clear();
        assert!(lookup.value.is_none());
    }

    #[test]
    fn worktree_registry_requires_unique_exact_branch_and_absolute_checkout() {
        let entry = "worktree /repo with spaces\nline\0HEAD abc\0branch refs/heads/feature\0\0";
        assert_eq!(
            worktree_checkout(entry, "feature").unwrap(),
            "/repo with spaces\nline"
        );
        assert!(worktree_checkout(entry, "feat").is_err());
        assert!(
            worktree_checkout(&entry.repeat(2), "feature")
                .unwrap_err()
                .to_string()
                .contains("Multiple")
        );
        for invalid in [
            "worktree relative\0branch refs/heads/feature\0\0",
            "worktree /repo\0HEAD abc\0detached\0\0",
            "worktree /repo\0branch refs/remotes/feature\0\0",
            "worktree /repo\0bare\0\0",
        ] {
            assert!(worktree_checkout(invalid, "feature").is_err());
        }
    }

    #[test]
    fn subprocess_success_errors_limits_timeout_and_cancellation() {
        let deadline = || Instant::now() + Duration::from_secs(5);
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "printf '%s' \"$GH_HOST:$GH_PROMPT_DISABLED:$GIT_TERMINAL_PROMPT\"",
        ]);
        assert_eq!(
            run(&mut command, deadline(), &|| false).unwrap(),
            (true, "github.com:1:0".into())
        );
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf failure >&2; exit 1"]);
        assert_eq!(
            run(&mut command, deadline(), &|| false).unwrap(),
            (false, "failure".into())
        );
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf '\\377'"]);
        assert!(
            run(&mut command, deadline(), &|| false).is_err(),
            "non-UTF8 Git paths must fail closed, not be lossily mapped"
        );
        assert!(
            run(&mut Command::new("/usr/bin/yes"), deadline(), &|| false)
                .unwrap_err()
                .to_string()
                .contains("size limit")
        );
        let mut sleep = Command::new("/bin/sleep");
        sleep.arg("5");
        assert!(
            run(
                &mut sleep,
                Instant::now() + Duration::from_millis(30),
                &|| false
            )
            .unwrap_err()
            .to_string()
            .contains("timed out")
        );
        assert!(
            run(&mut Command::new("/not/an/executable"), deadline(), &|| {
                true
            })
            .unwrap_err()
            .to_string()
            .contains("cancelled")
        );
        assert!(
            run(&mut Command::new("/not/an/executable"), deadline(), &|| {
                false
            })
            .unwrap_err()
            .to_string()
            .contains("install git")
        );
        let calls = std::cell::Cell::new(0);
        let mut sleep = Command::new("/bin/sleep");
        sleep.arg("5");
        assert!(
            run(&mut sleep, deadline(), &|| {
                calls.set(calls.get() + 1);
                calls.get() > 1
            })
            .unwrap_err()
            .to_string()
            .contains("cancelled")
        );
    }

    #[test]
    fn local_git_verification_rejects_wrong_checkout_branch_and_remote_before_gh() {
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("herdr-pr-{}-{suffix}", std::process::id());
        let directory = Directory(std::env::temp_dir().join(name));
        std::fs::create_dir(&directory.0).unwrap();
        let git = |args: &[&str]| {
            let mut command = Command::new("git");
            command.arg("-C").arg(&directory.0).args(args);
            let (ok, text) = run(&mut command, Instant::now() + TIMEOUT, &|| false).unwrap();
            assert!(ok, "fixture git failed: {text}");
        };
        git(&["init", "--quiet", "--template=", "-b", "feature"]);
        git(&[
            "config",
            "--local",
            "remote.origin.url",
            "https://unsupported.invalid/example/project.git",
        ]);
        let mut input = Input {
            checkout: Some(directory.0.to_str().unwrap().into()),
            repo_key: directory.0.join(".git").to_str().unwrap().into(),
            branch: "feature".into(),
        };
        assert!(
            fetch(&input, &"fixture".into(), || false)
                .unwrap_err()
                .to_string()
                .contains("GitHub.com origins only")
        );
        let mut registry_input = input.clone();
        registry_input.checkout = None;
        assert!(
            fetch(&registry_input, &"fixture".into(), || false)
                .unwrap_err()
                .to_string()
                .contains("GitHub.com origins only")
        );
        git(&[
            "config",
            "--local",
            "remote.origin.url",
            "https://github.com/example/project.git",
        ]);
        assert_eq!(
            local_repository(&registry_input, Instant::now() + TIMEOUT, &|| false).unwrap(),
            ("example".into(), "project".into())
        );
        registry_input.branch = "missing".into();
        assert!(
            local_repository(&registry_input, Instant::now() + TIMEOUT, &|| false)
                .unwrap_err()
                .to_string()
                .contains("No local worktree")
        );
        input.branch = "other".into();
        assert!(
            fetch(&input, &"fixture".into(), || false)
                .unwrap_err()
                .to_string()
                .contains("branch changed")
        );
        input.repo_key = directory.0.to_str().unwrap().into();
        assert!(
            fetch(&input, &"fixture".into(), || false)
                .unwrap_err()
                .to_string()
                .contains("does not match daemon metadata")
        );
        input.checkout = Some("relative".into());
        assert!(
            fetch(&input, &"fixture".into(), || false)
                .unwrap_err()
                .to_string()
                .contains("absolute checkout")
        );
    }
}
