//! Discord's local IPC sockets, for the Discord activity (`presence.rs`).

use std::path::{Path, PathBuf};
use std::time::Duration;

pub type Stream = std::os::unix::net::UnixStream;

/// Where Discord apps put their sockets, as the `discord-rich-presence`
/// crate searches them (also inside Flatpak and Snap), but every one that
/// exists rather than the first.
pub fn socket_paths() -> Vec<PathBuf> {
    const SUBPATHS: [&str; 7] = [
        "",
        "app/com.discordapp.Discord/",
        "app/dev.vencord.Vesktop/",
        ".flatpak/com.discordapp.Discord/xdg-run/",
        ".flatpak/dev.vencord.Vesktop/xdg-run/",
        "snap.discord-canary/",
        "snap.discord/",
    ];
    let mut found = Vec::new();
    // `/tmp` last, as the crate falls back to it when none is set.
    let bases = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .chain(std::iter::once("/tmp".to_string()));
    for base in bases {
        let base = PathBuf::from(base);
        for sub in SUBPATHS {
            for i in 0..10 {
                let path = base.join(sub).join(format!("discord-ipc-{i}"));
                if path.exists() && !found.contains(&path) {
                    found.push(path);
                }
            }
        }
    }
    found
}

pub fn connect(path: &Path) -> std::io::Result<Stream> {
    let stream = Stream::connect(path)?;
    // Discord answers at once; a hung one must not stall the activity thread.
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(stream)
}

/// Which socket file is at `path` (its inode): a Discord that starts again
/// makes a new one in the same place.
pub fn socket_id(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.ino())
}
