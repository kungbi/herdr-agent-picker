"""Codex/Hermes native ANSI previews through real fzf; no live agent input."""

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
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata

ROOT = Path(__file__).resolve().parents[1]
STYLES = re.compile(r'\x1b\[[0-9;:]*m')


def framed_snapshot(text, columns):
    rows = []
    for number, line in enumerate(text.splitlines()):
        width = sum(0 if unicodedata.category(character) in ('Mn', 'Me', 'Cf') else
                    2 if unicodedata.east_asian_width(character) in ('W', 'F') else 1
                    for character in STYLES.sub('', line))
        assert width < columns
        rows.append(line + ' ' * (columns - width - 1)
                    + '\x1b[0m\x1b[38;2;22;142;142m'
                    + ('│' if number % 2 else '┃') + '\x1b[0m\n')
    return ''.join(rows)


with tempfile.TemporaryDirectory(prefix='herdr-native-preview-') as directory:
    root = Path(directory)
    fixture = ('\x1b[1;38;2;139;233;253mFIRST_RESPONSE\x1b[0m\n\n'
               '\x1b[1m“응답 전송은 에이전트가 담당”\x1b[0m는 설명\n'
               '\x1b[38;2;241;250;140m    value = "**CODE_LITERAL**"\x1b[0m\n'
               '│ Name │ Status │\n│ task │ ready  │\n'
               + ('한글 미리보기 ' * 6).rstrip() + '\nWRAP_END\n')
    snapshots = root / 'snapshots.json'
    framed_fixture = framed_snapshot(fixture, 96)
    snapshots.write_text(json.dumps({'w1:p1': framed_fixture, 'w1:p2': 'SECOND_RESPONSE\n'}))
    fake = root / 'herdr'
    fake.write_text('#!' + sys.executable + '\n' + '''import json, os, sys
from pathlib import Path
args = sys.argv[1:]
with Path(os.environ['CALLS']).open('a') as log:
    log.write(' '.join(args) + '\\n')
if args == ['api', 'snapshot']:
    result = {'snapshot': {
        'agents': [dict(agent=kind, pane_id=f'w1:p{number}', workspace_id='w1',
                        tab_id='w1:t1', agent_status='working',
                        title=f'Fixture {number}', focused=number == 1)
                   for number, kind in ((1, 'hermes'), (2, 'codex'))],
        'workspaces': [dict(workspace_id='w1', label='Fixtures')],
        'tabs': [dict(tab_id='w1:t1', label='Test')]}}
elif args[:2] == ['pane', 'get']:
    result = {'pane': {'terminal_id': args[2]}}
elif args[:2] == ['terminal', 'attach']:
    import time
    print('\\x1b[2J', flush=True)
    while True: time.sleep(1)
elif args[:2] == ['pane', 'read']:
    assert args[3:] == ['--source', 'recent-unwrapped', '--lines', '30', '--format', 'ansi']
    sys.stdout.write(json.loads(Path(os.environ['SNAPSHOTS']).read_text())[args[2]])
    raise SystemExit(0)
elif args[:2] == ['agent', 'focus']:
    Path(os.environ['FOCUS']).write_text(args[2])
    result = {}
else:
    raise SystemExit(2)
print(json.dumps({'result': result}))
''')
    fake.chmod(0o700)
    config = root / 'config.toml'
    config.write_text('')
    socket = root / 'session.sock'
    socket.touch()

    for columns in (72, 100, 160):
        calls, focus = root / f'{columns}.calls', root / f'{columns}.focus'
        environment = dict(os.environ, TERM='xterm-256color', COLORTERM='truecolor',
                           HERDR_BIN_PATH=str(fake), HERDR_CONFIG_PATH=str(config),
                           HERDR_PLUGIN_STATE_DIR=str(root / f'state-{columns}'),
                           HERDR_SOCKET_PATH=str(socket), SNAPSHOTS=str(snapshots),
                           CALLS=str(calls), FOCUS=str(focus))
        if columns == 72:
            content = '    한글 e\u0301 **CODE_LITERAL** │'
            content_width = sum(0 if unicodedata.category(character) in ('Mn', 'Me', 'Cf') else
                                2 if unicodedata.east_asian_width(character) in ('W', 'F') else 1
                                for character in content)
            cases = [(framed_snapshot(content + '\n\n', content_width + padding + 1), content + '\n\n')
                     for padding in (0, 1, 2, 8, 31, 32)]
            literal_table = '│ value' + ' ' * 40 + '│\n    literal ┃\n'
            cases.append((literal_table, literal_table))
            cases.append(('    code  \x1b[0m  \x1b[31m \x1b[0m\n', '    code\n'))
            for source, expected in cases:
                snapshots.write_text(json.dumps({'w1:p1': source, 'w1:p2': 'SECOND_RESPONSE\n'}))
                rendered = subprocess.run(
                    [sys.executable, str(ROOT / 'agent-picker.py'), '--preview', 'w1:p1'],
                    env=environment, text=True, capture_output=True, check=True, timeout=5,
                ).stdout
                assert STYLES.sub('', rendered) == expected, repr(rendered)
                assert STYLES.findall(rendered) == STYLES.findall(source)
            snapshots.write_text(json.dumps({'w1:p1': framed_fixture, 'w1:p2': 'SECOND_RESPONSE\n'}))
            print('PASS: zero/short frame padding, combining Korean text, literal tables and interleaved ANSI padding')
        for pane_id in ('w1:p1', 'w1:p2'):
            rendered = subprocess.run(
                [sys.executable, str(ROOT / 'agent-picker.py'), '--preview', pane_id],
                env=environment, text=True, capture_output=True, check=True, timeout=5,
            ).stdout
            source = json.loads(snapshots.read_text())[pane_id]
            expected = fixture if pane_id == 'w1:p1' else source
            assert STYLES.sub('', rendered) == STYLES.sub('', expected), repr(rendered)
            assert STYLES.findall(rendered) == STYLES.findall(source)
            assert len(rendered.splitlines()) == len(source.splitlines())
        pid, fd = pty.fork()
        if pid == 0:
            os.execve(sys.executable, [sys.executable, str(ROOT / 'agent-picker.py')], environment)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 28, columns, 0, 0))
        output = bytearray()
        exited = False

        def drain(seconds=0.1):
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                if select.select([fd], [], [], 0.03)[0]:
                    try:
                        output.extend(os.read(fd, 65536))
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        return

        def wait_for(marker):
            until = time.monotonic() + 8
            while marker not in output and time.monotonic() < until:
                drain()
            assert marker in output, output.decode(errors='replace')[-10000:]

        try:
            wait_for(b'WRAP_END')
            visible = re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', output).replace(b'\r', b'')
            assert b'FIRST_RESPONSE' in visible and b'**CODE_LITERAL**' in visible
            assert '한글'.encode() in output
            assert re.search(rb'\x1b\[[0-9;]*38;2;139;233;253(?:;[0-9;]*)?m', output)
            assert b'Saved conversation' not in output and b'ASSISTANT' not in output
            assert not focus.exists()
            if columns == 100:
                changed = dict(json.loads(snapshots.read_text()), **{'w1:p1': 'UPDATED_NATIVE_SCREEN\n'})
                snapshots.write_text(json.dumps(changed))
                output.clear()
                wait_for(b'UPDATED_NATIVE_SCREEN')
                output.clear()
                os.write(fd, b'j')
                wait_for(b'SECOND_RESPONSE')
                os.write(fd, b'p')
                drain(0.5)
                previous_reads = sum(line.startswith('pane read ') for line in calls.read_text().splitlines())
                drain(2.1)
                current_reads = sum(line.startswith('pane read ') for line in calls.read_text().splitlines())
                assert current_reads == previous_reads, 'Hidden preview continued reading panes'
                snapshots.write_text(json.dumps({'w1:p1': framed_fixture, 'w1:p2': 'AFTER_HIDDEN\n'}))
                output.clear()
                os.write(fd, b'p')
                wait_for(b'AFTER_HIDDEN')
                fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 28, 72, 0, 0))
                drain(0.3)
                assert os.waitpid(pid, os.WNOHANG)[0] == 0
                snapshots.write_text(json.dumps({'w1:p1': framed_fixture, 'w1:p2': 'SECOND_RESPONSE\n'}))
            os.write(fd, b'q')
            until = time.monotonic() + 4
            while time.monotonic() < until:
                done, status = os.waitpid(pid, os.WNOHANG)
                if done:
                    exited = True
                    assert os.waitstatus_to_exitcode(status) == 0
                    break
                drain()
            assert exited, 'Picker failed to close'
            assert not focus.exists(), 'Preview verification changed focus'
            assert all(line == 'api snapshot' or line.startswith(('pane read ', 'pane get ', 'terminal attach '))
                       for line in calls.read_text().splitlines()), calls.read_text()
            print(f'PASS: native Codex/Hermes parity at {columns} columns; real fzf, synthetic terminal snapshots')
        finally:
            if not exited:
                try:
                    os.kill(pid, signal.SIGKILL)
                    os.waitpid(pid, 0)
                except ProcessLookupError:
                    pass
            os.close(fd)
