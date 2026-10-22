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
        Ok(o) if o.status.success() && has_image(out).await => Shot::Done(out.to_path_buf()),
        // Exit 0 without a file is a cancel: Esc, or Control held so the shot went to the
        // clipboard.
        Ok(o) if o.status.success() => Shot::Cancelled,
        // macOS 27 reports a missing permission as exit 1, which the preflight check normally
        // rules out.
        Ok(o) => Shot::Failed(failure("screencapture", o.status.code(), &o.stderr)),
        Err(e) => Shot::Failed(format!("Couldn’t run screencapture: {e}")),
    }
}
