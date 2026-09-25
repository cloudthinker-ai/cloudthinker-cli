use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path};

use cap_std::fs::Dir;
use cloudthinker_client::auth::worker_store::read_private;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Archive;

use super::{
    CHUNK_BYTES, MAX_ARCHIVE_BYTES, MAX_EXPANDED_BYTES, MAX_FILE_BYTES, MAX_FILES, is_digest,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BundleState {
    pub(super) digest: String,
    pub(super) archive_size: u64,
    pub(super) chunk_count: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BundleManifest {
    digest: String,
    archive_size: u64,
    files: Vec<BundleFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BundleFile {
    path: String,
    size: u64,
    digest: String,
}

pub(super) fn write_archive(
    chunks: &Dir,
    indexes: &[u64],
    path: &Path,
) -> Result<(), &'static str> {
    let temporary = path.with_file_name(".archive.tar.gz.tmp");
    for candidate in [&temporary, path] {
        match fs::symlink_metadata(candidate) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err("BUNDLE_CACHE_CORRUPT");
                }
                fs::remove_file(candidate).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("BUNDLE_CACHE_UNAVAILABLE"),
        }
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    }
    let result = (|| {
        for index in indexes {
            let bytes = read_private(chunks, &index.to_string(), CHUNK_BYTES as u64)
                .map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
            file.write_all(&bytes)
                .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        }
        file.sync_all().map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        fs::rename(&temporary, path).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn extract_archive(
    archive_path: &Path,
    tree: &Path,
    state: &BundleState,
) -> Result<BundleManifest, &'static str> {
    fs::create_dir(tree).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(tree, fs::Permissions::from_mode(0o700))
            .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    }
    let archive = File::open(archive_path).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
    let decoder = GzDecoder::new(archive);
    let mut archive = Archive::new(decoder);
    let mut paths = BTreeSet::new();
    let mut files = Vec::new();
    let mut expanded = 0u64;
    let entries = archive.entries().map_err(|_| "BUNDLE_ARCHIVE_INVALID")?;
    for entry in entries {
        let mut entry = entry.map_err(|_| "BUNDLE_ARCHIVE_INVALID")?;
        if paths.len() as u64 >= MAX_FILES {
            return Err("BUNDLE_ARCHIVE_TOO_MANY_FILES");
        }
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err("BUNDLE_ARCHIVE_UNSAFE_MEMBER");
        }
        let path = entry
            .path()
            .map_err(|_| "BUNDLE_ARCHIVE_UNSAFE_PATH")?
            .into_owned();
        validate_member_path(&path)?;
        let relative = path.to_str().ok_or("BUNDLE_ARCHIVE_UNSAFE_PATH")?;
        if matches!(relative, ".verified" | ".archive.tar.gz") {
            return Err("BUNDLE_ARCHIVE_UNSAFE_PATH");
        }
        if !paths.insert(relative.to_owned()) {
            return Err("BUNDLE_ARCHIVE_DUPLICATE_PATH");
        }
        let destination = tree.join(&path);
        if entry_type.is_dir() {
            fs::create_dir_all(&destination).map_err(|_| "BUNDLE_ARCHIVE_INVALID")?;
            continue;
        }
        let size = entry.size();
        if size > MAX_FILE_BYTES || expanded.saturating_add(size) > MAX_EXPANDED_BYTES {
            return Err("BUNDLE_ARCHIVE_TOO_LARGE");
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|_| "BUNDLE_ARCHIVE_INVALID")?;
        }
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&destination)
            .map_err(|_| "BUNDLE_ARCHIVE_INVALID")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))
                .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        }
        let mut hasher = Sha256::new();
        let mut limited = (&mut entry).take(size.saturating_add(1));
        let mut buffer = [0u8; 64 * 1024];
        let mut actual = 0u64;
        loop {
            let count = limited
                .read(&mut buffer)
                .map_err(|_| "BUNDLE_ARCHIVE_INVALID")?;
            if count == 0 {
                break;
            }
            actual = actual.saturating_add(count as u64);
            if actual > size {
                return Err("BUNDLE_ARCHIVE_INVALID");
            }
            output
                .write_all(&buffer[..count])
                .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
            hasher.update(&buffer[..count]);
        }
        if actual != size {
            return Err("BUNDLE_ARCHIVE_INVALID");
        }
        output.sync_all().map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        expanded = expanded.saturating_add(size);
        files.push(BundleFile {
            path: relative.to_owned(),
            size,
            digest: format!("{:x}", hasher.finalize()),
        });
    }
    if !paths.contains("SKILL.md") {
        return Err("BUNDLE_SKILL_MD_MISSING");
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(BundleManifest {
        digest: state.digest.clone(),
        archive_size: state.archive_size,
        files,
    })
}

