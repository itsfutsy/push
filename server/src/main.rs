//! Push server: stores the owner's uploads and serves them at `/i/{id}.{ext}`.

use std::{
    collections::HashSet,
    io,
    path::{Path as FsPath, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::{Path, Request, State, rejection::PathRejection},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, patch},
};
use futures_util::StreamExt;
use subtle::ConstantTimeEq;
use tokio::{fs, io::AsyncWriteExt};
use tower::ServiceExt;
use tower_http::{
    services::ServeFile,
    set_header::SetResponseHeaderLayer,
    trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer},
};
use tracing::Level;

/// Largest accepted PATCH body. Clients send 8 MiB chunks.
const MAX_CHUNK: u64 = 10 * 1024 * 1024;
/// A `.part` file nobody has written to for this long is an abandoned upload.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
const CLEANUP_EVERY: Duration = Duration::from_secs(60 * 60);

const UPLOAD_OFFSET: HeaderName = HeaderName::from_static("upload-offset");
const UPLOAD_LENGTH: HeaderName = HeaderName::from_static("upload-length");
const UPLOAD_COMPLETE: HeaderName = HeaderName::from_static("upload-complete");

const ROBOTS_TAG: &str = "noindex, nofollow, noarchive, nosnippet, noimageindex";
const CSP: &str =
    "default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'; sandbox";

/// Link-preview bots may fetch files so shared links unfurl; everyone else is kept out.
const ROBOTS_TXT: &str = "\
User-agent: Discordbot
User-agent: Twitterbot
User-agent: TelegramBot
User-agent: Slackbot-LinkExpanding
User-agent: facebookexternalhit
User-agent: WhatsApp
Allow: /i/
Disallow: /

User-agent: *
Disallow: /
";

/// A setting, trimmed. Blank counts as unset: docker-compose passes an unset optional one as "".
fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn port() -> Result<u16, String> {
    match env("PORT") {
        None => Ok(3000),
        Some(p) => p
            .parse()
            .map_err(|_| format!("PORT is not a port number: {p}")),
    }
}

/// An http(s) URL of printable ASCII without quotes or backslashes, so it is safe in JSON and headers.
fn is_url(s: &str) -> bool {
    (s.starts_with("https://") || s.starts_with("http://"))
        && s.bytes()
            .all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\')
}

struct App {
    token: Vec<u8>,
    public_url: String,
    root_redirect: Option<HeaderValue>,
    /// Finished uploads, served at `/i/{name}`.
    files: PathBuf,
    /// Uploads in progress, as `{name}.part`. Same volume as `files`, so the final rename is atomic.
    tmp: PathBuf,
    /// Names with a PATCH in flight.
    uploading: Arc<Mutex<HashSet<String>>>,
}

impl App {
    fn from_env() -> Result<Self, String> {
        let token = env("PUSH_TOKEN").ok_or("PUSH_TOKEN is not set")?;
        if token.len() < 32 {
            return Err(
                "PUSH_TOKEN is too short: use at least 32 characters (openssl rand -hex 32)".into(),
            );
        }
        let public_url = env("PUSH_PUBLIC_URL")
            .ok_or("PUSH_PUBLIC_URL is not set, for example https://share.example.com")?
            .trim_end_matches('/')
            .to_owned();
        if !is_url(&public_url) {
            return Err(format!(
                "PUSH_PUBLIC_URL is not an http(s) URL: {public_url}"
            ));
        }
        let root_redirect = match env("PUSH_ROOT_REDIRECT") {
            None => None,
            Some(url) if is_url(&url) => {
                Some(HeaderValue::try_from(url).map_err(|e| e.to_string())?)
            }
            Some(url) => return Err(format!("PUSH_ROOT_REDIRECT is not an http(s) URL: {url}")),
        };
        let data_dir = PathBuf::from(env("PUSH_DATA_DIR").unwrap_or_else(|| "/data".into()));
        let files = data_dir.join("files");
        let tmp = data_dir.join("tmp");
        // Create the storage dirs and check they're writable, so a bad volume fails loudly at boot.
        for dir in [&files, &tmp] {
            let probe = dir.join(".write-test");
            std::fs::create_dir_all(dir)
                .and_then(|()| std::fs::write(&probe, b""))
                .and_then(|()| std::fs::remove_file(&probe))
                .map_err(|e| {
                    format!(
                        "cannot write to {}: {e}. The data volume must be writable by uid 65532.",
                        dir.display()
                    )
                })?;
        }
        Ok(Self {
            token: token.into_bytes(),
            public_url,
            root_redirect,
            files,
            tmp,
            uploading: Arc::default(),
        })
    }

