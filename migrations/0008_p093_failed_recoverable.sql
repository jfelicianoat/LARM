-- H07: una transformación no reanudable interrumpida requiere un estado
-- durable distinto de FAILED para poder ofrecer un reinicio con nuevo preflight.
CREATE TABLE transformation_job_new (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  input_variant_id TEXT NOT NULL REFERENCES model_variant(id) DEFERRABLE INITIALLY DEFERRED,
  output_variant_id TEXT REFERENCES model_variant(id) DEFERRABLE INITIALLY DEFERRED,
  adapter_id TEXT NOT NULL,
  tool_version TEXT NOT NULL,
  parameters TEXT NOT NULL CHECK(json_valid(parameters)),
  plan_hash TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('PLANNED','QUEUED','RUNNING','PAUSING','PAUSED','VERIFYING','COMPLETED','CANCELLING','CANCELLED','FAILED','FAILED_RECOVERABLE')),
  attempt INTEGER NOT NULL CHECK(attempt>=0),
  error TEXT CHECK(json_valid(error))
) STRICT;

INSERT INTO transformation_job_new(
  id,created_at,updated_at,row_version,metadata,input_variant_id,output_variant_id,
  adapter_id,tool_version,parameters,plan_hash,state,attempt,error
)
SELECT
  id,created_at,updated_at,row_version,metadata,input_variant_id,output_variant_id,
  adapter_id,tool_version,parameters,plan_hash,state,attempt,error
FROM transformation_job;

DROP TABLE transformation_job;
ALTER TABLE transformation_job_new RENAME TO transformation_job;
CREATE INDEX idx_transformation_job_recovery ON transformation_job(state,updated_at,id);
