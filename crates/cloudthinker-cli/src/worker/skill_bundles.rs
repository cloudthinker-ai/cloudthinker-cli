use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use base64::Engine;
use cloudthinker_client::auth::worker_store::{private_directory, read_private, write_private};
use cloudthinker_client::worker_types as api;
use fs2::FileExt;
use serde::Deserialize;
use serde_json::{Value, json};

#[path = "skill_bundle_archive.rs"]
mod skill_bundle_archive;

pub const SKILL_BUNDLES_ENV: &str = "CLOUDTHINKER_SKILL_BUNDLES";
const KIND: &str = "skill_bundle_chunk";
const CHUNK_BYTES: usize = 128 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_FILES: u64 = 1024;
const MAX_CHUNKS: u64 = MAX_ARCHIVE_BYTES.div_ceil(CHUNK_BYTES as u64);
const MAX_STAGING_BUNDLES: usize = 8;
const MAX_CACHED_BUNDLES: usize = 64;
const STAGING_IDLE_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillBundleChunk {
    pub kind: String,
    pub digest: String,
    pub chunk_index: u64,
    pub chunk_count: u64,
    pub archive_size: u64,
    pub content_base64: String,
}

impl SkillBundleChunk {
    pub fn from_api(chunk: &api::SkillBundleChunk) -> Result<Self, &'static str> {
        let chunk = Self {
            kind: chunk.kind.to_string(),
            digest: String::from(chunk.digest.clone()),
            chunk_index: u64::try_from(chunk.chunk_index).map_err(|_| "BUNDLE_INVALID_CHUNK")?,
            chunk_count: chunk.chunk_count.get(),
            archive_size: chunk.archive_size.get(),
            content_base64: String::from(chunk.content_base64.clone()),
        };
        validate_chunk(&chunk)?;
        Ok(chunk)
    }
}

#[derive(Clone)]
pub struct SkillBundleStore {
    root: PathBuf,
}

