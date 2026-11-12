//! Real local Windows files. No worker qualification or conversion is claimed.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::panic)]
use larm_adapter_host::input_files::{PinnedDirectory, PinnedReadFile};

#[test]
fn input_and_ancestors_stay_readable_but_cannot_be_overwritten_replaced_or_removed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("área 模型");
    std::fs::create_dir(&root).unwrap();
    let path = root.join("input.bin");
    std::fs::write(&path, b"abcdef").unwrap();
    let pin = PinnedReadFile::open(&path).unwrap();
    assert_eq!(pin.size().unwrap(), 6);
    let mut bytes = [0; 2];
    assert_eq!(pin.read_at(2, &mut bytes).unwrap(), 2);
    assert_eq!(&bytes, b"cd");
    assert_eq!(pin.read_at(0, &mut bytes).unwrap(), 2);
    assert_eq!(&bytes, b"ab");
    assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
    assert!(std::fs::write(&path, b"changed").is_err());
    assert!(std::fs::rename(&path, root.join("replacement.bin")).is_err());
    assert!(std::fs::remove_file(&path).is_err());
    assert!(std::fs::rename(&root, temp.path().join("moved")).is_err());
    let retained = pin.clone();
    drop(pin);
    assert!(std::fs::write(&path, b"still pinned").is_err());
    retained.recheck().unwrap();
    drop(retained);
    std::fs::write(&path, b"released").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"released");
    std::fs::rename(&root, temp.path().join("moved")).unwrap();
}

#[test]
fn a_preexisting_writer_cannot_be_mistaken_for_a_read_only_fence() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("input.bin");
    std::fs::write(&path, b"original").unwrap();
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .share_mode(7)
        .open(&path)
        .unwrap();
    assert!(PinnedReadFile::open(&path).is_err());
    drop(writer);
    assert!(PinnedReadFile::open(&path).is_ok());
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
}

#[test]
fn missing_files_and_directories_have_no_read_file_owner() {
    let temp = tempfile::tempdir().unwrap();
    assert!(PinnedReadFile::open(temp.path()).is_err());
    assert!(PinnedReadFile::open(&temp.path().join("missing.bin")).is_err());
}

#[test]
fn directory_probe_is_scoped_and_its_owner_releases_the_rename_fence() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace 模型");
    std::fs::create_dir(&root).unwrap();
    let original = root.join("user.bin");
    std::fs::write(&original, b"original").unwrap();
    let pin = PinnedDirectory::open(&root).unwrap();
    pin.check_writable().unwrap();
    pin.recheck().unwrap();
    assert_eq!(std::fs::read(&original).unwrap(), b"original");
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    assert!(std::fs::rename(&root, temp.path().join("moved")).is_err());
    assert!(PinnedDirectory::open(&original).is_err());
    assert!(PinnedDirectory::open(&root.join("missing")).is_err());
    drop(pin);
    std::fs::rename(&root, temp.path().join("moved")).unwrap();
    temp.close().unwrap();
}

#[test]
fn pinned_workspace_roots_allow_child_staging_and_atomic_publication() {
    let temp = tempfile::tempdir().unwrap();
    let work = temp.path().join("work");
    let output = temp.path().join("output");
    std::fs::create_dir(&work).unwrap();
    std::fs::create_dir(&output).unwrap();
    let pins = [
        PinnedDirectory::open(&work).unwrap(),
        PinnedDirectory::open(&output).unwrap(),
    ];
    let stage = work.join("staging/attempt");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join("payload.bin"), b"verified bytes").unwrap();
    std::fs::rename(&stage, output.join("attempt")).unwrap();
    assert_eq!(
        std::fs::read(output.join("attempt/payload.bin")).unwrap(),
        b"verified bytes"
    );
    drop(pins);
    temp.close().unwrap();
}

#[test]
fn output_can_publish_beside_a_pinned_original_without_mutating_or_replacing_it() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("original.bin");
    std::fs::write(&source, b"original").unwrap();
    let input = PinnedReadFile::open(&source).unwrap();
    let root = PinnedDirectory::open(temp.path()).unwrap();
    let stage = temp.path().join("staging/attempt");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join("output.bin"), b"new output").unwrap();
    std::fs::rename(&stage, temp.path().join("published")).unwrap();
    assert!(std::fs::write(&source, b"forbidden").is_err());
    assert!(std::fs::rename(&source, temp.path().join("moved.bin")).is_err());
    assert_eq!(std::fs::read(&source).unwrap(), b"original");
    input.recheck().unwrap();
    root.recheck().unwrap();
    drop(input);
    drop(root);
    temp.close().unwrap();
}
