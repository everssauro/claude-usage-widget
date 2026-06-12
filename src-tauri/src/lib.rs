mod auth;
mod usage;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, LogicalPosition, Manager, WindowEvent};

/// Compute the top-right position of the macOS main display in **logical**
/// points — `set_position` expects logical units, and mixing in physical pixels
/// puts the window off-screen on Retina (the bug that hid it at x=4528).
/// `primary_monitor()` returns `None` intermittently during `setup()`, so we
/// prefer the `available_monitors()` entry at origin (0,0) = the main display.
fn top_right_pos(window: &tauri::WebviewWindow) -> Option<LogicalPosition<f64>> {
    // Window width in logical points; fall back to the configured 280 if the
    // window size isn't realized yet at setup time.
    let scale = window.scale_factor().unwrap_or(1.0);
    let win_w = window
        .outer_size()
        .ok()
        .filter(|s| s.width > 0)
        .map(|s| s.width as f64 / scale)
        .unwrap_or(280.0);
    let margin = 16.0;

    let place = move |m: &tauri::Monitor| {
        // Monitor geometry is physical; convert to logical points via the scale.
        let scale = m.scale_factor();
        let pos = m.position().to_logical::<f64>(scale);
        let size = m.size().to_logical::<f64>(scale);
        LogicalPosition::new(pos.x + size.width - win_w - margin, pos.y + margin)
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

// ---------------------------------------------------------------------------
// Position persistence — `~/Library/Application Support/<bundle-id>/window.json`
// (local, not iCloud). `Moved` fires continuously during a drag, so writes are
// throttled to ~1/s with a final flush on close.
// ---------------------------------------------------------------------------

struct PosSaver {
    pending: Option<LogicalPosition<f64>>,
    last_write: Instant,
}

struct PosState(Mutex<PosSaver>);

fn position_file(app: &AppHandle) -> Option<PathBuf> {
    Some(app.path().app_config_dir().ok()?.join("window.json"))
}

fn load_saved_position(app: &AppHandle) -> Option<LogicalPosition<f64>> {
    let raw = std::fs::read_to_string(position_file(app)?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(LogicalPosition::new(
        v.get("x")?.as_f64()?,
        v.get("y")?.as_f64()?,
    ))
}

fn write_position(app: &AppHandle, pos: LogicalPosition<f64>) {
    if let Some(path) = position_file(app) {
        let _ = std::fs::write(
            path,
            serde_json::json!({ "x": pos.x, "y": pos.y }).to_string(),
        );
    }
}

/// Record a move; write through at most once per second.
fn record_move(app: &AppHandle, pos: LogicalPosition<f64>) {
    let state = app.state::<PosState>();
    let mut saver = state.0.lock().unwrap();
    saver.pending = Some(pos);
    if saver.last_write.elapsed() >= Duration::from_secs(1) {
        saver.last_write = Instant::now();
        let pos = saver.pending.take().unwrap();
        drop(saver);
        write_position(app, pos);
    }
}

fn flush_position(app: &AppHandle) {
    let state = app.state::<PosState>();
    let pending = state.0.lock().unwrap().pending.take();
    if let Some(pos) = pending {
        write_position(app, pos);
    }
}

// ---------------------------------------------------------------------------
// PiP mode
// ---------------------------------------------------------------------------

/// Whether PiP (pin) mode is on — so it can be re-asserted on window focus.
struct Pinned(Mutex<bool>);

/// PiP mode (the pin toggle): when `on`, the widget is visible on **every Space**
/// (follows you when you switch desktops), floats **over fullscreen apps**, and
/// stays **on top** — like a Picture-in-Picture video. When `off`, it's a normal
/// window on the current Space (but still draggable onto a fullscreen Space).
///
/// Spaces-following uses Tauri's managed `set_visible_on_all_workspaces` (it
/// re-applies the bit, so it persists across Space switches — raw objc
/// `CanJoinAllSpaces` was getting reset by later events). FullScreenAuxiliary +
/// level are added on top via objc, OR'd into the current behavior so the
/// CanJoinAllSpaces bit Tauri set isn't clobbered. Floating over OTHER apps'
/// fullscreen Spaces additionally requires the window to be a non-activating
/// NSPanel — done once in `setup` via tauri-nspanel.
fn apply_pip(window: &tauri::WebviewWindow, on: bool) {
    // Keep Tauri's internal always-on-top flag in sync; the real level is set
    // directly below (setLevel wins over whatever this applies).
    let _ = window.set_always_on_top(on);
    // Managed CanJoinAllSpaces (persists across Space switches).
    let _ = window.set_visible_on_all_workspaces(on);

    #[cfg(target_os = "macos")]
    {
        use objc::{msg_send, runtime::Object, sel, sel_impl};
        if let Ok(ptr) = window.ns_window() {
            let ns_window = ptr as *mut Object;
            const FULLSCREEN_AUXILIARY: u64 = 1 << 8;
            const STATIONARY: u64 = 1 << 4;
            // Above fullscreen content. Screen-saver level is what real overlay
            // apps use to sit over other apps' fullscreen Spaces.
            const NS_SCREEN_SAVER_WINDOW_LEVEL: i64 = 1000;
            unsafe {
                // Keep whatever Tauri set (incl. CanJoinAllSpaces) and add fullscreen
                // overlay + stationary. Keep FullScreenAuxiliary even when unpinned so
                // it can overlay a fullscreen Space (like a Meet window).
                let cur: u64 = msg_send![ns_window, collectionBehavior];
                let behavior = cur | FULLSCREEN_AUXILIARY | STATIONARY;
                let level: i64 = if on { NS_SCREEN_SAVER_WINDOW_LEVEL } else { 0 };
                let _: () = msg_send![ns_window, setCollectionBehavior: behavior];
                let _: () = msg_send![ns_window, setLevel: level];
            }
        }
    }
}

/// Toggle PiP mode from the frontend (the pin button).
#[tauri::command]
fn set_pinned(window: tauri::WebviewWindow, state: tauri::State<Pinned>, on: bool) {
    *state.0.lock().unwrap() = on;
    apply_pip(&window, on);
}

// ---------------------------------------------------------------------------
// "Sign in with Claude" commands (auth.rs)
// ---------------------------------------------------------------------------

/// Open the browser on the OAuth page; returns the URL (UI fallback link).
#[tauri::command]
async fn start_login() -> String {
    tauri::async_runtime::spawn_blocking(auth::begin_login)
        .await
        .unwrap_or_default()
}

/// Exchange the pasted `code#state` for tokens and store them.
#[tauri::command]
async fn finish_login(code: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || auth::complete_login(&code))
        .await
        .map_err(|e| e.to_string())?
}

/// "own" (widget login) | "claude_code" (detected install) | "none".
#[tauri::command]
async fn auth_status() -> String {
    tauri::async_runtime::spawn_blocking(|| {
        if auth::signed_in() {
            "own".to_string()
        } else if usage::has_native_token() {
            "claude_code".to_string()
        } else {
            "none".to_string()
        }
    })
    .await
    .unwrap_or_else(|_| "none".to_string())
}

#[tauri::command]
fn sign_out() {
    auth::sign_out();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_notification::init());
    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_nspanel::init());

    builder
        .manage(Pinned(Mutex::new(true)))
        .manage(PosState(Mutex::new(PosSaver {
            pending: None,
            last_write: Instant::now(),
        })))
        .setup(|app| {
            if let Ok(dir) = app.path().app_config_dir() {
                let _ = std::fs::create_dir_all(&dir);
                auth::set_config_dir(dir); // auth.json lives next to window.json
            }
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
                // Convert to a non-activating NSPanel ONCE so it can overlay other
                // apps' fullscreen Spaces. Level + collectionBehavior stay in apply_pip.
                #[cfg(target_os = "macos")]
                {
                    use tauri_nspanel::WebviewWindowExt;
                    if let Ok(panel) = window.to_panel() {
                        const NONACTIVATING_PANEL: i32 = 1 << 7;
                        panel.set_style_mask(NONACTIVATING_PANEL);
                    }
                }
                apply_pip(&window, true); // PiP on by default; JS reconciles via localStorage
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Remember where the user drops the widget (throttled; flushed on close).
            WindowEvent::Moved(phys) => {
                let scale = window.scale_factor().unwrap_or(1.0);
                record_move(window.app_handle(), phys.to_logical::<f64>(scale));
            }
            WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed => {
                flush_position(window.app_handle());
            }
            // Re-assert PiP when the widget regains focus (macOS can reset the
            // window level / fullscreen behavior across Space switches).
            WindowEvent::Focused(true) => {
                let app = window.app_handle();
                let pinned = *app.state::<Pinned>().0.lock().unwrap();
                if let Some(w) = app.get_webview_window("main") {
                    apply_pip(&w, pinned);
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            usage::get_usage,
            usage::get_cost,
            usage::get_month_cost,
            set_pinned,
            start_login,
            finish_login,
            auth_status,
            sign_out
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
