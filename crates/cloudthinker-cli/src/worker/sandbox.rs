use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use axoupdater::Version;
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, OpenOptions};
use cloudthinker_client::installed_agent_binary;

pub const UNAVAILABLE_CODE: &str = "EXECUTOR_SANDBOX_UNAVAILABLE";
pub const UNAVAILABLE_MESSAGE: &str = "This conversation must run its commands in a sandbox, and this outpost cannot sandbox them. Serve a project folder, not your home folder, from Linux 6.2 or newer with Landlock enabled, and check with `cloudthinker worker doctor`. macOS and Windows outposts cannot run Loupe.";
pub const SANDBOX_DIR: &str = ".cloudthinker-sandbox";
const SANDBOX_HOME: &str = ".cloudthinker-sandbox/home";
const SANDBOX_TMP: &str = ".cloudthinker-sandbox/tmp";
const WORK_DIRS: [&str; 5] = [
    SANDBOX_DIR,
    ".cloudthinker-loupe/runs",
    ".cloudthinker-loupe/warm",
    ".cloudthinker-loupe/sessions",
    ".cloudthinker-loupe/recipes",
];
const PLAYWRIGHT_ENV: &str = "PLAYWRIGHT_BROWSERS_PATH";
const PLAYWRIGHT_CACHE: &str = ".cache/ms-playwright";
const AGENT_BIN_ENV: &str = "CLOUDTHINKER_AGENT_BIN";
const AGENT_BIN_ROOT: &str = ".cloudthinker/agent/bin";
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const SYSTEM_READ: [&str; 12] = [
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/etc",
    "/opt",
    "/proc",
    "/sys",
    "/run/systemd/resolve",
    "/snap",
];
const SYSTEM_WRITE: [&str; 7] = [
    "/dev/shm",
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
];
const SHARED_TMP: [&str; 2] = ["/tmp", "/var/tmp"];
const PRIVATE_TREES: [&str; 11] = [
    ".ssh",
    ".aws",
    ".config",
    ".kube",
    ".gnupg",
    ".docker",
    ".azure",
    ".password-store",
    ".cloudthinker",
    ".local/share/keyrings",
    ".local/state",
];
const PRIVATE_FILES: [&str; 6] = [
    ".netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    ".cargo/credentials",
    ".cargo/credentials.toml",
];
const PASSED_ENV: [&str; 14] = [
    "PATH",
    "TERM",
    "TZ",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];
