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
use gpui::{Context, Entity, Task, size};
use herdr_client::protocol::*;
#[cfg(test)]
use herdr_client::{ConnectOptions, ConnectTarget};
#[cfg(test)]
use std::sync::{Arc, Mutex};

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
            assert_eq!(bounds.left(), parent.left() + px(super::CHILD_INDENT));
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
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.0.clone()
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
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        crate::bind_keys(cx);
        // Deliberately do not call HerdrWindow::new: it connects and starts polling.
        let view = cx.new(|cx| HerdrWindow {
            target: ConnectTarget::Socket("/unused-layout-test.sock".into()),
            handle: None,
            inbox: Arc::new(Mutex::new(LiveState::default())),
            live: {
                let mut live = LiveState::default();
                live.snapshot = Some(Arc::new(snapshot(40)));
                live
            },
            focus: cx.focus_handle(),
            options: ConnectOptions::default(),
            sent_size: None,
            active: false,
            sent_focus: None,
            bounds: Bounds::default(),
            cell_width: 9.,
            painter: Default::default(),
            config: Default::default(),
            theme: Default::default(),
            marked: String::new(),
            local_error: None,
            menu: crate::menu::MenuState::new(cx),
            collapsed_repos: Default::default(),
            sidebar_visible: true,
            wheel: WheelAccumulator::default(),
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
        assert_eq!(name.left(), parent.left() + px(super::CHILD_INDENT));
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
    assert!(cx.debug_bounds("menu-reload GUI config").is_some());
    cx.simulate_keystrokes("down enter");
    cx.update(|window, cx| {
        window.draw(cx).clear();
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Keybinds));
    });
    let panel = cx.debug_bounds("menu-panel").unwrap();
    assert_eq!(panel.size.width, px(480.));
    assert_eq!(panel.center(), gpui::point(px(400.), px(300.)));
    let first_description = cx.debug_bounds("description-New Workspace").unwrap();
    for (keys, label) in [
        ("keys-New Workspace", "description-New Workspace"),
        ("keys-New Tab", "description-New Tab"),
        ("keys-Split Right", "description-Split Right"),
        ("keys-Split Down", "description-Split Down"),
    ] {
        let keys = cx.debug_bounds(keys).unwrap();
        let label = cx.debug_bounds(label).unwrap();
        assert!(keys.right() < label.left());
        assert_eq!(label.left(), first_description.left());
        assert!(label.right() < panel.right());
    }
    cx.simulate_resize(size(px(360.), px(240.)));
    cx.update(|window, cx| {
        window.draw(cx).clear();
    });
    let panel = cx.debug_bounds("menu-panel").unwrap();
    assert_eq!(panel.size.width, px(328.));
    assert!(panel.size.height <= px(208.));
    assert_eq!(panel.center(), gpui::point(px(180.), px(120.)));
    let header = cx.debug_bounds("keybinds-header").unwrap();
    let footer = cx.debug_bounds("keybinds-footer").unwrap();
    let body = cx.debug_bounds("keybinds-body").unwrap();
    assert!(body.size.height > px(0.));
    assert!(header.bottom() <= body.top());
    assert!(body.bottom() <= footer.top());
    assert!(footer.bottom() <= panel.bottom());
    let first_row = cx.debug_bounds("shortcut-New Workspace").unwrap();
    cx.simulate_keystrokes("pagedown");
    cx.update(|window, cx| window.draw(cx).clear());
    assert!(cx.debug_bounds("shortcut-New Workspace").unwrap().top() < first_row.top());
    assert_eq!(cx.debug_bounds("keybinds-header").unwrap(), header);
    assert_eq!(cx.debug_bounds("keybinds-footer").unwrap(), footer);
    let close = cx.debug_bounds("keybinds-close").unwrap();
    cx.simulate_click(close.center(), Default::default());
    cx.update(|window, cx| {
        assert!(view.read(cx).menu.page.is_none());
        assert!(view.read(cx).focus.is_focused(window));
        view.update(cx, |view, cx| view.open_keybinds(window, cx));
        window.draw(cx).clear();
    });
    assert_eq!(
        cx.debug_bounds("shortcut-New Workspace").unwrap(),
        first_row
    );
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        window.draw(cx).clear();
        assert!(view.read(cx).menu.page.is_none());
    });
    cx.simulate_click(menu.center(), Default::default());
    cx.update(|window, cx| {
        window.draw(cx).clear();
    });
    cx.simulate_click(gpui::point(px(700.), px(500.)), Default::default());
    cx.update(|_, cx| assert!(view.read(cx).menu.page.is_none()));

    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.config.sidebar.size = 24.;
            view.marked = "composition".into();
            view.open_keybinds(window, cx);
            assert!(view.marked.is_empty());
        });
        window.draw(cx).clear();
        assert!(!view.read(cx).focus.is_focused(window));
    });
    let line_height = cx.update(|_, cx| super::line_height(&view.read(cx).config.sidebar));
    assert_eq!(
        cx.debug_bounds("row-herdr").unwrap().size.height,
        px(2. * line_height + 8.)
    );
    assert_eq!(
        cx.debug_bounds("name-herdr").unwrap().size.height,
        px(line_height)
    );
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));
    cx.simulate_keystrokes("cmd-/");
    let shortcut_search = cx.update(|window, cx| {
        window.draw(cx).clear();
        let search = view.read(cx).menu.keybinds_search.as_ref().unwrap().clone();
        assert!(search.read(cx).focus.is_focused(window));
        search
    });
    cx.simulate_input("pane zoom");
    cx.update(|window, cx| {
        window.draw(cx).clear();
        assert_eq!(shortcut_search.read(cx).text(), "pane zoom");
    });
    assert!(cx.debug_bounds("shortcut-Toggle Pane Zoom").is_some());
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("no-shortcut-matches-xyz");
    cx.update(|window, cx| window.draw(cx).clear());
    assert!(cx.debug_bounds("keybinds-empty").is_some());
    cx.simulate_keystrokes("cmd-w");
    cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(crate::menu::Page::Keybinds)));
    cx.simulate_keystrokes("escape cmd-/");
    cx.update(|window, cx| {
        window.draw(cx).clear();
        let search = view.read(cx).menu.keybinds_search.as_ref().unwrap().clone();
        assert!(search.read(cx).text().is_empty());
        search.update(cx, |input, cx| {
            gpui::EntityInputHandler::replace_and_mark_text_in_range(
                input,
                None,
                "pane",
                Some(4..4),
                window,
                cx,
            )
        });
    });
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Keybinds));
        let search = view.read(cx).menu.keybinds_search.as_ref().unwrap().clone();
        search.update(cx, |input, cx| {
            gpui::EntityInputHandler::unmark_text(input, window, cx)
        });
    });
    cx.simulate_keystrokes("escape cmd-,");
    cx.simulate_resize(size(px(360.), px(240.)));
    cx.update(|window, cx| window.draw(cx).clear());
    let header = cx.debug_bounds("preferences-header").unwrap();
    let footer = cx.debug_bounds("preferences-footer").unwrap();
    let body = cx.debug_bounds("preferences-body").unwrap();
    let theme_row = cx.debug_bounds("preferences-theme").unwrap();
    assert!(body.size.height > px(0.));
    assert!(header.bottom() <= body.top());
    assert!(body.bottom() <= footer.top());
    cx.simulate_keystrokes("pagedown");
    cx.update(|window, cx| window.draw(cx).clear());
    assert!(cx.debug_bounds("preferences-theme").unwrap().top() < theme_row.top());
    assert_eq!(cx.debug_bounds("preferences-header").unwrap(), header);
    assert_eq!(cx.debug_bounds("preferences-footer").unwrap(), footer);
    let close = cx.debug_bounds("preferences-close").unwrap();
    cx.simulate_click(close.center(), Default::default());
    cx.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.simulate_keystrokes("cmd-,");
    cx.update(|window, cx| window.draw(cx).clear());
    let choose_theme = cx.debug_bounds("preferences-choose-theme").unwrap();
    cx.simulate_click(choose_theme.center(), Default::default());
    cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(crate::menu::Page::Themes)));
    cx.simulate_keystrokes("escape");

    let search = cx.update(|window, cx| {
        view.update(cx, |view, cx| view.open_theme_picker(window, cx));
        window.draw(cx).clear();
        let search = view.read(cx).menu.themes.as_ref().unwrap().search.clone();
        assert!(search.read(cx).focus.is_focused(window));
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("catppuccin mocha".into()));
        search
    });
    cx.simulate_keystrokes("cmd-v");
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear();
        assert_eq!(search.read(cx).text(), "catppuccin mocha");
        assert!(view.read(cx).marked.is_empty());
    });
    assert!(cx.debug_bounds("theme-name-Catppuccin Mocha").is_some());
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx).menu.themes.as_ref().unwrap().filtered,
            ["Catppuccin Mocha"]
        );
    });
    cx.simulate_keystrokes("cmd-a n o r d");
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear();
        assert_eq!(search.read(cx).text(), "nord");
        assert!(
            view.read(cx)
                .menu
                .themes
                .as_ref()
                .unwrap()
                .filtered
                .iter()
                .all(|name| name.to_lowercase().contains("nord"))
        );
    });
    cx.simulate_keystrokes("cmd-a");
    cx.update(|_, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("no-such-theme-xyz".into()))
    });
    cx.simulate_keystrokes("cmd-v");
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear());
    assert!(cx.debug_bounds("theme-empty").is_some());
    // Enter with no results must neither write a config nor dismiss the picker.
    cx.simulate_keystrokes("down enter");
    cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(crate::menu::Page::Themes)));
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        assert!(view.read(cx).focus.is_focused(window));
        view.update(cx, |view, cx| view.open_theme_picker(window, cx));
        window.draw(cx).clear();
        assert!(search.read(cx).text().is_empty());
    });
    cx.update(|window, cx| {
        search.update(cx, |search, cx| {
            gpui::EntityInputHandler::replace_and_mark_text_in_range(
                search,
                None,
                "Nord",
                Some(4..4),
                window,
                cx,
            );
        });
        window.draw(cx).clear();
    });
    cx.simulate_keystrokes("enter");
    cx.update(|_, cx| {
        assert!(
            view.read(cx).menu.page == Some(crate::menu::Page::Themes),
            "IME confirmation must not apply a theme"
        );
    });
    cx.update(|window, cx| {
        search.update(cx, |search, cx| {
            gpui::EntityInputHandler::unmark_text(search, window, cx)
        });
    });
    cx.simulate_keystrokes("escape");

    cx.simulate_keystrokes("cmd-shift-p");
    let palette_search = cx.update(|window, cx| {
        window.draw(cx).clear();
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Palette));
        view.read(cx).menu.palette.as_ref().unwrap().search.clone()
    });
    // Bound native commands must not fire while a search field has focus.
    cx.simulate_keystrokes("cmd-b");
    cx.update(|_, cx| assert!(view.read(cx).sidebar_visible));
    cx.simulate_input("toggle sidebar");
    cx.update(|_, cx| assert_eq!(palette_search.read(cx).text(), "toggle sidebar"));
    cx.simulate_keystrokes("enter");
    cx.update(|window, cx| {
        window.draw(cx).clear();
        assert!(!view.read(cx).sidebar_visible);
        assert!(view.read(cx).menu.page.is_none());
        assert!(view.read(cx).focus.is_focused(window));
    });
    cx.simulate_keystrokes("cmd-b cmd-,");
    cx.update(|_, cx| {
        assert!(view.read(cx).sidebar_visible);
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Preferences));
    });
    cx.simulate_keystrokes("escape cmd-p");
    cx.update(|window, cx| {
        window.draw(cx).clear();
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Palette));
        let search = &view.read(cx).menu.palette.as_ref().unwrap().search;
        assert!(search.read(cx).text().is_empty());
    });
    cx.simulate_input("no-workspace-matches-xyz");
    cx.simulate_keystrokes("enter");
    cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(crate::menu::Page::Palette)));
    cx.simulate_keystrokes("escape");

    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.live.snapshot = Some(Arc::new(
                serde_json::from_str(include_str!(
                    "../../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
                ))
                .unwrap(),
            ));
            cx.notify();
        });
    });
    cx.simulate_keystrokes("cmd-w");
    cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(crate::menu::Page::ConfirmClose)));
    cx.simulate_keystrokes("enter");
    cx.update(|_, cx| {
        assert!(
            view.read(cx).menu.page.is_none(),
            "Enter defaults to Cancel"
        )
    });
    cx.simulate_keystrokes("cmd-shift-w tab enter");
    cx.update(|window, cx| {
        window.draw(cx).clear();
        assert!(
            view.read(cx).menu.page == Some(crate::menu::Page::ConfirmClose),
            "disconnected confirmation stays open with error"
        );
        assert!(view.read(cx).handle.is_none());
    });
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));
}
