#!/usr/bin/env python3
"""P043: catálogo/CLI/HTTP con daemon, SQLite y paquetes de prueba reales.

La instalación source inicial se siembra como fixture INSTALLED/UNTRUSTED;
no acredita instalación ZIP, conformidad P026 ni un repositorio remoto.
No ejecuta el adaptador copiado ni transmite credenciales a fuentes.
"""
import copy
import hashlib
import json
import platform
import shutil
import sqlite3
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
import uuid
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path

from jsonschema import Draft202012Validator
from referencing import Registry, Resource
from p023_real import clean_env, package_hash, start, stop

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
OPERATION = "P043_post__sources"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def cli(data, *args):
    process = subprocess.run([str(EXE), "--data-root", str(data), *map(str, args)],
                             capture_output=True, env=clean_env(), timeout=60)
    assert process.returncode == 0, process.stderr.decode("utf-8", errors="replace")
    return json.loads(process.stdout)


def fetch(method, url, token=None, body=None, key=None, version=None, correlation=None):
    request = urllib.request.Request(url, method=method,
                                    data=json.dumps(body).encode() if body is not None else None)
    if token:
        request.add_header("authorization", f"Bearer {token}")
    if body is not None:
        request.add_header("content-type", "application/json")
    for name, value in (("idempotency-key", key), ("if-match", version), ("x-correlation-id", correlation)):
        if value is not None:
            request.add_header(name, str(value))
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            raw = response.read()
            return response.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        raw = error.read()
        return error.code, json.loads(raw) if raw else None


def snapshot(db):
    with sqlite3.connect(db) as connection:
        return {
            "sources": connection.execute("SELECT * FROM model_source ORDER BY id").fetchall(),
            "installations": connection.execute("SELECT id,row_version,metadata,state,trust FROM adapter_installation ORDER BY id").fetchall(),
            "adapters": connection.execute("SELECT * FROM model_source_adapter ORDER BY id").fetchall(),
            "events": connection.execute("SELECT * FROM event WHERE resource_type IN ('model_source','model_source_adapter') ORDER BY event_id").fetchall(),
            "outbox": connection.execute("SELECT o.event_id,o.event_record_id FROM event_outbox o JOIN event e ON e.event_id=o.event_id WHERE e.resource_type IN ('model_source','model_source_adapter') ORDER BY o.event_id").fetchall(),
            "replay": connection.execute("SELECT * FROM idempotency WHERE operation=? ORDER BY principal_id,key", (OPERATION,)).fetchall(),
            "jobs": connection.execute("SELECT * FROM operation_journal WHERE kind LIKE 'source.%' ORDER BY id").fetchall(),
            "reservations": connection.execute("SELECT * FROM volume_reservation WHERE job_id IN (SELECT id FROM operation_journal WHERE kind LIKE 'source.%')").fetchall(),
        }


def validator(name):
    registry = Registry()
    for schema in ("larm.schema.json", "domain.schema.json"):
        document = json.loads((REPO / "docs/contracts/schemas" / schema).read_text(encoding="utf-8"))
        registry = registry.with_resource(schema, Resource.from_contents(document))
    return Draft202012Validator({"$ref": f"larm.schema.json#/$defs/{name}"}, registry=registry)


