//! The game's frame rate, from Windows' own record of the frames it presents:
//! the DXGI provider's Present events (as PresentMon and the Game Bar read
//! them), counted for AION2's process. Nothing touches the game: Windows
//! writes these events for every program, and a trace session only listens.
//!
//! Starting a trace session needs administrator rights (or the Performance
//! Log Users group), so this works only while the meter runs elevated. The
//! session runs while something asks (`game_fps`) and the game is open, and
//! stops otherwise. A session outlives the process that started it, so one
//! left by a meter that crashed is stopped by name before a new one starts.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use windows::core::{GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS};
use windows::Win32::System::Diagnostics::Etw::{
    CloseTrace, ControlTraceW, EnableTraceEx2, OpenTraceW, ProcessTrace, StartTraceW, CONTROLTRACE_HANDLE,
    EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW,
    EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, PROCESSTRACE_HANDLE, PROCESS_TRACE_MODE_EVENT_RECORD,
    PROCESS_TRACE_MODE_REAL_TIME, TRACE_LEVEL_INFORMATION, WNODE_FLAG_TRACED_GUID,
};

use crate::platform::FpsReading;

const SESSION: &str = "A2Tools-FPS";
/// Microsoft-Windows-DXGI.
const DXGI: GUID = GUID::from_u128(0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9);
/// Present_Start and PresentMultiplaneOverlay_Start: one per frame handed to
/// Windows, by the process that presents it.
const PRESENT_EVENTS: [u16; 2] = [42, 55];
/// IDXGISwapChain_Present's start, also one per frame. AION2 (DirectX 12)
/// sends only this one on Windows 11: none of the above came, from it or from
/// any other program, so the FPS stayed empty (2026-10-29). Counted apart and
/// used only while the above don't come, so no frame counts twice.
const SWAPCHAIN_PRESENT: u16 = 178;
/// Event times are FILETIME, in 100 ns.
const SECOND: i64 = 10_000_000;
/// The session hands events over in batches, about every second: the rate is
/// taken over the frames of the last two seconds.
const WINDOW: i64 = 2 * SECOND;
/// No frame for this long: the game stopped presenting (minimised, say).
const STALE: i64 = 3 * SECOND;

/// The process whose frames count (0: none).
static TARGET: AtomicU32 = AtomicU32::new(0);
/// The target's frame times, oldest first, the last few seconds.
static FRAMES: Mutex<VecDeque<i64>> = Mutex::new(VecDeque::new());
/// The same from IDXGISwapChain_Present (SWAPCHAIN_PRESENT).
static SWAPCHAIN_FRAMES: Mutex<VecDeque<i64>> = Mutex::new(VecDeque::new());
/// Whether a session of ours runs, and its consuming thread with it.
static RUNNING: Mutex<bool> = Mutex::new(false);
/// Which start the running session is: a thread that ends late (its session
/// stopped, a new one started meanwhile) mustn't mark the new one stopped.
static GENERATION: AtomicU32 = AtomicU32::new(0);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// EVENT_TRACE_PROPERTIES with room for the session name after it, in a
/// buffer aligned for it.
fn properties() -> Vec<u64> {
    let base = std::mem::size_of::<EVENT_TRACE_PROPERTIES>();
    let bytes = base + 2 * (SESSION.len() + 1) + 64;
    let mut buf = vec![0u64; bytes.div_ceil(8)];
    // SAFETY: the buffer is zeroed, 8-byte aligned and larger than the struct.
    let props = unsafe { &mut *(buf.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES) };
    props.Wnode.BufferSize = bytes as u32;
    props.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
    // Timestamps from the performance counter, given as FILETIME.
    props.Wnode.ClientContext = 1;
    props.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
    props.FlushTimer = 1;
    props.LoggerNameOffset = base as u32;
    buf
}

fn stop_session() {
    let name = wide(SESSION);
    let mut props = properties();
    // SAFETY: a properties buffer as ControlTraceW wants it; the name lives
    // across the call. Stopping one that isn't running only fails.
    unsafe {
        let _ = ControlTraceW(
            CONTROLTRACE_HANDLE::default(),
            PCWSTR(name.as_ptr()),
            props.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES,
            EVENT_TRACE_CONTROL_STOP,
        );
    }
}

unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    // SAFETY: ETW passes a valid record for the duration of the callback.
    let header = unsafe { &(*record).EventHeader };
    let id = header.EventDescriptor.Id;
    if header.ProviderId != DXGI || !(PRESENT_EVENTS.contains(&id) || id == SWAPCHAIN_PRESENT) {
        return;
    }
    let target = TARGET.load(Ordering::Relaxed);
    if target == 0 || header.ProcessId != target {
        return;
    }
    let queue = if id == SWAPCHAIN_PRESENT { &SWAPCHAIN_FRAMES } else { &FRAMES };
    let mut frames = queue.lock().unwrap_or_else(|p| p.into_inner());
    frames.push_back(header.TimeStamp);
    while frames.front().is_some_and(|&t| header.TimeStamp - t > STALE) {
        frames.pop_front();
    }
}

