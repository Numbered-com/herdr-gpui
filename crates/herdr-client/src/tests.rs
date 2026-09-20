use super::*;
use std::os::unix::net::UnixListener;

const SNAPSHOT: &str =
    include_str!("../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json");
const WELCOME: &str = include_str!("../../herdr-protocol/tests/fixtures/endpoint-welcome-v1.json");

fn send(stream: &mut UnixStream, message: ServerMessage) {
    write_message(stream, &message, MAX_GRAPHICS_FRAME_SIZE).unwrap();
}
fn receive(stream: &mut UnixStream) -> ClientMessage {
    read_message(stream, MAX_FRAME_SIZE).unwrap()
}
fn event(client: &Client) -> ClientEvent {
    client.events.recv_timeout(Duration::from_secs(3)).unwrap()
}

fn handshake(stream: &mut UnixStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let ClientMessage::EndpointControl { kind, data } = receive(stream) else {
        panic!("not stable hello")
    };
    assert_eq!(kind, ENDPOINT_HELLO_KIND);
    let hello: EndpointClientHello = serde_json::from_str(&data).unwrap();
    assert_eq!(hello.generation, 1);
    assert!(hello.surface_active);
    assert!(!hello.surface_reuse && !hello.surface_delta && !hello.direct_graphics);
    send(
        stream,
        ServerMessage::EndpointControl {
            kind: ENDPOINT_WELCOME_KIND.into(),
            data: WELCOME.into(),
        },
    );
    send(
        stream,
        ServerMessage::EndpointControl {
            kind: ENDPOINT_SNAPSHOT_KIND.into(),
            data: SNAPSHOT.into(),
        },
    );
}

fn baseline() -> PaneSurfaceFrame {
    PaneSurfaceFrame {
        boot_id: "boot-v1".into(),
        projection_revision: 7,
        surface_revision: 1,
        frame: FrameData {
            width: 1,
            height: 1,
            cells: vec![CellData {
                symbol: "x".into(),
                fg: 0,
                bg: 0,
                modifier: 0,
                skip: false,
                hyperlink: None,
            }],
            cursor: None,
            hyperlinks: vec![],
            graphics: vec![],
        },
        panes: vec![],
        splits: vec![],
        popup: None,
        graphics: SurfaceGraphicsScene::default(),
    }
}

fn test_client() -> (Client, UnixStream, thread::JoinHandle<io::Result<()>>) {
    let (stream, server) = UnixStream::pair().unwrap();
    let (commands, rx) = bounded(COMMAND_CAPACITY);
    let (tx, events) = bounded(EVENT_CAPACITY);
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker =
        thread::spawn(move || run(stream, ConnectOptions::default(), rx, &tx, &worker_stop));
    (
        Client {
            handle: ClientHandle {
                inner: Arc::new(HandleInner {
                    commands,
                    stop,
                    next_request: AtomicU64::new(1),
                }),
            },
            events,
        },
        server,
        worker,
    )
}

#[test]
fn handshake_rejects_every_incompatible_core_selection() {
    for field in [
        "generation",
        "snapshot_codec",
        "surface_codec",
        "input_codec",
        "blob_codec",
        "error",
    ] {
        let (client, mut server, worker) = test_client();
        receive(&mut server);
        let mut welcome: Value = serde_json::from_str(WELCOME).unwrap();
        welcome[field] = match field {
            "generation" => json!(2),
            "error" => json!({"code": "no_common_core", "message": "unsupported"}),
            _ => json!("future.codec"),
        };
        send(
            &mut server,
            ServerMessage::EndpointControl {
                kind: ENDPOINT_WELCOME_KIND.into(),
                data: welcome.to_string(),
            },
        );
        assert!(worker.join().unwrap().is_err(), "{field}");
        assert!(client.events.try_recv().is_err());
    }
}

