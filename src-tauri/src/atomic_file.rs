//! Replace a file so that a crash, a kill or an exit mid-write leaves either
//! the old content or the new one, never a truncated file.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT: AtomicU64 = AtomicU64::new(0);
const RENAME_DELAYS_MS: [u64; 5] = [20, 40, 80, 160, 200];

/// Each call writes its own temporary file, so writers of the same target
/// never share one. Which complete write lands last is the caller's to order.
pub fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let result = write_with(
        path,
        contents,
        |from, to| std::fs::rename(from, to),
        std::thread::sleep,
    );
    if let Err(error) = &result {
        tracing::warn!("Could not atomically write {}: {error}", path.display());
    }
    result
}

fn create_temporary(path: &Path) -> io::Result<(PathBuf, File)> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?
        .to_owned();
    loop {
        let mut temporary_name = name.clone();
        temporary_name.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary = path.with_file_name(temporary_name);
        match crate::platform::atomic_file::create_new_private(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            // A leftover from a previous process with the same pid is not ours.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

fn write_with(
    path: &Path,
    contents: &[u8],
    mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
    mut sleep: impl FnMut(Duration),
) -> io::Result<()> {
    let permissions = crate::platform::atomic_file::replacement_permissions(path)?;
    let (temporary, mut file) = create_temporary(path)?;
    let written = file.write_all(contents).and_then(|_| {
        crate::platform::atomic_file::apply_permissions(&temporary, &file, &permissions)
    });
    drop(file);
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    let mut result = rename(&temporary, path);
    // Give an antivirus/indexer a bounded half second to release the file.
    for delay_ms in RENAME_DELAYS_MS {
        if !result
            .as_ref()
            .is_err_and(|error| crate::platform::atomic_file::rename_retryable(error, path))
        {
            break;
        }
        sleep(Duration::from_millis(delay_ms));
        result = rename(&temporary, path);
    }
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("a2t-atomic-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn replaces_the_file_and_leaves_no_temporary() {
        let dir = directory("replace");
        let path = dir.join("fight.json");
        write(&path, b"old").unwrap();
        write(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_write_warns_once_without_logging_its_contents() {
        #[derive(Clone)]
        struct LogWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl Write for LogWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let writer = LogWriter(Default::default());
        let output = writer.0.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || writer.clone())
            .finish();
        let dir = directory("warning");
        let dispatch = tracing::Dispatch::new(subscriber);
        // Another test failing a write at the same moment, on a thread with no
        // subscriber, can cache this warning as unwanted between our asking
        // again and our write, so it goes unlogged now and then (CI,
        // 2026-10-26 and 10-28): try again then, a few times.
        let mut logged = String::new();
        for _ in 0..5 {
            output.lock().unwrap().clear();
            tracing::dispatcher::with_default(&dispatch, || {
                tracing::callsite::rebuild_interest_cache();
                assert!(write(&dir.join("missing/settings.json"), b"private setting value").is_err());
            });
            logged = String::from_utf8(output.lock().unwrap().clone()).unwrap();
            if !logged.is_empty() {
                break;
            }
        }
        assert_eq!(logged.matches("Could not atomically write").count(), 1);
        assert!(logged.contains("settings.json"));
        assert!(!logged.contains("private setting value"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_write_keeps_the_previous_file() {
        let dir = directory("failed");
        let path = dir.join("settings.json");
        write(&path, b"previous").unwrap();
        let mut waited = Duration::ZERO;
        assert!(write_with(
            &path,
            b"lost",
            |_, _| {
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "held by a reader",
                ))
            },
            |delay| waited += delay
        )
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"previous");
        assert_eq!(waited, Duration::from_millis(500));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn permanent_rename_errors_fail_without_sleeping_or_losing_the_old_file() {
        let dir = directory("permanent");
        let path = dir.join("settings.json");
        write(&path, b"previous").unwrap();
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidInput,
            io::ErrorKind::NotFound,
        ] {
            let (mut attempts, mut sleeps) = (0, 0);
            assert!(write_with(
                &path,
                b"lost",
                |_, _| {
                    attempts += 1;
                    Err(io::Error::new(kind, "permanent failure"))
                },
                |_| sleeps += 1
            )
            .is_err());
            assert_eq!(attempts, 1);
            assert_eq!(sleeps, 0);
            assert_eq!(std::fs::read(&path).unwrap(), b"previous");
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_directory_target_is_refused_before_creating_a_temporary() {
        let dir = directory("directory");
        let path = dir.join("settings.json");
        std::fs::create_dir(&path).unwrap();
        assert_eq!(
            write(&path, b"private").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_reader_released_after_two_hundred_ms_does_not_lose_the_write() {
        let dir = directory("retry");
        let path = dir.join("settings.json");
        write(&path, b"previous").unwrap();
        let elapsed = std::cell::Cell::new(Duration::ZERO);
        write_with(
            &path,
            b"new",
            |from, to| {
                if elapsed.get() < Duration::from_millis(200) {
                    Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "held by a reader",
                    ))
                } else {
                    std::fs::rename(from, to)
                }
            },
            |delay| elapsed.set(elapsed.get() + delay),
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_slow_fight_write_does_not_block_an_unrelated_setting() {
        let dir = directory("independent");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let path = dir.join("fight.json");
        let slow = std::thread::spawn(move || {
            write_with(
                &path,
                b"fight",
                |from, to| {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    std::fs::rename(from, to)
                },
                std::thread::sleep,
            )
        });
        started_rx.recv().unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let settings = dir.join("settings.json");
        let fast = std::thread::spawn(move || done_tx.send(write(&settings, b"setting")).unwrap());
        let result = done_rx.recv_timeout(Duration::from_secs(2));
        // Release the worker even on failure so the test cannot hang.
        release_tx.send(()).unwrap();
        slow.join().unwrap().unwrap();
        fast.join().unwrap();
        result
            .expect("settings waited for the fight write")
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_writers_of_one_file_leave_a_whole_copy() {
        let dir = directory("concurrent");
        let path = dir.join("fight.json");
        let contents: Vec<Vec<u8>> = (0..8u8).map(|i| vec![b'a' + i; 256 * 1024]).collect();
        std::thread::scope(|scope| {
            for body in &contents {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..4 {
                        write(path, body).unwrap();
                    }
                });
            }
        });
        let written = std::fs::read(&path).unwrap();
        assert!(contents.contains(&written));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
