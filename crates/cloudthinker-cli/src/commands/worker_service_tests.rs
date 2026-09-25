use super::*;

use std::sync::{Mutex, OnceLock};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

static SERVICE_MANAGER_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[test]
fn ca_wo_41_service_ids_are_stable_for_same_target_and_directory() {
    let path = Path::new("/tmp/project");
    assert_eq!(
        service_id("https://app.example:443", Uuid::from_u128(9), path).unwrap(),
        service_id("https://app.example:443", Uuid::from_u128(9), path).unwrap()
    );
    assert_ne!(
        service_id("https://app.example:443", Uuid::from_u128(9), path).unwrap(),
        service_id("https://app.example:443", Uuid::from_u128(10), path).unwrap()
    );
    assert_ne!(
        service_id("https://app.example:443", Uuid::from_u128(9), path).unwrap(),
        service_id("https://other.example:443", Uuid::from_u128(9), path).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn ca_wo_43_systemd_manager_lifecycle_uses_private_unit_and_surfaces_bus_errors() {
    let _lock = SERVICE_MANAGER_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let home = root.path().join("home");
    let unit_dir = config.join("systemd/user");
    let bin = root.path().join("bin");
    let workdir = root.path().join("work");
    fs::create_dir_all(&unit_dir).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::create_dir(&workdir).unwrap();
    let private = fs::Permissions::from_mode(0o700);
    fs::set_permissions(root.path(), private.clone()).unwrap();
    fs::set_permissions(&config, private.clone()).unwrap();
    fs::set_permissions(&unit_dir, private.clone()).unwrap();
    fs::set_permissions(&bin, private.clone()).unwrap();
    fs::set_permissions(&home, private.clone()).unwrap();
    fs::set_permissions(&workdir, private).unwrap();

    let manager = bin.join("systemctl");
    let log = root.path().join("manager.log");
    let state = root.path().join("state");
    let bus_error = root.path().join("bus-error");
    let shell_path = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    fs::write(
        &manager,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> {log}
if [ -f {bus_error} ] && [ "$2" = "is-active" ]; then
  printf '%s\n' 'Failed to connect to bus: No medium found' >&2
  exit 1
fi
case "$2" in
  daemon-reload) exit 0 ;;
  enable)
    test -f {unit_dir}/"$3" || exit 42
    printf '%s' inactive > {state}
    exit 0
    ;;
  is-active)
    current=$(cat {state} 2>/dev/null || printf inactive)
    printf '%s\n' "$current"
    test "$current" = active
    exit $?
    ;;
  start) printf '%s' active > {state}; exit 0 ;;
  stop) printf '%s' inactive > {state}; exit 0 ;;
  disable) exit 0 ;;
  *) exit 64 ;;
esac
"#,
            log = shell_path(&log),
            bus_error = shell_path(&bus_error),
            unit_dir = shell_path(&unit_dir),
            state = shell_path(&state),
        ),
    )
    .unwrap();
    fs::set_permissions(&manager, fs::Permissions::from_mode(0o700)).unwrap();
    set_test_systemd_manager(&manager);
    let _manager_override = ManagerOverride;

    let workdir = workdir.canonicalize().unwrap();
    let target_id = Uuid::from_u128(42);
    let origin = "https://app.example:443";
    let service_id = service_id(origin, target_id, &workdir).unwrap();
    let service_name = format!("{SERVICE_NAMESPACE}.{service_id}");
    let descriptor_path = unit_dir.join(format!("{service_name}.service"));
    write_descriptor(&descriptor_path, b"[Service]\n").unwrap();
    let target = ServiceTarget {
        platform: ServicePlatform::Systemd,
        target_id,
        workdir,
        service_name: service_name.clone(),
        descriptor_path,
    };

    let result = (|| {
        manager_install(&target)?;
        assert_eq!(manager_status(&target)?, "inactive");
        manager_start(&target)?;
        assert_eq!(manager_status(&target)?, "active");
        fs::write(&bus_error, b"1").unwrap();
        assert!(manager_status(&target).is_err());
        fs::remove_file(&bus_error).unwrap();
        manager_stop(&target)?;
        assert_eq!(manager_status(&target)?, "inactive");
        manager_uninstall(&target)?;
        Ok::<(), CtError>(())
    })();
    result.unwrap();

    let log = fs::read_to_string(log).unwrap();
    assert!(log.contains(&format!("--user enable {service_name}.service")));
}

