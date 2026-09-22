//! The native menu bar. Every item dispatches the same `Command` the palette
//! and the keymap use, so a command exists in one place only.

use crate::{
    CheckForUpdates, Quit, RunCommand, ShowHerdrNotDetected, ShowLogs, ShowUpdatePreview,
    actions::ShowToastPreview, controls::Command,
};
use gpui::{Menu, MenuItem};
use herdr_client::protocol::SemanticNotificationKind;

pub(crate) fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Herdr".into(),
            items: vec![
                MenuItem::action(
                    "About Herdr",
                    RunCommand {
                        command: Command::About,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Command Palette",
                    RunCommand {
                        command: Command::Palette,
                    },
                ),
                MenuItem::action(
                    "Settings",
                    RunCommand {
                        command: Command::Settings,
                    },
                ),
                MenuItem::action(
                    "Keybinds",
                    RunCommand {
                        command: Command::Keybinds,
                    },
                ),
                MenuItem::action("Check for Updates...", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action("Quit Herdr", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action(
                    "New Workspace",
                    RunCommand {
                        command: Command::Workspace,
                    },
                ),
                MenuItem::action(
                    "New Tab",
                    RunCommand {
                        command: Command::Tab,
                    },
                ),
                MenuItem::action(
                    "Switch Workspace",
                    RunCommand {
                        command: Command::WorkspacePicker,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Close Pane...",
                    RunCommand {
                        command: Command::ClosePane,
                    },
                ),
                MenuItem::action(
                    "Close Tab...",
                    RunCommand {
                        command: Command::CloseTab,
                    },
                ),
            ],
        },
        Menu {
            name: "Terminal".into(),
            items: vec![
                MenuItem::action(
                    "Split Vertically (Right)",
                    RunCommand {
                        command: Command::SplitRight,
                    },
                ),
                MenuItem::action(
                    "Split Horizontally (Down)",
                    RunCommand {
                        command: Command::SplitDown,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Next Tab",
                    RunCommand {
                        command: Command::NextTab,
                    },
                ),
                MenuItem::action(
                    "Previous Tab",
                    RunCommand {
                        command: Command::PreviousTab,
                    },
                ),
                MenuItem::action(
                    "Toggle Pane Zoom",
                    RunCommand {
                        command: Command::Zoom,
                    },
                ),
                MenuItem::action(
                    "Toggle Sidebar",
                    RunCommand {
                        command: Command::ToggleSidebar,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Reconnect",
                    RunCommand {
                        command: Command::Reconnect,
                    },
                ),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action(
                    "New Window",
                    RunCommand {
                        command: Command::NewWindow,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action("GPUI Logs", ShowLogs),
            ],
        },
        Menu {
            name: "QA".into(),
            items: vec![
                MenuItem::action("Show herdr non-detected modal", ShowHerdrNotDetected),
                MenuItem::action("Show app update available", ShowUpdatePreview),
                MenuItem::separator(),
                MenuItem::action(
                    "Show NeedsAttention toast",
                    ShowToastPreview {
                        kind: SemanticNotificationKind::NeedsAttention,
                    },
                ),
                MenuItem::action(
                    "Show Finished toast",
                    ShowToastPreview {
                        kind: SemanticNotificationKind::Finished,
                    },
                ),
                MenuItem::action(
                    "Show UpdateInstalled toast",
                    ShowToastPreview {
                        kind: SemanticNotificationKind::UpdateInstalled,
                    },
                ),
                MenuItem::action(
                    "Show Custom toast",
                    ShowToastPreview {
                        kind: SemanticNotificationKind::Custom,
                    },
                ),
            ],
        },
    ]
}
