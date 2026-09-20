//! Read-only, on-demand PR lookup. One worker, one outstanding request, no UI I/O.
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

#[derive(Clone, Debug)]
pub(super) struct Input {
    pub checkout: String,
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
            _ if self.is_draft => "Open / Draft",
            _ => "Open / Ready for review",
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
        format!(
            "Checks: {} passed / {} failed / {} pending / {} skipped",
            counts[0], counts[1], counts[2], counts[3]
        )
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

type Result = std::result::Result<Option<PullRequest>, String>;

struct Worker {
    requests: mpsc::SyncSender<(u64, Input)>,
    results: mpsc::Receiver<(u64, Result)>,
}

#[derive(Default)]
pub(super) struct Lookup {
    worker: Option<Worker>,
    generation: Arc<AtomicU64>,
    busy: bool,
    waiting: Option<Input>,
    pub loading: bool,
    pub value: Option<PullRequest>,
    pub message: Option<String>,
    pub checked: Option<Instant>,
}

impl Drop for Lookup {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

impl Lookup {
    pub fn clear(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.waiting = None;
        self.loading = false;
        self.value = None;
        self.message = None;
        self.checked = None;
    }

    pub fn request(&mut self, input: Input) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.waiting = Some(input);
        self.loading = true;
        self.message = None;
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(worker) = &self.worker {
            match worker.results.try_recv() {
                Ok((generation, result)) => {
                    self.busy = false;
                    if generation == self.generation.load(Ordering::Relaxed) {
                        self.loading = false;
                        self.checked = Some(Instant::now());
                        match result {
                            Ok(value) => {
                                self.value = value;
                                self.message = None;
                            }
                            Err(error) => self.message = Some(error),
                        }
                        changed = true;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.busy = false;
                    self.loading = false;
                    self.waiting = None;
                    self.message = Some("PR worker stopped. Reopen the menu to retry.".into());
                    self.worker = None;
                    return true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if !self.busy
            && let Some(input) = self.waiting.take()
        {
            if self.worker.is_none() {
                let (requests, incoming) = mpsc::sync_channel::<(u64, Input)>(1);
                let (outgoing, results) = mpsc::sync_channel(1);
                let current = self.generation.clone();
                match thread::Builder::new()
                    .name("herdr-pr".into())
                    .spawn(move || {
                        for (generation, input) in incoming {
                            let result =
                                fetch(&input, || current.load(Ordering::Relaxed) != generation);
                            if outgoing.send((generation, result)).is_err() {
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
                    .try_send((self.generation.load(Ordering::Relaxed), input))
                    .is_ok();
            }
        }
        changed
    }
}

fn fetch(input: &Input, cancelled: impl Fn() -> bool) -> Result {
    let deadline = Instant::now() + TIMEOUT;
    if !Path::new(&input.checkout).is_absolute() || !Path::new(&input.repo_key).is_absolute() {
        return Err("Daemon did not provide an absolute checkout and repository key.".into());
    }
    let git = |args: &[&str]| {
        let mut command = Command::new("git");
        command
            .args(["-c", "core.fsmonitor=false", "-C", &input.checkout])
            .args(args);
        run(&mut command, deadline, &cancelled).and_then(|(ok, output)| {
            if ok {
                Ok(output.trim_end_matches(['\r', '\n']).to_owned())
            } else {
                Err("Local checkout unavailable or not a trusted Git repository.".into())
            }
        })
    };
    // Never infer a checkout from a creation-policy cwd, group label, or branch name.
    let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    if Path::new(&common)
        .canonicalize()
        .ok()
        .zip(Path::new(&input.repo_key).canonicalize().ok())
        .is_none_or(|(actual, expected)| actual != expected)
    {
        return Err("Local repository does not match daemon metadata.".into());
    }
    if git(&["symbolic-ref", "--quiet", "--short", "HEAD"])? != input.branch {
        return Err("Checkout branch changed. Reopen the menu after the daemon updates.".into());
    }
    let remote = git(&["config", "--get", "remote.origin.url"])?;
    let (owner, repo) = crate::avatars::github_repo(&remote)
        .ok_or("PR lookup supports GitHub.com origins only.")?;
    let timeout = deadline
        .checked_duration_since(Instant::now())
        .ok_or("PR lookup timed out (15 seconds).")?;
    let response = crate::github::graphql(
        QUERY,
        serde_json::json!({"owner":owner,"repo":repo,"branch":input.branch}),
        timeout,
        cancelled,
    )?;
    parse_graphql(response, &owner, &repo, &input.branch)
}

fn parse_graphql(response: serde_json::Value, owner: &str, repo: &str, branch: &str) -> Result {
    let mut nodes = response["data"]["repository"]["pullRequests"]["nodes"]
        .as_array()
        .ok_or("GitHub repository unavailable. Check repository access and token permissions.")?
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
        &serde_json::to_string(&nodes).map_err(|_| "Invalid GitHub PR response.")?,
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
        return Err("PR response exceeded the size limit.".into());
    }
    let mut values: Vec<PullRequest> =
        serde_json::from_str(text).map_err(|_| "Invalid GitHub PR response.")?;
    if values.is_empty() {
        return Ok(None);
    }
    if values.len() != 1 {
        return Err("Multiple PRs match this branch; no PR selected.".into());
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
        return Err("PR identity does not match the repository and branch.".into());
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
) -> std::result::Result<(bool, String), String> {
    if cancelled() {
        return Err("PR lookup cancelled.".into());
    }
    let (mut reader, writer) =
        UnixStream::pair().map_err(|_| "Could not create process output channel.")?;
    reader
        .set_nonblocking(true)
        .map_err(|_| "Could not configure process output.")?;
    let error_writer = writer
        .try_clone()
        .map_err(|_| "Could not configure process errors.")?;
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
    let mut child = command
        .spawn()
        .map_err(|_| "Could not launch Git. Install git on PATH.")?;
    // Command retains Stdio descriptors after spawn; release them so EOF is observable.
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let result = (|| {
        let mut output = Vec::new();
        let mut buffer = [0; 8192];
        let mut eof = false;
        loop {
            if cancelled() {
                return Err("PR lookup cancelled.".into());
            }
            if Instant::now() >= deadline {
                return Err("PR lookup timed out (15 seconds).".into());
            }
            match reader.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(n) => {
                    if output.len() + n > OUTPUT_LIMIT {
                        return Err("PR response exceeded the size limit.".into());
                    }
                    output.extend_from_slice(&buffer[..n]);
                    continue;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err("Could not read PR process output.".into()),
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|_| "Could not wait for PR process.")?
                && eof
            {
                return String::from_utf8(output)
                    .map(|text| (status.success(), text))
                    .map_err(|_| "Invalid process text.".into());
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
pub(super) fn fixture() -> std::result::Result<PullRequest, String> {
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
    }]).to_string(), "example", "project", "feature")?.ok_or("Missing fixture PR".into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn parses_identity_lifecycle_and_check_categories() {
        let mut pr = fixture().unwrap();
        assert_eq!(
            pr.checks(),
            "Checks: 1 passed / 1 failed / 1 pending / 0 skipped"
        );
        assert_eq!(pr.lifecycle(), "Open / Ready for review");
        assert_eq!(pr.review(), "Review required");
        pr.is_draft = true;
        assert_eq!(pr.lifecycle(), "Open / Draft");
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
        assert_eq!(
            pr.checks(),
            "Checks: 0 passed / 1 failed / 1 pending / 2 skipped"
        );
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
    fn worker_discards_stale_results_and_caches_only_current_menu() {
        let (requests, incoming) = mpsc::sync_channel(1);
        let (outgoing, results) = mpsc::sync_channel(1);
        let mut lookup = Lookup::default();
        lookup.worker = Some(Worker { requests, results });
        let input = Input {
            checkout: "/fixture".into(),
            repo_key: "/fixture/.git".into(),
            branch: "feature".into(),
        };
        lookup.request(input.clone());
        lookup.poll();
        let (old, _) = incoming.try_recv().unwrap();
        lookup.clear();
        lookup.request(input);
        lookup.poll();
        assert!(incoming.try_recv().is_err(), "single in-flight request");
        outgoing.send((old, Ok(Some(fixture().unwrap())))).unwrap();
        lookup.poll();
        assert!(lookup.value.is_none());
        assert!(lookup.loading);
        let (current, _) = incoming.try_recv().unwrap();
        outgoing
            .send((current, Ok(Some(fixture().unwrap()))))
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
        assert!(
            run(&mut Command::new("/usr/bin/yes"), deadline(), &|| false)
                .unwrap_err()
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
            .contains("timed out")
        );
        assert!(
            run(&mut Command::new("/not/an/executable"), deadline(), &|| {
                true
            })
            .unwrap_err()
            .contains("cancelled")
        );
        assert!(
            run(&mut Command::new("/not/an/executable"), deadline(), &|| {
                false
            })
            .unwrap_err()
            .contains("Install git")
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
            checkout: directory.0.to_str().unwrap().into(),
            repo_key: directory.0.join(".git").to_str().unwrap().into(),
            branch: "feature".into(),
        };
        assert!(
            fetch(&input, || false)
                .unwrap_err()
                .contains("GitHub.com origins only")
        );
        input.branch = "other".into();
        assert!(
            fetch(&input, || false)
                .unwrap_err()
                .contains("branch changed")
        );
        input.repo_key = directory.0.to_str().unwrap().into();
        assert!(
            fetch(&input, || false)
                .unwrap_err()
                .contains("does not match daemon metadata")
        );
        input.checkout = "relative".into();
        assert!(
            fetch(&input, || false)
                .unwrap_err()
                .contains("absolute checkout")
        );
    }
}
