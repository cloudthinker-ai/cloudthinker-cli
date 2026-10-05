//! Token persistence, codex `auth.json` style.
//!
//! Resolution order (cheapest / most-explicit first):
//!   1. env `CLOUDTHINKER_TOKEN` — an access-only override for CI. Read-only,
//!      refresh disabled; the client never writes or refreshes it.
//!   2. the platform config directory's `cloudthinker/credentials.json`, keyed
//!      by origin and workspace id. Mode 0600 on Unix; on Windows it rests on
//!      the per-user ACL of `%APPDATA%`, as Codex's `auth.json` does.
//!
//! Releases before 0.5.3 kept the logins in the OS keyring (service
//! `cloudthinker-cli`, account = origin). `FileStore::open_default` absorbs
//! that entry into the file once and deletes it, so the first store opened by
//! 0.5.3, on a read or a write path, migrates the login.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::client::login_command;
use crate::error::{CtError, CtResult};

const KEYRING_SERVICE: &str = "cloudthinker-cli";
const STORE_VERSION: u8 = 2;
pub const TOKEN_ENV_VAR: &str = "CLOUDTHINKER_TOKEN";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSource {
    Stored,
    Environment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialProvenance {
    Missing,
    Present(CredentialSource),
    Stale(CredentialSource),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoredWorkspace {
    pub workspace_id: Uuid,
    pub workspace_name: Option<String>,
    pub active: bool,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceSelector {
    Active,
    IdOrName(String),
}

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
    #[serde(default)]
    pub workspace_name: Option<String>,
}

impl StoredToken {
    /// Build from a freshly minted API `Token`.
    pub fn from_token(token: &cloudthinker_api::types::Token) -> Self {
        Self {
            access_token: token.access_token.clone(),
            refresh_token: Some(token.refresh_token.clone()),
            expires_at: token.expires_at,
            workspace_id: token.workspace_id,
            workspace_name: None,
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

/// Exclusive credential-store mutation guard. Stores without persistence use
/// the empty guard; file-backed stores hold an OS lock for its lifetime.
pub struct StoreLock {
    _file: Option<File>,
}

impl StoreLock {
    fn none() -> Self {
        Self { _file: None }
    }

    fn file(file: File) -> Self {
        Self { _file: Some(file) }
    }
}

/// Persistence backend for one host's credentials.
pub trait TokenStore: Send + Sync {
    fn source(&self) -> CredentialSource {
        CredentialSource::Stored
    }
    fn acquire_lock(&self) -> CtResult<StoreLock> {
        Ok(StoreLock::none())
    }
    fn load(&self) -> CtResult<Option<StoredToken>>;
    fn load_locked(&self, _lock: &StoreLock) -> CtResult<Option<StoredToken>> {
        self.load()
    }
    fn load_all(&self) -> CtResult<Vec<StoredToken>> {
        Ok(self.load()?.into_iter().collect())
    }
    fn load_all_locked(&self, _lock: &StoreLock) -> CtResult<Vec<StoredToken>> {
        self.load_all()
    }
    fn save(&self, token: &StoredToken) -> CtResult<()>;
    fn save_locked(&self, _lock: &StoreLock, token: &StoredToken) -> CtResult<()> {
        self.save(token)
    }
    fn replace_locked(&self, lock: &StoreLock, token: &StoredToken) -> CtResult<()> {
        self.save_locked(lock, token)
    }
    fn workspaces(&self) -> CtResult<Vec<StoredWorkspace>> {
        Ok(Vec::new())
    }
    fn activate(&self, _selector: &WorkspaceSelector) -> CtResult<StoredWorkspace> {
        Err(CtError::Usage(
            "this credential source has no stored workspaces to switch between".into(),
        ))
    }
    fn recovered_file(&self) -> Option<PathBuf> {
        None
    }
    fn clear(&self) -> CtResult<()>;
    fn clear_locked(&self, _lock: &StoreLock) -> CtResult<()> {
        self.clear()
    }
    fn clear_all(&self) -> CtResult<()> {
        self.clear()
    }
    fn clear_all_locked(&self, _lock: &StoreLock) -> CtResult<()> {
        self.clear_all()
    }
    /// False for the env-var store: the client must never attempt a refresh.
    fn refresh_enabled(&self) -> bool;
}

pub(crate) async fn acquire_credential_lock(store: Arc<dyn TokenStore>) -> CtResult<StoreLock> {
    tokio::task::spawn_blocking(move || store.acquire_lock())
        .await
        .map_err(|error| CtError::Store(format!("credential lock task failed: {error}")))?
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
    fn source(&self) -> CredentialSource {
        CredentialSource::Environment
    }
    fn load(&self) -> CtResult<Option<StoredToken>> {
        Ok(Some(StoredToken {
            access_token: self.access_token.clone(),
            refresh_token: None,
            expires_at: None,
            workspace_id: None,
            workspace_name: None,
        }))
    }

    fn save(&self, _token: &StoredToken) -> CtResult<()> {
        // Read-only override: silently ignore writes so a stray save during an
        // env-pinned session cannot clobber a real credential file.
        Ok(())
    }

    fn clear(&self) -> CtResult<()> {
        Ok(())
    }

    fn refresh_enabled(&self) -> bool {
        false
    }
}

/// The keyring entry releases before 0.5.3 kept for an origin: read once so
/// the credentials file can absorb it, then deleted.
struct KeyringLogins {
    origin: String,
}

impl KeyringLogins {
    fn entry(&self) -> Option<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, &self.origin).ok()
    }

    fn take(&self) -> Option<OriginCredentials> {
        let json = self.entry()?.get_password().ok()?;
        decode_origin(&json, "keyring").ok()
    }

    fn delete(&self) {
        if let Some(entry) = self.entry() {
            let _ = entry.delete_credential();
        }
    }
}

/// 0600 JSON file keyed by origin: `{ "<origin>": StoredToken }`.
pub struct FileStore {
    path: PathBuf,
    origin: String,
    selector: WorkspaceSelector,
    recovered: Mutex<Option<PathBuf>>,
}

impl FileStore {
    pub fn new(path: PathBuf, origin: impl Into<String>) -> Self {
        Self::with_selector(path, origin, WorkspaceSelector::Active)
    }

    pub fn with_selector(
        path: PathBuf,
        origin: impl Into<String>,
        selector: WorkspaceSelector,
    ) -> Self {
        Self {
            path,
            origin: origin.into(),
            selector,
            recovered: Mutex::new(None),
        }
    }

    fn login(&self) -> String {
        login_command(&self.origin)
    }

    /// Opens `<config dir>/cloudthinker/credentials.json`. The first open after
    /// the 0.5.3 upgrade writes: it moves the origin's keyring logins from
    /// releases before 0.5.3 into the file and deletes the keyring entry, so a
    /// read-only command keeps the login it had before the upgrade.
    pub fn open_default(origin: impl Into<String>, selector: WorkspaceSelector) -> CtResult<Self> {
        let base = dirs::config_dir()
            .ok_or_else(|| CtError::Store("no config dir for this platform".into()))?;
        let store = Self::with_selector(
            base.join("cloudthinker").join("credentials.json"),
            origin,
            selector,
        );
        store.absorb_keyring_logins()?;
        Ok(store)
    }

    fn absorb_keyring_logins(&self) -> CtResult<()> {
        let keyring = KeyringLogins {
            origin: self.origin.clone(),
        };
        self.absorb_keyring_logins_from(|| keyring.take(), || keyring.delete())
    }

    fn absorb_keyring_logins_from(
        &self,
        take: impl FnOnce() -> Option<OriginCredentials>,
        delete: impl FnOnce(),
    ) -> CtResult<()> {
        if self.holds_origin() {
            return Ok(());
        }
        let Some(credentials) = take() else {
            return Ok(());
        };
        let _lock = self.acquire_mutation_lock()?;
        self.absorb(credentials)?;
        delete();
        Ok(())
    }

    fn holds_origin(&self) -> bool {
        self.read_all()
            .is_ok_and(|all| all.origins.contains_key(&self.origin))
    }

    /// The file keeps its own logins for this origin when it has any;
    /// otherwise `credentials` become the file's. True when the file changed.
    fn absorb(&self, credentials: OriginCredentials) -> CtResult<bool> {
        let mut all = self.read_for_write()?;
        if all.origins.contains_key(&self.origin) {
            return Ok(false);
        }
        all.origins.insert(self.origin.clone(), credentials);
        self.write_all(&all)?;
        Ok(true)
    }

    fn read_contents(&self) -> CtResult<FileContents> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) if text.trim().is_empty() => {
                Ok(FileContents::Current(CredentialsFile::default()))
            }
            Ok(text) => Ok(decode_file(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(FileContents::Current(CredentialsFile::default()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Ok(FileContents::Corrupt),
            Err(e) => Err(CtError::Store(format!("credentials file read: {e}"))),
        }
    }

    fn read_all(&self) -> CtResult<CredentialsFile> {
        match self.read_contents()? {
            FileContents::Current(all) => Ok(all),
            FileContents::Older | FileContents::Corrupt => Err(CtError::ObsoleteCredentials {
                origin: self.origin.clone(),
            }),
            FileContents::Newer(version) => Err(newer_format_error(version)),
        }
    }

    fn read_for_write(&self) -> CtResult<CredentialsFile> {
        match self.read_contents()? {
            FileContents::Current(all) => Ok(all),
            FileContents::Older => Ok(CredentialsFile::default()),
            FileContents::Corrupt => {
                self.set_aside_unreadable_file()?;
                Ok(CredentialsFile::default())
            }
            FileContents::Newer(version) => Err(newer_format_error(version)),
        }
    }

    fn set_aside_unreadable_file(&self) -> CtResult<()> {
        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("credentials.json");
        let backup = self.path.with_file_name(format!(
            "{file_name}.unreadable-{}",
            Utc::now().format("%Y%m%dT%H%M%S%.fZ")
        ));
        std::fs::rename(&self.path, &backup)
            .map_err(|e| CtError::Store(format!("credentials file set aside: {e}")))?;
        if let Ok(mut recovered) = self.recovered.lock() {
            *recovered = Some(backup);
        }
        Ok(())
    }

    fn acquire_mutation_lock(&self) -> CtResult<StoreLock> {
        let lock_path = self.path.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CtError::Store(format!("credentials dir create: {e}")))?;
        }
        let lock = open_lock_file(&lock_path)?;
        lock.lock_exclusive()
            .map_err(|e| CtError::Store(format!("credentials lock acquire: {e}")))?;
        Ok(StoreLock::file(lock))
    }

    fn save_unlocked(&self, token: &StoredToken) -> CtResult<()> {
        let mut all = self.read_for_write()?;
        let login = self.login();
        all.origins
            .entry(self.origin.clone())
            .or_default()
            .insert(token, &login)?;
        self.write_all(&all)
    }

    fn replace_unlocked(&self, token: &StoredToken) -> CtResult<()> {
        let mut all = self.read_for_write()?;
        let login = self.login();
        all.origins
            .entry(self.origin.clone())
            .or_default()
            .replace(token, &login)?;
        self.write_all(&all)
    }

    fn readable_for_clear(&self) -> CtResult<Option<CredentialsFile>> {
        match self.read_all() {
            Ok(all) => Ok(Some(all)),
            Err(CtError::ObsoleteCredentials { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn clear_unlocked(&self) -> CtResult<()> {
        let Some(mut all) = self.readable_for_clear()? else {
            return Ok(());
        };
        let login = self.login();
        if let Some(credentials) = all.origins.get_mut(&self.origin) {
            credentials.remove(&self.selector, &login)?;
            if credentials.workspaces.is_empty() {
                all.origins.remove(&self.origin);
            }
            self.write_all(&all)?;
        }
        Ok(())
    }

    fn clear_all_unlocked(&self) -> CtResult<()> {
        let Some(mut all) = self.readable_for_clear()? else {
            return Ok(());
        };
        if all.origins.remove(&self.origin).is_some() {
            self.write_all(&all)?;
        }
        Ok(())
    }

    fn write_all(&self, all: &CredentialsFile) -> CtResult<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CtError::Store(format!("credentials dir create: {e}")))?;
        }
        let json = serde_json::to_string_pretty(all)
            .map_err(|e| CtError::Store(format!("credentials encode: {e}")))?;
        #[cfg(unix)]
        {
            self.write_atomic_unix(&json)?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&self.path, &json)
                .map_err(|e| CtError::Store(format!("credentials file write: {e}")))?;
        }
        // The renamed-in file is already 0600 (created that way, and POSIX
        // `rename` preserves the source inode's mode) — this re-assertion is
        // defense-in-depth, not the primary guarantee.
        set_owner_only(&self.path)?;
        Ok(())
    }

    /// Write `json` to a sibling temp file (created 0600, up front — no
    /// write-then-chmod TOCTOU window) and `rename` it over `self.path`.
    /// `rename(2)` within the same directory is atomic on POSIX, so a crash or
    /// a failed write never truncates or corrupts the existing credentials
    /// file: readers see either the old contents or the new ones, never a
    /// partial write.
    #[cfg(unix)]
    fn write_atomic_unix(&self, json: &str) -> CtResult<()> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;

        let parent = self
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let file_name = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("credentials.json");
        let tmp_path = parent.join(format!(".{file_name}.tmp-{:08x}", rand::random::<u32>()));

        let write_result = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)
            .and_then(|mut f| {
                f.write_all(json.as_bytes())?;
                f.flush()?;
                f.sync_all()
            });

        if let Err(e) = write_result {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(CtError::Store(format!("credentials file write: {e}")));
        }

        std::fs::rename(&tmp_path, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            CtError::Store(format!("credentials file rename: {e}"))
        })
    }
}

