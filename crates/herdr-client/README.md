# Native Client API

Unix/macOS local client for Herdr's stable generation 1 endpoint. This crate
does not link Herdr, GPUI, ratatui, crossterm, Tokio, or a PTY implementation.
The workspace centralizes `gpui = "=0.2.2"` for the forthcoming GUI member.

```rust,no_run
use herdr_client::{connect, ClientEvent, ConnectOptions, ConnectTarget};
use herdr_client::protocol::ClientPaneInputEvent;

let client = connect(ConnectTarget::Local, ConnectOptions::default())?;
let handle = client.handle.clone();
// Run this receiver loop on a GUI background task, NOT the UI thread.
while let Ok(event) = client.events.recv() {
    match event {
        ClientEvent::Snapshot(snapshot) => {
            if let Some(pane) = &snapshot.focused_pane_id {
                handle.send_input(&snapshot.boot_id, pane,
                    vec![ClientPaneInputEvent::TextCommit("hello".into())])?;
            }
        }
        ClientEvent::Surface(surface) => { /* publish to GPUI via cx.update */ }
        ClientEvent::Disconnected { reason } => break,
        _ => {}
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Public Interface

```text
connect(ConnectTarget, ConnectOptions) -> io::Result<Client>
Client { pub handle: ClientHandle, pub events: Receiver<ClientEvent> }
ConnectTarget::Local
ConnectTarget::Session { name: String, development: bool }
ConnectTarget::Socket(PathBuf)
ConnectTarget::socket_path(&self) -> io::Result<PathBuf>
session_socket(config_dir: &Path, name: &str) -> io::Result<PathBuf>
ConnectOptions { surface_size: ClientSurfaceSize, cell_width_px: u32, cell_height_px: u32 }
```

`ClientHandle` is cloneable. Its exact methods are:

```text
send_input(&self, boot_id: &str, pane_id: &str, events: Vec<ClientPaneInputEvent>) -> Result<(), SendError>
send_popup_input(&self, boot_id: &str, terminal_id: &str, events: Vec<ClientPaneInputEvent>) -> Result<(), SendError>
resize(&self, boot_id: &str, options: ConnectOptions) -> Result<(), SendError>
set_focus(&self, boot_id: &str, focused: bool) -> Result<(), SendError>
request(&self, boot_id: &str, method: &str, params: serde_json::Value) -> Result<String, SendError>
focus_pane(&self, boot_id: &str, pane_id: &str) -> Result<String, SendError>
focus_tab(&self, boot_id: &str, tab_id: &str) -> Result<String, SendError>
focus_workspace(&self, boot_id: &str, workspace_id: &str) -> Result<String, SendError>
disconnect(&self)
is_disconnected(&self) -> bool
```

`SendError` is `Full | Disconnected | Invalid(String)`. Sending never waits for
channel capacity; success means queued, not server acknowledgement. Request
methods return a unique ID. The worker rejects stale boot IDs, requests before
the first snapshot and unadvertised methods via
`CommandRejected`. Navigation uses the real `pane.focus`, `tab.focus`, and
`workspace.focus` API methods, not synthetic terminal keys.

`ClientEvent` variants:

```text
Connected(EndpointServerWelcome)
Snapshot(Arc<ClientShellSnapshot>)
Surface(Arc<PaneSurfaceFrame>)
Response { request_id: String, response: serde_json::Value }
CommandRejected { request_id: Option<String>, reason: String }
Message(ServerMessage)
Disconnected { reason: String }
```

The receiver is `crossbeam_channel::Receiver`, re-exported as `Receiver`.
Responses preserve either the endpoint's `{id,result}` or `{id,error}` object;
an API error is not a transport error. Optional unknown named controls are
ignored. Clipboard/title/notification events are data only: the client does
not execute escape sequences, read graphics file paths, or mutate the clipboard.

## Transport Rules

- One dedicated thread owns connect, handshake, ordered writes, and reads.
- 64 channel-queued commands plus one worker-held command, 8 ordered events,
  and one in-flight API request. A waiting request holds later commands in FIFO
  order until the preceding response is complete. Events
  use backpressure rather than losing snapshots, input, patches, or responses.
- 2 MiB outbound payload cap, 32 MiB inbound cap (semantic surfaces may include
  images), 8 MiB aggregate response assembly cap, strict full-payload decoding.
- 10-second handshake/initial-snapshot and partial-frame deadlines, 60-second
  request deadline (matching the upstream command lane);
  1-second socket write timeout; 10 ms read/cancellation polling. Partial reads
  survive polling timeouts without discarding any prefix or payload bytes.
- Geometry matches server bounds: nonzero dimensions, at most 4096 per axis,
  1,000,000 cells total, and cell pixel dimensions at most 4096.
- No reconnect or replay. A changed boot, regressing snapshot, malformed frame,
  mismatched patch, unsolicited response, or response overflow disconnects.
- Dropping the last handle or calling `disconnect()` cancels without joining
  on the GUI thread; dropping the event receiver stops delivery. Explicit
  cancellation closes the receiver without an extra Disconnected event.
- A full event queue intentionally pauses the worker, including writes. Drain
  it continuously in the GUI bridge. Do not clone receivers for broadcast:
  crossbeam receiver clones compete for events.

## Snapshot And Surface

Wire types are re-exported by `herdr_client::protocol` and defined in
`herdr-protocol`. They retain every gen1 bincode enum variant in source order.

- `ClientShellSnapshot`: `boot_id`, `revision`, focused workspace/tab/pane IDs,
  `workspaces`, `tabs`, `panes`, `agents`, `commands`, diagnostics and updates.
- `ClientShellWorkspace`: stable ID, active tab ID, number/label, branch,
  worktree, cwd, focus and agent status. Tabs and panes carry their parent IDs.
- `PaneSurfaceFrame`: `boot_id`, `projection_revision`, `surface_revision`,
  `frame`, pane geometry/scroll/mouse metadata, splits, optional popup, graphics.
- `FrameData`: row-major `cells` of exactly `width * height`, optional cursor,
  hyperlink URI table, and graphics bytes. Popup frames use the same model.
- `CellData`: grapheme `symbol`, packed `fg`/`bg`, `modifier`, `skip`, optional
  hyperlink index. Colors are NOT ARGB: `0..=16` are named colors (0 reset),
  `0x010000XX` is an indexed color, and `0x02RRGGBB` is RGB.
- `CursorState`: zero-based `x/y`, `visible`, DECSCUSR `shape` (0 through 6).
- `ClientPaneInputEvent`: semantic `Key`, `TextCommit`, `Mouse`, or `Paste`.
  Key includes code, modifier bits, kind, repeat count, shifted codepoint,
  generated text, release tracking, physical key ID and optional Windows record.
  Modifier bits: Shift=1, Control=2, Alt=4, Super=8, Hyper=16, Meta=32.

The handshake requests an active surface, with optional surface reuse/delta,
pixel mouse, direct graphics and server-owned keybindings disabled. Baseline
`PaneSurfacePatch` messages still exist in gen1: the client validates and applies
them atomically, then emits a complete immutable `Surface`, not a GUI patch.

Only surfaces matching the latest snapshot's projection revision are emitted.
Future surfaces (including subsequent patches) are retained and emitted after
their matching snapshot arrives.
When handling a new snapshot, the GUI should invalidate any displayed surface
whose `(boot_id, projection_revision)` differs from `(boot_id, revision)` and
wait for matching content. Snapshot metadata and cells must not be mixed across
focus changes. Full surfaces can skip surface revisions; patches cannot.

This is a complete **text** baseline. Images retain their wire scene semantics:
assets contain newly required bytes, not necessarily every live image's bytes.
Rendering/caching images, optional delta codecs, SSH, server spawning, discovery
of all running sessions, automatic reconnect, and Windows transport are out of
scope. No existing Herdr server or session is modified or started by discovery.

## Local Discovery

`Local` checks `HERDR_SOCKET_PATH` (derive `-client.sock` from its file stem),
then `HERDR_CLIENT_SOCKET_PATH`, then `HERDR_SESSION` (default: `default`).
Release config lives at `$XDG_CONFIG_HOME/herdr` or `$HOME/.config/herdr`.
An explicit `Session` bypasses socket overrides; `development: true` selects
`herdr-dev` regardless of this client's compilation profile. Named sessions use
`sessions/<name>/herdr-client.sock`; `default` uses `herdr-client.sock` directly.
`Socket` always means the binary client socket, not the JSON API socket.

## Verification

`cargo test -p herdr-protocol -p herdr-client` runs upstream JSON fixtures,
frozen bincode tags, independent positional source-shape comparisons, framing
and surface limits, and socket-pair/mock-server client tests. It does not build
or modify the sibling Herdr checkout. See `../herdr-protocol/NOTICE.md` and
`../herdr-protocol/LICENSE-APACHE` for upstream provenance and license.

### Opt-In Live Test

The ignored integration test requires an explicit absolute executable path:

```sh
HERDR_TEST_BINARY=/opt/homebrew/bin/herdr \
cargo test -p herdr-client --test live -- --ignored --nocapture
```

`HERDR_TEST_TMPDIR` is optional (defaults to the OS temporary directory); its
parent must exist and be short enough for Unix socket paths. The test creates
a private unique subdirectory using only `std`, with RAII cleanup. It launches
the selected binary's foreground `server` command with a cleared environment,
isolated HOME, XDG config/state/data/cache/runtime directories, explicit config
and socket paths, and a non-login `/bin/sh`. It never discovers an existing
daemon, invokes `server stop`, or signals anything except its own spawned child.
Cleanup kills and waits for that child and removes its temporary directory,
including on assertion failures.

The test exercises the actual stable endpoint welcome, matching snapshots and
surfaces, semantic text plus Enter (checking shell output rather than input
echo), tab/workspace creation and navigation, resize, and detach. After detach
it verifies the daemon is still alive and reconnects to the same boot ID with
the created workspace/tab state intact. Normal `cargo test` leaves it ignored;
explicitly running it without `HERDR_TEST_BINARY` fails rather than falling
back to a personal daemon. No root manifest or additional dependency is needed.