fn validate_member_path(path: &Path) -> Result<(), &'static str> {
    let text = path.to_str().ok_or("BUNDLE_ARCHIVE_UNSAFE_PATH")?;
    if text.is_empty()
        || text.contains('\\')
        || text.contains('\0')
        || text.contains("//")
        || text.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
    {
        return Err("BUNDLE_ARCHIVE_UNSAFE_PATH");
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err("BUNDLE_ARCHIVE_UNSAFE_PATH");
        }
    }
    Ok(())
}

pub(super) fn verify_bundle(path: &Path, expected_digest: &str) -> Result<(), &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("BUNDLE_CACHE_CORRUPT");
    }
    validate_readonly_metadata(&metadata, true)?;
    let marker = read_bundle_file(path, ".verified", 256 * 1024)?;
    let manifest: BundleManifest =
        serde_json::from_slice(&marker).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
    if manifest.digest != expected_digest
        || !is_digest(&manifest.digest)
        || manifest.archive_size == 0
        || manifest.archive_size > MAX_ARCHIVE_BYTES
        || manifest.files.len() as u64 > MAX_FILES
    {
        return Err("BUNDLE_DIGEST_MISMATCH");
    }
    let mut expanded = 0u64;
    let archive = path.join(".archive.tar.gz");
    let archive_meta = fs::symlink_metadata(&archive).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
    if archive_meta.file_type().is_symlink()
        || !archive_meta.is_file()
        || archive_meta.len() != manifest.archive_size
        || validate_readonly_metadata(&archive_meta, false).is_err()
        || hash_file(&archive)? != expected_digest
    {
        return Err("BUNDLE_DIGEST_MISMATCH");
    }
    let mut expected = BTreeSet::new();
    for file in &manifest.files {
        validate_member_path(Path::new(&file.path))?;
        if file.size > MAX_FILE_BYTES {
            return Err("BUNDLE_CACHE_CORRUPT");
        }
        expanded = expanded
            .checked_add(file.size)
            .ok_or("BUNDLE_CACHE_CORRUPT")?;
        if expanded > MAX_EXPANDED_BYTES || !is_digest(&file.digest) {
            return Err("BUNDLE_CACHE_CORRUPT");
        }
        if !expected.insert(file.path.clone()) {
            return Err("BUNDLE_CACHE_CORRUPT");
        }
        let file_path = path.join(&file.path);
        let metadata = fs::symlink_metadata(&file_path).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() != file.size
            || validate_readonly_metadata(&metadata, false).is_err()
            || hash_file(&file_path)? != file.digest
        {
            return Err("BUNDLE_CACHE_TAMPERED");
        }
    }
    if !expected.contains("SKILL.md") {
        return Err("BUNDLE_SKILL_MD_MISSING");
    }
    verify_tree_shape(path, path, &expected)?;
    Ok(())
}

