//! macOS: `/usr/sbin/screencapture -i`, gated on the Screen Recording permission.

use std::path::Path;

use tokio::process::Command;

use super::{Shot, failure, has_image};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    safe fn CGPreflightScreenCaptureAccess() -> bool;
    /// Shows the system prompt the first time only; later calls return immediately.
    safe fn CGRequestScreenCaptureAccess() -> bool;
}

pub(super) async fn capture(out: &Path) -> Shot {
    // Without permission screencapture still lets the user drag a region, then writes nothing.
    let allowed = tauri::async_runtime::spawn_blocking(|| {
        CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess()
    })
    .await
    .unwrap_or(false);
    if !allowed {
        return Shot::NoPermission;
    }
    // -i interactive (Space toggles window mode), -x no sound, -o no window shadow,
    // -r no DPI metadata (keeps Retina shots from showing at half size).
    let result = Command::new("/usr/sbin/screencapture")
        .args(["-i", "-x", "-o", "-r", "-t", "png"])
        .arg(out)
        .kill_on_drop(true)
        .output()
        .await;
    match result {
        Ok(o) => outcome(o.status.code(), has_image(out).await, &o.stderr, out),
        Err(e) => Shot::Failed(format!("Couldn’t run screencapture: {e}")),
    }
}

/// Exit 0 with a file is a shot. Exit 0 without one is a cancel: Esc, or Control held so the shot
/// went to the clipboard. Anything else is an error (macOS 27 reports a missing permission as
/// exit 1, which the preflight check normally rules out).
fn outcome(code: Option<i32>, has_image: bool, stderr: &[u8], out: &Path) -> Shot {
    match (code, has_image) {
        (Some(0), true) => Shot::Done(out.to_path_buf()),
        (Some(0), false) => Shot::Cancelled,
        _ => Shot::Failed(failure("screencapture", code, stderr)),
    }
}