const CA_ENV: [&str; 3] = ["NODE_EXTRA_CA_CERTS", "SSL_CERT_FILE", "SSL_CERT_DIR"];
const TOOL_HOMES: [&str; 14] = [
    ".asdf",
    ".homebrew",
    ".linuxbrew",
    ".volta",
    ".pyenv",
    ".rbenv",
    ".nodenv",
    ".nvm",
    ".fnm",
    ".bun",
    ".deno",
    ".local/share/mise",
    ".local/share/fnm",
    ".local/share/pnpm",
];
const LOCAL_BIN_TOOL_ROOTS: [&str; 3] = [
    ".local/share/uv/tools",
    ".local/share/uv/python",
    ".local/share/pipx",
];
const VERSION_MANAGERS: [(&str, &str); 5] = [
    (".asdf", "ASDF_DATA_DIR"),
    (".volta", "VOLTA_HOME"),
    (".pyenv", "PYENV_ROOT"),
    (".rbenv", "RBENV_ROOT"),
    (".local/share/mise", "MISE_DATA_DIR"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    Ready,
    Unavailable(String),
}

impl Support {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

#[derive(Debug)]
pub struct SandboxError(String);

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub struct PlanInputs<'a> {
    pub home: &'a Path,
    pub state: &'a Path,
    pub bundles: &'a Path,
    pub served: &'a [&'a Path],
    pub git_roots: &'a [PathBuf],
    pub resolver: Option<&'a Path>,
    pub playwright: Option<&'a Path>,
    pub agent_binary: Option<&'a Path>,
    pub process_env: &'a BTreeMap<String, String>,
    pub injected_env: &'a BTreeMap<String, String>,
    pub resolve: fn(&Path) -> Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPlan {
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
    pub env: BTreeMap<String, String>,
}

fn readable_root(home: &Path, state: &Path, candidate: &Path) -> bool {
    let trees: Vec<PathBuf> = PRIVATE_TREES
        .iter()
        .map(|name| home.join(name))
        .chain([state.to_path_buf()])
        .collect();
    candidate.is_absolute()
        && !SHARED_TMP.iter().any(|tmp| candidate.starts_with(tmp))
        && !home.starts_with(candidate)
        && !trees
            .iter()
            .chain(&PRIVATE_FILES.map(|name| home.join(name)))
            .any(|path| path.starts_with(candidate))
        && !trees.iter().any(|tree| candidate.starts_with(tree))
}

fn tool_home(home: &Path, parent: &Path) -> bool {
    parent.strip_prefix(home).map_or(true, |rest| {
        TOOL_HOMES.iter().any(|tool| rest.starts_with(tool))
    })
}

pub fn plan(inputs: &PlanInputs<'_>) -> SandboxPlan {
    let home = inputs.home;
    let operation_root = inputs.served.first().copied().unwrap_or(home);
    let sandbox_home = operation_root.join(SANDBOX_HOME);
    let sandbox_tmp = operation_root.join(SANDBOX_TMP);
    let allowed = |candidate: &Path| readable_root(home, inputs.state, candidate);
    let admit = |candidate: &Path| -> Option<PathBuf> {
        let real = (inputs.resolve)(candidate).unwrap_or_else(|| candidate.to_path_buf());
        (allowed(candidate) && allowed(&real)).then_some(real)
    };
    let mut read: Vec<PathBuf> = SYSTEM_READ.iter().map(PathBuf::from).collect();
    read.extend([
        inputs.bundles.to_path_buf(),
        home.join(AGENT_BIN_ROOT),
        home.join(".cloudthinker/tools"),
    ]);
    let playwright = inputs.playwright.filter(|path| allowed(path));
    read.extend(playwright.map(Path::to_path_buf));
    let path_value = inputs
        .process_env
        .get("PATH")
        .map(String::as_str)
        .unwrap_or_default();
    for entry in path_value.split(':').map(Path::new) {
        let mut candidates = vec![(entry.to_path_buf(), false)];
        if entry
            .file_name()
            .is_some_and(|name| name == "bin" || name == "shims")
            && let Some(parent) = entry.parent()
        {
            candidates.push((parent.to_path_buf(), true));
        }
        if entry == home.join(".cargo/bin") {
            candidates.push((home.join(".rustup"), false));
        }
        if entry == home.join(".local/bin") {
            candidates.extend(
                LOCAL_BIN_TOOL_ROOTS
                    .iter()
                    .map(|root| (home.join(root), false)),
            );
        }
        for (candidate, parent) in candidates {
            if let Some(real) = admit(&candidate)
                && (!parent || tool_home(home, &real))
                && !read.contains(&real)
            {
                read.push(real);
            }
        }
    }
    for root in inputs.served {
        if !read.iter().any(|known| known == root) {
            read.push(root.to_path_buf());
        }
    }
    for key in CA_ENV {
        if let Some(path) = inputs.process_env.get(key).map(Path::new)
            && let Some(real) = admit(path)
            && !read.iter().any(|known| real.starts_with(known))
        {
            read.push(real);
        }
    }
    for root in inputs
        .git_roots
        .iter()
        .map(PathBuf::as_path)
        .chain(inputs.resolver)
    {
        if let Some(real) = admit(root)
            && !read.iter().any(|known| real.starts_with(known))
        {
            read.push(real);
        }
    }
    let mut write: Vec<PathBuf> = WORK_DIRS
        .iter()
        .map(|name| operation_root.join(name))
        .collect();
    write.extend(SYSTEM_WRITE.iter().map(PathBuf::from));
    let mut env: BTreeMap<String, String> = inputs
        .process_env
        .iter()
        .filter(|(key, _)| {
            PASSED_ENV.contains(&key.as_str()) || key.as_str() == "LANG" || key.starts_with("LC_")
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    env.extend(
        inputs
            .injected_env
            .iter()
            .filter(|(key, _)| key.starts_with("CLOUDTHINKER_"))
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let text = |path: &Path| path.to_string_lossy().into_owned();
    env.insert("HOME".into(), text(&sandbox_home));
    env.insert("TMPDIR".into(), text(&sandbox_tmp));
    env.insert("XDG_CACHE_HOME".into(), text(&sandbox_home.join(".cache")));
    for (folder, key) in VERSION_MANAGERS {
        let root = inputs
            .process_env
            .get(key)
            .map_or_else(|| home.join(folder), PathBuf::from);
        if let Some(real) = admit(&root)
            && read.iter().any(|known| real.starts_with(known))
        {
            env.insert(key.into(), text(&real));
        }
    }
    if let Some(playwright) = playwright {
        env.insert(PLAYWRIGHT_ENV.into(), text(playwright));
    }
    if let Some(agent_binary) = inputs.agent_binary {
        env.insert(AGENT_BIN_ENV.into(), text(agent_binary));
    }
    SandboxPlan { read, write, env }
}

impl SandboxPlan {
    pub fn arguments(&self) -> Vec<OsString> {
        let mut arguments = Vec::new();
        for (flag, paths) in [("--read", &self.read), ("--write", &self.write)] {
            for path in paths {
                arguments.push(OsString::from(flag));
                arguments.push(path.clone().into_os_string());
            }
        }
        arguments
    }

    pub fn prepare_home(&self, operation_root: &Dir) -> Result<(), &'static str> {
        let mut builder = DirBuilder::new();
        builder.recursive(true).mode(0o700);
        for dir in WORK_DIRS.iter().chain(&[SANDBOX_HOME, SANDBOX_TMP]) {
            if operation_root
                .symlink_metadata(dir)
                .is_ok_and(|meta| !meta.is_dir())
            {
                operation_root
                    .remove_file(dir)
                    .map_err(|_| "TRUSTED_ROOT_REJECTED")?;
            }
            operation_root
                .create_dir_with(dir, &builder)
                .map_err(|_| "TRUSTED_ROOT_REJECTED")?;
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        match operation_root.open_with(Path::new(SANDBOX_DIR).join(".gitignore"), &options) {
            Ok(mut file) => file.write_all(b"*\n").map_err(|_| "TRUSTED_ROOT_REJECTED"),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
            Err(_) => Err("TRUSTED_ROOT_REJECTED"),
        }
    }
}

pub struct SandboxContext {
    pub support: Support,
    home: PathBuf,
    private_state: PathBuf,
    process_env: BTreeMap<String, String>,
    git_roots: Vec<PathBuf>,
}

impl SandboxContext {
    pub async fn detect(private_state: &Path, served: &Path) -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute() && home.parent().is_some())
            .map(|home| home.canonicalize().unwrap_or(home));
        let support = match &home {
            Some(home) if !readable_root(home, private_state, served) => Support::Unavailable(
                "the served folder is your home folder or holds private files; serve a project folder instead".into(),
            ),
            Some(_) => probe().await,
            None => Support::Unavailable("HOME is not set to a home folder".into()),
        };
        Self {
            support,
            home: home.unwrap_or_default(),
            private_state: private_state.to_path_buf(),
            process_env: std::env::vars().collect(),
            git_roots: resolve_git_roots(served),
        }
    }

    #[cfg(test)]
    pub fn unavailable(reason: &str) -> Self {
        Self {
            support: Support::Unavailable(reason.into()),
            home: PathBuf::new(),
            private_state: PathBuf::new(),
            process_env: BTreeMap::new(),
            git_roots: Vec::new(),
        }
    }

    pub fn plan(
        &self,
        operation_root: &Path,
        served: &Path,
        bundles: &Path,
        injected_env: &BTreeMap<String, String>,
    ) -> SandboxPlan {
        let agent_binary = newest_agent_binary(&self.home.join(AGENT_BIN_ROOT));
        let playwright = self.playwright();
        let resolver = std::fs::canonicalize("/etc/resolv.conf").ok();
        let git_roots = self.trusted_git_roots(operation_root, served);
        plan(&PlanInputs {
            home: &self.home,
            state: &self.private_state,
            bundles,
            served: &[operation_root, served],
            git_roots: &git_roots,
            resolver: resolver.as_deref(),
            playwright: playwright.as_deref(),
            agent_binary: agent_binary.as_deref(),
            process_env: &self.process_env,
            injected_env,
            resolve: |path| std::fs::canonicalize(path).ok(),
        })
    }
}

impl SandboxContext {
    fn trusted_git_roots(&self, operation_root: &Path, served: &Path) -> Vec<PathBuf> {
        let served = served
            .canonicalize()
            .unwrap_or_else(|_| served.to_path_buf());
        let mut roots = self.git_roots.clone();
        for root in resolve_git_roots(operation_root) {
            let trusted = root.starts_with(&served)
                || self.git_roots.iter().any(|known| root.starts_with(known));
            if trusted && !roots.contains(&root) {
                roots.push(root);
            }
        }
        roots
    }

    fn playwright(&self) -> Option<PathBuf> {
        self.process_env
            .get(PLAYWRIGHT_ENV)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| self.home.join(PLAYWRIGHT_CACHE))
            .canonicalize()
            .ok()
            .filter(|path| path.is_dir())
    }
}

fn resolve_git_roots(folder: &Path) -> Vec<PathBuf> {
    let dot_git = folder.join(".git");
    let git_dir = match std::fs::read_to_string(&dot_git) {
        Ok(text) => match text.trim().strip_prefix("gitdir:") {
            Some(path) => folder.join(path.trim()),
            None => return Vec::new(),
        },
        Err(_) => dot_git,
    };
    let Ok(git_dir) = git_dir.canonicalize() else {
        return Vec::new();
    };
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .and_then(|relative| git_dir.join(relative.trim()).canonicalize().ok());
    let objects = common.as_deref().unwrap_or(&git_dir).join("objects");
    let alternates = std::fs::read_to_string(objects.join("info/alternates")).unwrap_or_default();
    let alternates: Vec<PathBuf> = alternates
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| objects.join(line).canonicalize().ok())
        .collect();
    [git_dir]
        .into_iter()
        .chain(common)
        .chain(alternates)
        .collect()
}

