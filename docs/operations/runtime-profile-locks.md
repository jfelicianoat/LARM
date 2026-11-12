# Runtime profile lock acquisition

`tools/runtime_profile_locks.py` records an exact runtime profile candidate from a captured, version-pinned source document. It reads the 20 candidates and their required fields from `docs/contracts/runtime-profiles.json`; it does not infer support from candidate backends, protocols or commands.

The input JSON has exactly these top-level fields: `runtime`, `source`, `upstream_version`, `os`, `os_build`, `backend`, `protocol` and `fields`. `source` contains `url`, `revision`, `snapshot_path` and `content_sha256`. The URL must use HTTPS, the revision must not be an alias such as `main` or `latest`, and the SHA-256 must match the captured file. `snapshot_path` is relative to the input file and must resolve inside the same evidence directory. `fields` must contain exactly the `profile_required_fields` listed for that runtime, including fixture hashes and evidence references.

Acquire and verify a lock with:

```powershell
.venv-tools/Scripts/python.exe tools/runtime_profile_locks.py acquire --input docs/reports/P099_runs/<capture>/profile-input.json --output docs/reports/P099_runs/<capture>/profile-lock.json
.venv-tools/Scripts/python.exe tools/runtime_profile_locks.py verify --lock docs/reports/P099_runs/<capture>/profile-lock.json
```

The lock identity includes runtime, source URL and revision, captured-source hash, exact upstream version, OS/build, backend and protocol. Its content hash uses RFC 8785. Creating the same file again with identical content is idempotent; different content cannot replace an existing lock. Unknown versions remain explicit candidates and never gain qualification.

Every lock produced by this acquisition tool is `UNQUALIFIED`. Hash integrity, a complete field set and a pinned document capture do not prove that a probe, API, backend or installation mechanism works. Only a separate workflow with real version-specific probes, fixtures, qualification tests and smoke evidence may enable a runtime. The tool therefore cannot create a qualified or enabled lock.

## SQLite catalog

LARM stores acquired locks in `runtime_profile_lock`. The unique identity is
the full key tuple; an identical retry reuses the existing row and a changed
payload for the same key conflicts. The repository validates the RFC 8785
hash again on every read. SQLite rejects updates and deletes, and the initial
`UNQUALIFIED` status is enforced by the table constraint. A successful insert,
its operation journal entry, and its event/outbox record commit together.
`runtime.install` admission also resolves the persisted lock and checks its
runtime family, exact upstream version, and advertised installation strategy.
This admission check does not qualify the lock or authorize execution; a real
probe and smoke gate is still required before a worker may run commands.

## Authenticated API

`PUT /api/v1/runtime-profile-locks/{lock_sha256}` accepts the complete lock
document at its own content hash. The API validates the closed JSON body, the
catalog entry, all required fields and the canonical hash before storing it.
The authenticated principal becomes the journal actor. Repeating the same
request is safe and returns the same lock; changing content for the same key
conflicts. This operation requires `runtimes:write`.

`GET /api/v1/runtime-profile-locks/{lock_sha256}` retrieves a stored lock by its
lowercase SHA-256 and requires `read`. Unknown hashes return not found. The
response always retains `UNQUALIFIED`; these routes do not promote a profile
or bypass the real qualification gates used by installation.

## Separate qualification evidence

Migration `0005_runtime_profile_qualifications` stores results from the trusted
local qualification runner in a second immutable table. Each record is bound
to one lock hash, the complete fixture-hash set, the tested installation
strategies, a host identifier, a timestamp and a SHA-256 of the evidence
artifact. The record has its own canonical hash and is written with a journal
row and outbox event in one transaction. SQLite rejects edits and deletes; an
identical retry is safe, while a different result for the same lock conflicts.

There is deliberately no HTTP route for writing this evidence. The lock API
only acquires and reads candidates. `runtime.install` checks the separate
record and the exact requested strategy before reading the runtime catalog or
opening a worker. Until a trusted local runner captures and verifies real
version-specific probes, the table remains empty and installations stay
blocked. Sample data or client-supplied booleans do not qualify a profile.
