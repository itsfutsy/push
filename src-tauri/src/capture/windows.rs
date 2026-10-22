//! Windows: the system's own snipping overlay (the one behind Win+Shift+S). It puts the snip on
//! the clipboard, so this watches the clipboard while the overlay is up and saves what lands there.

use std::path::Path;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use super::Shot;

/// The processes that draw the overlay: ScreenClippingHost, or Snipping Tool itself on some
/// Windows 11 builds. The overlay holds the foreground while it is up.
const OVERLAY: [&str; 2] = ["screenclippinghost.exe", "snippingtool.exe"];
/// How long the overlay may take to appear.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// How long after the overlay closes the snip may still land on the clipboard.
const SETTLE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);

pub(super) async fn capture(out: &Path) -> Shot {
    let mut seq = clipboard_seq();
    if let Err(e) = tauri_plugin_opener::open_url("ms-screenclip:", None::<&str>) {
        return Shot::Failed(format!("Couldn’t open screen snipping: {e}"));
    }
    let started = Instant::now();
    let mut seen = false;
    let mut closed: Option<Instant> = None;
    loop {
        tokio::time::sleep(POLL).await;
        // Only a change while the overlay is up (or just closed) counts, so nothing the user
        // copies later is uploaded by mistake.
        let now = clipboard_seq();
        if now != seq {
            seq = now;
            let out = out.to_path_buf();
            match tauri::async_runtime::spawn_blocking(move || save_clipboard_image(&out)).await {
                Ok(Some(shot)) => return shot,
                Ok(None) => {} // not an image
                Err(e) => return Shot::Failed(e.to_string()),
            }
        }
        if overlay_in_front() {
            (seen, closed) = (true, None);
        } else if seen {
            if closed.get_or_insert_with(Instant::now).elapsed() > SETTLE {
                return Shot::Cancelled;
            }
        } else if started.elapsed() > OPEN_TIMEOUT {
            return Shot::Failed("Screen snipping didn’t open. Check that Snipping Tool is installed.".into());
        }
    }
}

/// Writes the clipboard image to `out` as a PNG. `None` when the clipboard holds something else.
fn save_clipboard_image(out: &Path) -> Option<Shot> {
    let image = match arboard::Clipboard::new().and_then(|mut c| c.get_image()) {
        Ok(image) => image,
        Err(arboard::Error::ContentNotAvailable) => return None,
        Err(e) => return Some(Shot::Failed(format!("Couldn’t read the snip from the clipboard: {e}"))),
    };
    let (width, height) = (image.width as u32, image.height as u32);
    let Some(pixels) = image::RgbaImage::from_raw(width, height, image.bytes.into_owned()) else {
        return Some(Shot::Failed("The snip on the clipboard couldn’t be read.".into()));
    };
    Some(match pixels.save_with_format(out, image::ImageFormat::Png) {
        Ok(()) => Shot::Done(out.to_path_buf()),
        Err(e) => Shot::Failed(format!("Couldn’t save the snip: {e}")),
    })
}

/// Changes whenever anything is put on the clipboard.
fn clipboard_seq() -> u32 {
    // SAFETY: no arguments, no preconditions.
    unsafe { GetClipboardSequenceNumber() }
}

/// Whether the foreground window belongs to the snipping overlay.
fn overlay_in_front() -> bool {
    let mut pid = 0;
    // SAFETY: plain Win32 calls; the handle is closed before returning and `name` outlives the call.
    unsafe {
        GetWindowThreadProcessId(GetForegroundWindow(), &mut pid);
        if pid == 0 {
            return false;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut name = [0u16; 1024];
        let mut len = name.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, name.as_mut_ptr(), &mut len) != 0;
        CloseHandle(process);
        if !ok {
            return false;
        }
        let path = String::from_utf16_lossy(&name[..len as usize]).to_ascii_lowercase();
        OVERLAY.iter().any(|exe| path.ends_with(&format!("\\{exe}")))
    }
}
