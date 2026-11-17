# -*- coding: utf-8 -*-
"""Reestructura interrupted() por escenario: la ventana de interrupción debe
aterrizar durante el subproceso del runtime (cancel/crash/eof); corrupt se
queda en su ventana de verificación final."""
import io

p = r'crates/larm-adapter-host/src/conformance/onnx_quantize.rs'
s = io.open(p, encoding='utf-8', newline='').read()

a = '''        let mut pending =
            Box::pin(session.call(request, &access, Some(Duration::from_secs(30)), None));
        if how == "cancel" {
            // Este conversor emite un único evento de progreso al final de la
            // cuantización: el cancel debe entrar durante el subproceso del
            // runtime. El job RUNNING en el ledger implica que el primer
            // check_cancelled ya pasó y el siguiente está tras la cuantización.
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                tokio::select! {
                    result = &mut pending => {
                        // La llamada terminó sin llegar a RUNNING: el fallo del
                        // escenario es ese, no el del poll.
                        return Err(result.err().unwrap_or_else(|| invalid("onnx_interrupted_call_succeeded")));
                    }
                    _ = tokio::time::sleep(Duration::from_millis(20)) => {
                        let running: Option<String> = sqlx::query_scalar(
                            "SELECT id FROM ref_job WHERE phase='RUNNING' ORDER BY rowid DESC LIMIT 1",
                        )
                        .fetch_optional(&mut *db)
                        .await
                        .map_err(db_error)?;
                        if running.is_some() {
                            break;
                        }
                        assert!(
                            std::time::Instant::now() < deadline,
                            "el job no llegó a RUNNING: {:?}",
                            sqlx::query_as::<_, (String, String)>(
                                "SELECT id,phase FROM ref_job ORDER BY rowid DESC LIMIT 3",
                            )
                            .fetch_all(&mut *db)
                            .await
                            .map_err(db_error)?
                        );
                    }
                }
            }
            report.check("cancel-ack", json!(true), json!(session.cancel(&id).await?))?;
        }
        let event = loop {
            tokio::select! {event=progress.recv()=>{let event=event.map_err(|_|invalid("onnx_progress_missing"))?;if event.phase=="ONNXQ_TENSOR"{break event;}},_=&mut pending=>return Err(invalid("onnx_completed_before_tensor_checkpoint"))}
        };
        report.check("encoding-progress-id", json!(id), json!(event.request_id))?;
        let active:(String,i64,i64)=sqlx::query_as("SELECT j.id,j.attempt,c.offset FROM ref_job j JOIN ref_checkpoint c ON c.job_id=j.id AND c.attempt=j.attempt WHERE j.phase='RUNNING' AND c.offset>0 ORDER BY j.rowid DESC LIMIT 1").fetch_one(&mut *db).await.map_err(db_error)?;
        report.check("encoding-durable-offset", json!(true), json!(active.2 > 0))?;
        let heartbeat = self
            .call(
                session,
                report,
                "core.heartbeat",
                json!({}),
                &Access::default(),
            )
            .await?;
        report.check(
            "encoding-heartbeat",
            json!(true),
            json!(heartbeat["running_request_ids"]
                .as_array()
                .is_some_and(|v| v.contains(&json!(id)))),
        )?;
        if how == "eof" {
            drop(pending);
            session.restart().await?;
        } else {
            if how == "crash" {
                session.crash()?;
            } else if how == "corrupt" {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(self.input(session))
                    .map_err(super::io)?;
                file.write_all(b"!").map_err(super::io)?;
                file.sync_all().map_err(super::io)?;
            } else if how != "cancel" {
                return Err(invalid("onnx_interrupt_mode_invalid"));
            }
            let failure = pending
                .await
                .err()
                .ok_or_else(|| invalid("onnx_interrupted_call_succeeded"))?;
            report.check(
                if how == "crash" {
                    "worker-crash"
                } else if how == "corrupt" {
                    "combined-fault-response"
                } else {
                    "cancel-result"
                },
                if how == "crash" {
                    json!("ADAPTER_CRASHED")
                } else if how == "corrupt" {
                    json!("product_onnxq_persistence_failed")
                } else {
                    json!("operation_cancelled")
                },
                if how == "crash" {
                    json!(failure.code.as_str())
                } else {
                    json!(failure.details.reason)
                },
            )?;
            report.checkpoints.insert(
                format!("{how}-error"),
                serde_json::to_value(failure).map_err(|_| invalid("onnx_error_invalid"))?,
            );
        }
        report.checkpoints.insert(
            how.into(),
            json!({"rpc_id":id,"job_id":active.0,"attempt":active.1,"durable_offset":active.2}),
        );
        Ok((active.0, active.2))
    }'''

