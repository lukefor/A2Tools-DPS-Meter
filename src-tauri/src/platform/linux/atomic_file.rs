//! File creation and permissions for an atomic replacement on Linux. A
//! temporary is private (0600) from creation, before it has a payload; a
//! replaced file keeps its mode, owner and access ACL (`file_permissions`).

use std::fs::{File, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use super::file_permissions::{self, SecuritySnapshot};

#[derive(Clone)]
pub struct ReplacementPermissions {
    permissions: Permissions,
    pub(super) security: SecuritySnapshot,
}

pub fn replacement_permissions(path: &Path) -> io::Result<Option<ReplacementPermissions>> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(ReplacementPermissions {
            permissions: metadata.permissions(),
            security: file_permissions::snapshot(path, &metadata)?,
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
    OpenOptions::new()
        .write(true)
        .create_new(true)
        // Set at creation, before either a name or any payload is exposed.
        .mode(0o600)
        .open(path)
}

pub fn apply_permissions(
    path: &Path,
    file: &File,
    permissions: &Option<ReplacementPermissions>,
) -> io::Result<()> {
    if let Some(permissions) = permissions {
        file_permissions::restore(path, file, &permissions.security)?;
        // The group mode bits are an ACL mask, not necessarily the owning
        // group's rights. Restore the ACL before widening that mask.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn temporary_is_private_before_any_payload_and_replacement_preserves_mode() {
        let dir = std::env::temp_dir().join(format!("a2t-private-temp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let mut file = create_new_private(&path).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 0);
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o077, 0);
        file.write_all(b"old").unwrap();
        drop(file);
        for mode in [0o600, 0o640] {
            std::fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
            crate::atomic_file::write(&path, b"new").unwrap();
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                mode
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"new");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Make a file these options create readable by its owner only (0600): the
/// debug log and packet logs hold character names and chat, and many
/// distributions leave home directories readable by every local user
/// (security audit L6). A file that already exists keeps its mode.
pub fn private(options: &mut OpenOptions) -> &mut OpenOptions {
    options.mode(0o600)
}
