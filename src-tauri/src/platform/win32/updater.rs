//! Installing a downloaded update.

use std::path::Path;

/// Updates install themselves here (the MSI).
pub fn supported() -> bool {
    true
}

/// The manifest's package for this platform.
pub fn package_url<'a>(packages: &crate::platform::UpdatePackages<'a>) -> &'a str {
    packages.msi
}

/// Start the MSI installer over the current install and return; the caller
/// exits so the installer can replace the running files.
///
/// msiexec.exe uses its own non-standard command line parser, so
/// PROPERTY="value" pairs with spaces require literal embedded quotes, which
/// std::process::Command's normal arg quoting does not produce. raw_arg
/// controls the exact command line.
///
/// The package is checked again through a handle that lets others only read
/// it, kept until the meter exits: nothing can swap it between the check and
/// msiexec reading it (#131).
pub fn run_installer(package: &Path, sha256: &str, install_dir: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::process::CommandExt;
    // FILE_SHARE_READ alone.
    let mut held = std::fs::OpenOptions::new().read(true).share_mode(0x1).open(package).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0u8; 1 << 16];
    loop {
        let n = std::io::Read::read(&mut held, &mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&chunk[..n]);
    }
    let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if !sha256.is_empty() && !actual.eq_ignore_ascii_case(sha256) {
        return Err("the update package changed after it was checked".into());
    }
    // Closed when the meter exits, after msiexec has it open.
    std::mem::forget(held);
    let mut cmd = std::process::Command::new("msiexec");
    cmd.raw_arg("/i")
        .raw_arg(format!("\"{}\"", package.display()))
        .raw_arg("/passive")
        .raw_arg(format!("INSTALLDIR=\"{}\"", install_dir))
        .raw_arg("AUTOLAUNCHAPP=1");
    tracing::info!(
        "msiexec args: /i \"{}\" /passive INSTALLDIR=\"{}\" AUTOLAUNCHAPP=1",
        package.display(),
        install_dir
    );
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}