b = '''        let mut pending =
            Box::pin(session.call(request, &access, Some(Duration::from_secs(30)), None));
        // Este conversor emite un único evento de progreso al final de la
        // cuantización y la emisión es atómica: los fallos que deben aterrizar
        // durante el trabajo (cancel/crash/eof) esperan al job RUNNING en el
        // ledger, que implica que el primer check_cancelled ya pasó y el
        // siguiente está tras el subproceso del runtime.
        let mut interrupted_job: Option<String> = None;
        if matches!(how, "cancel" | "crash" | "eof") {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                tokio::select! {
                    result = &mut pending => {
                        // La llamada terminó sin llegar a RUNNING: el fallo del
                        // escenario es ese, no el del poll.
                        return Err(result.err().unwrap_or_else(|| invalid("onnx_interrupted_call_succeeded")));
                    }
                    _ = tokio::time::sleep(Duration::from_millis(20)) => {
                        let running: Option<String> = sqlx::query_scalar(
                            "SELECT id FROM ref_job WHERE phase='RUNNING' ORDER BY rowid DESC LIMIT 1",
                        )
                        .fetch_optional(&mut *db)
                        .await
                        .map_err(db_error)?;
                        if running.is_some() {
                            interrupted_job = running;
                            break;
                        }
                        assert!(
                            std::time::Instant::now() < deadline,
                            "el job no llegó a RUNNING: {:?}",
                            sqlx::query_as::<_, (String, String)>(
                                "SELECT id,phase FROM ref_job ORDER BY rowid DESC LIMIT 3",
                            )
                            .fetch_all(&mut *db)
                            .await
                            .map_err(db_error)?
                        );
                    }
                }
            }
        }
        match how {
            "cancel" => {
                report.check("cancel-ack", json!(true), json!(session.cancel(&id).await?))?;
                let event = loop {
                    tokio::select! {event=progress.recv()=>{let event=event.map_err(|_|invalid("onnx_progress_missing"))?;if event.phase=="ONNXQ_TENSOR"{break event;}},_=&mut pending=>return Err(invalid("onnx_completed_before_tensor_checkpoint"))}
                };
                report.check("encoding-progress-id", json!(id), json!(event.request_id))?;
                let active:(String,i64,i64)=sqlx::query_as("SELECT j.id,j.attempt,c.offset FROM ref_job j JOIN ref_checkpoint c ON c.job_id=j.id AND c.attempt=j.attempt WHERE j.phase='RUNNING' AND c.offset>0 ORDER BY j.rowid DESC LIMIT 1").fetch_one(&mut *db).await.map_err(db_error)?;
                report.check("encoding-durable-offset", json!(true), json!(active.2 > 0))?;
                let heartbeat = self
                    .call(
                        session,
                        report,
                        "core.heartbeat",
                        json!({}),
                        &Access::default(),
                    )
                    .await?;
                report.check(
                    "encoding-heartbeat",
                    json!(true),
                    json!(heartbeat["running_request_ids"]
                        .as_array()
                        .is_some_and(|v| v.contains(&json!(id)))),
                )?;
                let failure = pending
                    .await
                    .err()
                    .ok_or_else(|| invalid("onnx_interrupted_call_succeeded"))?;
                report.check(
                    "cancel-result",
                    json!("operation_cancelled"),
                    json!(failure.details.reason),
                )?;
                report.checkpoints.insert(
                    "cancel-error",
                    serde_json::to_value(failure).map_err(|_| invalid("onnx_error_invalid"))?,
                );
                report.checkpoints.insert(
                    "cancel",
                    json!({"rpc_id":id,"job_id":active.0,"attempt":active.1,"durable_offset":active.2}),
                );
                Ok((active.0, active.2))
            }
            "crash" => {
                // Crash durante el subproceso: sin checkpoint duradero ni
                // parcial (la emisión es atómica); el job queda RUNNING.
                session.crash()?;
                let failure = pending
                    .await
                    .err()
                    .ok_or_else(|| invalid("onnx_interrupted_call_succeeded"))?;
                report.check("worker-crash", json!("ADAPTER_CRASHED"), json!(failure.code.as_str()))?;
                report.checkpoints.insert(
                    "crash-error",
                    serde_json::to_value(failure).map_err(|_| invalid("onnx_error_invalid"))?,
                );
                let job_id = interrupted_job.ok_or_else(|| invalid("onnx_interrupt_mode_invalid"))?;
                report.checkpoints.insert(
                    "crash",
                    json!({"rpc_id":id,"job_id":job_id,"attempt":1,"durable_offset":0}),
                );
                Ok((job_id, 0))
            }
            "eof" => {
                // EOF del cliente: el server cancela los callbacks y el job
                // queda CANCELADO para el siguiente arranque.
                drop(pending);
                session.restart().await?;
                let job_id = interrupted_job.ok_or_else(|| invalid("onnx_interrupt_mode_invalid"))?;
                report.checkpoints.insert(
                    "eof",
                    json!({"rpc_id":id,"job_id":job_id,"attempt":1,"durable_offset":0}),
                );
                Ok((job_id, 0))
            }
            "corrupt" => {
                // La corrupción aterriza en la ventana de verificación final:
                // el subproceso ya consumió el input y el hash del mismo falla.
                let event = loop {
                    tokio::select! {event=progress.recv()=>{let event=event.map_err(|_|invalid("onnx_progress_missing"))?;if event.phase=="ONNXQ_TENSOR"{break event;}},_=&mut pending=>return Err(invalid("onnx_completed_before_tensor_checkpoint"))}
                };
                report.check("encoding-progress-id", json!(id), json!(event.request_id))?;
                let active:(String,i64,i64)=sqlx::query_as("SELECT j.id,j.attempt,c.offset FROM ref_job j JOIN ref_checkpoint c ON c.job_id=j.id AND c.attempt=j.attempt WHERE j.phase='RUNNING' AND c.offset>0 ORDER BY j.rowid DESC LIMIT 1").fetch_one(&mut *db).await.map_err(db_error)?;
                report.check("encoding-durable-offset", json!(true), json!(active.2 > 0))?;
                let heartbeat = self
                    .call(
                        session,
                        report,
                        "core.heartbeat",
                        json!({}),
                        &Access::default(),
                    )
                    .await?;
                report.check(
                    "encoding-heartbeat",
                    json!(true),
                    json!(heartbeat["running_request_ids"]
                        .as_array()
                        .is_some_and(|v| v.contains(&json!(id)))),
                )?;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(self.input(session))
                    .map_err(super::io)?;
                file.write_all(b"!").map_err(super::io)?;
                file.sync_all().map_err(super::io)?;
                let failure = pending
                    .await
                    .err()
                    .ok_or_else(|| invalid("onnx_interrupted_call_succeeded"))?;
                report.check(
                    "combined-fault-response",
                    json!("product_onnxq_persistence_failed"),
                    json!(failure.details.reason),
                )?;
                report.checkpoints.insert(
                    "corrupt-error",
                    serde_json::to_value(failure).map_err(|_| invalid("onnx_error_invalid"))?,
                );
                report.checkpoints.insert(
                    "corrupt",
                    json!({"rpc_id":id,"job_id":active.0,"attempt":active.1,"durable_offset":active.2}),
                );
                Ok((active.0, active.2))
            }
            _ => Err(invalid("onnx_interrupt_mode_invalid")),
        }
    }'''

