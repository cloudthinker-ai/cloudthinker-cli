//! `cloudthinker-client` — the only hand-written crate that knows the wire.
//!
//! Everything the CLI needs to talk to CloudThinker: the typed `CtClient`, PKCE
//! login, token storage/refresh, and the `CtError` taxonomy. Future TUI/MCP
//! surfaces reuse this crate unchanged.

// Tests lean on unwrap/expect/panic for fixture setup and assertions; the deny
// lints stay in force for all non-test code.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod auth;
mod client;
mod error;
mod review_url;

#[cfg(test)]
mod test_support;

pub use auth::pkce::{Loopback, PkceChallenge, consent_url};
pub use auth::refresh::{PROACTIVE_REFRESH_SKEW_SECS, RefreshCoordinator};
pub use auth::store::{
    AutoStore, EnvTokenStore, FileStore, KeyringStore, SaveLocation, StoredToken, TOKEN_ENV_VAR,
    TokenStore,
};
pub use client::{
    CtClient, ReviewFinding, ReviewSeverityCounts, ReviewStatus, ReviewVerdict, ReviewView,
    RunStatus, RunView, SubmittedRun, host_of, persistent_store, resolve_store,
};
pub use error::{CtError, CtResult};
pub use review_url::{MrCoordinates, MrProvider, parse_mr_url};
