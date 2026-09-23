//! Exercise GUI lifecycle transitions through real, isolated client transports.
#![allow(clippy::unwrap_used)]
use super::*;
use crate::controls::Command;
use gpui::AppContext;
use herdr_client::{
    ClientEvent, Method,
    protocol::{endpoint::*, *},
};
use std::{
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    sync::atomic::AtomicU64,
};

struct Server {
    stream: UnixStream,
    path: PathBuf,
}

// Keep the tested entity out of the render tree: a terminal canvas would enqueue
// unrelated native resize requests while these tests advance the lifecycle.
struct Fixture(gpui::Entity<HerdrWindow>);
impl gpui::Render for Fixture {
    fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        gpui::div()
    }
}

impl Server {
    fn receive(&mut self) -> ClientMessage {
        read_message(&mut self.stream, MAX_FRAME_SIZE).unwrap()
    }

    fn respond(&mut self, request: &serde_json::Value) {
        let id = request["id"].as_str().unwrap();
        write_message(
            &mut self.stream,
            &ServerMessage::ClientShellEndpointResponseChunk {
                boot_id: snapshot().boot_id,
                request_id: id.into(),
                final_chunk: true,
                data: serde_json::to_vec(&serde_json::json!({"id": id, "result": {}})).unwrap(),
            },
            MAX_GRAPHICS_FRAME_SIZE,
        )
        .unwrap();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn snapshot() -> ClientShellSnapshot {
    serde_json::from_str(include_str!(
        "../../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
    ))
    .unwrap()
}

fn surface(snapshot: &ClientShellSnapshot) -> Arc<PaneSurfaceFrame> {
    Arc::new(PaneSurfaceFrame {
        boot_id: snapshot.boot_id.clone(),
        projection_revision: snapshot.revision,
        surface_revision: 1,
        frame: FrameData {
            width: 80,
            height: 24,
            cells: vec![],
            cursor: None,
            hyperlinks: vec![],
            graphics: vec![],
        },
        panes: vec![],
        splits: vec![],
        popup: None,
        graphics: Default::default(),
    })
}

/// Poll until the inbox has been projected into `live`, then return.
///
/// `ConnectionBridge::take_update` reads the inbox under `try_lock` so the UI
/// thread never blocks on the socket worker: a poll that races the worker
/// projects nothing and the next one delivers it. A test that polls once and
/// asserts is therefore reading whatever `live` happened to hold, which is a
/// revision behind whenever the worker held the lock. Drive polls until the
/// expected state lands instead of assuming a single poll suffices.
fn project_until(
    view: &mut HerdrWindow,
    cx: &mut Context<HerdrWindow>,
    what: &str,
    ready: impl Fn(&HerdrWindow) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        view.poll_endpoints(cx);
        if ready(view) {
            return;
        }
        assert!(Instant::now() < deadline, "{what} was never projected");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "client worker did not finish in time"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn connected_endpoint(id: &str) -> (Endpoint, Server) {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "hg-{}-{}.sock",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut endpoint = Endpoint::new(
        id.into(),
        id.into(),
        ConnectTarget::Socket(path.clone()),
        true,
    );
    endpoint.connect(ConnectOptions::default(), true);
    let mut accepted = None;
    wait_until(|| {
        accepted = listener.accept().ok();
        accepted.is_some()
    });
    let mut server = Server {
        stream: accepted.unwrap().0,
        path,
    };
    server.stream.set_nonblocking(false).unwrap();
    server
        .stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    server
        .stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    assert!(matches!(
        server.receive(),
        ClientMessage::EndpointControl { .. }
    ));
    let mut welcome: EndpointServerWelcome = serde_json::from_str(include_str!(
        "../../../herdr-protocol/tests/fixtures/endpoint-welcome-v1.json"
    ))
    .unwrap();
    welcome.capabilities.extend([
        "surface_interest".into(),
        "presentation_effects_fence".into(),
    ]);
    welcome.methods.extend(
        [
            Method::ClientShellSurfaceSet,
            Method::WorkspaceCreate,
            Method::TabCreate,
            Method::PaneSplit,
            Method::TabFocus,
            Method::PaneFocus,
            Method::WorkspaceFocus,
            Method::PaneFocusDirection,
            Method::PaneZoom,
            Method::PaneClose,
            Method::TabClose,
            Method::CommandInvoke,
            Method::WorkspaceClose,
            Method::WorktreeCreate,
            Method::WorktreeOpen,
            Method::WorktreeRemove,
        ]
        .map(|method| method.as_str().to_owned()),
    );
    for (kind, data) in [
        (
            ENDPOINT_WELCOME_KIND,
            serde_json::to_string(&welcome).unwrap(),
        ),
        (
            ENDPOINT_SNAPSHOT_KIND,
            serde_json::to_string(&snapshot()).unwrap(),
        ),
    ] {
        write_message(
            &mut server.stream,
            &ServerMessage::EndpointControl {
                kind: kind.into(),
                data,
            },
            MAX_GRAPHICS_FRAME_SIZE,
        )
        .unwrap();
    }
    wait_until(|| {
        endpoint.poll(Instant::now());
        endpoint.connection.handle.is_some() && endpoint.live.snapshot.is_some()
    });
    assert!(endpoint.live.supports_surface);
    let frame = surface(endpoint.live.snapshot.as_ref().unwrap());
    endpoint
        .connection
        .inbox
        .lock()
        .unwrap()
        .apply(ClientEvent::Surface(frame));
    endpoint.poll(Instant::now());
    (endpoint, server)
}

#[gpui::test]
fn startup_focus_waits_for_the_first_surface_without_flapping_on_later_updates(
    cx: &mut gpui::TestAppContext,
) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    let (mut endpoint, mut server) = connected_endpoint(LOCAL);
    endpoint.connection.inbox.lock().unwrap().surface = None;
    endpoint.live.surface = None;
    let inbox = endpoint.connection.inbox.clone();
    view.update(cx, |view, _| {
        view.endpoints = vec![endpoint];
        view.options = ConnectOptions::default();
        view.reset_selected();
        view.active = true;
        assert!(!view.input_ready());
        view.report_focus();
        assert_eq!(view.sent_focus, Some(false));
    });
    assert!(matches!(
        server.receive(),
        ClientMessage::ClientShellFocus { focused: false }
    ));

    inbox
        .lock()
        .unwrap()
        .apply(ClientEvent::Surface(surface(&snapshot())));
    view.update(cx, |view, cx| {
        project_until(view, cx, "startup surface ready", HerdrWindow::input_ready);
        view.report_focus();
        assert_eq!(view.sent_focus, Some(true));
        // New snapshot/surface pairs can arrive separately during normal activity.
        view.live.surface = None;
        view.report_focus();
        assert_eq!(view.sent_focus, Some(true));
        view.active = false;
        view.report_focus();
        assert_eq!(view.sent_focus, Some(false));
    });
    assert!(matches!(
        server.receive(),
        ClientMessage::ClientShellFocus { focused: true }
    ));
    assert!(matches!(
        server.receive(),
        ClientMessage::ClientShellFocus { focused: false }
    ));
}

#[gpui::test]
fn workspace_menu_keeps_immediate_and_deferred_navigation(cx: &mut gpui::TestAppContext) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    for deferred in [false, true] {
        let (endpoint, mut server) = connected_endpoint("local");
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.selected_endpoint = 0;
                view.endpoints = vec![endpoint];
                view.options = ConnectOptions::default();
                view.reset_selected();
                if deferred {
                    view.live.surface = None;
                }
                assert!(view.navigate_endpoint("local", NavigationTarget::Workspace("w1"), cx));
                view.open_workspace_menu("w1", Default::default(), window, cx);
                assert_eq!(view.menu.page, Some(crate::menu::Page::Workspace));
                if deferred {
                    assert!(view.pending_navigation.is_some());
                    view.live = view.endpoints[0].live.clone();
                    view.poll_endpoints(cx);
                }
                assert!(view.pending_navigation.is_none());
                assert!(!view.input_ready());
                assert_eq!(view.menu.page, Some(crate::menu::Page::Workspace));
                assert!(view.menu.focus.is_focused(window));
                // New input is still blocked while the menu owns focus.
                assert!(!view.navigate(NavigationTarget::Workspace("other"), cx));
            });
        });
        let ClientMessage::ClientShellEndpointRequest { request, .. } = server.receive() else {
            panic!("missing workspace focus request");
        };
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["method"], "workspace.focus");
        assert_eq!(request["params"], serde_json::json!({"workspace_id": "w1"}));
        server.respond(&request);
        let ClientMessage::ClientShellEndpointRequest { request, .. } = server.receive() else {
            panic!("missing surface barrier");
        };
        let barrier: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(barrier["method"], Method::ClientShellSurfaceSet.as_str());
        view.update(cx, |view, cx| {
            let mut next = snapshot();
            next.revision += 1;
            next.focused_workspace_id = Some("w1".into());
            for workspace in &mut next.workspaces {
                workspace.focused = workspace.workspace_id == "w1";
            }
            {
                let mut state = view.endpoints[0].connection.inbox.lock().unwrap();
                state.apply(ClientEvent::Snapshot(Arc::new(next.clone())));
                state.apply(ClientEvent::Surface(surface(&next)));
                state.apply(ClientEvent::Response {
                    request_id: barrier["id"].as_str().unwrap().into(),
                    response: serde_json::json!({"result": {
                        "type": "client_shell_surface_set", "active": true,
                        "projection_revision": next.revision
                    }}),
                });
            }
            project_until(view, cx, "workspace selection", HerdrWindow::input_ready);
            assert_eq!(view.menu.page, Some(crate::menu::Page::Workspace));
            let selected = view.live.snapshot.as_ref().unwrap();
            assert_eq!(selected.focused_workspace_id.as_deref(), Some("w1"));
            assert!(
                selected
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.workspace_id == "w1" && workspace.focused)
            );
        });
    }
}

