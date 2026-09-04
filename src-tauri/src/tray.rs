//! Menu-bar (system tray) presence.
//!
//! The widget stays a real desktop window — draggable, position-remembering,
//! PiP-capable. The tray is an ADDITIONAL door to it, not a replacement: a
//! menu-bar-only rewrite would have thrown away the drag, the multi-monitor
//! restore, the PiP level and the vibrancy, which are the expensive parts.
//!
//! Left click toggles the window. Right click opens a menu. Where the window
//! appears is the user's choice (`anchored` below), and so is whether the app
//! keeps a Dock icon.


use std::sync::atomic::{AtomicBool, Ordering};

/// Whether a menu-bar icon actually exists. Two behaviours hang off this:
/// closing hides instead of quitting, and the Dock tile is dropped. Both would
/// strand the app if the tray had failed — no window, no tile, no icon, and
/// nothing but Force Quit left.
static TRAY_ALIVE: AtomicBool = AtomicBool::new(false);
/// Set just before a deliberate exit, so the close handler stops intercepting —
/// without it the menu's own Quit would be swallowed and the app never exit.
static QUITTING: AtomicBool = AtomicBool::new(false);

pub fn tray_alive() -> bool {
    TRAY_ALIVE.load(Ordering::Relaxed)
}
pub fn is_quitting() -> bool {
    QUITTING.load(Ordering::Relaxed)
}
pub fn begin_quit() {
    QUITTING.store(true, Ordering::Relaxed);
}

use std::sync::Mutex;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, LogicalPosition, Manager,
};

/// Last known screen rect of the menu-bar icon, in physical pixels, so the
/// menu's own "Show / Hide" lands in the same place a click on the icon does.
static LAST_ICON_RECT: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);

/// Put the widget just below the menu-bar icon, clamped to that icon's screen.
///
/// The clamp is the part that matters: the icon can sit at the far right of a
/// wide display, or on a secondary monitor with negative coordinates, and a
/// naive centre-under-the-icon puts half the window past the edge — or fully
/// off-screen, where it can't be dragged back.
fn place_under_icon(window: &tauri::WebviewWindow, icon: (f64, f64, f64, f64)) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let (ix, iy, iw, ih) = icon;
    let (ix, iy, iw, ih) = (ix / scale, iy / scale, iw / scale, ih / scale);

    let (win_w, win_h) = window
        .outer_size()
        .map(|s| (s.width as f64 / scale, s.height as f64 / scale))
        .unwrap_or((280.0, 380.0));

    let mut x = ix + iw / 2.0 - win_w / 2.0;
    let mut y = iy + ih + 6.0;

    // Keep it on the screen the icon belongs to.
    if let Ok(monitors) = window.available_monitors() {
        let centre = ix + iw / 2.0;
        let screen = monitors.iter().find(|m| {
            let ms = m.scale_factor();
            let o = m.position().to_logical::<f64>(ms);
            let z = m.size().to_logical::<f64>(ms);
            centre >= o.x && centre <= o.x + z.width
        });
        if let Some(m) = screen {
            let ms = m.scale_factor();
            let o = m.position().to_logical::<f64>(ms);
            let z = m.size().to_logical::<f64>(ms);
            const MARGIN: f64 = 8.0;
            x = x.clamp(o.x + MARGIN, (o.x + z.width - win_w - MARGIN).max(o.x + MARGIN));
            y = y.min((o.y + z.height - win_h - MARGIN).max(o.y + MARGIN));
        }
    }
    let _ = window.set_position(LogicalPosition::new(x, y));
}

/// Clicking the icon always brings the widget to the Space and screen you are
/// looking at, directly under the icon. Leaving it open and dragging it
/// elsewhere still works — the position only moves when you ask for it through
/// the icon.
///
/// The Space half is not a position problem: `set_position` cannot pull a window
/// across Spaces. That is handled by the MoveToActiveSpace collection behaviour
/// (see `desired_pip`), which applies when the window is ordered to the front.
pub fn toggle_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }
    let icon = *LAST_ICON_RECT.lock().unwrap();
    if let Some(rect) = icon {
        place_under_icon(&window, rect);
    }
    let _ = window.show();
    let _ = window.set_focus();
    // The move was deliberate: stop the position watcher from undoing it.
    crate::adopt_current_position(app);
    crate::reassert_pip_if_drifted(app);
}

/// Drop the Dock tile once the menu-bar icon exists — the icon replaces it.
/// Guarded on the tray actually having been created: with no Dock tile, no
/// window and no icon, the app would be unreachable except via Force Quit.
#[cfg(target_os = "macos")]
fn hide_dock_icon(app: &AppHandle) {
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
}
#[cfg(not(target_os = "macos"))]
fn hide_dock_icon(_app: &AppHandle) {}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show / Hide Widget", true, None::<&str>)?;
    let pin = MenuItem::with_id(app, "pin", "Keep on Top (PiP)", true, None::<&str>)?;
    let sessions = MenuItem::with_id(app, "sessions", "Session Breakdown…", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &pin, &sessions, &settings, &sep, &quit])?;

    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-template.png"))?;

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        // Template = black+alpha art the system recolours for light/dark menu
        // bars and for the highlighted state. Without this the icon is a black
        // smudge on a dark menu bar.
        .icon_as_template(true)
        .tooltip("Claude Usage")
        .menu(&menu)
        // The menu must NOT open on left click — left click is the toggle.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => toggle_window(app),
            "pin" => crate::toggle_pin_from_tray(app),
            "sessions" => crate::open_sessions_from_tray(app),
            "settings" => crate::show_settings_from_tray(app),
            "quit" => {
                begin_quit();
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                // On macOS this Rect is already physical (tray-icon builds it
                // with the status window's backingScaleFactor). Converting with
                // the real scale is right either way: a physical value passes
                // through untouched, a logical one is scaled correctly.
                let scale = tray
                    .app_handle()
                    .get_webview_window("main")
                    .and_then(|w| w.scale_factor().ok())
                    .unwrap_or(1.0);
                let p = rect.position.to_physical::<f64>(scale);
                let sz = rect.size.to_physical::<f64>(scale);
                *LAST_ICON_RECT.lock().unwrap() = Some((p.x, p.y, sz.width, sz.height));
                toggle_window(tray.app_handle());
            }
        })
        .build(app)?;
    TRAY_ALIVE.store(true, Ordering::Relaxed);
    hide_dock_icon(app);
    Ok(())
}
