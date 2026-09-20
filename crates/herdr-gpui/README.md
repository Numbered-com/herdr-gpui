# Herdr Native Shell

A minimal macOS GPUI 0.2.2 client for an **already running** local Herdr daemon.
It does not link, start, stop, or modify Herdr, spawn a PTY, or emulate a terminal.
Runtime dependencies are GPUI, `herdr-client`, `serde_json` for API parameters,
and `unicode-segmentation` for grapheme-aware composer editing.

```sh
cargo run -p herdr-gpui
cargo run -p herdr-gpui -- --session default
cargo run -p herdr-gpui -- --session my-project --dev
cargo run -p herdr-gpui -- --socket /absolute/path/to/herdr-client.sock
```

Without flags, discovery follows `herdr-client`'s environment and release-session
rules. `--socket` must name the binary **client** socket, not the JSON API socket.
`--dev` selects the `herdr-dev` config directory. Connection failure is displayed
in the single-row status bar; Terminal > Reconnect makes a fresh connection with
no input replay. The status dot is green when connected and red otherwise.

## Supported

- Workspace/worktree sidebar with main-checkout parents, indented linked
  workspaces, local collapse arrows, branch details, and daemon-driven
  filled/hollow activity indicators with client-local unseen-completion tracking.
- In-app sidebar menu for settings information, keybinds, config reload, update
  information, and detach/reconnect. Settings are read-only for now.
- Title-only tabs, without an added tab number. Externally created workspaces
  arrive through pushed snapshots without manual refresh.
- Click workspace, tab, agent, or a visible split pane to focus through the API.
- Right-click a tab to select Terminal (default) or Agent presentation locally.
  Agent mode keeps the terminal surface and adds a multiline composer for the
  focused pane. Enter inserts a newline; Cmd-Enter/Send queues Paste + Enter in
  one input batch. Modes and pane-specific drafts survive same-boot reconnects
  in memory only; closing the GUI does not persist drafts or replay input.
- Native File/Terminal menus and creation buttons: **+ New Workspace** in the
  sidebar and a persistent **+** beside the horizontally scrolling tab strip.
