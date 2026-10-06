//! Handing things to the OS shell.

/// Open a URL in the default browser.
pub fn open_url(url: &str) {
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
}

/// What to tell the player when `ssh_command` finds no ssh client.
pub const SSH_MISSING_HELP: &str =
    "Add Windows' \"OpenSSH Client\" (Settings > System > Optional features)";

/// The ssh client, for capturing on the router: Windows' own OpenSSH, or
/// whichever `ssh` is on PATH when that is not installed. It runs with no
/// console window, which would otherwise flash up over the game.
pub fn ssh_command() -> std::process::Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let bundled = std::path::Path::new(&root).join(r"System32\OpenSSH\ssh.exe");
    let mut cmd = if bundled.exists() {
        std::process::Command::new(bundled)
    } else {
        std::process::Command::new("ssh")
    };
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}