fn newest_agent_binary(bin_root: &Path) -> Option<PathBuf> {
    if let Some(binary) = installed_agent_binary(bin_root, env!("CARGO_PKG_VERSION")) {
        return Some(binary);
    }
    std::fs::read_dir(bin_root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let version = entry.file_name().into_string().ok()?;
            let binary = installed_agent_binary(bin_root, &version)?;
            Some((Version::parse(&version).ok()?, binary))
        })
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, binary)| binary)
}

pub fn executable() -> PathBuf {
    PathBuf::from("/proc/self/exe")
}

pub fn exec_command(plan: &SandboxPlan, program: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(executable());
    command
        .args(["worker", "sandbox-exec"])
        .args(plan.arguments())
        .arg("--")
        .arg(program)
        .env_clear()
        .envs(&plan.env);
    command
}

pub async fn probe() -> Support {
    if !cfg!(target_os = "linux") {
        return Support::Unavailable("Landlock needs Linux".into());
    }
    let run = tokio::process::Command::new(executable())
        .args(["worker", "sandbox-exec", "--probe"])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(PROBE_TIMEOUT, run).await {
        Ok(Ok(output)) if output.status.success() => Support::Ready,
        Ok(Ok(output)) => Support::Unavailable(
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next_back()
                .unwrap_or("the Landlock probe failed")
                .trim()
                .to_owned(),
        ),
        Ok(Err(error)) => {
            Support::Unavailable(format!("the Landlock probe could not run: {error}"))
        }
        Err(_) => Support::Unavailable(format!(
            "the Landlock probe timed out after {} s",
            PROBE_TIMEOUT.as_secs()
        )),
    }
}

#[cfg(target_os = "linux")]
mod landlock_rules {
    use std::collections::BTreeMap;
    use std::fmt::Display;
    use std::path::PathBuf;

