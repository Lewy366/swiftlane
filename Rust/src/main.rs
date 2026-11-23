// Swiftlane, the Rust version. The window shows the same interface as before (Source/web);
// everything behind it moves from Python to Rust step by step (see api.rs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod config;
mod updater;
mod winapi;

use tauri::Manager;

fn main() {
    let minimized = std::env::args().any(|a| a == "--minimized");
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(api::AppState::load())
        .invoke_handler(tauri::generate_handler![api::call])
        .setup(move |app| {
            if minimized {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.minimize();
                }
            }
            let handle = app.handle().clone();
            std::thread::spawn(move || api::update_loop(handle));
            config::log(&format!("Swiftlane {} (Rust) started", api::VERSION));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Swiftlane couldn't start");
}
