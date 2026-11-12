#!/usr/bin/env python3
"""Oráculo independiente del índice formal P030; no toma su estado como prueba."""
import hashlib
import json
import re
from datetime import datetime

from evidence import Evidence, REPO


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    out = REPO / 'docs/reports/P030_runs'
    receipt = json.loads((out / 'formal.checks.json').read_bytes())
    assert receipt['source_before_sha256'] == receipt['source_sha256'] and receipt['sources_unchanged'] is True
    for name, digest in receipt['source_sha256'].items():
        assert sha(REPO / name) == sha(REPO / receipt['source_snapshot'] / name) == digest, name
    assert receipt['mandatory_inputs_sha256'] == Evidence('P030').inputs
    assert len(receipt['checks']) == len({c['id'] for c in receipt['checks']}) == 15
    counts = {}
    for check in receipt['checks']:
        log = REPO / check['log']
        assert sha(log) == check['sha256'] and check['result'] == 'PASS' and check['exit_code'] == 0, check['id']
        text = log.read_text(encoding='utf-8')
        assert '# exit=0 ' in text
        if check['command'][0] == 'cargo' and 'test' in check['command']:
            results = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', text)
            assert results and all(failed == ignored == '0' for _, failed, ignored in results)
            counts[check['id']] = sum(int(passed) for passed, _, _ in results)
    assert {key: counts[key] for key in ('domain', 'database', 'migrations', 'adapter-selection', 'catalog')} == {'domain': 10, 'database': 9, 'migrations': 16, 'adapter-selection': 9, 'catalog': 1}
    for name, digest in receipt['reports_sha256'].items():
        assert sha(REPO / name) == digest, name
    api = json.loads((out / 'api-real.json').read_bytes())
    assert api['status'] == 'PASS' and len(api['steps']) == 27 and all(s['result'] == 'PASS' for s in api['steps'])
    assert datetime.fromisoformat(api['generated_at']) >= datetime.fromisoformat(receipt['started_at'])
    assert sha(REPO / api['http_trace']['path']) == api['http_trace']['sha256']
    assert sha(REPO / 'tools/p030_api_real.py') == api['source_sha256']
    assert json.loads((REPO / api['run'] / 'steps.json').read_bytes()) == api
    ui = json.loads((out / 'ui-real.json').read_bytes())
    assert api['daemon_sha256'] == ui['source_sha256']['target/debug/larm-daemon.exe'] == sha(REPO / 'target/debug/larm-daemon.exe')
    result = {'status': 'PASS', 'sources': len(receipt['source_sha256']), 'checks': 15, 'rust_tests': counts,
              'api_checks': 27, 'ui_recorded_session': ui['run'], 'formal_receipt_sha256': sha(out / 'formal.checks.json')}
    (out / 'audit.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps(result, ensure_ascii=False))


if __name__ == '__main__':
    main()
