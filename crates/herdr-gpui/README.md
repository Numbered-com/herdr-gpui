# Herdr Native Shell

A macOS GPUI 0.2.2 client for a Local daemon and saved SSH hosts.
It starts an installed local `herdr server` when absent; explicit socket and
development targets remain attach-only. It does not link or install Herdr, stop
daemons, spawn a local PTY, or emulate a terminal. Herdr's remote bridge may start
the named remote session. SSH requires an installed POSIX Herdr, noninteractive authentication,
and an already trusted host key.
Runtime dependencies include GPUI, `herdr-client`, `serde_json` for API parameters,
`ureq` for background GitHub owner avatar downloads, and `serde`/`toml` for GUI configuration.

```sh
cargo run -p herdr-gpui
cargo run -p herdr-gpui -- --session default
cargo run -p herdr-gpui -- --session my-project --dev
cargo run -p herdr-gpui -- --socket /absolute/path/to/herdr-client.sock
```

Without flags, discovery follows `herdr-client`'s environment and release-session
rules. `--socket` must name the binary **client** socket, not the JSON API socket.
`--dev` selects the `herdr-dev` config directory. Connection failure is displayed
in the single-row status bar and host rows. Endpoints reconnect independently with
bounded backoff; Terminal > Reconnect retries the selected endpoint immediately,
without input replay. Detach pauses retries for that endpoint until Reconnect.
The status dot pulses amber during local daemon startup, is green when connected,
and red otherwise.

Spaces lists Local first, then saved hosts in the upstream catalog's order.
Enabled hosts connect in the background with inactive terminal surfaces; disabled
hosts remain visible. Host and repository collapse state is endpoint-scoped, and
Agents aggregates all connected endpoints with host labels. The catalog is read
through `herdr-client` every two seconds; changes to targets, sessions, enablement,
and ordering are reflected without restarting. Catalog errors preserve the last
valid list. The GUI never edits saved hosts or installs remote software.
An explicit `--socket` is isolated: it never loads or connects saved hosts, or
reads/writes saved selection. Other launches read `client/endpoint-selection.json`
once, under the same release/dev state root as the catalog. Local remains usable
while the desired host connects; restoration waits for its first snapshot rather
than timing out during SSH startup. Explicit host/workspace/agent choices cancel
pending restoration and persist selection asynchronously, without editing hosts.
Other running clients' choices never change this window's selection. Missing,
malformed, disabled or removed saved choices fall back to the valid legacy
catalog selection (normally Local), matching upstream. Live removal/disable
returns to Local and cancels pending restoration; re-enabling does not steal focus.
Automatic activation failure returns to Local without overwriting the saved
preference or repeatedly attempting the same handoff. Write failures are shown
in the status bar and do not undo the UI choice. Host editing remains in
`herdr machine`.

Switching revokes the old host's focus before releasing its surface, then resizes
and activates the selected host. Input waits for the activation acknowledgement
and a coherent surface at the current viewport size. Handoffs time out after five
seconds and return to Local; returning to Local never waits on a remote release.
Servers without surface-switching support remain usable as single targets.

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

## Title Bar

macOS keeps `Some(TitlebarOptions)` and the native Herdr window title/traffic lights,
with transparent chrome and lights positioned at (9, 9) logical pixels. A full-width
34px header blends `theme.surface` roughly 10% toward white, subtly lifting dark
themes while keeping light themes light. It sits above the sidebar and tabs: 80px
of traffic-light clearance, an empty flexible center, and a 40px upper-right slot.
The slot centers a 16px circular user avatar with a 12px SVG in a 28px hover target, tinted from
the theme foreground. This placeholder for future GitHub sign-in has no account
action, network requests, personal identity, or tooltip. It consumes clicks so
double-clicking it does not invoke the title-bar action.
The header and clearance remain in fullscreen so the body layout stays stable.
Windows/Linux keep the existing native frame and do not render this header.

The reference is Zed's `crates/platform_title_bar/src/platform_title_bar.rs` and
window options in `crates/zed/src/zed.rs`, not a build dependency. Double-click calls
`Window::titlebar_double_click()` to honor the OS preference. Unlike newer Zed,
registry GPUI 0.2.2 has no macOS `start_window_move` implementation and ignores
`WindowControlArea::Drag`. We leave `is_movable` unchanged and rely on native AppKit
dragging, rather than adding ineffective custom drag handlers or platform patches.

Headless tests check the actual root header/center/account-slot bounds at wide,
minimum, and narrow sizes, including mock fullscreen entry/exit, and that the
avatar and hit target stay centered. An SVG decoding test checks the embedded user
icon produces a nonempty mask. These do not verify AppKit behavior. Native QA remains
required for dragging across the header, traffic-light alignment and actions,
double-click preferences (zoom/minimize/do nothing), fullscreen transitions and
auto-hidden controls, theme changes, and modal/focus/IME behavior. Windows/Linux
native-frame appearance also remains unverified by these macOS tests.

## Supported

- Workspace/worktree sidebar with main-checkout parents, indented linked
  workspaces, local collapse arrows, branch details, and daemon-driven
  filled/hollow activity indicators with client-local unseen-completion tracking.
- Resizable sidebar with width persisted per local daemon socket, shared across
  host groups. Local workspace titles show repository owner avatars; remote
  workspaces use the GitHub fallback mark without resolving remote paths locally.
- In-app sidebar menu for settings information, keybinds, config reload, update
  information, and detach/reconnect. Styled Preferences include Appearance,
  Fonts, Configuration, and Connection sections, with theme selection and GUI
  config reload; font values remain read-only and are edited in the config file.
- A searchable theme picker previews the available names from built-ins and
  Herdr/Ghostty theme folders. Selecting a theme applies and saves it while
  preserving other GUI config settings and comments.
- Title-only tabs, without an added tab number. Externally created workspaces
  arrive through pushed snapshots without manual refresh.
- Right-click any tab without focusing it to open Rename.
  Actions retain the clicked tab/workspace and reject stale connections or targets.
  Rename selects the current label in a native IME-aware field, with inline errors;
  Close uses the existing cancel-by-default confirmation. Escape or an outside
  left/right click dismisses the menu without sending terminal input.
- Click workspace, tab, agent, or a visible split pane to focus through the API.
- Native File/Terminal menus and creation buttons: **+ New Workspace** in the
  sidebar and a persistent 18px SVG **+** in a 44px-wide button beside the horizontally
  scrolling tab strip. Each tab has a 16px SVG close cross in a 24px hit target;
  it opens the same cancel-by-default confirmation without focusing an inactive tab.
  Both icons use the current theme's foreground tint.
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

- macOS first; defaults to Menlo on macOS and DejaVu Sans Mono elsewhere, with
  system font fallback and no bundled Nerd Font.
  Private-use icons may be missing. Fonts and palettes are configured locally,
  not synchronized from the host terminal's theme.
- No draggable scrollback UI, text selection/copy, mouse button/motion reporting, split dragging,
  hyperlink activation, image rendering, or animated blinking.
- No workspace/pane rename dialogs, workspace close/delete actions, horizontal wheel handling,
  server-owned keybindings, session picker, saved-host editing, or daemon
  stop/upgrade management.
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

`just test-sidebar` runs isolated, daemon-free native fixtures on the active
desktop. On macOS it checks exact-window clicks with a decoy key window, host
selection/disabled hosts, scoped collapse, duplicate-ID navigation routing,
composition preservation, menu isolation, and long-label native glyph clipping.
Scroll independence uses scroll handles and native draws, not trackpad events.
See the root README for the full verification scope and remaining limitations.
