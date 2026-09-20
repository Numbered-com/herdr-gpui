# Herdr GPUI

[![CI](https://github.com/penso/herdr-gpui/actions/workflows/ci.yml/badge.svg)](https://github.com/penso/herdr-gpui/actions/workflows/ci.yml)

A native Rust/GPUI interface to an existing local Herdr daemon. Workspaces and
worktrees are on the left, agents below them, and the active workspace's tabs
across the top. The center paints the daemon's terminal cells, including split
panes, without running another terminal emulator or wrapping the TUI.

The compact sidebar uses single-line labels, muted branches, status dots, and
independently scrolling spaces and agents sections, following Herdr's TUI.
Linked workspaces are nested beneath the main checkout using Herdr's repository
group metadata, not branch-name guesses. Parent arrows collapse/expand children
locally without closing sessions or changing the selected pane; a child without
an open parent stays visible at the top level. Tabs show titles without added numbers.

Space and agent indicators follow Herdr's activity semantics: filled yellow for
working, red for blocked, teal for an unseen completion, hollow green for idle,
and muted for unknown. Like the TUI, the GUI tracks completion acknowledgement
per client using state-change sequences and coherent surfaces. A new connection
starts with a seen baseline, so its dots can differ from a long-running TUI's
unread history. Activity is never guessed from terminal output.

The sidebar's `menu` opens an in-app popover with Preferences and theme selection,
keybind help, GUI and daemon config reload, available-update information, and safe
detach/reconnect. Escape or clicking outside dismisses it; menu typing never
reaches the terminal. Update commands are displayed, not executed automatically.

Right-click a space to **Rename** it or **Close** it with confirmation. A main
checkout with multiple spaces in its repository offers **Close group**, which
terminates the group's terminals but does not delete checkout files or branches.
Non-linked Git parents also offer **New worktree**: enter a branch or leave it
blank for the daemon default. Creation uses `HEAD` and focuses the new workspace;
it does not grant repository trust. Actions target the clicked space, not the
active one. Text dialogs support Unicode/IME, selection, and Cmd-A/C/X/V.

Linked spaces also offer **Delete worktree checkout**. The GUI obtains the checkout
path from the daemon and requires typing `DELETE`. The daemon removes the checkout
and closes its workspace/terminals, but keeps branches. A daemon dirty/untracked
refusal exposes its error and a separate `FORCE DELETE` confirmation. No local Git
or filesystem checks/removal run in the GUI. **The daemon does not check unpushed
commits**; ignored files are not protected, and detached commits may become
unreachable. Review the warning before proceeding. Escape/outside click dismisses
the dialog, but cannot cancel an operation already queued to the daemon.
See [deletion safety](crates/herdr-gpui/WORKTREE-DELETION.md) for the API contract
and upstream source references.

This is an initial working macOS client, not complete TUI feature parity.
Herdr owns terminal processes and session state; closing this app only detaches.
The Herdr checkout does not need to be modified or linked into this build.

## Run

Install Rust/rustup and the macOS Xcode command-line tools. The repository pins
Rust 1.96.1 and GPUI 0.2.2. Start Herdr normally, then:

```sh
cargo run --locked --release -p herdr-gpui
# Or, with just installed:
just run
just run --session my-project
just run --socket /absolute/path/to/herdr-client.sock
```

`just run` uses the optimized release build for interactive performance. Use
`just run-debug` when debugging; unoptimized GPUI scene construction is notably
slower with a dense terminal on screen.

The explicit socket must be the binary **client** socket, not `herdr.sock`.
The app starts `herdr server` if the default or named-session daemon is absent,
then waits up to 20 seconds to connect without blocking the UI. Herdr must already
be installed. A pulsing status indicator and "Starting Herdr server..." message
remain visible while startup is pending. The executable is discovered
on PATH or in a standard Homebrew, Cargo, or `~/.local/bin` location.
If Herdr cannot be found, an installation modal offers an **Install** button that
opens [herdr.dev](https://herdr.dev/). It does not download or run an installer.
Use **QA > Show herdr non-detected modal** in the macOS menu bar to preview this
warning without restarting, disconnecting, or changing daemon detection.
Explicit `--socket` and `--dev` targets remain attach-only. The app never installs,
stops, or upgrades the daemon, and closing the window leaves it running. A failed
connection appears in the status bar with a red dot. Use Terminal > Reconnect to
retry; there is no permanent reconnect button.

Use **Report issue** on the right of the status bar to open this repository's
GitHub issue forms in your browser. Choose a bug report, feature request, or
documentation issue; redact secrets and private terminal content before submitting.

New workspaces created through Herdr appear automatically while connected.
Revisioned snapshots are pushed by the daemon and applied by the GUI without a
manual refresh. The native integration test checks creation by a separate client,
including preservation of the GUI's current selection and connection. Observed
latency is tens of milliseconds locally, not an instant-delivery guarantee.

### macOS App Bundle

`cargo run` and `just run` use an embedded original Herdr Dock icon, with no runtime
asset paths or image-generation processes. To create a local Finder-launchable app:

```sh
just bundle
open target/release/Herdr.app
```

The bundle is named **Herdr** and contains only the release GUI executable,
`Info.plist`, and its native `.icns` icon. It starts an installed daemon if needed;
it does not bundle, install, or stop a daemon. This is a local unsigned,
unnotarized bundle, not a distribution/signing pipeline. Its version metadata lives
in `assets/macos/Info.plist` and should be updated for releases.

The original charcoal/blue connected-H artwork and provenance are in
[`assets/icons`](assets/icons/README.md). `just icons` regenerates the checked-in
PNG and ICNS from the SVG with macOS Swift/CoreGraphics and `iconutil`.

## GUI Configuration

Click **? Keybinds** at the bottom right of the status bar, or press `Cmd-/`,
to open the native shortcut reference. Press Escape, click outside the modal,
or use its close button to return to the terminal. Terminal input is blocked
while the modal is open. Its search field filters by action, section, or key
combination (for example `pane zoom` or `Cmd+Shift+P`).

`Cmd-,` opens Preferences with Appearance, Fonts, Configuration, and Connection
sections. The theme picker and GUI config reload are available directly from
Preferences; font values remain read-only and are edited in the config file.

Click **Theme** beside Keybinds to browse built-in themes and theme files discovered
in the Herdr and Ghostty theme folders. Type to filter names (case-insensitive),
use Up/Down to navigate, then press Enter or click a result to apply and save it.
The current theme is marked in the list. Escape or clicking outside cancels without
changing the theme. Saving updates only `theme` in the GUI config, preserving its
comments and other settings; load/save errors leave the current appearance intact.

GUI settings live in `$XDG_CONFIG_HOME/herdr/config-gpui.toml`, falling back to
`~/.config/herdr/config-gpui.toml`. The GUI creates a commented default file if it
is absent, without overwriting an existing file. These settings are independent
of the daemon configuration and apply equally when connecting with `--dev`.

See the complete [example config](crates/herdr-gpui/config-gpui.example.toml).
Omitted settings keep their defaults, including individual fields inside a font
section. Unknown keys, empty font families, and invalid sizes are errors.

```toml
theme = "Nord"

[terminal]
family = "Menlo"
size = 14
```

| Section | Default Font Family | Default Size |
| --- | --- | --- |
| `sidebar` | `Menlo` | 12 |
| `tabs` | `.SystemUIFont` | 14 |
| `terminal` | `Menlo` | 14 |
| `ui` | `.SystemUIFont` | 12 |

Sizes are **logical pixels**, not points or physical display pixels. Fractional
sizes are supported; values must be finite and between 8 and 48 inclusive. Line
height scales as `size * 20 / 14`. Fonts must be installed locally; none are bundled.
Restart the GUI or use its **GUI config reload** action after editing. Reloading
daemon config is separate and does not apply these appearance settings. There is
no automatic file watcher.

### Themes

Built-in names are case-sensitive: `Default`, `Nord`, `Dracula`,
`Catppuccin Mocha`, and `Catppuccin Latte`. `Default` preserves the original
terminal background, foreground, cursor, ANSI/256-color palette and sidebar
surface/active/muted colors. Other themes derive chrome colors by blending the
background and foreground. Built-ins have small hardcoded palettes, not bundled
third-party assets; entries 16 through 255 retain the conventional color cube and
grayscale ramp.

`theme` also accepts an absolute path, a `~/` path, or a Ghostty theme filename.
Built-in names take priority; use an explicit path to select a file with the same
name. Named files are searched in this order:

1. `$XDG_CONFIG_HOME/herdr/themes` (or `~/.config/herdr/themes`).
2. `$XDG_CONFIG_HOME/ghostty/themes` (or `~/.config/ghostty/themes`).
3. `$GHOSTTY_RESOURCES_DIR/themes`, when set.
4. `/Applications/Ghostty.app/Contents/Resources/ghostty/themes`.
5. `$XDG_DATA_HOME/ghostty/themes` (or `~/.local/share/ghostty/themes`).
6. `ghostty/themes` under each `$XDG_DATA_DIRS` entry (defaults to
   `/usr/local/share` and `/usr/share`).

Ghostty files support `background`, `foreground`, `cursor-color`, and
`palette = INDEX=COLOR` for indices 0 through 255. Colors must be exactly six hex
digits, optionally prefixed with `#`. Blank lines and full-line `#` comments are
allowed; inline comments and named colors are not supported for color values.
Malformed supported colors report the file and line number. Other settings are
ignored, including includes and commands: theme loading does not execute them.
Repeated colors use the last value. Unspecified colors retain defaults, except
that an omitted cursor color follows the theme foreground.

## Controls

| Control | Action |
| --- | --- |
| Sidebar workspace/agent | Focus its workspace or pane |
| Right-click sidebar workspace | Rename, confirmed close, new worktree on Git parents, delete linked checkout |
| Top tab / terminal pane | Focus the tab or pane |
| Cmd-N | New workspace using daemon directory policy |
| Cmd-T | New tab |
| Cmd-D / Cmd-Shift-D | Split right / below |
| Cmd-Shift-] / Cmd-Shift-[ | Next / previous tab |
| Cmd-1 through Cmd-9 | Focus the corresponding numbered tab in the current workspace |
| Cmd-Alt-Left/Right/Up/Down | Focus a pane in that direction |
| Cmd-Alt-] / Cmd-Alt-[ | Next / previous pane in the current tab |
| Cmd-Shift-Enter | Toggle focused pane zoom |
| Cmd-W / Cmd-Shift-W | Confirm closing the focused pane / tab |
| Cmd-P | Workspace picker |
| Cmd-Shift-P | Command palette: native actions and configured daemon entries |
| Cmd-B | Toggle the sidebar locally |
| Cmd-, | Settings |
| Cmd-/ | Native shortcut reference |
| Wheel / trackpad | Scroll the hovered terminal through Herdr |
| Cmd-V | Semantic paste |
| Cmd-Q | Quit the GUI, leaving terminals running |

The native File and Terminal menus expose the creation and navigation actions.
Terminal keyboard input and committed Unicode text go directly to Herdr's
semantic input protocol.

The command palette includes native actions (including unbound Themes and
Reconnect) and configured daemon command entries. Cmd-P opens the workspace
picker, not the command palette. Cmd-B only changes this client's sidebar
visibility; it does not change daemon state.

Closing a pane or tab requires confirmation because it can terminate running
processes. **Cancel is selected by default**: Enter alone cancels; press Tab then
Enter to select and confirm Close. Quitting the GUI remains a detach operation,
not a pane/tab close.

## Structure

- `crates/herdr-protocol`: locally maintained generation-1 wire types, fixtures,
  framing and surface validation. See its `NOTICE.md` for upstream provenance.
- `crates/herdr-client`: bounded background socket transport, capability
  negotiation, ordered commands, boot fencing, and full text-surface assembly.
- `crates/herdr-gpui`: native shell, live state, input routing and GPUI canvas.

Following Arbor's approach, dependency versions live at workspace level and
blocking I/O stays off the GPUI thread. This build uses registry GPUI, not Zed's
editor crates or a dependency on a sibling checkout.

## Verification

```sh
just ci
just build-release
just test-build
# Optional, requires an installed Herdr executable:
just test-live /opt/homebrew/bin/herdr
# Native GUI integration; opens a temporary window on the active desktop:
just test-gui /opt/homebrew/bin/herdr
# Native sidebar text/glyph regression, no daemon required:
just test-sidebar
# Native hover/scroll benchmark with a 30 ms p95 CPU scene budget:
just test-perf
```

The live test creates a private temporary HOME/config/socket environment,
starts its own daemon, tests terminal input/output, creation/navigation, resize,
and detach/reconnect, and cleans up only that daemon. It is ignored by default.
It has been exercised against Herdr 0.9.1. Normal tests use fixtures/mock peers
and require no running daemon.

The native GUI test dispatches real GPUI keybindings and text input, checks the
state consumed by the window, and fails on status-bar errors. It covers tab and
workspace creation/navigation, both split directions, shell output, native window
resize, and reconnect with fresh input. It verifies that the isolated daemon
survives GUI exit. The `integration-test` feature enables this harness only for
test builds; it is not enabled by `just run`.

This tests the native app and GPUI event routing, but does not replace screenshot
comparison or OS-level keyboard/IME delivery testing.

### Continuous Integration

GitHub Actions checks formatting, denies Clippy warnings, and runs the tests with
both default and all features. The tests include real executable CLI checks for
help, malformed arguments, conflicting options, and test-mode gating, with a
timeout to catch startup hangs. These checks do not open windows.

A headless GPUI layout regression also renders the actual sidebar with 40
workspaces and short/long agent labels. It checks shaped text, not just container
widths: short names must remain intact, long names must retain a readable prefix
and ellipsis, and the agents section must stay visible. This catches premature
text truncation that protocol and action-dispatch tests cannot detect.

`just test-sidebar` complements that mock-platform test with the actual macOS
font renderer and full application layout. It checks native glyphs and clipping
over 12 draws at four window sizes. This catches truncated font runs that the
mock text system does not model. It requires an active desktop, but uses only
fixture data and never connects to your daemon. It is not a screenshot/pixel
comparison test.

The native sidebar fixture also right-clicks Git parent/child spaces and opens all four
workspace dialogs at 640x400 and 1200x780, checking Unicode editing, caret/IME
bounds, focus restoration, and terminal/action isolation without a daemon.

On macOS this also verifies that the running application's native Dock image is
valid and 1024x1024; a normal unit test checks the embedded PNG header/dimensions.

### Performance

`just test-perf` opens a daemon-free native fixture with a dense 160x50 terminal,
40 workspaces, and 40 agents. It dispatches real window-local mouse/scroll events
and measures cold frames, warm hover, and both sidebar lists' scrolling. It also
asserts that unchanged terminal cells need zero new text-shaping calls, validates
batched background counts, and compares cached glyphs with freshly shaped ones.

On the development M4 Max, caching and background batching reduced release hover
p95 from about 51 ms to 12 ms, and scrolling from 56 ms to 14 ms. The benchmark
measures CPU event-to-scene construction, not GPU completion or pointer-to-screen
latency. Use `just test-perf 50` to set a different calibrated budget; native tests
remain opt-in rather than imposing machine-dependent timings on hosted CI.

See [PERFORMANCE.md](crates/herdr-gpui/PERFORMANCE.md) for the before/after results,
reference mode, workload, deterministic checks, and remaining limitations.

Separate Apple Silicon and Intel macOS jobs build optimized executables and run
the CLI tests against those release binaries. CI validates builds but does not
publish distributable binaries; packaging, dependency notices, signing, and
notarization are a separate release milestone.
GPUI compiles its Metal shaders at runtime, so CI does not need the separate
build-time Metal compiler download.

The live protocol and desktop GUI tests are deliberately ignored in hosted CI:
they require an explicitly selected Herdr binary, and the GUI test also needs an
active desktop. Run `just test-live` and `just test-gui` locally as shown above.

## Sidebar Width

Workspace titles show the GitHub organization or owner avatar, resolved from
the local repository's `origin` remote. Git lookups and avatar downloads run
in the background, with results shared per owner for the app session. The
GitHub mark is used while loading or when an avatar is unavailable. No GitHub
token is needed; avatar requests go to `avatars.githubusercontent.com`.

Drag the sidebar's right edge to resize it; double-click the divider to restore
the default width. The terminal resizes automatically. Width is remembered per
daemon socket in `$XDG_STATE_HOME/herdr/gpui/local-<socket-hash>.json`, defaulting
to `~/.local/state/herdr/gpui/`. These logical-pixel preferences are separate
from the TUI's column-based settings. Narrow windows temporarily limit the
displayed width without replacing your saved preference.

## Next Milestones

- Selection/copy, hyperlink interaction, richer mouse support, and inline IME.
- Full worktree/agent management.
- Editable settings and bundled fonts.
- Automatic reconnect, optimized terminal painting and graphics support.
- Signed macOS app packaging, then SSH endpoints.

Current rendering defaults to Menlo with configurable fonts and themes. Images and terminal
notifications/clipboard writes are deliberately not executed. See
[`crates/herdr-gpui/README.md`](crates/herdr-gpui/README.md) for the detailed scope.

## License

Apache-2.0. See [the license](crates/herdr-protocol/LICENSE-APACHE) and
[upstream protocol attribution](crates/herdr-protocol/NOTICE.md).
