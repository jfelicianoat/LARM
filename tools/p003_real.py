#!/usr/bin/env python3
"""P003 - Pruebas reales de configuración, errores y logging sobre el daemon en Windows.

Cada subcomando es una prueba (exit 0 = PASS); trabajan en docs/reports/P003_runs/<caso>.

  precedence  defaults < --config < LARM_* < CLI con `larm-daemon print-settings`; la salida
              valida contra larm.schema.json#/$defs/Settings
  rejects     el daemon real rechaza con exit 2 y VALIDATION_FAILED: límite de velocidad 0,
              bytes como number, log_level dentro del fichero, LARM_LOG_LEVEL inválido, typo en
              una sección; y una LARM_* ajena (LARM_DESKTOP_SMOKE) no impide arrancar
  logging     LARM_LOG_FORMAT=json: fichero diario larm.AAAA-MM-DD.log, cada línea JSON con
              timestamp RFC3339 UTC con milisegundos y Z, correlation_id UUIDv7, telemetría off y
              un secreto inyectado por --bind redactado en el fichero; CTRL_BREAK -> exit 0
  retention   con 20 logs diarios antiguos en la carpeta, el daemon deja como mucho
              diagnostics.log_retention_days (14) ficheros larm.*.log
  stdout-bloqueado
              stdout del daemon en un pipe que nadie lee y log en debug: 500 peticiones
              responden igual (stdout no bloquea al daemon), el fichero las registra todas
              y CTRL_BREAK -> exit 0
"""
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

from jsonschema import Draft202012Validator
from referencing import Registry, Resource

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
RUNS = REPO / "docs/reports/P003_runs"
UUID7 = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
TS = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$")
SECRET = "supersecreto123456"


def case_dir(name: str) -> Path:
    d = RUNS / name
    shutil.rmtree(d, ignore_errors=True)
    d.mkdir(parents=True)
    return d


def clean_env() -> dict:
    env = {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}
    env["PYTHONIOENCODING"] = "utf-8"
    return env


def run(args: list[str], env: dict, timeout: int = 30) -> subprocess.CompletedProcess:
    return subprocess.run([str(EXE), *args], env=env, capture_output=True, timeout=timeout,
                          text=True, encoding="utf-8", errors="replace", check=False)


def settings_validator() -> Draft202012Validator:
    registry = Registry()
    for name in ("larm.schema.json", "domain.schema.json"):
        doc = json.loads((REPO / "docs/contracts/schemas" / name).read_text(encoding="utf-8"))
        registry = registry.with_resource(name, Resource.from_contents(doc))
    return Draft202012Validator({"$ref": "larm.schema.json#/$defs/Settings"}, registry=registry)


def precedence() -> str:
    d = case_dir("precedence")
    cfg = d / "config.json"
    cfg.write_text(json.dumps({"api": {"port": 9000},
                               "storage": {"download_bandwidth_limit_bytes_per_second": "1048576",
                                           "low_disk_thresholds": {"mode": "BYTES", "warning_bytes": "10737418240",
                                                                   "critical_bytes": "5368709120"}}}), encoding="utf-8")
    env = clean_env() | {"LARM_API__PORT": "9211", "LARM_STORAGE__MINIMUM_FREE_BYTES": "2147483648",
                         "LARM_DESKTOP_SMOKE": "1"}
    p = run(["--config", str(cfg), "--port", "9333", "--data-root", str(d), "print-settings"], env)
    (d / "print-settings.json").write_text(p.stdout, encoding="utf-8")
    if p.returncode != 0:
        raise AssertionError(f"exit {p.returncode}: {p.stderr[-400:]}")
    s = json.loads(p.stdout)
    errors = list(settings_validator().iter_errors(s))
    if errors:
        raise AssertionError(f"la salida no valida contra Settings: {errors[0].message}")
    got = (s["api"]["port"], s["storage"]["download_bandwidth_limit_bytes_per_second"],
           s["storage"]["minimum_free_bytes"], s["storage"]["low_disk_thresholds"]["mode"], s["api"]["bind"])
    want = (9333, "1048576", "2147483648", "BYTES", "127.0.0.1")
    if got != want:
        raise AssertionError(f"precedencia: esperado {want}, observado {got}")
    # sin capas: defaults.json del contrato; --data-root es la única capa CLI y cambia solo paths.data_root
    p0 = run(["--data-root", str(d), "print-settings"], clean_env())
    contract = json.loads((REPO / "docs/contracts/defaults.json").read_text(encoding="utf-8"))
    contract["paths"]["data_root"] = str(d)
    effective = json.loads(p0.stdout)
    if effective != contract:
        diff = [k for k in contract if contract[k] != effective.get(k)]
        raise AssertionError(f"sin capas, la configuración efectiva difiere de defaults.json en {diff}")
    return (f"cli 9333 > env 9211 > fichero 9000; bandwidth '1048576' (fichero), minimum_free_bytes '2147483648' "
            f"(env), thresholds BYTES; salida válida contra Settings; sin más capas == defaults.json")


