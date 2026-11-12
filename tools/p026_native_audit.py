#!/usr/bin/env python3
"""Reconstruct native reference evidence with independent Python RFC8785/statistics.

Reads retained observations only; does not import or issue qualification authority.
Temporary fixture paths are historical: published bytes/SQL rows are in checkpoints.
"""
import base64
import argparse
from collections import Counter
from datetime import datetime, timezone
import hashlib
import json
import math
from pathlib import Path
import re
import statistics

import rfc8785

REPO = Path(__file__).resolve().parent.parent
OUT = REPO / "docs/reports/P026_runs"


def load(path):
    return json.loads(path.read_bytes())


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def digest(value):
    return hashlib.sha256(rfc8785.dumps(value)).hexdigest()


def cases(values):
    assert values and all(v.get("passed", True) is True for v in values)
    assert all(v["expected"] == v["observed"] for v in values)


def wire(value, run_sha):
    cases(value["cases"])
    obs = value["observations"]["Ok"]
    assert len(obs) == 9
    for o in obs:
        assert o["passed"] is True
        assert o["evidence_sha256"] == digest({
            "run_sha256": run_sha, "check": o["check_id"], "cases": value["cases"],
            "before_work": value["before_work_sha256"],
            "after_rejection": value["rejected_work_sha256"], "exit": value["exit"],
        })
    return obs


def common(item, family):
    path = REPO / item["report"]
    assert sha(path) == item["sha256"]
    x = load(path)
    receipt = item["receipt"]
    assert receipt["run_sha256"] == x["run_sha256"]
    assert receipt["installation_revision"] == str(item["row_version"])
    if family != "runtime":
        assert digest(x["run"]) == x["run_sha256"]
        assert x["run"]["id"] == receipt["run_id"]
        assert x["run"]["package_sha256"] == receipt["package_sha256"]
    else:
        assert x["run_id"] == receipt["run_id"]
    probe = item["probe_job"]
    assert probe["status"] == "PASS" and probe["same_key_replay_unchanged"] is True
    accepted, final = probe["accepted"], probe["final"]
    assert accepted["state"] == "QUEUED" and final["job_id"] == accepted["job_id"]
    assert final["resource_id"] == receipt["installation_id"]
    assert final["result"]["conformance"] == receipt
    assert final["state"] == ("COMPLETED" if receipt["passed"] else "FAILED")
    assert probe["request_sha256"] == digest({"profile_id": None, "resource_id": None})
    assert (OUT / "native-probe-http" / (accepted["job_id"] + ".json")).is_file()
    assert load(OUT / "native-probe-http" / (accepted["job_id"] + ".json")) == probe
    registered = probe["registered_profile"]
    profile = registered["record"]
    journal = probe["core_journal"]
    assert digest(profile) == registered["sha256"]
    assert profile["installation_id"] == receipt["installation_id"]
    assert profile["actor_id"] == journal["run"]["actor_id"]
    assert profile["manifest_sha256"] == journal["run"]["manifest_sha256"]
    consumed = registered["consumed_approval"].copy()
    assert consumed["consumed_at"] is not None
    consumed["consumed_at"] = None
    assert consumed == profile["approval"]
    assert digest(journal["run"]) == receipt["run_sha256"]
    assert journal["receipt"] == receipt
    assert digest(journal["outcome"]) == receipt["outcome_sha256"]
    if not receipt["passed"]:
        assert x["observations"]["Err"]["code"] == "VALIDATION_FAILED"
        assert receipt["evidence_sha256"] is None
        assert receipt["outcome_sha256"] == digest({"status": "FAILED", "error_code": "VALIDATION_FAILED"})
        return x
    cases(x["cases"])
    obs = x["observations"]["Ok"]
    first = wire(x["wire"], x["run_sha256"])
    assert obs[:9] == first
    assert len(obs) == (21 if family == "source" else 17)
    assert len({o["check_id"] for o in obs}) == len(obs)
    material = {k: x[k] for k in ("run_sha256", "scope", "wire", "cases", "methods", "checkpoints")}
    if family == "runtime":
        material["scope"] = "reference echo; volatile plans/instances, no inference or Core job ledger"
    for o in obs[9:]:
        assert o["passed"] is True
        assert digest({"check": o["check_id"], "evidence": material}) == o["evidence_sha256"]
    proof = {"run_sha256": x["run_sha256"], "observations": sorted(obs, key=lambda o: o["check_id"])}
    assert digest(proof) == receipt["evidence_sha256"]
    assert digest({"status": "PASSED", "proof": proof}) == receipt["outcome_sha256"]
    return x


