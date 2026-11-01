/// Check if the current process is running with administrator privileges.
pub fn is_admin() -> bool {
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0u32;
        let result = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        );

        let _ = windows::Win32::Foundation::CloseHandle(token);

        result.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// Start this program again as administrator (Windows asks first), with
/// `args`. Ok(false) when the prompt was refused.
pub fn relaunch_elevated(args: &[String]) -> Result<bool, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // Each argument quoted; the meter's own are plain words and numbers.
    let line = args.iter().map(|a| format!("\"{}\"", a.replace('"', ""))).collect::<Vec<_>>().join(" ");
    let params: Vec<u16> = line.encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        // A debug build is a console program: started this way it gets a
        // console window of its own, and closing it closed the meter. Hidden.
        // A release build has none; its windows show themselves.
        nShow: if cfg!(debug_assertions) { SW_HIDE.0 } else { SW_SHOWNORMAL.0 },
        ..Default::default()
    };
    // SAFETY: `info` and the strings it points to live across the call.
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => Ok(true),
        Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

/// Wait, up to `timeout`, for process `pid` to end: the meter that started
/// this one elevated, still closing (it holds the one-meter lock until then).
pub fn wait_for_exit(pid: u32, timeout: std::time::Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    // SAFETY: a handle opened here and closed once.
    unsafe {
        if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(handle, timeout.as_millis().min(u32::MAX as u128) as u32);
            let _ = CloseHandle(handle);
        }
    }
}

/// Whether the player chose to run the meter as administrator
/// (`dpsMeter.runAsAdmin`, for the game's frame rate), read from the settings
/// file before anything starts.
pub fn wants_admin(identifier: &str) -> bool {
    let Some(dir) = std::env::var_os("APPDATA") else { return false };
    let path = std::path::Path::new(&dir).join(identifier).join("settings.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .is_some_and(|v| v.get("dpsMeter.runAsAdmin").and_then(|x| x.as_str()) == Some("true"))
}
