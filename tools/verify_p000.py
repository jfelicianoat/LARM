#!/usr/bin/env python3
"""P000 - Verificador del preflight: inventory, environment.lock y fixtures.lock.

Cada subcomando es una prueba independiente (exit 0 = PASS) para que
tools/p000_evidence.py registre su salida real:

  json            JSON validos y esquema minimo del inventario
  secrets         sin tokens/secretos en locks e inventario
  unknown         negativo: unknown rellenado con default (0, '', uint32 saturado) se rechaza
  schema-neg      negativo: inventario sin clave obligatoria se rechaza
  fixtures        hashes de fixtures del paquete recalculados
  llamacpp        dos versiones de llama.cpp fijadas por tag+commit
  volumes         volumenes con Windows Volume GUID; segundo volumen registrado sin inventar
  env             toolchains y dependencias fijadas con hash
  lock-coherence  environment.lock coincide con Cargo.lock/package-lock en lo que se usa
  compare A B     dos capturas reales normalizadas producen el mismo hash
"""
import hashlib
import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PKG = REPO.parent / "LARM_prompts_v3"
sys.path.insert(0, str(Path(__file__).resolve().parent))
from normalize_inventory import canonical_bytes, normalize  # noqa: E402

INV = REPO / "docs/implementation/inventory.json"
ENV = REPO / "environment.lock.json"
FIX = REPO / "fixtures.lock.json"

SECRET_PATTERNS = [
    re.compile(r"sk-[A-Za-z0-9]{20,}"),
    re.compile(r"ghp_[A-Za-z0-9]{30,}"),
    re.compile(r"github_pat_[A-Za-z0-9_]{30,}"),
    re.compile(r"hf_[A-Za-z0-9]{30,}"),
    re.compile(r"AKIA[0-9A-Z]{16}"),
    re.compile(r"Bearer\s+[A-Za-z0-9\-._~+/]{20,}"),
    re.compile(r"(?i)\"(password|passwd|api[_-]?key|token|secret)\"\s*:\s*\"(?!NO_DISPONIBLE|PENDING)[^\"]{6,}\""),
]
REQUIRED_INVENTORY_KEYS = ["os", "cpu", "ram", "gpus", "npu", "volumes", "detectedTools", "httpsReachability"]
# AdapterRAM de WMI es uint32: estos valores son saturacion, no VRAM medida.
WMI_UINT32_SATURATION = {4293918720, 4294967295, 4294967296}
DEFAULTS = (0, "", "0", "0.0.0", "unknown", "Unknown", "N/A")


