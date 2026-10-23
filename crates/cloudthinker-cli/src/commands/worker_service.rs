use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use clap::{Args, Subcommand};
use cloudthinker_client::auth::worker_store::{WorkerStore, private_directory};
use cloudthinker_client::{CtError, CtResult, origin_of};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[path = "worker_service_descriptor.rs"]
mod worker_service_descriptor;
#[path = "worker_service_identity.rs"]
mod worker_service_identity;

use worker_service_descriptor::{
    descriptor_path, path_text, render_descriptor, render_previous_descriptor, require_descriptor,
    sync_directory, validate_descriptor_metadata, validate_label, validate_service_text,
    validate_unix_user, write_descriptor,
};
use worker_service_identity::{SystemUser, lookup_user, system_target_id};

#[cfg(test)]
use worker_service_identity::{parse_dscl, parse_passwd};

const SERVICE_NAMESPACE: &str = "io.cloudthinker.worker";

#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    /// Register the worker with the service manager, without starting it.
    Install(ServiceInstallArgs),
    /// Stop the service and remove its descriptor, keeping the credential.
    Uninstall(ServiceTargetArgs),
    /// Report whether the service is absent, inactive, loaded, or active.
    Status(ServiceTargetArgs),
    /// Start the registered service now.
    Start(ServiceTargetArgs),
    /// Stop the service; the worker drains its assignments before it exits.
    Stop(ServiceTargetArgs),
}

#[derive(Debug, Args)]
pub struct ServiceInstallArgs {
    /// Outpost UUID or exact name; defaults to the only outpost set up on this machine.
    #[arg(long, env = "CLOUDTHINKER_OUTPOST_ID")]
    pub outpost: Option<String>,

    /// The one folder the service's worker serves.
    #[arg(long, default_value = ".")]
    pub workdir: PathBuf,

    /// How many assignments the service's worker serves at once.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u16).range(1..=32))]
    pub concurrency: u16,

    /// Short display name for the served folder.
    #[arg(long)]
    pub label: Option<String>,

    /// On Linux, turn on lingering for your account so the service runs after logout and at boot.
    #[arg(long, conflicts_with = "system")]
    pub enable_linger: bool,

    #[command(flatten)]
    pub scope: ServiceScopeArgs,

    /// Print JSON on stdout instead of text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ServiceScopeArgs {
    /// Manage a machine-wide service (systemd system unit or macOS LaunchDaemon); run as root with --user.
    #[arg(long, requires = "user")]
    pub system: bool,

    /// With --system, the account that runs the worker and keeps its credential.
    #[arg(long, requires = "system")]
    pub user: Option<String>,
}

#[derive(Debug, Args)]
pub struct ServiceTargetArgs {
    /// Outpost UUID or exact name; defaults to the only outpost set up on this machine.
    #[arg(long, env = "CLOUDTHINKER_OUTPOST_ID")]
    pub outpost: Option<String>,

    /// The served folder that identifies the service.
    #[arg(long, default_value = ".")]
    pub workdir: PathBuf,

    #[command(flatten)]
    pub scope: ServiceScopeArgs,

    /// Print JSON on stdout instead of text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServicePlatform {
    Systemd,
    Launchd,
}

