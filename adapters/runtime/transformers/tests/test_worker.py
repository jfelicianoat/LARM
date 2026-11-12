from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from contextlib import nullcontext
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
import worker  # noqa: E402
import sidecar  # noqa: E402


RUNTIME = "0192aaaa-0000-7000-8000-000000000001"
VERSION = "0192aaaa-0000-7000-8000-000000000002"
BACKEND = "0192aaaa-0000-7000-8000-000000000003"


class FakeRow:
    shape = (3,)

    def __getitem__(self, value):
        if isinstance(value, slice):
            return [101]
        raise IndexError(value)


class FakeBatch:
    shape = (1, 2)

    def to(self, _device):
        return self


class FakeOutput:
    def __getitem__(self, index):
        if index != 0:
            raise IndexError(index)
        return FakeRow()


class FakeTokenizer:
    calls = []

    @classmethod
    def from_pretrained(cls, path, **kwargs):
        cls.calls.append((path, kwargs))
        return cls()

    def __call__(self, _prompt, return_tensors):
        assert return_tensors == "pt"
        return {"input_ids": FakeBatch(), "attention_mask": FakeBatch()}

    def decode(self, _tokens, skip_special_tokens):
        assert skip_special_tokens is True
        return "respuesta local"


class FakeModel:
    calls = []

    @classmethod
    def from_pretrained(cls, path, **kwargs):
        cls.calls.append((path, kwargs))
        return cls()

    def to(self, _device):
        return self

    def eval(self):
        return None

    def generate(self, **kwargs):
        self.last_kwargs = kwargs
        return FakeOutput()


def fake_importer(name):
    if name == "torch":
        return SimpleNamespace(
            cuda=SimpleNamespace(is_available=lambda: False),
            xpu=SimpleNamespace(is_available=lambda: False),
            inference_mode=nullcontext,
        )
    if name == "transformers":
        return SimpleNamespace(AutoTokenizer=FakeTokenizer, AutoModelForCausalLM=FakeModel)
    raise ImportError(name)


class TransformersWorkerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=ROOT / "tests")
        self.model_root = Path(self.temp.name)
        self.model = self.model_root / "tiny"
        self.model.mkdir()
        self.config = {"architectures": ["TinyForCausalLM"]}
        (self.model / "config.json").write_text(json.dumps(self.config), encoding="utf-8")
        (self.model / "model.safetensors").write_bytes(b"fixture-header-only")
        FakeTokenizer.calls = []
        FakeModel.calls = []

    def tearDown(self):
        self.temp.cleanup()

    def env(self):
        return {"LARM_TRANSFORMERS_MODEL_ROOT": str(self.model_root)}

    def plan(self, **changes):
        payload = {
            "runtime_id": RUNTIME,
            "runtime_version_id": VERSION,
            "backend_id": BACKEND,
            "python_version": "3.12.4",
            "dependencies_lock_sha256": "a" * 64,
            "strategy": "UV",
            "announced_strategies": ["UV", "CONDA"],
        }
        payload.update(changes)
        return payload

    def test_probe_never_loads_packages_outside_managed_python(self):
        loaded = []
        result = worker.handle({"protocol_version": "1", "operation": "probe"}, managed=False,
                               importer=lambda name: loaded.append(name))
        self.assertFalse(result["result"]["managed_python"])
        self.assertIsNone(result["result"]["torch_version"])
        self.assertEqual(loaded, [])

    def test_managed_python_requires_interpreter_inside_the_exact_prefix(self):
        prefix = self.model_root / "managed-python"
        scripts = prefix / "Scripts"
        scripts.mkdir(parents=True)
        interpreter = scripts / "python.exe"
        interpreter.write_bytes(b"fixture")
        self.assertTrue(worker._managed_interpreter(
            runtime_root=str(prefix), prefix=str(prefix), base_prefix=sys.base_prefix, executable=str(interpreter)
        ))
        self.assertFalse(worker._managed_interpreter(
            runtime_root=str(prefix), prefix=str(prefix), base_prefix=sys.base_prefix, executable=sys.executable
        ))

    def test_environment_identity_is_stable_and_uses_declared_strategy(self):
        first = worker.handle({"protocol_version": "1", "operation": "environment_plan", "payload": self.plan()})
        second = worker.handle({"protocol_version": "1", "operation": "environment_plan", "payload": self.plan()})
        self.assertEqual(first["result"], second["result"])
        self.assertTrue(first["result"]["isolated"])
        self.assertFalse(first["result"]["created"])
        self.assertEqual(first["result"]["environment_id"], "8b80d5d12e3940ef2cd7da3d59740e9e6dfe2e451e9f1977ec0ec4f47a64eb4d")
        self.assertIn("python-envs/" + RUNTIME + "/" + VERSION, first["result"]["relative_path"])

    def test_unannounced_installation_strategy_fails_before_side_effect(self):
        with self.assertRaises(worker.WorkerError) as caught:
            worker.handle({"protocol_version": "1", "operation": "environment_plan",
                           "payload": self.plan(strategy="MAMBA")})
        self.assertEqual(caught.exception.code, "UNSUPPORTED_CAPABILITY")

    def test_closed_protocol_rejects_unknown_fields_and_duplicate_json_keys(self):
        with self.assertRaises(worker.WorkerError):
            worker.handle({"protocol_version": "1", "operation": "probe", "extra": True})
        with self.assertRaises(worker.WorkerError):
            worker.handle({"protocol_version": "1", "operation": []})
        with self.assertRaises(ValueError):
            json.loads('{"operation":"probe","operation":"version"}', object_pairs_hook=worker._duplicate_rejecting_object)

    def test_model_path_traversal_and_missing_configuration_are_rejected(self):
        with self.assertRaises(worker.WorkerError):
            worker._inspect_model(str(self.model_root), "../tiny")
        (self.model / "config.json").unlink()
        with self.assertRaises(worker.WorkerError) as caught:
            worker._inspect_model(str(self.model_root), "tiny")
        self.assertEqual(caught.exception.code, "MODEL_CONFIG_MISSING")

    def test_remote_code_requirement_blocks_before_transformers_import(self):
        (self.model / "config.json").write_text(
            json.dumps({"architectures": ["CustomModel"], "auto_map": {"AutoModel": "remote:Model"}}), encoding="utf-8"
        )
        with patch.dict("os.environ", self.env(), clear=False):
            with self.assertRaises(worker.WorkerError) as caught:
                worker._generate_text(str(self.model_root), {
                    "model_id": "tiny", "backend": "CPU", "prompt": "hola", "max_new_tokens": 4
                }, managed=True, importer=lambda _name: self.fail("no debe importar código de modelo"))
        self.assertEqual(caught.exception.code, "REMOTE_CODE_REQUIRED")

    def test_generation_uses_local_safetensors_and_disables_remote_code(self):
        with patch.dict("os.environ", self.env(), clear=False):
            result = worker._generate_text(str(self.model_root), {
                "model_id": "tiny", "backend": "CPU", "prompt": "hola", "max_new_tokens": 8
            }, managed=True, importer=fake_importer)
        self.assertEqual(result["text"], "respuesta local")
        self.assertEqual(result["backend"], "CPU")
        self.assertFalse(result["remote_code_used"])
        self.assertEqual(FakeTokenizer.calls[0][1], {"local_files_only": True, "trust_remote_code": False})
        self.assertEqual(FakeModel.calls[0][1], {
            "local_files_only": True, "trust_remote_code": False, "use_safetensors": True
        })

    def test_cuda_is_explicit_and_unavailable_cuda_is_rejected(self):
        with patch.dict("os.environ", self.env(), clear=False):
            with self.assertRaises(worker.WorkerError) as caught:
                worker._generate_text(str(self.model_root), {
                    "model_id": "tiny", "backend": "CUDA", "prompt": "hola", "max_new_tokens": 8
                }, managed=True, importer=fake_importer)
        self.assertEqual(caught.exception.code, "BACKEND_UNAVAILABLE")
        self.assertEqual(FakeModel.calls, [])