#[gpui::test]
fn toast_navigation_queues_typed_targets_and_fences_input(cx: &mut gpui::TestAppContext) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    for (tab, pane, method, params) in [
        (
            None,
            None,
            "workspace.focus",
            serde_json::json!({"workspace_id":"w1"}),
        ),
        (
            Some("w1:t1"),
            None,
            "tab.focus",
            serde_json::json!({"tab_id":"w1:t1"}),
        ),
        (
            Some("w1:t1"),
            Some("w1:p1"),
            "pane.focus",
            serde_json::json!({"pane_id":"w1:p1"}),
        ),
    ] {
        let (mut endpoint, mut server) = connected_endpoint("ssh:toast");
        let mut wire = crate::notifications::tests::notification("Navigate");
        wire.workspace_id = Some("w1".into());
        wire.tab_id = tab.map(str::to_owned);
        wire.pane_id = pane.map(str::to_owned);
        endpoint
            .toasts
            .receive([crate::notifications::Notice::new(wire, Instant::now())
                .with_snapshot(endpoint.live.snapshot.as_deref())
                .preview()]);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.endpoints.truncate(1);
                view.endpoints.push(endpoint);
                view.selected_endpoint = 1;
                view.options = ConnectOptions::default();
                view.reset_selected();
                window.focus(&view.focus);
                view.marked = "composition".into();
                assert!(view.input_ready());
                view.tick_toasts(false, Instant::now());
                view.command(Command::OpenNotificationTarget, window, cx);
                assert!(view.endpoints[1].toasts.entries.is_empty());
                assert!(view.marked.is_empty());
                assert!(view.focus.is_focused(window));
                assert!(!view.input_ready());
            })
        });
        let ClientMessage::ClientShellEndpointRequest { request, .. } = server.receive() else {
            panic!("expected semantic focus request")
        };
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["method"], method);
        assert_eq!(request["params"], params);
    }
}

#[gpui::test]
fn wire_completion_waits_for_evidence_then_command_uses_original_pane(
    cx: &mut gpui::TestAppContext,
) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    let (mut endpoint, mut server) = connected_endpoint("ssh:completion");
    let mut projection = snapshot();
    projection.revision += 1;
    projection.agents[0].agent_status = AgentStatus::Working;
    write_message(
        &mut server.stream,
        &ServerMessage::EndpointControl {
            kind: ENDPOINT_SNAPSHOT_KIND.into(),
            data: serde_json::to_string(&projection).unwrap(),
        },
        MAX_GRAPHICS_FRAME_SIZE,
    )
    .unwrap();
    let mut event = crate::notifications::tests::notification("wire completion");
    event.kind = SemanticNotificationKind::Finished;
    event.workspace_id = None;
    event.pane_id = Some("w1:p1".into());
    write_message(
        &mut server.stream,
        &ServerMessage::SemanticNotification(event),
        MAX_GRAPHICS_FRAME_SIZE,
    )
    .unwrap();
    wait_until(|| {
        endpoint.poll(Instant::now());
        !endpoint.toasts.entries.is_empty()
    });
    let now = Instant::now();
    view.update(cx, |view, _| {
        view.endpoints.push(endpoint);
        view.config.notifications.enabled = true;
        view.config.notifications.delay_seconds = 0;
        view.tick_toasts(false, now);
        assert!(!view.endpoints[1].toasts.entries[0].1.visible);
    });
    projection.revision += 1;
    projection.agents[0].agent_status = AgentStatus::Done;
    write_message(
        &mut server.stream,
        &ServerMessage::EndpointControl {
            kind: ENDPOINT_SNAPSHOT_KIND.into(),
            data: serde_json::to_string(&projection).unwrap(),
        },
        MAX_GRAPHICS_FRAME_SIZE,
    )
    .unwrap();
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            wait_until(|| {
                view.endpoints[1].poll(Instant::now());
                view.endpoints[1]
                    .live
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.revision == projection.revision)
            });
            view.tick_toasts(false, now + Duration::from_millis(50));
            assert!(view.endpoints[1].toasts.entries[0].1.visible);
            view.endpoints[1]
                .connection
                .inbox
                .lock()
                .unwrap()
                .apply(ClientEvent::Surface(surface(&projection)));
            wait_until(|| {
                view.endpoints[1].poll(Instant::now());
                view.endpoints[1].live.surface.is_some()
            });
            // Select the already-active fixture surface without issuing unrelated activation requests.
            view.selected_endpoint = 1;
            view.reset_selected();
            view.command(Command::OpenNotificationTarget, window, cx);
            assert!(view.endpoints[1].toasts.entries.is_empty());
            assert!(!view.input_ready());
        })
    });
    let ClientMessage::ClientShellEndpointRequest { request, .. } = server.receive() else {
        panic!("expected completion target focus");
    };
    let request: serde_json::Value = serde_json::from_str(&request).unwrap();
    assert_eq!(request["method"], "pane.focus");
    assert_eq!(request["params"]["pane_id"], "w1:p1");
}