impl ServicePlatform {
    fn current() -> CtResult<Self> {
        match env::consts::OS {
            "linux" => Ok(Self::Systemd),
            "macos" => Ok(Self::Launchd),
            _ => Err(CtError::Usage(
                "worker services require Linux systemd --user or macOS launchd".into(),
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Systemd => "systemd --user",
            Self::Launchd => "launchd",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ServiceScope {
    User,
    System(SystemUser),
}

impl ServiceScope {
    fn is_system(&self) -> bool {
        matches!(self, Self::System(_))
    }

    fn system_user(&self) -> Option<&SystemUser> {
        match self {
            Self::System(user) => Some(user),
            Self::User => None,
        }
    }
}

struct ServiceTarget {
    platform: ServicePlatform,
    scope: ServiceScope,
    target_id: Uuid,
    workdir: PathBuf,
    service_name: String,
    descriptor_path: PathBuf,
    log_path: PathBuf,
}

struct ServiceSpec {
    target: ServiceTarget,
    descriptor: String,
}

#[derive(Serialize)]
struct ServiceOutput {
    manager: &'static str,
    service: String,
    path: String,
    state: &'static str,
    logs: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    linger: Option<&'static str>,
}

pub fn execute(base_url: &str, workspace: Option<&str>, command: ServiceCommand) -> CtResult<()> {
    match command {
        ServiceCommand::Install(args) => install(base_url, workspace, args),
        ServiceCommand::Uninstall(args) => uninstall(base_url, args),
        ServiceCommand::Status(args) => status(base_url, args),
        ServiceCommand::Start(args) => start(base_url, args),
        ServiceCommand::Stop(args) => stop(base_url, args),
    }
}

fn install(base_url: &str, workspace: Option<&str>, args: ServiceInstallArgs) -> CtResult<()> {
    let origin = origin_of(base_url)?;
    let scope = scope_of(&args.scope)?;
    if args.enable_linger && ServicePlatform::current()? != ServicePlatform::Systemd {
        return Err(CtError::Usage(
            "--enable-linger applies only to a Linux user service".into(),
        ));
    }
    let target = target(&origin, args.outpost.as_deref(), &args.workdir, &scope)?;
    if !scope.is_system() {
        require_stored_credential(&origin, target.target_id)?;
    }
    validate_service_text(&origin, "host")?;
    if let Some(workspace) = workspace {
        validate_service_text(workspace, "workspace")?;
    }
    if let Some(label) = args.label.as_deref() {
        validate_label(label)?;
    }
    let executable = env::current_exe()
        .map_err(|_| CtError::Store("worker executable path unavailable".into()))?
        .canonicalize()
        .map_err(|_| CtError::Store("worker executable path unavailable".into()))?;
    #[cfg(unix)]
    if let Some(user) = scope.system_user() {
        require_trusted_executable(&executable, user.uid)?;
    }
    let executable = path_text(&executable, "worker executable")?;
    let workdir = path_text(&target.workdir, "workdir")?;
    let mut argv = vec![executable, "--url".into(), origin];
    if let Some(workspace) = workspace {
        argv.extend(["--workspace".into(), workspace.into()]);
    }
    argv.extend([
        "worker".into(),
        "start".into(),
        "--outpost".into(),
        target.target_id.to_string(),
        "--workdir".into(),
        workdir,
        "--concurrency".into(),
        args.concurrency.to_string(),
        "--stored-credential".into(),
    ]);
    if let Some(label) = args.label {
        argv.extend(["--label".into(), label]);
    }
    if target.platform == ServicePlatform::Launchd
        && !scope.is_system()
        && let Some(logs) = target.log_path.parent()
    {
        private_directory(logs)?;
    }
    let descriptor = render_descriptor(
        target.platform,
        &target.service_name,
        &argv,
        &target.workdir,
        &target.log_path,
        &target.scope,
    )?;
    let previous = if scope.is_system() {
        descriptor.clone()
    } else {
        render_previous_descriptor(
            target.platform,
            &target.service_name,
            &argv,
            &target.workdir,
        )?
    };
    let spec = ServiceSpec { target, descriptor };
    let state = apply_descriptor(&spec, &previous)?;
    let linger = if scope.is_system() || spec.target.platform != ServicePlatform::Systemd {
        None
    } else {
        Some(ensure_linger(args.enable_linger))
    };
    let mut output = spec.target.output(state);
    output.linger = linger.as_ref().map(|report| report.state);
    let flags = spec.target.scope_flags();
    let mut human = format!(
        "Worker service {state}; start it with cloudthinker worker service start --outpost {} --workdir {}{flags}",
        spec.target.target_id,
        spec.target.workdir.display()
    );
    if let Some(hint) = linger.and_then(|report| report.hint) {
        human.push('\n');
        human.push_str(&hint);
    }
    emit(&output, &human, args.json)
}

#[cfg(unix)]
fn require_trusted_executable(executable: &Path, account_uid: u32) -> CtResult<()> {
    use std::os::unix::fs::MetadataExt;

    for path in executable.ancestors() {
        if path.as_os_str().is_empty() {
            continue;
        }
        let metadata = fs::metadata(path).map_err(|error| {
            CtError::Store(format!("cannot inspect {}: {error}", path.display()))
        })?;
        let writable_by_others = metadata.mode() & 0o022 != 0;
        let trusted_owner = metadata.uid() == 0 || metadata.uid() == account_uid;
        if writable_by_others || !trusted_owner {
            return Err(CtError::Usage(format!(
                "refusing to install a system service: {} is writable by other users or owned by another account; install cloudthinker in a root-owned directory such as /usr/local/bin",
                path.display()
            )));
        }
    }
    Ok(())
}

struct LingerReport {
    state: &'static str,
    hint: Option<String>,
}

fn ensure_linger(enable: bool) -> LingerReport {
    let account = linger_account();
    let enable_command = format!("loginctl enable-linger {account}");
    match query_linger(&account) {
        Some(true) => LingerReport {
            state: "enabled",
            hint: None,
        },
        Some(false) if enable => match run_manager("loginctl", &["enable-linger", &account]) {
            Ok(()) => LingerReport {
                state: "enabled",
                hint: None,
            },
            Err(error) => LingerReport {
                state: "disabled",
                hint: Some(format!(
                    "Lingering could not be turned on ({error}). The service stops at logout. Run: sudo {enable_command}"
                )),
            },
        },
        Some(false) => LingerReport {
            state: "disabled",
            hint: Some(format!(
                "Lingering is off, so the service stops at logout and does not start at boot. Run: {enable_command}   (or install again with --enable-linger)"
            )),
        },
        None => LingerReport {
            state: "unknown",
            hint: Some(format!(
                "Could not read the lingering setting. To keep the service after logout and reboot, run: {enable_command}"
            )),
        },
    }
}

fn linger_account() -> String {
    env::var("USER")
        .ok()
        .filter(|name| validate_unix_user(name).is_ok())
        .unwrap_or_else(linger_uid)
}

fn linger_uid() -> String {
    #[cfg(unix)]
    {
        rustix::process::geteuid().as_raw().to_string()
    }
    #[cfg(not(unix))]
    {
        String::new()
    }
}

fn query_linger(account: &str) -> Option<bool> {
    let output = manager_output("loginctl", &["show-user", account, "--property=Linger"]).ok()?;
    if !output.status.success() {
        return None;
    }
    parse_linger(&String::from_utf8_lossy(&output.stdout))
}

fn parse_linger(text: &str) -> Option<bool> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("Linger="))
        .and_then(|value| match value.trim() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        })
}

fn apply_descriptor(spec: &ServiceSpec, previous: &str) -> CtResult<&'static str> {
    let existing = match fs::symlink_metadata(&spec.target.descriptor_path) {
        Ok(metadata) => {
            validate_descriptor_metadata(&metadata)?;
            Some(fs::read(&spec.target.descriptor_path).map_err(|_| {
                CtError::Store("worker service descriptor could not be read".into())
            })?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => {
            return Err(CtError::Store(
                "worker service descriptor unavailable".into(),
            ));
        }
    };
    let state = match existing.as_deref() {
        None => "installed",
        Some(existing) if existing == spec.descriptor.as_bytes() => "unchanged",
        Some(existing) if existing == previous.as_bytes() => "updated",
        Some(_) => {
            return Err(CtError::Usage(
                "a worker service already exists with different settings; uninstall it first"
                    .into(),
            ));
        }
    };
    if state != "unchanged" {
        write_descriptor(&spec.target.descriptor_path, spec.descriptor.as_bytes())?;
    }
    if state == "updated" {
        manager_reload(&spec.target)?;
    }
    manager_install(&spec.target)?;
    Ok(state)
}

fn manager_reload(target: &ServiceTarget) -> CtResult<()> {
    match target.platform {
        ServicePlatform::Systemd => Ok(()),
        ServicePlatform::Launchd => {
            if !launchd_loaded(target)? {
                return Ok(());
            }
            launchd_bootout(target)?;
            let domain = launchd_domain(target)?;
            let path = target.descriptor_path.display().to_string();
            run_manager("launchctl", &["bootstrap", &domain, &path])
        }
    }
}

fn uninstall(base_url: &str, args: ServiceTargetArgs) -> CtResult<()> {
    let scope = scope_of(&args.scope)?;
    let target = target(base_url, args.outpost.as_deref(), &args.workdir, &scope)?;
    require_descriptor(&target)?;
    manager_uninstall(&target)?;
    fs::remove_file(&target.descriptor_path)
        .map_err(|_| CtError::Store("worker service descriptor could not be removed".into()))?;
    sync_directory(
        target
            .descriptor_path
            .parent()
            .ok_or_else(|| CtError::Store("worker service directory unavailable".into()))?,
    )?;
    emit(
        &target.output("removed"),
        "Worker service removed",
        args.json,
    )
}

fn status(base_url: &str, args: ServiceTargetArgs) -> CtResult<()> {
    let scope = scope_of(&args.scope)?;
    let target = target(base_url, args.outpost.as_deref(), &args.workdir, &scope)?;
    match fs::symlink_metadata(&target.descriptor_path) {
        Ok(metadata) => validate_descriptor_metadata(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return emit(
                &target.output("absent"),
                "Worker service is not installed",
                args.json,
            );
        }
        Err(_) => {
            return Err(CtError::Store(
                "worker service descriptor unavailable".into(),
            ));
        }
    }
    let state = manager_status(&target)?;
    let mut output = target.output(state);
    if state != "active" {
        output.last_exit_code = manager_last_exit(&target);
    }
    let exit = output
        .last_exit_code
        .map(|code| format!(" (last exit code {code})"))
        .unwrap_or_default();
    let human = format!("Worker service is {state}{exit}. Logs: {}", output.logs);
    emit(&output, &human, args.json)
}

#[derive(Debug)]
pub struct ServiceHealth {
    pub manager: &'static str,
    pub state: &'static str,
    pub linger: Option<bool>,
}

pub fn health(base_url: &str, target_id: Uuid, workdir: &Path) -> CtResult<ServiceHealth> {
    let target = target(
        base_url,
        Some(&target_id.to_string()),
        workdir,
        &ServiceScope::User,
    )?;
    let linger = if target.platform == ServicePlatform::Systemd {
        user_linger()
    } else {
        None
    };
    let state = match fs::symlink_metadata(&target.descriptor_path) {
        Ok(metadata) => {
            validate_descriptor_metadata(&metadata)?;
            manager_status(&target)?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "absent",
        Err(_) => {
            return Err(CtError::Store(
                "worker service descriptor unavailable".into(),
            ));
        }
    };
    Ok(ServiceHealth {
        manager: target.platform.name(),
        state,
        linger,
    })
}

fn user_linger() -> Option<bool> {
    let user = env::var("USER").or_else(|_| env::var("LOGNAME")).ok()?;
    let output = manager_output("loginctl", &["show-user", &user, "--property=Linger"]).ok()?;
    if !output.status.success() {
        return None;
    }
    match String::from_utf8_lossy(&output.stdout).trim() {
        "Linger=yes" => Some(true),
        "Linger=no" => Some(false),
        _ => None,
    }
}

fn start(base_url: &str, args: ServiceTargetArgs) -> CtResult<()> {
    let scope = scope_of(&args.scope)?;
    let target = target(base_url, args.outpost.as_deref(), &args.workdir, &scope)?;
    require_descriptor(&target)?;
    if !scope.is_system() {
        require_stored_credential(base_url, target.target_id)?;
    }
    manager_start(&target)?;
    emit(
        &target.output("started"),
        "Worker service started",
        args.json,
    )
}

fn stop(base_url: &str, args: ServiceTargetArgs) -> CtResult<()> {
    let scope = scope_of(&args.scope)?;
    let target = target(base_url, args.outpost.as_deref(), &args.workdir, &scope)?;
    require_descriptor(&target)?;
    manager_stop(&target)?;
    emit(
        &target.output("stopped"),
        "Worker service stopped gracefully",
        args.json,
    )
}

fn require_stored_credential(origin: &str, target_id: Uuid) -> CtResult<()> {
    WorkerStore::open_default(origin)?.load(&target_id.to_string())?;
    Ok(())
}

fn scope_of(args: &ServiceScopeArgs) -> CtResult<ServiceScope> {
    if !args.system {
        return Ok(ServiceScope::User);
    }
    let name = args
        .user
        .as_deref()
        .ok_or_else(|| CtError::Usage("--system needs --user <account>".into()))?;
    require_root()?;
    Ok(ServiceScope::System(lookup_user(name)?))
}

fn require_root() -> CtResult<()> {
    #[cfg(unix)]
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(CtError::Usage(
            "--system must run as root; run it with sudo".into(),
        ));
    }
    Ok(())
}

fn target(
    base_url: &str,
    selector: Option<&str>,
    workdir: &Path,
    scope: &ServiceScope,
) -> CtResult<ServiceTarget> {
    let platform = ServicePlatform::current()?;
    let origin = origin_of(base_url)?;
    let target_id = match scope {
        ServiceScope::System(user) => system_target_id(user, &origin, selector)?,
        ServiceScope::User => match selector {
            Some(selector) => match Uuid::parse_str(selector) {
                Ok(id) => id,
                Err(_) => {
                    WorkerStore::open_default(&origin)?
                        .load(selector)?
                        .target_id
                }
            },
            None => WorkerStore::open_default(&origin)?.only()?.target_id,
        },
    };
    let workdir = workdir
        .canonicalize()
        .map_err(|_| CtError::Usage("--workdir must name an existing directory".into()))?;
    if !workdir.is_dir() {
        return Err(CtError::Usage(
            "--workdir must name an existing directory".into(),
        ));
    }
    let service_id = service_id(&origin, target_id, &workdir)?;
    let service_name = format!("{SERVICE_NAMESPACE}.{service_id}");
    let descriptor_path = descriptor_path(platform, scope, &service_name)?;
    let log_path = match scope {
        ServiceScope::User => WorkerStore::open_default(&origin)?
            .state_root()
            .join("logs")
            .join(format!("{service_name}.log")),
        ServiceScope::System(_) => PathBuf::from(format!("/var/log/{service_name}.log")),
    };
    Ok(ServiceTarget {
        platform,
        scope: scope.clone(),
        target_id,
        workdir,
        service_name,
        descriptor_path,
        log_path,
    })
}

fn service_id(origin: &str, target_id: Uuid, workdir: &Path) -> CtResult<String> {
    let workdir = path_text(workdir, "workdir")?;
    let digest = Sha256::digest(format!("{origin}\n{target_id}\n{workdir}").as_bytes());
    Ok(format!("{digest:x}")[..24].to_string())
}

fn manager_install(target: &ServiceTarget) -> CtResult<()> {
    match target.platform {
        ServicePlatform::Systemd => {
            let unit = target.unit_name();
            run_systemctl(target, &["daemon-reload"])?;
            run_systemctl(target, &["enable", &unit])
        }
        ServicePlatform::Launchd => Ok(()),
    }
}

fn manager_uninstall(target: &ServiceTarget) -> CtResult<()> {
    match target.platform {
        ServicePlatform::Systemd => {
            let unit = target.unit_name();
            run_systemctl(target, &["disable", "--now", &unit])
        }
        ServicePlatform::Launchd => launchd_bootout(target),
    }
}

fn manager_status(target: &ServiceTarget) -> CtResult<&'static str> {
    match target.platform {
        ServicePlatform::Systemd => {
            let unit = target.unit_name();
            let output = systemctl_output(target, &["is-active", &unit])?;
            if output.status.success() {
                Ok("active")
            } else {
                match String::from_utf8_lossy(&output.stdout).trim() {
                    "inactive" | "unknown" => Ok("inactive"),
                    "failed" => Ok("failed"),
                    "activating" => Ok("activating"),
                    "deactivating" => Ok("deactivating"),
                    _ => Err(CtError::Store(
                        "systemd --user could not query the worker service".into(),
                    )),
                }
            }
        }
        ServicePlatform::Launchd => {
            if !launchd_loaded(target)? {
                return Ok("inactive");
            }
            let output = manager_output("launchctl", &["print", &launchd_service(target)?])?;
            let text = String::from_utf8_lossy(&output.stdout);
            if text.lines().any(|line| line.trim() == "state = running") {
                Ok("active")
            } else {
                Ok("loaded")
            }
        }
    }
}

fn manager_last_exit(target: &ServiceTarget) -> Option<i32> {
    match target.platform {
        ServicePlatform::Systemd => {
            let unit = target.unit_name();
            let output = systemctl_output(
                target,
                &[
                    "show",
                    &unit,
                    "--property=ExecMainStatus",
                    "--property=Result",
                ],
            )
            .ok()?;
            let text = String::from_utf8_lossy(&output.stdout);
            let status = text
                .lines()
                .find_map(|line| line.strip_prefix("ExecMainStatus="))?
                .trim()
                .parse::<i32>()
                .ok()?;
            let succeeded = text.lines().any(|line| line.trim() == "Result=success");
            (status != 0 || !succeeded).then_some(status)
        }
        ServicePlatform::Launchd => {
            let output =
                manager_output("launchctl", &["print", &launchd_service(target).ok()?]).ok()?;
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .find_map(|line| {
                    line.trim()
                        .strip_prefix("last exit code = ")
                        .and_then(|code| code.trim().parse::<i32>().ok())
                })
        }
    }
}

fn manager_start(target: &ServiceTarget) -> CtResult<()> {
    match target.platform {
        ServicePlatform::Systemd => {
            let unit = target.unit_name();
            run_systemctl(target, &["start", &unit])
        }
        ServicePlatform::Launchd => {
            if !launchd_loaded(target)? {
                let domain = launchd_domain(target)?;
                let path = target.descriptor_path.display().to_string();
                run_manager("launchctl", &["bootstrap", &domain, &path])?;
            }
            run_manager("launchctl", &["kickstart", &launchd_service(target)?])
        }
    }
}

fn manager_stop(target: &ServiceTarget) -> CtResult<()> {
    match target.platform {
        ServicePlatform::Systemd => {
            let unit = target.unit_name();
            run_systemctl(target, &["stop", &unit])
        }
        ServicePlatform::Launchd => launchd_bootout(target),
    }
}

fn systemctl_args<'a>(target: &ServiceTarget, args: &[&'a str]) -> Vec<&'a str> {
    let mut full = if target.scope.is_system() {
        Vec::new()
    } else {
        vec!["--user"]
    };
    full.extend_from_slice(args);
    full
}

