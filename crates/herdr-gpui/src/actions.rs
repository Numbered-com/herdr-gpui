//! GPUI actions the app registers and the keystrokes bound to them. Every
//! shortcut comes from the config's resolved `Keymap`, so the palette, the
//! menu bar, and the keymap cannot drift apart.

use crate::controls::Command;
use gpui::{Action, App, KeyBinding, actions};

actions!(
    herdr,
    [
        Quit,
        PlaySound,
        ShowHerdrNotDetected,
        ShowLogs,
        CheckForUpdates,
        ShowUpdatePreview,
        ShowUpdateDownloadPreview,
        ShowUpdateHomebrewPreview
    ]
);

#[derive(Clone, PartialEq, serde::Deserialize, Action)]
#[action(no_json)]
pub(crate) struct RunCommand {
    pub(crate) command: Command,
}

#[derive(Clone, PartialEq, serde::Deserialize, Action)]
#[action(no_json)]
pub(crate) struct ShowToastPreview {
    pub(crate) kind: herdr_client::protocol::SemanticNotificationKind,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, PartialEq, serde::Deserialize, Action)]
#[action(no_json)]
pub(crate) struct SetBadgePreview {
    pub(crate) enabled: bool,
}

/// Binds the keymap from the last validated config, or the catalog defaults
/// before any config has loaded.
pub(crate) fn bind_keys(cx: &mut App) {
    let keymap = cx
        .try_global::<crate::app::InitialAppearance>()
        .map(|appearance| appearance.config.keybindings.clone())
        .unwrap_or_default();
    cx.bind_keys(keymap.bindings().map(|(command, keystroke)| {
        if command == Command::Quit {
            KeyBinding::new(keystroke, Quit, None)
        } else {
            KeyBinding::new(keystroke, RunCommand { command }, None)
        }
    }));
    cx.bind_keys(crate::log_window::key_bindings());
}

/// Replaces every binding after a config reload. The menu bar reads its
/// shortcut labels from the keymap when it is installed, so it is rebuilt too.
pub(crate) fn rebind_keys(cx: &mut App) {
    cx.clear_key_bindings();
    bind_keys(cx);
    cx.set_menus(crate::menus());
}
