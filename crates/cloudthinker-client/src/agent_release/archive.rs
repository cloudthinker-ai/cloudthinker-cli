use std::fmt::Display;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{BUNDLE_DIR_NAME, create_dir, fs_error};
use crate::error::{CtError, CtResult};

pub(super) struct SidecarVerifiedArchive {
    bytes: Vec<u8>,
    asset: String,
}

pub(super) struct VerifiedArchive(Vec<u8>);

pub(super) fn verify_digest(
    archive: Vec<u8>,
    sidecar: &[u8],
    asset: &str,
) -> CtResult<SidecarVerifiedArchive> {
    verify_checksum(&archive, sidecar, asset)?;
    Ok(SidecarVerifiedArchive {
        bytes: archive,
        asset: asset.to_owned(),
    })
}

impl SidecarVerifiedArchive {
    pub(super) fn verify_inventory(self, inventory: &[u8]) -> CtResult<VerifiedArchive> {
        verify_inventory(&self.bytes, inventory, &self.asset)?;
        Ok(VerifiedArchive(self.bytes))
    }
}

pub(super) fn extract_bundle(archive: &VerifiedArchive, dest: &Path) -> CtResult<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive.0.as_slice()));
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

fn verify_checksum(archive: &[u8], sidecar: &[u8], asset: &str) -> CtResult<()> {
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
    verify_checksum(archive, line.as_bytes(), asset)
}