#[gpui::test]
fn toast_click_uses_origin_and_close_never_navigates(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
    let (mut remote, _server) = connected_endpoint("ssh:toast");
    remote.initial_surface = false;
    let mut wire = crate::notifications::tests::notification("Remote target");
    wire.workspace_id = Some("w1".into());
    let notice = crate::notifications::Notice::new(wire, Instant::now())
        .with_snapshot(remote.live.snapshot.as_deref())
        .preview();
    remote.toasts.receive([notice.clone(), notice]);
    cx.simulate_resize(gpui::size(gpui::px(1000.), gpui::px(600.)));
    cx.update(|window, cx| {
        view.update(cx, |view, _| {
            // The same IDs on Local must not win over the notification's origin.
            view.endpoints[0].live.snapshot = remote.live.snapshot.clone();
            view.endpoints[0].detached = true;
            view.endpoints.push(remote);
            window.focus(&view.focus);
            view.marked = "composition".into();
        });
        window.draw(cx).clear();
    });
    let dismiss = cx.debug_bounds("toast-dismiss-ssh:toast-0").unwrap();
    cx.simulate_click(dismiss.center(), Default::default());
    cx.update(|window, cx| {
        let view = view.read(cx);
        assert_eq!(view.selected_endpoint, 0);
        assert!(view.pending_navigation.is_none());
        assert_eq!(view.marked, "composition");
        assert!(view.focus.is_focused(window));
        assert_eq!(view.endpoints[1].toasts.entries.len(), 1);
    });
    cx.update(|window, cx| window.draw(cx).clear());
    let card = cx.debug_bounds("toast-ssh:toast-1").unwrap();
    cx.simulate_click(card.center(), Default::default());
    view.update(cx, |view, _| {
        assert_eq!(view.selected_endpoint, 1);
        assert_eq!(view.pending_toast, Some(1));
        assert_eq!(
            view.pending_navigation,
            Some(NavigationTarget::Workspace("w1".into()))
        );
        assert_eq!(view.endpoints[1].toasts.entries.len(), 1);
        // A newer inbox boot is rejected even before it reaches the UI projection.
        let inbox = view.endpoints[1].connection.inbox.clone();
        {
            let mut state = inbox.lock().unwrap();
            Arc::make_mut(state.snapshot.as_mut().unwrap()).boot_id = "replacement".into();
        }
        assert_eq!(view.toast_target(1, 1), std::task::Poll::Ready(None));
        view.endpoints[1].stop();
        assert_eq!(view.toast_target(1, 1), std::task::Poll::Ready(None));
    });
}

#[gpui::test]
fn toast_rendered_clicks_reject_replaced_removed_and_disabled_origins(
    cx: &mut gpui::TestAppContext,
) {
    let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
    cx.simulate_resize(gpui::size(gpui::px(1000.), gpui::px(600.)));
    for change in 0..4 {
        let (mut remote, _server) = connected_endpoint("ssh:toast");
        let mut wire = crate::notifications::tests::notification("old render");
        wire.workspace_id = Some("w1".into());
        remote
            .toasts
            .receive([crate::notifications::Notice::new(wire, Instant::now())
                .with_snapshot(remote.live.snapshot.as_deref())
                .preview()]);
        cx.update(|window, cx| {
            view.update(cx, |view, _| {
                view.endpoints.truncate(1);
                view.endpoints.push(remote);
            });
            window.draw(cx).clear();
        });
        assert!(cx.debug_bounds("toast-ssh:toast-0").is_some());
        let (generation, inbox) = view.read_with(cx, |view, _| {
            (
                view.endpoints[1].generation,
                view.endpoints[1].connection.inbox.clone(),
            )
        });
        view.update(cx, |view, _| match change {
            0 => view.endpoints[1].generation += 1,
            1 => {
                view.endpoints[1].connection.inbox =
                    Arc::new(Mutex::new(view.endpoints[1].live.clone()))
            }
            2 => {
                view.endpoints.pop();
            }
            _ => view.endpoints[1].enabled = false,
        });
        // Invoke the captured callback identity directly: simulate_click redraws
        // first and would correctly capture the replacement generation instead.
        view.update(cx, |view, cx| {
            view.click_toast("ssh:toast", generation, &inbox, 0, cx)
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.selected_endpoint, 0);
            assert!(view.pending_navigation.is_none());
            assert!(view.pending_toast.is_none());
        });
    }
}

#[gpui::test]
fn newer_same_endpoint_navigation_cannot_replay_a_pending_toast(cx: &mut gpui::TestAppContext) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    let (mut endpoint, mut server) = connected_endpoint("ssh:toast");
    let inbox = endpoint.connection.inbox.clone();
    let mut wire = crate::notifications::tests::notification("older intent");
    wire.workspace_id = Some("w1".into());
    wire.pane_id = Some("w1:p1".into());
    endpoint
        .toasts
        .receive([crate::notifications::Notice::new(wire, Instant::now())
            .with_snapshot(endpoint.live.snapshot.as_deref())
            .preview()]);
    view.update(cx, |view, cx| {
        view.endpoints[0].detached = true;
        view.endpoints.push(endpoint);
        view.selected_endpoint = 1;
        view.options = ConnectOptions::default();
        view.reset_selected();
        view.tick_toasts(false, Instant::now());
        let held = inbox.lock().unwrap();
        view.click_toast("ssh:toast", view.endpoints[1].generation, &inbox, 0, cx);
        assert_eq!(view.pending_toast, Some(0));
        assert_eq!(
            view.pending_navigation,
            Some(NavigationTarget::Pane("w1:p1".into()))
        );
        drop(held);
        assert!(view.navigation_ready());
        view.navigate_endpoint("ssh:toast", NavigationTarget::Pane("new-pane"), cx);
        assert!(view.pending_toast.is_none());
        assert!(view.pending_navigation.is_none());
        assert!(!view.input_ready());
    });
    let ClientMessage::ClientShellEndpointRequest { request, .. } = server.receive() else {
        panic!("missing newer navigation");
    };
    let request: serde_json::Value = serde_json::from_str(&request).unwrap();
    assert_eq!(request["method"], "pane.focus");
    assert_eq!(request["params"]["pane_id"], "new-pane");
    server.respond(&request);
    let ClientMessage::ClientShellEndpointRequest { request, .. } = server.receive() else {
        panic!("missing newer navigation barrier");
    };
    let barrier: serde_json::Value = serde_json::from_str(&request).unwrap();
    assert_eq!(barrier["method"], Method::ClientShellSurfaceSet.as_str());
    let mut next = snapshot();
    next.revision += 1;
    let mut pane = next.panes[0].clone();
    pane.pane_id = "new-pane".into();
    next.panes[0].focused = false;
    next.panes.push(pane);
    next.focused_pane_id = Some("new-pane".into());
    write_message(&mut server.stream, &ServerMessage::ClientShellEndpointResponseChunk {
        boot_id: next.boot_id.clone(),
        request_id: barrier["id"].as_str().unwrap().into(),
        final_chunk: true,
        data: serde_json::to_vec(&serde_json::json!({"id": barrier["id"], "result": {
            "type": "client_shell_surface_set", "active": true, "projection_revision": next.revision
        }})).unwrap(),
    }, MAX_GRAPHICS_FRAME_SIZE).unwrap();
    wait_until(|| {
        inbox
            .lock()
            .unwrap()
            .activation
            .as_ref()
            .and_then(|a| a.revision)
            == Some(next.revision)
    });
    {
        let mut state = inbox.lock().unwrap();
        state.apply(ClientEvent::Snapshot(Arc::new(next.clone())));
        state.apply(ClientEvent::Surface(surface(&next)));
    }
    view.update(cx, |view, cx| {
        project_until(view, cx, "newer navigation completed", |view| {
            view.live
                .snapshot
                .as_ref()
                .is_some_and(|s| s.revision == next.revision)
        });
        view.poll_endpoints(cx);
        assert!(view.pending_navigation.is_none());
        assert!(view.input_ready());
        assert_eq!(
            view.live
                .snapshot
                .as_ref()
                .unwrap()
                .focused_pane_id
                .as_deref(),
            Some("new-pane")
        );
        // FIFO input proves no old pane.focus was queued after the completed barrier.
        view.send(
            ClientPaneInputEvent::TextCommit("new intent only".into()),
            cx,
        );
    });
    let ClientMessage::ClientShellPaneInput { pane_id, .. } = server.receive() else {
        panic!("old navigation replayed instead of input to the newer target");
    };
    assert_eq!(pane_id, "new-pane");
}

