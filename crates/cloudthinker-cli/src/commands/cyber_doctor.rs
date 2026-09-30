//! `cyber doctor`: report a machine's readiness for a local Cyber pentest run,
//! and with `--fix`, fetch the pinned probe toolpack.
//!
//! The report is run-independent and needs no CloudThinker API call. It checks
//! the four preconditions a local run has: the project config, the `[cyber.auth]`
//! identities, the floor tools, and the pinned toolpack. `--fix` downloads only
//! the missing or outdated toolpack binaries, each verified against its pinned
//! sha256 before it lands on disk.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cloudthinker_client::{install_tool, installed_tool_binary, tools_bin_root};
use serde::{Deserialize, Serialize};

use super::cyber_config;
use crate::engine::{exit::ExitCode, output};

/// The pinned manifest. A tool or a platform is a data row here, never code.
const MANIFEST_JSON: &str = include_str!("toolpack.json");

/// Tools a local run always expects. Absence is reported, never
/// auto-installed: the floor is the developer's own shell environment.
const FLOOR_TOOLS: &[&str] = &["curl"];

#[derive(Debug, Deserialize)]
struct Manifest {
    tools: Vec<ToolSpec>,
}

#[derive(Debug, Deserialize)]
struct ToolSpec {
    name: String,
    version: String,
    platforms: BTreeMap<String, PlatformSpec>,
}

