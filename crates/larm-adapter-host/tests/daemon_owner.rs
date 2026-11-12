//! Actual Windows sharing/path fences. Contents of the anchor grant nothing.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]
use larm_adapter_host::daemon_owner::{DaemonOwnership, OWNER_FILE};
use larm_domain::error::ErrorCode;

#[test]
fn retained_native_handles_keep_both_anchors_until_async_work_releases_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("host");
    let other = dir.path().join("other");
    let database = root.join("state/larm.sqlite");
    let owner =
        DaemonOwnership::acquire_for_database(&root, std::path::Path::new("state/larm.sqlite"))
            .unwrap();
    let retained = owner.retain().unwrap();
    assert_eq!(retained.epoch(), owner.epoch());
    drop(owner);
    retained
        .recheck_database(std::path::Path::new("state/larm.sqlite"))
        .unwrap();
    assert!(DaemonOwnership::acquire(&root).is_err());
    assert!(DaemonOwnership::acquire_for_database(&other, &database).is_err());
    drop(retained);
    let _next = DaemonOwnership::acquire_for_database(&other, &database).unwrap();
}

#[test]
fn only_one_host_can_own_a_root_including_dot_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("datos 模型");
    let first = DaemonOwnership::acquire(&root).unwrap();
    assert!(larm_domain::bridge::is_uuid_v7(first.epoch()));
    first.recheck().unwrap();
    for alias in [root.clone(), root.join(".")] {
        let error = DaemonOwnership::acquire(&alias).err().unwrap();
        assert_eq!(error.code, ErrorCode::Conflict);
        assert_eq!(error.details.reason, "daemon_data_root_in_use");
    }
    let epoch = first.epoch().to_owned();
    drop(first);
    let next = DaemonOwnership::acquire(&root).unwrap();
    assert_ne!(next.epoch(), epoch);
}

#[test]
fn stale_content_and_a_pid_are_never_ownership_authority() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(OWNER_FILE);
    let fake = br#"{"pid":0,"state":"STOPPED","closed":true}"#;
    std::fs::write(&path, fake).unwrap();
    let owner = DaemonOwnership::acquire(dir.path()).unwrap();
    assert!(DaemonOwnership::acquire(dir.path()).is_err());
    assert!(std::fs::write(&path, b"replace").is_err());
    drop(owner);
    assert_eq!(std::fs::read(&path).unwrap(), fake);
    let _next = DaemonOwnership::acquire(dir.path()).unwrap();
}

#[test]
fn the_owner_prevents_root_and_anchor_replacement_while_allowing_database_child_io() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let moved = dir.path().join("moved");
    let owner = DaemonOwnership::acquire(&root).unwrap();
    assert!(
        root.is_absolute()
            && moved.is_absolute()
            && root.starts_with(dir.path())
            && moved.starts_with(dir.path())
    );
    assert!(std::fs::rename(&root, &moved).is_err());
    assert!(std::fs::remove_file(root.join(OWNER_FILE)).is_err());
    std::fs::write(root.join("database.tmp"), b"sqlite IO").unwrap();
    std::fs::rename(root.join("database.tmp"), root.join("database.fixture")).unwrap();
    owner.recheck().unwrap();
    drop(owner);
    std::fs::rename(&root, &moved).unwrap();
}

#[test]
fn independent_roots_are_independent_and_directory_anchors_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let first = DaemonOwnership::acquire(&dir.path().join("first")).unwrap();
    let second = DaemonOwnership::acquire(&dir.path().join("second")).unwrap();
    assert_ne!(first.epoch(), second.epoch());
    let bad = dir.path().join("bad");
    std::fs::create_dir_all(bad.join(OWNER_FILE)).unwrap();
    assert!(DaemonOwnership::acquire(&bad).is_err());
}

#[test]
fn a_junction_cannot_redirect_a_daemon_owner_to_another_root() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let link = dir.path().join("redirect");
    let output = std::process::Command::new("cmd.exe")
        .args(["/c", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(DaemonOwnership::acquire(&link).is_err());
    assert!(!outside.join(OWNER_FILE).exists());
}

#[test]
fn ownership_binds_the_database_and_excludes_other_roots_using_its_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("first");
    let other = dir.path().join("second");
    let owner =
        DaemonOwnership::acquire_for_database(&root, std::path::Path::new("state/larm.sqlite"))
            .unwrap();
    owner
        .recheck_database(std::path::Path::new("state/larm.sqlite"))
        .unwrap();
    assert!(owner
        .recheck_database(std::path::Path::new("different.sqlite"))
        .is_err());
    let shared = root.join("state/larm.sqlite");
    assert_eq!(
        DaemonOwnership::acquire_for_database(&other, &shared)
            .err()
            .unwrap()
            .code,
        ErrorCode::Conflict
    );
    drop(owner);
    let next = DaemonOwnership::acquire_for_database(&other, &shared).unwrap();
    next.recheck_database(&shared).unwrap();
}