def main():
    out = REPO / "docs/reports/P043_runs"
    run = out / "sources-real" / uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    data, daemon = run / "data", None
    db = data / "state/larm.sqlite"
    names, steps = [], []
    success = False
    source_schema, page_schema = validator("model_source"), validator("model_sourceList")
    correlation = str(uuid.uuid7())

    def step(name, valid, detail):
        steps.append({"step": name, "result": "PASS" if valid else "FAIL", "detail": detail})
        print(f"[{'PASS' if valid else 'FAIL'}] {name}: {detail}", flush=True)
        assert valid, name

    try:
        cli(data, "db-info")
        pkg = data / "adapters/fixture.source/1.0.0"
        (pkg / "bin").mkdir(parents=True)
        shutil.copyfile(REPO / "target/debug/examples/fake_source.exe", pkg / "bin/fake_source.exe")
        table = json.loads((REPO / "docs/contracts/adapter-methods.json").read_text(encoding="utf-8"))
        methods = ["core.handshake", "core.cancel", "core.heartbeat"] + [m["method"] for m in table["methods"] if m["family"] == "source"]
        manifest = {
            "id": "fixture.source", "version": "1.0.0", "family": "source", "protocol_version": "1.0", "publisher": "P043 source fixture",
            "executable": {"relative_path": "bin/fake_source.exe", "args": ["--manifest", str(pkg / "manifest.json")]},
            "platforms": ["windows"], "architectures": ["x86_64"], "methods": methods,
            "permissions": {"read_roots": [], "write_roots": [], "network_origins": [], "process_allowlist": [], "secret_refs": []},
            "capabilities": ["source.search"], "package_sha256": package_hash(pkg), "signature": None, "installation_strategies": [],
        }
        (pkg / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
        installation = str(uuid.uuid7())
        stamp = datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
        with sqlite3.connect(db) as connection:
            connection.execute("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES (?,?,?,1,'{}',?,?,?,?,?,'UNTRUSTED',?,'INSTALLED')",
                               (installation, stamp, stamp, manifest["id"], manifest["version"], "source", manifest["package_sha256"], manifest["publisher"], json.dumps(manifest)))
        before_files = {str(p.relative_to(pkg)): sha(p) for p in pkg.rglob("*") if p.is_file()}
        descriptor = run / "search-support.json"
        descriptor.write_text(json.dumps({"search_applicable": True, "supported_filters": ["source.search.q"], "limitations": ["fixture_only"]}), encoding="utf-8")
        binding = cli(data, "register-source-adapter", "--installation", installation, "--source-type", "HTTP", "--descriptor", descriptor)
        first_binding = snapshot(db)
        again = cli(data, "register-source-adapter", "--installation", installation, "--source-type", "HTTP", "--descriptor", descriptor)
        step("binding-local-sin-cualificacion", binding == again and snapshot(db) == first_binding,
             "registro local repetible, capabilities derivadas, binding e índice público atómicos; no promueve la instalación fixture")
        principals = {}
        for role, kind, scopes in (("writer", "OPERATOR", "read,models:write"), ("reader", "OPERATOR", "read"), ("other", "OPERATOR", "read,models:write"), ("mcp", "MCP", "read,models:write")):
            name = f"p043-sources-{run.name}-{role}"
            names.append(name)
            args = ["enroll-principal", "--name", name, "--kind", kind, "--scopes", scopes]
            if kind == "MCP":
                args += ["--mcp-scopes", "admin"]
            principals[role] = cli(data, *args)
        tokens = {role: entry["token"] for role, entry in principals.items()}
        daemon, base = start(data)
        url = f"{base}/sources"
        status, adapters = fetch("GET", f"{base}/adapters?limit=200", tokens["reader"])
        public = next(item for item in adapters["items"] if item["id"] == installation)
        step("binding-visible-sin-identificadores-manuales", status == 200 and public["metadata"]["source_bindings"] == [{"id": binding["id"], "source_type": "HTTP"}] and public["state"] == "INSTALLED" and public["trust"] == "UNTRUSTED",
             "GET /adapters publica conexiones descriptivas para la UI; el servidor conserva el FK tipado y la cualificación como autoridad")
        body = {"name": "source_%", "adapter_id": binding["id"], "base_url": "https://EXAMPLE.invalid:443/models", "priority": 1, "enabled": False,
                "auth_mode": "NONE", "account_id": None, "policy": {"vendor": {"unknown": [1, True, None]}}}
        before = snapshot(db)
        denied = [fetch("POST", url, None, body, "no-auth")[0], fetch("POST", url, tokens["reader"], body, "reader")[0], fetch("POST", url, tokens["mcp"], body, "mcp")[0]]
        step("guard-api-y-mcp", denied == [401, 403, 403] and snapshot(db) == before,
             "Bearer, models:write e intersección MCP se exigen antes de cualquier mutación")
        invalid = []
        for key in body:
            missing = copy.deepcopy(body)
            missing.pop(key)
            invalid.append((missing, 400))
        invalid += [(body | {"server_field": True}, 400), (body | {"base_url": "https://user:fixture@example.invalid"}, 400),
                    (body | {"base_url": "https://example.invalid?token=x"}, 400), (body | {"policy": {"nested": {"api_key": "fixture-only"}}}, 400),
                    (body | {"base_url": "http://example.invalid", "auth_mode": "BASIC"}, 403), (body | {"enabled": True}, 409)]
        for index, (value, expected) in enumerate(invalid):
            status, error = fetch("POST", url, tokens["writer"], value, f"invalid-{index}")
            assert status == expected, (index, status, error)
        assert fetch("POST", url, tokens["writer"], body)[0] == 400
        step("dto-y-secretos-sin-efectos", snapshot(db) == before,
             "ocho claves obligatorias, campos extra, credenciales en URL/policy, transporte inseguro, habilitar sin conformidad y clave ausente se rechazan sin filas/replay/eventos")
        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(lambda _: fetch("POST", url, tokens["writer"], body, "same-key", correlation=correlation), range(2)))
        status, original = results[0]
        source_schema.validate(original)
        step("alta-concurrente-atomica", status == 201 and results[0] == results[1] and len(snapshot(db)["sources"]) == 1,
             "dos POST reales simultáneos devuelven el mismo 201 original; una sola fuente")
        identity = original["id"]
        with sqlite3.connect(db) as connection:
            persisted = connection.execute("SELECT base_url,policy FROM model_source WHERE id=?", (identity,)).fetchone()
            event = connection.execute("SELECT actor_id,correlation_id FROM event WHERE resource_id=?", (identity,)).fetchone()
            body_hash = connection.execute("SELECT body_hash FROM idempotency WHERE operation=?", (OPERATION,)).fetchone()[0]
        step("dto-derivado-y-contexto", original["base_url"] == "https://example.invalid/models" and persisted[0] == original["base_url"]
             and json.loads(persisted[1]) == body["policy"] and event == (principals["writer"]["principal"]["id"], correlation)
             and body_hash == hashlib.sha256(json.dumps(body).encode()).hexdigest(),
             "URL normalizada, política desconocida preservada, contexto autenticado y hash del cuerpo exacto coinciden con SQLite")
        before = snapshot(db)
        status, error = fetch("POST", url, tokens["writer"], body | {"name": "different"}, "same-key")
        step("conflicto-sin-duplicados", status == 409 and error["code"] == "IDEMPOTENCY_CONFLICT" and snapshot(db) == before,
             "misma clave con cuerpo distinto rechaza sin mutaciones")
        status, second = fetch("POST", url, tokens["other"], body, "same-key")
        step("aislamiento-de-actores", status == 201 and second["id"] != identity, "otro principal tiene su propio espacio idempotente")
        status, first_page = fetch("GET", f"{url}?limit=1", tokens["reader"])
        page_schema.validate(first_page)
        status2, next_page = fetch("GET", f"{url}?limit=1&cursor={urllib.parse.quote(first_page['next_cursor'])}", tokens["reader"])
        page_schema.validate(next_page)
        changed_cursor = urllib.parse.urlencode({"cursor": first_page["next_cursor"], "q": "changed"})
        filtered = urllib.parse.urlencode({"q": "_%", "capability": "source.search"})
        detail_status, detail = fetch("GET", f"{url}/{identity}", tokens["reader"])
        step("lista-detalle-y-cursor", status == status2 == detail_status == 200 and first_page["items"] == [original] and next_page["items"] == [second]
             and next_page["next_cursor"] is None and detail == original and fetch("GET", f"{url}?{changed_cursor}", tokens["reader"])[0] == 400
             and fetch("GET", f"{url}?enabled=true", tokens["reader"])[0] == 400 and len(fetch("GET", f"{url}?{filtered}", tokens["reader"])[1]["items"]) == 2,
             "dos páginas sin saltos, detalle exacto, filtro literal y cursor ligado a filtros; parámetro desconocido rechazado")
        target = f"{url}/{identity}"
        before = snapshot(db)
        assert fetch("PATCH", target, tokens["writer"], body)[0] == 428
        assert fetch("PATCH", target, tokens["writer"], body, version="invalid")[0] == 400
        assert fetch("PATCH", target, tokens["writer"], body, version=0)[0] == 409
        step("if-match-obligatorio", snapshot(db) == before, "revisión ausente, inválida o antigua no modifica la fuente")
        changed = body | {"name": "updated", "policy": {"vendor": {"retained": True}}}
        with ThreadPoolExecutor(max_workers=2) as executor:
            updates = list(executor.map(lambda _: fetch("PATCH", target, tokens["writer"], changed, version=1), range(2)))
        updated = next(value for code, value in updates if code == 200)
        source_schema.validate(updated)
        step("cas-real-un-ganador", sorted(code for code, _ in updates) == [200, 409] and updated["row_version"] == 2,
             "dos PATCH simultáneos sobre revisión 1 producen un único ganador")
        before = snapshot(db)
        assert fetch("DELETE", target, tokens["writer"], body={"ignored": True}, version=2)[0] == 400
        assert fetch("DELETE", target, tokens["writer"], version=1)[0] == 409
        step("borrado-no-ignora-body-ni-cas", snapshot(db) == before, "body no publicado y revisión antigua se rechazan sin borrar")
        account = str(uuid.uuid7())
        with sqlite3.connect(db) as connection:
            connection.execute("INSERT INTO model_source_account(id,created_at,updated_at,row_version,metadata,source_id,credential_reference_id,display_name,accepted_license_hashes) VALUES (?,?,?,1,'{}',?,NULL,'fixture','[]')", (account, stamp, stamp, second["id"]))
        before = snapshot(db)
        assert fetch("PATCH", target, tokens["writer"], changed | {"account_id": account}, version=2)[0] == 403
        assert fetch("DELETE", f"{url}/{second['id']}", tokens["other"], version=1)[0] == 409
        assert fetch("PATCH", f"{url}/{second['id']}", tokens["other"], body | {"base_url": "https://another.invalid"}, version=1)[0] == 409
        step("cuentas-referencias-e-identidad", snapshot(db) == before,
             "cuenta ajena, fuente referenciada y cambio de identidad con referencias no tienen efectos")
        stop(daemon)
        assert daemon.returncode == 0
        daemon, base = start(data)
        url, target = f"{base}/sources", f"{base}/sources/{identity}"
        step("reinicio-y-replay-original", fetch("GET", target, tokens["reader"]) == (200, updated)
             and fetch("POST", url, tokens["writer"], body, "same-key") == (201, original),
             "otro proceso conserva la edición y devuelve el 201 original, sin reemplazarlo por la versión actual")
        before = snapshot(db)
        with sqlite3.connect(db) as connection:
            connection.execute("BEGIN IMMEDIATE")
            connection.execute("CREATE TRIGGER p043_native_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(FAIL,'p043_native_outbox'); END")
        assert fetch("POST", url, tokens["writer"], body | {"name": "not-saved"}, "outbox-fault")[0] == 500
        assert fetch("PATCH", target, tokens["writer"], changed | {"name": "not-saved"}, version=2)[0] == 500
        assert fetch("DELETE", target, tokens["writer"], version=2)[0] == 500
        step("fallo-real-outbox-atomico", snapshot(db) == before,
             "un trigger real hace fallar alta/edición/borrado; fuente, replay, eventos y outbox revierten juntos")
        with sqlite3.connect(db) as connection:
            connection.execute("DROP TRIGGER p043_native_outbox")
        assert fetch("DELETE", target, tokens["writer"], version=2) == (204, None)
        assert fetch("GET", target, tokens["reader"])[0] == 404
        assert fetch("POST", url, tokens["writer"], body, "same-key") == (201, original)
        with sqlite3.connect(db) as connection:
            connection.execute("DELETE FROM model_source_account WHERE id=?", (account,))
        assert fetch("DELETE", f"{url}/{second['id']}", tokens["other"], version=1) == (204, None)
        with sqlite3.connect(db) as connection:
            connection.execute("DELETE FROM model_source_adapter WHERE id=?", (binding["id"],))
        before = snapshot(db)
        step("replay-tras-borrado-de-fuente-y-binding", fetch("POST", url, tokens["writer"], body, "same-key") == (201, original) and snapshot(db) == before,
             "el replay no depende del recurso/binding actual ni vuelve a crear fuentes borradas")
        with sqlite3.connect(db) as connection:
            state = connection.execute("SELECT state,trust FROM adapter_installation WHERE id=?", (installation,)).fetchone()
            versions = connection.execute("SELECT resource_version,json_extract(payload,'$.action') FROM event WHERE resource_id=? ORDER BY event_id", (identity,)).fetchall()
        step("eventos-y-paquete-conservados", state == ("INSTALLED", "UNTRUSTED") and versions == [(1, "created"), (2, "updated"), (3, "deleted")]
             and before_files == {str(p.relative_to(pkg)): sha(p) for p in pkg.rglob("*") if p.is_file()}
             and not snapshot(db)["jobs"] and not snapshot(db)["reservations"],
             "eventos ordenados por revisión; sin promoción, IO de paquete, journals de fuente ni reservas para CRUD de configuración")
        (run / "final-state.json").write_text(json.dumps(snapshot(db), indent=2, ensure_ascii=False), encoding="utf-8")
        success = True
        print(f"PASS: {len(steps)} pasos reales de catálogo/HTTP/SQLite; no descarga ni cualificación upstream", flush=True)
        return 0
    finally:
        if daemon is not None and daemon.poll() is None:
            try:
                stop(daemon)
            finally:
                if daemon.poll() is None:
                    daemon.kill()
                daemon.wait(timeout=15)
        if db.exists():
            with sqlite3.connect(db) as connection:
                connection.execute("DROP TRIGGER IF EXISTS p043_native_outbox")
        for name in names:
            subprocess.run([str(EXE), "--data-root", str(data), "revoke-principal", "--name", name], capture_output=True, env=clean_env(), timeout=30)
        record = {"scope": "catálogo/CLI/HTTP con SQLite y paquetes fixture reales; no ejecución de una fuente remota", "result": "PASS" if success else "FAIL",
                  "steps": steps, "run": str(run.relative_to(REPO)), "host": platform.platform(), "python": platform.python_version(), "daemon_sha256": sha(EXE),
                  "generated_at": datetime.now(timezone.utc).isoformat()}
        (run / "steps.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")
        (out / "sources-real.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
