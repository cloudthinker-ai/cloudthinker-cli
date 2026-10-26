use std::path::Path;
use std::process::Command;

use cloudthinker_client::auth::worker_store::{WORKER_TOKEN_ENV, WorkerStore};
use cloudthinker_client::worker_api::{ProbeFailure, WorkerClient, probe_origin};
use cloudthinker_client::{CtError, UpdateCache, origin_of, update_cache_path};
use serde::Serialize;
use uuid::Uuid;

use super::worker::DoctorArgs;
use super::worker_service;
use crate::engine::{exit::ExitCode, output};
use crate::worker::sandbox::{self, SandboxContext, Support};
use crate::worker::worktree::WORKTREES_DIR;

const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");
const SKEW_WARN_SECONDS: i64 = 10;
const SKEW_FAIL_SECONDS: i64 = 30;
const MIN_GIT: (u32, u32) = (2, 38);
const STATE_SCAN_LIMIT: usize = 2000;
const EXPIRY_WARN_DAYS: i64 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Pass,
    Warn,
    Fail,
    Skipped,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "ok",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skipped => "skip",
        }
    }
}

#[derive(Debug, Serialize)]
struct Check {
    id: &'static str,
    status: Status,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fix: Option<String>,
}

impl Check {
    fn new(id: &'static str, status: Status, detail: impl Into<String>) -> Self {
        Self {
            id,
            status,
            detail: detail.into(),
            fix: None,
        }
    }

    fn pass(id: &'static str, detail: impl Into<String>) -> Self {
        Self::new(id, Status::Pass, detail)
    }

    fn skipped(id: &'static str, detail: impl Into<String>) -> Self {
        Self::new(id, Status::Skipped, detail)
    }

    fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

#[derive(Debug, Serialize)]
struct Report {
    cli_version: &'static str,
    ok: bool,
    checks: Vec<Check>,
}

pub async fn run(base_url: &str, args: DoctorArgs) -> ExitCode {
    let checks = collect(base_url, &args).await;
    let ok = checks.iter().all(|check| check.status != Status::Fail);
    let report = Report {
        cli_version: CLI_VERSION,
        ok,
        checks,
    };
    let printed = if args.json() {
        output::emit_json(&report)
    } else {
        output::print_lines(&render(&report))
    };
    match printed {
        Err(error) => {
            output::eprintln_error(&error);
            ExitCode::JobFailed
        }
        Ok(()) if ok => ExitCode::Ok,
        Ok(()) => ExitCode::JobFailed,
    }
}

async fn collect(base_url: &str, args: &DoctorArgs) -> Vec<Check> {
    let mut checks = vec![version_check()];
    let (network, skew) = network_checks(base_url).await;
    let reachable = network.status != Status::Fail;
    checks.push(network);
    checks.push(skew);
    let credential = stored_credential(base_url, args.outpost());
    checks.push(credential_check(base_url, &credential, reachable).await);
    checks.push(workdir_check(base_url, args.workdir()));
    checks.push(state_dir_check(base_url));
    checks.push(service_check(base_url, &credential, args.workdir()));
    checks.push(git_check());
    checks.push(sandbox_check(
        sandbox_support(base_url, args.workdir()).await,
    ));
    checks
}

fn version_check() -> Check {
    let latest = update_cache_path()
        .ok()
        .map(|path| UpdateCache::load(&path))
        .and_then(|cache| cache.latest_version);
    let note = "The server publishes no minimum or recommended version yet.";
    match latest {
        Some(latest) if numeric_version(&latest) > numeric_version(CLI_VERSION) => Check::new(
            "cli_version",
            Status::Warn,
            format!("CLI {CLI_VERSION}; {latest} is available."),
        )
        .with_fix("Run `cloudthinker update`, then restart the worker."),
        _ => Check::pass("cli_version", format!("CLI {CLI_VERSION}. {note}")),
    }
}

fn numeric_version(text: &str) -> (u64, u64, u64) {
    let core = text.split(['-', '+']).next().unwrap_or_default();
    let mut parts = core.split('.').map(|part| part.parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

fn env_note() -> String {
    let proxy = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
        .into_iter()
        .find(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    let ca = ["SSL_CERT_FILE", "SSL_CERT_DIR"]
        .into_iter()
        .find(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    format!(
        "proxy {}; custom CA {}",
        proxy.map_or("none".to_owned(), |name| format!("{name} is set")),
        ca.map_or("none".to_owned(), |name| format!("{name} is set"))
    )
}

async fn network_checks(base_url: &str) -> (Check, Check) {
    let skipped_skew = |why: &str| Check::skipped("clock_skew", format!("Not measured: {why}."));
    let origin = match origin_of(base_url) {
        Ok(origin) => origin,
        Err(error) => {
            return (
                Check::new("https", Status::Fail, error.to_string())
                    .with_fix("Pass a full URL such as https://app.cloudthinker.io with --url."),
                skipped_skew("the server URL is invalid"),
            );
        }
    };
    let loopback = origin.starts_with("http://127.0.0.1")
        || origin.starts_with("http://localhost")
        || origin.starts_with("http://[::1]");
    if !origin.starts_with("https://") && !loopback {
        return (
            Check::new("https", Status::Fail, "The server URL does not use HTTPS.")
                .with_fix("Use an https:// URL with --url."),
            skipped_skew("the server URL does not use HTTPS"),
        );
    }
    match probe_origin(base_url).await {
        Ok(probe) => {
            let reached = format!(
                "Reached {origin} in {} ms (HTTP {}; {}).",
                probe.elapsed.as_millis(),
                probe.status,
                env_note()
            );
            let network = if probe.status >= 500 {
                Check::new("https", Status::Warn, reached)
                    .with_fix("Wait a few minutes and run the check again; the server is failing.")
            } else {
                Check::pass("https", reached)
            };
            (network, skew_check(probe.skew))
        }
        Err(failure) => (
            probe_failure_check(failure, &origin),
            skipped_skew("the server is unreachable"),
        ),
    }
}

fn probe_failure_check(failure: ProbeFailure, origin: &str) -> Check {
    let (detail, fix) = match failure {
        ProbeFailure::Dns => (
            "The host name did not resolve.",
            "Check the host name in --url and your DNS or VPN.",
        ),
        ProbeFailure::Timeout => (
            "The server did not answer within 10 seconds.",
            "Check your firewall, or set HTTPS_PROXY if your network needs a proxy.",
        ),
        ProbeFailure::Tls => (
            "The TLS handshake failed.",
            "If a proxy re-signs HTTPS, set SSL_CERT_FILE to your company CA bundle.",
        ),
        ProbeFailure::Connect => (
            "The connection was refused or blocked.",
            "Allow outbound HTTPS to the server, or set HTTPS_PROXY.",
        ),
        ProbeFailure::Other => (
            "The request failed.",
            "Check the network and the --url value.",
        ),
    };
    Check::new("https", Status::Fail, format!("{detail} ({origin})")).with_fix(fix)
}

fn skew_check(skew: Option<chrono::Duration>) -> Check {
    let Some(skew) = skew else {
        return Check::skipped("clock_skew", "The server sent no Date header.");
    };
    let seconds = skew.num_seconds();
    let direction = if seconds >= 0 { "behind" } else { "ahead of" };
    let detail = format!(
        "This machine's clock is {} s {direction} the server.",
        seconds.abs()
    );
    let fix = "Turn on network time sync, for example `sudo timedatectl set-ntp true`.";
    if seconds.abs() > SKEW_FAIL_SECONDS {
        Check::new("clock_skew", Status::Fail, detail).with_fix(fix)
    } else if seconds.abs() > SKEW_WARN_SECONDS {
        Check::new("clock_skew", Status::Warn, detail).with_fix(fix)
    } else {
        Check::pass("clock_skew", detail)
    }
}

enum StoredCredential {
    Found { target_id: Uuid, token: String },
    Missing(String),
}

fn stored_credential(base_url: &str, selector: Option<&str>) -> StoredCredential {
    if let Some(token) = std::env::var(WORKER_TOKEN_ENV)
        .ok()
        .filter(|token| !token.trim().is_empty())
        && let Some(target_id) = selector.and_then(|value| Uuid::parse_str(value).ok())
    {
        return StoredCredential::Found { target_id, token };
    }
    let Ok(root) = WorkerStore::default_state_root(base_url) else {
        return StoredCredential::Missing("The worker state directory is unavailable.".into());
    };
    if !root.is_dir() {
        return StoredCredential::Missing(
            "No outpost is set up on this machine for this server.".into(),
        );
    }
    let store = match WorkerStore::open_default(base_url) {
        Ok(store) => store,
        Err(error) => return StoredCredential::Missing(error.to_string()),
    };
    let loaded = match selector {
        Some(selector) => store.load(selector),
        None => store.only(),
    };
    match loaded {
        Ok(credential) => StoredCredential::Found {
            target_id: credential.target_id,
            token: credential.token,
        },
        Err(error) => StoredCredential::Missing(error.to_string()),
    }
}

async fn credential_check(base_url: &str, credential: &StoredCredential, reachable: bool) -> Check {
    let token = match credential {
        StoredCredential::Found { token, .. } => token,
        StoredCredential::Missing(detail) => {
            return Check::new("credential", Status::Fail, detail.clone()).with_fix(
                "Run `cloudthinker worker outpost create <name>`, or pass --outpost to pick one.",
            );
        }
    };
    if !reachable {
        return Check::skipped("credential", "Not checked: the server is unreachable.");
    }
    let client = match WorkerClient::new(base_url, token, CLI_VERSION) {
        Ok(client) => client,
        Err(error) => return Check::new("credential", Status::Fail, error.to_string()),
    };
    match client.credential_status().await {
        Ok(status) => {
            let expires_at = status.expires_at;
            let days = (expires_at - chrono::Utc::now()).num_days();
            let detail = format!(
                "The server accepts the credential. It expires {}.",
                expires_at.format("%Y-%m-%d")
            );
            if days < EXPIRY_WARN_DAYS {
                Check::new("credential", Status::Warn, detail).with_fix(
                    "Start the worker to renew it, or ask an admin for a new registration reference.",
                )
            } else {
                Check::pass("credential", detail)
            }
        }
        Err(CtError::Auth(_)) => Check::new(
            "credential",
            Status::Fail,
            "The server rejected the credential: it is expired or revoked.",
        )
        .with_fix(
            "Ask a workspace admin for a new registration reference, then run `cloudthinker worker start --register <reference> --workdir <folder>`.",
        ),
        Err(error) => Check::new("credential", Status::Fail, error.to_string())
            .with_fix("Run the check again when the network is stable."),
    }
}

fn workdir_check(base_url: &str, workdir: &Path) -> Check {
    use crate::worker::config::{SavedIdentity, WorkdirIdentity};
    use rustix::fs::{Access, access};
    let path = match workdir.canonicalize() {
        Ok(path) if path.is_dir() => path,
        _ => {
            return Check::new(
                "workdir",
                Status::Fail,
                format!("{} is not an existing directory.", workdir.display()),
            )
            .with_fix("Pass an existing folder with --workdir.");
        }
    };
    if access(&path, Access::READ_OK | Access::WRITE_OK | Access::EXEC_OK).is_err() {
        return Check::new(
            "workdir",
            Status::Fail,
            "This user cannot read, write, and enter the folder.",
        )
        .with_fix("Run `chmod u+rwx` on the folder, or serve a folder you own.");
    }
    let Ok(state_root) = WorkerStore::default_state_root(base_url) else {
        return Check::pass("workdir", "The folder is readable and writable.");
    };
    match WorkdirIdentity::saved(&path, &state_root) {
        Ok(SavedIdentity::Matches) => Check::pass(
            "workdir",
            "The folder is readable and writable, and matches its saved identity.",
        ),
        Ok(SavedIdentity::Absent) => Check::pass(
            "workdir",
            "The folder is readable and writable. The worker saves its identity at first start.",
        ),
        Ok(SavedIdentity::Replaced) => Check::new(
            "workdir",
            Status::Fail,
            "The folder was replaced since a worker last served it.",
        )
        .with_fix(
            "Run `cloudthinker worker start` once with --reset-folder to serve the new folder.",
        ),
        Err(error) => Check::new("workdir", Status::Fail, error.to_string()),
    }
}

fn state_dir_check(base_url: &str) -> Check {
    let Ok(root) = WorkerStore::default_state_root(base_url) else {
        return Check::skipped("state_dir", "The worker state directory is unavailable.");
    };
    if !root.is_dir() {
        return Check::skipped(
            "state_dir",
            "No worker state yet; the worker creates it with safe modes.",
        );
    }
    let mut loose = Vec::new();
    let mut seen = 0;
    scan_state(&root, &root, &mut loose, &mut seen);
    if loose.is_empty() {
        return Check::pass(
            "state_dir",
            "State directories are 0700 and files are 0600.",
        );
    }
    Check::new(
        "state_dir",
        Status::Fail,
        format!(
            "{} state entries let other users in, for example {}.",
            loose.len(),
            loose[0]
        ),
    )
    .with_fix(format!("Run `chmod -R go-rwx {}`.", root.display()))
}

fn scan_state(root: &Path, dir: &Path, loose: &mut Vec<String>, seen: &mut usize) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let checkouts = dir.file_name() == Some(std::ffi::OsStr::new(WORKTREES_DIR));
    for entry in entries.flatten() {
        *seen += 1;
        if *seen > STATE_SCAN_LIMIT {
            return;
        }
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || (checkouts && metadata.is_dir()) {
            continue;
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            loose.push(
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            );
        }
        if metadata.is_dir() {
            scan_state(root, &path, loose, seen);
        }
    }
}

fn service_check(base_url: &str, credential: &StoredCredential, workdir: &Path) -> Check {
    let StoredCredential::Found { target_id, .. } = credential else {
        return Check::skipped("service", "Not checked: no outpost credential.");
    };
    let health = match worker_service::health(base_url, *target_id, workdir) {
        Ok(health) => health,
        Err(error) => {
            return Check::skipped("service", format!("Not checked: {error}."));
        }
    };
    match health.state {
        "absent" => Check::skipped(
            "service",
            "No service is installed. The worker stops when its terminal closes.",
        )
        .with_fix("Run `cloudthinker worker service install` to keep it running."),
        "active" => match health.linger {
            Some(false) => Check::new(
                "service",
                Status::Warn,
                format!(
                    "The {} service is active, but it stops at logout and reboot.",
                    health.manager
                ),
            )
            .with_fix("Run `sudo loginctl enable-linger $USER`."),
            _ => Check::pass(
                "service",
                format!("The {} service is active.", health.manager),
            ),
        },
        state => Check::new(
            "service",
            Status::Warn,
            format!("The {} service is {state}.", health.manager),
        )
        .with_fix("Run `cloudthinker worker service start`, then `worker service status`."),
    }
}

fn git_check() -> Check {
    let fix = "Install git 2.38 or newer. Worktree mode needs it.";
    let Ok(output) = Command::new("git").arg("--version").output() else {
        return Check::new("git", Status::Warn, "git is not installed.").with_fix(fix);
    };
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let version = text
        .split_whitespace()
        .nth(2)
        .map(|version| {
            let mut parts = version
                .split('.')
                .map(|part| part.parse::<u32>().unwrap_or(0));
            (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
        })
        .unwrap_or((0, 0));
    if version >= MIN_GIT {
        Check::pass("git", text)
    } else {
        Check::new("git", Status::Warn, format!("{text} is too old.")).with_fix(fix)
    }
}

async fn sandbox_support(base_url: &str, workdir: &Path) -> Support {
    match (
        WorkerStore::default_state_root(base_url),
        workdir.canonicalize(),
    ) {
        (Ok(state_root), Ok(served)) => SandboxContext::detect(&state_root, &served).await.support,
        _ => sandbox::probe().await,
    }
}

fn sandbox_check(support: Support) -> Check {
    match support {
        Support::Ready => Check::pass(
            "sandbox",
            "Sandbox (Loupe): Landlock and the UNIX socket filter are enforced, so Loupe can run merge-request code here.",
        ),
        Support::Unavailable(reason) => Check::new(
            "sandbox",
            Status::Warn,
            format!(
                "Sandbox (Loupe): unavailable, {}.",
                reason.trim_end_matches('.')
            ),
        )
        .with_fix(
            "Serve a project folder, not your home folder, from Linux 6.2 or newer with Landlock enabled (`landlock` in /sys/kernel/security/lsm). macOS and Windows cannot run Loupe.",
        ),
    }
}

fn render(report: &Report) -> Vec<String> {
    let mut lines = Vec::new();
    for check in &report.checks {
        lines.push(format!(
            "{:<5} {:<18} {}",
            check.status.label(),
            check.id,
            check.detail
        ));
        if let Some(fix) = &check.fix {
            lines.push(format!("{:<5} {:<18} fix: {fix}", "", ""));
        }
    }
    lines.push(if report.ok {
        "All required checks passed.".to_owned()
    } else {
        "Some checks failed. Apply each fix, then run `cloudthinker worker doctor` again."
            .to_owned()
    });
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_skew_warns_over_ten_seconds_and_fails_over_thirty() {
        let check = |seconds| skew_check(Some(chrono::Duration::seconds(seconds))).status;
        assert_eq!(check(2), Status::Pass);
        assert_eq!(check(-10), Status::Pass);
        assert_eq!(check(11), Status::Warn);
        assert_eq!(check(-30), Status::Warn);
        assert_eq!(check(45), Status::Fail);
        assert!(
            skew_check(Some(chrono::Duration::seconds(45)))
                .fix
                .is_some()
        );
        assert_eq!(skew_check(None).status, Status::Skipped);
    }

    #[test]
    fn the_state_scan_skips_worktree_checkouts_but_not_their_records() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let worktrees = root.join("outpost").join(WORKTREES_DIR);
        let checkout = worktrees.join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        for dir in [root.join("outpost"), worktrees.clone()] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::set_permissions(&checkout, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(checkout.join("readme.md"), "base").unwrap();
        std::fs::set_permissions(
            checkout.join("readme.md"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let record = worktrees.join("record.json");
        std::fs::write(&record, "{}").unwrap();
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o644)).unwrap();
        let (mut loose, mut seen) = (Vec::new(), 0);
        scan_state(root, root, &mut loose, &mut seen);
        assert_eq!(loose, vec!["outpost/worktrees/record.json".to_owned()]);
    }

    #[test]
    fn report_lists_every_check_with_a_fix_for_each_problem() {
        let report = Report {
            cli_version: CLI_VERSION,
            ok: false,
            checks: vec![
                Check::pass("git", "git version 2.43.0"),
                Check::new("clock_skew", Status::Fail, "45 s").with_fix("Sync the clock."),
            ],
        };
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["checks"][0]["status"], "pass");
        assert!(value["checks"][0].get("fix").is_none());
        assert_eq!(value["checks"][1]["fix"], "Sync the clock.");
        let text = render(&report).join("\n");
        assert!(text.contains("fix: Sync the clock."));
        assert!(text.contains("Some checks failed"));
    }
}
