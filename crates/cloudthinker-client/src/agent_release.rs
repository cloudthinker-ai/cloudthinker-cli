//! Download, verify, and install the released `cloudthinker-agent` bundle.

use std::fmt::Display;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use fs2::FileExt as _;
use sha2::{Digest, Sha256};

use crate::error::{CtError, CtResult};

/// GitHub release download root the published agent assets hang off.
pub const DEFAULT_RELEASE_BASE_URL: &str =
    "https://github.com/cloudthinker-ai/cloudthinker-cli/releases/download";

/// The archive's single top-level directory, and the installed directory name.
const BUNDLE_DIR_NAME: &str = "cloudthinker-agent";

/// The executable inside the bundle directory.
const BUNDLE_BINARY_NAME: &str = "cloudthinker-agent";

const CONNECT_TIMEOUT_SECS: u64 = 30;
const CHECKSUM_INVENTORY: &str = "cloudthinker-agent-sha256.sum";

/// Upper bound on one download, connect to last body byte.
const REQUEST_TIMEOUT_SECS: u64 = 600;

/// Largest release asset the installer buffers; a bigger response is refused.
const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;

/// The executable of a freshly installed bundle, plus each stale version the
/// install could not remove.
#[derive(Debug)]
pub struct InstalledAgent {
    pub binary: PathBuf,
    pub prune_failures: Vec<CtError>,
}

/// The release target triple for an `(OS, ARCH)` pair from `std::env::consts`.
pub fn target_triple(os: &str, arch: &str) -> CtResult<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        _ => Err(CtError::AgentInstall(format!(
            "no agent build for {os}/{arch}"
        ))),
    }
}

/// The release target triple of the host this process runs on.
pub fn host_target_triple() -> CtResult<&'static str> {
    target_triple(std::env::consts::OS, std::env::consts::ARCH)
}

/// The release asset name carrying the bundle for `triple`.
pub fn asset_name(triple: &str) -> String {
    format!("cloudthinker-agent-{triple}.tar.gz")
}

/// The unauthenticated download URL of one release asset.
pub fn asset_url(release_base: &str, version: &str, asset: &str) -> String {
    format!("{}/v{version}/{asset}", release_base.trim_end_matches('/'))
}

/// `~/.cloudthinker/agent/bin`, the root every installed version lives under.
pub fn agent_bin_root() -> CtResult<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| CtError::AgentInstall("could not resolve the home directory".to_string()))?;
    Ok(home.join(".cloudthinker").join("agent").join("bin"))
}

/// The bundle directory of one version under `bin_root`.
pub fn agent_install_dir(bin_root: &Path, version: &str) -> PathBuf {
    bin_root.join(version).join(BUNDLE_DIR_NAME)
}

/// The installed executable for `version`, when it is already on disk.
pub fn installed_agent_binary(bin_root: &Path, version: &str) -> Option<PathBuf> {
    let binary = agent_install_dir(bin_root, version).join(BUNDLE_BINARY_NAME);
    binary.is_file().then_some(binary)
}

pub fn any_agent_installed(bin_root: &Path) -> bool {
    std::fs::read_dir(bin_root).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            !entry.file_name().to_string_lossy().starts_with('.')
                && entry
                    .path()
                    .join(BUNDLE_DIR_NAME)
                    .join(BUNDLE_BINARY_NAME)
                    .is_file()
        })
    })
}

pub async fn install_agent(
    release_base: &str,
    version: &str,
    triple: &str,
    bin_root: &Path,
) -> CtResult<InstalledAgent> {
    let asset = asset_name(triple);
    let http = http_client()?;
    let archive = download(
        &http,
        &asset_url(release_base, version, &asset),
        MAX_ASSET_BYTES,
    )
    .await?;
    let sidecar = download(
        &http,
        &asset_url(release_base, version, &format!("{asset}.sha256")),
        MAX_ASSET_BYTES,
    )
    .await?;
    verify_digest(&archive, &sidecar, &asset)?;
    let inventory = download(
        &http,
        &asset_url(release_base, version, CHECKSUM_INVENTORY),
        64 * 1024,
    )
    .await?;
    verify_inventory(&archive, &inventory, &asset)?;

    let bin_root = bin_root.to_path_buf();
    let version = version.to_string();
    tokio::task::spawn_blocking(move || commit_bundle(&archive, &bin_root, &version))
        .await
        .map_err(|e| CtError::AgentInstall(format!("bundle install task failed: {e}")))?
}

