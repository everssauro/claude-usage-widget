mod usage;

use std::path::PathBuf;

use tauri::{AppHandle, LogicalPosition, Manager, WindowEvent};

/// Compute the top-right position of the macOS main display in **logical**
/// points — `set_position` expects logical units, and mixing in physical pixels
/// puts the window off-screen on Retina (the bug that hid it at x=4528).
/// `primary_monitor()` returns `None` intermittently during `setup()`, so we
/// prefer the `available_monitors()` entry at origin (0,0) = the main display.
fn top_right_pos(window: &tauri::WebviewWindow) -> Option<LogicalPosition<f64>> {
    let win_w = 280.0;
    let margin = 16.0;

    let place = |m: &tauri::Monitor| {
        // Monitor geometry is physical; convert to logical points via the scale.
        let scale = m.scale_factor();
        let pos = m.position();
        let size = m.size();
        let mx = pos.x as f64 / scale;
        let my = pos.y as f64 / scale;
        let mw = size.width as f64 / scale;
        LogicalPosition::new(mx + mw - win_w - margin, my + margin)
    };

    if let Ok(monitors) = window.available_monitors() {
        if let Some(main) = monitors
            .iter()
            .find(|m| m.position().x == 0 && m.position().y == 0)
        {
            return Some(place(main));
        }
    }
    if let Ok(Some(m)) = window.primary_monitor() {
        return Some(place(&m));
    }
    window.current_monitor().ok().flatten().map(|m| place(&m))
}

/// `~/Library/Application Support/<bundle-id>/window.json` (local, not iCloud).
fn position_file(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_config_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("window.json"))
}

fn load_saved_position(app: &AppHandle) -> Option<LogicalPosition<f64>> {
    let raw = std::fs::read_to_string(position_file(app)?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(LogicalPosition::new(
        v.get("x")?.as_f64()?,
        v.get("y")?.as_f64()?,
    ))
}

fn save_position(app: &AppHandle, pos: LogicalPosition<f64>) {
    if let Some(path) = position_file(app) {
        let _ = std::fs::write(path, format!("{{\"x\":{},\"y\":{}}}", pos.x, pos.y));
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                // Restore the remembered position; otherwise top-right on first run.
                let pos = load_saved_position(app.handle()).or_else(|| top_right_pos(&window));
                match pos {
                    Some(p) => {
                        let _ = window.set_position(p);
                    }
                    None => {
                        let _ = window.center();
                    }
                }
                let _ = window.set_focus();
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Remember where the user drops the widget.
            if let WindowEvent::Moved(phys) = event {
                let scale = window.scale_factor().unwrap_or(1.0);
                let logical = LogicalPosition::new(phys.x as f64 / scale, phys.y as f64 / scale);
                save_position(window.app_handle(), logical);
            }
        })
        .invoke_handler(tauri::generate_handler![usage::get_usage, usage::get_cost])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
