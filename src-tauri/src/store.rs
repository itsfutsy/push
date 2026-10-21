//! State on disk: settings.json, uploads.json and the upload token.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::model::{Settings, Status, Upload};

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
    uploads.retain(|u| u.status == Status::Done && !u.url.is_empty());
    uploads.truncate(MAX_DONE);
    uploads
}

pub fn save_uploads(path: &Path, done: &[Upload]) -> std::io::Result<()> {
    write_atomic(path, &serde_json::to_vec(done)?)
}

/// The upload token. It lives in the macOS Keychain or the Linux Secret Service (with a
/// 0600 file fallback) and never reaches JavaScript.
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

#[cfg(not(target_os = "linux"))]
fn load_token(_dir: &Path) -> Result<Option<String>, String> {
    match entry().and_then(|e| e.get_password()) {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Couldn’t read the token from the Keychain ({e}).")),
    }
}

#[cfg(not(target_os = "linux"))]
fn save_token(_dir: &Path, token: &str) -> Result<(), String> {
    entry()
        .and_then(|e| e.set_password(token))
        .map_err(|e| format!("Couldn’t save the token to the Keychain ({e})."))
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
#[cfg(any(target_os = "linux", test))]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;

    fn upload(id: &str, status: Status) -> Upload {
        Upload {
            id: id.into(),
            name: format!("{id}.png"),
            url: if status == Status::Done { format!("https://example.com/i/{id}.png") } else { String::new() },
            file_name: "a.png".into(),
            size: 1,
            kind: Kind::Image,
            created_at: 1,
            status,
            sent: 1,
            error: None,
            thumb: Some("data:image/png;base64,AA==".into()),
        }
    }

    #[test]
    fn files_round_trip_atomically() {
        let dir = std::env::temp_dir().join(format!("push-store-{}", crate::model::new_id()));
        let settings_path = dir.join("nested/settings.json");
        assert_eq!(load_settings(&settings_path), Settings::default());
        let settings = Settings { server_url: "http://localhost:3000".into(), shortcut: None, launch_at_login: true };
        save_settings(&settings_path, &settings).unwrap();
        assert_eq!(load_settings(&settings_path), settings);
        assert!(!dir.join("nested/settings.json.tmp").exists());

        let uploads_path = dir.join("uploads.json");
        save_uploads(&uploads_path, &[upload("a", Status::Done), upload("b", Status::Failed)]).unwrap();
        let loaded = load_uploads(&uploads_path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "a");
        assert_eq!(loaded[0].thumb, None, "thumbnails are never read back");

        fs::write(&uploads_path, b"{ not json").unwrap();
        assert!(load_uploads(&uploads_path).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn saved_files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let base = std::env::temp_dir().join(format!("push-perms-{}", crate::model::new_id()));
        let path = base.join("data/uploads.json");
        // A world-readable temp file left over from a crash doesn't make it through.
        fs::create_dir_all(base.join("data")).unwrap();
        fs::write(base.join("data/uploads.json.tmp"), b"[]").unwrap();
        fs::set_permissions(base.join("data/uploads.json.tmp"), fs::Permissions::from_mode(0o644)).unwrap();
        save_uploads(&path, &[upload("a", Status::Done)]).unwrap();
        assert_eq!(mode(&path), 0o600);

        let nested = base.join("new/dir/settings.json");
        save_settings(&nested, &Settings::default()).unwrap();
        assert_eq!(mode(&nested), 0o600);
        assert_eq!(mode(&base.join("new/dir")), 0o700);
        assert_eq!(mode(&base.join("new")), 0o700);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn keyring_errors_fall_back_to_the_file_only_when_it_has_no_answer() {
        let token = || Some("t".repeat(64));
        assert_eq!(keyring_fallback(keyring::Error::NoEntry, token()), Ok(token()));
        assert_eq!(keyring_fallback(keyring::Error::NoEntry, None), Ok(None));
        assert_eq!(keyring_fallback(keyring::Error::NoDefaultStore, None), Ok(None));
        // A locked keyring: not "no token" (which would be cached), unless the file has one.
        let locked = || keyring::Error::NoStorageAccess("locked".into());
        assert!(keyring_fallback(locked(), None).is_err());
        assert_eq!(keyring_fallback(locked(), token()), Ok(token()));
    }
}
