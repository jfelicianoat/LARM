#!/usr/bin/env python3
"""Runner formal P026: ejecuta el catálogo completo y conserva el bloqueo P025.

Las fixtures de kernel/lifecycle no cualifican plugins. Las cinco recetas nativas
y los recorridos reales se registran separados de los contratos y fault injection.
"""
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

from evidence import Evidence, REPO, cargo_ok, exit_zero
from p026_prepare_evidence import preparation_commands, preparation_sources, source_hashes, write_receipt

REAL = {
    'profile-api-real', 'source-api-real', 'probe-api-real', 'protocol-process',
    'server-process-limits', 'protected-references', 'five-family-negotiation',
    'five-family-wire', 'declarative-regression', 'runtime-reference',
    'source-reference', 'protocol-reference', 'transformation-reference',
    'benchmark-reference',
}
FAULT = {
    'profile-persistence', 'probe-persistence', 'probe-service-lifecycle',
    'persistence-conformance', 'selection-conformance', 'source-conformance',
    'update-coordinator-conformance', 'native-conformance-engine',
}
INTEGRATION = {'workspace-tests', 'rust-sdk', 'python-sdk', 'host-regressions',
               'source-transfer', 'transformation-files', 'secret-storage'}
EXPECT = {
    'build': 'Daemon, SDK y host/examples compilados con la toolchain fijada antes de ejecutar los workers.',
    'fmt': 'Formato Rust de todo el workspace conforme.',
    'clippy': 'Todos los targets del workspace sin advertencias.',
    'workspace-tests': 'Workspace Rust completo: ninguna prueba fallida ni ignorada.',
    'generated-schemas': '80 schemas request/reply para las 40 firmas, iguales a los contratos.',
    'capability-schemas': 'Schemas de descriptor/negociación íntegros y sin referencias ausentes.',
    'capability-fixtures': 'Las fixtures cumplen sus schemas y fijan input/output/estimador.',
    'reference-capability-descriptors': 'Descriptores completos y coherentes para las cinco referencias.',
    'shared-corpus': '8903 casos compartidos reproducibles sin cambiar el corpus.',
    'differential': 'Rust/Python/JSON Schema concuerdan sobre válidos e inválidos del corpus.',
    'python-contracts': 'Validación Python estricta de firmas, replies, IDs, nulables y errores.',
    'python-capabilities': 'Registro obligatorio de capacidades/schema/estimador; desconocidas no se cualifican.',
    'python-negotiation': 'Negociación estricta de capacidades sin convertir desconocido en soporte.',
    'rust-sdk': 'SDK Rust: decoder, servidor, contratos, permisos, capacidades y cinco ejemplos sin fallos ni omisiones.',
    'domain-conformance': 'Invariantes/hash/binding/transiciones del dominio, sin autoridad de IO ficticia.',
    'core-conformance': 'El kernel resuelve instalación/actor; commit/replay/fallo preservan autoridad y error original.',
    'probe-api-contract': 'Probe contractual exacto, guard antes de aceptar y privacidad del job; servicio controlado CODE_PASS.',
    'profile-contract': 'Perfiles cerrados: actor, raíces, caducidad efectiva y renovación cambian el binding; sin Proof externo.',
    'profile-persistence': 'SQLite real: CAS/consumo/outbox/reapertura/rollback/corrupción/escrituras ignoradas; no cualificación de workers.',
    'profile-api-real': '25 checks daemon/TCP: perfil runtime/consumo/reinicio/17 observaciones/selección CLI/input cambiado/desactivar.',
    'source-api-real': '25 checks daemon/TCP: Source nativa 21 observaciones y CRUD habilitado con guard, actor/CAS/input/rollback/reinicio.',
    'probe-persistence': 'SQLite real: job/recibo/instalación/evento/outbox atómicos y replay aislado; observaciones CODE_PASS.',
    'probe-service-lifecycle': 'Aceptar sobrevive cancelación; shutdown espera IO y RUNNING heredado exige recuperación; ejecutor CODE_PASS.',
    'probe-api-real': '14 checks daemon/TCP/auth/SQLite: sin perfil registrado no se ejecuta ni valida el paquete.',
    'probe-openapi': 'OpenAPI publicado coincide con la ruta y DTO Probe contractuales.',
    'persistence-conformance': 'Recibo/journal/snapshot de conformidad atómicos, replay y corrupción; Proof de fixture CODE_PASS.',
    'selection-conformance': 'Promoción/enable/rollback requieren recibo y guard; error físico no deja escrituras; desactivar permitido.',
    'source-conformance': 'Fuentes habilitadas requieren conformidad vigente y guard; disabled/replay no transfieren autoridad.',
    'update-coordinator-conformance': 'Coordinador P024 conserva publicación/rollback/last-good y sus recursos ante fallos.',
    'python-sdk': 'Servidor SDK Python fuera de proceso: frames, callbacks, cola, cancelación y EOF.',
    'host-regressions': 'Regresiones del host/P022/declarativo/source con límites y permisos; no sustituyen oráculos funcionales.',
    'native-conformance-engine': 'Binding/ownership/IO/cancel/shutdown/gate físico con procesos propios; observaciones sintéticas CODE_PASS.',
    'protocol-process': 'NDJSON/handshake/nonce/versiones/errores reales de proceso en Windows.',
    'server-process-limits': 'Procesos SDK: límites de frame/cola, duplicados, cancelación y EOF, sin éxito ficticio.',
    'protected-references': 'Cinco workers reales por P022/perfiles persistentes/router/Core/SQLite: 89 observaciones completas, 45 parciales y 45 checks físicos; también runtime negativo.',
    'five-family-negotiation': '36 pasos reales de negociación/ownership por las cinco familias, sin promover cobertura parcial.',
    'five-family-wire': '145 casos wire/host con extra fields, major, IDs, duplicados, JSON/frame/cancel timeout/crash/stderr; mutantes no cualifican el original.',
    'declarative-regression': 'Daemon/worker declarativo/SQLite reales: alta UNTRUSTED, comandos acotados, auth, export/import y cierre.',
    'runtime-reference': 'Referencia runtime completa aplicable: versión/hash, planes, procesos/HTTP/stop/cancel/crash y recuperación; no inferencia.',
    'source-reference': 'Referencia source: búsqueda/inspección/transferencia/Range/hash/ledger/fallos/cancel/crash/replay reales; no P050.',
    'source-transfer': 'Transporte source: paths/origin/redirect/reanudación y fallos contra servidor propio.',
    'protocol-reference': 'Protocol HTTP/echo con backend Rust real, cuatro firmas, errores y fingerprint; no inferencia ni leases Core.',
    'transformation-files': 'Archivos reales: rutas, symlinks, presupuesto, publicación y recuperación Base64.',
    'transformation-reference': 'Transformation: cuatro firmas, bytes Base64/oráculo independiente, provenance/ledger/cancel/crash/fault recovery reales.',
    'benchmark-statistics': 'Fórmulas de estadísticas y fingerprint contrastadas sin promover unknown.',
    'benchmark-reference': 'Benchmark: tres firmas, muestras HTTP/tiempos/fórmulas/wire/ledger/cancel/crash y recuperación; no inferencia.',
    'package-copy': 'Copias normativas iguales al paquete vigente.',
    'architecture': 'Grafo de crates y límites arquitectónicos conformes.',
    'lock': 'Dependencias/estrategias/perfiles coherentes con environment.lock.',
    'secret-scan-contract': 'Seis pruebas: UTF-8/UTF-16 y patrones entre bloques, lectura binaria completa, conteo concurrente, errores/ausencias y alcance sin omisiones.',
    'secret-storage': 'Credenciales autorizadas recuperables y ausentes de los archivos inspeccionados; valores no impresos.',
}