fn verify_tree_shape(
    root: &Path,
    path: &Path,
    expected: &BTreeSet<String>,
) -> Result<(), &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
    validate_readonly_metadata(&metadata, true)?;
    for entry in fs::read_dir(path).map_err(|_| "BUNDLE_CACHE_CORRUPT")? {
        let entry = entry.map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
        let name = entry.file_name();
        let name = name.to_str().ok_or("BUNDLE_CACHE_CORRUPT")?;
        if matches!(name, ".verified" | ".archive.tar.gz") {
            continue;
        }
        let entry_path = entry.path();
        let relative = entry_path
            .strip_prefix(root)
            .map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
        let file_type = entry.file_type().map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
        if file_type.is_symlink() {
            return Err("BUNDLE_CACHE_TAMPERED");
        }
        if file_type.is_file() {
            validate_readonly_metadata(
                &entry.metadata().map_err(|_| "BUNDLE_CACHE_CORRUPT")?,
                false,
            )?;
            let relative = relative.to_str().ok_or("BUNDLE_CACHE_CORRUPT")?;
            if !expected.contains(relative) {
                return Err("BUNDLE_CACHE_TAMPERED");
            }
        } else if file_type.is_dir() {
            let prefix = if relative.as_os_str().is_empty() {
                String::new()
            } else {
                format!("{}/", relative.to_str().ok_or("BUNDLE_CACHE_CORRUPT")?)
            };
            if !expected.iter().any(|file| file.starts_with(&prefix)) {
                return Err("BUNDLE_CACHE_TAMPERED");
            }
            verify_tree_shape(root, &entry.path(), expected)?;
        } else {
            return Err("BUNDLE_CACHE_TAMPERED");
        }
    }
    Ok(())
}

fn read_bundle_file(path: &Path, name: &str, limit: u64) -> Result<Vec<u8>, &'static str> {
    let file_path = path.join(name);
    let metadata = fs::symlink_metadata(&file_path).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("BUNDLE_CACHE_CORRUPT");
    }
    validate_readonly_metadata(&metadata, false)?;
    let file = File::open(file_path).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
    if bytes.len() as u64 > limit {
        return Err("BUNDLE_CACHE_CORRUPT");
    }
    Ok(bytes)
}

fn validate_readonly_metadata(
    metadata: &fs::Metadata,
    directory: bool,
) -> Result<(), &'static str> {
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err("BUNDLE_CACHE_CORRUPT");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.permissions().mode() & 0o222 != 0
            || (!directory && metadata.nlink() != 1)
        {
            return Err("BUNDLE_CACHE_TAMPERED");
        }
    }
    #[cfg(not(unix))]
    if !metadata.permissions().readonly() {
        return Err("BUNDLE_CACHE_TAMPERED");
    }
    Ok(())
}

pub(super) fn hash_file(path: &Path) -> Result<String, &'static str> {
    let mut file = File::open(path).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn set_readonly_file(path: &Path) -> Result<(), &'static str> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o444))
            .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)
            .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    }
    Ok(())
}

pub(super) fn set_readonly_tree(path: &Path) -> Result<(), &'static str> {
    for entry in fs::read_dir(path).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")? {
        let entry = entry.map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        let file_type = entry.file_type().map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        if file_type.is_dir() {
            set_readonly_tree(&entry.path())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o555))
                    .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
            }
        } else if file_type.is_file() {
            set_readonly_file(&entry.path())?;
        } else {
            return Err("BUNDLE_CACHE_TAMPERED");
        }
    }
    Ok(())
}

pub(super) fn set_readonly_directory(path: &Path) -> Result<(), &'static str> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o555))
            .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)
            .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    }
    Ok(())
}

pub(super) fn remove_staging_tree(path: &Path) -> Result<(), &'static str> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("BUNDLE_CACHE_UNAVAILABLE"),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("BUNDLE_CACHE_CORRUPT");
    }
    set_staging_directory_writable(path)?;
    for entry in fs::read_dir(path).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")? {
        let entry = entry.map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        let child = entry.path();
        let metadata = fs::symlink_metadata(&child).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
        if metadata.file_type().is_symlink() {
            return Err("BUNDLE_CACHE_CORRUPT");
        }
        if metadata.is_dir() {
            remove_staging_tree(&child)?;
        } else if metadata.is_file() {
            set_staging_file_writable(&child)?;
            fs::remove_file(&child).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        } else {
            return Err("BUNDLE_CACHE_CORRUPT");
        }
    }
    fs::remove_dir(path).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")
}

fn set_staging_directory_writable(path: &Path) -> Result<(), &'static str> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)
            .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?
            .permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    }
    Ok(())
}

fn set_staging_file_writable(path: &Path) -> Result<(), &'static str> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)
            .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?
            .permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    }
    Ok(())
}

pub(super) fn fsync_dir(path: &Path) -> Result<(), &'static str> {
    File::open(path)
        .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?
        .sync_all()
        .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")
}
