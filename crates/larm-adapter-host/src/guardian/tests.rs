#![allow(clippy::unwrap_used)]
use super::*;

#[tokio::test]
async fn pipe_ocupado_espera_instancia_nueva_y_ausencia_sigue_siendo_error() {
    use tokio::net::windows::named_pipe::ServerOptions;
    let name = format!(r"\\.\pipe\LARM-guardian-unit-{}", uuid::Uuid::now_v7());
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&name)
        .unwrap();
    let first = ClientOptions::new().open(&name).unwrap();
    let pending_name = name.clone();
    let pending = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(1), connect_pipe(&pending_name)).await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!pending.is_finished());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), connect_pipe(&name))
            .await
            .is_err()
    );
    server.connect().await.unwrap();
    let replacement = ServerOptions::new().create(&name).unwrap();
    let second = pending.await.unwrap().unwrap().unwrap();
    replacement.connect().await.unwrap();
    drop((first, second, server, replacement));
    assert_eq!(
        connect_pipe(&name).await.unwrap_err().details.reason,
        "guardian_io_unavailable"
    );
}

fn challenge() -> (SigningKey, Challenge) {
    let key = SigningKey::from_bytes(&[31; 32]);
    let descriptor = GuardianDescriptor {
        schema: 1,
        session_id: uuid::Uuid::now_v7().to_string(),
        fence: TransformationAttemptFence {
            job_id: uuid::Uuid::now_v7().to_string(),
            claim_id: uuid::Uuid::now_v7().to_string(),
            generation: 3,
            attempt: 4,
            plan_hash: "a".repeat(64),
        },
        public_key: hex::encode(key.verifying_key().as_bytes()),
        session_identity: None,
        native_owner: None,
    };
    (
        key,
        Challenge {
            descriptor,
            nonce: uuid::Uuid::new_v4().to_string(),
            action: Action::Observe,
            control_signature: None,
        },
    )
}

#[test]
fn accion_progress_viaja_firmada_y_su_resultado_se_decodifica() {
    let (key, mut challenge) = challenge();
    challenge.action = Action::Progress;
    challenge.control_signature = Some(hex::encode(
        key.sign(&control_bytes(&challenge).unwrap()).to_bytes(),
    ));
    let wire = serde_json::to_string(&challenge).unwrap();
    let round: Challenge = serde_json::from_str(&wire).unwrap();
    assert_eq!(serde_json::to_string(&round).unwrap(), wire);
    assert!(matches!(round.action, Action::Progress));
    let observation = Observation {
        challenge: round,
        state: GuardianState::Closed,
        guardian_pid: 4242,
        worker_pid: 4243,
        active_rpcs: 0,
        result: Some(Ok(serde_json::json!([
            {"request_id": uuid::Uuid::now_v7().to_string(),
             "phase": "TRANSFORM_ENCODING",
             "completed_bytes": 512,
             "total_bytes": 1024,
             "message_code": "reference_block_durable"},
        ]))),
    };
    let signature = hex::encode(key.sign(&signed_bytes(&observation).unwrap()).to_bytes());
    let signed = SignedObservation {
        observation,
        signature,
    };
    let bytes = serde_json::to_vec(&signed).unwrap();
    let back: SignedObservation = serde_json::from_slice(&bytes).unwrap();
    let events: Vec<larm_adapter_sdk::protocol::Progress> =
        serde_json::from_value(back.observation.result.unwrap().unwrap()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].phase, "TRANSFORM_ENCODING");
    assert_eq!(events[0].completed_bytes, Some(512));
    assert_eq!(events[0].total_bytes, Some(1024));
}

