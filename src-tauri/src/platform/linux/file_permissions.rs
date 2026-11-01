//! Ownership and access ACLs for an atomic replacement on Linux.

use std::ffi::{CStr, CString};
use std::fs::{File, Metadata};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

const ACCESS_ACL: &CStr = c"system.posix_acl_access";
const ACL_READ_ATTEMPTS: usize = 4;
const MAX_XATTR_SIZE: usize = 65_536;

#[derive(Clone)]
pub(super) struct SecuritySnapshot {
    uid: libc::uid_t,
    gid: libc::gid_t,
    acl: Option<Vec<u8>>,
}

pub(super) fn snapshot(path: &Path, metadata: &Metadata) -> io::Result<SecuritySnapshot> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // Reading an ACL by name does not require read access to the payload. A
    // mode-0200 or mode-0000 file can still be replaced by its directory owner.
    let acl = read_acl_with(|buffer, size| {
        // SAFETY: both C strings remain alive; the writable buffer is either
        // null for a size query or has exactly `size` initialized bytes.
        let read =
            unsafe { libc::getxattr(path.as_ptr(), ACCESS_ACL.as_ptr(), buffer.cast(), size) };
        xattr_result(read)
    })?;
    Ok(SecuritySnapshot {
        uid: metadata.uid(),
        gid: metadata.gid(),
        acl,
    })
}

fn attribute_absent(error: &io::Error) -> bool {
    // ENOTSUP is the same value as EOPNOTSUPP on Linux.
    matches!(
        error.raw_os_error(),
        Some(libc::ENODATA) | Some(libc::EOPNOTSUPP)
    )
}

fn xattr_result(read: libc::ssize_t) -> io::Result<usize> {
    if read < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(read as usize)
    }
}

fn read_acl_with(
    mut read: impl FnMut(*mut u8, usize) -> io::Result<usize>,
) -> io::Result<Option<Vec<u8>>> {
    for _ in 0..ACL_READ_ATTEMPTS {
        let size = match read(std::ptr::null_mut(), 0) {
            Ok(size) => size,
            Err(error) if attribute_absent(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        if size == 0 || size > MAX_XATTR_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid access ACL size",
            ));
        }
        let mut acl = vec![0; size];
        match read(acl.as_mut_ptr(), acl.len()) {
            Ok(read) if read > 0 && read <= acl.len() => {
                acl.truncate(read);
                return Ok(Some(acl));
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid access ACL size",
                ));
            }
            // The ACL can grow between the size query and the read. Query
            // again, with a bounded number of attempts and no permission loss.
            Err(error) if error.raw_os_error() == Some(libc::ERANGE) => continue,
            Err(error) if attribute_absent(&error) => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "access ACL changed repeatedly",
    ))
}

/// An owner or group this user may not hand a file to: the previous file was
/// written by root (a sudo run, a rootful container) or for a group the user
/// has left, or the ID is unmapped in the current user namespace.
fn ownership_refused(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::EINVAL)
    )
}