impl SkillBundleStore {
    pub fn new(state: &Path) -> Self {
        Self {
            root: state.join("skill-bundles"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn install(&self, chunk: &SkillBundleChunk) -> Result<Value, &'static str> {
        validate_chunk(chunk)?;
        let _ = private_directory(&self.root).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        let _lock = lock_file(&self.root, &chunk.digest)?;
        let final_dir = self.root.join(&chunk.digest);
        if self.reuse_cached(&final_dir, &chunk.digest)? {
            return Ok(installed(&chunk.digest, true));
        }
        let staging_root = self.root.join(".staging");
        let _ = private_directory(&staging_root).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        let staging = staging_root.join(&chunk.digest);
        let Some((state, present)) = self.stage(chunk, &staging_root, &staging)? else {
            return Ok(installed(&chunk.digest, false));
        };
        let archive_path = self.assemble(&staging, &state, &present)?;
        self.publish(&archive_path, &staging, &final_dir, &state)?;
        Ok(installed(&chunk.digest, true))
    }

    fn reuse_cached(&self, final_dir: &Path, digest: &str) -> Result<bool, &'static str> {
        match fs::symlink_metadata(final_dir) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err("BUNDLE_CACHE_CORRUPT");
                }
                skill_bundle_archive::set_readonly_directory(final_dir)?;
                skill_bundle_archive::fsync_dir(final_dir)?;
                skill_bundle_archive::verify_bundle(final_dir, digest)?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err("BUNDLE_CACHE_UNAVAILABLE"),
        }
    }

    fn admit(&self, staging_root: &Path, staging: &Path) -> Result<(), &'static str> {
        if stage_exists(staging)? {
            return Ok(());
        }
        cleanup_stale_stages_at(&self.root, staging_root, SystemTime::now())?;
        let staging_count = staging_digests(staging_root)?.len();
        let cached_count = fs::read_dir(&self.root)
            .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?
            .try_fold(0usize, |count, entry| {
                let entry = entry.map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
                Ok::<usize, &'static str>(
                    count + usize::from(entry.file_name().to_str().is_some_and(is_digest)),
                )
            })?;
        if cached_count + staging_count >= MAX_CACHED_BUNDLES {
            return Err("BUNDLE_CACHE_FULL");
        }
        if staging_count >= MAX_STAGING_BUNDLES {
            return Err("BUNDLE_CACHE_BUSY");
        }
        Ok(())
    }

    fn stage(
        &self,
        chunk: &SkillBundleChunk,
        staging_root: &Path,
        staging: &Path,
    ) -> Result<Option<(skill_bundle_archive::BundleState, Vec<u64>)>, &'static str> {
        let _staging_lock = lock_staging(staging_root)?;
        self.admit(staging_root, staging)?;
        let staging_dir = private_directory(staging).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        let chunks = staging.join("chunks");
        let chunks_dir = private_directory(&chunks).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        let state = read_or_create_state(&staging_dir, staging, chunk)?;
        write_chunk(&chunks_dir, chunk)?;
        let state_bytes = serde_json::to_vec(&state).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
        write_private(&staging_dir, "state.json", &state_bytes)
            .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        let present = list_chunks(&chunks)?;
        if present.len() as u64 != state.chunk_count {
            return Ok(None);
        }
        if present
            .iter()
            .enumerate()
            .any(|(expected, actual)| *actual != expected as u64)
        {
            return Err("BUNDLE_CHUNKS_INCOMPLETE");
        }
        Ok(Some((state, present)))
    }

    fn assemble(
        &self,
        staging: &Path,
        state: &skill_bundle_archive::BundleState,
        present: &[u64],
    ) -> Result<PathBuf, &'static str> {
        let chunks_dir =
            private_directory(&staging.join("chunks")).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        let total = present.iter().try_fold(0u64, |total, index| {
            let bytes = read_private(&chunks_dir, &index.to_string(), CHUNK_BYTES as u64)
                .map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
            total
                .checked_add(bytes.len() as u64)
                .ok_or("BUNDLE_ARCHIVE_TOO_LARGE")
        })?;
        if total != state.archive_size {
            return Err("BUNDLE_ARCHIVE_SIZE_MISMATCH");
        }
        let tree = staging.join("tree");
        if fs::symlink_metadata(&tree).is_ok() {
            skill_bundle_archive::remove_staging_tree(&tree)?;
        }
        let archive_path = staging.join("archive.tar.gz");
        skill_bundle_archive::write_archive(&chunks_dir, present, &archive_path)?;
        if skill_bundle_archive::hash_file(&archive_path)? != state.digest {
            let _ = skill_bundle_archive::remove_staging_tree(staging);
            return Err("BUNDLE_DIGEST_MISMATCH");
        }
        Ok(archive_path)
    }

    fn publish(
        &self,
        archive_path: &Path,
        staging: &Path,
        final_dir: &Path,
        state: &skill_bundle_archive::BundleState,
    ) -> Result<(), &'static str> {
        let tree = staging.join("tree");
        let manifest = skill_bundle_archive::extract_archive(archive_path, &tree, state)?;
        let tree_archive = tree.join(".archive.tar.gz");
        fs::rename(archive_path, &tree_archive).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        skill_bundle_archive::set_readonly_file(&tree_archive)?;
        let marker = serde_json::to_vec(&manifest).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
        let tree_dir = private_directory(&tree).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        write_private(&tree_dir, ".verified", &marker).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        skill_bundle_archive::set_readonly_tree(&tree)?;
        skill_bundle_archive::fsync_dir(&tree)?;
        fs::rename(&tree, final_dir).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        skill_bundle_archive::set_readonly_directory(final_dir)?;
        skill_bundle_archive::fsync_dir(final_dir)?;
        skill_bundle_archive::fsync_dir(&self.root)?;
        skill_bundle_archive::remove_staging_tree(staging)?;
        skill_bundle_archive::verify_bundle(final_dir, &state.digest)
    }

    #[cfg(test)]
    pub fn verify_all(&self) -> Result<(), &'static str> {
        let metadata = match fs::symlink_metadata(&self.root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err("BUNDLE_CACHE_UNAVAILABLE"),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("BUNDLE_CACHE_CORRUPT");
        }
        let _ = private_directory(&self.root).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
        for entry in fs::read_dir(&self.root).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")? {
            let entry = entry.map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
            let name = entry.file_name();
            let Some(digest) = name.to_str() else {
                return Err("BUNDLE_CACHE_CORRUPT");
            };
            if digest == ".staging" || is_lock_name(digest) {
                continue;
            }
            if !is_digest(digest) {
                return Err("BUNDLE_CACHE_CORRUPT");
            }
            skill_bundle_archive::verify_bundle(&entry.path(), digest)?;
        }
        Ok(())
    }
}