def load(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def validate_min_schema(inv: dict) -> None:
    for k in REQUIRED_INVENTORY_KEYS:
        if k not in inv:
            raise ValueError(f"missing required key: {k}")
    if not inv["os"].get("build"):
        raise ValueError("os.build vacio")
    if not inv["cpu"].get("name"):
        raise ValueError("cpu.name vacio")


def defaulted_unknowns(inv: dict) -> list[str]:
    """Campos donde un dato desconocido aparece rellenado con un default."""
    hits = []
    for g in inv.get("gpus", []):
        vram = g.get("vramBytes")
        if vram in DEFAULTS or (str(vram).isdigit() and int(vram) in WMI_UINT32_SATURATION):
            hits.append(f"gpu {g.get('name')}: vramBytes={vram}")
        if g.get("driverVersion") in DEFAULTS:
            hits.append(f"gpu {g.get('name')}: driverVersion={g.get('driverVersion')}")
    for d in inv.get("graphicsDrivers", []):
        if d.get("driverVersion") in DEFAULTS:
            hits.append(f"driver {d.get('deviceName')}: driverVersion={d.get('driverVersion')}")
    if inv.get("npu") in DEFAULTS:
        hits.append(f"npu={inv.get('npu')}")
    for v in inv.get("volumes", []):
        if v.get("volumeGuid") in DEFAULTS:
            hits.append(f"volume {v.get('driveLetter')}: volumeGuid={v.get('volumeGuid')}")
    return hits


def cmd_json() -> str:
    payload = load(INV)
    load(ENV)
    load(FIX)
    validate_min_schema(payload["inventory"])
    recorded = payload.get("inventory_sha256")
    actual = hashlib.sha256(canonical_bytes(payload["inventory"])).hexdigest()
    if recorded != actual:
        raise ValueError(f"inventory_sha256 registrado {recorded} != recalculado {actual}")
    return f"3 JSON validos; claves {REQUIRED_INVENTORY_KEYS} presentes; inventory_sha256={actual}"


def cmd_secrets() -> str:
    found = []
    for t in (INV, REPO / "docs/implementation/inventory.raw.json", ENV, FIX):
        text = t.read_text(encoding="utf-8")
        for pat in SECRET_PATTERNS:
            m = pat.search(text)
            if m:
                found.append(f"{t.name}:{m.group(0)[:16]}...")
    if found:
        raise ValueError("secretos: " + "; ".join(found))
    return "sin coincidencias de patrones de secreto en 4 ficheros"


def cmd_unknown() -> str:
    inv = load(INV)["inventory"]
    real_hits = defaulted_unknowns(inv)
    if real_hits:
        raise ValueError("el inventario real contiene unknown rellenado: " + "; ".join(real_hits))
    mutants = []
    for mutate in (
        lambda i: i["gpus"][0].__setitem__("vramBytes", 0),
        lambda i: i["gpus"][0].__setitem__("vramBytes", "4293918720"),
        lambda i: i["gpus"][0].__setitem__("driverVersion", "unknown"),
        lambda i: i.__setitem__("npu", ""),
    ):
        bad = json.loads(json.dumps(inv))
        mutate(bad)
        mutants.append(bool(defaulted_unknowns(bad)))
    if not all(mutants):
        raise ValueError(f"mutantes no detectados: {mutants}")
    return f"real limpio; {len(mutants)}/{len(mutants)} mutantes con default detectados (incluye AdapterRAM uint32 saturado)"


def cmd_schema_neg() -> str:
    inv = load(INV)["inventory"]
    rejected = []
    for key in ("os", "volumes", "npu"):
        bad = dict(inv)
        bad.pop(key)
        try:
            validate_min_schema(bad)
        except ValueError as e:
            rejected.append(str(e))
            continue
        raise ValueError(f"se acepto un inventario sin {key}")
    return "; ".join(rejected)


def cmd_fixtures() -> str:
    fix = load(FIX)
    gen_dir = PKG / "fixtures/generated"
    files = fix.get("package_generated_fixtures", {}).get("files", [])
    if not files:
        raise ValueError("fixtures.lock sin package_generated_fixtures")
    bad = []
    for f in files:
        p = gen_dir / f["file"]
        if not p.exists() or hashlib.sha256(p.read_bytes()).hexdigest() != f["sha256"] or p.stat().st_size != f["bytes"]:
            bad.append(f["file"])
    if bad:
        raise ValueError(f"no coinciden: {bad}")
    return f"{len(files)} fixtures recalculados coinciden: " + ", ".join(f["file"] for f in files)


def cmd_llamacpp() -> str:
    versions = load(FIX).get("llama_cpp_versions", [])
    if len(versions) != 2:
        raise ValueError(f"se esperaban 2 versiones, hay {len(versions)}")
    for v in versions:
        if not re.fullmatch(r"b\d+", v.get("tag", "")) or not re.fullmatch(r"[0-9a-f]{40}", v.get("commit_sha", "")):
            raise ValueError(f"tag/commit invalido: {v.get('tag')} {v.get('commit_sha')}")
        if v["tag"] == versions[0]["tag"] and v is not versions[0]:
            raise ValueError("las dos versiones son la misma")
    return "versiones " + ", ".join(f"{v['tag']}@{v['commit_sha'][:10]}" for v in versions)


def cmd_volumes() -> str:
    inv = load(INV)["inventory"]
    vols = inv["volumes"]
    guid = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
    if not vols or not all(guid.fullmatch(str(v.get("volumeGuid"))) for v in vols):
        raise ValueError("volumen sin Windows Volume GUID valido")
    acc = load(FIX).get("acceptance_volumes", {})
    listed = acc.get("volumes", [])
    known = {v["volumeGuid"] for v in vols}
    for v in listed:
        if v.get("volume_guid") not in known:
            raise ValueError(f"volumen de aceptacion {v.get('volume_guid')} no aparece en la captura real")
    if len(listed) < 2 and not str(acc.get("second_volume_status", "")).startswith("NO_DISPONIBLE"):
        raise ValueError("falta el segundo volumen y no se registra como NO_DISPONIBLE")
    return f"{len(vols)} volumenes con GUID; aceptacion: {len(listed)} real(es), segundo={acc.get('second_volume_status', 'presente')}"


def cmd_env() -> str:
    env = load(ENV)
    crates = env.get("rust", {}).get("crates", {})
    npm = env.get("node", {}).get("packages", {})
    bad_c = [n for n, c in crates.items() if not re.fullmatch(r"[0-9a-f]{64}", c.get("sha256", ""))]
    bad_n = [n for n, v in npm.items() if not str(v.get("integrity_sha512_base64", "")).startswith("sha512-")]
    if bad_c or bad_n or len(crates) < 20 or len(npm) < 10:
        raise ValueError(f"crates sin sha256={bad_c}; npm sin integrity={bad_n}")
    if not env.get("rust", {}).get("rustc") or not env.get("node", {}).get("node"):
        raise ValueError("toolchain sin fijar")
    return f"{len(crates)} crates con sha256, {len(npm)} paquetes npm con integrity; rustc={env['rust']['rustc']} node={env['node']['node']}"


def cmd_lock_coherence() -> str:
    env = load(ENV)
    lock = load(REPO / "package-lock.json")["packages"]
    problems, checked = [], 0
    for name, v in env["node"]["packages"].items():
        entry = lock.get("node_modules/" + name)
        if entry is None:
            continue
        checked += 1
        if entry["version"] != v["version"] or entry.get("integrity") != v.get("integrity_sha512_base64"):
            problems.append(f"npm {name}: lock={v['version']} package-lock={entry['version']}")
    cargo_lock = {}
    for blk in (REPO / "Cargo.lock").read_text(encoding="utf-8").split("[[package]]")[1:]:
        n = re.search(r'name = "([^"]+)"', blk).group(1)
        ver = re.search(r'version = "([^"]+)"', blk).group(1)
        c = re.search(r'checksum = "([^"]+)"', blk)
        cargo_lock.setdefault(n, set()).add((ver, c.group(1) if c else None))
    workspace = (REPO / "Cargo.toml").read_text(encoding="utf-8")
    for name, c in env["rust"]["crates"].items():
        declared = re.search(rf'^{re.escape(name)} = .*?"=([^"]+)"', workspace, re.M)
        if declared and declared.group(1) != c["version"]:
            problems.append(f"crate {name}: lock={c['version']} Cargo.toml={declared.group(1)}")
        if name in cargo_lock and declared and any(v == c["version"] for v, _ in cargo_lock[name]):
            checked += 1
            if (c["version"], c["sha256"]) not in cargo_lock[name]:
                problems.append(f"crate {name} {c['version']}: checksum distinto en Cargo.lock")
    # Toda dependencia fijada (`=x.y.z`) del workspace debe estar en el lock de entorno.
    section = workspace.split("[workspace.dependencies]", 1)[-1].split("\n[", 1)[0]
    for m in re.finditer(r'^([A-Za-z0-9_-]+) = .*?"=([^"]+)"', section, re.M):
        if m.group(1) not in env["rust"]["crates"]:
            problems.append(f"crate {m.group(1)} {m.group(2)}: fijada en Cargo.toml y ausente de environment.lock.json")
    if problems:
        raise ValueError("; ".join(problems))
    return f"{checked} dependencias usadas coinciden en version y hash con Cargo.lock/package-lock.json"


def cmd_compare(a: str, b: str) -> str:
    ha = hashlib.sha256(canonical_bytes(normalize(load(Path(a))))).hexdigest()
    hb = hashlib.sha256(canonical_bytes(normalize(load(Path(b))))).hexdigest()
    if ha != hb:
        na, nb = normalize(load(Path(a))), normalize(load(Path(b)))
        diff = [k for k in na if na[k] != nb.get(k)]
        raise ValueError(f"hash distinto {ha[:12]} != {hb[:12]}; claves que difieren: {diff}")
    return f"dos capturas independientes -> mismo hash {ha}"


COMMANDS = {
    "json": cmd_json, "secrets": cmd_secrets, "unknown": cmd_unknown, "schema-neg": cmd_schema_neg,
    "fixtures": cmd_fixtures, "llamacpp": cmd_llamacpp, "volumes": cmd_volumes, "env": cmd_env,
    "lock-coherence": cmd_lock_coherence,
}


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__)
        return 2
    try:
        if argv[0] == "compare":
            print("PASS " + cmd_compare(argv[1], argv[2]))
        else:
            print("PASS " + COMMANDS[argv[0]]())
        return 0
    except (ValueError, KeyError, OSError, json.JSONDecodeError) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
