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

use axoupdater::AxoUpdater;
use serde::Serialize;

use crate::engine::exit::ExitCode;
use crate::engine::output;

/// The cargo-dist app name. The install receipt filename, the release assets,
/// and the GitHub repo all key off it — keep in lockstep with
/// `cli/dist-workspace.toml` and the released assets.
const APP_NAME: &str = "cloudthinker-cli";

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

pub async fn run(force: bool, json: bool) -> ExitCode {
    warn_on_env_overrides();

    let mut updater = AxoUpdater::new_for(APP_NAME);
    if let Err(err) = updater.load_receipt() {
        // No receipt: the binary was not installed by a cargo-dist installer
        // (copied in place, or installed by a future package manager). Refuse
        // rather than guess where to write a replacement (pi's stance).
        return refuse(&err.to_string());
    }
    // axoupdater treats an exe/receipt mismatch as "no update needed" — a
    // silent lie for a binary that came from somewhere else. Surface it.
    if !matches!(updater.check_receipt_is_for_this_executable(), Ok(true)) {
        return refuse("the running binary does not match the install receipt");
    }
    if force {
        updater.always_update(true);
    }

    match updater.run().await {
        Ok(Some(result)) => {
            let old = result.old_version.as_ref().map(ToString::to_string);
            let new = result.new_version.to_string();
            let human = format!(
                "Updated cloudthinker from {} to {new}",
                old.as_deref().unwrap_or("an unknown version")
            );
            render(
                &UpdateEnvelope {
                    updated: true,
                    old_version: old,
                    new_version: Some(new.clone()),
                },
                &human,
                json,
            )
        }
        Ok(None) => render(
            &UpdateEnvelope {
                updated: false,
                old_version: None,
                new_version: None,
            },
            "cloudthinker is already up to date",
            json,
        ),
        Err(err) => {
            output::eprintln_error(&format!("update failed: {err}"));
            ExitCode::JobFailed
        }
    }
}

/// The one place owning the stdout-purity + exit-code mapping for an update
/// outcome: JSON envelope on stdout when `--json`, else the human line; any
/// render failure goes to stderr and maps to `JobFailed`.
fn render(envelope: &UpdateEnvelope, human: &str, json: bool) -> ExitCode {
    let rendered = if json {
        output::emit_json(envelope)
    } else {
        output::print_update_result(human)
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
