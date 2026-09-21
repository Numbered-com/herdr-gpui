//! Native chrome and GitHub account access.
use crate::{HerdrWindow, menu::Page};
use gpui::{prelude::*, *};

impl HerdrWindow {
    fn open_profile(&mut self, connect: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.page != Some(Page::GitHub) && !self.open_menu(window, cx) {
            return;
        }
        self.menu.page = Some(Page::GitHub);
        if connect && !self.menu.github.connected() && !self.menu.github.loading_profile() {
            self.menu.github.start(&self.config);
        }
        cx.notify();
    }

    pub(super) fn render_titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let background = rgb(self.theme.surface).blend(rgba(0xffffff1a));
        let image = self
            .menu
            .github
            .profile
            .as_ref()
            .and_then(|p| p.avatar.clone());
        render(self.theme.surface).child(
            div()
                .debug_selector(|| "titlebar-account-slot".into())
                .flex()
                .items_center()
                .justify_center()
                .flex_none()
                .w(px(40.))
                .h_full()
                .child(
                    div()
                        .id("titlebar-avatar")
                        .debug_selector(|| "titlebar-avatar".into())
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(28.))
                        .rounded_full()
                        .cursor_pointer()
                        .hover(|s| {
                            s.bg(background.blend(rgba((self.theme.foreground << 8) | 0x14)))
                        })
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_profile(true, window, cx);
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.open_profile(false, window, cx);
                            }),
                        )
                        .child(
                            div()
                                .debug_selector(|| "titlebar-avatar-circle".into())
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(if image.is_some() { 24. } else { 16. }))
                                .rounded_full()
                                .overflow_hidden()
                                .bg(background.blend(rgba((self.theme.foreground << 8) | 0x26)))
                                .when(
                                    self.menu.github.busy() || self.menu.github.loading_profile(),
                                    |s| s.border_1().border_color(rgb(self.theme.palette[3])),
                                )
                                .when(self.menu.github.failed, |s| {
                                    s.border_1().border_color(rgb(self.theme.palette[1]))
                                })
                                .map(|circle| match image {
                                    Some(image) => {
                                        circle.child(img(image).size(px(24.)).rounded_full())
                                    }
                                    None => circle.child(
                                        svg()
                                            .path("icons/user.svg")
                                            .size(px(12.))
                                            .text_color(rgb(self.theme.foreground)),
                                    ),
                                }),
                        ),
                ),
        )
    }
}

pub(super) fn render(surface: u32) -> Stateful<Div> {
    // AppKit owns dragging; GPUI's macOS backend cannot start a custom move.
    div()
        .id("titlebar")
        .debug_selector(|| "titlebar".into())
        .flex()
        .flex_none()
        .w_full()
        .h(px(34.))
        .bg(rgb(surface).blend(rgba(0xffffff1a)))
        .child(div().flex_none().w(px(80.)).h_full())
        .child(
            div()
                .debug_selector(|| "titlebar-center".into())
                .flex_1()
                .min_w_0()
                .h_full(),
        )
        .on_click(|event, window, _| {
            if event.click_count() == 2 {
                window.titlebar_double_click();
            }
        })
}

pub(super) fn options(title: &str) -> TitlebarOptions {
    TitlebarOptions {
        title: Some(title.to_owned().into()),
        appears_transparent: cfg!(target_os = "macos"),
        traffic_light_position: cfg!(target_os = "macos").then(|| point(px(9.), px(9.))),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use crate::menu::Page;
    use gpui::{Bounds, Modifiers, MouseButton, MouseDownEvent, TestAppContext, point, px, size};

    #[gpui::test]
    fn profile_slot_bounds_and_context_menu_do_not_start_auth(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        for (width, height) in [(1200., 780.), (640., 400.), (360., 400.)] {
            cx.simulate_resize(size(px(width), px(height)));
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
            assert_eq!(
                cx.debug_bounds("titlebar").unwrap(),
                Bounds::new(point(px(0.), px(0.)), size(px(width), px(34.)))
            );
            assert_eq!(
                cx.debug_bounds("titlebar-avatar").unwrap(),
                Bounds::new(point(px(width - 34.), px(3.)), size(px(28.), px(28.)))
            );
            let banner_height = if env!("HERDR_BUILD_WORKTREE") == "1" {
                22.
            } else {
                0.
            };
            assert_eq!(
                cx.debug_bounds("window-body").unwrap().top(),
                px(34. + banner_height)
            );
        }
        let bounds = cx.debug_bounds("titlebar-avatar").unwrap();
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Right,
            position: bounds.center(),
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        cx.update(|_, cx| {
            let view = view.read(cx);
            assert!(view.menu.page == Some(Page::GitHub));
            assert!(!view.menu.github.busy());
            assert!(!view.menu.github.loading_profile());
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.dismiss_menu(window, cx);
                view.menu.github = crate::github::Auth::connected_fixture();
                view.open_profile(true, window, cx);
                assert!(view.menu.github.connected());
                assert!(!view.menu.github.busy());
            })
        });
    }
}

#[cfg(all(test, target_os = "macos"))]
#[allow(clippy::unwrap_used)]
mod native_chrome_tests {
    use gpui::{Bounds, TestAppContext, point, px, size};

    #[gpui::test]
    fn header_bounds_above_body_in_windowed_and_fullscreen(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        for fullscreen in [false, true, false] {
            cx.update(|window, _| {
                if window.is_fullscreen() != fullscreen {
                    window.toggle_fullscreen();
                }
                assert_eq!(window.is_fullscreen(), fullscreen);
            });
            for (width, height) in [(1200., 780.), (640., 400.), (360., 400.)] {
                cx.simulate_resize(size(px(width), px(height)));
                cx.run_until_parked();
                cx.update(|window, cx| {
                    window.refresh();
                    let _ = window.draw(cx);
                });
                assert_eq!(
                    cx.debug_bounds("titlebar").unwrap(),
                    Bounds::new(point(px(0.), px(0.)), size(px(width), px(34.)))
                );
                assert_eq!(
                    cx.debug_bounds("titlebar-account-slot").unwrap(),
                    Bounds::new(point(px(width - 40.), px(0.)), size(px(40.), px(34.)))
                );
                assert_eq!(
                    cx.debug_bounds("titlebar-avatar").unwrap(),
                    Bounds::new(point(px(width - 34.), px(3.)), size(px(28.), px(28.)))
                );
                assert_eq!(
                    cx.debug_bounds("titlebar-avatar-circle").unwrap(),
                    Bounds::new(point(px(width - 28.), px(9.)), size(px(16.), px(16.)))
                );
                assert_eq!(
                    cx.debug_bounds("titlebar-center").unwrap(),
                    Bounds::new(point(px(80.), px(0.)), size(px(width - 120.), px(34.)))
                );
                let body = cx.debug_bounds("window-body").unwrap();
                let banner_height = if env!("HERDR_BUILD_WORKTREE") == "1" {
                    22.
                } else {
                    0.
                };
                assert_eq!(body.top(), px(34. + banner_height));
                assert_eq!(body.size.width, px(width));
                assert!(body.bottom() <= px(height));
            }
        }
    }
}
