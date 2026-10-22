//! The menu bar (tray) item. Left click opens the menu everywhere.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::model::View;
use crate::{Core, intake, shortcut, windows};

/// The items whose state follows the app.
pub struct Items {
    capture: MenuItem<Wry>,
    copy_last: MenuItem<Wry>,
}

/// Menu accelerators are display-only hints here; the real shortcut is global.
fn hint(accelerator: &str) -> Option<&str> {
    cfg!(target_os = "macos").then_some(accelerator)
}

pub fn build(app: &AppHandle) -> tauri::Result<Items> {
    let capture = MenuItem::with_id(app, "capture", "Capture Area", true, None::<&str>)?;
    let upload = MenuItem::with_id(app, "upload", "Upload File…", true, None::<&str>)?;
    let copy_last = MenuItem::with_id(app, "copy-last", "Copy Last Link", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open Push", true, None::<&str>)?;
    let library = MenuItem::with_id(app, "library", "Library", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, hint("CmdOrCtrl+,"))?;
    let quit = MenuItem::with_id(app, "quit", "Quit Push", true, hint("CmdOrCtrl+Q"))?;
    let menu = Menu::with_items(
        app,
        &[
            &capture,
            &upload,
            &copy_last,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &library,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    #[cfg(target_os = "macos")]
    let icon = tauri::include_image!("icons/tray.png");
    #[cfg(target_os = "linux")]
    let icon = tauri::include_image!("icons/tray-linux.png");
    // Windows tray icons are in colour, and the taskbar may be light or dark.
    #[cfg(windows)]
    let icon = tauri::include_image!("icons/32x32.png");

    let tray = TrayIconBuilder::with_id("tray")
        .icon(icon)
        .icon_as_template(cfg!(target_os = "macos"))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "capture" => intake::capture(app),
            "upload" => {
                windows::show_main(app);
                intake::pick_files(app);
            }
            "copy-last" => {
                let link = app.state::<Core>().uploads.lock().unwrap().last_link();
                if let Some(link) = link {
                    let _ = app.clipboard().write_text(link);
                }
            }
            "open" => windows::show_main(app),
            "library" => windows::show_library(app),
            "settings" => {
                windows::show_main(app);
                windows::navigate(app, View::Settings);
            }
            "quit" => app.exit(0),
            _ => {}
        });
    #[cfg(not(target_os = "linux"))]
    let tray = tray.tooltip("Push");
    tray.build(app)?;
    Ok(Items { capture, copy_last })
}

/// Whether the desktop shows tray icons at all. Linux desktops without a
/// StatusNotifierWatcher (stock GNOME) accept the icon but never show it.
pub fn host_present() -> bool {
    #[cfg(target_os = "linux")]
    {
        let watcher = || -> zbus::Result<bool> {
            let bus = zbus::blocking::Connection::session()?;
            let dbus = zbus::blocking::fdo::DBusProxy::new(&bus)?;
            Ok(dbus.name_has_owner("org.kde.StatusNotifierWatcher".try_into()?)?)
        };
        watcher().unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    true
}

/// Syncs "Copy Last Link" and the "Capture Area" shortcut hint with the current state.
pub fn refresh(app: &AppHandle) {
    let core = app.state::<Core>();
    let Some(items) = &core.tray else { return };
    let has_done = core.uploads.lock().unwrap().has_done();
    let shortcut = core.settings.lock().unwrap().shortcut.clone().filter(|_| shortcut::supported());
    let _ = items.copy_last.set_enabled(has_done);
    if items.capture.set_accelerator(shortcut.as_deref()).is_err() {
        let _ = items.capture.set_accelerator(None::<&str>);
    }
}
