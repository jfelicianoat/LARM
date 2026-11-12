//! Actual file hashes and Windows sharing fences, independently of the SQL
//! observation fixtures. These tests do not run a conversion or qualify tools.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::panic)]
use larm_domain::{
    transformations::{
        InputArtifact, TransformationInputGuard, TransformationInputIdentity,
        TransformationInputLocation,
    },
    units::DecimalU64,
};
use larm_transform::input::VerifiedTransformationInput;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf};

fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}
struct Fixture {
    directory: tempfile::TempDir,
    root: PathBuf,
    path: PathBuf,
    identity: TransformationInputIdentity,
    artifacts: Vec<InputArtifact>,
    locations: Vec<TransformationInputLocation>,
    roots: BTreeMap<String, PathBuf>,
}
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("almacén 模型");
        let path = root.join("blobs/sha256/data");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let artifact = InputArtifact {
            artifact_id: id(),
            relative_path: "model/weights.bin".into(),
            size_bytes: DecimalU64::new(bytes.len() as u64).unwrap(),
            sha256: hex::encode(Sha256::digest(bytes)),
        };
        let location = TransformationInputLocation {
            artifact_id: artifact.artifact_id.clone(),
            artifact_location_id: id(),
            storage_location_id: id(),
            canonical_storage_path: root.to_str().unwrap().into(),
            relative_path: "blobs/sha256/data".into(),
            volume_key: "fixture-volume".into(),
        };
        let roots = BTreeMap::from([(location.storage_location_id.clone(), root.clone())]);
        Self {
            directory,
            root,
            path,
            identity: TransformationInputIdentity {
                variant_id: id(),
                revision_id: id(),
                variant_bundle_hash: "a".repeat(64),
                bundle_id: id(),
                bundle_row_version: 1,
                bundle_manifest_hash: "a".repeat(64),
            },
            artifacts: vec![artifact],
            locations: vec![location],
            roots,
        }
    }
    async fn acquire(&self) -> Result<VerifiedTransformationInput, larm_domain::error::LarmError> {
        VerifiedTransformationInput::acquire(
            self.identity.clone(),
            self.artifacts.clone(),
            self.locations.clone(),
            self.roots.clone(),
        )
        .await
    }
}

#[tokio::test]
async fn actual_hashes_zero_byte_files_and_chunked_files_are_verified_and_kept_immutable() {
    for bytes in [vec![], b"actual input".to_vec(), vec![0x9a; (1 << 20) + 11]] {
        let f = Fixture::new(&bytes);
        let input = f.acquire().await.unwrap();
        assert_eq!(input.artifacts(), f.artifacts);
        assert_eq!(input.locations(), f.locations);
        assert_ne!(
            input.artifacts()[0].relative_path,
            input.locations()[0].relative_path
        );
        assert!(std::fs::write(&f.path, b"tampered").is_err());
        assert!(std::fs::rename(&f.root, f.directory.path().join("moved")).is_err());
        input.recheck().await.unwrap();
        assert_eq!(std::fs::read(&f.path).unwrap(), bytes);
        drop(input);
        std::fs::write(&f.path, b"released").unwrap();
    }
}

#[tokio::test]
async fn same_length_corruption_wrong_sizes_and_missing_files_do_not_produce_a_guard() {
    for failure in ["hash", "short", "long", "missing"] {
        let f = Fixture::new(b"original");
        match failure {
            "hash" => std::fs::write(&f.path, b"replaced").unwrap(),
            "short" => std::fs::write(&f.path, b"short").unwrap(),
            "long" => std::fs::write(&f.path, b"longer than original").unwrap(),
            "missing" => std::fs::remove_file(&f.path).unwrap(),
            _ => unreachable!(),
        }
        assert!(f.acquire().await.is_err(), "{failure}");
        // Failed acquisition must close all pins; it does not repair the input.
        std::fs::write(&f.path, b"editable again").unwrap();
    }
}

