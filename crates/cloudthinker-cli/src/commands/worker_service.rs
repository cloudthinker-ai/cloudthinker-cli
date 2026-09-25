use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use clap::{Args, Subcommand};
use cloudthinker_client::auth::worker_store::WorkerStore;
use cloudthinker_client::{CtError, CtResult, origin_of};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[path = "worker_service_descriptor.rs"]
mod worker_service_descriptor;

use worker_service_descriptor::{
    descriptor_path, path_text, render_descriptor, require_descriptor, sync_directory,
    validate_descriptor_metadata, validate_label, validate_service_text, write_descriptor,
};

const SERVICE_NAMESPACE: &str = "io.cloudthinker.worker";

#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    /// Register the worker with this user's service manager, without starting it.
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
    /// Outpost UUID or exact name.
    #[arg(long, env = "CLOUDTHINKER_OUTPOST_ID")]
    pub outpost: String,

    /// The one folder the service's worker serves.
    #[arg(long)]
    pub workdir: PathBuf,

    /// How many assignments the service's worker serves at once.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u16).range(1..=32))]
    pub concurrency: u16,

    /// Short display name for the served folder.
    #[arg(long)]
    pub label: Option<String>,

    /// Emit the machine-readable service contract.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ServiceTargetArgs {
    /// Outpost UUID or exact name.
    #[arg(long, env = "CLOUDTHINKER_OUTPOST_ID")]
    pub outpost: String,

    /// The served folder that identifies the service.
    #[arg(long)]
    pub workdir: PathBuf,

    /// Emit the machine-readable service contract.
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

struct ServiceTarget {
    platform: ServicePlatform,
    target_id: Uuid,
    workdir: PathBuf,
    service_name: String,
    descriptor_path: PathBuf,
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
}

