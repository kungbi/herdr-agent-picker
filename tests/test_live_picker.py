"""Exercise live reloads using real fzf and a mutable, fake Herdr snapshot."""

import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import sys
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from agent_completion_times import cache_path, load_times, record_event


def snapshot(agents):
    return {
        'agents': [dict(agent='claude', workspace_id='w1', tab_id='w1:t1',
                        pane_id=f'w1:p{number}', agent_status=status, title=title,
                        focused=number == 1) for number, status, title in agents],
        'workspaces': [{'workspace_id': 'w1', 'label': 'Workspace'}],
        'tabs': [{'tab_id': 'w1:t1', 'label': 'Tab'}],
    }


initial = snapshot([(1, 'working', 'Original task'), (2, 'blocked', 'Waiting task'),
                    (3, 'idle', 'Third task')])
updated = snapshot([(1, 'done', 'Finished task'), (2, 'working', 'Continuing task'),
                    (3, 'idle', 'Updated third task'), (4, 'blocked', 'New blocked task')])

with tempfile.TemporaryDirectory(prefix='herdr-live-test-') as directory:
    root = Path(directory)
    fake = root / 'herdr'
    fake.write_text('#!' + sys.executable + '\n' + '''import json, os, sys, time
from pathlib import Path
args = sys.argv[1:]
with Path(os.environ['CALLS']).open('a') as log:
    log.write(' '.join(args) + '\\n')
if args == ['api', 'snapshot']:
    if os.environ.get('SLOW_REFRESH') and Path(os.environ['CALLS']).read_text().count('api snapshot') > 1:
        Path(os.environ['REFRESH_STARTED']).touch()
        time.sleep(1.5)
    result = {'snapshot': json.loads(Path(os.environ['SNAPSHOT']).read_text())}
elif args[:2] == ['agent', 'focus']:
    Path(os.environ['FOCUS']).write_text(args[2])
    result = {}
else:
    raise SystemExit(2)
print(json.dumps({'result': result}))
''')
    fake.chmod(0o700)
    config = root / 'config.toml'
    config.write_text('[ui]\nagent_panel_sort = "priority"\n')
    socket_path = root / 'session.sock'
    socket_path.touch()
    os.environ.update(HERDR_SOCKET_PATH=str(socket_path), HERDR_CONFIG_PATH=str(config),
                      HERDR_PLUGIN_STATE_DIR=str(root / 'state'))

    cases = [
        ('preserve_cursor', b'\r', 'w1:p3'),
        ('new_agent_shortcut', b'2', 'w1:p4'),
        ('changed_section_shortcut', b'1', 'w1:p1'),
        ('done_only', b'1', 'w1:p1'),
        ('third_done_via_idle', b'1', 'w1:p3'),
        ('removed_agent', b'1', 'w1:p4'),
        ('initially_empty', b'2', 'w1:p4'),
        ('becomes_empty', b'\x1b', None),
        ('failed_refresh', b'3', 'w1:p3'),
        ('refresh_recovers', b'2', 'w1:p4'),
        ('slow_navigation', b'j\r', 'w1:p3'),
        ('slow_enter', b'\r', 'w1:p1'),
        ('slow_number', b'3', 'w1:p3'),
        ('slow_preserve_cursor', b'\r', 'w1:p3'),
    ]
    for scenario, key, expected in cases:
        os.environ['HERDR_PLUGIN_STATE_DIR'] = str(root / f'{scenario}-state')
        data = root / f'{scenario}.json'
        calls = root / f'{scenario}.calls'
        focus = root / f'{scenario}.focus'
        refresh_started = root / f'{scenario}.refresh-started'
        starting = snapshot([]) if scenario == 'initially_empty' else initial
        if scenario == 'third_done_via_idle':
            starting = snapshot([(1, 'done', 'First finish'), (2, 'done', 'Second finish'),
                                 (3, 'working', 'Third task')])
        data.write_text(json.dumps(starting))
        pid, fd = pty.fork()
        if pid == 0:
            environment = os.environ.copy()
            environment.update(TERM='xterm-256color', HERDR_BIN_PATH=str(fake),
                               SNAPSHOT=str(data), CALLS=str(calls), FOCUS=str(focus))
            if scenario.startswith('slow_'):
                environment.update(SLOW_REFRESH='1', REFRESH_STARTED=str(refresh_started))
            os.execve(sys.executable, [sys.executable, str(ROOT / 'agent-picker.py')], environment)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 120, 0, 0))
        output = bytearray()
        exited = False

        def drain():
            if select.select([fd], [], [], 0.1)[0]:
                try:
                    output.extend(os.read(fd, 65536))
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise

        def wait_for(predicate):
            deadline = time.monotonic() + 7
            while not predicate() and time.monotonic() < deadline:
                drain()
            assert predicate(), output.decode(errors='replace')

        try:
            marker = b'No running agents' if scenario == 'initially_empty' else b'Third task'
            wait_for(lambda: marker in output)
            if scenario.startswith('slow_') and scenario != 'slow_preserve_cursor':
                wait_for(refresh_started.exists)
                pressed_at = time.monotonic()
                os.write(fd, key)
                while not focus.exists() and time.monotonic() - pressed_at < 0.7:
                    drain()
                assert focus.exists(), 'Input was blocked during the 1.5-second snapshot refresh'
                assert focus.read_text() == expected, focus.read_text()
                latency_ms = (time.monotonic() - pressed_at) * 1000
                deadline = time.monotonic() + 2
                while time.monotonic() < deadline:
                    done, status = os.waitpid(pid, os.WNOHANG)
                    if done:
                        exited = True
                        assert os.waitstatus_to_exitcode(status) == 0, output.decode(errors='replace')
                        break
                    drain()
                assert exited, 'Picker did not exit during a slow refresh'
                print(f'PASS: live {scenario}; input handled in {latency_ms:.0f} ms during slow refresh', flush=True)
                continue
            if scenario in ('preserve_cursor', 'slow_preserve_cursor'):
                if scenario == 'slow_preserve_cursor':
                    wait_for(refresh_started.exists)
                os.write(fd, b'j')  # Move away from the initially focused agent.
                time.sleep(0.15)
            if scenario in ('failed_refresh', 'refresh_recovers'):
                data.write_text('invalid JSON')
                wait_for(lambda: calls.read_text().count('api snapshot') >= 2)
                time.sleep(0.2)
                assert not focus.exists(), 'Failed refresh changed focus'
            if scenario != 'failed_refresh':
                changed = snapshot([]) if scenario == 'becomes_empty' else updated
                if scenario == 'done_only':
                    changed = dict(updated, agents=[a for a in updated['agents'] if a['agent_status'] in ('done', 'idle')])
                if scenario == 'removed_agent':
                    changed = dict(updated, agents=[a for a in updated['agents'] if a['pane_id'] != 'w1:p1'])
                if scenario == 'third_done_via_idle':
                    changed = snapshot([(1, 'done', 'Finished task'), (2, 'done', 'Second finish'),
                                        (3, 'idle', 'Third completion')])
                    record_event({'event': 'pane.agent_status_changed', 'data': {
                        'pane_id': 'w1:p3', 'agent_status': 'working'}}, cache_path(), time.time() - 1)
                    record_event({'event': 'pane.agent_status_changed', 'data': {
                        'pane_id': 'w1:p3', 'agent_status': 'idle'}}, cache_path(), time.time())
                record_event({'event': 'pane_agent_status_changed', 'data': {
                    'pane_id': 'w1:p1', 'agent_status': 'done'}}, cache_path(),
                    time.time() - (2 if scenario == 'third_done_via_idle' else 0))
                data.write_text(json.dumps(changed))
                output.clear()
                marker = (b'No running agents' if scenario == 'becomes_empty' else
                          b'Third completion' if scenario == 'third_done_via_idle' else
                          b'Finished task' if scenario == 'done_only' else b'New blocked task')
                wait_for(lambda: marker in output)
                if scenario == 'third_done_via_idle':
                    assert output.count(b'[done]') >= 3, 'Direct-idle finish did not join the two done agents'
                    assert b'[idle]' not in output, 'Third completion still shows idle'
                if scenario not in ('becomes_empty', 'removed_agent'):
                    assert b'Finished task' in output, 'Status/title update is missing'
                    assert b'just now' in output, 'Completion age did not update'
                    assert b'Done' in output, 'Completed agent did not move into Done'
                assert not focus.exists(), 'Refreshing changed focus'
            if scenario == 'becomes_empty':
                os.write(fd, b'19\r')
                time.sleep(0.15)
                assert not focus.exists(), 'Empty list accepted a selection'
            os.write(fd, key)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                done, status = os.waitpid(pid, os.WNOHANG)
                if done:
                    exited = True
                    assert os.waitstatus_to_exitcode(status) == 0, output.decode(errors='replace')
                    break
                drain()
            assert exited, 'Picker did not exit'
            if expected is None:
                assert not focus.exists(), 'Cancel changed focus'
            else:
                assert focus.read_text() == expected, focus.read_text()
                if scenario == 'third_done_via_idle':
                    entry = load_times(cache_path())['w1:p3']
                    assert entry['pending_done'] is False, 'Selecting the completed agent did not acknowledge it'
                    assert entry['completed_at'] is not None, 'Acknowledgement lost completion history'
            if scenario == 'becomes_empty':
                stopped_calls = calls.read_text()
                time.sleep(1.2)
                assert calls.read_text() == stopped_calls, 'Polling continued after closing'
            print(f'PASS: live {scenario}; real fzf, fake Herdr', flush=True)
        finally:
            if not exited:
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            os.close(fd)
