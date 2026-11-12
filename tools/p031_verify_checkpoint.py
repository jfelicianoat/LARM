#!/usr/bin/env python3
"""Audita y conserva un incremento P031. No lo convierte en cierre de fase."""
import hashlib
import json
import re
import shutil
import sqlite3
from datetime import datetime, timezone
from pathlib import Path

from check_integration_credentials import scan_files, scan_paths, secret
from evidence import Evidence, REPO, cargo_ok


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load(path):
    return json.loads(path.read_bytes())


def resolve(name):
    path = (REPO / name).resolve(strict=True)
    assert path.is_relative_to(REPO) and path.is_file(), name
    return path


def rust_count(log):
    text = log.read_text(encoding='utf-8')
    results = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', text)
    assert results and all(failed == ignored == '0' for _, failed, ignored in results)
    return sum(int(passed) for passed, _, _ in results)


def main():
    out = REPO / 'docs/reports/P031_runs'
    prep = load(out / 'preparation.checks.json')
    assert prep['phase_status'] == 'IN_PROGRESS' and prep['implemented_checks_status'] == 'PASS'
    assert prep['source_before_sha256'] == prep['source_sha256'] and prep['sources_unchanged'] is True
    assert prep['mandatory_inputs_sha256'] == Evidence('P031').inputs
    selected = set()
    for name, digest in prep['source_sha256'].items():
        current, frozen = resolve(name), resolve(prep['source_snapshot'] + '/' + name)
        assert sha(current) == sha(frozen) == digest, name
        selected.add(current)
    assert len(prep['checks']) == len({c['id'] for c in prep['checks']}) == 16
    counts = {}
    for check in prep['checks']:
        log = resolve(check['log'])
        assert check['result'] == 'PASS' and check['exit_code'] == 0 and sha(log) == check['sha256'], check['id']
        selected.add(log)
        if check['command'][0] == 'cargo' and 'test' in check['command']:
            assert cargo_ok(check['exit_code'], log.read_text(encoding='utf-8'))
            counts[check['id']] = rust_count(log)
    assert counts == {'domain': 5, 'discovery': 10, 'plan': 9, 'sdk': 28, 'native-client': 8, 'discovery-persistence': 10, 'runtime-installation-persistence': 9, 'native-discovery': 6, 'workspace-transcript': 712}, counts
    gate = load(resolve(prep['quality_gate']['path']))
    assert sha(resolve(prep['quality_gate']['path'])) == prep['quality_gate']['sha256']
    assert gate['status'] == 'PASS' and len(gate['checks']) == len(gate['transcripts']) == 12
    assert all(check['result'] == 'PASS' for check in gate['checks'])
    for name, record in gate['transcripts'].items():
        log = resolve(record['path'])
        assert record['exit_code'] == 0 and sha(log) == record['sha256'], name
        selected.add(log)
    assert rust_count(resolve(gate['transcripts']['cargo_test']['path'])) == 712
    frontend = resolve(gate['transcripts']['npm_test']['path']).read_text(encoding='utf-8')
    assert re.search(r'Tests\s+50 passed', frontend)
    api_path = resolve(prep['api_receipt']['path'])
    assert sha(api_path) == prep['api_receipt']['sha256']
    api = load(api_path)
    assert api['status'] == 'PASS' and len(api['steps']) == 13 and all(s['result'] == 'PASS' for s in api['steps'])
    assert api['cleanup'] == {'principals_revoked': True, 'daemon_exit_code': 0}
    assert api['source_sha256'] == sha(REPO / 'tools/p031_versions_real.py')
    assert sha(resolve(api['http_trace']['path'])) == api['http_trace']['sha256']
    assert api['daemon_sha256'] == prep['daemon_sha256'] == sha(REPO / 'target/debug/larm-daemon.exe')
    assert load(REPO / api['run'] / 'receipt.json') == api
    assert sha(REPO / api['run'] / 'export.sqlite') == api['backup_sha256']
    selected.update(p for p in (REPO / api['run']).rglob('*') if p.is_file())
    p030_path = resolve(prep['api_regression']['path'])
    p030 = load(p030_path)
    assert sha(p030_path) == prep['api_regression']['sha256']
    assert p030['status'] == 'PASS' and len(p030['steps']) == 27 and all(s['result'] == 'PASS' for s in p030['steps'])
    assert p030['daemon_sha256'] == prep['daemon_sha256']
    assert p030['source_sha256'] == sha(REPO / 'tools/p030_api_real.py')
    assert sha(resolve(p030['http_trace']['path'])) == p030['http_trace']['sha256']
    selected.add(p030_path)
    selected.update(p for p in (REPO / p030['run']).rglob('*') if p.is_file())
    for name, digest in prep['binaries_sha256'].items():
        path = resolve(name)
        assert sha(path) == digest
        selected.add(path)
    native_path = resolve(prep['native_consumer']['path'])
    native = load(native_path)
    assert sha(native_path) == prep['native_consumer']['sha256'] and native['status'] == 'PASS'
    assert native['versions'] == 5 and native['discovery_commits'] == 2 and native['rpc_calls'] == 11
    assert native['io_drained_and_package_released'] is True
    assert native['fixture_worker_sha256'] == native['files_sha256']['fake_runtime_worker.exe']
    assert native['backup_verification']['integrity'] == 'ok'
    assert native['backup_verification']['live_rows'] == native['backup_verification']['backup_rows']
    for name, digest in native['files_sha256'].items():
        path = (native_path.parent / name).resolve(strict=True)
        assert path.is_relative_to(native_path.parent) and sha(path) == digest
    trace = [json.loads(line) for line in (native_path.parent / 'requests.jsonl').read_text(encoding='utf-8').splitlines()]
    channels = ['stable', 'prerelease', 'nightly', 'commit', 'custom']
    assert len(trace) == 11 and all(row['method'] == 'runtime.list_versions' for row in trace)
    assert [row['params']['channel'] for row in trace[1:]] == channels * 2
    assert all(row['params']['runtime_id'] == native['runtime_id'] for row in trace[1:])
    with sqlite3.connect(native_path.parent / 'inventory.sqlite') as conn:
        assert conn.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
        rows = conn.execute('SELECT runtime_id,channel,promotion,metadata FROM runtime_version').fetchall()
        assert len(rows) == 5 and sorted(row[1] for row in rows) == sorted(channels)
        assert all(row[0] == native['runtime_id'] and row[2] == 'NONE' for row in rows)
        custom = next(row for row in rows if row[1] == 'custom')
        assert json.loads(custom[3])['upstream_channel'] == {'value': 'fixture-preview/42', 'reason': 'UNKNOWN'}
        assert conn.execute("SELECT count(*) FROM operation_journal WHERE kind='runtime.discovery' AND phase='COMPLETED'").fetchone()[0] == 2
        assert all(conn.execute(f'SELECT count(*) FROM {name}').fetchone()[0] == 0 for name in ('runtime_installation','resource_lock','volume_reservation','memory_reservation'))
    selected.update(p for p in native_path.parent.rglob('*') if p.is_file())
    selected.update(resolve(name) for name in ('tools/p031_verify_checkpoint.py', 'docs/reports/P031_descubrimiento-e-instalacion-de-versiones-de-runtime.md', 'docs/implementation/STATE.md', 'docs/reports/P031_runs/preparation.checks.json', 'docs/reports/check_P031.json', 'docs/reports/P031_runs/versions-real.json', 'target/debug/larm-daemon.exe'))
    previous = REPO / 'docs/reports/P030_runs/formal-checkpoints/20261020T140509Z'
    p030 = load(previous / 'docs/reports/P030_runs/ui-real.json')
    ui_files = {name: digest for name, digest in p030['source_sha256'].items() if name.startswith('apps/desktop/')}
    assert all(sha(resolve(name)) == digest for name, digest in ui_files.items())
    stamp = REPO / 'apps/desktop/dist/ui-build.json'
    selected.add(stamp)
    for group in ('sources', 'assets'):
        for name, digest in load(stamp)[group].items():
            path = resolve('apps/desktop/' + name)
            assert sha(path) == digest
            selected.add(path)
    checkpoint = out / 'checkpoints' / datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    checkpoint.mkdir(parents=True)
    records = []
    for source in sorted(selected):
        relative = source.relative_to(REPO)
        dest = checkpoint / relative
        assert dest.is_relative_to(checkpoint)
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, dest)
        digest = sha(source)
        assert sha(dest) == digest
        records.append({'original': relative.as_posix(), 'archive': dest.relative_to(REPO).as_posix(), 'sha256': digest})
    scan = set(scan_paths(out)) | set(scan_paths(REPO / 'docs/reports/check_runs')) | selected
    needles = [secret(name).encode(enc) for name in ('ai-broker', 'agora') for enc in ('utf-8', 'utf-16-le')]
    count, size = scan_files(iter(sorted(scan)), needles, workers=8)
    record = {'phase': 'P031', 'phase_status': 'IN_PROGRESS', 'verification_status': 'PASS', 'files': records,
              'rust_tests': counts, 'frontend_tests': 50, 'api_checks': 13, 'p030_api_regression': 27, 'native_fixture_rpc_calls': 11, 'normative_inputs': len(prep['mandatory_inputs_sha256']),
              'frontend_files_unchanged_from_p030': ui_files, 'preparation_receipt_sha256': sha(out / 'preparation.checks.json'),
              'secret_scan': {'files': count, 'bytes': str(size), 'matches': 0, 'encodings': ['utf-8', 'utf-16-le']},
              'scope': 'DTO/planes puros, sesión nativa y descubrimiento transaccional (cualificación CODE_PASS sintética), admisión durable de instalación con journal/locks/reservas/outbox/idempotencia, GET jobs por actor/admin y transiciones atómicas QUEUED→RUNNING e INSTALL_PLANNED→INSTALLING, y fallo terminal desde QUEUED/RUNNING con evento y liberación de guards. El daemon aún no consume ni ejecuta el plan; worker managed, finalización exitosa, cancelación/recuperación y cierre formal pendientes.',
              'generated_at': datetime.now(timezone.utc).isoformat()}
    index = checkpoint / 'index.json'
    index.write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding='utf-8')
    summary = {k: v for k, v in record.items() if k not in ('files', 'frontend_files_unchanged_from_p030')}
    summary.update({'checkpoint': index.relative_to(REPO).as_posix(), 'checkpoint_sha256': sha(index), 'files': len(records)})
    (out / 'checkpoint-verification.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps(summary, ensure_ascii=False))


if __name__ == '__main__':
    main()
