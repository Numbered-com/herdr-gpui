# Herdr Native Shell

A minimal macOS GPUI 0.2.2 client for a local Herdr daemon.
It starts an installed `herdr server` when the local daemon is absent. Explicit
socket and development targets remain attach-only. It does not link, install,
stop, or upgrade Herdr, spawn a PTY, or emulate a terminal.
Runtime dependencies include GPUI, `herdr-client`, `serde_json` for API parameters,
and `serde`/`toml` for GUI configuration.

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
no input replay. The status dot is green when connected, pulses during startup,
and turns red on connection failure.

Default and named-session startup discovers Herdr on PATH or in standard
Homebrew, Cargo, or `~/.local/bin` locations, then waits up to 20 seconds to
connect without blocking the UI. If Herdr cannot be found, an installation modal
offers an **Install** button that opens [herdr.dev](https://herdr.dev/); it never
downloads or runs an installer. After installing, choose Terminal > Reconnect.
**QA > Show herdr non-detected modal** previews the warning without restarting,
disconnecting, or changing daemon detection. Closing the GUI leaves the daemon
and its terminals running.

## Configuration

See [GUI configuration](../../README.md#gui-configuration) for the config path,
font defaults, theme lookup order, and reload behavior, and
[`config-gpui.example.toml`](config-gpui.example.toml) for a complete example.
Font sizes use logical pixels (finite 8..48), not typographic points. Restart the
GUI or invoke GUI config reload after edits; daemon config reload is separate.

The standalone `src/config.rs` module exposes `Config::load()` and
`Config::path()`, both returning errors as strings. `Config::theme()` resolves
built-ins or Ghostty files into a `Theme` with packed 24-bit RGB colors and all
256 palette entries. Theme resolution is a separate fallible step from loading
and validating TOML. Font sections can override either family or size without
repeating the other field. `FontConfig::line_height()` returns `size * 20 / 14`.
Config and theme I/O is synchronous; GUI callers should schedule it accordingly.

## Supported

- Workspace/worktree sidebar with main-checkout parents, indented linked
  workspaces, local collapse arrows, branch details, and daemon-driven
  filled/hollow activity indicators with client-local unseen-completion tracking.
- In-app sidebar menu for settings information, keybinds, config reload, update
  information, and detach/reconnect. Styled Preferences include Appearance,
  Fonts, Configuration, and Connection sections, with theme selection and GUI
  config reload; font values remain read-only and are edited in the config file.
- A searchable theme picker previews the available names from built-ins and
  Herdr/Ghostty theme folders. Selecting a theme applies and saves it while
  preserving other GUI config settings and comments.
- Title-only tabs, without an added tab number. Externally created workspaces
  arrive through pushed snapshots without manual refresh.
- Click workspace, tab, agent, or a visible split pane to focus through the API.
- Native File/Terminal menus and creation buttons: **+ New Workspace** in the
  sidebar and a persistent **+** beside the horizontally scrolling tab strip.
- Cmd-N creates and focuses a workspace; Cmd-T creates and focuses a tab.
  Cmd-D splits the focused pane vertically (new pane on the right);
  Cmd-Shift-D splits horizontally (new pane below). Cmd-Shift-] / Cmd-Shift-[
  cycles next/previous tab within the current workspace, wrapping at the ends.
  These shortcuts are native actions, not bytes sent to a terminal.
- Cmd-1 through Cmd-9 focuses the corresponding numbered tab in the current
  workspace. Cmd-Alt-Left/Right/Up/Down focuses a pane in that direction;
  Cmd-Alt-] / Cmd-Alt-[ cycles next/previous pane within the current tab.
  Cmd-Shift-Enter toggles focused pane zoom.
- Cmd-W closes the focused pane and Cmd-Shift-W closes the focused tab only after
  a confirmation dialog. **Cancel is selected by default**: Enter alone cancels;
  Tab then Enter selects and confirms Close. Closing can terminate running
  processes, unlike quitting the GUI, which only detaches.
- Cmd-Shift-P opens the command palette with native actions and configured daemon
  command entries, including native Themes and Reconnect actions without dedicated
  shortcuts. Cmd-P opens the workspace picker instead.
- Cmd-B toggles sidebar visibility locally without changing daemon state.
  Cmd-, opens Settings; Cmd-/ opens the grouped native shortcut reference.
  Native shortcut labels and keycaps come from the shared `controls::COMMANDS`
  catalog, with Cmd-V semantic paste shown separately. Search filters by action,
  section, or key combination. Preferences, keybinds, theme/palette pickers, and
  close confirmations use themed centered modals and configured UI fonts;
  modal input does not reach the terminal.
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
- Resize uses the actual terminal canvas bounds and measured configured font cell width,
  excluding the native sidebar, tabs and status bar.

Socket I/O belongs to `herdr-client`'s worker. A separate event thread drains all
ordered events into a bounded latest-state cache. The UI samples changed state
at most once per 16 ms without blocking. Snapshots invalidate surfaces with a
different boot/projection revision; input waits for a coherent surface. Reconnect
replaces the cache, so late events from an old connection cannot affect the UI.

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

- macOS first; defaults to system Menlo and system font fallback, no bundled Nerd Font.
  Private-use icons may be missing. Fonts and palettes are configured locally,
  not synchronized from the host terminal's theme.
- No draggable scrollback UI, text selection/copy, mouse button/motion reporting, split dragging,
  hyperlink activation, image rendering, or animated blinking.
- No rename dialogs, workspace close/delete actions, horizontal wheel handling,
  server-owned keybindings, SSH,
  session picker, automatic reconnect, or daemon stop/upgrade management.
- IME uses a minimal transient buffer, not a local editable terminal document;
  composition appears in the status bar rather than inline. Key releases and
  physical-key/extended keyboard protocol metadata are not reported.
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
```

Requires the normal macOS Rust/Xcode development environment. GPUI's
`runtime_shaders` feature compiles native Metal shaders at app launch, avoiding
the separate downloadable build-time Metal compiler. Tests cover wire colors,
cell modifiers, viewport bounds, semantic key selection, revision coherence,
creation request parameters, workspace-local tab cycling, wheel accumulation,
pane-relative hit testing, and popup routing.
They do not replace an interactive smoke test against a live daemon.
