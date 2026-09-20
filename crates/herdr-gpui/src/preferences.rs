use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Asynchronous, endpoint-local preferences. Dropping lets the worker drain queued
/// saves without waiting; process exit may interrupt pending writes.
pub struct Preferences {
    saves: Option<Sender<Option<f32>>>,
    loaded: Option<Receiver<Option<f32>>>,
    worker: Option<JoinHandle<()>>,
}

impl Preferences {
    pub fn new(socket: &Path) -> Self {
        let root = env::var_os("XDG_STATE_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join(".local/state"))
            });
        Self::start(
            root.map(|root| endpoint_path(&root, socket))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "neither XDG_STATE_HOME nor HOME is set",
                    )
                }),
        )
    }

    fn start(path: io::Result<PathBuf>) -> Self {
        let (saves, requests) = mpsc::channel();
        let (loaded_tx, loaded) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("gpui-preferences".into())
            .spawn(move || {
                let path = match path {
                    Ok(path) => path,
                    Err(error) => {
                        eprintln!("Cannot locate GPUI preferences: {error}");
                        let _ = loaded_tx.send(None);
                        return;
                    }
                };
                let width = match read_width(&path) {
                    Ok(width) => width,
                    Err(error) => {
                        eprintln!("Cannot read GPUI preferences {}: {error}", path.display());
                        None
                    }
                };
                let _ = loaded_tx.send(width);
                for width in requests {
                    if let Err(error) = write_width(&path, width) {
                        eprintln!("Cannot save GPUI preferences {}: {error}", path.display());
                    }
                }
            });
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                eprintln!("Cannot start GPUI preferences worker: {error}");
                None
            }
        };
        Self {
            saves: Some(saves),
            loaded: Some(loaded),
            worker,
        }
    }

    /// Takes the initial result once; outer `None` means pending or already taken.
    /// Inner `None` means the default width (including a failed initial load).
    pub fn loaded(&mut self) -> Option<Option<f32>> {
        let result = match self.loaded.as_ref()?.try_recv() {
            Ok(width) => Some(width),
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Some(None),
        };
        self.loaded = None;
        result
    }

    /// Queues a width, or `None` to persist a reset to the default width.
    pub fn save(&self, width: Option<f32>) {
        if let Some(saves) = &self.saves
            && saves.send(width).is_err()
        {
            eprintln!("Cannot queue GPUI preferences save: worker disconnected");
        }
    }
}

impl Drop for Preferences {
    fn drop(&mut self) {
        // Disconnect and detach: the worker drains queued saves while the process
        // remains alive, without making the UI wait for disk I/O.
        self.saves.take();
        drop(self.worker.take());
    }
}

fn endpoint_path(root: &Path, socket: &Path) -> PathBuf {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in socket.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    root.join("herdr/gpui")
        .join(format!("local-{hash:016x}.json"))
}

fn read_width(path: &Path) -> io::Result<Option<f32>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let object = value.as_object().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "preferences must be an object")
    })?;
    match object.get("sidebar_width_px") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => {
            let width = value.as_f64().map(|width| width as f32);
            match width {
                Some(width) if width.is_finite() && width > 0.0 => Ok(Some(width)),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "sidebar_width_px must be finite and positive, or null",
                )),
            }
        }
    }
}

