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
use std::sync::Mutex;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, LogicalPosition, Manager, Runtime,
};

/// Where the widget appears when opened from the menu bar.
///  * `false` — wherever the user last dragged it (the widget's own memory).
///  * `true`  — pinned under the menu-bar icon, like a menu-bar app's popover.
pub struct TrayPrefs {
    pub anchored: bool,
    pub hide_dock: bool,
}
pub struct TrayState(pub Mutex<TrayPrefs>);

/// Whether a menu-bar icon actually exists. Closing the window hides it instead
/// of quitting — the market-standard behaviour — but ONLY when this is true. If
/// the tray failed to build, hiding would leave an app with no Dock tile, no
/// window and no icon: unreachable except through Force Quit.
static TRAY_ALIVE: AtomicBool = AtomicBool::new(false);
/// Set just before a deliberate exit, so the close handler stops intercepting.
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

const PREFS_FILE: &str = "tray.json";

fn prefs_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    Some(app.path().app_config_dir().ok()?.join(PREFS_FILE))
}

pub fn load_prefs(app: &AppHandle) -> TrayPrefs {
    let v: Option<serde_json::Value> = prefs_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok());
    TrayPrefs {
        anchored: v
            .as_ref()
            .and_then(|v| v.get("anchored"))
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        // Never defaults on: with no Dock icon and no menu-bar icon (the bar can
        // silently hide items on a notched display) the app would have no way in.
        hide_dock: v
            .as_ref()
            .and_then(|v| v.get("hide_dock"))
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
    }
}

fn save_prefs(app: &AppHandle, p: &TrayPrefs) {
    if let Some(path) = prefs_path(app) {
        let _ = std::fs::write(
            path,
            serde_json::json!({ "anchored": p.anchored, "hide_dock": p.hide_dock }).to_string(),
        );
    }
}

/// Place the widget just under the menu-bar icon, horizontally centred on it.
/// `rect` comes from the tray click event in PHYSICAL pixels; window positioning
/// is logical, so it has to be divided by the scale factor — mixing the two is
/// what once put this window off-screen at x=4528.
fn anchor_under_icon<R: Runtime>(window: &tauri::WebviewWindow<R>, rect: tauri::Rect) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let pos = rect.position.to_logical::<f64>(scale);
    let size = rect.size.to_logical::<f64>(scale);
    let win_w = window
        .outer_size()
        .ok()
        .filter(|s| s.width > 0)
        .map(|s| s.width as f64 / scale)
        .unwrap_or(280.0);
    let x = pos.x + size.width / 2.0 - win_w / 2.0;
    let y = pos.y + size.height + 6.0;
    let _ = window.set_position(LogicalPosition::new(x.max(8.0), y));
}

pub fn toggle_window(app: &AppHandle, rect: Option<tauri::Rect>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }
    let anchored = app
        .state::<TrayState>()
        .0
        .lock()
        .map(|p| p.anchored)
        .unwrap_or(false);
    if anchored {
        if let Some(r) = rect {
            anchor_under_icon(&window, r);
            // Tell the position watcher this IS the intended spot, or it would
            // drag the window back to the last hand-placed position within a
            // second of it appearing.
            crate::adopt_current_position(app);
        }
    }
    let _ = window.show();
    crate::reassert_pip_if_drifted(app);
}

/// Dock icon on/off. Accessory = no Dock tile and no cmd-tab entry.
#[cfg(target_os = "macos")]
fn apply_dock_visibility(app: &AppHandle, hide: bool) {
    let policy = if hide {
        tauri::ActivationPolicy::Accessory
    } else {
        tauri::ActivationPolicy::Regular
    };
    let _ = app.set_activation_policy(policy);
}
#[cfg(not(target_os = "macos"))]
fn apply_dock_visibility(_app: &AppHandle, _hide: bool) {}

#[tauri::command]
pub fn set_tray_anchored(app: AppHandle, anchored: bool) {
    if let Ok(mut p) = app.state::<TrayState>().0.lock() {
        p.anchored = anchored;
        save_prefs(&app, &p);
    }
}

#[tauri::command]
pub fn set_hide_dock(app: AppHandle, hide: bool) {
    if let Ok(mut p) = app.state::<TrayState>().0.lock() {
        p.hide_dock = hide;
        save_prefs(&app, &p);
    }
    apply_dock_visibility(&app, hide);
}

#[tauri::command]
pub fn tray_prefs(app: AppHandle) -> serde_json::Value {
    let p = app.state::<TrayState>();
    let p = p.0.lock().unwrap();
    serde_json::json!({ "anchored": p.anchored, "hide_dock": p.hide_dock })
}

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
            "show" => toggle_window(app, None),
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
                toggle_window(tray.app_handle(), Some(rect));
            }
        })
        .build(app)?;
    TRAY_ALIVE.store(true, Ordering::Relaxed);
    Ok(())
}