def ledger(x, family):
    data = x["checkpoints"]["final-ledger"]
    assert data["ref_lock"] == data["ref_reservation"] == []
    assert [v[0] for v in data["ref_event"]] == [v[0] for v in data["ref_outbox"]]
    assert all(v[1] is None for v in data["ref_outbox"])
    rows = data["ref_run"] if family == "benchmark" else data["ref_job"]
    phases = Counter(v[1 if family == "benchmark" else 3] for v in rows)
    expected = {"source": {"COMPLETED": 4, "CANCELLED": 1},
                "transformation": {"COMPLETED": 4, "CANCELLED": 2},
                "benchmark": {"COMPLETED": 13, "FAILED": 4, "CANCELLED": 2}}[family]
    assert phases == expected
    result = {"jobs" if family != "benchmark" else "runs": len(rows),
              "phases": phases, "events": len(data["ref_event"]), "outbox": len(data["ref_outbox"]),
              "locks": 0, "reservations": 0, "journals": len(data["ref_journal"])}
    if family == "transformation":
        assert len(data["ref_attempt"]) == len(data["ref_journal"]) == 10
        result["attempts"] = 10
    return result


def close(actual, expected):
    if isinstance(expected, dict):
        assert set(actual) == set(expected)
        for k in expected:
            close(actual[k], expected[k])
    elif isinstance(expected, (int, float)):
        assert math.isclose(actual, expected, rel_tol=1e-10, abs_tol=1e-10), (actual, expected)
    else:
        assert actual == expected


def benchmark(x):
    cp = x["checkpoints"]
    events = cp["http-events"]
    requests = {v["seq"]: v for v in events if v["kind"] == "request"}
    replies = {v["seq"]: v for v in events if v["kind"] == "response"}
    bundle = load(REPO / "adapters/runtime/reference/capabilities.json")["entries"][0]
    measurements = [v for k, v in cp.items() if k.startswith("measurement-")]
    assert len(measurements) == 13
    statuses = Counter()
    identities = {"PASS": set(), "FAIL": set()}
    for m in measurements:
        report, artifact = m["report"], m["artifact"]
        assert report["benchmark_id"] == artifact["benchmark_id"]
        assert digest(artifact) == report["metrics"]["observation_artifact_sha256"]
        assert artifact["samples"] == report["raw_samples"]
        assert artifact["binding"]["fixture_hash"] == report["fixture_hash"]
        assert artifact["profile_hash"] == report["metrics"]["observer_profile_hash"]
        samples = artifact["samples"]
        assert len(samples) == len(artifact["observations"]) == 6
        durations = []
        for i, (sample, observation) in enumerate(zip(samples, artifact["observations"])):
            assert sample["iteration"] == observation["iteration"] == i
            assert sample["warmup"] is (i == 0)
            ns = int(sample["end_monotonic_ns"]) - int(sample["start_monotonic_ns"])
            assert ns > 0
            assert all(sample[k] is None for k in report["metrics"]["unavailable"])
            seq = m["first_http_seq"] + i
            request = requests[seq]
            assert request["method"] == "POST" and request["path"] == "/echo"
            assert request["body_sha256"] == digest(bundle["fixture"]["input"])
            assert request["authorization_present"] is False
            reply = replies.get(seq)
            if observation["wire_sha256"] is not None:
                assert reply["wire_sha256"] == observation["wire_sha256"]
            if sample["success"]:
                assert observation["response"] == bundle["fixture"]["output"]
                assert ns >= int(reply["duration_ns"])
                if i:
                    durations.append(ns / 1e6)
            else:
                assert sample["error"] is not None
        assert len(durations) in (0, 5)
        if durations:
            mean, sd = statistics.mean(durations), statistics.stdev(durations)
            margin = 2.776 * sd / math.sqrt(5)
            expected = {"count": 5, "mean": mean, "median": statistics.median(durations),
                        "p95": max(durations), "sample_stddev": sd,
                        "ci95_mean": {"lower": max(0, mean-margin), "upper": mean+margin}, "unit": "ms"}
        else:
            expected = None
        close(report["metrics"]["latency_ms"], expected)
        close(m["rust_statistics"], expected)
        assert report["status"] == ("PASS" if durations else "FAIL")
        assert report["metrics"]["inference_qualification_eligible"] is False
        assert report["metrics"]["generation_settings_applicable"] is False
        components = report["fingerprint"]["components"]
        assert components["scope"]["value"] == "controlled_reference_http_measurement"
        assert sum(c["status"] == "UNAVAILABLE" for c in components.values()) == 13
        assert all(c["value"] is None for c in components.values() if c["status"] == "UNAVAILABLE")
        identity = digest({k: {"value": c["value"], "status": c["status"]} for k, c in components.items()})
        assert identity == report["fingerprint"]["hash"]
        identities[report["status"]].add(identity)
        statuses[report["status"]] += 1
        rows = [v for v in cp["final-ledger"]["ref_sample"] if v[0] == report["benchmark_id"]]
        assert len(rows) == 6
        for row in rows:
            assert json.loads(row[2]) == samples[row[1]]
            assert json.loads(row[3]) == artifact["observations"][row[1]]
    assert statuses == {"PASS": 4, "FAIL": 9}
    assert all(len(v) == 1 for v in identities.values())
    assert all(v["authorization_present"] is False and v["path"] == "/echo" for v in requests.values())
    return {"measurements": 13, "reports": statuses, "durable_samples": len(cp["final-ledger"]["ref_sample"]),
            "http_requests": len(requests), "statistics_oracle": "Python statistics from integer monotonic ns strings"}


