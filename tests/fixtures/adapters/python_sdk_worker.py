"""Fixture exclusiva de transporte P025; no es la fuente de referencia."""
import copy
import json
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "adapters/sdk/python"))
from larm_adapter_sdk import CapabilityRegistry, Contracts, DomainError, ProtocolServer  # noqa: E402

contracts = Contracts(REPO / "docs")
manifest = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
fixture = json.loads((REPO / "tests/fixtures/source/protocol.json").read_text(encoding="utf-8"))
mode=sys.argv[5] if len(sys.argv)>5 else "normal"


def record(event):
    if len(sys.argv)>6:
        with Path(sys.argv[6]).open("a",encoding="utf-8") as output:
            output.write(event+"\n")


class Handler:
    def __init__(self):
        if not manifest["capabilities"] or mode=="missing-registry":
            self.registry=None
            return
        bundle=json.loads((REPO / "adapters/source/reference/capabilities.json").read_text(encoding="utf-8"))
        selected=["source.sync"] if mode=="missing-capability" else manifest["capabilities"]
        bundle["entries"]=[entry for entry in bundle["entries"] if entry["id"] in selected]
        def estimate(_input):
            record("estimate")
            if mode=="invalid-estimate": return {}
            if mode=="raised-estimate": raise ValueError("private-estimator-marker")
            result={key:None for key in ("weights_bytes","kv_bytes","activation_bytes","workspace_bytes","overhead_bytes","total_bytes","multimodal_bytes","temporary_bytes","safety_margin_bytes")}
            result.update(workspace_bytes="2097152",per_pool=[],confidence="UNKNOWN",evidence_refs=["fixture:rpc-buffers-only"])
            return result
        self.registry=CapabilityRegistry(bundle,contracts,{"reference.source.buffers.v1":estimate},{e["mapping"] for e in bundle["entries"]})

    def open_session(self):
        record("open")
        if mode=="session-failure": raise OSError("private-session-marker")

    def call(self, method, params, ctx):
        record("call")
        if method != "source.search":
            raise DomainError("UNSUPPORTED_CAPABILITY", "Fixture de transporte", "fixture_transport_only")
        if params["q"] == "slow":
            ctx.progress("QUERY", None, None, "fixture_waiting")
            while not ctx.cancelled:
                time.sleep(0.005)
            ctx.check_cancelled()
        if params["q"] == "bad-reply":
            return {"wrong": True}
        if params["q"] == "oversized":
            value = copy.deepcopy(fixture["responses"][method])
            value["items"][0]["metadata"]["large"] = "x" * (1 << 20)
            return value
        if params["q"] == "exception":
            raise ValueError("fixture-value-that-must-not-leak")
        if params["q"] == "invalid-error":
            error = DomainError("AUTH_REQUIRED", "Credencial pendiente", "fixture_auth")
            error.data["unexpected"] = "fixture-value-that-must-not-leak"
            raise error
        return copy.deepcopy(fixture["responses"][method])

    def shutdown(self):
        Path(sys.argv[2]).write_text("shutdown", encoding="utf-8")


workers = int(sys.argv[3]) if len(sys.argv) > 3 else 4
queue = int(sys.argv[4]) if len(sys.argv) > 4 else 64
ProtocolServer(contracts, manifest, Handler(), workers=workers, queue_size=queue).serve(sys.stdin.buffer, sys.stdout.buffer)
