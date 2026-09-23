use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CtError, CtResult};

const CACHE_FILE_NAME: &str = "update-check.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateCache {
    pub channel: Option<String>,
    pub latest_version: Option<String>,
    pub checked_at_unix: u64,
    pub failed_at_unix: u64,
    pub dismissed_version: Option<String>,
}

pub fn update_cache_path() -> CtResult<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| CtError::Store("could not resolve the home directory".to_string()))?;
    Ok(home.join(".cloudthinker").join(CACHE_FILE_NAME))
}

impl UpdateCache {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> CtResult<()> {
        let parent = path
            .parent()
            .ok_or_else(|| CtError::Store(format!("no parent for {}", path.display())))?;
        std::fs::create_dir_all(parent).map_err(|e| store_error("create", parent, &e))?;
        let body = serde_json::to_vec_pretty(self).map_err(|e| CtError::Store(e.to_string()))?;
        let tmp = parent.join(format!(
            ".{CACHE_FILE_NAME}.tmp-{:08x}",
            rand::random::<u32>()
        ));
        std::fs::write(&tmp, body).map_err(|e| store_error("write", &tmp, &e))?;
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            store_error("replace", path, &e)
        })
    }

    pub fn latest_for(&self, channel: &str) -> Option<&str> {
        (self.channel.as_deref() == Some(channel))
            .then_some(self.latest_version.as_deref())
            .flatten()
    }

    pub fn is_fresh(
        &self,
        channel: &str,
        now_unix: u64,
        max_age_secs: u64,
        retry_after_secs: u64,
    ) -> bool {
        self.channel.as_deref() == Some(channel)
            && (younger_than(self.checked_at_unix, now_unix, max_age_secs)
                || younger_than(self.failed_at_unix, now_unix, retry_after_secs))
    }

    pub fn record_check(&mut self, channel: &str, latest: Option<String>, now_unix: u64) {
        if self.channel.as_deref() != Some(channel) {
            self.latest_version = None;
            self.checked_at_unix = 0;
            self.failed_at_unix = 0;
        }
        self.channel = Some(channel.to_string());
        match latest {
            Some(version) => {
                self.latest_version = Some(version);
                self.checked_at_unix = now_unix;
                self.failed_at_unix = 0;
            }
            None => self.failed_at_unix = now_unix,
        }
    }

    pub fn is_dismissed(&self, version: &str) -> bool {
        self.dismissed_version.as_deref() == Some(version)
    }
}

fn younger_than(stamp_unix: u64, now_unix: u64, max_age_secs: u64) -> bool {
    stamp_unix > 0
        && now_unix
            .checked_sub(stamp_unix)
            .is_some_and(|age| age < max_age_secs)
}

fn store_error(action: &str, path: &Path, error: &std::io::Error) -> CtError {
    CtError::Store(format!("could not {action} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 60 * 60;
    const HOUR: u64 = 60 * 60;

    fn cache(channel: &str, latest: &str, checked_at_unix: u64) -> UpdateCache {
        UpdateCache {
            channel: Some(channel.to_string()),
            latest_version: Some(latest.to_string()),
            checked_at_unix,
            failed_at_unix: 0,
            dismissed_version: None,
        }
    }

    #[test]
    fn a_recent_check_for_the_same_channel_is_fresh() {
        let cache = cache("stable", "0.6.0", 1_000);
        assert!(cache.is_fresh("stable", 1_000 + DAY - 1, DAY, HOUR));
    }

    #[test]
    fn an_old_check_is_stale() {
        let cache = cache("stable", "0.6.0", 1_000);
        assert!(!cache.is_fresh("stable", 1_000 + DAY, DAY, HOUR));
    }

    #[test]
    fn a_check_stamped_in_the_future_is_stale() {
        let cache = cache("stable", "0.6.0", 5_000);
        assert!(!cache.is_fresh("stable", 1_000, DAY, HOUR));
    }

    #[test]
    fn another_channel_is_stale_and_has_no_latest() {
        let cache = cache("stable", "0.6.0", 1_000);
        assert!(!cache.is_fresh("dev", 1_000, DAY, HOUR));
        assert_eq!(cache.latest_for("dev"), None);
        assert_eq!(cache.latest_for("stable"), Some("0.6.0"));
    }

    #[test]
    fn a_failed_check_keeps_the_known_latest_and_retries_after_the_retry_interval() {
        let mut cache = cache("stable", "0.6.0", 1_000);
        let failed_at = 1_000 + DAY;
        cache.record_check("stable", None, failed_at);
        assert_eq!(cache.latest_for("stable"), Some("0.6.0"));
        assert_eq!(cache.checked_at_unix, 1_000);
        assert!(cache.is_fresh("stable", failed_at + HOUR - 1, DAY, HOUR));
        assert!(!cache.is_fresh("stable", failed_at + HOUR, DAY, HOUR));
    }

    #[test]
    fn a_successful_check_after_a_failure_is_fresh_for_the_full_interval() {
        let mut cache = cache("stable", "0.6.0", 1_000);
        cache.record_check("stable", None, 1_000 + DAY);
        cache.record_check("stable", Some("0.6.1".to_string()), 2_000 + DAY);
        assert_eq!(cache.latest_for("stable"), Some("0.6.1"));
        assert_eq!(cache.failed_at_unix, 0);
        assert!(cache.is_fresh("stable", 2_000 + DAY + HOUR, DAY, HOUR));
    }

    #[test]
    fn a_check_on_another_channel_drops_the_old_latest() {
        let mut cache = cache("stable", "0.6.0", 1_000);
        cache.dismissed_version = Some("0.6.0".to_string());
        cache.record_check("dev", None, 2_000);
        assert_eq!(cache.latest_for("dev"), None);
        cache.record_check("dev", Some("0.7.0-dev.1".to_string()), 3_000);
        assert_eq!(cache.latest_for("dev"), Some("0.7.0-dev.1"));
        assert!(cache.is_dismissed("0.6.0"));
    }

    #[test]
    fn a_missing_or_corrupt_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE_NAME);
        assert_eq!(UpdateCache::load(&path), UpdateCache::default());
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(UpdateCache::load(&path), UpdateCache::default());
    }

    #[test]
    fn save_then_load_round_trips_and_creates_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(CACHE_FILE_NAME);
        let mut saved = cache("dev", "0.6.0-dev.1", 42);
        saved.dismissed_version = Some("0.6.0-dev.1".to_string());
        saved.save(&path).unwrap();
        let loaded = UpdateCache::load(&path);
        assert_eq!(loaded, saved);
        assert!(loaded.is_dismissed("0.6.0-dev.1"));
        assert!(!loaded.is_dismissed("0.6.0"));
    }
}