#[test]
fn full_popup_surface_is_delivered_and_invalid_cells_fail_closed() {
    let (client, mut server, worker) = test_client();
    handshake(&mut server);
    event(&client);
    event(&client);
    let mut surface = baseline();
    surface.popup = Some(Box::new(ClientShellPopupSurface {
        terminal_id: "popup-1".into(),
        title: "Popup".into(),
        width: Some(ClientShellPopupSize::Percent(80)),
        height: Some(ClientShellPopupSize::Cells(1)),
        frame: surface.frame.clone(),
        mouse_reporting: true,
        sgr_pixel_mouse: false,
        pixel_width: 8,
        pixel_height: 16,
    }));
    send(&mut server, ServerMessage::PaneSurface(surface.clone()));
    assert!(matches!(event(&client), ClientEvent::Surface(s) if *s == surface));
    surface.surface_revision += 1;
    surface.popup.as_mut().unwrap().frame.cells.clear();
    send(&mut server, ServerMessage::PaneSurface(surface));
    assert!(
        worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("cell count")
    );
}

#[test]
fn requests_wait_for_final_response_and_preserve_fifo() {
    let (client, mut server, worker) = test_client();
    handshake(&mut server);
    event(&client);
    event(&client);
    let first = client.handle.focus_pane("boot-v1", "w1:p1").unwrap();
    let second = client.handle.focus_pane("boot-v1", "w1:p2").unwrap();
    client.handle.set_focus("boot-v1", false).unwrap();
    assert!(
        matches!(receive(&mut server), ClientMessage::ClientShellEndpointRequest { request, .. }
        if serde_json::from_str::<Value>(&request).unwrap()["id"] == first)
    );
    send(
        &mut server,
        ServerMessage::ClientShellEndpointResponseChunk {
            boot_id: "boot-v1".into(),
            request_id: first.clone(),
            final_chunk: false,
            data: format!("{{\"id\":\"{first}\",").into_bytes(),
        },
    );
    server
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut byte = [0];
    assert!(matches!(
        server.read(&mut byte).unwrap_err().kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ));
    send(
        &mut server,
        ServerMessage::ClientShellEndpointResponseChunk {
            boot_id: "boot-v1".into(),
            request_id: first,
            final_chunk: true,
            data: b"\"result\":{}}".to_vec(),
        },
    );
    assert!(matches!(event(&client), ClientEvent::Response { .. }));
    assert!(
        matches!(receive(&mut server), ClientMessage::ClientShellEndpointRequest { request, .. }
        if serde_json::from_str::<Value>(&request).unwrap()["id"] == second)
    );
    assert_eq!(
        receive(&mut server),
        ClientMessage::ClientShellFocus { focused: false }
    );
    client.handle.disconnect();
    worker.join().unwrap().unwrap();
}

#[test]
fn future_surface_and_patch_wait_for_matching_snapshot() {
    let (client, mut server, worker) = test_client();
    handshake(&mut server);
    event(&client);
    event(&client);
    let mut future = baseline();
    future.projection_revision = 9;
    send(&mut server, ServerMessage::PaneSurface(future));
    send(
        &mut server,
        ServerMessage::PaneSurfacePatch(PaneSurfacePatch {
            boot_id: "boot-v1".into(),
            projection_revision: 9,
            base_surface_revision: 1,
            surface_revision: 2,
            rows: vec![],
            panes: vec![],
            cursor: None,
        }),
    );
    let mut snapshot: Value = serde_json::from_str(SNAPSHOT).unwrap();
    for revision in [8, 9] {
        snapshot["revision"] = revision.into();
        send(
            &mut server,
            ServerMessage::EndpointControl {
                kind: ENDPOINT_SNAPSHOT_KIND.into(),
                data: snapshot.to_string(),
            },
        );
        assert!(matches!(event(&client), ClientEvent::Snapshot(s) if s.revision == revision));
    }
    assert!(matches!(event(&client), ClientEvent::Surface(s)
        if s.projection_revision == 9 && s.surface_revision == 2));
    client.handle.disconnect();
    worker.join().unwrap().unwrap();
}

