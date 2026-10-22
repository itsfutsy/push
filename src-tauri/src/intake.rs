//! How files reach the queue: launch and single-instance arguments, macOS open
//! events (including the Share extension's inbox fallback), the file picker and captures.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tauri::{AppHandle, Manager, Url};
use tauri_plugin_dialog::DialogExt;

use crate::capture::{self, Shot};
use crate::queue::{self, Origin, Source};
use crate::{Core, windows};

/// Events that can arrive before `setup` has created the app state.
enum Early {
    Args(Vec<String>, PathBuf),
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Urls(Vec<Url>),
}

static EARLY: Mutex<Vec<Early>> = Mutex::new(Vec::new());

fn dispatch(app: &AppHandle, event: Early) {
    {
        let mut early = EARLY.lock().unwrap();
        if app.try_state::<Core>().is_none() {
            early.push(event);
            return;
        }
    }
    match event {
        Early::Args(args, cwd) => handle_args(app, args, &cwd),
        Early::Urls(urls) => handle_urls(app, urls),
    }
}

/// Handles whatever arrived before the state existed. Call right after managing `Core`.
pub fn drain_early(app: &AppHandle) {
    let early = std::mem::take(&mut *EARLY.lock().unwrap());
    for event in early {
        dispatch(app, event);
    }
}

/// `true` when launched to stay in the background: for a capture hotkey bound to
/// `push --capture`, or as a login item when the tray icon is there to reach Push.
pub fn launched_hidden(args: &[String], tray_visible: bool) -> bool {
    args.iter().any(|a| a == "--capture" || (a == "--autostart" && tray_visible))
}

const INBOX_MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// Deletes inbox folders older than an hour. The Share extension's copies are removed after
/// they upload or are dismissed; failed rows don't survive a restart, so anything this old is
/// left over. Newer ones may belong to a share that is launching Push right now.
pub fn prune_inbox(inbox: &Path) {
    let Ok(entries) = std::fs::read_dir(inbox) else { return };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.modified().is_ok_and(|t| t.elapsed().is_ok_and(|age| age > INBOX_MAX_AGE)) {
            continue;
        }
        let removed =
            if meta.is_dir() { std::fs::remove_dir_all(entry.path()) } else { std::fs::remove_file(entry.path()) };
        if let Err(e) = removed {
            eprintln!("push: couldn’t remove an old inbox folder: {e}");
        }
    }
}

/// macOS installs the Share extension switched off. Turns it on in the background, unless the
/// user has turned it off themselves.
#[cfg(target_os = "macos")]
pub fn enable_share_extension() {
    const ID: &str = "io.github.itsfutsy.push.share";
    let pluginkit = |args: &[&str]| std::process::Command::new("/usr/bin/pluginkit").args(args).output();
    std::thread::spawn(move || {
        // Right after an install, macOS can take a moment to find the extension.
        for _ in 0..30 {
            let Ok(out) = pluginkit(&["-m", "-i", ID]) else { return };
            // pluginkit marks it "+" when on, "-" when the user turned it off, and nothing when never set.
            match String::from_utf8_lossy(&out.stdout).trim_start().chars().next() {
                Some('+' | '-') => return,
                Some(_) => {
                    let _ = pluginkit(&["-e", "use", "-i", ID]);
                    return;
                }
                None => std::thread::sleep(Duration::from_secs(1)),
            }
        }
    });
}

/// The single-instance callback: another `push` process forwarded its arguments.
pub fn forwarded(app: &AppHandle, argv: Vec<String>, cwd: String) {
    let args: Vec<String> = argv.into_iter().skip(1).collect();
    if args.is_empty() {
        windows::show_main(app);
        return;
    }
    dispatch(app, Early::Args(args, PathBuf::from(cwd)));
}

pub fn launch_args(app: &AppHandle, args: Vec<String>) {
    let cwd = std::env::current_dir().unwrap_or_default();
    dispatch(app, Early::Args(args, cwd));
}

/// An argument naming a file: a path (relative to `cwd`) or a `file://` URL. Other URLs are ignored.
fn arg_path(arg: &str, cwd: &Path) -> Option<PathBuf> {
    if arg.contains("://") {
        return Url::parse(arg).ok().filter(|u| u.scheme() == "file")?.to_file_path().ok();
    }
    Some(cwd.join(arg))
}

fn handle_args(app: &AppHandle, args: Vec<String>, cwd: &Path) {
    let mut paths = Vec::new();
    for arg in &args {
        match arg.as_str() {
            "--capture" => capture(app),
            a if a.starts_with('-') => {} // --autostart, and anything a launcher adds
            a => paths.extend(arg_path(a, cwd)),
        }
    }
    add_files(app, paths);
}

