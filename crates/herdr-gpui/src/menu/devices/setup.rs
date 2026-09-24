//! Delegate provisioning to the installed Herdr CLI. A host that already runs
//! a compatible Herdr is saved without a terminal; anything that needs SSH or
//! installation prompts runs in a local workspace, so no remote installer
//! policy is duplicated here.
use crate::{Error, Result};
use herdr_client::HostProbe;
use std::{
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// `machine add` connects, starts the remote server, and verifies it.
const SAVE_TIMEOUT: Duration = Duration::from_secs(120);
/// Enough for the CLI's final error line without retaining remote output.
const SAVE_STDERR_LIMIT: u64 = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Request {
    target: String,
    label: String,
    session: String,
}

impl Request {
    pub(super) fn new(target: &str, label: &str, session: &str) -> Result<Self> {
        let target = target.trim();
        let label = label.trim();
        let session = session.trim();
        if target.is_empty()
            || target.len() > 1024
            || target.starts_with('-')
            || target
                .strip_prefix("ssh://")
                .unwrap_or(target)
                .rsplit_once('@')
                .is_some_and(|(user, _)| user.contains(':'))
            || target.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(Error::DeviceSetupInput(
                "Enter an SSH target such as user@hostname or an SSH alias.",
            ));
        }
        if label.is_empty() || label.len() > 128 || label.chars().any(char::is_control) {
            return Err(Error::DeviceSetupInput(
                "Enter a device label (at most 128 bytes).",
            ));
        }
        let session = if session.is_empty() {
            "default"
        } else {
            session
        };
        if session.len() > 64
            || matches!(session, "." | "..")
            || !session
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        {
            return Err(Error::DeviceSetupInput(
                "Session names use letters, digits, dots, underscores, or hyphens (at most 64 bytes), excluding '.' and '..'.",
            ));
        }
        Ok(Self {
            target: target.into(),
            label: label.into(),
            session: session.into(),
        })
    }

    pub(super) fn target(&self) -> &str {
        &self.target
    }

    pub(super) fn label(&self) -> &str {
        &self.label
    }

    /// Blocks on SSH: call it from the background executor.
    pub(super) fn probe(&self) -> Result<HostProbe> {
        Ok(herdr_client::probe_host(&self.target, &self.session)?)
    }

    fn arguments(&self) -> [&str; 7] {
        [
            "machine",
            "add",
            &self.target,
            "--label",
            &self.label,
            "--remote-session",
            &self.session,
        ]
    }
}

// The command is typed into a terminal shell.
// Single-quote each argument so labels/SSH aliases never become shell syntax.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn shell_command(executable: &str, request: &Request, environment: &[(String, String)]) -> String {
    let mut args = vec!["env".to_owned()];
    // The daemon's shell does not inherit the GUI's environment. Clear
    // optional overrides before restoring this window's exact catalog roots.
    for name in ["HERDR_CONFIG_PATH", "XDG_STATE_HOME", "XDG_CONFIG_HOME"] {
        args.extend(["-u".into(), name.into()]);
    }
    args.extend(
        environment
            .iter()
            .map(|(key, value)| format!("{key}={value}")),
    );
    args.push(executable.into());
    args.extend(request.arguments().into_iter().map(str::to_owned));
    args.iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs `machine add` without a terminal, for a host whose probe found a
/// compatible Herdr: the CLI starts a stopped server itself. Blocks on SSH, so
/// call it from the background executor. Stdin is closed, so any approval the
/// CLI would need fails instead of waiting for input nobody can see.
pub(super) fn save(request: &Request) -> Result<()> {
    save_with(crate::daemon::executable(), request)
}

fn save_with(executable: impl AsRef<std::ffi::OsStr>, request: &Request) -> Result<()> {
    let mut child = Command::new(executable)
        .args(request.arguments())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stderr = child.stderr.take().ok_or(Error::DeviceSetupTimeout)?;
    let reader = std::thread::Builder::new()
        .name("herdr-device-save".into())
        .spawn(move || {
            let mut output = Vec::new();
            // Keep draining past the limit so a chatty CLI never blocks on a full pipe.
            let kept = (&mut stderr)
                .take(SAVE_STDERR_LIMIT)
                .read_to_end(&mut output);
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            kept.map(|_| output)
        })?;
    let deadline = Instant::now() + SAVE_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::DeviceSetupTimeout);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let output = reader
        .join()
        .map_err(|_| Error::DeviceSetupTimeout)?
        .unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    Err(Error::DeviceSetup {
        status,
        detail: last_line(&output),
    })
}

/// The CLI ends with its most specific error; earlier lines are progress.
fn last_line(output: &[u8]) -> String {
    String::from_utf8_lossy(output)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect()
}

