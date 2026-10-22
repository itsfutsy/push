//! The upload list and its single FIFO worker: intake, progress, completion,
//! clipboard batches, cancel, retry and delete.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use image::DynamicImage;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tokio::sync::{mpsc, oneshot};

use crate::model::{
    ErrorKind, Kind, Progress, Status, Upload, UploadError, kind_for, new_id, now_ms, sanitize_ext,
};
use crate::net::{Api, OnSent, is_web_url, validate_server_url};
use crate::transfer::{self, Sent};
use crate::{Core, preview, store, tray, windows};

/// Longest edge of the inline thumbnail images get while they upload, in pixels (the HUD shows
/// it at 48 pt on Retina).
const THUMB_EDGE: u32 = 192;
const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// A cancelled upload is deleted from the server right away and once more after this long:
/// bytes the OS had already accepted keep going out after the abort, and on a slow link
/// they can complete the file on the server well after the first delete.
const DISCARD_AGAIN: Duration = Duration::from_secs(60);

pub const GONE: &str = "That upload isn’t there anymore.";
pub const NO_TOKEN: &str = "Add your upload token in Settings.";
pub const BAD_SERVER: &str = "Check the server URL in Settings.";
const OTHER_SERVER: &str = "This upload is on a different server.";

/// Where a file came from, which decides what to clean up after it uploads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    File,
    /// A screenshot in the captures folder: deleted after upload.
    Capture,
    /// A file in `inbox/<uuid>/`: the folder is deleted once nothing in it is pending.
    Inbox(PathBuf),
}

pub struct Source {
    pub path: PathBuf,
    pub origin: Origin,
}

struct Item {
    upload: Upload,
    path: Option<PathBuf>,
    origin: Origin,
    batch: u64,
    last_attempt: Option<Stamp>,
}

impl Item {
    fn may_be_on_server(&self) -> bool {
        self.last_attempt.is_some() || self.upload.status == Status::Uploading
    }
}

/// Size and modification time of a file, to notice it changed between attempts.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self { size: meta.len(), modified: meta.modified().ok() }
    }
}

#[derive(Default)]
pub struct Uploads {
    items: Vec<Item>,
    counter: u64,
    /// The uploading item and the switch that cancels it.
    current: Option<(String, oneshot::Sender<()>)>,
}

impl Uploads {
    /// Starts from the persisted list (newest first).
    pub fn from_done(done: Vec<Upload>) -> Self {
        Self {
            items: done
                .into_iter()
                .rev()
                .map(|upload| Item { upload, path: None, origin: Origin::File, batch: 0, last_attempt: None })
                .collect(),
            ..Self::default()
        }
    }

    fn next(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    /// In-flight and failed first, then done; newest first within each group.
    fn sorted(&self) -> Vec<&Item> {
        let mut items: Vec<&Item> = self.items.iter().rev().collect();
        items.sort_by_key(|i| (i.upload.status == Status::Done, Reverse(i.upload.created_at)));
        items
    }

    /// The list as the UI shows it (`AppInfo.uploads`).
    pub fn list(&self) -> Vec<Upload> {
        self.sorted().into_iter().map(|i| i.upload.clone()).collect()
    }

    /// Finished uploads, newest first (what uploads.json stores).
    fn done(&self) -> Vec<Upload> {
        self.sorted().into_iter().filter(|i| i.upload.status == Status::Done).map(|i| i.upload.clone()).collect()
    }

    pub fn last_link(&self) -> Option<String> {
        self.sorted().into_iter().find(|i| i.upload.status == Status::Done).map(|i| i.upload.url.clone())
    }

    pub fn has_done(&self) -> bool {
        self.items.iter().any(|i| i.upload.status == Status::Done)
    }

    fn get(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.upload.id == id)
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut Item> {
        self.items.iter_mut().find(|i| i.upload.id == id)
    }

    fn remove(&mut self, id: &str) -> Option<Item> {
        let index = self.items.iter().position(|i| i.upload.id == id)?;
        Some(self.items.remove(index))
    }

    /// Adds a batch of new items; returns the ids to queue.
    fn add(&mut self, uploads: Vec<(Upload, PathBuf, Origin)>) -> Vec<String> {
        let batch = self.next();
        let mut queued = Vec::new();
        for (upload, path, origin) in uploads {
            if upload.status == Status::Queued {
                queued.push(upload.id.clone());
            }
            self.items.push(Item { upload, path: Some(path), origin, batch, last_attempt: None });
        }
        queued
    }

    /// The clipboard text: every finished link of `batch`, in intake order.
    fn links(&self, batch: u64) -> String {
        let links: Vec<&str> = self
            .items
            .iter()
            .filter(|i| i.batch == batch && i.upload.status == Status::Done)
            .map(|i| i.upload.url.as_str())
            .collect();
        links.join("\n")
    }

    fn trim_done(&mut self) {
        let keep: HashSet<String> =
            self.done().into_iter().take(store::MAX_DONE).map(|u| u.id).collect();
        self.items.retain(|i| i.upload.status != Status::Done || keep.contains(&i.upload.id));
    }

    fn cleanup_for(&self, path: Option<PathBuf>, origin: &Origin) -> Cleanup {
        match origin {
            Origin::File => Cleanup::None,
            Origin::Capture => path.map_or(Cleanup::None, Cleanup::File),
            Origin::Inbox(dir) => {
                let in_use = self
                    .items
                    .iter()
                    .any(|i| i.upload.status != Status::Done && i.origin == Origin::Inbox(dir.clone()));
                if in_use { Cleanup::None } else { Cleanup::Dir(dir.clone()) }
            }
        }
    }
}

enum Cleanup {
    None,
    File(PathBuf),
    Dir(PathBuf),
}

impl Cleanup {
    fn run(self) {
        let result = match &self {
            Cleanup::None => return,
            Cleanup::File(path) => std::fs::remove_file(path),
            Cleanup::Dir(dir) => std::fs::remove_dir_all(dir),
        };
        if let Err(e) = result
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("push: cleanup failed: {e}");
        }
    }
}

