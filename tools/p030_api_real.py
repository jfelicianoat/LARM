#!/usr/bin/env python3
"""P030: daemon/TCP/credenciales Windows/SQLite reales. Instalación y versiones
iniciales son fixtures de inventario; no instala, ejecuta ni cualifica un engine.
"""
import copy
import hashlib
import json
import platform
import sqlite3
import subprocess
import sys
import uuid
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import quote

from p023_real import start, stop, clean_env
from p043_sources_real import REPO, EXE, cli, fetch, validator


def main():
    out = REPO / 'docs/reports/P030_runs'
    run = out / 'api-real' / uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    data, daemon = run / 'data', None
    db = data / 'state/larm.sqlite'
    names, tokens, steps, trace = [], {}, [], []
    passed = False
    stamp = datetime.now(timezone.utc).isoformat(timespec='milliseconds').replace('+00:00', 'Z')

    def step(name, valid):
        steps.append({'step': name, 'result': 'PASS' if valid else 'FAIL'})
        print(f"[{steps[-1]['result']}] {name}", flush=True)
        assert valid, name

    def snapshot():
        with sqlite3.connect(db) as conn:
            tables = ('runtime', 'runtime_version', 'runtime_adapter', 'runtime_installation', 'runtime_backend',
                      'external_host', 'operation_journal', 'event', 'idempotency', 'volume_reservation',
                      'memory_reservation')
            state = {name: conn.execute(f'SELECT * FROM {name} ORDER BY 1').fetchall() for name in tables if name != 'operation_journal'}
            # El refresh de hardware puede avanzar su journal mientras corre la
            # API; esta prueba solo verifica el journal de operaciones runtime.
            state['operation_journal'] = conn.execute(
                "SELECT * FROM operation_journal WHERE kind LIKE 'runtime.%' ORDER BY id"
            ).fetchall()
            # La entrega de eventos existentes es asíncrona; compara la identidad
            # persistida del outbox, no el delivered_at que cambia en segundo plano.
            state['event_outbox'] = conn.execute(
                'SELECT event_id, event_record_id FROM event_outbox ORDER BY event_id'
            ).fetchall()
            return state

    def call(method, path, role='writer', body=None, key=None, version=None):
        status, value = fetch(method, base + path, tokens.get(role), body, key, version)
        trace.append({'method': method, 'path': path, 'role': role, 'status': status, 'response': value})
        return status, value

    try:
        info = cli(data, 'db-info')
        expected_migrations = sorted(path.stem for path in (REPO / 'migrations').glob('*.sql'))
        step('migrations-idempotent', info['migrated_now'] == expected_migrations and not cli(data, 'db-info')['migrated_now'])
        installation = str(uuid.uuid7())
        manifest = {'id': 'fixture.p030.runtime', 'version': '1.0.0', 'family': 'runtime', 'capabilities': ['vendor.future']}
        with sqlite3.connect(db) as conn:
            conn.execute("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES(?,?,?,1,'{\"fixture_only\":true}',?,'1.0.0','runtime',?,'fixture','UNTRUSTED',?,'INSTALLED')",
                         (installation, stamp, stamp, manifest['id'], 'a' * 64, json.dumps(manifest)))
        binding = cli(data, 'register-runtime-adapter', '--installation', installation, '--runtime-family', 'fixture.engine')
        before = snapshot()
        step('binding-cli-repeat-no-qualification', binding == cli(data, 'register-runtime-adapter', '--installation', installation, '--runtime-family', 'fixture.engine') and snapshot() == before)
        for role, scopes in (('writer', 'read,runtimes:write'), ('reader', 'read'), ('other', 'read,models:write')):
            name = f'p030-{run.name}-{role}'
            names.append(name)
            tokens[role] = cli(data, 'enroll-principal', '--name', name, '--kind', 'OPERATOR', '--scopes', scopes)['token']
        daemon, base = start(data)
        status, catalog = call('GET', '/adapters?limit=200', 'reader')
        public = next(item for item in catalog['items'] if item['id'] == installation)
        step('binding-public-without-manual-id', status == 200 and public['metadata']['runtime_bindings'] == [{'id': binding['id'], 'runtime_family': 'fixture.engine', 'capabilities': ['vendor.future']}])
        body = {'name': 'Runtime local de prueba', 'adapter_id': binding['id'], 'capabilities': ['vendor.future'], 'default_version_id': None, 'channel_policy': {'allowed_channels': ['stable']}}
        before = snapshot()
        step('auth-and-runtime-scope', [call('POST', '/runtimes', role, body, 'denied')[0] for role in ('none', 'reader', 'other')] == [401, 403, 403] and snapshot() == before)
        invalid = []
        for field in body:
            value = copy.deepcopy(body)
            value.pop(field)
            invalid.append(value)
        invalid += [body | {'extra': True}, body | {'channel_policy': {'allowed_channels': []}}, body | {'channel_policy': {'allowed_channels': ['nightly', 'nightly']}}, body | {'channel_policy': {'allowed_channels': ['experimental']}}]
        step('closed-create-and-policy-schema', all(call('POST', '/runtimes', body=value, key=f'invalid-{i}')[0] == 400 for i, value in enumerate(invalid)) and call('POST', '/runtimes', body=body)[0] == 400 and snapshot() == before)
        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(lambda _: call('POST', '/runtimes', body=body, key='one-create'), range(2)))
        step('concurrent-create-original-replay', all(status == 201 for status, _ in results) and results[0][1] == results[1][1])
        runtime = results[0][1]
        validator('runtime').validate(runtime)
        identity = runtime['id']
        path = f'/runtimes/{identity}'
        step('read-runtime-contract', call('GET', path, 'reader') == (200, runtime))
        before = snapshot()
        step('same-key-other-body-conflict', call('POST', '/runtimes', body=body | {'name': 'otro'}, key='one-create')[0] == 409 and snapshot() == before)
        versions = []
        with sqlite3.connect(db) as conn:
            conn.execute('PRAGMA foreign_keys=ON')
            for channel in ('stable', 'prerelease', 'nightly', 'commit', 'custom'):
                version_id = str(uuid.uuid7())
                metadata = {'fixture_only': True}
                if channel == 'custom':
                    metadata['upstream_channel'] = {'value': 'experimental.vendor/42', 'reason': 'UNKNOWN'}
                conn.execute("INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion) VALUES(?,?,?,1,?,?,? ,?,'NONE')",
                             (version_id, stamp, stamp, json.dumps(metadata), identity, 'b7142-local', channel))
                versions.append(version_id)
        status, page = call('GET', path + '/versions', 'reader')
        validator('runtime_versionList').validate(page)
        step('five-channels-same-upstream-build', status == 200 and {v['channel'] for v in page['items']} == {'stable', 'prerelease', 'nightly', 'commit', 'custom'} and all(v['version_string'] == 'b7142-local' for v in page['items']))
        custom = next(v for v in page['items'] if v['channel'] == 'custom')
        step('unknown-original-preserved', custom['metadata']['upstream_channel'] == {'value': 'experimental.vendor/42', 'reason': 'UNKNOWN'} and custom['promotion'] == 'NONE')
        status, first = call('GET', path + '/versions?limit=2', 'reader')
        status2, second = call('GET', path + '/versions?cursor=' + quote(first['next_cursor']), 'reader')
        step('signed-pagination', status == status2 == 200 and len(first['items']) == 2 and len(second['items']) == 3 and not second['next_cursor'] and len({v['id'] for v in first['items'] + second['items']}) == 5)
        step('cursor-filter-and-route-binding', call('GET', path + '/versions?q=other&cursor=' + quote(first['next_cursor']), 'reader')[0] == 400 and call('GET', '/runtimes?cursor=' + quote(first['next_cursor']), 'reader')[0] == 400)
        policy_path = path + '/channel-policy'
        policy = {'allowed_channels': ['prerelease', 'commit']}
        before = snapshot()
        step('policy-auth-if-match-invalid-stale', [call('PATCH', policy_path, 'reader', policy, version=1)[0], call('PATCH', policy_path, body=policy)[0], call('PATCH', policy_path, body=policy, version='invalid')[0], call('PATCH', policy_path, body=policy, version=9)[0]] == [403, 428, 400, 409] and snapshot() == before)
        for invalid_policy in ({'allowed_channels': []}, {'allowed_channels': ['custom', 'custom']}, {'allowed_channels': ['unknown']}, policy | {'extra': True}):
            assert call('PATCH', policy_path, body=invalid_policy, version=1)[0] == 400
        step('invalid-policy-no-effects', snapshot() == before)
        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(lambda _: call('PATCH', policy_path, body=policy, version=1), range(2)))
        step('policy-concurrent-cas', sorted(x[0] for x in results) == [200, 409])
        runtime = next(value for status, value in results if status == 200)
        step('policy-persists-with-inventory-visible', runtime['row_version'] == 2 and runtime['channel_policy'] == policy and len(call('GET', path + '/versions', 'reader')[1]['items']) == 5)
        for behavior in ("ABORT, 'injected'", 'IGNORE'):
            before = snapshot()
            with sqlite3.connect(db) as conn:
                conn.execute(f'CREATE TRIGGER p030_outbox_fault BEFORE INSERT ON event_outbox BEGIN SELECT RAISE({behavior}); END')
            status, _ = call('PATCH', policy_path, body={'allowed_channels': ['nightly']}, version=2)
            after = snapshot()
            if after != before:
                print('[DIFF] rollback snapshot tables: ' + ','.join(name for name in before if before[name] != after[name]), flush=True)
            step('outbox-rollback-' + behavior.split(',')[0].lower(), status == 500 and after == before)
            with sqlite3.connect(db) as conn:
                conn.execute('DROP TRIGGER p030_outbox_fault')
        status, runtime = call('PATCH', policy_path, body={'allowed_channels': ['nightly']}, version=2)
        step('policy-retry-once-after-fault', status == 200 and runtime['row_version'] == 3)
        host, installed, backend = (str(uuid.uuid7()) for _ in range(3))
        with sqlite3.connect(db) as conn:
            conn.execute('PRAGMA foreign_keys=ON')
            conn.execute("INSERT INTO external_host(registration_mode,id,created_at,updated_at,row_version,metadata,name,base_url,host_type,state,ownership) VALUES('ADOPTED',?,?,?,1,'{}','fixture host','http://127.0.0.1:23239','vendor.future.host','UNKNOWN','EXTERNAL')", (host, stamp, stamp))
            conn.execute("INSERT INTO runtime_installation(registration_mode,stage,id,created_at,updated_at,row_version,metadata,runtime_version_id,strategy,external_host_id,state,ownership) VALUES('ADOPTED','DISCOVERED',?,?,?,1,'{}',?,'EXTERNAL_ADOPTION',?,'DISCOVERED','EXTERNAL')", (installed, stamp, stamp, versions[0], host))
            conn.execute("INSERT INTO runtime_backend(limits,id,created_at,updated_at,row_version,metadata,runtime_installation_id,backend_type,status,evidence_refs) VALUES(?,?,?, ?,1,'{}',?,'vendor.future.provider/v42','UNKNOWN','[]')", (json.dumps({'max_instances': 1, 'multi_model_support': False, 'parallel_requests': 1, 'gpu_sharing': False}), backend, stamp, stamp, installed))
        for route, row_id, schema in (('runtime-installations', installed, 'runtime_installation'), ('runtime-backends', backend, 'runtime_backend'), ('external-hosts', host, 'external_host')):
            status, value = call('GET', f'/{route}/{row_id}', 'reader')
            validator(schema).validate(value)
            status2, page = call('GET', f'/{route}', 'reader')
            validator(schema + 'List').validate(page)
            step('read-and-page-' + route, status == status2 == 200 and page['items'] == [value])
        step('unknown-backend-not-promoted', call('GET', f'/runtime-backends/{backend}', 'reader')[1]['status'] == 'UNKNOWN')
        step('closed-filters-and-ids', all(call('GET', p, 'reader')[0] == 400 for p in ('/runtimes?other=x', '/runtime-backends?state=VERIFIED', '/runtime-installations?runtime_id=bad', '/external-hosts?state=AVAILABLE', '/runtimes/bad')))
        stop(daemon)
        step('graceful-daemon-stop', daemon.returncode == 0)
        daemon = None
        daemon, base = start(data)
        step('restart-policy-and-five-channels', call('GET', path, 'reader') == (200, runtime) and len(call('GET', path + '/versions', 'reader')[1]['items']) == 5)
        with sqlite3.connect(db) as conn:
            state = conn.execute('SELECT state,trust FROM adapter_installation WHERE id=?', (installation,)).fetchone()
            events = conn.execute("SELECT resource_version,json_extract(payload,'$.action') FROM event WHERE resource_id=? ORDER BY event_id", (identity,)).fetchall()
            outbox = conn.execute("SELECT count(*) FROM event_outbox o JOIN event e ON e.event_id=o.event_id WHERE e.resource_id=?", (identity,)).fetchone()[0]
        step('ordered-events-and-no-execution-authority', state == ('INSTALLED', 'UNTRUSTED') and events == [(1, 'registered'), (2, 'channel_policy_changed'), (3, 'channel_policy_changed')] and outbox == 3)
        passed = True
        return 0
    finally:
        if daemon is not None and daemon.poll() is None:
            stop(daemon)
        if db.exists():
            with sqlite3.connect(db) as conn:
                conn.execute('DROP TRIGGER IF EXISTS p030_outbox_fault')
        for name in names:
            process = subprocess.run([str(EXE), '--data-root', str(data), 'revoke-principal', '--name', name], capture_output=True, env=clean_env(), timeout=30)
            assert process.returncode == 0, 'own principal cleanup failed'
        encoded = json.dumps({'observations': trace}, ensure_ascii=False)
        assert all(token not in encoded for token in tokens.values())
        trace_path = run / 'http-trace.json'
        trace_path.write_text(encoded, encoding='utf-8')
        record = {'status': 'PASS' if passed else 'FAIL', 'scope': __doc__, 'steps': steps, 'run': run.relative_to(REPO).as_posix(),
                  'http_trace': {'path': trace_path.relative_to(REPO).as_posix(), 'sha256': hashlib.sha256(trace_path.read_bytes()).hexdigest()},
                  'source_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(), 'daemon_sha256': hashlib.sha256(EXE.read_bytes()).hexdigest(),
                  'host': platform.platform(), 'generated_at': datetime.now(timezone.utc).isoformat()}
        (run / 'steps.json').write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding='utf-8')
        (out / 'api-real.json').write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding='utf-8')


if __name__ == '__main__':
    raise SystemExit(main())
