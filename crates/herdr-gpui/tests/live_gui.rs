//! Requires an active native desktop and an explicitly selected daemon executable.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::{
    fs::{self, DirBuilder, File},
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Isolated {
    dir: PathBuf,
    daemon: Option<Child>,
    gui: Option<Child>,
}

impl Isolated {
    fn command(&self, binary: impl AsRef<std::ffi::OsStr>, log: &str) -> Command {
        let log = File::create(self.dir.join(log)).unwrap();
        let mut command = Command::new(binary);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &self.dir)
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("XDG_DATA_HOME", self.dir.join("data"))
            .env("XDG_CACHE_HOME", self.dir.join("cache"))
            .env("XDG_RUNTIME_DIR", &self.dir)
            .env("TMPDIR", &self.dir)
            .env("HERDR_CONFIG_PATH", self.dir.join("config.toml"))
            .env("HERDR_SOCKET_PATH", self.dir.join("a.sock"))
            .env("HERDR_CLIENT_SOCKET_PATH", self.dir.join("a-client.sock"))
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("PS1", "LIVE> ")
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        // Preserve only desktop transport, not user config or daemon discovery variables.
        for name in ["DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
    }
}

impl Drop for Isolated {
    fn drop(&mut self) {
        // Only the exact children created by this test. Never discover or stop a user daemon.
        for child in [&mut self.gui, &mut self.daemon].into_iter().flatten() {
            let _ = child.kill();
            let _ = child.wait();
        }
        for name in ["gui.log", "daemon.log"] {
            if let Ok(log) = fs::read_to_string(self.dir.join(name)) {
                eprintln!("{name} (last 40 lines, at most 600 chars each):");
                let lines = log.lines().rev().take(40).collect::<Vec<_>>();
                for line in lines.into_iter().rev() {
                    eprintln!("{}", line.chars().take(600).collect::<String>());
                }
            }
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
#[ignore = "requires active desktop and explicit HERDR_TEST_BINARY; launches a native GUI and isolated daemon"]
fn native_gui_live() {
    let binary = PathBuf::from(
        std::env::var_os("HERDR_TEST_BINARY")
            .expect("set HERDR_TEST_BINARY to an explicit absolute herdr executable"),
    );
    assert!(
        binary.is_absolute() && binary.is_file(),
        "explicit daemon binary must exist"
    );
    let parent = std::env::var_os("HERDR_TEST_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    assert!(parent.is_dir(), "temporary parent must already exist");
    let dir = parent.join(format!(
        "g{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    DirBuilder::new().mode(0o700).create(&dir).unwrap();
    let mut isolated = Isolated {
        dir,
        daemon: None,
        gui: None,
    };
    let socket = isolated.dir.join("a-client.sock");
    assert!(
        socket.as_os_str().len() < 104,
        "temporary path too long for Unix socket"
    );
    fs::write(
        isolated.dir.join("config.toml"),
        "onboarding = false\n[terminal]\ndefault_shell = \"/bin/sh\"\nshell_mode = \"non_login\"\n",
    )
    .unwrap();
    isolated.daemon = Some(
        isolated
            .command(binary, "daemon.log")
            .arg("server")
            .spawn()
            .unwrap(),
    );
    eprintln!(
        "isolated daemon pid={} dir={}",
        isolated.daemon.as_ref().unwrap().id(),
        isolated.dir.display()
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while !socket.exists() {
        assert!(
            isolated
                .daemon
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none(),
            "daemon exited during startup"
        );
        assert!(Instant::now() < deadline, "daemon startup timeout");
        thread::sleep(Duration::from_millis(20));
    }
    isolated.gui = Some(
        isolated
            .command(env!("CARGO_BIN_EXE_herdr-gpui"), "gui.log")
            .arg("--socket")
            .arg(&socket)
            .arg("--integration-test")
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = isolated.gui.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "native GUI timeout (active desktop required)"
        );
        assert!(
            isolated
                .daemon
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none(),
            "daemon died while GUI was running"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "GUI failed: {status}");
    let log = fs::read_to_string(isolated.dir.join("gui.log")).unwrap();
    assert!(
        log.contains("GUI integration PASS:"),
        "GUI exited without completing harness"
    );
    assert!(
        log.contains("GUI input pipeline verified:"),
        "GUI did not verify native action, key, and text delivery"
    );
    assert!(
        isolated
            .daemon
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none(),
        "GUI exit killed daemon"
    );
    eprintln!(
        "GUI exited successfully; isolated daemon is still alive; cleaning up only owned children"
    );
}
