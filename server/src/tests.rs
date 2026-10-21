//! Drives the router in-process against a temporary data dir.

use std::{
    io,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::SystemTime,
};

use axum::body::{Bytes, to_bytes};
use axum::http::Method;

use super::*;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PUBLIC_URL: &str = "https://example.com";

/// A router over a fresh data dir that is deleted on drop.
struct TestApp {
    router: Router,
    app: Arc<App>,
    dir: PathBuf,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn setup_with(redirect: Option<&str>) -> TestApp {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "push-server-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let data = dir.display().to_string();
    let config = Config::from_env(|key| match key {
        "PUSH_TOKEN" => Some(TOKEN.into()),
        "PUSH_PUBLIC_URL" => Some(format!("{PUBLIC_URL}/")),
        "PUSH_DATA_DIR" => Some(data.clone()),
        "PUSH_ROOT_REDIRECT" => redirect.map(Into::into),
        _ => None,
    })
    .unwrap();
    let app = Arc::new(App::new(&config).unwrap());
    TestApp {
        router: router(app.clone()),
        app,
        dir,
    }
}

fn setup() -> TestApp {
    setup_with(None)
}

impl TestApp {
    /// Sends a request and checks the headers every response must carry.
    async fn send(&self, req: Request<Body>) -> Response {
        let res = self.router.clone().oneshot(req).await.unwrap();
        let h = res.headers();
        assert_eq!(h["x-robots-tag"], ROBOTS_TAG, "{}", res.status());
        assert_eq!(h["referrer-policy"], "no-referrer");
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert_eq!(h["content-security-policy"], CSP);
        assert!(h.contains_key(header::CACHE_CONTROL));
        res
    }

    async fn get(&self, uri: &str) -> Response {
        self.send(Request::get(uri).body(Body::empty()).unwrap())
            .await
    }