impl TokenStore for FileStore {
    fn acquire_lock(&self) -> CtResult<StoreLock> {
        self.acquire_mutation_lock()
    }

    fn load(&self) -> CtResult<Option<StoredToken>> {
        let all = self.read_all()?;
        match all.origins.get(&self.origin) {
            Some(credentials) => credentials.select(&self.selector, &self.login()),
            None => Ok(None),
        }
    }

    fn load_all(&self) -> CtResult<Vec<StoredToken>> {
        Ok(self
            .read_all()?
            .origins
            .get(&self.origin)
            .map(|credentials| credentials.workspaces.values().cloned().collect())
            .unwrap_or_default())
    }

    fn load_locked(&self, _lock: &StoreLock) -> CtResult<Option<StoredToken>> {
        self.load()
    }

    fn load_all_locked(&self, _lock: &StoreLock) -> CtResult<Vec<StoredToken>> {
        self.load_all()
    }

    fn save(&self, token: &StoredToken) -> CtResult<()> {
        let _lock = self.acquire_mutation_lock()?;
        self.save_unlocked(token)
    }

    fn save_locked(&self, _lock: &StoreLock, token: &StoredToken) -> CtResult<()> {
        self.save_unlocked(token)
    }

    fn replace_locked(&self, _lock: &StoreLock, token: &StoredToken) -> CtResult<()> {
        self.replace_unlocked(token)
    }