fn write_width(path: &Path, width: Option<f32>) -> io::Result<()> {
    if width.is_some_and(|width| !width.is_finite() || width <= 0.0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid sidebar width",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "preferences path has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    let (temporary, mut file) = loop {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".preferences-{}-{sequence}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = (|| {
        serde_json::to_writer(&mut file, &serde_json::json!({ "sidebar_width_px": width }))?;
        file.write_all(b"\n")?;
        file.sync_all()
    })();
    drop(file);
    let result = result.and_then(|()| fs::rename(&temporary, path));
    if result.is_err()
        && let Err(error) = fs::remove_file(&temporary)
    {
        eprintln!(
            "Cannot clean up GPUI preferences {}: {error}",
            temporary.display()
        );
    }
    result
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::time::{Duration, Instant};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            loop {
                let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let path = env::temp_dir().join(format!(
                    "herdr-preferences-test-{}-{sequence}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("Cannot create test directory: {error}"),
                }
            }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn await_loaded(preferences: &mut Preferences) -> Option<f32> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(width) = preferences.loaded() {
                assert_eq!(preferences.loaded(), None);
                return width;
            }
            assert!(Instant::now() < deadline, "preferences load timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn roundtrip_and_reset_drain_after_drop() {
        let directory = TestDirectory::new();
        let path = endpoint_path(&directory.0, Path::new("/tmp/test.sock"));
        let mut preferences = Preferences::start(Ok(path.clone()));
        assert_eq!(await_loaded(&mut preferences), None);
        for width in 1..=100 {
            preferences.save(Some(width as f32));
        }
        let worker = preferences.worker.take().unwrap();
        drop(preferences);
        // Only the test waits for persistence before reading or removing files.
        worker.join().unwrap();
        let mut preferences = Preferences::start(Ok(path.clone()));
        assert_eq!(await_loaded(&mut preferences), Some(100.0));
        preferences.save(None);
        let worker = preferences.worker.take().unwrap();
        drop(preferences);
        worker.join().unwrap();
        assert_eq!(read_width(&path).unwrap(), None);
        let mut preferences = Preferences::start(Ok(path));
        assert_eq!(await_loaded(&mut preferences), None);
    }

    #[test]
    fn drop_does_not_wait_for_blocked_worker_and_queued_saves_still_drain() {
        let (saves, requests) = mpsc::channel();
        let (ready_tx, ready) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (drained_tx, drained) = mpsc::channel();
        let worker = thread::spawn(move || {
            ready_tx.send(()).unwrap();
            blocked.recv().unwrap();
            drained_tx
                .send(requests.into_iter().collect::<Vec<_>>())
                .unwrap();
        });
        let preferences = Preferences {
            saves: Some(saves),
            loaded: None,
            worker: Some(worker),
        };
        let queued = [Some(160.), Some(400.), None];
        for width in queued {
            preferences.save(width);
        }
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        let (dropped_tx, dropped) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(preferences);
            dropped_tx.send(()).unwrap();
        });
        let result = dropped.recv_timeout(Duration::from_secs(5));
        // Release even on failure so a regressed join does not strand the threads.
        release.send(()).unwrap();
        let saved = drained.recv_timeout(Duration::from_secs(5)).unwrap();
        dropper.join().unwrap();
        assert!(result.is_ok(), "drop waited for the blocked worker");
        assert_eq!(saved, queued);
    }

    #[test]
    fn malformed_and_invalid_widths_fall_back_to_default() {
        let directory = TestDirectory::new();
        let path = directory.0.join("preferences.json");
        for contents in [
            "not json",
            "[]",
            "null",
            r#"{"sidebar_width_px":0}"#,
            r#"{"sidebar_width_px":-1}"#,
            r#"{"sidebar_width_px":"200"}"#,
            r#"{"sidebar_width_px":true}"#,
            r#"{"sidebar_width_px":1e100}"#,
            r#"{"sidebar_width_px":1e-100}"#,
            r#"{"sidebar_width_px":NaN}"#,
        ] {
            fs::write(&path, contents).unwrap();
            assert!(read_width(&path).is_err(), "accepted {contents}");
            let mut preferences = Preferences::start(Ok(path.clone()));
            assert_eq!(await_loaded(&mut preferences), None);
        }
        for contents in ["{}", r#"{"sidebar_width_px":null}"#] {
            fs::write(&path, contents).unwrap();
            assert_eq!(read_width(&path).unwrap(), None);
        }
        for width in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -1.0] {
            assert!(write_width(&path, Some(width)).is_err());
        }
        write_width(&path, Some(237.5)).unwrap();
        assert_eq!(read_width(&path).unwrap(), Some(237.5));
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[test]
    fn endpoint_paths_use_stable_fnv1a() {
        let root = Path::new("/state");
        assert_eq!(
            endpoint_path(root, Path::new("hello")),
            Path::new("/state/herdr/gpui/local-a430d84680aabd0b.json")
        );
        assert_eq!(
            endpoint_path(root, Path::new("")),
            Path::new("/state/herdr/gpui/local-cbf29ce484222325.json")
        );
        assert_ne!(
            endpoint_path(root, Path::new("/a.sock")),
            endpoint_path(root, Path::new("/b.sock"))
        );
    }
}
