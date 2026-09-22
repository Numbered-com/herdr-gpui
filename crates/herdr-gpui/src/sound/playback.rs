use crate::{Error, Result};
use herdr_client::protocol::SemanticNotificationSound as Sound;
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(super) fn muted() -> bool {
    std::env::var_os("HERDR_DISABLE_SOUND").is_some() || std::env::var_os("NEXTEST").is_some()
}

pub(super) fn play(
    sound: Sound,
    custom: Option<&Path>,
    cancel: &AtomicBool,
    stop: &AtomicBool,
) -> Result<()> {
    if muted() {
        return Ok(());
    }
    if let Some(path) = custom {
        match play_file(path, cancel, stop) {
            Ok(()) => return Ok(()),
            Err(error @ (Error::SoundCancelled | Error::SoundTimeout)) => return Err(error),
            Err(error) => tracing::debug!(%error, "Custom sound failed; using built-in sound"),
        }
    }
    let mut file = tempfile::Builder::new()
        .prefix("herdr-gpui-sound-")
        .suffix(".mp3")
        .tempfile()?;
    file.write_all(match sound {
        Sound::Done => include_bytes!("../../../../assets/sounds/done.mp3"),
        Sound::Request => include_bytes!("../../../../assets/sounds/request.mp3"),
    })?;
    play_file(file.path(), cancel, stop)
}

fn play_file(path: &Path, cancel: &AtomicBool, stop: &AtomicBool) -> Result<()> {
    // Absolute arguments cannot be interpreted as player options. Only local config
    // supplies paths; daemon titles, bodies and terminal escapes never reach here.
    let path = std::fs::canonicalize(path)?;
    #[cfg(target_os = "macos")]
    let players: &[(&str, &[&str])] = &[("/usr/bin/afplay", &[])];
    #[cfg(not(target_os = "macos"))]
    let players: &[(&str, &[&str])] = &[
        ("paplay", &[]),
        ("pw-play", &[]),
        ("ffplay", &["-nodisp", "-autoexit", "-loglevel", "quiet"]),
        ("mpg123", &["-q"]),
        ("mpv", &["--no-video", "--really-quiet"]),
    ];
    let mut last = Error::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no audio player available",
    ));
    let deadline = Instant::now() + Duration::from_secs(15);
    for (program, args) in players {
        let mut command = Command::new(program);
        command.args(*args).arg(&path);
        match run(&mut command, deadline, cancel, stop) {
            Ok(()) => return Ok(()),
            Err(error @ (Error::SoundCancelled | Error::SoundTimeout)) => return Err(error),
            Err(error) => last = error,
        }
    }
    Err(last)
}

fn run(
    command: &mut Command,
    deadline: Instant,
    cancel: &AtomicBool,
    stop: &AtomicBool,
) -> Result<()> {
    if cancel.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
        return Err(Error::SoundCancelled);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(Error::SoundExit(status))
                };
            }
            Ok(None) => {}
            Err(error) => break Err(Error::Io(error)),
        }
        if cancel.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
            break Err(Error::SoundCancelled);
        }
        if Instant::now() >= deadline {
            break Err(Error::SoundTimeout);
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    // Worker-only cleanup: no UI joins, inherited pipes, or unreaped children.
    let _ = child.kill();
    child.wait()?;
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn player_results_are_typed_without_playing_audio() {
        let cancel = AtomicBool::new(false);
        let stop = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(
            run(
                Command::new("/bin/sh").args(["-c", "exit 0"]),
                deadline,
                &cancel,
                &stop
            )
            .is_ok()
        );
        assert!(
            matches!(run(Command::new("/bin/sh").args(["-c", "exit 7"]), deadline, &cancel, &stop), Err(Error::SoundExit(status)) if status.code() == Some(7))
        );
        let error = run(
            &mut Command::new("/nonexistent-herdr-audio-player"),
            deadline,
            &cancel,
            &stop,
        )
        .unwrap_err();
        assert!(matches!(error, Error::Io(_)));
        assert!(std::error::Error::source(&error).is_some());
        cancel.store(true, Ordering::Release);
        assert!(matches!(
            run(
                &mut Command::new("/must-not-spawn"),
                deadline,
                &cancel,
                &stop
            ),
            Err(Error::SoundCancelled)
        ));
    }

    #[test]
    fn timeout_kills_and_reaps_only_owned_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "printf '%s' \"$$\" > \"$1\"; exec sleep 10",
                "sound-test",
            ])
            .arg(&pid);
        let result = run(
            &mut command,
            Instant::now() + Duration::from_millis(200),
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        );
        assert!(matches!(result, Err(Error::SoundTimeout)));
        let pid = std::fs::read_to_string(pid).unwrap();
        assert!(
            !Command::new("/bin/kill")
                .args(["-0", &pid])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn cancellation_stops_running_player() {
        let cancel = AtomicBool::new(false);
        let stop = AtomicBool::new(false);
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("started");
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                run(
                    Command::new("/bin/sh")
                        .args(["-c", "printf started > \"$1\"; exec sleep 10", "sound-test"])
                        .arg(&marker),
                    Instant::now() + Duration::from_secs(5),
                    &cancel,
                    &stop,
                )
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            while !marker.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(marker.exists());
            stop.store(true, Ordering::Release);
            assert!(matches!(worker.join().unwrap(), Err(Error::SoundCancelled)));
        });
    }
}
