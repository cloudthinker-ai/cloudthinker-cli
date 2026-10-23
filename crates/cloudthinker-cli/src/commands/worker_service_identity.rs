use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cloudthinker_client::{CtError, CtResult};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::worker_service_descriptor::validate_unix_user;
use super::{ServicePlatform, system_program};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SystemUser {
    pub(super) name: String,
    pub(super) uid: u32,
    pub(super) home: PathBuf,
}

pub(super) fn lookup_user(name: &str) -> CtResult<SystemUser> {
    validate_unix_user(name)?;
    let platform = ServicePlatform::current()?;
    let record = match platform {
        ServicePlatform::Systemd => Command::new(system_program("getent"))
            .args(["passwd", name])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| parse_passwd(&String::from_utf8_lossy(&output.stdout), name)),
        ServicePlatform::Launchd => Command::new(system_program("dscl"))
            .args([
                ".",
                "-read",
                &format!("/Users/{name}"),
                "UniqueID",
                "NFSHomeDirectory",
            ])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| parse_dscl(&String::from_utf8_lossy(&output.stdout), name)),
    };
    record.ok_or_else(|| CtError::Usage(format!("--user {name} is not an account on this machine")))
}

pub(super) fn parse_passwd(text: &str, name: &str) -> Option<SystemUser> {
    let mut fields = text.lines().next()?.split(':');
    if fields.next()? != name {
        return None;
    }
    let uid = fields.nth(1)?.parse().ok()?;
    let home = PathBuf::from(fields.nth(2)?);
    home.is_absolute().then(|| SystemUser {
        name: name.to_owned(),
        uid,
        home,
    })
}

pub(super) fn parse_dscl(text: &str, name: &str) -> Option<SystemUser> {
    let mut uid = None;
    let mut home = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("UniqueID:") {
            uid = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("NFSHomeDirectory:") {
            home = Some(PathBuf::from(value.trim()));
        }
    }
    let home = home.filter(|path| path.is_absolute())?;
    Some(SystemUser {
        name: name.to_owned(),
        uid: uid?,
        home,
    })
}

fn system_state_root(user: &SystemUser, origin: &str) -> PathBuf {
    let config = if cfg!(target_os = "macos") {
        user.home.join("Library/Application Support")
    } else {
        user.home.join(".config")
    };
    config
        .join("cloudthinker/worker-state")
        .join(format!("{:x}", Sha256::digest(origin.as_bytes())))
}

pub(super) fn system_target_id(
    user: &SystemUser,
    origin: &str,
    selector: Option<&str>,
) -> CtResult<Uuid> {
    let root = system_state_root(user, origin);
    let target_id = match selector {
        Some(selector) => Uuid::parse_str(selector).map_err(|_| {
            CtError::Usage("with --system, pass the outpost id (a UUID) to --outpost".into())
        })?,
        None => only_credential(&root, user)?,
    };
    require_user_credential(&root, target_id, user)?;
    Ok(target_id)
}

fn missing_user_credential(user: &SystemUser, path: &Path) -> CtError {
    CtError::Auth(format!(
        "no stored worker credential at {}; as {}, run cloudthinker worker start --register <reference> --workdir <folder> once, then install the service again",
        path.display(),
        user.name
    ))
}

fn only_credential(root: &Path, user: &SystemUser) -> CtResult<Uuid> {
    let entries = fs::read_dir(root)
        .map_err(|_| missing_user_credential(user, &root.join("<outpost>.json")))?;
    let mut found = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            Uuid::parse_str(name.strip_suffix(".json")?).ok()
        })
        .collect::<Vec<_>>();
    match found.len() {
        0 => Err(missing_user_credential(user, &root.join("<outpost>.json"))),
        1 => Ok(found.remove(0)),
        _ => Err(CtError::Usage(
            "this account has more than one outpost; pass --outpost <id>".into(),
        )),
    }
}

fn require_user_credential(root: &Path, target_id: Uuid, user: &SystemUser) -> CtResult<()> {
    let path = root.join(format!("{target_id}.json"));
    let metadata = fs::symlink_metadata(&path).map_err(|_| missing_user_credential(user, &path))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !metadata.is_file() || metadata.uid() != user.uid || metadata.mode() & 0o077 != 0 {
            let shown = path.display();
            return Err(CtError::Store(format!(
                "worker credential {shown} must be a private file owned by {}; run: chown {} '{shown}' && chmod 600 '{shown}'",
                user.name, user.name
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(())
}