def rejects() -> str:
    d = case_dir("rejects")
    cases = {
        "bandwidth_cero": ({"storage": {"download_bandwidth_limit_bytes_per_second": "0"}}, {}),
        "bytes_como_number": ({"storage": {"minimum_free_bytes": 1024}}, {}),
        "log_level_en_fichero": ({"log_level": "debug"}, {}),
        "log_level_env_invalido": (None, {"LARM_LOG_LEVEL": "verbose"}),
        "typo_en_seccion": (None, {"LARM_API__PROT": "9000"}),
        "umbral_cruzado": ({"storage": {"low_disk_thresholds": {"mode": "PERCENT", "warning_basis_points": 500,
                                                                "critical_basis_points": 900}}}, {}),
    }
    observed = []
    for name, (file_cfg, extra_env) in cases.items():
        args = ["--data-root", str(d / name)]
        if file_cfg is not None:
            cfg = d / f"{name}.json"
            cfg.write_text(json.dumps(file_cfg), encoding="utf-8")
            args = ["--config", str(cfg), *args]
        p = run(args, clean_env() | extra_env, timeout=20)
        if p.returncode != 2 or "Configuración inválida" not in p.stderr:
            raise AssertionError(f"{name}: exit {p.returncode}, stderr {p.stderr[-300:]!r}")
        if (d / name / "logs").exists():
            raise AssertionError(f"{name}: inicializó logs pese a la config inválida")
        observed.append(name)
    return f"{len(observed)} configuraciones inválidas rechazadas con exit 2 antes de escribir logs: {observed}"


