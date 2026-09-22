//! Painting the new worktree dialog's tab strip and its two GitHub listings.
//! Rows are drawn from the prepared filter, never from a query made while
//! rendering.

use super::{
    Page, WorkspaceAction,
    worktree_source::{Tab, WorktreeSource},
};
use crate::{
    HerdrWindow,
    repo_items::{Kind, Origin},
};
use gpui::{prelude::*, *};

impl HerdrWindow {
    /// The tab strip. The GitHub tabs are inert until an account is connected,
    /// because neither listing can be fetched without one.
    pub(super) fn render_worktree_tabs(&self, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        let connected = self.menu.github.connected();
        let current = self
            .menu
            .worktree
            .as_ref()
            .map_or(Tab::Branch, |source| source.tab);
        let busy = self
            .menu
            .worktree
            .as_ref()
            .is_some_and(WorktreeSource::busy);
        let mut strip = div()
            .debug_selector(|| "worktree-tabs".into())
            .flex()
            .gap(px(4.))
            .px(px(16.))
            .pt(px(10.));
        for tab in Tab::ALL {
            let label = tab.label();
            let enabled = connected || tab.kind().is_none();
            let selected = tab == current;
            strip = strip.child(
                div()
                    .id(label)
                    .debug_selector(move || format!("worktree-tab-{label}"))
                    .px(px(10.))
                    .py(px(5.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(rgb(if selected {
                        theme.foreground
                    } else {
                        theme.active
                    }))
                    .when(selected, |button| button.bg(rgb(theme.active)))
                    .text_color(rgb(if !enabled {
                        theme.muted
                    } else if selected {
                        theme.foreground
                    } else {
                        theme.muted
                    }))
                    .when(enabled && !busy, |button| {
                        button
                            .cursor_pointer()
                            .hover(|button| button.bg(rgb(theme.active)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.select_worktree_tab(tab, window, cx);
                            }))
                    })
                    .child(label),
            );
        }
        if !connected {
            strip = strip.child(
                div()
                    .debug_selector(|| "worktree-tabs-note".into())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .py(px(5.))
                    .text_color(rgb(theme.muted))
                    .child("sign in to GitHub to browse"),
            );
        }
        strip
    }

    /// One GitHub listing: its search field, how much of it the search kept,
    /// the rows themselves, and whatever the worker last had to say.
    pub(super) fn render_worktree_items(&self, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        let font = &self.config.ui;
        let Some(source) = &self.menu.worktree else {
            return div();
        };
        let Some(kind) = source.tab.kind() else {
            return div();
        };
        let (shown, total) = source.counts();
        let rows = source.filtered.len();
        let status = if let Some(item) = &source.pending {
            // Dismissing only closes the panel; the daemon keeps queued work,
            // as the branch tab's own waiting note says.
            format!(
                "Creating a checkout for {}. Dismissing does not cancel it.",
                item.label()
            )
        } else if let Some(error) = &source.lookup.message {
            error.clone()
        } else if source.lookup.loading {
            "Loading from GitHub...".to_owned()
        } else {
            source
                .lookup
                .origin
                .as_ref()
                .map(Origin::slug)
                .unwrap_or_default()
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex_none()
                    .px(px(16.))
                    .pt(px(10.))
                    .child(source.search.clone())
                    .child(
                        div()
                            .debug_selector(|| "worktree-count".into())
                            .pt(px(6.))
                            .text_color(rgb(theme.muted))
                            .child(format!(
                                "{shown} of {total} open {}",
                                match kind {
                                    Kind::PullRequest => "pull requests",
                                    Kind::Issue => "issues",
                                }
                            )),
                    ),
            )
            .when(rows == 0, |panel| {
                panel.child(
                    div()
                        .debug_selector(|| "worktree-empty".into())
                        .flex_1()
                        .px(px(16.))
                        .py(px(12.))
                        .text_color(rgb(theme.muted))
                        .child(if source.lookup.loading {
                            "Loading from GitHub..."
                        } else {
                            kind.empty_label()
                        }),
                )
            })
            .when(rows > 0, |panel| {
                panel.child(
                    uniform_list(
                        "worktree-items",
                        rows,
                        cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                            range.map(|row| this.render_worktree_row(row, cx)).collect()
                        }),
                    )
                    .track_scroll(source.scroll.clone())
                    .flex_1()
                    .min_h_0(),
                )
            })
            .child(
                div()
                    .id("worktree-status")
                    .debug_selector(|| "worktree-status".into())
                    .flex_none()
                    .h(px(font.line_height() * 2. + 16.))
                    .overflow_y_scroll()
                    .px(px(16.))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(rgb(theme.active))
                    .text_color(rgb(theme.muted))
                    .child(status),
            )
    }

    fn render_worktree_row(&self, row: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let font = &self.config.ui;
        let Some(source) = &self.menu.worktree else {
            return div().into_any_element();
        };
        let Some(item) = source.item(row) else {
            return div().into_any_element();
        };
        let selected = row == source.selected;
        // A fork's head branch has no ref on `origin`, so the row says why it
        // cannot be picked rather than failing once it is.
        let detail = if item.fork_owner.is_some() {
            "from a fork - check out manually".to_owned()
        } else {
            let branch = item.branch();
            match item.author.is_empty() {
                true => branch,
                false => format!("{} - {branch}", item.author),
            }
        };
        div()
            .id(row)
            .debug_selector(move || format!("worktree-row-{row}"))
            .w_full()
            .h(px(font.line_height() * 2. + 14.))
            .px(px(16.))
            .py(px(5.))
            .flex()
            .flex_col()
            .justify_center()
            .cursor_pointer()
            .when(selected, |row| row.bg(rgb(theme.active)))
            .hover(|row| row.bg(rgb(theme.active)))
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .min_w_0()
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(theme.muted))
                            .child(format!("#{}", item.number)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(item.title.clone()),
                    )
                    .when(item.draft, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_color(rgb(theme.muted))
                                .child("draft"),
                        )
                    }),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(theme.muted))
                    .child(detail),
            )
            .on_hover(cx.listener(move |this, hovered, _, cx| {
                if *hovered && let Some(source) = &mut this.menu.worktree {
                    source.selected = row;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.create_from_repo_item(row, cx);
            }))
            .into_any_element()
    }

    /// Whether a listing's search field is mid-composition, in which case the
    /// platform owns every key until the composition commits.
    pub(super) fn worktree_source_composing(&self, cx: &Context<Self>) -> bool {
        self.menu
            .worktree
            .as_ref()
            .is_some_and(|source| source.search.read(cx).is_composing())
    }

    /// Keys the new worktree dialog's GitHub tabs own. Reports whether the key
    /// was consumed, so the branch tab keeps its existing handling untouched.
    pub(super) fn worktree_source_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.menu.page != Some(Page::Dialog(WorkspaceAction::NewWorktree)) {
            return false;
        }
        if self
            .menu
            .input
            .as_ref()
            .is_some_and(|input| input.marked.is_some())
        {
            return false;
        }
        let key = event.keystroke.key.as_str();
        if key == "tab" {
            let forward = !event.keystroke.modifiers.shift;
            let current = self
                .menu
                .worktree
                .as_ref()
                .map_or(Tab::Branch, |source| source.tab);
            let tabs: Vec<Tab> = Tab::ALL
                .into_iter()
                .filter(|tab| tab.kind().is_none() || self.menu.github.connected())
                .collect();
            if let Some(index) = tabs.iter().position(|tab| *tab == current) {
                let next = if forward {
                    (index + 1) % tabs.len()
                } else {
                    (index + tabs.len() - 1) % tabs.len()
                };
                self.select_worktree_tab(tabs[next], window, cx);
            }
            return true;
        }
        let Some(source) = &mut self.menu.worktree else {
            return false;
        };
        if source.tab.kind().is_none() {
            return false;
        }
        let rows = source.filtered.len();
        match key {
            "up" | "down" if rows > 0 => {
                source.selected = match key {
                    "up" => (source.selected + rows - 1) % rows,
                    _ => (source.selected + 1) % rows,
                };
                let selected = source.selected;
                source.scroll.scroll_to_item(selected, ScrollStrategy::Top);
                cx.notify();
                true
            }
            "up" | "down" => true,
            "enter" => {
                let selected = source.selected;
                self.create_from_repo_item(selected, cx);
                true
            }
            _ => false,
        }
    }
}