#[gpui::test]
fn toast_handoff_retains_busy_validation_and_revalidates_before_queueing(
    cx: &mut gpui::TestAppContext,
) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    for (deleted, busy_click, already_active) in [
        (false, false, false),
        (false, true, false),
        (true, false, false),
        (true, true, false),
        (false, true, true),
        (true, true, true),
    ] {
        let (mut endpoint, mut server) = connected_endpoint("ssh:toast");
        endpoint.initial_surface = already_active;
        let mut wire = crate::notifications::tests::notification("handoff");
        wire.workspace_id = Some("w1".into());
        endpoint
            .toasts
            .receive([crate::notifications::Notice::new(wire, Instant::now())
                .with_snapshot(endpoint.live.snapshot.as_deref())
                .preview()]);
        view.update(cx, |view, cx| {
            view.selected_endpoint = 0;
            view.endpoints.truncate(1);
            view.endpoints[0].detached = true;
            view.endpoints.push(endpoint);
            view.options = ConnectOptions::default();
            if already_active {
                view.selected_endpoint = 1;
                view.reset_selected();
                assert!(view.input_ready());
            }
            let inbox = view.endpoints[1].connection.inbox.clone();
            view.tick_toasts(false, Instant::now());
            let held = busy_click.then(|| inbox.lock().unwrap());
            view.click_toast("ssh:toast", view.endpoints[1].generation, &inbox, 0, cx);
            drop(held);
            assert_eq!(view.pending_toast, Some(0));
            assert!(!view.input_ready());
            assert_eq!(view.endpoints[1].toasts.entries.len(), 1);
            // Complete the handoff with a coherent current projection.
            {
                let mut state = inbox.lock().unwrap();
                if deleted {
                    Arc::make_mut(state.snapshot.as_mut().unwrap())
                        .workspaces
                        .clear();
                }
                state.surface = Some(surface(state.snapshot.as_ref().unwrap()));
                state.activation = None;
                state.dirty = true;
                view.endpoints[1].initial_surface = true;
                // Project the completed handoff, then deterministically hold
                // the inbox across polling and attempted terminal input.
                view.endpoints[1].live = state.clone();
                view.live = state.clone();
                for _ in 0..2 {
                    view.poll_endpoints(cx);
                    assert!(view.navigation_ready());
                    assert!(!view.input_ready());
                    assert_eq!(view.pending_toast, Some(0));
                    assert_eq!(
                        view.pending_navigation,
                        Some(NavigationTarget::Workspace("w1".into()))
                    );
                    assert_eq!(view.endpoints[1].toasts.entries.len(), 1);
                    view.send(
                        ClientPaneInputEvent::TextCommit("must stay fenced".into()),
                        cx,
                    );
                }
            }
            project_until(view, cx, "toast handoff", |view| {
                view.pending_toast.is_none()
            });
            assert_eq!(view.endpoints[1].toasts.entries.len(), usize::from(deleted));
            assert!(view.pending_navigation.is_none());
            assert_eq!(view.input_ready(), deleted);
            if deleted {
                view.endpoints[1]
                    .connection
                    .handle
                    .as_ref()
                    .unwrap()
                    .set_focus(&snapshot().boot_id, false)
                    .unwrap();
            }
        });
        if !deleted {
            let ClientMessage::ClientShellEndpointRequest { request, .. } = server.receive() else {
                panic!("expected focus after handoff")
            };
            let request: serde_json::Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["method"], "workspace.focus");
        } else {
            assert!(matches!(
                server.receive(),
                ClientMessage::ClientShellFocus { focused: false }
            ));
        }
    }
}

#[gpui::test]
fn accepted_toast_survives_expiry_but_not_invalidation(cx: &mut gpui::TestAppContext) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    for invalidation in [
        "none",
        "dismiss",
        "replace",
        "boot",
        "membership",
        "generation",
        "overflow",
    ] {
        let (mut endpoint, mut server) = connected_endpoint("ssh:toast");
        endpoint.initial_surface = false;
        let mut wire = crate::notifications::tests::notification("accepted");
        wire.workspace_id = Some("w1".into());
        wire.pane_id = Some("w1:p1".into());
        endpoint.toasts.receive([
            crate::notifications::Notice::new(wire.clone(), Instant::now())
                .with_snapshot(endpoint.live.snapshot.as_deref())
                .preview(),
        ]);
        view.update(cx, |view, cx| {
            view.selected_endpoint = 0;
            view.endpoints.truncate(1);
            view.endpoints[0].detached = true;
            view.reset_selected();
            view.endpoints.push(endpoint);
            view.tick_toasts(false, Instant::now());
            let inbox = view.endpoints[1].connection.inbox.clone();
            view.click_toast("ssh:toast", view.endpoints[1].generation, &inbox, 0, cx);
            assert_eq!(view.pending_toast, Some(0));
            assert!(!view.input_ready());
            let after_expiry =
                view.endpoints[1].toasts.entries[0].1.expires + Duration::from_secs(1);
            view.tick_toasts(false, after_expiry);
            assert_eq!(view.endpoints[1].toasts.entries.len(), 1);
            // An accepted intent also remains eligible between timer samples.
            view.endpoints[1].toasts.entries[0].1.expires = Instant::now();
            assert!(matches!(
                view.toast_target(1, 0),
                std::task::Poll::Ready(Some(_))
            ));
            match invalidation {
                "dismiss" => view.endpoints[1].toasts.dismiss(0),
                "replace" => {
                    // Revalidate against an undrained replacement, not just the UI queue.
                    inbox.lock().unwrap().apply(ClientEvent::Message(
                        ServerMessage::SemanticNotification(wire),
                    ));
                }
                "boot" => {
                    Arc::make_mut(inbox.lock().unwrap().snapshot.as_mut().unwrap()).boot_id =
                        "other".into()
                }
                "membership" => Arc::make_mut(inbox.lock().unwrap().snapshot.as_mut().unwrap())
                    .panes
                    .clear(),
                "generation" => view.endpoints[1].stop(),
                "overflow" => {
                    let mut state = inbox.lock().unwrap();
                    state.apply(ClientEvent::Message(ServerMessage::SemanticNotification(
                        wire,
                    )));
                    for _ in 0..crate::notifications::PENDING_LIMIT {
                        state.apply(ClientEvent::Message(ServerMessage::SemanticNotification(
                            crate::notifications::tests::notification("other"),
                        )));
                    }
                }
                _ => {}
            }
            if invalidation != "generation" {
                view.endpoints[1].initial_surface = true;
                view.live = inbox.lock().unwrap().clone();
                view.live.surface = Some(surface(view.live.snapshot.as_ref().unwrap()));
                view.live.activation = None;
                view.navigate_toast(0, cx);
                assert!(view.pending_toast.is_none());
                if invalidation == "none" {
                    assert!(view.endpoints[1].toasts.entries.is_empty());
                } else {
                    view.endpoints[1]
                        .connection
                        .handle
                        .as_ref()
                        .unwrap()
                        .set_focus(&snapshot().boot_id, false)
                        .unwrap();
                }
            } else {
                assert_eq!(view.toast_target(1, 0), std::task::Poll::Ready(None));
            }
        });
        if invalidation == "none" {
            let ClientMessage::ClientShellEndpointRequest { request, .. } = server.receive() else {
                panic!("missing accepted focus")
            };
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&request).unwrap()["method"],
                "pane.focus"
            );
        } else if invalidation != "generation" {
            assert!(
                matches!(
                    server.receive(),
                    ClientMessage::ClientShellFocus { focused: false }
                ),
                "{invalidation}"
            );
        }
    }
}