fn commit_bundle(archive: &[u8], bin_root: &Path, version: &str) -> CtResult<InstalledAgent> {
    create_dir(bin_root)?;
    let lock_path = bin_root.join(".install.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| fs_error("open", &lock_path, &e))?;
    lock.lock_exclusive()
        .map_err(|e| fs_error("lock", &lock_path, &e))?;
    let staging = bin_root.join(format!(".staging-{:016x}", rand::random::<u64>()));
    let binary = match stage_bundle(archive, &staging, bin_root, version) {
        Ok(binary) => binary,
        Err(err) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
    };
    Ok(InstalledAgent {
        binary,
        prune_failures: prune_other_versions(bin_root, version),
    })
}

fn stage_bundle(
    archive: &[u8],
    staging: &Path,
    bin_root: &Path,
    version: &str,
) -> CtResult<PathBuf> {
    create_dir(staging)?;
    extract_bundle(archive, staging)?;
    if !staging.join(BUNDLE_BINARY_NAME).is_file() {
        return Err(CtError::AgentInstall(format!(
            "release archive carries no {BUNDLE_DIR_NAME}/{BUNDLE_BINARY_NAME}"
        )));
    }
    probe_bundle(staging, Duration::from_secs(10))?;
    let installed = agent_install_dir(bin_root, version);
    if let Some(version_dir) = installed.parent() {
        create_dir(version_dir)?;
    }
    if installed.exists() {
        if installed.join(BUNDLE_BINARY_NAME).is_file() {
            std::fs::remove_dir_all(staging).map_err(|e| fs_error("remove", staging, &e))?;
            return Ok(installed.join(BUNDLE_BINARY_NAME));
        }
        return Err(CtError::AgentInstall(format!(
            "incomplete existing bundle at {}",
            installed.display()
        )));
    }
    std::fs::rename(staging, &installed).map_err(|e| fs_error("install", &installed, &e))?;
    Ok(installed.join(BUNDLE_BINARY_NAME))
}

fn prune_other_versions(bin_root: &Path, version: &str) -> Vec<CtError> {
    prune_versions_with(bin_root, version, bundle_is_unused)
}

fn prune_versions_with(
    bin_root: &Path,
    version: &str,
    unused: impl Fn(&Path) -> bool,
) -> Vec<CtError> {
    let entries = match std::fs::read_dir(bin_root) {
        Ok(entries) => entries,
        Err(e) => return vec![fs_error("read", bin_root, &e)],
    };
    let mut versions: Vec<_> = entries
        .flatten()
        .filter(|entry| entry.file_name() != *version)
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| entry.path().join(BUNDLE_DIR_NAME).is_dir())
        .collect();
    versions
        .sort_by_key(|entry| std::cmp::Reverse(entry.metadata().and_then(|m| m.modified()).ok()));
    versions
        .into_iter()
        .skip(1)
        .filter(|entry| unused(&entry.path().join(BUNDLE_DIR_NAME)))
        .filter_map(|entry| {
            let path = entry.path();
            std::fs::remove_dir_all(&path)
                .err()
                .map(|e| fs_error("remove", &path, &e))
        })
        .collect()
}

fn wait_bounded(command: &mut Command, timeout: Duration) -> std::io::Result<Output> {
    let start = Instant::now();
    let mut child = loop {
        match command.spawn() {
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && start.elapsed() < timeout =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => break result?,
        }
    };
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output(),
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(result.err().unwrap_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "process probe timed out")
                }));
            }
        }
    }
}

