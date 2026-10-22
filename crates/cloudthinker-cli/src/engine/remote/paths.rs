use std::path::PathBuf;

use cap_std::fs::Dir;
use cloudthinker_client::auth::worker_store::private_directory;
use cloudthinker_client::{CtError, CtResult};

pub const READY_ENV: &str = "CLOUDTHINKER_REMOTE_READY";
pub const REMOTE_ENV: &str = "CLOUDTHINKER_REMOTE";
pub const SSH_PROGRAM_ENV: &str = "CLOUDTHINKER_REMOTE_SSH";
pub const REMOTE_BIN_ENV: &str = "CLOUDTHINKER_REMOTE_BIN";
pub const ROOT_ENV: &str = "CLOUDTHINKER_REMOTE_DIR";
pub const HOST_PID_ENV: &str = "CLOUDTHINKER_REMOTE_HOST_PID";
pub const PASTE_MAX_AGE_SECS: u64 = 24 * 60 * 60;
const SOCKET_PATH_LIMIT: usize = 100;

pub fn validate_name(name: &str) -> CtResult<()> {
    let valid = (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(CtError::Usage(format!(
            "--name {name:?} must be 1 to 32 letters, digits, '-' or '_'"
        )))
    }
}

#[derive(Debug, Clone)]
pub struct RemotePaths {
    root: PathBuf,
    name: String,
}

impl RemotePaths {
    pub fn for_name(name: &str) -> CtResult<Self> {
        validate_name(name)?;
        let paths = Self {
            root: remote_root()?,
            name: name.to_string(),
        };
        if paths.socket().as_os_str().len() > SOCKET_PATH_LIMIT {
            return Err(CtError::Usage(format!(
                "the socket path {} is too long for a unix socket; set {ROOT_ENV} to a shorter folder",
                paths.socket().display()
            )));
        }
        Ok(paths)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn open_root(&self) -> CtResult<Dir> {
        private_folder(&self.root)
    }

    pub fn open_pastes(&self) -> CtResult<Dir> {
        private_folder(&self.pastes())
    }

    pub fn socket(&self) -> PathBuf {
        self.root.join(format!("{}.sock", self.name))
    }

    pub fn lock(&self) -> PathBuf {
        self.root.join(format!("{}.lock", self.name))
    }

    pub fn ready(&self) -> PathBuf {
        self.root.join(format!("{}.ready", self.name))
    }

    pub fn agent_socket(&self) -> PathBuf {
        self.root.join(format!("{}.agent.sock", self.name))
    }

    pub fn log(&self) -> PathBuf {
        self.root.join(format!("{}.log", self.name))
    }

    pub fn pastes(&self) -> PathBuf {
        self.root.join("pastes")
    }

    pub fn meta(&self) -> PathBuf {
        self.root.join(format!("{}.meta", self.name))
    }

    pub fn exit(&self) -> PathBuf {
        self.root.join(format!("{}.exit", self.name))
    }
}

pub fn remote_root() -> CtResult<PathBuf> {
    match std::env::var_os(ROOT_ENV) {
        Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir)),
        _ => Ok(home()?.join(".cloudthinker").join("remote")),
    }
}

pub fn name_for_folder(folder: &str) -> String {
    let base: String = std::path::Path::new(folder)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .take(24)
        .collect();
    let base = if base.trim_matches('-').is_empty() {
        "root".to_string()
    } else {
        base
    };
    let hash = folder.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    format!("{base}-{:06x}", hash & 0x00ff_ffff)
}

fn private_folder(path: &std::path::Path) -> CtResult<Dir> {
    private_directory(path).map_err(|_| {
        CtError::Usage(format!(
            "{} must be a folder only you can open; fix it with: chmod 700 {}",
            path.display(),
            path.display()
        ))
    })
}

fn home() -> CtResult<PathBuf> {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => Ok(PathBuf::from(home)),
        _ => Err(CtError::Usage("HOME is not set".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_short_and_shell_safe() {
        for folder in [
            "/home/me/app",
            "/",
            "/srv/my app.v2",
            &format!("/x/{}", "y".repeat(80)),
        ] {
            let name = name_for_folder(folder);
            assert!(validate_name(&name).is_ok(), "{folder} -> {name}");
            assert_eq!(name, name_for_folder(folder));
        }
        assert_ne!(name_for_folder("/a/app"), name_for_folder("/b/app"));
        assert!(name_for_folder("/home/me/app").starts_with("app-"));
        for good in ["default", "review-2", "a_b", &"x".repeat(32)] {
            assert!(validate_name(good).is_ok(), "{good}");
        }
        for bad in ["", "a b", "../x", "x;rm", "é", &"x".repeat(33)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }
}
