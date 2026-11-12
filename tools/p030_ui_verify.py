#!/usr/bin/env python3
"""Audita el recibo CUA real ya ejecutado; no reproduce navegador ni WebView."""
import json
import sqlite3
from pathlib import Path

from p043_sources_real import REPO, sha


def read(db, sql, args=()):
    with sqlite3.connect(f'file:{db.as_posix()}?mode=ro', uri=True) as conn:
        return conn.execute(sql, args).fetchall()


def main():
    receipt = REPO / 'docs/reports/P030_runs/ui-real.json'
    record = json.loads(receipt.read_bytes())
    assert record['status'] == 'PASS'
    assert record['cleanup'] == {'principal_revoked': True, 'daemon_exit_code': 0}
    for name, expected in record['source_sha256'].items():
        assert sha(REPO / name) == expected, name
    # El sello de producción liga también las restantes fuentes y assets.
    root = REPO / 'apps/desktop'
    stamp = json.loads((root / 'dist/ui-build.json').read_bytes())
    for group in ('sources', 'assets'):
        for name, expected in stamp[group].items():
            assert sha(root / name) == expected, name
    run = REPO / record['run']
    assert json.loads((run / 'receipt.json').read_bytes()) == record
    assert sha(run / 'requests.json') == record['requests_sha256']
    requests = json.loads((run / 'requests.json').read_bytes())
    assert requests == record['requests']
    writes = [r for r in requests if r['method'] != 'GET']
    assert [(r['method'], r['status']) for r in writes] == [('PATCH', 200), ('POST', 201)]
    before, db = run / 'export.sqlite', run / 'data/state/larm.sqlite'
    assert sha(before) == record['backup_sha256'] and record['export_import_inventory_equal'] is True
    channels = read(before, 'SELECT channel,metadata FROM runtime_version ORDER BY channel')
    assert channels == read(db, 'SELECT channel,metadata FROM runtime_version ORDER BY channel')
    assert [c for c, _ in channels] == ['commit', 'custom', 'nightly', 'prerelease', 'stable']
    assert json.loads(dict(channels)['custom'])['upstream_channel'] == {'value': 'experimental.vendor/42', 'reason': 'UNKNOWN'}
    for table in ('runtime_version', 'runtime_installation', 'runtime_backend', 'external_host'):
        assert read(before, f'SELECT * FROM {table} ORDER BY id') == read(db, f'SELECT * FROM {table} ORDER BY id'), table
    identity, initial = read(before, 'SELECT id,row_version FROM runtime')[0]
    current, policy = read(db, 'SELECT row_version,channel_policy FROM runtime WHERE id=?', (identity,))[0]
    assert current == initial + 1 and json.loads(policy) == {'allowed_channels': ['stable', 'prerelease', 'commit']}
    assert read(db, 'SELECT name,row_version FROM runtime WHERE id<>?', (identity,)) == [('Runtime QA real', 1)]
    shown = [r['response']['items'] for r in requests if r['method'] == 'GET' and r['status'] == 200 and len(r['response']['items']) == 5]
    assert shown and all({v['channel'] for v in items} == {c for c, _ in channels} for items in shown)
    assert read(db, 'SELECT resource_version FROM event WHERE resource_id=? ORDER BY event_id', (identity,)) == [(1,), (2,), (3,), (4,)]
    assert read(db, 'SELECT count(*) FROM event_outbox o JOIN event e ON o.event_id=e.event_id WHERE e.resource_id=?', (identity,)) == [(4,)]
    assert read(db, 'SELECT state,trust FROM adapter_installation') == [('INSTALLED', 'UNTRUSTED')]
    assert read(db, 'SELECT status FROM runtime_backend') == [('UNKNOWN',)]
    assert read(db, 'PRAGMA integrity_check') == [('ok',)]
    print('PASS: recibo CUA/producción vigente, PATCH200/POST201, backup/restore conserva cinco canales y UNKNOWN, SQLite/outbox 1-2-3-4; limpieza confirmada. No repite UI ni acredita WebView o inferencia.')


if __name__ == '__main__':
    main()