#[test]
fn boot_change_in_partial_frame_prevents_queued_input() {
    let (client, mut server, worker) = test_client();
    handshake(&mut server);
    event(&client);
    event(&client);
    let mut snapshot: Value = serde_json::from_str(SNAPSHOT).unwrap();
    snapshot["boot_id"] = "replacement".into();
    let bytes = encode_message(
        &ServerMessage::EndpointControl {
            kind: ENDPOINT_SNAPSHOT_KIND.into(),
            data: snapshot.to_string(),
        },
        MAX_FRAME_SIZE,
    )
    .unwrap();
    server.write_all(&bytes[..5]).unwrap();
    thread::sleep(Duration::from_millis(100));
    client.handle.set_focus("boot-v1", true).unwrap();
    server.write_all(&bytes[5..]).unwrap();
    assert!(
        worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("boot changed")
    );
    let mut byte = [0];
    assert_eq!(server.read(&mut byte).unwrap(), 0);
}

#[test]
fn snapshot_surface_patch_navigation_input_resize_and_response() {
    let (client, mut server, worker) = test_client();
    handshake(&mut server);
    assert!(matches!(event(&client), ClientEvent::Connected(_)));
    let ClientEvent::Snapshot(s) = event(&client) else {
        panic!("snapshot missing")
    };
    assert_eq!(s.panes[0].pane_id, "w1:p1");
    send(&mut server, ServerMessage::PaneSurface(baseline()));
    let ClientEvent::Surface(first) = event(&client) else {
        panic!("surface missing")
    };
    send(
        &mut server,
        ServerMessage::PaneSurfacePatch(PaneSurfacePatch {
            boot_id: s.boot_id.clone(),
            projection_revision: 7,
            base_surface_revision: 1,
            surface_revision: 2,
            rows: vec![PaneSurfacePatchRow {
                x: 0,
                y: 0,
                cells: vec![CellData {
                    symbol: "y".into(),
                    ..first.frame.cells[0].clone()
                }],
            }],
            panes: vec![],
            cursor: None,
        }),
    );
    let ClientEvent::Surface(second) = event(&client) else {
        panic!("patched surface missing")
    };
    assert_eq!(first.frame.cells[0].symbol, "x"); // Published Arcs are immutable.
    assert_eq!(second.frame.cells[0].symbol, "y");
    assert_eq!(second.surface_revision, 2);
    client
        .handle
        .send_input(
            &s.boot_id,
            "w1:p1",
            vec![ClientPaneInputEvent::TextCommit("hello".into())],
        )
        .unwrap();
    client
        .handle
        .resize(
            &s.boot_id,
            ConnectOptions {
                surface_size: ClientSurfaceSize {
                    cols: 100,
                    rows: 30,
                },
                ..ConnectOptions::default()
            },
        )
        .unwrap();
    let id = client.handle.focus_pane(&s.boot_id, "w1:p1").unwrap();
    assert!(
        matches!(receive(&mut server), ClientMessage::ClientShellPaneInput { pane_id, events } if pane_id == "w1:p1" && events == vec![ClientPaneInputEvent::TextCommit("hello".into())])
    );
    assert!(matches!(
        receive(&mut server),
        ClientMessage::ClientShellResize {
            surface_size: ClientSurfaceSize {
                cols: 100,
                rows: 30
            },
            ..
        }
    ));
    let ClientMessage::ClientShellEndpointRequest { boot_id, request } = receive(&mut server)
    else {
        panic!("request missing")
    };
    assert_eq!(boot_id, s.boot_id);
    assert_eq!(
        serde_json::from_str::<Value>(&request).unwrap(),
        json!({"id": id, "method": "pane.focus", "params": {"pane_id": "w1:p1"}})
    );
    let response = json!({"id": id, "result": {"type": "pane_info", "pane": {"pane_id": "w1:p1", "focused": true}}}).to_string();
    let mid = response.len() / 2;
    for (final_chunk, data) in [
        (false, &response.as_bytes()[..mid]),
        (true, &response.as_bytes()[mid..]),
    ] {
        send(
            &mut server,
            ServerMessage::ClientShellEndpointResponseChunk {
                boot_id: s.boot_id.clone(),
                request_id: id.clone(),
                final_chunk,
                data: data.to_vec(),
            },
        );
    }
    assert!(
        matches!(event(&client), ClientEvent::Response { request_id, response } if request_id == id && response["result"]["pane"]["focused"] == true)
    );
    client.handle.disconnect();
    worker.join().unwrap().unwrap();
}

