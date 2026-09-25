use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use cap_std::fs::{Dir, MetadataExt};
use cloudthinker_client::auth::worker_store::{private_directory, read_private, write_private};
use cloudthinker_client::{CtError, CtResult};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryIdentity {
    workdir_id: Uuid,
    device: u64,
    inode: u64,
}

pub struct WorkdirIdentity {
    pub installation_id: Uuid,
    pub workdir_id: Uuid,
    pub root: Dir,
    pub path: PathBuf,
    pub state: PathBuf,
    pub background_bytes: std::sync::atomic::AtomicU64,
    _lock: std::fs::File,
    _served_lock: std::fs::File,
}

impl Drop for WorkdirIdentity {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self._lock);
        let _ = FileExt::unlock(&self._served_lock);
    }
}

impl WorkdirIdentity {
    pub fn open(workdir: &Path, state_root: &Path, outpost_id: Uuid) -> CtResult<Self> {
        let path = workdir.canonicalize().map_err(|_| invalid_directory())?;
        let root = Dir::open_ambient_dir(&path, cap_std::ambient_authority())
            .map_err(|_| invalid_directory())?;
        let state_root = state_root.canonicalize().map_err(|_| state_error())?;
        if state_root.starts_with(&path) {
            return Err(CtError::Usage(
                "worker state must be outside the served directory".into(),
            ));
        }
        let state_dir = private_directory(&state_root)?;
        let lock = create_lock(&state_dir, "identity.lock")?;
        lock.lock_exclusive().map_err(|_| state_error())?;
        let registered = installation_id(&state_dir).and_then(|installation| {
            Ok((installation, directory_identity(&state_dir, &root, &path)?))
        });
        FileExt::unlock(&lock).map_err(|_| state_error())?;
        let (installation_id, identity) = registered?;
        let state = state_root
            .join(outpost_id.to_string())
            .join(identity.workdir_id.to_string());
        let served_lock = served_directory_lock(&state_root, &path, &state)?;
        let dir = private_directory(&state)?;
        let process_lock = create_lock(&dir, "worker.lock")?;
        process_lock
            .try_lock_exclusive()
            .map_err(|_| CtError::Usage("a worker already serves this outpost directory".into()))?;
        Ok(Self {
            installation_id,
            workdir_id: identity.workdir_id,
            root,
            path,
            state,
            background_bytes: std::sync::atomic::AtomicU64::new(0),
            _lock: process_lock,
            _served_lock: served_lock,
        })
    }

    pub fn revalidate(&self) -> CtResult<()> {
        let current = Dir::open_ambient_dir(&self.path, cap_std::ambient_authority())
            .map_err(|_| invalid_directory())?;
        let before = self.root.dir_metadata().map_err(|_| invalid_directory())?;
        let after = current.dir_metadata().map_err(|_| invalid_directory())?;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(invalid_directory());
        }
        Ok(())
    }
}

fn installation_id(state_dir: &Dir) -> CtResult<Uuid> {
    if state_dir
        .try_exists("installation.json")
        .map_err(|_| state_error())?
    {
        return serde_json::from_slice(&read_private(state_dir, "installation.json", 256)?)
            .map_err(|_| state_error());
    }
    let id = Uuid::new_v4();
    write_private(
        state_dir,
        "installation.json",
        &serde_json::to_vec(&id).map_err(|_| state_error())?,
    )?;
    Ok(id)
}

