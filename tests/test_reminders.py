"""Time interpretation, durable scheduling, retries and safe notification arguments."""

from datetime import datetime
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from agent_reminders import ReminderStore, notify, parse_time, process_due

now = datetime(2026, 10, 23, 14, 30).timestamp()
for text, seconds in [('5m', 300), ('10 minutes', 600), ('15분', 900), ('30분 후', 1800),
                      ('1h', 3600), ('2시간', 7200), ('1h 30m', 5400), ('in 20 minutes', 1200)]:
    assert parse_time(text, now) == now + seconds, text
assert datetime.fromtimestamp(parse_time('내일 9시', now)) == datetime(2026, 10, 24, 9)
assert parse_time('내일 9시', now) == parse_time('tomorrow 09:00', now)
assert datetime.fromtimestamp(parse_time('09:00', now)) == datetime(2026, 10, 24, 9)
assert datetime.fromtimestamp(parse_time('오늘 16시 30분', now)) == datetime(2026, 10, 23, 16, 30)
assert datetime.fromtimestamp(parse_time('2026-10-24 14:30', now)) == datetime(2026, 10, 24, 14, 30)
for text in ['', '0m', '-5m', '5m junk', '오늘 9시', '25:00', 'tomorrow 09:99', '2020-01-01 12:00', '9' * 200 + 'h']:
    try:
        parse_time(text, now)
    except (ValueError, OverflowError):
        pass
    else:
        raise AssertionError(f'Invalid time accepted: {text}')

previous_tz = os.environ.get('TZ')
try:
    os.environ['TZ'] = 'America/New_York'
    time.tzset()
    before_dst = datetime(2026, 3, 7, 12).timestamp()
    assert parse_time('tomorrow 09:00', before_dst) - before_dst == 20 * 3600
finally:
    if previous_tz is None:
        os.environ.pop('TZ', None)
    else:
        os.environ['TZ'] = previous_tz
    time.tzset()

with tempfile.TemporaryDirectory(prefix='herdr-reminder-test-') as directory:
    root = Path(directory)
    store = ReminderStore(root)
    context = dict(scope='session-a', socket='/example/session-a.sock', config='/example/config.toml', herdr='/example/herdr')
    assert store.pending() == [] and not store.path.exists()
    first = store.schedule('p1', 'First task', 1100, context, now=1000)
    replacement = store.schedule('p1', 'Replacement', 1200, context, now=1000)
    store.cancel(first)
    assert [item['id'] for item in store.pending()] == [replacement]
    assert store.path.stat().st_mode & 0o777 == 0o600
    other = store.schedule('p1', 'Other session', 1300, dict(context, scope='session-b'), now=1000)
    assert len(store.pending()) == 2 and len(store.pending('session-a')) == 1
    store.cancel(other)
    delivered = []
    process_due(store, 1199, delivered.append)
    assert delivered == []

    def fail(reminder):
        raise RuntimeError('Temporary notification failure')

    process_due(store, 1200, fail)
    item = store.pending()[0]
    assert item['retry_at'] == 1260 and item['attempts'] == 1 and item['error']
    process_due(store, 1259, delivered.append)
    assert delivered == []
    process_due(store, 1260, delivered.append)
    process_due(store, 2000, delivered.append)
    assert len(delivered) == 1 and store.pending() == [], 'Notification was duplicated'
    with patch('agent_reminders.subprocess.run') as run, patch('agent_reminders.sys.platform', 'darwin'):
        run.return_value = subprocess.CompletedProcess([], 0)
        notify(dict(delivered[0], title='Quotes " and $(literal)'))
        commands = run.call_args_list
        assert len(commands) == 2
        assert commands[0].args[0][0] == '/example/herdr'
        assert commands[0].kwargs['env']['HERDR_SOCKET_PATH'] == context['socket']
        assert 'Quotes " and $(literal)' in commands[1].args[0][-1]
        assert not any(call.kwargs.get('shell') for call in commands)

    # A fresh worker recovers a delivery interrupted before notification.
    store.schedule('p2', 'Recovered task', time.time() + 0.1, context)
    with store.database() as database:
        database.execute("UPDATE reminders SET status='delivering' WHERE pane_id='p2'")
    output = root / 'delivered.json'
    code = '''import json, pathlib, sys
sys.path.insert(0, sys.argv[1])
import agent_reminders as reminders
original = reminders.process_due
def deliver(item):
    pathlib.Path(sys.argv[3]).write_text(json.dumps(item))
reminders.process_due = lambda store: original(store, deliver=deliver)
reminders.worker(pathlib.Path(sys.argv[2]))
'''
    process = subprocess.Popen([sys.executable, '-c', code, str(ROOT), str(root), str(output)],
                               start_new_session=True)
    try:
        assert process.wait(timeout=5) == 0
        assert json.loads(output.read_text())['title'] == 'Recovered task'
        assert ReminderStore(root).pending() == []
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()

print('PASS: presets, custom dates, DST, persistence, replacement, cancellation, retries, recovery and notification arguments')
