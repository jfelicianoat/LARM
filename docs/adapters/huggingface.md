# Hugging Face source adapter — P046

The adapter discovers public Hub models, builds format-specific candidate bundles, inspects pinned revisions, reports evidence-backed sizes, and streams bounded file ranges to the Download Manager. It never writes model files to a user destination. `source.download` returns `UNSUPPORTED_CAPABILITY`; transfers use the optional `source.download_chunk` method so P051 can verify and publish the bytes.

## Profile and API mapping

The profile is `HUGGING_FACE` in `docs/contracts/source-profiles.json`; package revision 3.4.0 declares its optional `source.download_chunk` capability. The worker uses the public HTTP/JSON Hub API directly, without the Hugging Face SDK in Core or in the worker. It queries `GET /api/models` for search, `GET /api/models/{namespace}/{model}` for model metadata, `?blobs=true` for file/LFS metadata, `GET /api/models/{namespace}/{model}/refs` for branches and tags, and `GET /{namespace}/{model}/resolve/{commit}/{path}` for pinned file ranges.

Search forwards text only. Capability filters are rejected as `UNSUPPORTED_CAPABILITY` and remain for Core to apply, so the adapter does not claim an upstream OR/AND behavior it cannot express. API `Link: rel=next` values are kept in an opaque cursor bound to source, text, and page limit. The cursor accepts only the Hub model endpoint, approved query keys, the original search text, and the original limit. `snapshot_at` describes when the first page was read; the Hub is a moving catalogue, not a frozen search snapshot.

Each repository identity maps to one distribution. Supported files form candidate variants: GGUF quantizations are separate; a lone GGUF model can be paired with one projector; safetensors and PyTorch files are grouped with recognized config/tokenizer files in their own directory; ONNX graphs include only matching external-data sidecars. Weight directories stay separate. The upstream `siblings` array is projected into these descriptors rather than copied wholesale into search metadata, and files outside a recognized bundle are not offered for download.

An immutable revision uses the Hub commit SHA (40–64 lowercase hexadecimal characters) and a bundle key encoded in `revision_key`. The manifest hash is RFC 8785 over sorted relative paths, published SHA-256, published byte size, and required status. A second observation that changes the manifest for the same commit fails with `INTEGRITY_FAILED`; it never silently rewrites a pinned revision.

## Size and integrity evidence

The adapter distinguishes the contract’s confidence levels per artifact:

- `EXACT` requires both a byte size and SHA-256 published for the pinned file.
- `HIGH` carries a published size without SHA-256, including Git LFS metadata or `Content-Length` from an HTTPS `HEAD` request.
- `UNKNOWN` keeps bytes and bounds null when neither metadata nor `HEAD` provides a valid size.

Git `blob_id` values are not treated as SHA-256. A `HEAD` fallback is stored only as `size_estimate`; it does not alter `size_bytes` or the manifest hash. A known length without a digest is never promoted to exact or treated as a verified artifact. The Download Manager verifies complete bytes after EOF.

Downloads request at most 512 KiB per range. A resume must use the same strong ETag or Last-Modified value bound to a published SHA-256. Missing or changed validators fail closed so Core can restart from offset zero. Redirects are limited to the exact approved Hub and official storage origins; credentials and cookies are never forwarded across origins, and signed URLs are never returned to Core or included in errors.

## Access, licensing, and errors

The worker currently supports anonymous public access only. It does not read credentials or accept a token as a URL/header input. `source.auth_probe` reports that configured credentials require the P045/P141 resolver; private repositories return `AUTH_REQUIRED`, and gated repositories return `LICENSE_REQUIRED` before manifest or file access. A visible license identifier is metadata, not acceptance. The user accepts any required license through LARM’s account/license flow before a qualified private/gated transfer is enabled.

HTTP 401/403/404 map to access, license, or not-found errors; 429 maps to retryable `UPSTREAM_UNAVAILABLE` with the available rate-limit fields; timeout and 5xx responses are retryable upstream errors. Response bodies and signed redirect targets are not copied into error details. The public profile declares no secret references.

## Build and verification

Build a portable package with the locked Python environment:

```powershell
.\.venv-tools\Scripts\python.exe adapters\source\huggingface\package.py `
  --package target\huggingface-package `
  --work target\huggingface-work
```

The package includes the Python runtime, SDK contracts, and method schemas. `work/profile.json` contains only the source UUID and base URL; the worker’s SQLite identity map lives under the same private work root. Production network permissions list exact Hub/CDN origins; loopback HTTP is accepted only for local tests.

`tests/integration/huggingface_adapter.py` exercises the JSON schemas and adapter over an in-memory HTTP-shaped transport. The P046 evidence runner also launches the built worker through its handshake. A read-only public search completed during qualification; a restricted network environment is recorded as blocked rather than as a successful live check. P046 remains `REQUIRES_REAL_QUALIFICATION` until private/gated authorization through P045/P141 and dependent REAL_PASS gates are available.

Primary references: [Hub API](https://huggingface.co/docs/hub/api), [gated model access](https://huggingface.co/docs/hub/models-gated), and the [maintained Hub OpenAPI document](https://huggingface.co/.well-known/openapi.json).
