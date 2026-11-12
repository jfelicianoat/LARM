//! Requests públicos P028 para exportar paquetes de adaptadores.
use serde::{Deserialize, Serialize};

use crate::bridge::is_uuid_v7;
use crate::error::{ErrorCode, LarmError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterExport {
    pub adapter_installation_id: String,
    pub destination_path: String,
}

impl AdapterExport {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.adapter_installation_id)
            || self.destination_path.trim().is_empty()
            || self.destination_path.chars().any(char::is_control)
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Solicitud de exportación no válida",
                "adapter_export_invalid",
            ));
        }
        Ok(())
    }
}

/// Forma pública de `CommandReceipt` para mantener los crates de dominio
/// independientes de los tipos de respuesta HTTP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterExportReceipt {
    pub resource_id: String,
    pub row_version: i64,
    pub event_id: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn adapter_export_cerrado_y_validado() {
        let valid = AdapterExport {
            adapter_installation_id: "0192f000-0000-7000-8000-000000000001".into(),
            destination_path: "C:/exports/adapter.zip".into(),
        };
        valid.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&valid).unwrap(),
            serde_json::json!({
                "adapter_installation_id": valid.adapter_installation_id,
                "destination_path": valid.destination_path,
            })
        );
        for bad in [
            AdapterExport {
                adapter_installation_id: "not-a-uuid".into(),
                destination_path: "C:/exports/adapter.zip".into(),
            },
            AdapterExport {
                adapter_installation_id: valid.adapter_installation_id.clone(),
                destination_path: "  ".into(),
            },
            AdapterExport {
                adapter_installation_id: valid.adapter_installation_id.clone(),
                destination_path: "C:/exports/\nunsafe.zip".into(),
            },
        ] {
            assert!(bad.validate().is_err());
        }
        let mut extra = serde_json::to_value(valid).unwrap();
        extra["trusted"] = serde_json::json!(true);
        assert!(serde_json::from_value::<AdapterExport>(extra).is_err());
    }
}
