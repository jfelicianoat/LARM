-- 0001_bootstrap: DDL normativo del paquete (contracts/persistence.sql) + schema_metadata.
-- sha256 del DDL contractual: 9865aa3e57a5258ebe9b6e2bdf2649dc9927f6d696e9125aa5f5f498c10448b2
-- Copiado verbatim; cambios solo via migracion contractual nueva.

-- schema_metadata la crea y gestiona el runner de migraciones (larm-db::migrate).

-- DDL normativo; cambiar sólo mediante migración contractual.
PRAGMA foreign_keys=ON;
PRAGMA journal_mode=WAL;
PRAGMA busy_timeout=5000;

CREATE TABLE canonical_model (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  name TEXT NOT NULL,
  upstream_identity TEXT,
  capabilities TEXT NOT NULL CHECK(json_valid(capabilities)),
  possible_duplicates TEXT NOT NULL CHECK(json_valid(possible_duplicates))
) STRICT;

CREATE TABLE model_distribution (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  canonical_model_id TEXT NOT NULL REFERENCES canonical_model(id) DEFERRABLE INITIALLY DEFERRED,
  source_id TEXT NOT NULL REFERENCES model_source(id) DEFERRABLE INITIALLY DEFERRED,
  upstream_namespace TEXT NOT NULL,
  upstream_model_id TEXT NOT NULL,
  license_id TEXT
) STRICT;

CREATE TABLE model_revision (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  distribution_id TEXT NOT NULL REFERENCES model_distribution(id) DEFERRABLE INITIALLY DEFERRED,
  revision_key TEXT NOT NULL,
  upstream_commit TEXT,
  manifest_hash TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('PROVISIONAL','CANDIDATE','CURRENT','RETIRED','REJECTED','SUPERSEDED'))
) STRICT;

CREATE TABLE model_variant (
  update_available INTEGER NOT NULL CHECK(update_available IN (0,1)),
  status TEXT NOT NULL CHECK(status IN ('DISCOVERED','SELECTED','DOWNLOAD_PLANNED','DOWNLOADING','DOWNLOADED','VERIFIED','PREPARED','TECHNICALLY_VALIDATED','AVAILABLE','RUNNING','OFFLINE','CORRUPT','UPDATE_AVAILABLE','FAILED','REJECTED')),
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  revision_id TEXT NOT NULL REFERENCES model_revision(id) DEFERRABLE INITIALLY DEFERRED,
  format TEXT,
  quantization TEXT,
  architecture TEXT,
  bundle_hash TEXT,
  lifecycle TEXT NOT NULL CHECK(lifecycle IN ('DISCOVERED','SELECTED','DOWNLOAD_PLANNED','DOWNLOADING','DOWNLOADED','VERIFIED','PREPARED','TECHNICALLY_VALIDATED','AVAILABLE','RUNNING','FAILED','REJECTED','CORRUPT')),
  availability TEXT NOT NULL CHECK(availability IN ('UNKNOWN','OFFLINE','INCOMPATIBLE','PREPARING','AVAILABLE','DEGRADED','CORRUPT')),
  ownership TEXT NOT NULL CHECK(ownership IN ('LARM_MANAGED','HOST_MANAGED','EXTERNAL','SHARED')),
  promotion TEXT NOT NULL CHECK(promotion IN ('NONE','CANDIDATE','TECHNICALLY_APPROVED','EVALUATION_APPROVED','DEFAULT','PINNED','REJECTED')),
  system1_state TEXT CHECK(system1_state IN ('SYSTEM1_CANDIDATE','SYSTEM1_TECHNICALLY_VALID','SYSTEM1_EVALUATED','SYSTEM1_CERTIFIED','SYSTEM1_REJECTED')),
  system_role TEXT NOT NULL CHECK(system_role IN ('GENERAL','SYSTEM1','AUXILIARY')),
  capabilities TEXT NOT NULL CHECK(json_valid(capabilities)),
  model_limits TEXT NOT NULL CHECK(json_valid(model_limits))
) STRICT;

