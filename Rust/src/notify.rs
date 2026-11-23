//! Windows notifications ("File received", "Update available"), only when switched on in Settings.

use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::api::AppState;

pub fn show(app: &AppHandle, title: &str, text: &str) {
    if !app.state::<AppState>().cfg.lock().unwrap()["notify"].as_bool().unwrap_or(true) {
        return;
    }
    let text: String = text.chars().take(200).collect();
    let _ = app.notification().builder().title(format!("Swiftlane: {title}")).body(text).show();
}