/// The shell command a local workspace runs to set the host up interactively.
pub(super) fn terminal_command(request: &Request) -> Result<String> {
    if cfg!(windows) {
        return Err(Error::DeviceSetupInput(
            "Saved SSH devices are unavailable on Windows.",
        ));
    }
    let executable = crate::daemon::executable();
    let executable = executable.to_str().ok_or(Error::DeviceSetupInput(
        "The Herdr executable path must be UTF-8.",
    ))?;
    let mut environment = Vec::new();
    for key in [
        "HOME",
        "PATH",
        "HERDR_CONFIG_PATH",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
    ] {
        if let Some(value) = std::env::var_os(key) {
            environment.push((
                key.into(),
                value
                    .into_string()
                    .map_err(|_| Error::DeviceSetupInput("The setup environment must be UTF-8."))?,
            ));
        }
    }
    Ok(shell_command(executable, request, &environment))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_setup_fields_before_launch() -> Result<()> {
        for target in ["", "-oProxyCommand=bad", "two hosts", "host\ncommand"] {
            assert!(matches!(
                Request::new(target, "Label", ""),
                Err(Error::DeviceSetupInput(_))
            ));
        }
        assert!(Request::new("host", "", "").is_err());
        assert!(Request::new("host", "Label", "../session").is_err());
        assert!(Request::new("host", "Label", ".").is_err());
        assert!(Request::new("host", "Label", "dev.session").is_ok());
        assert!(Request::new("ssh://user:password@host", "Label", "").is_err());
        let request = Request::new(" user@host ", " My Device ", "")?;
        assert_eq!(
            request.arguments(),
            [
                "machine",
                "add",
                "user@host",
                "--label",
                "My Device",
                "--remote-session",
                "default"
            ]
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn save_reports_the_cli_failure_without_waiting_for_input() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("herdr-device-save-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let binary = root.join("herdr");
        // Stdin is closed, so a prompt reads EOF instead of blocking.
        std::fs::write(
            &binary,
            "#!/bin/sh\n[ \"$1 $2 $3\" = 'machine add ok' ] && exit 0\nread -r answer\necho 'error: approval required' >&2\nexit 3\n",
        )?;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
        save_with(&binary, &Request::new("ok", "Label", "")?)?;
        let error = save_with(&binary, &Request::new("host", "Label", "")?).err();
        std::fs::remove_dir_all(&root)?;
        match error {
            Some(Error::DeviceSetup { status, detail }) => {
                assert_eq!(status.code(), Some(3));
                assert_eq!(detail, "error: approval required");
            }
            other => panic!("unexpected result: {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn save_failures_keep_only_the_final_diagnostic_line() {
        assert_eq!(
            last_line(b"connecting\nerror: remote server is not ready\n\n"),
            "error: remote server is not ready"
        );
        assert_eq!(last_line(b"\x1b[31mbad\x1b[0m"), "[31mbad[0m");
        assert_eq!(last_line(&[b'x'; 1000]).len(), 300);
        assert_eq!(last_line(b""), "");
    }

    #[test]
    fn shell_arguments_and_catalog_roots_are_quoted() -> Result<()> {
        let request = Request::new("host", "Alice's $(printf INJECTED); device", "work")?;
        let command = shell_command(
            "/a path/herdr",
            &request,
            &[("XDG_STATE_HOME".into(), "/state user's".into())],
        );
        assert!(command.contains("'XDG_STATE_HOME=/state user'\\''s'"));
        assert!(command.contains("'/a path/herdr' 'machine' 'add' 'host'"));
        assert!(command.contains("'Alice'\\''s $(printf INJECTED); device'"));
        assert!(command.contains("'-u' 'HERDR_CONFIG_PATH'"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn terminal_shell_preserves_exact_arguments_without_expansion() -> Result<()> {
        let request = Request::new("user@host", "Alice's $(printf INJECTED); device", "work")?;
        let command = shell_command(
            "/a path/herdr",
            &request,
            &[("XDG_STATE_HOME".into(), "/state user's".into())],
        );
        let output = Command::new("/bin/sh")
            .args(["-c", &format!("set -- {command}; printf '%s\\0' \"$@\"")])
            .output()?;
        assert!(output.status.success());
        let args: Vec<_> = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|value| !value.is_empty())
            .collect();
        assert_eq!(
            &args[args.len() - 7..],
            request.arguments().map(str::as_bytes).as_slice()
        );
        assert!(args.contains(&b"XDG_STATE_HOME=/state user's".as_slice()));
        assert!(args.contains(&b"/a path/herdr".as_slice()));
        Ok(())
    }
}
