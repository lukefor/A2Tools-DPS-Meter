//! Asking the desktop to keep the meter above a fullscreen game. Nothing to
//! ask here: Windows keeps a topmost window above a borderless game itself.

/// See the Linux implementation. Always done.
pub fn keep_above_fullscreen(_done: bool) -> Result<bool, String> {
    Ok(true)
}

/// See the Linux implementation. Always done.
pub fn click_without_focus(_done: bool) -> Result<bool, String> {
    Ok(true)
}

/// See the Linux implementation. Always done.
pub fn icons_without_focus(_done: bool) -> Result<bool, String> {
    Ok(true)
}

/// See the Linux implementation. Always done.
pub fn plugin_windows_above(_done: bool) -> Result<bool, String> {
    Ok(true)
}

/// See the Linux implementation. Nothing to keep.
pub fn keep_rules() {}