#[test]
fn stale_boot_and_unsupported_commands_never_reach_socket() {
    let (client, mut server, worker) = test_client();
    handshake(&mut server);
    event(&client);
    event(&client);
    client
        .handle
        .send_input(
            "old-boot",
            "w1:p1",
            vec![ClientPaneInputEvent::Paste("bad".into())],
        )
        .unwrap();
    assert!(matches!(
        event(&client),
        ClientEvent::CommandRejected {
            request_id: None,
            ..
        }
    ));
    let unsupported = client
        .handle
        .request("boot-v1", "not.advertised", json!({}))
        .unwrap();
    assert!(
        matches!(event(&client), ClientEvent::CommandRejected { request_id: Some(id), .. } if id == unsupported)
    );
    client.handle.set_focus("boot-v1", true).unwrap();
    assert_eq!(
        receive(&mut server),
        ClientMessage::ClientShellFocus { focused: true }
    );
    let mut snapshot: Value = serde_json::from_str(SNAPSHOT).unwrap();
    snapshot["boot_id"] = "replacement-boot".into();
    send(
        &mut server,
        ServerMessage::EndpointControl {
            kind: ENDPOINT_SNAPSHOT_KIND.into(),
            data: snapshot.to_string(),
        },
    );
    assert!(
        worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("boot changed")
    );
}

#[test]
fn fragmented_frames_survive_timeout_between_every_byte() {
    let (mut client, mut server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_millis(1)))
        .unwrap();
    let expected = ServerMessage::TerminalBell { count: 300 };
    let bytes = encode_message(&expected, MAX_FRAME_SIZE).unwrap();
    let mut reader = FrameReader::new();
    let mut message = None;
    for byte in bytes {
        assert!(reader.poll(&mut client).unwrap().is_none()); // timeout, preserving state
        server.write_all(&[byte]).unwrap();
        if let Some(next) = reader.poll(&mut client).unwrap() {
            message = Some(next);
        }
    }
    assert_eq!(message, Some(expected));
}

#[test]
fn malformed_handshake_and_patch_fail_closed() {
    let (client, mut server, worker) = test_client();
    receive(&mut server);
    send(
        &mut server,
        ServerMessage::Welcome {
            version: 22,
            encoding: RenderEncoding::SemanticFrame,
            error: None,
        },
    );
    assert!(worker.join().unwrap().is_err());
    drop(client);
    let (client, mut server, worker) = test_client();
    handshake(&mut server);
    event(&client);
    event(&client);
    send(
        &mut server,
        ServerMessage::PaneSurfacePatch(PaneSurfacePatch {
            boot_id: "boot-v1".into(),
            projection_revision: 7,
            base_surface_revision: 1,
            surface_revision: 2,
            rows: vec![],
            panes: vec![],
            cursor: None,
        }),
    );
    assert!(
        worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("patch before baseline")
    );
}

#[test]
fn bounded_command_queue_and_outbound_limit_are_explicit() {
    let (commands, _rx) = bounded(1);
    let handle = ClientHandle {
        inner: Arc::new(HandleInner {
            commands,
            stop: Arc::new(AtomicBool::new(false)),
            next_request: AtomicU64::new(1),
        }),
    };
    assert!(matches!(
        handle.set_focus("", true),
        Err(SendError::Invalid(_))
    ));
    handle.set_focus("boot", true).unwrap();
    assert_eq!(handle.set_focus("boot", false), Err(SendError::Full));
    assert!(matches!(
        handle.send_input(
            "boot",
            "p",
            vec![ClientPaneInputEvent::Paste("x".repeat(MAX_FRAME_SIZE))]
        ),
        Err(SendError::Invalid(_))
    ));
    handle.disconnect();
    assert_eq!(
        handle.set_focus("boot", false),
        Err(SendError::Disconnected)
    );
}

