"""Synthetic state fixtures only; all exercised runtime processes are native Rust.

This intentionally does not import the former Python implementation. Fixture keys
are computed independently to check the on-disk Python-to-Rust compatibility.
"""
from contextlib import contextmanager
import tempfile
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess

ROOT = Path(__file__).resolve().parents[1]
FIXTURE_TMP = ROOT / 'target/test-tmp'
FIXTURE_TMP.mkdir(parents=True, exist_ok=True)
os.environ['TMPDIR'] = str(FIXTURE_TMP)
tempfile.tempdir = str(FIXTURE_TMP)
BINARY = Path(os.environ.get('HERDR_AGENT_PICKER_TEST_BINARY', ROOT / 'target/debug/herdr-agent-picker')).resolve()
if not BINARY.is_file():
    raise RuntimeError(f'Build the Rust runtime first: missing {BINARY}')
with BINARY.open('rb') as executable:
    assert executable.read(4) in (b'\x7fELF', b'\xcf\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\xca\xfe\xba\xbe'), 'Rust regression target must be a native executable'
version = subprocess.check_output([str(BINARY), '--version'], text=True)
assert version.startswith('herdr-agent-picker '), version
print(f'RUST RUNTIME UNDER TEST: {BINARY}', flush=True)


def state_dir():
    return Path(os.environ['HERDR_PLUGIN_STATE_DIR'])


def cache_path():
    socket = os.environ['HERDR_SOCKET_PATH']
    stat = os.stat(socket)
    key = hashlib.sha256(f'{socket}:{stat.st_ino}:{stat.st_ctime_ns}'.encode()).hexdigest()[:24]
    return state_dir() / 'completions' / f'{key}.json'


def current_context():
    socket = os.environ['HERDR_SOCKET_PATH']
    stat = os.stat(socket)
    identity = f'{socket}:{stat.st_dev}:{stat.st_ino}:{stat.st_ctime_ns}'
    return dict(scope=hashlib.sha256(identity.encode()).hexdigest(), socket=socket,
                config=os.environ.get('HERDR_CONFIG_PATH', ''), herdr=os.environ.get('HERDR_BIN_PATH', 'herdr'))


def load_times(path):
    return json.loads(path.read_text()) if path.exists() else {}


def seed_event(event, path, observed_at):
    """Seed historical data, without waiting minutes/days for actual hook events."""
    times = load_times(path)
    pane = event['data']['pane_id']
    status = event['data']['agent_status']
    old = times.get(pane, {})
    completed = old.get('completed_at')
    pending = False
    if status == 'done' and (old.get('status') not in ('done', 'idle') or completed is None):
        completed = observed_at
    if status == 'idle' and old.get('status') in ('working', 'blocked'):
        completed, pending = observed_at, True
    if status not in ('done', 'idle'):
        completed = None
    times[pane] = dict(status=status, completed_at=completed, pending_done=pending, observed_at=observed_at)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix('.fixture-tmp')
    temporary.write_text(json.dumps(times))
    temporary.replace(path)


SCHEMA = '''CREATE TABLE IF NOT EXISTS reminders (
    id TEXT PRIMARY KEY, scope TEXT NOT NULL, pane_id TEXT NOT NULL,
    title TEXT NOT NULL, due REAL NOT NULL, context TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending', retry_at REAL NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0, error TEXT, UNIQUE(scope, pane_id));
    CREATE TABLE IF NOT EXISTS unread_reminders (
    scope TEXT NOT NULL, pane_id TEXT NOT NULL, PRIMARY KEY(scope,pane_id));'''


class FixtureStore:
    def __init__(self, root=None):
        self.root = Path(root) if root is not None else state_dir()
        self.path = self.root / 'reminders.sqlite3'

    @contextmanager
    def database(self, create=False):
        self.root.mkdir(parents=True, exist_ok=True)
        connection = sqlite3.connect(self.path, timeout=3)
        connection.row_factory = sqlite3.Row
        if create:
            connection.executescript(SCHEMA)
        try:
            with connection:
                yield connection
        finally:
            connection.close()

    def pending(self):
        if not self.path.exists():
            return []
        with self.database() as db:
            return [dict(r) for r in db.execute("SELECT * FROM reminders WHERE status IN ('pending','delivering') ORDER BY due")]

    def unread(self, scope):
        if not self.path.exists():
            return set()
        with self.database() as db:
            return {r[0] for r in db.execute('SELECT pane_id FROM unread_reminders WHERE scope=?', (scope,))}

    def acknowledge(self, scope, pane):
        with self.database() as db:
            db.execute('DELETE FROM unread_reminders WHERE scope=? AND pane_id=?', (scope, pane))

    def cancel(self, identifier):
        with self.database() as db:
            db.execute('DELETE FROM reminders WHERE id=?', (identifier,))
