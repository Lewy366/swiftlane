// Swiftlane, the Rust version. The window shows the same interface as before (Source/web);
// everything behind it moves from Python to Rust step by step (see api.rs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod config;
mod notify;
mod funnel;
mod jobs;
mod share;
mod share_server;
mod sharing;
#[allow(dead_code)] // shared helpers (ask_save, program, command) for the tools still being ported
mod tools;
mod tls;
mod transfer;
mod updater;
mod winapi;

use serde_json::json;
use tauri::{DragDropEvent, Manager, WindowEvent};

fn main() {
    if tools::findfiles::run_mft_cli_if_requested()
        || tools::diskmap::run_mft_disk_cli_if_requested()
    {
        return;
    }
    let cli = tools::explorer::parse_args(std::env::args());
    if let Some((ref cmd, ref paths)) = cli.explorer {
        if tools::explorer::try_handoff(cmd, paths) {
            return;
        }
    }
    tools::explorer::set_start_page(cli.start_page);
    let minimized = cli.minimized;
    let uninstall_mode = cli.uninstall;
    let initial_explorer = cli.explorer;
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(api::AppState::load())
        .invoke_handler(tauri::generate_handler![api::call])
        .setup(move |app| {
            let handle = app.handle().clone();
            if uninstall_mode {
                let _ = tools::uninstall::open_window(&handle);
                return Ok(());
            }
            if minimized {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.minimize();
                }
            }
            tools::shrink::wipe(); // leftovers of an earlier run (Shrink to size's private folder)
            tools::biosusb::wipe_temp(); // leftovers of an earlier run (BIOS update USB staging folder)
            tools::explorer::start(&handle, initial_explorer);
            if app.state::<api::AppState>().cfg.lock().unwrap()["features"]["transfer"].as_bool().unwrap_or(true) {
                let _ = transfer::start(&handle);
            }
            let watcher = handle.clone();
            std::thread::spawn(move || transfer::watch_addresses(watcher));
            let sharer = handle.clone();
            std::thread::spawn(move || sharing::share_loop(sharer));
            tools::vault::start(&handle);
            tools::casting::start_with_swiftlane(&handle);
            let peers_handle = handle.clone();
            std::thread::spawn(move || tools::peers::sync(&peers_handle));
            std::thread::spawn(move || api::update_loop(handle));
            config::log(&format!("Swiftlane {} (Rust) started", api::VERSION));
            Ok(())
        })
        .on_window_event(|window, event| {
            // Files dropped on the window: their full paths only ever go to Rust, never into the page.
            if let WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) = event {
                let app = window.app_handle();
                for p in paths.iter().filter(|p| p.is_file()) {
                    tools::biosusb::offer_path(p); // the BIOS USB tool only inspects paths Rust handed out
                }
                let files: Vec<_> = paths.iter().filter(|p| p.is_file()).map(|p| api::file_info(p)).collect();
                let folders: Vec<_> = paths.iter().filter(|p| p.is_dir()).map(|p| p.to_string_lossy().to_string()).collect();
                if !folders.is_empty() {
                    api::push(app, "dropped-all", json!({ "files": files, "folders": folders }));
                } else if !files.is_empty() {
                    api::push(app, "dropped", json!(files));
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("Swiftlane couldn't start")
        .run(|app, event| {
            // Closing: links stop working until Swiftlane runs again, so Funnel closes too, and
            // the vault locks (which also wipes files opened from it).
            if let tauri::RunEvent::Exit = event {
                tools::peers::shutdown();
                tools::explorer::shutdown();
                tools::shrink::wipe();
                tools::biosusb::wipe_temp();
                tools::vault::shutdown();
                tools::phonecam::shutdown();
                tools::casting::shutdown(app);
                sharing::shutdown(app);
            }
        });
}
