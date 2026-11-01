//! Handing things to the OS shell.

/// Open a URL in the default browser.
///
/// ShellExecuteW hands the URL to its registered handler as it is. `cmd /C
/// start` did this before, and cmd reads `&`, `|` and the like in the URL as
/// its own syntax: a link from a plugin could have run a command.
pub fn open_url(url: &str) {
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
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
