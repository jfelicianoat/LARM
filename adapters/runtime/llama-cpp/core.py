"""P061: núcleo del adaptador llama.cpp — compatibilidad, lanzamiento, serving
y benchmark técnico. Todo read-only respecto al sistema anfitrión: los procesos
se lanzan con argumentos explícitos desde el LaunchProfile (sin shell), con
timeout, kill al vencer, puerto asignado por el operador y salidas acotadas.
La compatibilidad se determina por metadata GGUF + capacidades del build, no
por la extensión `.gguf`; tool calling/multimodal se declara false salvo
evidencia de la versión/modelo."""
from __future__ import annotations

import hashlib
import json
import re
import subprocess
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path


class LlamaCppError(Exception):
    """Error controlado del adaptador con código de motivo estable."""

    def __init__(self, reason: str, detail: str = ""):
        super().__init__(detail or reason)
        self.reason = reason
        self.detail = detail


# ---------------------------------------------------------------------------
# 1) Compatibilidad por metadata GGUF + capacidades del build
# ---------------------------------------------------------------------------

GGUF_MAGIC = b"GGUF"

# general.architecture admite estos valores; cualquier otro arch queda
# incompatibile (unsupported_arch) hasta evidencia del build.
KNOWN_ARCHITECTURES = {"llama", "gemma", "gemma2", "gemma3", "qwen2", "qwen3",
                       "phi3", "phi2", "starcoder2", "granite", "bert", "nomic-bert",
                       "mllama", "minicpm", "smoldlm", "deci"}

# Claves de metadata GGUF que el lector extrae (uint64_le + valor).
_META_STRING_PREFIX = "general."


def read_gguf_metadata(path: Path, max_keys: int = 512) -> dict[str, object]:
    """Lee sólo la cabecera de metadata del GGUF (sin tensores): determinista,
    con límites acotados y sin dependencias externas. Lanza LlamaCppError con
    `gguf_invalid` ante cabecera corrupta y `gguf_too_large` si excede los
    límites de lectura."""
    size = path.stat().st_size
    with path.open("rb") as handle:
        magic = handle.read(4)
        if magic != GGUF_MAGIC:
            raise LlamaCppError("gguf_invalid", f"magic={magic!r}")
        version_bytes = handle.read(4)
        version = int.from_bytes(version_bytes, "little")
        if version not in (1, 2, 3):
            raise LlamaCppError("gguf_invalid", f"version={version}")
        tensor_count = int.from_bytes(handle.read(8), "little")
        key_value_count = int.from_bytes(handle.read(8), "little")
        if key_value_count > max_keys:
            raise LlamaCppError("gguf_too_large", f"kv={key_value_count}")

        sizes = {0: 1, 1: 1, 2: 2, 3: 2, 4: 4, 5: 4, 6: 4, 7: 1, 10: 8, 11: 8, 12: 8}

        def read_string(skip: bool = False) -> str:
            length = int.from_bytes(handle.read(8), "little")
            if skip:
                # Los vocabularios reales miden megas: se saltan sin decodificar.
                handle.seek(length, 1)
                return ""
            if length > 1 << 20:
                raise LlamaCppError("gguf_too_large", f"string={length}")
            return handle.read(length).decode("utf-8", "replace")

        def skip_value(value_type: int) -> None:
            widths = {0: 1, 1: 1, 2: 2, 3: 2, 4: 4, 5: 4, 6: 4, 7: 1, 10: 8, 11: 8, 12: 8}
            if value_type == 8:
                read_string(skip=True)
            elif value_type == 9:
                inner_type = int.from_bytes(handle.read(4), "little")
                count = int.from_bytes(handle.read(8), "little")
                skip_array(inner_type, count)
            else:
                width = widths.get(value_type)
                if width is None:
                    raise LlamaCppError("gguf_invalid", f"value_type={value_type}")
                handle.seek(width, 1)

        def read_value(value_type: int, skip: bool = False) -> object:
            if value_type == 8:  # string
                return read_string(skip=skip)
            if value_type == 9:  # array (la especificación GGUF: 9=array)
                inner_type = int.from_bytes(handle.read(4), "little")
                count = int.from_bytes(handle.read(8), "little")
                if skip or count > 4096:
                    skip_array(inner_type, count)
                    return []
                return [read_value(inner_type) for _ in range(count)]
            width = sizes.get(value_type)
            if width is None:
                raise LlamaCppError("gguf_invalid", f"value_type={value_type}")
            raw = handle.read(width)
            if value_type in (0, 1, 2, 3, 4, 5, 10, 11):
                return int.from_bytes(raw, "little")
            if value_type == 7:
                return bool(raw[0])
            if value_type == 6:  # float32
                import struct
                return struct.unpack("<f", raw)[0]
            if value_type == 12:  # float64
                import struct
                return struct.unpack("<d", raw)[0]
            raise LlamaCppError("gguf_invalid", f"value_type={value_type}")

        def skip_array(inner_type: int, count: int) -> None:
            if inner_type == 8:
                for _ in range(count):
                    read_string(skip=True)
            elif inner_type == 9:
                for _ in range(count):
                    nested = int.from_bytes(handle.read(4), "little")
                    nested_count = int.from_bytes(handle.read(8), "little")
                    skip_array(nested, nested_count)
            else:
                width = sizes.get(inner_type)
                if width is None:
                    raise LlamaCppError("gguf_invalid", f"array_type={inner_type}")
                handle.seek(width * count, 1)

        metadata: dict[str, object] = {}
        for _ in range(key_value_count):
            key = read_string()
            value_type = int.from_bytes(handle.read(4), "little")
            # Los vocabularios reales (arrays de cientos de miles de strings)
            # se saltan sin decodificar: el lector sólo necesita los metadatos
            # generales para la compatibilidad.
            metadata[key] = read_value(value_type, skip=key.startswith("tokenizer."))
    return {
        "gguf_version": version,
        "tensor_count": tensor_count,
        "file_size_bytes": size,
        **metadata,
    }


