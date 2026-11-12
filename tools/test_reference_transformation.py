#!/usr/bin/env python3
"""Límites de la conversión/IO real; el proceso completo tiene otro runner."""
import base64
import hashlib
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0,str(REPO / "adapters/sdk/python"))
sys.path.insert(0,str(REPO / "adapters/transformation/reference"))
from larm_adapter_sdk import DomainError  # noqa: E402
import files  # noqa: E402


class Context:
    def __init__(self,cancel_at=None):
        self.cancel_at,self.progresses=cancel_at,[]
    def check_cancelled(self):
        if self.cancel_at is not None and self.progresses and self.progresses[-1][1]>=self.cancel_at:
            raise DomainError("RESOURCE_UNAVAILABLE","Cancelado","operation_cancelled")
    def progress(self,*values): self.progresses.append(values)


class Transformation(unittest.TestCase):
    def setUp(self):
        self.base=(REPO / "docs/reports/P026_runs").resolve()
        self.temp=tempfile.TemporaryDirectory(prefix="transform-unit-",dir=self.base)
        self.root=Path(self.temp.name).resolve()
        assert self.root.is_relative_to(self.base)
        self.read,self.write=self.root/"input",self.root/"output"
        self.read.mkdir(); self.write.mkdir()
        self.source,self.target=self.read/"input.bin",self.write/"encoded.b64"
    def tearDown(self):
        assert self.root.resolve().is_relative_to(self.base)
        self.temp.cleanup()
    def expected(self,data): return {"sha256":hashlib.sha256(data).hexdigest(),"size_bytes":str(len(data))}
    def run_encoding(self,data,ctx=None,prediction=None):
        self.source.write_bytes(data)
        ctx=ctx or Context()
        prediction=prediction or files.inspect_input(self.source,self.expected(data),[self.read],Context())
        checkpoints=[]
        def checkpoint(offset):
            # Observación del fichero real en el momento de confirmar el ledger.
            self.assertEqual(self.target.stat().st_size,offset)
            checkpoints.append(offset)
        files.encode(self.source,self.target,self.expected(data),prediction,[self.read],[self.write],ctx,checkpoint,lambda _:None)
        return self.target.read_bytes(),checkpoints
    def test_empty_and_padding_goldens(self):
        for raw,expected in ((b"",b""),(b"f",b"Zg=="),(b"fo",b"Zm8="),(b"foo",b"Zm9v"),(b"foob",b"Zm9vYg=="),(b"fooba",b"Zm9vYmE="),(b"foobar",b"Zm9vYmFy")):
            with self.subTest(raw=raw):
                output,_=self.run_encoding(raw)
                self.assertEqual(output,expected)
                self.assertEqual(self.source.read_bytes(),raw)
                self.target.unlink()
    def test_stream_boundaries_inverse_and_durable_checkpoint(self):
        for size in (files.BLOCK-1,files.BLOCK,files.BLOCK+1,2*files.BLOCK+2):
            data=(bytes(range(256))*((size+255)//256))[:size]
            encoded,checkpoints=self.run_encoding(data)
            self.assertEqual(base64.b64decode(encoded,validate=True),data)
            self.assertEqual(checkpoints[-1],len(encoded))
            self.assertEqual(len(encoded),4*((size+2)//3))
            self.target.unlink()
    def test_cancel_keeps_original_and_durable_prefix(self):
        data=b"LARM-owned\x00"*20000
        ctx=Context(cancel_at=65536)
        with self.assertRaises(DomainError) as error: self.run_encoding(data,ctx)
        self.assertEqual(error.exception.data["details"]["reason"],"operation_cancelled")
        encoded=self.target.read_bytes()
        self.assertLess(len(encoded),4*((len(data)+2)//3))
        self.assertTrue(data.startswith(base64.b64decode(encoded,validate=True)))
        self.assertEqual(self.source.read_bytes(),data)
    def test_output_budget_and_existing_target_never_overwritten(self):
        data=b"not empty"
        prediction={"sha256":"a"*64,"size_bytes":"1"}
        with self.assertRaises(DomainError) as error: self.run_encoding(data,prediction=prediction)
        self.assertEqual(error.exception.data["details"]["reason"],"reference_transform_output_budget_exceeded")
        self.assertEqual(self.target.read_bytes(),b"")
        self.target.write_bytes(b"existing-owned-sentinel")
        with self.assertRaises(FileExistsError): self.run_encoding(data)
        self.assertEqual(self.target.read_bytes(),b"existing-owned-sentinel")
        self.assertEqual(self.source.read_bytes(),data)
    def test_preflight_changed_hash_and_size_has_no_output(self):
        expected=self.expected(b"original")
        for data in (b"changed!",b"short"):
            self.source.write_bytes(data)
            with self.assertRaises(DomainError) as error: files.inspect_input(self.source,expected,[self.read],Context())
            self.assertEqual(error.exception.data["details"]["reason"],"reference_transform_input_changed")
            self.assertEqual(list(self.write.iterdir()),[])
    def test_bundle_uses_numeric_sizes_and_rejects_alias_paths(self):
        entries=[{"relative_path":"B.b64","sha256":"b"*64,"size_bytes":"9007199254740992","required":False,"role":"DATA"},{"relative_path":"a.bin","sha256":"a"*64,"size_bytes":"12","required":True,"role":"DATA"}]
        # Cadena exacta independiente del helper, conforme a BundleEntry del dominio.
        raw='[{"relative_path":"B.b64","required":false,"sha256":"'+"b"*64+'","size_bytes":9007199254740992},{"relative_path":"a.bin","required":true,"sha256":"'+"a"*64+'","size_bytes":12}]'
        self.assertEqual(files.bundle_hash(entries),hashlib.sha256(raw.encode()).hexdigest())
        self.assertEqual(files.bundle_hash(entries),files.bundle_hash(list(reversed(entries))))
        for invalid in (entries+[entries[0]|{"relative_path":"b.b64"}],[entries[0]|{"size_bytes":"9007199254740993"}],[entries[0]|{"relative_path":"../escaped"}],[entries[0]|{"size_bytes":"00"}]):
            with self.assertRaises(DomainError): files.bundle_hash(invalid)
    @unittest.skipUnless(os.name=="nt","junction Windows")
    def test_real_junction_never_reads_external_input(self):
        outside=self.root/"external"; outside.mkdir(); (outside/"secret.bin").write_bytes(b"owned-external-sentinel")
        link=self.read/"junction"
        quote=lambda value:"'"+str(value).replace("'","''")+"'"
        subprocess.run(["powershell","-NoProfile","-Command",f"New-Item -ItemType Junction -Path {quote(link)} -Target {quote(outside)} | Out-Null"],capture_output=True,check=True,timeout=15)
        with self.assertRaises(DomainError) as error: files.inspect_input(link/"secret.bin",self.expected(b"owned-external-sentinel"),[self.read],Context())
        self.assertEqual(error.exception.data["details"]["reason"],"reference_reparse_denied")
        self.assertEqual(list(self.write.iterdir()),[])
        self.assertEqual((outside/"secret.bin").read_bytes(),b"owned-external-sentinel")


if __name__=="__main__":
    unittest.main(verbosity=2)