    fn workspaces(&self) -> CtResult<Vec<StoredWorkspace>> {
        Ok(self
            .read_all()?
            .origins
            .get(&self.origin)
            .map(OriginCredentials::listing)
            .unwrap_or_default())
    }

    fn activate(&self, selector: &WorkspaceSelector) -> CtResult<StoredWorkspace> {
        let _lock = self.acquire_mutation_lock()?;
        let mut all = self.read_all()?;
        let login = self.login();
        let credentials = all.origins.get_mut(&self.origin).ok_or_else(|| {
            CtError::Auth(format!("no stored login for this host; run `{login}`"))
        })?;
        let id = credentials.activate(selector, &login)?;
        let activated = credentials
            .listing()
            .into_iter()
            .find(|workspace| workspace.workspace_id == id)
            .ok_or_else(|| missing_workspace_error(&id.to_string(), &login))?;
        self.write_all(&all)?;
        Ok(activated)
    }

    fn recovered_file(&self) -> Option<PathBuf> {
        self.recovered.lock().ok().and_then(|path| path.clone())
    }

    fn clear(&self) -> CtResult<()> {
        let _lock = self.acquire_mutation_lock()?;
        self.clear_unlocked()
    }

    fn clear_locked(&self, _lock: &StoreLock) -> CtResult<()> {
        self.clear_unlocked()
    }