CREATE TABLE model_bundle (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  variant_id TEXT NOT NULL REFERENCES model_variant(id) DEFERRABLE INITIALLY DEFERRED,
  required_artifact_ids TEXT NOT NULL CHECK(json_valid(required_artifact_ids)),
  optional_artifact_ids TEXT NOT NULL CHECK(json_valid(optional_artifact_ids)),
  manifest_hash TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('INCOMPLETE','VERIFIED','CORRUPT'))
) STRICT;

CREATE TABLE model_artifact (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  variant_id TEXT NOT NULL REFERENCES model_variant(id) DEFERRABLE INITIALLY DEFERRED,
  filename TEXT NOT NULL,
  format TEXT,
  quantization TEXT,
  expected_size INTEGER CHECK(expected_size>=0),
  actual_size INTEGER CHECK(actual_size>=0),
  sha256 TEXT,
  source_url TEXT,
  required INTEGER NOT NULL CHECK(required IN (0,1))
) STRICT;

CREATE TABLE artifact_provenance (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  artifact_id TEXT NOT NULL REFERENCES model_artifact(id) DEFERRABLE INITIALLY DEFERRED,
  source_id TEXT REFERENCES model_source(id) DEFERRABLE INITIALLY DEFERRED,
  revision_key TEXT,
  parents TEXT NOT NULL CHECK(json_valid(parents)),
  tool_id TEXT,
  tool_version TEXT,
  parameters TEXT NOT NULL CHECK(json_valid(parameters)),
  license_id TEXT,
  license_hash TEXT,
  trust TEXT NOT NULL CHECK(trust IN ('UNTRUSTED','DECLARED','VERIFIED','REVOKED')),
  actor_id TEXT NOT NULL,
  evidence_refs TEXT NOT NULL CHECK(json_valid(evidence_refs))
) STRICT;

CREATE TABLE artifact_location (
  ownership TEXT NOT NULL CHECK(ownership IN ('LARM_MANAGED','HOST_MANAGED','EXTERNAL','SHARED')),
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  artifact_id TEXT NOT NULL REFERENCES model_artifact(id) DEFERRABLE INITIALLY DEFERRED,
  storage_location_id TEXT NOT NULL REFERENCES storage_location(id) DEFERRABLE INITIALLY DEFERRED,
  relative_path TEXT NOT NULL,
  file_size INTEGER CHECK(file_size>=0),
  sha256 TEXT,
  state TEXT NOT NULL CHECK(state IN ('ONLINE','OFFLINE','CORRUPT','MISSING','COPYING','UNKNOWN')),
  verified_at TEXT
) STRICT;

CREATE TABLE model_source (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  name TEXT NOT NULL,
  adapter_id TEXT NOT NULL REFERENCES model_source_adapter(id) DEFERRABLE INITIALLY DEFERRED,
  base_url TEXT NOT NULL,
  priority INTEGER NOT NULL CHECK(priority>=0),
  enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
  auth_mode TEXT NOT NULL CHECK(auth_mode IN ('NONE','TOKEN','BEARER','API_TOKEN','OAUTH_DEVICE','OAUTH_AUTH_CODE_PKCE','BASIC','SSH','COOKIE','GIT_LFS','WINDOWS_INTEGRATED')),
  account_id TEXT REFERENCES model_source_account(id) DEFERRABLE INITIALLY DEFERRED,
  policy TEXT NOT NULL CHECK(json_valid(policy))
) STRICT;

CREATE TABLE model_source_adapter (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  adapter_installation_id TEXT NOT NULL REFERENCES adapter_installation(id) DEFERRABLE INITIALLY DEFERRED,
  source_type TEXT NOT NULL,
  capabilities TEXT NOT NULL CHECK(json_valid(capabilities))
) STRICT;