#[derive(Debug, Deserialize)]
struct PlatformSpec {
    url: String,
    sha256: String,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ToolStatus {
    Present,
    Outdated,
    Missing,
    Unsupported,
}

impl ToolStatus {
    fn label(&self) -> &'static str {
        match self {
            ToolStatus::Present => "present",
            ToolStatus::Outdated => "outdated",
            ToolStatus::Missing => "missing",
            ToolStatus::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Serialize)]
struct Identity {
    role: String,
    env: String,
    set: bool,
}

#[derive(Debug, Serialize)]
struct ContextItem {
    label: String,
    source: String,
    resolves: bool,
}

#[derive(Debug, Serialize)]
struct FloorTool {
    name: String,
    present: bool,
}

#[derive(Debug, Serialize)]
struct ToolReport {
    name: String,
    version: String,
    status: ToolStatus,
    path: Option<String>,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    platform: Option<String>,
    config_file: String,
    config: BTreeMap<String, String>,
    mode: String,
    mode_reason: String,
    identities: Vec<Identity>,
    bola_ready: bool,
    context: Vec<ContextItem>,
    floor: Vec<FloorTool>,
    toolpack: Vec<ToolReport>,
}

pub async fn doctor(fix: bool, json: bool) -> ExitCode {
    let manifest = match parse_manifest() {
        Ok(manifest) => manifest,
        Err(error) => {
            output::eprintln_error(&error);
            return ExitCode::JobFailed;
        }
    };
    let bin_root = match tools_bin_root() {
        Ok(root) => root,
        Err(error) => {
            output::eprintln_error(&error.to_string());
            return ExitCode::JobFailed;
        }
    };
    let platform = current_platform();

    let mut fix_failed = false;
    if fix {
        fix_failed = fix_toolpack(&manifest, &bin_root, platform.as_deref()).await;
    }

    let report = build_report(&manifest, &bin_root, platform);
    let result = if json {
        output::emit_json(&report)
    } else {
        render(&report)
    };
    match result {
        Err(error) => {
            output::eprintln_error(&error);
            ExitCode::JobFailed
        }
        Ok(()) if fix_failed => ExitCode::JobFailed,
        Ok(()) => ExitCode::Ok,
    }
}

/// Fetch every missing or outdated tool for this platform. Returns true when at
/// least one install failed; the others still proceed.
async fn fix_toolpack(manifest: &Manifest, bin_root: &Path, platform: Option<&str>) -> bool {
    let Some(platform) = platform else {
        output::warn("no toolpack build for this platform; nothing to fetch");
        return false;
    };
    let mut failed = false;
    for spec in &manifest.tools {
        let (status, _) = resolve_tool(bin_root, spec, Some(platform));
        if status == ToolStatus::Present || status == ToolStatus::Unsupported {
            continue;
        }
        let Some(pin) = spec.platforms.get(platform) else {
            continue;
        };
        output::progress(&format!("fetching {} {}", spec.name, spec.version));
        if let Err(error) = install_tool(
            &pin.url,
            &pin.sha256,
            bin_root,
            &spec.name,
            &spec.version,
            &member_name(&spec.name),
        )
        .await
        {
            output::eprintln_error(&error.to_string());
            failed = true;
        }
    }
    failed
}

fn build_report(manifest: &Manifest, bin_root: &Path, platform: Option<String>) -> DoctorReport {
    let identities: Vec<Identity> = cyber_config::auth_identities()
        .into_iter()
        .map(|(role, env)| {
            let set = std::env::var_os(&env).is_some_and(|value| !value.is_empty());
            Identity { role, env, set }
        })
        .collect();
    let bola_ready = identities.len() >= cyber_config::BOLA_MIN_IDENTITIES;
    let config = cyber_config::effective_values()
        .into_iter()
        .filter(|(key, _)| !key.starts_with("auth.") && !key.starts_with("context."))
        .collect();
    let context = cyber_config::context_items()
        .into_iter()
        .map(|(label, source)| {
            let resolves = context_resolves(&source);
            ContextItem {
                label,
                source,
                resolves,
            }
        })
        .collect();
    let (mode, mode_reason) = cyber_config::derived_mode();
    let mut floor: Vec<FloorTool> = FLOOR_TOOLS
        .iter()
        .map(|name| FloorTool {
            name: (*name).to_string(),
            present: on_path(name),
        })
        .collect();
    floor.push(FloorTool {
        name: "python3 >= 3.10".to_string(),
        present: discovery_python_ready(),
    });
    floor.push(FloorTool {
        name: "discovery runtime".to_string(),
        present: super::cyber_discovery::RUNTIME_BYTES.starts_with(b"PK"),
    });
    let toolpack = manifest
        .tools
        .iter()
        .map(|spec| {
            let (status, path) = resolve_tool(bin_root, spec, platform.as_deref());
            ToolReport {
                name: spec.name.clone(),
                version: spec.version.clone(),
                status,
                path: path.map(|path| path.display().to_string()),
            }
        })
        .collect();
    DoctorReport {
        platform,
        config_file: cyber_config::project_config_path(),
        config,
        mode: mode.label().to_string(),
        mode_reason: mode_reason.to_string(),
        identities,
        bola_ready,
        context,
        floor,
        toolpack,
    }
}

/// True when a context source is usable now: a URL is taken as declared (doctor
/// does not fetch it), a path must exist relative to the working directory.
fn context_resolves(source: &str) -> bool {
    if source.starts_with("http://") || source.starts_with("https://") {
        return true;
    }
    Path::new(source).exists()
}

fn render(report: &DoctorReport) -> Result<(), String> {
    let mut lines = Vec::new();
    lines.push(format!(
        "platform  {}",
        report.platform.as_deref().unwrap_or("unsupported")
    ));
    lines.push(format!("config    {}", report.config_file));
    for (key, value) in &report.config {
        lines.push(format!("config    {key} = {value}"));
    }
    lines.push(format!(
        "mode      {} ({})",
        report.mode, report.mode_reason
    ));
    if report.identities.is_empty() {
        lines.push("auth      none configured".to_string());
    }
    for identity in &report.identities {
        let state = if identity.set { "set" } else { "MISSING" };
        lines.push(format!(
            "auth      {}  {}  {state}",
            identity.role, identity.env
        ));
    }
    if !report.bola_ready {
        lines.push(format!(
            "authz     fewer than {} identities; an authorization test (BOLA) needs two roles",
            cyber_config::BOLA_MIN_IDENTITIES
        ));
    }
    if report.context.is_empty() {
        lines.push("context   none configured".to_string());
    }
    for item in &report.context {
        let state = if item.resolves { "ok" } else { "UNRESOLVED" };
        lines.push(format!(
            "context   {}  {}  {state}",
            item.label, item.source
        ));
    }
    for tool in &report.floor {
        let state = if tool.present { "present" } else { "ABSENT" };
        lines.push(format!("floor     {}  {state}", tool.name));
    }
    for tool in &report.toolpack {
        let hint = match tool.status {
            ToolStatus::Present => tool.path.clone().unwrap_or_default(),
            ToolStatus::Missing | ToolStatus::Outdated => "run `cyber doctor --fix`".to_string(),
            ToolStatus::Unsupported => "no build for this platform".to_string(),
        };
        lines.push(format!(
            "toolpack  {} {}  {}  {hint}",
            tool.name,
            tool.version,
            tool.status.label()
        ));
    }
    output::print_lines(&lines)
}

fn parse_manifest() -> Result<Manifest, String> {
    serde_json::from_str(MANIFEST_JSON)
        .map_err(|error| format!("toolpack manifest is invalid: {error}"))
}

/// The `{os}-{arch}` manifest key for the host, or `None` when no build exists.
fn current_platform() -> Option<String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return None,
    };
    Some(format!("{os}-{arch}"))
}

/// The binary's name inside the release archive and on disk. ProjectDiscovery
/// appends `.exe` on Windows only.
fn member_name(tool: &str) -> String {
    if cfg!(windows) {
        format!("{tool}.exe")
    } else {
        tool.to_string()
    }
}

fn resolve_tool(
    bin_root: &Path,
    spec: &ToolSpec,
    platform: Option<&str>,
) -> (ToolStatus, Option<PathBuf>) {
    let Some(platform) = platform else {
        return (ToolStatus::Unsupported, None);
    };
    if !spec.platforms.contains_key(platform) {
        return (ToolStatus::Unsupported, None);
    }
    let member = member_name(&spec.name);
    if let Some(path) = installed_tool_binary(bin_root, &spec.name, &spec.version, &member) {
        return (ToolStatus::Present, Some(path));
    }
    if has_other_version(bin_root, &spec.name, &spec.version, &member) {
        return (ToolStatus::Outdated, None);
    }
    (ToolStatus::Missing, None)
}

