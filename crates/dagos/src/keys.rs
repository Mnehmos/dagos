//! Saved API keys: a small per-user file, outside every project, so keys entered in the app are
//! never part of a workspace and never reach the database, the IR, or an API response.
//!
//! The file lives in the user's configuration directory (`%APPDATA%\dagos\keys.json` on Windows,
//! `$XDG_CONFIG_HOME/dagos/keys.json` or `~/.config/dagos/keys.json` elsewhere; `DAGOS_CONFIG_DIR`
//! overrides the directory). On Unix it is created readable by its owner only. An API key in the
//! environment always wins over a saved one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dagos_core::domain::ProviderId;
use serde::{Deserialize, Serialize};

/// The saved keys file inside the user configuration directory.
pub const KEYS_FILE: &str = "keys.json";

/// The longest key accepted; real keys are far shorter.
const MAX_KEY_LENGTH: usize = 4096;

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct KeysFile {
    #[serde(default)]
    keys: BTreeMap<ProviderId, String>,
}

/// API keys saved per provider ID.
#[derive(Debug, Clone)]
pub struct KeyStore {
    path: PathBuf,
}

impl KeyStore {
    /// The keys file at `path`.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The per-user keys file, located through `env`; `None` if no configuration directory can
    /// be determined.
    pub fn for_user(env: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let set = |name| env(name).filter(|value| !value.is_empty()).map(PathBuf::from);
        let dir = set("DAGOS_CONFIG_DIR").or_else(|| {
            let base = if cfg!(windows) {
                set("APPDATA")
            } else {
                set("XDG_CONFIG_HOME").or_else(|| set("HOME").map(|home| home.join(".config")))
            };
            base.map(|base| base.join("dagos"))
        })?;
        Some(Self::at(dir.join(KEYS_FILE)))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every saved key; none if the file does not exist yet.
    pub fn load(&self) -> Result<BTreeMap<ProviderId, String>, String> {
        Ok(self.read()?.keys)
    }

    /// Saves `key` for `provider`, replacing any saved key.
    pub fn set(&self, provider: &ProviderId, key: &str) -> Result<(), String> {
        let key = validate(key)?;
        let mut file = self.read()?;
        file.keys.insert(provider.clone(), key.to_owned());
        self.write(&file)
    }

    /// Removes the saved key for `provider`; returns whether there was one.
    pub fn remove(&self, provider: &ProviderId) -> Result<bool, String> {
        let mut file = self.read()?;
        let removed = file.keys.remove(provider).is_some();
        if removed {
            self.write(&file)?;
        }
        Ok(removed)
    }

    fn read(&self) -> Result<KeysFile, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| format!("invalid {}: {error}", self.path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(KeysFile::default()),
            Err(error) => Err(format!("cannot read {}: {error}", self.path.display())),
        }
    }

    /// Replaces the file atomically, readable by its owner only.
    fn write(&self, file: &KeysFile) -> Result<(), String> {
        let fail = |error: std::io::Error| format!("cannot save {}: {error}", self.path.display());
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(fail)?;
        }
        let text = serde_json::to_string_pretty(file).expect("keys serialize");
        let temporary = self.path.with_extension("json.tmp");
        write_private(&temporary, text.as_bytes()).map_err(fail)?;
        std::fs::rename(&temporary, &self.path).map_err(fail)
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // The per-user configuration directory is already private to the user on Windows.
    std::fs::write(path, bytes)
}

/// The trimmed key, if it looks like a key: non-empty, one line, no spaces or control characters.
pub fn validate(key: &str) -> Result<&str, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("the API key is empty".into());
    }
    if key.len() > MAX_KEY_LENGTH {
        return Err("the API key is too long".into());
    }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(
            "the API key must not contain spaces, line breaks, or control characters".into()
        );
    }
    Ok(key)
}

/// A recognisable but useless hint for a key, e.g. `sk-o…3f9a`, so people can tell keys apart.
pub fn hint(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() < 12 {
        return "…".to_owned();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> ProviderId {
        ProviderId::parse(value).unwrap()
    }

    #[test]
    fn keys_are_saved_replaced_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::at(dir.path().join("nested").join(KEYS_FILE));
        assert!(store.load().unwrap().is_empty(), "a missing file means no keys");

        store.set(&id("openrouter"), "  sk-or-v1-first  ").unwrap();
        store.set(&id("zai"), "zai-key-123456").unwrap();
        store.set(&id("openrouter"), "sk-or-v1-second").unwrap();
        let keys = store.load().unwrap();
        assert_eq!(keys[&id("openrouter")], "sk-or-v1-second", "trimmed and replaced");
        assert_eq!(keys.len(), 2);

        assert!(store.remove(&id("zai")).unwrap());
        assert!(!store.remove(&id("zai")).unwrap());
        assert_eq!(store.load().unwrap().len(), 1);
        assert!(!store.path().with_extension("json.tmp").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn malformed_keys_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::at(dir.path().join(KEYS_FILE));
        for key in ["", "   ", "two words", "line\nbreak", &"x".repeat(MAX_KEY_LENGTH + 1)] {
            assert!(store.set(&id("openrouter"), key).is_err(), "accepted {key:?}");
        }
        assert!(!store.path().exists(), "nothing was written");
    }

    #[test]
    fn the_user_file_follows_the_platform_configuration_directory() {
        let explicit =
            KeyStore::for_user(|name| (name == "DAGOS_CONFIG_DIR").then(|| "/cfg".into()));
        assert_eq!(explicit.unwrap().path(), Path::new("/cfg").join(KEYS_FILE));
        assert!(KeyStore::for_user(|_| None).is_none());
        let platform = KeyStore::for_user(|name| match name {
            "APPDATA" if cfg!(windows) => Some("/appdata".into()),
            "HOME" if !cfg!(windows) => Some("/home/me".into()),
            _ => None,
        });
        let expected = if cfg!(windows) { "/appdata/dagos" } else { "/home/me/.config/dagos" };
        assert_eq!(platform.unwrap().path(), Path::new(expected).join(KEYS_FILE));
    }

    #[test]
    fn hints_never_reveal_short_keys_or_the_middle_of_long_ones() {
        assert_eq!(hint("sk-or-v1-abcdef1234567890"), "sk-o…7890");
        assert_eq!(hint("short"), "…");
    }
}
