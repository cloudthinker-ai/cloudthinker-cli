use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use axoupdater::Version;
use cloudthinker_client::UpdateCache;
use cloudthinker_client::cli_release::{
    CliUpdateError, CliUpdater, GHE_BASE_ENV, GITHUB_BASE_ENV, UpdateTarget,
};
use serde::Serialize;

use crate::engine::exit::ExitCode;
use crate::engine::output;

/// Opt out of the start-up release check entirely.
const UPDATE_CHECK_OPT_OUT: &str = "CLOUDTHINKER_NO_UPDATE_CHECK";

/// The version the start-up offer prints as "you have" and compares against.
const RUNNING_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long the start-up check may take before the agent starts anyway. The
/// check exists to save a user a stale session, never to delay one.
const UPDATE_CHECK_BUDGET: Duration = Duration::from_secs(2);

const CHECK_INTERVAL_SECS: u64 = 20 * 60 * 60;
const RETRY_INTERVAL_SECS: u64 = 60 * 60;

/// Printed when the freshly installed binary could not take over this start.
const RESTART_HINT: &str = "restart cloudthinker to pick up the new version";

/// Manual reinstall command, printed when self-update is not possible.
const REINSTALL_HINT: &str = "reinstall with: curl -fsSL https://cloudthinker.io/install.sh | sh";

/// Where a user picks a version for `update --version`.
const RELEASES_URL: &str = "https://github.com/cloudthinker-ai/cloudthinker-cli/releases";

/// The `--json` envelope for `update`.
#[derive(Debug, Serialize)]
pub struct UpdateEnvelope {
    /// Whether a new release was actually installed.
    pub updated: bool,
    /// Version before the update; null when nothing was installed.
    pub old_version: Option<String>,
    /// Version after the update; null when nothing was installed.
    pub new_version: Option<String>,
}

/// Refuse to update: reason + the manual reinstall command, both on stderr.
fn refuse(reason: &str) -> ExitCode {
    output::eprintln_error(&format!("cannot self-update this installation: {reason}"));
    output::eprintln_error(REINSTALL_HINT);
    ExitCode::JobFailed
}

/// What one attempt to install a release did.
enum Outcome {
    Updated { old: Option<String>, new: String },
    Current,
    AlreadyAt(String),
    NoSuchRelease(String),
    Refused(String),
    RateLimited,
    Failed(String),
}

/// Which release track this installation follows.
#[derive(Clone, Copy)]
pub(crate) enum Channel {
    /// The production origin: GitHub `releases/latest` only.
    Stable,
    /// Any other origin: the newest release, prereleases included.
    Dev,
}

impl Channel {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Dev => "dev",
        }
    }
}

/// The production origin follows the stable track; any other valid origin —
/// the dev cluster, staging, a local stack — follows dev prereleases, because
/// whoever points the CLI at a non-prod backend is testing against it. An
/// origin that fails to parse is not a deliberate dev choice, so it fails
/// closed to stable.
pub(crate) fn channel_of(base_url: &str) -> Channel {
    match (
        cloudthinker_client::origin_of(base_url),
        cloudthinker_client::origin_of(crate::DEFAULT_BASE_URL),
    ) {
        (Ok(origin), Ok(prod)) if origin != prod => Channel::Dev,
        _ => Channel::Stable,
    }
}

/// Accepts `0.12.2` or `v0.12.2` and keeps the bare semver the release tags use.
pub fn parse_version(raw: &str) -> Result<String, String> {
    let bare = raw.trim().trim_start_matches('v');
    Version::parse(bare)
        .map(|version| version.to_string())
        .map_err(|_| format!("`{raw}` is not a release version; use one like 0.12.2"))
}

fn request_for(channel: Channel) -> UpdateTarget<'static> {
    match channel {
        Channel::Stable => UpdateTarget::Stable,
        Channel::Dev => UpdateTarget::Dev,
    }
}

async fn install(force: bool, channel: Channel, pinned: Option<&str>) -> Outcome {
    let target = match pinned {
        Some(version) => UpdateTarget::Version(version),
        None => request_for(channel),
    };
    let result = match CliUpdater::load() {
        Ok(updater) => updater.install(target, force).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(Some(result)) => Outcome::Updated {
            old: result.old_version,
            new: result.new_version,
        },
        Ok(None) => match pinned {
            Some(version) => Outcome::AlreadyAt(version.to_owned()),
            None => Outcome::Current,
        },
        Err(CliUpdateError::NoSuchRelease(version)) => Outcome::NoSuchRelease(version),
        Err(CliUpdateError::RateLimited) => Outcome::RateLimited,
        Err(error @ (CliUpdateError::NoReceipt(_) | CliUpdateError::ReceiptMismatch)) => {
            Outcome::Refused(error.to_string())
        }
        Err(error) => Outcome::Failed(error.to_string()),
    }
}

