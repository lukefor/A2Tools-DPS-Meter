//! "Meter stays fixed at": where the overlay goes as it auto-resizes, so its
//! top, centre or bottom edge stays put. The maths is OS-neutral; moving the
//! window is `window::set_size_and_position`, in one step where the OS allows.
//! Only where the meter places its own windows (`window::places_own_windows`);
//! on KDE native Wayland the KWin script keeps the anchor (`linux/focus.rs`).

use super::window;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Top,
    Middle,
    Bottom,
}

impl Anchor {
    /// The saved setting, or what this platform did before the setting
    /// existed when it is unset: the top edge stays where the meter places its
    /// own windows, the centre on KDE Wayland, where KWin keeps it.
    pub fn from_setting(value: Option<&str>) -> Anchor {
        match value.map(str::trim) {
            Some("top") => Anchor::Top,
            Some("middle") => Anchor::Middle,
            Some("bottom") => Anchor::Bottom,
            _ => Anchor::platform_default(),
        }
    }

    pub fn platform_default() -> Anchor {
        if window::places_own_windows() { Anchor::Top } else { Anchor::Middle }
    }

    pub fn name(self) -> &'static str {
        match self {
            Anchor::Top => "top",
            Anchor::Middle => "middle",
            Anchor::Bottom => "bottom",
        }
    }
}

/// A size in physical pixels, a logical one at the window's scale.
pub fn physical_size(window: &tauri::WebviewWindow, size: &tauri::Size) -> Option<tauri::PhysicalSize<u32>> {
    match size {
        tauri::Size::Physical(s) => Some(*s),
        tauri::Size::Logical(s) => {
            let scale = window.scale_factor().ok()?;
            let width = (s.width * scale).round();
            let height = (s.height * scale).round();
            if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
                return None;
            }
            Some(tauri::PhysicalSize { width: width as u32, height: height as u32 })
        }
    }
}

/// How much of the window must stay above a work area's bottom to grab the
/// header: as much as the KWin script asks of a window it puts back
/// (`onScreen` in `linux/focus.rs`).
const GRAB: i32 = 32;

/// A work area: x, y, width, height in physical pixels.
type Area = (i32, i32, i32, i32);

/// The new top of a window at `x`, `width` wide, `old_height` tall at
/// `old_y`, resized to `new_height` with `anchor` kept. Only the move the
/// anchor makes is limited, and only so far that the top edge stays on one of
/// `areas` where it can be grabbed; a window parked partly off screen or
/// across monitors stays there.
fn anchored_y(anchor: Anchor, x: i32, width: i32, old_y: i32, old_height: i32, new_height: i32, areas: &[Area]) -> i32 {
    let want = match anchor {
        Anchor::Top => return old_y,
        Anchor::Middle => old_y + (old_height - new_height) / 2,
        Anchor::Bottom => old_y + (old_height - new_height),
    };
    if want == old_y {
        return old_y;
    }
    let (lo, hi) = (want.min(old_y), want.max(old_y));
    let mut best: Option<i32> = None;
    for &(ax, ay, aw, ah) in areas {
        if x + width <= ax || x >= ax + aw {
            continue;
        }
        // Where the top edge can be on this area, and still between the old
        // place and the anchored one.
        let (top, bottom) = (ay.max(lo), (ay + ah - GRAB).min(hi));
        if top > bottom {
            continue;
        }
        let y = want.clamp(top, bottom);
        if best.is_none_or(|b| (y - want).abs() < (b - want).abs()) {
            best = Some(y);
        }
    }
    // No area within reach: no move at all rather than further off screen.
    best.unwrap_or(old_y)
}