fn run_systemctl(target: &ServiceTarget, args: &[&str]) -> CtResult<()> {
    run_manager("systemctl", &systemctl_args(target, args))
}

fn systemctl_output(target: &ServiceTarget, args: &[&str]) -> CtResult<Output> {
    manager_output("systemctl", &systemctl_args(target, args))
}

fn manager_output(program: &str, args: &[&str]) -> CtResult<Output> {
    manager_command(program)
        .args(args)
        .env("SYSTEMD_PAGER", "cat")
        .env("SYSTEMD_COLORS", "0")
        .output()
        .map_err(|_| {
            CtError::Store(format!(
                "{program} is unavailable; install the native worker service manager"
            ))
        })
}

fn manager_command(program: &str) -> Command {
    #[cfg(test)]
    if let Some(path) = test_manager_path(program) {
        return Command::new(path);
    }
    Command::new(system_program(program))
}

fn system_program(program: &str) -> PathBuf {
    #[cfg(unix)]
    if rustix::process::geteuid().as_raw() == 0 {
        return ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
            .iter()
            .map(|directory| Path::new(directory).join(program))
            .find(|path| path.is_file())
            .unwrap_or_else(|| Path::new("/usr/bin").join(program));
    }
    PathBuf::from(program)
}

#[cfg(test)]
static TEST_SYSTEMD_MANAGER: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

