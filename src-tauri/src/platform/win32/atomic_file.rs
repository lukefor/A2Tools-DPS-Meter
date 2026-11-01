//! Temporary files for an atomic replacement, and keeping a replaced file's DACL.
//!
//! New files and temporaries take the DACL their folder hands down. The meter's
//! data lives under %APPDATA%, which is already private to the user, and the
//! inherited ACEs name the user directly. An explicit OWNER RIGHTS ACE would
//! not: an elevated run's files are owned by BUILTIN\Administrators, which is
//! deny-only in the user's normal token, so a later non-elevated run could
//! neither read nor replace them.
//!
//! Replacing a file keeps its DACL when it can. When the DACL cannot be read or
//! applied (written by an elevated run, or a filesystem without Windows
//! security), the replacement inherits instead of failing the write.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetFileSecurityW, GetSecurityDescriptorControl, SetFileSecurityW, DACL_SECURITY_INFORMATION,
    OBJECT_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    SE_DACL_PROTECTED, UNPROTECTED_DACL_SECURITY_INFORMATION,
};

/// The DACL an unreleased build of the private-settings change gave every file
/// it wrote. Keeping it would carry the elevated-owner problem above into each
/// replacement, so a file that has it inherits its folder's DACL instead.
const LEGACY_PRIVATE_DACL: &str = "D:P(A;;FA;;;SY)(A;;FA;;;OW)";

pub struct ReplacementPermissions {
    // u64 keeps the self-relative SECURITY_DESCRIPTOR suitably aligned.
    descriptor: Vec<u64>,
    information: OBJECT_SECURITY_INFORMATION,
}

impl ReplacementPermissions {
    fn pointer(&self) -> PSECURITY_DESCRIPTOR {
        PSECURITY_DESCRIPTOR(self.descriptor.as_ptr().cast_mut().cast())
    }
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    // canonicalize supplies the extended Windows prefix for long paths.
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut wide: Vec<_> = parent
        .canonicalize()?
        .join(name)
        .as_os_str()
        .encode_wide()
        .collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "nul in file name",
        ));
    }
    wide.push(0);
    Ok(wide)
}

/// The filesystem has no Windows security to query or set (FAT, exFAT, some
/// network and virtual drives), as opposed to refusing this process.
fn security_unsupported(error: &io::Error) -> bool {
    // ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_CALL_NOT_IMPLEMENTED.
    matches!(error.raw_os_error(), Some(1 | 50 | 120))
}

/// A DACL that cannot be kept is not a reason to lose the write: the
/// replacement inherits the folder's DACL, like a newly created file.
fn inherit_instead(action: &str, path: &Path, error: &io::Error) {
    if security_unsupported(error) {
        tracing::debug!(
            "No file security to {action} for {}; it inherits its folder's: {error}",
            path.display()
        );
    } else {
        tracing::warn!(
            "Could not {action} the permissions of {}; it inherits its folder's: {error}",
            path.display()
        );
    }
}

fn read_dacl(wide: &[u16]) -> io::Result<ReplacementPermissions> {
    let mut needed = 0;
    let _ = unsafe {
        GetFileSecurityW(
            PCWSTR(wide.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            None,
            0,
            &mut needed,
        )
    };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut descriptor = vec![0u64; (needed as usize).div_ceil(8)];
    let pointer = PSECURITY_DESCRIPTOR(descriptor.as_mut_ptr().cast());
    if !unsafe {
        GetFileSecurityW(
            PCWSTR(wide.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            Some(pointer),
            needed,
            &mut needed,
        )
    }
    .as_bool()
    {
        return Err(io::Error::last_os_error());
    }
    let (mut control, mut revision) = (0, 0);
    unsafe { GetSecurityDescriptorControl(pointer, &mut control, &mut revision) }
        .map_err(io::Error::from)?;
    let protection = if control & SE_DACL_PROTECTED.0 != 0 {
        PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        UNPROTECTED_DACL_SECURITY_INFORMATION
    };
    Ok(ReplacementPermissions {
        descriptor,
        information: DACL_SECURITY_INFORMATION | protection,
    })
}

fn dacl_string(permissions: &ReplacementPermissions) -> io::Result<String> {
    let mut string = PWSTR::null();
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            permissions.pointer(),
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut string,
            None,
        )
    }
    .map_err(io::Error::from)?;
    let result = unsafe { string.to_string() };
    unsafe { LocalFree(Some(HLOCAL(string.0.cast()))) };
    result.map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DACL is not UTF-16"))
}

