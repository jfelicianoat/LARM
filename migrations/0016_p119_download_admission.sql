-- P119 (revisión 3.16.0): política opt-in de descargas automáticas
-- (singleton versionada) y historial inmutable de admisiones con day_key
-- para la cuota diaria y revocación inmediata. Sin política habilitada el
-- watcher sólo notifica; las admitidas entran como BACKGROUND.

CREATE TABLE download_admission_policy (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
  source_allowlist TEXT NOT NULL CHECK(json_valid(source_allowlist)),
  capability_allowlist TEXT NOT NULL CHECK(json_valid(capability_allowlist)),
  license_allowlist TEXT NOT NULL CHECK(json_valid(license_allowlist)),
  min_trust TEXT NOT NULL CHECK(min_trust IN ('CANDIDATE','TECHNICALLY_APPROVED','EVALUATION_APPROVED')),
  daily_quota_bytes TEXT CHECK(daily_quota_bytes IS NULL OR length(daily_quota_bytes)<=19),
  storage_location_id TEXT,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE download_admission (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  variant_id TEXT NOT NULL,
  source_id TEXT NOT NULL,
  estimated_bytes TEXT NOT NULL,
  decision TEXT NOT NULL CHECK(decision IN ('ADMITTED','BLOCKED','REVOKED')),
  reason TEXT NOT NULL,
  policy_row_version INTEGER NOT NULL CHECK(policy_row_version>=1),
  day_key TEXT NOT NULL,
  job_id TEXT
) STRICT;

CREATE INDEX download_admission_day ON download_admission(day_key, decision);

CREATE TRIGGER download_admission_immutable_update
BEFORE UPDATE ON download_admission
WHEN NEW.decision != 'REVOKED' OR OLD.decision == 'REVOKED'
BEGIN
  SELECT RAISE(ABORT, 'download admissions are immutable once terminal');
END;

CREATE TRIGGER download_admission_immutable_delete
BEFORE DELETE ON download_admission
BEGIN
  SELECT RAISE(ABORT, 'download admissions are immutable');
END;
