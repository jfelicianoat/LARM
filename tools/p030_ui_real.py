#!/usr/bin/env python3
"""Bundle real → puente QA loopback → daemon/SQLite. El navegador no recibe el token.
El inventario inicial procede de fixtures explícitas, exportadas/importadas por P004.
"""
import functools
import http.server
import json
import platform
import re
import sqlite3
import subprocess
import sys
import threading
import urllib.error
import urllib.request
import uuid
from datetime import datetime, timezone

from p023_real import clean_env, start, stop
from p043_sources_real import REPO, EXE, cli, sha


def inventory(db):
    with sqlite3.connect(db) as conn:
        return {table: conn.execute(f'SELECT * FROM {table} ORDER BY id').fetchall() for table in ('runtime', 'runtime_version', 'runtime_installation', 'runtime_backend', 'external_host')}


def main():
    out = REPO / 'docs/reports/P030_runs'
    run = out / 'ui-real' / uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    previous = json.loads((out / 'api-real.json').read_bytes())
    assert previous['status'] == 'PASS' and previous['daemon_sha256'] == sha(EXE)
    source_data = REPO / previous['run'] / 'data'
    original = inventory(source_data / 'state/larm.sqlite')
    backup = run / 'export.sqlite'
    export = cli(source_data, 'db-backup', '--out', backup)
    assert export['verification']['is_valid'] if 'is_valid' in export['verification'] else export['verification']['backup_rows']['runtime_version'] == 5
    data = run / 'data'
    cli(data, 'db-restore', '--from', backup)
    db = data / 'state/larm.sqlite'
    assert inventory(db) == original
    with sqlite3.connect(db) as conn:
        identity, initial_revision = conn.execute('SELECT id,row_version FROM runtime').fetchone()
        initial_channels = conn.execute('SELECT channel,metadata FROM runtime_version ORDER BY channel').fetchall()
    assert {c for c, _ in initial_channels} == {'stable', 'prerelease', 'nightly', 'commit', 'custom'}
    name = f'p030-ui-{run.name}'
    principal = cli(data, 'enroll-principal', '--name', name, '--kind', 'OPERATOR', '--scopes', 'read,runtimes:write')
    token, daemon, server = principal['token'], None, None
    records = []
    nonce = uuid.uuid4().hex
    script = """<script>window.__TAURI_INTERNALS__={invoke:async function(cmd,args){
 if(cmd==='smoke_enabled')return false;if(cmd!=='api_request')throw new Error('Comando ajeno a QA');
 const r=await fetch('/qa-api-NONCE',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(args.request)});
 if(!r.ok)throw new Error('Puente QA no disponible');return r.json();}};</script>""".replace('NONCE', nonce)

    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            if self.path != '/qa.html':
                return super().do_GET()
            html = (REPO / 'apps/desktop/dist/index.html').read_text(encoding='utf-8').replace('<head>', '<head>' + script)
            html = html.replace('<body>', '<body><p style="margin:0;padding:0.5rem 1rem;background:#ffecb0;color:#111418">QA real · inventario de prueba aislado</p>')
            payload = html.encode('utf-8')
            self.send_response(200)
            self.send_header('Content-Type', 'text/html; charset=utf-8')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_POST(self):
            if self.headers.get('Origin') != f'http://127.0.0.1:{server.server_port}':
                return self.send_error(403)
            if self.path == f'/qa-finish-{nonce}':
                self.send_response(204)
                self.end_headers()
                threading.Thread(target=server.shutdown, daemon=True).start()
                return
            if self.path != f'/qa-api-{nonce}':
                return self.send_error(403)
            length = int(self.headers.get('Content-Length', '0'))
            if not 0 < length <= 65536:
                return self.send_error(400)
            request = json.loads(self.rfile.read(length))
            method, path = request['method'], request['path']
            resource = path.split('?')[0]
            allowed = method == 'GET' and (resource in ('/api/v1/health/live', '/api/v1/health/ready', '/api/v1/adapters', '/api/v1/runtimes', '/api/v1/runtime-installations', '/api/v1/runtime-backends', '/api/v1/external-hosts') or re.fullmatch(r'/api/v1/runtimes/[0-9a-f-]{36}(?:/versions)?', resource))
            allowed = bool(allowed)
            allowed |= method == 'POST' and resource == '/api/v1/runtimes'
            allowed |= method == 'PATCH' and bool(re.fullmatch(r'/api/v1/runtimes/[0-9a-f-]{36}/channel-policy', resource))
            if not allowed or '..' in path or '#' in path:
                return self.send_error(403)
            body = json.dumps(request['body']).encode() if 'body' in request else None
            upstream = urllib.request.Request(base.removesuffix('/api/v1') + path, method=method, data=body)
            upstream.add_header('authorization', f'Bearer {token}')
            if body is not None:
                upstream.add_header('content-type', 'application/json')
            for header, value in request.get('headers', {}).items():
                if header.lower() in ('idempotency-key', 'if-match', 'x-correlation-id'):
                    upstream.add_header(header, value)
            try:
                with urllib.request.urlopen(upstream, timeout=30) as response:
                    status, raw = response.status, response.read()
            except urllib.error.HTTPError as error:
                status, raw = error.code, error.read()
            value = json.loads(raw) if raw else None
            if method != 'GET' or resource.endswith('/versions'):
                records.append({'method': method, 'path': path, 'body': request.get('body'), 'status': status, 'response': value})
                (run / 'requests.json').write_text(json.dumps(records, ensure_ascii=False, indent=2), encoding='utf-8')
            payload = json.dumps({'status': status, 'body': value}).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    passed = False
    try:
        daemon, base = start(data)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(REPO / 'apps/desktop/dist')))
        print(f'P030_UI_REAL http://127.0.0.1:{server.server_port}/qa.html#/runtimes', flush=True)
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        if server:
            server.server_close()
        if daemon and daemon.poll() is None:
            stop(daemon)
            daemon.wait(timeout=30)
        revoked = subprocess.run([str(EXE), '--data-root', str(data), 'revoke-principal', '--name', name], capture_output=True, env=clean_env(), timeout=30)
        with sqlite3.connect(db) as conn:
            rows = conn.execute('SELECT id,name,row_version,channel_policy FROM runtime ORDER BY id').fetchall()
            version_rows = conn.execute('SELECT channel,metadata FROM runtime_version ORDER BY channel').fetchall()
            state = conn.execute('SELECT state,trust FROM adapter_installation').fetchone()
            versions = conn.execute("SELECT resource_version FROM event WHERE resource_id=? ORDER BY event_id", (identity,)).fetchall()
        writes = [r for r in records if r['method'] != 'GET']
        old = next(r for r in rows if r[0] == identity)
        new = [r for r in rows if r[0] != identity]
        shown = [r for r in records if r['method'] == 'GET' and r['status'] == 200 and len(r['response']['items']) == 5]
        passed = ([r['status'] for r in writes] == [200, 201] and old[2] == initial_revision + 1 and json.loads(old[3]) == {'allowed_channels': ['stable', 'prerelease', 'commit']}
                  and len(new) == 1 and new[0][1] == 'Runtime QA real' and new[0][2] == 1 and version_rows == initial_channels and shown
                  and versions == [(1,), (2,), (3,), (4,)] and state == ('INSTALLED', 'UNTRUSTED') and revoked.returncode == 0 and daemon.returncode == 0)
        assert token not in json.dumps(records)
        paths = ['apps/desktop/src/runtimes.tsx', 'apps/desktop/src/api/runtimes.ts', 'apps/desktop/src/App.tsx', 'apps/desktop/src/styles.css', 'apps/desktop/dist/index.html', 'tools/p030_ui_real.py', 'target/debug/larm-daemon.exe']
        paths += [p.relative_to(REPO).as_posix() for p in sorted((REPO / 'apps/desktop/dist/assets').glob('*')) if p.is_file()]
        record = {'status': 'PASS' if passed else 'FAIL', 'scope': __doc__ + ' No WebView nativo, instalación ni inferencia cualificada.', 'run': run.relative_to(REPO).as_posix(),
                  'source_sha256': {p: sha(REPO / p) for p in paths}, 'backup_sha256': sha(backup), 'requests_sha256': sha(run / 'requests.json'), 'export_import_inventory_equal': True, 'five_channels_after_restore': initial_channels,
                  'requests': records, 'runtime_event_versions': versions, 'cleanup': {'principal_revoked': revoked.returncode == 0, 'daemon_exit_code': daemon.returncode},
                  'host': platform.platform(), 'generated_at': datetime.now(timezone.utc).isoformat()}
        (run / 'receipt.json').write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding='utf-8')
        (out / 'ui-real.json').write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding='utf-8')
        print('P030 UI real:', record['status'], flush=True)
    return int(not passed)


if __name__ == '__main__':
    if len(sys.argv) == 3 and sys.argv[1] == '--finish':
        url = sys.argv[2].split('#')[0]
        assert re.fullmatch(r'http://127\.0\.0\.1:\d+/qa\.html', url)
        html = urllib.request.urlopen(url, timeout=10).read().decode()
        nonce = re.search(r'/qa-api-([a-f0-9]{32})', html).group(1)
        origin = url.removesuffix('/qa.html')
        with urllib.request.urlopen(urllib.request.Request(f'{origin}/qa-finish-{nonce}', method='POST', headers={'Origin': origin}), timeout=10) as response:
            assert response.status == 204
        raise SystemExit(0)
    raise SystemExit(main())