CREATE TABLE model_source_account (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  source_id TEXT NOT NULL REFERENCES model_source(id) DEFERRABLE INITIALLY DEFERRED,
  credential_reference_id TEXT REFERENCES credential_reference(id) DEFERRABLE INITIALLY DEFERRED,
  display_name TEXT NOT NULL,
  accepted_license_hashes TEXT NOT NULL CHECK(json_valid(accepted_license_hashes))
) STRICT;

CREATE TABLE runtime (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  channel_policy TEXT NOT NULL CHECK(json_valid(channel_policy)),
  name TEXT NOT NULL,
  adapter_id TEXT NOT NULL REFERENCES runtime_adapter(id) DEFERRABLE INITIALLY DEFERRED,
  capabilities TEXT NOT NULL CHECK(json_valid(capabilities)),
  default_version_id TEXT REFERENCES runtime_version(id) DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE TABLE runtime_adapter (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  adapter_installation_id TEXT NOT NULL REFERENCES adapter_installation(id) DEFERRABLE INITIALLY DEFERRED,
  runtime_family TEXT NOT NULL,
  capabilities TEXT NOT NULL CHECK(json_valid(capabilities))
) STRICT;

CREATE TABLE runtime_version (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  runtime_id TEXT NOT NULL REFERENCES runtime(id) DEFERRABLE INITIALLY DEFERRED,
  version_string TEXT NOT NULL,
  "commit" TEXT,
  build TEXT,
  channel TEXT NOT NULL CHECK(channel IN ('stable','prerelease','nightly','commit','custom')),
  released_at TEXT,
  artifact_hash TEXT,
  promotion TEXT NOT NULL CHECK(promotion IN ('NONE','CANDIDATE','DEFAULT','PINNED','REJECTED'))
) STRICT;

CREATE TABLE runtime_installation (
  registration_mode TEXT NOT NULL CHECK(registration_mode IN ('MANAGED','ADOPTED','EXTERNAL')),
  stage TEXT NOT NULL CHECK(stage IN ('DISCOVERED','INSTALL_PLANNED','INSTALLING','INSTALLED','VALIDATING','VALIDATED','FAILED','RETIRED')),
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  runtime_version_id TEXT NOT NULL REFERENCES runtime_version(id) DEFERRABLE INITIALLY DEFERRED,
  storage_location_id TEXT REFERENCES storage_location(id) DEFERRABLE INITIALLY DEFERRED,
  relative_path TEXT,
  strategy TEXT NOT NULL CHECK(strategy IN ('PREBUILT_BINARY','GITHUB_RELEASE','SOURCE_BUILD','WINGET','PIP','UV','CONDA','MAMBA','NUGET','WSL','CONTAINER','CUSTOM','MSI_MANAGED','PYTHON_VENV','EXTERNAL_ADOPTION')),
  external_host_id TEXT REFERENCES external_host(id) DEFERRABLE INITIALLY DEFERRED,
  state TEXT NOT NULL CHECK(state IN ('DISCOVERED','INSTALL_PLANNED','INSTALLING','INSTALLED','VALIDATING','VALIDATED','ACTIVE','DEGRADED','BROKEN','ROLLED_BACK','FAILED','RETIRED')),
  ownership TEXT NOT NULL CHECK(ownership IN ('LARM_MANAGED','EXTERNAL')),
  lock_hash TEXT
) STRICT;

CREATE TABLE runtime_backend (
  limits TEXT NOT NULL CHECK(json_valid(limits)),
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  runtime_installation_id TEXT NOT NULL REFERENCES runtime_installation(id) DEFERRABLE INITIALLY DEFERRED,
  backend_type TEXT NOT NULL,
  device_id TEXT,
  driver_version TEXT,
  status TEXT NOT NULL CHECK(status IN ('AVAILABLE','UNAVAILABLE','UNKNOWN')),
  evidence_refs TEXT NOT NULL CHECK(json_valid(evidence_refs))
) STRICT;

CREATE TABLE runtime_instance (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  runtime_installation_id TEXT NOT NULL REFERENCES runtime_installation(id) DEFERRABLE INITIALLY DEFERRED,
  variant_id TEXT NOT NULL REFERENCES model_variant(id) DEFERRABLE INITIALLY DEFERRED,
  backend_id TEXT NOT NULL REFERENCES runtime_backend(id) DEFERRABLE INITIALLY DEFERRED,
  launch_profile_id TEXT NOT NULL REFERENCES launch_profile(id) DEFERRABLE INITIALLY DEFERRED,
  state TEXT NOT NULL CHECK(state IN ('CREATED','STARTING','LOADING','READY','BUSY','DEGRADED','FAILED','STOPPING','STOPPED','CRASHED')),
  pid INTEGER CHECK(pid>=0),
  process_started_at TEXT,
  executable_hash TEXT,
  generation INTEGER NOT NULL CHECK(generation>=0),
  execution_fingerprint TEXT,
  last_used_at TEXT,
  ownership TEXT NOT NULL CHECK(ownership IN ('LARM_MANAGED','EXTERNAL'))
) STRICT;

CREATE TABLE external_host (
  registration_mode TEXT NOT NULL CHECK(registration_mode IN ('MANAGED','ADOPTED','EXTERNAL')),
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  name TEXT NOT NULL,
  base_url TEXT NOT NULL,
  host_type TEXT NOT NULL,
  credential_reference_id TEXT REFERENCES credential_reference(id) DEFERRABLE INITIALLY DEFERRED,
  state TEXT NOT NULL CHECK(state IN ('ONLINE','OFFLINE','UNKNOWN')),
  ownership TEXT NOT NULL CHECK(ownership IN ('EXTERNAL')),
  observed_version TEXT
) STRICT;

CREATE TABLE hardware_profile (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  hardware_fingerprint TEXT NOT NULL,
  snapshot TEXT NOT NULL CHECK(json_valid(snapshot)),
  evidence_refs TEXT NOT NULL CHECK(json_valid(evidence_refs)),
  probed_at TEXT NOT NULL
) STRICT;

CREATE TABLE storage_location (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  name TEXT NOT NULL,
  root_path TEXT NOT NULL,
  canonical_path TEXT NOT NULL,
  volume_guid TEXT,
  drive_letter TEXT,
  unc_path TEXT,
  filesystem TEXT NOT NULL,
  storage_type TEXT NOT NULL,
  capacity_bytes INTEGER NOT NULL CHECK(capacity_bytes>=0),
  free_bytes INTEGER NOT NULL CHECK(free_bytes>=0),
  reserved_bytes INTEGER NOT NULL CHECK(reserved_bytes>=0),
  priority INTEGER NOT NULL CHECK(priority>=0),
  auto_placement INTEGER NOT NULL CHECK(auto_placement IN (0,1)),
  state TEXT NOT NULL CHECK(state IN ('ONLINE','OFFLINE','READ_ONLY','LOW_SPACE','DEGRADED','MISSING','ERROR','UNKNOWN')),
  last_seen_at TEXT,
  volume_key TEXT NOT NULL,
  performance_class TEXT NOT NULL CHECK(performance_class IN ('NVME','SSD','HDD','NETWORK','REMOVABLE','UNKNOWN'))
) STRICT;

CREATE TABLE compatibility_record (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  variant_id TEXT NOT NULL REFERENCES model_variant(id) DEFERRABLE INITIALLY DEFERRED,
  runtime_version_id TEXT NOT NULL REFERENCES runtime_version(id) DEFERRABLE INITIALLY DEFERRED,
  backend_id TEXT NOT NULL REFERENCES runtime_backend(id) DEFERRABLE INITIALLY DEFERRED,
  hardware_fingerprint TEXT NOT NULL,
  launch_profile_hash TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('COMPATIBLE','COMPATIBLE_WITH_LIMITATIONS','EXPERIMENTAL','CONVERSION_REQUIRED','RUNTIME_REQUIRED','BACKEND_REQUIRED','INCOMPATIBLE','UNKNOWN')),
  hard_gates TEXT NOT NULL CHECK(json_valid(hard_gates)),
  limitations TEXT NOT NULL CHECK(json_valid(limitations)),
  technical_fit REAL,
  evidence_refs TEXT NOT NULL CHECK(json_valid(evidence_refs))
) STRICT;

