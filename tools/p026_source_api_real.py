#!/usr/bin/env python3
"""Source habilitada por API real tras conformidad nativa, con perfil persistente.

Paquete/fila inicial y servidor HTTP son fixtures propios; no acredita P028 ni P050.
Sólo el worker y los oráculos compilados pueden confirmar conformidad.
"""
import hashlib
import json
import os
import platform
import queue
import signal
import sqlite3
import subprocess
import sys
import threading
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path

import rfc8785
from p005_real import REPO, EXE, cli, clean_env, step
from p023_real import start, package_hash
from p043_sources_real import fetch as request


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    out = REPO / "docs/reports/P026_runs"
    run = out / "source-api-real" / uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    data, db = run / "data", run / "data/state/larm.sqlite"
    daemon, helper, log, success = None, None, None, False
    steps, tokens, principals, trace = [], {}, {}, []
    names = {role: f"p026-source-{run.name}-{role}" for role in ("writer", "other", "reader")}
    digest = lambda value: hashlib.sha256(rfc8785.dumps(value)).hexdigest()

    def fetch(method, url, token=None, body=None, key=None, version=None):
        status, value = request(method, url, token, body, key, version)
        # No registrar tokens. Los polls no terminales no añaden evidencia de CRUD.
        if not (method == 'GET' and '/jobs/' in url and isinstance(value, dict) and value.get('state') in ('QUEUED', 'RUNNING')):
            trace.append({'method': method, 'path': '/api/v1' + url.split('/api/v1', 1)[1],
                          'actor_role': next((role for role, secret in tokens.items() if secret == token), 'unauthenticated'),
                          'request_sha256': hashlib.sha256(json.dumps(body).encode()).hexdigest() if body is not None else None,
                          'idempotency_key': key, 'if_match': version, 'status': status, 'response': value})
        return status, value

    def shutdown():
        nonlocal daemon
        if daemon is not None and daemon.poll() is None:
            os.kill(daemon.pid, signal.CTRL_BREAK_EVENT)
            daemon.wait(timeout=45)
            assert daemon.returncode == 0
        daemon = None

    def snapshot():
        with sqlite3.connect(db) as conn:
            tables = ("adapter_installation", "model_source_adapter", "model_source", "event", "event_outbox", "operation_journal", "idempotency", "resource_lock", "volume_reservation")
            return {table: conn.execute(f'SELECT * FROM "{table}" ORDER BY rowid').fetchall() for table in tables}

    def inert():
        paths = [p for p in (run / "work").rglob('*') if p.is_file() and p.suffix in ('.sqlite', '.db', '.ndjson', '.log')]
        return {p.relative_to(run).as_posix(): sha(p) for p in paths} | {"http_requests": sha(Path(info["requests"]))}

    def poll(base, accepted):
        deadline = time.monotonic() + 240
        while True:
            status, state = fetch("GET", base.removesuffix('/api/v1') + accepted['status_url'], tokens['writer'])
            assert status == 200
            if state['state'] in ('COMPLETED', 'FAILED'):
                return state
            assert time.monotonic() < deadline, 'el sondeo no terminó'
            time.sleep(0.05)

    try:
        log = (run / 'fixture.stderr.log').open('wb')
        helper = subprocess.Popen([sys.executable, '-X', 'utf8', 'tools/p025_host_fixture.py', 'source-conformance', str(run)], cwd=REPO, env=clean_env(), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW)
        line = queue.Queue()
        threading.Thread(target=lambda: line.put(helper.stdout.readline()), daemon=True).start()
        info = json.loads(line.get(timeout=30))
        package, work = Path(info['package']), Path(info['work'])
        manifest = json.loads((package / 'manifest.json').read_bytes())
        for role, name in names.items():
            enrolled = cli(data, 'enroll-principal', '--name', name, '--kind', 'OPERATOR', '--scopes', 'admin' if role == 'writer' else ('read,models:write' if role == 'other' else 'read'))
            tokens[role], principals[role] = enrolled['token'], enrolled['principal']['id']
        installation = str(uuid.uuid7())
        at = datetime.now(timezone.utc).isoformat(timespec='milliseconds').replace('+00:00', 'Z')
        with sqlite3.connect(db) as conn:
            conn.execute("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES(?,?,?,1,?,?,?,?,?,?,'UNTRUSTED',?,'INSTALLED')", (installation, at, at, json.dumps({'fixture': 'controlled initial installation, not imported qualification'}), manifest['id'], manifest['version'], manifest['family'], manifest['package_sha256'], manifest['publisher'], json.dumps(manifest)))
        support = run / 'search-support.json'
        support.write_text(json.dumps({'search_applicable': True, 'supported_filters': ['source.search.q'], 'limitations': ['owned_reference_only']}), encoding='utf-8')
        binding = cli(data, 'register-source-adapter', '--installation', installation, '--source-type', 'HTTP', '--descriptor', str(support))
        body = {'name': 'native source', 'adapter_id': binding['id'], 'base_url': info['base_url'], 'priority': 1, 'enabled': True, 'auth_mode': 'NONE', 'account_id': None, 'policy': {'vendor': {'retained': [1, True, None]}}}
        daemon, base = start(data)
        url = base + '/sources'
        before = snapshot()
        for name, token, expected in (('unauthenticated', None, 401), ('missing-scope', tokens['reader'], 403)):
            status, _ = fetch('POST', url, token, body, name)
            step(steps, name, status == expected and snapshot() == before, 'rechazo antes de mutaciones')
        status, denied = fetch('POST', url, tokens['writer'], body, 'unqualified')
        step(steps, 'unqualified-source-denied', status == 409 and denied['details']['reason'] == 'source_adapter_unqualified' and snapshot() == before, 'binding registrado no equivale a conformidad')
        forbidden = run / 'oracle/forbidden.bin'
        forbidden.parent.mkdir()
        forbidden.write_bytes(b'owned immutable oracle input outside grants')
        evidence = run / 'native-reports'
        config = {'schema_version': '1.0', 'package_dir': str(package), 'work_dir': str(work),
                  'roots': [['reference-package', str(package)], ['reference-work', str(work)], ['reference-catalog', str(work / 'catalog')]],
                  'custom': False, 'capability_profile': 'reference-capabilities.json', 'immutable_inputs': [str(REPO / 'tools/p025_host_fixture.py'), str(REPO / 'tools/p025_source_real.py')], 'timeout_seconds': 180,
                  'suite': {'recipe': 'reference.source.catalog.v1', 'parameters': {'source_id': info['config']['source_id'], 'storage_id': info['config']['storage_location_id'], 'base_url': info['base_url'], 'catalog': info['catalog'], 'expected_files': info['expected_files'], 'control': info['control'], 'requests': info['requests'], 'forbidden': str(forbidden), 'evidence_root': str(evidence)}}}
        file = run / 'profile.json'
        file.write_text(json.dumps(config, ensure_ascii=False, indent=2), encoding='utf-8')
        operation = {'operation': 'adapter.execute', 'adapter_id': manifest['id'], **{key: manifest[key] for key in ('version', 'package_sha256', 'publisher', 'permissions')}}
        approval_body = {'operation_digest': digest(operation), 'resource_ids': [f"adapter:{manifest['id']}@{manifest['version']}"], 'code_hash': manifest['package_sha256'], 'permissions': {'publisher': manifest['publisher'], 'permissions': manifest['permissions'], 'expires_at': datetime.fromtimestamp(time.time()+3600, timezone.utc).isoformat(timespec='milliseconds').replace('+00:00', 'Z'), 'secret_refs': []}, 'approved': True}
        status, approved = fetch('POST', base + '/approvals', tokens['writer'], approval_body, 'source-profile-approval')
        assert status == 201
        registered = cli(data, 'register-adapter-probe-profile', '--installation', installation, '--actor-id', principals['writer'], '--approval-id', approved['resource_id'], '--file', str(file))
        step(steps, 'source-profile-registered', bool(registered['profile_id']), 'perfil persistente y aprobación de un uso, sin VALIDATED')
        status, accepted = fetch('POST', base + f'/adapters/{installation}/probe', tokens['writer'], {'resource_id': None, 'profile_id': None}, 'source-conformance')
        assert status == 202
        state = poll(base, accepted)
        step(steps, 'native-source-conformance', state['state'] == 'COMPLETED' and state['result']['conformance']['passed'], 'worker real: nueve firmas, cinco capacidades, HTTP/archivos/ledger/cancel/crash y recuperación')
        receipt = state['result']['conformance']
        report = json.loads((evidence / receipt['run_id'] / 'source.json').read_bytes())
        step(steps, 'native-observations-and-ledger', len(report['observations']['Ok']) == 21 and report['checkpoints']['final-ledger']['ref_lock'] == report['checkpoints']['final-ledger']['ref_reservation'] == [], '21 observaciones completas, jobs terminales y recursos privados liberados')
        idle = inert()
        status, original = fetch('POST', url, tokens['writer'], body, 'enabled-source')
        step(steps, 'enabled-source-real-gate', status == 201 and original['enabled'] is True and original['row_version'] == 1 and original['base_url'] == info['base_url']+'/' and inert() == idle, 'alta HTTP con recibo vigente y relectura física, sin volver a ejecutar el worker')
        source_url = url + '/' + original['id']
        baseline = snapshot()
        status, replay = fetch('POST', url, tokens['writer'], body, 'enabled-source')
        step(steps, 'enabled-replay-no-effects', status == 201 and replay == original and snapshot() == baseline, 'mismo recibo histórico sin otra mutación')
        changed = body | {'priority': 3}
        status, current = fetch('PATCH', source_url, tokens['writer'], changed, version=1)
        step(steps, 'enabled-update-real-gate', status == 200 and current['row_version'] == 2 and current['priority'] == 3 and inert() == idle, 'edición habilitada conserva el guard hasta COMMIT')
        baseline = snapshot()
        for name, version, expected in (('missing-if-match', None, 428), ('invalid-if-match', 'invalid', 400), ('stale-if-match', 1, 409)):
            status, _ = fetch('PATCH', source_url, tokens['writer'], changed, version=version)
            step(steps, name, status == expected and snapshot() == baseline, 'CAS rechazado sin efectos')
        original_input = forbidden.read_bytes()
        forbidden.write_bytes(b'owned input changed after native conformance')
        status, error = fetch('POST', url, tokens['writer'], body | {'name': 'changed input'}, 'changed-input-create')
        step(steps, 'changed-input-create-rollback', status == 409 and error['details']['reason'] == 'conformance_context_changed' and snapshot() == baseline, 'sin nueva fuente, replay, evento, outbox ni journal')
        status, error = fetch('PATCH', source_url, tokens['writer'], changed, version=2)
        step(steps, 'changed-input-enabled-update-rollback', status == 409 and error['details']['reason'] == 'conformance_context_changed' and snapshot() == baseline, 'también se remide una edición con la misma configuración habilitada')
        status, error = fetch('POST', url, tokens['other'], body, 'other-actor-enabled')
        step(steps, 'grant-not-transferred-to-source-writer', status == 403 and snapshot() == baseline, 'models:write no transfiere la concesión P022 de otro actor')
        status, disabled_other = fetch('POST', url, tokens['other'], body | {'name': 'disabled config', 'enabled': False}, 'disabled-other')
        step(steps, 'disabled-config-despite-stale-input', status == 201 and disabled_other['enabled'] is False and inert() == idle, 'configuración deshabilitada permitida sin ejecutar ni cualificar')
        disabled = changed | {'enabled': False}
        status, current = fetch('PATCH', source_url, tokens['writer'], disabled, version=2)
        step(steps, 'disable-despite-stale-input', status == 200 and current['enabled'] is False and current['row_version'] == 3, 'desactivar no necesita un contexto físico vigente')
        baseline = snapshot()
        status, replay = fetch('POST', url, tokens['writer'], body, 'enabled-source')
        read_status, actual = fetch('GET', source_url, tokens['writer'])
        step(steps, 'historical-replay-does-not-reactivate', status == 201 and replay == original and read_status == 200 and actual == current and snapshot() == baseline, 'replay original habilitado no cambia la fila actualmente deshabilitada')
        forbidden.write_bytes(original_input)
        status, current = fetch('PATCH', source_url, tokens['writer'], changed, version=3)
        step(steps, 'restored-input-enable', status == 200 and current['enabled'] is True and current['row_version'] == 4 and inert() == idle, 'restaurar bytes medidos permite habilitar sin inventar otro PASS')
        baseline = snapshot()
        with sqlite3.connect(db) as conn:
            conn.execute("CREATE TRIGGER owned_source_outbox_fault BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'owned source API outbox fault'); END")
        status, error = fetch('PATCH', source_url, tokens['writer'], changed | {'name': 'retry after fault'}, version=4)
        step(steps, 'source-outbox-fault-full-rollback', status == 500 and snapshot() == baseline and inert() == idle, 'fallo tardío revierte fuente/evento/outbox y libera el guard')
        with sqlite3.connect(db) as conn:
            conn.execute('DROP TRIGGER owned_source_outbox_fault')
        status, current = fetch('PATCH', source_url, tokens['writer'], changed | {'name': 'retry after fault'}, version=4)
        step(steps, 'source-outbox-retry-single-commit', status == 200 and current['row_version'] == 5, 'reintento con misma revisión confirma una sola edición')
        shutdown()
        daemon, base = start(data)
        url = base + '/sources'
        status, restarted = fetch('POST', url, tokens['writer'], body | {'name': 'after restart'}, 'after-restart')
        step(steps, 'source-gate-after-daemon-restart', status == 201 and restarted['enabled'] is True and inert() == idle, 'nuevo proceso reconstruye perfil y contexto, sin otra suite ni requests al servidor fuente')
        for source in (current, restarted):
            config_body = {key: source[key] for key in body} | {'enabled': False}
            status, saved = fetch('PATCH', url + '/' + source['id'], tokens['writer'], config_body, version=source['row_version'])
            assert status == 200 and saved['enabled'] is False
        with sqlite3.connect(db) as conn:
            enabled = conn.execute('SELECT count(*) FROM model_source WHERE enabled=1').fetchone()[0]
            events = conn.execute('SELECT (SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox)').fetchone()
            row = conn.execute('SELECT state,trust,metadata FROM adapter_installation WHERE id=?', (installation,)).fetchone()
            journals = conn.execute("SELECT payload FROM operation_journal WHERE kind='adapter.conformance' AND id=?", (receipt['run_id'],)).fetchone()
        step(steps, 'final-sources-disabled-and-receipt-intact', enabled == 0 and events[0] == events[1] and row[:2] == ('VALIDATED', 'UNTRUSTED') and json.loads(row[2])['larm_conformance_v1'] == receipt and inert() == idle and package_hash(package) == manifest['package_sha256'], 'sin nuevos workers/downloads, todas las fuentes deshabilitadas y recibo/paquete originales intactos')
        journal = json.loads(journals[0])
        step(steps, 'independent-core-receipt-hashes', digest(journal['run']) == receipt['run_sha256'] and digest(journal['outcome']) == receipt['outcome_sha256'] and journal['receipt'] == receipt, 'reconstrucción JCS independiente de Run/Outcome/recibo tras CRUD y reinicio')
        shutdown()
        helper.stdin.close()
        helper.wait(timeout=30)
        step(steps, 'owned-services-closed', helper.returncode == 0 and daemon is None, 'daemon, workers y servidor de fixture propios cerrados')
        success = True
        return 0
    finally:
        if daemon is not None and daemon.poll() is None:
            try:
                shutdown()
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=15)
        if helper is not None and helper.poll() is None:
            helper.stdin.close()
            try:
                helper.wait(timeout=30)
            except subprocess.TimeoutExpired:
                helper.kill()
                helper.wait(timeout=15)
        if log is not None:
            log.close()
        for name in names.values():
            subprocess.run([str(EXE), '--data-root', str(data), 'revoke-principal', '--name', name], capture_output=True, env=clean_env(), timeout=30)
        trace_path = run / 'http-trace.json'
        encoded_trace = json.dumps({'scope': 'HTTP observations without authorization header/token; nonterminal job polls omitted', 'observations': trace}, ensure_ascii=False, indent=2)
        assert all(value not in encoded_trace for value in tokens.values()), 'own test token leaked to trace'
        trace_path.write_text(encoded_trace, encoding='utf-8')
        record = {'status': 'PASS' if success and len(steps) >= 20 and all(v['result'] == 'PASS' for v in steps) else 'FAIL', 'scope': 'real daemon/TCP/guard/local profile/Windows credentials/Core SQLite and compiled source recipe; initial installation and HTTP server owned fixtures, no P028 import/P050 product download admission', 'steps': steps, 'run': run.relative_to(REPO).as_posix(), 'http_trace': {'path': trace_path.relative_to(REPO).as_posix(), 'sha256': sha(trace_path)}, 'daemon_sha256': sha(EXE), 'source_sha256': sha(Path(__file__)), 'host': platform.platform(), 'python': platform.python_version(), 'generated_at': datetime.now(timezone.utc).isoformat()}
        (run / 'receipt.json').write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding='utf-8')
        (out / 'source-api-real.json').write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding='utf-8')


if __name__ == '__main__':
    sys.stdout.reconfigure(encoding='utf-8', errors='replace')
    raise SystemExit(main())
