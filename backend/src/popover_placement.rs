//! Where the menu-bar popover goes on screen (#175).
//!
//! macOS has a single global coordinate space measured in *points* (logical
//! px). tao and tray-icon each hand us that space converted to "physical" px,
//! but every source multiplies by a *different* scale factor: the tray rect by
//! the clicked display's, each monitor's bounds by its own, and
//! `set_position(Physical…)` divides by the popover's *current* display's. On a
//! mixed-DPI setup (2× Retina laptop + 1× external) those spaces disagree, and
//! the popover landed on the wrong display. So every input is normalized back
//! to points first, and all math here — and the final `set_size`/`set_position`
//! — happens in points, independent of which display the window was last on.
//!
//! Pure (no Tauri types) so the display arrangements that caused the bug can be
//! unit-tested; `main.rs::position_popover` is the thin glue around it.

/// A rectangle in global macOS points (origin top-left of the main display,
/// y growing downward — the space tao reports after its flip).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    fn contains(&self, (px, py): (f64, f64)) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    fn scaled(&self, by: f64) -> Rect {
        Rect { x: self.x / by, y: self.y / by, w: self.w / by, h: self.h / by }
    }
}

/// One display: its bounds in points plus its backing scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Display {
    pub bounds: Rect,
    pub scale: f64,
}

/// The popover's target frame, in points. `size` is `Some` only when the window
/// must shrink to fit the display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub display: usize,
    pub position: (f64, f64),
    pub size: Option<(f64, f64)>,
}

/// Small on-screen margin (points) kept between the popover and the display's
/// edge when there's no tray rect to anchor to.
pub const TOP_RIGHT_MARGIN: f64 = 8.0;

fn display_containing(displays: &[Display], p: (f64, f64)) -> Option<usize> {
    displays.iter().position(|d| d.bounds.contains(p))
}

/// Convert tray-icon's physical tray rect to points and find its display.
///
/// tray-icon multiplied the rect by the clicked display's scale, which we don't
/// know. `cursor` — the pointer in points, sampled when the rect was captured,
/// so it was over the icon — picks the display unambiguously. Without it, fall
/// back to the first display that contains the rect once divided by that
/// display's own scale; that alone can be ambiguous on mixed-DPI setups (a 1×
/// icon at px 3000 ÷ 2 = pt 1500 also lies inside a 2× laptop), hence the cursor.
pub fn tray_to_logical(
    tray_phys: Rect,
    cursor: Option<(f64, f64)>,
    displays: &[Display],
) -> Option<(Rect, usize)> {
    if let Some(i) = cursor.and_then(|c| display_containing(displays, c)) {
        return Some((tray_phys.scaled(displays[i].scale), i));
    }
    displays.iter().enumerate().find_map(|(i, d)| {
        let tray = tray_phys.scaled(d.scale);
        d.bounds.contains(tray.center()).then_some((tray, i))
    })
}

/// The display to use with no tray rect: the one under `fallback_point` (the
/// cursor), else the main display (the one at the origin), else the first.
fn fallback_display(displays: &[Display], fallback_point: Option<(f64, f64)>) -> usize {
    fallback_point
        .and_then(|p| display_containing(displays, p))
        .or_else(|| display_containing(displays, (0.0, 0.0)))
        .unwrap_or(0)
}

