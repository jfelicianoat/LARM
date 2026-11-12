# Reference Protocol Sidecar

This reference process is a local, per-runtime-instance HTTP bridge for a configured CLI. It binds only to `127.0.0.1` on an ephemeral port and reads its startup JSON, including its per-instance bearer token, from stdin. The parent already knows the token; the `ready` record on stdout contains only the endpoint, protocol label, and profile hash.

The declared subset is custom REST: authenticated `GET /v1/describe`, `GET /v1/metrics`, `POST /v1/invoke`, and `POST /v1/cancel`. `protocol.invoke` supports `text.generate` with role-tagged text messages only. It rejects streaming, generation overrides, tools, tool messages, and media with `UNSUPPORTED_CAPABILITY`. It does not claim OpenAI compatibility. Prompt bytes are sent on the child process stdin; the configured executable and fixed arguments are launched directly without a shell, inherited environment is cleared, stdout/stderr are drained with limits, and cancellation/timeout abort the child process.

The echo CLI is a test/reference fixture, not an AI runtime. The sidecar framework and fixture validate transport, authentication, cancellation, limits, and error behavior; they do not qualify a runtime, backend, or prompt template. Windows job-tree containment and managed integration remain dependent on P021/P026 and their real acceptance.
