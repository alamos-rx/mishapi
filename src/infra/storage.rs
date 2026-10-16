//! Reading and writing the app's JSON files, and the folder where they live.

use std::path::{Path, PathBuf};

use serde::{Serialize, de::DeserializeOwned};

pub const COLLECTIONS: &str = "collections.json";
pub const ENVIRONMENTS: &str = "environments.json";
pub const AGENTS: &str = "agents.json";
pub const SETTINGS: &str = "settings.json";

/// Ok(None) if the file does not exist. Any other error is reported so it is not overwritten on save.
pub fn load<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<Option<T>, String> {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).map(Some).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Writes to a temp file and renames: an interruption halfway does not corrupt the file.
pub fn save<T: Serialize>(path: impl AsRef<Path>, value: &T) -> Result<(), String> {
    let path = path.as_ref();
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let json = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    write_private(Path::new(&tmp), json.as_bytes()).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| format!("{}: {e}", path.display()))
}

/// The JSON files contain tokens: on Unix only their owner can read them (0600). On Windows %APPDATA% is already per user.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    std::io::Write::write_all(&mut o.open(path)?, bytes)
}

/// Creates the data folder; on Unix, accessible only by its owner (0700).
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)
}

/// Data folder. The current one if it already has app data (portable mode, e.g. next to the executable);
/// otherwise the user's: `%APPDATA%\mishapi` on Windows, `$XDG_DATA_HOME/mishapi` or
/// `~/.local/share/mishapi` on Linux.
pub fn data_dir() -> PathBuf {
    if [COLLECTIONS, SETTINGS].iter().any(|f| Path::new(f).exists()) {
        return PathBuf::from(".");
    }
    std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .map_or_else(|| PathBuf::from("."), |b| b.join("mishapi"))
}
