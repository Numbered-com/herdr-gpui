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
            name: "View".into(),
            items: vec![
                MenuItem::action(
                    "Increase Font Size",
                    RunCommand {
                        command: Command::IncreaseFontSize,
                    },
                ),
                MenuItem::action(
                    "Decrease Font Size",
                    RunCommand {
                        command: Command::DecreaseFontSize,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Reset Font Size",
                    RunCommand {
                        command: Command::ResetFontSize,
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
                    "Open Notification Target",
                    RunCommand {
                        command: Command::OpenNotificationTarget,
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The font size items are the only way to reach these commands from the
    /// macOS menu bar, and each must dispatch the catalog command rather than
    /// an action of its own.
    #[test]
    fn view_menu_carries_the_font_size_commands() {
        let menus = menus();
        let names: Vec<_> = menus.iter().map(|menu| menu.name.as_ref()).collect();
        assert_eq!(names, ["Herdr", "File", "View", "Terminal", "Window", "QA"]);

        let view = menus
            .iter()
            .find(|menu| menu.name.as_ref() == "View")
            .unwrap();
        let actions: Vec<_> = view
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, action, .. } => Some((name.as_ref(), action)),
                _ => None,
            })
            .collect();
        let expected = [
            ("Increase Font Size", Command::IncreaseFontSize),
            ("Decrease Font Size", Command::DecreaseFontSize),
            ("Reset Font Size", Command::ResetFontSize),
        ];
        assert_eq!(actions.len(), expected.len());
        for ((name, action), (label, command)) in actions.iter().zip(expected) {
            assert_eq!(*name, label);
            assert!(action.partial_eq(&RunCommand { command }), "{label}");
        }
        // Reset is a different kind of act from stepping, so it sits apart.
        assert!(matches!(view.items[2], MenuItem::Separator));
    }
}
