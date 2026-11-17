# Guía de backup, restauración y actualización de LARM (P133)

## Qué incluye un backup (spec §133)

El bundle de backup/export incluye el **snapshot consistente de la base**
(`VACUUM INTO`, válido con WAL), la configuración del operador y los
manifiestos de versiones (`*.lock.json`), con un `manifest.json`
determinista que registra la versión del bundle, la versión del contrato
sellado vigente y el **SHA-256 de cada fichero**. La base no contiene blobs
(los modelos viven en el CAS del almacenamiento) ni secretos (los tokens
viven en el Credential Manager): los modelos se reindexan desde sus
ubicaciones y las credenciales se recrean al restaurar.

## Copia de seguridad

- **Automática antes de actualizar**: `POST /api/v1/updates/larm` crea
  `backups/pre-update-<fecha>.sqlite` verificado y el bundle
  `backups/bundle-<fecha>/` antes de tocar nada.
- **Manual**: `larm-daemon db-backup` (P004) o `VACUUM INTO` equivalente; el
  snapshot es consistente aunque la base esté en WAL.

## Restauración

`larm-daemon db-restore --from <backup>` (P004) o `larm_db::backup::restore`:
comprueba el backup antes de tocar nada, aparta la base actual como
`*.pre-restore`, coloca la copia y verifica integridad y filas tabla a tabla;
si algo falla devuelve la base previa a su sitio.

**Import de un bundle**: se valida contra `manifest.json` — versión del
bundle y del contrato (`backup_import_version_unsupported`), presencia de
ficheros (`backup_import_incomplete`) y digest byte a byte
(`backup_import_digest_mismatch`). El import validado se copia a **staging**;
nunca toca la instalación viva: el operador revisa el staging y fusiona.

## Rebuild Registry (spec §134, P018)

Si la base se pierde, `POST /api/v1/recovery/rebuild` (o el CLI
`db-rebuild`) escanea las raíces de las ubicaciones de almacenamiento
registradas, reconstruye el registro en una **base de staging** y deja el
informe (`recuperadas`, `incompletas`, `inválidas`, `huérfanos`, `conflictos`).
Las variantes con todos sus blobs se registran con identidad propia; las que
tienen blobs ausentes o corruptos quedan en el informe sin inventar nada. El
operador revisa el staging y lo fusiona con un restore verificado.

## Actualización de LARM (spec §132)

1. `POST /api/v1/updates/larm` con `ExecutePlan` (`plan_id`, `plan_hash` y
   `approval_id` opcional). **Con leases o jobs activas el updater no fuerza
   nada sin confirmación** (`update_blocked_active_leases`); la confirmación
   es la aprobación ligada al `plan_hash`.
2. El job del update: verifica el artefacto staged
   (`updates/staging/larm-update.bin` contra su `*.sha256`; discrepancia →
   `update_staged_hash_mismatch`), hace el backup previo verificado y escribe
   el bundle. **El reinicio y la migración los aplica el arranque siguiente**
   (P132): las migraciones son reversibles cuando es viable y siempre hay
   backup previo a una migración incompatible.
3. Rollback: el binario anterior se conserva side-by-side cuando es posible;
   si la base nueva no arranca, `db-restore` desde el backup íntegro del
   bundle devuelve la instalación al estado previo compatible.