#[cfg(test)]
fn set_test_systemd_manager(path: &Path) {
    let manager = TEST_SYSTEMD_MANAGER.get_or_init(|| Mutex::new(None));
    *manager.lock().unwrap() = Some(path.to_path_buf());
}

#[cfg(test)]
fn clear_test_systemd_manager() {
    if let Some(manager) = TEST_SYSTEMD_MANAGER.get() {
        *manager.lock().unwrap() = None;
    }
}

#[cfg(test)]
static TEST_LAUNCHD_MANAGER: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

#[cfg(test)]
static TEST_LAUNCHD_UID: OnceLock<Mutex<Option<u32>>> = OnceLock::new();

#[cfg(test)]
fn set_test_launchd_manager(path: &Path) {
    let manager = TEST_LAUNCHD_MANAGER.get_or_init(|| Mutex::new(None));
    *manager.lock().unwrap() = Some(path.to_path_buf());
}

#[cfg(test)]
fn clear_test_launchd_manager() {
    if let Some(manager) = TEST_LAUNCHD_MANAGER.get() {
        *manager.lock().unwrap() = None;
    }
}

#[cfg(test)]
fn set_test_launchd_uid(uid: u32) {
    let value = TEST_LAUNCHD_UID.get_or_init(|| Mutex::new(None));
    *value.lock().unwrap() = Some(uid);
}