    use landlock::{
        ABI, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, RulesetCreatedAttr,
        RulesetError, RulesetStatus, Scope, path_beneath_rules,
    };
    use seccompiler::{
        BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule, TargetArch, apply_filter,
    };

    use super::SandboxError;

    const REQUIRED_ABI: ABI = ABI::V3;
    const NEWEST_ABI: ABI = ABI::V9;
    const X32_SYSCALL_BIT: i64 = 0x4000_0000;
    const X32_IOCTL: i64 = 514;

    fn failed(error: RulesetError) -> SandboxError {
        SandboxError(format!("Landlock refused the sandbox: {error}"))
    }

    fn seccomp_failed(error: impl Display) -> SandboxError {
        SandboxError(format!("seccomp refused the sandbox: {error}"))
    }

    fn with_x32(number: i64) -> Vec<i64> {
        if cfg!(target_arch = "x86_64") {
            vec![number, X32_SYSCALL_BIT | number]
        } else {
            vec![number]
        }
    }

    fn arg_equals(index: u8, value: u64) -> Result<SeccompRule, SandboxError> {
        SeccompCondition::new(index, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, value)
            .and_then(|condition| SeccompRule::new(vec![condition]))
            .map_err(seccomp_failed)
    }

    fn ioctl_numbers() -> Vec<i64> {
        let mut numbers = with_x32(libc::SYS_ioctl);
        if cfg!(target_arch = "x86_64") {
            numbers.push(X32_SYSCALL_BIT | X32_IOCTL);
        }
        numbers
    }

    fn syscall_filter() -> Result<BpfProgram, SandboxError> {
        let unix = arg_equals(0, u64::from(libc::AF_UNIX.unsigned_abs()))?;
        let terminal_input = vec![
            arg_equals(1, libc::TIOCSTI)?,
            arg_equals(1, libc::TIOCLINUX)?,
        ];
        let mut rules: BTreeMap<i64, Vec<SeccompRule>> = with_x32(libc::SYS_socket)
            .into_iter()
            .map(|number| (number, vec![unix.clone()]))
            .collect();
        rules.extend(
            [
                libc::SYS_io_uring_setup,
                libc::SYS_keyctl,
                libc::SYS_add_key,
                libc::SYS_request_key,
            ]
            .into_iter()
            .flat_map(with_x32)
            .map(|number| (number, Vec::new())),
        );
        rules.extend(
            ioctl_numbers()
                .into_iter()
                .map(|number| (number, terminal_input.clone())),
        );
        let arch = TargetArch::try_from(std::env::consts::ARCH).map_err(seccomp_failed)?;
        SeccompFilter::new(
            rules,
            SeccompAction::Allow,
            SeccompAction::Errno(libc::EACCES.unsigned_abs()),
            arch,
        )
        .and_then(BpfProgram::try_from)
        .map_err(seccomp_failed)
    }

    fn deny_syscalls() -> Result<(), SandboxError> {
        apply_filter(&syscall_filter()?).map_err(seccomp_failed)
    }

    pub fn probe_self() -> Result<(), SandboxError> {
        let status = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(REQUIRED_ABI))
            .and_then(|ruleset| ruleset.create())
            .and_then(|ruleset| ruleset.restrict_self())
            .map_err(failed)?;
        if status.ruleset != RulesetStatus::FullyEnforced {
            return Err(SandboxError(
                "Landlock is not fully enforced on this kernel".into(),
            ));
        }
        deny_syscalls()
    }

    pub fn restrict(read: &[PathBuf], write: &[PathBuf]) -> Result<(), SandboxError> {
        let status = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(REQUIRED_ABI))
            .and_then(|ruleset| {
                ruleset
                    .set_compatibility(CompatLevel::BestEffort)
                    .handle_access(AccessFs::from_all(NEWEST_ABI))
            })
            .and_then(|ruleset| ruleset.scope(Scope::from_all(NEWEST_ABI)))
            .and_then(|ruleset| ruleset.create())
            .and_then(|ruleset| {
                ruleset.add_rules(path_beneath_rules(read, AccessFs::from_read(NEWEST_ABI)))
            })
            .and_then(|ruleset| {
                ruleset.add_rules(path_beneath_rules(write, AccessFs::from_all(NEWEST_ABI)))
            })
            .and_then(|ruleset| ruleset.restrict_self())
            .map_err(failed)?;
        if status.ruleset == RulesetStatus::NotEnforced || !status.no_new_privs {
            return Err(SandboxError(
                "Landlock is not enforced on this kernel".into(),
            ));
        }
        deny_syscalls()
    }
}

#[cfg(target_os = "linux")]
pub use landlock_rules::{probe_self, restrict};

#[cfg(not(target_os = "linux"))]
pub fn probe_self() -> Result<(), SandboxError> {
    Err(SandboxError("Landlock needs Linux".into()))
}

