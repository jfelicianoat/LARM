-- P099: los profile locks documentales son inmutables y nunca habilitan soporte.
CREATE TABLE runtime_profile_lock (
  lock_sha256 TEXT PRIMARY KEY CHECK(length(lock_sha256)=64 AND lock_sha256 NOT GLOB '*[^0-9a-f]*'),
  runtime TEXT NOT NULL,
  source_url TEXT NOT NULL,
  source_revision TEXT NOT NULL,
  source_sha256 TEXT NOT NULL CHECK(length(source_sha256)=64 AND source_sha256 NOT GLOB '*[^0-9a-f]*'),
  upstream_version TEXT NOT NULL,
  os TEXT NOT NULL,
  os_build TEXT NOT NULL,
  backend TEXT NOT NULL,
  protocol TEXT NOT NULL,
  qualification TEXT NOT NULL CHECK(qualification='UNQUALIFIED'),
  payload TEXT NOT NULL CHECK(json_valid(payload)),
  created_at TEXT NOT NULL
) STRICT;

CREATE UNIQUE INDEX runtime_profile_lock_identity ON runtime_profile_lock(
  runtime, source_url, source_revision, source_sha256, upstream_version,
  os, os_build, backend, protocol
);

CREATE TRIGGER runtime_profile_lock_immutable_update
BEFORE UPDATE ON runtime_profile_lock
BEGIN
  SELECT RAISE(ABORT, 'runtime profile locks are immutable');
END;

CREATE TRIGGER runtime_profile_lock_immutable_delete
BEFORE DELETE ON runtime_profile_lock
BEGIN
  SELECT RAISE(ABORT, 'runtime profile locks are immutable');
END;
