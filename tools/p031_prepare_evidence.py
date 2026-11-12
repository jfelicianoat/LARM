#!/usr/bin/env python3
"""Checkpoint incremental P031. No cierra la fase ni sustituye PromptEvidence."""
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

from evidence import Evidence, REPO, cargo_ok
from check_integration_credentials import scan_files, scan_paths, secret


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def sources():
    files = {REPO / name for name in ('Cargo.toml', 'Cargo.lock', 'environment.lock.json', 'docs/api/openapi.json', 'docs/contracts/adapter-methods.json', 'docs/contracts/api-operations.json', 'docs/contracts/schemas/larm.schema.json', 'docs/contracts/schemas/domain.schema.json', 'docs/adapters/method-schemas/index.json', 'docs/spec/PACKAGE_COPY_HASHES.json', 'docs/contracts/runtime-profiles.json', 'tools/p026_schemas.py', 'tools/p031_prepare_evidence.py', 'tools/p031_verify_checkpoint.py', 'tools/p031_versions_real.py', 'tools/evidence.py', 'tools/check.ps1', 'tools/check_integration_credentials.py')}
    method_schemas = REPO / 'docs/adapters/method-schemas'
    files.update(path for path in method_schemas.rglob('*') if path.is_file())
    files.update(REPO / name for name in ('tools/p030_api_real.py', 'tools/p023_real.py', 'tools/p043_sources_real.py', 'tools/validate_evidence.py', 'tools/package_copy.py', 'tools/check_architecture.ps1', 'tools/verify_p000.py', 'apps/daemon/Cargo.toml', 'apps/daemon/src/main.rs', 'apps/daemon/src/serve.rs', 'apps/daemon/src/admin.rs'))
    files.update(REPO / name for name in ('apps/desktop/package.json', 'apps/desktop/vite.config.ts', 'apps/desktop/vitest.config.ts', 'apps/desktop/tsconfig.json', 'apps/desktop/tsconfig.app.json', 'apps/desktop/eslint.config.js', 'apps/desktop/build-ui-stamp.mjs', 'apps/desktop/index.html'))
    desktop_src = REPO / 'apps/desktop/src'
    files.update(path for path in desktop_src.rglob('*') if path.is_file() and path.suffix.lower() in {'.ts', '.tsx', '.css'})
    for folder in ('crates', 'apps/daemon', 'apps/desktop/src-tauri/src', 'adapters', 'tests/fixtures/adapters'):
        root = REPO / folder
        files.update(root.rglob('*.rs'))
        files.update(root.rglob('Cargo.toml'))
    files.add(REPO / 'apps/desktop/src-tauri/Cargo.toml')
    return {p.relative_to(REPO).as_posix(): sha(p) for p in sorted(files)}


