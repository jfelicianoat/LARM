# ModelScope source adapter — P047

P047 implements a portable, isolated Python worker for public ModelScope Hub metadata. The worker uses the Hub's documented HTTP API and stays independent of the LARM Core. Its profile pins the official `modelscope_hub` v0.4.0 reference; it does not import that SDK at runtime.

| LARM method | Upstream mapping | Notes |
| --- | --- | --- |
| `source.search` | `GET /openapi/v1/models?search=…&page_number=…&page_size=…` | Public search; a query-bound cursor represents the page and snapshot. Capability filtering remains Core-owned. |
| `source.get_model` | `GET /openapi/v1/models/{namespace}/{name}` plus revision and file APIs | Preserves upstream fields after redaction and emits normalized model variants. |
| `source.list_revisions` | `GET /api/v1/models/{repo}/revisions` | Keeps branch/tag names separate from immutable commit IDs. Movable refs can be inspected but cannot be downloaded. |
| `source.get_manifest`, `source.estimate_size` | `GET /api/v1/models/{repo}/repo/files?Revision=…&Recursive=True` | File records provide optional byte sizes and hashes. Missing either value lowers confidence; absent sizes remain unknown. |
| `source.download_plan` | Revalidates public repository metadata, pinned commit, and manifest | Produces a preflight only. Destination space and explicit user confirmation remain Core/Download Manager responsibilities. |
| `source.download_chunk` | `GET /api/v1/models/{repo}/repo?Revision=…&FilePath=…` with a bounded `Range` request | Returns at most 512 KiB to the Download Manager. It validates content range, encoding, size and resume validator. It never writes to a destination. |
| `source.download` | Not implemented as a direct transfer | Returns `UNSUPPORTED_CAPABILITY`; byte execution belongs to the Download Manager through `source.download_chunk`. |
| `source.auth_probe` | Local public/credential-state response | Anonymous public metadata is allowed. Credentials remain unresolved until P045/P141. |

## Network and data handling

The worker disables ambient proxy use, allows only manifest-listed origins, requires HTTPS except loopback test endpoints, caps JSON responses at 8 MiB and chunk responses at 512 KiB, limits redirects to five, and strips authorization-related headers on cross-origin redirects. Signed URL query parameters are not accepted as API URLs or included in diagnostics. Operators must explicitly provision any storage origin required by redirects; an unlisted origin fails closed. The default package permits only the configured ModelScope API origin, so redirected object-storage downloads require separate origin qualification before they can be accepted as real.

Public repository metadata is read-only. Tokens, cookies, passwords, signatures, and similarly named fields are redacted from retained upstream data. Private repositories fail with `AUTH_REQUIRED`; gated repositories fail with `LICENSE_REQUIRED`. The worker does not request license acceptance. Rate limiting and upstream timeouts become retryable `UPSTREAM_UNAVAILABLE`; malformed payloads, invalid identities, unpinned revisions, invalid paths, range mismatches, and resume-validator changes fail closed with normalized errors.

Size confidence is `EXACT` only when a file has a valid published size and SHA-256; a published size without a valid hash is `HIGH`; missing/invalid size evidence is `UNKNOWN`. The Core still performs destination preflight and requires explicit confirmation before it creates a download.

## Qualification and limits

`tests/integration/modelscope_adapter.py` exercises the worker against deterministic HTTP-shaped fixtures, including identity, size/hash mapping, search, manifest, pinned revisions, resumable chunks, private/gated access, cancellation, rate limits, malformed inputs, and redirect rejection. `tools/p047_package_smoke.py` builds a self-contained package and tests its hash, isolated handshake, and anonymous auth probe. `tools/p047_live_probe.py` performs at most one anonymous, read-only public search and does not download model files.

P047 remains `REQUIRES_REAL_QUALIFICATION` until a live ModelScope query and the P047 Windows behavior are recorded, its dependent prompts reach `REAL_PASS`, and private/gated flows are qualified through P045/P141. A passing mock suite does not satisfy those gates.

Official references: [modelscope_hub v0.4.0 README and API](https://github.com/modelscope/modelscope_hub/tree/v0.4.0), [HubApi implementation](https://github.com/modelscope/modelscope_hub/blob/v0.4.0/src/modelscope_hub/api.py), [OpenAPI implementation](https://github.com/modelscope/modelscope_hub/blob/v0.4.0/src/modelscope_hub/_openapi.py), and [legacy repository API implementation](https://github.com/modelscope/modelscope_hub/blob/v0.4.0/src/modelscope_hub/_legacy_api.py).
