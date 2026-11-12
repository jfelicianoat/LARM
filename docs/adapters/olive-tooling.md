# Microsoft Olive tooling worker

`adapters/runtime/olive-tooling/worker.py` is a private, one-request JSON worker for P065. It is not a public adapter-family contract and does not add operations to the LARM API. P095 can use its closed protocol while the transformation adapter continues to speak the published `transformation.describe`, `plan`, `execute` and `verify` methods.

The worker implements `probe`, `version`, `capabilities`, `plan` and `execute_fixture`. Requests use protocol version `1.0`; unknown fields, duplicate JSON keys, oversized messages and unknown operations fail before tool execution. The only permitted input is the locked `tensor_identity` ONNX fixture. Its SHA256 and byte count are pinned in `fixtures.json`; a changed file, arbitrary path or unlisted fixture is rejected.

The worker only treats a Python environment as managed when `LARM_PYTHON_ROOT` resolves to the active virtual environment and Python reports an isolated prefix. It never installs packages and does not inspect/import Olive or ONNX Runtime from an unmanaged interpreter. `probe` reports package and provider data as unknown when there is no managed environment. The environment manager P071 must provision and lock Olive, ONNX Runtime, and the relevant provider before product execution is enabled.

Planning is read-only and deterministic. It records the exact input size, exact installed dependency versions when known, requested provider, normalized precision, and `trust_remote_code=false`. The output estimate is explicitly unknown until a qualified Olive estimator exists. The plan is eligible only when managed Python, the Olive optimization entry point, ONNX Runtime, and the requested provider are all observed. No model-store path is accepted or written.

`execute_fixture` requires the hash of the current plan and rechecks the fixture and environment. It invokes the supported `olive.optimize` entry point in a child Python process with fixed arguments and `trust_remote_code=False`, enforces a bounded timeout, rejects links and outputs over configured file/byte limits, hashes every output, and checks that the source fixture stayed unchanged. A verified candidate and its provenance are published by same-volume rename under `LARM_OLIVE_OUTPUT_ROOT/candidates/<plan-hash>`. Existing candidates are never replaced or promoted. The caller must supply an existing managed output root; the transformation job, peak-space preflight, reservations, durable journal, smoke verification and model-store registration remain owned by P093.

The child process inherits the adapter host's process containment. Until P021/P022/P130 provide their required Windows job-object and permission gates, this private worker is not exposed as a product execution route. Worker cancellation is performed by the supervising adapter host; timeout handling is tested here, while tree termination and durable failed-job recovery remain acceptance work for the host and P093.

The initial Olive call follows the documented Python `olive.optimize` interface ([Microsoft Olive Python API](https://github.com/microsoft/Olive/blob/main/docs/source/reference/python_api.md)). A versioned Olive/ONNX profile and exact package lock must be qualified before enabling it on a real managed environment; P099 supplies that external-profile evidence.

Run the focused contract tests with:

```powershell
.venv-tools/Scripts/python.exe -m unittest discover -s adapters/runtime/olive-tooling/tests -p test_worker.py -v
.venv-tools/Scripts/python.exe tools/p065_real_probe.py
```

The tests use a temporary isolated virtual environment with fake package metadata and a fake `olive.optimize` entry point. They prove protocol behavior and filesystem boundaries, not real Olive compatibility or optimization quality.