#[test]
fn descriptor_registrado_reconstruye_binding_sin_admitir_identidades_divergentes() {
    let (_, challenge) = challenge();
    let owner = TransformationNativeOwner {
        schema_version: "1.0".into(),
        session_id: challenge.descriptor.session_id.clone(),
        public_key: challenge.descriptor.public_key.clone(),
        queued_row_version: 1,
        fence: challenge.descriptor.fence.clone(),
        adapter_id: "reference.transform".into(),
        boot_session: 0,
        binding: larm_domain::transformations::TransformationExecutionBinding {
            adapter_installation_id: uuid::Uuid::now_v7().to_string(),
            conformance_run_id: uuid::Uuid::now_v7().to_string(),
            conformance_run_sha256: "a".repeat(64),
            profile_sha256: "b".repeat(64),
            worker_plan_id: uuid::Uuid::now_v7().to_string(),
            worker_plan_hash: "c".repeat(64),
        },
    };
    let descriptor = GuardianDescriptor::from_native_owner(owner.clone()).unwrap();
    assert_eq!(descriptor.schema, 3);
    assert_eq!(descriptor.native_owner.as_ref(), Some(&owner));
    assert_eq!(
        descriptor.session_identity.as_ref().unwrap().run_id,
        owner.binding.conformance_run_id
    );
    descriptor.validate().unwrap();
    for change in 0..8 {
        let mut bad = descriptor.clone();
        match change {
            0 => bad.schema = 2,
            1 => bad.session_id = uuid::Uuid::now_v7().to_string(),
            2 => bad.fence.generation += 1,
            3 => bad.native_owner = None,
            4 => bad.session_identity.as_mut().unwrap().profile_sha256 = "d".repeat(64),
            5 => bad.native_owner.as_mut().unwrap().adapter_id = "other.transform".into(),
            6 => bad.native_owner.as_mut().unwrap().queued_row_version = 0,
            _ => {
                bad.native_owner.as_mut().unwrap().public_key =
                    hex::encode(SigningKey::from_bytes(&[33; 32]).verifying_key().as_bytes())
            }
        }
        assert!(bad.validate().is_err(), "mutation {change}");
    }
    let value = serde_json::to_value(&descriptor).unwrap();
    assert!(value.get("signing_seed").is_none() && value.get("launch").is_none());
}

fn response(key: &SigningKey, challenge: Challenge) -> SignedObservation {
    let observation = Observation {
        challenge,
        state: GuardianState::Closed,
        guardian_pid: 20,
        worker_pid: 21,
        active_rpcs: 0,
        result: None,
    };
    let signature = hex::encode(key.sign(&signed_bytes(&observation).unwrap()).to_bytes());
    SignedObservation {
        observation,
        signature,
    }
}

#[test]
fn firma_vincula_nonce_intento_accion_y_estado() {
    let (key, expected) = challenge();
    assert_eq!(
        verify(
            &key.verifying_key(),
            &expected,
            response(&key, expected.clone())
        )
        .unwrap(),
        GuardianState::Closed
    );
    for change in 0..7 {
        let mut reply = response(&key, expected.clone());
        match change {
            0 => reply.observation.challenge.nonce = uuid::Uuid::new_v4().to_string(),
            1 => reply.observation.challenge.descriptor.fence.generation += 1,
            2 => {
                reply.observation.challenge.descriptor.fence.claim_id =
                    uuid::Uuid::now_v7().to_string()
            }
            3 => reply.observation.challenge.action = Action::Release,
            4 => reply.observation.state = GuardianState::Running,
            5 => reply.observation.worker_pid += 1,
            _ => reply.observation.challenge.descriptor.public_key = "0".repeat(64),
        }
        assert!(verify(&key.verifying_key(), &expected, reply).is_err());
    }
}

#[test]
fn otro_firmante_y_firmas_malformadas_se_rechazan() {
    let (key, expected) = challenge();
    let other = SigningKey::from_bytes(&[32; 32]);
    assert!(verify(
        &key.verifying_key(),
        &expected,
        response(&other, expected.clone())
    )
    .is_err());
    for signature in [
        "".to_string(),
        "f".repeat(128),
        "00".repeat(63),
        "nohex".to_string(),
    ] {
        let mut reply = response(&key, expected.clone());
        reply.signature = signature;
        assert!(verify(&key.verifying_key(), &expected, reply).is_err());
    }
}

#[test]
fn descriptor_no_admite_pid_flag_closed_ni_nombres_de_pipe_arbitrarios() {
    let (_, expected) = challenge();
    let mut json = serde_json::to_value(&expected.descriptor).unwrap();
    for (name, value) in [
        ("closed", serde_json::json!(true)),
        ("pid", serde_json::json!(42)),
        ("pipe", serde_json::json!(r"\\remote\pipe\fake")),
    ] {
        json[name] = value;
        assert!(serde_json::from_value::<GuardianDescriptor>(json.clone()).is_err());
        json.as_object_mut().unwrap().remove(name);
    }
    for change in 0..4 {
        let mut descriptor = expected.descriptor.clone();
        match change {
            0 => descriptor.schema = 2,
            1 => descriptor.session_id = "../arbitrary".into(),
            2 => descriptor.fence.attempt = 0,
            _ => descriptor.public_key = "invalid".into(),
        }
        assert!(descriptor.validate().is_err());
    }
}