- Cmd-N creates and focuses a workspace; Cmd-T creates and focuses a tab.
  Cmd-D splits the focused pane vertically (new pane on the right);
  Cmd-Shift-D splits horizontally (new pane below). Cmd-Shift-] / Cmd-Shift-[
  cycles next/previous tab within the current workspace, wrapping at the ends.
  These shortcuts are native actions, not bytes sent to a terminal.
- Creation omits `cwd`, labels, environment overrides, and split ratio: the
  daemon applies its existing defaults and directory policy. Workspace creation
  supplies the currently focused source workspace when available; tabs and splits
  target the current workspace/pane explicitly. An empty session can create a
  workspace without guessing a local path. Nothing is created while disconnected.
- Vertical mouse-wheel/trackpad scrolling targets the pane under the pointer
  (inside its content, not borders). Fractional pixel motion accumulates into
  terminal lines, with bounded per-event work. Popups capture wheel input only
  within their displayed bounds; input never falls through to a covered pane.
- Direct semantic cell canvas: named ANSI colors, indexed 256-color palette,
  RGB, reset foreground/background, reverse, dim, hidden, bold, italic,
  underline, strikeout, wide-cell skip handling, and cursor shapes.
- Server popup text surfaces centered above the main surface, with popup input
  routing while one is active.
- Native committed text through `EntityInputHandler`, including Unicode and
  composition. In-progress marked text is shown in the status bar.
- Enter, Tab/BackTab, Escape, Backspace, arrows, navigation/editing keys,
  F1-F24, Control characters and modifiers on special keys. Option-printable
  input follows the macOS keyboard layout, including dead keys.
- Cmd-V sends semantic Paste; Cmd-Q or window close detaches without killing
  the daemon or its terminals. Window activation is reported to the daemon.
- Resize uses the actual terminal canvas bounds and measured Menlo cell width,
  excluding the native sidebar, tabs, status bar, and optional agent composer.

Socket I/O belongs to `herdr-client`'s worker. A separate event thread drains all
ordered events into a bounded latest-state cache. The UI samples changed state
at most once per 16 ms without blocking. Snapshots invalidate surfaces with a
different boot/projection revision; input waits for a coherent surface. Reconnect
replaces the cache, so late events from an old connection cannot affect the UI.

### Composer Safety

Composer drafts are keyed by daemon boot, tab and pane identity, never by label.
Send revalidates the captured recipient against the authoritative inbox and
refuses active popups, navigation in progress, stale surfaces, or disconnection.
Queue failure preserves the draft. Queue success clears it and displays **queued,
not confirmed**; it is not an acknowledgement from the agent application.

The native editor and terminal have separate input handlers. Registration-time
session guards reject stale native callbacks after target/mode/focus changes;
submission events also carry an editor revision. A same-pane snapshot/surface gap
blocks sending but does not redirect typing or cancel local IME composition.
Navigation failures are correlated to the latest tracked GUI request so a rejected
request does not permanently suspend the composer.

Herdr still owns the terminal process and conversation, so its TUI can continue
the same session. The local composer cannot see or replace an agent application's
existing prompt buffer. Use an empty prompt and direct terminal mode for approvals
and menus. Agent mode also works on ordinary panes; sending to a shell submits
shell input. No agent-specific chat protocol or automatic prompt detection is used.

### Scrolling Semantics

Upstream `PaneScrollParams` is exactly `{ "pane_id": string,
"offset_from_bottom": u64 }`, an absolute scrollback position, not a wheel delta.
Like the upstream TUI's normal wheel handling, this GUI instead sends semantic
`ClientPaneInputEvent::Mouse` (`ScrollUp`/`ScrollDown`, pane-relative position,
modifiers, and line count). The daemon's `apply_scroll` chooses host scrollback,
alternate-screen behavior, or application mouse reporting using the current
terminal mode. This avoids racing absolute `pane.scroll` offsets against incoming
frames and avoids duplicating terminal-mode policy in the GUI. Scrolling does not
change keyboard focus to the hovered pane. The existing client advertises no pixel
mouse capability, so the daemon uses the supplied cell-coordinate fallback.

Reference sources (read-only): Herdr's `src/api/schema/{workspaces,tabs,panes}.rs`,
`src/app/api/workspaces.rs`, `src/client/shell/mouse.rs`, and
`src/server/pane_input.rs`; Arbor's `crates/arbor-gui/src/app_bootstrap.rs` for
GPUI native action/menu/keybinding patterns.

## Deliberate Limitations

- macOS first; uses system Menlo and system font fallback, no bundled Nerd Font.
  Private-use icons may be missing. ANSI colors use a fixed conventional palette,
  not a synchronized host-terminal theme.
- No draggable scrollback UI, text selection/copy, mouse button/motion reporting, split dragging,
  hyperlink activation, image rendering, or animated blinking.
- No pane/tab/workspace close or delete actions (deferred until confirmation UI),
  horizontal wheel handling, command palette, server-owned keybindings, SSH,
  session picker, automatic reconnect, or daemon lifecycle management.
- IME uses a minimal transient buffer, not a local editable terminal document;
  composition appears in the status bar rather than inline. Key releases and
  physical-key/extended keyboard protocol metadata are not reported.
- The optional composer supports local selection, clipboard operations, Unicode
  grapheme editing and inline IME. Its 16 KiB documents scroll without soft wrap;
  undo/redo and persistent drafts/history are not implemented. Terminal creation
  shortcuts are suppressed while it is focused; buttons remain available.
- Popups have a basic centered text presentation, without native title/border
  chrome. Server notifications/clipboard writes are not executed.
- Rendering is a simple two-pass cell painter, not an optimized damaged-row
  renderer. Large/high-frequency surfaces can consume significant CPU.

## Build And Test

```sh
cargo check -p herdr-gpui
cargo test -p herdr-gpui
cargo clippy -p herdr-gpui --all-targets -- -D warnings
cargo fmt -p herdr-gpui -- --check

# Native agent menu/editor fixtures at two sizes, no daemon or agent process.
just test-agent
```

Requires the normal macOS Rust/Xcode development environment. GPUI's
`runtime_shaders` feature compiles native Metal shaders at app launch, avoiding
the separate downloadable build-time Metal compiler. Tests cover wire colors,
cell modifiers, viewport bounds, semantic key selection, revision coherence,
creation request parameters, workspace-local tab cycling, wheel accumulation,
pane-relative hit testing, and popup routing.
They do not replace an interactive smoke test against a live daemon.
Agent-view tests additionally cover inactive-tab menus, pane-specific drafts,
boot/reconnect fencing, stale native callbacks, transient surface gaps, popup
blocking and atomic input batching with a mock peer. The native fixture uses
AppKit right-click delivery and GPUI keyboard dispatch; IME calls use the explicit
`EntityInputHandler` fallback, not an OS input-method automation driver. Clipboard
shortcuts have headless coverage; native clipboard checks are opt-in via
`HERDR_AGENT_TEST_CLIPBOARD` and restore GPUI-readable clipboard contents. The
default native test leaves the system clipboard untouched.
Actual pi/OpenCode/Claude/Codex workflows and switching between live TUI/GUI clients
remain manual QA; no personal daemon is touched by these fixtures.
