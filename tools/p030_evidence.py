#!/usr/bin/env python3
"""P030 formal: dominio/SQLite/API/IO/UI, sin promover fixtures a engines."""
import json
import re
import shutil
import sys
from datetime import datetime, timezone
from pathlib import Path

from evidence import Evidence, REPO, cargo_ok
from p030_prepare_evidence import OUT, sha, sources


def commands():
    npm = shutil.which('npm.cmd') or 'npm'
    py = [sys.executable, '-X', 'utf8']
    return [
        ('build', 'INTEGRATION', ['cargo', 'build', '-p', 'larm-daemon', '--bin', 'larm-daemon'], 'Daemon compilado con la toolchain fijada.'),
        ('workspace', 'INTEGRATION', ['cargo', 'test', '--workspace'], 'Todas las pruebas Rust pasan sin fallos ni omisiones; transcripción íntegra conservada.'),
        ('domain', 'CONTRACT', ['cargo', 'test', '-p', 'larm-domain', '--test', 'runtime'], 'Diez pruebas de DTO cerrado, nulables, cinco canales, unknown, serialización, política, lifecycle e identidad.'),
        ('database', 'FAULT', ['cargo', 'test', '-p', 'larm-db', '--test', 'runtimes'], 'Nueve pruebas SQLite: atomicidad, replay/CAS concurrentes, corrupción/reapertura, ABORT/IGNORE e índice público reparable sin autoridad.'),
        ('migrations', 'FAULT', ['cargo', 'test', '-p', 'larm-db', '--test', 'integration'], 'Dieciséis pruebas; upgrade 0001/0002 conserva datos/pin/revisión y distingue cinco canales; fallo de DDL revierte.'),
        ('adapter-selection', 'INTEGRATION', ['cargo', 'test', '-p', 'larm-db', '--test', 'adapter_selection'], 'Nueve regresiones de selección y migraciones con promoción protegida.'),
        ('catalog', 'INTEGRATION', ['cargo', 'test', '-p', 'larm-daemon', '--test', 'runtime_catalog'], 'Política persistida filtra descubrimiento y distingue canales; nunca oculta inventario ni acepta UNKNOWN automáticamente.'),
        ('api', 'REAL', py + ['tools/p030_api_real.py'], '27 checks Windows/daemon/TCP/SQLite: schemas, permisos, replay/CAS, cinco canales, unknown, fallos outbox, reinicio y limpieza propios.'),
        ('ui', 'INTEGRATION', [npm, 'test', '--workspace', '@larm/desktop', '--', 'src/runtimes.test.tsx'], 'Once pruebas UI: cinco canales/foco/vacío, CAS/errores/borradores, replay, paginación y refresco/respuestas tardías sin perder revisión del borrador ni un guardado confirmado.'),
        ('ui-receipt', 'CONTRACT', py + ['tools/p030_ui_verify.py'], 'Audita la sesión CUA ya ejecutada contra bundle y SQLite; backup/restore conserva cinco canales y unknown; PATCH200/POST201/outbox/limpieza. No reproduce navegador ni acredita WebView.'),
        ('openapi', 'CONTRACT', ['target/debug/larm-daemon.exe', 'openapi', '--out', 'docs/api/openapi.json', '--check'], 'OpenAPI coincide con contrato y once operaciones P030 publicadas.'),
        ('package-copy', 'CONTRACT', py + ['tools/package_copy.py', 'check'], 'Copias normativas idénticas al paquete vigente.'),
        ('architecture', 'CONTRACT', ['powershell', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'tools/check_architecture.ps1'], 'Límites del grafo de crates conservados; API no depende de SQL/runtime concreto.'),
        ('lock', 'CONTRACT', py + ['tools/verify_p000.py', 'lock-coherence'], 'Dependencias y toolchain coherentes con environment.lock.'),
        ('secret-storage', 'INTEGRATION', py + ['tools/check_integration_credentials.py', '--scan-only'], 'Credenciales exactas Broker/Agora recuperables y ausentes de todos los archivos inspeccionados UTF-8/UTF-16LE; no se imprimen valores.'),
    ]


def main():
    catalog = commands()
    if '--list' in sys.argv:
        print(json.dumps(catalog, ensure_ascii=False, indent=2))
        return 0
    ev = Evidence('P030')
    before, started = sources(), datetime.now(timezone.utc).isoformat()
    checks = []
    for name, kind, command, expected in catalog:
        fixtures = [REPO / 'tools/p030_evidence.py', REPO / 'environment.lock.json']
        fixtures += [REPO / part for part in command if (REPO / part).is_file() and Path(part).suffix in ('.py', '.ps1')]
        if name == 'ui-receipt':
            fixtures.append(REPO / 'docs/reports/P030_runs/ui-real.json')
        ev.run(f'P030-{kind}-{name}', kind, command, expected,
               ok=cargo_ok if command[0] == 'cargo' and 'test' in command else lambda code, _: code == 0,
               fixtures=fixtures, timeout=1800 if name in ('secret-storage', 'workspace') else 600)
        test = ev.tests[-1]
        log, digest = test['evidence_paths'][0].rsplit('#sha256=', 1)
        checks.append({'id': name, 'kind': kind, 'command': command, 'result': test['result'],
                       'exit_code': int(re.match(r'exit=(-?\d+)', test['observed'])[1]), 'log': log, 'sha256': digest})
    after = sources()
    snapshot = OUT / 'formal-sources'
    for name in after:
        dest = snapshot / name
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(REPO / name, dest)
    reports = ['docs/reports/P030_runs/api-real.json', 'docs/reports/P030_runs/ui-real.json']
    receipt = {'phase': 'P030', 'checks': checks, 'source_before_sha256': before, 'source_sha256': after,
               'sources_unchanged': before == after, 'source_snapshot': snapshot.relative_to(REPO).as_posix(),
               'mandatory_inputs_sha256': ev.inputs, 'reports_sha256': {name: sha(REPO / name) for name in reports},
               'started_at': started, 'generated_at': datetime.now(timezone.utc).isoformat()}
    (OUT / 'formal.checks.json').write_text(json.dumps(receipt, ensure_ascii=False, indent=2), encoding='utf-8')
    ev.run('P030-CONTRACT-independent-audit', 'CONTRACT', [sys.executable, '-X', 'utf8', 'tools/p030_audit.py'],
           'Fuentes actuales y copias congeladas idénticas, todos los logs/recibos ligados por hash, API fresca con 27 pasos, pruebas Rust sin ignoradas y UI producida con el mismo daemon.',
           fixtures=[REPO / 'tools/p030_audit.py', OUT / 'formal.checks.json'])
    changed = sorted(set(after) | {'docs/reports/P030_dominio-y-persistencia-de-runtimes.md', 'docs/implementation/STATE.md'})
    return int(ev.finish(changed, []) == 'BLOCKED')


if __name__ == '__main__':
    raise SystemExit(main())
