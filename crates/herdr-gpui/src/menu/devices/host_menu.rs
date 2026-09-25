//! The sidebar host header's context menu. Only saved SSH devices have one:
//! Local is always present, and an explicit socket is not in the catalog.
use super::setup;
use crate::{HerdrWindow, github::Account, menu::Page};
use gpui::{prelude::*, *};
use herdr_client::ConnectTarget;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Remove,
}

const ACTIONS: [(Action, &str); 1] = [(Action::Remove, "Remove device…")];

pub(crate) struct HostMenu {
    id: String,
    label: String,
    target: String,
    session: String,
    selected: Option<usize>,
    /// Also delete the device's own GitHub sign-in. Offered only when it has
    /// one, since its account panel disappears with the device.
    forget_github: bool,
    removing: bool,
    error: Option<String>,
    task: Option<Task<()>>,
}

impl HerdrWindow {
    pub(crate) fn open_host_menu(
        &mut self,
        id: &str,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The CLI edits the default catalog, the one device setup also uses.
        if self.device_setup_unavailable().is_some() {
            return;
        }
        let Some(endpoint) = self.endpoints.iter().find(|endpoint| endpoint.id == id) else {
            return;
        };
        let ConnectTarget::Ssh { target, session } = &endpoint.connection.target else {
            return;
        };
        let menu = HostMenu {
            id: endpoint.id.clone(),
            label: endpoint.label.clone(),
            target: target.clone(),
            session: session.clone(),
            selected: None,
            forget_github: true,
            removing: false,
            error: None,
            task: None,
        };
        if !self.open_menu(window, cx) {
            return;
        }
        self.menu.anchor = anchor;
        self.menu.page = Some(Page::Host);
        self.menu.host = Some(menu);
    }

    /// Whether the device has a GitHub sign-in of its own saved on this computer.
    fn host_has_github(&self, id: &str) -> bool {
        self.menu
            .github_hosts
            .get(id)
            .is_some_and(|auth| auth.can_sign_out())
    }

    fn activate_host_menu(&mut self, action: Action, cx: &mut Context<Self>) {
        match action {
            Action::Remove => {
                if let Some(host) = &mut self.menu.host {
                    host.error = None;
                }
                self.menu.page = Some(Page::RemoveDevice);
            }
        }
        cx.notify();
    }

