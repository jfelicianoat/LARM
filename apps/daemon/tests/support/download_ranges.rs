//! 416 sólo cierra un archivo después de verificar su contenido local completo.
use super::*;
use std::io::{Seek, SeekFrom};

async fn pause_at_full_checkpoint(case: &Case<'_>, job: &Job, size: u64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let snapshot = job_snapshot(case.db, &job.id).await;
        let offset = snapshot.2["artifacts"][&job.artifact_id]["durable_offset"]
            .as_str()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        assert!(
            !matches!(snapshot.0.as_str(), "FAILED" | "COMPLETED"),
            "sin oportunidad de pausar: {snapshot:?}"
        );
        if offset >= size - (512 << 10) && offset < size {
            break;
        }
        assert!(Instant::now() < deadline, "sin último bloque: {snapshot:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    control(case, job, "pause").await;
    let paused = wait_state(case.db, &job.id, "PAUSED").await;
    let saved = paused.2["artifacts"][&job.artifact_id].clone();
    assert_eq!(saved["durable_offset"], size.to_string());
    assert_eq!(std::fs::metadata(&job.partial).unwrap().len(), size);
    saved
}

async fn published_bytes(case: &Case<'_>, job: &Job) -> Vec<u8> {
    let relative: String =
        sqlx::query_scalar("SELECT relative_path FROM artifact_location WHERE artifact_id=?")
            .bind(&job.artifact_id)
            .fetch_one(&case.db.pool)
            .await
            .unwrap();
    std::fs::read(Path::new(&case.location.canonical_path).join(relative)).unwrap()
}

async fn full_file_416(case: &Case<'_>, index: usize, corrupt: bool) -> Value {
    source_mode(case.data, "slow");
    let expected = std::fs::read(case.data["expected_files"][index].as_str().unwrap()).unwrap();
    let sha = hex::encode(Sha256::digest(&expected));
    let job = new_job(case, index).await;
    let saved = pause_at_full_checkpoint(case, &job, expected.len() as u64).await;
    let mut before_hash = sha.clone();
    if corrupt {
        let partial = std::fs::canonicalize(&job.partial).unwrap();
        assert!(partial.starts_with(std::fs::canonicalize(&case.location.canonical_path).unwrap()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(partial)
            .unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&[expected[0] ^ 0xff]).unwrap();
        file.sync_all().unwrap();
        drop(file);
        before_hash = hex::encode(Sha256::digest(std::fs::read(&job.partial).unwrap()));
        assert_ne!(before_hash, sha);
    }
    let before = requests(case.data).len();
    source_mode(case.data, "range_end");
    control(case, &job, "resume").await;
    let phase = if corrupt { "FAILED" } else { "COMPLETED" };
    let (done, samples) = terminal(case, &job, phase).await;
    let wire = requests(case.data)
        .into_iter()
        .skip(before)
        .filter(|r| r["path"] == format!("/artifact/{index}"))
        .collect::<Vec<_>>();
    assert_eq!(wire.len(), 1, "416 no entrega más bytes del artefacto");
    assert_eq!(wire[0]["status"], 416);
    assert_eq!(wire[0]["range"], format!("bytes={}-", expected.len()));
    assert_eq!(wire[0]["if_range"], saved["resume_validator"]["value"]);
    assert_eq!(wire[0]["range_total"], expected.len().to_string());
    assert_eq!(
        wire[0]["response_body_sha256"],
        hex::encode(Sha256::digest(b""))
    );
    assert!(!job.partial.exists());
    if corrupt {
        assert_eq!(done["error"]["code"], "INTEGRITY_FAILED");
        assert_eq!(done["error"]["retryable"], false);
        let (locations,provenance):(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM artifact_location WHERE artifact_id=?),(SELECT count(*) FROM artifact_provenance WHERE artifact_id=?)")
            .bind(&job.artifact_id).bind(&job.artifact_id).fetch_one(&case.db.pool).await.unwrap();
        assert_eq!((locations, provenance), (0, 0));
        for hash in [&sha, &before_hash] {
            assert!(!Path::new(&case.location.canonical_path)
                .join(larm_storage::cas::blob_relative(hash).unwrap())
                .exists());
        }
    } else {
        assert_eq!(published_bytes(case, &job).await, expected);
        assert_eq!(done["attempt"], 2);
    }
    let closure = durable_closure(case, &job, phase).await;
    let stable = requests(case.data);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        requests(case.data),
        stable,
        "sin lecturas después del terminal 416"
    );
    source_mode(case.data, "normal");
    eprintln!("P051 ranges: 416 de archivo completo, corrupto={corrupt}, resultado {phase}");
    json!({"job_id":job.id,"saved_checkpoint":saved,"file_bytes_before_resume":expected.len().to_string(),"expected_sha256":sha,"partial_sha256_before_resume":before_hash,"requests":wire,"terminal":done,"progress_samples":samples,"closure":closure,"partial_removed":true,"no_extra_reads_after_terminal":true,"corrupt_partial_rejected":corrupt})
}

async fn rejected_short_range(case: &Case<'_>) -> Value {
    source_mode(case.data, "slow");
    let job = new_job(case, 5).await;
    wait_bytes(case, &job).await;
    control(case, &job, "pause").await;
    let paused = wait_state(case.db, &job.id, "PAUSED").await;
    let saved = paused.2["artifacts"][&job.artifact_id].clone();
    let offset = saved["durable_offset"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let expected = std::fs::read(case.data["expected_files"][5].as_str().unwrap()).unwrap();
    assert!(offset > 0 && offset < (expected.len() as u64));
    let before = requests(case.data).len();
    source_mode(case.data, "range_once");
    control(case, &job, "resume").await;
    let (done, samples) = terminal(case, &job, "COMPLETED").await;
    let wire = requests(case.data)
        .into_iter()
        .skip(before)
        .filter(|r| r["path"] == "/artifact/5")
        .collect::<Vec<_>>();
    assert_eq!(wire[0]["status"], 416);
    assert_eq!(wire[0]["range"], format!("bytes={offset}-"));
    assert!(wire[1]["range"].is_null() && wire[1]["if_range"].is_null());
    assert!(samples
        .iter()
        .any(|c| c["durable_offset"] == "0" && c["resume_validator"].is_null()));
    assert_eq!(published_bytes(case, &job).await, expected);
    assert!(!job.partial.exists());
    let closure = durable_closure(case, &job, "COMPLETED").await;
    source_mode(case.data, "normal");
    eprintln!("P051 ranges: 416 prematuro reinicia a cero y verifica el SHA fijado");
    json!({"job_id":job.id,"saved_checkpoint":saved,"requests":wire,"progress_samples":samples,"terminal":done,"closure":closure,"expected_sha256":hex::encode(Sha256::digest(expected)),"partial_removed":true})
}

pub(super) async fn exercise(case: &Case<'_>) -> Value {
    let complete = full_file_416(case, 3, false).await;
    let corrupt = full_file_416(case, 4, true).await;
    let short = rejected_short_range(case).await;
    json!({"complete_verified":complete,"complete_corrupt":corrupt,"short_restarted":short})
}
