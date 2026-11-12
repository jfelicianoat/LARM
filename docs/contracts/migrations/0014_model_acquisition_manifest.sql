-- P044/P052 / contrato 3.12.0: snapshots remotos; nunca equivalen a bytes verificados.
CREATE TABLE model_acquisition_manifest (
  id TEXT PRIMARY KEY,
  variant_id TEXT NOT NULL REFERENCES model_variant(id) DEFERRABLE INITIALLY DEFERRED,
  revision_id TEXT NOT NULL REFERENCES model_revision(id) DEFERRABLE INITIALLY DEFERRED,
  source_id TEXT NOT NULL REFERENCES model_source(id) DEFERRABLE INITIALLY DEFERRED,
  source_reference TEXT NOT NULL CHECK(json_valid(source_reference) AND json_type(source_reference)='object'),
  upstream_manifest_hash TEXT NOT NULL CHECK(length(upstream_manifest_hash)=64 AND upstream_manifest_hash NOT GLOB '*[^0-9a-f]*'),
  snapshot_hash TEXT NOT NULL CHECK(length(snapshot_hash)=64 AND snapshot_hash NOT GLOB '*[^0-9a-f]*'),
  observed_at TEXT NOT NULL,
  required_artifact_ids TEXT NOT NULL CHECK(json_valid(required_artifact_ids) AND json_type(required_artifact_ids)='array' AND json_array_length(required_artifact_ids)>0),
  optional_artifact_ids TEXT NOT NULL CHECK(json_valid(optional_artifact_ids) AND json_type(optional_artifact_ids)='array'),
  artifacts TEXT NOT NULL CHECK(json_valid(artifacts) AND json_type(artifacts)='array' AND json_array_length(artifacts)>0),
  adapter_installation_id TEXT NOT NULL REFERENCES adapter_installation(id) DEFERRABLE INITIALLY DEFERRED,
  adapter_installation_revision TEXT NOT NULL CHECK(length(adapter_installation_revision) BETWEEN 1 AND 19 AND adapter_installation_revision NOT GLOB '*[^0-9]*' AND substr(adapter_installation_revision,1,1)<>'0'),
  adapter_installation_sha256 TEXT NOT NULL CHECK(length(adapter_installation_sha256)=64 AND adapter_installation_sha256 NOT GLOB '*[^0-9a-f]*'),
  conformance_run_id TEXT NOT NULL,
  conformance_run_sha256 TEXT NOT NULL CHECK(length(conformance_run_sha256)=64 AND conformance_run_sha256 NOT GLOB '*[^0-9a-f]*'),
  UNIQUE(variant_id,snapshot_hash)
) STRICT;
CREATE INDEX model_acquisition_manifest_variant ON model_acquisition_manifest(variant_id,observed_at,id);
CREATE TRIGGER model_acquisition_manifest_immutable_update BEFORE UPDATE ON model_acquisition_manifest
BEGIN SELECT RAISE(ABORT,'acquisition_manifest_immutable'); END;
CREATE TRIGGER model_acquisition_manifest_immutable_delete BEFORE DELETE ON model_acquisition_manifest
BEGIN SELECT RAISE(ABORT,'acquisition_manifest_immutable'); END;
