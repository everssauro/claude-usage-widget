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

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

/// Show or hide the widget, always at the position the user left it in. There
/// is deliberately no "open under the icon" mode: it fought the position
/// watcher, and the widget's whole model is that it stays where you put it.
pub fn toggle_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }
    let _ = window.show();
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
                ..
            } = event
            {
                toggle_window(tray.app_handle());
            }
        })
        .build(app)?;
    TRAY_ALIVE.store(true, Ordering::Relaxed);
    hide_dock_icon(app);
    Ok(())
}
