//! Explicitly opt-in: never discovers or attaches to an existing daemon.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../test-support/sandbox.rs"]
mod sandbox;

use herdr_client::{
    Client, ClientEvent, ConnectOptions, ConnectTarget, connect,
    protocol::{
        ClientClipboardImageTarget, ClientKeyCode, ClientKeyKind, ClientPaneInputEvent,
        ClientShellSnapshot, ClientSurfaceSize,
    },
};
use sandbox::{Sandbox, daemon_binary, stop_children};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::Child,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(20);

struct Daemon {
    sandbox: Sandbox,
    child: Option<Child>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        stop_children([&mut self.child]);
        if thread::panicking()
            && let Ok(log) = fs::read_to_string(self.sandbox.dir.join("daemon.log"))
        {
            eprintln!("isolated daemon output:\n{log}");
        }
    }
}

impl Daemon {
    fn start() -> Self {
        let binary = daemon_binary();
        let mut daemon = Self {
            sandbox: Sandbox::new(),
            child: None,
        };
        daemon.child = Some(
            daemon
                .sandbox
                .command(binary, "daemon.log")
                .arg("server")
                .spawn()
                .unwrap(),
        );
        daemon
            .sandbox
            .wait_for_daemon(daemon.child.as_mut().unwrap(), TIMEOUT);
        daemon
    }

    fn socket(&self) -> PathBuf {
        self.sandbox.socket()
    }
}

struct Session {
    client: Client,
    snapshot: Option<Arc<ClientShellSnapshot>>,
}

impl Session {
    fn open(daemon: &Daemon) -> Self {
        let mut session = Self {
            client: connect(
                ConnectTarget::Socket(daemon.socket()),
                ConnectOptions::default(),
            )
            .unwrap(),
            snapshot: None,
        };
        session.until("stable welcome", |event| {
            if let ClientEvent::Connected(welcome) = event {
                eprintln!("stable endpoint welcome: {welcome:?}");
                assert_eq!(welcome.generation, 1);
                true
            } else {
                false
            }
        });
        session.until("initial snapshot", |event| {
            matches!(event, ClientEvent::Snapshot(_))
        });
        assert!(
            session
                .snapshot
                .as_ref()
                .unwrap()
                .config_diagnostic
                .is_none()
        );
        session.until("initial surface", |event| {
            matches!(event, ClientEvent::Surface(_))
        });
        session
    }

    fn until(&mut self, label: &str, mut predicate: impl FnMut(&ClientEvent) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let event = self
                .client
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!("waiting for {label}: {error}; snapshot={:?}", self.snapshot)
                });
            match &event {
                ClientEvent::Disconnected { reason } => panic!("{label}: disconnected: {reason}"),
                ClientEvent::CommandRejected { reason, .. } => {
                    panic!("{label}: rejected: {reason}")
                }
                ClientEvent::Snapshot(snapshot) => self.snapshot = Some(snapshot.clone()),
                ClientEvent::Surface(surface) => {
                    let snapshot = self.snapshot.as_ref().unwrap();
                    assert_eq!(surface.boot_id, snapshot.boot_id);
                    assert_eq!(surface.projection_revision, snapshot.revision);
                    surface.frame.validate().unwrap();
                }
                _ => {}
            }
            if predicate(&event) {
                return;
            }
            assert!(Instant::now() < deadline, "waiting for {label}");
        }
    }

    fn response(&mut self, id: String) -> Value {
        let mut result = None;
        self.until("API response", |event| {
            if let ClientEvent::Response {
                request_id,
                response,
            } = event
            {
                assert_eq!(*request_id, id);
                assert!(response.get("error").is_none(), "{response}");
                result = Some(response["result"].clone());
                true
            } else {
                false
            }
        });
        result.unwrap()
    }

    fn focused(&mut self, workspace: &str, tab: &str) {
        let matches = |s: &ClientShellSnapshot| {
            s.focused_workspace_id.as_deref() == Some(workspace)
                && s.focused_tab_id.as_deref() == Some(tab)
        };
        if !matches(self.snapshot.as_ref().unwrap()) {
            self.until(
                "focus snapshot",
                |event| matches!(event, ClientEvent::Snapshot(s) if matches(s)),
            );
        }
        eprintln!("focus verified: workspace={workspace} tab={tab}");
    }

    fn detach(self) {
        self.client.handle.disconnect();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self
                .client
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                Err(error) => panic!("detach did not close worker: {error}"),
                Ok(_) => {}
            }
        }
    }
}

