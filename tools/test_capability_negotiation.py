#!/usr/bin/env python3
"""SDK Python real: recursos/estado/callbacks sólo tras negociación válida."""
import sys
import unittest
import uuid
from pathlib import Path

from test_python_adapter_sdk import Worker,Contracts

REPO=Path(__file__).resolve().parent.parent


class Negotiation(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        (REPO/"docs/reports/P025_runs").mkdir(exist_ok=True)
        cls.contracts=Contracts(REPO/"docs")

    def worker(self,mode,**options):
        instance=Worker(self.contracts,gate_mode=mode,**options)
        self.addCleanup(instance.close)
        return instance

    def events(self,worker):
        return worker.trace.read_text(encoding="utf-8").splitlines() if worker.trace.exists() else []

    def hello(self,worker,version="1.0",identity=None):
        worker.send("core.handshake",{"protocol_version":version,"adapter_id":identity or worker.manifest["id"],"nonce":uuid.uuid4().hex})
        return worker.receive()

    def blocked_call(self,worker):
        worker.search()
        response=worker.receive()
        self.assertEqual(response["error"]["data"]["details"]["reason"],"handshake_required")

    def test_missing_registry_or_capability_never_opens_or_invokes(self):
        for mode in ("missing-registry","missing-capability"):
            worker=self.worker(mode)
            reply=self.hello(worker)
            self.assertEqual(reply["error"]["data"]["details"]["reason"],"capability_descriptor_missing")
            self.blocked_call(worker)
            self.assertEqual(self.events(worker),[])

    def test_invalid_or_raised_estimator_never_opens_and_never_leaks(self):
        for mode in ("invalid-estimate","raised-estimate"):
            worker=self.worker(mode)
            reply=self.hello(worker)
            self.assertEqual(reply["error"]["data"]["details"]["reason"],"capability_estimator_failed")
            self.assertNotIn("private-estimator-marker",str(reply))
            self.blocked_call(worker)
            self.assertEqual(self.events(worker),["estimate"])

    def test_bad_major_and_identity_before_resources_then_one_session_open(self):
        worker=self.worker("normal")
        for version,identity in (("2.0",None),("1.0","different.adapter")):
            self.assertEqual(self.hello(worker,version,identity)["error"]["data"]["details"]["reason"],"handshake_mismatch")
            self.assertEqual(self.events(worker),[])
            self.blocked_call(worker)
        self.assertIn("result",self.hello(worker))
        self.assertEqual(self.events(worker),["estimate","open"])
        self.assertIn("result",self.hello(worker))
        self.assertEqual(self.events(worker),["estimate","open","estimate"])
        worker.search(); self.assertIn("result",worker.receive())
        self.assertEqual(self.events(worker),["estimate","open","estimate","call"])

    def test_failed_session_open_does_not_authorize_callbacks_or_leak(self):
        worker=self.worker("session-failure")
        reply=self.hello(worker)
        self.assertEqual(reply["error"]["data"]["details"]["reason"],"adapter_session_start_failed")
        self.assertNotIn("private-session-marker",str(reply))
        self.blocked_call(worker)
        self.assertEqual(self.events(worker),["estimate","open"])

    def test_explicit_empty_capabilities_does_not_invent_a_registry(self):
        worker=self.worker("normal",capabilities=[])
        self.assertEqual(self.hello(worker)["result"]["capabilities"],[])
        self.assertEqual(self.events(worker),["open"])
        worker.search(); self.assertIn("result",worker.receive())
        self.assertEqual(self.events(worker),["open","call"])


if __name__=="__main__": unittest.main(verbosity=2)
