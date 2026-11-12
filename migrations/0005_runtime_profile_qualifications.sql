-- P099: probe evidence is a separate immutable record; acquisition locks remain UNQUALIFIED.
CREATE TABLE runtime_profile_qualification (
  lock_sha256 TEXT PRIMARY KEY REFERENCES runtime_profile_lock(lock_sha256),
  qualification_sha256 TEXT NOT NULL UNIQUE CHECK(length(qualification_sha256)=64 AND qualification_sha256 NOT GLOB '*[^0-9a-f]*'),
  evidence_sha256 TEXT NOT NULL CHECK(length(evidence_sha256)=64 AND evidence_sha256 NOT GLOB '*[^0-9a-f]*'),
  payload TEXT NOT NULL CHECK(json_valid(payload)),
  actor_id TEXT NOT NULL,
  qualified_at TEXT NOT NULL
) STRICT;

CREATE TRIGGER runtime_profile_qualification_immutable_update
BEFORE UPDATE ON runtime_profile_qualification
BEGIN
  SELECT RAISE(ABORT, 'runtime profile qualifications are immutable');
END;

CREATE TRIGGER runtime_profile_qualification_immutable_delete
BEFORE DELETE ON runtime_profile_qualification
BEGIN
  SELECT RAISE(ABORT, 'runtime profile qualifications are immutable');
END;