    fn clear_all(&self) -> CtResult<()> {
        let _lock = self.acquire_mutation_lock()?;
        self.clear_all_unlocked()
    }

    fn clear_all_locked(&self, _lock: &StoreLock) -> CtResult<()> {
        self.clear_all_unlocked()
    }

    fn refresh_enabled(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CredentialsFile {
    version: u8,
    origins: BTreeMap<String, OriginCredentials>,
}

impl Default for CredentialsFile {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            origins: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OriginCredentials {
    #[serde(default)]
    active_workspace_id: Option<Uuid>,
    #[serde(default)]
    workspaces: BTreeMap<Uuid, StoredToken>,
}

impl OriginCredentials {
    fn insert(&mut self, token: &StoredToken, login: &str) -> CtResult<()> {
        let workspace_id = self.replace(token, login)?;
        self.active_workspace_id = Some(workspace_id);
        Ok(())
    }

    fn replace(&mut self, token: &StoredToken, login: &str) -> CtResult<Uuid> {
        let workspace_id = token.workspace_id.ok_or_else(|| {
            CtError::Store(format!(
                "login response did not include a workspace; run `{login}` again"
            ))
        })?;
        self.workspaces.insert(workspace_id, token.clone());
        Ok(workspace_id)
    }

    fn listing(&self) -> Vec<StoredWorkspace> {
        self.workspaces
            .iter()
            .map(|(id, token)| StoredWorkspace {
                workspace_id: *id,
                workspace_name: token.workspace_name.clone(),
                active: self.active_workspace_id == Some(*id),
                expires_at: token.expires_at,
            })
            .collect()
    }

    fn activate(&mut self, selector: &WorkspaceSelector, login: &str) -> CtResult<Uuid> {
        let id = match selector {
            WorkspaceSelector::Active => self.active_workspace_id,
            WorkspaceSelector::IdOrName(_) => self.selected_id(selector)?,
        }
        .filter(|id| self.workspaces.contains_key(id))
        .ok_or_else(|| self.missing_selection_error(selector, login))?;
        self.active_workspace_id = Some(id);
        Ok(id)
    }

    fn stored_summary(&self) -> String {
        self.workspaces
            .iter()
            .map(|(id, token)| match &token.workspace_name {
                Some(name) => format!("{name} ({id})"),
                None => id.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn missing_selection_error(&self, selector: &WorkspaceSelector, login: &str) -> CtError {
        match selector {
            WorkspaceSelector::Active if !self.workspaces.is_empty() => CtError::Auth(format!(
                "no active workspace for this host; stored workspaces: {}. Run `cloudthinker auth switch <id|name>`, or run `{login}`",
                self.stored_summary()
            )),
            WorkspaceSelector::Active => missing_workspace_error("active", login),
            WorkspaceSelector::IdOrName(value) => {
                if self.workspaces.is_empty() {
                    missing_workspace_error(value, login)
                } else {
                    CtError::Auth(format!(
                        "no stored credential for workspace `{value}`; stored workspaces: {}. Run `{login}` to add it",
                        self.stored_summary()
                    ))
                }
            }
        }
    }

    fn selected_id(&self, selector: &WorkspaceSelector) -> CtResult<Option<Uuid>> {
        match selector {
            WorkspaceSelector::Active => Ok(self.active_workspace_id),
            WorkspaceSelector::IdOrName(value) => {
                if let Ok(id) = Uuid::parse_str(value) {
                    return Ok(Some(id));
                }
                let matches: Vec<Uuid> = self
                    .workspaces
                    .iter()
                    .filter_map(|(id, token)| {
                        (token.workspace_name.as_deref() == Some(value.as_str())).then_some(*id)
                    })
                    .collect();
                match matches.as_slice() {
                    [] => Ok(None),
                    [id] => Ok(Some(*id)),
                    ids => Err(CtError::Usage(format!(
                        "workspace name `{value}` is ambiguous; use one of these IDs: {}",
                        ids.iter()
                            .map(Uuid::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))),
                }
            }
        }
    }

    fn select(&self, selector: &WorkspaceSelector, login: &str) -> CtResult<Option<StoredToken>> {
        let Some(id) = self.selected_id(selector)? else {
            return match selector {
                WorkspaceSelector::Active if self.workspaces.is_empty() => Ok(None),
                _ => Err(self.missing_selection_error(selector, login)),
            };
        };
        match self.workspaces.get(&id) {
            Some(token) => Ok(Some(token.clone())),
            None => {
                Err(self
                    .missing_selection_error(&WorkspaceSelector::IdOrName(id.to_string()), login))
            }
        }
    }

    fn remove(&mut self, selector: &WorkspaceSelector, login: &str) -> CtResult<()> {
        if let Some(id) = self.selected_id(selector)? {
            let removed = self.workspaces.remove(&id);
            if removed.is_none() && matches!(selector, WorkspaceSelector::IdOrName(_)) {
                return Err(missing_workspace_error(&id.to_string(), login));
            }
            if self.active_workspace_id == Some(id) {
                self.active_workspace_id = None;
            }
        } else if let WorkspaceSelector::IdOrName(value) = selector {
            return Err(missing_workspace_error(value, login));
        }
        Ok(())
    }
}

fn missing_workspace_error(selection: &str, login: &str) -> CtError {
    CtError::Auth(format!(
        "no stored credential for workspace `{selection}`; run `{login}`"
    ))
}

fn newer_format_error(version: u64) -> CtError {
    CtError::Store(format!(
        "the credentials file uses format {version}, which is newer than this cloudthinker {} understands; it was left unchanged. Run `cloudthinker update`",
        env!("CARGO_PKG_VERSION")
    ))
}

enum FileContents {
    Current(CredentialsFile),
    Older,
    Newer(u64),
    Corrupt,
}

fn decode_file(text: &str) -> FileContents {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return FileContents::Corrupt;
    };
    if !value.is_object() {
        return FileContents::Corrupt;
    }
    let Some(version) = value.get("version") else {
        return FileContents::Older;
    };
    let current = u64::from(STORE_VERSION);
    match version.as_u64() {
        Some(version) if version == current => {
            serde_json::from_value(value).map_or(FileContents::Corrupt, FileContents::Current)
        }
        Some(version) if version > current => FileContents::Newer(version),
        Some(_) => FileContents::Older,
        None => FileContents::Corrupt,
    }
}

fn decode_origin(text: &str, source: &str) -> CtResult<OriginCredentials> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| CtError::Store(format!("{source} decode: {e}")))?;
    if value.get("workspaces").is_none() {
        return Err(CtError::Store(format!(
            "{source} decode: obsolete login format"
        )));
    }
    serde_json::from_value(value).map_err(|e| CtError::Store(format!("{source} decode: {e}")))
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

#[cfg(unix)]
fn open_lock_file(path: &std::path::Path) -> CtResult<File> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
        .open(path)
        .map_err(|e| CtError::Store(format!("credentials lock open: {e}")))?;
    let file_type = lock
        .metadata()
        .map_err(|e| CtError::Store(format!("credentials lock metadata: {e}")))?
        .file_type();
    if !file_type.is_file() {
        return Err(CtError::Store(
            "credentials lock target is not a regular file".into(),
        ));
    }
    lock.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| CtError::Store(format!("credentials lock chmod: {e}")))?;
    Ok(lock)
}

#[cfg(not(unix))]
fn open_lock_file(path: &std::path::Path) -> CtResult<File> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| CtError::Store(format!("credentials lock open: {e}")))?;
    if !lock
        .metadata()
        .map_err(|e| CtError::Store(format!("credentials lock metadata: {e}")))?
        .is_file()
    {
        return Err(CtError::Store(
            "credentials lock target is not a regular file".into(),
        ));
    }
    Ok(lock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::stored;

    fn workspace_token(id: u128, name: &str, access: &str) -> StoredToken {
        StoredToken {
            access_token: access.into(),
            refresh_token: Some(format!("refresh-{access}")),
            expires_at: None,
            workspace_id: Some(Uuid::from_u128(id)),
            workspace_name: Some(name.into()),
        }
    }

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

    // CA-CLI-6: the adjacent mutation lock carries the same owner-only mode.
    #[cfg(unix)]
    #[test]
    fn ca_cli_6_file_store_lock_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = FileStore::new(path.clone(), "app.example.com");
        store.save(&stored("access", "refresh")).unwrap();

        let mode = std::fs::metadata(path.with_extension("lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "credentials lock must be 0600");
    }

    // CA-CLI-6: a lock symlink must never redirect the process to an
    // attacker-chosen file.
    #[cfg(unix)]
    #[test]
    fn ca_cli_6_file_store_rejects_symlinked_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let lock_path = path.with_extension("lock");
        let target = dir.path().join("target");
        std::fs::write(&target, b"target").unwrap();
        std::os::unix::fs::symlink(&target, &lock_path).unwrap();
        let store = FileStore::new(path, "app.example.com");

        let error = store.save(&stored("access", "refresh")).unwrap_err();

        assert!(matches!(error, CtError::Store(message) if message.contains("lock")));
        assert_eq!(std::fs::read(&target).unwrap(), b"target");
    }

    #[cfg(unix)]
    #[test]
    fn ca_cli_6_lock_open_refuses_a_dangling_symlink_without_creating_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("credentials.lock");
        let target = dir.path().join("elsewhere").join("planted");
        std::fs::create_dir(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &lock_path).unwrap();

        let error = open_lock_file(&lock_path).unwrap_err();

        assert!(
            matches!(&error, CtError::Store(message) if message.starts_with("credentials lock open:")),
            "{error:?}"
        );
        assert!(!target.exists());
    }

    // A failed write must never truncate the existing credentials file: the
    // atomic write goes to a sibling temp file first, so if the write can't
    // even start the real file is untouched. This forces the failure path in
    // `write_atomic_unix` that a mocked `Write` couldn't reach without
    // duplicating the real fs.
    //
    // The failure is forced by pointing the store at a symlinked directory and
    // then re-aiming that symlink at a regular file, so `create_dir_all` fails
    // with ENOTDIR. Directory permission bits are deliberately NOT used: CI
    // runs the suite as root, and root bypasses them, so a `0o500` directory
    // would stay writable and the write would wrongly succeed.
    #[cfg(unix)]
    #[test]
    fn ca_cli_6_failed_write_never_truncates_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real");
        let link_dir = dir.path().join("link");
        let blocker = dir.path().join("blocker");
        std::fs::create_dir(&real_dir).unwrap();
        std::fs::write(&blocker, b"not a directory").unwrap();
        std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();

        let real_path = real_dir.join("credentials.json");
        let store = FileStore::new(link_dir.join("credentials.json"), "app.example.com");
        store
            .save(&stored("first-access", "first-refresh"))
            .unwrap();
        let original = std::fs::read_to_string(&real_path).unwrap();

        // Re-aim the symlink at a regular file: every uid, root included, now
        // fails to resolve the parent as a directory.
        std::fs::remove_file(&link_dir).unwrap();
        std::os::unix::fs::symlink(&blocker, &link_dir).unwrap();

        let result = store.save(&stored("second-access", "second-refresh"));

        assert!(
            result.is_err(),
            "write below a non-directory parent must fail"
        );
        let after = std::fs::read_to_string(&real_path).unwrap();
        assert_eq!(
            after, original,
            "a failed write must not touch the existing credentials file"
        );
    }

