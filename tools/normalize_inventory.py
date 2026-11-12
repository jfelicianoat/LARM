#!/usr/bin/env python3
"""P000 - Normaliza inventory.raw.json en inventory.json determinista.

Elimina metricas volatiles (pids, latencias ms, mensajes de error de red,
fecha/hora de ejecucion) para que dos capturas equivalentes produzcan el
mismo hash. La captura completa queda en inventory.raw.json.
"""
import hashlib
import json
import sys
from pathlib import Path

VOLATILE_NOTE = "hash determinista: excluye pids, latencias, mensajes de red transitorios y marcas de tiempo"


def normalize(raw: dict) -> dict:
    out = {}

    out["os"] = raw["os"]

    out["cpu"] = raw["cpu"]

    out["ram"] = {"totalBytes": raw["ram"]["totalBytes"], "modules": raw["ram"].get("modules", [])}

    gpus = sorted(raw["gpus"], key=lambda g: (str(g.get("name")), str(g.get("driverVersion"))))
    out["gpus"] = [
        {k: g.get(k, "NO_DISPONIBLE") for k in ("name", "adapterCompat", "pnpDeviceId", "driverVersion", "driverDate", "vramBytes", "vramSource", "status")}
        for g in gpus
    ]

    npu = raw["npu"]
    if isinstance(npu, dict):
        npu = [npu]  # PowerShell desenvuelve arrays de un elemento
    out["npu"] = npu

    drivers = sorted(raw.get("graphicsDrivers", []), key=lambda d: (str(d.get("deviceName")), str(d.get("infName"))))
    out["graphicsDrivers"] = [
        {k: d.get(k, "NO_DISPONIBLE") for k in ("deviceName", "driverVersion", "driverDate", "infName")}
        for d in drivers
    ]

    vols = raw["volumes"]
    if isinstance(vols, dict):
        vols = [vols]
    vols = sorted(vols, key=lambda v: str(v.get("volumeGuid")))
    out["volumes"] = [
        {k: v.get(k, "NO_DISPONIBLE") for k in ("volumeGuid", "volumeGuidPath", "driveLetter", "gptPartitionGuid",
                                                "diskNumber", "driveType", "sizeBytes", "label", "fileSystem")}
        for v in vols
    ]

    # procesos host: nombre y presencia de binario, sin pids ni rutas de sesion (volatiles)
    procs = raw.get("aiHostProcesses", [])
    seen = sorted({p["name"] for p in procs})
    out["aiHostProcesses"] = [{"name": n} for n in seen]

    # herramientas: nombre/version, la ruta puede variar por sesion pero es estable en el host; se conserva
    out["detectedTools"] = raw.get("detectedTools", {})

    # accesibilidad HTTPS: endpoint + ok; latencia y status exactos son volatiles
    # tolerante a entrada cruda (ok/ms/status) o ya normalizada (reachable): idempotente
    https = raw.get("httpsReachability", [])
    out["httpsReachability"] = sorted(
        [{"endpoint": h["endpoint"], "reachable": bool(h.get("ok", h.get("reachable")))} for h in https],
        key=lambda h: h["endpoint"],
    )

    # capabilities del Broker: status y version de contrato; el hash del cuerpo queda solo en raw
    caps = raw.get("brokerCapabilities", "NO_DISPONIBLE")
    if isinstance(caps, dict):
        caps = {k: caps.get(k, "NO_DISPONIBLE") for k in ("endpoint", "status", "contractVersion")}
    out["brokerCapabilities"] = caps
    out["brokerAuthCheck"] = raw.get("brokerAuthCheck", "NO_DISPONIBLE_SIN_CREDENCIAL_AUTORIZADA")

    out["normalization"] = VOLATILE_NOTE
    return out


def canonical_bytes(obj) -> bytes:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def main() -> int:
    raw_path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("docs/implementation/inventory.raw.json")
    out_path = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("docs/implementation/inventory.json")

    raw = json.loads(raw_path.read_text(encoding="utf-8"))
    norm = normalize(raw)
    digest = hashlib.sha256(canonical_bytes(norm)).hexdigest()
    payload = {"inventory": norm, "inventory_sha256": digest}
    out_path.write_text(json.dumps(payload, indent=2, ensure_ascii=False, sort_keys=True) + "\n", encoding="utf-8")
    print(digest)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
