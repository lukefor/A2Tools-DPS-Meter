//! Updating the installed package: the package manager that installed the
//! meter installs the new one (the Arch package through pacman, the .deb
//! through apt, the .rpm through dnf or zypper), and pkexec asks for the
//! password through the desktop's own prompt (KDE, GNOME…). Each package's
//! install script grants packet capture to the new binary again.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::platform::UpdatePackages;

const INSTALLED_BINARY: &str = "/usr/bin/a2tools-dps-meter";

/// The package manager that owns the installed meter, and how it installs a
/// downloaded package file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Manager {
    Pacman,
    Apt,
    Dpkg,
    Dnf,
    Zypper,
    Rpm,
}

impl Manager {
    /// The command, run as root, that installs the package file given as `$1`.
    fn install_command(self) -> &'static str {
        match self {
            Manager::Pacman => "/usr/bin/pacman -U --noconfirm",
            // apt resolves any new dependency; dpkg alone would leave it broken.
            Manager::Apt => "/usr/bin/apt-get install -y --allow-downgrades",
            Manager::Dpkg => "/usr/bin/dpkg -i",
            Manager::Dnf => "/usr/bin/dnf install -y",
            // Signed, but with a key zypper doesn't know; the package's
            // hash is the signed manifest's, checked before this runs.
            Manager::Zypper => "/usr/bin/zypper --non-interactive install --allow-unsigned-rpm",
            Manager::Rpm => "/usr/bin/rpm -U --replacepkgs",
        }
    }

    fn package<'a>(self, packages: &UpdatePackages<'a>) -> &'a str {
        match self {
            Manager::Pacman => packages.arch,
            Manager::Apt | Manager::Dpkg => packages.deb,
            Manager::Dnf | Manager::Zypper | Manager::Rpm => packages.rpm,
        }
    }
}

fn succeeds(program: &str, args: &[&str]) -> bool {
    Path::new(program).exists()
        && Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// Which package manager installed the running meter, asked of each one's
/// database. A build run from a checkout, or a copy put in place by hand, is
/// owned by none of them and is left alone.
fn manager() -> Option<Manager> {
    let installed = std::env::current_exe().is_ok_and(|exe| exe == Path::new(INSTALLED_BINARY));
    if !installed || !Path::new("/usr/bin/pkexec").exists() {
        return None;
    }
    // Bazzite, Silverblue, Kinoite and the other image-based Fedoras: /usr is
    // read-only, a package is layered with rpm-ostree and only takes effect
    // after a reboot. The meter comes from our RPM repository there and is
    // updated with the system (`rpm-ostree upgrade`, or the distribution's
    // automatic updates), so offering a dnf install would only fail and ask
    // again at every start.
    if Path::new("/run/ostree-booted").exists() {
        return None;
    }
    if succeeds("/usr/bin/pacman", &["-Qqo", INSTALLED_BINARY]) {
        return Some(Manager::Pacman);
    }
    if succeeds("/usr/bin/dpkg", &["-S", INSTALLED_BINARY]) {
        return Some(if Path::new("/usr/bin/apt-get").exists() { Manager::Apt } else { Manager::Dpkg });
    }
    if succeeds("/usr/bin/rpm", &["-qf", INSTALLED_BINARY]) {
        return Some(if Path::new("/usr/bin/dnf").exists() {
            Manager::Dnf
        } else if Path::new("/usr/bin/zypper").exists() {
            Manager::Zypper
        } else {
            Manager::Rpm
        });
    }
    None
}

/// Only a meter a package manager installed updates itself.
pub fn supported() -> bool {
    manager().is_some()
}

/// The manifest's package for the package manager that installed this meter.
pub fn package_url<'a>(packages: &UpdatePackages<'a>) -> &'a str {
    manager().map_or("", |m| m.package(packages))
}

/// What runs as root: the package copied into a folder only root can write,
/// its SHA-256 checked there, and that copy installed. The download sits in
/// the player's own /tmp file until then, where anything running as the
/// player could swap it after the meter checked it (security audit M2).
fn root_script(install: &str) -> String {
    format!(
        r#"d=$(/usr/bin/mktemp -d) || exit 1
f="$d/$(/usr/bin/basename "$1")"
/usr/bin/cp -- "$1" "$f" && echo "$2  $f" | /usr/bin/sha256sum -c --status && {install} "$f"
s=$?
/usr/bin/rm -rf "$d"
exit $s"#
    )
}

/// Hand the package to its package manager once the meter has exited, then
/// start the meter again: the new one, or the old one if the password prompt
/// was cancelled, so a refused update never leaves the player without a meter.
pub fn run_installer(package: &Path, sha256: &str, _install_dir: &str) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let manager = manager().ok_or("this install cannot update itself")?;
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("no checksum for the package".into());
    }
    let script = format!(
        r#"
        while kill -0 "$2" 2>/dev/null; do sleep 0.2; done
        pkexec /bin/sh -c '{root}' a2tools-update "$1" "$3"
        rm -f "$1"
        exec {INSTALLED_BINARY}
    "#,
        root = root_script(manager.install_command()),
    );
    tracing::info!("Updating through {:?}: {}", manager, package.display());
    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("a2tools-update")
        .arg(package)
        .arg(std::process::id().to_string())
        .arg(sha256.to_ascii_lowercase())
        .stdin(Stdio::null())
        // Its own process group, so closing a terminal the meter was started
        // from does not take the update down with it.
        .process_group(0)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not start the update: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_manager_takes_its_own_package() {
        let p = UpdatePackages { msi: "m", arch: "a", deb: "d", rpm: "r" };
        assert_eq!(Manager::Pacman.package(&p), "a");
        assert_eq!(Manager::Apt.package(&p), "d");
        assert_eq!(Manager::Dpkg.package(&p), "d");
        assert_eq!(Manager::Dnf.package(&p), "r");
        assert_eq!(Manager::Zypper.package(&p), "r");
        assert_eq!(Manager::Rpm.package(&p), "r");
    }

    #[test]
    fn root_checks_its_own_copy_before_installing_it() {
        let script = root_script(Manager::Pacman.install_command());
        // Quoted once inside sh -c '…': no single quote of its own.
        assert!(!script.contains('\''));
        let check = script.find("sha256sum -c").unwrap();
        let install = script.find("/usr/bin/pacman -U").unwrap();
        assert!(script.find("cp --").unwrap() < check && check < install);
        assert!(script.contains(r#"&& /usr/bin/pacman -U --noconfirm "$f""#));
    }

    #[test]
    fn a_test_run_is_not_an_installed_meter() {
        // The test binary is not /usr/bin/a2tools-dps-meter.
        assert_eq!(manager(), None);
        assert!(!supported());
    }
}
