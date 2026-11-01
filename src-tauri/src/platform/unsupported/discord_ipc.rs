//! Discord's local IPC: not ported to this target, so no Discord is found.

use std::path::{Path, PathBuf};

pub type Stream = std::fs::File;

pub fn socket_paths() -> Vec<PathBuf> {
    Vec::new()
}

pub fn connect(_path: &Path) -> std::io::Result<Stream> {
    Err(std::io::ErrorKind::Unsupported.into())
}

pub fn socket_id(_path: &Path) -> Option<u64> {
    None
}
