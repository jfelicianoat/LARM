"""Physical IO tests on owned fixtures; no daemon, network or model execution."""
import hashlib
import json
import os
from pathlib import Path
import struct
import sys
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[4]
sys.path[:0] = [str(REPO / "adapters/sdk/python"), str(REPO / "adapters/runtime/ollama")]
from larm_adapter_sdk import Contracts, DomainError
from store import Store, open_readonly, parse_json, MANIFEST_TYPES, MAX_BYTES

MEDIA = "application/vnd.docker.distribution.manifest.v2+json"
CONFIG = "application/vnd.docker.container.image.v1+json"
MODEL = "application/vnd.ollama.image.model"


class Context:
    def __init__(self, cancel_at=None):
        self.calls, self.cancel_at, self.events = 0, cancel_at, []

    def check_cancelled(self):
        self.calls += 1
        if self.cancel_at is not None and self.calls >= self.cancel_at:
            raise DomainError("RESOURCE_UNAVAILABLE", "Cancelado", "operation_cancelled")

    def progress(self, *args):
        self.events.append(args)


class StoreTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.contracts = Contracts(REPO / "docs")

    def setUp(self):
        parent = REPO / "target/tmp"
        parent.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=parent)
        self.base = Path(self.temp.name)
        self.root = self.base / "store"
        (self.root / "blobs").mkdir(parents=True)
        self.manifest = self.root / "manifests/registry.ollama.ai/library/test/tiny"
        self.manifest.parent.mkdir(parents=True)
        self.config = self.blob(json.dumps({"model_format":"gguf", "file_type":"F16", "architecture":"amd64",
                                            "model_family":"bert", "new_vendor_field":{"arbitrary":True}}).encode(), CONFIG)
        # Deliberately synthetic bytes; header inspection is not a load smoke.
        self.model = self.blob(b"GGUF"+struct.pack("<IQQ",3,0,0)+b"owned unit fixture", MODEL)
        self.license = self.blob(b"self-authored fixture; no upstream license acceptance", "application/vnd.ollama.image.license")
        self.value = {"schemaVersion":2,"mediaType":MEDIA,"config":self.config,"layers":[self.model,self.license]}
        self.save()

    def tearDown(self):
        self.assertTrue(self.base.resolve().is_relative_to((REPO / "target/tmp").resolve()))
        self.temp.cleanup()

    def blob(self, raw, media):
        checksum = hashlib.sha256(raw).hexdigest()
        (self.root / "blobs" / ("sha256-"+checksum)).write_bytes(raw)
        return {"mediaType":media,"digest":"sha256:"+checksum,"size":len(raw)}

    def blob_path(self, descriptor):
        return self.root / "blobs" / descriptor["digest"].replace(":","-",1)

    def save(self):
        self.manifest.write_text(json.dumps(self.value),encoding="utf-8")

    def inspect(self, hint=None, ctx=None):
        result = Store(self.root).inspect(self.manifest, ctx or Context(), hint)
        self.contracts.response("runtime.inspect_model",result)
        return result

    def error(self, reason, call=None):
        with self.assertRaises(DomainError) as error:
            (call or self.inspect)()
        self.assertEqual(error.exception.data["details"]["reason"],reason)

    def test_physical_hashes_roles_schema_unknowns_and_no_mutation(self):
        before = {str(p.relative_to(self.root)):p.read_bytes() for p in self.root.rglob("*") if p.is_file()}
        ctx = Context()
        result = self.inspect("GGUF",ctx)
        self.assertEqual((result["format"], result["quantization"]),("GGUF","F16"))
        self.assertIsNone(result["architecture"])
        self.assertIsNone(result["license"])
        self.assertEqual(result["capabilities"],[])
        self.assertEqual(result["limits"],{})
        self.assertEqual({a["role"] for a in result["artifacts"]},{"manifest","config","weights","license"})
        for artifact in result["artifacts"]:
            raw = (self.root / artifact["relative_path"]).read_bytes()
            self.assertEqual((artifact["size_bytes"],artifact["sha256"]),(str(len(raw)),hashlib.sha256(raw).hexdigest()))
        self.assertEqual(before,{str(p.relative_to(self.root)):p.read_bytes() for p in self.root.rglob("*") if p.is_file()})
        self.assertTrue(ctx.events)
        self.assertEqual(result,self.inspect("GGUF"))

    def test_nested_manifest_deduplicates_shared_config_and_layers(self):
        nested = self.blob(json.dumps(self.value).encode(),MEDIA)
        self.value["layers"].append(nested)
        self.save()
        result = self.inspect()
        self.assertEqual(len(result["artifacts"]),5)
        self.assertEqual(sum(a["role"]=="weights" for a in result["artifacts"]),1)

    def test_descriptor_from_and_urls_never_choose_a_read_path(self):
        self.model.update({"from":"C:/not-authorized/never-opened", "urls":["https://not-authorized.invalid/model"]})
        self.save()
        self.assertEqual(self.inspect()["format"],"GGUF")

    def test_missing_manifest_is_model_not_found(self):
        self.manifest.unlink()
        self.error("ollama_local_file_missing")

    def test_missing_blob_is_not_successful_partial_inspection(self):
        self.blob_path(self.model).unlink()
        self.error("ollama_local_file_missing")

    def test_blob_hash_mismatch(self):
        raw = self.blob_path(self.model).read_bytes()
        self.blob_path(self.model).write_bytes(raw[:-1]+b"!")
        self.error("ollama_blob_hash_mismatch")

    def test_blob_size_mismatch(self):
        self.blob_path(self.model).write_bytes(b"short")
        self.error("ollama_blob_size_mismatch")

    def test_invalid_sizes_and_digests_are_rejected(self):
        original = dict(self.model)
        for field,value in [("size",True),("size",-1),("size",MAX_BYTES+1),("size","42"),
                            ("digest","sha256:../../outside"),("digest","sha512:"+"a"*64)]:
            self.value["layers"][0] = original | {field:value}
            self.save()
            self.error("ollama_descriptor_invalid")

    def test_remote_manifest_is_not_local(self):
        self.value["remote_host"] = "https://ollama.com"
        self.save()
        self.error("ollama_model_is_remote")

    def test_remote_config_is_not_local(self):
        self.value["config"] = self.blob(b'{"remote_model":"cloud-model"}',CONFIG)
        self.save()
        self.error("ollama_model_is_remote")

    def test_outside_store_and_non_manifest_paths_are_rejected(self):
        outside = self.base / "outside"
        outside.write_text("not authorized")
        self.error("reference_path_outside_roots",lambda:Store(self.root).inspect(outside,Context()))
        self.error("ollama_manifest_outside_store",lambda:Store(self.root).inspect(self.blob_path(self.model),Context()))

    def test_config_image_architecture_does_not_become_model_architecture(self):
        self.assertIsNone(self.inspect()["architecture"])

    def test_malformed_json_duplicate_keys_and_non_finite_values(self):
        for raw in (b'{"a":1,"a":2}',b'{"a":NaN}',b'{"a":Infinity}',b'not-json'):
            self.error("ollama_metadata_json_invalid",lambda:parse_json(raw))

    def test_excessively_nested_json_has_a_normalized_error(self):
        self.error("ollama_metadata_json_invalid",lambda:parse_json(b'{"nested":'+b'['*2000+b'0'+b']'*2000+b'}'))

    def test_brackets_and_escaped_quotes_inside_strings_do_not_count_as_depth(self):
        value = {"vendor_text":'["'+"["*2000+'\\"'}
        self.assertEqual(parse_json(json.dumps(value).encode()),value)

    def test_special_windows_path_is_rejected_before_open(self):
        self.error("relative_path_invalid",lambda:Store(self.root).inspect(str(self.manifest)+":hidden-stream",Context()))

    def test_gguf_hint_and_physical_header_are_checked(self):
        self.error("ollama_format_hint_mismatch",lambda:self.inspect("SAFETENSORS"))
        self.value["layers"][0] = self.blob(b"not a GGUF file, but correct hash",MODEL)
        self.save()
        self.error("ollama_model_format_mismatch")

    def test_conflicting_descriptors_are_rejected(self):
        self.value["layers"].append(self.model | {"size":self.model["size"]+1})
        self.save()
        self.error("ollama_descriptor_conflict")

    def test_cancelled_hash_releases_file_handles(self):
        self.error("operation_cancelled",lambda:self.inspect(ctx=Context(cancel_at=3)))
        self.blob_path(self.config).write_bytes(b"owned test can still edit after cancellation")

    def test_nested_manifest_depth_is_bounded(self):
        value = self.value
        for _ in range(10):
            nested = self.blob(json.dumps(value).encode(),MEDIA)
            value = {"schemaVersion":2,"mediaType":MEDIA,"config":self.config,"layers":[nested]}
        self.value = value
        self.save()
        self.error("ollama_manifest_depth_limit")

    @unittest.skipUnless(os.name=="nt","Windows native file-sharing test")
    def test_windows_read_pin_denies_write_and_rename(self):
        path = self.blob_path(self.model)
        with open_readonly(path,self.root) as stream:
            self.assertEqual(stream.read(4),b"GGUF")
            with self.assertRaises(OSError):
                path.write_bytes(b"must fail")
            with self.assertRaises(OSError):
                path.rename(path.with_name("renamed"))
        self.assertTrue(path.exists())

    @unittest.skipUnless(os.name=="nt","Windows native junction test")
    def test_windows_junction_escape_is_rejected(self):
        import _winapi
        outside = self.base / "owned-outside"
        outside.mkdir()
        (outside / "model").write_text("outside")
        junction = self.root / "escape"
        _winapi.CreateJunction(str(outside),str(junction))
        try:
            self.error("reference_reparse_denied",lambda:Store(self.root).inspect(junction / "model",Context()))
        finally:
            os.rmdir(junction)


if __name__ == "__main__":
    unittest.main(verbosity=2)
