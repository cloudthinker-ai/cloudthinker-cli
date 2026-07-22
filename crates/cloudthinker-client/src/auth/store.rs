//! Token persistence with codex-style `Auto` resolution.
//!
//! Resolution order (cheapest / most-explicit first):
//!   1. env `CLOUDTHINKER_TOKEN` — an access-only override for CI. Read-only,
//!      refresh disabled; the client never writes or refreshes it.
//!   2. OS keyring (service `cloudthinker-cli`, account = base-url host).
//!   3. file `~/.config/cloudthinker/credentials.json`, mode 0600, keyed by host
//!      so a dev login and a prod login coexist.
//!
//! `AutoStore` prefers the keyring and falls back to the file; a later
//! successful keyring save deletes the migrated file entry.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{CtError, CtResult};

const KEYRING_SERVICE: &str = "cloudthinker-cli";
pub const TOKEN_ENV_VAR: &str = "CLOUDTHINKER_TOKEN";

/// A persisted credential set for one API host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredToken {
    pub access_token: String,
    /// Absent for the env-var store (which cannot refresh).
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
}

impl StoredToken {
    /// Build from a freshly minted API `Token`.
    pub fn from_token(token: &cloudthinker_api::types::Token) -> Self {
        Self {
            access_token: token.access_token.clone(),
            refresh_token: Some(token.refresh_token.clone()),
            expires_at: token.expires_at,
            workspace_id: token.workspace_id,
        }
    }

    /// True when the access token is within `skew` of expiry (or already past).
    pub fn expires_within(&self, skew: chrono::Duration) -> bool {
        match self.expires_at {
            Some(exp) => Utc::now() + skew >= exp,
            None => false,
        }
    }
}

/// Where a `save` landed — the login command warns when it fell back to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveLocation {
    Keyring,
    File,
}

/// Persistence backend for one host's credentials.
pub trait TokenStore: Send + Sync {
    fn load(&self) -> CtResult<Option<StoredToken>>;
    fn save(&self, token: &StoredToken) -> CtResult<SaveLocation>;
    fn clear(&self) -> CtResult<()>;
    /// False for the env-var store: the client must never attempt a refresh.
    fn refresh_enabled(&self) -> bool;
}

/// Access-only store fed by `CLOUDTHINKER_TOKEN`. Never persists, never refreshes.
pub struct EnvTokenStore {
    access_token: String,
}

impl EnvTokenStore {
    pub fn from_env() -> Option<Self> {
        match std::env::var(TOKEN_ENV_VAR) {
            Ok(v) if !v.trim().is_empty() => Some(Self {
                access_token: v.trim().to_string(),
            }),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_token(token: impl Into<String>) -> Self {
        Self {
            access_token: token.into(),
        }
    }
}

impl TokenStore for EnvTokenStore {
    fn load(&self) -> CtResult<Option<StoredToken>> {
        Ok(Some(StoredToken {
            access_token: self.access_token.clone(),
            refresh_token: None,
            expires_at: None,
            workspace_id: None,
        }))
    }

    fn save(&self, _token: &StoredToken) -> CtResult<SaveLocation> {
        // Read-only override: silently ignore writes so a stray save during an
        // env-pinned session cannot clobber a real credential file.
        Ok(SaveLocation::File)
    }

    fn clear(&self) -> CtResult<()> {
        Ok(())
    }

    fn refresh_enabled(&self) -> bool {
        false
    }
}

/// Keyring-backed store; account is the API host so multi-host logins coexist.
pub struct KeyringStore {
    host: String,
}

impl KeyringStore {
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into() }
    }

    fn entry(&self) -> CtResult<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, &self.host)
            .map_err(|e| CtError::Store(format!("keyring open: {e}")))
    }
}

impl TokenStore for KeyringStore {
    fn load(&self) -> CtResult<Option<StoredToken>> {
        match self.entry()?.get_password() {
            Ok(json) => {
                let token = serde_json::from_str(&json)
                    .map_err(|e| CtError::Store(format!("keyring decode: {e}")))?;
                Ok(Some(token))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CtError::Store(format!("keyring read: {e}"))),
        }
    }

    fn save(&self, token: &StoredToken) -> CtResult<SaveLocation> {
        let json = serde_json::to_string(token)
            .map_err(|e| CtError::Store(format!("keyring encode: {e}")))?;
        self.entry()?
            .set_password(&json)
            .map_err(|e| CtError::Store(format!("keyring write: {e}")))?;
        Ok(SaveLocation::Keyring)
    }

    fn clear(&self) -> CtResult<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CtError::Store(format!("keyring delete: {e}"))),
        }
    }

    fn refresh_enabled(&self) -> bool {
        true
    }
}

/// 0600 JSON file keyed by host: `{ "<host>": StoredToken }`.
pub struct FileStore {
    path: PathBuf,
    host: String,
}

impl FileStore {
    pub fn new(path: PathBuf, host: impl Into<String>) -> Self {
        Self {
            path,
            host: host.into(),
        }
    }

    /// Default location under `~/.config/cloudthinker/credentials.json`.
    pub fn default_for(host: impl Into<String>) -> CtResult<Self> {
        let base = dirs::config_dir()
            .ok_or_else(|| CtError::Store("no config dir for this platform".into()))?;
        Ok(Self::new(
            base.join("cloudthinker").join("credentials.json"),
            host,
        ))
    }