#[gpui::test]
fn toast_handoff_defers_a_contended_source_without_activating_destination(
    cx: &mut gpui::TestAppContext,
) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    let (source, mut source_server) = connected_endpoint("ssh:source");
    let (mut target, mut target_server) = connected_endpoint("ssh:target");
    target.initial_surface = false;
    let source_inbox = source.connection.inbox.clone();
    let target_inbox = target.connection.inbox.clone();
    let target_handle = target.connection.handle.clone().unwrap();
    let mut wire = crate::notifications::tests::notification("target");
    wire.workspace_id = Some("w1".into());
    target
        .toasts
        .receive([crate::notifications::Notice::new(wire, Instant::now())
            .with_snapshot(target.live.snapshot.as_deref())
            .preview()]);
    let held = source_inbox.lock().unwrap();
    view.update(cx, |view, cx| {
        view.endpoints[0].detached = true;
        view.endpoints.extend([source, target]);
        view.selected_endpoint = 1;
        view.reset_selected();
        view.tick_toasts(false, Instant::now());
        view.click_toast(
            "ssh:target",
            view.endpoints[2].generation,
            &target_inbox,
            0,
            cx,
        );
        assert_eq!(view.selected_endpoint, 2);
        assert_eq!(view.pending_toast, Some(0));
        for _ in 0..2 {
            view.poll_endpoints(cx);
            assert!(matches!(
                view.pending_releases[0].phase,
                ReleasePhase::Deferred(_)
            ));
            assert!(!view.endpoints[2].initial_surface);
            assert!(!view.input_ready());
        }
    });
    target_handle.set_focus(&snapshot().boot_id, false).unwrap();
    assert!(matches!(
        target_server.receive(),
        ClientMessage::ClientShellFocus { focused: false }
    ));
    drop(held);
    view.update(cx, |view, cx| {
        project_until(view, cx, "source release queued", |view| {
            matches!(view.pending_releases[0].phase, ReleasePhase::Sent(_))
        })
    });
    assert!(matches!(
        source_server.receive(),
        ClientMessage::ClientShellFocus { focused: false }
    ));
    let ClientMessage::ClientShellEndpointRequest { request, .. } = source_server.receive() else {
        panic!("missing source release")
    };
    let request: serde_json::Value = serde_json::from_str(&request).unwrap();
    assert_eq!(request["method"], Method::ClientShellSurfaceSet.as_str());
    assert_eq!(request["params"]["active"], false);
    target_handle.set_focus(&snapshot().boot_id, true).unwrap();
    assert!(matches!(
        target_server.receive(),
        ClientMessage::ClientShellFocus { focused: true }
    ));
    source_inbox.lock().unwrap().apply(ClientEvent::Response {
        request_id: request["id"].as_str().unwrap().into(),
        response: serde_json::json!({"result": {"type": "client_shell_surface_set", "active": false, "projection_revision": 7}}),
    });
    view.update(cx, |view, cx| {
        project_until(view, cx, "source release acknowledged", |view| {
            view.endpoints[2].initial_surface
        });
        assert!(view.pending_releases.is_empty());
        assert!(view.endpoints[2].initial_surface);
        assert!(!view.input_ready());
    });
    assert!(matches!(
        target_server.receive(),
        ClientMessage::ClientShellResize { .. }
    ));
    let ClientMessage::ClientShellEndpointRequest { request, .. } = target_server.receive() else {
        panic!("missing destination activation")
    };
    let request: serde_json::Value = serde_json::from_str(&request).unwrap();
    assert_eq!(request["method"], Method::ClientShellSurfaceSet.as_str());
    assert_eq!(request["params"]["active"], true);
}

#[gpui::test]
fn deferred_release_is_generation_fenced_and_local_can_escape(cx: &mut gpui::TestAppContext) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    for change in ["retire", "boot", "local"] {
        let (source, _source_server) = connected_endpoint("ssh:source");
        let (mut target, _target_server) = connected_endpoint("ssh:target");
        target.initial_surface = false;
        let inbox = source.connection.inbox.clone();
        let drained = source.connection.drained.clone();
        let handle = source.connection.handle.clone().unwrap();
        let mut held = inbox.lock().unwrap();
        view.update(cx, |view, cx| {
            view.pending_releases.clear();
            view.endpoints.truncate(1);
            view.endpoints[0].detached = true;
            view.endpoints.extend([source, target]);
            view.selected_endpoint = 1;
            view.reset_selected();
            assert!(view.select_endpoint("ssh:target", cx));
            assert!(matches!(
                view.pending_releases[0].phase,
                ReleasePhase::Deferred(_)
            ));
            match change {
                "retire" => {
                    view.endpoints[1].stop();
                    view.endpoints[1].detached = true;
                    assert!(!Arc::ptr_eq(&inbox, &view.endpoints[1].connection.inbox));
                    assert!(handle.is_disconnected());
                }
                "boot" => {
                    Arc::make_mut(held.snapshot.as_mut().unwrap()).boot_id = "replacement".into()
                }
                _ => {
                    assert!(view.select_endpoint(LOCAL, cx));
                    assert!(view.pending_releases.is_empty());
                    assert!(handle.is_disconnected());
                }
            }
            assert!(!view.endpoints[2].initial_surface);
        });
        drop(held);
        view.update(cx, |view, cx| {
            if change == "boot" {
                project_until(view, cx, "stale source retired", |_| {
                    handle.is_disconnected()
                });
                view.endpoints[1].detached = true;
            }
        });
        wait_until(|| drained.load(Ordering::Acquire));
        if change != "local" {
            view.update(cx, |view, cx| {
                project_until(view, cx, "retired source drained", |view| {
                    view.endpoints[2].initial_surface
                });
                assert!(view.pending_releases.is_empty());
            });
        }
    }
}

#[gpui::test]
fn toast_queue_failure_retains_notice(cx: &mut gpui::TestAppContext) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    let (mut endpoint, _server) = connected_endpoint("ssh:toast");
    let mut wire = crate::notifications::tests::notification("queue failure");
    wire.workspace_id = Some("w1".into());
    endpoint
        .toasts
        .receive([crate::notifications::Notice::new(wire, Instant::now())
            .with_snapshot(endpoint.live.snapshot.as_deref())
            .preview()]);
    view.update(cx, |view, cx| {
        view.endpoints.push(endpoint);
        view.selected_endpoint = 1;
        view.options = ConnectOptions::default();
        view.reset_selected();
        // Keep the projected connected state to exercise enqueue failure itself.
        view.endpoints[1].connection.inbox = Arc::new(Mutex::new(view.live.clone()));
        view.endpoints[1]
            .connection
            .handle
            .as_ref()
            .unwrap()
            .disconnect();
        assert!(view.input_ready());
        view.tick_toasts(false, Instant::now());
        view.navigate_toast(0, cx);
        assert_eq!(view.endpoints[1].toasts.entries.len(), 1);
        assert!(view.local_error.is_some());
    });
}