    async fn api(&self, method: Method, name: &str) -> Response {
        self.send(
            authed(method, &format!("/api/files/{name}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn patch(&self, name: &str, offset: u64, total: u64, data: &[u8]) -> Response {
        self.send(patch_req(
            name,
            offset,
            total,
            data.len(),
            Body::from(data.to_vec()),
        ))
        .await
    }

    /// Puts a finished file straight into storage.
    fn put(&self, name: &str, data: &[u8]) {
        std::fs::write(self.app.files.join(name), data).unwrap();
    }

    fn part_len(&self, name: &str) -> Option<u64> {
        std::fs::metadata(self.app.part(name)).ok().map(|m| m.len())
    }
}

fn authed(method: Method, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
}

fn patch_req(name: &str, offset: u64, total: u64, len: usize, body: Body) -> Request<Body> {
    authed(Method::PATCH, &format!("/api/files/{name}"))
        .header("upload-offset", offset)
        .header("upload-length", total)
        .header(header::CONTENT_LENGTH, len)
        .body(body)
        .unwrap()
}

async fn text(res: Response) -> String {
    String::from_utf8(
        to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

fn hdr<'a>(res: &'a Response, name: &str) -> Option<&'a str> {
    res.headers().get(name).map(|v| v.to_str().unwrap())
}

/// A request body fed by the test, one piece at a time.
fn channel_body() -> (tokio::sync::mpsc::Sender<io::Result<Bytes>>, Body) {
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    (tx, Body::from_stream(stream))
}

async fn wait_for(mut done: impl FnMut() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out");
}

#[tokio::test]
async fn missing_and_wrong_tokens_get_identical_401s() {
    let t = setup();
    let wrong = format!("Bearer {}", TOKEN.replace('0', "1"));
    let mut seen = Vec::new();
    for auth in [
        None,
        Some(wrong.as_str()),
        Some(TOKEN),
        Some("Bearer "),
        Some("bearer x"),
    ] {
        let mut req = Request::get("/api/ping");
        if let Some(auth) = auth {
            req = req.header(header::AUTHORIZATION, auth);
        }
        let res = t.send(req.body(Body::empty()).unwrap()).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let mut headers = res.headers().clone();
        headers.remove(header::DATE);
        seen.push((headers, text(res).await));
    }
    assert!(
        seen.windows(2).all(|w| w[0] == w[1]),
        "401s differ: {seen:?}"
    );

    // Every API route and method is guarded, including ones that don't exist for that path.
    for method in [
        Method::HEAD,
        Method::DELETE,
        Method::PATCH,
        Method::GET,
        Method::POST,
    ] {
        let req = Request::builder()
            .method(method)
            .uri("/api/files/abcdef.png")
            .body(Body::empty())
            .unwrap();
        assert_eq!(t.send(req).await.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn auth_is_checked_before_the_body_is_read() {
    let t = setup();
    let polled = Arc::new(AtomicBool::new(false));
    let flag = polled.clone();
    let body = Body::from_stream(futures_util::stream::once(async move {
        flag.store(true, Ordering::SeqCst);
        Ok::<_, io::Error>(Bytes::from_static(b"hello"))
    }));
    // A huge Content-Length still gets 401, not 413: auth runs before the size check.
    let req = Request::patch("/api/files/abcdef.txt")
        .header(header::AUTHORIZATION, "Bearer nope")
        .header("upload-offset", 0)
        .header("upload-length", 5)
        .header(header::CONTENT_LENGTH, 50 * 1024 * 1024)
        .body(body)
        .unwrap();
    assert_eq!(t.send(req).await.status(), StatusCode::UNAUTHORIZED);
    assert!(!polled.load(Ordering::SeqCst));
    assert_eq!(t.part_len("abcdef.txt"), None);
}

#[tokio::test]
async fn ping() {
    let t = setup();
    let res = t
        .send(
            authed(Method::GET, "/api/ping")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(hdr(&res, "cache-control"), Some("no-store"));
}

#[tokio::test]
async fn single_chunk_upload() {
    let t = setup();
    let res = t.patch("Xc3nzK9qPa.png", 0, 5, b"hello").await;
    assert_eq!(res.status(), StatusCode::CREATED);
    assert_eq!(hdr(&res, "content-type"), Some("application/json"));
    assert_eq!(
        text(res).await,
        r#"{"url":"https://example.com/i/Xc3nzK9qPa.png","name":"Xc3nzK9qPa.png","size":5}"#
    );
    assert_eq!(
        std::fs::read(t.app.files.join("Xc3nzK9qPa.png")).unwrap(),
        b"hello"
    );
    assert_eq!(t.part_len("Xc3nzK9qPa.png"), None);

    let res = t.get("/i/Xc3nzK9qPa.png").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(text(res).await, "hello");
}

#[tokio::test]
async fn multi_chunk_upload() {
    let t = setup();
    let name = "abcdef1234.bin";
    let res = t.patch(name, 0, 10, b"0123").await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert_eq!(hdr(&res, "upload-offset"), Some("4"));
    assert_eq!(text(res).await, r#"{"offset":4}"#);

    let res = t.patch(name, 4, 10, b"4567").await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert_eq!(hdr(&res, "upload-offset"), Some("8"));

    // Not served while incomplete.
    assert_eq!(
        t.get(&format!("/i/{name}")).await.status(),
        StatusCode::NOT_FOUND
    );

    let res = t.patch(name, 8, 10, b"89").await;
    assert_eq!(res.status(), StatusCode::CREATED);
    assert_eq!(
        std::fs::read(t.app.files.join(name)).unwrap(),
        b"0123456789"
    );
}

#[tokio::test]
async fn offset_mismatch_is_409_with_actual_offset() {
    let t = setup();
    let name = "abcdef1234.bin";
    t.patch(name, 0, 10, b"0123").await;
    for wrong in [0, 2, 6] {
        let res = t.patch(name, wrong, 10, b"45").await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(hdr(&res, "upload-offset"), Some("4"));
        assert_eq!(text(res).await, r#"{"error":"offset"}"#);
    }
    // No partial upload yet: the actual offset is 0.
    let res = t.patch("zzzzzz.bin", 3, 10, b"45").await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(hdr(&res, "upload-offset"), Some("0"));
    assert_eq!(t.part_len("zzzzzz.bin"), None);
    assert_eq!(t.part_len(name), Some(4));
}

#[tokio::test]
async fn existing_file_is_409() {
    let t = setup();
    t.put("abcdef.png", b"old");
    let res = t.patch("abcdef.png", 0, 3, b"new").await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(text(res).await, r#"{"error":"exists"}"#);
    assert_eq!(
        std::fs::read(t.app.files.join("abcdef.png")).unwrap(),
        b"old"
    );
    assert_eq!(t.part_len("abcdef.png"), None);
}

#[tokio::test]
async fn concurrent_patch_is_409_busy() {
    let t = setup();
    let name = "abcdef.mp4";
    let (tx, body) = channel_body();
    let first = tokio::spawn(t.router.clone().oneshot(patch_req(name, 0, 10, 10, body)));
    tx.send(Ok(Bytes::from_static(b"01234"))).await.unwrap();
    wait_for(|| t.part_len(name) == Some(5)).await;

    let res = t.patch(name, 5, 10, b"56789").await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(text(res).await, r#"{"error":"busy"}"#);

    tx.send(Ok(Bytes::from_static(b"56789"))).await.unwrap();
    drop(tx);
    assert_eq!(first.await.unwrap().unwrap().status(), StatusCode::CREATED);
    assert_eq!(
        std::fs::read(t.app.files.join(name)).unwrap(),
        b"0123456789"
    );
    // The lock is gone again.
    assert!(t.app.uploading.lock().unwrap().is_empty());
}

#[tokio::test]
async fn hang_up_mid_chunk_keeps_received_bytes() {
    let t = setup();
    let name = "abcdef.mp4";
    let (tx, body) = channel_body();
    let first = tokio::spawn(t.router.clone().oneshot(patch_req(name, 0, 10, 10, body)));
    tx.send(Ok(Bytes::from_static(b"0123"))).await.unwrap();
    tx.send(Err(io::Error::new(io::ErrorKind::ConnectionReset, "gone")))
        .await
        .unwrap();
    assert_eq!(
        first.await.unwrap().unwrap().status(),
        StatusCode::BAD_REQUEST
    );

    let res = t.api(Method::HEAD, name).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(hdr(&res, "upload-offset"), Some("4"));
    assert_eq!(
        t.patch(name, 4, 10, b"456789").await.status(),
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn hang_up_with_handler_dropped_still_flushes() {
    let t = setup();
    let name = "abcdef.mp4";
    let (tx, body) = channel_body();
    let first = tokio::spawn(t.router.clone().oneshot(patch_req(name, 0, 10, 10, body)));
    tx.send(Ok(Bytes::from_static(b"0123"))).await.unwrap();
    wait_for(|| t.part_len(name) == Some(4)).await;
    // Like hyper dropping the request on disconnect: the handler future goes away.
    first.abort();
    drop(tx);
    wait_for(|| t.app.uploading.lock().unwrap().is_empty()).await;
    let res = t.api(Method::HEAD, name).await;
    assert_eq!(hdr(&res, "upload-offset"), Some("4"));
}

#[tokio::test]
async fn body_over_10_mib_is_413() {
    let t = setup();
    let max = MAX_CHUNK as usize;
    let big = vec![0u8; max + 1];
    let res = t.patch("abcdef.bin", 0, big.len() as u64, &big).await;
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(t.part_len("abcdef.bin"), None);

    // Exactly the cap is fine.
    let res = t.patch("abcdef.bin", 0, MAX_CHUNK + 1, &big[..max]).await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert_eq!(t.part_len("abcdef.bin"), Some(MAX_CHUNK));
}

#[tokio::test]
async fn chunked_body_over_10_mib_is_413_and_truncated() {
    let t = setup();
    t.patch("abcdef.bin", 0, 100 << 20, b"0123").await;
    // 11 pieces of 1 MiB with no Content-Length: only caught while streaming.
    let pieces = (0..11).map(|_| Ok::<_, io::Error>(Bytes::from(vec![0u8; 1 << 20])));
    let mut req = patch_req(
        "abcdef.bin",
        4,
        100 << 20,
        0,
        Body::from_stream(futures_util::stream::iter(pieces)),
    );
    req.headers_mut().remove(header::CONTENT_LENGTH);
    assert_eq!(t.send(req).await.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(t.part_len("abcdef.bin"), Some(4));
}

#[tokio::test]
async fn chunked_body_without_content_length_works() {
    let t = setup();
    let pieces = [b"hel".as_slice(), b"lo"].map(|p| Ok::<_, io::Error>(Bytes::from_static(p)));
    let mut req = patch_req(
        "abcdef.txt",
        0,
        5,
        0,
        Body::from_stream(futures_util::stream::iter(pieces)),
    );
    req.headers_mut().remove(header::CONTENT_LENGTH);
    assert_eq!(t.send(req).await.status(), StatusCode::CREATED);
    assert_eq!(
        std::fs::read(t.app.files.join("abcdef.txt")).unwrap(),
        b"hello"
    );
}

#[tokio::test]
async fn bad_headers_are_400() {
    let t = setup();
    let name = "abcdef.bin";
    // Upload-Length 0, offset past the end.
    assert_eq!(
        t.patch(name, 0, 0, b"").await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        t.patch(name, 6, 5, b"").await.status(),
        StatusCode::BAD_REQUEST
    );
    // Missing or malformed headers.
    for (key, value) in [
        ("upload-offset", "-1"),
        ("upload-length", "x"),
        ("upload-offset", ""),
    ] {
        let mut req = patch_req(name, 0, 5, 5, Body::from("hello"));
        req.headers_mut().insert(
            HeaderName::from_static(key),
            HeaderValue::from_static(value),
        );
        assert_eq!(
            t.send(req).await.status(),
            StatusCode::BAD_REQUEST,
            "{key}: {value}"
        );
    }
    for key in ["upload-offset", "upload-length"] {
        let mut req = patch_req(name, 0, 5, 5, Body::from("hello"));
        req.headers_mut().remove(key);
        assert_eq!(
            t.send(req).await.status(),
            StatusCode::BAD_REQUEST,
            "without {key}"
        );
    }
    assert_eq!(t.part_len(name), None);
}

#[tokio::test]
async fn chunk_past_upload_length_is_400_and_truncated() {
    let t = setup();
    let name = "abcdef.bin";
    t.patch(name, 0, 10, b"0123").await;
    let res = t.patch(name, 4, 10, b"4567890").await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(t.part_len(name), Some(4));

    // Also when the excess arrives after part of the chunk was already written.
    let (tx, body) = channel_body();
    let req = tokio::spawn(t.router.clone().oneshot(patch_req(name, 4, 10, 8, body)));
    tx.send(Ok(Bytes::from_static(b"4567"))).await.unwrap();
    tx.send(Ok(Bytes::from_static(b"8901"))).await.unwrap();
    drop(tx);
    assert_eq!(
        req.await.unwrap().unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(t.part_len(name), Some(4));
}

#[tokio::test]
async fn head_reports_upload_state() {
    let t = setup();
    let res = t.api(Method::HEAD, "abcdef.png").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    t.patch("abcdef.png", 0, 10, b"012").await;
    let res = t.api(Method::HEAD, "abcdef.png").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(hdr(&res, "upload-offset"), Some("3"));
    assert_eq!(hdr(&res, "upload-complete"), None);

    t.put("ghijkl.png", b"12345");
    let res = t.api(Method::HEAD, "ghijkl.png").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(hdr(&res, "upload-offset"), Some("5"));
    assert_eq!(hdr(&res, "upload-complete"), Some("1"));
    assert_eq!(hdr(&res, "cache-control"), Some("no-store"));
}

#[tokio::test]
async fn delete_removes_file_and_partial() {
    let t = setup();
    t.put("abcdef.png", b"x");
    assert_eq!(
        t.api(Method::DELETE, "abcdef.png").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(t.get("/i/abcdef.png").await.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        t.api(Method::DELETE, "abcdef.png").await.status(),
        StatusCode::NOT_FOUND
    );

    t.patch("ghijkl.png", 0, 10, b"012").await;
    assert_eq!(
        t.api(Method::DELETE, "ghijkl.png").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(t.part_len("ghijkl.png"), None);
}

#[tokio::test]
async fn serves_inline_types() {
    let t = setup();
    for (ext, mime) in [
        ("png", "image/png"),
        ("jpeg", "image/jpeg"),
        ("m4v", "video/mp4"),
        ("mov", "video/quicktime"),
        ("m4a", "audio/mp4"),
        ("txt", "text/plain; charset=utf-8"),
    ] {
        let name = format!("abcdef.{ext}");
        t.put(&name, b"data");
        let res = t.get(&format!("/i/{name}")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(hdr(&res, "content-type"), Some(mime));
        assert_eq!(hdr(&res, "content-disposition"), None);
        assert_eq!(hdr(&res, "cache-control"), Some("private, max-age=86400"));
        assert_eq!(hdr(&res, "access-control-allow-origin"), Some("*"), "sandboxed video pages are cross-origin");
        assert_eq!(text(res).await, "data");
    }
}

#[tokio::test]
async fn risky_types_download_as_attachments() {
    let t = setup();
    for ext in ["html", "htm", "svg", "js", "pdf", "xml", "bin"] {
        let name = format!("abcdef.{ext}");
        t.put(&name, b"<script>alert(1)</script>");
        let res = t.get(&format!("/i/{name}")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            hdr(&res, "content-type"),
            Some("application/octet-stream"),
            "{ext}"
        );
        assert_eq!(
            hdr(&res, "content-disposition"),
            Some("attachment"),
            "{ext}"
        );
    }
}

#[test]
fn every_inline_type_is_a_valid_mime() {
    for ext in [
        "png", "jpg", "jpeg", "gif", "webp", "avif", "bmp", "mp4", "m4v", "mov", "webm", "mp3",
        "m4a", "wav", "ogg", "txt",
    ] {
        let mime = inline_type(ext).unwrap_or_else(|| panic!("{ext} is not inline"));
        mime.parse::<mime::Mime>().unwrap();
    }
}

#[tokio::test]
async fn head_range_and_etag() {
    let t = setup();
    let data: Vec<u8> = (0..100).collect();
    t.put("abcdef.mp4", &data);

    let res = t
        .send(Request::head("/i/abcdef.mp4").body(Body::empty()).unwrap())
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(hdr(&res, "content-length"), Some("100"));
    assert!(text(res).await.is_empty());

    let req = Request::get("/i/abcdef.mp4")
        .header(header::RANGE, "bytes=10-19")
        .body(Body::empty())
        .unwrap();
    let res = t.send(req).await;
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(hdr(&res, "content-range"), Some("bytes 10-19/100"));
    assert_eq!(hdr(&res, "content-type"), Some("video/mp4"));
    assert_eq!(hdr(&res, "cache-control"), Some("private, max-age=86400"));
    assert_eq!(
        to_bytes(res.into_body(), usize::MAX).await.unwrap(),
        data[10..20]
    );

    let res = t.get("/i/abcdef.mp4").await;
    let etag = hdr(&res, "etag").expect("etag").to_owned();
    let req = Request::get("/i/abcdef.mp4")
        .header(header::IF_NONE_MATCH, &etag)
        .body(Body::empty())
        .unwrap();
    let res = t.send(req).await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(hdr(&res, "cache-control"), Some("private, max-age=86400"));
}

#[tokio::test]
async fn invalid_names_and_traversal_are_rejected() {
    let t = setup();
    // Bait outside files/ that a traversal would reach.
    std::fs::write(t.dir.join("secret.png"), b"secret").unwrap();
    t.patch("abcdef.png", 0, 10, b"partial").await;
    t.put("abcdef.txt", b"fine");

    for uri in [
        "/i/..%2Fsecret.png",
        "/i/..%2F..%2Fetc%2Fpasswd",
        "/i/%2e%2e%2fsecret.png",
        "/i/..",
        "/i/../secret.png",
        "/i/..%2Ftmp%2Fabcdef.png.part",
        "/i/abcdef.png.part",
        "/i/abcdef.TXT",
        "/i/ABCDEF.Txt",
        "/i/abcde.txt",
        "/i/abcdefghijabcdefghijabcdefghijabc.txt",
        "/i/abc-def.txt",
        "/i/abcdef",
        "/i/abcdef.",
        "/i/.txt",
        "/i/abcdef.txt/",
        "/i/abcdef.txt%00",
        "/i/%FF%FEabcdef.txt",
        "/i/abcdef.verylongext",
        "/i/",
    ] {
        let res = t.get(uri).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(text(res).await, "Not found", "{uri}");
    }
    assert_eq!(t.get("/i/abcdef.txt").await.status(), StatusCode::OK);

    for name in [
        "..%2Fsecret.png",
        "abcdef.PNG",
        "abcde.png",
        "abcdef",
        "abc%2Fdef.png",
        "%2E%2E",
    ] {
        assert_eq!(
            t.patch(name, 0, 3, b"abc").await.status(),
            StatusCode::BAD_REQUEST,
            "{name}"
        );
        assert_eq!(
            t.api(Method::HEAD, name).await.status(),
            StatusCode::BAD_REQUEST,
            "{name}"
        );
        assert_eq!(
            t.api(Method::DELETE, name).await.status(),
            StatusCode::BAD_REQUEST,
            "{name}"
        );
    }
    assert_eq!(std::fs::read(t.dir.join("secret.png")).unwrap(), b"secret");
}

#[tokio::test]
async fn robots_txt() {
    let t = setup();
    let res = t.get("/robots.txt").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(hdr(&res, "content-type"), Some("text/plain; charset=utf-8"));
    assert_eq!(hdr(&res, "cache-control"), Some("no-store"));
    assert_eq!(text(res).await, ROBOTS_TXT);
    assert!(ROBOTS_TXT.ends_with("User-agent: *\nDisallow: /\n"));
}

#[tokio::test]
async fn healthz() {
    let t = setup();
    let res = t.get("/healthz").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(text(res).await, "ok");
}

#[tokio::test]
async fn root_redirect_on_and_off() {
    let t = setup_with(Some("https://example.org/about"));
    let res = t.get("/").await;
    assert_eq!(res.status(), StatusCode::FOUND);
    assert_eq!(
        hdr(&res, "location"),
        Some("https://example.org/about")
    );

    let t = setup();
    let res = t.get("/").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn everything_else_is_404() {
    let t = setup();
    for uri in [
        "/nope",
        "/i",
        "/api",
        "/api/nope",
        "/sitemap.xml",
        "/files/abcdef.png",
        "/tmp/",
    ] {
        let res = t.get(uri).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(hdr(&res, "content-type"), Some("text/plain; charset=utf-8"));
        assert_eq!(hdr(&res, "cache-control"), Some("no-store"));
    }
    // Unsupported methods on real routes too.
    t.put("abcdef.png", b"x");
    for uri in ["/i/abcdef.png", "/healthz", "/robots.txt", "/"] {
        let res = t
            .send(Request::post(uri).body(Body::empty()).unwrap())
            .await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
    }
    assert_eq!(
        t.api(Method::GET, "abcdef.png").await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        t.api(Method::PUT, "abcdef.png").await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn removes_only_stale_part_files() {
    let t = setup();
    let old = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
    for (file, mtime) in [
        ("old.png.part", Some(old)),
        ("new.png.part", None),
        ("old.txt", Some(old)),
    ] {
        let f = std::fs::File::create(t.app.tmp.join(file)).unwrap();
        if let Some(mtime) = mtime {
            f.set_modified(mtime).unwrap();
        }
    }
    assert_eq!(t.app.remove_stale_parts(STALE_AFTER).unwrap(), 1);
    assert!(!t.app.tmp.join("old.png.part").exists());
    assert!(t.app.tmp.join("new.png.part").exists());
    assert!(t.app.tmp.join("old.txt").exists());
}

/// Makes the partial upload of `name` look abandoned: last written 25 hours ago.
fn age_part(t: &TestApp, name: &str) {
    let old = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
    std::fs::File::options()
        .append(true)
        .open(t.app.part(name))
        .unwrap()
        .set_modified(old)
        .unwrap();
}

#[tokio::test]
async fn stale_cleanup_skips_uploads_in_flight() {
    let t = setup();
    let name = "abcdef.mp4";
    t.patch(name, 0, 10, b"01234").await;
    age_part(&t, name);

    // A PATCH resumes the day-old partial and is waiting for its body when the sweep runs.
    let (tx, body) = channel_body();
    let resume = tokio::spawn(t.router.clone().oneshot(patch_req(name, 5, 10, 5, body)));
    wait_for(|| t.app.uploading.lock().unwrap().contains(name)).await;
    assert_eq!(t.app.remove_stale_parts(STALE_AFTER).unwrap(), 0);
    assert_eq!(t.part_len(name), Some(5));

    tx.send(Ok(Bytes::from_static(b"56789"))).await.unwrap();
    drop(tx);
    assert_eq!(resume.await.unwrap().unwrap().status(), StatusCode::CREATED);
    assert_eq!(
        std::fs::read(t.app.files.join(name)).unwrap(),
        b"0123456789"
    );
}

#[tokio::test]
async fn chunk_lands_in_the_partial_it_was_checked_against() {
    let t = setup();
    let name = "abcdef.mp4";
    t.patch(name, 0, 20, b"01234").await;

    // A body that flags when the server starts reading it, which is after the offset check.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<io::Result<Bytes>>(1);
    let reading = Arc::new(AtomicBool::new(false));
    let flag = reading.clone();
    let body = Body::from_stream(futures_util::stream::poll_fn(move |cx| {
        flag.store(true, Ordering::SeqCst);
        rx.poll_recv(cx)
    }));
    let resume = tokio::spawn(t.router.clone().oneshot(patch_req(name, 5, 20, 5, body)));
    wait_for(|| reading.load(Ordering::SeqCst)).await;

    // The partial vanishes between the check and the first write.
    std::fs::remove_file(t.app.part(name)).unwrap();
    tx.send(Ok(Bytes::from_static(b"56789"))).await.unwrap();
    drop(tx);

    // The chunk went to the checked file, not to offset 0 of a new one.
    let res = resume.await.unwrap().unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert_eq!(hdr(&res, "upload-offset"), Some("10"));
    assert_eq!(t.part_len(name), None);
}

#[tokio::test]
async fn delete_during_last_chunk_does_not_publish() {
    let t = setup();
    let name = "abcdef.mp4";
    t.patch(name, 0, 10, b"01234").await;

    let (tx, body) = channel_body();
    let last = tokio::spawn(t.router.clone().oneshot(patch_req(name, 5, 10, 5, body)));
    tx.send(Ok(Bytes::from_static(b"56"))).await.unwrap();
    wait_for(|| t.part_len(name) == Some(7)).await;

    // Like the app cancelling: DELETE while the last chunk is still arriving.
    assert_eq!(
        t.api(Method::DELETE, name).await.status(),
        StatusCode::NO_CONTENT
    );
    tx.send(Ok(Bytes::from_static(b"789"))).await.unwrap();
    drop(tx);

    assert_eq!(last.await.unwrap().unwrap().status(), StatusCode::NOT_FOUND);
    assert_eq!(
        t.get(&format!("/i/{name}")).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        t.api(Method::HEAD, name).await.status(),
        StatusCode::NOT_FOUND
    );
    assert!(t.app.uploading.lock().unwrap().is_empty());
}

#[test]
fn config_validation() {
    let env = |token: &'static str, url: &'static str, redirect: &'static str| {
        move |key: &str| {
            match key {
                "PUSH_TOKEN" => Some(token),
                "PUSH_PUBLIC_URL" => Some(url),
                "PUSH_ROOT_REDIRECT" => Some(redirect),
                _ => None,
            }
            .map(Into::into)
        }
    };
    let config = Config::from_env(env(TOKEN, "https://example.com//", "")).unwrap();
    assert_eq!(config.public_url, "https://example.com");
    assert_eq!(config.port, 3000);
    assert_eq!(config.data_dir, PathBuf::from("/data"));
    assert!(config.root_redirect.is_none());

    assert!(Config::from_env(env("", "https://example.com", "")).is_err());
    assert!(
        Config::from_env(env(
            "0123456789abcdef0123456789abcde",
            "https://example.com",
            ""
        ))
        .is_err()
    );
    assert!(Config::from_env(env(TOKEN, "", "")).is_err());
    assert!(Config::from_env(env(TOKEN, "example.com", "")).is_err());
    assert!(Config::from_env(env(TOKEN, "https://example.com\"x", "")).is_err());
    assert!(Config::from_env(env(TOKEN, "https://example.com", "javascript:alert(1)")).is_err());
    assert!(
        Config::from_env(env(
            TOKEN,
            "https://example.com",
            "https://example.org/about"
        ))
        .is_ok()
    );
}

#[test]
fn name_rules() {
    assert_eq!(name_ext("Xc3nzK9qPa.png"), Some("png"));
    assert_eq!(name_ext("abcdef.mp4"), Some("mp4"));
    assert_eq!(
        name_ext("abcdefghijabcdefghijabcdefghijab.0123456789"),
        Some("0123456789")
    );
    for bad in [
        "abcde.png",
        "abcdef.PNG",
        "abcdef",
        "abcdef.",
        ".png",
        "abc.def.png",
        "abc/de.png",
        "abcdef.png/",
        "ab\0def.png",
        "abcdéf.png",
    ] {
        assert_eq!(name_ext(bad), None, "{bad}");
    }
}
