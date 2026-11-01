/// On Linux the question the meter is really asking is "can we capture?",
/// which needs CAP_NET_RAW (granted with `setcap`, or by running as root).
pub fn is_admin() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .is_ok_and(|status| crate::platform::procfs::can_capture(&status))
}

/// No elevated restart here.
pub fn relaunch_elevated(_args: &[String]) -> Result<bool, String> {
    Err("not on this system".into())
}

pub fn wait_for_exit(_pid: u32, _timeout: std::time::Duration) {}

pub fn wants_admin(_identifier: &str) -> bool {
    false
}
