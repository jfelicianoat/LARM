-- P030 / contrato 04: un mismo build puede publicarse en canales distintos.
-- No reescribe versiones ni promoción; el canal sigue siendo una dimensión propia.
DROP INDEX runtime_version_identity;
CREATE UNIQUE INDEX runtime_version_identity ON runtime_version(
  runtime_id, version_string, coalesce("commit", ''), coalesce(build, ''),
  coalesce(artifact_hash, ''), channel
);
