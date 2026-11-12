# P127 — Implementación contractual del estado operativo de fuentes

Fecha: 2026-10-25  
Paquete base: LARM 3.10.1. Revisión normativa implementada: [3.10.2](../contracts/REVISION_3_10_2_P127.md).  
Estado: código y contrato implementados; aceptación con fuente real autorizada pendiente.

P050 ya proporciona el recorrido durable de `source.sync`; P127 lo conecta al snapshot operativo por fuente. La respuesta `model_source` añade `operational` sin modificar `model_sourceCreate`; `SyncResult` admite un límite observado opcional para adaptadores actualizados y conserva compatibilidad con los ya instalados.

La tabla `model_source_operational_snapshot` separa telemetría de configuración. Las fuentes existentes y nuevas empiezan en `UNKNOWN`. El worker registra `SYNCING` y finaliza con snapshot, cursor del seguimiento, evento y outbox en una transacción. El estado no incrementa la versión de configuración. Errores 401/403 y 429 se distinguen; sólo se conservan códigos seguros y los campos tipados de límite informados por el adaptador.

Fuentes separa activación y estado operativo, presenta valores ausentes sin estimarlos y muestra la última sincronización registrada. El estado se alimenta de seguimientos programados P050; no hay una segunda ruta manual ni se usa `source.auth_probe` como sincronización.

La evidencia de código y pruebas se irá anotando aquí al cerrar cada verificación. P127 continúa `BLOCKED_REAL` hasta probar un origen autorizado y satisfacer P024/P045/P049/P141/P028.
