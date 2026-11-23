"""Rust CLI and hook compatibility; no imports of a Python runtime."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import time
from rust_support import BINARY, ROOT, SCHEMA, cache_path, current_context, load_times

with tempfile.TemporaryDirectory(prefix='herdr-rust-runtime-') as directory:
    root = Path(directory)
    socket = root / 'session.sock'
    socket.touch()
    config = root / 'config.toml'
    config.write_text('')
    state = root / 'state'
    os.environ.update(HERDR_SOCKET_PATH=str(socket), HERDR_CONFIG_PATH=str(config), HERDR_PLUGIN_STATE_DIR=str(state))

    def native(*args, **kwargs):
        return subprocess.run([str(BINARY), *args], text=True, capture_output=True, check=True, timeout=8, **kwargs)

    def record(name, status=None):
        data = {'pane_id': 'p1'}
        if status is not None:
            data['agent_status'] = status
        event = dict(event=name, data=data)
        native('record', env=dict(os.environ, HERDR_PLUGIN_EVENT_JSON=json.dumps(event)))

    path = cache_path()
    # Test the original Python cache identity and migration fallback independently.
    legacy = root / 'agent-completions' / path.name
    legacy.parent.mkdir()
    legacy.write_text('{"p1":{"status":"idle","completed_at":42,"observed_at":42}}')
    record('pane.focused')
    entry = load_times(path)['p1']
    assert entry['pending_done'] is False and entry['completed_at'] == 42
    assert path.stat().st_mode & 0o777 == 0o600
    assert path.parent.stat().st_mode & 0o777 == 0o700
    record('pane.agent_status_changed', 'working')
    assert load_times(path)['p1']['completed_at'] is None
    record('pane_agent_status_changed', 'idle')
    entry = load_times(path)['p1']
    assert entry['pending_done'] is True and abs(entry['completed_at'] - time.time()) < 5
    record('pane.focused')
    assert load_times(path)['p1']['pending_done'] is False
    for event in ('pane.closed', 'pane.exited', 'pane.agent_detected'):
        record('pane.agent_status_changed', 'done')
        record(event)
        assert load_times(path) == {}
    record('pane.agent_status_changed', 'done')
    old_path, old_scope = path, current_context()['scope']
    socket.unlink()
    socket.touch()
    assert cache_path() != old_path and current_context()['scope'] != old_scope
    record('pane.agent_status_changed', 'working')
    assert load_times(old_path)['p1']['status'] == 'done'
    assert load_times(cache_path())['p1']['status'] == 'working'
    print('PASS: native hooks, underscore/dotted events, cache identity, legacy migration, permissions, server isolation')

    fake = root / 'herdr'
    fake.write_text('#!' + sys.executable + '\n' + '''import json, os, sys, signal, time
from pathlib import Path
args = sys.argv[1:]
with Path(os.environ['CALLS']).open('a') as f: f.write(json.dumps(args) + '\\n')
if args == ['--version']: print('herdr 0.9.5'); sys.exit(0)
if args[:2] == ['pane','get']:
 if os.environ.get('FAIL_ATTACH_SPAWN'): Path(sys.argv[0]).chmod(0o600)
 print(json.dumps({'result':{'pane':{'terminal_id':args[2]}}}))
elif args[:2] == ['terminal','attach']:
 assert len(args)==3
 Path(os.environ['ATTACHED']).write_text(str(os.getpid()))
 def finish(*_):
  Path(os.environ['RELEASED']).touch();sys.exit(0)
 signal.signal(signal.SIGTERM,finish)
 print('\\x1b[2J',flush=True)
 while True: time.sleep(1)
elif args[:2] == ['pane','read']: print('SNAPSHOT')
else: print(json.dumps({'result':{}}))
''')
    fake.chmod(0o700)
    env = dict(os.environ, HERDR_BIN_PATH=str(fake), CALLS=str(root / 'calls'),
               ATTACHED=str(root / 'attached'), RELEASED=str(root / 'released'))
    subprocess.run(['sh', str(ROOT / 'run'), 'open'], env=env, check=True, capture_output=True)
    assert json.loads((root / 'calls').read_text().splitlines()[0]) == ['plugin','pane','open','--plugin','agent-picker','--entrypoint','picker']
    native('check', env=env)
    assert subprocess.run([str(BINARY), 'unknown'], capture_output=True).returncode == 1
    assert subprocess.run(['sh', str(ROOT / 'run'), 'unknown'], capture_output=True).returncode == 2
    # Invalid dimensions and section rows may never even request a pane.
    checkpoint = (root / 'calls').read_text()
    for cols in ('0', '-1', '65536', 'invalid'):
        result = native('--live-preview', str(root / 'preview.lock'), 'p1',
                        env=dict(env, FZF_PREVIEW_COLUMNS=cols, FZF_PREVIEW_LINES='10'))
        assert 'Preview unavailable' in result.stdout
    native('--live-preview', str(root / 'preview.lock'), '@section', env=env)
    assert (root / 'calls').read_text() == checkpoint
    print('PASS: native open/check CLI, errors and rejected preview dimensions without pane requests')

    # Brutally kill only the isolated preview process. Its detached Rust guard
    # must notice pipe EOF, release the fake attachment and unlock the next one.
    preview_env = dict(env, FZF_PREVIEW_COLUMNS='43', FZF_PREVIEW_LINES='17')
    process = subprocess.Popen([str(BINARY), '--live-preview', str(root / 'preview.lock'), 'p1', 'other-origin'],
                               env=preview_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        deadline = time.monotonic() + 5
        while not (root / 'attached').exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        assert (root / 'attached').exists(), process.poll()
        process.kill()
        process.wait(timeout=3)
        deadline = time.monotonic() + 5
        while not (root / 'released').exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        assert (root / 'released').exists(), 'Orphaned attachment after SIGKILL'
        attach_pid = int((root / 'attached').read_text())
        deadline = time.monotonic() + 4
        while time.monotonic() < deadline:
            try:
                os.kill(attach_pid, 0)
            except ProcessLookupError:
                break
            time.sleep(.02)
        else:
            raise AssertionError('Attachment still alive')
        # Guard releases this only after wait/reap, which may follow the marker.
        with (root / 'preview.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        calls = [json.loads(line) for line in (root / 'calls').read_text().splitlines()]
        assert not any('--takeover' in a for a in calls)
        assert not any(a[:2] in (['agent','focus'], ['pane','write'], ['pane','resize']) for a in calls)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
    print('PASS: killed preview releases attachment and lock; no takeover/input/focus/split resize')

    started = time.monotonic()
    failed_spawn = native('--live-preview', str(root / 'preview.lock'), 'p1', 'other-origin',
                          env=dict(preview_env, FAIL_ATTACH_SPAWN='1'))
    fake.chmod(0o700)
    assert 'Preview unavailable' in failed_spawn.stdout
    assert time.monotonic() - started < 2, 'Internal guard waited for interactive input on spawn failure'
    with (root / 'preview.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    print('PASS: failed attachment spawn closes internal guard promptly and releases its lock')

    # The real worker recovers an interrupted claim and enforces singleton
    # ownership. Keep due in the future so this test cannot notify the desktop.
    state.mkdir(exist_ok=True)
    database = state / 'reminders.sqlite3'
    with sqlite3.connect(database) as db:
        db.executescript(SCHEMA)
        db.execute('INSERT INTO reminders(id,scope,pane_id,title,due,context,status) VALUES (?,?,?,?,?,?,?)',
                   ('fixture', current_context()['scope'], 'p1', 'Future synthetic alarm', time.time()+3600,
                    json.dumps(current_context()), 'delivering'))
    worker = subprocess.Popen([str(BINARY), 'worker', str(state)], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        deadline = time.monotonic()+5
        while time.monotonic()<deadline:
            with sqlite3.connect(database) as db:
                status = db.execute('SELECT status FROM reminders').fetchone()[0]
            if status == 'pending':
                break
            time.sleep(.02)
        assert status == 'pending'
        native('worker', str(state), env=env)  # Existing worker owns the lock.
        assert worker.poll() is None
        with sqlite3.connect(database) as db:
            db.execute('DELETE FROM reminders')
        assert worker.wait(timeout=4) == 0
        assert database.stat().st_mode & 0o777 == 0o600
        assert 'notification' not in (root / 'calls').read_text()
    finally:
        if worker.poll() is None:
            worker.kill()
            worker.wait()
    print('PASS: native worker recovery, singleton lock and shutdown with existing SQLite schema; no notifications')
