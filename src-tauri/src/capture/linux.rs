//! Linux: the best region picker for the session, else the XDG Screenshot portal (GNOME, others).

use std::env;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

use super::{Shot, failure, has_image};

enum Backend {
    Spectacle,
    SlurpGrim,
    Maim,
    Scrot,
    Portal,
}

pub(super) async fn capture(out: &Path) -> Shot {
    // Looking through PATH touches the disk, so keep it off the async runtime.
    let backend = tauri::async_runtime::spawn_blocking(backend)
        .await
        .unwrap_or(Backend::Portal);
    match backend {
        Backend::Spectacle => spectacle(out).await,
        Backend::SlurpGrim => slurp_grim(out).await,
        Backend::Maim => pick(Command::new("maim").args(["-s", "-u"]).arg(out), out).await,
        Backend::Scrot => pick(Command::new("scrot").arg("-s").arg(out), out).await,
        Backend::Portal => portal(out).await,
    }
}

fn backend() -> Backend {
    // A colon-separated list such as "ubuntu:GNOME".
    let desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let wayland = env::var_os("WAYLAND_DISPLAY").is_some()
        || env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland");
    let is = |name: &str| desktop.split(':').any(|d| d.eq_ignore_ascii_case(name));
    let (gnome, kde) = (is("GNOME"), is("KDE"));
    if kde && on_path("spectacle") {
        // The KDE portal has no region mode.
        Backend::Spectacle
    } else if wayland && !gnome && !kde && on_path("slurp") && on_path("grim") {
        // wlroots compositors (Sway, Hyprland, river, niri, labwc). Their portals run these anyway,
        // with worse defaults (a shared /tmp/out.png, or a silent full-screen shot without slurp).
        Backend::SlurpGrim
    } else if !wayland && !gnome && on_path("maim") {
        Backend::Maim
    } else if !wayland && !gnome && on_path("scrot") {
        Backend::Scrot
    } else {
        // GNOME 46+ opens its own screenshot UI here.
        Backend::Portal
    }
}

/// True if `name` is an executable file in a `PATH` directory.
fn on_path(name: &str) -> bool {
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|dir| {
            dir.join(name)
                .metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

/// Runs an interactive picker that writes `out`. These exit non-zero when the user presses Esc,
/// so anything other than success with an image counts as a cancel.
async fn pick(cmd: &mut Command, out: &Path) -> Shot {
    match cmd.kill_on_drop(true).output().await {
        Ok(o) if o.status.success() && has_image(out).await => Shot::Done(out.to_path_buf()),
        Ok(_) => Shot::Cancelled,
        Err(e) => {
            let tool = cmd.as_std().get_program().display();
            Shot::Failed(format!("Couldn’t run {tool}: {e}"))
        }
    }
}

async fn spectacle(out: &Path) -> Shot {
    let mut cmd = Command::new("spectacle");
    // -i own instance (a running Spectacle would swallow the request), -b no window,
    // -n no notification, -r region.
    cmd.args(["-i", "-b", "-n", "-r"]);
    if spectacle_release_capture().await {
        cmd.arg("-k"); // accept the region on mouse release
    }
    pick(cmd.arg("-o").arg(out), out).await
}

/// Whether the installed Spectacle accepts `-k`. Older versions reject unknown flags.
async fn spectacle_release_capture() -> bool {
    static SUPPORTED: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
    *SUPPORTED
        .get_or_init(|| async {
            let probe = Command::new("spectacle")
                .arg("--version")
                .kill_on_drop(true)
                .output();
            match tokio::time::timeout(Duration::from_secs(5), probe).await {
                Ok(Ok(o)) => spectacle_version(&String::from_utf8_lossy(&o.stdout))
                    .is_some_and(has_release_capture),
                _ => false,
            }
        })
        .await
}

/// Parses `spectacle --version` output, e.g. "spectacle 6.7.0", into (major, minor).
fn spectacle_version(output: &str) -> Option<(u32, u32)> {
    let version = output
        .split_whitespace()
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))?;
    let mut parts = version.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// `-k` arrived in 6.7. Before moving to Plasma numbering (6.3, in 2025) Spectacle used KDE Gear
/// numbering (yy.mm, up to 24.12), so a large major version is an old release.
fn has_release_capture(version: (u32, u32)) -> bool {
    version >= (6, 7) && version.0 < 15
}

async fn slurp_grim(out: &Path) -> Shot {
    let selection = match Command::new("slurp").kill_on_drop(true).output().await {
        Ok(o) => o,
        Err(e) => return Shot::Failed(format!("Couldn’t run slurp: {e}")),
    };
    // On Esc slurp exits 1 with "selection cancelled" and prints nothing.
    let stdout = String::from_utf8_lossy(&selection.stdout);
    let Some(geometry) = parse_geometry(&stdout).filter(|_| selection.status.success()) else {
        return Shot::Cancelled;
    };
    let mut grim = Command::new("grim");
    grim.arg("-g").arg(geometry).arg(out).kill_on_drop(true);
    match grim.output().await {
        Ok(o) if o.status.success() && has_image(out).await => Shot::Done(out.to_path_buf()),
        Ok(o) => Shot::Failed(failure("grim", o.status.code(), &o.stderr)),
        Err(e) => Shot::Failed(format!("Couldn’t run grim: {e}")),
    }
}

/// Checks slurp's `x,y wxh` output and returns it trimmed, or None for anything else
/// (including a zero-size selection, which grim rejects).
fn parse_geometry(output: &str) -> Option<&str> {
    let geometry = output.trim();
    let (position, size) = geometry.split_once(' ')?;
    let (x, y) = position.split_once(',')?;
    let (w, h) = size.split_once('x')?;
    x.parse::<i32>().ok()?;
    y.parse::<i32>().ok()?;
    let (w, h) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
    (w > 0 && h > 0).then_some(geometry)
}

async fn portal(out: &Path) -> Shot {
    use ashpd::desktop::screenshot::Screenshot;

    let request = Screenshot::request().interactive(true).modal(false);
    let uri = match request.send().await.and_then(|r| r.response()) {
        Ok(screenshot) => screenshot.uri().as_str().to_owned(),
        // GNOME reports a user cancel as Other, not Cancelled.
        Err(ashpd::Error::Response(_)) => return Shot::Cancelled,
        Err(e) => return Shot::Failed(format!("The screenshot portal failed: {e}")),
    };
    let Some(source) = file_path(&uri) else {
        return Shot::Failed("The screenshot portal didn’t return a local file.".into());
    };
    // Copy, never move: on GNOME the original is the user's own ~/Pictures/Screenshots history,
    // and the caller deletes what it gets back.
    match tokio::fs::copy(&source, out).await {
        Ok(_) if has_image(out).await => Shot::Done(out.to_path_buf()),
        Ok(_) => Shot::Cancelled,
        Err(e) => Shot::Failed(format!("Couldn’t copy the screenshot: {e}")),
    }
}

/// Turns a `file://` URI (percent-encoded) into a path.
fn file_path(uri: &str) -> Option<std::path::PathBuf> {
    url::Url::parse(uri).ok()?.to_file_path().ok()
}
