use crate::{APP_VERSION, HerdrWindow, menu::Page, updater::State};
use gpui::{prelude::*, *};

#[derive(Clone, Copy)]
enum UpdateAction {
    Check,
    Download,
    Install,
    Cancel,
}

impl HerdrWindow {
    pub(super) fn open_app_update(
        &mut self,
        preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_menu(window, cx);
        self.menu.page = Some(Page::AppUpdate);
        self.update_preview = preview.then(|| State::Available {
            version: "9999.0.0".into(),
        });
    }

    pub(super) fn render_app_update(&self, cx: &mut Context<Self>) -> Div {
        let state = self.update_preview.as_ref().unwrap_or(self.updater.state());
        let (message, action) = match state {
            State::Disabled(reason) => (format!("In-app updates unavailable: {reason}"), None),
            State::Idle => (
                "Check GitHub for a new app release.".into(),
                Some(UpdateAction::Check),
            ),
            State::Checking => ("Checking for updates...".into(), Some(UpdateAction::Cancel)),
            State::Current => (
                "You are running the latest available release.".into(),
                Some(UpdateAction::Check),
            ),
            State::Available { .. } => (
                "A new app release is available.".into(),
                Some(UpdateAction::Download),
            ),
            State::Downloading { received, total } => (
                if *total == 0 {
                    format!("Downloading: {received} bytes received (size unknown)")
                } else {
                    format!(
                        "Downloading: {received} / {total} bytes ({:.0}%)",
                        (*received as f64 / *total as f64 * 100.).min(100.)
                    )
                },
                Some(UpdateAction::Cancel),
            ),
            State::Ready { .. } => (
                "Download verified. Install and restart when you are ready.".into(),
                Some(UpdateAction::Install),
            ),
            State::Installing => (
                "Preparing installation and restart. Herdr will quit when the update is ready."
                    .into(),
                Some(UpdateAction::Cancel),
            ),
            State::Cancelling => (
                "Cancelling update... Waiting for current I/O to finish or time out.".into(),
                None,
            ),
            State::Error(error) => (format!("Update failed: {error}"), Some(UpdateAction::Check)),
        };
        let latest = match state {
            State::Available { version } | State::Ready { version } => version.as_str(),
            State::Current => APP_VERSION,
            _ => "Not yet known",
        };
        let panel = div()
            .debug_selector(|| "app-update-panel".into())
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(12.))
            .min_w_0()
            .overflow_hidden()
            .child(div().text_size(px(self.config.ui.size * 1.3)).child("App Updates"))
            .when(self.update_preview.is_some(), |panel| {
                panel.child(div().debug_selector(|| "app-update-preview".into()).child(
                    "QA preview: Download simulates Ready; Install and Restart only closes this panel. No network, installation, or settings changes.",
                ))
            })
            .child(div().truncate().child(format!("Current version: {APP_VERSION}")))
            .child(div().truncate().child(format!("Latest version: {latest}")))
            .child(div().child(message))
            .child(div().text_color(rgb(self.theme.muted)).child(
                "Signed archive manifests verify downloads. Installation requires approval. Daemon sessions keep running.",
            ))
            .child(div().text_color(rgb(self.theme.muted)).child(
                "Close does not cancel downloads or an approved restart. Use Cancel to request cancellation.",
            ));
        let mut buttons = div().flex().flex_wrap().gap(px(8.));
        if let Some(action) = action {
            let label = match action {
                UpdateAction::Check if matches!(state, State::Error(_)) => "Retry",
                UpdateAction::Check => "Check for Updates",
                UpdateAction::Download => "Download",
                UpdateAction::Install => "Install and Restart",
                UpdateAction::Cancel => "Cancel",
            };
            buttons = buttons.child(
                div()
                    .id("app-update-action")
                    .debug_selector(|| "app-update-action".into())
                    .p(px(8.))
                    .rounded(px(3.))
                    .bg(rgb(self.theme.active))
                    .cursor_pointer()
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        // Preview actions must never reach the service, including cancellation.
                        if let Some(state) = this.update_preview.take() {
                            match state {
                                State::Available { version } => {
                                    this.update_preview = Some(State::Ready { version });
                                }
                                State::Ready { .. } => this.dismiss_menu(window, cx),
                                _ => this.update_preview = Some(State::Idle),
                            }
                        } else {
                            // A mailbox transition cannot turn a stale Download click into approval.
                            match (action, this.updater.state()) {
                                (
                                    UpdateAction::Check,
                                    State::Idle | State::Current | State::Error(_),
                                ) => this.updater.check(),
                                (UpdateAction::Download, State::Available { .. }) => {
                                    this.updater.download()
                                }
                                (UpdateAction::Install, State::Ready { .. }) => {
                                    this.updater.install()
                                }
                                (
                                    UpdateAction::Cancel,
                                    State::Checking | State::Downloading { .. } | State::Installing,
                                ) => this.updater.cancel(),
                                _ => {}
                            }
                        }
                        cx.notify();
                    })),
            );
        }
        buttons = buttons
            .child(
                div()
                    .id("app-update-releases")
                    .debug_selector(|| "app-update-releases".into())
                    .p(px(8.))
                    .cursor_pointer()
                    .child("Manual Releases")
                    .on_click(|_, _, cx| {
                        cx.stop_propagation();
                        cx.open_url("https://github.com/penso/herdr-gpui/releases");
                    }),
            )
            .child(
                div()
                    .id("app-update-close")
                    .debug_selector(|| "app-update-close".into())
                    .p(px(8.))
                    .cursor_pointer()
                    .child("Close (Escape)")
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.dismiss_menu(window, cx);
                    })),
            );
        panel.child(buttons)
    }
}
