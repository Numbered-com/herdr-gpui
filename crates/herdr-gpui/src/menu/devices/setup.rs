//! Delegate provisioning to the installed Herdr CLI. Its approval and SSH
//! prompts require a real terminal; no remote installer policy is duplicated.
use crate::{Error, Result};

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

// The command is sent to a terminal shell.
// Single-quote each argument so labels/SSH aliases never become shell syntax.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn shell_command(executable: &str, request: &Request, environment: &[(String, String)]) -> String {
    let mut args = vec!["env".to_owned()];
    // A running macOS Terminal does not inherit the GUI's environment. Clear
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

pub(super) fn launch(request: Request) -> Result<()> {
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
    let command = shell_command(executable, &request, &environment);
    launch_terminal(&command)
}

#[cfg(target_os = "macos")]
fn launch_terminal(command: &str) -> Result<()> {
    use std::{
        fs::OpenOptions,
        io::Write,
        os::unix::fs::OpenOptionsExt,
        process::{Command, Stdio},
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };
    struct Script(std::path::PathBuf);
    impl Drop for Script {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut script = None;
    for _ in 0..64 {
        let path = std::env::temp_dir().join(format!(
            "herdr-gpui-device-{}-{}.command",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&path)
        {
            Ok(mut file) => {
                let owned = Script(path);
                // Opening a .command through Launch Services needs no Apple Events
                // entitlement or Automation permission. Unlink once the shell owns it.
                writeln!(
                    file,
                    "#!/bin/sh\n/bin/rm -- \"$0\"\n{command}\nstatus=$?\nprintf '\\nHerdr setup exited with status %s.\\n' \"$status\"\nexit \"$status\""
                )?;
                script = Some(owned);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let script = script.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "Could not create a unique device setup script",
        )
    })?;
    let mut child = Command::new("/usr/bin/open")
        .args(["-b", "com.apple.Terminal", "--"])
        .arg(&script.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(Error::DeviceSetupTerminal(status));
                }
                // A successful `open` only queues Launch Services. Keep the script
                // until Terminal actually starts it, with bounded cleanup on failure.
                while script.0.try_exists()? {
                    if Instant::now() >= deadline {
                        return Err(Error::DeviceSetupTimeout);
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                return Ok(());
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return match result {
                    Err(error) => Err(error.into()),
                    _ => Err(Error::DeviceSetupTimeout),
                };
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn launch_terminal(command: &str) -> Result<()> {
    use std::process::{Command, Stdio};
    // Keep the result visible even for emulators that close when the command
    // exits. This shell only executes our quoted argv, never terminal output.
    let script =
        format!("{command}; printf '\\nSetup finished. Press Enter to close.'; read -r reply");
    for (terminal, separator) in [
        ("x-terminal-emulator", "-e"),
        ("gnome-terminal", "--"),
        ("konsole", "-e"),
        ("kitty", "--"),
        ("alacritty", "-e"),
        ("xterm", "-e"),
    ] {
        match Command::new(terminal)
            .args([separator, "/bin/sh", "-c", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                // The external terminal owns the interactive setup, and can
                // outlive this window. Reap only the launcher we created.
                std::thread::Builder::new()
                    .name("herdr-device-terminal".into())
                    .spawn(move || {
                        let _ = child.wait();
                    })?;
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(Error::DeviceSetupInput(
        "No supported terminal found. Install x-terminal-emulator, GNOME Terminal, Konsole, Kitty, Alacritty, or xterm.",
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn launch_terminal(_: &str) -> Result<()> {
    Err(Error::DeviceSetupInput(
        "Remote device setup requires macOS or Linux.",
    ))
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
        let output = std::process::Command::new("/bin/sh")
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
