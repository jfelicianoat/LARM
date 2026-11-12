from __future__ import annotations

import hashlib
import json
import os
import struct
import sys
import tempfile
import threading
import time
import unittest
import zlib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
PROJECT_ROOT = ROOT.parents[2]
sys.path.insert(0, str(ROOT))
import worker  # noqa: E402
import sidecar  # noqa: E402

RUNTIME = "0192aaaa-0000-7000-8000-000000000001"
VERSION = "0192aaaa-0000-7000-8000-000000000002"
BACKEND = "0192aaaa-0000-7000-8000-000000000003"
LEASE = "0192aaaa-0000-7000-8000-000000000004"
REQUEST = "0192aaaa-0000-7000-8000-000000000005"


def _png(width: int, height: int) -> bytes:
    def chunk(kind: bytes, payload: bytes) -> bytes:
        return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)

    scanline = b"\x00" + bytes(width * 3)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(scanline * height))
        + chunk(b"IEND", b"")
    )


class FakeImage:
    def __init__(self, width: int, height: int):
        self.size = (width, height)
        self.png = _png(width, height)

    def save(self, path: str | Path, format: str) -> None:
        if format != "PNG":
            raise AssertionError(format)
        Path(path).write_bytes(self.png)


class FakePipeline:
    def __init__(self):
        self.device = None
        self.calls: list[dict[str, object]] = []

    def to(self, device: str):
        self.device = device
        return self

    def __call__(self, **kwargs):
        self.calls.append(kwargs)
        callback = kwargs["callback_on_step_end"]
        for step in range(kwargs["num_inference_steps"]):
            callback(self, step, step, {})
        return SimpleNamespace(images=[FakeImage(kwargs["width"], kwargs["height"])])


class FakeGenerator:
    def __init__(self, device: str):
        self.device = device
        self.seed = None

    def manual_seed(self, seed: int):
        self.seed = seed
        return self


class FakeTorch:
    __version__ = "2.5.0-test"
    Generator = FakeGenerator
    cuda = SimpleNamespace(is_available=lambda: False)
    xpu = SimpleNamespace(is_available=lambda: False)


