//! Push: press a shortcut or drop a file, get a private link on the clipboard.

mod capture;
#[cfg(target_os = "macos")]
mod command_keys;
mod commands;
mod intake;
mod model;
mod net;
mod preview;
mod queue;
mod shortcut;
mod store;
mod transfer;
mod tray;
mod windows;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, DragDropEvent, Manager, RunEvent, WindowEvent};
use tokio::sync::mpsc;

use crate::model::{HudPayload, Settings};

/// App state, created in `setup`.
pub struct Core {
    http: reqwest::Client,
    settings_path: PathBuf,
    uploads_path: Mutex<PathBuf>,
    /// Library previews (see `preview`).
    previews_dir: PathBuf,
    cache_dir: PathBuf,
    settings: Mutex<Settings>,
    token: store::Token,
    uploads: Mutex<queue::Uploads>,
    /// Ids for the upload worker, in order.
    jobs: mpsc::UnboundedSender<String>,
    /// What the HUD shows (or will show once its page loads); `None` when hidden.
    hud: Mutex<Option<HudPayload>>,
    tray: Option<tray::Items>,
    /// The tray icon is there and visible. Without it the main window is the only way in, so
    /// it shows at a login launch and closing it quits.
    has_tray: bool,
    /// The registered capture shortcut, if any.
    shortcut: Mutex<Option<shortcut::Hotkey>>,
    /// An interactive capture is on screen (the HUD stays hidden meanwhile).
    capturing: AtomicBool,
    /// Launched as a login item (or for a capture): don't show the main window by itself.
    launched_hidden: bool,
    /// The main window has been shown once at launch.
    shown: AtomicBool,
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();
    let paths = app.path();
    let (config_dir, data_dir, cache_dir) = (paths.app_config_dir()?, paths.app_data_dir()?, paths.app_cache_dir()?);
    let settings_path = config_dir.join("settings.json");
    let uploads_path = data_dir.join("uploads.json");
    let previews_dir = data_dir.join("previews");
    let settings = store::load_settings(&settings_path);
    let done = store::load_uploads(&uploads_path);
    preview::prune(&previews_dir, &done.iter().map(|u| u.id.clone()).collect::<HashSet<_>>());
    let uploads = queue::Uploads::from_done(done);
    // Nothing references old captures after a restart (failed items aren't kept).
    let _ = std::fs::remove_dir_all(cache_dir.join("captures"));
    let _ = std::fs::remove_dir_all(cache_dir.join("quicklook"));
    intake::prune_inbox(&cache_dir.join("inbox"));
    #[cfg(target_os = "macos")]
    intake::enable_share_extension();

    let tray = tray::build(&handle).inspect_err(|e| eprintln!("push: no tray icon: {e}")).ok();
    let has_tray = tray.is_some() && tray::host_present();
    if tray.is_some() && !has_tray {
        eprintln!("push: no tray host on this desktop, so the window stays available");
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let launch_at_login = settings.launch_at_login;
    let (jobs, queue) = mpsc::unbounded_channel();
    app.manage(Core {
        http: net::client(),
        settings_path,
        uploads_path: Mutex::new(uploads_path),
        previews_dir,
        cache_dir,
        settings: Mutex::new(settings),
        token: store::Token::new(config_dir),
        uploads: Mutex::new(uploads),
        jobs,
        hud: Mutex::new(None),
        tray,
        has_tray,
        shortcut: Mutex::new(None),
        capturing: AtomicBool::new(false),
        launched_hidden: intake::launched_hidden(&args, has_tray),
        shown: AtomicBool::new(false),
    });
    tauri::async_runtime::spawn(queue::worker(handle.clone(), queue));
    tray::refresh(&handle);
    shortcut::init(&handle);
    // Re-register the login item so it follows the app if it moved.
    if launch_at_login {
        use tauri_plugin_autostart::ManagerExt;
        let _ = handle.autolaunch().enable();
    }
    intake::launch_args(&handle, args);
    intake::drain_early(&handle);
    Ok(())
}

/// Shows the main window after its first paint, unless launched hidden (always on first run).
fn on_page_load(webview: &tauri::Webview, payload: &tauri::webview::PageLoadPayload<'_>) {
    if payload.event() != PageLoadEvent::Finished {
        return;
    }
    let app = webview.app_handle().clone();
    let Some(core) = app.try_state::<Core>() else { return };
    match webview.label() {
        "main" if !core.shown.swap(true, Ordering::SeqCst) => {
            let hidden = core.launched_hidden;
            tauri::async_runtime::spawn(async move {
                if !hidden || !app.state::<Core>().token.has().await {
                    windows::show_main(&app);
                }
            });
        }
        "hud" => windows::hud_loaded(&app),
        "library" => windows::show_library(&app),
        _ => {}
    }
}

fn on_window_event(window: &tauri::Window, event: &WindowEvent) {
    match event {
        // The Library really closes, which frees its images; `Destroyed` follows.
        WindowEvent::CloseRequested { .. } if window.label() == "library" => {}
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            let app = window.app_handle();
            match window.label() {
                "main" if app.state::<Core>().has_tray => windows::hide_main(app),
                // A hidden window with no tray icon would leave Push running out of reach.
                "main" => app.exit(0),
                _ => windows::hide_hud(app),
            }
        }
        WindowEvent::Destroyed if window.label() == "library" => windows::library_closed(window.app_handle()),
        WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) => {
            intake::add_files(window.app_handle(), paths.clone());
        }
        _ => {}
    }
}

fn on_run_event(app: &AppHandle, event: RunEvent) {
    match event {
        // Only Quit (app.exit) ends the app; never "last window closed".
        RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => windows::show_main(app),
        #[cfg(target_os = "macos")]
        RunEvent::Opened { urls } => intake::opened(app, urls),
        _ => {}
    }
    let _ = app;
}

pub fn run() {
    let builder = tauri::Builder::default()
        // First: a second instance forwards its arguments here and exits during setup.
        .plugin(tauri_plugin_single_instance::init(intake::forwarded))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .invoke_handler(tauri::generate_handler![
            commands::get_info,
            commands::set_server_url,
            commands::set_token,
            commands::test_connection,
            commands::set_shortcut,
            commands::pause_shortcut,
            commands::set_launch_at_login,
            commands::pick_files,
            commands::copy_link,
            commands::open_link,
            commands::delete_upload,
            commands::cancel_upload,
            commands::retry_upload,
            commands::show_main,
            commands::show_library,
            commands::hud_ready,
            commands::hud_hovered,
            commands::hud_dismiss,
            commands::open_permission_settings,
        ])
        .register_asynchronous_uri_scheme_protocol("preview", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            tauri::async_runtime::spawn(async move {
                let Some(core) = app.try_state::<Core>() else { return responder.respond(preview::not_found()) };
                responder.respond(preview::respond(&core.previews_dir, request.uri().path()).await);
            });
        })
        .setup(setup)
        .on_page_load(on_page_load)
        .on_window_event(on_window_event);
    // A menu bar app: no Dock icon and no stolen focus at launch. tao applies both as the app
    // finishes launching, which is before `setup` runs, so they're set on the app up front.
    #[cfg(target_os = "macos")]
    let builder = builder.activate_ignoring_other_apps(false);
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut app = builder.build(tauri::generate_context!()).expect("error while starting Push");
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    app.run(on_run_event);
}