def main():
    commands = [
        ('build-daemon', ['cargo', 'build', '-p', 'larm-daemon']),
        ('build-sdk-fixtures', ['cargo', 'build', '-p', 'larm-adapter-sdk', '--bins', '--examples']),
        ('build-native-fixtures', ['cargo', 'build', '-p', 'larm-adapter-host', '--examples']),
        ('domain', ['cargo', 'test', '-p', 'larm-domain', '--test', 'runtime_install']),
        ('discovery', ['cargo', 'test', '-p', 'larm-runtime', '--test', 'version_discovery']),
        ('plan', ['cargo', 'test', '-p', 'larm-runtime', '--test', 'install']),
        ('sdk', ['cargo', 'test', '-p', 'larm-adapter-sdk', '--lib']),
        ('native-client', ['cargo', 'test', '-p', 'larm-adapter-host', '--test', 'runtime']),
        ('discovery-persistence', ['cargo', 'test', '-p', 'larm-db', '--test', 'runtime_execution']),
        ('runtime-installation-persistence', ['cargo', 'test', '-p', 'larm-db', '--test', 'runtime_installation']),
        ('native-discovery', ['cargo', 'test', '-p', 'larm-runtime', '--test', 'discovery_service']),
        ('general-gate', ['powershell', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'tools/check.ps1', '-Prompt', 'P031']),
        ('p030-api-regression', [sys.executable, '-X', 'utf8', 'tools/p030_api_real.py']),
        ('versions-real', [sys.executable, '-X', 'utf8', 'tools/p031_versions_real.py']),
        ('evidence-shape', [sys.executable, '-X', 'utf8', 'tools/validate_evidence.py']),
    ]
    if '--list' in sys.argv:
        print(json.dumps(commands, ensure_ascii=False, indent=2))
        return 0
    inputs = Evidence('P031').inputs
    started = datetime.now(timezone.utc)
    run = REPO / 'docs/reports/P031_runs/preparation' / started.strftime('%Y%m%dT%H%M%S%fZ')
    run.mkdir(parents=True)
    test_tmp = REPO / 'target/larm-p031-tmp'
    test_tmp.mkdir(parents=True, exist_ok=True)
    before = sources()
    for name in before:
        dest = run / 'sources' / name
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(REPO / name, dest)
    checks = []
    for name, command in commands:
        environment = {**os.environ, 'PYTHONIOENCODING': 'utf-8', 'PYTHONUTF8': '1',
                       'TEMP': str(test_tmp), 'TMP': str(test_tmp), 'TMPDIR': str(test_tmp)}
        environment.pop('LARM_P031_NATIVE_EVIDENCE_DIR', None)
        if name == 'native-discovery':
            environment['LARM_P031_NATIVE_EVIDENCE_DIR'] = str(run / 'native-consumer')
        try:
            proc = subprocess.run(command, cwd=REPO, capture_output=True, timeout=1200,
                                  env=environment)
            code, raw = proc.returncode, proc.stdout + b'\n' + proc.stderr
        except subprocess.TimeoutExpired as exc:
            code, raw = -1, b'TIMEOUT\n' + (exc.stdout or b'') + (exc.stderr or b'')
        log = run / f'{name}.log'
        log.write_bytes(raw)
        passed = cargo_ok(code, raw.decode('utf-8', 'replace')) if command[0] == 'cargo' and 'test' in command else code == 0
        checks.append({'id': name, 'command': command, 'exit_code': code, 'result': 'PASS' if passed else 'FAIL',
                       'log': log.relative_to(REPO).as_posix(), 'sha256': sha(log)})
        print(name, checks[-1]['result'], flush=True)
    after = sources()
    gate = json.loads((REPO / 'docs/reports/check_P031.json').read_bytes())
    workspace = gate.get('transcripts', {}).get('cargo_test')
    if workspace is None:
        workspace = {'exit_code': -2}
        raw = b'MISSING WORKSPACE TRANSCRIPT: general gate did not record cargo_test\n'
    else:
        raw = (REPO / workspace['path']).read_bytes()
        assert sha(REPO / workspace['path']) == workspace['sha256']
    (run / 'workspace-transcript.log').write_bytes(raw)
    workspace_passed = cargo_ok(workspace['exit_code'], raw.decode('utf-8', 'replace'))
    checks.append({'id': 'workspace-transcript', 'command': ['cargo', 'test', '--workspace'], 'recorded_by': 'general-gate',
                   'exit_code': workspace['exit_code'], 'result': 'PASS' if workspace_passed else 'FAIL',
                   'log': (run / 'workspace-transcript.log').relative_to(REPO).as_posix(), 'sha256': sha(run / 'workspace-transcript.log')})
    paths = {REPO / p for p in after} | set(run.glob('*.log'))
    api_receipt = json.loads((REPO / 'docs/reports/P031_runs/versions-real.json').read_bytes())
    p030_path = REPO / 'docs/reports/P030_runs/api-real.json'
    p030 = json.loads(p030_path.read_bytes())
    native_path = run / 'native-consumer/receipt.json'
    native = json.loads(native_path.read_bytes()) if native_path.exists() else {'status': 'MISSING'}
    paths.update(scan_paths(REPO / api_receipt['run']))
    paths.update(scan_paths(REPO / p030['run']))
    paths.update(scan_paths(run / 'native-consumer'))
    paths.update(REPO / value['path'] for value in gate.get('transcripts', {}).values())
    paths.update(p for p in (REPO / 'docs/reports').glob('P031*.md'))
    secret_scan = {'status': 'PASS', 'files': 0, 'bytes': '0', 'matches': 0,
                   'encodings': ['utf-8', 'utf-16-le']}
    try:
        needles = [secret(name).encode(enc) for name in ('ai-broker', 'agora') for enc in ('utf-8', 'utf-16-le')]
        count, size = scan_files(iter(sorted(paths)), needles, workers=8)
        secret_scan.update({'files': count, 'bytes': str(size)})
    except RuntimeError as exc:
        secret_scan.update({'status': 'BLOCKED_REAL', 'reason': str(exc), 'files': None,
                            'bytes': None, 'matches': None})
    daemon_after = sha(REPO / 'target/debug/larm-daemon.exe')
    binaries = {name: sha(REPO / name) for name in ('target/debug/larm-daemon.exe', 'target/debug/examples/fake_runtime_worker.exe', 'target/debug/examples/fake_runtime.exe')}
    passed = (secret_scan['status'] == 'PASS' and before == after and daemon_after == api_receipt['daemon_sha256'] == p030['daemon_sha256']
              and native['status'] == 'PASS'
              and all(c['result'] == 'PASS' for c in checks) and gate['status'] == api_receipt['status'] == p030['status'] == 'PASS')
    checks_pass = all(c['result'] == 'PASS' for c in checks) and gate['status'] == api_receipt['status'] == p030['status'] == 'PASS'
    implementation_status = 'PASS' if passed else ('BLOCKED_REAL' if checks_pass and native['status'] == 'PASS' and secret_scan['status'] == 'BLOCKED_REAL' else 'FAIL')
    record = {'phase': 'P031', 'phase_status': 'IN_PROGRESS', 'implemented_checks_status': implementation_status,
              'scope': 'DTO/planes puros; descubrimiento local CLI con sesión P021/P022/P026 y persistencia atómica; admisión durable de runtime.install con catálogo versionado, journal, locks exclusivos, reservas, evento/outbox e idempotencia; GET jobs limitado a actor/admin; consumidor durable con claim CAS, ejecución supervisada, reconciliación por ID/hash tras reinicio y cierre terminal sólo después de drenar el worker; worker nativo/SQLite y GET daemon reales. Faltan cualificación real P099, consumo atómico de aprobación, cancelación segura, instalación managed y escenarios Windows de producto.',
              'checks': checks, 'sources_unchanged': before == after, 'source_before_sha256': before, 'source_sha256': after,
              'source_snapshot': (run / 'sources').relative_to(REPO).as_posix(), 'mandatory_inputs_sha256': inputs,
              'quality_gate': {'path': 'docs/reports/check_P031.json', 'sha256': sha(REPO / 'docs/reports/check_P031.json')},
              'api_receipt': {'path': 'docs/reports/P031_runs/versions-real.json', 'sha256': sha(REPO / 'docs/reports/P031_runs/versions-real.json')},
              'api_regression': {'path': p030_path.relative_to(REPO).as_posix(), 'sha256': sha(p030_path)},
              'native_consumer': {'path': native_path.relative_to(REPO).as_posix(), 'sha256': sha(native_path)} if native_path.exists() else None,
              'binaries_sha256': binaries,
              'daemon_sha256': daemon_after,
              'secret_scan': secret_scan,
              'host': platform.platform(), 'python': platform.python_version(), 'started_at': started.isoformat(),
              'generated_at': datetime.now(timezone.utc).isoformat(),
              'pending': ['Profile lock P099 y clasificación tipada para selección automática; la operación HTTP de discovery aún no está publicada en api-operations.json', 'Consumo de aprobación, recuperación/cancelación y limpieza del consumidor del job de instalación', 'Mecanismos ausentes/UNKNOWN, fake managed real y limpieza sólo de archivos propios', 'Escenarios Windows de instalación y runner formal -Real', 'Mantener dependencias P030/P021/P019 sin REAL_PASS']}
    (run / 'receipt.json').write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding='utf-8')
    (REPO / 'docs/reports/P031_runs/preparation.checks.json').write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding='utf-8')
    print('P031 incremental', record['implemented_checks_status'], '; fase IN_PROGRESS, sin cierre formal', flush=True)
    return int(not passed)


if __name__ == '__main__':
    raise SystemExit(main())
