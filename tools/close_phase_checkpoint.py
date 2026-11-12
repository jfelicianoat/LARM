#!/usr/bin/env python3
"""Congela una fase formal antes de consumidores; copia, nunca mueve originales."""
import argparse
import hashlib
import json
import re
import shutil
from datetime import datetime, timezone
from pathlib import Path

from check_integration_credentials import scan_files, secret
from evidence import REPO, prompt_entry


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load(path):
    return json.loads(path.read_bytes())


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('phase')
    parser.add_argument('--stamp', required=True)
    args = parser.parse_args()
    assert re.fullmatch(r'P\d{3}', args.phase) and re.fullmatch(r'\d{8}T\d{6}Z', args.stamp)
    phase = args.phase
    out = REPO / f'docs/reports/{phase}_runs'
    formal = load(out / 'formal.checks.json')
    evidence_path = REPO / (prompt_entry(phase)['report'] + '.evidence.json')
    evidence = load(evidence_path)
    assert evidence['status'] in ('REAL_PASS', 'BLOCKED_REAL')
    assert all(t['result'] == 'PASS' for t in evidence['tests'])
    assert formal['sources_unchanged'] and formal['source_before_sha256'] == formal['source_sha256']
    selected = {REPO / name for name in formal['source_sha256']}
    for name, expected in formal['source_sha256'].items():
        assert sha(REPO / name) == sha(REPO / formal['source_snapshot'] / name) == expected, name
    for test in evidence['tests']:
        for ref in test['evidence_paths']:
            name, expected = ref.rsplit('#sha256=', 1)
            assert sha(REPO / name) == expected, name
            selected.add(REPO / name)
        for ref in test['fixture_hashes']:
            name, expected = ref.rsplit(':', 1)
            assert sha(REPO / name) == expected, name
            selected.add(REPO / name)
    gate = REPO / f'docs/reports/check_{phase}_real.json'
    assert load(gate)['status'] == 'PASS' and all(c['result'] == 'PASS' for c in load(gate)['checks'])
    for name, expected in formal['reports_sha256'].items():
        assert sha(REPO / name) == expected
        record = load(REPO / name)
        selected.add(REPO / name)
        run = REPO / record['run']
        if 'source_sha256' in record and isinstance(record['source_sha256'], dict):
            for source, digest in record['source_sha256'].items():
                assert sha(REPO / source) == digest, source
                selected.add(REPO / source)
        selected.update(p for p in run.rglob('*') if p.is_file())
    selected.update(p for p in out.iterdir() if p.is_file() and p.suffix in ('.json', '.log', '.md'))
    selected.update({gate, evidence_path, REPO / (prompt_entry(phase)['report'] + '.md'), REPO / 'docs/implementation/STATE.md', Path(__file__)})
    stamp = REPO / 'apps/desktop/dist/ui-build.json'
    if stamp.exists():
        selected.add(stamp)
        for group in ('sources', 'assets'):
            selected.update(REPO / 'apps/desktop' / name for name in load(stamp)[group])
    snapshot = out / 'formal-checkpoints' / args.stamp
    assert not snapshot.exists(), 'El checkpoint existente nunca se sobrescribe'
    records = []
    for source in sorted(selected):
        assert source.is_relative_to(REPO) and source.is_file(), source
        relative = source.relative_to(REPO)
        dest = snapshot / relative
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, dest)
        digest = sha(source)
        assert sha(dest) == digest
        records.append({'original': relative.as_posix(), 'archive': dest.relative_to(REPO).as_posix(), 'sha256': digest})
    needles = [secret(name).encode(enc) for name in ('ai-broker', 'agora') for enc in ('utf-8', 'utf-16-le')]
    count, size = scan_files(iter([REPO / r['archive'] for r in records]), needles, workers=8)
    result = {'phase': phase, 'status': 'PASS', 'files': records,
              'formal_sha256': sha(out / 'formal.checks.json'), 'evidence_sha256': sha(evidence_path),
              'secret_scan': {'files': count, 'bytes': str(size), 'matches': 0, 'encodings': ['utf-8', 'utf-16-le']},
              'generated_at': datetime.now(timezone.utc).isoformat()}
    (snapshot / 'index.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    print(f'PASS: {phase}; {len(records)} originales/copias idénticos; tokens ausentes de {count} archivos/{size} bytes; {snapshot.relative_to(REPO).as_posix()}/index.json')


if __name__ == '__main__':
    main()