@dataclass(frozen=True)
class BuildCapabilities:
    """Capacidades observadas de un build concreto (del probe P060)."""

    version_string: str
    commit: str | None
    server_available: bool
    bench_available: bool
    devices: tuple[str, ...] = ()
    multimodal_mmproj: bool = False

    @property
    def text_generation(self) -> bool:
        return self.server_available


def check_compatibility(metadata: dict[str, object], build: BuildCapabilities) -> dict[str, object]:
    """Decide compatibilidad por metadata GGUF + capacidades del build.

    Reglas del checklist: no basta la extensión `.gguf`; arch desconocido es
    `unsupported_arch`; tool calling y multimodal son false salvo evidencia
    (mmproj en el bundle y build multimodal)."""
    architecture = str(metadata.get("general.architecture", "")).strip()
    file_type = metadata.get("general.file_type")
    reasons: list[str] = []
    if not architecture:
        reasons.append("general.architecture ausente")
    elif architecture.lower() not in KNOWN_ARCHITECTURES:
        reasons.append(f"arch no soportado por este build: {architecture}")
    if not build.server_available:
        reasons.append("el build no tiene llama-server")
    compatible = not reasons
    multimodal = bool(
        build.multimodal_mmproj
        and architecture.lower() in KNOWN_ARCHITECTURES
        and metadata.get("general.name") is not None
        and any(key.startswith("clip.") or key.startswith("mmproj") for key in metadata)
    )
    return {
        "compatible": compatible,
        "architecture": architecture or None,
        "file_type": file_type,
        "reasons": reasons,
        "capabilities": {
            "text_generation": compatible and build.text_generation,
            "embeddings": compatible and architecture.lower() in {"bert", "nomic-bert", "granite"},
            "tool_calling": False,
            "multimodal": multimodal,
        },
    }