#[tokio::test]
async fn escape_ads_reserved_names_and_unresolved_roots_are_rejected() {
    for relative in [
        "../outside.bin",
        "C:/outside.bin",
        "/outside.bin",
        "blobs/sha256/data:stream",
        "NUL",
        "blobs/./sha256/data",
    ] {
        let mut f = Fixture::new(b"original");
        f.locations[0].relative_path = relative.into();
        assert!(f.acquire().await.is_err(), "{relative}");
        assert_eq!(std::fs::read(&f.path).unwrap(), b"original");
    }
    let mut f = Fixture::new(b"original");
    f.roots.clear();
    assert!(f.acquire().await.is_err());
}

#[tokio::test]
async fn a_junction_cannot_redirect_an_input_outside_its_resolved_root() {
    let mut f = Fixture::new(b"original");
    let outside = f.directory.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("payload.bin"), b"original").unwrap();
    let link = f.root.join("redirect");
    let result = std::process::Command::new("cmd.exe")
        .args(["/c", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    f.locations[0].relative_path = "redirect/payload.bin".into();
    assert!(f.acquire().await.is_err());
    assert_eq!(
        std::fs::read(outside.join("payload.bin")).unwrap(),
        b"original"
    );
}

#[tokio::test]
async fn changed_input_late_in_a_bundle_releases_every_preceding_file() {
    let mut f = Fixture::new(b"original");
    let second = f.root.join("second.bin");
    std::fs::write(&second, b"corrupted").unwrap();
    let mut artifact = f.artifacts[0].clone();
    artifact.artifact_id = id();
    artifact.relative_path = "second.bin".into();
    let mut location = f.locations[0].clone();
    location.artifact_id = artifact.artifact_id.clone();
    location.artifact_location_id = id();
    location.relative_path = "second.bin".into();
    f.artifacts.push(artifact);
    f.locations.push(location);
    assert!(f.acquire().await.is_err());
    std::fs::write(&f.path, b"released").unwrap();
    std::fs::write(&second, b"released").unwrap();
}

#[tokio::test]
async fn preexisting_writer_is_rejected_and_equivalent_verified_physical_aliases_are_allowed() {
    use std::os::windows::fs::OpenOptionsExt;
    let mut f = Fixture::new(b"original");
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .share_mode(7)
        .open(&f.path)
        .unwrap();
    assert!(f.acquire().await.is_err());
    drop(writer);
    let mut artifact = f.artifacts[0].clone();
    artifact.artifact_id = id();
    artifact.relative_path = "another.bin".into();
    let mut location = f.locations[0].clone();
    location.artifact_id = artifact.artifact_id.clone();
    location.artifact_location_id = id();
    location.relative_path = location.relative_path.to_uppercase();
    f.artifacts.push(artifact);
    f.locations.push(location);
    let verified = f.acquire().await.unwrap();
    verified.recheck().await.unwrap();
    drop(verified);
    std::fs::write(&f.path, b"released").unwrap();
}

#[tokio::test]
async fn distinct_logical_artifacts_can_share_one_verified_cas_blob() {
    let mut f = Fixture::new(b"shared contents");
    let mut artifact = f.artifacts[0].clone();
    artifact.artifact_id = id();
    artifact.relative_path = "config/another.bin".into();
    let mut location = f.locations[0].clone();
    location.artifact_id = artifact.artifact_id.clone();
    location.artifact_location_id = id();
    f.artifacts.push(artifact);
    f.locations.push(location);
    let input = f.acquire().await.unwrap();
    assert_eq!(input.artifacts().len(), 2);
    input.recheck().await.unwrap();
    assert!(std::fs::write(&f.path, b"cannot change the shared blob").is_err());
    drop(input);
    std::fs::write(&f.path, b"released").unwrap();
}

#[tokio::test]
async fn one_physical_blob_cannot_satisfy_conflicting_logical_descriptors() {
    let mut f = Fixture::new(b"shared contents");
    let mut artifact = f.artifacts[0].clone();
    artifact.artifact_id = id();
    artifact.relative_path = "another.bin".into();
    artifact.sha256 = "0".repeat(64);
    let mut location = f.locations[0].clone();
    location.artifact_id = artifact.artifact_id.clone();
    location.artifact_location_id = id();
    f.artifacts.push(artifact);
    f.locations.push(location);
    assert!(f.acquire().await.is_err());
    std::fs::write(&f.path, b"all pins released").unwrap();
}
