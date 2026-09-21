#![allow(clippy::unwrap_used)]

use super::HerdrWindow;
use crate::{
    WINDOW_TITLE,
    controls::Command,
    sidebar::layout_tests::{fixture_window, snapshot},
};
use std::sync::Arc;

fn main_windows(cx: &mut gpui::App) -> Vec<gpui::WindowHandle<HerdrWindow>> {
    cx.windows()
        .iter()
        .filter_map(gpui::AnyWindowHandle::downcast::<HerdrWindow>)
        .collect()
}

#[gpui::test]
fn new_window_adds_one_client_of_the_same_target_without_disturbing_the_first(
    cx: &mut gpui::TestAppContext,
) {
    let (view, cx) = cx.add_window_view(fixture_window);
    let target = view.update(cx, |view, _| view.endpoints[0].connection.target.clone());
    let before = cx.update(|_, cx| main_windows(cx));
    assert_eq!(before.len(), 1);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| view.command(Command::NewWindow, window, cx));
    });
    cx.run_until_parked();
    let opened = cx.update(|_, cx| main_windows(cx));
    assert_eq!(opened.len(), 2, "one more window onto the same daemon");
    let second = opened
        .into_iter()
        .find(|handle| !before.contains(handle))
        .unwrap();
    let first_inbox = view.update(cx, |view, _| view.endpoints[0].connection.inbox.clone());
    cx.update(|_, cx| {
        // The new window is a separate client: its own endpoint and inbox.
        second
            .update(cx, |second, _, _| {
                assert_eq!(second.endpoints.len(), 1);
                assert_eq!(second.endpoints[0].connection.target, target);
                assert!(!Arc::ptr_eq(
                    &second.endpoints[0].connection.inbox,
                    &first_inbox
                ));
            })
            .unwrap();
    });
    // The originating window keeps its own selection and error state.
    view.read_with(cx, |view, _| {
        assert_eq!(view.selected_endpoint, 0);
        assert!(view.local_error.is_none());
    });
}

#[gpui::test]
fn window_title_follows_the_focused_space_of_that_window(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(fixture_window);
    cx.update(|window, cx| {
        view.update(cx, |view, _| {
            let mut snapshot = snapshot(4);
            snapshot.focused_workspace_id = Some("w0".into());
            view.live.snapshot = Some(Arc::new(snapshot));
        });
        view.update(cx, |view, _| view.sync_window_title(window));
    });
    assert_eq!(
        view.read_with(cx, |view, _| view.title.clone()),
        format!("{WINDOW_TITLE} \u{2014} herdr")
    );
    // An unknown focus falls back to the bare product name.
    cx.update(|window, cx| {
        view.update(cx, |view, _| {
            view.live.snapshot = None;
            view.sync_window_title(window);
        });
    });
    assert_eq!(
        view.read_with(cx, |view, _| view.title.clone()),
        WINDOW_TITLE
    );
}