fn unsupported(message: &str) -> UploadError {
    UploadError::new(ErrorKind::Unsupported, message)
}

fn missing(message: &str) -> UploadError {
    UploadError::new(ErrorKind::Missing, message)
}

fn open_error(e: &std::io::Error) -> UploadError {
    if e.kind() == std::io::ErrorKind::NotFound {
        missing("The file is no longer there.")
    } else {
        missing("Push can’t read this file.")
    }
}

async fn uploadable(path: &Path) -> Result<u64, UploadError> {
    match tokio::fs::metadata(path).await {
        Ok(m) if m.is_dir() => Err(unsupported("Folders can’t be uploaded.")),
        Ok(m) if !m.is_file() => Err(unsupported("Only files can be uploaded.")),
        Ok(m) if m.len() == 0 => Err(unsupported("This file is empty.")),
        Ok(m) => Ok(m.len()),
        Err(e) => Err(open_error(&e)),
    }
}

/// A small PNG `data:` URL. It is resent with every `uploads` event while the item is in
/// flight, so it must stay tiny.
async fn thumbnail(img: DynamicImage) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut png = std::io::Cursor::new(Vec::new());
        img.thumbnail(THUMB_EDGE, THUMB_EDGE).write_to(&mut png, image::ImageFormat::Png).ok()?;
        Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png.into_inner())))
    })
    .await
    .ok()
    .flatten()
}

/// The remote extension in a name like `Xc3nzK9qPa.png`.
fn ext_of(name: &str) -> &str {
    name.rsplit_once('.').map_or("bin", |(_, ext)| ext)
}

/// Makes a queued upload's Library preview from its file and, for an image, shows a thumbnail
/// of it while it uploads, unless the item has moved on.
async fn add_thumbnail(app: &AppHandle, id: &str) {
    let core = app.state::<Core>();
    let source = core
        .uploads
        .lock()
        .unwrap()
        .get(id)
        .and_then(|i| Some((i.path.clone()?, i.upload.name.clone(), i.upload.size, i.upload.kind)));
    let Some((path, name, size, kind)) = source else { return };
    let scratch = core.cache_dir.join("quicklook");
    let Some(img) = preview::make(&core.previews_dir, &scratch, id, &path, ext_of(&name), size).await else { return };
    let cancelled = core.uploads.lock().unwrap().get(id).is_none();
    if cancelled {
        return preview::remove(&core.previews_dir, id);
    }
    if kind != Kind::Image {
        return;
    }
    let Some(thumb) = thumbnail(img).await else { return };
    let upload = {
        let mut uploads = core.uploads.lock().unwrap();
        let in_flight = |i: &&mut Item| matches!(i.upload.status, Status::Queued | Status::Uploading);
        let Some(item) = uploads.get_mut(id).filter(in_flight) else { return };
        item.upload.thumb = Some(thumb);
        item.upload.clone()
    };
    emit_uploads(app);
    windows::announce(app, &upload, false);
}