/// Start the session and its consuming thread. Err(true) when Windows says
/// the meter may not (not elevated).
fn start() -> Result<(), bool> {
    stop_session();
    let name = wide(SESSION);
    let mut props = properties();
    let mut handle = CONTROLTRACE_HANDLE::default();
    // SAFETY: as in stop_session.
    let started = unsafe {
        StartTraceW(&mut handle, PCWSTR(name.as_ptr()), props.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES)
    };
    if started != ERROR_SUCCESS {
        tracing::info!("FPS: could not start the frame trace ({})", started.0);
        return Err(started == ERROR_ACCESS_DENIED);
    }
    // SAFETY: a session handle just started; the GUID outlives the call.
    let enabled = unsafe {
        EnableTraceEx2(handle, &DXGI, EVENT_CONTROL_CODE_ENABLE_PROVIDER.0, TRACE_LEVEL_INFORMATION as u8, 0, 0, 0, None)
    };
    if enabled != ERROR_SUCCESS {
        tracing::info!("FPS: could not listen to DXGI ({})", enabled.0);
        stop_session();
        return Err(enabled == ERROR_ACCESS_DENIED);
    }
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    std::thread::Builder::new()
        .name("fps-trace".into())
        .spawn(move || {
            let mut name = wide(SESSION);
            let mut logfile = EVENT_TRACE_LOGFILEW { LoggerName: PWSTR(name.as_mut_ptr()), ..Default::default() };
            logfile.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
            logfile.Anonymous2.EventRecordCallback = Some(on_event);
            // SAFETY: the logfile and the name it points to live until
            // ProcessTrace returns, which it does when the session stops.
            unsafe {
                let trace: PROCESSTRACE_HANDLE = OpenTraceW(&mut logfile);
                if trace.Value == u64::MAX {
                    tracing::info!("FPS: could not open the frame trace");
                } else {
                    let _ = ProcessTrace(&[trace], None, None);
                    let _ = CloseTrace(trace);
                }
            }
            if GENERATION.load(Ordering::Relaxed) == generation {
                *RUNNING.lock().unwrap_or_else(|p| p.into_inner()) = false;
            }
            tracing::debug!("FPS: frame trace ended");
        })
        .map_err(|_| {
            stop_session();
            false
        })?;
    tracing::info!("FPS: counting the game's frames");
    Ok(())
}

/// Stop counting: nothing asks for the frame rate, or the game closed.
pub fn stop() {
    let mut running = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
    if *running {
        stop_session();
        *running = false;
    }
    TARGET.store(0, Ordering::Relaxed);
    FRAMES.lock().unwrap_or_else(|p| p.into_inner()).clear();
    SWAPCHAIN_FRAMES.lock().unwrap_or_else(|p| p.into_inner()).clear();
}

fn now_filetime() -> i64 {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64 / 100)
        .unwrap_or(0);
    unix + 11_644_473_600 * SECOND
}

/// The game's frames per second now, starting to count when not yet.
pub fn game_fps() -> FpsReading {
    let Some(pid) = super::window_detector::game_pid() else {
        stop();
        return FpsReading::NoGame;
    };
    if !super::admin::is_admin() {
        return FpsReading::NeedsAdmin;
    }
    if TARGET.swap(pid, Ordering::Relaxed) != pid {
        FRAMES.lock().unwrap_or_else(|p| p.into_inner()).clear();
        SWAPCHAIN_FRAMES.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }
    {
        let mut running = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
        if !*running {
            match start() {
                Ok(()) => *running = true,
                Err(true) => return FpsReading::NeedsAdmin,
                Err(false) => return FpsReading::Unsupported,
            }
        }
    }
    let now = now_filetime();
    match rate(&FRAMES.lock().unwrap_or_else(|p| p.into_inner()), now) {
        FpsReading::Fps(0) => rate(&SWAPCHAIN_FRAMES.lock().unwrap_or_else(|p| p.into_inner()), now),
        reading => reading,
    }
}

/// Frames per second over the frames of the last WINDOW before the newest;
/// 0 when the newest is older than STALE (or none came yet).
fn rate(frames: &VecDeque<i64>, now: i64) -> FpsReading {
    let Some(&newest) = frames.back() else { return FpsReading::Fps(0) };
    if now - newest > STALE {
        return FpsReading::Fps(0);
    }
    let recent: Vec<i64> = frames.iter().copied().filter(|&t| newest - t <= WINDOW).collect();
    if recent.len() < 2 {
        return FpsReading::Fps(0);
    }
    let span = (newest - recent[0]) as f64 / SECOND as f64;
    FpsReading::Fps(((recent.len() - 1) as f64 / span).round() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_is_frames_over_their_span_and_zero_once_they_stop() {
        let at = 1_000 * SECOND;
        // 60 frames a second for 3 s.
        let frames: VecDeque<i64> = (0..=180).map(|i| at + i * SECOND / 60).collect();
        let newest = *frames.back().unwrap();
        assert_eq!(rate(&frames, newest + SECOND / 2), FpsReading::Fps(60));
        assert_eq!(rate(&frames, newest + 4 * SECOND), FpsReading::Fps(0), "the game stopped presenting");
        assert_eq!(rate(&VecDeque::new(), at), FpsReading::Fps(0));
    }
}
