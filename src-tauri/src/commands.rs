//! The commands the UI calls. Errors are short, user-facing sentences.

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_opener::OpenerExt;

use crate::model::{AppInfo, Connection, ConnectionStatus, HudPayload, Settings, View};
use crate::net::{Api, validate_server_url};
use crate::{Core, capture, intake, queue, shortcut, store, tray, windows};

type Result<T> = std::result::Result<T, String>;

/// Applies a settings change, saves it and tells every window.
fn update_settings(app: &AppHandle, change: impl FnOnce(&mut Settings)) -> Result<Settings> {
    let core = app.state::<Core>();
    let settings = {
        let mut settings = core.settings.lock().unwrap();
        change(&mut settings);
        store::save_settings(&core.settings_path, &settings).map_err(|_| "Couldn’t save the settings.")?;
        settings.clone()
    };
    tray::refresh(app);
    let _ = app.emit("settings", &settings);
    Ok(settings)
}

#[tauri::command]
pub async fn get_info(app: AppHandle) -> AppInfo {
    let core = app.state::<Core>();
    let has_token = core.token.has().await;
    let settings = core.settings.lock().unwrap().clone();
    let uploads = core.uploads.lock().unwrap().list();
    AppInfo {
        settings,
        has_token,
        platform: std::env::consts::OS,
        shortcut_supported: capture::shortcut_supported(),
        version: app.package_info().version.to_string(),
        uploads,
    }
}

#[tauri::command]
pub async fn set_server_url(app: AppHandle, url: String) -> Result<Settings> {
    let url = validate_server_url(&url)?;
    update_settings(&app, |s| s.server_url = url)
}

#[tauri::command]
pub async fn set_token(app: AppHandle, token: String) -> Result<()> {
    let token = token.trim();
    if token.len() < 32 {
        return Err("That token looks too short".into());
    }
    if !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("That token has characters that aren’t allowed.".into());
    }
    app.state::<Core>().token.set(token.to_owned()).await
}

#[tauri::command]
pub async fn test_connection(app: AppHandle) -> Connection {
    let core = app.state::<Core>();
    let token = core.token.get().await.ok().flatten();
    let server = core.settings.lock().unwrap().server_url.clone();
    let connection = |status, message: &str| Connection { status, message: message.into() };
    let (Some(token), Ok(base)) = (token, validate_server_url(&server)) else {
        return connection(ConnectionStatus::Unconfigured, "Enter a server URL and token.");
    };
    match Api::new(core.http.clone(), base, token).ping().await {
        Ok(204) => connection(ConnectionStatus::Connected, ""),
        Ok(401) => connection(ConnectionStatus::Unauthorized, "The token wasn’t accepted."),
        _ => connection(ConnectionStatus::Unreachable, "The server didn’t respond."),
    }
}

#[tauri::command]
pub async fn set_shortcut(app: AppHandle, accelerator: Option<String>) -> Result<Settings> {
    let accelerator = accelerator.map(|a| a.trim().to_owned()).filter(|a| !a.is_empty());
    let next = accelerator.as_deref().map(shortcut::parse).transpose()?;
    if capture::shortcut_supported() {
        shortcut::replace(&app, next)?;
    }
    update_settings(&app, |s| s.shortcut = accelerator)
}

#[tauri::command]
pub async fn pause_shortcut(app: AppHandle, paused: bool) -> Result<()> {
    shortcut::pause(&app, paused)
}

#[tauri::command]
pub async fn set_launch_at_login(app: AppHandle, enabled: bool) -> Result<Settings> {
    let autostart = app.autolaunch();
    let changed = if enabled { autostart.enable() } else { autostart.disable() };
    changed.map_err(|_| "Couldn’t change the login item.")?;
    update_settings(&app, |s| s.launch_at_login = enabled)
}

#[tauri::command]
pub async fn pick_files(app: AppHandle) {
    intake::pick_files(&app);
}

#[tauri::command]
pub async fn copy_link(app: AppHandle, id: String) -> Result<()> {
    let link = queue::link(&app, &id)?;
    app.clipboard().write_text(link).map_err(|_| "Couldn’t copy the link.".into())
}

#[tauri::command]
pub async fn open_link(app: AppHandle, id: String) -> Result<()> {
    let link = queue::link(&app, &id)?;
    app.opener().open_url(link, None::<&str>).map_err(|_| "Couldn’t open the link.".into())
}

#[tauri::command]
pub async fn delete_upload(app: AppHandle, id: String) -> Result<()> {
    queue::delete(&app, &id).await
}

#[tauri::command]
pub async fn cancel_upload(app: AppHandle, id: String) -> Result<()> {
    queue::cancel(&app, &id)
}

#[tauri::command]
pub async fn retry_upload(app: AppHandle, id: String) -> Result<()> {
    queue::retry(&app, &id).await
}

#[tauri::command]
pub async fn show_main(app: AppHandle, view: View) {
    windows::show_main(&app);
    windows::navigate(&app, view);
}

#[tauri::command]
pub async fn show_library(app: AppHandle) {
    windows::show_library(&app);
}

#[tauri::command]
pub async fn hud_ready(app: AppHandle) -> Option<HudPayload> {
    windows::hud_payload(&app)
}

#[tauri::command]
pub async fn hud_hovered(app: AppHandle) -> bool {
    windows::hud_hovered(&app)
}

#[tauri::command]
pub async fn hud_dismiss(app: AppHandle) {
    windows::hide_hud(&app);
}

#[tauri::command]
pub async fn open_permission_settings() -> Result<()> {
    tauri::async_runtime::spawn_blocking(capture::open_permission_settings)
        .await
        .map_err(|_| "Couldn’t open System Settings.".into())
}
