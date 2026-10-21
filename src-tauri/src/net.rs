//! HTTP access to the Push server: server URL rules and the authenticated API.

use std::time::Duration;

use reqwest::header::{CONTENT_LENGTH, HeaderMap};
use serde::Deserialize;
use tauri::Url;

use crate::transfer::{Head, NetError, OnSent, Reply, Transport};

const PATCH_TIMEOUT: Duration = Duration::from_secs(55);
const SHORT_TIMEOUT: Duration = Duration::from_secs(15);

pub const BAD_URL: &str = "Enter a URL that starts with https://";

/// Never follows redirects, so the bearer token can't be forwarded anywhere else.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .user_agent(concat!("Push/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("HTTP client")
}

/// Trims, drops trailing slashes and checks the scheme: `https://`, or plain `http://` only
/// for localhost and 127.0.0.1. Returns the normalised URL.
pub fn validate_server_url(input: &str) -> Result<String, &'static str> {
    let url = Url::parse(input.trim().trim_end_matches('/')).map_err(|_| BAD_URL)?;
    let scheme_ok = match url.scheme() {
        "https" => true,
        "http" => matches!(url.host_str(), Some("localhost" | "127.0.0.1")),
        _ => false,
    };
    let plain = url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none();
    if !scheme_ok || !plain || url.host_str().is_none_or(str::is_empty) {
        return Err(BAD_URL);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Only links that can be safely opened in a browser are accepted from the server.
pub fn is_web_url(link: &str) -> bool {
    Url::parse(link).is_ok_and(|u| matches!(u.scheme(), "https" | "http") && u.host_str().is_some())
}

pub struct Api {
    client: reqwest::Client,
    base: String,
    token: String,
}

#[derive(Deserialize)]
struct Created {
    url: String,
}

#[derive(Deserialize, Default)]
struct Body {
    #[serde(default)]
    error: String,
    offset: Option<u64>,
}

fn upload_offset(headers: &HeaderMap) -> Option<u64> {
    headers.get("upload-offset")?.to_str().ok()?.trim().parse().ok()
}

fn net(_: reqwest::Error) -> NetError {
    NetError
}

impl Api {
    pub fn new(client: reqwest::Client, base: String, token: String) -> Self {
        Self { client, base, token }
    }

    fn file_url(&self, name: &str) -> String {
        format!("{}/api/files/{name}", self.base)
    }

    /// `GET /api/ping`, returning the status code.
    pub async fn ping(&self) -> Result<u16, NetError> {
        let res = self.client.get(format!("{}/api/ping", self.base)).bearer_auth(&self.token).timeout(SHORT_TIMEOUT);
        Ok(res.send().await.map_err(net)?.status().as_u16())
    }

    /// `DELETE /api/files/{name}`, returning the status code.
    pub async fn delete(&self, name: &str) -> Result<u16, NetError> {
        let res = self.client.delete(self.file_url(name)).bearer_auth(&self.token).timeout(SHORT_TIMEOUT);
        Ok(res.send().await.map_err(net)?.status().as_u16())
    }
}

impl Transport for Api {
    async fn patch(&self, name: &str, offset: u64, total: u64, body: Vec<Vec<u8>>, on_sent: OnSent) -> Result<Reply, NetError> {
        let len: u64 = body.iter().map(|p| p.len() as u64).sum();
        let mut sent = 0;
        let stream = futures_util::stream::iter(body.into_iter().map(move |piece| {
            sent += piece.len() as u64;
            on_sent(sent);
            Ok::<_, std::convert::Infallible>(piece)
        }));
        let res = self
            .client
            .patch(self.file_url(name))
            .bearer_auth(&self.token)
            .header("Upload-Offset", offset)
            .header("Upload-Length", total)
            .header(CONTENT_LENGTH, len)
            .timeout(PATCH_TIMEOUT)
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .map_err(net)?;
        let status = res.status().as_u16();
        let header = upload_offset(res.headers());
        Ok(match status {
            201 => Reply::Created(res.json::<Created>().await.map_err(net)?.url),
            202 => match header {
                Some(at) => Reply::Accepted(at),
                None => res.json::<Body>().await.ok().and_then(|b| b.offset).map_or(Reply::Status(202), Reply::Accepted),
            },
            409 => match (res.json::<Body>().await.unwrap_or_default().error.as_str(), header) {
                ("offset", Some(at)) => Reply::Offset(at),
                ("exists", _) => Reply::Exists,
                ("busy", _) => Reply::Busy,
                _ => Reply::Status(409),
            },
            s => Reply::Status(s),
        })
    }

    async fn head(&self, name: &str) -> Result<Head, NetError> {
        let res = self.client.head(self.file_url(name)).bearer_auth(&self.token).timeout(SHORT_TIMEOUT);
        let res = res.send().await.map_err(net)?;
        Ok(match (res.status().as_u16(), upload_offset(res.headers())) {
            (200, Some(at)) if res.headers().get("upload-complete").is_some_and(|v| v == "1") => Head::Complete(at),
            (200, Some(at)) => Head::Partial(at),
            (404, _) => Head::Missing,
            (s, _) => Head::Status(s),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls() {
        let ok = |s: &str| validate_server_url(s).unwrap();
        assert_eq!(ok("https://example.com"), "https://example.com");
        assert_eq!(ok("  https://example.com//  "), "https://example.com");
        assert_eq!(ok("HTTPS://Example.Com/"), "https://example.com");
        assert_eq!(ok("https://example.com/push/"), "https://example.com/push");
        assert_eq!(ok("https://example.com:8443"), "https://example.com:8443");
        assert_eq!(ok("http://localhost:3000"), "http://localhost:3000");
        assert_eq!(ok("http://127.0.0.1:3000/"), "http://127.0.0.1:3000");
        for bad in [
            "",
            "example.com",
            "http://example.com",
            "http://localhost.evil.com",
            "http://192.168.1.2",
            "ftp://example.com",
            "file:///etc/passwd",
            "https://",
            "https://user:pw@example.com",
            "https://example.com?x=1",
            "https://example.com#x",
            "javascript:alert(1)",
        ] {
            assert_eq!(validate_server_url(bad), Err(BAD_URL), "{bad}");
        }
    }

    #[test]
    fn web_urls() {
        assert!(is_web_url("https://example.com/i/Xc3nzK9qPa.png"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("not a url"));
    }

    /// Against a running server: `PUSH_TEST_SERVER=http://127.0.0.1:3000 PUSH_TEST_TOKEN=... cargo test -- --ignored`
    #[test]
    #[ignore]
    fn uploads_to_a_real_server() {
        use crate::transfer::{POLICY, Sent, send};
        use std::sync::{Arc, Mutex};

        let server = std::env::var("PUSH_TEST_SERVER").expect("PUSH_TEST_SERVER");
        let token = std::env::var("PUSH_TEST_TOKEN").expect("PUSH_TEST_TOKEN");
        let api = Api::new(client(), validate_server_url(&server).unwrap(), token);
        let data: Vec<u8> = (0..20 * 1024 * 1024 + 123).map(|i| (i % 251) as u8).collect();
        let path = std::env::temp_dir().join(format!("push-e2e-{}", crate::model::new_id()));
        std::fs::write(&path, &data).unwrap();
        let name = format!("{}.bin", crate::model::new_id());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let progress: OnSent = {
            let seen = seen.clone();
            Arc::new(move |n| seen.lock().unwrap().push(n))
        };
        tauri::async_runtime::block_on(async {
            assert_eq!(api.ping().await.unwrap(), 204);
            let mut file = tokio::fs::File::open(&path).await.unwrap();
            let sent = send(&api, &mut file, &name, data.len() as u64, false, &POLICY, &progress).await.unwrap();
            let Sent::Url(url) = sent else { panic!("expected a URL") };
            assert!(url.ends_with(&format!("/i/{name}")), "{url}");
            let body = client().get(&url).send().await.unwrap().bytes().await.unwrap();
            assert_eq!(body.len(), data.len());
            assert!(body == data, "downloaded bytes match");
            // Resuming a finished upload reports it as already there.
            let mut file = tokio::fs::File::open(&path).await.unwrap();
            let again = send(&api, &mut file, &name, data.len() as u64, true, &POLICY, &progress).await.unwrap();
            assert_eq!(again, Sent::Exists);
            assert_eq!(api.delete(&name).await.unwrap(), 204);
            assert_eq!(api.delete(&name).await.unwrap(), 404);
            let wrong = Api::new(client(), validate_server_url(&server).unwrap(), "x".repeat(64));
            assert_eq!(wrong.ping().await.unwrap(), 401);
        });
        let seen = seen.lock().unwrap();
        assert_eq!(*seen.last().unwrap(), data.len() as u64);
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "progress only grows");
        assert!(seen.len() >= 80, "progress per 256 KiB piece, got {}", seen.len());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn redirects_are_not_followed() {
        // A local server that redirects every request: the client must surface the 3xx itself.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0; 4096];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/steal\r\nContent-Length: 0\r\n\r\n");
        });
        let api = Api::new(client(), format!("http://127.0.0.1:{port}"), "t".repeat(64));
        let status = tauri::async_runtime::block_on(api.ping()).unwrap();
        assert_eq!(status, 302);
    }
}
