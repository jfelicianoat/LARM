//! Actual file IO: canonical digest, bounded reads and fail-closed checkpoints.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use larm_adapter_sdk::permissions::{package_hash, package_hash_checked};
use larm_domain::error::{ErrorCode, LarmError};
use sha2::{Digest, Sha256};

fn payload() -> Vec<u8> {
    (0..(2 * 1024 * 1024 + 17))
        .map(|i| (i % 251) as u8)
        .collect()
}
fn abort(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "Owned IO fixture stopped",
        reason,
    )
}

#[test]
fn canonical_hash_uses_bounded_batches_across_large_and_small_files() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = payload();
    std::fs::write(dir.path().join("A.bin"), &bytes).unwrap();
    std::fs::write(dir.path().join("b.bin"), b"small").unwrap();
    std::fs::write(dir.path().join("empty"), b"").unwrap();
    std::fs::write(dir.path().join("manifest.json"), b"excluded fixture").unwrap();
    let mut deltas = Vec::new();
    let mut previous = 0;
    let actual = package_hash_checked(dir.path(), &["manifest.json"], &mut |_, _, observed| {
        assert!(observed >= previous);
        if observed > previous {
            deltas.push(observed - previous);
        }
        previous = observed;
        Ok(())
    })
    .unwrap();
    let mut canonical = Sha256::new();
    for (name, data) in [
        ("a.bin", bytes.as_slice()),
        ("b.bin", b"small".as_slice()),
        ("empty", b"".as_slice()),
    ] {
        canonical.update(format!("{name}\0{}\n", hex::encode(Sha256::digest(data))).as_bytes());
    }
    assert_eq!(actual, hex::encode(canonical.finalize()));
    assert_eq!(previous, bytes.len() as u64 + 5);
    assert!(deltas.iter().all(|n| *n <= 1024 * 1024));
    assert_eq!(
        deltas.iter().copied().max(),
        Some(1024 * 1024),
        "verification must batch large file reads without removing checkpoints"
    );
}

#[test]
fn cancellation_after_first_read_stops_before_another_chunk() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("large"), payload()).unwrap();
    let mut stopped = false;
    let mut read_bytes = 0;
    let error = package_hash_checked(dir.path(), &[], &mut |_, _, observed| {
        assert!(!stopped, "no IO checkpoint is allowed after cancellation");
        if observed > 0 {
            stopped = true;
            read_bytes = observed;
            return Err(abort("fixture_cancelled"));
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(error.details.reason, "fixture_cancelled");
    assert!(stopped);
    assert!((1..=1024 * 1024).contains(&read_bytes));
}

#[test]
fn byte_limit_and_entry_fence_never_return_a_digest() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("large"), payload()).unwrap();
    let mut observed = 0;
    let error = package_hash_checked(dir.path(), &[], &mut |_, _, bytes| {
        observed = bytes;
        if bytes > 1_500_000 {
            Err(abort("fixture_size_limit"))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.details.reason, "fixture_size_limit");
    assert!(observed > 1_500_000 && observed <= 1_500_000 + 1024 * 1024);
    let mut reached_entry = false;
    let error = package_hash_checked(dir.path(), &[], &mut |path, _, bytes| {
        if path.file_name().is_some_and(|name| name == "large") {
            reached_entry = true;
            assert_eq!(bytes, 0);
            Err(abort("fixture_path_fence"))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert!(reached_entry);
    assert_eq!(error.details.reason, "fixture_path_fence");
}

#[test]
fn bytes_beyond_first_batch_still_change_the_full_digest() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = payload();
    let path = dir.path().join("large");
    std::fs::write(&path, &bytes).unwrap();
    let before = package_hash(dir.path(), &[]).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();
    let after = package_hash(dir.path(), &[]).unwrap();
    assert_ne!(before, after);
}