pub fn replacement_permissions(path: &Path) -> io::Result<Option<ReplacementPermissions>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target is not a regular file",
        ));
    }
    if metadata.permissions().readonly() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "target is read-only",
        ));
    }
    let permissions = match read_dacl(&wide_path(path)?) {
        Ok(permissions) => permissions,
        Err(error) => {
            // Replacing still works: the folder's delete-child right lets the
            // rename remove a file this process cannot open.
            inherit_instead("read", path, &error);
            return Ok(None);
        }
    };
    if dacl_string(&permissions).is_ok_and(|dacl| dacl == LEGACY_PRIVATE_DACL) {
        tracing::debug!(
            "{} has the old owner-only DACL; its replacement inherits its folder's",
            path.display()
        );
        return Ok(None);
    }
    Ok(Some(permissions))
}

/// Creates a new file with the DACL its folder hands down (see the module
/// documentation for why it is not an explicit owner-only DACL).
pub fn create_new_private(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE.
        .share_mode(0x1 | 0x2 | 0x4)
        .open(path)
}

pub fn apply_permissions(
    path: &Path,
    _file: &File,
    permissions: &Option<ReplacementPermissions>,
) -> io::Result<()> {
    if let Some(permissions) = permissions {
        let wide = wide_path(path)?;
        if !unsafe {
            SetFileSecurityW(
                PCWSTR(wide.as_ptr()),
                permissions.information,
                permissions.pointer(),
            )
        }
        .as_bool()
        {
            // The temporary keeps the DACL it inherited at creation.
            inherit_instead("apply", path, &io::Error::last_os_error());
        }
    }
    // Keep std::fs::rename: ReplaceFileW can remove the old target on a failed
    // replacement (ERROR_UNABLE_TO_MOVE_REPLACEMENT), unlike this contract.
    Ok(())
}