def transformation(x):
    data = bytes(range(256)) * (48 << 10) + b"LARM!"
    encoded = base64.b64encode(data)
    outputs = [v for k, v in x["checkpoints"].items() if k.startswith("output-")]
    assert len(outputs) == 4
    for v in outputs:
        assert v["decoded_sha256"] == hashlib.sha256(data).hexdigest()
        assert v["decoded_bytes"] == str(len(data))
        artifacts = v["manifest"]["artifacts"]
        assert len(artifacts) == 2
        for a in artifacts:
            raw = encoded if a["role"] == "DATA" else rfc8785.dumps(v["provenance"])
            assert a["sha256"] == hashlib.sha256(raw).hexdigest()
            assert a["size_bytes"] == str(len(raw))
        descriptors = sorted(({k: a[k] for k in ("relative_path", "sha256", "required")} |
                              {"size_bytes": int(a["size_bytes"])} for a in artifacts), key=lambda a: a["relative_path"])
        h = digest(descriptors)
        assert v["manifest"]["manifest_hash"] == h
        assert v["manifest"]["revision_key"] == "content:" + h
    return {"verified_outputs": len(outputs), "input_size_bytes": str(len(data)), "output_size_bytes": str(len(encoded))}


def protocol(x):
    cp = x["checkpoints"]
    fingerprints = [v for k, v in cp.items() if k.startswith("fingerprint-")]
    invocations = [v for k, v in cp.items() if k.startswith("invoke-")]
    for fp in fingerprints + [v["fingerprint"] for v in invocations]:
        assert digest(fp["components"]) == fp["hash"]
        assert fp["components"]["backend_identity"]["value"] is None
        assert fp["components"]["backend_identity"]["status"] == "UNAVAILABLE"
    fixture = load(REPO / "adapters/runtime/reference/capabilities.json")["entries"][0]["fixture"]
    for v in invocations:
        assert v["output"] == fixture["output"]
        assert v["usage"]["tokens_input"] is v["usage"]["tokens_output"] is None
    assert len(fingerprints) == 7 and len(invocations) == 6
    requests = cp["http-requests"]
    assert all(v["path"] in ("/echo", "/health") and v["authorization_present"] is False for v in requests)
    return {"fingerprints": len(fingerprints), "invoke_replies": len(invocations), "http_requests": len(requests)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--checks', type=Path, default=OUT / 'native-engine.checks.json')
    args = parser.parse_args()
    checks_path = args.checks if args.checks.is_absolute() else REPO / args.checks
    checks = load(checks_path)
    assert checks["implemented_checks_status"] == "PASS"
    if 'source_before_sha256' in checks:
        assert checks['sources_unchanged'] is True
        assert checks['source_before_sha256'] == checks['source_sha256']
    for p, h in checks["source_sha256"].items():
        assert sha(REPO / p) == h, p
    for c in checks["checks"]:
        assert c["result"] == "PASS" and c["exit_code"] == 0
        assert sha(REPO / c["log"]) == c["sha256"]
    log = (REPO / next(c["log"] for c in checks["checks"] if c["id"] == "workspace-tests")).read_text(encoding="utf-8")
    count = sum(map(int, re.findall(r"test result: ok\. (\d+) passed;", log)))
    binding = {"path": checks_path.relative_to(REPO).as_posix(), "sha256": sha(checks_path),
               "groups": len(checks["checks"]), "source_files": len(checks["source_sha256"]),
               "unchanged_previous_sources": checks.get("previous_preparation", {}).get("unchanged_source_count"),
               "workspace_tests": count}
    for family in ("runtime", "source", "protocol", "transformation", "benchmark"):
        root_path = OUT / f"native-{family}.json"
        root = load(root_path)
        assert root["status"] == "PASS"
        if 'started_at' in checks:
            assert datetime.fromisoformat(checks['started_at']) <= datetime.fromisoformat(root['generated_at']) <= datetime.fromisoformat(checks['generated_at'])
        item = root["outcomes"][0] if family == "runtime" else root
        x = common(item, family)
        result = {"status": "PASS", "scope": "Independent UTF-8/RFC8785 reconstruction of reference observations and Core receipts; retained private fixture evidence, not formal P026 qualification or inference/Core admission.",
                  "checks_receipt": binding, "audit_source_sha256": sha(Path(__file__)),
                  f"{family}_report": {"path": root_path.relative_to(REPO).as_posix(), "sha256": sha(root_path),
                                      "report_sha256": item["sha256"], "functional_cases": len(x["cases"]),
                                      "wire_cases": len(x["wire"]["cases"]), "observations": len(x["observations"]["Ok"]),
                                      "signatures": sum(m.startswith(family + ".") for m in x["methods"])},
                  "generated_at": datetime.now(timezone.utc).isoformat()}
        physical = item["physical_gate"]
        assert physical["status"] == "PASS" and physical["run_sha256"] == x["run_sha256"]
        cases(physical["cases"])
        assert len(physical["cases"]) == 9
        result["physical_gate_cases"] = len(physical["cases"])
        result["physical_gate_sha256"] = digest(physical)
        result["probe_job_id"] = item["probe_job"]["accepted"]["job_id"]
        result["probe_job_sha256"] = digest(item["probe_job"])
        if family in ("source", "transformation", "benchmark"):
            result["private_ledger"] = ledger(x, family)
        if family == "benchmark":
            result.update(benchmark(x))
        if family == "transformation":
            result.update(transformation(x))
        if family == "protocol":
            result.update(protocol(x))
        if family == "runtime":
            assert common(root["outcomes"][1], family)["observations"]["Err"]
            original = load(OUT / "native-wire-original.json")
            for f in original["families"]:
                assert sha(REPO / f["case_file"]) == f["case_sha256"]
                wire(load(REPO / f["case_file"]), digest(f["run"]))
            assert len(original["families"]) == 5
            result["partial_wire_observations"] = 45
        (OUT / f"native-{family}-audit.json").write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
        print(f"PASS: {family} independent native evidence")
    executed = {c['id'] for c in checks['checks']}
    for name, expected, script in (('probe', 14, 'p026_probe_api_real.py'), ('profile', 25, 'p026_profile_api_real.py'), ('source', 25, 'p026_source_api_real.py')):
        if not executed.intersection((f'{name}-api', f'{name}-api-real')):
            continue
        record = load(OUT / f'{name}-api-real.json')
        assert record['status'] == 'PASS' and len(record['steps']) == expected
        assert all(step['result'] == 'PASS' for step in record['steps'])
        if 'source_sha256' in record:
            assert record['source_sha256'] == sha(REPO / 'tools' / script)
        assert record['daemon_sha256'] == sha(REPO / 'target/debug/larm-daemon.exe')
        if 'started_at' in checks:
            assert datetime.fromisoformat(checks['started_at']) <= datetime.fromisoformat(record['generated_at']) <= datetime.fromisoformat(checks['generated_at'])
        if name == 'source':
            trace = record['http_trace']
            assert sha(REPO / trace['path']) == trace['sha256']
            observed = load(REPO / trace['path'])['observations']
            assert len(observed) >= 25
            assert any(v['status'] == 428 for v in observed)
            assert any(v['status'] == 500 for v in observed)
            assert any(v['status'] == 201 and v['response'].get('enabled') is True for v in observed if isinstance(v['response'], dict))
            assert all(set(v) == {'method','path','actor_role','request_sha256','idempotency_key','if_match','status','response'} for v in observed)
        print(f'PASS: {name} fresh real daemon/API evidence ({expected} checks)')
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
