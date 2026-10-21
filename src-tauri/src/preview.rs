//! Upload previews for the Library: a small image per upload in `app_data_dir()/previews`,
//! made from the local file while it is queued and served to the page as `preview://localhost/<id>`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use image::DynamicImage;
use tauri::http::{Response, StatusCode, header};

use crate::model::{Kind, kind_for};
use crate::store;

/// Longest edge, in pixels. Library tiles are about 130 pt, so this covers Retina with room to spare.
const EDGE: u32 = 480;
/// Images up to this size are decoded in process; bigger ones go to Quick Look (macOS).
const DECODE_MAX: u64 = 32 * 1024 * 1024;
#[cfg(target_os = "macos")]
const QUICK_LOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

fn path(dir: &Path, id: &str) -> PathBuf {
    dir.join(id)
}

/// The saved preview, or a new one made from `source`, the local file of an upload of type `ext`.
/// `None` when there is nothing to show (archives, audio) or it can't be read.
pub async fn make(dir: &Path, scratch: &Path, id: &str, source: &Path, ext: &str, size: u64) -> Option<DynamicImage> {
    let saved = path(dir, id);
    if let Ok(img) = decode(saved.clone()).await {
        return Some(img);
    }
    let img = render(scratch, id, source, ext, size).await?;
    let img = if img.width().max(img.height()) > EDGE { img.thumbnail(EDGE, EDGE) } else { img };
    let bytes = encode(&img)?;
    let written = tauri::async_runtime::spawn_blocking(move || store::write_atomic(&saved, &bytes)).await;
    if let Ok(Err(e)) = written {
        eprintln!("push: couldn’t save a preview: {e}");
    }
    Some(img)
}

async fn render(scratch: &Path, id: &str, source: &Path, ext: &str, size: u64) -> Option<DynamicImage> {
    if size <= DECODE_MAX
        && image::ImageFormat::from_extension(ext).is_some()
        && let Ok(img) = decode(source.to_owned()).await
    {
        return Some(img);
    }
    if !quick_look_handles(ext) {
        return None;
    }
    #[cfg(target_os = "macos")]
    return quick_look(scratch, id, source).await;
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (scratch, id);
        None
    }
}

async fn decode(path: PathBuf) -> image::ImageResult<DynamicImage> {
    tauri::async_runtime::spawn_blocking(move || image::ImageReader::open(&path)?.with_guessed_format()?.decode())
        .await
        .unwrap_or_else(|e| Err(image::ImageError::IoError(std::io::Error::other(e))))
}

/// Videos, PDFs and the images `image` can't decode (HEIC, TIFF, AVIF). Quick Look shows archives
/// and audio as their file icon, which the Library draws better itself.
fn quick_look_handles(ext: &str) -> bool {
    matches!(kind_for(ext), Kind::Image | Kind::Video) || ext == "pdf"
}

/// A JPEG, or a PNG if the image has transparency (window captures have transparent corners).
fn encode(img: &DynamicImage) -> Option<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    if img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p[3] < 255) {
        img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    } else {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img.to_rgb8()).ok()?;
    }
    Some(out.into_inner())
}

/// `qlmanage -t` writes `<file name>.png` into a folder of its own, which is removed afterwards.
#[cfg(target_os = "macos")]
async fn quick_look(scratch: &Path, id: &str, source: &Path) -> Option<DynamicImage> {
    use std::os::unix::fs::DirBuilderExt;

    let out = scratch.join(id);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&out).ok()?;
    let run = tokio::process::Command::new("/usr/bin/qlmanage")
        .args(["-t", "-s", &EDGE.to_string(), "-o"])
        .arg(&out)
        .arg(source)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    let ran = tokio::time::timeout(QUICK_LOOK_TIMEOUT, run).await;
    let png = std::fs::read_dir(&out).ok().and_then(|entries| {
        entries.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "png"))
    });
    let img = match (ran, png) {
        (Ok(Ok(status)), Some(png)) if status.success() => decode(png).await.ok(),
        _ => None,
    };
    let _ = std::fs::remove_dir_all(&out);
    img
}

pub fn remove(dir: &Path, id: &str) {
    if let Err(e) = std::fs::remove_file(path(dir, id))
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("push: couldn’t remove a preview: {e}");
    }
}

/// Removes previews of uploads that are no longer in the list (trimmed, failed, or from a crash).
pub fn prune(dir: &Path, keep: &HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if !keep.contains(entry.file_name().to_string_lossy().as_ref()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The `preview://localhost/<id>` response: the saved preview, or 404.
pub async fn respond(dir: &Path, uri_path: &str) -> Response<Vec<u8>> {
    let id = uri_path.trim_start_matches('/');
    let valid = (1..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric());
    let Some(bytes) = (if valid { tokio::fs::read(path(dir, id)).await.ok() } else { None }) else {
        return not_found();
    };
    let kind = if bytes.starts_with(b"\x89PNG") { "image/png" } else { "image/jpeg" };
    Response::builder()
        .header(header::CONTENT_TYPE, kind)
        .header(header::CACHE_CONTROL, "no-cache")
        .body(bytes)
        .expect("valid response")
}

pub fn not_found() -> Response<Vec<u8>> {
    Response::builder().status(StatusCode::NOT_FOUND).body(Vec::new()).expect("valid response")
}
