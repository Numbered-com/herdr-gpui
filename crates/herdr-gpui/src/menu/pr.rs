use super::*;
use crate::pull_request::{Input, clean};
use std::sync::Arc;

impl HerdrWindow {
    pub(super) fn refresh_workspace_pr(&mut self) {
        if self.menu.pr.loading {
            return;
        }
        let result = (|| {
            let target = self.menu.target.as_ref().ok_or("Workspace unavailable.")?;
            if target.worktree.is_none() || target.branch.as_deref().is_none_or(str::is_empty) {
                return Err("No Git branch/repository metadata available.".into());
            }
            if self.selected_endpoint != 0 || !self.live.local_daemon_peer {
                return Err("PR lookup unavailable: socket peer could not be verified as the local Herdr installation. Forwarders and daemons running a removed/replaced executable are unsupported.".into());
            }
            if !self.workspace_pr_target_current() {
                return Err("Workspace changed or disconnected. Reopen the menu.".into());
            }
            self.endpoints[self.selected_endpoint]
                .connection
                .request_dialog(
                    &target.boot_id,
                    "workspace.get",
                    serde_json::json!({"workspace_id": target.id}),
                )
        })();
        match result {
            Ok(id) => {
                self.menu.pr_pending = Some(id);
                self.menu.pr_connection = Some(Arc::downgrade(
                    &self.endpoints[self.selected_endpoint].connection.inbox,
                ));
                self.menu.pr.loading = true;
                self.menu.pr.message = None;
            }
            Err(error) => self.menu.pr.message = Some(error),
        }
    }