async fn prepare(source: Source) -> (Upload, PathBuf, Origin) {
    let Source { path, origin } = source;
    let id = new_id();
    let ext = sanitize_ext(&path);
    let file_name = match origin {
        // Like macOS names its own screenshots: "Screenshot 2026-10-21 at 14.03.22.png".
        Origin::Capture => chrono::Local::now().format("Screenshot %Y-%m-%d at %H.%M.%S.png").to_string(),
        _ => path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()),
    };
    let (size, error) = match uploadable(&path).await {
        Ok(size) => (size, None),
        Err(e) => (0, Some(e)),
    };
    let upload = Upload {
        name: format!("{id}.{ext}"),
        id,
        url: String::new(),
        file_name,
        size,
        kind: kind_for(&ext),
        created_at: now_ms(),
        status: if error.is_some() { Status::Failed } else { Status::Queued },
        sent: 0,
        error,
        thumb: None,
    };
    (upload, path, origin)
}

pub fn emit_uploads(app: &AppHandle) {
    let list = app.state::<Core>().uploads.lock().unwrap().list();
    let _ = app.emit("uploads", list);
}

fn persist(app: &AppHandle) {
    let core = app.state::<Core>();
    let _guard = core.persist.lock().unwrap();
    let done = core.uploads.lock().unwrap().done();
    if let Err(e) = store::save_uploads(&core.uploads_path, &done) {
        eprintln!("push: couldn’t save uploads.json: {e}");
    }
}

/// Adds files from one intake event (one clipboard batch) and queues them in order. Image
/// previews follow once decoded, so a big image never holds up the list or the queue.
pub async fn intake(app: &AppHandle, sources: Vec<Source>) {
    if sources.is_empty() {
        return;
    }
    let mut prepared = Vec::with_capacity(sources.len());
    for source in sources {
        prepared.push(prepare(source).await);
    }
    let failed: Vec<Upload> =
        prepared.iter().filter(|(u, ..)| u.status == Status::Failed).map(|(u, ..)| u.clone()).collect();
    let core = app.state::<Core>();
    let queued = core.uploads.lock().unwrap().add(prepared);
    for id in &queued {
        let _ = core.jobs.send(id.clone());
    }
    emit_uploads(app);
    for upload in &failed {
        windows::announce(app, upload, true);
    }
    for id in &queued {
        add_thumbnail(app, id).await;
    }
}

struct Job {
    id: String,
    name: String,
    path: PathBuf,
    last_attempt: Option<Stamp>,
}

/// Runs queued uploads one at a time, in order.
pub async fn worker(app: AppHandle, mut jobs: mpsc::UnboundedReceiver<String>) {
    while let Some(id) = jobs.recv().await {
        let core = app.state::<Core>();
        let (cancel, cancelled) = oneshot::channel();
        let (job, upload) = {
            let mut uploads = core.uploads.lock().unwrap();
            let Some(item) = uploads.get_mut(&id).filter(|i| i.upload.status == Status::Queued) else { continue };
            let Some(path) = item.path.clone() else { continue };
            item.upload.status = Status::Uploading;
            item.upload.sent = 0;
            let job = Job { id: id.clone(), name: item.upload.name.clone(), path, last_attempt: item.last_attempt };
            let upload = item.upload.clone();
            uploads.current = Some((id.clone(), cancel));
            (job, upload)
        };
        emit_uploads(&app);
        windows::announce(&app, &upload, true);
        let result = tokio::select! {
            result = run(&app, &job) => Some(result),
            _ = cancelled => None,
        };
        core.uploads.lock().unwrap().current = None;
        if let Some(result) = result {
            finish(&app, &id, result);
        }
    }
}

fn progress_reporter(app: &AppHandle, id: &str, total: u64) -> OnSent {
    let (app, id) = (app.clone(), id.to_owned());
    let last = Mutex::new(None::<Instant>);
    Arc::new(move |sent| {
        let now = Instant::now();
        {
            let mut last = last.lock().unwrap();
            if sent < total && last.is_some_and(|t| now.duration_since(t) < PROGRESS_EVERY) {
                return;
            }
            *last = Some(now);
        }
        if let Some(item) = app.state::<Core>().uploads.lock().unwrap().get_mut(&id) {
            item.upload.sent = sent;
        }
        let _ = app.emit("upload-progress", Progress { id: id.clone(), sent, total });
    })
}

