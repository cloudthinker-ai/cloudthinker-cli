use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use cloudthinker_client::{CtError, CtResult};
use uuid::Uuid;

use super::{ServicePlatform, ServiceTarget};

pub(super) fn descriptor_path(platform: ServicePlatform, service_name: &str) -> CtResult<PathBuf> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| CtError::Store("home directory unavailable".into()))?;
    let root = match platform {
        ServicePlatform::Systemd => env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".config"))
            .join("systemd/user"),
        ServicePlatform::Launchd => home.join("Library/LaunchAgents"),
    };
    let suffix = match platform {
        ServicePlatform::Systemd => "service",
        ServicePlatform::Launchd => "plist",
    };
    Ok(root.join(format!("{service_name}.{suffix}")))
}

pub(super) fn render_descriptor(
    platform: ServicePlatform,
    service_name: &str,
    argv: &[String],
    workdir: &Path,
) -> CtResult<String> {
    match platform {
        ServicePlatform::Systemd => render_systemd(service_name, argv, workdir),
        ServicePlatform::Launchd => render_launchd(service_name, argv),
    }
}

pub(super) fn render_systemd(
    service_name: &str,
    argv: &[String],
    workdir: &Path,
) -> CtResult<String> {
    let command = argv
        .iter()
        .map(|value| systemd_quote(value))
        .collect::<CtResult<Vec<_>>>()?
        .join(" ");
    let workdir = systemd_path(&path_text(workdir, "workdir")?)?;
    Ok(format!(
        "[Unit]\nDescription=CloudThinker worker {service_name}\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=exec\nExecStart={command}\nWorkingDirectory={workdir}\nRestart=on-failure\nRestartPreventExitStatus=2 3\nRestartSec=5\nKillMode=mixed\nKillSignal=SIGTERM\nTimeoutStopSec=300\nStandardOutput=journal\nStandardError=journal\n\n[Install]\nWantedBy=default.target\n"
    ))
}

pub(super) fn render_launchd(service_name: &str, argv: &[String]) -> CtResult<String> {
    let mut output = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
    );
    xml_key_value(&mut output, "Label", service_name)?;
    output.push_str("<key>ProgramArguments</key>\n<array>\n");
    for value in argv {
        output.push_str("<string>");
        output.push_str(&xml_escape(value)?);
        output.push_str("</string>\n");
    }
    output.push_str(
        "</array>\n<key>RunAtLoad</key>\n<true/>\n<key>KeepAlive</key>\n<dict>\n<key>SuccessfulExit</key>\n<false/>\n</dict>\n<key>ExitTimeOut</key>\n<integer>300</integer>\n<key>ProcessType</key>\n<string>Background</string>\n<key>LowPriorityIO</key>\n<true/>\n<key>ThrottleInterval</key>\n<integer>10</integer>\n</dict>\n</plist>\n",
    );
    Ok(output)
}

fn xml_key_value(output: &mut String, key: &str, value: &str) -> CtResult<()> {
    output.push_str("<key>");
    output.push_str(key);
    output.push_str("</key>\n<string>");
    output.push_str(&xml_escape(value)?);
    output.push_str("</string>\n");
    Ok(())
}

fn xml_escape(value: &str) -> CtResult<String> {
    validate_service_text(value, "service argument")?;
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

fn systemd_quote(value: &str) -> CtResult<String> {
    validate_service_text(value, "service argument")?;
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '$' => output.push_str("$$"),
            '%' => output.push_str("%%"),
            _ => output.push(character),
        }
    }
    output.push('"');
    Ok(output)
}

fn systemd_path(value: &str) -> CtResult<String> {
    if value.trim() != value {
        return Err(CtError::Usage(
            "workdir must not start or end with whitespace".into(),
        ));
    }
    Ok(value.replace('%', "%%"))
}

pub(super) fn validate_service_text(value: &str, field: &str) -> CtResult<()> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(CtError::Usage(format!(
            "{field} contains unsupported control characters"
        )));
    }
    Ok(())
}

