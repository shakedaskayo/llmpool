#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The llmpool window and tray icon. The interface is a client of the
//! daemon's HTTP API, exactly like the command line.

use llmpool_daemon::client::Client;
use llmpool_daemon::{http, Daemon};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::Manager;

/// Serializes daemon start-up so two early calls cannot both host one.
static STARTING: Mutex<()> = Mutex::new(());

/// Reach the daemon; if none is running, host one inside this process.
fn client() -> Result<Client, String> {
    let timeout = Duration::from_secs(15);
    let _guard = STARTING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(client) = Client::existing(timeout) {
        return Ok(client);
    }
    let daemon = Daemon::open_default().map_err(|e| e.message)?;
    let running = http::start(Arc::new(daemon)).map_err(|e| e.to_string())?;
    std::thread::spawn(move || running.wait());
    Client::existing(timeout).ok_or_else(|| "could not start the llmpool daemon".to_string())
}

#[tauri::command(async)]
fn api(method: String, path: String, body: Option<Value>) -> Result<Value, String> {
    client()?.call(&method, &path, body)
}

fn show_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![api])
        .setup(|app| {
            let open = MenuItem::with_id(app, "open", "Open llmpool", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            let mut tray = TrayIconBuilder::new().menu(&menu).tooltip("llmpool");
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.on_menu_event(|app, event| match event.id.as_ref() {
                "open" => show_window(app),
                "quit" => app.exit(0),
                _ => {}
            })
            .build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("llmpool could not start its window");
}
