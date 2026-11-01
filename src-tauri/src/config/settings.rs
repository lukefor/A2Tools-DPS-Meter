use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use tracing::info;

/// Application settings stored as key-value pairs.
/// Persists to settings.json in the app data directory.
/// Also attempts to migrate from the Kotlin app's settings.properties on first run.
pub struct Settings {
    inner: Arc<SettingsInner>,
}

struct StoredValues {
    values: HashMap<String, String>,
    generation: u64,
}

#[cfg(test)]
type WriteHook = dyn Fn(&std::path::Path, &[u8]) -> io::Result<()> + Send + Sync;

struct SettingsInner {
    values: RwLock<StoredValues>,
    file_path: PathBuf,
    /// Held from the snapshot to the rename, so an older snapshot never
    /// lands after a newer one.
    saving: Mutex<()>,
    saved_generation: AtomicU64,
    writer_running: AtomicBool,
    #[cfg(test)]
    write_hook: RwLock<Option<Arc<WriteHook>>>,
}

// Release admission even if a blocking task panics; a later setter or flush
// can still save the dirty generation.
struct RunningWriter(Arc<SettingsInner>);

impl Drop for RunningWriter {
    fn drop(&mut self) {
        self.0.writer_running.store(false, Ordering::Release);
    }
}

impl SettingsInner {
    fn needs_save(&self) -> bool {
        self.values.read().generation != self.saved_generation.load(Ordering::Acquire)
    }

    fn start_writer(self: &Arc<Self>) {
        if !self.needs_save()
            || self
                .writer_running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        let inner = self.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let running = RunningWriter(inner.clone());
            let (attempted, result) = inner.save_latest();
            if let Err(error) = result {
                tracing::warn!("Could not persist settings: {error}");
            }
            drop(running);
            // A setter racing the completed write may have seen this writer
            // still admitted. Pick up its newer value, including after failure.
            // Do not spin on an unchanged failed generation: flush/next set
            // retries that one.
            if inner.values.read().generation != attempted {
                inner.start_writer();
            }
        });
    }

    fn save_latest(&self) -> (u64, io::Result<()>) {
        let _saving = self.saving.lock();
        let values = self.values.read();
        let generation = values.generation;
        if generation == self.saved_generation.load(Ordering::Acquire) {
            return (generation, Ok(()));
        }
        let json = serde_json::to_vec_pretty(&values.values).map_err(io::Error::other);
        drop(values);
        let result = json.and_then(|json| {
            if let Some(parent) = self.file_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            #[cfg(test)]
            if let Some(write) = self.write_hook.read().clone() {
                return write(&self.file_path, &json);
            }
            crate::atomic_file::write(&self.file_path, &json)
        });
        if result.is_ok() {
            self.saved_generation.store(generation, Ordering::Release);
        }
        (generation, result)
    }
}

impl Settings {
    pub fn new(app_data_dir: PathBuf) -> Self {
        let file_path = app_data_dir.join("settings.json");
        let mut values: HashMap<String, String> = if file_path.exists() {
            match std::fs::read_to_string(&file_path) {
                Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
                Err(_) => HashMap::new(),
            }
        } else {
            HashMap::new()
        };

        // Migrate from Kotlin app's settings.properties if our settings are empty
        if values.is_empty() {
            if let Some(migrated) = Self::try_migrate_from_kotlin() {
                values = migrated;
                info!("Migrated {} settings from Kotlin app", values.len());
            }
        }

        let missing = !file_path.exists();
        let s = Self {
            inner: Arc::new(SettingsInner {
                values: RwLock::new(StoredValues {
                    values,
                    generation: u64::from(missing),
                }),
                file_path,
                saving: Mutex::new(()),
                saved_generation: AtomicU64::new(0),
                writer_running: AtomicBool::new(false),
                #[cfg(test)]
                write_hook: RwLock::new(None),
            }),
        };
        if missing {
            // Startup precedes the event loop. Create the initial private file
            // synchronously so readers of the on-disk language can find it.
            let _ = s.flush();
        }
        s
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.inner.values.read().values.get(key).cloned()
    }

    /// Store a value. Returns true when it actually changed, which is what gates
    /// the cross-window `setting-changed` broadcast — re-emitting on a no-op
    /// write would bounce the event back and forth between the windows.
    pub fn set(&self, key: &str, value: &str) -> bool {
        let changed = {
            let mut values = self.inner.values.write();
            if values
                .values
                .get(key)
                .is_some_and(|existing| existing == value)
            {
                false
            } else {
                values.values.insert(key.to_string(), value.to_string());
                values.generation = values.generation.wrapping_add(1);
                true
            }
        };
        // Memory and the event-echo boolean are immediate. Disk retries never
        // occupy the caller's UI/cooperative async thread.
        self.inner.start_writer();
        changed
    }

