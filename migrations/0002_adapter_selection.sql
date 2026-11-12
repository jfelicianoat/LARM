-- P024: selección de versión independiente del lifecycle del contrato.
-- La migración 0001 y sus entidades publicadas conservan su contenido.
CREATE UNIQUE INDEX adapter_identity_version ON adapter_installation(
    adapter_id COLLATE NOCASE, version COLLATE NOCASE
);

CREATE TABLE adapter_selection (
    adapter_id TEXT PRIMARY KEY COLLATE NOCASE,
    active_installation_id TEXT REFERENCES adapter_installation(id) ON DELETE RESTRICT,
    previous_installation_id TEXT REFERENCES adapter_installation(id) ON DELETE RESTRICT,
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    row_version INTEGER NOT NULL CHECK(row_version >= 1),
    updated_at TEXT NOT NULL,
    CHECK(previous_installation_id IS NULL OR active_installation_id IS NOT NULL),
    CHECK(previous_installation_id IS NULL OR previous_installation_id <> active_installation_id),
    CHECK(enabled = 0 OR active_installation_id IS NOT NULL)
) STRICT;