#[test]
fn cancellation_interrupts_full_event_queue_and_idle_read() {
    let (client, mut server, worker) = test_client();
    handshake(&mut server);
    for _ in 0..EVENT_CAPACITY + 2 {
        send(&mut server, ServerMessage::TerminalBell { count: 1 });
    }
    client.handle.disconnect();
    // Cancellation during a bounded send returns Interrupted; idle cancellation returns Ok.
    let result = worker.join().unwrap();
    assert!(result.is_ok() || result.unwrap_err().kind() == io::ErrorKind::Interrupted);

    let (client, mut server, worker) = test_client();
    handshake(&mut server);
    event(&client);
    event(&client);
    drop(client.handle); // Last handle drop also cancels.
    worker.join().unwrap().unwrap();
}

#[test]
fn public_connect_delivers_shutdown_and_socket_failure() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    // Deep worktree paths can exceed the Unix socket address limit on macOS.
    let path = std::env::temp_dir().join(format!(
        "test-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&path).unwrap();
    let client = connect(
        ConnectTarget::Socket(path.clone()),
        ConnectOptions::default(),
    )
    .unwrap();
    let (mut server, _) = listener.accept().unwrap();
    std::fs::remove_file(&path).unwrap();
    handshake(&mut server);
    event(&client);
    event(&client);
    send(
        &mut server,
        ServerMessage::ServerShutdown {
            reason: Some("test shutdown".into()),
        },
    );
    assert!(
        matches!(event(&client), ClientEvent::Disconnected { reason } if reason == "test shutdown")
    );
    let missing = connect(ConnectTarget::Socket(path), ConnectOptions::default()).unwrap();
    assert!(matches!(event(&missing), ClientEvent::Disconnected { .. }));
}

#[test]
fn geometry_and_frame_reader_limits() {
    for (cols, rows, cell_width_px) in [(0, 24, 0), (4097, 1, 0), (1001, 1000, 0), (80, 24, 4097)] {
        assert!(
            validate_options(ConnectOptions {
                surface_size: ClientSurfaceSize { cols, rows },
                cell_width_px,
                cell_height_px: 0,
            })
            .is_err()
        );
    }
    let (mut client, mut server) = UnixStream::pair().unwrap();
    server
        .write_all(&((MAX_GRAPHICS_FRAME_SIZE + 1) as u32).to_le_bytes())
        .unwrap();
    assert!(FrameReader::new().poll(&mut client).is_err());
    let mut reader = FrameReader::new();
    reader.started = Some(Instant::now() - TIMEOUT - POLL);
    assert!(
        reader
            .poll(&mut client)
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
}

#[test]
fn response_boot_id_correlation_and_assembly_limits() {
    for case in ["boot", "id", "limit"] {
        let (client, mut server, worker) = test_client();
        handshake(&mut server);
        event(&client);
        event(&client);
        let id = client.handle.focus_pane("boot-v1", "w1:p1").unwrap();
        receive(&mut server);
        let data = match case {
            "limit" => vec![b' '; MAX_RESPONSE_BYTES + 1],
            "id" => br#"{"id":"wrong","result":{}}"#.to_vec(),
            _ => vec![],
        };
        send(
            &mut server,
            ServerMessage::ClientShellEndpointResponseChunk {
                boot_id: if case == "boot" { "stale" } else { "boot-v1" }.into(),
                request_id: id,
                final_chunk: true,
                data,
            },
        );
        let error = worker.join().unwrap().unwrap_err().to_string();
        assert!(
            error.contains(match case {
                "boot" => "boot mismatch",
                "id" => "ID mismatch",
                _ => "limit exceeded",
            }),
            "{error}"
        );
    }
}
