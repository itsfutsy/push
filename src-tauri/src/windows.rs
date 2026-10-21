//! The main window, the Library, the macOS HUD and notifications on Linux and Windows.
//!
//! Never call these while holding a `Core` lock: window calls block on the main thread.

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::model::{HudPayload, Navigate, Upload, View};

pub fn show_main(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn hide_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    menu_bar_only_unless(app, "library");
}

/// Shows the Library, creating it the first time. A new one stays hidden until its page has
/// loaded, which calls this again.
pub fn show_library(app: &AppHandle) {
    let Some(window) = app.get_webview_window("library") else {
        if let Err(e) = build_library(app) {
            eprintln!("push: couldn’t open the Library: {e}");
        }
        return;
    };
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

fn build_library(app: &AppHandle) -> tauri::Result<()> {
    let builder = WebviewWindowBuilder::new(app, "library", WebviewUrl::App("library.html".into()))
        .title("Library")
        .inner_size(880.0, 620.0)
        .min_inner_size(600.0, 440.0)
        .center()
        .visible(false);
    // The page draws a 52 pt toolbar; the traffic lights sit centred in it.
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .traffic_light_position(tauri::LogicalPosition::new(18.0, 19.0))
        .allow_link_preview(false);
    builder.build().map(drop)
}

/// The Library closed (and was destroyed, freeing its images).
pub fn library_closed(app: &AppHandle) {
    menu_bar_only_unless(app, "main");
}

/// Back to a menu bar app with no Dock icon (macOS), unless window `other` is still showing.
fn menu_bar_only_unless(app: &AppHandle, other: &str) {
    #[cfg(target_os = "macos")]
    if !app.get_webview_window(other).is_some_and(|w| w.is_visible().unwrap_or(false)) {
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, other);
}

pub fn navigate(app: &AppHandle, view: View) {
    let _ = app.emit("navigate", Navigate { view });
}

/// The main window is showing and has focus, so it already tells the user everything.
#[cfg(target_os = "macos")]
fn main_active(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .is_some_and(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false))
}

