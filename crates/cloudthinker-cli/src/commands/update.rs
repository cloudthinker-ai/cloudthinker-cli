//! `cloudthinker update` — self-update from GitHub releases.
//!
//! Deliberate exception to the "no reqwest in commands" rule: all network
//! (GitHub releases API, installer download) lives inside the `axoupdater`
//! crate — the same org as cargo-dist, and the crate cargo-dist itself uses
//! for its updater binary. The command only wires receipt → run → render.
//!
//! An update re-runs the cargo-dist installer for the new release (the same
//! model as `pi update` re-invoking its package manager), so the running
//! binary is replaced by the installer, never by this process.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use axoupdater::{AxoUpdater, UpdateRequest, Version};
use serde::Serialize;

use crate::engine::exit::ExitCode;
use crate::engine::output;

/// The cargo-dist app name. The install receipt filename, the release assets,
/// and the GitHub repo all key off it — keep in lockstep with
/// `cli/dist-workspace.toml` and the released assets.
const APP_NAME: &str = "cloudthinker-cli";

/// Opt out of the start-up release check entirely.
const UPDATE_CHECK_OPT_OUT: &str = "CLOUDTHINKER_NO_UPDATE_CHECK";

/// The version the start-up offer prints as "you have" and compares against.
const RUNNING_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long the start-up check may take before the agent starts anyway. The
/// check exists to save a user a stale session, never to delay one.
const UPDATE_CHECK_BUDGET: Duration = Duration::from_secs(2);

/// Printed when the freshly installed binary could not take over this start.
const RESTART_HINT: &str = "restart cloudthinker to pick up the new version";

/// Manual reinstall command, printed when self-update is not possible.
const REINSTALL_HINT: &str = "reinstall with: curl --proto '=https' --tlsv1.2 -LsSf \
    https://github.com/cloudthinker-ai/cloudthinker-cli/releases/latest/download/cloudthinker-cli-installer.sh | sh";

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

/// What one attempt to install the latest release did.
enum Outcome {
    Updated { old: Option<String>, new: String },
    Current,
    Refused(String),
    Failed(String),
}

/// Which release track this installation follows.
#[derive(Clone, Copy)]
enum Channel {
    /// The production origin: GitHub `releases/latest` only.
    Stable,
    /// Any other origin: the newest release, prereleases included.
    Dev,
}

/// The production origin follows the stable track; any other valid origin —
/// the dev cluster, staging, a local stack — follows dev prereleases, because
/// whoever points the CLI at a non-prod backend is testing against it. An
/// origin that fails to parse is not a deliberate dev choice, so it fails
/// closed to stable.
fn channel_of(base_url: &str) -> Channel {
    match (
        cloudthinker_client::origin_of(base_url),
        cloudthinker_client::origin_of(crate::DEFAULT_BASE_URL),
    ) {
        (Ok(origin), Ok(prod)) if origin != prod => Channel::Dev,
        _ => Channel::Stable,
    }
}

/// axoupdater owns the release math; the channel only picks its strategy.
/// Both strategies key on the release source the receipt names, so the origin
/// never touches the network — it only selects.
fn request_for(channel: Channel) -> UpdateRequest {
    match channel {
        Channel::Stable => UpdateRequest::Latest,
        Channel::Dev => UpdateRequest::LatestMaybePrerelease,
    }
}

async fn install_latest(force: bool, channel: Channel) -> Outcome {
    let mut updater = AxoUpdater::new_for(APP_NAME);
    if let Err(err) = updater.load_receipt() {
        // No receipt: the binary was not installed by a cargo-dist installer
        // (copied in place, or installed by a future package manager). Refuse
        // rather than guess where to write a replacement (pi's stance).
        return Outcome::Refused(err.to_string());
    }
    // axoupdater treats an exe/receipt mismatch as "no update needed" — a
    // silent lie for a binary that came from somewhere else. Surface it.
    if !matches!(updater.check_receipt_is_for_this_executable(), Ok(true)) {
        return Outcome::Refused("the running binary does not match the install receipt".into());
    }
    updater.configure_version_specifier(request_for(channel));
    if force {
        updater.always_update(true);
    }
    match updater.run().await {
        Ok(Some(result)) => Outcome::Updated {
            old: result.old_version.as_ref().map(ToString::to_string),
            new: result.new_version.to_string(),
        },
        Ok(None) => Outcome::Current,
        Err(err) => Outcome::Failed(err.to_string()),
    }
}

pub async fn run(force: bool, json: bool, base_url: &str) -> ExitCode {
    warn_on_env_overrides();
    render(&install_latest(force, channel_of(base_url)).await, json)
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
            format!(
                "Updated cloudthinker from {} to {new}",
                old.as_deref().unwrap_or("an unknown version")
            ),
        ),
        Outcome::Current => (
            UpdateEnvelope {
                updated: false,
                old_version: None,
                new_version: None,
            },
            "cloudthinker is already up to date".to_string(),
        ),
        Outcome::Refused(reason) => return refuse(reason),
        Outcome::Failed(err) => {
            output::eprintln_error(&format!("update failed: {err}"));
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
            "CLOUDTHINKER_CLI_INSTALLER_GHE_BASE_URL",
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

/// Offer a newer release before a long interactive session starts.
///
/// Every failure path is silent and non-blocking: no receipt, no TTY, a slow
/// or unreachable release host, or a declined prompt all fall through to the
/// session. A start-up check that can delay or fail a start is worse than no
/// check at all, so the network side runs under `UPDATE_CHECK_BUDGET`.
pub async fn offer_on_start(base_url: &str) {
    if std::env::var_os(UPDATE_CHECK_OPT_OUT).is_some() {
        return;
    }
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return;
    }
    let channel = channel_of(base_url);
    let Ok(Some(version)) = tokio::time::timeout(UPDATE_CHECK_BUDGET, newer_release(channel)).await
    else {
        return;
    };
    if !accepts_install(channel, &version) {
        return;
    }
    warn_on_env_overrides();
    let outcome = install_latest(false, channel).await;
    render(&outcome, false);
    if matches!(outcome, Outcome::Updated { .. }) {
        restart_into_installed_binary();
    }
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

/// The version of a newer release this installation can actually install, if any.
async fn newer_release(channel: Channel) -> Option<String> {
    let mut updater = AxoUpdater::new_for(APP_NAME);
    updater.load_receipt().ok()?;
    if !matches!(updater.check_receipt_is_for_this_executable(), Ok(true)) {
        return None;
    }
    updater.configure_version_specifier(request_for(channel));
    newer_than_running(updater.query_new_version().await.ok().flatten()?)
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

/// Ask once on the terminal. Anything but an explicit yes keeps the session.
fn accepts_install(channel: Channel, version: &str) -> bool {
    output::progress(&offer_line(channel, version));
    eprint!("Install it now? [y/N] ");
    if std::io::stderr().flush().is_err() {
        return false;
    }
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim(), "y" | "Y" | "yes" | "Yes")
}

#[cfg(test)]
mod tests {
    use super::{
        Channel, RUNNING_VERSION, channel_of, newer_than, newer_than_running, offer_line,
        request_for,
    };
    use axoupdater::{UpdateRequest, Version};

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
    fn channels_map_to_axoupdater_strategies() {
        assert!(matches!(
            request_for(Channel::Stable),
            UpdateRequest::Latest
        ));
        assert!(matches!(
            request_for(Channel::Dev),
            UpdateRequest::LatestMaybePrerelease
        ));
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
}
