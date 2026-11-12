#!/usr/bin/env python3
"""P031: lecturas publicadas por daemon/TCP/SQLite reales. Inventario fixture
restaurado mediante P004; no instala ni cualifica un engine.
"""
import json
import platform
import sqlite3
import subprocess
import uuid
from datetime import datetime, timezone
from urllib.parse import quote

from p023_real import clean_env, start, stop
from p043_sources_real import EXE, REPO, cli, fetch, sha, validator


def main():
    out = REPO / 'docs/reports/P031_runs'
    run = out / 'versions-real' / uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    previous = json.loads((REPO / 'docs/reports/P030_runs/api-real.json').read_bytes())
    assert previous['status'] == 'PASS' and previous['daemon_sha256'] == sha(EXE), 'Ejecutar P030 API real con este daemon antes del recorrido P031'
    source_data = REPO / previous['run'] / 'data'
    backup = run / 'export.sqlite'
    cli(source_data, 'db-backup', '--out', backup)
    data = run / 'data'
    cli(data, 'db-restore', '--from', backup)
    db = data / 'state/larm.sqlite'
    names, tokens, steps, trace, daemon = [], {}, [], [], None
    passed = False

    def step(name, valid):
        steps.append({'id': name, 'result': 'PASS' if valid else 'FAIL'})
        print(name, steps[-1]['result'], flush=True)
        assert valid, name

    def snapshot():
        with sqlite3.connect(db) as conn:
            values = {name: conn.execute(f'SELECT * FROM {name} ORDER BY 1').fetchall() for name in ('runtime', 'runtime_adapter', 'runtime_version', 'runtime_installation', 'runtime_backend', 'external_host', 'event', 'event_outbox', 'idempotency', 'volume_reservation', 'memory_reservation', 'resource_lock')}
            # El refresco de hardware del arranque tiene su journal independiente.
            # Se compara todo el journal de runtimes, sin atribuir el sondeo a GET.
            values['operation_journal'] = conn.execute("SELECT * FROM operation_journal WHERE kind LIKE 'runtime.%' ORDER BY id").fetchall()
            return values

    def call(path, role='reader'):
        status, body = fetch('GET', base + path, tokens.get(role))
        trace.append({'path': path, 'role': role, 'status': status, 'response': body})
        return status, body

    try:
        for role, scopes in [('reader', 'read'), ('other', 'models:write')]:
            name = f'p031-versions-{run.name}-{role}'
            names.append(name)
            tokens[role] = cli(data, 'enroll-principal', '--name', name, '--kind', 'OPERATOR', '--scopes', scopes)['token']
        with sqlite3.connect(db) as conn:
            runtime = conn.execute('SELECT id FROM runtime').fetchone()[0]
        daemon, base = start(data)
        before = snapshot()
        step('auth-scopes-before-reads', call('/runtime-versions', 'none')[0] == 401 and call('/runtime-versions', 'other')[0] == 403)
        status, first = call('/runtime-versions?limit=2')
        validator('runtime_versionList').validate(first)
        step('first-published-page', status == 200 and len(first['items']) == 2 and first['next_cursor'] is not None)
        items, cursor = list(first['items']), first['next_cursor']
        seen = set()
        while cursor:
            assert cursor not in seen
            seen.add(cursor)
            status, page = call('/runtime-versions?limit=2&cursor=' + quote(cursor, safe=''))
            validator('runtime_versionList').validate(page)
            assert status == 200
            items += page['items']
            cursor = page['next_cursor']
        step('all-five-channel-pages', len(items) == len({v['id'] for v in items}) == 5 and {v['channel'] for v in items} == {'stable', 'prerelease', 'nightly', 'commit', 'custom'})
        for version in items:
            status, value = call('/runtime-versions/' + version['id'])
            validator('runtime_version').validate(value)
            assert status == 200 and value == version
        step('individual-version-contract', True)
        custom = next(v for v in items if v['channel'] == 'custom')
        step('unknown-original-preserved', custom['metadata']['upstream_channel'] == {'value': 'experimental.vendor/42', 'reason': 'UNKNOWN'})
        status, filtered = call('/runtime-versions?runtime_id=' + runtime)
        step('runtime-filter-keeps-inventory', status == 200 and filtered['items'] == items)
        status, filtered = call('/runtime-versions?q=b7142-local')
        step('version-query', status == 200 and filtered['items'] == items and call('/runtime-versions?q=__absent_fixture__')[1]['items'] == [])
        encoded = quote(first['next_cursor'], safe='')
        step('cursor-bound-to-route-and-filters', call(f'/runtimes/{runtime}/versions?limit=2&cursor={encoded}')[0] == 400 and call('/runtime-versions?q=changed&cursor=' + encoded)[0] == 400 and call('/runtime-versions?runtime_id=' + str(uuid.uuid7()) + '&cursor=' + encoded)[0] == 400)
        unknown = str(uuid.uuid7())
        step('unknown-id-and-empty-runtime-filter', call('/runtime-versions/' + unknown)[0] == 404 and call('/runtime-versions?runtime_id=' + unknown)[1]['items'] == [])
        step('invalid-ids-and-closed-filters', all(call(path)[0] == 400 for path in ('/runtime-versions/bad', '/runtime-versions?runtime_id=bad', '/runtime-versions?channel=stable', '/runtime-versions?state=VERIFIED', '/runtime-versions?limit=0', '/runtime-versions?cursor=not-a-cursor')))
        step('read-only-no-side-effects', snapshot() == before)
        stop(daemon)
        step('graceful-stop', daemon.returncode == 0)
        daemon = None
        daemon, base = start(data)
        step('restart-preserves-version-record', call('/runtime-versions/' + items[0]['id']) == (200, items[0]) and snapshot() == before)
        passed = True
    finally:
        if daemon and daemon.poll() is None:
            stop(daemon)
        revoked = []
        for name in names:
            process = subprocess.run([str(EXE), '--data-root', str(data), 'revoke-principal', '--name', name], capture_output=True, env=clean_env(), timeout=30)
            revoked.append(process.returncode == 0)
        passed = passed and all(revoked) and daemon is not None and daemon.returncode == 0
        encoded = json.dumps(trace, ensure_ascii=False)
        assert all(token not in encoded for token in tokens.values())
        trace_path = run / 'http-trace.json'
        trace_path.write_text(encoded, encoding='utf-8')
        record = {'status': 'PASS' if passed else 'FAIL', 'scope': __doc__, 'steps': steps,
                  'run': run.relative_to(REPO).as_posix(), 'source_sha256': sha(REPO / 'tools/p031_versions_real.py'),
                  'daemon_sha256': sha(EXE), 'backup_sha256': sha(backup),
                  'http_trace': {'path': trace_path.relative_to(REPO).as_posix(), 'sha256': sha(trace_path)},
                  'cleanup': {'principals_revoked': all(revoked), 'daemon_exit_code': daemon.returncode if daemon else None},
                  'host': platform.platform(), 'generated_at': datetime.now(timezone.utc).isoformat()}
        (run / 'receipt.json').write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding='utf-8')
        (out / 'versions-real.json').write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding='utf-8')
    return int(not passed)


if __name__ == '__main__':
    raise SystemExit(main())