/// True when a version other than `version` is installed. It marks the pin as
/// outdated so `--fix` fetches the pinned one beside it.
pub(super) fn discovery_python_ready() -> bool {
    Command::new("python3")
        .args([
            "-I",
            "-c",
            "import sys; raise SystemExit(sys.version_info < (3, 10))",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub(super) fn installed_tool_directories() -> Result<Vec<PathBuf>, String> {
    let manifest = parse_manifest()?;
    let root = tools_bin_root().map_err(|error| error.to_string())?;
    let platform = current_platform();
    Ok(manifest
        .tools
        .iter()
        .filter_map(|spec| resolve_tool(&root, spec, platform.as_deref()).1)
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect())
}

fn has_other_version(bin_root: &Path, tool: &str, version: &str, member: &str) -> bool {
    let dir = bin_root.join(tool);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name() != *version)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .any(|entry| entry.path().join(member).is_file())
}

fn on_path(name: &str) -> bool {
    let executable = member_name(name);
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(&executable).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    #[test]
    fn the_shipped_manifest_parses() {
        let manifest = parse_manifest().unwrap();
        assert!(manifest.tools.iter().any(|tool| tool.name == "httpx"));
        assert!(manifest.tools.iter().any(|tool| tool.name == "katana"));
    }

    #[test]
    fn every_tool_pins_the_five_common_platforms() {
        let manifest = parse_manifest().unwrap();
        for tool in &manifest.tools {
            for platform in [
                "darwin-arm64",
                "darwin-amd64",
                "linux-amd64",
                "linux-arm64",
                "windows-amd64",
            ] {
                let pin = tool.platforms.get(platform).unwrap_or_else(|| {
                    panic!("{} has no pin for {platform}", tool.name);
                });
                assert_eq!(pin.sha256.len(), 64, "{} {platform} sha256", tool.name);
            }
        }
    }

    #[test]
    fn a_platform_with_no_pin_is_unsupported() {
        let manifest = parse_manifest().unwrap();
        let root = std::env::temp_dir();
        let (status, _) = resolve_tool(&root, &manifest.tools[0], Some("solaris-sparc"));
        assert_eq!(status, ToolStatus::Unsupported);
    }

    #[test]
    fn a_missing_platform_argument_is_unsupported() {
        let manifest = parse_manifest().unwrap();
        let root = std::env::temp_dir();
        let (status, _) = resolve_tool(&root, &manifest.tools[0], None);
        assert_eq!(status, ToolStatus::Unsupported);
    }

    #[test]
    fn an_absent_binary_is_missing() {
        let manifest = parse_manifest().unwrap();
        let root = unique_root();
        let (status, path) = resolve_tool(&root, &manifest.tools[0], Some("linux-amd64"));
        assert_eq!(status, ToolStatus::Missing);
        assert!(path.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_installed_pinned_binary_is_present() {
        let manifest = parse_manifest().unwrap();
        let spec = &manifest.tools[0];
        let root = unique_root();
        let member = member_name(&spec.name);
        let dir = root.join(&spec.name).join(&spec.version);
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join(&member);
        std::fs::write(&binary, b"binary").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let digest = format!("{:x}", sha2::Sha256::digest(b"binary"));
        std::fs::write(binary.with_file_name(format!("{member}.sha256")), digest).unwrap();
        let (status, path) = resolve_tool(&root, spec, Some("linux-amd64"));
        assert_eq!(status, ToolStatus::Present);
        assert!(path.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_installed_binary_without_verified_integrity_is_missing() {
        let manifest = parse_manifest().unwrap();
        let spec = &manifest.tools[0];
        let root = unique_root();
        let dir = root.join(&spec.name).join(&spec.version);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(member_name(&spec.name)), b"binary").unwrap();
        let (status, path) = resolve_tool(&root, spec, Some("linux-amd64"));
        assert_eq!(status, ToolStatus::Missing);
        assert!(path.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_different_installed_version_is_outdated() {
        let manifest = parse_manifest().unwrap();
        let spec = &manifest.tools[0];
        let root = unique_root();
        let member = member_name(&spec.name);
        let dir = root.join(&spec.name).join("0.0.1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(&member), b"binary").unwrap();
        let (status, _) = resolve_tool(&root, spec, Some("linux-amd64"));
        assert_eq!(status, ToolStatus::Outdated);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_url_context_source_resolves_without_a_fetch() {
        assert!(context_resolves("https://api.example.com/openapi.json"));
        assert!(context_resolves("http://localhost:8000/openapi.json"));
    }

    #[test]
    fn a_missing_context_path_does_not_resolve() {
        let root = unique_root();
        let absent = root.join("openapi.json");
        assert!(!context_resolves(&absent.display().to_string()));
        let present = root.join("present.json");
        std::fs::write(&present, b"{}").unwrap();
        assert!(context_resolves(&present.display().to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn unique_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("ct-doctor-{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }
}