    pub fn remove(&self, key: &str) {
        {
            let mut values = self.inner.values.write();
            if values.values.remove(key).is_some() {
                values.generation = values.generation.wrapping_add(1);
            }
        }
        self.inner.start_writer();
    }

    pub fn clear(&self) {
        {
            let mut values = self.inner.values.write();
            if !values.values.is_empty() {
                values.values.clear();
                values.generation = values.generation.wrapping_add(1);
            }
        }
        self.inner.start_writer();
    }

    pub fn get_all(&self) -> HashMap<String, String> {
        self.inner.values.read().values.clone()
    }

    /// Persist the latest values before exiting, even when a queued writer has
    /// not started or the previous write failed. No timeout discards a value.
    pub fn flush(&self) -> io::Result<()> {
        loop {
            let (attempted, result) = self.inner.save_latest();
            result?;
            if self.inner.values.read().generation == attempted {
                return Ok(());
            }
        }
    }

    /// Try to migrate settings from the Kotlin app's settings.properties file.
    fn try_migrate_from_kotlin() -> Option<HashMap<String, String>> {
        let appdata = std::env::var("APPDATA").ok()?;
        let kotlin_file = PathBuf::from(&appdata)
            .join("AionDPS")
            .join("settings.properties");
        if !kotlin_file.exists() {
            return None;
        }
        let text = std::fs::read_to_string(&kotlin_file).ok()?;
        let mut map = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                map.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
        if map.is_empty() {
            None
        } else {
            Some(map)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fixture(name: &str) -> (std::path::PathBuf, Arc<Settings>) {
        let dir = std::env::temp_dir().join(format!("a2t-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A nonempty fixture avoids consulting any legacy user settings.
        std::fs::write(dir.join("settings.json"), br#"{"seed":"fixture"}"#).unwrap();
        let settings = Arc::new(Settings::new(dir.clone()));
        (dir, settings)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_blocked_writer_keeps_setters_and_timers_responsive_and_flush_keeps_the_latest() {
        let (dir, settings) = fixture("responsive");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started_tx));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = Mutex::new(release_rx);
        let writes = Arc::new(AtomicU64::new(0));
        let calls = writes.clone();
        *settings.inner.write_hook.write() = Some(Arc::new(move |path, bytes| {
            if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                started.lock().take().unwrap().send(()).unwrap();
                // Even a broken setter test eventually releases its worker.
                let _ = release.lock().recv_timeout(Duration::from_secs(3));
            }
            crate::atomic_file::write(path, bytes)
        }));
        assert!(settings.set("value", "first"));
        started_rx.await.unwrap();
        let start = std::time::Instant::now();
        for i in 0..100 {
            assert!(settings.set("value", &i.to_string()));
        }
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "setters waited for the blocked disk writer"
        );
        assert_eq!(settings.get("value").as_deref(), Some("99"));
        assert!(
            !settings.set("value", "99"),
            "an event echo is still a no-op"
        );
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::time::sleep(Duration::from_millis(10)),
        )
        .await
        .unwrap();
        let final_settings = settings.clone();
        let flush = tokio::task::spawn_blocking(move || final_settings.flush());
        tokio::task::yield_now().await;
        assert!(!flush.is_finished());
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), flush)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let saved: HashMap<String, String> =
            serde_json::from_slice(&std::fs::read(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(saved.get("value").map(String::as_str), Some("99"));
        assert_eq!(
            writes.load(Ordering::Relaxed),
            2,
            "the queued changes were coalesced"
        );
        assert!(!settings.inner.needs_save());
        settings.flush().unwrap();
        assert_eq!(
            writes.load(Ordering::Relaxed),
            2,
            "a late flush cannot write an older snapshot"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_background_write_stays_dirty_and_final_flush_retries_it() {
        let (dir, settings) = fixture("retry");
        let (failed_tx, failed_rx) = tokio::sync::oneshot::channel();
        let failed = Mutex::new(Some(failed_tx));
        let writes = Arc::new(AtomicU64::new(0));
        let calls = writes.clone();
        *settings.inner.write_hook.write() = Some(Arc::new(move |path, bytes| {
            if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                failed.lock().take().unwrap().send(()).unwrap();
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "fixture failure",
                ));
            }
            crate::atomic_file::write(path, bytes)
        }));
        assert!(settings.set("value", "latest"));
        failed_rx.await.unwrap();
        assert!(settings.inner.needs_save());
        settings.flush().unwrap();
        assert!(!settings.inner.needs_save());
        let saved: HashMap<String, String> =
            serde_json::from_slice(&std::fs::read(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(saved.get("value").map(String::as_str), Some("latest"));
        assert_eq!(writes.load(Ordering::Relaxed), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_language_observes_a_change_before_its_disk_write() {
        let (dir, settings) = fixture("upload-language");
        settings.set("dpsMeter.language", "en");
        settings.flush().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started_tx));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = Mutex::new(release_rx);
        let writes = AtomicU64::new(0);
        *settings.inner.write_hook.write() = Some(Arc::new(move |path, bytes| {
            if writes.fetch_add(1, Ordering::Relaxed) == 0 {
                started.lock().take().unwrap().send(()).unwrap();
                let _ = release.lock().recv_timeout(Duration::from_secs(3));
            }
            crate::atomic_file::write(path, bytes)
        }));
        settings.set("dpsMeter.language", "ru");
        started_rx.await.unwrap();
        let saved: HashMap<String, String> =
            serde_json::from_slice(&std::fs::read(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(
            saved.get("dpsMeter.language").map(String::as_str),
            Some("en")
        );
        // Exercise the source used by both manual and automatic upload bodies,
        // without accessing the keyring or sending a log to the service.
        assert_eq!(crate::share::ui_language(&settings), "ru");
        release_tx.send(()).unwrap();
        settings.flush().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_no_op_setter_retries_a_failed_generation_without_echoing_a_change() {
        let (dir, settings) = fixture("no-op-retry");
        let (failed_tx, failed_rx) = tokio::sync::oneshot::channel();
        let failed = Mutex::new(Some(failed_tx));
        let (saved_tx, saved_rx) = tokio::sync::oneshot::channel();
        let saved = Mutex::new(Some(saved_tx));
        let writes = Arc::new(AtomicU64::new(0));
        let calls = writes.clone();
        *settings.inner.write_hook.write() = Some(Arc::new(move |path, bytes| {
            if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                failed.lock().take().unwrap().send(()).unwrap();
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "fixture failure",
                ));
            }
            crate::atomic_file::write(path, bytes)?;
            saved.lock().take().unwrap().send(()).unwrap();
            Ok(())
        }));
        assert!(settings.set("value", "latest"));
        failed_rx.await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while settings.inner.writer_running.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(settings.inner.needs_save());
        assert!(
            !settings.set("value", "latest"),
            "a retry must not emit a setting-changed echo"
        );
        tokio::time::timeout(Duration::from_secs(1), saved_rx)
            .await
            .unwrap()
            .unwrap();
        settings.flush().unwrap();
        assert!(!settings.inner.needs_save());
        let saved: HashMap<String, String> =
            serde_json::from_slice(&std::fs::read(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(saved.get("value").map(String::as_str), Some("latest"));
        assert_eq!(writes.load(Ordering::Relaxed), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remove_and_clear_during_a_blocked_write_are_included_in_the_final_flush() {
        let (dir, settings) = fixture("remove-clear");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started_tx));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = Mutex::new(release_rx);
        let writes = Arc::new(AtomicU64::new(0));
        let calls = writes.clone();
        *settings.inner.write_hook.write() = Some(Arc::new(move |path, bytes| {
            if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                started.lock().take().unwrap().send(()).unwrap();
                let _ = release.lock().recv_timeout(Duration::from_secs(3));
            }
            crate::atomic_file::write(path, bytes)
        }));
        assert!(settings.set("value", "first"));
        started_rx.await.unwrap();
        settings.remove("seed");
        assert_eq!(settings.get("seed"), None);
        settings.clear();
        assert!(settings.get_all().is_empty());
        assert!(settings.set("value", "latest"));
        let final_settings = settings.clone();
        let flush = tokio::task::spawn_blocking(move || final_settings.flush());
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), flush)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let saved: HashMap<String, String> =
            serde_json::from_slice(&std::fs::read(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(
            saved,
            HashMap::from([("value".to_string(), "latest".to_string())])
        );
        assert_eq!(writes.load(Ordering::Relaxed), 2);
        assert!(!settings.inner.needs_save());
        settings.flush().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
