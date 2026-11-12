#!/usr/bin/env python3
"""Oráculos estáticos de estadísticas y fingerprint del benchmark de referencia."""
import copy
import importlib.util
import sys
import unittest
from pathlib import Path

REPO=Path(__file__).resolve().parent.parent
sys.path.insert(0,str(REPO/"adapters/sdk/python"))
spec=importlib.util.spec_from_file_location("reference_benchmark_stats",REPO/"adapters/benchmark/reference/stats.py")
stats=importlib.util.module_from_spec(spec); spec.loader.exec_module(stats)


def samples(values):
    return [{"warmup":i==0,"success":True,"start_monotonic_ns":"10000000000","end_monotonic_ns":str(10_000_000_000+int(ms*1_000_000))} for i,ms in enumerate([1000]+values)]


class Statistics(unittest.TestCase):
    def test_five_repetitions_warmup_excluded_nearest_rank_and_sample_variance(self):
        result=stats.summarize(samples([5,1,4,2,3]))
        self.assertEqual({key:result[key] for key in ("count","mean","median","p95","unit")},{"count":5,"mean":3.,"median":3.,"p95":5.,"unit":"ms"})
        self.assertAlmostEqual(result["sample_stddev"],1.5811388300841898,places=13)
        self.assertAlmostEqual(result["ci95_mean"]["lower"],1.037071575426144,places=12)
        self.assertAlmostEqual(result["ci95_mean"]["upper"],4.962928424573856,places=12)

    def test_insufficient_or_nonpositive_samples_never_get_a_summary(self):
        for values in ([1,2,3,4],[1,2,3,4,0],[1,2,3,4,-1],[1,2,3,4,5,6]):
            self.assertIsNone(stats.summarize(samples(values)))
        failed=samples([1,2,3,4,5]); failed[3]["success"]=False
        self.assertIsNone(stats.summarize(failed))

    def test_confidence_interval_nonnegative_and_equal_samples_have_zero_spread(self):
        result=stats.summarize(samples([1,1,1,1,1000]))
        self.assertEqual(result["ci95_mean"]["lower"],0.)
        self.assertGreater(result["ci95_mean"]["upper"],result["mean"])
        result=stats.summarize(samples([2]*5))
        self.assertEqual(result["sample_stddev"],0.)
        self.assertEqual(result["ci95_mean"],{"lower":2.,"upper":2.})

    def test_fingerprint_trace_and_order_are_separate_from_execution_identity(self):
        components={"protocol":{"value":"reference.http.echo.v1","status":"VERIFIED","evidence_refs":["old-trace"]},"backend":{"value":None,"status":"UNAVAILABLE","evidence_refs":[]}}
        same=copy.deepcopy(dict(reversed(list(components.items())))); same["protocol"]["evidence_refs"]=["new-trace","2026-10-18T00:00:00.000Z"]
        self.assertEqual(stats.fingerprint_hash(components),stats.fingerprint_hash(same))
        changed=copy.deepcopy(same); changed["protocol"]["status"]="DECLARED"
        self.assertNotEqual(stats.fingerprint_hash(components),stats.fingerprint_hash(changed))
        changed=copy.deepcopy(same); changed["backend"].update(value="cpu",status="DECLARED")
        self.assertNotEqual(stats.fingerprint_hash(components),stats.fingerprint_hash(changed))


if __name__=="__main__": unittest.main(verbosity=2)
