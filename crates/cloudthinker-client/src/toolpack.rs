//! Download, verify, and install one pinned third-party probe tool.
//!
//! Mirrors `agent_release`, but for a single-binary `.zip` release whose sha256
//! the caller pins in a manifest. The digest is not fetched from a sidecar: a
//! third-party release host is not the CloudThinker release channel, so the
//! trust anchor is the pin the CLI ships, verified before anything is written.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::error::{CtError, CtResult};

const CONNECT_TIMEOUT_SECS: u64 = 30;

/// Upper bound on one download, connect to last body byte.
const REQUEST_TIMEOUT_SECS: u64 = 600;

/// Largest release archive the installer buffers; a bigger response is refused.
const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;

/// Largest single extracted binary; guards against a zip that declares a huge
/// member. A real probe binary is tens of MB.
const MAX_MEMBER_BYTES: u64 = 256 * 1024 * 1024;

/// `~/.cloudthinker/tools`, the root every installed probe tool lives under.
pub fn tools_bin_root() -> CtResult<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| CtError::ToolInstall("could not resolve the home directory".to_string()))?;
    Ok(home.join(".cloudthinker").join("tools"))
}

/// The install directory of one pinned version: `<bin_root>/<tool>/<version>`.
pub fn tool_install_dir(bin_root: &Path, tool: &str, version: &str) -> PathBuf {
    bin_root.join(tool).join(version)
}

/// The installed binary for one pinned version, when it is already on disk.
pub fn installed_tool_binary(
    bin_root: &Path,
    tool: &str,
    version: &str,
    member: &str,
) -> Option<PathBuf> {
    let binary = tool_install_dir(bin_root, tool, version).join(member);
    let metadata = std::fs::symlink_metadata(&binary).ok()?;
    if !metadata.file_type().is_file() || !is_executable(&metadata) {
        return None;
    }
    let expected = read_digest(&binary)?;
    (file_sha256(&binary).ok()?.eq_ignore_ascii_case(&expected)).then_some(binary)
}

/// Download the pinned archive, verify its sha256, extract `member`, and place
/// it at `<bin_root>/<tool>/<version>/<member>`. Idempotent: an existing binary
/// at that path is returned without a re-download.
pub async fn install_tool(
    url: &str,
    sha256_hex: &str,
    bin_root: &Path,
    tool: &str,
    version: &str,
    member: &str,
) -> CtResult<PathBuf> {
    let dir = tool_install_dir(bin_root, tool, version);
    let installed = dir.join(member);
    if installed_tool_binary(bin_root, tool, version, member).is_some() {
        return Ok(installed);
    }
    remove_invalid_install(&installed)?;
    let http = http_client()?;
    let archive = download(&http, url, MAX_ASSET_BYTES).await?;
    verify_sha256(&archive, sha256_hex, tool)?;

    let member = member.to_string();
    tokio::task::spawn_blocking(move || commit_tool(&archive, &dir, &member))
        .await
        .map_err(|e| CtError::ToolInstall(format!("tool install task failed: {e}")))?
}

/// Extract `member` and rename it into place. A rename inside one directory is
/// atomic, so a crash never leaves a half-written binary on PATH.
fn commit_tool(archive: &[u8], dir: &Path, member: &str) -> CtResult<PathBuf> {
    create_dir(dir)?;
    let final_path = dir.join(member);
    if installed_binary_at(&final_path) {
        return Ok(final_path);
    }
    remove_invalid_install(&final_path)?;
    let bytes = extract_member(archive, member)?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let staging = dir.join(format!(".staging-{:016x}", rand::random::<u64>()));
    if let Err(err) = write_executable(&staging, &bytes) {
        let _ = std::fs::remove_file(&staging);
        return Err(err);
    }
    match std::fs::rename(&staging, &final_path) {
        Ok(()) => {
            write_digest(&final_path, &digest)?;
            Ok(final_path)
        }
        Err(err) => {
            let _ = std::fs::remove_file(&staging);
            if installed_binary_at(&final_path) {
                Ok(final_path)
            } else {
                Err(fs_error("install", &final_path, &err))
            }
        }
    }
}

