#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The llmpool window and tray icon. The interface is a client of the
//! daemon's HTTP API, exactly like the command line.

use llmpool_daemon::client::Client;
use llmpool_daemon::{http, Daemon};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, WindowEvent, Wry};

const TRAY: &str = "llmpool";
/// How often the tray menu is brought up to date.
const TRAY_REFRESH: Duration = Duration::from_secs(5);

/// Serializes daemon start-up so two early calls cannot both host one.
static STARTING: Mutex<()> = Mutex::new(());
/// What the tray menu was last built from, so it is rebuilt only on change.
static TRAY_STATE: Mutex<String> = Mutex::new(String::new());

/// Reach the daemon; if none is running, host one inside this process.
fn client() -> Result<Client, String> {
    let timeout = Duration::from_secs(15);
    let _guard = STARTING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(client) = Client::existing(timeout) {
        return Ok(client);
    }
    let lock = http::instance_lock(&llmpool_daemon::client::data_dir()).map_err(|e| e.to_string())?;
    let daemon = Daemon::open_default().map_err(|e| e.message)?;
    let running = http::start(Arc::new(daemon), lock).map_err(|e| e.to_string())?;
    std::thread::spawn(move || running.wait());
    Client::existing(timeout).ok_or_else(|| "could not start the llmpool daemon".to_string())
}

#[tauri::command(async)]
fn api(method: String, path: String, body: Option<Value>) -> Result<Value, String> {
    client()?.call(&method, &path, body)
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn percent(usage: &Value, window: &str) -> Option<String> {
    usage["windows"][window]["used_percent"].as_f64().map(|p| format!("{window} {p:.0}%"))
}

/// "api · work-1 · 5h 42% · 7d 12%"
fn session_title(session: &Value) -> String {
    let cwd = session["cwd"].as_str().unwrap_or_default();
    let folder = cwd.trim_end_matches('/').rsplit('/').next().unwrap_or(cwd);
    let mut parts = vec![folder.to_string(), session["account_label"].as_str().unwrap_or("your own login").to_string()];
    parts.extend(["5h", "7d"].iter().filter_map(|w| percent(&session["usage"], w)));
    parts.join(" · ")
}

/// The tray menu: one entry per running session, each opening onto the
/// accounts it can be switched to.
fn tray_menu(app: &AppHandle, sessions: &Value, accounts: &Value) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::new(app)?;
    let none: Option<&str> = None;
    let sessions = sessions.as_array().map(Vec::as_slice).unwrap_or_default();
    if sessions.is_empty() {
        menu.append(&MenuItem::new(app, "No sessions running", false, none)?)?;
    }
    for session in sessions {
        let sid = session["id"].as_str().unwrap_or_default();
        if session["store"].is_null() {
            menu.append(&MenuItem::new(app, session_title(session), false, none)?)?;
            continue;
        }
        let submenu = Submenu::new(app, session_title(session), true)?;
        for account in accounts.as_array().into_iter().flatten() {
            let id = account["id"].as_str().unwrap_or_default();
            let current = session["account_id"].as_str() == Some(id);
            // An account live in another profile cannot be put in this one too.
            let free = current || account["sessions"].as_array().is_none_or(Vec::is_empty);
            let usable = account["enabled"] == true && account["health"] == "ok" && free;
            let mut label = account["label"].as_str().unwrap_or_default().to_string();
            for part in ["5h", "7d"].iter().filter_map(|w| percent(&account["usage"], w)) {
                label = format!("{label} · {part}");
            }
            let item = CheckMenuItem::with_id(app, format!("swap|{sid}|{id}"), label, usable && !current, current, none)?;
            submenu.append(&item)?;
        }
        menu.append(&submenu)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "open", "Open llmpool", true, none)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit llmpool", true, none)?)?;
    Ok(menu)
}

/// Rebuild the tray menu if what it shows has changed.
fn refresh_tray(app: &AppHandle) {
    let (sessions, accounts) = match client().and_then(|c| Ok((c.call("GET", "/sessions", None)?, c.call("GET", "/accounts", None)?))) {
        Ok(state) => state,
        Err(_) => (json!([]), json!([])),
    };
    let signature = format!("{sessions}{accounts}");
    {
        let mut last = TRAY_STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if *last == signature {
            return;
        }
        *last = signature;
    }
    let handle = app.clone();
    // Menus belong to the main thread.
    let _ = app.run_on_main_thread(move || {
        if let (Some(tray), Ok(menu)) = (handle.tray_by_id(TRAY), tray_menu(&handle, &sessions, &accounts)) {
            let _ = tray.set_menu(Some(menu));
        }
    });
}

fn on_menu(app: &AppHandle, id: &str) {
    match id.split('|').collect::<Vec<_>>().as_slice() {
        ["open"] => show_window(app),
        ["quit"] => app.exit(0),
        ["swap", session, account] => {
            let (app, session, account) = (app.clone(), session.to_string(), account.to_string());
            std::thread::spawn(move || {
                let _ = client().and_then(|c| c.call("POST", &format!("/sessions/{session}/swap"), Some(json!({"account": account}))));
                TRAY_STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
                refresh_tray(&app);
            });
        }
        _ => {}
    }
}

fn main() {
    let app = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![api])
        .on_window_event(|window, event| {
            // Closing the window leaves llmpool in the tray, where it keeps
            // watching usage; Quit is in the tray menu.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            let mut tray = TrayIconBuilder::with_id(TRAY)
                .menu(&tray_menu(&handle, &json!([]), &json!([]))?)
                .tooltip("llmpool")
                .on_menu_event(|app, event| on_menu(app, event.id.as_ref()));
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            std::thread::spawn(move || loop {
                refresh_tray(&handle);
                std::thread::sleep(TRAY_REFRESH);
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("llmpool could not start its window");

    app.run(|_app, _event| {
        // Clicking the Dock icon brings the window back after it was closed.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = _event {
            show_window(_app);
        }
    });
}