    #[test]
    fn file_store_absorbs_keyring_logins_only_when_it_has_none_for_the_origin() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("credentials.json"), "host.example");
        let mut from_keyring = OriginCredentials::default();
        from_keyring
            .insert(&stored("keyring-access", "r"), "cloudthinker login")
            .unwrap();

        assert!(store.absorb(from_keyring.clone()).unwrap());
        assert_eq!(
            store.load().unwrap().map(|t| t.access_token),
            Some("keyring-access".to_string())
        );

        store.save(&stored("newer-access", "r2")).unwrap();
        assert!(!store.absorb(from_keyring).unwrap());
        assert_eq!(
            store.load().unwrap().map(|t| t.access_token),
            Some("newer-access".to_string())
        );
    }

    #[test]
    fn file_store_is_keyed_by_origin() {
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

    #[test]
    fn file_store_keeps_multiple_workspaces_and_activates_the_latest_login() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = FileStore::new(path.clone(), "https://app.example:443");
        store
            .save(&workspace_token(1, "Development", "dev"))
            .unwrap();
        store
            .save(&workspace_token(2, "Production", "prod"))
            .unwrap();

        assert_eq!(store.load().unwrap().unwrap().access_token, "prod");
        let selected = FileStore::with_selector(
            path,
            "https://app.example:443",
            WorkspaceSelector::IdOrName(Uuid::from_u128(1).to_string()),
        );
        assert_eq!(selected.load().unwrap().unwrap().access_token, "dev");
    }

    #[test]
    fn workspace_name_selection_is_exact_and_ambiguous_names_list_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = FileStore::new(path.clone(), "origin");
        store.save(&workspace_token(1, "Shared", "one")).unwrap();
        store.save(&workspace_token(2, "Shared", "two")).unwrap();
        let selected =
            FileStore::with_selector(path, "origin", WorkspaceSelector::IdOrName("Shared".into()));

        let error = selected.load().unwrap_err();
        assert!(matches!(error, CtError::Usage(message)
            if message.contains(&Uuid::from_u128(1).to_string())
                && message.contains(&Uuid::from_u128(2).to_string())));
    }

    #[test]
    fn missing_workspace_never_falls_back_to_active() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = FileStore::new(path.clone(), "origin");
        store
            .save(&workspace_token(1, "Development", "dev"))
            .unwrap();
        let selected = FileStore::with_selector(
            path,
            "origin",
            WorkspaceSelector::IdOrName("Production".into()),
        );

        assert!(matches!(selected.load(), Err(CtError::Auth(_))));
    }

    #[test]
    fn clearing_active_workspace_does_not_activate_another_credential() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = FileStore::new(path, "origin");
        store
            .save(&workspace_token(1, "Development", "dev"))
            .unwrap();
        store
            .save(&workspace_token(2, "Production", "prod"))
            .unwrap();
        store.clear().unwrap();

        assert!(matches!(store.load(), Err(CtError::Auth(_))));
    }

    #[test]
    fn legacy_origin_only_file_requires_login_and_login_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(
            &path,
            r#"{"origin":{"access_token":"old","refresh_token":"old"}}"#,
        )
        .unwrap();
        let store = FileStore::new(path, "origin");

        assert!(matches!(
            store.load(),
            Err(CtError::ObsoleteCredentials { .. })
        ));
        store
            .save(&workspace_token(1, "Development", "new"))
            .unwrap();
        assert_eq!(store.load().unwrap().unwrap().access_token, "new");
    }

    #[test]
    fn clear_all_removes_every_workspace_for_only_this_origin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let origin = FileStore::new(path.clone(), "origin");
        origin.save(&workspace_token(1, "One", "one")).unwrap();
        origin.save(&workspace_token(2, "Two", "two")).unwrap();
        let other = FileStore::new(path, "other");
        other.save(&workspace_token(3, "Three", "three")).unwrap();

        origin.clear_all().unwrap();

        assert_eq!(origin.load().unwrap(), None);
        assert_eq!(other.load().unwrap().unwrap().access_token, "three");
    }

    #[test]
    fn a_file_from_a_newer_release_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let newer = r#"{"version":3,"origins":{"other":{"future":true}}}"#;
        std::fs::write(&path, newer).unwrap();
        let store = FileStore::new(path.clone(), "origin");

        let save = store.save(&workspace_token(1, "One", "one")).unwrap_err();
        let load = store.load().unwrap_err();

        assert!(
            matches!(&save, CtError::Store(message) if message.contains("cloudthinker update"))
        );
        assert!(matches!(&load, CtError::Store(message) if message.contains("format 3")));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);
    }

    #[test]
    fn an_unreadable_file_is_set_aside_before_a_login_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, b"{not json").unwrap();
        let store = FileStore::new(path.clone(), "https://dev.cloudthinker.io:443");

        let load = store.load().unwrap_err();
        assert!(
            matches!(&load, CtError::ObsoleteCredentials { origin } if origin == "https://dev.cloudthinker.io:443"),
            "{load:?}"
        );
        store.save(&workspace_token(1, "One", "one")).unwrap();

        let recovered = store.recovered_file().expect("the unreadable file is kept");
        assert_eq!(std::fs::read(&recovered).unwrap(), b"{not json");
        assert_eq!(store.load().unwrap().unwrap().access_token, "one");
    }

    #[test]
    fn a_file_that_holds_this_origin_skips_the_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("credentials.json"), "origin");
        store.save(&workspace_token(1, "One", "one")).unwrap();

        store
            .absorb_keyring_logins_from(
                || panic!("the keyring must not be read"),
                || panic!("the keyring must not be written"),
            )
            .unwrap();

        let empty = FileStore::new(dir.path().join("empty.json"), "origin");
        let mut read = false;
        empty
            .absorb_keyring_logins_from(
                || {
                    read = true;
                    None
                },
                || {},
            )
            .unwrap();
        assert!(read);
    }

    #[test]
    fn switching_activates_a_stored_workspace_and_lists_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = FileStore::new(path, "origin");
        store
            .save(&workspace_token(1, "Development", "dev"))
            .unwrap();
        store
            .save(&workspace_token(2, "Production", "prod"))
            .unwrap();

        let switched = store
            .activate(&WorkspaceSelector::IdOrName("Development".into()))
            .unwrap();

        assert!(switched.active);
        assert_eq!(store.load().unwrap().unwrap().access_token, "dev");
        let listed = store.workspaces().unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed.iter().filter(|w| w.active).count(), 1);
        let missing = store
            .activate(&WorkspaceSelector::IdOrName("Staging".into()))
            .unwrap_err();
        assert!(
            matches!(&missing, CtError::Auth(message) if message.contains("Development (") && message.contains("Production (")),
            "{missing:?}"
        );
    }

    #[test]
    fn a_cleared_active_workspace_names_the_stored_ones() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("credentials.json"), "origin");
        store
            .save(&workspace_token(1, "Development", "dev"))
            .unwrap();
        store
            .save(&workspace_token(2, "Production", "prod"))
            .unwrap();
        store.clear().unwrap();

        let error = store.load().unwrap_err();

        assert!(
            matches!(&error, CtError::Auth(message)
                if message.contains("no active workspace")
                    && message.contains("Development (")
                    && message.contains("auth switch")),
            "{error:?}"
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
