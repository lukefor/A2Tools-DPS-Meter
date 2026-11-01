use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tracing::{info, warn};

/// On unless the user turns them off (Settings → Keybinds).
pub const DEFAULT_ON: bool = true;

/// Global hotkey manager using Win32 RegisterHotKey.
/// Runs its own message loop thread to receive WM_HOTKEY messages.
pub struct HotkeyManager {
    /// The listener thread and its own stop flag, while one runs. Each start
    /// makes a new pair, so a stop and a start cannot share a flag.
    listener: std::sync::Mutex<Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>>,
}

impl HotkeyManager {
    pub fn new() -> Self {
        Self {
            listener: std::sync::Mutex::new(None),
        }
    }

    /// Start the hotkey listener thread with `bindings`: each one's
    /// `on_fire` runs when its key is pressed. `on_status(name, registered)`
    /// says, for each key set, whether Windows let the meter have it: false
    /// when another program holds that combination.
    pub fn start(
        &self,
        bindings: Vec<crate::platform::hotkeys::Binding>,
        on_status: impl Fn(&'static str, &'static str, Option<String>) + Send + 'static,
    ) {
        let mut listener = self.listener.lock().unwrap_or_else(|p| p.into_inner());
        if listener.as_ref().is_some_and(|(running, _)| running.load(Ordering::SeqCst)) {
            return;
        }

        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();

        let thread = std::thread::spawn(move || {
            use windows::Win32::UI::Input::KeyboardAndMouse::*;
            use windows::Win32::UI::WindowsAndMessaging::*;

            unsafe {
                let norepeat = 0x4000u32;
                // Each is registered under its place in the list, from 1.
                let mut registered = vec![false; bindings.len()];
                for (n, binding) in bindings.iter().enumerate() {
                    if binding.vk == 0 {
                        continue;
                    }
                    let ok = RegisterHotKey(None, n as i32 + 1,
                        HOT_KEY_MODIFIERS(binding.mods | norepeat), binding.vk).is_ok();
                    if ok {
                        info!("Hotkey {} registered: mods={:#x} vk={:#x}", binding.name, binding.mods, binding.vk);
                    } else {
                        warn!("Hotkey {} unavailable (mods={:#x} vk={:#x}) — already in use by another app",
                            binding.name, binding.mods, binding.vk);
                    }
                    registered[n] = ok;
                    on_status(binding.name, if ok { "ok" } else { "taken" }, None);
                }

                // Message loop
                let mut msg = MSG::default();
                while running.load(Ordering::SeqCst) {
                    let ret = PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE);
                    if ret.as_bool() {
                        if msg.message == WM_HOTKEY {
                            if let Some(binding) = msg.wParam.0.checked_sub(1).and_then(|n| bindings.get(n)) {
                                (binding.on_fire)();
                            }
                        }
                    } else {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }

                for (n, ok) in registered.iter().enumerate() {
                    if *ok {
                        let _ = UnregisterHotKey(None, n as i32 + 1);
                    }
                }
                info!("Global hotkeys unregistered");
            }
        });
        *listener = Some((flag, thread));
    }

    /// Windows has no desktop setting for an app's keys: Settings sets them.
    pub fn open_desktop_settings(&self) -> Result<(), String> {
        Err("the keys are set in the meter's Settings here".into())
    }

    /// Stop listening and unregister the hotkeys, waiting for the listener
    /// to do so (it checks every 50 ms), so a start right after can register
    /// the same keys again.
    pub fn stop(&self) {
        let listener = self.listener.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some((running, thread)) = listener {
            running.store(false, Ordering::SeqCst);
            let _ = thread.join();
        }
    }
}

/// The hotkeys helper (`a2tools-hotkeys`) is for Linux, where the portal
/// cannot be asked from the meter itself; here it has nothing to do.
pub fn run_helper() -> i32 {
    println!("error the hotkeys helper is only used on Linux");
    2
}
