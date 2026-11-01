//! Real Windows sharing violations, rather than a mocked rename failure.

use std::fs::OpenOptions;
use std::io::Read;
use std::os::windows::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::Duration;

fn directory(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("a2t-win-atomic-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_reader_released_after_two_hundred_ms_allows_replacement() {
    let dir = directory("released");
    let path = dir.join("settings.json");
    crate::atomic_file::write(&path, b"old").unwrap();
    // FILE_SHARE_READ | FILE_SHARE_WRITE, deliberately no FILE_SHARE_DELETE.
    let reader = OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(&path)
        .unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        drop(reader);
    });
    let result = crate::atomic_file::write(&path, b"new");
    release.join().unwrap();
    result.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_reader_held_past_the_retry_limit_keeps_the_old_file() {
    let dir = directory("held");
    let path = dir.join("fight.json");
    crate::atomic_file::write(&path, b"old").unwrap();
    let reader = OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(&path)
        .unwrap();
    assert!(crate::atomic_file::write(&path, b"new").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    drop(reader);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_reader_sharing_delete_keeps_its_old_handle_and_allows_replacement() {
    let dir = directory("share-delete");
    let path = dir.join("fight.json");
    crate::atomic_file::write(&path, b"old").unwrap();
    let mut reader = OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2 | 0x4)
        .open(&path)
        .unwrap();
    crate::atomic_file::write(&path, b"new").unwrap();
    let mut previous = Vec::new();
    reader.read_to_end(&mut previous).unwrap();
    assert_eq!(previous, b"old");
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    drop(reader);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_read_only_target_fails_immediately_and_keeps_its_previous_contents() {
    let dir = directory("read-only");
    let path = dir.join("settings.json");
    crate::atomic_file::write(&path, b"old").unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions.clone()).unwrap();
    let start = std::time::Instant::now();
    let result = crate::atomic_file::write(&path, b"new");
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).unwrap();
    assert!(result.is_err());
    assert!(
        start.elapsed() < Duration::from_millis(400),
        "read-only errors were retried"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    let _ = std::fs::remove_dir_all(dir);
}