    fn read_all(&self) -> CtResult<BTreeMap<String, StoredToken>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) if text.trim().is_empty() => Ok(BTreeMap::new()),
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| CtError::Store(format!("credentials file decode: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(CtError::Store(format!("credentials file read: {e}"))),
        }
    }

    fn write_all(&self, all: &BTreeMap<String, StoredToken>) -> CtResult<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CtError::Store(format!("credentials dir create: {e}")))?;
        }
        let json = serde_json::to_string_pretty(all)
            .map_err(|e| CtError::Store(format!("credentials encode: {e}")))?;
        // Write then chmod 0600 — credentials must never be group/world readable.
        std::fs::write(&self.path, json)
            .map_err(|e| CtError::Store(format!("credentials file write: {e}")))?;
        set_owner_only(&self.path)?;
        Ok(())
    }
}

impl TokenStore for FileStore {
    fn load(&self) -> CtResult<Option<StoredToken>> {
        Ok(self.read_all()?.get(&self.host).cloned())
    }

    fn save(&self, token: &StoredToken) -> CtResult<SaveLocation> {
        let mut all = self.read_all()?;
        all.insert(self.host.clone(), token.clone());
        self.write_all(&all)?;
        Ok(SaveLocation::File)
    }

    fn clear(&self) -> CtResult<()> {
        let mut all = self.read_all()?;
        if all.remove(&self.host).is_some() {
            self.write_all(&all)?;
        }
        Ok(())
    }

    fn refresh_enabled(&self) -> bool {
        true
    }
}

/// 0600 on Unix; a no-op elsewhere (Windows ACLs handle this differently).
fn set_owner_only(path: &std::path::Path) -> CtResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)
            .map_err(|e| CtError::Store(format!("credentials chmod: {e}")))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Keyring-preferred store with a 0600-file fallback (codex `Auto` mode).
///
/// The keyring backend is boxed so tests can substitute a failing double
/// without touching the real OS keyring.
pub struct AutoStore {
    keyring: Box<dyn TokenStore>,
    file: FileStore,
}

impl AutoStore {
    pub fn from_parts(keyring: Box<dyn TokenStore>, file: FileStore) -> Self {
        Self { keyring, file }
    }

    /// Build the default `Auto` store for a host.
    pub fn default_for(host: impl Into<String> + Clone) -> CtResult<Self> {
        Ok(Self::from_parts(
            Box::new(KeyringStore::new(host.clone())),
            FileStore::default_for(host)?,
        ))
    }
}

impl TokenStore for AutoStore {
    fn load(&self) -> CtResult<Option<StoredToken>> {
        // Keyring wins; on any keyring failure fall back to the file so a broken
        // keyring never locks the user out of a credential they already have.
        match self.keyring.load() {
            Ok(Some(token)) => Ok(Some(token)),
            Ok(None) | Err(_) => self.file.load(),
        }
    }

    fn save(&self, token: &StoredToken) -> CtResult<SaveLocation> {
        match self.keyring.save(token) {
            Ok(SaveLocation::Keyring) => {
                // Migrate: drop any stale file entry once the keyring holds it.
                let _ = self.file.clear();
                Ok(SaveLocation::Keyring)
            }
            Ok(other) => Ok(other),
            Err(_) => {
                self.file.save(token)?;
                Ok(SaveLocation::File)
            }
        }
    }

    fn clear(&self) -> CtResult<()> {
        let keyring = self.keyring.clear();
        let file = self.file.clear();
        keyring.and(file)
    }

    fn refresh_enabled(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FailingStore, MockTokenStore, stored};

    // CA-CLI-6: the credentials file is written owner-only (0600). This is the
    // load-bearing security invariant of the file fallback.
    #[cfg(unix)]
    #[test]
    fn ca_cli_6_file_store_writes_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = FileStore::new(path.clone(), "app.example.com");
        store.save(&stored("access", "refresh")).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credentials file must be 0600");
    }

    // CA-CLI-6: an unusable keyring falls back to the 0600 file without error.
    #[test]
    fn ca_cli_6_autostore_falls_back_to_file_when_keyring_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store =
            AutoStore::from_parts(Box::new(FailingStore), FileStore::new(path, "host.example"));
        let location = store.save(&stored("a", "r")).unwrap();
        assert_eq!(location, SaveLocation::File);
        // load also survives a failing keyring, reading the file instead.
        assert_eq!(store.load().unwrap(), Some(stored("a", "r")));
    }

    #[test]
    fn autostore_prefers_keyring_over_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let file = FileStore::new(path, "host.example");
        file.save(&stored("file-token", "r")).unwrap();
        let keyring = Box::new(MockTokenStore::new(Some(stored("keyring-token", "r"))));
        let store = AutoStore::from_parts(keyring, file);
        assert_eq!(
            store.load().unwrap().map(|t| t.access_token),
            Some("keyring-token".to_string())
        );
    }

    #[test]
    fn file_store_is_keyed_by_host() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        FileStore::new(path.clone(), "dev.example")
            .save(&stored("dev", "r"))
            .unwrap();
        FileStore::new(path.clone(), "prod.example")
            .save(&stored("prod", "r"))
            .unwrap();
        assert_eq!(
            FileStore::new(path.clone(), "dev.example")
                .load()
                .unwrap()
                .map(|t| t.access_token),
            Some("dev".to_string())
        );
        assert_eq!(
            FileStore::new(path, "prod.example")
                .load()
                .unwrap()
                .map(|t| t.access_token),
            Some("prod".to_string())
        );
    }

    // CA-CLI-18: the env override is read-only and refresh-disabled.
    #[test]
    fn ca_cli_18_env_store_is_read_only_and_no_refresh() {
        let store = EnvTokenStore::with_token("env-access");
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.access_token, "env-access");
        assert!(loaded.refresh_token.is_none());
        assert!(!store.refresh_enabled());
    }
}
