//! What a pull request looks like to this client, and the repository/branch
//! pair a lookup is keyed by. Remote text is untrusted: it is cleaned and
//! length-bounded before it can reach a label.

#[cfg(any(test, all(feature = "integration-test", target_os = "macos")))]
use super::parse::parse;
use crate::Error;
use serde::Deserialize;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Input {
    pub checkout: Option<String>,
    pub repo_key: String,
    pub branch: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PullRequest {
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
    pub(super) status_check_rollup: Option<Vec<Check>>,
    pub(super) head_repository_owner: Owner,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Owner {
    pub(super) login: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Check {
    #[serde(rename = "__typename")]
    pub(super) kind: String,
    pub(super) state: Option<String>,
    pub(super) status: Option<String>,
    pub(super) conclusion: Option<String>,
}

impl PullRequest {
    /// Lifecycle color, shared by the workspace menu and the sidebar badge so
    /// one legend covers both: merged, closed, draft, open.
    pub fn color(&self, theme: &crate::config::Theme) -> u32 {
        match self.state.as_str() {
            "MERGED" => theme.palette[5],
            "CLOSED" => theme.palette[1],
            _ if self.is_draft => theme.muted,
            _ => theme.palette[2],
        }
    }

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

/// Daemon worktree metadata as a lookup key. The checkout is resolved later,
/// from Git's own registry, so a workspace never points work at another tree.
pub(crate) fn repository_input(
    worktree: Option<&herdr_client::protocol::ClientShellWorktree>,
    branch: Option<&str>,
) -> crate::Result<Input> {
    let key = worktree
        .map(|tree| tree.key.as_str())
        .ok_or(Error::PrMetadata)?;
    let branch = branch
        .filter(|branch| {
            !branch.is_empty() && branch.len() <= 1024 && !branch.chars().any(char::is_control)
        })
        .ok_or(Error::PrBranch)?;
    if !Path::new(key).is_absolute() {
        return Err(Error::PrAbsolutePath);
    }
    Ok(Input {
        checkout: None,
        repo_key: key.into(),
        branch: branch.into(),
    })
}

pub(crate) fn clean(text: &str) -> String {
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

#[cfg(any(test, all(feature = "integration-test", target_os = "macos")))]
pub(crate) fn fixture() -> crate::Result<PullRequest> {
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
