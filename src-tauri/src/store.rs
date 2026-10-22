//! State on disk: settings.json, uploads.json and the upload token.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::model::{Settings, Status, Upload};
use crate::net::is_web_url;

/// Most finished uploads kept in the list and in uploads.json.
pub const MAX_DONE: usize = 500;

const KEYCHAIN_SERVICE: &str = "io.github.itsfutsy.push";
const KEYCHAIN_USER: &str = "upload-token";

/// Writes `bytes` to a temporary file next to `path`, then renames it over `path`. The file
/// is private to the user (uploads.json holds every link, and the links are the only access
/// control on the files), and so is any folder this creates.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    if let Some(dir) = path.parent() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(dir)?;
    }
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&tmp)?;
    // `mode` only applies when the file is created; a leftover temp file keeps its own.
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)
}

pub fn load_settings(path: &Path) -> Settings {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            eprintln!("push: ignoring unreadable {}: {e}", path.display());
            Settings::default()
        }),
        Err(_) => Settings::default(),
    }
}

pub fn save_settings(path: &Path, settings: &Settings) -> std::io::Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(settings)?)
}

/// Finished uploads, newest first.
pub fn load_uploads(path: &Path) -> Vec<Upload> {
    let Ok(bytes) = fs::read(path) else { return Vec::new() };
    let mut uploads: Vec<Upload> = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        eprintln!("push: ignoring unreadable {}: {e}", path.display());
        Vec::new()
    });
    uploads.retain(|u| u.status == Status::Done && is_web_url(&u.url));
    uploads.truncate(MAX_DONE);
    uploads
}

pub fn save_uploads(path: &Path, done: &[Upload]) -> std::io::Result<()> {
    write_atomic(path, &serde_json::to_vec(done)?)
}

/// The upload token. It lives in the macOS Keychain, Windows Credential Manager or the Linux
/// Secret Service (with a 0600 file fallback) and never reaches JavaScript.
pub struct Token {
    config_dir: PathBuf,
    /// `Some(token or None)` once read.
    cache: Mutex<Option<Option<String>>>,
}

impl Token {
    pub fn new(config_dir: PathBuf) -> Self {
        Self { config_dir, cache: Mutex::new(None) }
    }

    /// The token, if one is stored. Keychain access runs on a blocking thread.
    pub async fn get(&self) -> Result<Option<String>, String> {
        if let Some(token) = env_override() {
            return Ok(Some(token));
        }
        if let Some(cached) = self.cache.lock().unwrap().clone() {
            return Ok(cached);
        }
        let dir = self.config_dir.clone();
        let token = tauri::async_runtime::spawn_blocking(move || load_token(&dir))
            .await
            .map_err(|e| e.to_string())??;
        *self.cache.lock().unwrap() = Some(token.clone());
        Ok(token)
    }

    pub async fn has(&self) -> bool {
        matches!(self.get().await, Ok(Some(_)))
    }

    pub async fn set(&self, token: String) -> Result<(), String> {
        if env_override().is_some() {
            eprintln!("push: PUSH_TOKEN is set, so the token is not saved (debug build)");
            return Ok(());
        }
        let dir = self.config_dir.clone();
        let saved = token.clone();
        tauri::async_runtime::spawn_blocking(move || save_token(&dir, &saved))
            .await
            .map_err(|e| e.to_string())??;
        *self.cache.lock().unwrap() = Some(Some(token));
        Ok(())
    }
}

/// Debug builds read `PUSH_TOKEN` instead of the Keychain (dev binaries change signature on
/// every build, so each Keychain read would prompt).
fn env_override() -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var("PUSH_TOKEN").ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

fn entry() -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER)
}

/// Where `entry` lives, for error messages.
#[cfg(not(target_os = "linux"))]
const STORE: &str = if cfg!(windows) { "Credential Manager" } else { "the Keychain" };

#[cfg(not(target_os = "linux"))]
fn load_token(_dir: &Path) -> Result<Option<String>, String> {
    match entry().and_then(|e| e.get_password()) {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Couldn’t read the token from {STORE} ({e}).")),
    }
}

#[cfg(not(target_os = "linux"))]
fn save_token(_dir: &Path, token: &str) -> Result<(), String> {
    entry()
        .and_then(|e| e.set_password(token))
        .map_err(|e| format!("Couldn’t save the token to {STORE} ({e})."))
}

#[cfg(target_os = "linux")]
fn load_token(dir: &Path) -> Result<Option<String>, String> {
    match entry().and_then(|e| e.get_password()) {
        Ok(token) => Ok(Some(token)),
        Err(e) => {
            let file = fs::read_to_string(dir.join("token")).ok().map(|t| t.trim().to_owned());
            keyring_fallback(e, file.filter(|t| !t.is_empty()))
        }
    }
}

/// The token when the keyring couldn't give one, given what the fallback file holds.
#[cfg(target_os = "linux")]
fn keyring_fallback(error: keyring::Error, file: Option<String>) -> Result<Option<String>, String> {
    match error {
        // No Secret Service, or no entry there: the private file is the only place left.
        keyring::Error::NoEntry | keyring::Error::NoDefaultStore => Ok(file),
        // A locked or failing keyring may still hold the token, so this is no answer (and
        // isn't cached), unless the file has one: it is only written when the keyring can't be.
        e => file.map(Some).ok_or_else(|| format!("Couldn’t read the token from the keyring ({e}).")),
    }
}

#[cfg(target_os = "linux")]
fn save_token(dir: &Path, token: &str) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    let file = dir.join("token");
    if entry().and_then(|e| e.set_password(token)).is_ok() {
        let _ = fs::remove_file(file);
        return Ok(());
    }
    // Best effort: an older token left in the keyring would win over the file on the next read.
    let _ = entry().and_then(|e| e.delete_credential());
    let write = || -> std::io::Result<()> {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let tmp = dir.join("token.tmp");
        let _ = fs::remove_file(&tmp);
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
        f.write_all(token.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, &file)
    };
    write().map_err(|e| format!("Couldn’t save the token ({e})."))
}