#[cfg(test)]
fn clear_test_launchd_uid() {
    if let Some(value) = TEST_LAUNCHD_UID.get() {
        *value.lock().unwrap() = None;
    }
}

#[cfg(test)]
fn test_manager_path(program: &str) -> Option<PathBuf> {
    let manager = match program {
        "systemctl" => TEST_SYSTEMD_MANAGER.get(),
        "launchctl" => TEST_LAUNCHD_MANAGER.get(),
        _ => return None,
    }?;
    manager.lock().unwrap().clone()
}

fn run_manager(program: &str, args: &[&str]) -> CtResult<()> {
    let output = manager_output(program, args)?;
    if output.status.success() {
        return Ok(());
    }
    let detail: String = String::from_utf8_lossy(&output.stderr)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|character| !character.is_control())
        .take(300)
        .collect();
    Err(CtError::Store(if detail.is_empty() {
        format!("{program} could not manage the worker service")
    } else {
        format!("{program} could not manage the worker service: {detail}")
    }))
}

fn launchd_domain(target: &ServiceTarget) -> CtResult<String> {
    if target.scope.is_system() {
        return Ok("system".into());
    }
    #[cfg(test)]
    if let Some(uid) = TEST_LAUNCHD_UID
        .get()
        .and_then(|value| value.lock().unwrap().to_owned())
    {
        return Ok(format!("gui/{uid}"));
    }
    launchd_user_domain()
}

