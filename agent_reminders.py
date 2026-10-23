"""Per-agent reminders, with local persistence and a detached notification worker."""

from contextlib import contextmanager
from datetime import datetime, timedelta
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import sys
import time
import uuid

from reminder_dialog import Dialog

PRESETS = (
    ('5 minutes', '5m'), ('10 minutes', '10m'), ('15 minutes', '15m'),
    ('30 minutes', '30m'), ('1 hour', '1h'), ('2 hours', '2h'),
    ('Tomorrow 09:00', 'tomorrow 09:00'), ('Custom time...', None),
)


def parse_time(value, now=None):
    now = time.time() if now is None else now
    local = datetime.fromtimestamp(now)
    text = value.strip().casefold()
    duration = re.sub(r'^(?:in\s+)|\s*(?:후|later)$', '', text).strip()
    units = {'s': 1, 'sec': 1, 'secs': 1, 'second': 1, 'seconds': 1, '초': 1,
             'm': 60, 'min': 60, 'mins': 60, 'minute': 60, 'minutes': 60, '분': 60,
             'h': 3600, 'hr': 3600, 'hrs': 3600, 'hour': 3600, 'hours': 3600, '시간': 3600,
             'd': 86400, 'day': 86400, 'days': 86400, '일': 86400}
    pattern = r'(\d+(?:\.\d+)?)\s*(seconds?|secs?|s|minutes?|mins?|m|hours?|hrs?|h|days?|d|초|분|시간|일)'
    parts = list(re.finditer(pattern, duration))
    if parts and not re.sub(pattern, '', duration).strip():
        seconds = sum(float(part[1]) * units[part[2]] for part in parts)
        due = now + seconds
    else:
        match = re.fullmatch(r'(?:(tomorrow|today|내일|오늘)\s*)?(\d{1,2})(?::(\d{2})|시(?:\s*(\d{1,2})분)?)', text)
        if match:
            day, hour, minute, korean_minute = match.groups()
            due_local = local.replace(hour=int(hour), minute=int(minute or korean_minute or 0), second=0, microsecond=0)
            if day in ('tomorrow', '내일') or day is None and due_local.timestamp() <= now:
                due_local += timedelta(days=1)
            due = due_local.timestamp()
        else:
            try:
                due = datetime.fromisoformat(text).timestamp()
            except ValueError as error:
                raise ValueError('Use 20m, 1h 30m, 내일 9시, tomorrow 09:00, or YYYY-MM-DD HH:MM.') from error
    if not math.isfinite(due) or due <= now:
        raise ValueError('Choose a time in the future.')
    try:
        datetime.fromtimestamp(due)
    except (ValueError, OSError, OverflowError) as error:
        raise ValueError('That date is outside the supported range.') from error
    return due


def state_dir():
    if os.environ.get('HERDR_PLUGIN_STATE_DIR'):
        return Path(os.environ['HERDR_PLUGIN_STATE_DIR'])
    return Path(os.environ.get('XDG_STATE_HOME', str(Path.home() / '.local/state'))) / 'herdr-agent-picker'


def current_context():
    socket = os.environ.get('HERDR_SOCKET_PATH', '')
    identity = socket
    if socket:
        try:
            stat = os.stat(socket)
            identity += f':{stat.st_dev}:{stat.st_ino}:{stat.st_ctime_ns}'
        except OSError:
            pass
    return dict(scope=hashlib.sha256(identity.encode()).hexdigest(), socket=socket,
                config=os.environ.get('HERDR_CONFIG_PATH', ''),
                herdr=shutil.which(os.environ.get('HERDR_BIN_PATH', 'herdr')) or 'herdr')