#[cfg(not(target_os = "linux"))]
pub fn restrict(_read: &[PathBuf], _write: &[PathBuf]) -> Result<(), SandboxError> {
    Err(SandboxError("Landlock needs Linux".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn plan_reads_path_installs_under_home_but_never_home_secrets_or_state() {
        let home = Path::new("/home/dev");
        let state = Path::new("/home/dev/.config/cloudthinker/worker-state/abc");
        let served = Path::new("/srv/repo");
        let process = env(&[
            (
                "PATH",
                "/home/dev/.nvm/versions/node/v20/bin:/home/dev/.asdf/shims:/tmp/bin:/home/dev/.bun/bin:/home/dev/.local/bin:/home/dev/.cargo/bin:/home/dev:/home/dev/.config/tool/bin:/usr/bin:/:relative/bin",
            ),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
            ("TERM", "xterm"),
            ("SSH_AUTH_SOCK", "/tmp/ssh-x/agent.1"),
            ("AWS_SECRET_ACCESS_KEY", "secret"),
            ("GITHUB_TOKEN", "secret"),
            ("KUBECONFIG", "/home/dev/.kube/config"),
            ("CLOUDTHINKER_WORKER_TOKEN", "secret"),
            ("HOME", "/home/dev"),
        ]);
        let injected = env(&[("CLOUDTHINKER_SKILL_BUNDLES", "/bundles"), ("USER", "dev")]);
        let playwright = home.join(".cache/ms-playwright");
        let plan = plan(&PlanInputs {
            home,
            state,
            bundles: Path::new("/bundles"),
            served: &[served],
            git_roots: &[],
            resolver: Some(Path::new("/run/NetworkManager/resolv.conf")),
            playwright: Some(&playwright),
            agent_binary: Some(Path::new(
                "/home/dev/.cloudthinker/agent/bin/0.10.0/cloudthinker-agent/cloudthinker-agent",
            )),
            process_env: &process,
            injected_env: &injected,
            resolve: |_| None,
        });
        for expected in [
            "/home/dev/.nvm/versions/node/v20/bin",
            "/home/dev/.nvm/versions/node/v20",
            "/home/dev/.bun/bin",
            "/home/dev/.bun",
            "/home/dev/.local/bin",
            "/home/dev/.cargo/bin",
            "/home/dev/.rustup",
            "/home/dev/.asdf",
            "/run/NetworkManager/resolv.conf",
            "/home/dev/.cloudthinker/agent/bin",
            "/home/dev/.cache/ms-playwright",
            "/bundles",
            "/usr",
        ] {
            assert!(plan.read.contains(&PathBuf::from(expected)), "{expected}");
        }
        for forbidden in [
            "/home/dev",
            "/home",
            "/",
            "/home/dev/.local",
            "/home/dev/.cargo",
            "/home/dev/.config/tool/bin",
            "/home/dev/.config/tool",
            "relative/bin",
        ] {
            let forbidden = PathBuf::from(forbidden);
            assert!(!plan.read.contains(&forbidden), "{}", forbidden.display());
            assert!(!plan.write.contains(&forbidden), "{}", forbidden.display());
        }
        assert!(
            plan.read
                .iter()
                .chain(&plan.write)
                .all(|root| !state.starts_with(root) && !root.starts_with(state))
        );
        assert!(plan.read.contains(&served.to_path_buf()));
        assert!(!plan.write.contains(&served.to_path_buf()));
        for work in [
            "/srv/repo/.cloudthinker-sandbox",
            "/srv/repo/.cloudthinker-loupe/runs",
            "/srv/repo/.cloudthinker-loupe/warm",
        ] {
            assert!(plan.write.contains(&PathBuf::from(work)), "{work}");
        }
        assert!(
            !plan
                .write
                .contains(&PathBuf::from("/srv/repo/.cloudthinker-loupe"))
        );
        for shared in ["/tmp", "/tmp/bin", "/run", "/dev", "/dev/pts", "/dev/ptmx"] {
            let shared = PathBuf::from(shared);
            assert!(!plan.read.contains(&shared) && !plan.write.contains(&shared));
        }
        let sandbox_home = "/srv/repo/.cloudthinker-sandbox/home";
        assert_eq!(
            plan.env,
            env(&[
                (
                    "PATH",
                    "/home/dev/.nvm/versions/node/v20/bin:/home/dev/.asdf/shims:/tmp/bin:/home/dev/.bun/bin:/home/dev/.local/bin:/home/dev/.cargo/bin:/home/dev:/home/dev/.config/tool/bin:/usr/bin:/:relative/bin",
                ),
                ("LANG", "C.UTF-8"),
                ("LC_ALL", "C.UTF-8"),
                ("TERM", "xterm"),
                ("CLOUDTHINKER_SKILL_BUNDLES", "/bundles"),
                ("HOME", sandbox_home),
                ("TMPDIR", "/srv/repo/.cloudthinker-sandbox/tmp"),
                (
                    "XDG_CACHE_HOME",
                    "/srv/repo/.cloudthinker-sandbox/home/.cache"
                ),
                ("ASDF_DATA_DIR", "/home/dev/.asdf"),
                ("PLAYWRIGHT_BROWSERS_PATH", "/home/dev/.cache/ms-playwright"),
                (
                    "CLOUDTHINKER_AGENT_BIN",
                    "/home/dev/.cloudthinker/agent/bin/0.10.0/cloudthinker-agent/cloudthinker-agent",
                ),
            ])
        );
    }

    #[test]
    fn plan_checks_the_real_path_and_reads_a_tool_home_only_for_known_tools() {
        fn nfs(path: &Path) -> Option<PathBuf> {
            path.strip_prefix("/home/dev")
                .ok()
                .map(|rest| Path::new("/nfs/home/dev").join(rest))
        }
        let home = Path::new("/nfs/home/dev");
        let process = env(&[
            (
                "PATH",
                "/home/dev/bin:/home/dev/.pulumi/bin:/home/dev/.bun/bin:/home/dev/.asdf/shims:/usr/bin",
            ),
            ("SSL_CERT_FILE", "/home/dev/.ssh/ca.pem"),
        ]);
        let plan = plan(&PlanInputs {
            home,
            state: Path::new("/nfs/home/dev/.config/cloudthinker/worker-state/abc"),
            bundles: Path::new("/bundles"),
            served: &[Path::new("/srv/repo")],
            git_roots: &[],
            resolver: None,
            playwright: None,
            agent_binary: None,
            process_env: &process,
            injected_env: &BTreeMap::new(),
            resolve: nfs,
        });
        for expected in [
            "/nfs/home/dev/bin",
            "/nfs/home/dev/.pulumi/bin",
            "/nfs/home/dev/.bun",
            "/nfs/home/dev/.asdf",
        ] {
            assert!(plan.read.contains(&PathBuf::from(expected)), "{expected}");
        }
        for forbidden in [
            "/home/dev",
            "/nfs/home/dev",
            "/nfs/home/dev/.pulumi",
            "/nfs/home/dev/.ssh/ca.pem",
        ] {
            assert!(
                !plan.read.contains(&PathBuf::from(forbidden)),
                "{forbidden}"
            );
        }
        assert_eq!(
            plan.env.get("ASDF_DATA_DIR").map(String::as_str),
            Some("/nfs/home/dev/.asdf")
        );
    }

    #[cfg(unix)]
    #[test]
    fn preparing_the_home_never_writes_through_a_link_the_sandbox_planted() {
        let root = tempfile::tempdir().unwrap();
        let served = root.path();
        std::fs::create_dir_all(served.join(".git")).unwrap();
        std::fs::write(served.join(".git/config"), "[core]\n").unwrap();
        std::fs::create_dir_all(served.join(SANDBOX_DIR)).unwrap();
        std::os::unix::fs::symlink(
            "../.git/config",
            served.join(SANDBOX_DIR).join(".gitignore"),
        )
        .unwrap();
        std::os::unix::fs::symlink("../.git", served.join(SANDBOX_HOME)).unwrap();
        std::fs::write(served.join(SANDBOX_TMP), "planted").unwrap();
        let plan = SandboxPlan {
            read: Vec::new(),
            write: Vec::new(),
            env: BTreeMap::new(),
        };
        let dir = Dir::open_ambient_dir(served, cap_std::ambient_authority()).unwrap();

        plan.prepare_home(&dir).unwrap();

        assert_eq!(
            std::fs::read_to_string(served.join(".git/config")).unwrap(),
            "[core]\n"
        );
        for dir in [SANDBOX_HOME, SANDBOX_TMP] {
            let meta = std::fs::symlink_metadata(served.join(dir)).unwrap();
            assert!(meta.is_dir() && !meta.file_type().is_symlink(), "{dir}");
        }
    }

    #[test]
    fn plan_never_reads_a_browser_folder_inside_home_secrets() {
        let home = Path::new("/home/dev");
        let state = Path::new("/home/dev/.config/cloudthinker/worker-state/abc");
        let process = env(&[("PATH", "/usr/bin")]);
        for browsers in ["/home/dev/.config/pw-browsers", "/home/dev", "/home"] {
            let browsers = Path::new(browsers);
            let plan = plan(&PlanInputs {
                home,
                state,
                bundles: Path::new("/bundles"),
                served: &[Path::new("/srv/repo")],
                git_roots: &[],
                resolver: None,
                playwright: Some(browsers),
                agent_binary: None,
                process_env: &process,
                injected_env: &BTreeMap::new(),
                resolve: |_| None,
            });
            assert!(
                !plan.read.contains(&browsers.to_path_buf()),
                "{}",
                browsers.display()
            );
            assert!(
                !plan.env.contains_key(PLAYWRIGHT_ENV),
                "{}",
                browsers.display()
            );
        }
    }

    #[test]
    fn git_roots_follow_a_linked_worktree_to_its_common_dir_and_alternates() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let main = root.join("main/.git");
        let linked = main.join("worktrees/feature");
        let shared = root.join("shared/objects");
        let served = root.join("feature");
        for dir in [&linked, &main.join("objects/info"), &shared, &served] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(linked.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            main.join("objects/info/alternates"),
            "# shared\n../../../shared/objects\n/missing/objects\n",
        )
        .unwrap();
        std::fs::write(
            served.join(".git"),
            format!("gitdir: {}\n", linked.display()),
        )
        .unwrap();
        assert_eq!(resolve_git_roots(&served), [linked, main, shared]);
        assert!(resolve_git_roots(&root).is_empty());
    }

    #[test]
    fn git_roots_added_after_the_worker_starts_stay_unreadable() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let served = root.join("served");
        let objects = served.join(".git/objects");
        let shared = root.join("shared/objects");
        let private = root.join("home/.claude");
        for dir in [&objects.join("info"), &shared, &private] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let alternates = objects.join("info/alternates");
        std::fs::write(&alternates, format!("{}\n", shared.display())).unwrap();
        let context = SandboxContext {
            support: Support::Ready,
            home: root.join("home"),
            private_state: root.join("state"),
            process_env: BTreeMap::new(),
            git_roots: resolve_git_roots(&served),
        };
        std::fs::write(
            &alternates,
            format!("{}\n{}\n", shared.display(), private.display()),
        )
        .unwrap();
        let roots = context.trusted_git_roots(&served, &served);
        assert!(roots.contains(&shared));
        assert!(!roots.contains(&private));
    }

    #[test]
    fn plan_reads_git_roots_outside_the_served_folder_but_never_home_secrets() {
        let home = Path::new("/home/dev");
        let state = Path::new("/home/dev/.config/cloudthinker/worker-state/abc");
        let process = env(&[("PATH", "/usr/bin")]);
        let git_roots = [
            "/home/dev/src/main/.git/worktrees/feature",
            "/home/dev/src/main/.git",
            "/srv/repo/.git",
            "/home/dev/.config/repo/.git",
            "/home/dev",
            "/home",
        ]
        .map(PathBuf::from);
        let plan = plan(&PlanInputs {
            home,
            state,
            bundles: Path::new("/bundles"),
            served: &[Path::new("/srv/repo")],
            git_roots: &git_roots,
            resolver: None,
            playwright: None,
            agent_binary: None,
            process_env: &process,
            injected_env: &BTreeMap::new(),
            resolve: |_| None,
        });
        let read = |path: &str| plan.read.contains(&PathBuf::from(path));
        assert!(read("/home/dev/src/main/.git/worktrees/feature"));
        assert!(read("/home/dev/src/main/.git"));
        for refused in [
            "/srv/repo/.git",
            "/home/dev/.config/repo/.git",
            "/home/dev",
            "/home",
        ] {
            assert!(!read(refused), "{refused}");
        }
    }

    #[test]
    fn only_a_project_folder_can_be_served_to_the_sandbox() {
        let home = Path::new("/home/dev");
        let state = Path::new("/home/dev/.config/cloudthinker/worker-state/abc");
        for refused in [
            "/home/dev",
            "/home",
            "/",
            "/home/dev/.ssh/repo",
            "/home/dev/.config",
        ] {
            assert!(!readable_root(home, state, Path::new(refused)), "{refused}");
        }
        assert!(readable_root(home, state, Path::new("/home/dev/src/app")));
    }

    #[test]
    fn plan_keeps_proxy_and_ca_settings_and_reads_the_ca_file() {
        let process = env(&[
            ("PATH", "/usr/bin"),
            ("HTTPS_PROXY", "http://proxy.corp:3128"),
            ("NODE_EXTRA_CA_CERTS", "/srv/certs/corp.pem"),
            ("SSL_CERT_FILE", "/home/dev/.ssh/ca.pem"),
        ]);
        let plan = plan(&PlanInputs {
            home: Path::new("/home/dev"),
            state: Path::new("/home/dev/.config/cloudthinker/worker-state/abc"),
            bundles: Path::new("/bundles"),
            served: &[Path::new("/srv/repo")],
            git_roots: &[],
            resolver: None,
            playwright: None,
            agent_binary: None,
            process_env: &process,
            injected_env: &BTreeMap::new(),
            resolve: |_| None,
        });
        assert_eq!(
            plan.env.get("HTTPS_PROXY").map(String::as_str),
            Some("http://proxy.corp:3128")
        );
        assert!(plan.read.contains(&PathBuf::from("/srv/certs/corp.pem")));
        assert!(!plan.read.contains(&PathBuf::from("/home/dev/.ssh/ca.pem")));
    }

    #[test]
    fn the_newest_agent_version_sorts_by_number_and_release_after_prerelease() {
        let mut versions = ["0.9.3", "0.10.0-dev.10", "0.10.0", "0.10.0-dev.2", "0.8.12"];
        versions.sort_by_key(|version| Version::parse(version).unwrap());
        assert_eq!(
            versions,
            ["0.8.12", "0.9.3", "0.10.0-dev.2", "0.10.0-dev.10", "0.10.0"]
        );
    }

    #[cfg(target_os = "linux")]
    struct Jail {
        _root: tempfile::TempDir,
        binary: PathBuf,
        home: PathBuf,
        state: PathBuf,
        served: PathBuf,
    }

    #[cfg(target_os = "linux")]
    impl Jail {
        fn new() -> Self {
            let mut binary = std::env::current_exe().unwrap();
            binary.pop();
            let root = tempfile::tempdir_in(&binary).unwrap();
            binary.pop();
            binary.push("cloudthinker");
            assert!(
                binary.is_file(),
                "run `cargo test` for the whole crate so the cloudthinker binary exists"
            );
            let home = root.path().join("home");
            let state = home.join(".config/cloudthinker/worker-state/abc");
            let served = root.path().join("served");
            for dir in [&state, &served] {
                std::fs::create_dir_all(dir).unwrap();
            }
            Self {
                _root: root,
                binary,
                home,
                state,
                served,
            }
        }

        fn run(&self, program: &[&std::ffi::OsStr]) -> std::process::Output {
            self.run_with_stdin(program, std::process::Stdio::null())
        }

        fn run_with_stdin(
            &self,
            program: &[&std::ffi::OsStr],
            stdin: std::process::Stdio,
        ) -> std::process::Output {
            let process = env(&[("PATH", "/usr/bin:/bin"), ("SECRET_TOKEN", "secret")]);
            let plan = plan(&PlanInputs {
                home: &self.home,
                state: &self.state,
                bundles: &self.state.join("skill-bundles"),
                served: &[self.served.as_path()],
                git_roots: &[],
                resolver: None,
                playwright: None,
                agent_binary: None,
                process_env: &process,
                injected_env: &BTreeMap::new(),
                resolve: |_| None,
            });
            let dir = Dir::open_ambient_dir(&self.served, cap_std::ambient_authority()).unwrap();
            plan.prepare_home(&dir).unwrap();
            std::process::Command::new(&self.binary)
                .args(["worker", "sandbox-exec"])
                .args(plan.arguments())
                .arg("--")
                .args(program)
                .current_dir(&self.served)
                .env_clear()
                .envs(&plan.env)
                .stdin(stdin)
                .output()
                .unwrap()
        }
    }

    #[cfg(target_os = "linux")]
    fn succeeded(output: &std::process::Output) -> String {
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_sandboxed_child_reaches_the_served_folder_but_not_home_secrets_or_state() {
        let jail = Jail::new();
        let key = jail.home.join(".ssh/id_ed25519");
        let credential = jail.state.join("target.json");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, "private key").unwrap();
        std::fs::write(&credential, "worker token").unwrap();
        std::fs::write(jail.served.join("input"), "input").unwrap();
        std::fs::create_dir_all(jail.served.join(".git/hooks")).unwrap();
        std::fs::create_dir_all(jail.served.join(".cloudthinker-loupe/node_modules")).unwrap();
        let script = r#"
            cat "$1" 2>/dev/null && exit 10
            cat "$2" 2>/dev/null && exit 11
            ls "$3" >/dev/null 2>&1 && exit 12
            cat "/proc/$PPID/environ" >/dev/null 2>&1 && exit 21
            [ -z "$SECRET_TOKEN" ] || exit 13
            cat input >/dev/null || exit 14
            (printf x > output) 2>/dev/null && exit 15
            (printf x > .git/hooks/post-checkout) 2>/dev/null && exit 16
            (printf x > "/tmp/cloudthinker-sandbox-probe-$$") 2>/dev/null && exit 17
            (printf x > .cloudthinker-loupe/node_modules/planted.js) 2>/dev/null && exit 22
            printf written > .cloudthinker-loupe/runs/output || exit 18
            mv .cloudthinker-loupe/runs/output .cloudthinker-loupe/runs/moved || exit 19
            touch "$HOME/profile" "$TMPDIR/scratch" || exit 20
            printf ok
        "#;
        let output = jail.run(&[
            "/bin/sh".as_ref(),
            "-c".as_ref(),
            script.as_ref(),
            "sh".as_ref(),
            key.as_os_str(),
            credential.as_os_str(),
            jail.home.as_os_str(),
        ]);
        assert_eq!(succeeded(&output), "ok");
        assert_eq!(
            std::fs::read_to_string(jail.served.join(".cloudthinker-loupe/runs/moved")).unwrap(),
            "written"
        );
        assert!(jail.served.join(SANDBOX_HOME).join("profile").is_file());
        assert!(!jail.served.join("output").exists());
        assert!(!jail.served.join(".git/hooks/post-checkout").exists());
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "private key");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_sandboxed_child_cannot_open_a_unix_socket_but_keeps_socketpair_and_tcp() {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            eprintln!("skipped: /usr/bin/python3 is not installed");
            return;
        }
        let jail = Jail::new();
        let socket_dir = tempfile::tempdir_in("/dev/shm").unwrap();
        let socket_path = socket_dir.path().join("agent.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        let script = r#"
import socket, sys
try:
    socket.socket(socket.AF_UNIX).connect(sys.argv[1])
except PermissionError:
    pass
else:
    sys.exit(20)
left, right = socket.socketpair()
left.sendall(b"ok")
socket.socket(socket.AF_INET).close()
sys.stdout.write(right.recv(2).decode())
"#;
        let output = jail.run(&[
            python.as_os_str(),
            "-c".as_ref(),
            script.as_ref(),
            socket_path.as_os_str(),
        ]);
        assert_eq!(succeeded(&output), "ok");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_sandboxed_child_cannot_read_the_kernel_keyrings() {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            eprintln!("skipped: /usr/bin/python3 is not installed");
            return;
        }
        let script = r#"
import ctypes, errno, sys
libc = ctypes.CDLL(None, use_errno=True)
found = libc.syscall(int(sys.argv[1]), 0, -4, 0)
sys.stdout.write("ok" if found != -1 else errno.errorcode[ctypes.get_errno()])
"#;
        let output = Jail::new().run(&[
            python.as_os_str(),
            "-c".as_ref(),
            script.as_ref(),
            libc::SYS_keyctl.to_string().as_ref(),
        ]);
        assert_eq!(succeeded(&output), "EACCES");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_sandboxed_child_cannot_push_input_into_a_terminal() {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            eprintln!("skipped: /usr/bin/python3 is not installed");
            return;
        }
        let script = r#"
import errno, fcntl, os, sys, termios
def code(action):
    try:
        action()
    except OSError as error:
        return errno.errorcode[error.errno]
    return "ok"
sys.stdout.write(code(lambda: fcntl.ioctl(0, termios.TIOCSTI, b"x")) + " " + code(os.openpty))
"#;
        let master =
            rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)
                .unwrap();
        rustix::pty::grantpt(&master).unwrap();
        rustix::pty::unlockpt(&master).unwrap();
        let name = rustix::pty::ptsname(&master, Vec::new()).unwrap();
        let terminal = rustix::fs::open(
            name.as_c_str(),
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        let output = Jail::new().run_with_stdin(
            &[python.as_os_str(), "-c".as_ref(), script.as_ref()],
            std::process::Stdio::from(terminal),
        );
        assert_eq!(succeeded(&output), "EACCES EACCES");
    }
}
