//! Test doubles shared across the crate's unit tests. Compiled only under test.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::auth::store::{SaveLocation, StoredToken, TokenStore};
use crate::error::{CtError, CtResult};

/// In-memory `TokenStore` double. Tracks save calls and supports simulating an
/// external rotation via [`MockTokenStore::set`].
pub struct MockTokenStore {
    inner: Mutex<Option<StoredToken>>,
    refresh_enabled: bool,
    save_count: AtomicUsize,
}

impl MockTokenStore {
    pub fn new(initial: Option<StoredToken>) -> Self {
        Self {
            inner: Mutex::new(initial),
            refresh_enabled: true,
            save_count: AtomicUsize::new(0),
        }
    }

    /// A read-only double (models the env-var override: refresh disabled).
    pub fn read_only(initial: Option<StoredToken>) -> Self {
        Self {
            inner: Mutex::new(initial),
            refresh_enabled: false,
            save_count: AtomicUsize::new(0),
        }
    }

    pub fn save_count(&self) -> usize {
        self.save_count.load(Ordering::SeqCst)
    }
}

impl TokenStore for MockTokenStore {
    fn load(&self) -> CtResult<Option<StoredToken>> {
        Ok(self.inner.lock().expect("mock lock").clone())
    }

    fn save(&self, token: &StoredToken) -> CtResult<SaveLocation> {
        self.save_count.fetch_add(1, Ordering::SeqCst);
        *self.inner.lock().expect("mock lock") = Some(token.clone());
        Ok(SaveLocation::File)
    }

    fn clear(&self) -> CtResult<()> {
        *self.inner.lock().expect("mock lock") = None;
        Ok(())
    }

    fn refresh_enabled(&self) -> bool {
        self.refresh_enabled
    }
}

/// Build a stored credential with the given access + refresh tokens.
pub fn stored(access: &str, refresh: &str) -> StoredToken {
    StoredToken {
        access_token: access.to_string(),
        refresh_token: Some(refresh.to_string()),
        expires_at: None,
        workspace_id: None,
    }
}

/// A minimal API `Token` JSON body for wiremock responses.
pub fn token_json(access: &str, refresh: &str) -> serde_json::Value {
    serde_json::json!({
        "access_token": access,
        "refresh_token": refresh,
        "token_type": "bearer",
    })
}

/// A `TokenStore` whose every operation fails — stands in for an unusable OS
/// keyring so `AutoStore` fallback is testable without the real keyring.
pub struct FailingStore;

impl TokenStore for FailingStore {
    fn load(&self) -> CtResult<Option<StoredToken>> {
        Err(CtError::Store("keyring unavailable".into()))
    }

    fn save(&self, _token: &StoredToken) -> CtResult<SaveLocation> {
        Err(CtError::Store("keyring unavailable".into()))
    }

    fn clear(&self) -> CtResult<()> {
        Err(CtError::Store("keyring unavailable".into()))
    }

    fn refresh_enabled(&self) -> bool {
        true
    }
}
