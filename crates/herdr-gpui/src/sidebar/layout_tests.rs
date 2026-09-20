//! Paint-phase probes shared by the headless layout and native full-window tests.
//! Headless NoopTextSystem ignores font-run lengths, so only the native smoke
//! test can catch GPUI's stale truncation runs. Keep headless checks for geometry.
#![allow(clippy::unwrap_used)]
#[cfg(test)]
use super::HerdrWindow;
#[cfg(test)]
use crate::{LiveState, WheelAccumulator};
use gpui::{
    App, Bounds, ElementId, Global, GlobalElementId, InspectorElementId, LayoutId, Pixels,
    SharedString, TextLayout, Window, prelude::*, px,
};
#[cfg(test)]
use gpui::{Context, Entity, Task, div, size};
use herdr_client::protocol::*;
#[cfg(test)]
use herdr_client::{ConnectOptions, ConnectTarget};
#[cfg(test)]
use std::sync::Arc;

#[derive(Default)]
struct TextProbes(std::collections::BTreeMap<String, (Bounds<Pixels>, String, Pixels)>);
impl Global for TextProbes {}

#[derive(Default)]
pub(crate) struct PaintedProbes(pub std::collections::BTreeMap<String, PaintedText>);
impl Global for PaintedProbes {}

#[derive(Debug)]
#[cfg_attr(not(feature = "integration-test"), allow(dead_code))]
pub(crate) struct PaintedText {
    pub bounds: Bounds<Pixels>,
    pub mask: Bounds<Pixels>,
    pub cached: String,
    pub glyph_text: String,
    pub width: Pixels,
    pub clipped: bool,
}

// Delegate every phase to the production SharedString element. Native checks
// inspect the glyph stream used by paint, not just the cached backing string.
pub(super) struct ProbeText(pub SharedString);

impl IntoElement for ProbeText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for ProbeText {
    type RequestLayoutState = TextLayout;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, TextLayout) {
        self.0.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut TextLayout,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.prepaint(id, inspector_id, bounds, state, window, cx);
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut TextLayout,
        prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0
            .paint(id, inspector_id, bounds, state, prepaint, window, cx);
        let width = state
            .line_layout_for_index(0)
            .unwrap()
            .unwrapped_layout
            .width;
        cx.default_global::<TextProbes>().0.insert(
            self.0.to_string(),
            (state.bounds(), state.wrapped_text(), width),
        );
        if !cx.has_global::<PaintedProbes>() {
            return;
        }
        // Inspect the actual native glyph stream consumed by WrappedLine::paint.
        // Its backing string can be longer than the shaped font runs (GPUI 0.2.2).
        let line = state.line_layout_for_index(0).unwrap();
        let layout = &line.unwrapped_layout;
        let text = state.text();
        let mask = window.content_mask().bounds;
        let mut glyph_text = String::new();
        let mut clipped = !line.wrap_boundaries.is_empty();
        let baseline = bounds.origin.y
            + (state.line_height() - layout.ascent - layout.descent) / 2.
            + layout.ascent;
        for run in &layout.runs {
            for glyph in &run.glyphs {
                let ch = text[glyph.index..].chars().next().unwrap();
                let ink = cx
                    .text_system()
                    .typographic_bounds(run.font_id, layout.font_size, ch)
                    .unwrap();
                let left = bounds.origin.x + glyph.position.x + ink.origin.x;
                let right = left + ink.size.width;
                let top = baseline + glyph.position.y - ink.bottom();
                let bottom = top + ink.size.height;
                if ink.size.width > px(0.) && ink.size.height > px(0.) {
                    clipped |= left < mask.left()
                        || right > mask.right()
                        || top < mask.top()
                        || bottom > mask.bottom();
                }
                // Resolve the ID independently, not merely its cached source index.
                let expected = window.text_system().shape_line(
                    ch.to_string().into(),
                    layout.font_size,
                    &[window.text_style().to_run(ch.len_utf8())],
                    None,
                );
                assert_eq!(
                    expected.runs[0].glyphs[0].id, glyph.id,
                    "painted glyph ID for {ch:?}"
                );
                glyph_text.push(ch);
            }
        }
        // These extra fixture rows leave the original smoke/performance labels
        // untouched. Check their native glyphs whenever the whole row is visible.
        if matches!(
            self.0.as_ref(),
            "sidebar-child" | "sidebar-child-with-a-long-readable-branch-name"
        ) && bounds.top() >= mask.top()
            && bounds.bottom() <= mask.bottom()
        {
            let parent = &cx.global::<TextProbes>().0["agent-launcher"].0;
            assert_eq!(
                bounds.left(),
                parent.left() + px(super::CHILD_INDENT - super::ICON_RESERVE)
            );
            assert_eq!(
                bounds.size.width,
                px(super::LABEL_WIDTH - super::CHILD_INDENT - super::ARROW_RESERVE)
            );
            assert_eq!(mask.size.width, bounds.size.width);
            assert_eq!(glyph_text, state.wrapped_text());
            assert!(!clipped, "child glyphs clipped: {glyph_text}");
            if self.0.as_ref() == "sidebar-child" {
                assert_eq!(glyph_text, "sidebar-child");
            } else {
                assert!(glyph_text.starts_with("sidebar-child"));
                assert!(glyph_text.ends_with('\u{2026}'));
                assert!(width > px(150.));
            }
            eprintln!("SIDEBAR child verified: {glyph_text}");
        }
        cx.default_global::<PaintedProbes>()
            .0
            .entry(self.0.to_string())
            .or_insert(PaintedText {
                bounds,
                mask,
                cached: state.wrapped_text(),
                glyph_text,
                width,
                clipped,
            });
    }
}