    fn workspace_pr_target_current(&self) -> bool {
        self.menu_target_current()
            && self.live.status.is_connected()
            && self.menu.pr_connection.as_ref().is_none_or(|old| {
                old.upgrade().is_some_and(|old| {
                    Arc::ptr_eq(
                        &old,
                        &self.endpoints[self.selected_endpoint].connection.inbox,
                    )
                })
            })
            && self.menu.target.as_ref().is_some_and(|target| {
                self.live.snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.boot_id == target.boot_id
                        && snapshot.workspaces.iter().any(|workspace| {
                            workspace.workspace_id == target.id
                                && workspace.worktree == target.worktree
                                && workspace.branch == target.branch
                        })
                })
            })
    }

    pub(crate) fn update_workspace_pr(&mut self) -> bool {
        let mut changed = self.menu.github.poll();
        if self.menu.page == Some(Page::Workspace) && !self.workspace_pr_target_current() {
            if self.menu.pr.message.as_deref()
                != Some("Workspace changed or disconnected. Reopen the menu.")
            {
                self.menu.pr.clear();
                self.menu.pr_pending = None;
                self.menu.pr.message =
                    Some("Workspace changed or disconnected. Reopen the menu.".into());
                changed = true;
            }
        } else if let Some(id) = &self.menu.pr_pending
            && let Some((response_id, Some(response))) = &self.live.dialog_response
            && id == response_id
        {
            let result = response
                .as_ref()
                .map_err(|_| "Daemon checkout lookup failed.".to_owned())
                .and_then(|response| {
                    let target = self.menu.target.as_ref().ok_or("Workspace unavailable.")?;
                    checkout_input(
                        response,
                        &target.id,
                        target.worktree.as_ref(),
                        target.branch.as_deref(),
                    )
                });
            self.menu.pr_pending = None;
            match result {
                Ok(input) => self.menu.pr.request(input),
                Err(error) => {
                    self.menu.pr.loading = false;
                    self.menu.pr.message = Some(error);
                }
            }
            changed = true;
        }
        self.menu.pr.poll() || changed
    }

    pub(super) fn open_workspace_pr(&self, cx: &mut Context<Self>) {
        if self.workspace_pr_target_current()
            && let Some(pr) = &self.menu.pr.value
        {
            cx.open_url(&pr.url);
        }
    }

    pub(super) fn render_workspace_pr(&self, text_width: Pixels, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        let pr = &self.menu.pr;
        let mut section = div()
            .debug_selector(|| "workspace-pr".into())
            .mt(px(6.))
            .pt(px(8.))
            .px(px(8.))
            .pb(px(6.))
            .border_t_1()
            .border_color(rgb(theme.active))
            .flex()
            .flex_col()
            .gap(px(4.))
            .min_w_0()
            .child(
                div()
                    .text_color(rgb(theme.muted))
                    .child("GITHUB PULL REQUEST"),
            );
        if let Some(value) = &pr.value {
            section =
                section
                    .child(
                        div()
                            .debug_selector(|| "workspace-pr-title".into())
                            .w(text_width)
                            .flex_none()
                            .truncate()
                            .child(crate::sidebar::label_text(&format!(
                                "#{} {}",
                                value.number, value.title
                            ))),
                    )
                    .child(
                        div()
                            .w(text_width)
                            .flex_none()
                            .truncate()
                            .text_color(rgb(theme.muted))
                            .child(format!(
                                "{} -> {}",
                                value.head_ref_name, value.base_ref_name
                            )),
                    )
                    .child(div().child(value.lifecycle()))
                    .child(div().child(format!(
                        "Merge: {} / {}",
                        clean(&value.merge_state_status),
                        value.review()
                    )))
                    .child(div().child(value.checks_summary.clone()))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(div().text_color(rgb(theme.palette[2])).child(
                                crate::sidebar::label_text(&format!("+{}", value.additions)),
                            ))
                            .child(div().text_color(rgb(theme.palette[1])).child(
                                crate::sidebar::label_text(&format!("-{}", value.deletions)),
                            ))
                            .child(format!("/ {} files", value.changed_files)),
                    )
                    .child(
                        div()
                            .w(text_width)
                            .flex_none()
                            .truncate()
                            .text_color(rgb(theme.muted))
                            .child(format!("Updated {}", value.updated_at)),
                    );
        }
        if pr.loading {
            section = section.child(
                div()
                    .text_color(rgb(theme.muted))
                    .child("Checking GitHub..."),
            );
        } else if let Some(message) = &pr.message {
            section = section.child(div().text_color(rgb(theme.muted)).child(format!(
                "{}{message}",
                if pr.value.is_some() { "Stale: " } else { "" }
            )));
        } else if pr.value.is_none() {
            section = section.child(
                div()
                    .text_color(rgb(theme.muted))
                    .child("No PR found for this origin and branch."),
            );
        }
        if let Some(checked) = pr.checked {
            section = section.child(
                div()
                    .text_color(rgb(theme.muted))
                    .child(format!("Checked {}s ago", checked.elapsed().as_secs())),
            );
        }
        section.child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(16.))
                .when(pr.value.is_some(), |row| {
                    row.child(
                        div()
                            .id("workspace-pr-open")
                            .cursor_pointer()
                            .text_color(rgb(theme.palette[4]))
                            .child("Open PR (O)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.open_workspace_pr(cx);
                            })),
                    )
                })
                .child(
                    div()
                        .id("workspace-github-auth")
                        .cursor_pointer()
                        .child("GitHub sign-in")
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.menu.pr.clear();
                            this.menu.pr_pending = None;
                            this.menu.page = Some(Page::GitHub);
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id("workspace-pr-refresh")
                        .text_color(rgb(if pr.loading {
                            theme.muted
                        } else {
                            theme.foreground
                        }))
                        .when(!pr.loading, |button| button.cursor_pointer())
                        .child("Refresh (R)")
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.refresh_workspace_pr();
                            cx.notify();
                        })),
                ),
        )
    }

    pub(super) fn render_github_auth(&self, cx: &mut Context<Self>) -> Div {
        let auth = &self.menu.github;
        let mut section = div().p(px(12.)).flex().flex_col().gap(px(12.))
            .child("GitHub")
            .child("Native GitHub.com authentication. GH_TOKEN, then GITHUB_TOKEN, then this app's macOS Keychain token are used. No GitHub CLI required.")
            .child(auth.message.clone().unwrap_or_else(|| "Sign in with your browser, or remove this app's saved token. PR repository verification is independent of sign-in.".into()));
        if let Some(code) = auth.code() {
            section = section
                .child(
                    div()
                        .debug_selector(|| "github-device-code".into())
                        .child(crate::sidebar::label_text(&format!("Code: {code}"))),
                )
                .child(
                    div()
                        .id("github-open")
                        .cursor_pointer()
                        .child("Open GitHub (O)")
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.stop_propagation();
                            cx.open_url(crate::github::VERIFY_URL);
                        })),
                );
        }
        if auth.busy() {
            section = section.child(
                div()
                    .id("github-cancel")
                    .cursor_pointer()
                    .child("Cancel sign-in (C)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.menu.github.cancel();
                        cx.notify();
                    })),
            );
        } else {
            section = section
                .child(
                    div()
                        .id("github-start")
                        .cursor_pointer()
                        .child("Sign in (S)")
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.menu.github.start();
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id("github-sign-out")
                        .cursor_pointer()
                        .child("Remove saved token (D)")
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.menu.pr.clear();
                            this.menu.github.sign_out();
                            cx.notify();
                        })),
                );
        }
        section
    }

    #[cfg(any(test, feature = "integration-test"))]
    pub(crate) fn github_fixture(
        &mut self,
        waiting: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_menu(window, cx);
        self.menu.page = Some(Page::GitHub);
        self.menu.github = crate::github::Auth::fixture(waiting);
        cx.notify();
    }
}