fn installed(digest: &str, installed: bool) -> Value {
    json!({"digest": digest, "installed": installed})
}

fn read_or_create_state(
    staging_dir: &cap_std::fs::Dir,
    staging: &Path,
    chunk: &SkillBundleChunk,
) -> Result<skill_bundle_archive::BundleState, &'static str> {
    match fs::symlink_metadata(staging.join("state.json")) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err("BUNDLE_CACHE_CORRUPT");
            }
            let bytes = read_private(staging_dir, "state.json", 4096)
                .map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
            let state: skill_bundle_archive::BundleState =
                serde_json::from_slice(&bytes).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
            if state.digest != chunk.digest
                || state.archive_size != chunk.archive_size
                || state.chunk_count != chunk.chunk_count
            {
                return Err("BUNDLE_MANIFEST_MISMATCH");
            }
            Ok(state)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let state = skill_bundle_archive::BundleState {
                digest: chunk.digest.clone(),
                archive_size: chunk.archive_size,
                chunk_count: chunk.chunk_count,
            };
            let bytes = serde_json::to_vec(&state).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
            write_private(staging_dir, "state.json", &bytes)
                .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
            Ok(state)
        }
        Err(_) => Err("BUNDLE_CACHE_UNAVAILABLE"),
    }
}

fn write_chunk(
    chunks_dir: &cap_std::fs::Dir,
    chunk: &SkillBundleChunk,
) -> Result<(), &'static str> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&chunk.content_base64)
        .map_err(|_| "BUNDLE_INVALID_CHUNK")?;
    if bytes.is_empty() || bytes.len() > CHUNK_BYTES {
        return Err("BUNDLE_INVALID_CHUNK");
    }
    let expected_size = if chunk.chunk_index + 1 == chunk.chunk_count {
        chunk.archive_size - CHUNK_BYTES as u64 * (chunk.chunk_count - 1)
    } else {
        CHUNK_BYTES as u64
    };
    if bytes.len() as u64 != expected_size {
        return Err("BUNDLE_INVALID_CHUNK");
    }
    let chunk_name = chunk.chunk_index.to_string();
    if chunks_dir
        .try_exists(&chunk_name)
        .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?
    {
        let existing = read_private(chunks_dir, &chunk_name, CHUNK_BYTES as u64)
            .map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
        if existing != bytes {
            return Err("BUNDLE_CHUNK_MISMATCH");
        }
        return Ok(());
    }
    write_private(chunks_dir, &chunk_name, &bytes).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")
}

fn validate_chunk(chunk: &SkillBundleChunk) -> Result<(), &'static str> {
    if chunk.kind != KIND
        || !is_digest(&chunk.digest)
        || chunk.archive_size == 0
        || chunk.archive_size > MAX_ARCHIVE_BYTES
        || chunk.chunk_count == 0
        || chunk.chunk_count > MAX_CHUNKS
        || chunk.chunk_index >= chunk.chunk_count
    {
        return Err("BUNDLE_INVALID_CHUNK");
    }
    let expected = chunk.archive_size.div_ceil(CHUNK_BYTES as u64);
    if chunk.chunk_count != expected {
        return Err("BUNDLE_INVALID_CHUNK");
    }
    Ok(())
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn stage_exists(path: &Path) -> Result<bool, &'static str> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err("BUNDLE_CACHE_CORRUPT");
            }
            if !metadata.is_dir() {
                return Err("BUNDLE_CACHE_CORRUPT");
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("BUNDLE_CACHE_UNAVAILABLE"),
    }
}

fn staging_digests(staging_root: &Path) -> Result<Vec<String>, &'static str> {
    let mut digests = Vec::new();
    for entry in fs::read_dir(staging_root).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")? {
        let entry = entry.map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err("BUNDLE_CACHE_CORRUPT");
        };
        if !is_digest(name) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| "BUNDLE_CACHE_CORRUPT")?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("BUNDLE_CACHE_CORRUPT");
        }
        digests.push(name.to_owned());
    }
    Ok(digests)
}