def resolve_model_bundle(model_path: Path, mmproj_path: Path | None) -> dict[str, object]:
    """ModelBundle: modelo principal + mmproj si el bundle lo requiere. El
    mmproj forma parte del bundle cuando existe junto al modelo; falta sólo
    constituye error si la metadata del modelo declara arquitectura multimodal."""
    if not model_path.is_file():
        raise LlamaCppError("model_missing", str(model_path))
    metadata = read_gguf_metadata(model_path)
    architecture = str(metadata.get("general.architecture", "")).lower()
    multimodal_arch = architecture in {"mllama", "minicpm"}
    bundle: dict[str, object] = {
        "model": str(model_path),
        "model_sha256": hashlib.sha256(model_path.read_bytes()).hexdigest() if model_path.stat().st_size <= (1 << 32) else None,
        "mmproj": None,
        "multimodal_architecture": multimodal_arch,
    }
    if mmproj_path is not None:
        if not mmproj_path.is_file():
            raise LlamaCppError("mmproj_missing", str(mmproj_path))
        bundle["mmproj"] = str(mmproj_path)
    elif multimodal_arch:
        raise LlamaCppError("bundle_incomplete", f"arquitectura {architecture} requiere mmproj")
    return bundle


# ---------------------------------------------------------------------------
# 2) Lanzamiento: argumentos desde LaunchProfile, sin shell
# ---------------------------------------------------------------------------

_LAUNCH_FLAG_MAP = {
    "port": "--port",
    "host": "--host",
    "context_size": "-c",
    "threads": "-t",
    "gpu_layers": "-ngl",
    "parallel_slots": "-np",
    "timeout_idle_seconds": "--timeout",
}

_LAUNCH_INT_FIELDS = {"port", "context_size", "threads", "gpu_layers", "parallel_slots", "timeout_idle_seconds"}


def build_launch_args(executable: Path, model_path: Path, launch_profile: dict[str, object],
                      mmproj_path: Path | None = None) -> list[str]:
    """Construye los argumentos de llama-server desde el LaunchProfile: sólo
    pares flag/valor del mapa contractual, enteros validados, sin shell y sin
    interpolación de cadenas del operador en flags desconocidas."""
    if not executable.is_file():
        raise LlamaCppError("executable_missing", str(executable))
    if not model_path.is_file():
        raise LlamaCppError("model_missing", str(model_path))
    args = [str(executable), "-m", str(model_path)]
    if mmproj_path is not None:
        args.extend(["--mmproj", str(mmproj_path)])
    for field, flag in sorted(_LAUNCH_FLAG_MAP.items()):
        if field not in launch_profile:
            continue
        value = launch_profile[field]
        if field in _LAUNCH_INT_FIELDS:
            try:
                number = int(value)  # type: ignore[arg-type]
            except (TypeError, ValueError):
                raise LlamaCppError("launch_profile_invalid", f"{field}={value!r}")
            if number <= 0:
                raise LlamaCppError("launch_profile_invalid", f"{field}={number}")
            args.extend([flag, str(number)])
        else:
            text = str(value).strip()
            if not text:
                raise LlamaCppError("launch_profile_invalid", f"{field} vacío")
            args.extend([flag, text])
    unknown = sorted(set(launch_profile) - set(_LAUNCH_FLAG_MAP))
    if unknown:
        raise LlamaCppError("launch_profile_unknown_fields", ",".join(unknown))
    return args


# ---------------------------------------------------------------------------
# 3) Ciclo de vida del servidor: arranque, health, parada
# ---------------------------------------------------------------------------