CREATE TABLE launch_profile (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  name TEXT NOT NULL CHECK(name IN ('ECO','BALANCED','PERFORMANCE','LONG_CONTEXT','CUSTOM')),
  parameters TEXT NOT NULL CHECK(json_valid(parameters)),
  profile_hash TEXT NOT NULL,
  backend_id TEXT
) STRICT;

CREATE TABLE runtime_lease (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  owner_id TEXT NOT NULL,
  request_id TEXT NOT NULL,
  instance_id TEXT REFERENCES runtime_instance(id) DEFERRABLE INITIALLY DEFERRED,
  generation INTEGER NOT NULL CHECK(generation>=0),
  state TEXT NOT NULL CHECK(state IN ('REQUESTED','ADMITTING','ACTIVE','EXPIRING','EXPIRED','RELEASED','FAILED')),
  expires_at TEXT,
  endpoint_id TEXT REFERENCES inference_endpoint(id) DEFERRABLE INITIALLY DEFERRED,
  constraints TEXT NOT NULL CHECK(json_valid(constraints))
) STRICT;

CREATE TABLE inference_endpoint (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  instance_id TEXT NOT NULL REFERENCES runtime_instance(id) DEFERRABLE INITIALLY DEFERRED,
  protocol TEXT NOT NULL,
  protocol_profile_hash TEXT NOT NULL,
  base_url TEXT NOT NULL,
  credential_reference_id TEXT REFERENCES credential_reference(id) DEFERRABLE INITIALLY DEFERRED,
  capabilities TEXT NOT NULL CHECK(json_valid(capabilities)),
  qualification TEXT NOT NULL CHECK(qualification IN ('UNTESTED','QUALIFIED','FAILED')),
  evidence_refs TEXT NOT NULL CHECK(json_valid(evidence_refs))
) STRICT;