pub(super) fn validate_label(value: &str) -> CtResult<()> {
    validate_service_text(value, "--label")?;
    if value.contains('/') || value.contains('\\') {
        return Err(CtError::Usage(
            "--label must be a short display name, not a path".into(),
        ));
    }
    Ok(())
}

pub(super) fn path_text(path: &Path, field: &str) -> CtResult<String> {
    let value = path
        .to_str()
        .ok_or_else(|| CtError::Usage(format!("{field} must be valid UTF-8")))?;
    validate_service_text(value, field)?;
    Ok(value.to_owned())
}

pub(super) fn write_descriptor(path: &Path, contents: &[u8]) -> CtResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| CtError::Store("worker service directory unavailable".into()))?;
    let mut directories = fs::DirBuilder::new();
    directories.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directories.mode(0o700);
    }
    directories
        .create(parent)
        .map_err(|_| CtError::Store("worker service directory unavailable".into()))?;
    let metadata = fs::symlink_metadata(parent)
        .map_err(|_| CtError::Store("worker service directory unavailable".into()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CtError::Store("worker service directory is unsafe".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
            return Err(CtError::Store("worker service directory is unsafe".into()));
        }
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| CtError::Store("worker service descriptor name unavailable".into()))?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        file_name.to_string_lossy(),
        Uuid::new_v4()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|_| CtError::Store("worker service descriptor could not be written".into()))?;
    let result = (|| {
        file.write_all(contents)
            .map_err(|_| CtError::Store("worker service descriptor could not be written".into()))?;
        file.sync_all()
            .map_err(|_| CtError::Store("worker service descriptor could not be synced".into()))?;
        fs::rename(&temporary, path).map_err(|_| {
            CtError::Store("worker service descriptor could not be installed".into())
        })?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn validate_descriptor_metadata(metadata: &fs::Metadata) -> CtResult<()> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CtError::Store("worker service descriptor is unsafe".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(CtError::Store("worker service descriptor is unsafe".into()));
        }
    }
    Ok(())
}

pub(super) fn sync_directory(path: &Path) -> CtResult<()> {
    File::open(path)
        .map_err(|_| CtError::Store("worker service directory could not be opened".into()))?
        .sync_all()
        .map_err(|_| CtError::Store("worker service directory could not be synced".into()))
}

