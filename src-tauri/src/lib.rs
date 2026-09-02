mod auth;
mod sessions;
mod tray;
mod usage;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, LogicalPosition, Manager, WindowEvent};

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
// Position persistence + display-change resilience
// `~/Library/Application Support/<bundle-id>/window.json` (local, not iCloud).
// `Moved` fires continuously during a drag, so writes are throttled to ~1/s
// with a final flush on close.
//
// Two rules make the widget survive monitors coming and going like a normal
// window does:
//   1. ONLY A MOVE THE USER MADE COUNTS. When a display sleeps or disconnects,
//      macOS relocates the window to a surviving screen and that fires `Moved`
//      too — persisting it would overwrite the remembered spot, so the widget
//      "forgets" where it lived the moment a second monitor powers off.
//   2. WHEN THE REMEMBERED SPOT IS REACHABLE AGAIN, GO BACK TO IT. A watcher
//      notices the display returned and restores the position, instead of
//      leaving the widget wherever the system dumped it.
// ---------------------------------------------------------------------------

/// A `Moved` this soon after the user's last drag motion still counts as that
/// drag (they may pause mid-gesture); anything later is the system moving us.
const DRAG_GRACE: Duration = Duration::from_secs(10);
/// How often the watcher wakes. Cheap: it only compares two `Instant`s unless
/// there is something to do.
const WATCH_TICK: Duration = Duration::from_millis(250);
/// Safety-net sweep, for a drift that somehow produced no `Moved` event.
const WATCH_SWEEP: Duration = Duration::from_secs(3);
/// A system relocation arrives as a burst of `Moved` events while the display
/// layout settles. Wait this long after the LAST one before restoring, so we
/// correct once instead of fighting the reconfiguration — and so the visible
/// jump is a blink rather than the 3s it took to notice by polling alone.
const RESTORE_DEBOUNCE: Duration = Duration::from_millis(400);
/// Ignore rounding noise when comparing positions.
const POS_EPSILON: f64 = 2.0;

struct PosSaver {
    pending: Option<LogicalPosition<f64>>,
    last_write: Instant,
    /// Where the *user* put the widget — the source of truth we restore to.
    desired: Option<LogicalPosition<f64>>,
    /// Last user drag motion (set by `start_drag`, refreshed by user moves).
    dragging_since: Option<Instant>,
    /// Set when the system (not the user) moved us: restore once this passes.
    restore_after: Option<Instant>,
}

struct PosState(Mutex<PosSaver>);

impl PosSaver {
    fn user_is_dragging(&self) -> bool {
        self.dragging_since
            .map(|t| t.elapsed() < DRAG_GRACE)
            .unwrap_or(false)
    }
}

/// Whether `p` sits on a currently-connected monitor, with enough of the widget
/// reachable to grab it. Returns `None` when the monitor list can't be read —
/// callers must NOT treat "unknown" as "off-screen": enumeration is flaky during
/// `setup()` (see `top_right_pos`), and discarding a valid saved position there
/// would silently reset the widget's home on every launch.
fn position_on_a_monitor(window: &tauri::WebviewWindow, p: LogicalPosition<f64>) -> Option<bool> {
    let monitors = window.available_monitors().ok()?;
    if monitors.is_empty() {
        return None;
    }
    Some(monitors.iter().any(|m| {
        let s = m.scale_factor();
        let o = m.position().to_logical::<f64>(s);
        let z = m.size().to_logical::<f64>(s);
        p.x >= o.x - 40.0
            && p.x <= o.x + z.width - 40.0
            && p.y >= o.y
            && p.y <= o.y + z.height - 40.0
    }))
}

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

/// Record a move; write through at most once per second. A move the user didn't
/// make (macOS relocating us because a display slept or was unplugged) is
/// ignored entirely — see rule 1 above.
fn record_move(app: &AppHandle, pos: LogicalPosition<f64>) {
    let state = app.state::<PosState>();
    let mut saver = state.0.lock().unwrap();
    if !saver.user_is_dragging() {
        // Not us — the system moved the widget (a display slept, woke, or was
        // unplugged). That event IS the signal; no need to discover it by
        // polling. Debounced so a burst of reconfiguration moves collapses into
        // one restore.
        saver.restore_after = Some(Instant::now() + RESTORE_DEBOUNCE);
        return;
    }
    saver.restore_after = None; // the user is in charge now
    saver.dragging_since = Some(Instant::now()); // keep the drag alive across pauses
    saver.desired = Some(pos);
    saver.pending = Some(pos);
    if saver.last_write.elapsed() >= Duration::from_secs(1) {
        saver.last_write = Instant::now();
        let pos = saver.pending.take().unwrap();
        drop(saver);
        write_position(app, pos);
    }
}

