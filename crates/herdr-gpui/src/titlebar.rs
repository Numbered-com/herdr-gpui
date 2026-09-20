#[cfg(target_os = "macos")]
use gpui::prelude::*;
use gpui::*;

pub(super) fn options(title: &str) -> TitlebarOptions {
    TitlebarOptions {
        title: Some(title.to_owned().into()),
        appears_transparent: cfg!(target_os = "macos"),
        traffic_light_position: cfg!(target_os = "macos").then(|| point(px(9.), px(9.))),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn render(surface: u32, foreground: u32) -> impl IntoElement {
    let background = rgb(surface).blend(rgba(0xffffff1a));
    // AppKit owns dragging. GPUI 0.2.2's macOS backend cannot start a custom move.
    div()
        .id("titlebar")
        .debug_selector(|| "titlebar".into())
        .flex()
        .flex_none()
        .w_full()
        .h(px(34.))
        .bg(background)
        .child(div().flex_none().w(px(80.)).h_full())
        .child(
            div()
                .debug_selector(|| "titlebar-center".into())
                .flex_1()
                .min_w_0()
                .h_full(),
        )
        .child(
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
                        .hover(|style| style.bg(background.blend(rgba((foreground << 8) | 0x14))))
                        // A placeholder only; do not trigger the title bar's double-click action.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .debug_selector(|| "titlebar-avatar-circle".into())
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(16.))
                                .rounded_full()
                                .bg(background.blend(rgba((foreground << 8) | 0x26)))
                                .child(
                                    svg()
                                        .path("icons/user.svg")
                                        .size(px(12.))
                                        .text_color(rgb(foreground)),
                                ),
                        ),
                ),
        )
        .on_click(|event, window, _| {
            if event.click_count() == 2 {
                window.titlebar_double_click();
            }
        })
}

#[cfg(all(test, target_os = "macos"))]
#[allow(clippy::unwrap_used)]
mod tests {
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
                assert_eq!(body.top(), px(34.));
                assert_eq!(body.size.width, px(width));
                assert!(body.bottom() <= px(height));
            }
        }
    }
}