#[cfg(target_os = "macos")]
fn launchd_user_domain() -> CtResult<String> {
    Ok(format!("gui/{}", rustix::process::geteuid().as_raw()))
}

#[cfg(not(target_os = "macos"))]
fn launchd_user_domain() -> CtResult<String> {
    Err(CtError::Usage("launchd is available only on macOS".into()))
}

fn launchd_service(target: &ServiceTarget) -> CtResult<String> {
    Ok(format!(
        "{}/{}",
        launchd_domain(target)?,
        target.service_name
    ))
}

fn launchd_bootout(target: &ServiceTarget) -> CtResult<()> {
    if !launchd_loaded(target)? {
        return Ok(());
    }
    run_manager("launchctl", &["bootout", &launchd_service(target)?])
}

fn launchd_loaded(target: &ServiceTarget) -> CtResult<bool> {
    let domain_output = manager_output("launchctl", &["print", &launchd_domain(target)?])?;
    if !domain_output.status.success() {
        return Err(CtError::Store("launchd user domain is unavailable".into()));
    }
    let output = manager_output("launchctl", &["print", &launchd_service(target)?])?;
    if output.status.success() {
        return Ok(true);
    }
    let detail = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if detail.contains("Could not find service") && detail.contains(&target.service_name) {
        Ok(false)
    } else {
        Err(CtError::Store(
            "launchd service status is unavailable".into(),
        ))
    }
}

