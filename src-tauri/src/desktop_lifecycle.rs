//! Closing the main window is a persisted choice, not an implicit shutdown.
use crate::telemetry_store::TelemetryStore;
use serde::{Deserialize, Serialize};
use std::sync::{atomic::{AtomicBool, AtomicU8, Ordering}, Mutex};
use tauri::{menu::{Menu, MenuItem}, tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent}, AppHandle, Emitter, Manager, Window};

const SETTING: &str = "desktop.close_behavior";
const TRAY_ID: &str = "workspace-tray";
const CLOSE_EVENT: &str = "desktop-close-requested";

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehavior { #[default] Ask, Background, Exit }
impl CloseBehavior {
    fn code(self) -> u8 { match self { Self::Ask => 0, Self::Background => 1, Self::Exit => 2 } }
    fn from_code(value: u8) -> Self { match value { 1 => Self::Background, 2 => Self::Exit, _ => Self::Ask } }
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum CloseDecision { Prompt, Hide, Exit }
fn decide(behavior: CloseBehavior, tray_available: bool) -> CloseDecision {
    match behavior {
        CloseBehavior::Exit => CloseDecision::Exit,
        CloseBehavior::Background if tray_available => CloseDecision::Hide,
        _ => CloseDecision::Prompt,
    }
}

#[derive(Default)]
pub struct DesktopLifecycle {
    behavior: AtomicU8,
    pending: AtomicBool,
    tray_available: AtomicBool,
    write_lock: Mutex<()>,
}
// Keep native menu ownership separate from the pure close-policy state. In
// particular, policy tests must not link/drop Windows GUI control handles.
struct TrayMenu {
    open: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
}
#[derive(Clone, Serialize)]
pub struct DesktopPreferences {
    close_behavior: CloseBehavior,
    close_pending: bool,
    tray_available: bool,
}
fn snapshot(app: &AppHandle) -> DesktopPreferences {
    let state = app.state::<DesktopLifecycle>();
    DesktopPreferences {
        close_behavior: CloseBehavior::from_code(state.behavior.load(Ordering::SeqCst)),
        close_pending: state.pending.load(Ordering::SeqCst),
        tray_available: state.tray_available.load(Ordering::SeqCst),
    }
}
fn read_behavior(store: &TelemetryStore) -> CloseBehavior {
    store.app_setting_get(SETTING).unwrap_or_default()
}
fn save_behavior(state: &DesktopLifecycle, store: &TelemetryStore, value: CloseBehavior) -> Result<(), String> {
    let _guard = state.write_lock.lock().map_err(|_| "Window preferences are busy".to_string())?;
    // Persist before changing the in-memory policy or hiding/exiting the app.
    store.app_setting_set(SETTING, &value)?;
    state.behavior.store(value.code(), Ordering::SeqCst);
    Ok(())
}
fn open_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
fn update_language(app: &AppHandle, language: &str) {
    if let Some(menu) = app.try_state::<TrayMenu>() {
        let english = language.starts_with("en");
        let _ = menu.open.set_text(if english { "Open Vibe Assistant" } else { "打开智管" });
        let _ = menu.quit.set_text(if english { "Quit Vibe Assistant" } else { "退出智管" });
    }
}

pub fn setup(app: &AppHandle) {
    let state = app.state::<DesktopLifecycle>();
    state.behavior.store(read_behavior(&app.state::<TelemetryStore>()).code(), Ordering::SeqCst);
    // A failed tray never leaves the user with an unreachable hidden window.
    if let Err(error) = setup_tray(app) { eprintln!("[desktop] tray unavailable: {error}"); }
}
fn setup_tray(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let open = MenuItem::with_id(app, "workspace-open", "打开智管", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "workspace-exit", "退出智管", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let icon = app.default_window_icon().ok_or("Application tray icon missing")?.clone();
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon).tooltip("Vibe Assistant").menu(&menu).show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "workspace-open" => open_window(app),
            // Explicit tray Quit always exits, even if window-close means Background.
            "workspace-exit" => app.exit(0),
            _ => {},
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(event,
                TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
                | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. }) {
                open_window(tray.app_handle());
            }
        }).build(app)?;
    let state = app.state::<DesktopLifecycle>();
    app.manage(TrayMenu { open, quit });
    state.tray_available.store(true, Ordering::SeqCst);
    Ok(())
}
fn request_choice(window: &Window) {
    let state = window.app_handle().state::<DesktopLifecycle>();
    if state.pending.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
        // The frontend also reads close_pending after subscribing: an early close
        // before the webview listener is ready cannot lose the first-run prompt.
        if let Err(error) = window.emit(CLOSE_EVENT, snapshot(window.app_handle())) {
            state.pending.store(false, Ordering::SeqCst);
            eprintln!("[desktop] cannot request close choice: {error}");
        }
    }
}
pub fn on_window_event(window: &Window, event: &tauri::WindowEvent) {
    if window.label() != "main" { return; }
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let state = window.app_handle().state::<DesktopLifecycle>();
        let action = decide(CloseBehavior::from_code(state.behavior.load(Ordering::SeqCst)), state.tray_available.load(Ordering::SeqCst));
        match action {
            CloseDecision::Prompt => request_choice(window),
            CloseDecision::Hide => { if window.hide().is_err() { request_choice(window); } },
            CloseDecision::Exit => window.app_handle().exit(0),
        }
    }
}

