/// The freedesktop way, which is what a Unix-like target most likely has.
pub fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// What to tell the player when `ssh_command` finds no ssh client.
pub const SSH_MISSING_HELP: &str = "Install an OpenSSH client (e.g. `sudo apt install openssh-client`)";

/// The ssh client, for capturing on the router.
pub fn ssh_command() -> std::process::Command {
    std::process::Command::new("ssh")
}