#[gpui::test]
fn notification_command_rejects_ineligible_cards_without_selection_or_requests(
    cx: &mut gpui::TestAppContext,
) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    for case in 0..7 {
        let (mut endpoint, mut server) = connected_endpoint("ssh:toast");
        let mut wire = crate::notifications::tests::notification("ineligible");
        wire.workspace_id = (case != 0).then(|| "w1".into());
        let mut notice = crate::notifications::Notice::new(wire, Instant::now())
            .with_snapshot(endpoint.live.snapshot.as_deref())
            .preview();
        if case != 4 {
            notice.promote(Instant::now());
        }
        if case == 3 {
            notice.expires = Instant::now();
        }
        endpoint.toasts.receive([notice]);
        if case == 1 || case == 2 {
            let mut state = endpoint.connection.inbox.lock().unwrap();
            let snapshot = Arc::make_mut(state.snapshot.as_mut().unwrap());
            if case == 1 {
                snapshot.workspaces.clear();
            } else {
                snapshot.boot_id = "new-boot".into();
            }
        }
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.menu.reset();
                view.endpoints.truncate(1);
                view.endpoints.push(endpoint);
                view.selected_endpoint = 0;
                view.toasts_hidden = case == 5;
                if case == 6 {
                    view.open_keybinds(window, cx);
                }
                view.command(Command::OpenNotificationTarget, window, cx);
                assert_eq!(view.selected_endpoint, 0, "case {case}");
                assert!(view.pending_navigation.is_none());
                assert!(view.pending_toast.is_none());
                assert_eq!(view.endpoints[1].toasts.entries.len(), 1);
                view.endpoints[1]
                    .connection
                    .handle
                    .as_ref()
                    .unwrap()
                    .set_focus(&snapshot().boot_id, false)
                    .unwrap();
            })
        });
        // Ordered sentinel proves that no focus request preceded it.
        assert!(matches!(
            server.receive(),
            ClientMessage::ClientShellFocus { focused: false }
        ));
    }
}