    fn confirm_remove_device(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(host) = &self.menu.host else {
            return;
        };
        if host.removing {
            return;
        }
        let forget = host.forget_github && self.host_has_github(&host.id);
        let store = self.menu.github_hosts.get(&host.id).map_or_else(
            || crate::github::Store::select(&self.config),
            |auth| auth.store(),
        );
        let (id, target, session) = (host.id.clone(), host.target.clone(), host.session.clone());
        // The outer result is the removal itself; the inner one, the GitHub
        // credential that only matters once the device is gone.
        let background = cx.background_executor().spawn(async move {
            let _claim = setup::claim_saved(&target, &session)?;
            setup::remove(&id)?;
            Ok::<_, crate::Error>(match Account::host(&id).filter(|_| forget) {
                Some(account) => crate::github::forget(store, &account),
                None => Ok(()),
            })
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = background.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.device_removed(result, window, cx)
            });
        });
        if let Some(host) = &mut self.menu.host {
            host.removing = true;
            host.error = None;
            host.task = Some(task);
        }
        cx.notify();
    }

    fn device_removed(
        &mut self,
        result: crate::Result<crate::Result<()>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(host) = &mut self.menu.host else {
            return;
        };
        host.removing = false;
        match result {
            Err(error) => host.error = Some(error.to_string()),
            Ok(forgotten) => {
                // The catalog watcher drops the device, and a filter or
                // selection on it falls back to Local, as for any removal.
                if host.forget_github {
                    self.menu.github_hosts.remove(&host.id);
                }
                match forgotten {
                    Ok(()) => return self.dismiss_menu(window, cx),
                    Err(error) => {
                        host.error = Some(format!(
                            "Removed the device, but could not delete its GitHub sign-in: {error}"
                        ));
                    }
                }
            }
        }
        cx.notify();
    }

    pub(in crate::menu) fn host_menu_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(host) = &mut self.menu.host else {
            return;
        };
        cx.stop_propagation();
        window.prevent_default();
        match event.keystroke.key.as_str() {
            "escape" if !host.removing => self.dismiss_menu(window, cx),
            "enter" if self.menu.page == Some(Page::RemoveDevice) => {
                self.confirm_remove_device(window, cx)
            }
            "space" if self.menu.page == Some(Page::RemoveDevice) && !host.removing => {
                host.forget_github = !host.forget_github;
                cx.notify();
            }
            key @ ("up" | "down") if self.menu.page == Some(Page::Host) => {
                host.selected = Some(match (host.selected, key) {
                    (None, "up") => ACTIONS.len() - 1,
                    (None, _) => 0,
                    (Some(i), "up") => (i + ACTIONS.len() - 1) % ACTIONS.len(),
                    (Some(i), _) => (i + 1) % ACTIONS.len(),
                });
                cx.notify();
            }
            "enter" => {
                if let Some(index) = host.selected {
                    self.activate_host_menu(ACTIONS[index].0, cx);
                }
            }
            _ => {}
        }
    }

    pub(in crate::menu) fn render_host_menu(&self, cx: &mut Context<Self>) -> Div {
        let Some(host) = &self.menu.host else {
            return div();
        };
        let theme = &self.theme;
        let mut body = div().flex().flex_col();
        if self.menu.page == Some(Page::Host) {
            for (index, (action, label)) in ACTIONS.into_iter().enumerate() {
                body = body.child(
                    div()
                        .id(("host-menu-action", index))
                        .debug_selector(move || format!("host-menu-{index}"))
                        .min_h(px(self.config.ui.line_height() + 12.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .text_color(crate::menu::danger(theme))
                        .when(host.selected == Some(index), |row| {
                            row.bg(rgb(theme.active))
                        })
                        .hover(|row| row.bg(rgb(theme.active)))
                        .child(label)
                        .on_hover(cx.listener(move |this, hovered, _, cx| {
                            if *hovered && let Some(host) = &mut this.menu.host {
                                host.selected = Some(index);
                                cx.notify();
                            }
                        }))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.activate_host_menu(action, cx)),
                        ),
                );
            }
            return body;
        }
        let github = self.host_has_github(&host.id);
        body = body
            .debug_selector(|| "remove-device".into())
            .p(px(8.))
            .gap(px(12.))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Remove device"),
            )
            .child(format!(
                "Remove {} ({} · {}) from this computer? Herdr keeps running on that host.",
                host.label, host.target, host.session
            ))
            .when(github, |body| {
                body.child(
                    div()
                        .id("remove-device-github")
                        .debug_selector(|| "remove-device-github".into())
                        .flex()
                        .gap(px(8.))
                        .cursor_pointer()
                        .child(if host.forget_github { "☑" } else { "☐" })
                        .child("Also delete its GitHub sign-in from this computer")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(host) = &mut this.menu.host
                                && !host.removing
                            {
                                host.forget_github = !host.forget_github;
                                cx.notify();
                            }
                        })),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("remove-device-cancel")
                            .p(px(6.))
                            .cursor_pointer()
                            .child("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.menu.host.as_ref().is_some_and(|host| !host.removing) {
                                    this.dismiss_menu(window, cx);
                                }
                            })),
                    )
                    .child(
                        div()
                            .id("remove-device-submit")
                            .debug_selector(|| "remove-device-submit".into())
                            .p(px(6.))
                            .rounded(px(crate::config::corners::CONTROL))
                            .bg(rgb(theme.active))
                            .text_color(crate::menu::danger(theme))
                            .cursor_pointer()
                            .child(if host.removing {
                                "Removing…"
                            } else {
                                "Remove"
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm_remove_device(window, cx)
                            })),
                    ),
            );
        body.when_some(host.error.clone(), |body, error| {
            body.child(div().text_color(crate::menu::danger(theme)).child(error))
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{Action, HostMenu};
    use crate::{menu::Page, sidebar::layout_tests::fixture_window};
    use gpui::{Modifiers, MouseButton, TestAppContext, point, px, size};

    const HOST: &str = "0123456789abcdef0123456789abcdef";
    const HOST_HEADER: &str = "host-0123456789abcdef0123456789abcdef";
    const LOCAL_HEADER: &str = "host-local";

    fn add_host(view: &mut crate::HerdrWindow) {
        view.endpoints[0].connection.target = herdr_client::ConnectTarget::Local;
        // Fold Local's many fixture workspaces so the new host's header is on screen.
        view.endpoints[0].collapsed = true;
        view.endpoints.push(crate::endpoint::Endpoint::new(
            HOST.into(),
            "m5max-ms".into(),
            herdr_client::ConnectTarget::Ssh {
                target: "penso@box".into(),
                session: "default".into(),
            },
            true,
        ));
    }

    fn host(view: &crate::HerdrWindow) -> &HostMenu {
        view.menu.host.as_ref().unwrap()
    }

    #[gpui::test]
    fn right_clicking_a_saved_host_offers_removal_but_local_has_no_menu(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            crate::bind_keys(cx);
            let mut view = fixture_window(window, cx);
            add_host(&mut view);
            view
        });
        cx.simulate_resize(size(px(800.), px(600.)));
        cx.update(|window, cx| crate::sidebar::layout_tests::full_draw(window, cx).clear(cx));
        let local = cx.debug_bounds(LOCAL_HEADER).unwrap();
        cx.simulate_mouse_down(local.center(), MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(local.center(), MouseButton::Right, Modifiers::default());
        assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));

        let remote = cx.debug_bounds(HOST_HEADER).unwrap();
        cx.simulate_mouse_down(remote.center(), MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(remote.center(), MouseButton::Right, Modifiers::default());
        view.read_with(cx, |view, _| {
            assert_eq!(view.menu.page, Some(Page::Host));
            assert_eq!(host(view).target, "penso@box");
        });
        cx.simulate_keystrokes("down enter");
        assert!(view.read_with(cx, |view, _| view.menu.page == Some(Page::RemoveDevice)));
        cx.update(|window, cx| crate::sidebar::layout_tests::full_draw(window, cx).clear(cx));
        assert!(cx.debug_bounds("remove-device").is_some());
        // Without a GitHub sign-in of its own, there is nothing else to delete.
        assert!(cx.debug_bounds("remove-device-github").is_none());
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));
    }

    #[gpui::test]
    fn removal_offers_the_github_sign_in_and_reports_each_outcome(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                add_host(view);
                view.menu
                    .github_hosts
                    .insert(HOST.into(), crate::github::Auth::connected_fixture());
                view.open_host_menu(HOST, point(px(10.), px(10.)), window, cx);
                view.activate_host_menu(Action::Remove, cx);
            });
            crate::sidebar::layout_tests::full_draw(window, cx).clear(cx);
        });
        assert!(cx.debug_bounds("remove-device-github").is_some());
        cx.simulate_keystrokes("space");
        assert!(view.read_with(cx, |view, _| !host(view).forget_github));
        cx.simulate_keystrokes("space");
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                // A failed removal keeps the dialog open with its reason.
                view.device_removed(Err(crate::Error::DeviceAdding), window, cx);
                assert_eq!(view.menu.page, Some(Page::RemoveDevice));
                assert_eq!(
                    host(view).error.as_deref(),
                    Some("This host is already being added.")
                );
                // The device is gone even when its credential could not be.
                view.device_removed(Ok(Err(crate::Error::CredentialPolicy)), window, cx);
                assert!(!view.menu.github_hosts.contains_key(HOST));
                assert!(
                    host(view)
                        .error
                        .as_deref()
                        .unwrap()
                        .starts_with("Removed the device, but")
                );
                view.device_removed(Ok(Ok(())), window, cx);
                assert!(view.menu.page.is_none());
                assert!(view.menu.host.is_none());
                assert!(view.focus.is_focused(window));
            });
        });
    }
}