    fn part(&self, name: &str) -> PathBuf {
        self.tmp.join(format!("{name}.part"))
    }

    /// Claims `name` for one PATCH, or `None` if another one holds it.
    fn lock(&self, name: &str) -> Option<UploadLock> {
        let fresh = self.uploading.lock().unwrap().insert(name.to_owned());
        fresh.then(|| UploadLock {
            set: self.uploading.clone(),
            name: name.to_owned(),
        })
    }

    /// Deletes `.part` files in `tmp` that nobody wrote to for `max_age`, except those with a
    /// PATCH in flight. Returns how many. Blocking, so it runs on its own thread.
    fn remove_stale_parts(&self, max_age: Duration) -> io::Result<usize> {
        let mut removed = 0;
        for entry in std::fs::read_dir(&self.tmp)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str().and_then(|f| f.strip_suffix(".part")) else {
                continue;
            };
            // Hold the set while checking and removing, so no PATCH can claim the name and
            // resume this file in between.
            let uploading = self.uploading.lock().unwrap();
            if uploading.contains(name) {
                continue;
            }
            let stale = match entry.metadata().and_then(|m| m.modified()) {
                Ok(modified) => modified.elapsed().is_ok_and(|age| age > max_age),
                Err(_) => false,
            };
            if stale && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

struct UploadLock {
    set: Arc<Mutex<HashSet<String>>>,
    name: String,
}

impl Drop for UploadLock {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.name);
    }
}

/// Validates a `{id}.{ext}` name and returns its extension. The rules leave no room for
/// separators or dots in the id, so a valid name can never point outside its directory.
fn name_ext(name: &str) -> Option<&str> {
    let (id, ext) = name.split_once('.')?;
    let id_ok = (6..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric());
    let ext_ok = (1..=10).contains(&ext.len())
        && ext
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    (id_ok && ext_ok).then_some(ext)
}

/// Types that may render in the browser. Everything else downloads as an attachment,
/// so uploaded HTML, SVG, JS or PDF never runs on the site's origin.
fn inline_type(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "txt" => "text/plain; charset=utf-8",
        _ => return None,
    })
}

fn router(app: Arc<App>) -> Router {
    let api = Router::new()
        .route("/api/ping", get(|| async { StatusCode::NO_CONTENT }))
        .route(
            "/api/files/{name}",
            patch(upload).head(upload_status).delete(delete),
        )
        .method_not_allowed_fallback(|| async { not_found() })
        // Runs before every API handler, so nothing about the request (not even its size)
        // is looked at without the token.
        .route_layer(middleware::from_fn_with_state(app.clone(), require_token));

    let always = |name: &'static str, value: &'static str| {
        SetResponseHeaderLayer::overriding(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        )
    };

    Router::new()
        .route("/", get(root))
        .route("/robots.txt", get(|| async { ROBOTS_TXT }))
        .route("/i/{name}", get(serve_file))
        .method_not_allowed_fallback(|| async { not_found() })
        .merge(api)
        .fallback(|| async { not_found() })
        .with_state(app)
        // One line per request: method, path, status, latency. Headers are never logged.
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        // Added after the trace layer, so the Docker healthcheck doesn't log a line every 10 s.
        .route(
            "/healthz",
            get(|| async { "ok" }).fallback(|| async { not_found() }),
        )
        .layer(always("x-robots-tag", ROBOTS_TAG))
        .layer(always("referrer-policy", "no-referrer"))
        .layer(always("x-content-type-options", "nosniff"))
        .layer(always("content-security-policy", CSP))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "Not found").into_response()
}

fn bad_request() -> Response {
    (StatusCode::BAD_REQUEST, "Bad request").into_response()
}

fn too_large() -> Response {
    (StatusCode::PAYLOAD_TOO_LARGE, "Chunk too large").into_response()
}