CREATE TABLE download_job (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  priority TEXT NOT NULL CHECK(priority IN ('BACKGROUND','NORMAL','HIGH')),
  variant_id TEXT NOT NULL REFERENCES model_variant(id) DEFERRABLE INITIALLY DEFERRED,
  storage_location_id TEXT NOT NULL REFERENCES storage_location(id) DEFERRABLE INITIALLY DEFERRED,
  plan_hash TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('PLANNED','QUEUED','RUNNING','PAUSING','PAUSED','VERIFYING','COMPLETED','CANCELLING','CANCELLED','FAILED')),
  attempt INTEGER NOT NULL CHECK(attempt>=0),
  progress TEXT NOT NULL CHECK(json_valid(progress)),
  error TEXT CHECK(json_valid(error))
) STRICT;

CREATE TABLE transformation_job (
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

CREATE TABLE technical_benchmark (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  variant_id TEXT NOT NULL REFERENCES model_variant(id) DEFERRABLE INITIALLY DEFERRED,
  execution_fingerprint TEXT NOT NULL,
  suite_id TEXT NOT NULL,
  suite_hash TEXT NOT NULL,
  fixture_hash TEXT NOT NULL,
  settings_hash TEXT NOT NULL,
  raw_artifact_refs TEXT NOT NULL CHECK(json_valid(raw_artifact_refs)),
  metrics TEXT NOT NULL CHECK(json_valid(metrics)),
  status TEXT NOT NULL CHECK(status IN ('PLANNED','RUNNING','PASS','FAIL','INCONCLUSIVE','BLOCKED_REAL')),
  started_at TEXT,
  completed_at TEXT
) STRICT;

CREATE TABLE evaluation_reference (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  source_system TEXT NOT NULL,
  external_id TEXT NOT NULL,
  source_version TEXT NOT NULL,
  experiment_id TEXT NOT NULL,
  suite_hash TEXT NOT NULL,
  dataset_hash TEXT NOT NULL,
  variant_id TEXT NOT NULL REFERENCES model_variant(id) DEFERRABLE INITIALLY DEFERRED,
  execution_fingerprint TEXT NOT NULL,
  score REAL,
  scale TEXT NOT NULL CHECK(json_valid(scale)),
  status TEXT NOT NULL CHECK(status IN ('PASS','FAIL','INCONCLUSIVE','BLOCKED','EXPIRED')),
  artifact_refs TEXT NOT NULL CHECK(json_valid(artifact_refs)),
  issued_at TEXT NOT NULL,
  signature TEXT
) STRICT;

CREATE TABLE certification (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  evaluation_reference_id TEXT NOT NULL REFERENCES evaluation_reference(id) DEFERRABLE INITIALLY DEFERRED,
  variant_id TEXT NOT NULL REFERENCES model_variant(id) DEFERRABLE INITIALLY DEFERRED,
  runtime_version_id TEXT NOT NULL REFERENCES runtime_version(id) DEFERRABLE INITIALLY DEFERRED,
  backend_id TEXT NOT NULL REFERENCES runtime_backend(id) DEFERRABLE INITIALLY DEFERRED,
  hardware_fingerprint TEXT NOT NULL,
  launch_profile_hash TEXT NOT NULL,
  suite_hash TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('VALID','EXPIRED','REVOKED')),
  expires_at TEXT
) STRICT;