#[gpui::test]
fn qa_play_sound_dispatches_without_daemon_or_pane(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
    let (sound, played) = crate::sound::Service::recording();
    view.update(cx, |view, _| {
        view.sound = sound;
        for endpoint in &mut view.endpoints {
            endpoint.stop();
            endpoint.live = Default::default();
        }
    });
    cx.update(|window, cx| {
        view.read(cx).focus.focus(window);
        window.draw(cx).clear();
        let menus = crate::menus();
        let qa = menus
            .iter()
            .find(|menu| menu.name.as_ref() == "QA")
            .unwrap();
        let action = qa
            .items
            .iter()
            .find_map(|item| match item {
                gpui::MenuItem::Action { name, action, .. } if name.as_ref() == "Play Sound" => {
                    Some(action)
                }
                _ => None,
            })
            .unwrap();
        assert!(action.partial_eq(&crate::PlaySound));
        window.dispatch_action(action.boxed_clone(), cx);
    });
    assert_eq!(
        played.recv_timeout(Duration::from_secs(3)).unwrap(),
        SemanticNotificationSound::Done
    );
    view.update(cx, |view, _| {
        for endpoint in &view.endpoints {
            assert!(endpoint.live.snapshot.is_none());
            assert!(endpoint.live.sound_events.is_empty());
        }
        view.sound = Default::default();
    });
    assert!(matches!(
        played.recv_timeout(Duration::from_secs(3)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
}

#[gpui::test]
fn inactive_endpoint_semantic_sound_reaches_worker_once(cx: &mut gpui::TestAppContext) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    let (remote, mut server) = connected_endpoint("ssh:sound");
    let (sound, played) = crate::sound::Service::recording();
    view.update(cx, |view, _| {
        view.sound = sound;
        view.endpoints[0].detached = true;
        view.endpoints.push(remote);
        assert_eq!(view.selected_endpoint, 0);
    });
    for message in [
        ServerMessage::Notify {
            kind: NotifyKind::Sound,
            message: "legacy".into(),
            body: None,
        },
        ServerMessage::TerminalBell { count: 1 },
        ServerMessage::SemanticNotification(SemanticNotification {
            kind: SemanticNotificationKind::Custom,
            title: "test".into(),
            body: None,
            sound: Some(SemanticNotificationSound::Request),
            agent: None,
            workspace_id: None,
            tab_id: None,
            pane_id: None,
            position: None,
        }),
    ] {
        write_message(&mut server.stream, &message, MAX_FRAME_SIZE).unwrap();
    }
    view.update(cx, |view, cx| {
        wait_until(|| {
            view.poll_endpoints(cx);
            match played.try_recv() {
                Ok(sound) => {
                    assert_eq!(sound, SemanticNotificationSound::Request);
                    true
                }
                Err(_) => false,
            }
        });
        assert!(view.endpoints[1].live.sound_events.is_empty());
        // Subsequent coalesced updates cannot redeliver the moved event.
        view.endpoints[1]
            .connection
            .inbox
            .lock()
            .unwrap()
            .set_outer_focus(true);
        view.poll_endpoints(cx);
        assert!(played.try_recv().is_err());
        view.endpoints[1].stop();
    });
}

#[gpui::test]
fn saved_selection_waits_for_snapshot_without_overwriting_preference(
    cx: &mut gpui::TestAppContext,
) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    let (mut remote, _server) = connected_endpoint("ssh:saved");
    let ready = remote.live.clone();
    remote.live.snapshot = None;
    remote.initial_surface = false;
    view.update(cx, |view, cx| {
        view.catalog.desired = Some("saved".into());
        view.catalog.initialized = true;
        view.catalog.restore_pending = true;
        // The catalog and then its connection can arrive long after startup.
        view.restore_selection(cx);
        assert!(view.catalog.restore_pending);
        view.endpoints.push(remote);
        for _ in 0..10 {
            view.restore_selection(cx);
            assert_eq!(view.selected_endpoint, 0);
            assert!(view.activation_deadline.is_none());
        }
        view.endpoints[1].live = ready;
        view.restore_selection(cx);
        assert_eq!(view.selected_endpoint, 1);
        assert!(!view.catalog.restore_pending);
        assert!(view.catalog.queued_write.is_none());
        // Automatic fallback is not a user choice and must not cause a loop.
        view.switch_endpoint(LOCAL, cx);
        view.restore_selection(cx);
        assert_eq!(view.selected_endpoint, 0);
        assert_eq!(view.catalog.desired.as_deref(), Some("saved"));
        assert!(view.catalog.queued_write.is_none());
        // An explicit Local click cancels even a not-yet-ready restore.
        view.catalog.restore_pending = true;
        view.select_endpoint(LOCAL, cx);
        view.restore_selection(cx);
        assert_eq!(view.catalog.desired, None);
        assert_eq!(view.selected_endpoint, 0);
    });
}

#[gpui::test]
fn dialog_response_survives_initial_surface_activation(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
    let (mut endpoint, _server) = connected_endpoint("ssh:fixture");
    endpoint.initial_surface = false;
    let response = serde_json::json!({"result":{"type":"worktree_list","worktrees":[]}});
    {
        let mut state = endpoint.connection.inbox.lock().unwrap();
        state.dialog_response = Some(("lookup".into(), Some(Ok(response.clone()))));
        state.dirty = true;
    }
    view.update(cx, |view, cx| {
        view.endpoints[0].detached = true;
        view.endpoints.push(endpoint);
        view.selected_endpoint = 1;
        view.reset_selected();
        view.poll_endpoints(cx);
        assert!(view.live.activation.is_some());
        assert!(matches!(&view.live.dialog_response, Some((id, Some(Ok(value)))) if id == "lookup" && value == &response));
        assert!(
            view.endpoints[1]
                .connection
                .inbox
                .lock()
                .unwrap()
                .dialog_response
                .as_ref()
                .unwrap()
                .1
                .is_none()
        );
    });
}

#[gpui::test]
fn every_focus_changing_command_fences_immediate_input_until_ack_and_surface(
    cx: &mut gpui::TestAppContext,
) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    for (command, method, confirm_close_tab, explicit_tab) in [
        (Command::SplitRight, Method::PaneSplit),
        (Command::SplitDown, Method::PaneSplit),
        (Command::Tab, Method::TabCreate),
        (Command::Workspace, Method::WorkspaceCreate),
        (Command::NextTab, Method::TabFocus),
        (Command::PreviousTab, Method::TabFocus),
        (Command::TabNumber(1), Method::TabFocus),
        (Command::FocusLeft, Method::PaneFocusDirection),
        (Command::FocusRight, Method::PaneFocusDirection),
        (Command::FocusUp, Method::PaneFocusDirection),
        (Command::FocusDown, Method::PaneFocusDirection),
        (Command::NextPane, Method::PaneFocus),
        (Command::PreviousPane, Method::PaneFocus),
        (Command::Zoom, Method::PaneZoom),
        (Command::ClosePane, Method::PaneClose),
        (Command::CloseTab, Method::TabClose),
        (Command::WorkspacePicker, Method::WorkspaceFocus),
        (Command::Palette, Method::CommandInvoke),
        (Command::Workspace, Method::WorkspaceClose),
        (Command::Workspace, Method::WorktreeCreate),
        (Command::Workspace, Method::WorktreeOpen),
        (Command::Workspace, Method::WorktreeRemove),
    ]
    .into_iter()
    .map(|(command, method)| (command, method, true, None))
    .chain([
        (Command::CloseTab, Method::TabClose, false, None),
        (Command::CloseTab, Method::TabClose, false, Some("inactive")),
    ]) {
        let (endpoint, mut server) = connected_endpoint("ssh:fixture");
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.endpoints.truncate(1);
                view.endpoints.push(endpoint);
                view.selected_endpoint = 1;
                view.options = ConnectOptions::default();
                view.reset_selected();
                view.activation_deadline = None;
                view.config.confirm_close_tab = confirm_close_tab;
                assert!(view.input_ready());
                if let Some(id) = explicit_tab {
                    let snapshot = Arc::make_mut(view.live.snapshot.as_mut().unwrap());
                    let mut tab = snapshot.tabs[0].clone();
                    tab.tab_id = id.into();
                    tab.focused = false;
                    snapshot.tabs.push(tab);
                    assert_ne!(snapshot.focused_tab_id.as_deref(), Some(id));
                    view.open_tab_close(id, window, cx);
                } else if matches!(
                    method,
                    Method::WorkspaceClose
                        | Method::WorktreeCreate
                        | Method::WorktreeOpen
                        | Method::WorktreeRemove
                ) {
                    crate::menu::workspace_tests::submit_focus_change(view, method, window, cx);
                } else {
                    view.command(command, window, cx);
                }
                let key = |key: &str| gpui::KeyDownEvent {
                    keystroke: gpui::Keystroke::parse(key).unwrap(),
                    is_held: false,
                };
                match command {
                    Command::ClosePane | Command::CloseTab if confirm_close_tab => {
                        view.close_confirmation_key(&key("tab"), window, cx);
                        view.close_confirmation_key(&key("enter"), window, cx);
                    }
                    Command::WorkspacePicker => view.palette_key(&key("enter"), window, cx),
                    Command::Palette => {
                        // The configured entry follows all native entries except Palette.
                        for _ in 0..crate::controls::COMMANDS.len() - 1 {
                            view.palette_key(&key("down"), window, cx);
                        }
                        view.palette_key(&key("enter"), window, cx);
                    }
                    _ => {}
                }
                if !confirm_close_tab {
                    assert!(view.menu.page.is_none());
                }
                assert!(!view.input_ready(), "{method} must fence immediately");
                assert!(view.activation_deadline.is_some());
                view.send(
                    ClientPaneInputEvent::TextCommit("must not reach old pane".into()),
                    cx,
                );
                let boot = view.live.snapshot.as_ref().unwrap().boot_id.clone();
                // An ordered marker exposes any input that incorrectly escaped.
                view.endpoints[view.selected_endpoint]
                    .connection
                    .handle
                    .as_ref()
                    .unwrap()
                    .set_focus(&boot, false)
                    .unwrap();
            })
        });
        let ClientMessage::ClientShellEndpointRequest { request, .. } = server.receive() else {
            panic!("missing command");
        };
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["method"], method.as_str());
        if method == Method::WorktreeOpen {
            assert_eq!(
                request["params"],
                serde_json::json!({"workspace_id": "w3",
                "path": "/endpoint/existing checkout ", "focus": true, "trust_repository": false})
            );
        }
        if method == Method::TabClose {
            let focused = snapshot().focused_tab_id.unwrap();
            assert_eq!(
                request["params"],
                serde_json::json!({"tab_id": explicit_tab.unwrap_or(&focused)})
            );
        }
        // herdr-client serializes API requests behind their predecessor's reply.
        server.respond(&request);
        let ClientMessage::ClientShellEndpointRequest { request, .. } = server.receive() else {
            panic!("missing surface barrier");
        };
        let barrier: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(barrier["method"], Method::ClientShellSurfaceSet.as_str());
        assert!(matches!(
            server.receive(),
            ClientMessage::ClientShellFocus { focused: false }
        ));
        view.update(cx, |view, cx| {
            let mut next = snapshot();
            next.revision += 1;
            next.focused_pane_id = Some("new-pane".into());
            {
                let mut state = view.endpoints[view.selected_endpoint]
                    .connection
                    .inbox
                    .lock()
                    .unwrap();
                // A fresh frame alone must not open input before the ordered ack.
                state.apply(ClientEvent::Snapshot(Arc::new(next.clone())));
                state.apply(ClientEvent::Surface(surface(&next)));
            }
            project_until(view, cx, "fresh frame", |view| {
                view.live.snapshot.as_ref().map(|s| s.revision) == Some(next.revision)
                    && view.live.activation.is_some()
            });
            assert!(!view.input_ready());
            {
                let mut state = view.endpoints[view.selected_endpoint]
                    .connection
                    .inbox
                    .lock()
                    .unwrap();
                state.apply(ClientEvent::Response {
                    request_id: barrier["id"].as_str().unwrap().into(),
                    response: serde_json::json!({"result": {
                        "type": "client_shell_surface_set", "active": true,
                        "projection_revision": next.revision + 1
                    }}),
                });
            }
            project_until(view, cx, "ack ahead of the frame", |view| {
                view.live
                    .activation
                    .as_ref()
                    .and_then(|activation| activation.revision)
                    == Some(next.revision + 1)
            });
            assert!(
                !view.input_ready(),
                "ack newer than frame still fences input"
            );
            next.revision += 1;
            {
                let mut state = view.endpoints[view.selected_endpoint]
                    .connection
                    .inbox
                    .lock()
                    .unwrap();
                state.apply(ClientEvent::Snapshot(Arc::new(next.clone())));
                state.apply(ClientEvent::Surface(surface(&next)));
            }
            project_until(view, cx, "frame matching the ack", |view| {
                view.live.surface.as_ref().map(|s| s.projection_revision) == Some(next.revision)
            });
            assert!(view.input_ready());
            assert!(view.activation_deadline.is_none());
            view.send(ClientPaneInputEvent::TextCommit("new pane only".into()), cx);
        });
        let ClientMessage::ClientShellPaneInput { pane_id, .. } = server.receive() else {
            panic!("missing input after fence");
        };
        assert_eq!(pane_id, "new-pane");
    }
}

