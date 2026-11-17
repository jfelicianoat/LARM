"""P061: oráculo focal del núcleo del adaptador llama.cpp.

Cubierta sintética (cabecera GGUF construida byte a byte): lector de metadata,
reglas de compatibilidad (arch desconocido, tool calling false), ModelBundle y
constructor de argumentos sin shell. Cubierta REAL (PC IA): compatibilidad con
el GGUF real de Ollama (granite-embedding 30m), ciclo de vida completo de
llama-server (arranque/health//v1/models//v1/embeddings/parada) y llama-bench
con salida cruda parseada en los dos builds reales (b79696f y d2d6269).
"""
from __future__ import annotations

import json
import socket
import struct
import sys
import unittest
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # adapters/runtime/llama-cpp

import core  # noqa: E402
from core import (  # noqa: E402
    BuildCapabilities,
    LlamaCppError,
    build_launch_args,
    check_compatibility,
    read_gguf_metadata,
    resolve_model_bundle,
)


def write_gguf(path: Path, metadata: dict[str, object]) -> None:
    """Escribe un GGUF v3 mínimo válido (cabecera + metadata, sin tensores)."""
    with path.open("wb") as handle:
        handle.write(b"GGUF")
        handle.write(struct.pack("<I", 3))
        handle.write(struct.pack("<Q", 0))  # tensor_count
        handle.write(struct.pack("<Q", len(metadata)))
        for key, value in metadata.items():
            encoded = key.encode("utf-8")
            handle.write(struct.pack("<Q", len(encoded)))
            handle.write(encoded)
            if isinstance(value, str):
                handle.write(struct.pack("<I", 8))
                raw = value.encode("utf-8")
                handle.write(struct.pack("<Q", len(raw)))
                handle.write(raw)
            elif isinstance(value, int):
                handle.write(struct.pack("<I", 4))
                handle.write(struct.pack("<I", value))
            else:
                raise TypeError(f"tipo no soportado en el fixture: {type(value)}")


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


BUILD1 = Path("C:/Desarrollo/llama.cpp/build/bin")
BUILD2 = Path("C:/Desarrollo/llama.cpp-b7020/build/bin")
OLLAMA_BLOBS = Path.home() / ".ollama" / "models" / "blobs"