CREATE TABLE source_watch (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  source_id TEXT NOT NULL REFERENCES model_source(id) DEFERRABLE INITIALLY DEFERRED,
  query TEXT NOT NULL CHECK(json_valid(query)),
  cursor TEXT,
  etag TEXT,
  interval_seconds INTEGER NOT NULL CHECK(interval_seconds>=0),
  policy TEXT NOT NULL CHECK(json_valid(policy)),
  next_run_at TEXT,
  enabled INTEGER NOT NULL CHECK(enabled IN (0,1))
) STRICT;

CREATE TABLE notification (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  dedupe_key TEXT NOT NULL,
  severity TEXT NOT NULL CHECK(severity IN ('info','warning','error')),
  message_code TEXT NOT NULL,
  resource_id TEXT,
  action TEXT CHECK(json_valid(action)),
  read_at TEXT
) STRICT;

CREATE TABLE event (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  event_id INTEGER NOT NULL CHECK(event_id>=0),
  event_type TEXT NOT NULL,
  actor_id TEXT NOT NULL,
  resource_type TEXT NOT NULL,
  resource_id TEXT NOT NULL,
  resource_version INTEGER NOT NULL CHECK(resource_version>=0),
  correlation_id TEXT NOT NULL,
  payload TEXT NOT NULL CHECK(json_valid(payload)),
  occurred_at TEXT NOT NULL
) STRICT;

CREATE TABLE credential_reference (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  service TEXT NOT NULL,
  username TEXT NOT NULL,
  backend TEXT NOT NULL CHECK(backend IN ('WINDOWS_CREDENTIAL_MANAGER')),
  secret_type TEXT NOT NULL CHECK(secret_type IN ('TOKEN','PASSWORD','PRIVATE_KEY','COOKIE','OAUTH_REFRESH')),
  scopes TEXT NOT NULL CHECK(json_valid(scopes)),
  expires_at TEXT
) STRICT;

