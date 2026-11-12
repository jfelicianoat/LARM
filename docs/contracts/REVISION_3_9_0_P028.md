# Revisión de contrato LARM 3.9.0 — registro firmado e importación inicial P028

Fecha: 2026-10-24  
Base: paquete 3.8.0  
Estado: revisión aprobada por el operador; materializada en el paquete.

P028 tenía una descripción textual del registro sin formas cerradas para el índice, la firma del envelope, los pins de clave, la revocación persistida ni el journal de importación inicial. Esta revisión publica `RegistryIndexEnvelope`, `RegistryIndexPayload`, `RegistryPackageEntry`, `RegistryRevocation`, `RegistryCheckpoint`, `RegistryRevocationRecord`, `AdapterImportPlan` y `AdapterImportJournal` en `schemas/larm.schema.json`.

El payload firmado usa RFC 8785 UTF-8 y una firma Ed25519 hex minúscula de 64 bytes. La clave debe estar fijada por el operador; el registro no puede autoautorizar claves. El índice fija secuencia, vencimiento, identidad del registry, hash del ZIP y hash P022 del contenido. Las URLs son HTTPS, sin userinfo/query/fragment y con origen coincidente o allowlist exacta. Las fechas de revocación admiten un adelanto máximo de300s. Los checkpoints impiden rollback; las revocaciones son durables y acumulativas. `registry_url=null` deja el registry deshabilitado.

La revisión distingue el hash de bytes ZIP (`sha256`) del hash de archivos del adaptador (`package_sha256`). La firma de cada paquete es exactamente la que debe contener su AdapterManifest y se valida otra vez conforme a P022. Un índice válido sólo autentica los metadatos publicados. No concede OFFICIAL, VALIDATED, permiso de ejecución ni cualificación P026.

La importación inicial reutiliza `operation_journal`, `idempotency` y `event_outbox`: `kind=adapter.import`, payload conforme a `AdapterImportJournal`, job ID UUIDv7, respuesta 202 y replay de 30 días. Las fases y la reconciliación de `PUBLISHING` quedan especificadas; el GET de jobs no revela rutas ni payload privado. La instalación inicial es side-by-side y queda `INSTALLED`, sin seleccionar ni promover.

DDL: añade `adapter_registry_checkpoint` y `adapter_registry_revocation` en `contracts/migrations/0007_p028_signed_adapter_registry.sql`, reflejadas en `contracts/persistence.sql`. Settings añade `registry_allowed_origins`; el default es una lista vacía. No se cambia la firma pública de las rutas existentes.

Compatibilidad: los settings JSON cerrados existentes deben migrarse al agregar `registry_allowed_origins` con `[]`; no se convierte ningún pin anterior en confianza oficial. La configuración usa clave pública Ed25519 en hex minúsculo de 64 caracteres, compatible con `PinnedKey.public_key` de P022.

Dependencias: P024 y P026 permanecen `BLOCKED_REAL` hasta sus pruebas reales. P028 sólo puede cerrar con ambas en `REAL_PASS` y su aceptación HTTPS real ejecutada.
