#!/usr/bin/env python3
"""P000 - ejecuta las pruebas del preflight y escribe la evidencia PromptEvidence."""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence  # noqa: E402

PY = sys.executable
RUNS = REPO / "docs/reports/P000_runs"


def main() -> int:
    ev = Evidence("P000")
    fixtures = [REPO / "environment.lock.json", REPO / "fixtures.lock.json"]
    checks = [
        ("P000-CONTRACT-json", "json", "inventory/environment/fixtures son JSON validos con claves minimas y hash registrado igual al recalculado"),
        ("P000-CONTRACT-secrets", "secrets", "ningun patron de token/secreto en inventario, captura cruda y locks"),
        ("P000-FAULT-unknown-default", "unknown", "inventario real sin unknown rellenado; mutantes con 0/''/uint32 saturado rechazados"),
        ("P000-FAULT-schema-invalid", "schema-neg", "inventario sin os/volumes/npu rechazado"),
        ("P000-CONTRACT-package-fixtures", "fixtures", "sha256 y tamano recalculados de fixtures/generated coinciden con fixtures.lock"),
        ("P000-CONTRACT-llamacpp-two-versions", "llamacpp", "dos tags distintos de llama.cpp con commit de 40 hex"),
        ("P000-CONTRACT-acceptance-volumes", "volumes", "volumenes con Windows Volume GUID; segundo volumen NO_DISPONIBLE sin GUID inventado"),
        ("P000-CONTRACT-environment-lock", "env", "toolchains fijados; cada crate con sha256 y cada paquete npm con integrity"),
        ("P000-CONTRACT-lock-coherence", "lock-coherence", "environment.lock coincide con Cargo.toml, Cargo.lock y package-lock en version y hash"),
    ]
    for test_id, sub, expected in checks:
        kind = "FAULT" if test_id.startswith("P000-FAULT") else "CONTRACT"
        ev.run(test_id, kind, [PY, "tools/verify_p000.py", sub], expected, fixtures=fixtures)

    win = str(RUNS).replace("/", "\\")
    for label in ("a", "b"):
        ev.run(f"P000-REAL-capture-pcia-{label}", "REAL",
               ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/capture_inventory.ps1",
                "-OutFile", f"{win}\\capture_{label}.raw.json"],
               "captura read-only del PC IA escrita en UTF-8 sin error")
    ev.run("P000-REAL-capture-deterministic", "REAL",
           [PY, "tools/verify_p000.py", "compare", str(RUNS / "capture_a.raw.json"), str(RUNS / "capture_b.raw.json")],
           "dos capturas reales independientes, normalizadas sin metricas volatiles, dan el mismo hash",
           fixtures=[RUNS / "capture_a.raw.json", RUNS / "capture_b.raw.json"])

    inv = json.loads((RUNS / "capture_a.raw.json").read_text(encoding="utf-8"))
    reach = inv["httpsReachability"]
    (RUNS / "https_reachability.json").write_text(json.dumps(reach, indent=2) + "\n", encoding="utf-8")
    ev.run("P000-REAL-https-topology", "REAL",
           [PY, "-c", "import json,sys;r=json.load(open(sys.argv[1]));"
                      "assert len(r)==6 and all('endpoint' in x and 'ok' in x for x in r);"
                      "print('; '.join(f\"{x['endpoint']} ok={x['ok']} status={x.get('status')}\" for x in r))",
            str(RUNS / "https_reachability.json")],
           "accesibilidad HTTPS real registrada para 6 endpoints, con status observado y sin relleno")
    ev.run("P000-REAL-broker-capabilities", "REAL",
           [PY, "-c", "import json,sys;c=json.load(open(sys.argv[1]))['brokerCapabilities'];"
                      "assert c['status']==200 and c['contractVersion'];print(c)",
            str(RUNS / "capture_a.raw.json")],
           "GET /api/v1/capabilities (publico) del Broker local responde 200 con contract_version")
    broker_enrolled = (REPO / ".local/integrations.json").is_file()
    if broker_enrolled:
        ev.run("P000-REAL-broker-auth-check", "REAL", [PY, "tools/check_integration_credentials.py", "--broker-only"],
               "auth/check con la credencial autorizada por el operador: 200, authenticated y auth_required true; sin secretos en la salida")
    else:
        ev.blocked("P000-REAL-broker-auth-check", "REAL", "GET http://127.0.0.1:8765/api/v1/auth/check con credencial",
                   "auth/check con credencial autorizada responde 200", "sin referencia de credencial enrolada para el Broker")
    ev.blocked("P000-REAL-capture-pc-principal", "REAL",
               "powershell -File tools/capture_inventory.ps1 -OutFile <ruta> (en el PC principal)",
               "captura read-only del PC principal y su accesibilidad hacia el PC IA",
               "esta sesion solo tiene acceso al PC IA (A9_MEGA); el script es portable y queda listo para ejecutarse alli")

    changed = [
        "tools/capture_inventory.ps1", "tools/normalize_inventory.py", "tools/verify_p000.py",
        "tools/p000_evidence.py", "tools/evidence.py", "tools/validate_evidence.py", "tools/check_integration_credentials.py",
        "docs/implementation/inventory.raw.json", "docs/implementation/inventory.json",
        "environment.lock.json", "fixtures.lock.json", ".gitignore",
    ]
    blockers = [
        "segundo volumen de aceptacion: NO_DISPONIBLE_UNICO_VOLUMEN (G04/G05); requiere VHDX o disco autorizado por el operador",
        "captura en el PC principal no ejecutada: sin acceso desde esta sesion",
    ]
    if not broker_enrolled:
        blockers.append("Broker auth/check: NO_DISPONIBLE_SIN_CREDENCIAL_AUTORIZADA")
    ev.finish(changed, blockers)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
