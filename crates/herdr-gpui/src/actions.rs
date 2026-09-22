//! GPUI actions the app registers and the keystrokes bound to them. Every
//! shortcut comes from the single `controls::COMMANDS` table, so the palette,
//! the menu bar, and the keymap cannot drift apart.

use crate::controls::{self, Command};
use gpui::{Action, App, KeyBinding, actions};

actions!(
    herdr,
    [
        Quit,
        PlaySound,
        ShowHerdrNotDetected,
        ShowLogs,
        CheckForUpdates,
        ShowUpdatePreview
    ]
);

#[derive(Clone, PartialEq, serde::Deserialize, Action)]
#[action(no_json)]
pub(crate) struct RunCommand {
    pub(crate) command: Command,
}

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
    cx.bind_keys(
        controls::COMMANDS
            .iter()
            .filter(|info| !info.shortcut.is_empty() && info.command != Command::Quit)
            .map(|info| {
                KeyBinding::new(
                    info.shortcut,
                    RunCommand {
                        command: info.command,
                    },
                    None,
                )
            }),
    );
}