/// Reports an upload outside the main window. `started` is true when it begins (or fails
/// right away); later calls only update a HUD that already shows it.
pub fn announce(app: &AppHandle, upload: &Upload, started: bool) {
    #[cfg(target_os = "macos")]
    {
        if started && !main_active(app) {
            hud::show(app, HudPayload::Upload { upload: upload.clone() });
        } else {
            hud::update(app, upload);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = started;
        if matches!(upload.status, crate::model::Status::Done | crate::model::Status::Failed) {
            let focused = app.get_webview_window("main").is_some_and(|w| w.is_focused().unwrap_or(false));
            if !focused {
                notify::upload(app, upload);
            }
        }
    }
}

/// The item is gone: stop showing it.
pub fn forget(app: &AppHandle, id: &str) {
    let hud = app.state::<crate::Core>().hud.lock().unwrap().as_ref().map(|p| matches!(p, HudPayload::Upload { upload } if upload.id == id));
    if hud == Some(true) {
        hide_hud(app);
    }
}

/// Screen Recording permission is missing (macOS). Shown even over the main window, which
/// has no other place for it.
pub fn permission_needed(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    hud::show(app, HudPayload::Permission);
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// A capture tool failed. Linux and Windows show a notification; the macOS HUD has no state for this.
pub fn capture_failed(reason: &str) {
    eprintln!("push: capture failed: {reason}");
    #[cfg(not(target_os = "macos"))]
    notify::show("Couldn’t capture the screen".into(), reason.to_owned());
}

pub fn hud_payload(app: &AppHandle) -> Option<HudPayload> {
    app.state::<crate::Core>().hud.lock().unwrap().clone()
}

/// Whether the pointer is over the HUD. Its page can't tell: the window is never key, so
/// WebKit sends it no hover events.
pub fn hud_hovered(app: &AppHandle) -> bool {
    #[cfg(target_os = "macos")]
    {
        hud::hovered(app)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        false
    }
}

pub fn hide_hud(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    hud::hide(app);
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// A capture just ended: show what the HUD was given while it had to stay out of the shot.
pub fn capture_ended(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    if let Some(payload) = hud_payload(app) {
        hud::show(app, payload);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// Shows a freshly created HUD once its page has loaded (it fetches the payload itself).
pub fn hud_loaded(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let pending = app.state::<crate::Core>().hud.lock().unwrap().is_some();
        if pending && let Some(window) = app.get_webview_window("hud") {
            hud::reveal(app, &window);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

#[cfg(target_os = "macos")]
mod hud {
    use std::sync::atomic::Ordering;

    use tauri::window::{Effect, EffectState, EffectsBuilder};
    use tauri::{AppHandle, Emitter, LogicalPosition, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

    use crate::Core;
    use crate::model::{HudPayload, Upload};

    const WIDTH: f64 = 300.0;
    const HEIGHT: f64 = 64.0;
    /// Distance from the bottom-right corner of the work area, in points.
    const MARGIN: f64 = 20.0;

    fn build(app: &AppHandle) -> tauri::Result<WebviewWindow> {
        WebviewWindowBuilder::new(app, "hud", WebviewUrl::App("hud.html".into()))
            .title("Push")
            .inner_size(WIDTH, HEIGHT)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .decorations(false)
            .transparent(true)
            .shadow(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible_on_all_workspaces(true)
            .focusable(false)
            .focused(false)
            .accept_first_mouse(true)
            .visible(false)
            .effects(
                EffectsBuilder::new()
                    .effects([Effect::LiquidGlassRegular, Effect::HudWindow])
                    .radius(18.0)
                    .state(EffectState::Active)
                    .build(),
            )
            .build()
    }

    /// Bottom-right of the monitor under the pointer.
    fn place(app: &AppHandle, window: &WebviewWindow) -> tauri::Result<()> {
        let primary = app.primary_monitor()?;
        // tao reports the pointer in pixels of the primary display but finds monitors by points.
        let scale = primary.as_ref().map_or(1.0, |m| m.scale_factor());
        let cursor = app.cursor_position()?;
        let Some(monitor) = app.monitor_from_point(cursor.x / scale, cursor.y / scale)?.or(primary) else {
            return Ok(());
        };
        let (scale, area) = (monitor.scale_factor(), monitor.work_area());
        let x = (area.position.x as f64 + area.size.width as f64) / scale - WIDTH - MARGIN;
        let y = (area.position.y as f64 + area.size.height as f64) / scale - HEIGHT - MARGIN;
        window.set_position(LogicalPosition::new(x, y))
    }

    /// Whether the pointer is over the HUD. Same coordinate rules as `place`: the pointer comes in
    /// pixels of the primary display, the window in pixels of its own.
    pub fn hovered(app: &AppHandle) -> bool {
        let Some(window) = app.get_webview_window("hud") else { return false };
        if !window.is_visible().unwrap_or(false) {
            return false;
        }
        let primary = app.primary_monitor().ok().flatten().map_or(1.0, |m| m.scale_factor());
        let (Ok(cursor), Ok(pos), Ok(size), Ok(scale)) =
            (app.cursor_position(), window.outer_position(), window.outer_size(), window.scale_factor())
        else {
            return false;
        };
        let (pos, size) = (pos.to_logical::<f64>(scale), size.to_logical::<f64>(scale));
        let (x, y) = (cursor.x / primary, cursor.y / primary);
        (pos.x..pos.x + size.width).contains(&x) && (pos.y..pos.y + size.height).contains(&y)
    }

    /// Shows the HUD, except during a capture, where it would end up in the screenshot
    /// (`capture_ended` shows it afterwards). The payload is stored before this runs.
    pub fn reveal(app: &AppHandle, window: &WebviewWindow) {
        if app.state::<Core>().capturing.load(Ordering::SeqCst) {
            return;
        }
        if let Err(e) = place(app, window) {
            eprintln!("push: couldn’t place the HUD: {e}");
        }
        let _ = window.show();
    }

    pub fn show(app: &AppHandle, payload: HudPayload) {
        *app.state::<Core>().hud.lock().unwrap() = Some(payload.clone());
        match app.get_webview_window("hud") {
            Some(window) => {
                let _ = app.emit_to("hud", "hud", payload);
                reveal(app, &window);
            }
            // The new page asks for the payload with `hud_ready`; `hud_loaded` then shows it.
            None => {
                if let Err(e) = build(app) {
                    eprintln!("push: couldn’t create the HUD: {e}");
                }
            }
        }
    }

    /// Refreshes the HUD if it currently shows this upload.
    pub fn update(app: &AppHandle, upload: &Upload) {
        let core = app.state::<Core>();
        let payload = {
            let mut hud = core.hud.lock().unwrap();
            match &mut *hud {
                Some(HudPayload::Upload { upload: shown }) if shown.id == upload.id => {
                    *shown = upload.clone();
                    hud.clone()
                }
                _ => None,
            }
        };
        if let Some(payload) = payload {
            let _ = app.emit_to("hud", "hud", payload);
        }
    }

    pub fn hide(app: &AppHandle) {
        *app.state::<Core>().hud.lock().unwrap() = None;
        if let Some(window) = app.get_webview_window("hud") {
            let _ = window.hide();
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod notify {
    use tauri::{AppHandle, Manager};

    use crate::model::{ErrorKind, Upload};

    pub fn show(summary: String, body: String) {
        // notify-rust blocks on D-Bus (and on WinRT).
        std::thread::spawn(move || {
            let mut notification = notify_rust::Notification::new();
            notification.appname("Push").icon("push").summary(&summary).body(&body);
            // Toasts need the AppUserModelID the installer puts on the Start menu shortcut. Dev
            // builds have none, so they borrow PowerShell's (notify-rust's default).
            #[cfg(windows)]
            if !cfg!(debug_assertions) {
                notification.app_id("io.github.itsfutsy.push");
            }
            if let Err(e) = notification.show() {
                eprintln!("push: notification failed: {e}");
            }
        });
    }

    pub fn upload(app: &AppHandle, upload: &Upload) {
        let Some(error) = &upload.error else {
            return show("Link copied".into(), upload.url.clone());
        };
        let title = match error.kind {
            ErrorKind::Auth => "Upload token not accepted".to_owned(),
            ErrorKind::Network => {
                let server = app.state::<crate::Core>().settings.lock().unwrap().server_url.clone();
                let host = tauri::Url::parse(&server).ok().and_then(|u| u.host_str().map(str::to_owned));
                format!("Can’t reach {}", host.as_deref().unwrap_or("the server"))
            }
            ErrorKind::Server => "Upload failed".to_owned(),
            ErrorKind::Unsupported => format!("Can’t upload “{}”", upload.file_name),
            ErrorKind::Missing => "File not found".to_owned(),
        };
        show(title, error.message.clone());
    }
}