fn probe_bundle(staging: &Path, timeout: Duration) -> CtResult<()> {
    let staging = std::fs::canonicalize(staging).map_err(|e| fs_error("resolve", staging, &e))?;
    for argument in ["--version", "--help"] {
        let mut command = Command::new(staging.join(BUNDLE_BINARY_NAME));
        command
            .arg(argument)
            .current_dir(&staging)
            .env_clear()
            .env("HOME", &staging)
            .env("PI_SKIP_VERSION_CHECK", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let status = wait_bounded(&mut command, timeout)
            .map_err(|e| CtError::AgentInstall(format!("staged agent {argument} failed: {e}")))?;
        if !status.status.success() {
            return Err(CtError::AgentInstall(format!(
                "staged agent {argument} exited with {}",
                status.status
            )));
        }
    }
    Ok(())
}

fn bundle_is_unused(bundle: &Path) -> bool {
    let Ok(probe) = std::fs::File::open(bundle.join(BUNDLE_BINARY_NAME)) else {
        return false;
    };
    if bundle_process_status(bundle) != Some(0) {
        return false;
    }
    drop(probe);
    bundle_process_status(bundle) == Some(1)
}

fn bundle_process_status(bundle: &Path) -> Option<i32> {
    let mut command = Command::new("lsof");
    command
        .args(["-F", "p"])
        .arg("+D")
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    wait_bounded(&mut command, Duration::from_secs(2))
        .ok()
        .filter(|output| output.stderr.is_empty())
        .and_then(|output| output.status.code())
}

/// Unpack the archive's `cloudthinker-agent/` directory into `dest`, rejecting
/// an entry that is not a plain file or directory inside it.
fn extract_bundle(archive: &[u8], dest: &Path) -> CtResult<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let entries = tar.entries().map_err(archive_read_error)?;
    for entry in entries {
        let mut entry = entry.map_err(archive_read_error)?;
        let path = entry.path().map_err(archive_read_error)?.into_owned();
        let relative = bundle_relative_path(&path).ok_or_else(|| {
            CtError::AgentInstall(format!(
                "release archive entry outside {BUNDLE_DIR_NAME}/: {}",
                path.display()
            ))
        })?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = dest.join(&relative);
        match entry.header().entry_type() {
            tar::EntryType::Directory => create_dir(&target)?,
            tar::EntryType::Regular => {
                if let Some(parent) = target.parent() {
                    create_dir(parent)?;
                }
                let mode = entry.header().mode().unwrap_or(0o644);
                write_file(&mut entry, &target, mode)?;
            }
            _ => {
                return Err(CtError::AgentInstall(format!(
                    "unsupported release archive entry: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn archive_read_error(e: impl Display) -> CtError {
    CtError::AgentInstall(format!("unreadable release archive: {e}"))
}

fn write_file(source: &mut impl Read, target: &Path, mode: u32) -> CtResult<()> {
    let mut file = std::fs::File::create(target).map_err(|e| fs_error("write", target, &e))?;
    std::io::copy(source, &mut file).map_err(|e| fs_error("write", target, &e))?;
    apply_mode(&file, mode).map_err(|e| fs_error("write", target, &e))
}

#[cfg(unix)]
fn apply_mode(file: &std::fs::File, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let bits = if mode & 0o111 == 0 { 0o644 } else { 0o755 };
    file.set_permissions(std::fs::Permissions::from_mode(bits))
}

#[cfg(not(unix))]
fn apply_mode(_file: &std::fs::File, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// The path under the bundle directory, or `None` when the entry sits outside it.
fn bundle_relative_path(path: &Path) -> Option<PathBuf> {
    let mut components = path.components().peekable();
    while components.peek() == Some(&Component::CurDir) {
        components.next();
    }
    match components.next() {
        Some(Component::Normal(name)) if name == BUNDLE_DIR_NAME => {}
        _ => return None,
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(relative)
}

/// Check the archive against its `sha256sum`-format sidecar, which must name
/// the asset it covers.
fn verify_digest(archive: &[u8], sidecar: &[u8], asset: &str) -> CtResult<()> {
    let text = String::from_utf8_lossy(sidecar);
    let mut fields = text.split_whitespace();
    let (Some(expected), Some(named)) = (fields.next(), fields.next()) else {
        return Err(CtError::AgentInstall(format!(
            "malformed checksum file for {asset}"
        )));
    };
    if named.trim_start_matches('*') != asset {
        return Err(CtError::AgentInstall(format!(
            "checksum file names {named}, not {asset}"
        )));
    }
    let actual = format!("{:x}", Sha256::digest(archive));
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(CtError::AgentInstall(format!(
            "checksum mismatch for {asset}: expected {expected}, got {actual}"
        )));
    }
    Ok(())
}

fn verify_inventory(archive: &[u8], inventory: &[u8], asset: &str) -> CtResult<()> {
    let text = std::str::from_utf8(inventory)
        .map_err(|_| CtError::AgentInstall("checksum inventory is not UTF-8".into()))?;
    let mut selected = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 2
            || fields[0].len() != 64
            || !fields[0].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(CtError::AgentInstall("malformed checksum inventory".into()));
        }
        if fields[1].trim_start_matches('*') == asset {
            if selected.is_some() {
                return Err(CtError::AgentInstall(format!(
                    "duplicate inventory entry for {asset}"
                )));
            }
            selected = Some(line);
        }
    }
    let line = selected.ok_or_else(|| {
        CtError::AgentInstall(format!("checksum inventory has no entry for {asset}"))
    })?;
    verify_digest(archive, line.as_bytes(), asset)
}

fn http_client() -> CtResult<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| CtError::Transport(format!("http client build: {e}")))
}

/// Fetch `url` into memory, refusing a body over `max_bytes` before buffering
/// when the server declares its length and as soon as the stream crosses it.
async fn download(http: &reqwest::Client, url: &str, max_bytes: u64) -> CtResult<Vec<u8>> {
    let mut response = http
        .get(url)
        .send()
        .await
        .map_err(|e| CtError::Transport(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(CtError::AgentInstall(format!(
            "{url} returned HTTP {}",
            status.as_u16()
        )));
    }
    if response.content_length().is_some_and(|len| len > max_bytes) {
        return Err(oversized_error(url, max_bytes));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| CtError::Transport(e.to_string()))?
    {
        if (body.len() + chunk.len()) as u64 > max_bytes {
            return Err(oversized_error(url, max_bytes));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn oversized_error(url: &str, max_bytes: u64) -> CtError {
    CtError::AgentInstall(format!("{url} is larger than the {max_bytes} byte limit"))
}

fn create_dir(path: &Path) -> CtResult<()> {
    std::fs::create_dir_all(path).map_err(|e| fs_error("create", path, &e))
}

fn fs_error(action: &str, path: &Path, error: &std::io::Error) -> CtError {
    CtError::AgentInstall(format!("could not {action} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn tar_gz(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data, mode) in entries {
            let mut header = tar::Header::new_gnu();
            if path.contains("..") {
                let gnu = header.as_gnu_mut().unwrap();
                gnu.name[..path.len()].copy_from_slice(path.as_bytes());
                header.set_size(data.len() as u64);
                header.set_mode(*mode);
                header.set_entry_type(tar::EntryType::Regular);
                header.set_cksum();
                builder.append(&header, *data).unwrap();
            } else {
                header.set_size(data.len() as u64);
                header.set_mode(*mode);
                header.set_entry_type(tar::EntryType::Regular);
                builder.append_data(&mut header, path, *data).unwrap();
            }
        }
        let tar = builder.into_inner().unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&tar).unwrap();
        encoder.finish().unwrap()
    }

    fn good_bundle() -> Vec<u8> {
        tar_gz(&[
            (
                "cloudthinker-agent/cloudthinker-agent",
                b"#!/bin/sh\nexit 0\n",
                0o755,
            ),
            ("cloudthinker-agent/package.json", b"{}", 0o644),
            ("cloudthinker-agent/theme/dark.json", b"{}", 0o644),
        ])
    }

    fn sidecar(archive: &[u8], asset: &str) -> Vec<u8> {
        format!("{:x}  {asset}\n", Sha256::digest(archive)).into_bytes()
    }

    #[cfg(unix)]
    #[test]
    fn any_agent_installed_needs_a_bundle_binary_in_a_version_folder() {
        let root = tempfile::tempdir().unwrap();
        assert!(!any_agent_installed(&root.path().join("missing")));
        assert!(!any_agent_installed(root.path()));
        std::fs::create_dir_all(root.path().join(".staging-1").join(BUNDLE_DIR_NAME)).unwrap();
        std::fs::write(
            root.path()
                .join(".staging-1")
                .join(BUNDLE_DIR_NAME)
                .join(BUNDLE_BINARY_NAME),
            b"binary",
        )
        .unwrap();
        std::fs::create_dir_all(agent_install_dir(root.path(), "0.5.0")).unwrap();
        assert!(!any_agent_installed(root.path()));
        std::fs::write(
            agent_install_dir(root.path(), "0.5.0").join(BUNDLE_BINARY_NAME),
            b"binary",
        )
        .unwrap();
        assert!(any_agent_installed(root.path()));
    }

    #[test]
    fn ca_ad_1_probe_cannot_inherit_parent_credentials() {
        if std::env::var_os("CT_PROBE_PARENT").is_none() {
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "agent_release::tests::ca_ad_1_probe_cannot_inherit_parent_credentials",
                ])
                .env("CT_PROBE_PARENT", "1")
                .env("CLOUDTHINKER_TOKEN", "synthetic-parent-token")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let archive = tar_gz(&[(
            "cloudthinker-agent/cloudthinker-agent",
            b"#!/bin/sh\n[ -z \"${CLOUDTHINKER_TOKEN+x}\" ] && [ -z \"${CT_PROBE_PARENT+x}\" ] && [ \"$HOME\" = \"$PWD\" ] && [ \"$PI_SKIP_VERSION_CHECK\" = 1 ]\n",
            0o755,
        )]);
        extract_bundle(&archive, root.path()).unwrap();
        probe_bundle(root.path(), Duration::from_secs(10)).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ca_ad_busy_probe_waits_for_writer_and_stays_bounded() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("probe");
        std::fs::write(&binary, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&binary)
            .unwrap();
        let result = wait_bounded(&mut Command::new(&binary), Duration::from_millis(30));
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::ExecutableFileBusy
        );
        std::thread::scope(|scope| {
            let probe =
                scope.spawn(|| wait_bounded(&mut Command::new(&binary), Duration::from_secs(1)));
            std::thread::sleep(Duration::from_millis(50));
            drop(writer);
            assert!(probe.join().unwrap().unwrap().status.success());
        });
    }

    #[cfg(unix)]
    #[test]
    fn ca_ad_1_failed_probe_preserves_existing_install() {
        for body in [b"#!/bin/sh\nexit 1\n".as_slice(), b"not executable"] {
            let root = tempfile::tempdir().unwrap();
            let installed = agent_install_dir(root.path(), "0.3.0");
            std::fs::create_dir_all(&installed).unwrap();
            std::fs::write(installed.join(BUNDLE_BINARY_NAME), b"previous").unwrap();
            let archive = tar_gz(&[("cloudthinker-agent/cloudthinker-agent", body, 0o755)]);
            let result = stage_bundle(
                &archive,
                &root.path().join(".staging-test"),
                root.path(),
                "0.3.0",
            );
            assert!(result.is_err());
            assert_eq!(
                std::fs::read(installed.join(BUNDLE_BINARY_NAME)).unwrap(),
                b"previous"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn ca_ad_1_timeout_kills_the_probe_and_install_failure_cleans_staging() {
        let root = tempfile::tempdir().unwrap();
        let bundle = tar_gz(&[(
            "cloudthinker-agent/cloudthinker-agent",
            b"#!/bin/sh\nwhile :; do :; done\n",
            0o755,
        )]);
        let staging = root.path().join("probe");
        extract_bundle(&bundle, &staging).unwrap();
        let started = Instant::now();
        assert!(probe_bundle(&staging, Duration::from_millis(30)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        let failed = tar_gz(&[(
            "cloudthinker-agent/cloudthinker-agent",
            b"#!/bin/sh\nexit 9\n",
            0o755,
        )]);
        assert!(commit_bundle(&failed, root.path(), "0.4.0").is_err());
        assert!(!root.path().join("0.4.0").exists());
        assert!(
            !std::fs::read_dir(root.path())
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().starts_with(".staging-"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn ca_ad_2_retains_previous_and_open_older_bundle() {
        let root = tempfile::tempdir().unwrap();
        commit_bundle(&good_bundle(), root.path(), "0.1.0").unwrap();
        let oldest = agent_install_dir(root.path(), "0.1.0");
        let open = std::fs::File::open(oldest.join(BUNDLE_BINARY_NAME)).unwrap();
        commit_bundle(&good_bundle(), root.path(), "0.2.0").unwrap();
        commit_bundle(&good_bundle(), root.path(), "0.3.0").unwrap();
        assert!(oldest.exists());
        assert!(agent_install_dir(root.path(), "0.2.0").exists());
        drop(open);
        if bundle_is_unused(&oldest) {
            assert!(prune_other_versions(root.path(), "0.3.0").is_empty());
            assert!(!oldest.exists());
        }
        assert!(agent_install_dir(root.path(), "0.2.0").exists());
    }

    #[cfg(unix)]
    #[test]
    fn ca_ad_2_pruning_keeps_two_versions_and_preserves_unknown_process_state() {
        let root = tempfile::tempdir().unwrap();
        for (index, version) in ["0.1.0", "0.2.0", "0.3.0"].iter().enumerate() {
            let bundle = agent_install_dir(root.path(), version);
            std::fs::create_dir_all(&bundle).unwrap();
            let parent = std::fs::File::open(bundle.parent().unwrap()).unwrap();
            parent
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(index as u64))
                .unwrap();
        }
        let oldest = root.path().join("0.1.0");
        assert!(prune_versions_with(root.path(), "0.3.0", |_| false).is_empty());
        assert!(oldest.exists());
        assert!(prune_versions_with(root.path(), "0.3.0", |_| true).is_empty());
        assert!(!oldest.exists());
        assert!(root.path().join("0.2.0").exists());
        assert!(root.path().join("0.3.0").exists());
    }

    #[test]
    fn target_triple_maps_every_released_host() {
        let cases = [
            ("macos", "aarch64", "aarch64-apple-darwin"),
            ("macos", "x86_64", "x86_64-apple-darwin"),
            ("linux", "aarch64", "aarch64-unknown-linux-gnu"),
            ("linux", "x86_64", "x86_64-unknown-linux-gnu"),
        ];
        for (os, arch, expected) in cases {
            assert_eq!(target_triple(os, arch).unwrap(), expected);
        }
    }

    #[test]
    fn target_triple_names_an_unreleased_host_in_one_line() {
        let err = target_triple("linux", "riscv64").unwrap_err();

        assert!(matches!(err, CtError::AgentInstall(_)));
        assert!(err.to_string().contains("no agent build for linux/riscv64"));
        assert!(target_triple("windows", "x86_64").is_err());
    }

    #[test]
    fn asset_name_and_url_follow_the_release_contract() {
        let asset = asset_name("aarch64-apple-darwin");

        assert_eq!(asset, "cloudthinker-agent-aarch64-apple-darwin.tar.gz");
        assert_eq!(
            asset_url(DEFAULT_RELEASE_BASE_URL, "0.2.1", &asset),
            "https://github.com/cloudthinker-ai/cloudthinker-cli/releases/download/v0.2.1/cloudthinker-agent-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            asset_url("http://localhost:9/", "0.2.1", &format!("{asset}.sha256")),
            "http://localhost:9/v0.2.1/cloudthinker-agent-aarch64-apple-darwin.tar.gz.sha256"
        );
    }

    #[test]
    fn install_dir_is_the_bundle_directory_under_its_version() {
        let root = Path::new("/home/dev/.cloudthinker/agent/bin");

        assert_eq!(
            agent_install_dir(root, "0.2.1"),
            Path::new("/home/dev/.cloudthinker/agent/bin/0.2.1/cloudthinker-agent")
        );
        assert_eq!(installed_agent_binary(root, "0.2.1"), None);
    }

    #[test]
    fn installed_binary_is_found_only_for_its_own_version() {
        let root = tempfile::tempdir().unwrap();
        let dir = agent_install_dir(root.path(), "0.2.1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cloudthinker-agent"), b"binary").unwrap();

        assert_eq!(
            installed_agent_binary(root.path(), "0.2.1"),
            Some(dir.join("cloudthinker-agent"))
        );
        assert_eq!(installed_agent_binary(root.path(), "0.2.2"), None);
    }

    #[test]
    fn digest_verification_accepts_the_matching_sidecar() {
        let archive = good_bundle();

        assert!(
            verify_digest(
                &archive,
                &sidecar(
                    &archive,
                    "cloudthinker-agent-x86_64-unknown-linux-gnu.tar.gz"
                ),
                "cloudthinker-agent-x86_64-unknown-linux-gnu.tar.gz",
            )
            .is_ok()
        );
    }

    #[test]
    fn ca_ad_3_inventory_requires_one_matching_well_formed_digest() {
        let archive = good_bundle();
        let asset = asset_name("x86_64-unknown-linux-gnu");
        let valid = sidecar(&archive, &asset);
        assert!(verify_inventory(&archive, &valid, &asset).is_ok());
        for invalid in [
            Vec::new(),
            b"malformed".to_vec(),
            [valid.clone(), valid.clone()].concat(),
            sidecar(b"other archive", &asset),
            sidecar(&archive, "other.tar.gz"),
        ] {
            assert!(verify_inventory(&archive, &invalid, &asset).is_err());
        }
    }

    #[tokio::test]
    async fn ca_ad_3_disagreeing_published_inventory_preserves_the_previous_bundle() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};
        let server = wiremock::MockServer::start().await;
        let archive = good_bundle();
        let asset = asset_name("x86_64-unknown-linux-gnu");
        for (name, bytes) in [
            (asset.clone(), archive.clone()),
            (format!("{asset}.sha256"), sidecar(&archive, &asset)),
            (
                CHECKSUM_INVENTORY.into(),
                sidecar(b"inconsistent upload", &asset),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/v0.3.0/{name}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
                .mount(&server)
                .await;
        }
        let root = tempfile::tempdir().unwrap();
        let previous = agent_install_dir(root.path(), "0.2.0");
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::write(previous.join(BUNDLE_BINARY_NAME), b"previous").unwrap();
        let error = install_agent(
            &server.uri(),
            "0.3.0",
            "x86_64-unknown-linux-gnu",
            root.path(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"));
        assert_eq!(
            std::fs::read(previous.join(BUNDLE_BINARY_NAME)).unwrap(),
            b"previous"
        );
        assert!(!root.path().join("0.3.0").exists());
    }

    #[test]
    fn digest_verification_rejects_a_tampered_archive_and_a_foreign_sidecar() {
        let archive = good_bundle();
        let asset = "cloudthinker-agent-x86_64-unknown-linux-gnu.tar.gz";
        let sidecar_for_asset = sidecar(&archive, asset);

        let mut tampered = archive.clone();
        tampered.push(0);
        let mismatch = verify_digest(&tampered, &sidecar_for_asset, asset).unwrap_err();
        assert!(mismatch.to_string().contains("checksum mismatch"));

        let foreign = verify_digest(
            &archive,
            &sidecar(&archive, "cloudthinker-agent-aarch64-apple-darwin.tar.gz"),
            asset,
        )
        .unwrap_err();
        assert!(foreign.to_string().contains("checksum file names"));

        let malformed = verify_digest(&archive, b"deadbeef\n", asset).unwrap_err();
        assert!(malformed.to_string().contains("malformed checksum file"));
    }

    #[test]
    fn extraction_strips_the_top_level_directory_and_keeps_the_executable_bit() {
        let dest = tempfile::tempdir().unwrap();

        extract_bundle(&good_bundle(), dest.path()).unwrap();

        let binary = dest.path().join("cloudthinker-agent");
        assert!(binary.is_file());
        assert!(dest.path().join("package.json").is_file());
        assert!(dest.path().join("theme/dark.json").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                binary.metadata().unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert_eq!(
                dest.path()
                    .join("package.json")
                    .metadata()
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o644
            );
        }
    }

    #[test]
    fn extraction_rejects_an_entry_that_escapes_the_bundle_directory() {
        let dest = tempfile::tempdir().unwrap();
        let archive = tar_gz(&[
            ("cloudthinker-agent/cloudthinker-agent", b"binary", 0o755),
            ("cloudthinker-agent/../escape.txt", b"owned", 0o644),
        ]);

        let err = extract_bundle(&archive, dest.path()).unwrap_err();

        assert!(err.to_string().contains("outside cloudthinker-agent/"));
        assert!(!dest.path().parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn extraction_rejects_an_entry_outside_the_bundle_directory() {
        let dest = tempfile::tempdir().unwrap();
        let archive = tar_gz(&[("elsewhere/cloudthinker-agent", b"binary", 0o755)]);

        assert!(extract_bundle(&archive, dest.path()).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn install_downloads_verifies_and_retains_the_previous_version() {
        let server = wiremock::MockServer::start().await;
        let archive = good_bundle();
        let asset = asset_name("x86_64-unknown-linux-gnu");
        mount_release(
            &server,
            "0.3.0",
            &asset,
            &archive,
            &sidecar(&archive, &asset),
        )
        .await;
        let root = tempfile::tempdir().unwrap();
        let stale = agent_install_dir(root.path(), "0.2.0");
        std::fs::create_dir_all(&stale).unwrap();

        let installed = install_agent(
            &server.uri(),
            "0.3.0",
            "x86_64-unknown-linux-gnu",
            root.path(),
        )
        .await
        .unwrap();

        assert_eq!(
            installed.binary,
            agent_install_dir(root.path(), "0.3.0").join("cloudthinker-agent")
        );
        assert_eq!(
            std::fs::read(&installed.binary).unwrap(),
            b"#!/bin/sh\nexit 0\n"
        );
        assert!(
            agent_install_dir(root.path(), "0.3.0")
                .join("theme/dark.json")
                .is_file()
        );
        assert!(root.path().join("0.2.0").exists());
        assert!(installed.prune_failures.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn ca_ad_2_incomplete_same_version_is_preserved_and_rejected() {
        let root = tempfile::tempdir().unwrap();
        let installed = agent_install_dir(root.path(), "0.3.0");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("partial"), b"preserve").unwrap();
        assert!(commit_bundle(&good_bundle(), root.path(), "0.3.0").is_err());
        assert_eq!(
            std::fs::read(installed.join("partial")).unwrap(),
            b"preserve"
        );
        assert!(!installed.join(BUNDLE_BINARY_NAME).exists());
    }

    #[cfg(unix)]
    #[test]
    fn staging_preserves_an_existing_install_of_the_same_version() {
        let root = tempfile::tempdir().unwrap();
        let installed = agent_install_dir(root.path(), "0.3.0");
        std::fs::create_dir_all(installed.join("theme")).unwrap();
        std::fs::write(installed.join("cloudthinker-agent"), b"stale binary").unwrap();
        std::fs::write(installed.join("theme/retired.json"), b"{}").unwrap();

        let binary = stage_bundle(
            &good_bundle(),
            &root.path().join(".staging-test"),
            root.path(),
            "0.3.0",
        )
        .unwrap();

        assert_eq!(binary, installed.join("cloudthinker-agent"));
        assert_eq!(std::fs::read(&binary).unwrap(), b"stale binary");
        assert!(installed.join("theme/retired.json").is_file());
        assert!(!root.path().join(".staging-test").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn install_leaves_unrelated_files_alone() {
        let server = wiremock::MockServer::start().await;
        let archive = good_bundle();
        let asset = asset_name("x86_64-unknown-linux-gnu");
        mount_release(
            &server,
            "0.3.0",
            &asset,
            &archive,
            &sidecar(&archive, &asset),
        )
        .await;
        let root = tempfile::tempdir().unwrap();
        let stray = root.path().join("stray");
        std::fs::write(&stray, b"not a version directory").unwrap();

        let installed = install_agent(
            &server.uri(),
            "0.3.0",
            "x86_64-unknown-linux-gnu",
            root.path(),
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read(&installed.binary).unwrap(),
            b"#!/bin/sh\nexit 0\n"
        );
        assert!(installed.prune_failures.is_empty());
        assert_eq!(std::fs::read(&stray).unwrap(), b"not a version directory");
    }

    #[tokio::test]
    async fn download_refuses_a_declared_body_over_the_limit() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/big"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 17]))
            .mount(&server)
            .await;
        let http = http_client().unwrap();
        let url = format!("{}/big", server.uri());

        let err = download(&http, &url, 16).await.unwrap_err();

        assert!(matches!(err, CtError::AgentInstall(_)));
        assert!(err.to_string().contains("larger than the 16 byte limit"));
        assert_eq!(download(&http, &url, 17).await.unwrap().len(), 17);
    }

    #[tokio::test]
    async fn download_refuses_a_streamed_body_once_it_crosses_the_limit() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/chunked", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            use std::io::Write;

            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request);
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                      10\r\n0123456789abcdef\r\n1\r\nx\r\n0\r\n\r\n",
                )
                .unwrap();
        });
        let http = http_client().unwrap();

        let err = download(&http, &url, 16).await.unwrap_err();

        assert!(err.to_string().contains("larger than the 16 byte limit"));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn install_rejects_a_digest_mismatch_and_leaves_nothing_behind() {
        let server = wiremock::MockServer::start().await;
        let archive = good_bundle();
        let asset = asset_name("x86_64-unknown-linux-gnu");
        let wrong = sidecar(b"someone else's bytes", &asset);
        mount_release(&server, "0.3.0", &asset, &archive, &wrong).await;
        let root = tempfile::tempdir().unwrap();

        let err = install_agent(
            &server.uri(),
            "0.3.0",
            "x86_64-unknown-linux-gnu",
            root.path(),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CtError::AgentInstall(_)));
        assert!(err.to_string().contains("checksum mismatch"));
        assert_eq!(installed_agent_binary(root.path(), "0.3.0"), None);
        assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn install_reports_a_missing_release_asset() {
        let server = wiremock::MockServer::start().await;
        let root = tempfile::tempdir().unwrap();

        let err = install_agent(
            &server.uri(),
            "9.9.9",
            "x86_64-unknown-linux-gnu",
            root.path(),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("HTTP 404"));
    }

    async fn mount_release(
        server: &wiremock::MockServer,
        version: &str,
        asset: &str,
        archive: &[u8],
        sidecar: &[u8],
    ) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        Mock::given(method("GET"))
            .and(path(format!("/v{version}/{asset}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(archive.to_vec()))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v{version}/{asset}.sha256")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(sidecar.to_vec()))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v{version}/{CHECKSUM_INVENTORY}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(sidecar.to_vec()))
            .mount(server)
            .await;
    }
}
