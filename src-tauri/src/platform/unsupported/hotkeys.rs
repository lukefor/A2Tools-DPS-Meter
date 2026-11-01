/// There are no hotkeys here to turn on.
pub const DEFAULT_ON: bool = false;

/// No global hotkeys on this platform.
pub struct HotkeyManager;

impl HotkeyManager {
    pub fn new() -> Self {
        Self
    }
    pub fn start(
        &self,
        _: Vec<crate::platform::hotkeys::Binding>,
        _: impl Fn(&'static str, &'static str, Option<String>) + Send + 'static,
    ) {
    }
    pub fn open_desktop_settings(&self) -> Result<(), String> {
        Err("no desktop shortcut settings here".into())
    }
    pub fn stop(&self) {}
}

/// The hotkeys helper (`a2tools-hotkeys`) is for Linux, where the portal
/// cannot be asked from the meter itself; here it has nothing to do.
pub fn run_helper() -> i32 {
    println!("error the hotkeys helper is only used on Linux");
    2
}