fn json(status: StatusCode, body: String) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn conflict(error: &str) -> Response {
    json(StatusCode::CONFLICT, format!(r#"{{"error":"{error}"}}"#))
}

/// A chunk that doesn't start where the server's copy ends: tells the client where it does.
fn offset_conflict(on_disk: u64) -> Response {
    let mut res = conflict("offset");
    res.headers_mut().insert(UPLOAD_OFFSET, on_disk.into());
    res
}

/// A storage failure: logged, answered with a bare 500.
struct ServerError(io::Error);

impl From<io::Error> for ServerError {
    fn from(e: io::Error) -> Self {
        Self(e)
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        tracing::error!(error = %self.0, "storage error");
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response()
    }
}

type Result<T = Response, E = ServerError> = std::result::Result<T, E>;

/// Size of the file at `path`, or `None` if there is none.
async fn file_len(path: &FsPath) -> io::Result<Option<u64>> {
    match fs::metadata(path).await {
        Ok(meta) => Ok(Some(meta.len())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

async fn root(State(app): State<Arc<App>>) -> Response {
    match &app.root_redirect {
        Some(url) => (StatusCode::FOUND, [(header::LOCATION, url.clone())]).into_response(),
        None => not_found(),
    }
}

/// GET/HEAD /i/{name}. ServeFile handles Range, HEAD, ETag and conditional requests.
async fn serve_file(
    State(app): State<Arc<App>>,
    name: Result<Path<String>, PathRejection>,
    req: Request,
) -> Response {
    let Ok(Path(name)) = name else {
        return not_found();
    };
    let Some(ext) = name_ext(&name) else {
        return not_found();
    };
    let inline = inline_type(ext);
    let mime: mime::Mime = inline
        .unwrap_or("application/octet-stream")
        .parse()
        .expect("valid mime");
    let Ok(res) = ServeFile::new_with_mime(app.files.join(&name), &mime)
        .oneshot(req)
        .await;
    let mut res = res.map(Body::new);

    let status = res.status();
    if status == StatusCode::NOT_FOUND {
        return not_found();
    }
    if status.is_success() || status == StatusCode::NOT_MODIFIED {
        let headers = res.headers_mut();
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=86400"),
        );
        // The CSP sandbox gives the page an opaque origin, and Chrome's player page loads the
        // video with crossorigin="anonymous", so without this a linked video never plays.
        // The link is the only secret, and it already lets anyone fetch the file.
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
        if inline.is_none() {
            headers.insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment"),
            );
        }
    }
    res
}

/// Guards every `/api` route. Missing and wrong tokens get the same answer.
async fn require_token(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let valid = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| bool::from(t.as_bytes().ct_eq(&app.token)));
    if valid {
        return next.run(req).await;
    }
    let ip = req
        .headers()
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    tracing::warn!(ip, "rejected request without a valid token");
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "Unauthorized",
    )
        .into_response()
}

/// HEAD /api/files/{name}: how much of an upload the server has, so the client can resume.
async fn upload_status(State(app): State<Arc<App>>, Path(name): Path<String>) -> Result {
    if name_ext(&name).is_none() {
        return Ok(bad_request());
    }
    if let Some(len) = file_len(&app.files.join(&name)).await? {
        let headers = [
            (UPLOAD_OFFSET, HeaderValue::from(len)),
            (UPLOAD_COMPLETE, HeaderValue::from(1)),
        ];
        return Ok((StatusCode::OK, headers).into_response());
    }
    Ok(match file_len(&app.part(&name)).await? {
        Some(len) => (StatusCode::OK, [(UPLOAD_OFFSET, HeaderValue::from(len))]).into_response(),
        None => not_found(),
    })
}

/// DELETE /api/files/{name}: removes the file and any partial upload of it.
async fn delete(State(app): State<Arc<App>>, Path(name): Path<String>) -> Result {
    if name_ext(&name).is_none() {
        return Ok(bad_request());
    }
    let mut found = false;
    // The .part goes first. A PATCH in flight for this name then either published before
    // that (and its file goes next) or finds its .part gone when it tries to publish.
    for path in [app.part(&name), app.files.join(&name)] {
        match fs::remove_file(&path).await {
            Ok(()) => found = true,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(if found {
        StatusCode::NO_CONTENT.into_response()
    } else {
        not_found()
    })
}

/// PATCH /api/files/{name}: appends bytes `[offset, offset + len)` of a `total`-byte upload.
async fn upload(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result {
    let number = |h: &HeaderName| headers.get(h)?.to_str().ok()?.parse::<u64>().ok();
    // Refuse oversized chunks up front. Bodies without a Content-Length (chunked) are
    // allowed and capped while streaming instead; hyper rejects a malformed Content-Length.
    if number(&header::CONTENT_LENGTH).is_some_and(|len| len > MAX_CHUNK) {
        return Ok(too_large());
    }

    // 1. Well-formed request.
    let (Some(offset), Some(total)) = (number(&UPLOAD_OFFSET), number(&UPLOAD_LENGTH)) else {
        return Ok(bad_request());
    };
    if name_ext(&name).is_none() || total == 0 || offset > total {
        return Ok(bad_request());
    }

    // 2 and 3. Never overwrite a finished file, and allow one writer per name. Claiming the
    // lock first means the existence check can't race with a same-name upload finishing.
    let lock = app.lock(&name);
    if file_len(&app.files.join(&name)).await?.is_some() {
        return Ok(conflict("exists"));
    }
    let Some(lock) = lock else {
        return Ok(conflict("busy"));
    };

    // 4. The chunk must start exactly where the server's copy ends. The length is read from
    // the handle the chunk is then written through, so both refer to the same file.
    let opened = fs::OpenOptions::new()
        .append(true)
        .create(offset == 0) // only a first chunk starts a new .part
        .open(app.part(&name))
        .await;
    let file = match opened {
        Ok(file) => file,
        // No partial upload to resume.
        Err(e) if e.kind() == io::ErrorKind::NotFound && offset > 0 => {
            return Ok(offset_conflict(0));
        }
        Err(e) => return Err(e.into()),
    };
    let on_disk = file.metadata().await?.len();
    if on_disk != offset {
        return Ok(offset_conflict(on_disk));
    }

    // 5 and 6 run in their own task: if the client hangs up, hyper may drop this handler,
    // but the chunk still gets flushed and the lock released.
    tokio::spawn(async move {
        let _lock = lock;
        append_chunk(&app, &name, file, offset, total, body).await
    })
    .await
    .map_err(|e| ServerError(io::Error::other(e)))?
}

async fn append_chunk(
    app: &App,
    name: &str,
    mut file: fs::File,
    offset: u64,
    total: u64,
    body: Body,
) -> Result {
    let mut stream = body.into_data_stream(); // streamed, never buffered whole
    let mut received = 0;
    let mut failure = None;
    while let Some(bytes) = stream.next().await {
        let Ok(bytes) = bytes else {
            failure = Some(bad_request()); // the client hung up mid-chunk
            break;
        };
        received += bytes.len() as u64;
        let rejected = if received > MAX_CHUNK {
            Some(too_large())
        } else if offset + received > total {
            Some(bad_request())
        } else {
            None
        };
        if let Some(res) = rejected {
            // Drop what this request already wrote: a rejected chunk leaves no trace.
            file.flush().await?;
            file.set_len(offset).await?;
            return Ok(res);
        }
        if let Err(e) = file.write_all(&bytes).await {
            failure = Some(ServerError(e).into_response());
            break;
        }
    }
    // Always flush, even after a hang-up: tokio finishes writes in the background, and the
    // length on disk is what HEAD reports and the client resumes from.
    file.flush().await?;
    if let Some(res) = failure {
        return Ok(res);
    }

    let len = file.metadata().await?.len();
    if len < total {
        let mut res = json(StatusCode::ACCEPTED, format!(r#"{{"offset":{len}}}"#));
        res.headers_mut().insert(UPLOAD_OFFSET, len.into());
        return Ok(res);
    }
    drop(file);
    match fs::rename(app.part(name), app.files.join(name)).await {
        // A DELETE removed the .part while this chunk was in flight: don't publish.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(not_found()),
        result => result?,
    }
    let url = format!("{}/i/{name}", app.public_url);
    Ok(json(
        StatusCode::CREATED,
        format!(r#"{{"url":"{url}","name":"{name}","size":{total}}}"#),
    ))
}

async fn shutdown_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    tracing::info!("shutting down");
}

/// `push-server healthcheck`: exits 0 if the server on `$PORT` answers `/healthz`.
/// Used by the Docker HEALTHCHECK, since the distroless image has no curl.
fn healthcheck() -> ExitCode {
    use std::io::{Read, Write};
    let Ok(port) = port() else {
        return ExitCode::FAILURE;
    };
    let healthy = (|| -> io::Result<bool> {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
        let mut status = [0; 12];
        stream.read_exact(&mut status)?;
        Ok(&status == b"HTTP/1.1 200")
    })()
    .unwrap_or(false);
    if healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

async fn run() -> Result<(), String> {
    let port = port()?;
    let app = Arc::new(App::from_env()?);

    let cleanup = app.clone();
    std::thread::spawn(move || {
        loop {
            match cleanup.remove_stale_parts(STALE_AFTER) {
                Ok(0) => {}
                Ok(n) => tracing::info!(removed = n, "removed abandoned uploads"),
                Err(e) => tracing::warn!(error = %e, "cleanup failed"),
            }
            std::thread::sleep(CLEANUP_EVERY);
        }
    });

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .map_err(|e| format!("cannot listen on port {port}: {e}"))?;
    tracing::info!(port, files = %app.files.display(), "listening");
    axum::serve(listener, router(app))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        return healthcheck();
    }
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::FAILURE
        }
    }
}