/// Uploads one file and returns its public URL and size.
async fn run(app: &AppHandle, job: &Job) -> Result<(String, u64), UploadError> {
    let core = app.state::<Core>();
    let token = match core.token.get().await {
        Ok(Some(token)) => token,
        Ok(None) => return Err(UploadError::new(ErrorKind::Auth, NO_TOKEN)),
        Err(e) => return Err(UploadError::new(ErrorKind::Auth, e)),
    };
    let server = core.settings.lock().unwrap().server_url.clone();
    let base = validate_server_url(&server).map_err(|_| UploadError::new(ErrorKind::Server, BAD_SERVER))?;
    let mut file = tokio::fs::File::open(&job.path).await.map_err(|e| open_error(&e))?;
    let meta = file.metadata().await.map_err(|e| open_error(&e))?;
    let total = meta.len();
    if total == 0 {
        return Err(unsupported("This file is empty."));
    }
    let api = Api::new(core.http.clone(), base.clone(), token);
    let stamp = Stamp::of(&meta);
    let resume = job.last_attempt == Some(stamp);
    let changed = job.last_attempt.is_some() && !resume;
    if changed {
        // Resuming would splice the old version's bytes into the new one.
        match api.delete(&job.name).await {
            Ok(204 | 404) => {}
            Ok(401) => return Err(transfer::auth_error()),
            Ok(_) => return Err(UploadError::new(ErrorKind::Server, "The server ran into a problem.")),
            Err(_) => return Err(transfer::network_error()),
        }
    }
    if let Some(item) = core.uploads.lock().unwrap().get_mut(&job.id) {
        item.last_attempt = Some(stamp);
    }
    let progress = progress_reporter(app, &job.id, total);
    let url = match transfer::send(&api, &mut file, &job.name, total, resume, &progress).await? {
        Sent::Url(url) if is_web_url(&url) => url,
        Sent::Url(_) => return Err(UploadError::new(ErrorKind::Server, "The server sent an invalid link.")),
        Sent::Exists => format!("{base}/i/{}", job.name),
    };
    Ok((url, total))
}

fn finish(app: &AppHandle, id: &str, result: Result<(String, u64), UploadError>) {
    let core = app.state::<Core>();
    let (upload, clipboard, cleanup) = {
        let mut uploads = core.uploads.lock().unwrap();
        let Some(item) = uploads.get_mut(id) else { return };
        item.upload.thumb = None;
        let mut clipboard = None;
        let mut done = None;
        match result {
            Ok((url, size)) => {
                item.upload.status = Status::Done;
                item.upload.url = url;
                item.upload.size = size;
                item.upload.sent = size;
                item.upload.error = None;
                done = Some((item.batch, item.path.take(), item.origin.clone()));
            }
            Err(error) => {
                item.upload.status = Status::Failed;
                item.upload.error = Some(error);
            }
        }
        let upload = item.upload.clone();
        let mut cleanup = Cleanup::None;
        if let Some((batch, path, origin)) = done {
            clipboard = Some(uploads.links(batch));
            cleanup = uploads.cleanup_for(path, &origin);
            uploads.trim_done();
        }
        (upload, clipboard, cleanup)
    };
    if let Some(text) = clipboard {
        if let Err(e) = app.clipboard().write_text(text) {
            eprintln!("push: couldn’t write the clipboard: {e}");
        }
        persist(app);
        tray::refresh(app);
    }
    cleanup.run();
    emit_uploads(app);
    windows::announce(app, &upload, false);
}

/// Cancels a queued or uploading item, or dismisses a failed one.
pub fn cancel(app: &AppHandle, id: &str) -> Result<(), String> {
    let core = app.state::<Core>();
    let (cleanup, on_server) = {
        let mut uploads = core.uploads.lock().unwrap();
        match uploads.get(id) {
            None => return Err(GONE.into()),
            Some(item) if item.upload.status == Status::Done => return Err("This upload is already done.".into()),
            Some(_) => {}
        }
        let item = uploads.remove(id).expect("item exists");
        if let Some((_, cancel)) = uploads.current.take_if(|(current, _)| current == id) {
            let _ = cancel.send(());
        }
        let on_server = item.may_be_on_server().then(|| item.upload.name.clone());
        (uploads.cleanup_for(item.path, &item.origin), on_server)
    };
    cleanup.run();
    preview::remove(&core.previews_dir, id);
    if let Some(name) = on_server {
        discard(app, name);
    }
    emit_uploads(app);
    windows::forget(app, id);
    Ok(())
}