pub async fn run(force: bool, pinned: Option<&str>, json: bool, base_url: &str) -> ExitCode {
    warn_on_env_overrides();
    let label = pinned.map_or_else(
        || "Checking for updates".to_string(),
        |version| format!("Finding cloudthinker {version}"),
    );
    let step = (!json).then(|| output::step(&label));
    let outcome = install(force, channel_of(base_url), pinned).await;
    drop(step);
    if let Outcome::Updated { new, .. } = &outcome
        && crate::commands::agent::bundle_in_use()
    {
        let step = (!json).then(|| output::step(&updating_line(new)));
        let prefetched = crate::commands::agent::prefetch_bundle(new).await;
        drop(step);
        if let Err(error) = prefetched {
            output::warn(&format!(
                "the next `cloudthinker agent` start finishes the update: {error}"
            ));
        }
    }
    render(&outcome, json)
}

/// The one place owning the stdout-purity + exit-code mapping for an update
/// outcome: JSON envelope on stdout when `--json`, else the human line; any
/// render failure goes to stderr and maps to `JobFailed`.
fn render(outcome: &Outcome, json: bool) -> ExitCode {
    let (envelope, human) = match outcome {
        Outcome::Updated { old, new } => (
            UpdateEnvelope {
                updated: true,
                old_version: old.clone(),
                new_version: Some(new.clone()),
            },
            updated_line(old.as_deref().unwrap_or("an unknown version"), new),
        ),
        Outcome::Current => (
            UpdateEnvelope {
                updated: false,
                old_version: None,
                new_version: None,
            },
            "cloudthinker is already up to date".to_string(),
        ),
        Outcome::AlreadyAt(version) => (
            UpdateEnvelope {
                updated: false,
                old_version: None,
                new_version: None,
            },
            format!("cloudthinker is already {version}"),
        ),
        Outcome::Refused(reason) => return refuse(reason),
        Outcome::NoSuchRelease(version) => {
            output::eprintln_error(&format!("cloudthinker {version} was never released"));
            output::eprintln_error(&format!("pick a version from {RELEASES_URL}"));
            return ExitCode::Usage;
        }
        Outcome::RateLimited => {
            output::eprintln_error(
                "GitHub is limiting update checks from your network. Run: curl -fsSL https://cloudthinker.io/install.sh | sh",
            );
            return ExitCode::JobFailed;
        }
        Outcome::Failed(err) => {
            output::eprintln_error(&format!("update failed: {err}"));
            output::eprintln_error(&format!(
                "run `cloudthinker update` again, or {REINSTALL_HINT}"
            ));
            return ExitCode::JobFailed;
        }
    };
    let rendered = if json {
        output::emit_json(&envelope)
    } else {
        output::print_update_result(&human)
    };
    match rendered {
        Ok(()) => ExitCode::Ok,
        Err(err) => {
            output::eprintln_error(&err);
            ExitCode::JobFailed
        }
    }
}

/// Runtime env hooks that can repoint this command. axoupdater honors them
/// by design (cargo-dist's updater contract, e.g. GitHub Enterprise bases),
/// so an environment that exports them can redirect an update — including to
/// a hostile installer, which this command then executes. Warn instead of
/// redirecting silently. (Note: axoupdater 0.10 verifies no installer
/// checksum; TLS to the release host is the trust boundary.)
fn warn_on_env_overrides() {
    for (name, effect) in [
        (
            "AXOUPDATER_CONFIG_PATH",
            "the install receipt is read from a non-default location",
        ),
        (
            GHE_BASE_ENV,
            "releases and installers are fetched from a non-default GitHub base",
        ),
        (
            GITHUB_BASE_ENV,
            "releases and installers are fetched from a non-default GitHub base",
        ),
    ] {
        if std::env::var_os(name).is_some() {
            output::eprintln_error(&format!(
                "update: environment variable {name} is set — {effect}"
            ));
        }
    }
}

