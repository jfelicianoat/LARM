//! Los ejemplos de `docs/adapters/examples` (validados también con
//! `jsonschema` por `tools/p020_schemas.py`) dan el mismo veredicto con los
//! tipos del SDK: el schema y el código no divergen.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::protocol::{
    decode, CancelRequest, CancelResult, Handshake, HandshakeResult, Heartbeat, PROTOCOL_VERSION,
};

fn verdict(kind: &str, text: &str) -> Option<bool> {
    let ok = match kind {
        "AdapterManifest" => AdapterManifest::parse(text).is_ok(),
        "Handshake" => serde_json::from_str::<Handshake>(text)
            .is_ok_and(|h| h.protocol_version == PROTOCOL_VERSION),
        "HandshakeResult" => serde_json::from_str::<HandshakeResult>(text)
            .is_ok_and(|h| h.protocol_version == PROTOCOL_VERSION),
        "CancelRequest" => serde_json::from_str::<CancelRequest>(text)
            .is_ok_and(|c| larm_domain::bridge::is_uuid_v7(&c.request_id)),
        "CancelResult" => serde_json::from_str::<CancelResult>(text).is_ok(),
        "Heartbeat" => serde_json::from_str::<Heartbeat>(text).is_ok(),
        k if k.starts_with("frame.") => {
            // El decoder de producción decide; el oráculo no corrige su veredicto.
            // Las fixtures JSON legibles se convierten a una única línea NDJSON.
            let value: serde_json::Value = serde_json::from_str(text).unwrap();
            decode(value.to_string().as_bytes()).is_ok()
        }
        _ => return None,
    };
    Some(ok)
}

#[test]
fn ejemplos_validos_e_invalidos() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/adapters/examples");
    let mut checked = 0;
    for (kind, expected) in [("valid", true), ("invalid", false)] {
        for entry in std::fs::read_dir(root.join(kind)).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let target = name.split("--").next().unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            let got =
                verdict(target, &text).unwrap_or_else(|| panic!("tipo sin oráculo: {target}"));
            assert_eq!(got, expected, "{kind}/{name}");
            checked += 1;
        }
    }
    assert!(checked >= 20, "{checked}");
}
