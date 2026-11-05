import errno
from datetime import datetime, timedelta
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
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
elif args[:2] == ['agent', 'get']:
    result = {'agent': {'agent': 'claude'}}
elif args[:2] == ['pane', 'read']:
    preview_text = 'PREVIEW for ' + args[2]
    if os.environ.get('PICKER_LONG_PREVIEW'):
        preview_text += '\\nLONG_PREVIEW ' + 'word ' * 40 + 'END_OF_PREVIEW'
    if os.environ.get('PICKER_PADDED_PREVIEW'):
        preview_text += '\\n' + '\\n'.join(
            (f'    ROW {number}' if number != 3 else '').ljust(289)
            + '\\x1b[0m\\x1b[38;2;22;142;142m' + ('┃' if number < 6 else '│') + '\\x1b[0m'
            for number in range(8)
        ) + '\\n    한글 들여쓰기   \\x1b[0m\\nliteral │\\nliteral ┃\\nPADDED_END'
    if '--raw' in args or args[-2:] == ['--format', 'ansi']:
        preview_text = '\\x1b[38;2;17;203;91m' + preview_text + '\\x1b[0m'
    print(preview_text)
    raise SystemExit(0)
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
    preview_calls = root / 'colored-preview.calls'
    preview_result = subprocess.run(
        [PYTHON, PICKER, '--preview', 'w1:p1'],
        env=dict(os.environ, HERDR_BIN_PATH=str(fake), PICKER_CALL_LOG=str(preview_calls)),
        text=True, capture_output=True, check=True, timeout=5,
    )
    assert preview_result.stdout == '\x1b[38;2;17;203;91mPREVIEW for w1:p1\x1b[0m\n', repr(preview_result.stdout)
    assert preview_calls.read_text().splitlines() == [
        'agent get w1:p1',
        'pane read w1:p1 --source recent-unwrapped --lines 30 --format ansi',
    ], preview_calls.read_text()
    print('PASS: preview preserves rendered ANSI colors without adding blank lines')
    record_event({'event': 'pane_agent_status_changed', 'data': {'pane_id': 'w1:p3', 'agent_status': 'done'}}, cache_path(), time.time() - 180)
    history_time = (datetime.now() - timedelta(days=1)).replace(hour=12, minute=0, second=0, microsecond=0)
    record_event({'event': 'pane_agent_status_changed', 'data': {'pane_id': 'w1:p4', 'agent_status': 'done'}}, cache_path(), history_time.timestamp())
    record_event({'event': 'pane_agent_status_changed', 'data': {'pane_id': 'w1:p4', 'agent_status': 'idle'}}, cache_path(), history_time.timestamp() + 1)

    scenarios = [
        ('preview_padded', 5, [b'\r'], 'w1:p1'),
        ('quit', 5, [b'q'], None),
        ('search_q_text', 5, [b'/', b'Agent2q', b'\r', b'\x7f', b'\r'], 'w1:p2'),
        ('search_escape_quit', 5, [b'/', b'q', b'\x1b', b'q'], None),
        # One fuzzy term avoids matching the separate '2' against a clock-dependent age.
        ('search_title', 5, [b'/', b'Agent2', b'\r'], 'w1:p2'),
        ('search_workspace', 5, [b'/', b'Test workspace', b'\r'], 'w1:p1'),
        ('search_agent_name', 5, [b'/', b'claude', b'\r'], 'w1:p1'),
        ('search_escape_empty', 5, [b'/', b'\x1b', b'\x1b'], None),
        ('search_escape_text', 5, [b'/', b'Agent2', b'\x1b', b'\x1b'], None),
        ('search_escape_number', 5, [b'/', b'Agent2', b'\x1b', b'1'], 'w1:p3'),
        ('search_escape_no_match', 5, [b'/', b'No such agent 987', b'\x1b', b'1'], 'w1:p3'),
        ('search_escape_navigation', 5, [b'/', b'Agent2', b'\x1b', b'j', b'\r'], 'w1:p1'),
        ('search_escape_preview', 5, [b'/', b'Agent2', b'\x1b', b'p', b'p', b'\r'], 'w1:p2'),
        ('search_escape_reenter', 5, [b'/', b'Agent2', b'\x1b', b'/', b'Agent4', b'\r'], 'w1:p4'),
        ('preview_default', 5, [b'\r'], 'w1:p1'),
        ('preview_move', 5, [b'j', b'\r'], 'w1:p4'),
        ('preview_toggle', 5, [b'p', b'p', b'\r'], 'w1:p1'),
        ('preview_hidden', 5, [b'p', b'j', b'\r'], 'w1:p4'),
        ('preview_long_line', 5, [b'\r'], 'w1:p1'),
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
        opened_day = datetime.now().date()
        pid, fd = pty.fork()
        if pid == 0:
            environment = os.environ.copy()
            environment.update(TERM='xterm-256color', COLORTERM='truecolor', HERDR_BIN_PATH=str(fake), HERDR_CONFIG_PATH=str(config), PICKER_FOCUS_LOG=str(focus_log), PICKER_CALL_LOG=str(call_log), PICKER_AGENT_COUNT=str(count))
            if scenario == 'preview_long_line':
                environment['PICKER_LONG_PREVIEW'] = '1'
            if scenario == 'preview_padded':
                environment['PICKER_PADDED_PREVIEW'] = '1'
            os.execve(PYTHON, [PYTHON, PICKER], environment)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
        output = bytearray()
        deadline = time.monotonic() + 8
        status = None
        try:
            ready_item = b'Agent 5' if count <= 12 else b'Agent 10'
            while (ready_item not in output or b'PREVIEW for w1:p1' not in output) and time.monotonic() < deadline:
                if select.select([fd], [], [], 0.1)[0]:
                    output.extend(os.read(fd, 65536))
            assert ready_item in output, output.decode(errors='replace')
            assert b'PREVIEW for w1:p1' in output, 'Preview is not visible by default'
            ready_ms = (time.monotonic() - opened_at) * 1000
            calls = call_log.read_text().splitlines()
            data_calls = [call for call in calls if not call.startswith(('pane read ', 'agent get '))]
            assert data_calls in (["api snapshot"], ["agent list", "workspace list", "tab list"]), calls
            assert 'pane read w1:p1 --source recent-unwrapped --lines 30 --format ansi' in calls, calls
            assert not focus_log.exists(), 'Opening the picker changed focus'
            if count <= 12:
                assert b'3m ago' in output, 'Completion age is missing'
                # Rendering may cross midnight: this fixture then moves from
                # Yesterday to its ISO date, just as real completion history does.
                history_labels = {
                    {0: 'Today', 1: 'Yesterday'}.get((day - history_time.date()).days, history_time.date().isoformat()).encode()
                    for day in (opened_day, datetime.now().date())
                }
                history_heading = next((heading for heading in history_labels if heading in output), None)
                assert history_heading is not None, f'Missing history section: {history_labels}'
                for heading in (b'Active', b'Done', b'Other'):
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
                assert plain.index(b'Active') < plain.index(history_heading), 'Active must appear above idle history'
                assert plain.index(b'[working]') < plain.index(b'[idle]'), 'Idle must not precede Active'
                numbers = re.findall(rb'\[(\d+)\]', plain)
                expected_numbers = [str(number).encode() for number in range(1, min(count, 9) + 1)]
                assert numbers == expected_numbers, numbers
            for keys in key_steps[:-1]:
                if scenario.startswith('search_') and keys == b'/':
                    while select.select([fd], [], [], 0.05)[0]:
                        os.read(fd, 65536)
                    output.clear()
                os.write(fd, keys)
                step_deadline = time.monotonic() + (0.6 if scenario.startswith('search_') else 0.15)
                while time.monotonic() < step_deadline:
                    if select.select([fd], [], [], 0.05)[0]:
                        try:
                            output.extend(os.read(fd, 65536))
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                            break
                assert os.waitpid(pid, os.WNOHANG)[0] == 0, f'{scenario}: picker closed before the final action'
                if scenario.startswith('search_') and keys == b'/':
                    while select.select([fd], [], [], 0.05)[0]:
                        output.extend(os.read(fd, 65536))
                    assert b'!^' not in output, repr(output[-4000:])
                assert not focus_log.exists(), 'Partial input or navigation changed focus'
            if scenario in ('preview_default', 'preview_move', 'preview_toggle', 'preview_long_line', 'preview_padded', 'search_escape_preview'):
                preview_text = f'PREVIEW for {expected}'.encode()
                until = time.monotonic() + 3
                while preview_text not in output and time.monotonic() < until:
                    if select.select([fd], [], [], 0.1)[0]:
                        output.extend(os.read(fd, 65536))
                assert preview_text in output, output.decode(errors='replace')
                assert re.search(rb'\x1b\[[0-9;]*38;2;17;203;91(?:;[0-9;]*)?m', output), 'Preview color was lost'
                plain_preview = re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', output)
                assert b'[38;2;17;203;91m' not in plain_preview, 'ANSI fragments are visible'
            if scenario == 'preview_long_line':
                until = time.monotonic() + 3
                while b'END_OF_PREVIEW' not in output and time.monotonic() < until:
                    if select.select([fd], [], [], 0.1)[0]:
                        output.extend(os.read(fd, 65536))
                assert b'LONG_PREVIEW' in output and b'END_OF_PREVIEW' in output, 'Long preview line was clipped'
            if scenario == 'preview_hidden':
                assert not any(call.startswith('pane read w1:p4 ') for call in call_log.read_text().splitlines()), 'Hidden preview still reads panes'
            if scenario == 'preview_padded':
                until = time.monotonic() + 3
                while b'PADDED_END' not in output and time.monotonic() < until:
                    if select.select([fd], [], [], 0.1)[0]:
                        output.extend(os.read(fd, 65536))
                assert b'PADDED_END' in output, 'Right-edge padding wrapped into blank rows and pushed content off screen'
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
                assert focus_log.read_text() == expected, (scenario, focus_log.read_text(), output.decode(errors='replace')[-3000:])
            print(f'PASS: {scenario}; ready {ready_ms:.1f} ms; {len(data_calls)} snapshot request(s); real fzf, fake Herdr')
        finally:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.close(fd)

    padded_environment = dict(os.environ, HERDR_BIN_PATH=str(fake),
                              PICKER_CALL_LOG=str(root / 'padded-output.calls'), PICKER_PADDED_PREVIEW='1')
    padded_result = subprocess.run(
        [PYTHON, PICKER, '--preview', 'w1:p1'], env=padded_environment,
        text=True, capture_output=True, check=True, timeout=5,
    ).stdout
    original_padded = subprocess.run(
        [str(fake), 'pane', 'read', 'w1:p1', '--format', 'ansi'], env=padded_environment,
        text=True, capture_output=True, check=True, timeout=5,
    ).stdout
    styles = re.compile(r'\x1b\[[0-9;:]*m')
    assert styles.findall(padded_result) == styles.findall(original_padded), 'Padding cleanup changed ANSI styles'
    assert styles.sub('', padded_result) == (
        'PREVIEW for w1:p1\n' + '\n'.join(f'    ROW {number}' if number != 3 else '' for number in range(8))
        + '\n    한글 들여쓰기\nliteral │\nliteral ┃\nPADDED_END\n'
    ), repr(padded_result)
    print('PASS: padding cleanup preserves colors, Korean indentation, blank lines and literal pipes')
