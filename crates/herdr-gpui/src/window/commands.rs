//! Command dispatch and focus changes. A focus change is fenced behind an
//! ordered surface barrier so input cannot reach the previous pane while the
//! navigation and its projection are still in flight.

use super::HerdrWindow;
use crate::{
    controls::{self, Command},
    log_window,
    navigation::{NavigationTarget, OwnedNavigationTarget},
    open_additional_window, state,
};
use gpui::{Context, Window};
use std::time::Duration;

impl HerdrWindow {
    pub(crate) fn navigate(&mut self, target: NavigationTarget<&str>, cx: &mut Context<Self>) {
        if self.menu.page.is_some() || !self.input_ready() {
            return;
        }
        self.request_focus_change(
            "Navigate",
            Some(target.to_owned()),
            |handle, boot| match target {
                NavigationTarget::Workspace(id) => handle.focus_workspace(boot, id),
                NavigationTarget::Tab(id) => handle.focus_tab(boot, id),
                NavigationTarget::Pane(id) => handle.focus_pane(boot, id),
            },
        );
        self.marked.clear();
        cx.notify();
    }

    /// `label` is what a failure is reported as, not a method name: navigation
    /// picks its method from the target, so it reports itself by name.
    pub(crate) fn request_focus_change(
        &mut self,
        label: &str,
        focus: Option<OwnedNavigationTarget>,
        enqueue: impl FnOnce(
            &herdr_client::ClientHandle,
            &str,
        ) -> Result<String, herdr_client::SendError>,
    ) {
        if let (Some(handle), Some(snapshot)) = (
            &self.endpoints[self.selected_endpoint].connection.handle,
            &self.live.snapshot,
        ) {
            if let Err(error) = enqueue(handle, &snapshot.boot_id) {
                self.local_error = Some(format!("{label}: {error}"));
            } else {
                self.fence_focus_change(focus);
            }
        }
    }

    pub(crate) fn fence_focus_change(&mut self, focus: Option<OwnedNavigationTarget>) {
        if !self.live.supports_surface {
            return;
        }
        if let (Some(handle), Some(snapshot)) = (
            &self.endpoints[self.selected_endpoint].connection.handle,
            &self.live.snapshot,
        ) && let Ok(mut state) = self.endpoints[self.selected_endpoint]
            .connection
            .inbox
            .lock()
        {
            // An ordered surface barrier prevents input hitting the previous
            // pane while navigation/creation and its projection are in flight.
            // Register the barrier under the inbox lock before input can resume.
            let (request, failed) = match handle.set_surface_active(&snapshot.boot_id, true) {
                Ok(request) => (request, false),
                Err(error) => {
                    self.local_error = Some(error.to_string());
                    (String::new(), true)
                }
            };
            state.activation = Some(state::SurfaceActivation {
                request,
                boot: snapshot.boot_id.clone(),
                revision: None,
                failed,
                focus,
                active: true,
            });
            state.surface = None;
            state.dirty = true;
            self.live = state.clone();
            self.activation_deadline = Some(
                std::time::Instant::now()
                    + if failed {
                        Duration::ZERO
                    } else {
                        Duration::from_secs(5)
                    },
            );
        }
    }

    pub(crate) fn command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.page.is_some() {
            return;
        }
        #[cfg(feature = "integration-test")]
        {
            self.input_probe.actions += 1;
        }
        match command {
            Command::Logs => {
                log_window::open(cx);
                return;
            }
            Command::NewWindow => {
                // Another client of the same launch target, not another daemon.
                open_additional_window(self.endpoints[0].connection.target.clone(), cx);
                return;
            }
            Command::ClosePane | Command::CloseTab => {
                self.open_close_confirmation(command, window, cx);
                return;
            }
            Command::Palette | Command::WorkspacePicker => {
                self.open_palette(command == Command::WorkspacePicker, window, cx);
                return;
            }
            Command::Keybinds => {
                self.open_keybinds(window, cx);
                return;
            }
            Command::Themes => {
                self.open_theme_picker(window, cx);
                return;
            }
            Command::Settings => {
                self.open_preferences(window, cx);
                return;
            }
            Command::About => {
                self.open_about(window, cx);
                return;
            }
            Command::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
            Command::Reconnect => self.reconnect(),
            Command::Quit => {
                cx.quit();
                return;
            }
            _ => {}
        }
        if self.activation_deadline.is_some()
            || !self.endpoints[self.selected_endpoint].surface_requested()
        {
            return;
        }
        if let Some(snapshot) = &self.live.snapshot
            && let Some((method, params)) = controls::request(command, snapshot)
        {
            self.request_focus_change(method.as_str(), None, |handle, boot| {
                handle.request(boot, method, params)
            });
            self.marked.clear();
        }
        window.focus(&self.focus);
        cx.notify();
    }
}