pub(super) fn require_descriptor(target: &ServiceTarget) -> CtResult<()> {
    let metadata = fs::symlink_metadata(&target.descriptor_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CtError::Usage("worker service is not installed".into())
        } else {
            CtError::Store("worker service descriptor unavailable".into())
        }
    })?;
    validate_descriptor_metadata(&metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_wo_45_systemd_descriptor_preserves_argv_and_graceful_stop() {
        let workdir = PathBuf::from("/srv/cloud thinker/work");
        let argv = vec![
            "/usr/local/bin/cloudthinker".into(),
            "--url".into(),
            "https://app.example/%worker".into(),
            "worker".into(),
            "start".into(),
            "--workdir".into(),
            "/srv/cloud thinker/work".into(),
            "--label".into(),
            "name with $ and %".into(),
            "--stored-credential".into(),
        ];
        let rendered = render_systemd("io.cloudthinker.worker.demo", &argv, &workdir).unwrap();
        assert!(rendered.contains("ExecStart=\"/usr/local/bin/cloudthinker\""));
        assert!(rendered.contains("https://app.example/%%worker"));
        assert!(rendered.contains("name with $$ and %%"));
        assert!(rendered.contains("Type=exec"));
        assert!(rendered.contains("\nWorkingDirectory=/srv/cloud thinker/work\n"));
        assert!(rendered.contains("KillSignal=SIGTERM"));
        assert!(rendered.contains("TimeoutStopSec=300"));
        assert!(rendered.contains("RestartPreventExitStatus=2 3"));
        assert!(rendered.contains("KillMode=mixed"));
        assert!(!rendered.contains("PrivateTmp=true"));
        assert!(rendered.contains("--stored-credential"));
        assert!(!rendered.contains("CLOUDTHINKER_WORKER_TOKEN"));
    }

    #[test]
    fn ca_wo_45_systemd_descriptor_preserves_unicode() {
        assert_eq!(systemd_quote("café / 目录").unwrap(), "\"café / 目录\"");
    }

    #[test]
    fn ca_wo_45_systemd_working_directory_is_the_literal_path_systemd_reads_back() {
        let workdir = PathBuf::from("/srv/cloud thinker/100% done");
        let rendered = render_systemd(
            "io.cloudthinker.worker.demo",
            &["/usr/local/bin/cloudthinker".into()],
            &workdir,
        )
        .unwrap();
        assert!(rendered.contains("\nWorkingDirectory=/srv/cloud thinker/100%% done\n"));
        assert!(!rendered.contains(r"\x"));
    }

    #[test]
    fn ca_wo_45_systemd_rejects_a_working_directory_a_unit_file_cannot_carry() {
        let argv = vec!["/usr/local/bin/cloudthinker".to_owned()];
        for path in [
            "/srv/new\nline",
            "/srv/tab\there",
            "/srv/trailing ",
            " /lead",
        ] {
            assert!(matches!(
                render_systemd("io.cloudthinker.worker.demo", &argv, Path::new(path)),
                Err(CtError::Usage(_))
            ));
        }
    }

    #[test]
    fn ca_wo_45_systemd_quote_rejects_a_control_character_before_quoting() {
        assert!(matches!(
            systemd_quote("start\nExecStart=/bin/sh"),
            Err(CtError::Usage(_))
        ));
        assert!(matches!(systemd_quote(""), Err(CtError::Usage(_))));
    }

    #[test]
    fn ca_wo_45_launchd_descriptor_uses_argv_array_without_shell_or_secret() {
        let argv = vec![
            "/usr/local/bin/cloudthinker".into(),
            "--url".into(),
            "https://app.example".into(),
            "worker".into(),
            "start".into(),
            "--label".into(),
            "a & b".into(),
            "--stored-credential".into(),
        ];
        let rendered = render_launchd("io.cloudthinker.worker.demo", &argv).unwrap();
        assert!(rendered.contains("<key>ProgramArguments</key>"));
        assert!(rendered.contains("<string>a &amp; b</string>"));
        assert!(rendered.contains("<key>RunAtLoad</key>\n<true/>"));
        assert!(rendered.contains("<key>SuccessfulExit</key>\n<false/>"));
        assert!(rendered.contains("<key>ExitTimeOut</key>\n<integer>300</integer>"));
        assert!(!rendered.contains("CLOUDTHINKER_WORKER_TOKEN"));
    }

    #[cfg(unix)]
    #[test]
    fn ca_wo_42_a_group_readable_descriptor_fails_closed() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), PermissionsExt::from_mode(0o700)).unwrap();
        let path = root.path().join("worker.service");
        write_descriptor(&path, b"[Service]\n").unwrap();
        validate_descriptor_metadata(&std::fs::symlink_metadata(&path).unwrap()).unwrap();

        std::fs::set_permissions(&path, PermissionsExt::from_mode(0o644)).unwrap();

        let error = validate_descriptor_metadata(&std::fs::symlink_metadata(&path).unwrap())
            .expect_err("another user can read the descriptor");
        assert!(matches!(error, CtError::Store(_)), "got {error:?}");
    }

    #[cfg(unix)]
    #[test]
    fn ca_wo_46_descriptor_write_is_private_and_replaces_a_symlink_atomically() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            root.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let path = root.path().join("worker.service");
        let target = root.path().join("target.service");
        std::fs::write(&target, b"outside").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let directory_metadata = std::fs::symlink_metadata(root.path()).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            directory_metadata.uid(),
            rustix::process::geteuid().as_raw()
        );
        assert_eq!(directory_metadata.mode() & 0o022, 0);

        write_descriptor(&path, b"[Service]\n").unwrap();

        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(metadata.is_file());
        assert!(!metadata.file_type().is_symlink());
        assert_eq!(std::fs::read(&path).unwrap(), b"[Service]\n");
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(std::fs::read(&target).unwrap(), b"outside");
    }
}
