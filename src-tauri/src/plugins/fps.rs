//! The game's frame rate for plugins (permission `fps`, meter 2.0.78 on).
//!
//! A plugin asks with `a2.watchFps(true)`; while any plugin does, the meter
//! counts the game's frames (`platform::fps`) and tells plugins with `fps` an
//! "fps" event when the reading changes: `{ fps, state }`, state being "ok",
//! "noGame", "needsAdmin" or "unsupported". On Windows counting needs the
//! meter to run as administrator: `a2.fpsAdmin()` asks the player (the
//! meter's own prompt, then Windows') and restarts the meter elevated, and it
//! stays so (`dpsMeter.runAsAdmin`) until turned off in Settings.

use std::collections::HashSet;
use std::sync::Mutex;

use serde_json::Value;
use super::{require, window_plugin};
use crate::platform::FpsReading;

/// The plugins that asked for the frame rate.
static WATCHERS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn watching() -> bool {
    WATCHERS.lock().unwrap_or_else(|p| p.into_inner()).as_ref().is_some_and(|w| !w.is_empty())
}

/// The reading as plugins get it.
pub(super) fn reading_json(reading: FpsReading) -> Value {
    let (fps, state) = match reading {
        FpsReading::Fps(n) => (Some(n), "ok"),
        FpsReading::NoGame => (None, "noGame"),
        FpsReading::NeedsAdmin => (None, "needsAdmin"),
        FpsReading::Unsupported => (None, "unsupported"),
    };
    serde_json::json!({ "fps": fps, "state": state })
}

/// Read now, while any plugin asks; stopped (and None) otherwise. Called
/// from the plugin event loop about once a second.
pub(super) fn poll() -> Option<FpsReading> {
    if watching() {
        Some(crate::platform::fps::game_fps())
    } else {
        crate::platform::fps::stop();
        None
    }
}

/// Start or stop asking for the frame rate. Answers with the reading now.
#[tauri::command]
pub fn plugin_fps_watch(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, on: bool) -> Result<Value, String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "fps")?;
    {
        let mut watchers = WATCHERS.lock().unwrap_or_else(|p| p.into_inner());
        let set = watchers.get_or_insert_with(HashSet::new);
        if on {
            set.insert(id);
        } else {
            set.remove(&id);
        }
    }
    Ok(reading_json(poll().unwrap_or(FpsReading::NoGame)))
}

/// A plugin's window has gone: it asks no more.
pub fn forget_plugin(id: &str) {
    if let Some(set) = WATCHERS.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
        set.remove(id);
    }
}

/// Ask the player to run the meter as administrator for the frame rate, and
/// restart it so. Ok(false) when they or Windows said no.
#[tauri::command]
pub async fn plugin_fps_admin(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>) -> Result<bool, String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "fps")?;
    if crate::platform::admin::is_admin() {
        return Ok(true);
    }
    let title = super::ui_text(&app, &["settings"], "runAsAdminTitle", "A2Tools - Run as administrator");
    let message = super::ui_text(
        &app,
        &["settings"],
        "runAsAdminAsk",
        "Showing the game's frame rate needs the meter to run as administrator.\n\n\
         Restart the meter as administrator now? Windows will ask each time the meter starts. \
         You can turn this off in Settings.",
    );
    let accepted = tokio::task::spawn_blocking(move || crate::platform::dialog::ask_yes_no(&title, &message))
        .await
        .unwrap_or(false);
    if !accepted {
        return Ok(false);
    }
    // Windows' own prompt next; accepted, the elevated meter starts and this
    // one closes (so no answer comes back then).
    let handle = app.clone();
    tokio::task::spawn_blocking(move || crate::app::restart_as_admin(&handle))
        .await
        .map_err(|e| e.to_string())?
}