def start_and_stop(env: dict, args: list[str], data: Path) -> tuple[int, str]:
    proc = subprocess.Popen([str(EXE), "--data-root", str(data), *args], env=env,
                            creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    try:
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline and proc.poll() is None:
            if list((data / "logs").glob("larm.*.log")):
                break
            time.sleep(0.3)
        time.sleep(1.0)
        if proc.poll() is not None:
            out = proc.stdout.read().decode("utf-8", "replace") if proc.stdout else ""
            raise AssertionError(f"el daemon terminó antes de la señal (rc={proc.returncode}): {out[-400:]}")
        os.kill(proc.pid, signal.CTRL_BREAK_EVENT)
        rc = proc.wait(timeout=20)
        out = proc.stdout.read().decode("utf-8", "replace") if proc.stdout else ""
        return rc, out
    finally:
        if proc.poll() is None:
            proc.kill()


def logging_case() -> str:
    d = case_dir("logging")
    env = clean_env() | {"LARM_LOG_FORMAT": "json"}
    rc, stdout = start_and_stop(env, ["--bind", f"token={SECRET}"], d)
    if rc != 0:
        raise AssertionError(f"exit {rc} tras CTRL_BREAK")
    files = sorted((d / "logs").glob("larm.*.log"))
    if len(files) != 1 or not re.fullmatch(r"larm\.\d{4}-\d{2}-\d{2}\.log", files[0].name):
        raise AssertionError(f"fichero diario esperado larm.AAAA-MM-DD.log, observado {[f.name for f in files]}")
    text = files[0].read_text(encoding="utf-8")
    lines = [json.loads(ln) for ln in text.splitlines() if ln.strip()]
    bad_ts = [ln.get("timestamp") for ln in lines if not TS.match(str(ln.get("timestamp")))]
    if bad_ts:
        raise AssertionError(f"timestamps fuera de formato: {bad_ts[:3]}")
    fields = [ln.get("fields", {}) for ln in lines]
    corr = [f["correlation_id"] for f in fields if "correlation_id" in f]
    if not corr or not all(UUID7.match(c) for c in corr):
        raise AssertionError(f"correlation_id ausente o no UUIDv7: {corr}")
    if not any(f.get("external_telemetry") is False for f in fields):
        raise AssertionError("no consta la telemetría externa desactivada")
    if SECRET in text or SECRET in stdout:
        raise AssertionError("el secreto inyectado aparece sin redactar")
    if "token=[REDACTED]" not in text:
        raise AssertionError("no aparece la marca de redacción del secreto inyectado")
    if not any("cierre limpio" in str(f.get("message")) for f in fields):
        raise AssertionError("no consta el cierre limpio")
    return (f"{files[0].name}: {len(lines)} líneas JSON, timestamps ms+Z, correlation_id UUIDv7 {corr[0]}, "
            f"telemetría off, secreto inyectado -> token=[REDACTED] (fichero y stdout), exit 0")


def retention() -> str:
    d = case_dir("retention")
    logs = d / "logs"
    logs.mkdir()
    for day in range(1, 21):
        (logs / f"larm.2026-09-{day:02d}.log").write_text("antiguo\n", encoding="utf-8")
    rc, _ = start_and_stop(clean_env(), [], d)
    if rc != 0:
        raise AssertionError(f"exit {rc}")
    remaining = sorted(p.name for p in logs.glob("larm.*.log"))
    retention_days = json.loads((REPO / "docs/contracts/defaults.json").read_text(encoding="utf-8"))["diagnostics"]["log_retention_days"]
    if len(remaining) > retention_days:
        raise AssertionError(f"{len(remaining)} ficheros > retención {retention_days}: {remaining}")
    if "larm.2026-09-01.log" in remaining:
        raise AssertionError("se conservó el más antiguo y se podó otro")
    return f"21 ficheros (20 antiguos + hoy) -> {len(remaining)} conservados (retención {retention_days}); el más antiguo conservado: {remaining[0]}"


def stdout_blocked() -> str:
    import socket
    import urllib.request
    d = case_dir("stdout-bloqueado")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    proc = subprocess.Popen([str(EXE), "--data-root", str(d), "--port", str(port), "--serve-api", "--log-level", "debug"],
                            env=clean_env(), creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)  # nadie lee este pipe
    try:
        url = f"http://127.0.0.1:{port}/api/v1/health/live"
        for _ in range(100):
            try:
                urllib.request.urlopen(url, timeout=1)
                break
            except OSError:
                time.sleep(0.2)
        started = time.monotonic()
        for i in range(500):
            with urllib.request.urlopen(url, timeout=10) as r:
                if r.status != 200:
                    raise AssertionError(f"petición {i}: {r.status}")
        took = time.monotonic() - started
        os.kill(proc.pid, signal.CTRL_BREAK_EVENT)
        rc = proc.wait(timeout=30)
    finally:
        if proc.poll() is None:
            proc.kill()
    text = "".join(f.read_text(encoding="utf-8", errors="replace") for f in (d / "logs").glob("larm.*.log"))
    logged = text.count('"petición"') or text.count("petición")
    if rc != 0:
        raise AssertionError(f"exit {rc}")
    if logged < 500:
        raise AssertionError(f"el fichero registra {logged} de 500 peticiones")
    return f"500 peticiones en {took:.1f}s con stdout sin leer; {logged} registradas en el fichero; exit 0"


COMMANDS = {"stdout-bloqueado": stdout_blocked, "precedence": precedence, "rejects": rejects, "logging": logging_case, "retention": retention}

if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    if not EXE.exists():
        print(f"FAIL no existe {EXE}; cargo build -p larm-daemon")
        raise SystemExit(1)
    try:
        print("PASS " + COMMANDS[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, IndexError, OSError, json.JSONDecodeError, subprocess.TimeoutExpired) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
