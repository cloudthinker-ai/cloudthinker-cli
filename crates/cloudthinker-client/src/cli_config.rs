use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::client::{DEFAULT_BASE_URL, origin_of};
use crate::error::CtResult;
use crate::update_cache::{cloudthinker_home, write_json_file};

const CONFIG_FILE_NAME: &str = "config.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CliConfig {
    pub default_url: Option<String>,
}

pub fn cli_config_path() -> CtResult<PathBuf> {
    Ok(cloudthinker_home()?.join(CONFIG_FILE_NAME))
}

impl CliConfig {
    pub fn load_default() -> Self {
        cli_config_path()
            .map(|path| Self::load(&path))
            .unwrap_or_default()
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> CtResult<()> {
        write_json_file(path, self)
    }

    pub fn saved_url(&self) -> Option<&str> {
        self.default_url
            .as_deref()
            .filter(|url| origin_of(url).is_ok())
    }

    pub fn remember_url(&mut self, url: &str, built_in: &str) -> CtResult<bool> {
        let origin = origin_of(url)?;
        let next = (origin != origin_of(built_in)?).then(|| url.trim_end_matches('/').to_string());
        let changed = self.default_url != next;
        self.default_url = next;
        Ok(changed)
    }

    pub fn forget_url(&mut self, url: &str) -> bool {
        let remembered = self
            .saved_url()
            .and_then(|saved| origin_of(saved).ok())
            .is_some_and(|saved| origin_of(url).is_ok_and(|origin| origin == saved));
        if remembered {
            self.default_url = None;
        }
        remembered
    }
}

pub fn effective_default_url() -> String {
    resolve_base_url(None, &CliConfig::load_default(), DEFAULT_BASE_URL)
}

pub fn resolve_base_url(explicit: Option<String>, config: &CliConfig, built_in: &str) -> String {
    explicit
        .or_else(|| config.saved_url().map(str::to_string))
        .unwrap_or_else(|| built_in.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROD: &str = "https://app.cloudthinker.io";
    const DEV: &str = "https://dev.cloudthinker.io";

    #[test]
    fn an_explicit_url_wins_over_the_saved_one_and_the_saved_one_over_the_built_in() {
        let saved = CliConfig {
            default_url: Some(DEV.into()),
        };
        assert_eq!(
            resolve_base_url(Some("http://localhost:8000".into()), &saved, PROD),
            "http://localhost:8000"
        );
        assert_eq!(resolve_base_url(None, &saved, PROD), DEV);
        assert_eq!(resolve_base_url(None, &CliConfig::default(), PROD), PROD);
    }

    #[test]
    fn an_unusable_saved_url_falls_back_to_the_built_in() {
        let saved = CliConfig {
            default_url: Some("http://example.com".into()),
        };
        assert_eq!(resolve_base_url(None, &saved, PROD), PROD);
    }

    #[test]
    fn remembering_the_built_in_origin_clears_the_saved_default() {
        let mut config = CliConfig::default();
        assert!(config.remember_url(&format!("{DEV}/"), PROD).unwrap());
        assert_eq!(config.default_url.as_deref(), Some(DEV));
        assert!(!config.remember_url(DEV, PROD).unwrap());
        assert!(config.remember_url(PROD, PROD).unwrap());
        assert_eq!(config.default_url, None);
    }

    #[test]
    fn logging_out_of_the_remembered_origin_forgets_it() {
        let mut config = CliConfig {
            default_url: Some(DEV.into()),
        };
        assert!(!config.forget_url("http://localhost:8000"));
        assert_eq!(config.default_url.as_deref(), Some(DEV));
        assert!(config.forget_url("https://dev.cloudthinker.io:443/"));
        assert_eq!(config.default_url, None);
        assert!(!config.forget_url(DEV));
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(CONFIG_FILE_NAME);
        let config = CliConfig {
            default_url: Some(DEV.into()),
        };
        config.save(&path).unwrap();
        assert_eq!(CliConfig::load(&path), config);
    }
}
