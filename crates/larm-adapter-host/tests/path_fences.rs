#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_adapter_host::package::{check_managed_path, controlled_hash, PackageControl};
use std::path::Path;
use std::process::Command;

fn junction(path: &Path, target: &Path) {
    use std::os::windows::process::CommandExt;
    let output = Command::new("C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command",
            "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:LARM_FENCE_LINK -Target $env:LARM_FENCE_TARGET | Out-Null"])
        .env("LARM_FENCE_LINK", path)
        .env("LARM_FENCE_TARGET", target)
        .creation_flags(0x0800_0000)
        .output().unwrap();
    assert!(
        output.status.success(),
        "junction creation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn junction_itself_and_all_descendants_are_rejected_after_a_prior_good_scan() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("package");
    let nested = package.join("nested");
    let target = root.path().join("target");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("payload.bin"), b"target is outside package").unwrap();
    controlled_hash(&package, &PackageControl::default()).unwrap();
    std::fs::remove_dir(&nested).unwrap(); // Owned empty directory only.
    junction(&nested, &target);
    for path in [
        &nested,
        &nested.join("payload.bin"),
        &nested.join("missing/child.bin"),
    ] {
        assert_eq!(
            check_managed_path(path).unwrap_err().details.reason,
            "package_reparse_rejected"
        );
    }
    assert_eq!(
        controlled_hash(&package, &PackageControl::default())
            .unwrap_err()
            .details
            .reason,
        "package_reparse_rejected"
    );
    std::fs::remove_dir(&nested).unwrap(); // Remove the owned junction, never its target.
    assert_eq!(
        std::fs::read(target.join("payload.bin")).unwrap(),
        b"target is outside package"
    );
}

#[test]
fn per_hash_ownership_measures_the_full_package_and_observes_later_changes() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("one/two/three/four/five/six");
    std::fs::create_dir_all(&parent).unwrap();
    let paths: Vec<_> = (0..100)
        .map(|index| parent.join(format!("{index}.bin")))
        .collect();
    for path in &paths {
        std::fs::write(path, b"measurement").unwrap();
    }
    let baseline = std::time::Instant::now();
    let mut expected = String::new();
    for _ in 0..3 {
        expected = larm_adapter_sdk::permissions::package_hash_checked(
            &parent,
            &["manifest.json"],
            &mut |path, _, _| check_managed_path(path),
        )
        .unwrap();
    }
    let baseline = baseline.elapsed();
    let fresh = std::time::Instant::now();
    for _ in 0..3 {
        assert_eq!(
            controlled_hash(&parent, &PackageControl::default()).unwrap(),
            expected
        );
    }
    eprintln!(
        "full hashes: 3 scans of 100 files; prior_us={}; owned_paths_us={}",
        baseline.as_micros(),
        fresh.elapsed().as_micros()
    );
    // A later change must be visible even after successful repeated checks.
    let target = root.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let link = parent.join("new-link");
    junction(&link, &target);
    assert_eq!(
        check_managed_path(&link).unwrap_err().details.reason,
        "package_reparse_rejected"
    );
    std::fs::remove_dir(link).unwrap();
}

#[test]
fn cancellation_during_a_pinned_read_releases_all_paths_without_a_digest() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("large.bin");
    std::fs::write(&file, vec![17; (2 << 20) + 1]).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let control = PackageControl::default().with_io_hooks(
        Arc::new(move || {
            if observed.fetch_add(1, Ordering::SeqCst) + 1 == 7 {
                return Err(larm_domain::error::LarmError::new(
                    larm_domain::error::ErrorCode::ResourceUnavailable,
                    "cancelled by test",
                    "operation_cancelled",
                ));
            }
            Ok(())
        }),
        Arc::new(|_| Ok(())),
    );
    assert_eq!(
        controlled_hash(root.path(), &control)
            .unwrap_err()
            .details
            .reason,
        "operation_cancelled"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 7);
    std::fs::rename(&file, root.path().join("released.bin")).unwrap();
}
