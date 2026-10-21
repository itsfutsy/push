//! How files reach the queue: launch and single-instance arguments, macOS open
//! events (including the Share extension's inbox fallback), the file picker and captures.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tauri::{AppHandle, Manager, Url};
use tauri_plugin_dialog::DialogExt;

use crate::capture::{self, Shot};
use crate::model::screenshot_name;
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
pub fn launched_hidden(args: &[String], has_tray: bool) -> bool {
    args.iter().any(|a| a == "--capture" || (a == "--autostart" && has_tray))
}

/// Deletes inbox folders older than `max_age`. The Share extension's copies are removed after
/// they upload or are dismissed; failed rows don't survive a restart, so anything this old is
/// left over. Newer ones may belong to a share that is launching Push right now.
pub fn prune_inbox(inbox: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(inbox) else { return };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.modified().is_ok_and(|t| t.elapsed().is_ok_and(|age| age > max_age)) {
            continue;
        }
        let removed =
            if meta.is_dir() { std::fs::remove_dir_all(entry.path()) } else { std::fs::remove_file(entry.path()) };
        if let Err(e) = removed {
            eprintln!("push: couldn’t remove an old inbox folder: {e}");
        }
    }
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
                Some(dir) => Source { path, origin: Origin::Inbox(dir), file_name: None },
                None => Source::file(path),
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
    files.into_iter().map(|path| Source { path, origin: Origin::Inbox(dir.clone()), file_name: None }).collect()
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
            Shot::Done(path) => {
                let name = screenshot_name(chrono::Local::now());
                queue::intake(&app, vec![Source { path, origin: Origin::Capture, file_name: Some(name) }]).await;
            }
            Shot::Cancelled => {}
            Shot::NoPermission => windows::permission_needed(&app),
            Shot::Failed(reason) => windows::capture_failed(&reason),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0F8E0C4B-6B1A-4C1E-9D52-3A5C8F1B2E7D";

    fn ids(url: &str) -> Vec<String> {
        inbox_ids(&Url::parse(url).unwrap())
    }

    #[test]
    fn only_strict_inbox_urls_are_accepted() {
        assert_eq!(ids(&format!("push://upload?id={ID}")), [ID]);
        assert_eq!(ids(&format!("push://upload/?id={}", ID.to_lowercase())), [ID.to_lowercase()]);
        assert!(ids(&format!("push://upload/x?id={ID}")).is_empty());
        assert!(ids(&format!("push://other?id={ID}")).is_empty());
        assert!(ids(&format!("https://upload?id={ID}")).is_empty());
        assert!(ids("push://upload?id=../../../../etc").is_empty());
        assert!(ids("push://upload?id=..%2F..%2F..%2F..%2F..%2F..%2F..%2Fetc%2Fpass").is_empty());
        assert!(ids("push://upload?id=0F8E0C4B-6B1A-4C1E-9D52-3A5C8F1B2E7").is_empty());
        assert!(ids("push://upload?path=/etc/passwd").is_empty());
        assert!(ids("push://upload").is_empty());
    }

    #[test]
    fn inbox_files_stay_inside_the_inbox() {
        let base = std::env::temp_dir().join(format!("push-inbox-{}", crate::model::new_id()));
        let inbox = base.join("inbox");
        let dir = inbox.join(ID);
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("b.png"), b"b").unwrap();
        std::fs::write(dir.join("a.txt"), b"a").unwrap();
        std::fs::write(dir.join(".DS_Store"), b"x").unwrap();
        std::fs::write(dir.join("nested/c.txt"), b"c").unwrap();
        std::fs::write(base.join("secret"), b"s").unwrap();
        std::os::unix::fs::symlink(base.join("secret"), dir.join("link")).unwrap();

        let (found_dir, files) = inbox_files(&inbox, ID).unwrap();
        let real_dir = dir.canonicalize().unwrap();
        assert_eq!(found_dir, real_dir);
        assert_eq!(files, [real_dir.join("a.txt"), real_dir.join("b.png")]);

        // A <uuid> entry that is a symlink to somewhere else is refused.
        let other = "11111111-2222-3333-4444-555555555555";
        std::os::unix::fs::symlink(&base, inbox.join(other)).unwrap();
        assert!(inbox_files(&inbox, other).is_none());
        assert!(inbox_files(&inbox, "22222222-2222-3333-4444-555555555555").is_none());

        assert_eq!(inbox_folder_of(&dir.join("a.txt"), &inbox), Some(real_dir.clone()));
        assert_eq!(inbox_folder_of(&dir.join("nested/c.txt"), &inbox), None);
        assert_eq!(inbox_folder_of(&base.join("secret"), &inbox), None);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn arguments_become_paths() {
        let cwd = Path::new("/home/me");
        assert_eq!(arg_path("shot.png", cwd), Some(PathBuf::from("/home/me/shot.png")));
        assert_eq!(arg_path("/tmp/a b.png", cwd), Some(PathBuf::from("/tmp/a b.png")));
        assert_eq!(arg_path("file:///tmp/a%20b.png", cwd), Some(PathBuf::from("/tmp/a b.png")));
        assert_eq!(arg_path("push://upload?id=x", cwd), None);
        assert_eq!(arg_path("https://example.com/x", cwd), None);
        assert!(launched_hidden(&["--autostart".into()], true));
        assert!(launched_hidden(&["--capture".into()], true));
        assert!(!launched_hidden(&["a.png".into()], true));
        // Without a tray icon a hidden login launch would leave no way in.
        assert!(!launched_hidden(&["--autostart".into()], false));
        assert!(launched_hidden(&["--capture".into()], false));
    }

    #[test]
    fn old_inbox_folders_are_pruned() {
        let inbox = std::env::temp_dir().join(format!("push-prune-{}", crate::model::new_id()));
        let (old, fresh) = (inbox.join(ID), inbox.join("11111111-2222-3333-4444-555555555555"));
        for dir in [&old, &fresh] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("a.png"), b"a").unwrap();
        }
        let two_hours_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
        std::fs::File::open(&old).unwrap().set_modified(two_hours_ago).unwrap();
        prune_inbox(&inbox, Duration::from_secs(3600));
        assert!(!old.exists());
        assert!(fresh.join("a.png").exists());
        prune_inbox(&inbox.join("missing"), Duration::ZERO);
        std::fs::remove_dir_all(inbox).unwrap();
    }

    #[test]
    fn inbox_folders_with_nothing_to_upload_are_removed() {
        let inbox = std::env::temp_dir().join(format!("push-empty-{}", crate::model::new_id())).join("inbox");
        let dir = inbox.join(ID);
        std::fs::create_dir_all(dir.join("Shared Folder")).unwrap();
        std::fs::write(dir.join("Shared Folder/a.png"), b"a").unwrap();
        std::fs::write(dir.join(".DS_Store"), b"x").unwrap();
        assert!(inbox_sources(&inbox, ID).is_empty());
        assert!(!dir.exists());

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), b"a").unwrap();
        let sources = inbox_sources(&inbox, ID);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].origin, Origin::Inbox(dir.canonicalize().unwrap()));
        assert!(dir.exists(), "kept until its files are uploaded");
        std::fs::remove_dir_all(inbox.parent().unwrap()).unwrap();
    }
}
