import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import sys
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from agent_completion_times import cache_path, record_event

PYTHON = sys.executable
PICKER = sys.argv[1] if len(sys.argv) > 1 else str(ROOT / 'agent-picker.py')

with tempfile.TemporaryDirectory(prefix='herdr-picker-test-') as directory:
    root = Path(directory)
    fake = root / 'herdr'
    fake.write_text('#!' + sys.executable + '\n' + '''import json, os, sys
from pathlib import Path
args = sys.argv[1:]
with Path(os.environ['PICKER_CALL_LOG']).open('a') as log:
    log.write(' '.join(args) + '\\n')
if args == ['api', 'snapshot']:
    result = {'type': 'session_snapshot', 'snapshot': {
        'agents': [dict(agent='claude', agent_status=status, workspace_id='w1', tab_id='w1:t1', pane_id=f'w1:p{i}', focused=i == 1, title=f'Agent {i} · gpt-6.1-sol · ~') for i, status in enumerate(['working', 'blocked', 'done', 'idle', 'unknown'] + ['idle'] * (int(os.environ.get('PICKER_AGENT_COUNT', '5')) - 5), start=1)],
        'workspaces': [{'workspace_id': 'w1', 'label': 'Test workspace'}],
        'tabs': [{'tab_id': 'w1:t1', 'label': 'Test tab'}],
    }}
elif args == ['agent', 'list']:
    result = {'agents': [dict(agent='claude', agent_status=status, workspace_id='w1', tab_id='w1:t1', pane_id=f'w1:p{i}', focused=i == 1, title=f'Agent {i}') for i, status in enumerate(['working', 'blocked', 'done', 'idle', 'unknown'] + ['idle'] * (int(os.environ.get('PICKER_AGENT_COUNT', '5')) - 5), start=1)]}
elif args == ['workspace', 'list']:
    result = {'workspaces': [{'workspace_id': 'w1', 'label': 'Test workspace'}]}
elif args == ['tab', 'list']:
    result = {'tabs': [{'tab_id': 'w1:t1', 'label': 'Test tab'}]}
elif args[:2] == ['agent', 'focus']:
    Path(os.environ['PICKER_FOCUS_LOG']).write_text(args[2])
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
    os.environ.update(HERDR_SOCKET_PATH=str(socket_path), HERDR_CONFIG_PATH=str(config), HERDR_PLUGIN_STATE_DIR=str(root / 'plugin-state'))
    record_event({'event': 'pane_agent_status_changed', 'data': {'pane_id': 'w1:p3', 'agent_status': 'done'}}, cache_path(), time.time() - 180)
    record_event({'event': 'pane_agent_status_changed', 'data': {'pane_id': 'w1:p4', 'agent_status': 'done'}}, cache_path(), time.time() - 86400)
    record_event({'event': 'pane_agent_status_changed', 'data': {'pane_id': 'w1:p4', 'agent_status': 'idle'}}, cache_path(), time.time() - 86399)

    scenarios = [
        ('select', 5, [b'jkj', b'\r'], 'w1:p4'),
        ('cancel', 5, [b'jkj', b'\x1b'], None),
        ('cross_up', 5, [b'jk', b'\r'], 'w1:p1'),
        ('arrows', 5, [b'\x1b[B\x1b[A\x1b[B', b'\r'], 'w1:p4'),
        ('number_first', 5, [b'1'], 'w1:p3'),
        ('number_date_section', 5, [b'4'], 'w1:p4'),
        ('number_last', 5, [b'5'], 'w1:p5'),
        ('number_missing', 5, [b'09', b'3'], 'w1:p1'),
        ('number_first_large_list', 12, [b'1'], 'w1:p3'),
        ('number_ninth', 12, [b'9'], 'w1:p10'),
        ('number_zero_ignored', 12, [b'0', b'9'], 'w1:p10'),
        ('navigate_tenth', 12, [b'jjjjjjj', b'\r'], 'w1:p11'),
        ('navigate_last', 12, [b'jjjjjjjjj', b'\r'], 'w1:p5'),
        ('jump_down', 40, [b'\x04', b'\r'], 'w1:p9'),
        ('jump_up', 40, [b'\x04\x15', b'\r'], 'w1:p1'),
        ('jump_small_down', 5, [b'\x04', b'\r'], 'w1:p5'),
        ('jump_small_up', 5, [b'\x15', b'\r'], 'w1:p3'),
        ('jump_up_top', 40, [b'\x15', b'\r'], 'w1:p3'),
        ('jump_down_end', 40, [b'\x04' * 8, b'\r'], 'w1:p5'),
        ('jump_up_end', 40, [b'\x04' * 8 + b'\x15', b'\r'], 'w1:p36'),
    ]
    for scenario, count, key_steps, expected in scenarios:
        focus_log = root / f'{scenario}.focus'
        call_log = root / f'{scenario}.calls'
        opened_at = time.monotonic()
        pid, fd = pty.fork()
        if pid == 0:
            environment = os.environ.copy()
            environment.update(TERM='xterm-256color', HERDR_BIN_PATH=str(fake), HERDR_CONFIG_PATH=str(config), PICKER_FOCUS_LOG=str(focus_log), PICKER_CALL_LOG=str(call_log), PICKER_AGENT_COUNT=str(count))
            os.execve(PYTHON, [PYTHON, PICKER], environment)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
        output = bytearray()
        deadline = time.monotonic() + 8
        status = None
        try:
            ready_item = b'Agent 5' if count <= 12 else b'Agent 10'
            while ready_item not in output and time.monotonic() < deadline:
                if select.select([fd], [], [], 0.1)[0]:
                    output.extend(os.read(fd, 65536))
            assert ready_item in output, output.decode(errors='replace')
            ready_ms = (time.monotonic() - opened_at) * 1000
            calls = call_log.read_text().splitlines()
            assert calls in (["api snapshot"], ["agent list", "workspace list", "tab list"]), calls
            assert not focus_log.exists(), 'Opening the picker changed focus'
            if count <= 12:
                assert b'3m ago' in output, 'Completion age is missing'
                for heading in (b'Active', b'Done', b'Yesterday', b'Other'):
                    assert heading in output, f'Missing section: {heading}'
                assert b'gpt-6.1-sol' not in output, 'Model metadata is visible'
                visible_states = (b'working', b'blocked', b'done', b'idle', b'unknown')
                for state in visible_states:
                    assert b'[' + state + b']' in output, state
                assert b'38;' in output, 'No ANSI foreground colors rendered'
                state_colors = {state: color for color, state in re.findall(rb'\x1b\[([0-9;]*)m[^\x1b]*\[(working|blocked|done|idle|unknown)\]', output)}
                assert len(set(state_colors.values())) == len(visible_states), state_colors
                plain = re.sub(rb'\x1b\[[0-9;]*m', b'', output)
                assert plain.index(b'Done') < plain.index(b'Active'), 'Done must appear above Active'
                assert plain.index(b'Active') < plain.index(b'Yesterday'), 'Active must appear above idle history'
                assert plain.index(b'[working]') < plain.index(b'[idle]'), 'Idle must not precede Active'
                numbers = re.findall(rb'\[(\d+)\]', plain)
                expected_numbers = [str(number).encode() for number in range(1, min(count, 9) + 1)]
                assert numbers == expected_numbers, numbers
            for keys in key_steps[:-1]:
                os.write(fd, keys)
                time.sleep(0.15)
                assert not focus_log.exists(), 'Partial input or navigation changed focus'
            os.write(fd, key_steps[-1])
            while time.monotonic() < deadline:
                done, status = os.waitpid(pid, os.WNOHANG)
                if done:
                    break
                if select.select([fd], [], [], 0.1)[0]:
                    try:
                        output.extend(os.read(fd, 65536))
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
            else:
                raise AssertionError('Picker did not exit')
            assert os.waitstatus_to_exitcode(status) == 0, output.decode(errors='replace')
            if expected is None:
                assert not focus_log.exists(), 'Escape changed focus'
            else:
                assert focus_log.read_text() == expected, focus_log.read_text()
            print(f'PASS: {scenario}; ready {ready_ms:.1f} ms; {len(calls)} data request(s); real fzf, fake Herdr')
        finally:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.close(fd)