#[tauri::command]
pub fn desktop_preferences_get(app: AppHandle, language: Option<String>) -> DesktopPreferences {
    if let Some(language) = language { update_language(&app, &language); }
    snapshot(&app)
}
#[tauri::command]
pub fn desktop_preferences_set(app: AppHandle, close_behavior: CloseBehavior) -> Result<DesktopPreferences, String> {
    let state = app.state::<DesktopLifecycle>();
    if close_behavior == CloseBehavior::Background && !state.tray_available.load(Ordering::SeqCst) {
        return Err("System tray is unavailable; keep the window open or choose Quit".into());
    }
    save_behavior(&state, &app.state::<TelemetryStore>(), close_behavior)?;
    Ok(snapshot(&app))
}
#[tauri::command]
pub fn desktop_close_cancel(app: AppHandle) {
    app.state::<DesktopLifecycle>().pending.store(false, Ordering::SeqCst);
}
#[tauri::command]
pub fn desktop_close_resolve(app: AppHandle, action: CloseBehavior) -> Result<DesktopPreferences, String> {
    let state = app.state::<DesktopLifecycle>();
    if !state.pending.load(Ordering::SeqCst) { return Err("No window close is pending".into()); }
    if action == CloseBehavior::Ask { return Err("Choose Background or Quit".into()); }
    if action == CloseBehavior::Background && !state.tray_available.load(Ordering::SeqCst) {
        return Err("System tray is unavailable; the window has been kept open".into());
    }
    save_behavior(&state, &app.state::<TelemetryStore>(), action)?;
    if action == CloseBehavior::Background {
        let window = app.get_webview_window("main").ok_or("Main window is unavailable")?;
        window.hide().map_err(|error| error.to_string())?;
    }
    state.pending.store(false, Ordering::SeqCst);
    let result = snapshot(&app);
    if action == CloseBehavior::Exit { app.exit(0); }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_close_asks_and_missing_tray_never_hides() {
        assert_eq!(decide(CloseBehavior::default(), true), CloseDecision::Prompt);
        assert_eq!(decide(CloseBehavior::Background, false), CloseDecision::Prompt);
        assert_eq!(decide(CloseBehavior::Background, true), CloseDecision::Hide);
        assert_eq!(decide(CloseBehavior::Exit, true), CloseDecision::Exit);
        assert_eq!(decide(CloseBehavior::Exit, false), CloseDecision::Exit);
    }
    #[test]
    fn choices_persist_and_can_be_reset_to_ask() {
        let store = TelemetryStore::in_memory();
        let state = DesktopLifecycle::default();
        assert_eq!(read_behavior(&store), CloseBehavior::Ask);
        for value in [CloseBehavior::Background, CloseBehavior::Exit, CloseBehavior::Ask] {
            save_behavior(&state, &store, value).unwrap();
            assert_eq!(read_behavior(&store), value);
            assert_eq!(CloseBehavior::from_code(state.behavior.load(Ordering::SeqCst)), value);
        }
        store.app_setting_set(SETTING, &"unknown").unwrap();
        assert_eq!(read_behavior(&store), CloseBehavior::Ask);
    }
    #[test]
    fn repeated_close_requests_have_one_pending_choice() {
        let state = DesktopLifecycle::default();
        assert!(state.pending.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok());
        assert!(state.pending.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err());
        state.pending.store(false, Ordering::SeqCst);
        assert!(state.pending.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok());
    }
}