CREATE TABLE adapter_installation (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  row_version INTEGER NOT NULL CHECK(row_version>=1),
  metadata TEXT NOT NULL CHECK(json_valid(metadata)),
  adapter_id TEXT NOT NULL,
  version TEXT NOT NULL,
  family TEXT NOT NULL CHECK(family IN ('runtime','source','protocol','transformation','benchmark')),
  package_hash TEXT NOT NULL,
  publisher TEXT NOT NULL,
  trust TEXT NOT NULL CHECK(trust IN ('UNTRUSTED','VERIFIED','REVOKED')),
  manifest TEXT NOT NULL CHECK(json_valid(manifest)),
  state TEXT NOT NULL CHECK(state IN ('STAGED','VERIFIED','INSTALLED','VALIDATED','RETIRED','FAILED'))
) STRICT;

CREATE UNIQUE INDEX distribution_identity ON model_distribution(source_id,upstream_namespace,upstream_model_id);
CREATE UNIQUE INDEX revision_identity ON model_revision(distribution_id,revision_key);
CREATE UNIQUE INDEX location_identity ON artifact_location(artifact_id,storage_location_id,relative_path);
CREATE UNIQUE INDEX compatibility_identity ON compatibility_record(variant_id,runtime_version_id,backend_id,hardware_fingerprint,launch_profile_hash);
CREATE UNIQUE INDEX lease_request_identity ON runtime_lease(owner_id,request_id);
CREATE UNIQUE INDEX event_sequence ON event(event_id);
CREATE TABLE operation_journal(id TEXT PRIMARY KEY,kind TEXT NOT NULL,resource_ids TEXT NOT NULL CHECK(json_valid(resource_ids)),phase TEXT NOT NULL,plan_hash TEXT NOT NULL,payload TEXT NOT NULL CHECK(json_valid(payload)),updated_at TEXT NOT NULL);
CREATE TABLE volume_reservation(id TEXT PRIMARY KEY,volume_key TEXT NOT NULL,job_id TEXT NOT NULL,total_bytes INTEGER NOT NULL CHECK(total_bytes>=0),remaining_bytes INTEGER NOT NULL CHECK(remaining_bytes>=0),expires_at TEXT);
CREATE TABLE resource_lock(resource_id TEXT NOT NULL,owner_id TEXT NOT NULL,generation INTEGER NOT NULL CHECK(generation>=0),mode TEXT NOT NULL CHECK(mode IN ('SHARED','EXCLUSIVE')),PRIMARY KEY(resource_id,owner_id,generation));
CREATE TABLE event_outbox(event_id INTEGER PRIMARY KEY AUTOINCREMENT,event_record_id TEXT NOT NULL UNIQUE REFERENCES event(id),delivered_at TEXT);
CREATE TABLE idempotency(principal_id TEXT NOT NULL,operation TEXT NOT NULL,key TEXT NOT NULL,body_hash TEXT NOT NULL,status_code INTEGER NOT NULL,response_json TEXT NOT NULL CHECK(json_valid(response_json)),expires_at TEXT NOT NULL,PRIMARY KEY(principal_id,operation,key));
CREATE TABLE memory_reservation(id TEXT PRIMARY KEY,pool_key TEXT NOT NULL,owner_id TEXT NOT NULL,bytes INTEGER NOT NULL CHECK(bytes>=0));
CREATE TABLE artifact_reference(artifact_id TEXT NOT NULL REFERENCES model_artifact(id),variant_id TEXT NOT NULL REFERENCES model_variant(id),PRIMARY KEY(artifact_id,variant_id));
CREATE TABLE settings(id TEXT PRIMARY KEY,revision INTEGER NOT NULL,payload TEXT NOT NULL CHECK(json_valid(payload)));

CREATE TABLE event_sequence_alloc(id INTEGER PRIMARY KEY AUTOINCREMENT);
CREATE UNIQUE INDEX runtime_version_identity ON runtime_version(runtime_id,version_string,coalesce("commit",''),coalesce(build,''),coalesce(artifact_hash,''));
CREATE UNIQUE INDEX variant_identity ON model_variant(revision_id,format,coalesce(quantization,''),architecture,bundle_hash) WHERE bundle_hash IS NOT NULL;