pub async fn offer_on_start(base_url: &str) -> PendingCheck {
    if std::env::var_os(UPDATE_CHECK_OPT_OUT).is_some() {
        return PendingCheck(None);
    }
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return PendingCheck(None);
    }
    let Ok(cache_path) = cloudthinker_client::update_cache_path() else {
        return PendingCheck(None);
    };
    let channel = channel_of(base_url);
    let cache = UpdateCache::load(&cache_path);
    let pending = if cache.is_fresh(
        channel.name(),
        now_unix(),
        CHECK_INTERVAL_SECS,
        RETRY_INTERVAL_SECS,
    ) {
        PendingCheck(None)
    } else {
        PendingCheck(Some((
            tokio::spawn(refresh_cache(cache_path.clone(), channel)),
            Instant::now(),
        )))
    };
    let Some(version) = offerable(&cache, channel) else {
        return pending;
    };
    match ask(channel, &version) {
        Answer::Install => {}
        Answer::NotNow => return pending,
        Answer::Skip => {
            pending.settle().await;
            let mut cache = UpdateCache::load(&cache_path);
            cache.dismissed_version = Some(version);
            let _ = cache.save(&cache_path);
            return PendingCheck(None);
        }
    }
    warn_on_env_overrides();
    let step = output::step(&updating_line(&version));
    let outcome = install(false, channel, None).await;
    let Outcome::Updated { new, .. } = &outcome else {
        drop(step);
        render(&outcome, false);
        return pending;
    };
    let _ = crate::commands::agent::prefetch_bundle(new).await;
    drop(step);
    output::done(&updated_line(RUNNING_VERSION, new));
    restart_into_installed_binary();
    pending
}

pub struct UpdateNotice {
    cache_path: PathBuf,
    channel: Channel,
    pending: PendingCheck,
}

pub fn start_notice(base_url: &str) -> Option<UpdateNotice> {
    if std::env::var_os(UPDATE_CHECK_OPT_OUT).is_some() || !std::io::stderr().is_terminal() {
        return None;
    }
    let cache_path = cloudthinker_client::update_cache_path().ok()?;
    let channel = channel_of(base_url);
    let cache = UpdateCache::load(&cache_path);
    let pending = if cache.is_fresh(
        channel.name(),
        now_unix(),
        CHECK_INTERVAL_SECS,
        RETRY_INTERVAL_SECS,
    ) {
        PendingCheck(None)
    } else {
        PendingCheck(Some((
            tokio::spawn(refresh_cache(cache_path.clone(), channel)),
            Instant::now(),
        )))
    };
    Some(UpdateNotice {
        cache_path,
        channel,
        pending,
    })
}

impl UpdateNotice {
    pub async fn finish(self) {
        self.pending.settle().await;
        if let Some(version) = offerable(&UpdateCache::load(&self.cache_path), self.channel) {
            output::progress(&notice_line(self.channel, &version));
        }
    }
}

fn notice_line(channel: Channel, version: &str) -> String {
    format!(
        "{}. Run `cloudthinker update` to install it.",
        offer_line(channel, version)
    )
}

pub struct PendingCheck(Option<(tokio::task::JoinHandle<()>, Instant)>);

impl PendingCheck {
    pub async fn settle(self) {
        let Some((mut task, started)) = self.0 else {
            return;
        };
        let remaining = UPDATE_CHECK_BUDGET.saturating_sub(started.elapsed());
        if tokio::time::timeout(remaining, &mut task).await.is_err() {
            task.abort();
        }
    }
}