fn directory_identity(state_dir: &Dir, root: &Dir, path: &Path) -> CtResult<DirectoryIdentity> {
    let key = format!(
        "directory-{:x}.json",
        Sha256::digest(path.as_os_str().as_encoded_bytes())
    );
    let metadata = root.dir_metadata().map_err(|_| invalid_directory())?;
    if state_dir.try_exists(&key).map_err(|_| state_error())? {
        let saved: DirectoryIdentity =
            serde_json::from_slice(&read_private(state_dir, &key, 1024)?)
                .map_err(|_| state_error())?;
        if saved.device != metadata.dev() || saved.inode != metadata.ino() {
            return Err(CtError::Usage(
                "WORKDIR_IDENTITY_CHANGED: register a new directory association".into(),
            ));
        }
        return Ok(saved);
    }
    let saved = DirectoryIdentity {
        workdir_id: Uuid::new_v4(),
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    write_private(
        state_dir,
        &key,
        &serde_json::to_vec(&saved).map_err(|_| state_error())?,
    )?;
    Ok(saved)
}

fn served_directory_lock(
    state_root: &Path,
    served: &Path,
    state: &Path,
) -> CtResult<std::fs::File> {
    let parent = state_root.parent().ok_or_else(state_error)?;
    let dir = private_directory(&parent.join("served"))?;
    let name = format!(
        "{:x}.lock",
        Sha256::digest(served.as_os_str().as_encoded_bytes())
    );
    let mut lock = create_lock(&dir, &name)?;
    if lock.try_lock_exclusive().is_err() {
        let mut holder = String::new();
        let _ = lock.read_to_string(&mut holder);
        let holder = holder.trim();
        let message = if holder.is_empty() {
            format!("a worker already serves {}", served.display())
        } else {
            format!(
                "a worker already serves {} from state {holder}",
                served.display()
            )
        };
        return Err(CtError::Usage(message));
    }
    lock.set_len(0)
        .and_then(|()| lock.write_all(state.as_os_str().as_encoded_bytes()))
        .map_err(|_| state_error())?;
    Ok(lock)
}

fn create_lock(dir: &Dir, name: &str) -> CtResult<std::fs::File> {
    use cap_std::fs::{OpenOptions, OpenOptionsExt, PermissionsExt};
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    let file = dir.open_with(name, &options).map_err(|_| state_error())?;
    let metadata = file.metadata().map_err(|_| state_error())?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(state_error());
    }
    Ok(file.into_std())
}

fn invalid_directory() -> CtError {
    CtError::Usage("WORKDIR_IDENTITY_CHANGED: worker directory unavailable or replaced".into())
}

fn state_error() -> CtError {
    CtError::Store("worker identity could not be persisted securely".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_wo_27_restart_preserves_identity_and_28_replacement_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let work = temporary.path().join("project");
        std::fs::create_dir(&work).unwrap();
        let state = temporary.path().join("state");
        private_directory(&state).unwrap();
        let target = Uuid::new_v4();
        let first = WorkdirIdentity::open(&work, &state, target).unwrap();
        let expected = (first.installation_id, first.workdir_id);
        assert!(WorkdirIdentity::open(&work, &state, target).is_err());
        drop(first);
        let second = WorkdirIdentity::open(&work, &state, target).unwrap();
        assert_eq!((second.installation_id, second.workdir_id), expected);
        std::fs::rename(&work, temporary.path().join("old-project")).unwrap();
        std::fs::create_dir(&work).unwrap();
        assert!(second.revalidate().is_err());
        drop(second);
        assert!(WorkdirIdentity::open(&work, &state, target).is_err());
    }

    #[test]
    fn state_root_inside_the_served_directory_is_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let work = temporary.path().join("project");
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(work.join("state")).unwrap();
        for state in [work.join("state"), work.clone()] {
            assert!(matches!(
                WorkdirIdentity::open(&work, &state, Uuid::new_v4()),
                Err(CtError::Usage(message))
                    if message.contains("outside the served directory")
            ));
        }
        let outside = temporary.path().join("state");
        private_directory(&outside).unwrap();
        assert!(WorkdirIdentity::open(&work, &outside, Uuid::new_v4()).is_ok());
    }

    #[test]
    fn ca_bg_22_second_state_root_cannot_serve_the_same_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let work = temporary.path().join("project");
        let other_work = temporary.path().join("other");
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(&other_work).unwrap();
        let state = temporary.path().join("state-a");
        let other_state = temporary.path().join("state-b");
        private_directory(&state).unwrap();
        private_directory(&other_state).unwrap();
        let first = WorkdirIdentity::open(&work, &state, Uuid::new_v4()).unwrap();
        let refused = WorkdirIdentity::open(&work, &other_state, Uuid::new_v4());
        let served = work.canonicalize().unwrap();
        assert!(matches!(
            refused,
            Err(CtError::Usage(message))
                if message.contains(&served.display().to_string())
                    && message.contains(&first.state.display().to_string())
        ));
        assert!(WorkdirIdentity::open(&work, &state, Uuid::new_v4()).is_err());
        let second = WorkdirIdentity::open(&other_work, &other_state, Uuid::new_v4()).unwrap();
        drop(first);
        drop(second);
        assert!(WorkdirIdentity::open(&work, &other_state, Uuid::new_v4()).is_ok());
    }
}