pub(super) fn restore(path: &Path, file: &File, security: &SecuritySnapshot) -> io::Result<()> {
    let current = file.metadata()?;
    if current.uid() != security.uid || current.gid() != security.gid {
        // Unchanged IDs use -1: changing just the group should not require
        // permission to change the owner.
        let uid = if current.uid() == security.uid {
            libc::uid_t::MAX
        } else {
            security.uid
        };
        let gid = if current.gid() == security.gid {
            libc::gid_t::MAX
        } else {
            security.gid
        };
        // SAFETY: the file owns the live descriptor; uid_t/gid_t are Linux IDs.
        if unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } != 0 {
            let error = io::Error::last_os_error();
            if !ownership_refused(&error) {
                return Err(error);
            }
            // Failing here would fail every later save of that file, fights
            // included. Keep the write; the file now belongs to this user.
            tracing::warn!(
                "Could not keep the owner of {}; saving it as the current user: {error}",
                path.display()
            );
        } else {
            let restored = file.metadata()?;
            if restored.uid() != security.uid || restored.gid() != security.gid {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "file ownership was not preserved",
                ));
            }
        }
    }
    if let Some(acl) = &security.acl {
        // SAFETY: file owns the descriptor, the attribute name is a C string,
        // and the initialized ACL buffer remains alive for the call.
        if unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                ACCESS_ACL.as_ptr(),
                acl.as_ptr().cast(),
                acl.len(),
                0,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
    } else {
        // A mode-0600 temporary can inherit a parent's default ACL. Remove it
        // before chmod: widening its mask could otherwise grant named users
        // rights that the existing file never had.
        // SAFETY: file owns the descriptor and the attribute name is a C string.
        if unsafe { libc::fremovexattr(file.as_raw_fd(), ACCESS_ACL.as_ptr()) } != 0 {
            let error = io::Error::last_os_error();
            if !attribute_absent(&error) {
                return Err(error);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::atomic_file::{
        apply_permissions, create_new_private, replacement_permissions,
    };
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Directory(std::path::PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "a2t-linux-permissions-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // Linux's posix_acl_xattr_header/entry layout. Named-user read access is
    // intentional; the owning group must retain no access despite mode 0640.
    fn acl() -> Vec<u8> {
        // SAFETY: geteuid takes no parameters and returns the current user ID.
        let user = unsafe { libc::geteuid() };
        let mut acl = 2_u32.to_le_bytes().to_vec();
        for (tag, rights, id) in [
            (0x01_u16, 6_u16, u32::MAX),
            (0x02, 4, user),
            (0x04, 0, u32::MAX),
            (0x10, 4, u32::MAX),
            (0x20, 0, u32::MAX),
        ] {
            acl.extend_from_slice(&tag.to_le_bytes());
            acl.extend_from_slice(&rights.to_le_bytes());
            acl.extend_from_slice(&id.to_le_bytes());
        }
        acl
    }

    fn set_acl(file: &File, name: &CStr, acl: &[u8]) -> bool {
        // SAFETY: all pointers, the file descriptor and the ACL buffer are live.
        let result = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                acl.as_ptr().cast(),
                acl.len(),
                0,
            )
        };
        if result == 0 {
            true
        } else {
            let error = io::Error::last_os_error();
            if attribute_absent(&error)
                || matches!(
                    error.raw_os_error(),
                    Some(libc::ENOSYS | libc::EPERM | libc::EACCES)
                )
            {
                false
            } else {
                panic!("{error}");
            }
        }
    }

    fn file_acl(file: &File) -> Option<Vec<u8>> {
        read_acl_with(|buffer, size| {
            // SAFETY: file owns the live descriptor, name is a C string, and
            // buffer is null or points to `size` initialized bytes.
            xattr_result(unsafe {
                libc::fgetxattr(file.as_raw_fd(), ACCESS_ACL.as_ptr(), buffer.cast(), size)
            })
        })
        .unwrap()
    }

    #[test]
    fn atomic_replacement_preserves_the_exact_access_acl_and_owner() {
        let dir = Directory::new();
        let path = dir.0.join("settings.json");
        let mut file = create_new_private(&path).unwrap();
        file.write_all(b"old").unwrap();
        if !set_acl(&file, ACCESS_ACL, &acl()) {
            eprintln!("POSIX ACLs are not supported on this filesystem");
            return;
        }
        let before = file.metadata().unwrap();
        assert_eq!(before.permissions().mode() & 0o777, 0o640);
        drop(file);
        for mode in [0o640, 0o600] {
            let original = File::open(&path).unwrap();
            original
                .set_permissions(std::fs::Permissions::from_mode(mode))
                .unwrap();
            // chmod changes the ACL mask. At 0600 the named reader remains
            // present but must retain no effective access after replacement.
            let expected_acl = file_acl(&original).unwrap();
            drop(original);
            for contents in [b"new".as_slice(), b"newer".as_slice()] {
                crate::atomic_file::write(&path, contents).unwrap();
                let file = File::open(&path).unwrap();
                let after = file.metadata().unwrap();
                assert_eq!(file_acl(&file).unwrap(), expected_acl);
                assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
                assert_eq!(after.permissions().mode() & 0o777, mode);
                assert_eq!(std::fs::read(&path).unwrap(), contents);
                assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
            }
        }
    }

    #[test]
    fn default_acl_does_not_leak_into_a_replacement_without_an_access_acl() {
        let dir = Directory::new();
        let path = dir.0.join("settings.json");
        let original = create_new_private(&path).unwrap();
        original
            .set_permissions(std::fs::Permissions::from_mode(0o640))
            .unwrap();
        assert!(file_acl(&original).is_none());
        if !set_acl(
            &File::open(&dir.0).unwrap(),
            c"system.posix_acl_default",
            &acl(),
        ) {
            eprintln!("POSIX ACLs are not supported on this filesystem");
            return;
        }
        let temporary = create_new_private(&dir.0.join("probe.tmp")).unwrap();
        assert_eq!(temporary.metadata().unwrap().len(), 0);
        assert_eq!(
            temporary.metadata().unwrap().permissions().mode() & 0o077,
            0
        );
        assert!(
            file_acl(&temporary).is_some(),
            "default ACL must be inherited for this regression"
        );
        drop(temporary);
        std::fs::remove_file(dir.0.join("probe.tmp")).unwrap();
        crate::atomic_file::write(&path, b"new").unwrap();
        let replaced = File::open(&path).unwrap();
        assert!(
            file_acl(&replaced).is_none(),
            "named users must not gain access when chmod widens the group bits"
        );
        assert_eq!(
            replaced.metadata().unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn an_existing_file_does_not_require_read_or_write_access_to_its_payload() {
        let dir = Directory::new();
        let path = dir.0.join("settings.json");
        std::fs::write(&path, b"old").unwrap();
        for mode in [0o000, 0o200, 0o400, 0o600] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            crate::atomic_file::write(&path, b"new").unwrap();
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                mode
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), b"new");
        }
    }

    #[test]
    fn owning_group_is_preserved_when_it_differs_from_new_file_creation() {
        let dir = Directory::new();
        let path = dir.0.join("settings.json");
        let original = create_new_private(&path).unwrap();
        let initial = original.metadata().unwrap();
        // SAFETY: size-0 getgroups performs a size query and ignores the pointer.
        let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        assert!(count >= 0);
        let mut groups = vec![0; count as usize];
        // SAFETY: the initialized buffer has the queried number of gid_t items.
        let count = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
        assert!(count >= 0);
        groups.truncate(count as usize);
        let Some(group) = groups.into_iter().find(|group| {
            *group != initial.gid()
                && unsafe { libc::fchown(original.as_raw_fd(), libc::uid_t::MAX, *group) } == 0
        }) else {
            eprintln!("owning-group regression requires a usable second supplementary group");
            return;
        };
        original
            .set_permissions(std::fs::Permissions::from_mode(0o640))
            .unwrap();
        drop(original);
        crate::atomic_file::write(&path, b"new").unwrap();
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!((after.uid(), after.gid()), (initial.uid(), group));
        assert_eq!(after.permissions().mode() & 0o777, 0o640);
    }

    #[test]
    fn permission_restoration_uses_the_descriptor_if_the_temporary_name_changes() {
        let dir = Directory::new();
        let original_path = dir.0.join("settings.json");
        let original = create_new_private(&original_path).unwrap();
        if !set_acl(&original, ACCESS_ACL, &acl()) {
            eprintln!("POSIX ACLs are not supported on this filesystem");
            return;
        }
        let permissions = replacement_permissions(&original_path).unwrap();
        let temporary_path = dir.0.join("temporary.json");
        let temporary = create_new_private(&temporary_path).unwrap();
        let moved_path = dir.0.join("moved.json");
        std::fs::rename(&temporary_path, &moved_path).unwrap();
        let unrelated_path = dir.0.join("unrelated.json");
        std::fs::write(&unrelated_path, b"untouched").unwrap();
        std::fs::set_permissions(&unrelated_path, std::fs::Permissions::from_mode(0o400)).unwrap();
        std::os::unix::fs::symlink(&unrelated_path, &temporary_path).unwrap();
        apply_permissions(&temporary_path, &temporary, &permissions).unwrap();
        assert_eq!(file_acl(&temporary), file_acl(&original));
        assert_eq!(
            temporary.metadata().unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(
            std::fs::metadata(&unrelated_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o400
        );
        assert_eq!(std::fs::read(&unrelated_path).unwrap(), b"untouched");
    }

    #[test]
    fn an_owner_that_cannot_be_restored_does_not_lose_the_write() {
        // Root can chown to any ID, so this tests the unprivileged path: the
        // previous file belonged to someone this user cannot give it back to.
        // SAFETY: geteuid has no arguments and returns the effective user ID.
        let user = unsafe { libc::geteuid() };
        if user == 0 {
            eprintln!("owner-restoration regression requires an unprivileged user");
            return;
        }
        let dir = Directory::new();
        let path = dir.0.join("settings.json");
        std::fs::write(&path, b"previous").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let mut permissions = replacement_permissions(&path).unwrap();
        let security = &mut permissions.as_mut().unwrap().security;
        security.uid = if security.uid == 65_534 {
            65_533
        } else {
            65_534
        };
        let temporary_path = dir.0.join("temporary.json");
        let mut temporary = create_new_private(&temporary_path).unwrap();
        temporary.write_all(b"new").unwrap();
        apply_permissions(&temporary_path, &temporary, &permissions).unwrap();
        let metadata = temporary.metadata().unwrap();
        assert_eq!(metadata.uid(), user);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
        drop(temporary);
        std::fs::rename(&temporary_path, &path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::metadata(&path).unwrap().uid(), user);
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn only_a_refused_owner_change_is_skipped() {
        for refused in [libc::EPERM, libc::EACCES, libc::EINVAL] {
            assert!(ownership_refused(&io::Error::from_raw_os_error(refused)));
        }
        for error in [libc::EIO, libc::EBADF, libc::EROFS] {
            assert!(!ownership_refused(&io::Error::from_raw_os_error(error)));
        }
    }

    #[test]
    fn a_growing_acl_is_queried_again_after_erange() {
        let expected = acl();
        let mut calls = 0;
        let read = read_acl_with(|buffer, size| {
            calls += 1;
            match calls {
                1 => Ok(4),
                2 => Err(io::Error::from_raw_os_error(libc::ERANGE)),
                3 => Ok(expected.len()),
                4 => {
                    assert_eq!(size, expected.len());
                    // SAFETY: read_acl_with provided an initialized buffer of size.
                    unsafe { std::ptr::copy_nonoverlapping(expected.as_ptr(), buffer, size) };
                    Ok(size)
                }
                _ => panic!("unexpected ACL read"),
            }
        })
        .unwrap();
        assert_eq!(read, Some(expected));
        assert_eq!(calls, 4);
    }

    #[test]
    fn acl_read_errors_are_not_treated_as_unrestricted_permissions() {
        for error in [libc::EACCES, libc::EIO, libc::EPERM] {
            assert_eq!(
                read_acl_with(|_, _| Err(io::Error::from_raw_os_error(error)))
                    .unwrap_err()
                    .raw_os_error(),
                Some(error)
            );
        }
        for absent in [libc::ENODATA, libc::EOPNOTSUPP] {
            assert!(
                read_acl_with(|_, _| Err(io::Error::from_raw_os_error(absent)))
                    .unwrap()
                    .is_none()
            );
        }
        let mut calls = 0;
        assert!(
            read_acl_with(|_, _| {
                calls += 1;
                if calls % 2 == 1 {
                    Ok(4)
                } else {
                    Err(io::Error::from_raw_os_error(libc::ENODATA))
                }
            })
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn a_repeatedly_growing_acl_fails_instead_of_retrying_forever() {
        let mut calls = 0;
        let error = read_acl_with(|_, _| {
            calls += 1;
            if calls % 2 == 1 {
                Ok(4)
            } else {
                Err(io::Error::from_raw_os_error(libc::ERANGE))
            }
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(calls, ACL_READ_ATTEMPTS * 2);
    }
}
