//! Internal failures retain their categories and sources until presentation.
use std::{
    io,
    path::{Path, PathBuf},
};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Toml(#[from] toml::de::Error),
    #[error("{0}")]
    TomlEdit(#[from] toml_edit::TomlError),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Client(#[from] herdr_client::Error),
    #[error("neither XDG_STATE_HOME nor HOME is set")]
    MissingStateRoot,
    #[error("preferences must be an object")]
    PreferencesNotObject,
    #[error("sidebar_width_px must be finite and positive, or null")]
    InvalidStoredWidth,
    #[error("invalid sidebar width")]
    InvalidSidebarWidth,
    #[error("preferences path has no parent")]
    PreferencesPath,
    #[error("{}: {source}", path.display())]
    Path {
        path: PathBuf,
        #[source]
        source: Box<Error>,
    },
    #[error("{source}; removing {}: {cleanup}", path.display())]
    Cleanup {
        #[source]
        source: io::Error,
        path: PathBuf,
        cleanup: io::Error,
    },
    #[error("HOME is not set")]
    MissingHome,
    #[error("XDG_CONFIG_HOME must be an absolute path")]
    RelativeConfigRoot,
    #[error("theme must not be empty")]
    EmptyTheme,
    #[error("{0}.family must not be empty")]
    EmptyFontFamily(&'static str),
    #[error("{0}.size must be finite and between 8 and 48 logical pixels")]
    InvalidFontSize(&'static str),
    #[error("theme must be a name, absolute path, or ~/ path")]
    InvalidThemePath,
    #[error("theme {name:?} not found in {directories:?}")]
    ThemeNotFound {
        name: String,
        directories: Vec<PathBuf>,
    },
    #[error("line {line}: {key}: {source}")]
    ThemeLine {
        line: usize,
        key: String,
        #[source]
        source: ThemeParseError,
    },
    #[error("The original target changed or no longer exists. Cancel and try again.")]
    StaleCloseTarget,
    #[error("The selected connection changed or is not ready. Cancel and try again.")]
    StaleConnection,
    #[error("Not connected to a daemon.")]
    NotConnected,
    #[error("The original tab changed or no longer exists. Cancel and try again.")]
    StaleTab,
    #[error("Enter a tab name.")]
    EmptyTabName,
    #[error("No tab selected.")]
    NoTab,
    #[error("The connection is not ready. Try again.")]
    ConnectionNotReady,
    #[error("Connection is busy. Try again.")]
    ConnectionBusy,
    #[error("The daemon session changed. Reopen the palette.")]
    PaletteSessionChanged,
    #[error("This workspace no longer exists. Reopen the palette.")]
    PaletteWorkspaceRemoved,
    #[error("This command action is not supported by this client.")]
    UnsupportedCommand,
    #[error("This command changed or was removed. Reopen the palette.")]
    PaletteCommandChanged,
    #[error("The original tab no longer exists in its workspace. Reopen the palette.")]
    PaletteTabRemoved,
    #[error("The original pane no longer exists in its tab. Reopen the palette.")]
    PalettePaneRemoved,
    #[error("The selected connection is not ready.")]
    PaletteConnectionNotReady,
    #[error("No current daemon snapshot.")]
    NoSnapshot,
    #[error("No captured daemon session. Reopen the palette.")]
    NoPaletteSession,
    #[error("{0}")]
    DaemonResponse(serde_json::Value),
    #[error("Could not start herdr server: {source}. Use Terminal > Reconnect to retry.")]
    DaemonSpawn {
        #[source]
        source: io::Error,
    },
    #[error("daemon startup cancelled")]
    DaemonCancelled,
    #[error("Timed out waiting for herdr server at {}. Check the Herdr server log and use Terminal > Reconnect.", .0.display())]
    DaemonTimeout(PathBuf),
    #[error("herdr server exited before accepting connections: {0}. Check the Herdr server log.")]
    DaemonExited(std::process::ExitStatus),
}

impl Error {
    pub(crate) fn at_path(self, path: &Path) -> Self {
        Self::Path {
            path: path.to_owned(),
            source: Box::new(self),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ThemeParseError {
    #[error("expected a six-digit RGB hex color (optionally prefixed by #)")]
    InvalidColor,
    #[error("invalid hex color")]
    InvalidHex(#[source] std::num::ParseIntError),
    #[error("expected index=color")]
    MissingPaletteColor,
    #[error("palette index must be between 0 and 255")]
    InvalidPaletteIndex(#[source] std::num::ParseIntError),
    #[error("palette index must be between 0 and 255")]
    PaletteIndexOutOfRange,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn io_context_retains_source_kind_and_cleanup_failure() {
        let error = Error::from(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
            .at_path(Path::new("config.toml"));
        assert_eq!(error.to_string(), "config.toml: denied");
        assert!(
            error
                .source()
                .and_then(|source| source.source())
                .and_then(|source| source.downcast_ref::<io::Error>())
                .is_some_and(|source| source.kind() == io::ErrorKind::PermissionDenied)
        );

        let error = Error::Cleanup {
            source: io::Error::other("write failed"),
            path: "temporary".into(),
            cleanup: io::Error::new(io::ErrorKind::PermissionDenied, "cleanup denied"),
        };
        assert_eq!(
            error.to_string(),
            "write failed; removing temporary: cleanup denied"
        );
        assert!(
            error
                .source()
                .is_some_and(|source| source.is::<io::Error>())
        );
        assert!(
            matches!(error, Error::Cleanup { cleanup, .. } if cleanup.kind() == io::ErrorKind::PermissionDenied)
        );
    }

    #[test]
    fn client_schema_presentation_remains_redacted() -> anyhow::Result<()> {
        let source = serde_json::from_str::<Vec<String>>("{}")
            .err()
            .ok_or_else(|| anyhow::anyhow!("expected schema error"))?;
        let error = Error::from(herdr_client::Error::CatalogSchema(source));
        assert_eq!(error.to_string(), "invalid endpoint catalog schema");
        assert!(
            error
                .source()
                .and_then(|source| source.source())
                .is_some_and(|source| source.is::<serde_json::Error>())
        );
        Ok(())
    }
}
