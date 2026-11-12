#!/usr/bin/env python3
"""Checkpoint verificable P030 en desarrollo. No sustituye el cierre PromptEvidence."""
import hashlib
import json
import platform
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

from evidence import Evidence
from check_integration_credentials import secret, scan_files, scan_paths

REPO = Path(__file__).resolve().parent.parent
OUT = REPO / 'docs/reports/P030_runs'


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def sources():
    names = ['Cargo.toml', 'Cargo.lock', 'environment.lock.json', 'docs/api/openapi.json',
             'migrations/0003_runtime_channel_identity.sql', 'tools/p030_api_real.py',
             'tools/p030_prepare_evidence.py', 'tools/p004_real.py', 'tools/p004_evidence.py',
             'tools/p024_selection_real.py', 'tools/check.ps1', 'tools/check_integration_credentials.py']
    for crate, files in {
        'larm-domain': ['src/lib.rs', 'src/runtime.rs', 'src/runtime_ports.rs', 'tests/runtime.rs'],
        'larm-db': ['src/lib.rs', 'src/runtimes.rs', 'src/events.rs', 'src/migrate.rs', 'tests/runtimes.rs', 'tests/integration.rs', 'tests/adapter_selection.rs'],
        'larm-runtime': ['src/lib.rs', 'src/catalog.rs'],
        'larm-api': ['src/lib.rs', 'src/runtimes.rs', 'src/openapi.rs'],
    }.items():
        names += [f'crates/{crate}/{name}' for name in files + ['Cargo.toml']]
    names += ['apps/daemon/' + name for name in ('Cargo.toml', 'src/main.rs', 'src/serve.rs', 'tests/runtime_catalog.rs')]
    names += [p.relative_to(REPO).as_posix() for p in (REPO / 'tools').glob('p030_*.py')]
    names += ['tools/evidence.py', 'tools/p023_real.py', 'tools/p043_sources_real.py', 'tools/validate_evidence.py', 'tools/package_copy.py', 'tools/verify_p000.py', 'tools/check_architecture.ps1']
    names += ['package.json', 'package-lock.json', 'apps/desktop/package.json', 'apps/desktop/build-ui-stamp.mjs']
    names += [p.relative_to(REPO).as_posix() for p in (REPO / 'apps/desktop/src').rglob('*') if p.is_file()]
    for crate in ('larm-domain', 'larm-db', 'larm-runtime', 'larm-api'):
        for folder in ('src', 'tests'):
            names += [p.relative_to(REPO).as_posix() for p in (REPO / 'crates' / crate / folder).rglob('*.rs')]
    return {name: sha(REPO / name) for name in sorted(set(names))}


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    inputs = Evidence('P030').inputs
    before = sources()
    checks = []
    started = datetime.now(timezone.utc).isoformat()
    commands = [
        ('build', ['cargo', 'build', '-p', 'larm-daemon', '--bin', 'larm-daemon']),
        ('general-gate', ['powershell', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'tools/check.ps1', '-Prompt', 'P030']),
        ('api-real', [sys.executable, '-X', 'utf8', 'tools/p030_api_real.py']),
        ('openapi', [str(REPO / 'target/debug/larm-daemon.exe'), 'openapi', '--out', 'docs/api/openapi.json', '--check']),
        ('lock', [sys.executable, 'tools/verify_p000.py', 'lock-coherence']),
        ('evidence-shape', [sys.executable, 'tools/validate_evidence.py']),
    ]
    for name, command in commands:
        try:
            proc = subprocess.run(command, cwd=REPO, capture_output=True, timeout=900)
            code, output = proc.returncode, proc.stdout + b'\n' + proc.stderr
        except subprocess.TimeoutExpired as exc:
            code, output = -1, b'TIMEOUT\n' + (exc.stdout or b'') + (exc.stderr or b'')
        log = OUT / f'preparation-{name}.log'
        log.write_bytes(output)
        checks.append({'id': name, 'result': 'PASS' if code == 0 else 'FAIL', 'exit_code': code, 'command': command, 'log': log.relative_to(REPO).as_posix(), 'sha256': sha(log)})
        print(name, checks[-1]['result'], flush=True)
        if code:
            print(output.decode('utf-8', errors='replace')[-2500:], flush=True)
    after = sources()
    gate = json.loads((REPO / 'docs/reports/check_P030.json').read_bytes())
    api = json.loads((OUT / 'api-real.json').read_bytes())
    assert gate['status'] == 'PASS' and len(gate['checks']) == 12
    assert api['status'] == 'PASS' and len(api['steps']) == 27
    selected = {REPO / name for name in after}
    selected.update(scan_paths(REPO / api['run']))
    selected.update(p for p in OUT.iterdir() if p.is_file() and p.suffix in ('.json', '.log'))
    needles = [secret(name).encode(enc) for name in ('ai-broker', 'agora') for enc in ('utf-8', 'utf-16-le')]
    count, size = scan_files(iter(sorted(selected)), needles, workers=8)
    record = {'phase': 'P030', 'phase_status': 'IN_PROGRESS', 'implemented_checks_status': 'PASS' if before == after and all(c['result'] == 'PASS' for c in checks) else 'FAIL',
              'scope': 'Domain, typed SQLite repositories, channel identity migration, runtime use cases and real daemon API; installation/version/backend/host seeds explicitly fixtures, not qualified engine execution.',
              'checks': checks, 'source_before_sha256': before, 'source_sha256': after, 'sources_unchanged': before == after,
              'mandatory_inputs_sha256': inputs, 'quality_gate': {'path': 'docs/reports/check_P030.json', 'sha256': sha(REPO / 'docs/reports/check_P030.json'), 'checks': 12},
              'api_real': {'path': 'docs/reports/P030_runs/api-real.json', 'sha256': sha(OUT / 'api-real.json'), 'checks': 27},
              'secret_scan': {'files': count, 'bytes': str(size), 'matches': 0, 'encodings': ['utf-8', 'utf-16-le']},
              'host': platform.platform(), 'python': platform.python_version(), 'started_at': started, 'generated_at': datetime.now(timezone.utc).isoformat(),
              'pending': ['This helper is an incremental checkpoint; use p030_evidence.py and check.ps1 -Real for formal closure', 'Product export/import owner P028 must preserve the P030 channel representation']}
    (OUT / 'preparation.checks.json').write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding='utf-8')
    print('P030 increment', record['implemented_checks_status'], len(after), 'frozen sources; pending UI/formal closure', flush=True)
    return int(record['implemented_checks_status'] != 'PASS')


if __name__ == '__main__':
    raise SystemExit(main())
