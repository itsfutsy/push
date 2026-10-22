//! HTTP access to the Push server: server URL rules and the authenticated API.

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{CONTENT_LENGTH, HeaderMap};
use serde::Deserialize;
use tauri::Url;

const PATCH_TIMEOUT: Duration = Duration::from_secs(55);
const SHORT_TIMEOUT: Duration = Duration::from_secs(15);

const BAD_URL: &str = "Enter a URL that starts with https://";

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

/// Called with the number of bytes handed to the connection so far.
pub type OnSent = Arc<dyn Fn(u64) + Send + Sync>;

/// A server reply to `PATCH /api/files/{name}`.
#[derive(Debug)]
pub enum Reply {
    /// 201 with the public URL.
    Created(String),
    /// 202 with the new offset.
    Accepted(u64),
    /// 409 `offset` with the server's offset.
    Offset(u64),
    /// 409 `exists`.
    Exists,
    /// 409 `busy`.
    Busy,
    Status(u16),
}

/// A server reply to `HEAD /api/files/{name}`.
#[derive(Debug)]
pub enum Head {
    Partial(u64),
    Complete(u64),
    Missing,
    Status(u16),
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

impl Api {
    pub fn new(client: reqwest::Client, base: String, token: String) -> Self {
        Self { client, base, token }
    }

    fn file_url(&self, name: &str) -> String {
        format!("{}/api/files/{name}", self.base)
    }

    /// `GET /api/ping`, returning the status code.
    pub async fn ping(&self) -> reqwest::Result<u16> {
        let res = self.client.get(format!("{}/api/ping", self.base)).bearer_auth(&self.token).timeout(SHORT_TIMEOUT);
        Ok(res.send().await?.status().as_u16())
    }

    /// `DELETE /api/files/{name}`, returning the status code.
    pub async fn delete(&self, name: &str) -> reqwest::Result<u16> {
        let res = self.client.delete(self.file_url(name)).bearer_auth(&self.token).timeout(SHORT_TIMEOUT);
        Ok(res.send().await?.status().as_u16())
    }

    pub async fn patch(&self, name: &str, offset: u64, total: u64, body: Vec<Vec<u8>>, on_sent: OnSent) -> reqwest::Result<Reply> {
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
            .await?;
        let status = res.status().as_u16();
        let header = upload_offset(res.headers());
        Ok(match status {
            201 => Reply::Created(res.json::<Created>().await?.url),
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

    pub async fn head(&self, name: &str) -> reqwest::Result<Head> {
        let res = self.client.head(self.file_url(name)).bearer_auth(&self.token).timeout(SHORT_TIMEOUT);
        let res = res.send().await?;
        Ok(match (res.status().as_u16(), upload_offset(res.headers())) {
            (200, Some(at)) if res.headers().get("upload-complete").is_some_and(|v| v == "1") => Head::Complete(at),
            (200, Some(at)) => Head::Partial(at),
            (404, _) => Head::Missing,
            (s, _) => Head::Status(s),
        })
    }
}