assert s.count(a) == 1, ('interrupted', s.count(a))
s = s.replace(a, b)

# --- caller del crash: sin parcial previo (emisión atómica) ---
a = '''        let crash = self.plan(session, report).await?;
        let (crashed, offset) = self
            .interrupted(session, db, report, &crash, "crash")
            .await?;
        let partial = session
            .work_dir()
            .join("state/staging")
            .join(format!("{crashed}-1/model.onnx"));
        report.check("crash-durable-bytes-and-resources",json!({"durable":true,"held":2}),json!({"durable":std::fs::metadata(&partial).map_err(super::io)?.len()>=offset as u64,"held":held(db,&crashed).await?}))?;
        let partial_sha = file_digest(&partial)?;
        let incomplete = session
            .work_dir()
            .join("state")
            .join(format!("fault-{crashed}-1.tmp"));'''
b = '''        let crash = self.plan(session, report).await?;
        let (crashed, _offset) = self
            .interrupted(session, db, report, &crash, "crash")
            .await?;
        // La cuantización es atómica: el crash durante el subproceso deja el
        // job RUNNING sin checkpoint ni parcial; las reservas siguen vivas.
        report.check(
            "crash-resources-held",
            json!(2),
            json!(held(db, &crashed).await?),
        )?;
        let incomplete = session
            .work_dir()
            .join("state")
            .join(format!("fault-{crashed}-1.tmp"));'''
assert s.count(a) == 1, ('crash caller', s.count(a))
s = s.replace(a, b)

a = '''        report.check(
            "restart-does-not-resume-old-partial",
            json!(partial_sha),
            json!(file_digest(&partial)?),
        )?;'''
b = '''        // Sin parcial previo que conservar: el reintento publica un modelo
        // nuevo verificado byte a byte por output().'''
assert s.count(a) == 1, ('partial check', s.count(a))
s = s.replace(a, b)

io.open(p, 'w', encoding='utf-8', newline='').write(s)
print('interrupted() y caller del crash reestructurados')
