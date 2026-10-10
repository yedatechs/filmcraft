//! Bring the window forward for the control channel without taking keyboard focus.
//!
//! Occluded macOS windows stop running egui's `ui` pass, so UI-level control requests (synthetic
//! input, screenshots) need the window on screen. Activating the app for that
//! (`ViewportCommand::Focus`) steals keyboard focus from whatever the user is typing in, and their
//! keystrokes then land in FilmCraft as shortcuts. `orderFrontRegardless` orders the window to the
//! front without activating the app, so the key window (and the user's focus) stays where it is.

/// Order the app's windows to the front without activating the app (macOS). Returns whether it
/// did anything; elsewhere the caller only requests a repaint.
pub fn raise_without_focus() -> bool {
    #[cfg(target_os = "macos")]
    {
        let Some(mtm) = objc2::MainThreadMarker::new() else { return false };
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        let windows = app.windows();
        for w in windows.iter() {
            if w.isVisible() || w.isMiniaturized() {
                w.orderFrontRegardless();
            }
        }
        true
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Put FilmCraft's recording border window (Window ▸ Record; its title starts with `title`) over
/// `frame` (points, desktop coordinates from the top left of the main display) above the menu bar:
/// macOS keeps ordinary windows below the menu bar, which would leave the top of the display
/// unframed. Returns whether a window was placed (macOS).
pub fn place_overlay(title: &str, frame: [f64; 4]) -> bool {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let Some(mtm) = objc2::MainThreadMarker::new() else { return false };
        // AppKit measures from the bottom left of the main (menu bar) display
        let Some(main) = objc2_app_kit::NSScreen::screens(mtm).firstObject() else { return false };
        let main_h = main.frame().size.height;
        let [x, y, w, h] = frame;
        if ![x, y, w, h, main_h].iter().all(|v| v.is_finite()) || w < 1.0 || h < 1.0 {
            return false;
        }
        let rect = NSRect::new(NSPoint::new(x, main_h - y - h), NSSize::new(w, h));
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        let mut placed = false;
        for win in app.windows().iter() {
            if win.title().to_string().starts_with(title) {
                win.setLevel(objc2_app_kit::NSStatusWindowLevel + 1);
                win.setFrame_display(rect, true);
                placed = true;
            }
        }
        placed
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (title, frame);
        false
    }
}