fn installed_binary_at(binary: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(binary) else {
        return false;
    };
    if !metadata.file_type().is_file() || !is_executable(&metadata) {
        return false;
    }
    let Some(expected) = read_digest(binary) else {
        return false;
    };
    file_sha256(binary).is_ok_and(|actual| actual.eq_ignore_ascii_case(&expected))
}

fn digest_path(binary: &Path) -> PathBuf {
    let mut name = binary.file_name().unwrap_or_default().to_os_string();
    name.push(".sha256");
    binary.with_file_name(name)
}

fn read_digest(binary: &Path) -> Option<String> {
    let path = digest_path(binary);
    if std::fs::metadata(&path).ok()?.len() != 64 {
        return None;
    }
    let value = std::fs::read_to_string(path).ok()?;
    value
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit())
        .then_some(value)
}

fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn write_digest(binary: &Path, digest: &str) -> CtResult<()> {
    let path = digest_path(binary);
    let staging = path.with_file_name(format!(".digest-{:016x}", rand::random::<u64>()));
    std::fs::write(&staging, digest).map_err(|error| fs_error("write", &staging, &error))?;
    if let Err(error) = std::fs::rename(&staging, &path) {
        let _ = std::fs::remove_file(&staging);
        return Err(fs_error("install digest for", &path, &error));
    }
    Ok(())
}