class CompatibilidadSintetica(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(__file__).resolve().parent / "__p061-fixtures"
        self.tmp.mkdir(exist_ok=True)
        self.gguf = self.tmp / "fixture.gguf"

    def tearDown(self) -> None:
        self.gguf.unlink(missing_ok=True)

    def test_lee_metadata_y_version_del_gguf_sintetico(self):
        write_gguf(self.gguf, {"general.architecture": "llama", "general.file_type": 7})
        metadata = read_gguf_metadata(self.gguf)
        self.assertEqual(metadata["general.architecture"], "llama")
        self.assertEqual(metadata["general.file_type"], 7)
        self.assertEqual(metadata["gguf_version"], 3)
        self.assertEqual(metadata["tensor_count"], 0)

    def test_arch_desconocido_es_incompatible_y_tool_calling_false(self):
        write_gguf(self.gguf, {"general.architecture": "turbo-futuro"})
        metadata = read_gguf_metadata(self.gguf)
        build = BuildCapabilities(version_string="1", commit="a" * 7,
                                  server_available=True, bench_available=True)
        verdict = check_compatibility(metadata, build)
        self.assertFalse(verdict["compatible"])
        self.assertTrue(verdict["reasons"][0].startswith("arch no soportado"))
        self.assertFalse(verdict["capabilities"]["tool_calling"])
        self.assertFalse(verdict["capabilities"]["multimodal"])

    def test_llama_con_server_es_compatible(self):
        write_gguf(self.gguf, {"general.architecture": "llama", "general.file_type": 7})
        metadata = read_gguf_metadata(self.gguf)
        build = BuildCapabilities(version_string="1", commit="a" * 7,
                                  server_available=True, bench_available=True)
        verdict = check_compatibility(metadata, build)
        self.assertTrue(verdict["compatible"])
        self.assertTrue(verdict["capabilities"]["text_generation"])
        self.assertFalse(verdict["capabilities"]["tool_calling"])

    def test_magic_invalido_falla_cerrado(self):
        self.gguf.write_bytes(b"NOPE" + b"\x00" * 24)
        with self.assertRaises(LlamaCppError) as caught:
            read_gguf_metadata(self.gguf)
        self.assertEqual(caught.exception.reason, "gguf_invalid")

    def test_bundle_requiere_mmproj_para_arquitectura_multimodal(self):
        write_gguf(self.gguf, {"general.architecture": "mllama"})
        with self.assertRaises(LlamaCppError) as caught:
            resolve_model_bundle(self.gguf, None)
        self.assertEqual(caught.exception.reason, "bundle_incomplete")
        bundle = resolve_model_bundle(self.gguf, self.gguf)
        self.assertEqual(bundle["mmproj"], str(self.gguf))
        self.assertTrue(bundle["multimodal_architecture"])

    def test_argumentos_solo_flags_contractuales_y_sin_shell(self):
        exe = self.tmp / "llama-server.exe"
        exe.write_bytes(b"")
        model = self.tmp / "model.gguf"
        write_gguf(model, {"general.architecture": "llama"})
        args = build_launch_args(exe, model, {
            "port": 24891, "context_size": 2048, "threads": 2,
        })
        self.assertEqual(args[0], str(exe))
        self.assertIn("-m", args)
        self.assertEqual(args[args.index("--port") + 1], "24891")
        self.assertEqual(args[args.index("-c") + 1], "2048")
        self.assertFalse(any(token.startswith("--$") or "|" in token for token in args))

    def test_argumentos_rechaza_campos_desconocidos_y_enteros_invalidos(self):
        exe = self.tmp / "llama-server.exe"
        exe.write_bytes(b"")
        model = self.tmp / "model.gguf"
        write_gguf(model, {"general.architecture": "llama"})
        with self.assertRaises(LlamaCppError) as caught:
            build_launch_args(exe, model, {"flag_inventada": "x"})
        self.assertEqual(caught.exception.reason, "launch_profile_unknown_fields")
        with self.assertRaises(LlamaCppError) as caught:
            build_launch_args(exe, model, {"port": "no-es-numero"})
        self.assertEqual(caught.exception.reason, "launch_profile_invalid")
        with self.assertRaises(LlamaCppError) as caught:
            build_launch_args(exe, model, {"threads": 0})
        self.assertEqual(caught.exception.reason, "launch_profile_invalid")


class FormatoBench(unittest.TestCase):
    RAW = (
        "| model                          | size    | params | backend  | ngl | n_threads | test              | t_s     | t/s     |\n"
        "|--------------------------------|---------|--------|----------|-----|-----------|-------------------|---------|---------|\n"
        "| granite-embedding 30m Q8_0     | 29.5 MiB|  12.89M| CPU      |  -1 |         2 | pp2048            |  0.115  | 17845.2 |\n"
        "| granite-embedding 30m Q8_0     | 29.5 MiB|  12.89M| CPU      |  -1 |         2 | tg32              |  1.204  |    26.6 |\n"
    )

    def test_parser_del_formato_tabular_real(self):
        rows = core.parse_llama_bench(self.RAW, "build b79696f")
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0].test, "pp2048")
        self.assertEqual(rows[0].backend, "CPU")
        self.assertAlmostEqual(rows[1].tokens_per_second, 26.6)
        self.assertEqual(rows[1].model, "granite-embedding 30m Q8_0")

    def test_formato_desconocido_falla_versionadamente(self):
        with self.assertRaises(LlamaCppError) as caught:
            core.parse_llama_bench("ggml_backend_sched: unknown output", "build d2d6269")
        self.assertEqual(caught.exception.reason, "bench_unsupported_output")


