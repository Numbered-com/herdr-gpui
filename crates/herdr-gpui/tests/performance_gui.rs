//! Explicit native opt-in: unlike GPUI unit tests, this launches a real AppKit app.
#![cfg(target_os = "macos")]
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
#[ignore = "opens native fixture window; run explicitly on a macOS desktop"]
fn dense_terminal_sidebar_performance() -> Result<(), Box<dyn std::error::Error>> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_herdr-gpui"))
        .arg("--performance-test")
        .env_remove("HERDR_PERF_UNCACHED")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            assert!(
                status.success(),
                "native performance fixture failed: {status}"
            );
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(180) {
            child.kill()?;
            child.wait()?;
            return Err("native performance fixture timed out".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