#[cfg(unix)]
#[test]
fn ca_wo_43_launchd_manager_lifecycle_bootstraps_on_start_and_surfaces_domain_errors() {
    let _lock = SERVICE_MANAGER_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let launch_dir = root.path().join("Library/LaunchAgents");
    let bin = root.path().join("bin");
    let workdir = root.path().join("work");
    fs::create_dir_all(&launch_dir).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir(&workdir).unwrap();
    let private = fs::Permissions::from_mode(0o700);
    fs::set_permissions(root.path(), private.clone()).unwrap();
    fs::set_permissions(&launch_dir, private.clone()).unwrap();
    fs::set_permissions(&bin, private.clone()).unwrap();
    fs::set_permissions(&workdir, private).unwrap();

    let manager = bin.join("launchctl");
    let log = root.path().join("manager.log");
    let loaded = root.path().join("loaded");
    let active = root.path().join("active");
    let manager_error = root.path().join("manager-error");
    let domain = "gui/4242";
    let shell_path = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    let workdir = workdir.canonicalize().unwrap();
    let target_id = Uuid::from_u128(43);
    let origin = "https://app.example:443";
    let service_id = service_id(origin, target_id, &workdir).unwrap();
    let service_name = format!("{SERVICE_NAMESPACE}.{service_id}");
    let service = format!("{domain}/{service_name}");
    let descriptor_path = launch_dir.join(format!("{service_name}.plist"));
    fs::write(
        &manager,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> {log}
if [ "$1" = "print" ] && [ "$2" = "{domain}" ]; then
  if [ -f {manager_error} ]; then
    printf '%s\n' 'launchd unavailable' >&2
    exit 1
  fi
  exit 0
fi
case "$1" in
  print)
    if [ -f {loaded} ]; then
      if [ "$(cat {active})" = active ]; then
        printf '%s\n' 'state = running'
      else
        printf '%s\n' 'state = exited'
      fi
      exit 0
    fi
    printf '%s\n' 'Could not find service "{service_name}" in domain for user gui: 4242' >&2
    exit 1
    ;;
  bootstrap)
    test "$2" = "{domain}" || exit 42
    test -f "$3" || exit 43
    printf '%s' loaded > {loaded}
    printf '%s' active > {active}
    exit 0
    ;;
  kickstart)
    test "$2" = "{service}" || exit 44
    printf '%s' active > {active}
    exit 0
    ;;
  kill)
    test "$2" = SIGTERM || exit 45
    test "$3" = "{service}" || exit 46
    printf '%s' inactive > {active}
    exit 0
    ;;
  bootout)
    test "$2" = "{service}" || exit 47
    rm -f {loaded} {active}
    exit 0
    ;;
  *) exit 64 ;;
esac
"#,
            log = shell_path(&log),
            manager_error = shell_path(&manager_error),
            loaded = shell_path(&loaded),
            active = shell_path(&active),
        ),
    )
    .unwrap();
    fs::set_permissions(&manager, fs::Permissions::from_mode(0o700)).unwrap();
    set_test_launchd_manager(&manager);
    set_test_launchd_uid(4242);
    let _manager_override = LaunchdOverride;

    write_descriptor(&descriptor_path, b"<plist/>\n").unwrap();
    let target = ServiceTarget {
        platform: ServicePlatform::Launchd,
        target_id,
        workdir,
        service_name: service_name.clone(),
        descriptor_path: descriptor_path.clone(),
    };

    manager_install(&target).unwrap();
    assert_eq!(manager_status(&target).unwrap(), "inactive");
    manager_start(&target).unwrap();
    assert_eq!(manager_status(&target).unwrap(), "active");
    fs::write(&manager_error, b"1").unwrap();
    assert!(manager_status(&target).is_err());
    fs::remove_file(&manager_error).unwrap();
    manager_stop(&target).unwrap();
    assert_eq!(manager_status(&target).unwrap(), "inactive");
    manager_uninstall(&target).unwrap();
    fs::remove_file(&descriptor_path).unwrap();

    let log = fs::read_to_string(log).unwrap();
    assert!(log.contains(&format!("bootstrap {domain} ")));
    assert!(log.contains(&format!("kickstart {service}")));
    assert!(log.contains(&format!("bootout {service}")));
}

#[cfg(unix)]
struct ManagerOverride;

#[cfg(unix)]
impl Drop for ManagerOverride {
    fn drop(&mut self) {
        clear_test_systemd_manager();
    }
}

#[cfg(unix)]
struct LaunchdOverride;

#[cfg(unix)]
impl Drop for LaunchdOverride {
    fn drop(&mut self) {
        clear_test_launchd_manager();
        clear_test_launchd_uid();
    }
}

#[cfg(unix)]
struct UmaskOverride(rustix::fs::Mode);

#[cfg(unix)]
impl Drop for UmaskOverride {
    fn drop(&mut self) {
        rustix::process::umask(self.0);
    }
}

#[cfg(unix)]
#[test]
fn ca_wo_46_descriptor_write_creates_a_private_service_directory_under_a_group_umask() {
    let _lock = SERVICE_MANAGER_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let config = root.path().join("config");
    let unit_dir = config.join("systemd/user");
    let descriptor_path = unit_dir.join("worker.service");

    let _umask = UmaskOverride(rustix::process::umask(rustix::fs::Mode::from_raw_mode(
        0o002,
    )));
    write_descriptor(&descriptor_path, b"[Service]\n").unwrap();

    for directory in [&config, &config.join("systemd"), &unit_dir] {
        let mode = fs::symlink_metadata(directory)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o022, 0, "{} is {mode:o}", directory.display());
    }
    assert_eq!(fs::read(&descriptor_path).unwrap(), b"[Service]\n");
}