async fn refresh_cache(cache_path: PathBuf, channel: Channel) {
    let latest = latest_release(channel).await;
    let mut cache = UpdateCache::load(&cache_path);
    cache.record_check(channel.name(), latest, now_unix());
    let _ = cache.save(&cache_path);
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn offerable(cache: &UpdateCache, channel: Channel) -> Option<String> {
    let latest = Version::parse(cache.latest_for(channel.name())?).ok()?;
    newer_than_running(&latest).filter(|version| !cache.is_dismissed(version))
}

pub fn updating_line(version: &str) -> String {
    format!("Updating cloudthinker to {version}")
}

fn updated_line(old: &str, new: &str) -> String {
    format!("Updated cloudthinker from {old} to {new}")
}

/// Continue this start in the binary the installer renamed into place: the old
/// process would run the old agent bundle, and on macOS its first Keychain
/// read fails once its on-disk code changed (`errSecAuthFailed`).
#[cfg(unix)]
fn restart_into_installed_binary() {
    use std::os::unix::process::CommandExt;

    let Some(binary) = installed_binary_path() else {
        output::progress(RESTART_HINT);
        return;
    };
    let error = std::process::Command::new(&binary)
        .args(std::env::args_os().skip(1))
        .env(UPDATE_CHECK_OPT_OUT, "1")
        .exec();
    output::eprintln_error(&format!("could not restart {}: {error}", binary.display()));
    output::progress(RESTART_HINT);
}

#[cfg(not(unix))]
fn restart_into_installed_binary() {
    output::progress(RESTART_HINT);
}

/// This executable's directory plus the released name; on Linux `current_exe`
/// alone names the unlinked inode as `<path> (deleted)`.
fn installed_binary_path() -> Option<PathBuf> {
    Some(
        std::env::current_exe()
            .ok()?
            .parent()?
            .join(env!("CARGO_BIN_NAME")),
    )
}

pub(crate) enum ReleaseLookup {
    NoReceipt,
    Unreachable,
    Latest(Option<String>),
}

pub(crate) async fn lookup_latest(channel: Channel) -> ReleaseLookup {
    let updater = match CliUpdater::load() {
        Ok(updater) => updater,
        Err(CliUpdateError::NoReceipt(_) | CliUpdateError::ReceiptMismatch) => {
            return ReleaseLookup::NoReceipt;
        }
        Err(_) => return ReleaseLookup::Unreachable,
    };
    match updater.latest(request_for(channel)).await {
        Ok(latest) => ReleaseLookup::Latest(latest),
        Err(_) => ReleaseLookup::Unreachable,
    }
}

pub(crate) fn is_newer(latest: &str) -> bool {
    Version::parse(latest)
        .ok()
        .and_then(|latest| newer_than_running(&latest))
        .is_some()
}

async fn latest_release(channel: Channel) -> Option<String> {
    match lookup_latest(channel).await {
        ReleaseLookup::Latest(latest) => latest,
        ReleaseLookup::NoReceipt | ReleaseLookup::Unreachable => None,
    }
}

/// The release worth offering, given the latest one the source carries.
fn newer_than_running(latest: &Version) -> Option<String> {
    newer_than(latest, &Version::parse(RUNNING_VERSION).ok()?)
}

/// Whether `latest` is worth offering to a binary at `running`.
fn newer_than(latest: &Version, running: &Version) -> Option<String> {
    (latest > running).then(|| latest.to_string())
}

/// The one offer line; the dev channel is named so a tester always knows
/// which track pulled the build in.
fn offer_line(channel: Channel, version: &str) -> String {
    let track = match channel {
        Channel::Stable => "",
        Channel::Dev => " on the dev channel",
    };
    format!("cloudthinker {version} is available{track} (you have {RUNNING_VERSION})")
}

enum Answer {
    Install,
    NotNow,
    Skip,
}

fn answer_of(line: &str) -> Answer {
    match line.trim() {
        "y" | "Y" | "yes" | "Yes" => Answer::Install,
        "s" | "S" | "skip" | "Skip" => Answer::Skip,
        _ => Answer::NotNow,
    }
}

fn ask(channel: Channel, version: &str) -> Answer {
    output::progress(&offer_line(channel, version));
    eprint!("Install it now? [y/N] (s skips this version) ");
    if std::io::stderr().flush().is_err() {
        return Answer::NotNow;
    }
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return Answer::NotNow;
    }
    answer_of(&line)
}

#[cfg(test)]
mod tests {
    use super::{
        Answer, Channel, PendingCheck, RUNNING_VERSION, UPDATE_CHECK_BUDGET, answer_of, channel_of,
        newer_than, newer_than_running, offer_line, offerable,
    };
    use axoupdater::Version;
    use cloudthinker_client::UpdateCache;

    #[test]
    fn only_yes_installs_and_only_s_skips() {
        assert!(matches!(answer_of("y\n"), Answer::Install));
        assert!(matches!(answer_of("Yes"), Answer::Install));
        assert!(matches!(answer_of("s\n"), Answer::Skip));
        assert!(matches!(answer_of("skip"), Answer::Skip));
        assert!(matches!(answer_of("\n"), Answer::NotNow));
        assert!(matches!(answer_of("n"), Answer::NotNow));
        assert!(matches!(answer_of("sure"), Answer::NotNow));
    }

