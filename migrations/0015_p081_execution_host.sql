-- P081 (revisión 3.15.0): registro read-only de hosts de ejecución
-- virtualizados observados (WSL2 y motor de contenedores) y la referencia
-- opcional desde una instalación runtime. El registro nunca habilita
-- ejecución ni acredita compatibilidad: es observación estructurada.

CREATE TABLE execution_host (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  kind TEXT NOT NULL CHECK(kind IN ('WSL2','CONTAINER')),
  name TEXT NOT NULL CHECK(length(name)>=1 AND length(name)<=256),
  engine_version TEXT,
  wsl_version INTEGER CHECK(wsl_version IN (1,2)),
  default_host INTEGER NOT NULL CHECK(default_host IN (0,1)),
  status TEXT NOT NULL CHECK(status IN ('ONLINE','OFFLINE','UNKNOWN')),
  evidence_refs TEXT NOT NULL CHECK(json_valid(evidence_refs))
) STRICT;

ALTER TABLE runtime_installation ADD COLUMN execution_host_id TEXT REFERENCES execution_host(id) DEFERRABLE INITIALLY DEFERRED;