/// Compute the popover frame. With a tray rect: centered under the icon and
/// dropped down from the menu bar (always at the top of its display on macOS),
/// shrunk to fit and clamped fully onto the tray's display. Without one: pinned
/// to the fallback display's top-right corner. `None` only if there are no
/// displays at all.
pub fn place(
    tray: Option<(Rect, usize)>,
    win: (f64, f64),
    displays: &[Display],
    fallback_point: Option<(f64, f64)>,
) -> Option<Placement> {
    if displays.is_empty() {
        return None;
    }
    let display = match tray {
        Some((_, i)) if i < displays.len() => i,
        _ => fallback_display(displays, fallback_point),
    };
    let b = displays[display].bounds;

    // Shrink to fit so the popover can never be cropped, whatever its saved size.
    let (w, h) = (win.0.min(b.w), win.1.min(b.h));
    let size = (w != win.0 || h != win.1).then_some((w, h));

    let (x, y) = match tray {
        Some((t, i)) if i == display => (t.x + t.w / 2.0 - w / 2.0, t.y + t.h),
        _ => (b.x + b.w - w - TOP_RIGHT_MARGIN, b.y + TOP_RIGHT_MARGIN),
    };
    // Floored at the top-left so a window as large as the display pins to it.
    let x = x.clamp(b.x, (b.x + b.w - w).max(b.x));
    let y = y.clamp(b.y, (b.y + b.h - h).max(b.y));

    Some(Placement { display, position: (x, y), size })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    fn display(x: f64, y: f64, w: f64, h: f64, scale: f64) -> Display {
        Display { bounds: rect(x, y, w, h), scale }
    }

    /// 2× MacBook (main, at the origin) and a 1× 1920×1080 external.
    const LAPTOP: (f64, f64) = (1512.0, 982.0);
    fn laptop() -> Display {
        display(0.0, 0.0, LAPTOP.0, LAPTOP.1, 2.0)
    }

    /// A 24×24pt tray icon at `(x, y)` points, as tray-icon reports it: in
    /// physical px of its display's `scale`.
    fn tray_phys(x: f64, y: f64, scale: f64) -> Rect {
        rect(x * scale, y * scale, 24.0 * scale, 24.0 * scale)
    }

    const WIN: (f64, f64) = (680.0, 460.0);

    fn place_click(tray_pt: (f64, f64), scale: f64, displays: &[Display]) -> Placement {
        let cursor = (tray_pt.0 + 12.0, tray_pt.1 + 12.0);
        let tray = tray_to_logical(tray_phys(tray_pt.0, tray_pt.1, scale), Some(cursor), displays);
        place(tray, WIN, displays, Some(cursor)).unwrap()
    }

    fn assert_under_icon(p: Placement, tray_pt: (f64, f64), d: &Display) {
        assert_eq!(p.position.1, tray_pt.1 + 24.0, "drops down from the menu bar");
        assert_eq!(p.position.0, tray_pt.0 + 12.0 - WIN.0 / 2.0, "centered under the icon");
        assert!(d.bounds.contains(p.position));
    }

    #[test]
    fn external_right_1x_tray_while_main_is_2x() {
        let displays = [laptop(), display(1512.0, 0.0, 1920.0, 1080.0, 1.0)];
        let tray = (3000.0, 0.0);
        let p = place_click(tray, 1.0, &displays);
        assert_eq!(p.display, 1);
        assert_under_icon(p, tray, &displays[1]);
    }

    #[test]
    fn tray_on_2x_display_with_1x_main() {
        // Main is the 1× external; the 2× laptop sits to its right.
        let displays = [display(0.0, 0.0, 1920.0, 1080.0, 1.0), display(1920.0, 0.0, 1512.0, 982.0, 2.0)];
        let tray = (3000.0, 0.0);
        let p = place_click(tray, 2.0, &displays);
        assert_eq!(p.display, 1);
        assert_under_icon(p, tray, &displays[1]);
    }

    #[test]
    fn external_above_main() {
        let displays = [laptop(), display(-200.0, -1080.0, 1920.0, 1080.0, 1.0)];
        let tray = (1000.0, -1080.0);
        let p = place_click(tray, 1.0, &displays);
        assert_eq!(p.display, 1);
        assert_under_icon(p, tray, &displays[1]);
    }

    #[test]
    fn external_below_main_drops_down_not_up() {
        let displays = [laptop(), display(0.0, 982.0, 1920.0, 1080.0, 1.0)];
        let tray = (1400.0, 982.0);
        let p = place_click(tray, 1.0, &displays);
        assert_eq!(p.display, 1);
        assert_under_icon(p, tray, &displays[1]);
    }

    #[test]
    fn external_left_negative_x() {
        let displays = [laptop(), display(-1920.0, 0.0, 1920.0, 1080.0, 1.0)];
        let tray = (-500.0, 0.0);
        let p = place_click(tray, 1.0, &displays);
        assert_eq!(p.display, 1);
        assert_under_icon(p, tray, &displays[1]);
    }

    #[test]
    fn cursor_disambiguates_overlapping_scaled_rects() {
        // px 3000 on the 1× external ÷ 2 = pt 1500, which is inside the 2× laptop.
        let displays = [laptop(), display(1512.0, 0.0, 1920.0, 1080.0, 1.0)];
        let (tray, i) = tray_to_logical(tray_phys(3000.0, 0.0, 1.0), Some((3010.0, 10.0)), &displays).unwrap();
        assert_eq!(i, 1);
        assert_eq!(tray, rect(3000.0, 0.0, 24.0, 24.0));
    }

    #[test]
    fn without_cursor_falls_back_to_scale_division() {
        let displays = [laptop(), display(1512.0, 0.0, 1920.0, 1080.0, 1.0)];
        let (tray, i) = tray_to_logical(tray_phys(3300.0, 0.0, 1.0), None, &displays).unwrap();
        assert_eq!(i, 1);
        assert_eq!(tray.x, 3300.0);
    }

    #[test]
    fn larger_than_display_shrinks_to_fit() {
        let displays = [laptop()];
        let cursor = (1400.0, 10.0);
        let tray = tray_to_logical(tray_phys(1390.0, 0.0, 2.0), Some(cursor), &displays);
        let p = place(tray, (3000.0, 2000.0), &displays, Some(cursor)).unwrap();
        assert_eq!(p.size, Some(LAPTOP));
        assert_eq!(p.position, (0.0, 0.0));
    }

    #[test]
    fn far_right_tray_clamps_on_screen() {
        let displays = [laptop()];
        let p = place_click((1490.0, 0.0), 2.0, &displays);
        assert_eq!(p.size, None);
        assert_eq!(p.position, (LAPTOP.0 - WIN.0, 24.0));
    }

    #[test]
    fn no_tray_uses_cursor_display_top_right() {
        let displays = [laptop(), display(1512.0, 0.0, 1920.0, 1080.0, 1.0)];
        let p = place(None, WIN, &displays, Some((2000.0, 500.0))).unwrap();
        assert_eq!(p.display, 1);
        assert_eq!(p.position, (1512.0 + 1920.0 - WIN.0 - TOP_RIGHT_MARGIN, TOP_RIGHT_MARGIN));
    }

    #[test]
    fn no_tray_no_cursor_uses_main_display() {
        let displays = [display(1512.0, 0.0, 1920.0, 1080.0, 1.0), laptop()];
        let p = place(None, WIN, &displays, None).unwrap();
        assert_eq!(p.display, 1);
        assert_eq!(p.position, (LAPTOP.0 - WIN.0 - TOP_RIGHT_MARGIN, TOP_RIGHT_MARGIN));
    }

    #[test]
    fn no_displays_places_nothing() {
        assert_eq!(place(None, WIN, &[], None), None);
    }
}