fn stage_activity_modified(path: &Path) -> Result<Option<SystemTime>, &'static str> {
    let state = path.join("state.json");
    match fs::symlink_metadata(&state) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err("BUNDLE_CACHE_CORRUPT");
            }
            metadata
                .modified()
                .map(Some)
                .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let metadata = match fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => return Err("BUNDLE_CACHE_UNAVAILABLE"),
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err("BUNDLE_CACHE_CORRUPT");
            }
            metadata
                .modified()
                .map(Some)
                .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")
        }
        Err(_) => Err("BUNDLE_CACHE_UNAVAILABLE"),
    }
}

fn cleanup_stale_stages_at(
    root: &Path,
    staging_root: &Path,
    now: SystemTime,
) -> Result<(), &'static str> {
    for digest in staging_digests(staging_root)? {
        let staging = staging_root.join(&digest);
        let Some(modified) = stage_activity_modified(&staging)? else {
            continue;
        };
        if now
            .duration_since(modified)
            .map_or(true, |age| age < STAGING_IDLE_TTL)
        {
            continue;
        }
        let Some(_lock) = try_lock_file(root, &digest)? else {
            continue;
        };
        let Some(modified) = stage_activity_modified(&staging)? else {
            continue;
        };
        if now
            .duration_since(modified)
            .map_or(true, |age| age < STAGING_IDLE_TTL)
        {
            continue;
        }
        skill_bundle_archive::remove_staging_tree(&staging)?;
    }
    Ok(())
}

fn lock_staging(staging_root: &Path) -> Result<File, &'static str> {
    let file = open_lock(staging_root, ".staging.lock")?;
    file.lock_exclusive()
        .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    Ok(file)
}

fn lock_file(root: &Path, digest: &str) -> Result<File, &'static str> {
    let file = open_lock(root, &lock_name(digest))?;
    file.lock_exclusive()
        .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    Ok(file)
}

fn try_lock_file(root: &Path, digest: &str) -> Result<Option<File>, &'static str> {
    let file = open_lock(root, &lock_name(digest))?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(_) => Err("BUNDLE_CACHE_UNAVAILABLE"),
    }
}

fn lock_name(digest: &str) -> String {
    format!(".bundle-{}.lock", &digest[..2])
}

#[cfg(test)]
fn is_lock_name(value: &str) -> bool {
    let Some(prefix) = value
        .strip_prefix(".bundle-")
        .and_then(|value| value.strip_suffix(".lock"))
    else {
        return false;
    };
    prefix.len() == 2
        && prefix
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn open_lock(root: &Path, name: &str) -> Result<File, &'static str> {
    let path = root.join(name);
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        #[cfg(target_os = "linux")]
        const O_NOFOLLOW: i32 = 0o400000;
        #[cfg(target_vendor = "apple")]
        const O_NOFOLLOW: i32 = 0x100;
        #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
        const O_NOFOLLOW: i32 = 0;
        options.custom_flags(O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("BUNDLE_CACHE_CORRUPT");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err("BUNDLE_CACHE_CORRUPT");
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
    }
    Ok(file)
}

fn list_chunks(chunks: &Path) -> Result<Vec<u64>, &'static str> {
    let mut result = Vec::new();
    for entry in fs::read_dir(chunks).map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")? {
        let entry = entry.map_err(|_| "BUNDLE_CACHE_UNAVAILABLE")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err("BUNDLE_CACHE_CORRUPT");
        };
        let Ok(index) = name.parse::<u64>() else {
            return Err("BUNDLE_CACHE_CORRUPT");
        };
        if index >= MAX_CHUNKS
            || !entry
                .file_type()
                .map_err(|_| "BUNDLE_CACHE_CORRUPT")?
                .is_file()
        {
            return Err("BUNDLE_CACHE_CORRUPT");
        }
        result.push(index);
    }
    result.sort_unstable();
    Ok(result)
}

#[cfg(test)]
#[path = "skill_bundles_tests.rs"]
mod tests;