#[cfg(test)]
struct SidebarFixture(Entity<HerdrWindow>);

#[cfg(test)]
impl Render for SidebarFixture {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.0.update(cx, |view, cx| {
            div()
                .size_full()
                .relative()
                .flex()
                .child(view.render_sidebar(window, cx))
                .when(view.menu.page.is_some(), |root| {
                    root.child(view.render_menu(window, cx))
                })
        })
    }
}

pub(crate) fn snapshot(workspace_count: usize) -> ClientShellSnapshot {
    serde_json::from_value(serde_json::json!({
        "boot_id": "layout-test", "revision": 1,
        "update_install_command": "", "latest_release_notes_available": false,
        "integration_updates_available": false, "worktree_directory": "",
        "tab_bar_right": [], "tab_bar_right_separator": "", "agent_order": [],
        "tabs": [], "panes": [], "commands": [],
        "workspaces": (0..workspace_count).map(|i| serde_json::json!({
            "workspace_id": format!("w{i}"), "active_tab_id": "t", "new_workspace_cwd": "/tmp",
            "number": i + 1,
            "label": match i { 0 => "herdr", 1 => "herdr-gpui-sidebar-rendering-regression-investigation", 3..=5 => "agent-launcher", _ => "another workspace" },
            "custom_label": false,
            "branch": match i { 0 => "main", 2 => "1256789", 3 => "develop", 4 => "worktree/sidebar-child", 5 => "worktree/sidebar-child-with-a-long-readable-branch-name", _ => "fix/sidebar-label-width-and-overflow-regression" },
            "worktree": if (3..=5).contains(&i) { serde_json::json!({
                "key": "/fixture/agent-launcher/.git", "label": "agent-launcher", "is_linked_worktree": i != 3
            }) } else { serde_json::Value::Null },
            "tokens": [], "focused": i == 0, "agent_status": "working"
        })).collect::<Vec<_>>(),
        "agents": (["review", "Investigate sidebar rendering and verify long agent labels"].into_iter().enumerate().map(|(i, name)| serde_json::json!({
            "pane_id": format!("p{i}"), "workspace_id": "w0", "tab_id": "t",
            "name": name, "display_agent": if i == 0 { "Claude Code" } else { "agent" }, "agent": "claude",
            "agent_status": "working", "state_change_seq": 0, "state_labels": [],
            "tokens": [], "focused": false
        })).collect::<Vec<_>>())
    })).unwrap()
}

