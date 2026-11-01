/// No elevation concept to report.
pub fn is_admin() -> bool {
    false
}

/// No elevated restart here.
pub fn relaunch_elevated(_args: &[String]) -> Result<bool, String> {
    Err("not on this system".into())
}

pub fn wait_for_exit(_pid: u32, _timeout: std::time::Duration) {}

pub fn wants_admin(_identifier: &str) -> bool {
    false
}
