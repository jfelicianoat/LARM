CREATE TABLE adapter_registry_checkpoint (
  registry_id TEXT PRIMARY KEY CHECK(length(registry_id) BETWEEN 1 AND 127),
  registry_url TEXT NOT NULL CHECK(length(registry_url) BETWEEN 1 AND 4096),
  sequence INTEGER NOT NULL CHECK(sequence BETWEEN 1 AND 9007199254740991),
  payload_sha256 TEXT NOT NULL CHECK(length(payload_sha256)=64 AND payload_sha256 NOT GLOB '*[^0-9a-f]*'),
  issued_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  accepted_at TEXT NOT NULL,
  envelope_json TEXT NOT NULL CHECK(json_valid(envelope_json)),
  updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE adapter_registry_revocation (
  registry_id TEXT NOT NULL REFERENCES adapter_registry_checkpoint(registry_id) ON DELETE RESTRICT,
  target_type TEXT NOT NULL CHECK(target_type IN ('PACKAGE_SHA256','PUBLISHER_KEY')),
  target TEXT NOT NULL CHECK(length(target) BETWEEN 1 AND 128),
  revoked_at TEXT NOT NULL,
  reason_code TEXT NOT NULL CHECK(reason_code IN ('COMPROMISED_KEY','MALICIOUS_PACKAGE','INVALID_PROVENANCE','OPERATOR_REQUEST','OTHER')),
  source_sequence INTEGER NOT NULL CHECK(source_sequence BETWEEN 1 AND 9007199254740991),
  PRIMARY KEY(registry_id,target_type,target)
) STRICT;

CREATE INDEX adapter_registry_revocation_lookup ON adapter_registry_revocation(target_type,target);
