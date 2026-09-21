//! Local socket rules adapted from herdr src/server/socket_paths.rs and
//! src/session.rs (Apache-2.0; see ../../herdr-protocol/NOTICE.md).
//! Modified: explicit release/dev selection, no server spawning or global state.
use crate::{Error, Result};
use std::{
    env,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ConnectTarget {
    /// Environment overrides, then HERDR_SESSION in the release config directory.
    #[default]
    Local,
    /// Explicit session bypasses socket overrides; "default" selects the root.
    Session { name: String, development: bool },
    /// Exact client protocol socket, not the JSON API socket.
    Socket(PathBuf),
    /// Noninteractive SSH attachment to an installed remote Herdr (POSIX hosts).
    Ssh { target: String, session: String },
}

impl ConnectTarget {
    /// Whether the daemon runs on another machine, so local filesystem paths are
    /// meaningless to it. The other variants all address a socket on this host.
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Ssh { .. })
    }
}

pub fn session_socket(config_dir: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty()
        || name.len() > 64
        || matches!(name, "." | "..")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(Error::InvalidSession);
    }
    Ok(if name == "default" {
        config_dir.to_owned()
    } else {
        config_dir.join("sessions").join(name)
    }
    .join("herdr-client.sock"))
}

impl ConnectTarget {
    pub fn socket_path(&self) -> Result<PathBuf> {
        self.socket_path_with(|name| env::var_os(name))
    }

    /// Standard session endpoint, ignoring socket overrides (including `Socket`).
    /// This identifies a user-configured local location, not a daemon executable.
    pub fn local_session_socket_path(&self) -> Result<PathBuf> {
        self.local_session_socket_path_with(|name| env::var_os(name))
    }

    fn local_session_socket_path_with(
        &self,
        var: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Result<PathBuf> {
        let target = if matches!(self, Self::Socket(_)) {
            &Self::Local
        } else {
            self
        };
        target.socket_path_with(|name| match name {
            "HERDR_SOCKET_PATH" | "HERDR_CLIENT_SOCKET_PATH" => None,
            _ => var(name),
        })
    }

    fn socket_path_with(
        &self,
        var: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Result<PathBuf> {
        if matches!(self, Self::Ssh { .. }) {
            return Err(Error::NoLocalSocket);
        }
        if let Self::Socket(path) = self {
            return Ok(path.clone());
        }
        if matches!(self, Self::Local) {
            if let Some(path) = var("HERDR_SOCKET_PATH") {
                let path = PathBuf::from(path);
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("herdr");
                return Ok(path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join(format!("{stem}-client.sock")));
            }
            if let Some(path) = var("HERDR_CLIENT_SOCKET_PATH") {
                return Ok(path.into());
            }
        }
        let development = matches!(
            self,
            Self::Session {
                development: true,
                ..
            }
        );
        let base = var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| var("HOME").map(|p| PathBuf::from(p).join(".config")))
            .unwrap_or_else(env::temp_dir)
            .join(if development { "herdr-dev" } else { "herdr" });
        let name = match self {
            Self::Session { name, .. } => name.clone(),
            _ => var("HERDR_SESSION")
                .and_then(|name| name.into_string().ok())
                .unwrap_or_else(|| "default".into()),
        };
        session_socket(&base, &name)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    #[test]
    fn local_origin_ignores_overrides_but_preserves_session_and_os_paths() {
        use std::os::unix::ffi::OsStringExt;
        let root = std::ffi::OsString::from_vec(b"/config-\xff".to_vec());
        let var = |name: &str| match name {
            "XDG_CONFIG_HOME" => Some(root.clone()),
            "HERDR_SESSION" => Some("work".into()),
            "HERDR_SOCKET_PATH" => Some("/forwarded/herdr.sock".into()),
            "HERDR_CLIENT_SOCKET_PATH" => Some("/forwarded-client.sock".into()),
            _ => None,
        };
        let expected = PathBuf::from(&root).join("herdr/sessions/work/herdr-client.sock");
        for target in [
            ConnectTarget::Local,
            ConnectTarget::Socket(expected.clone()),
            ConnectTarget::Socket("/forwarded.sock".into()),
        ] {
            assert_eq!(
                target.local_session_socket_path_with(var).unwrap(),
                expected
            );
        }
        assert_eq!(
            ConnectTarget::Session {
                name: "default".into(),
                development: true
            }
            .local_session_socket_path_with(var)
            .unwrap(),
            PathBuf::from(&root).join("herdr-dev/herdr-client.sock")
        );
        assert!(
            ConnectTarget::Ssh {
                target: "host".into(),
                session: "work".into()
            }
            .local_session_socket_path_with(var)
            .is_err()
        );
    }
    #[test]
    fn inherited_socket_precedence_and_environment_changes() {
        let resolve = |target: &ConnectTarget, api: Option<&str>, client: Option<&str>| {
            target
                .socket_path_with(|name| match name {
                    "HERDR_SOCKET_PATH" => api.map(Into::into),
                    "HERDR_CLIENT_SOCKET_PATH" => client.map(Into::into),
                    "XDG_CONFIG_HOME" => Some("/config".into()),
                    "HERDR_SESSION" => Some("work".into()),
                    // HERDR_SOCKET is not a discovery variable.
                    "HERDR_SOCKET" => Some("/ignored.sock".into()),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(
            resolve(
                &ConnectTarget::Local,
                Some("/local/herdr.sock"),
                Some("/ignored.sock")
            ),
            Path::new("/local/herdr-client.sock")
        );
        assert_eq!(
            resolve(&ConnectTarget::Local, None, Some("/forwarded.sock")),
            Path::new("/forwarded.sock")
        );
        assert_eq!(
            resolve(&ConnectTarget::Local, None, None),
            Path::new("/config/herdr/sessions/work/herdr-client.sock")
        );
        assert_eq!(
            resolve(
                &ConnectTarget::Socket("/explicit.sock".into()),
                Some("/ignored.sock"),
                None
            ),
            Path::new("/explicit.sock")
        );
        assert_eq!(
            resolve(
                &ConnectTarget::Session {
                    name: "default".into(),
                    development: false
                },
                Some("/ignored.sock"),
                None
            ),
            Path::new("/config/herdr/herdr-client.sock")
        );
    }
    #[test]
    fn sessions_are_contained_and_default_is_not_nested() {
        let root = Path::new("/config/herdr");
        assert_eq!(
            session_socket(root, "default").unwrap(),
            root.join("herdr-client.sock")
        );
        assert_eq!(
            session_socket(root, "work.1").unwrap(),
            root.join("sessions/work.1/herdr-client.sock")
        );
        for name in ["", ".", "..", "../other", "a/b", "a b"] {
            assert!(session_socket(root, name).is_err());
        }
        assert!(session_socket(root, &"a".repeat(65)).is_err());
        let target = ConnectTarget::Socket("/tmp/explicit.sock".into());
        assert_eq!(
            target.socket_path().unwrap(),
            PathBuf::from("/tmp/explicit.sock")
        );
    }
}