/// Restore the remembered position once it's reachable again (rule 2). Runs off
/// the main thread; Tauri's window calls proxy to the event loop.
fn spawn_position_watcher(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last_sweep = Instant::now();
        loop {
            std::thread::sleep(WATCH_TICK);
            let (desired, dragging, due) = {
                let state = app.state::<PosState>();
                let mut saver = state.0.lock().unwrap();
                let due = saver
                    .restore_after
                    .map(|t| Instant::now() >= t)
                    .unwrap_or(false);
                if due {
                    saver.restore_after = None;
                }
                (saver.desired, saver.user_is_dragging(), due)
            };
            // Act on the event; otherwise only on the slow safety sweep.
            if !due && last_sweep.elapsed() < WATCH_SWEEP {
                continue;
            }
            last_sweep = Instant::now();
            let Some(window) = app.get_webview_window("main") else {
                continue;
            };
            // Never fight the user mid-gesture.
            let (Some(want), false) = (desired, dragging) else {
                continue;
            };
            let scale = window.scale_factor().unwrap_or(1.0);
            let Ok(cur) = window.outer_position() else {
                continue;
            };
            let cur = cur.to_logical::<f64>(scale);
            if (cur.x - want.x).abs() < POS_EPSILON && (cur.y - want.y).abs() < POS_EPSILON {
                continue; // already home
            }
            // Only pull it back when that spot actually exists right now;
            // otherwise the monitor is still off and wherever macOS parked us is
            // the best we can do.
            if position_on_a_monitor(&window, want) == Some(true) {
                let _ = window.set_position(want);
            }
        }
    });
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
    // NOTE: we deliberately do NOT call `window.set_always_on_top()`. tao routes
    // it through `set_level_async` = `dispatch_async(main)`, which drains in
    // kCFRunLoopCommonModes — including NSEventTrackingRunLoopMode. That means
    // the deferred `setLevel:` executes *inside* `performWindowDragWithEvent:`'s
    // nested tracking loop (re-ordering the window mid-drag), and it overwrites
    // our screen-saver level with NSFloatingWindowLevel(3) one runloop turn
    // later — which is why "floats over other apps' fullscreen Spaces" never
    // actually held. The level is set synchronously via objc below instead.
    //
    // Managed CanJoinAllSpaces (persists across Space switches).
    let _ = window.set_visible_on_all_workspaces(on);

    #[cfg(target_os = "macos")]
    {
        use objc::{msg_send, runtime::Object, sel, sel_impl};
        if let Ok(ptr) = window.ns_window() {
            let ns_window = ptr as *mut Object;
            unsafe {
                let (behavior, level) = desired_pip(ns_window, on);
                let _: () = msg_send![ns_window, setCollectionBehavior: behavior];
                let _: () = msg_send![ns_window, setLevel: level];
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = on;
}

#[cfg(target_os = "macos")]
const FULLSCREEN_AUXILIARY: u64 = 1 << 8;
#[cfg(target_os = "macos")]
const STATIONARY: u64 = 1 << 4;
/// Above fullscreen content — the level real overlay apps use to sit over other
/// apps' fullscreen Spaces.
#[cfg(target_os = "macos")]
const NS_SCREEN_SAVER_WINDOW_LEVEL: i64 = 1000;

/// The collectionBehavior + level this window should have for a given pin state.
/// Keeps whatever is already set (incl. the CanJoinAllSpaces bit Tauri manages)
/// and adds fullscreen-overlay + stationary. FullScreenAuxiliary stays on even
/// when unpinned so the widget can still overlay a fullscreen Space.
#[cfg(target_os = "macos")]
unsafe fn desired_pip(ns_window: *mut objc::runtime::Object, on: bool) -> (u64, i64) {
    use objc::{msg_send, sel, sel_impl};
    let cur: u64 = msg_send![ns_window, collectionBehavior];
    (
        cur | FULLSCREEN_AUXILIARY | STATIONARY,
        if on { NS_SCREEN_SAVER_WINDOW_LEVEL } else { 0 },
    )
}

/// Re-assert PiP on focus **only if macOS actually drifted**. The `Focused(true)`
/// event fires on the very click that makes the panel key — i.e. the click the
/// user is trying to drag with — so writing window state unconditionally there
/// mutates the window mid-gesture. Reading first makes the common case a no-op.
fn reassert_pip_if_drifted_window(window: &tauri::WebviewWindow, on: bool) {
    #[cfg(target_os = "macos")]
    {
        use objc::{msg_send, runtime::Object, sel, sel_impl};
        if let Ok(ptr) = window.ns_window() {
            let ns_window = ptr as *mut Object;
            unsafe {
                let (behavior, level) = desired_pip(ns_window, on);
                let cur_behavior: u64 = msg_send![ns_window, collectionBehavior];
                let cur_level: i64 = msg_send![ns_window, level];
                if cur_behavior == behavior && cur_level == level {
                    return; // nothing drifted — don't touch the window
                }
            }
        }
    }
    apply_pip(window, on);
}

/// Open (or focus) the session-breakdown window.
///
/// Deliberately a NORMAL window — decorated, resizable, activating. The widget
/// itself is a non-activating NSPanel, which is the wrong host for a table you
/// need to scroll, filter and copy out of. Only "main" is ever converted to a
/// panel (see `setup`), so this window keeps ordinary window behaviour.
#[tauri::command]
async fn open_sessions(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("sessions") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    } else {
        tauri::WebviewWindowBuilder::new(
            &app,
            "sessions",
            tauri::WebviewUrl::App("sessions.html".into()),
        )
        .title("Claude Usage — Sessions")
        .inner_size(940.0, 620.0)
        .min_inner_size(560.0, 320.0)
        .resizable(true)
        // Windows created at runtime do NOT inherit tauri.conf.json's window
        // config, so this has to be set here too — without it the first click
        // on the table (while the window isn't key) is eaten to focus it, and
        // rows look unclickable. Same root cause as the widget's own bug.
        .accept_first_mouse(true)
        .build()
        .map_err(|e| e.to_string())?;
    }
    // The app is non-activating by nature, so a new window can open behind the
    // app the user is actually looking at. Ask for activation explicitly.
    #[cfg(target_os = "macos")]
    unsafe {
        use objc::runtime::{Object, YES};
        use objc::{class, msg_send, sel, sel_impl};
        let ns_app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![ns_app, activateIgnoringOtherApps: YES];
    }
    Ok(())
}

/// Read the clipboard as text.
///
/// The connect view needs this because the widget is a NON-ACTIVATING NSPanel:
/// clicking it never makes our app active, so the menu bar keeps belonging to
/// whatever app is in front — and ⌘V is dispatched through the *active* app's
/// Edit menu. The result is that pasting the OAuth code into `#codeInput`
/// silently does nothing. A Paste button that reads NSPasteboard directly needs
/// no menu, no key equivalent, and no app activation.
#[tauri::command]
fn read_clipboard() -> String {
    #[cfg(target_os = "macos")]
    unsafe {
        use objc::runtime::Object;
        use objc::{class, msg_send, sel, sel_impl};
        const NS_UTF8: usize = 4; // NSUTF8StringEncoding
        let ty: *mut Object = {
            let s = "public.utf8-plain-text"; // NSPasteboardTypeString
            let obj: *mut Object = msg_send![class!(NSString), alloc];
            msg_send![obj, initWithBytes: s.as_ptr() as *const std::ffi::c_void
                                length: s.len()
                              encoding: NS_UTF8]
        };
        let pb: *mut Object = msg_send![class!(NSPasteboard), generalPasteboard];
        let s: *mut Object = msg_send![pb, stringForType: ty];
        if s.is_null() {
            return String::new();
        }
        let bytes: *const std::os::raw::c_char = msg_send![s, UTF8String];
        if bytes.is_null() {
            return String::new();
        }
        return std::ffi::CStr::from_ptr(bytes)
            .to_string_lossy()
            .into_owned();
    }
    #[allow(unreachable_code)]
    String::new()
}

/// Take keyboard focus so a text field in the widget can actually be typed in.
/// A non-activating panel doesn't do this on its own — without it the OAuth code
/// field can be clicked but not typed into.
#[tauri::command]
fn focus_for_input(window: tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    unsafe {
        use objc::runtime::{Object, YES};
        use objc::{class, msg_send, sel, sel_impl};
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: YES];
        if let Ok(ptr) = window.ns_window() {
            let ns_window = ptr as *mut Object;
            let _: () = msg_send![ns_window, makeKeyAndOrderFront: std::ptr::null::<Object>()];
            let content_view: *mut Object = msg_send![ns_window, contentView];
            let _: () = msg_send![ns_window, makeFirstResponder: content_view];
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}

/// Begin a window drag. Goes through Rust (instead of the JS `startDragging`)
/// so we can mark the moves that follow as USER-initiated — that's what lets
/// `record_move` tell a real drag apart from macOS relocating the widget when a
/// display sleeps or is unplugged.
#[tauri::command]
fn start_drag(window: tauri::WebviewWindow, state: tauri::State<PosState>) {
    state.0.lock().unwrap().dragging_since = Some(Instant::now());
    let _ = window.start_dragging();
}

/// Toggle PiP mode from the frontend (the pin button).
#[tauri::command]
fn set_pinned(window: tauri::WebviewWindow, state: tauri::State<Pinned>, on: bool) {
    *state.0.lock().unwrap() = on;
    apply_pip(&window, on);
}

/// Toggle the macOS frosted-glass (vibrancy) backing behind the card. On → a
/// native NSVisualEffectView blurs the real desktop behind the transparent
/// window (the card CSS goes translucent to reveal it); off → the card's own
/// opaque background shows (the default solid look). macOS-only; no-op elsewhere.
const CARD_RADIUS: f64 = 18.0; // keep in sync with .card border-radius in styles.css

#[tauri::command]
fn set_glass(window: tauri::WebviewWindow, on: bool) {
    #[cfg(target_os = "macos")]
    {
        use objc::{msg_send, runtime::Object, sel, sel_impl};
        use window_vibrancy::{
            apply_vibrancy, clear_vibrancy, NSVisualEffectMaterial, NSVisualEffectState,
        };
        if on {
            // UnderWindowBackground — the most translucent material; reads better
            // here than HudWindow (which washed the text out). Renders dark-frosted
            // in a dark-appearance window. Keep it Active even when the panel isn't key.
            let _ = apply_vibrancy(
                &window,
                NSVisualEffectMaterial::UnderWindowBackground,
                Some(NSVisualEffectState::Active),
                Some(CARD_RADIUS),
            );
        } else {
            let _ = clear_vibrancy(&window);
        }
        // Clip the window contentView to the card's rounded rect — ALWAYS, glass or
        // not. Once the view is layer-backed its square corners poke out past the
        // rounded card (the rectangular NSVisualEffectView when glass is on, the
        // opaque backing when it's off). The card is always the same rounded shape,
        // so the clip is constant; only the vibrancy/translucency toggles.
        if let Ok(ptr) = window.ns_window() {
            let ns_window = ptr as *mut Object;
            unsafe {
                let content_view: *mut Object = msg_send![ns_window, contentView];
                let _: () = msg_send![content_view, setWantsLayer: true];
                let layer: *mut Object = msg_send![content_view, layer];
                let _: () = msg_send![layer, setCornerRadius: CARD_RADIUS];
                let _: () = msg_send![layer, setMasksToBounds: true];
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (window, on);
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

// ---------------------------------------------------------------------------
// Bridges the tray calls into. Kept here because they need `Pinned`/`PosState`,
// which the tray module deliberately doesn't know about.
// ---------------------------------------------------------------------------

/// Accept wherever the window is RIGHT NOW as the position to defend.
/// Without this the watcher treats an anchored placement as a system relocation
/// and yanks the window back to the last hand-dragged spot moments after it
/// appears — the two features would fight each other on screen.
pub fn adopt_current_position(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Ok(phys) = window.outer_position() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let pos = phys.to_logical::<f64>(scale);
    if let Ok(mut saver) = app.state::<PosState>().0.lock() {
        saver.desired = Some(pos);
        saver.restore_after = None;
    }
    write_position(app, pos);
}

pub fn reassert_pip_if_drifted(app: &AppHandle) {
    let on = *app.state::<Pinned>().0.lock().unwrap();
    if let Some(window) = app.get_webview_window("main") {
        reassert_pip_if_drifted_window(&window, on);
    }
}

pub fn toggle_pin_from_tray(app: &AppHandle) {
    let state = app.state::<Pinned>();
    let on = {
        let mut p = state.0.lock().unwrap();
        *p = !*p;
        *p
    };
    if let Some(window) = app.get_webview_window("main") {
        apply_pip(&window, on);
        // The card's pin button has to agree with the menu.
        let _ = window.emit("tray://pinned", on);
    }
}

pub fn open_sessions_from_tray(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = open_sessions(app).await {
            eprintln!("tray: open sessions failed: {e}");
        }
    });
}

pub fn show_settings_from_tray(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.emit("tray://settings", ());
    }
}

pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_notification::init());
    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_nspanel::init());

    builder
        .manage(Pinned(Mutex::new(true)))
        .manage(tray::TrayState(Mutex::new(tray::TrayPrefs { anchored: false, hide_dock: false })))
        .manage(PosState(Mutex::new(PosSaver {
            pending: None,
            last_write: Instant::now(),
            desired: None,
            dragging_since: None,
            restore_after: None,
        })))
        .setup(|app| {
            if let Ok(dir) = app.path().app_config_dir() {
                let _ = std::fs::create_dir_all(&dir);
                auth::set_config_dir(dir); // auth.json lives next to window.json
            }
            // Menu-bar icon: a second door to the widget, not a replacement.
            let prefs = tray::load_prefs(app.handle());
            let hide_dock = prefs.hide_dock;
            *app.state::<tray::TrayState>().0.lock().unwrap() = prefs;
            if let Err(e) = tray::build(app.handle()) {
                // Not fatal: the widget still works as a plain window, and
                // failing to launch over a missing menu-bar icon would be worse
                // than launching without one.
                eprintln!("tray icon unavailable: {e}");
            } else if hide_dock {
                let _ = tray::set_hide_dock(app.handle().clone(), true);
            }
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                // Restore the remembered position; otherwise top-right on first run.
                // A saved point is only rejected when the monitors could actually be
                // read AND none of them contains it (e.g. it was left on a display
                // that is now gone) — "unknown" must not discard a good position.
                let saved = load_saved_position(app.handle())
                    .filter(|p| position_on_a_monitor(&window, *p) != Some(false));
                let pos = saved.or_else(|| top_right_pos(&window));
                match pos {
                    Some(p) => {
                        let _ = window.set_position(p);
                    }
                    None => {
                        let _ = window.center();
                    }
                }
                // Remember where the widget belongs, even if macOS moves it later.
                if let Some(p) = load_saved_position(app.handle()).or(pos) {
                    app.state::<PosState>().0.lock().unwrap().desired = Some(p);
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
                        // Mandatory after changing the style mask: AppKit rebuilds the
                        // window's frame view and can leave firstResponder nil, which
                        // breaks key handling (the OAuth code field would silently
                        // refuse keystrokes). tao does exactly this in its own
                        // set_style_mask helper, with the same warning.
                        panel.make_first_responder(Some(panel.content_view()));
                    }
                }
                apply_pip(&window, true); // PiP on by default; JS reconciles via localStorage
            }
            spawn_position_watcher(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            // These all describe the WIDGET. Without this guard the sessions
            // window would save ITS position into the widget's window.json and
            // re-assert PiP whenever it took focus.
            if window.label() != "main" {
                return;
            }
            match event {
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
                        reassert_pip_if_drifted_window(&w, pinned);
                    }
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            usage::get_usage,
            usage::get_cost,
            usage::get_month_cost,
            sessions::get_sessions,
            sessions::get_groups,
            sessions::save_groups,
            tray::set_tray_anchored,
            tray::set_hide_dock,
            tray::tray_prefs,
            open_sessions,
            set_pinned,
            set_glass,
            start_drag,
            read_clipboard,
            focus_for_input,
            start_login,
            finish_login,
            auth_status,
            sign_out
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
