//! Explicitly opt-in: never discovers or attaches to an existing daemon.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use herdr_client::{
    Client, ClientEvent, ConnectOptions, ConnectTarget, connect,
    protocol::{
        ClientKeyCode, ClientKeyKind, ClientPaneInputEvent, ClientShellSnapshot, ClientSurfaceSize,
    },
};
use serde_json::{Value, json};
use std::{
    fs::{self, DirBuilder, File},
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const TIMEOUT: Duration = Duration::from_secs(20);
static NEXT_DAEMON: AtomicU64 = AtomicU64::new(0);

struct Daemon {
    dir: PathBuf,
    child: Option<Child>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            // Never use discovery, `server stop`, process-name matching, or group kills.
            let _ = child.kill();
            let _ = child.wait();
        }
        if thread::panicking()
            && let Ok(log) = fs::read_to_string(self.dir.join("daemon.log"))
        {
            eprintln!("isolated daemon output:\n{log}");
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Daemon {
    fn start() -> Self {
        let binary = PathBuf::from(
            std::env::var_os("HERDR_TEST_BINARY")
                .expect("set HERDR_TEST_BINARY to an explicit absolute herdr executable"),
        );
        assert!(binary.is_absolute() && binary.is_file());
        let parent = std::env::var_os("HERDR_TEST_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        assert!(parent.is_dir(), "temporary parent must already exist");
        let dir = parent.join(format!(
            "h{:x}-{:x}-{:x}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .subsec_nanos(),
            NEXT_DAEMON.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let mut daemon = Self { dir, child: None };
        assert!(
            daemon.socket().as_os_str().len() < 104,
            "temporary path too long for Unix socket"
        );
        fs::write(daemon.dir.join("config.toml"),
            "onboarding = false\n[terminal]\ndefault_shell = \"/bin/sh\"\nshell_mode = \"non_login\"\n").unwrap();
        let log = File::create(daemon.dir.join("daemon.log")).unwrap();
        let child = Command::new(binary)
            .arg("server")
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &daemon.dir)
            .env("XDG_CONFIG_HOME", daemon.dir.join("config"))
            .env("XDG_STATE_HOME", daemon.dir.join("state"))
            .env("XDG_DATA_HOME", daemon.dir.join("data"))
            .env("XDG_CACHE_HOME", daemon.dir.join("cache"))
            .env("XDG_RUNTIME_DIR", &daemon.dir)
            .env("TMPDIR", &daemon.dir)
            .env("HERDR_CONFIG_PATH", daemon.dir.join("config.toml"))
            .env("HERDR_SOCKET_PATH", daemon.dir.join("a.sock"))
            .env("HERDR_CLIENT_SOCKET_PATH", daemon.socket())
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("PS1", "LIVE> ")
            .current_dir(&daemon.dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        eprintln!(
            "isolated daemon pid={} dir={}",
            child.id(),
            daemon.dir.display()
        );
        daemon.child = Some(child);
        let deadline = Instant::now() + TIMEOUT;
        while !daemon.socket().exists() {
            assert!(
                daemon.child.as_mut().unwrap().try_wait().unwrap().is_none(),
                "daemon exited during startup"
            );
            assert!(Instant::now() < deadline, "daemon startup timed out");
            thread::sleep(Duration::from_millis(20));
        }
        daemon
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("a-client.sock")
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
        json!({"workspace_id": workspace, "cwd": daemon.dir, "focus": true, "label": "live-tab"})).unwrap();
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
            json!({"cwd": daemon.dir, "focus": true, "label": "live-workspace"}),
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
        let mut api = UnixStream::connect(daemon.dir.join("a.sock")).unwrap();
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