/// Queues one intake event (one clipboard batch): `paths`, plus the files of the given inbox
/// folders. File system checks run off the calling (often main) thread.
fn add(app: &AppHandle, paths: Vec<PathBuf>, inbox_ids: Vec<String>) {
    if paths.is_empty() && inbox_ids.is_empty() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let inbox = app.state::<Core>().cache_dir.join("inbox");
        // Plain paths, except files the Share extension copied into the inbox (cleaned up after upload).
        let mut sources: Vec<Source> = paths
            .into_iter()
            .map(|path| match inbox_folder_of(&path, &inbox) {
                Some(dir) => Source { path, origin: Origin::Inbox(dir) },
                None => Source { path, origin: Origin::File },
            })
            .collect();
        for id in inbox_ids {
            sources.extend(inbox_sources(&inbox, &id));
        }
        queue::intake(&app, sources).await;
    });
}

/// Files from a drop, the picker or the command line.
pub fn add_files(app: &AppHandle, paths: Vec<PathBuf>) {
    add(app, paths, Vec::new());
}

/// macOS `RunEvent::Opened`: Finder, the Dock, Open With and the Share extension.
#[cfg(target_os = "macos")]
pub fn opened(app: &AppHandle, urls: Vec<Url>) {
    dispatch(app, Early::Urls(urls));
}

/// `file://` URLs are uploaded; `push://upload?id=<uuid>` uploads that inbox folder; anything else is ignored.
fn handle_urls(app: &AppHandle, urls: Vec<Url>) {
    let mut paths = Vec::new();
    let mut ids = Vec::new();
    for url in urls {
        match url.scheme() {
            "file" => paths.extend(url.to_file_path().ok()),
            "push" => ids.extend(inbox_ids(&url)),
            _ => {}
        }
    }
    add(app, paths, ids);
}

/// The ids in `push://upload?id=<uuid>`. Anything else yields nothing.
fn inbox_ids(url: &Url) -> Vec<String> {
    if url.scheme() != "push" || url.host_str() != Some("upload") || !matches!(url.path(), "" | "/") {
        return Vec::new();
    }
    url.query_pairs()
        .filter(|(key, _)| key == "id")
        .map(|(_, id)| id.into_owned())
        .filter(|id| id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        .collect()
}

/// The regular files directly inside `inbox/<id>/`, canonicalised and checked to really be
/// there (no symlinks out of the inbox).
fn inbox_files(inbox: &Path, id: &str) -> Option<(PathBuf, Vec<PathBuf>)> {
    let root = inbox.canonicalize().ok()?;
    let dir = root.join(id).canonicalize().ok()?;
    if dir.parent() != Some(root.as_path()) {
        return None;
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|entry| entry.path().canonicalize().ok())
        .filter(|path| path.parent() == Some(dir.as_path()) && path.is_file())
        .collect();
    files.sort();
    Some((dir, files))
}

/// The uploads for `push://upload?id=<id>`. A folder with nothing to upload (a shared folder,
/// say) is deleted right away, since no upload would ever clean it up.
fn inbox_sources(inbox: &Path, id: &str) -> Vec<Source> {
    let Some((dir, files)) = inbox_files(inbox, id) else { return Vec::new() };
    if files.is_empty()
        && let Err(e) = std::fs::remove_dir_all(&dir)
    {
        eprintln!("push: couldn’t remove an empty inbox folder: {e}");
    }
    files.into_iter().map(|path| Source { path, origin: Origin::Inbox(dir.clone()) }).collect()
}

/// `inbox/<id>` if `path` is a file the Share extension copied there.
fn inbox_folder_of(path: &Path, inbox: &Path) -> Option<PathBuf> {
    let root = inbox.canonicalize().ok()?;
    let dir = path.canonicalize().ok()?.parent()?.to_path_buf();
    (dir.parent() == Some(root.as_path())).then_some(dir)
}

/// The native multi-file picker.
pub fn pick_files(app: &AppHandle) {
    let mut dialog = app.dialog().file();
    if let Some(main) = app.get_webview_window("main").filter(|w| w.is_visible().unwrap_or(false)) {
        dialog = dialog.set_parent(&main);
    }
    let handle = app.clone();
    dialog.pick_files(move |picked| {
        let paths = picked.unwrap_or_default().into_iter().filter_map(|p| p.into_path().ok()).collect();
        add_files(&handle, paths);
    });
}

/// Interactive region capture, then upload. Ignored while a capture is already running.
pub fn capture(app: &AppHandle) {
    if app.state::<Core>().capturing.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        windows::hide_hud(&app);
        let core = app.state::<Core>();
        let shot = capture::capture_region(&core.cache_dir.join("captures")).await;
        core.capturing.store(false, Ordering::SeqCst);
        if !matches!(shot, Shot::NoPermission) {
            windows::capture_ended(&app);
        }
        match shot {
            Shot::Done(path) => queue::intake(&app, vec![Source { path, origin: Origin::Capture }]).await,
            Shot::Cancelled => {}
            Shot::NoPermission => windows::permission_needed(&app),
            Shot::Failed(reason) => windows::capture_failed(&reason),
        }
    });
}
