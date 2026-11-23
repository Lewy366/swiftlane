// Swiftlane, the Rust version. The window shows the same interface as before (Source/web);
// everything behind it moves from Python to Rust step by step (see api.rs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod config;

fn main() {
    tauri::Builder::default()
        .manage(api::AppState::load())
        .invoke_handler(tauri::generate_handler![api::call])
        .run(tauri::generate_context!())
        .expect("Swiftlane couldn't start");
}
