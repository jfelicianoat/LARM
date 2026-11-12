CREATE TABLE model_source_operational_snapshot (
  source_id TEXT PRIMARY KEY REFERENCES model_source(id) ON DELETE CASCADE,
  status TEXT NOT NULL CHECK(status IN ('UNKNOWN','SYNCING','HEALTHY','RATE_LIMITED','AUTH_REQUIRED','ERROR')),
  last_sync_at TEXT,
  rate_limit_json TEXT CHECK(rate_limit_json IS NULL OR (json_valid(rate_limit_json) AND json_type(rate_limit_json)='object')),
  updated_at TEXT NOT NULL
) STRICT;

INSERT INTO model_source_operational_snapshot(source_id,status,last_sync_at,rate_limit_json,updated_at)
SELECT id,'UNKNOWN',NULL,NULL,updated_at FROM model_source;

CREATE TRIGGER model_source_operational_snapshot_default
AFTER INSERT ON model_source
WHEN NOT EXISTS (SELECT 1 FROM model_source_operational_snapshot WHERE source_id=NEW.id)
BEGIN
  INSERT INTO model_source_operational_snapshot(source_id,status,last_sync_at,rate_limit_json,updated_at)
  VALUES(NEW.id,'UNKNOWN',NULL,NULL,NEW.updated_at);
END;