impl ServiceTarget {
    fn unit_name(&self) -> String {
        format!("{}.service", self.service_name)
    }

    fn manager_name(&self) -> &'static str {
        match (self.scope.is_system(), self.platform) {
            (true, ServicePlatform::Systemd) => "systemd",
            (true, ServicePlatform::Launchd) => "launchd (system)",
            (false, platform) => platform.name(),
        }
    }

    fn scope_flags(&self) -> String {
        match self.scope.system_user() {
            Some(user) => format!(" --system --user {}", user.name),
            None => String::new(),
        }
    }

    fn output(&self, state: &'static str) -> ServiceOutput {
        ServiceOutput {
            manager: self.manager_name(),
            service: self.service_name.clone(),
            path: self.descriptor_path.display().to_string(),
            state,
            logs: self.logs(),
            last_exit_code: None,
            linger: None,
        }
    }

    fn logs(&self) -> String {
        match self.platform {
            ServicePlatform::Systemd if self.scope.is_system() => {
                format!("journalctl -u {}", self.unit_name())
            }
            ServicePlatform::Systemd => format!("journalctl --user -u {}", self.unit_name()),
            ServicePlatform::Launchd => self.log_path.display().to_string(),
        }
    }
}

fn emit(value: &ServiceOutput, human: &str, json: bool) -> CtResult<()> {
    crate::engine::output::emit_worker(value, human, json).map_err(CtError::Store)
}

#[cfg(test)]
#[path = "worker_service_tests.rs"]
mod tests;