pub async fn execute(
    base_url: &str,
    workspace: Option<&str>,
    command: ServiceCommand,
) -> CtResult<()> {
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
    let target = target(&origin, &args.outpost, &args.workdir)?;
    require_stored_credential(&origin, target.target_id)?;
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
    let descriptor = render_descriptor(
        target.platform,
        &target.service_name,
        &argv,
        &target.workdir,
    )?;
    let spec = ServiceSpec { target, descriptor };
    let existed = match fs::symlink_metadata(&spec.target.descriptor_path) {
        Ok(metadata) => {
            validate_descriptor_metadata(&metadata)?;
            let existing = fs::read(&spec.target.descriptor_path).map_err(|_| {
                CtError::Store("worker service descriptor could not be read".into())
            })?;
            if existing != spec.descriptor.as_bytes() {
                return Err(CtError::Usage(
                    "a worker service already exists with different settings; uninstall it first"
                        .into(),
                ));
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => {
            return Err(CtError::Store(
                "worker service descriptor unavailable".into(),
            ));
        }
    };
    if !existed {
        write_descriptor(&spec.target.descriptor_path, spec.descriptor.as_bytes())?;
    }
    manager_install(&spec.target)?;
    let state = if existed { "unchanged" } else { "installed" };
    emit(
        spec.target.output(state),
        &format!(
            "Worker service {state}; start it with cloudthinker worker service start --outpost {} --workdir {}",
            spec.target.target_id,
            spec.target.workdir.display()
        ),
        args.json,
    )
}

fn uninstall(base_url: &str, args: ServiceTargetArgs) -> CtResult<()> {
    let target = target(base_url, &args.outpost, &args.workdir)?;
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
        target.output("removed"),
        "Worker service removed",
        args.json,
    )
}

fn status(base_url: &str, args: ServiceTargetArgs) -> CtResult<()> {
    let target = target(base_url, &args.outpost, &args.workdir)?;
    match fs::symlink_metadata(&target.descriptor_path) {
        Ok(metadata) => validate_descriptor_metadata(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return emit(
                target.output("absent"),
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
    emit(
        target.output(state),
        &format!("Worker service is {state}"),
        args.json,
    )
}

fn start(base_url: &str, args: ServiceTargetArgs) -> CtResult<()> {
    let target = target(base_url, &args.outpost, &args.workdir)?;
    require_descriptor(&target)?;
    require_stored_credential(base_url, target.target_id)?;
    manager_start(&target)?;
    emit(
        target.output("started"),
        "Worker service started",
        args.json,
    )
}

fn stop(base_url: &str, args: ServiceTargetArgs) -> CtResult<()> {
    let target = target(base_url, &args.outpost, &args.workdir)?;
    require_descriptor(&target)?;
    manager_stop(&target)?;
    emit(
        target.output("stopped"),
        "Worker service stopped gracefully",
        args.json,
    )
}

fn require_stored_credential(origin: &str, target_id: Uuid) -> CtResult<()> {
    WorkerStore::open_default(origin)?.load(&target_id.to_string())?;
    Ok(())
}

fn target(base_url: &str, selector: &str, workdir: &Path) -> CtResult<ServiceTarget> {
    let platform = ServicePlatform::current()?;
    let origin = origin_of(base_url)?;
    let target_id = match Uuid::parse_str(selector) {
        Ok(id) => id,
        Err(_) => {
            WorkerStore::open_default(&origin)?
                .load(selector)?
                .target_id
        }
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
    let descriptor_path = descriptor_path(platform, &service_name)?;
    Ok(ServiceTarget {
        platform,
        target_id,
        workdir,
        service_name,
        descriptor_path,
    })
}

fn service_id(origin: &str, target_id: Uuid, workdir: &Path) -> CtResult<String> {
    let workdir = path_text(workdir, "workdir")?;
    let digest = Sha256::digest(format!("{origin}\n{target_id}\n{workdir}").as_bytes());
    Ok(format!("{:x}", digest)[..24].to_string())
}

fn manager_install(target: &ServiceTarget) -> CtResult<()> {
    match target.platform {
        ServicePlatform::Systemd => {
            let unit = target.unit_name();
            run_manager("systemctl", &["--user", "daemon-reload"])?;
            run_manager("systemctl", &["--user", "enable", &unit])
        }
        ServicePlatform::Launchd => Ok(()),
    }
}

fn manager_uninstall(target: &ServiceTarget) -> CtResult<()> {
    match target.platform {
        ServicePlatform::Systemd => {
            let unit = target.unit_name();
            run_manager("systemctl", &["--user", "disable", "--now", &unit])
        }
        ServicePlatform::Launchd => launchd_bootout(target),
    }
}

fn manager_status(target: &ServiceTarget) -> CtResult<&'static str> {
    match target.platform {
        ServicePlatform::Systemd => {
            let unit = target.unit_name();
            let output = manager_output("systemctl", &["--user", "is-active", &unit])?;
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

fn manager_start(target: &ServiceTarget) -> CtResult<()> {
    match target.platform {
        ServicePlatform::Systemd => {
            let unit = target.unit_name();
            run_manager("systemctl", &["--user", "start", &unit])
        }
        ServicePlatform::Launchd => {
            if !launchd_loaded(target)? {
                let domain = launchd_domain()?;
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
            run_manager("systemctl", &["--user", "stop", &unit])
        }
        ServicePlatform::Launchd => launchd_bootout(target),
    }
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
    Command::new(program)
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
        Ok(())
    } else {
        Err(CtError::Store(format!(
            "{program} could not manage the worker service"
        )))
    }
}

fn launchd_domain() -> CtResult<String> {
    #[cfg(test)]
    if let Some(uid) = TEST_LAUNCHD_UID
        .get()
        .and_then(|value| value.lock().unwrap().to_owned())
    {
        return Ok(format!("gui/{uid}"));
    }
    #[cfg(target_os = "macos")]
    {
        return Ok(format!("gui/{}", rustix::process::geteuid().as_raw()));
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(CtError::Usage("launchd is available only on macOS".into()))
    }
}

fn launchd_service(target: &ServiceTarget) -> CtResult<String> {
    Ok(format!("{}/{}", launchd_domain()?, target.service_name))
}

fn launchd_bootout(target: &ServiceTarget) -> CtResult<()> {
    if !launchd_loaded(target)? {
        return Ok(());
    }
    run_manager("launchctl", &["bootout", &launchd_service(target)?])
}

fn launchd_loaded(target: &ServiceTarget) -> CtResult<bool> {
    let domain_output = manager_output("launchctl", &["print", &launchd_domain()?])?;
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

    fn output(&self, state: &'static str) -> ServiceOutput {
        ServiceOutput {
            manager: self.platform.name(),
            service: self.service_name.clone(),
            path: self.descriptor_path.display().to_string(),
            state,
        }
    }
}

fn emit(value: ServiceOutput, human: &str, json: bool) -> CtResult<()> {
    crate::engine::output::emit_worker(&value, human, json).map_err(CtError::Store)
}

#[cfg(test)]
#[path = "worker_service_tests.rs"]
mod tests;