class ReminderStore:
    def __init__(self, root=None):
        self.root = state_dir() if root is None else Path(root)
        self.path = self.root / 'reminders.sqlite3'

    @contextmanager
    def database(self, create=False):
        if not create and not self.path.exists():
            yield None
            return
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        connection = sqlite3.connect(self.path, timeout=3)
        connection.row_factory = sqlite3.Row
        try:
            os.chmod(self.path, 0o600)
            with connection:
                if create:
                    connection.execute('''CREATE TABLE IF NOT EXISTS reminders (
                        id TEXT PRIMARY KEY, scope TEXT NOT NULL, pane_id TEXT NOT NULL,
                        title TEXT NOT NULL, due REAL NOT NULL, context TEXT NOT NULL,
                        status TEXT NOT NULL DEFAULT 'pending', retry_at REAL NOT NULL DEFAULT 0,
                        attempts INTEGER NOT NULL DEFAULT 0, error TEXT,
                        UNIQUE(scope, pane_id))''')
                yield connection
        finally:
            connection.close()

    def pending(self, scope=None):
        with self.database() as database:
            if database is None:
                return []
            query = "SELECT * FROM reminders WHERE status IN ('pending', 'delivering')"
            args = ()
            if scope is not None:
                query += ' AND scope=?'
                args = (scope,)
            return [dict(row) for row in database.execute(query + ' ORDER BY due', args)]

    def schedule(self, pane_id, title, due, context=None, now=None):
        now = time.time() if now is None else now
        if not math.isfinite(due) or due <= now:
            raise ValueError('Choose a time in the future.')
        context = current_context() if context is None else context
        reminder_id = uuid.uuid4().hex
        with self.database(create=True) as database:
            database.execute('''INSERT OR REPLACE INTO reminders
                (id, scope, pane_id, title, due, context) VALUES (?, ?, ?, ?, ?, ?)''',
                (reminder_id, context['scope'], pane_id, title, due, json.dumps(context)))
            database.execute("DELETE FROM reminders WHERE status='fired' AND due < ?", (now - 7 * 86400,))
        return reminder_id

    def cancel(self, reminder_id):
        with self.database() as database:
            if database is not None:
                database.execute('DELETE FROM reminders WHERE id=?', (reminder_id,))


def alarm_badges():
    try:
        return {item['pane_id']: item['due'] for item in ReminderStore().pending(current_context()['scope'])}
    except (OSError, sqlite3.Error):
        return {}


def notify(reminder):
    context = json.loads(reminder['context'])
    environment = os.environ.copy()
    for name, key in (('HERDR_SOCKET_PATH', 'socket'), ('HERDR_CONFIG_PATH', 'config')):
        if context[key]:
            environment[name] = context[key]
        else:
            environment.pop(name, None)
    title, body = 'Herdr Agent Picker', f"Time to revisit: {reminder['title']}"
    commands = [[context['herdr'], 'notification', 'show', 'Agent reminder', '--body', body, '--sound', 'done']]
    if sys.platform == 'darwin':
        commands.append(['/usr/bin/osascript', '-e',
                         'on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv)\nend run', title, body])
    elif shutil.which('notify-send'):
        commands.append(['notify-send', title, body])
    delivered, errors = False, []
    for command in commands:
        try:
            subprocess.run(command, env=environment, capture_output=True, text=True, check=True, timeout=5)
            delivered = True
        except (OSError, subprocess.SubprocessError) as error:
            errors.append(str(error))
    if not delivered:
        raise RuntimeError('; '.join(errors))


def process_due(store, now=None, deliver=notify):
    now = time.time() if now is None else now
    for reminder in store.pending():
        if reminder['status'] != 'pending' or max(reminder['due'], reminder['retry_at']) > now:
            continue
        with store.database() as database:
            claimed = database.execute("UPDATE reminders SET status='delivering' WHERE id=? AND status='pending'",
                                       (reminder['id'],)).rowcount
        if not claimed:
            continue
        try:
            deliver(reminder)
        except Exception as error:
            with store.database() as database:
                database.execute("UPDATE reminders SET status='pending', retry_at=?, attempts=attempts+1, error=? WHERE id=?",
                                 (now + 60, str(error), reminder['id']))
        else:
            with store.database() as database:
                database.execute("UPDATE reminders SET status='fired', error=NULL WHERE id=?", (reminder['id'],))