@dataclass
class ServerHandle:
    """Servidor vivo: proceso, puerto y capacidad de parada determinista."""

    process: subprocess.Popen
    port: int
    log_path: Path
    model_path: str

    def health(self, timeout_seconds: float = 30.0, interval: float = 0.25) -> dict[str, object]:
        """Espera /health hasta 200 OK o agota el plazo (timeout/cancel)."""
        deadline = time.monotonic() + timeout_seconds
        url = f"http://127.0.0.1:{self.port}/health"
        last_error = ""
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(url, timeout=2.0) as response:
                    body = response.read(4096).decode("utf-8", "replace")
                    if response.status == 200:
                        return {"healthy": True, "status_code": 200, "body": body[:512]}
                    last_error = f"status={response.status}"
            except urllib.error.HTTPError as error:
                last_error = f"status={error.code}"
            except (urllib.error.URLError, OSError) as error:
                last_error = str(error)[:200]
            if self.process.poll() is not None:
                raise LlamaCppError("server_exited", f"rc={self.process.returncode}")
            time.sleep(interval)
        raise LlamaCppError("health_timeout", last_error)

    def models(self) -> dict[str, object]:
        """GET /v1/models: model listing/protocol tras health."""
        url = f"http://127.0.0.1:{self.port}/v1/models"
        with urllib.request.urlopen(url, timeout=10.0) as response:
            return json.loads(response.read(1 << 20).decode("utf-8", "replace"))

    def chat(self, prompt: str, max_tokens: int = 16, timeout_seconds: float = 120.0) -> dict[str, object]:
        """Smoke de inferencia: POST /v1/chat/completions con token cap."""
        payload = json.dumps({
            "model": "default",
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": max_tokens,
            "temperature": 0.0,
            "stream": False,
        }).encode("utf-8")
        request = urllib.request.Request(
            f"http://127.0.0.1:{self.port}/v1/chat/completions",
            data=payload, headers={"Content-Type": "application/json"}, method="POST",
        )
        started = time.monotonic()
        with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
            body = json.loads(response.read(4 << 20).decode("utf-8", "replace"))
        content = (body.get("choices") or [{}])[0].get("message", {}).get("content")
        if not isinstance(content, str) or not content:
            raise LlamaCppError("smoke_empty", "respuesta sin contenido")
        return {"content": content[:512], "elapsed_seconds": round(time.monotonic() - started, 3)}

    def stop(self) -> None:
        """Parada determinista: terminate + wait con margen, luego kill."""
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=10)


def launch_server(command: list[str], work_dir: Path, port: int,
                  expected_model: str) -> ServerHandle:
    """Arranca llama-server con el argv completo (sin shell), entorno mínimo,
    log a fichero y kill-on-exit."""
    if not command:
        raise LlamaCppError("launch_command_empty", "argv vacío")
    executable = Path(command[0])
    if not executable.is_file():
        raise LlamaCppError("executable_missing", str(executable))
    work_dir.mkdir(parents=True, exist_ok=True)
    log_path = work_dir / f"llama-server-{port}.log"
    log = log_path.open("ab")
    minimal_env = {
        key: value for key, value in {
            "SystemRoot": __import__("os").environ.get("SystemRoot", ""),
            "SystemDrive": __import__("os").environ.get("SystemDrive", ""),
            "TEMP": __import__("os").environ.get("TEMP", ""),
            "TMP": __import__("os").environ.get("TMP", ""),
        }.items() if value
    }
    process = subprocess.Popen(
        [str(arg) for arg in command],
        cwd=str(work_dir), env=minimal_env, stdin=subprocess.DEVNULL,
        stdout=log, stderr=subprocess.STDOUT,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )
    log.close()
    handle = ServerHandle(process=process, port=port, log_path=log_path, model_path=expected_model)
    try:
        handle.health(timeout_seconds=120.0)
    except LlamaCppError:
        handle.stop()
        raise
    return handle


# ---------------------------------------------------------------------------
# 4) llama-bench: salida cruda + parser versionado + cinco repeticiones
# ---------------------------------------------------------------------------

BENCH_REPETITIONS = 5


@dataclass
class BenchRow:
    model: str
    size: str
    params: str
    backend: str
    n_gpu_layers: str
    n_threads: str
    test: str
    t_s: float | None
    tokens_per_second: float | None
    repetitions: list[float] = field(default_factory=list)


BENCH_HEADER = re.compile(r"^\s*\|\s*(model\s*\|.*len\s*\|.*t/s\s*)\|\s*$", re.IGNORECASE)