fn remove_invalid_install(binary: &Path) -> CtResult<()> {
    for path in [binary.to_path_buf(), digest_path(binary)] {
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let result = if metadata.file_type().is_dir() {
            std::fs::remove_dir(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(error) = result {
            return Err(fs_error("remove invalid install", &path, &error));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

fn extract_member(archive: &[u8], member: &str) -> CtResult<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive))
        .map_err(|e| CtError::ToolInstall(format!("unreadable release archive: {e}")))?;
    let mut file = zip
        .by_name(member)
        .map_err(|e| CtError::ToolInstall(format!("archive has no member {member}: {e}")))?;
    if !file.is_file() {
        return Err(CtError::ToolInstall(format!(
            "archive member {member} is not a file"
        )));
    }
    if file.size() > MAX_MEMBER_BYTES {
        return Err(CtError::ToolInstall(format!(
            "archive member {member} is larger than the {MAX_MEMBER_BYTES} byte limit"
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| CtError::ToolInstall(format!("could not read {member}: {e}")))?;
    Ok(bytes)
}

fn write_executable(path: &Path, bytes: &[u8]) -> CtResult<()> {
    std::fs::write(path, bytes).map_err(|e| fs_error("write", path, &e))?;
    apply_exec_mode(path)
}

#[cfg(unix)]
fn apply_exec_mode(path: &Path) -> CtResult<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| fs_error("chmod", path, &e))
}

#[cfg(not(unix))]
fn apply_exec_mode(_path: &Path) -> CtResult<()> {
    Ok(())
}

fn verify_sha256(archive: &[u8], expected_hex: &str, tool: &str) -> CtResult<()> {
    let actual = format!("{:x}", Sha256::digest(archive));
    if !actual.eq_ignore_ascii_case(expected_hex.trim()) {
        return Err(CtError::ToolInstall(format!(
            "checksum mismatch for {tool}: expected {expected_hex}, got {actual}"
        )));
    }
    Ok(())
}

fn http_client() -> CtResult<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| CtError::Transport(format!("http client build: {e}")))
}

/// Fetch `url` into memory, refusing a body over `max_bytes` before buffering
/// when the server declares its length, and as soon as the stream crosses it.
async fn download(http: &reqwest::Client, url: &str, max_bytes: u64) -> CtResult<Vec<u8>> {
    let mut response = http
        .get(url)
        .send()
        .await
        .map_err(|e| CtError::Transport(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(CtError::ToolInstall(format!(
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
    CtError::ToolInstall(format!("{url} is larger than the {max_bytes} byte limit"))
}

fn create_dir(path: &Path) -> CtResult<()> {
    std::fs::create_dir_all(path).map_err(|e| fs_error("create", path, &e))
}

fn fs_error(action: &str, path: &Path, error: &std::io::Error) -> CtError {
    CtError::ToolInstall(format!("could not {action} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn zip_with(member: &str, data: &[u8]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        writer.start_file(member, options).unwrap();
        writer.write_all(data).unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn verify_sha256_accepts_the_matching_digest() {
        let archive = zip_with("httpx", b"binary");
        let digest = format!("{:x}", Sha256::digest(&archive));
        assert!(verify_sha256(&archive, &digest, "httpx").is_ok());
    }

    #[test]
    fn verify_sha256_rejects_a_tampered_archive() {
        let archive = zip_with("httpx", b"binary");
        let wrong = "0".repeat(64);
        let err = verify_sha256(&archive, &wrong, "httpx").unwrap_err();
        assert!(matches!(err, CtError::ToolInstall(_)));
    }

    #[test]
    fn extract_member_returns_the_named_binary_bytes() {
        let archive = zip_with("katana", b"ELF...payload");
        let bytes = extract_member(&archive, "katana").unwrap();
        assert_eq!(bytes, b"ELF...payload");
    }

    #[test]
    fn extract_member_fails_when_the_name_is_absent() {
        let archive = zip_with("katana", b"payload");
        let err = extract_member(&archive, "httpx").unwrap_err();
        assert!(matches!(err, CtError::ToolInstall(_)));
    }

    #[test]
    fn commit_tool_installs_the_member_and_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let dir = tool_install_dir(root.path(), "httpx", "1.6.10");
        let archive = zip_with("httpx", b"payload");
        let first = commit_tool(&archive, &dir, "httpx").unwrap();
        assert!(first.is_file());
        assert_eq!(std::fs::read(&first).unwrap(), b"payload");
        // A second call over an installed binary is a no-op that returns the path.
        let second = commit_tool(&archive, &dir, "httpx").unwrap();
        assert_eq!(first, second);
        assert_eq!(
            installed_tool_binary(root.path(), "httpx", "1.6.10", "httpx"),
            Some(first)
        );
    }

    #[test]
    fn installed_binary_integrity_failure_is_detected_and_repaired() {
        let root = tempfile::tempdir().unwrap();
        let dir = tool_install_dir(root.path(), "httpx", "1.6.10");
        let archive = zip_with("httpx", b"verified");
        let path = commit_tool(&archive, &dir, "httpx").unwrap();
        std::fs::write(&path, b"tampered").unwrap();
        assert_eq!(
            installed_tool_binary(root.path(), "httpx", "1.6.10", "httpx"),
            None
        );
        let repaired = commit_tool(&archive, &dir, "httpx").unwrap();
        assert_eq!(std::fs::read(&repaired).unwrap(), b"verified");
        assert_eq!(
            installed_tool_binary(root.path(), "httpx", "1.6.10", "httpx"),
            Some(repaired)
        );
    }

    #[cfg(unix)]
    #[test]
    fn installed_binary_without_execute_permission_is_not_ready() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let dir = tool_install_dir(root.path(), "httpx", "1.6.10");
        let path = commit_tool(&zip_with("httpx", b"payload"), &dir, "httpx").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            installed_tool_binary(root.path(), "httpx", "1.6.10", "httpx"),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn commit_tool_marks_the_binary_executable() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let dir = tool_install_dir(root.path(), "httpx", "1.6.10");
        let path = commit_tool(&zip_with("httpx", b"x"), &dir, "httpx").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111);
    }
}
