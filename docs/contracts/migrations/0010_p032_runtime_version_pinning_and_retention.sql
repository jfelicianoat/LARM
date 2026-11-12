-- P032 / contrato 3.11.0: ciclo de vida, pins y retención side-by-side.
ALTER TABLE runtime ADD COLUMN retention_policy TEXT NOT NULL DEFAULT '{"keep_last_validated":3}' CHECK(json_valid(retention_policy));
ALTER TABLE runtime_version ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'CANDIDATE' CHECK(lifecycle IN ('CANDIDATE','INSTALLING','VALIDATING','VALIDATED','DEFAULT','RETAINED','BROKEN','REJECTED'));
UPDATE runtime_version SET lifecycle=CASE
  WHEN EXISTS(SELECT 1 FROM runtime r WHERE r.default_version_id=runtime_version.id) THEN 'DEFAULT'
  WHEN promotion='REJECTED' THEN 'REJECTED'
  WHEN EXISTS(SELECT 1 FROM runtime_installation i WHERE i.runtime_version_id=runtime_version.id AND i.state IN ('VALIDATED','ACTIVE','DEGRADED','ROLLED_BACK')) THEN 'VALIDATED'
  WHEN promotion='CANDIDATE' THEN 'CANDIDATE'
  ELSE 'CANDIDATE' END;
CREATE TABLE runtime_version_pin (
  id TEXT PRIMARY KEY, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1), metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  variant_id TEXT NOT NULL REFERENCES model_variant(id) DEFERRABLE INITIALLY DEFERRED,
  launch_profile_id TEXT NOT NULL REFERENCES launch_profile(id) DEFERRABLE INITIALLY DEFERRED,
  runtime_version_id TEXT NOT NULL REFERENCES runtime_version(id) DEFERRABLE INITIALLY DEFERRED,
  UNIQUE(variant_id,launch_profile_id)
) STRICT;
CREATE INDEX runtime_version_pin_version ON runtime_version_pin(runtime_version_id);
CREATE TABLE benchmark_comparison (
  id TEXT PRIMARY KEY, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1), metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  left_benchmark_id TEXT NOT NULL REFERENCES technical_benchmark(id) DEFERRABLE INITIALLY DEFERRED,
  right_benchmark_id TEXT NOT NULL REFERENCES technical_benchmark(id) DEFERRABLE INITIALLY DEFERRED,
  axis TEXT NOT NULL CHECK(axis IN ('runtime_version','model_revision','backend','launch_profile')),
  outcome TEXT NOT NULL CHECK(outcome IN ('PASS','FAIL','INCONCLUSIVE','INCOMPARABLE')),
  left_runtime_version_id TEXT REFERENCES runtime_version(id) DEFERRABLE INITIALLY DEFERRED,
  right_runtime_version_id TEXT REFERENCES runtime_version(id) DEFERRABLE INITIALLY DEFERRED,
  left_fingerprint TEXT NOT NULL CHECK(length(left_fingerprint)=64 AND left_fingerprint NOT GLOB '*[^0-9a-f]*'),
  right_fingerprint TEXT NOT NULL CHECK(length(right_fingerprint)=64 AND right_fingerprint NOT GLOB '*[^0-9a-f]*'),
  differing_components TEXT NOT NULL CHECK(json_valid(differing_components)),
  limitations TEXT NOT NULL CHECK(json_valid(limitations)),
  evidence_refs TEXT NOT NULL CHECK(json_valid(evidence_refs))
) STRICT;
CREATE INDEX benchmark_comparison_versions ON benchmark_comparison(left_runtime_version_id,right_runtime_version_id);