class OpenAISidecarTests(unittest.TestCase):
    def test_completion_and_chat_subsets_normalize_only_supported_fields(self):
        self.assertEqual(sidecar.completion_request({"model": "tiny", "prompt": "hola"}, "tiny"), ("hola", 64, False))
        prompt, limit, stream = sidecar.chat_request({
            "model": "tiny", "messages": [{"role": "user", "content": "hola"}], "max_tokens": 12
        }, "tiny")
        self.assertIn("user: hola", prompt)
        self.assertEqual((limit, stream), (12, False))

    def test_sidecar_rejects_tools_streaming_extra_fields_and_other_models(self):
        for body in (
            {"model": "tiny", "prompt": "hola", "temperature": 0},
            {"model": "tiny", "prompt": "hola", "stream": True},
            {"model": "other", "prompt": "hola"},
        ):
            with self.assertRaises(sidecar.SidecarError):
                sidecar.completion_request(body, "tiny")
        with self.assertRaises(sidecar.SidecarError):
            sidecar.chat_request({"model": "tiny", "messages": [{"role": "tool", "content": "x"}]}, "tiny")
        with self.assertRaises(sidecar.SidecarError):
            sidecar.chat_request({"model": "tiny", "messages": [{"role": [], "content": "x"}]}, "tiny")

    def test_sidecar_response_uses_actual_token_counts_and_openai_shape(self):
        response = sidecar.completion_response(
            text="hola", model="tiny", prompt_tokens=2, completion_tokens=1, finish_reason="stop", chat=True
        )
        self.assertEqual(response["object"], "chat.completion")
        self.assertEqual(response["usage"]["total_tokens"], 3)
        self.assertEqual(response["choices"][0]["message"]["content"], "hola")

    def test_sidecar_refuses_non_loopback_or_missing_host_token(self):
        for kwargs in (
            {"host": "0.0.0.0", "port": 0, "model": "tiny", "bearer_token": "x" * 32, "generate": lambda *_: {}},
            {"host": "127.0.0.1", "port": 0, "model": "tiny", "bearer_token": "short", "generate": lambda *_: {}},
            {"host": "127.0.0.1", "port": 0, "model": "tiny", "bearer_token": "x" * 31 + "\n", "generate": lambda *_: {}},
        ):
            with self.assertRaises(ValueError):
                sidecar.create_server(**kwargs)


if __name__ == "__main__":
    unittest.main()
