//! Interactive region capture. Each platform module provides `capture(out)`,
//! which writes a PNG to exactly `out`; this module owns the folder, the file names and cleanup.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

#[derive(Debug, PartialEq, Eq)]
pub enum Shot {
    /// A non-empty PNG inside the capture folder. The caller owns it (and deletes it after upload).
    Done(PathBuf),
    Cancelled,
    /// macOS only: Screen Recording is not granted (the system prompt has been requested).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    NoPermission,
    /// A short reason, shown to the user.
    Failed(String),
}

/// Interactive region capture. Writes a PNG inside `dir` (create it if needed). Never blocks the async runtime.
pub async fn capture_region(dir: &Path) -> Shot {
    let out = match new_capture_path(dir).await {
        Ok(out) => out,
        Err(e) => return Shot::Failed(format!("Couldn’t create the captures folder: {e}")),
    };
    let shot = platform::capture(&out).await;
    if !matches!(shot, Shot::Done(_)) {
        // Cancelled or failed tools can leave an empty or partial file behind.
        let _ = tokio::fs::remove_file(&out).await;
    }
    shot
}

/// macOS: open System Settings > Screen & System Audio Recording. Linux: no-op.
pub fn open_permission_settings() {
    platform::open_permission_settings();
}

/// false on Linux when WAYLAND_DISPLAY is set.
pub fn shortcut_supported() -> bool {
    platform::shortcut_supported()
}

/// Creates `dir` (owner-only: captures can contain anything on screen) and returns an unused
/// PNG path in it.
async fn new_capture_path(dir: &Path) -> std::io::Result<PathBuf> {
    tokio::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .await?;
    tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).await?;
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    loop {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("capture-{ms}-{seq}.png"));
        if !tokio::fs::try_exists(&path).await? {
            return Ok(path);
        }
    }
}

/// True if `path` is a non-empty regular file.
async fn has_image(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// A one-line message for a tool that exited unsuccessfully: its first stderr line, else the
/// exit code.
fn failure(tool: &str, code: Option<i32>, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    match (stderr.lines().map(str::trim).find(|l| !l.is_empty()), code) {
        (Some(line), _) => {
            let line: String = line.chars().take(200).collect();
            format!("{tool} failed: {line}")
        }
        (None, Some(code)) => format!("{tool} exited with status {code}"),
        (None, None) => format!("{tool} was stopped"),
    }
}
