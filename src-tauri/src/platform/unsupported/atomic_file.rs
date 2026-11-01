//! File creation and permissions for every target without a port. New files
//! get the platform's default permissions; a replaced file keeps the
//! permissions the standard library can carry over (its read-only flag or
//! mode bits).

use std::fs::{File, OpenOptions, Permissions};
use std::io;
use std::path::Path;

#[derive(Clone)]
pub struct ReplacementPermissions {
    permissions: Permissions,
}

pub fn replacement_permissions(path: &Path) -> io::Result<Option<ReplacementPermissions>> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(ReplacementPermissions {
            permissions: metadata.permissions(),
        })),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target is not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn create_new_private(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

pub fn apply_permissions(
    _path: &Path,
    file: &File,
    permissions: &Option<ReplacementPermissions>,
) -> io::Result<()> {
    if let Some(permissions) = permissions {
        file.set_permissions(permissions.permissions.clone())?;
    }
    Ok(())
}

pub fn rename_retryable(error: &io::Error, _target: &Path) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

/// The platform's default permissions: no port to narrow them.
pub fn private(options: &mut OpenOptions) -> &mut OpenOptions {
    options
}