def kind(name):
    return 'REAL' if name in REAL else 'FAULT' if name in FAULT else 'INTEGRATION' if name in INTEGRATION else 'CONTRACT'


def main():
    commands = preparation_commands()
    assert len(commands) == 51 and len({name for name, _ in commands}) == 51
    assert set(EXPECT) == {name for name, _ in commands}, 'cada grupo debe declarar su oráculo'
    if '--list' in sys.argv:
        print(json.dumps([{'id': name, 'kind': kind(name), 'command': command, 'expected': EXPECT[name]} for name, command in commands], ensure_ascii=False, indent=2))
        return 0
    ev = Evidence('P026')
    before = source_hashes()
    started = datetime.now(timezone.utc).isoformat()
    checks = []
    for name, command in commands:
        category = kind(name)
        fixtures = [REPO / 'tools/p026_evidence.py', REPO / 'environment.lock.json']
        fixtures += [REPO / token for token in command if (REPO / token).is_file() and Path(token).suffix in ('.py', '.ps1')]
        ev.run(f'P026-{category}-{name}', category, command, EXPECT[name],
               ok=cargo_ok if command[0] == 'cargo' and 'test' in command else exit_zero,
               fixtures=fixtures, timeout=1800 if name == 'secret-storage' else 600)
        test = ev.tests[-1]
        log, digest = test['evidence_paths'][0].rsplit('#sha256=', 1)
        code = int(re.match(r'exit=(-?\d+)', test['observed'])[1])
        checks.append({'id': name, 'command': command, 'result': test['result'], 'exit_code': code, 'log': log, 'sha256': digest})
    write_receipt(checks, before, started)
    ev.run('P026-CONTRACT-independent-native-audit', 'CONTRACT',
           [sys.executable, 'tools/p026_native_audit.py', '--checks', 'docs/reports/P026_runs/preparation.checks.json'],
           'Fuentes/logs congelados, Run/observaciones/Proof/Outcome/recibos/perfiles/consumo y ledgers reconstruidos; HTTP real fresco con trazas sin tokens.',
           fixtures=[REPO / 'tools/p026_native_audit.py'], timeout=600)
    changed = [p.relative_to(REPO).as_posix() for p in preparation_sources()]
    changed += ['docs/implementation/STATE.md', 'docs/reports/P026_conformance-sdk-para-las-cinco-familias-de-plugins.md']
    return 1 if ev.finish(sorted(set(changed)), []) == 'BLOCKED' else 0


if __name__ == '__main__':
    raise SystemExit(main())