pub fn rename_retryable(error: &io::Error, target: &Path) -> bool {
    match error.raw_os_error() {
        Some(32 | 33) => true, // sharing/lock violation
        Some(5) => {
            // Some rename implementations report ACCESS_DENIED for a held
            // destination. Retry it only when opening for DELETE proves that
            // it is a sharing violation, not a persistent ACL denial.
            matches!(OpenOptions::new().access_mode(0x0001_0000)
                .share_mode(0x1 | 0x2 | 0x4).open(target),
                Err(error) if matches!(error.raw_os_error(), Some(32 | 33)))
        }
        _ => matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::w;
    use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;

    struct Directory(std::path::PathBuf);

    impl Directory {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("a2t-dacl-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn file_dacl(path: &Path) -> ReplacementPermissions {
        read_dacl(&wide_path(path).unwrap()).unwrap()
    }

    fn sddl(path: &Path) -> String {
        dacl_string(&file_dacl(path)).unwrap()
    }

    fn protected(path: &Path) -> bool {
        let permissions = file_dacl(path);
        let (mut control, mut revision) = (0, 0);
        unsafe { GetSecurityDescriptorControl(permissions.pointer(), &mut control, &mut revision) }
            .unwrap();
        control & SE_DACL_PROTECTED.0 != 0
    }

    /// Sets a DACL the way an older build or another tool would have.
    fn set_dacl(path: &Path, dacl: PCWSTR) {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                dacl,
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
            .unwrap();
        }
        let wide = wide_path(path).unwrap();
        let set = unsafe {
            SetFileSecurityW(
                PCWSTR(wide.as_ptr()),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor,
            )
        };
        unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        assert!(set.as_bool(), "{}", io::Error::last_os_error());
    }

    #[test]
    fn a_written_file_inherits_its_folders_dacl() {
        let dir = Directory::new("inherit");
        let path = dir.0.join("settings.json");
        crate::atomic_file::write(&path, b"first").unwrap();
        assert!(
            !protected(&path),
            "new file has a protected DACL: {}",
            sddl(&path)
        );
        // No `;ID;` check: a folder whose ACEs do not inherit (CI's temp
        // folder) gives a new file the token's default DACL instead, also
        // unprotected. Not protected is what matters.
        crate::atomic_file::write(&path, b"second").unwrap();
        assert!(
            !protected(&path),
            "replacement has a protected DACL: {}",
            sddl(&path)
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn a_temporary_inherits_and_has_no_payload_yet() {
        let dir = Directory::new("temporary");
        let path = dir.0.join("settings.json.tmp");
        let file = create_new_private(&path).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 0);
        assert!(
            !protected(&path),
            "temporary has a protected DACL: {}",
            sddl(&path)
        );
        assert_eq!(
            create_new_private(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn an_existing_protected_dacl_survives_replacement() {
        let dir = Directory::new("preserve");
        let path = dir.0.join("settings.json");
        crate::atomic_file::write(&path, b"old").unwrap();
        // A stricter DACL than the folder's, set by the user or another tool.
        set_dacl(&path, w!("D:P(A;;FA;;;OW)"));
        let before = sddl(&path);
        crate::atomic_file::write(&path, b"private settings").unwrap();
        assert_eq!(sddl(&path), before);
        assert!(protected(&path));
        assert_eq!(std::fs::read(&path).unwrap(), b"private settings");
    }

    #[test]
    fn the_old_owner_only_dacl_is_replaced_by_the_folders() {
        let dir = Directory::new("legacy");
        let path = dir.0.join("settings.json");
        crate::atomic_file::write(&path, b"old").unwrap();
        set_dacl(&path, w!("D:P(A;;FA;;;SY)(A;;FA;;;OW)"));
        assert_eq!(sddl(&path), LEGACY_PRIVATE_DACL);
        crate::atomic_file::write(&path, b"new").unwrap();
        assert!(!protected(&path), "the old DACL was kept: {}", sddl(&path));
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn a_file_this_process_cannot_open_is_still_replaced() {
        // Like a file an elevated run left behind: the owner rights grant only
        // FILE_READ_ATTRIBUTES, so this process can neither read the payload
        // nor the DACL. The folder's delete-child right still allows the rename.
        let dir = Directory::new("unreadable");
        let path = dir.0.join("fight.json");
        crate::atomic_file::write(&path, b"old").unwrap();
        set_dacl(&path, w!("D:P(A;;FA;;;SY)(A;;0x80;;;OW)"));
        assert!(std::fs::read(&path).is_err());
        assert!(read_dacl(&wide_path(&path).unwrap()).is_err());
        crate::atomic_file::write(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(
            !protected(&path),
            "replacement did not inherit: {}",
            sddl(&path)
        );
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn a_dacl_that_cannot_be_applied_leaves_the_inherited_one() {
        let dir = Directory::new("apply");
        let source = dir.0.join("source.json");
        crate::atomic_file::write(&source, b"source").unwrap();
        let permissions = Some(file_dacl(&source));
        let temporary_path = dir.0.join("temporary.json");
        let temporary = create_new_private(&temporary_path).unwrap();
        // Make the temporary refuse WRITE_DAC to its owner.
        set_dacl(&temporary_path, w!("D:P(A;;FA;;;SY)(A;;0x80;;;OW)"));
        assert!(read_dacl(&wide_path(&temporary_path).unwrap()).is_err());
        apply_permissions(&temporary_path, &temporary, &permissions).unwrap();
    }

    #[test]
    fn missing_file_security_is_told_apart_from_a_refusal() {
        for code in [1, 50, 120] {
            assert!(security_unsupported(&io::Error::from_raw_os_error(code)));
        }
        for code in [5, 2, 87] {
            assert!(!security_unsupported(&io::Error::from_raw_os_error(code)));
        }
    }
}

/// Files the meter creates under %APPDATA% are already private to the user
/// (the folder's inherited DACL): nothing to change.
pub fn private(options: &mut std::fs::OpenOptions) -> &mut std::fs::OpenOptions {
    options
}