/// Best effort: deletes what a cancelled or dismissed upload left on the server, so a file
/// that completes after the abort doesn't stay public with no row left to delete it from.
fn discard(app: &AppHandle, name: String) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        for delay in [Duration::ZERO, DISCARD_AGAIN] {
            tokio::time::sleep(delay).await;
            let core = app.state::<Core>();
            let Ok(Some(token)) = core.token.get().await else { return };
            let server = core.settings.lock().unwrap().server_url.clone();
            let Ok(base) = validate_server_url(&server) else { return };
            let _ = Api::new(core.http.clone(), base, token).delete(&name).await;
        }
    });
}

/// Queues a failed item again (same id). The next attempt resumes whatever reached the
/// server, or starts over if the file changed.
pub async fn retry(app: &AppHandle, id: &str) -> Result<(), String> {
    let core = app.state::<Core>();
    let path = {
        let uploads = core.uploads.lock().unwrap();
        let item = uploads.get(id).ok_or(GONE)?;
        if item.upload.status != Status::Failed {
            return Ok(());
        }
        item.path.clone().ok_or("The file is no longer there.")?
    };
    let size = uploadable(&path).await.map_err(|e| e.message)?;
    {
        let mut uploads = core.uploads.lock().unwrap();
        let batch = uploads.next();
        let item = uploads.get_mut(id).filter(|i| i.upload.status == Status::Failed).ok_or(GONE)?;
        item.upload.status = Status::Queued;
        item.upload.error = None;
        item.upload.sent = 0;
        item.upload.size = size;
        item.batch = batch;
    }
    let _ = core.jobs.send(id.to_owned());
    emit_uploads(app);
    add_thumbnail(app, id).await;
    Ok(())
}

/// The API base a finished upload was made to: its link is `{base}/i/{name}`.
fn base_of(upload: &Upload) -> Option<String> {
    let base = upload.url.strip_suffix(&format!("/i/{}", upload.name))?;
    validate_server_url(base).ok()
}

/// Deletes a finished upload from the server it was made to, then from the list. Anything
/// else is cancelled.
pub async fn delete(app: &AppHandle, id: &str) -> Result<(), String> {
    let core = app.state::<Core>();
    let (name, home) = {
        let uploads = core.uploads.lock().unwrap();
        let item = uploads.get(id).ok_or(GONE)?;
        if item.upload.status != Status::Done {
            drop(uploads);
            return cancel(app, id);
        }
        (item.upload.name.clone(), base_of(&item.upload))
    };
    let token = core.token.get().await?.ok_or(NO_TOKEN)?;
    let server = core.settings.lock().unwrap().server_url.clone();
    let current = validate_server_url(&server).ok();
    // A 404 only means "already deleted" from the server that is set now and made the upload.
    let here = home.is_some() && home == current;
    let base = home.or(current).ok_or(BAD_SERVER)?;
    match Api::new(core.http.clone(), base, token).delete(&name).await {
        Ok(204) => {}
        Ok(404) if here => {}
        Ok(404) => return Err(OTHER_SERVER.into()),
        Ok(401) => return Err("The token wasn’t accepted.".into()),
        Ok(_) => return Err("The server couldn’t delete it.".into()),
        Err(_) => return Err("Can’t reach the server.".into()),
    }
    core.uploads.lock().unwrap().remove(id);
    preview::remove(&core.previews_dir, id);
    persist(app);
    tray::refresh(app);
    emit_uploads(app);
    windows::forget(app, id);
    Ok(())
}

/// The public link of a finished upload.
pub fn link(app: &AppHandle, id: &str) -> Result<String, String> {
    let core = app.state::<Core>();
    let uploads = core.uploads.lock().unwrap();
    let item = uploads.get(id).ok_or(GONE)?;
    if item.upload.status != Status::Done {
        return Err("This upload isn’t finished yet.".into());
    }
    Ok(item.upload.url.clone())
}
