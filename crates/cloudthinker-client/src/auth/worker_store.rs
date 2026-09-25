use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{CtError, CtResult, origin_of};

pub const WORKER_TOKEN_ENV: &str = "CLOUDTHINKER_WORKER_TOKEN";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerCredential {
    pub target_id: Uuid,
    pub name: String,
    pub token: String,
}

pub struct WorkerStore {
    dir: Dir,
    state_root: PathBuf,
}

impl WorkerStore {
    pub fn open_default(base_url: &str) -> CtResult<Self> {
        let root = dirs::config_dir()
            .ok_or_else(|| CtError::Store("worker configuration directory unavailable".into()))?;
        Self::open(&root.join("cloudthinker/worker-state"), base_url)
    }

    pub fn open(root: &Path, base_url: &str) -> CtResult<Self> {
        let origin = origin_of(base_url)?;
        let digest = format!("{:x}", Sha256::digest(origin.as_bytes()));
        let state_root = root.join(digest);
        let dir = private_directory(&state_root)?;
        Ok(Self { dir, state_root })
    }

    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    pub fn save(&self, credential: &WorkerCredential) -> CtResult<()> {
        let bytes = serde_json::to_vec(credential).map_err(|_| store_error())?;
        write_private(&self.dir, &format!("{}.json", credential.target_id), &bytes)
    }

    pub fn load(&self, selector: &str) -> CtResult<WorkerCredential> {
        if let Ok(id) = Uuid::parse_str(selector) {
            return self.read(id);
        }
        let mut matched = None;
        for entry in self.dir.entries().map_err(|_| store_error())? {
            let entry = entry.map_err(|_| store_error())?;
            let name = entry.file_name();
            let Some(id) = name
                .to_str()
                .and_then(|s| s.strip_suffix(".json"))
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            let candidate = self.read(id)?;
            if candidate.name == selector {
                if matched.is_some() {
                    return Err(CtError::Usage(
                        "outpost name is ambiguous; use its id".into(),
                    ));
                }
                matched = Some(candidate);
            }
        }
        matched
            .ok_or_else(|| CtError::Auth("worker credential missing; register this outpost".into()))
    }

    fn read(&self, id: Uuid) -> CtResult<WorkerCredential> {
        let bytes = read_private(&self.dir, &format!("{id}.json"), 65536)?;
        let credential: WorkerCredential =
            serde_json::from_slice(&bytes).map_err(|_| store_error())?;
        if credential.target_id != id || credential.token.trim().is_empty() {
            return Err(store_error());
        }
        Ok(credential)
    }
}

pub fn private_directory(path: &Path) -> CtResult<Dir> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|_| store_error())?;
        let metadata = std::fs::symlink_metadata(path).map_err(|_| store_error())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(store_error());
        }
        let dir =
            Dir::open_ambient_dir(path, cap_std::ambient_authority()).map_err(|_| store_error())?;
        validate_metadata(&dir.dir_metadata().map_err(|_| store_error())?, true)?;
        Ok(dir)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(CtError::Usage("worker requires Linux or macOS".into()))
    }
}

pub fn read_private(dir: &Dir, name: &str, limit: u64) -> CtResult<Vec<u8>> {
    if dir
        .symlink_metadata(name)
        .map_err(|_| store_error())?
        .file_type()
        .is_symlink()
    {
        return Err(store_error());
    }
    let file = dir.open(name).map_err(|_| store_error())?;
    validate_metadata(&file.metadata().map_err(|_| store_error())?, false)?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| store_error())?;
    if bytes.len() as u64 > limit {
        return Err(store_error());
    }
    Ok(bytes)
}

pub fn write_private(dir: &Dir, name: &str, bytes: &[u8]) -> CtResult<()> {
    let temporary = format!(".{}.tmp", Uuid::new_v4());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = dir
        .open_with(&temporary, &options)
        .map_err(|_| store_error())?;
    let outcome = (|| {
        file.write_all(bytes).map_err(|_| store_error())?;
        file.sync_all().map_err(|_| store_error())?;
        dir.rename(&temporary, dir, name)
            .map_err(|_| store_error())?;
        dir.open(".")
            .map_err(|_| store_error())?
            .sync_all()
            .map_err(|_| store_error())
    })();
    if outcome.is_err() {
        let _ = dir.remove_file(&temporary);
    }
    outcome
}

fn validate_metadata(metadata: &cap_std::fs::Metadata, directory: bool) -> CtResult<()> {
    #[cfg(unix)]
    {
        use cap_std::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.permissions().mode() & 0o077 != 0
            || (directory && !metadata.is_dir())
            || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
        {
            return Err(store_error());
        }
    }
    Ok(())
}

fn store_error() -> CtError {
    CtError::Store("worker state could not be accessed securely".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_wo_31_store_is_private_and_origin_scoped() {
        let root = tempfile::tempdir().unwrap();
        let first = WorkerStore::open(&root.path().join("workers"), "https://one.example").unwrap();
        let id = Uuid::new_v4();
        first
            .save(&WorkerCredential {
                target_id: id,
                name: "build".into(),
                token: "private-token".into(),
            })
            .unwrap();
        assert_eq!(first.load("build").unwrap().target_id, id);
        assert_eq!(first.load(&id.to_string()).unwrap().token, "private-token");
        let other = WorkerStore::open(&root.path().join("workers"), "https://two.example").unwrap();
        assert!(other.load("build").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(first.state_root.join(format!("{id}.json")))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn ca_wo_31_a_name_two_outposts_share_must_be_resolved_by_id() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkerStore::open(&root.path().join("workers"), "https://one.example").unwrap();
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        for id in [first, second] {
            store
                .save(&WorkerCredential {
                    target_id: id,
                    name: "build".into(),
                    token: format!("token-{id}"),
                })
                .unwrap();
        }

        let Err(error) = store.load("build") else {
            panic!("the name matches two entries");
        };
        assert!(
            matches!(&error, CtError::Usage(message) if message.contains("ambiguous")),
            "got {error:?}"
        );
        assert_eq!(store.load(&first.to_string()).unwrap().target_id, first);
    }

    #[test]
    fn ca_wo_31_read_private_refuses_a_file_over_its_limit() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkerStore::open(&root.path().join("workers"), "https://one.example").unwrap();
        write_private(&store.dir, "big.json", &[b'x'; 64]).unwrap();

        assert_eq!(read_private(&store.dir, "big.json", 64).unwrap().len(), 64);
        assert!(read_private(&store.dir, "big.json", 63).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn ca_wo_31_reject_symlink_and_public_credentials() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let store = WorkerStore::open(&root.path().join("workers"), "https://one.example").unwrap();
        let id = Uuid::new_v4();
        let path = store.state_root.join(format!("{id}.json"));
        symlink(root.path().join("outside"), &path).unwrap();
        assert!(store.load(&id.to_string()).is_err());
        std::fs::remove_file(&path).unwrap();
        store
            .save(&WorkerCredential {
                target_id: id,
                name: "build".into(),
                token: "private".into(),
            })
            .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.load(&id.to_string()).is_err());
    }
}