fn checkout_input(
    response: &serde_json::Value,
    id: &str,
    worktree: Option<&ClientShellWorktree>,
    branch: Option<&str>,
) -> Result<Input, String> {
    let result = &response["result"];
    let workspace = &result["workspace"];
    let tree = &workspace["worktree"];
    let key = worktree
        .map(|tree| tree.key.as_str())
        .ok_or("No repository metadata.")?;
    if response.get("error").is_some()
        || result["type"] != "workspace_info"
        || workspace["workspace_id"] != id
        || tree["repo_key"] != key
    {
        return Err("Daemon did not identify the requested checkout. Reopen the menu.".into());
    }
    let checkout = tree["checkout_path"]
        .as_str()
        .filter(|s| std::path::Path::new(s).is_absolute())
        .ok_or("Daemon did not provide an absolute checkout path.")?;
    let branch = branch
        .filter(|branch| {
            !branch.is_empty() && branch.len() <= 1024 && !branch.chars().any(char::is_control)
        })
        .ok_or("No supported branch available.")?;
    Ok(Input {
        checkout: checkout.into(),
        repo_key: key.into(),
        branch: branch.into(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::checkout_input;
    use herdr_client::protocol::ClientShellWorktree;

    #[test]
    fn checkout_response_requires_authoritative_identity_and_absolute_path() {
        let tree = ClientShellWorktree {
            key: "/repo/.git".into(),
            label: "repo".into(),
            is_linked_worktree: true,
        };
        let response = serde_json::json!({"result":{"type":"workspace_info", "workspace":{
            "workspace_id":"w", "worktree":{"repo_key":"/repo/.git", "checkout_path":"/worktree"}
        }}});
        let input = checkout_input(&response, "w", Some(&tree), Some("feature")).unwrap();
        assert_eq!(input.checkout, "/worktree");
        assert_eq!(input.repo_key, "/repo/.git");
        assert_eq!(input.branch, "feature");
        assert!(checkout_input(&response, "wrong", Some(&tree), Some("feature")).is_err());
        for branch in [None, Some(""), Some("bad\nbranch")] {
            assert!(checkout_input(&response, "w", Some(&tree), branch).is_err());
        }
        let mut bad = response.clone();
        bad["result"]["workspace"]["worktree"]["checkout_path"] = "relative".into();
        assert!(checkout_input(&bad, "w", Some(&tree), Some("feature")).is_err());
        let mut bad = response.clone();
        bad["result"]["workspace"]["worktree"]["repo_key"] = "/other/.git".into();
        assert!(checkout_input(&bad, "w", Some(&tree), Some("feature")).is_err());
        let mut bad = response;
        bad["error"] = serde_json::json!({"code":"unsupported"});
        assert!(checkout_input(&bad, "w", Some(&tree), Some("feature")).is_err());
    }
}
