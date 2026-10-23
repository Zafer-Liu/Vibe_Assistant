"""Probe a real observed MCP assignment without printing credentials or tool content.

Example: python tools/mcp-relay-smoke.py --name zread --agent claude_desktop
Add --direct to probe the catalog command without the launcher/relay.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import sqlite3
import subprocess
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--name', default='zread')
    parser.add_argument('--agent', default='claude_desktop')
    parser.add_argument('--direct', action='store_true')
    parser.add_argument('--initialize-only', action='store_true', help='Only initialize and list tools (any MCP server)')
    parser.add_argument('--timeout', type=float, default=90)
    args = parser.parse_args()
    root = Path(os.environ['APPDATA']) / 'agent-manager'
    manifests = [json.loads(p.read_text('utf-8')) for p in (root / 'mcp-observation').glob('*.json')]
    matches = [m for m in manifests if m['name'] == args.name and m['agent'] == args.agent]
    if len(matches) != 1 or matches[0]['phase'] != 'active':
        raise RuntimeError('Expected exactly one active assignment')
    manifest = matches[0]
    with sqlite3.connect((root / 'telemetry.sqlite3').as_uri() + '?mode=ro', uri=True) as db:
        catalog = json.loads(db.execute("SELECT value_json FROM app_settings WHERE setting_key='mcp_catalog'").fetchone()[0])
    entries = [e for e in catalog if e['name'] == args.name]
    if len(entries) != 1:
        raise RuntimeError('Expected exactly one catalog entry')
    entry = entries[0]
    secrets = [v for v in entry.get('env', {}).values() if len(v) >= 8]

    def redact(text):
        for secret in secrets:
            text = text.replace(secret, '[redacted]')
        return text[:500]

    env = os.environ.copy()
    if args.direct:
        command = [entry['command'], *entry.get('args', [])]
        env.update(entry.get('env', {}))
    else:
        if not Path(manifest['proxy_command']).is_file():
            raise RuntimeError('Configured launcher file does not exist')
        command = [manifest['proxy_command'], '--mcp-relay', manifest['id']]
    responses = queue.Queue()
    errors = bytearray()
    write_lock = threading.Lock()
    child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, env=env)
    started = time.monotonic()

    def send(message):
        with write_lock:
            child.stdin.write(json.dumps(message).encode('utf-8') + b'\n')
            child.stdin.flush()

    def read_stdout():
        try:
            for raw in child.stdout:
                message = json.loads(raw)
                if 'id' in message and 'method' in message:
                    result = {'roots': []} if message['method'] == 'roots/list' else None
                    reply = {'jsonrpc': '2.0', 'id': message['id']}
                    if result is None:
                        reply['error'] = {'code': -32601, 'message': 'Method not supported by smoke client'}
                    else:
                        reply['result'] = result
                    send(reply)
                elif 'id' in message:
                    responses.put(message)
        except Exception as error:
            responses.put(error)
        finally:
            responses.put(None)

    def read_stderr():
        while chunk := child.stderr.read(1024):
            errors.extend(chunk)
            del errors[:-4096]

    readers = [threading.Thread(target=read_stdout, daemon=True),
               threading.Thread(target=read_stderr, daemon=True)]
    for reader in readers:
        reader.start()
    sequence = 0

    def request(method, params):
        nonlocal sequence
        sequence += 1
        send({'jsonrpc': '2.0', 'id': sequence, 'method': method, 'params': params})
        deadline = time.monotonic() + args.timeout
        while True:
            try:
                message = responses.get(timeout=max(0, deadline - time.monotonic()))
            except queue.Empty:
                raise TimeoutError(f'{method} timed out after {args.timeout:g}s') from None
            if message is None:
                raise RuntimeError('MCP stdout closed before response')
            if isinstance(message, Exception):
                raise message
            if message['id'] != sequence:
                continue
            if 'error' in message:
                raise RuntimeError(redact(json.dumps(message['error'], ensure_ascii=False)))
            result = message.get('result')
            if not isinstance(result, dict):
                raise RuntimeError('MCP response missing result object')
            if result.get('isError'):
                raise RuntimeError(redact(json.dumps(result, ensure_ascii=False)))
            print(f'{method}: OK ({time.monotonic() - started:.2f}s)', flush=True)
            return result

    succeeded = False
    try:
        request('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {},
                              'clientInfo': {'name': 'agent-manager-smoke', 'version': '1'}})
        send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
        tools = request('tools/list', {})
        if args.initialize_only:
            print(f'PASS mode={"direct" if args.direct else "relay"}; tools={len(tools.get("tools", []))}', flush=True)
            succeeded = True
            return 0
        if 'get_repo_structure' not in [tool['name'] for tool in tools.get('tools', [])]:
            raise RuntimeError('Expected get_repo_structure tool missing')
        result = request('tools/call', {'name': 'get_repo_structure',
                                       'arguments': {'repo_name': 'sindresorhus/is'}})
        if not result.get('content'):
            raise RuntimeError('Tool response content is empty')
        print(f'PASS mode={"direct" if args.direct else "relay"}; content blocks={len(result["content"])}', flush=True)
        succeeded = True
    except Exception as error:
        print(f'FAIL: {redact(str(error)) or type(error).__name__}', flush=True)
    finally:
        try:
            child.stdin.close()
        except OSError:
            pass
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            # Only terminate the process tree started by this probe.
            subprocess.run(['taskkill', '/PID', str(child.pid), '/T', '/F'], capture_output=True)
            child.wait(timeout=5)
        for reader in readers:
            reader.join(timeout=1)
        if errors:
            print('stderr:', redact(errors.decode('utf-8', 'replace')))
    return 0 if succeeded else 1


if __name__ == '__main__':
    raise SystemExit(main())
