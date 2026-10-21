//! The JSON types shared with the UI and small pure helpers.

use std::path::Path;

use rand::RngExt;
use rand::distr::Alphanumeric;
use serde::{Deserialize, Serialize};

pub const DEFAULT_SERVER: &str = "";
pub const DEFAULT_SHORTCUT: &str = "CommandOrControl+Shift+Digit2";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Video,
    Audio,
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Uploading,
    Done,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorKind {
    Auth,
    Network,
    Server,
    Unsupported,
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadError {
    pub kind: ErrorKind,
    pub message: String,
}

impl UploadError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Upload {
    pub id: String,
    pub name: String,
    pub url: String,
    pub file_name: String,
    pub size: u64,
    pub kind: Kind,
    pub created_at: u64,
    pub status: Status,
    pub sent: u64,
    #[serde(default)]
    pub error: Option<UploadError>,
    /// Local preview while queued or uploading. Never read back from disk.
    #[serde(default, skip_deserializing)]
    pub thumb: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub server_url: String,
    pub shortcut: Option<String>,
    pub launch_at_login: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server_url: DEFAULT_SERVER.into(),
            shortcut: Some(DEFAULT_SHORTCUT.into()),
            launch_at_login: false,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub settings: Settings,
    pub has_token: bool,
    pub platform: &'static str,
    pub shortcut_supported: bool,
    pub version: String,
    pub uploads: Vec<Upload>,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionStatus {
    Connected,
    Unauthorized,
    Unreachable,
    Unconfigured,
}

#[derive(Serialize)]
pub struct Connection {
    pub status: ConnectionStatus,
    pub message: String,
}

/// What the HUD shows (macOS only; elsewhere Push uses notifications).
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum HudPayload {
    Upload { upload: Upload },
    Permission,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum View {
    Main,
    Settings,
}

#[derive(Clone, Serialize)]
pub struct Navigate {
    pub view: View,
}

#[derive(Clone, Serialize)]
pub struct Progress {
    pub id: String,
    pub sent: u64,
    pub total: u64,
}

pub const PLATFORM: &str = if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(windows) {
    "windows"
} else {
    "linux"
};

/// 10 base62 characters from the thread-local CSPRNG (seeded from the OS).
pub fn new_id() -> String {
    rand::rng().sample_iter(Alphanumeric).take(10).map(char::from).collect()
}

/// The remote extension: the original one lowercased, ASCII alphanumerics only, at most 10 chars, else `bin`.
pub fn sanitize_ext(path: &Path) -> String {
    let ext: String = path
        .extension()
        .map(|e| e.to_string_lossy())
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(10)
        .collect::<String>()
        .to_ascii_lowercase();
    if ext.is_empty() { "bin".into() } else { ext }
}

pub fn kind_for(ext: &str) -> Kind {
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "bmp" | "heic" | "tiff" => Kind::Image,
        "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" => Kind::Video,
        "mp3" | "m4a" | "wav" | "aac" | "flac" | "ogg" => Kind::Audio,
        _ => Kind::File,
    }
}


/// "Screenshot 2026-10-21 at 14.03.22.png", like macOS names its own screenshots.
pub fn screenshot_name(at: chrono::DateTime<chrono::Local>) -> String {
    at.format("Screenshot %Y-%m-%d at %H.%M.%S.png").to_string()
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