class DiffusersWorkerTests(unittest.TestCase):
    def setUp(self):
        test_root = ROOT / "tests"
        test_root.mkdir(parents=True, exist_ok=True)
        self.sandbox = Path(tempfile.mkdtemp(prefix="p072-", dir=test_root))
        self.model_root = self.sandbox / "models"
        self.model = self.model_root / "tiny-fixture"
        self.output_root = self.sandbox / "outputs"
        self.model.mkdir(parents=True)
        self.output_root.mkdir()
        self._make_model()
        self.env = {
            "LARM_DIFFUSERS_MODEL_ROOT": str(self.model_root),
            "LARM_DIFFUSERS_MODEL_PATH": str(self.model),
            "LARM_DIFFUSERS_OUTPUT_ROOT": str(self.output_root),
            "LARM_DIFFUSERS_BACKEND": "CPU",
        }

    def tearDown(self):
        import shutil

        shutil.rmtree(self.sandbox, ignore_errors=True)

    def _write_json(self, path: Path, value: dict[str, object]) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value, sort_keys=True), encoding="utf-8")

    def _make_model(self, pipeline_class: str = "StableDiffusionPipeline", *, custom_text: bool = False) -> None:
        text_encoder = ["transformers", "CLIPTextModel"]
        if custom_text:
            text_encoder = ["transformers_modules", "CustomEncoder"]
        index = {
            "_class_name": pipeline_class,
            "_diffusers_version": "test-fixture",
            "unet": ["diffusers", "UNet2DConditionModel"],
            "vae": ["diffusers", "AutoencoderKL"],
            "text_encoder": text_encoder,
            "tokenizer": ["transformers", "CLIPTokenizer"],
            "scheduler": ["diffusers", "DDIMScheduler"],
            "safety_checker": None,
        }
        self._write_json(self.model / "model_index.json", index)
        for name in ("unet", "vae", "text_encoder"):
            self._write_json(self.model / name / "config.json", {"fixture": True})
            (self.model / name / "diffusion_pytorch_model.safetensors").write_bytes(b"fixture-only-not-real-weights")
        self._write_json(self.model / "tokenizer" / "tokenizer.json", {"fixture": True})
        self._write_json(self.model / "scheduler" / "scheduler_config.json", {"_class_name": "DDIMScheduler"})

    def _request(self) -> dict[str, object]:
        return {
            "prompt": "test image",
            "negative_prompt": "",
            "width": 64,
            "height": 64,
            "steps": 3,
            "seed": 17,
        }

    def test_environment_lock_hash_separates_diffusers_dependencies(self):
        base = {
            "runtime_id": RUNTIME,
            "runtime_version_id": VERSION,
            "backend_id": BACKEND,
            "python_version": "3.12.4",
            "dependencies_lock_sha256": "a" * 64,
            "strategy": "UV",
            "announced_strategies": ["UV"],
        }
        first = worker.environment_plan(base)
        base["dependencies_lock_sha256"] = "b" * 64
        second = worker.environment_plan(base)
        self.assertNotEqual(first["environment_id"], second["environment_id"])
        self.assertNotEqual(first["relative_path"], second["relative_path"])
        self.assertFalse(first["created"])

    def test_inspection_maps_diffusers_components_to_contract_roles(self):
        result = worker.inspect_bundle(str(self.model_root), str(self.model))
        roles = {item["component"]: item["role"] for item in result["component_roles"]}
        self.assertEqual(roles, {
            "scheduler": "config",
            "text_encoder": "text_encoder",
            "tokenizer": "tokenizer",
            "unet": "weights",
            "vae": "vae",
        })
        unet = next(item for item in result["components"] if item["facts"].get("diffusers_component") == "unet")
        self.assertEqual(unet["role"], "weights")
        self.assertEqual(unet["size_bytes"], str(len(b"fixture-only-not-real-weights")))

    def test_custom_pipeline_and_custom_component_are_blocked_before_import(self):
        self._write_json(self.model / "model_index.json", {"_class_name": "UserPipeline"})
        with patch.dict(os.environ, self.env), patch.object(worker, "_managed_interpreter", return_value=True):
            with self.assertRaises(worker.WorkerError) as error:
                worker.load_pipeline(str(self.model), "CPU", importer=lambda _name: self.fail("no dynamic imports"))
        self.assertEqual(error.exception.code, "REMOTE_CODE_REQUIRED")

        self._make_model(custom_text=True)
        with self.assertRaises(worker.WorkerError) as error:
            worker.inspect_bundle(str(self.model_root), str(self.model))
        self.assertEqual(error.exception.code, "REMOTE_CODE_REQUIRED")

    def test_bundle_rejects_unknown_paths_and_pickle_only_weights(self):
        with self.assertRaises(worker.WorkerError) as error:
            worker.inspect_bundle(str(self.model_root), str(self.sandbox))
        self.assertEqual(error.exception.code, "MODEL_UNAVAILABLE")

        (self.model / "unet" / "diffusion_pytorch_model.safetensors").unlink()
        (self.model / "unet" / "pytorch_model.bin").write_bytes(b"unsafe")
        with self.assertRaises(worker.WorkerError) as error:
            worker.inspect_bundle(str(self.model_root), str(self.model))
        self.assertEqual(error.exception.code, "UNSAFE_WEIGHTS")

    def test_image_input_is_strict_and_rejects_booleans_as_integers(self):
        invalid = self._request()
        invalid["width"] = True
        with self.assertRaises(worker.WorkerError) as error:
            worker.validate_image_input(invalid)
        self.assertEqual(error.exception.code, "INVALID_REQUEST")
        invalid = self._request()
        invalid["remote_code"] = True
        with self.assertRaises(worker.WorkerError):
            worker.validate_image_input(invalid)

    def test_mock_pipeline_writes_contract_image_and_local_metadata(self):
        pipeline = FakePipeline()
        events: list[tuple[int, int]] = []
        with patch.dict(os.environ, self.env):
            result = worker.generate_image(
                REQUEST,
                self._request(),
                pipeline_loader=lambda _model, _backend: (pipeline, FakeTorch(), 0.25),
                progress=lambda done, total: events.append((done, total)),
            )
        image_output = result["output"]
        schema = json.loads((PROJECT_ROOT / "docs/contracts/schemas/larm.schema.json").read_text(encoding="utf-8"))
        import jsonschema

        jsonschema.Draft202012Validator(schema["$defs"]["image_generate_output"]).validate(image_output)
        image_file = self.output_root / REQUEST / "image.png"
        self.assertEqual(image_file.read_bytes()[:8], b"\x89PNG\r\n\x1a\n")
        self.assertEqual(image_output["images"][0]["sha256"], hashlib.sha256(image_file.read_bytes()).hexdigest())
        self.assertEqual(events, [(1, 3), (2, 3), (3, 3)])
        metadata = json.loads((self.output_root / REQUEST / "metadata.json").read_text(encoding="utf-8"))
        self.assertEqual(metadata["seed"], 17)
        self.assertEqual(metadata["peak_memory_bytes"], None)
        self.assertEqual(metadata["memory_reason"], "CPU_PEAK_MEMORY_UNAVAILABLE")
        self.assertNotIn("test image", json.dumps(metadata))
        self.assertEqual(pipeline.device, None)  # injected loader owns its test device setup

    def test_loader_forces_local_safetensors_and_explicit_cpu(self):
        calls: dict[str, object] = {}

        class Loader:
            @classmethod
            def from_pretrained(cls, path, **kwargs):
                calls["path"] = path
                calls["kwargs"] = kwargs
                return FakePipeline()

        def importer(name: str):
            calls.setdefault("imports", []).append(name)
            if name == "torch":
                return FakeTorch()
            if name == "diffusers":
                return SimpleNamespace(StableDiffusionPipeline=Loader)
            raise AssertionError(name)

        with patch.dict(os.environ, self.env), patch.object(worker, "_managed_interpreter", return_value=True):
            _pipeline, _torch, _elapsed = worker.load_pipeline(str(self.model), "CPU", importer=importer)
            self.assertEqual(os.environ["HF_HUB_OFFLINE"], "1")
        self.assertEqual(calls["imports"], ["torch", "diffusers"])
        self.assertEqual(calls["path"], str(self.model))
        self.assertEqual(calls["kwargs"], {
            "local_files_only": True,
            "use_safetensors": True,
            "trust_remote_code": False,
        })

    def test_cancel_during_pipeline_removes_partial_artifacts(self):
        pipeline = FakePipeline()
        cancellation = threading.Event()

        def set_cancelled(done: int, _total: int):
            if done == 1:
                cancellation.set()

        with patch.dict(os.environ, self.env):
            with self.assertRaises(worker.WorkerError) as error:
                worker.generate_image(
                    REQUEST,
                    self._request(),
                    pipeline_loader=lambda _model, _backend: (pipeline, FakeTorch(), 0.1),
                    progress=set_cancelled,
                    cancelled=cancellation.is_set,
                )
        self.assertEqual(error.exception.code, "CANCELLED")
        self.assertFalse((self.output_root / REQUEST).exists())

    def test_output_io_failure_leaves_no_partial_job_directory(self):
        class BrokenImage(FakeImage):
            def save(self, _path, format):
                raise OSError("fixture disk failure")

        class BrokenPipeline(FakePipeline):
            def __call__(self, **kwargs):
                return SimpleNamespace(images=[BrokenImage(kwargs["width"], kwargs["height"])])

        with patch.dict(os.environ, self.env):
            with self.assertRaises(worker.WorkerError) as error:
                worker.generate_image(
                    REQUEST,
                    self._request(),
                    pipeline_loader=lambda _model, _backend: (BrokenPipeline(), FakeTorch(), 0.1),
                )
        self.assertEqual(error.exception.code, "OUTPUT_IO_FAILED")
        self.assertFalse((self.output_root / REQUEST).exists())

    def test_protocol_response_uses_contract_fingerprint_and_reports_progress(self):
        events: list[dict[str, object]] = []

        def image_generator(request_id, payload, *, progress, cancelled):
            progress(1, payload["steps"])
            self.assertFalse(cancelled())
            return {
                "output": {"images": [{
                    "artifact_id": request_id,
                    "sha256": "a" * 64,
                    "media_type": "image/png",
                    "size_bytes": "128",
                }]},
                "metrics": {"seconds_per_image": 0.5},
            }

        fingerprint = {
            "hash": "b" * 64,
            "components": {"backend": {"value": "CPU", "status": "VERIFIED", "evidence_refs": ["fixture:P072"]}},
        }
        handler = sidecar.DiffusersProtocolHandler(
            fingerprint_provider=lambda: fingerprint,
            progress_sink=events.append,
            image_generator=image_generator,
        )
        response = handler.invoke({
            "lease_id": LEASE,
            "request_id": REQUEST,
            "capability": "image.generate",
            "input": self._request(),
            "generation": None,
            "stream": False,
        })
        schema = json.loads((PROJECT_ROOT / "docs/contracts/schemas/larm.schema.json").read_text(encoding="utf-8"))
        import jsonschema

        jsonschema.Draft202012Validator(schema).validate(response)
        self.assertEqual(handler.heartbeat(), {"alive": True, "running_request_ids": []})
        self.assertEqual(handler.progress(REQUEST)["completed_steps"], 1)
        self.assertIn("core.progress", [event["method"] for event in events])

    def test_protocol_cancel_reaches_running_pipeline_and_clears_active_state(self):
        entered = threading.Event()
        events: list[dict[str, object]] = []

        def image_generator(request_id, payload, *, progress, cancelled):
            progress(1, payload["steps"])
            entered.set()
            while not cancelled():
                time.sleep(0.005)
            raise worker.WorkerError("CANCELLED", "cancelled fixture")

        handler = sidecar.DiffusersProtocolHandler(
            fingerprint_provider=lambda: {
                "hash": "b" * 64,
                "components": {"backend": {"value": "CPU", "status": "VERIFIED", "evidence_refs": ["fixture:P072"]}},
            },
            progress_sink=events.append,
            image_generator=image_generator,
        )
        request = {
            "lease_id": LEASE,
            "request_id": REQUEST,
            "capability": "image.generate",
            "input": self._request(),
            "generation": None,
            "stream": False,
        }
        with ThreadPoolExecutor(max_workers=1) as executor:
            future = executor.submit(handler.invoke, request)
            self.assertTrue(entered.wait(timeout=2))
            self.assertEqual(handler.heartbeat()["running_request_ids"], [REQUEST])
            self.assertEqual(handler.cancel({"request_id": REQUEST}), {"cancelled": True})
            with self.assertRaises(worker.WorkerError) as error:
                future.result(timeout=2)
        self.assertEqual(error.exception.code, "CANCELLED")
        self.assertEqual(handler.heartbeat()["running_request_ids"], [])
        self.assertEqual(handler.cancel({"request_id": REQUEST}), {"cancelled": False})

    def test_missing_fingerprint_fails_before_generation(self):
        called = False

        def image_generator(*_args, **_kwargs):
            nonlocal called
            called = True
            raise AssertionError("generation must not start")

        handler = sidecar.DiffusersProtocolHandler(fingerprint_provider=lambda: None, image_generator=image_generator)
        with self.assertRaises(worker.WorkerError) as error:
            handler.invoke({
                "lease_id": LEASE,
                "request_id": REQUEST,
                "capability": "image.generate",
                "input": self._request(),
                "generation": None,
                "stream": False,
            })
        self.assertEqual(error.exception.code, "FINGERPRINT_REQUIRED")
        self.assertFalse(called)


if __name__ == "__main__":
    unittest.main()