def parse_llama_bench(raw: str, source_version: str) -> list[BenchRow]:
    """Parser versionado del resultado tabular de llama-bench. Formato
    soportado: b<NNNN>+ (cabecera `| model | … | t/s |` y filas de datos con
    celdas separadas por `|`). LlamaCppError `bench_unsupported_output` ante
    cualquier otro formato; las filas conservan la salida cruda ajenas."""
    lines = raw.splitlines()
    header_index = None
    for index, line in enumerate(lines):
        if line.strip().startswith("|") and "t/s" in line and "model" in line:
            header_index = index
            break
    if header_index is None:
        raise LlamaCppError("bench_unsupported_output", f"build {source_version}")
    columns = [cell.strip().lower() for cell in lines[header_index].strip().strip("|").split("|")]
    rows: list[BenchRow] = []
    for line in lines[header_index + 1:]:
        if not line.strip().startswith("|"):
            continue
        if set(line.strip()) <= {"|", "-", " ", ":"}:
            continue  # línea separadora de cabecera md
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if len(cells) != len(columns):
            continue
        record = dict(zip(columns, cells))
        def _float(value: str) -> float | None:
            try:
                return float(value)
            except ValueError:
                return None
        rows.append(BenchRow(
            model=record.get("model", ""),
            size=record.get("size", ""),
            params=record.get("params", ""),
            backend=record.get("backend", ""),
            n_gpu_layers=record.get("n_gpu_layers", record.get("ngl", "")),
            n_threads=record.get("n_threads", record.get("nthread", record.get("t_th", ""))),
            test=record.get("test", ""),
            t_s=_float(record.get("t_s", "")),
            tokens_per_second=_float(record.get("t/s", record.get("ts", ""))),
        ))
    if not rows:
        raise LlamaCppError("bench_unsupported_output", f"sin filas de datos (build {source_version})")
    return rows


def run_llama_bench(executable: Path, model_path: Path, work_dir: Path,
                    repetitions: int = BENCH_REPETITIONS, timeout_seconds: float = 600.0,
                    source_version: str | None = None) -> dict[str, object]:
    """Ejecuta llama-bench con cinco repeticiones (-r), guarda el raw output
    con su sha256 y devuelve métricas parseadas versionadamente. llama-bench
    no tiene --version: la versión del build se pasa explícita o se sonsajea
    del llama-cli adyacente (mismo bin dir)."""
    if repetitions <= 0:
        raise LlamaCppError("bench_invalid_repetitions", str(repetitions))
    if source_version is None:
        sibling = executable.parent / "llama-cli.exe"
        version_probe = subprocess.run(
            [str(sibling), "--version"], capture_output=True, timeout=30,
            stdin=subprocess.DEVNULL,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        # La salida de versión de llama.cpp va a stderr en los builds MSVC:
        # combinar ambas tuberías antes de decidir "unknown".
        stdout_text = version_probe.stdout.decode("utf-8", "replace").strip()
        stderr_lines = version_probe.stderr.decode("utf-8", "replace").strip().splitlines()
        source_version = stdout_text or (stderr_lines[0].strip() if stderr_lines else "unknown")
    work_dir.mkdir(parents=True, exist_ok=True)
    raw_path = work_dir / f"llama-bench-{int(time.time())}.txt"
    process = subprocess.run(
        [str(executable), "-m", str(model_path), "-r", str(repetitions),
         "-p", "64", "-n", "32", "-t", "2"],
        capture_output=True, timeout=timeout_seconds, stdin=subprocess.DEVNULL,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )
    raw_output = process.stdout.decode("utf-8", "replace")
    raw_path.write_text(raw_output, encoding="utf-8", newline="\n")
    if process.returncode != 0:
        raise LlamaCppError("bench_failed",
                            f"rc={process.returncode}: {process.stderr.decode('utf-8', 'replace')[-300:]}")
    rows = parse_llama_bench(raw_output, source_version)
    return {
        "source_version": source_version,
        "repetitions": repetitions,
        "raw_output_path": str(raw_path),
        "raw_output_sha256": hashlib.sha256(raw_path.read_bytes()).hexdigest(),
        "rows": [
            {
                "model": row.model, "size": row.size, "params": row.params,
                "backend": row.backend, "n_gpu_layers": row.n_gpu_layers,
                "n_threads": row.n_threads, "test": row.test,
                "t_s": row.t_s, "tokens_per_second": row.tokens_per_second,
            }
            for row in rows
        ],
    }
