use std::env;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use axoupdater::{AxoUpdater, AxoupdateError, ReleaseSourceType, UpdateRequest, Version};
use reqwest::header::{ACCEPT, LOCATION};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Deserializer};
use url::Url;

use crate::agent_release::{http_client, read_download};
use crate::error::CtError;

const APP_NAME: &str = "cloudthinker-cli";
const REPO_PATH: &str = "/cloudthinker-ai/cloudthinker-cli";
const GITHUB: &str = "https://github.com";
pub const GHE_BASE_ENV: &str = "CLOUDTHINKER_CLI_INSTALLER_GHE_BASE_URL";
pub const GITHUB_BASE_ENV: &str = "CLOUDTHINKER_CLI_INSTALLER_GITHUB_BASE_URL";
const INSTALLER_EXT: &str = if cfg!(windows) { "ps1" } else { "sh" };
const METADATA_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RECEIPT_BYTES: u64 = 64 * 1024;
const MAX_RELEASE_FEED_BYTES: u64 = 4 * 1024 * 1024;
const MAX_INSTALLER_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy)]
pub enum UpdateTarget<'a> {
    Stable,
    Dev,
    Version(&'a str),
}

impl UpdateTarget<'_> {
    fn api_request(self) -> UpdateRequest {
        match self {
            Self::Stable => UpdateRequest::Latest,
            Self::Dev => UpdateRequest::LatestMaybePrerelease,
            Self::Version(version) => UpdateRequest::SpecificVersion(version.to_owned()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CliUpdateError {
    #[error("{0}")]
    NoReceipt(String),
    #[error("the running binary does not match the install receipt")]
    ReceiptMismatch,
    #[error("GitHub is limiting update checks from your network")]
    RateLimited,
    #[error("cloudthinker {0} was never released")]
    NoSuchRelease(String),
    #[error("{0}")]
    Failed(String),
}

pub struct InstalledCli {
    pub old_version: Option<String>,
    pub new_version: String,
}

pub struct CliUpdater {
    api: AxoUpdater,
    public: Option<PublicReleases>,
}

impl CliUpdater {
    pub fn load() -> Result<Self, CliUpdateError> {
        let mut api = AxoUpdater::new_for(APP_NAME);
        api.set_client(http_client().map_err(failed)?);
        api.load_receipt()
            .map_err(|error| CliUpdateError::NoReceipt(error.to_string()))?;
        if !matches!(api.check_receipt_is_for_this_executable(), Ok(true)) {
            return Err(CliUpdateError::ReceiptMismatch);
        }
        if env::var_os(GHE_BASE_ENV).is_none()
            && env::var_os(GITHUB_BASE_ENV).is_none()
            && api
                .source
                .as_ref()
                .is_some_and(|source| source.release_type == ReleaseSourceType::GitHub)
            && let Ok(token) = env::var("GITHUB_TOKEN")
            && !token.trim().is_empty()
        {
            api.set_github_token(&token);
        }
        let public = PublicReleases::for_receipt(&api);
        Ok(Self { api, public })
    }

    pub async fn latest(
        mut self,
        target: UpdateTarget<'_>,
    ) -> Result<Option<String>, CliUpdateError> {
        let mut web_error = None;
        if let Some(public) = &self.public {
            match public.resolve(target).await {
                Ok(release) => return Ok(Some(release.version.to_string())),
                Err(error) => web_error = Some(error),
            }
        }
        self.api.configure_version_specifier(target.api_request());
        self.api
            .query_new_version()
            .await
            .map(|version| version.map(ToString::to_string))
            .map_err(|error| api_error(error, web_error.as_ref()))
    }

    pub async fn install(
        mut self,
        target: UpdateTarget<'_>,
        force: bool,
    ) -> Result<Option<InstalledCli>, CliUpdateError> {
        let mut web_error = None;
        if let Some(public) = &self.public
            && let Some(receipt) = ReceiptOptions::load()
        {
            match public.resolve(target).await {
                Ok(release) => {
                    let needs_update = match target {
                        UpdateTarget::Version(_) => receipt.version != release.version,
                        UpdateTarget::Stable | UpdateTarget::Dev => {
                            receipt.version < release.version
                        }
                    };
                    if !force && !needs_update {
                        return Ok(None);
                    }
                    match public.download(&release).await {
                        Ok(script) => {
                            let dir = self.api.install_prefix_root().map_err(failed)?;
                            run_installer(script, dir.into_std_path_buf(), receipt.modify_path)
                                .await?;
                            return Ok(Some(InstalledCli {
                                old_version: Some(receipt.version.to_string()),
                                new_version: release.version.to_string(),
                            }));
                        }
                        Err(error) => web_error = Some(error),
                    }
                }
                Err(error) => web_error = Some(error),
            }
        }

        self.api
            .configure_version_specifier(target.api_request())
            .disable_installer_output()
            .always_update(force);
        self.api
            .run()
            .await
            .map(|result| {
                result.map(|result| InstalledCli {
                    old_version: result.old_version.map(|version| version.to_string()),
                    new_version: result.new_version.to_string(),
                })
            })
            .map_err(|error| api_error(error, web_error.as_ref()))
    }
}

fn api_error(error: AxoupdateError, web_error: Option<&PublicError>) -> CliUpdateError {
    match error {
        AxoupdateError::Reqwest(ref error) if error.status().is_some_and(rate_limited) => {
            CliUpdateError::RateLimited
        }
        AxoupdateError::Reqwest(_) | AxoupdateError::VersionNotFound { .. }
            if matches!(web_error, Some(PublicError::RateLimited)) =>
        {
            CliUpdateError::RateLimited
        }
        AxoupdateError::VersionNotFound { version, .. } => CliUpdateError::NoSuchRelease(version),
        error => failed(error),
    }
}

fn failed(error: impl std::fmt::Display) -> CliUpdateError {
    CliUpdateError::Failed(error.to_string())
}

fn rate_limited(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
    )
}

#[derive(Debug, thiserror::Error)]
enum PublicError {
    #[error("public release was unavailable")]
    Unavailable,
    #[error("public release lookup was rate limited")]
    RateLimited,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Transport(#[from] CtError),
}

struct PublicRelease {
    version: Version,
    installer: Url,
}

struct PublicReleases {
    base: Url,
}

impl PublicReleases {
    fn for_receipt(api: &AxoUpdater) -> Option<Self> {
        let source = api.source.as_ref()?;
        if source.release_type != ReleaseSourceType::GitHub
            || source.owner != "cloudthinker-ai"
            || source.name != APP_NAME
            || source.app_name != APP_NAME
            || env::var_os(GITHUB_BASE_ENV).is_some()
        {
            return None;
        }
        let base = env::var(GHE_BASE_ENV).unwrap_or_else(|_| GITHUB.to_owned());
        let base = Url::parse(&base).ok()?;
        Some(Self { base })
    }

    async fn resolve(&self, target: UpdateTarget<'_>) -> Result<PublicRelease, PublicError> {
        let filename = format!("{APP_NAME}-installer.{INSTALLER_EXT}");
        let release_path = format!("{REPO_PATH}/releases");
        let http = crate::tls::http_client_builder()
            .timeout(METADATA_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let tag = match target {
            UpdateTarget::Stable => {
                let latest = self.url(&format!("{release_path}/latest/download/{filename}"))?;
                let response = http.head(latest.clone()).send().await?;
                if !status(&response)?.is_redirection() {
                    return Err(PublicError::Unavailable);
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|header| header.to_str().ok());
                let location = latest
                    .join(location.ok_or(PublicError::Unavailable)?)
                    .map_err(|_| PublicError::Unavailable)?;
                let prefix = format!("{release_path}/download/");
                let suffix = format!("/{filename}");
                if location.origin() != latest.origin() {
                    return Err(PublicError::Unavailable);
                }
                let tag = location
                    .path()
                    .strip_prefix(&prefix)
                    .and_then(|path| path.strip_suffix(&suffix))
                    .ok_or(PublicError::Unavailable)?;
                let release = self.for_tag(tag, &filename)?;
                if !release.version.pre.is_empty() {
                    return Err(PublicError::Unavailable);
                }
                return Ok(release);
            }
            UpdateTarget::Dev => self.dev_tag(&http, &release_path).await?,
            UpdateTarget::Version(version) => format!("v{version}"),
        };

        let release = self.for_tag(&tag, &filename)?;
        status(&http.head(release.installer.clone()).send().await?)?;
        Ok(release)
    }

    async fn dev_tag(&self, http: &Client, release_path: &str) -> Result<String, PublicError> {
        let feed = self.url(&format!("{release_path}.atom"))?;
        let response = http.get(feed.clone()).send().await?;
        if !status(&response)?.is_success() {
            return Err(PublicError::Unavailable);
        }
        let body = read_download(
            response,
            feed.as_str(),
            MAX_RELEASE_FEED_BYTES,
            CtError::Transport,
        )
        .await?;
        let feed: ReleaseFeed =
            quick_xml::de::from_reader(body.as_slice()).map_err(|_| PublicError::Unavailable)?;
        feed.entries
            .into_iter()
            .filter_map(|entry| {
                let tag = entry
                    .id
                    .strip_prefix("tag:github.com,2008:Repository/")?
                    .split_once('/')?
                    .1;
                Some((parse_tag(tag)?, tag.to_owned()))
            })
            .max_by(|left, right| left.0.cmp(&right.0))
            .map(|(_, tag)| tag)
            .ok_or(PublicError::Unavailable)
    }

    fn for_tag(&self, tag: &str, filename: &str) -> Result<PublicRelease, PublicError> {
        let version = parse_tag(tag).ok_or(PublicError::Unavailable)?;
        let installer = self.url(&format!("{REPO_PATH}/releases/download/{tag}/{filename}"))?;
        Ok(PublicRelease { version, installer })
    }

    fn url(&self, path: &str) -> Result<Url, PublicError> {
        self.base.join(path).map_err(|_| PublicError::Unavailable)
    }

    async fn download(&self, release: &PublicRelease) -> Result<Vec<u8>, PublicError> {
        let url = &release.installer;
        let response = crate::tls::http_client_builder()
            .https_only(url.scheme() == "https")
            .timeout(Duration::from_secs(60))
            .build()?
            .get(url.clone())
            .header(ACCEPT, "application/octet-stream")
            .send()
            .await?;
        if !status(&response)?.is_success() {
            return Err(PublicError::Unavailable);
        }
        let script = read_download(
            response,
            url.as_str(),
            MAX_INSTALLER_BYTES,
            CtError::Transport,
        )
        .await?;
        if script.is_empty() || (cfg!(unix) && !script.starts_with(b"#!")) {
            return Err(PublicError::Unavailable);
        }
        Ok(script)
    }
}

fn status(response: &reqwest::Response) -> Result<StatusCode, PublicError> {
    let status = response.status();
    if rate_limited(status) {
        return Err(PublicError::RateLimited);
    }
    if status.is_success() || status.is_redirection() {
        Ok(status)
    } else {
        Err(PublicError::Unavailable)
    }
}

#[derive(Deserialize)]
struct ReleaseFeed {
    #[serde(rename = "entry", default)]
    entries: Vec<ReleaseEntry>,
}

#[derive(Deserialize)]
struct ReleaseEntry {
    id: String,
}

fn parse_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v')?).ok()
}

#[derive(Deserialize)]
struct ReceiptOptions {
    #[serde(deserialize_with = "receipt_version")]
    version: Version,
    #[serde(default = "modify_path_by_default")]
    modify_path: bool,
}

fn modify_path_by_default() -> bool {
    true
}

fn receipt_version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Version, D::Error> {
    let version = String::deserialize(deserializer)?;
    Version::parse(&version).map_err(serde::de::Error::custom)
}

impl ReceiptOptions {
    fn load() -> Option<Self> {
        let filename = format!("{APP_NAME}-receipt.json");
        let path = if env::var("AXOUPDATER_CONFIG_WORKING_DIR").is_ok() {
            env::current_dir().ok()?.join(&filename)
        } else if let Ok(dir) = env::var("AXOUPDATER_CONFIG_PATH") {
            PathBuf::from(dir).join(&filename)
        } else {
            let xdg_path = env::var("XDG_CONFIG_HOME")
                .ok()
                .map(|dir| PathBuf::from(dir).join(APP_NAME).join(&filename));
            match xdg_path.filter(|path| path.exists()) {
                Some(path) => path,
                None => {
                    let dir = if cfg!(windows) {
                        PathBuf::from(env::var("LOCALAPPDATA").ok()?)
                    } else {
                        homedir::my_home().ok().flatten()?.join(".config")
                    };
                    dir.join(APP_NAME).join(&filename)
                }
            }
        };
        let mut body = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(MAX_RECEIPT_BYTES + 1)
            .read_to_end(&mut body)
            .ok()?;
        if body.len() as u64 > MAX_RECEIPT_BYTES {
            return None;
        }
        serde_json::from_slice(&body).ok()
    }
}

async fn run_installer(
    script: Vec<u8>,
    dir: PathBuf,
    modify_path: bool,
) -> Result<(), CliUpdateError> {
    tokio::task::spawn_blocking(move || {
        let temp = tempfile::tempdir().map_err(failed)?;
        let path = temp.path().join(format!("installer.{INSTALLER_EXT}"));
        std::fs::write(&path, &script).map_err(failed)?;
        let mut command = installer_command(&path);
        command
            .env("CARGO_DIST_FORCE_INSTALL_DIR", &dir)
            .env("CLOUDTHINKER_CLI_INSTALL_DIR", &dir)
            .env_remove("GITHUB_TOKEN");
        if !modify_path {
            command.env("CLOUDTHINKER_CLI_NO_MODIFY_PATH", "1");
        }
        let output = installer_output(&mut command).map_err(failed)?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let details = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        Err(failed(format!(
            "the installer exited with {}. {details}",
            output.status
        )))
    })
    .await
    .map_err(failed)?
}

#[cfg(not(windows))]
fn installer_command(path: &Path) -> Command {
    let mut command = Command::new("sh");
    command.arg(path);
    command
}

#[cfg(windows)]
fn installer_command(path: &Path) -> Command {
    let mut command = Command::new("powershell");
    command
        .args(["-ExecutionPolicy", "ByPass", "-File"])
        .arg(path)
        .env_remove("PSModulePath");
    command
}

#[cfg(not(windows))]
fn installer_output(command: &mut Command) -> std::io::Result<Output> {
    command.output()
}

#[cfg(windows)]
fn installer_output(command: &mut Command) -> std::io::Result<Output> {
    let exe = env::current_exe()?;
    let mut backup_name = exe.as_os_str().to_os_string();
    backup_name.push(format!(".{}.previous.exe", std::process::id()));
    let backup = PathBuf::from(backup_name);
    std::fs::rename(&exe, &backup)?;
    let result = command.output();
    if matches!(&result, Ok(output) if output.status.success()) {
        self_replace::self_delete_at(&backup)?;
    } else {
        std::fs::rename(&backup, &exe)?;
    }
    result
}