def ensure_worker(store=None):
    store = ReminderStore() if store is None else store
    try:
        if not store.pending():
            return
    except sqlite3.Error as error:
        raise RuntimeError('Could not read saved alarms.') from error
    with (store.root / 'reminders-worker.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return
    with (store.root / 'reminders-worker.log').open('ab') as log:
        os.chmod(store.root / 'reminders-worker.log', 0o600)
        subprocess.Popen([sys.executable, str(Path(__file__).resolve()), 'worker', str(store.root)],
                         stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                         start_new_session=True, close_fds=True)


def worker(root):
    store = ReminderStore(root)
    with (store.root / 'reminders-worker.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return
        with store.database() as database:
            if database is not None:
                database.execute("UPDATE reminders SET status='pending' WHERE status='delivering'")
        while store.pending():
            process_due(store)
            time.sleep(1)
    # Scheduling while this worker was shutting down must still start a worker.
    ensure_worker(store)


def choose(rows, header, title='Alarm'):
    with Dialog() as dialog:
        return dialog.choose(rows, header, title)


def menu(pane_id):
    if not pane_id or pane_id == '@section':
        return
    context, store = current_context(), ReminderStore()
    response = subprocess.run([context['herdr'], 'api', 'snapshot'], capture_output=True, text=True, check=True, timeout=3)
    snapshot = json.loads(response.stdout)['result']['snapshot']
    agent = next((item for item in snapshot['agents'] if item['pane_id'] == pane_id and item.get('agent')), None)
    if agent is None:
        raise ValueError('This agent is no longer available.')
    title = agent.get('terminal_title_stripped') or agent.get('title') or agent.get('name') or agent['agent']
    title = ' '.join(''.join(character if character.isprintable() else ' ' for character in str(title)).split())
    current = next((item for item in store.pending(context['scope']) if item['pane_id'] == pane_id), None)
    rows = [f'{index}\t[{index}] {label}' for index, (label, _) in enumerate(PRESETS, 1)]
    if current:
        rows.append('9\t[9] Cancel current alarm')
    selected = choose(rows, title + '\nj/k ↑/↓: move  1-9 / Enter: set  Esc: back')
    if selected is None:
        return
    if selected == '9' and current:
        store.cancel(current['id'])
        return
    expression = PRESETS[int(selected)-1][1]
    if expression is None:
        with Dialog() as dialog:
            error_message = ''
            while True:
                expression = dialog.prompt(error_message)
                if not expression:
                    return
                try:
                    due = parse_time(expression)
                    break
                except ValueError as error:
                    error_message = str(error)
    else:
        due = parse_time(expression)
    store.schedule(pane_id, title, due, context)
    ensure_worker(store)


def manage():
    store = ReminderStore()
    items = store.pending()
    if not items:
        with Dialog() as dialog:
            dialog.message('No scheduled alarms.')
        return
    rows = [f"{item['id']}\t{'[' + str(index) + ']' if index <= 9 else '   '} "
            f"{datetime.fromtimestamp(item['due']):%Y-%m-%d %H:%M}  {item['title']}"
            + (' (delivery failed; retrying)' if item['error'] else '')
            for index, item in enumerate(items, 1)]
    selected = choose(rows, 'j/k ↑/↓: move  Enter: cancel  Esc: back', 'Scheduled alarms')
    if selected is not None:
        store.cancel(selected)


if __name__ == '__main__':
    try:
        if sys.argv[1] == 'worker':
            worker(Path(sys.argv[2]))
        elif sys.argv[1] == 'menu':
            menu(sys.argv[2] if len(sys.argv) > 2 else '')
        elif sys.argv[1] == 'list':
            manage()
    except (KeyboardInterrupt, EOFError):
        pass
    except (OSError, ValueError, KeyError, RuntimeError, sqlite3.Error, subprocess.SubprocessError) as error:
        if sys.argv[1] in ('menu', 'list'):
            with Dialog() as dialog:
                dialog.message(str(error))
        else:
            print(f'Alarm: {error}', file=sys.stderr)
        sys.exit(1)