#[gpui::test]
fn sidebar_allocates_text_width(cx: &mut gpui::TestAppContext) {
    use gpui::{Modifiers, MouseButton, MouseDownEvent, point};
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        // Deliberately do not call HerdrWindow::new: it connects and starts polling.
        let view = cx.new(|cx| HerdrWindow {
            connection: crate::connection::ConnectionBridge::new(ConnectTarget::Socket(
                "/unused-layout-test.sock".into(),
            )),
            live: {
                let mut live = LiveState::default();
                live.snapshot = Some(Arc::new(snapshot(40)));
                live
            },
            focus: cx.focus_handle(),
            options: ConnectOptions::default(),
            last_queued_options: None,
            active: false,
            sent_focus: None,
            bounds: Bounds::default(),
            cell_width: 9.,
            painter: Default::default(),
            marked: String::new(),
            local_error: None,
            menu: crate::menu::MenuState::new(cx),
            install_warning_shown: false,
            collapsed_repos: Default::default(),
            wheel: WheelAccumulator::default(),
            sidebar_width: None,
            sidebar_drag: None,
            sidebar_preferences: None,
            sidebar_modified: false,
            avatars: None,
            #[cfg(feature = "integration-test")]
            input_probe: crate::smoke::InputProbe::default(),
            #[cfg(feature = "integration-test")]
            sidebar_scroll: Default::default(),
            _poll: Task::ready(()),
            _activation: cx.observe_window_activation(window, |_, _, _| {}),
        });
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        SidebarFixture(view)
    });
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    cx.update(|_, cx| {
        for (input, (bounds, rendered, width)) in &cx.global::<TextProbes>().0 {
            eprintln!(
                "text {input:?}: bounds={bounds:?}, rendered={rendered:?}, glyph width={width:?}"
            );
            assert!(
                *width <= bounds.size.width,
                "glyphs must fit the allocation"
            );
        }
        for input in ["herdr", "main", "review", "Claude Code"] {
            let (bounds, rendered, _) = &cx.global::<TextProbes>().0[input];
            assert_eq!(
                rendered, input,
                "short label must not ellipsize: {bounds:?}"
            );
        }
        for input in [
            "herdr-gpui-sidebar-rendering-regression-investigation",
            "fix/sidebar-label-width-and-overflow-regression",
            "Investigate sidebar rendering and verify long agent labels",
        ] {
            let (bounds, rendered, width) = &cx.global::<TextProbes>().0[input];
            assert!(bounds.size.width > px(150.));
            assert!(*width > px(150.), "long labels must use available width");
            assert_eq!(bounds.size.height, px(16.));
            assert!(
                rendered.ends_with('\u{2026}'),
                "long label must ellipsize: {rendered:?}"
            );
            let prefix = rendered.trim_end_matches('\u{2026}');
            assert!(prefix.len() > 10 && input.starts_with(prefix));
            assert!(rendered.len() < input.len());
            assert!(!rendered.contains('\n'));
        }
    });

    let sidebar = cx.debug_bounds("sidebar").unwrap();
    let spaces = cx.debug_bounds("spaces-scroll").unwrap();
    let agents = cx.debug_bounds("agents-scroll").unwrap();
    assert_eq!(sidebar.size.width, px(232.));
    let icon = cx.debug_bounds("github-herdr").unwrap();
    let title = cx.debug_bounds("name-herdr").unwrap();
    let detail = cx.debug_bounds("detail-herdr").unwrap();
    assert_eq!(icon.size, size(px(12.), px(12.)));
    assert_eq!(title.left(), icon.right() + px(6.));
    assert_eq!(icon.left(), detail.left());
    assert_eq!(title.right(), detail.right());
    assert!(cx.debug_bounds("github-agent-launcher").is_some());
    assert!(cx.debug_bounds("github-sidebar-child").is_none());
    assert!(cx.debug_bounds("github-review").is_none());
    assert!(spaces.size.height > px(200.));
    assert!(agents.size.height > px(200.));
    assert!(agents.bottom() <= sidebar.bottom());
    let parent = cx.debug_bounds("name-agent-launcher").unwrap();
    for (name, detail) in [
        ("name-sidebar-child", "detail-sidebar-child"),
        (
            "name-sidebar-child-with-a-long-readable-branch-name",
            "detail-sidebar-child-with-a-long-readable-branch-name",
        ),
    ] {
        let name = cx.debug_bounds(name).unwrap();
        let detail = cx.debug_bounds(detail).unwrap();
        assert_eq!(
            name.left(),
            parent.left() + px(super::CHILD_INDENT - super::ICON_RESERVE)
        );
        assert_eq!(
            name.size.width,
            px(super::LABEL_WIDTH - super::CHILD_INDENT - super::ARROW_RESERVE)
        );
        assert_eq!(name.right(), parent.right());
        assert_eq!(detail.size.width, name.size.width);
        assert_eq!(name.size.height, px(16.));
    }

    for (row, column, name, detail) in [
        ("row-herdr", "column-herdr", "name-herdr", "detail-herdr"),
        (
            "row-herdr-gpui-sidebar-rendering-regression-investigation",
            "column-herdr-gpui-sidebar-rendering-regression-investigation",
            "name-herdr-gpui-sidebar-rendering-regression-investigation",
            "detail-herdr-gpui-sidebar-rendering-regression-investigation",
        ),
        (
            "row-review",
            "column-review",
            "name-review",
            "detail-review",
        ),
        (
            "row-Investigate sidebar rendering and verify long agent labels",
            "column-Investigate sidebar rendering and verify long agent labels",
            "name-Investigate sidebar rendering and verify long agent labels",
            "detail-Investigate sidebar rendering and verify long agent labels",
        ),
    ] {
        let row_bounds = cx.debug_bounds(row).unwrap();
        let column_bounds = cx.debug_bounds(column).unwrap();
        let name_bounds = cx.debug_bounds(name).unwrap();
        let detail_bounds = cx.debug_bounds(detail).unwrap();
        eprintln!(
            "{row}: row={row_bounds:?}, column={column_bounds:?}, name={name_bounds:?}, detail={detail_bounds:?}"
        );
        assert!(name_bounds.size.width > px(150.), "{name}: {name_bounds:?}");
        assert!(
            detail_bounds.size.width > px(150.),
            "{detail}: {detail_bounds:?}"
        );
        assert_eq!(name_bounds.size.height, px(16.), "single-line name");
        assert_eq!(detail_bounds.size.height, px(16.), "single-line detail");
        assert_eq!(row_bounds.size.height, px(40.));
        assert!(name_bounds.right() <= sidebar.right() - px(12.));
        assert!(detail_bounds.right() <= sidebar.right() - px(12.));
        assert!(
            row_bounds.bottom() <= sidebar.bottom(),
            "visible initial rows"
        );
    }

    // Drag beyond the divider, then back to a narrower allocation. Text must
    // be remeasured in both directions rather than retaining truncated runs.
    for target in [400., 160., 480.] {
        let divider = cx.debug_bounds("sidebar-resize").unwrap();
        let start = divider.center();
        let old_width = cx.debug_bounds("sidebar").unwrap().size.width;
        let end = point(start.x + px(target) - old_width, start.y);
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| {
            fixture.update(cx, |_, cx| cx.notify());
            let _ = window.draw(cx);
        });
        assert_eq!(cx.debug_bounds("sidebar").unwrap().size.width, px(target));
        let label = cx.debug_bounds("name-herdr").unwrap();
        assert_eq!(
            label.size.width,
            px(super::LABEL_WIDTH + target - 232. - super::ICON_RESERVE)
        );
        let parent = cx.debug_bounds("name-agent-launcher").unwrap();
        let child = cx.debug_bounds("name-sidebar-child").unwrap();
        assert_eq!(
            parent.size.width,
            label.size.width - px(super::ARROW_RESERVE)
        );
        assert_eq!(
            child.size.width,
            parent.size.width - px(super::CHILD_INDENT) + px(super::ICON_RESERVE)
        );
        assert_eq!(child.right(), parent.right());
        cx.update(|_, cx| {
            for (text, (bounds, rendered, glyph_width)) in &cx.global::<TextProbes>().0 {
                // GPUI rounds available text width to physical pixels.
                assert!(*glyph_width <= bounds.size.width + px(1.), "width={target}, text={text:?}, rendered={rendered:?}, bounds={bounds:?}, glyphs={glyph_width:?}");
            }
        });
        cx.simulate_mouse_move(point(px(600.), start.y), None, Modifiers::default());
        cx.update(|window, cx| {
            fixture.update(cx, |_, cx| cx.notify());
            let _ = window.draw(cx);
        });
        assert_eq!(cx.debug_bounds("sidebar").unwrap().size.width, px(target));
    }
    cx.simulate_resize(size(px(640.), px(600.)));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(cx.debug_bounds("sidebar").unwrap().size.width, px(400.));
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(cx.debug_bounds("sidebar").unwrap().size.width, px(480.));

    let position = cx.debug_bounds("sidebar-resize").unwrap().center();
    cx.simulate_event(MouseDownEvent {
        position,
        button: MouseButton::Left,
        click_count: 2,
        ..Default::default()
    });
    cx.update(|window, cx| {
        fixture.update(cx, |_, cx| cx.notify());
        let _ = window.draw(cx);
    });
    assert_eq!(cx.debug_bounds("sidebar").unwrap().size.width, px(232.));

    let view = cx.update(|_, cx| fixture.read(cx).0.clone());
    let before = cx.update(|_, cx| {
        view.update(cx, |view, _| {
            let snapshot = Arc::make_mut(view.live.snapshot.as_mut().unwrap());
            snapshot.focused_workspace_id = Some("w4".into());
            for workspace in &mut snapshot.workspaces {
                workspace.focused = workspace.workspace_id == "w4";
            }
            view.marked = "selection must survive toggle".into();
            snapshot.clone()
        })
    });
    for collapsed in [true, false] {
        let arrow = cx.debug_bounds("collapse-3").unwrap();
        cx.simulate_click(arrow.center(), Default::default());
        cx.update(|window, cx| {
            cx.default_global::<TextProbes>().0.clear();
            window.refresh();
            window.draw(cx).clear();
            let view = view.read(cx);
            assert_eq!(view.live.snapshot.as_deref(), Some(&before));
            assert_eq!(view.marked, "selection must survive toggle");
            assert!(cx.global::<TextProbes>().0.contains_key(if collapsed {
                "\u{25b8}"
            } else {
                "\u{25be}"
            }));
            assert_eq!(
                view.collapsed_repos
                    .contains("/fixture/agent-launcher/.git"),
                collapsed
            );
            assert_eq!(
                !cx.global::<TextProbes>().0.contains_key("sidebar-child"),
                collapsed
            );
        });
    }
    let menu = cx.debug_bounds("sidebar-menu").unwrap();
    cx.simulate_click(menu.center(), Default::default());
    cx.update(|window, cx| {
        window.draw(cx).clear();
    });
    assert!(cx.debug_bounds("menu-panel").is_some());
    cx.simulate_keystrokes("down enter");
    cx.update(|window, cx| {
        window.draw(cx).clear();
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Keybinds));
    });
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        window.draw(cx).clear();
        assert!(view.read(cx).menu.page.is_none());
    });
    cx.simulate_click(menu.center(), Default::default());
    cx.update(|window, cx| {
        window.draw(cx).clear();
    });
    cx.simulate_click(point(px(700.), px(500.)), Default::default());
    cx.update(|_, cx| assert!(view.read(cx).menu.page.is_none()));
    cx.update(|window, cx| {
        view.update(cx, |view, cx| view.show_install_modal(window, cx));
    });
    cx.update(|window, cx| {
        window.draw(cx).clear();
        let view = view.read(cx);
        assert!(view.menu.page == Some(crate::menu::Page::Install));
        assert!(!view.live.missing_installation);
        assert_eq!(view.live.snapshot.as_deref(), Some(&before));
    });
    assert!(cx.debug_bounds("menu-install").is_some());
    assert!(cx.debug_bounds("menu-dismiss").is_some());
    cx.simulate_keystrokes("escape");
    cx.update(|_, cx| assert!(view.read(cx).menu.page.is_none()));
}