    fn cache_with(latest: &str, dismissed: Option<&str>) -> UpdateCache {
        UpdateCache {
            channel: Some("stable".to_string()),
            latest_version: Some(latest.to_string()),
            checked_at_unix: 0,
            failed_at_unix: 0,
            dismissed_version: dismissed.map(ToString::to_string),
        }
    }

    #[test]
    fn a_cached_newer_release_is_offered_unless_skipped() {
        let newer = "999.0.0";
        assert_eq!(
            offerable(&cache_with(newer, None), Channel::Stable),
            Some(newer.to_string())
        );
        assert_eq!(
            offerable(&cache_with(newer, Some(newer)), Channel::Stable),
            None
        );
        assert_eq!(
            offerable(&cache_with(newer, Some("998.0.0")), Channel::Stable),
            Some(newer.to_string())
        );
    }

    #[test]
    fn a_cached_release_is_not_offered_across_channels_or_when_not_newer() {
        assert_eq!(offerable(&cache_with("999.0.0", None), Channel::Dev), None);
        assert_eq!(
            offerable(&cache_with(RUNNING_VERSION, None), Channel::Stable),
            None
        );
        assert_eq!(
            offerable(&cache_with("not a version", None), Channel::Stable),
            None
        );
    }

    #[test]
    fn the_prod_origin_follows_stable() {
        assert!(matches!(
            channel_of("https://app.cloudthinker.io"),
            Channel::Stable
        ));
    }

    #[test]
    fn any_other_origin_follows_dev() {
        assert!(matches!(
            channel_of("https://dev.cloudthinker.io"),
            Channel::Dev
        ));
        assert!(matches!(channel_of("http://localhost:8080"), Channel::Dev));
        assert!(matches!(
            channel_of("https://staging.cloudthinker.io"),
            Channel::Dev
        ));
    }

    #[test]
    fn an_unparseable_origin_fails_closed_to_stable() {
        assert!(matches!(channel_of("not a url"), Channel::Stable));
        assert!(matches!(channel_of(""), Channel::Stable));
    }

    #[test]
    fn the_offer_names_the_dev_channel() {
        assert_eq!(
            offer_line(Channel::Stable, "0.6.0"),
            format!("cloudthinker 0.6.0 is available (you have {RUNNING_VERSION})")
        );
        assert_eq!(
            offer_line(Channel::Dev, "0.6.0-dev.1"),
            format!(
                "cloudthinker 0.6.0-dev.1 is available on the dev channel (you have {RUNNING_VERSION})"
            )
        );
    }

    #[test]
    fn a_prerelease_beats_an_older_stable_but_yields_to_its_own_stable() {
        let running = Version::parse("0.5.7").unwrap();
        assert_eq!(
            newer_than(&Version::parse("0.6.0-dev.1").unwrap(), &running),
            Some("0.6.0-dev.1".to_string())
        );
        let dev = Version::parse("0.6.0-dev.1").unwrap();
        assert_eq!(
            newer_than(&Version::parse("0.6.0").unwrap(), &dev),
            Some("0.6.0".to_string())
        );
        assert_eq!(newer_than(&dev, &Version::parse("0.6.0").unwrap()), None);
    }

    #[test]
    fn offers_a_strictly_newer_release() {
        assert_eq!(
            newer_than(&Version::new(0, 5, 1), &Version::new(0, 5, 0)),
            Some("0.5.1".to_string())
        );
    }

    #[test]
    fn offers_nothing_when_the_release_is_the_running_version() {
        assert_eq!(
            newer_than(&Version::new(0, 5, 0), &Version::new(0, 5, 0)),
            None
        );
    }

    #[test]
    fn offers_nothing_when_the_release_is_older() {
        assert_eq!(
            newer_than(&Version::new(0, 4, 0), &Version::new(0, 5, 0)),
            None
        );
    }

    #[test]
    fn compares_against_the_running_binary_version() {
        let running = Version::parse(RUNNING_VERSION).unwrap();
        assert_eq!(newer_than_running(&running), None);
    }

    #[tokio::test]
    async fn a_check_past_its_budget_is_stopped_before_it_can_write() {
        let wrote = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = wrote.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let started = std::time::Instant::now()
            .checked_sub(UPDATE_CHECK_BUDGET)
            .unwrap();
        PendingCheck(Some((task, started))).settle().await;
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert!(!wrote.load(std::sync::atomic::Ordering::SeqCst));
    }
}