/// Resize the meter to `size`, keeping `anchor`, from the height it last had
/// when anchored. With none (`last_height` None: no `window.h` saved yet, or
/// the player just sized the meter), it keeps its top, and its new height is
/// the one the edge is kept from. The height it is anchored at now, or None
/// when it was only resized (minimized, its place or size unknown).
pub fn resize(window: &tauri::WebviewWindow, size: tauri::Size, anchor: Anchor, last_height: Option<i32>) -> Option<i32> {
    let old_pos = window.outer_position().ok();
    let new_size = physical_size(window, &size);
    // A minimized window's place is not its place (-32000 on Windows).
    let parked = window.is_minimized().unwrap_or(false) || old_pos.is_some_and(|p| p.x <= -10000 || p.y <= -10000);
    let (Some(old_pos), Some(new_size)) = (old_pos, new_size) else {
        window::set_size(window, size);
        return None;
    };
    if parked {
        window::set_size(window, size);
        return None;
    }
    let new_height = new_size.height as i32;
    let old_height = last_height.filter(|h| *h > 0).unwrap_or(new_height);
    let areas: Vec<Area> = window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let a = m.work_area();
            (a.position.x, a.position.y, a.size.width as i32, a.size.height as i32)
        })
        .collect();
    // At its new width: the old one can be a tooltip's, on its way out.
    let y = anchored_y(anchor, old_pos.x, new_size.width as i32, old_pos.y, old_height, new_height, &areas);
    if y == old_pos.y {
        window::set_size(window, size);
    } else {
        window::set_size_and_position(window, size, tauri::PhysicalPosition { x: old_pos.x, y });
    }
    Some(new_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Area = (0, 0, 1920, 1040);

    #[test]
    fn top_never_moves() {
        assert_eq!(anchored_y(Anchor::Top, 100, 400, 500, 300, 600, &[SCREEN]), 500);
        assert_eq!(anchored_y(Anchor::Top, 100, 400, -200, 300, 600, &[SCREEN]), -200);
    }

    #[test]
    fn bottom_and_middle_follow_the_height() {
        assert_eq!(anchored_y(Anchor::Bottom, 100, 400, 500, 300, 400, &[SCREEN]), 400);
        assert_eq!(anchored_y(Anchor::Bottom, 100, 400, 500, 300, 200, &[SCREEN]), 600);
        assert_eq!(anchored_y(Anchor::Middle, 100, 400, 500, 300, 400, &[SCREEN]), 450);
    }

    #[test]
    fn growth_stops_at_the_top_of_the_work_area() {
        assert_eq!(anchored_y(Anchor::Bottom, 100, 400, 50, 300, 500, &[SCREEN]), 0);
        assert_eq!(anchored_y(Anchor::Bottom, 100, 400, 50, 300, 500, &[(0, 40, 1920, 1000)]), 40);
    }

    #[test]
    fn a_window_parked_above_the_screen_is_not_pulled_down_or_pushed_further() {
        assert_eq!(anchored_y(Anchor::Bottom, 100, 400, -100, 300, 500, &[SCREEN]), -100);
        // Shrinking brings it back toward the screen, as the anchor says.
        assert_eq!(anchored_y(Anchor::Bottom, 100, 400, -100, 300, 100, &[SCREEN]), 100);
    }

    #[test]
    fn x_and_other_monitors_are_respected() {
        let left = (-1920, -300, 1920, 1380);
        // Across two monitors: the left one has room above, the right none.
        assert_eq!(anchored_y(Anchor::Bottom, -200, 400, 50, 300, 500, &[left, (0, 200, 1920, 840)]), -150);
        assert_eq!(anchored_y(Anchor::Bottom, -200, 400, 50, 300, 500, &[(-1920, 0, 1920, 1080), SCREEN]), 0);
        // On a monitor higher up, beside the primary.
        assert_eq!(anchored_y(Anchor::Bottom, 1950, 400, 50, 300, 500, &[SCREEN, (1920, -500, 1920, 1080)]), -150);
    }

    #[test]
    fn shrinking_keeps_the_header_on_screen() {
        // Bottom edge parked below the screen: the top stops where it can be grabbed.
        assert_eq!(anchored_y(Anchor::Bottom, 100, 400, 900, 400, 50, &[SCREEN]), 1040 - GRAB);
    }

    #[test]
    fn the_setting_defaults_to_the_platform() {
        assert_eq!(Anchor::from_setting(Some("bottom")), Anchor::Bottom);
        assert_eq!(Anchor::from_setting(Some("middle")), Anchor::Middle);
        assert_eq!(Anchor::from_setting(None), Anchor::platform_default());
        assert_eq!(Anchor::from_setting(Some("")), Anchor::platform_default());
    }
}