#[gpui::test]
fn unconfirmed_tab_close_rejects_invalid_targets_and_unready_input(cx: &mut gpui::TestAppContext) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    let (endpoint, mut server) = connected_endpoint("ssh:fixture");
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.endpoints.push(endpoint);
            view.selected_endpoint = 1;
            view.reset_selected();
            view.activation_deadline = None;
            view.config.confirm_close_tab = false;
            assert!(view.input_ready());
            let ready = view.live.clone();
            let tab = ready
                .snapshot
                .as_ref()
                .unwrap()
                .focused_tab_id
                .as_ref()
                .unwrap();
            for explicit in [true, false] {
                for rejection in ["missing-tab", "missing-workspace", "unready-input"] {
                    view.live = ready.clone();
                    match rejection {
                        "missing-tab" => Arc::make_mut(view.live.snapshot.as_mut().unwrap())
                            .tabs
                            .clear(),
                        "missing-workspace" => Arc::make_mut(view.live.snapshot.as_mut().unwrap())
                            .workspaces
                            .clear(),
                        _ => {
                            view.live.surface = None;
                            assert!(!view.input_ready());
                        }
                    }
                    if explicit {
                        view.open_tab_close(tab, window, cx);
                    } else {
                        view.command(Command::CloseTab, window, cx);
                    }
                    assert!(view.activation_deadline.is_none());
                    assert!(view.pending_navigation.is_none());
                    view.dismiss_menu(window, cx);
                }
            }
            // FIFO marker proves none of the rejected attempts reached the peer.
            view.endpoints[1]
                .connection
                .handle
                .as_ref()
                .unwrap()
                .set_focus(&ready.snapshot.as_ref().unwrap().boot_id, false)
                .unwrap();
        });
    });
    assert!(matches!(
        server.receive(),
        ClientMessage::ClientShellFocus { focused: false }
    ));
}

#[gpui::test]
fn retiring_release_source_unblocks_destination_without_waiting_for_timeout(
    cx: &mut gpui::TestAppContext,
) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    for change in ["remove", "disable", "retarget", "disconnect"] {
        let (mut source, mut source_server) = connected_endpoint("ssh:source");
        let (mut target, mut target_server) = connected_endpoint("ssh:target");
        let profile = |id: &str| SavedHost {
            id: id.into(),
            label: id.into(),
            target: id.into(),
            session: "default".into(),
            enabled: true,
        };
        // These already-connected test transports stand in for SSH profiles;
        // catalog reconciliation must not try opening actual SSH connections.
        source.connection.target = ConnectTarget::Ssh {
            target: "source".into(),
            session: "default".into(),
        };
        target.connection.target = ConnectTarget::Ssh {
            target: "target".into(),
            session: "default".into(),
        };
        target.initial_surface = false;
        let drained = source.connection.drained.clone();
        let source_handle = source.connection.handle.clone().unwrap();
        view.update(cx, |view, cx| {
            view.endpoints.truncate(1);
            view.endpoints[0].detached = true;
            view.endpoints.extend([source, target]);
            view.selected_endpoint = 1;
            view.reset_selected();
            view.select_endpoint("ssh:target", cx);
            assert_eq!(view.pending_releases.len(), 1);
            view.poll_endpoints(cx);
            assert!(!view.endpoints[2].initial_surface);
        });
        // The release went onto the old transport but is never acknowledged.
        assert!(matches!(
            source_server.receive(),
            ClientMessage::ClientShellFocus { focused: false }
        ));
        assert!(matches!(
            source_server.receive(),
            ClientMessage::ClientShellEndpointRequest { .. }
        ));
        view.update(cx, |view, cx| {
            let mut source_profile = profile("source");
            match change {
                "remove" => view.reconcile_catalog(vec![profile("target")], cx),
                "disable" => {
                    source_profile.enabled = false;
                    view.reconcile_catalog(vec![source_profile, profile("target")], cx);
                }
                "retarget" => {
                    source_profile.session = "new-session".into();
                    view.reconcile_catalog(vec![source_profile, profile("target")], cx);
                }
                _ => {}
            }
            for endpoint in &mut view.endpoints {
                endpoint.retry_at = Instant::now() + Duration::from_secs(120);
            }
        });
        if change == "disconnect" {
            source_server
                .stream
                .shutdown(std::net::Shutdown::Both)
                .unwrap();
        }
        wait_until(|| drained.load(Ordering::Acquire));
        assert!(source_handle.is_disconnected());
        view.update(cx, |view, cx| {
            assert!(!view.pending_releases.is_empty());
            view.poll_endpoints(cx);
            assert!(view.pending_releases.is_empty(), "{change}");
            assert_eq!(view.endpoints[view.selected_endpoint].id, "ssh:target");
            assert!(view.endpoints[view.selected_endpoint].initial_surface);
            assert!(view.activation_deadline.unwrap() > Instant::now());
        });
        assert!(matches!(
            target_server.receive(),
            ClientMessage::ClientShellResize { .. }
        ));
        let ClientMessage::ClientShellEndpointRequest { request, .. } = target_server.receive()
        else {
            panic!("destination not activated");
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&request).unwrap()["method"],
            Method::ClientShellSurfaceSet.as_str()
        );
    }
}

#[test]
fn retry_backoff_resets_only_after_sixty_seconds_of_healthy_connection() {
    let (mut endpoint, _server) = connected_endpoint(LOCAL);
    endpoint.attempts = 8;
    let now = Instant::now();
    endpoint.online_since = None;
    endpoint.poll(now);
    endpoint.poll(now + Duration::from_secs(59));
    assert_eq!(endpoint.retry_delay(), Duration::from_secs(120));
    endpoint.poll(now + Duration::from_secs(60));
    assert_eq!(endpoint.attempts, 0);
    assert_eq!(endpoint.retry_delay(), Duration::from_millis(500));
    endpoint.connection.handle.as_ref().unwrap().disconnect();
    endpoint.poll(now + Duration::from_secs(61));
    assert_eq!(
        endpoint.retry_at,
        now + Duration::from_secs(61) + Duration::from_millis(500)
    );
    assert!(endpoint.online_since.is_none());
}

#[test]
fn brief_success_preserves_backoff_and_disconnect_restarts_stability_window() {
    let (mut endpoint, _server) = connected_endpoint(LOCAL);
    endpoint.attempts = 8;
    let now = Instant::now();
    endpoint.online_since = None;
    endpoint.poll(now);
    endpoint.poll(now + Duration::from_secs(59));
    endpoint.connection.handle.as_ref().unwrap().disconnect();
    endpoint.poll(now + Duration::from_secs(59));
    assert_eq!(endpoint.attempts, 8);
    assert!(endpoint.online_since.is_none());
    assert_eq!(endpoint.retry_at, now + Duration::from_secs(59 + 120));
    endpoint.connect(ConnectOptions::default(), false);
    assert_eq!(
        endpoint.attempts, 9,
        "automatic retry must preserve failed attempts"
    );
    assert!(endpoint.online_since.is_none());
}

#[gpui::test]
fn changed_target_and_manual_reconnect_reset_retry_history(cx: &mut gpui::TestAppContext) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        Fixture(cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx)))
    });
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    view.update(cx, |view, cx| {
        let host = SavedHost {
            id: "test".into(),
            label: "Test".into(),
            target: "unused".into(),
            session: "default".into(),
            enabled: true,
        };
        view.reconcile_catalog(vec![host.clone()], cx);
        view.endpoints[1].attempts = 8;
        view.reconcile_catalog(vec![host.clone()], cx);
        assert_eq!(
            view.endpoints[1].attempts, 8,
            "unchanged catalog preserves backoff"
        );
        view.reconcile_catalog(
            vec![SavedHost {
                session: "changed".into(),
                ..host
            }],
            cx,
        );
        assert_eq!(view.endpoints[1].attempts, 0);
        assert!(view.endpoints[1].online_since.is_none());
        view.endpoints[0].attempts = 8;
        view.reconnect(); // Explicit isolated missing socket, never SSH/discovery.
        assert_eq!(
            view.endpoints[0].attempts, 1,
            "manual reconnect starts a fresh first attempt"
        );
        assert_eq!(view.endpoints[0].retry_delay(), Duration::from_secs(1));
    });
}