/// The remote image drop path: bytes leave this machine and the daemon stages
/// them under its own TMPDIR, then pastes the path it owns into the pane. Only a
/// real daemon proves the staging and paste, so this is an opt-in live test.
#[test]
#[ignore = "requires explicit HERDR_TEST_BINARY; spawns an isolated live daemon"]
fn clipboard_image_bridge_live() {
    let daemon = Daemon::start();
    let mut session = Session::open(&daemon);
    let snapshot = session.snapshot.as_ref().unwrap();
    let boot = snapshot.boot_id.clone();
    let pane = snapshot.focused_pane_id.clone().unwrap();

    // A minimal but genuine PNG: the daemon stores bytes verbatim, unexamined.
    let image: Vec<u8> = b"\x89PNG\r\n\x1a\n"
        .iter()
        .copied()
        .chain((0u8..=255).cycle().take(4096))
        .collect();
    session
        .client
        .handle
        .send_clipboard_image(
            &boot,
            ClientClipboardImageTarget::Pane(pane.clone()),
            "png".into(),
            image.clone(),
        )
        .unwrap();

    // TMPDIR is the sandbox, so the staged file is the daemon's, not this host's.
    // The directory carries the daemon's own uid, so it is matched by prefix.
    let sandbox_dir = daemon.sandbox.dir.clone();
    let staged_image = |image: &[u8]| {
        let staging = fs::read_dir(&sandbox_dir).ok()?.flatten().find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("herdr-clipboard-images-")
        })?;
        fs::read_dir(staging.path())
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| fs::read(path).is_ok_and(|bytes| bytes == image))
    };
    let deadline = Instant::now() + TIMEOUT;
    let staged = loop {
        if let Some(path) = staged_image(&image) {
            break path;
        }
        assert!(
            Instant::now() < deadline,
            "daemon never staged the bridged image under {}",
            sandbox_dir.display()
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(staged.extension().and_then(|e| e.to_str()), Some("png"));

    // The pane is given the daemon's path, never this machine's. Rows are joined
    // because an 80-column pane wraps the path without inserting characters.
    let name = staged.file_name().unwrap().to_string_lossy().into_owned();
    session.until("staged path pasted into the pane", |event| {
        matches!(event, ClientEvent::Surface(s) if s
            .frame
            .cells
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>()
            .contains(&name))
    });
    eprintln!("clipboard image bridged and pasted as {}", staged.display());
    session.detach();
}

#[test]
#[ignore = "requires explicit HERDR_TEST_BINARY; spawns an isolated live daemon"]
fn stable_endpoint_live() {
    let mut daemon = Daemon::start();
    let mut session = Session::open(&daemon);
    let snapshot = session.snapshot.as_ref().unwrap();
    let boot = snapshot.boot_id.clone();
    let workspace = snapshot.focused_workspace_id.clone().unwrap();
    let tab = snapshot.focused_tab_id.clone().unwrap();
    let pane = snapshot.focused_pane_id.clone().unwrap();

    // Split the literal so terminal input echo alone cannot satisfy the assertion.
    let marker = format!("HERDR_LIVE_{}_OK", std::process::id());
    session
        .client
        .handle
        .send_input(
            &boot,
            &pane,
            vec![
                ClientPaneInputEvent::TextCommit(format!(
                    "echo HERDR_LIVE_{}\"_OK\"",
                    std::process::id()
                )),
                ClientPaneInputEvent::Key {
                    code: ClientKeyCode::Enter,
                    modifiers: 0,
                    kind: ClientKeyKind::Press,
                    repeat_count: 1,
                    shifted_codepoint: None,
                    generated_text: None,
                    tracks_release: false,
                    physical_key_id: None,
                    windows_record: None,
                },
            ],
        )
        .unwrap();
    session.until("shell echo marker", |event| {
        matches!(event, ClientEvent::Surface(s) if s.frame.cells.chunks(usize::from(s.frame.width)).any(|row|
            row.iter().map(|cell| cell.symbol.as_str()).collect::<String>().trim() == marker))
    });
    eprintln!("semantic TextCommit + Enter produced shell output: {marker}");

    let id = session.client.handle.request(&boot, "tab.create",
        json!({"workspace_id": workspace, "cwd": daemon.sandbox.dir, "focus": true, "label": "live-tab"})).unwrap();
    let result = session.response(id);
    let second_tab = result["tab"]["tab_id"]
        .as_str()
        .expect("created tab ID")
        .to_owned();
    session.focused(&workspace, &second_tab);
    let id = session.client.handle.focus_tab(&boot, &tab).unwrap();
    session.response(id);
    session.focused(&workspace, &tab);
    let id = session.client.handle.focus_tab(&boot, &second_tab).unwrap();
    session.response(id);
    session.focused(&workspace, &second_tab);

    let id = session
        .client
        .handle
        .request(
            &boot,
            "workspace.create",
            json!({"cwd": daemon.sandbox.dir, "focus": true, "label": "live-workspace"}),
        )
        .unwrap();
    let result = session.response(id);
    let second_workspace = result["workspace"]["workspace_id"]
        .as_str()
        .expect("created workspace ID")
        .to_owned();
    if session
        .snapshot
        .as_ref()
        .unwrap()
        .focused_workspace_id
        .as_deref()
        != Some(&second_workspace)
    {
        session.until("created workspace snapshot", |event| matches!(event,
            ClientEvent::Snapshot(s) if s.focused_workspace_id.as_deref() == Some(&second_workspace)));
    }
    let second_workspace_tab = session
        .snapshot
        .as_ref()
        .unwrap()
        .focused_tab_id
        .clone()
        .unwrap();
    let id = session
        .client
        .handle
        .focus_workspace(&boot, &workspace)
        .unwrap();
    session.response(id);
    session.focused(&workspace, &second_tab);
    let id = session
        .client
        .handle
        .focus_workspace(&boot, &second_workspace)
        .unwrap();
    session.response(id);
    session.focused(&second_workspace, &second_workspace_tab);

    session
        .client
        .handle
        .resize(
            &boot,
            ConnectOptions {
                surface_size: ClientSurfaceSize {
                    cols: 100,
                    rows: 30,
                },
                ..ConnectOptions::default()
            },
        )
        .unwrap();
    session.until("100x30 surface", |event| {
        matches!(event,
        ClientEvent::Surface(s) if s.frame.width == 100 && s.frame.height == 30)
    });
    eprintln!("resize verified: 100x30 surface");

    session.detach();
    assert!(daemon.child.as_mut().unwrap().try_wait().unwrap().is_none());
    let reattached = Session::open(&daemon);
    assert_eq!(reattached.snapshot.as_ref().unwrap().boot_id, boot);
    assert_eq!(reattached.snapshot.as_ref().unwrap().workspaces.len(), 2);
    assert_eq!(reattached.snapshot.as_ref().unwrap().tabs.len(), 3);
    reattached.detach();
    assert!(daemon.child.as_mut().unwrap().try_wait().unwrap().is_none());
    eprintln!(
        "detach verified: daemon alive; reconnected to same boot {boot} with 2 workspaces / 3 tabs"
    );
}

#[test]
#[ignore = "requires explicit HERDR_TEST_BINARY; spawns an isolated live daemon"]
fn client_local_completion_status_live() {
    use herdr_client::{presentation::AgentPresentation, protocol::AgentStatus};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
    };
    let daemon = Daemon::start();
    let mut session = Session::open(&daemon);
    let initial = session.snapshot.as_ref().unwrap();
    let boot = initial.boot_id.clone();
    let pane = initial.focused_pane_id.clone().unwrap();
    let mut presentation = AgentPresentation::default();
    let mut previous_sequence = None;
    for (seq, wire_status, expected) in [
        (1, "working", AgentStatus::Working),
        (2, "blocked", AgentStatus::Blocked),
        (3, "idle", AgentStatus::Done),
    ] {
        // Agent hooks use the JSON API, not the shell's UI-only command allowlist.
        let mut api = UnixStream::connect(daemon.sandbox.dir.join("a.sock")).unwrap();
        api.set_read_timeout(Some(TIMEOUT)).unwrap();
        writeln!(
            api,
            "{}",
            json!({
                "id": format!("state-{seq}"), "method": "pane.report_agent",
                "params": {"pane_id": pane, "source": "claude-hook", "agent": "claude",
                    "state": wire_status, "seq": seq}
            })
        )
        .unwrap();
        let mut response = String::new();
        BufReader::new(api).read_line(&mut response).unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert!(response.get("error").is_none(), "{response}");
        let matches = |snapshot: &ClientShellSnapshot| {
            snapshot.agents.iter().any(|agent| {
                agent.pane_id == pane
                    && previous_sequence.is_none_or(|previous| agent.state_change_seq > previous)
                    && match wire_status {
                        "working" => agent.agent_status == AgentStatus::Working,
                        "blocked" => agent.agent_status == AgentStatus::Blocked,
                        _ => matches!(agent.agent_status, AgentStatus::Idle | AgentStatus::Done),
                    }
            })
        };
        if !matches(session.snapshot.as_ref().unwrap()) {
            session.until("agent status snapshot", |event| {
                matches!(event,
                ClientEvent::Snapshot(snapshot) if matches(snapshot))
            });
        }
        let raw = session.snapshot.as_ref().unwrap();
        assert_eq!(raw.boot_id, boot);
        let mut projected = (**raw).clone();
        presentation.project_snapshot(&mut projected);
        let agent = projected
            .agents
            .iter()
            .find(|agent| agent.pane_id == pane)
            .unwrap();
        previous_sequence = Some(agent.state_change_seq);
        assert_eq!(agent.agent_status, expected);
        assert_eq!(projected.workspaces[0].agent_status, expected);
        eprintln!(
            "live status: raw={:?} seq={} effective={:?}",
            raw.agents[0].agent_status, agent.state_change_seq, expected
        );
        if expected == AgentStatus::Done {
            session.until("completion surface", |event| {
                if let ClientEvent::Surface(surface) = event
                    && presentation.acknowledge_surface(&mut projected, surface, true)
                {
                    assert_eq!(projected.agents[0].agent_status, AgentStatus::Idle);
                    assert_eq!(projected.workspaces[0].agent_status, AgentStatus::Idle);
                    return true;
                }
                false
            });
        }
    }
    session.detach();
}