#[tokio::test]
async fn frame_excesivo_se_rechaza_antes_del_cuerpo_y_eof_no_es_prueba() {
    let (mut write, mut read) = tokio::io::duplex(16);
    write.write_u32_le(MAX_MESSAGE as u32 + 1).await.unwrap();
    assert!(tokio::time::timeout(
        Duration::from_millis(100),
        read_message::<Challenge>(&mut read)
    )
    .await
    .unwrap()
    .is_err());
    drop(write);
    assert!(read_message::<SignedObservation>(&mut read).await.is_err());
}

#[tokio::test]
async fn descriptor_persistido_sin_supervisor_vivo_no_acredita_cierre() {
    let (_, expected) = challenge();
    assert!(NativeGuardian::attach(expected.descriptor).await.is_err());
}

#[tokio::test]
async fn bootstrap_sin_datos_o_excesivo_falla_cerrado() {
    let mut reader = BufReader::new(&b""[..]);
    assert!(GuardianBootstrap::read_from(&mut reader).await.is_err());
    let bytes = vec![b'x'; MAX_BOOTSTRAP + 1];
    let mut reader = BufReader::new(&bytes[..]);
    assert!(GuardianBootstrap::read_from(&mut reader).await.is_err());
}

#[tokio::test]
async fn bootstrap_vincula_clave_y_manifiesto_y_exige_linea_completa() {
    let (_, expected) = challenge();
    let manifest = larm_adapter_sdk::manifest::AdapterManifest::parse(include_str!(
        "../../../../tests/fixtures/fake_adapter/manifest.json"
    ))
    .unwrap();
    let bootstrap = GuardianBootstrap::new(
        WorkerSpec {
            package_dir: "package".into(),
            manifest,
            environment: vec![],
            work_dir: "work".into(),
        },
        expected.descriptor.fence,
    )
    .unwrap();
    let mut bytes = Vec::new();
    bootstrap.write_to(&mut bytes).await.unwrap();
    let restored = GuardianBootstrap::read_from(&mut BufReader::new(bytes.as_slice()))
        .await
        .unwrap();
    assert!(restored.descriptor == bootstrap.descriptor);
    assert!(
        GuardianBootstrap::read_from(&mut BufReader::new(&bytes[..bytes.len() - 1]))
            .await
            .is_err()
    );
    for change in 0..2 {
        let mut json = serde_json::to_value(&bootstrap).unwrap();
        if change == 0 {
            json["signing_seed"][0] =
                serde_json::json!(json["signing_seed"][0].as_u64().unwrap() ^ 1);
        } else {
            json["launch"]["manifest"]["methods"] = serde_json::json!([]);
        }
        let mut line = serde_json::to_vec(&json).unwrap();
        line.push(b'\n');
        assert!(
            GuardianBootstrap::read_from(&mut BufReader::new(line.as_slice()))
                .await
                .is_err()
        );
    }
}

#[test]
fn control_exige_clave_privada_y_vincula_reto_accion_y_descriptor() {
    let (key, mut request) = challenge();
    assert!(verify_control(&key.verifying_key(), &request).is_err());
    request.control_signature = Some(hex::encode(
        key.sign(&control_bytes(&request).unwrap()).to_bytes(),
    ));
    verify_control(&key.verifying_key(), &request).unwrap();
    for change in 0..4 {
        let mut wrong = request.clone();
        match change {
            0 => wrong.nonce = uuid::Uuid::new_v4().to_string(),
            1 => wrong.action = Action::Stop,
            2 => wrong.descriptor.fence.attempt += 1,
            _ => wrong.control_signature = Some("0".repeat(128)),
        }
        assert!(verify_control(&key.verifying_key(), &wrong).is_err());
    }
    assert!(verify_control(&SigningKey::from_bytes(&[41; 32]).verifying_key(), &request).is_err());
}

#[test]
fn respuesta_firma_tambien_el_resultado_y_error_rpc() {
    let (key, expected) = challenge();
    for value in [
        Some(Ok(serde_json::json!({"forged":true}))),
        Some(Err(error("forged"))),
    ] {
        let mut reply = response(&key, expected.clone());
        reply.observation.result = value;
        assert!(verify(&key.verifying_key(), &expected, reply).is_err());
    }
}