class RealLlamaCppPCIA(unittest.TestCase):
    """Aceptación real sobre los dos builds compilados y el GGUF real de
    Ollama. Los procesos son efímeros, en loopback, con token cap y parada
    determinista; sin escribir fuera de los directorios de trabajo."""

    GRANITE = "granite-embedding"

    @classmethod
    def buscar_blob(cls, model_ref: str) -> Path | None:
        import json as jsonlib
        base = Path.home() / ".ollama" / "models"
        for manifest in (base / "manifests").rglob("*"):
            if not manifest.is_file():
                continue
            parts = manifest.relative_to(base / "manifests").parts
            if parts[-2] != model_ref.split(":")[0]:
                continue
            try:
                document = jsonlib.loads(manifest.read_text(encoding="utf-8"))
            except ValueError:
                continue
            for layer in document.get("layers") or []:
                if "model" in str(layer.get("mediaType", "")):
                    return base / "blobs" / str(layer["digest"]).replace(":", "-")
        return None

    def test_00_compatibilidad_con_gguf_real_de_ollama(self):
        blob = self.buscar_blob(self.GRANITE)
        self.assertIsNotNone(blob, "el GGUF real granite-embedding debe existir en Ollama")
        metadata = read_gguf_metadata(blob)
        self.assertIn("general.architecture", metadata)
        build = BuildCapabilities(
            version_string="1.0.0-dev (build 1, commit b79696f)", commit="b79696f",
            server_available=True, bench_available=True,
        )
        verdict = check_compatibility(metadata, build)
        self.assertTrue(verdict["compatible"], verdict)
        self.assertTrue(verdict["capabilities"]["embeddings"], verdict)
        print(f"REAL compat: arch={verdict['architecture']} capabilities={verdict['capabilities']}")

    def test_10_ciclo_de_vida_server_health_models_embeddings_y_parada(self):
        blob = self.buscar_blob(self.GRANITE)
        exe = BUILD1 / "llama-server.exe"
        port = free_port()
        args = [str(exe), "-m", str(blob), "--port", str(port), "--embedding",
                "-c", "512", "-t", "2"]
        handle = core.launch_server(args,
                                    work_dir=REPO / "docs/reports/P061_runs/server", port=port,
                                    expected_model=str(blob))
        try:
            health = handle.health(timeout_seconds=60)
            self.assertTrue(health["healthy"])
            listing = handle.models()
            self.assertTrue(listing.get("data"), listing)
            payload = json.dumps({"input": "prueba de embeddings"}).encode()
            request = urllib.request.Request(
                f"http://127.0.0.1:{port}/v1/embeddings", data=payload,
                headers={"Content-Type": "application/json"}, method="POST")
            with urllib.request.urlopen(request, timeout=60) as response:
                embedding = json.loads(response.read(1 << 20).decode("utf-8", "replace"))
            self.assertTrue(embedding.get("data"), embedding)
            self.assertGreater(len(embedding["data"][0]["embedding"]), 0)
            print(f"REAL server: health=200 models={len(listing['data'])} embedding_dim={len(embedding['data'][0]['embedding'])}")
        finally:
            handle.stop()
        self.assertIsNotNone(handle.process.returncode)

    def test_20_llama_bench_real_en_los_dos_builds(self):
        blob = self.buscar_blob(self.GRANITE)
        results = {}
        for label, exe in (("b79696f", BUILD1 / "llama-bench.exe"),
                           ("d2d6269", BUILD2 / "llama-bench.exe")):
            result = core.run_llama_bench(
                exe, blob, work_dir=REPO / "docs/reports/P061_runs/bench",
                repetitions=5, timeout_seconds=900,
            )
            results[label] = result
            self.assertTrue(result["rows"], result)
            self.assertEqual(result["repetitions"], 5)
            self.assertTrue(Path(result["raw_output_path"]).is_file())
        self.assertNotEqual(results["b79696f"]["source_version"],
                            results["d2d6269"]["source_version"],
                            "los dos builds deben reportar versiones distintas")
        print(f"REAL bench: versiones={results['b79696f']['source_version']!r} vs {results['d2d6269']['source_version']!r}; "
              f"filas={len(results['b79696f']['rows'])}/{len(results['d2d6269']['rows'])}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
