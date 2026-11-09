"""Readable preview controls in real fzf PTYs, using fictional Herdr only."""
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

with tempfile.TemporaryDirectory(prefix='herdr-preview-controls-test-') as directory:
    root = Path(directory)
    source = root / 'screen'
    initial = ('TOP_HIDDEN\n' + ''.join(f'ROW_{n:02d}\n' for n in range(1, 28))
               + '\x1b[31mLEFT_EDGE ' + '한글e\u0301' * 12 + ' FULL_WIDTH_END\x1b[0m\nLATEST_BOTTOM\n')
    source.write_text(initial)
    calls, focus = root / 'calls', root / 'focus'
    fake = root / 'herdr'
    fake.write_text('#!' + sys.executable + '\n' + '''import json, os, sys
from pathlib import Path
args = sys.argv[1:]
with Path(os.environ['CALLS']).open('a') as log:
    log.write(' '.join(args) + '\\n')
if args == ['api', 'snapshot']:
    result = {'snapshot': {
        'agents': [dict(agent='hermes', pane_id='w1:p1', workspace_id='w1', tab_id='w1:t1',
                        agent_status='working', title='wrap preview', focused=True)],
        'workspaces': [dict(workspace_id='w1', label='Fictional')],
        'tabs': [dict(tab_id='w1:t1', label='Test')]}}
elif args == ['pane', 'read', 'w1:p1', '--source', 'recent-unwrapped', '--lines', '30', '--format', 'ansi']:
    sys.stdout.write(Path(os.environ['SOURCE']).read_text())
    raise SystemExit(0)
elif args == ['agent', 'focus', 'w1:p1']:
    Path(os.environ['FOCUS']).write_text(args[2])
    result = {}
else:
    raise SystemExit('Unexpected mutation/API call: ' + repr(args))
print(json.dumps({'result': result}))
''')
    fake.chmod(0o700)
    config = root / 'config.toml'
    config.write_text('')
    socket = root / 'session.sock'
    socket.touch()
    environment = dict(os.environ, TERM='xterm-256color', COLORTERM='truecolor',
                       HERDR_BIN_PATH=str(fake), HERDR_CONFIG_PATH=str(config),
                       HERDR_PLUGIN_STATE_DIR=str(root / 'state'), HERDR_SOCKET_PATH=str(socket),
                       SOURCE=str(source), CALLS=str(calls), FOCUS=str(focus))
    before = set(Path(tempfile.gettempdir()).glob('herdr-agent-picker-*'))
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(sys.executable, [sys.executable, str(ROOT / 'agent-picker.py')], environment)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
    output = bytearray()
    exited = False

    def drain(seconds=0.15):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if select.select([fd], [], [], 0.025)[0]:
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
        assert marker in output, (marker, output.decode(errors='replace')[-5000:])

    def keys(value):
        drain()
        output.clear()
        os.write(fd, value)
        drain(0.4)
        assert not focus.exists(), 'Preview control changed focus'

    try:
        wait_for(b'LATEST_BOTTOM')
        wait_for('이미지는 Enter로 원본 확인'.encode())
        assert b'TOP_HIDDEN' not in output, 'Preview did not start at the bottom'
        assert b'LEFT_EDGE' in output
        assert b'FULL_WIDTH_END' not in output, 'Default preview unexpectedly wraps'
        assert re.search(rb'\x1b\[[0-9;]*31(?:;[0-9;]*)?m', output), 'ANSI color lost'
        print('PASS: initial bottom/latest, nowrap, ANSI; real fzf')

        keys(b'w')
        wait_for(b'FULL_WIDTH_END')
        keys(b'w')
        assert b'LEFT_EDGE' in output and b'FULL_WIDTH_END' not in output, 'Wrap did not turn off'
        print('PASS: w toggles wrapping both ways')

        keys(b'\x10')
        wait_for(b'FULL_WIDTH_END')
        keys(b'\x10')
        assert b'LEFT_EDGE' in output and b'FULL_WIDTH_END' not in output, 'Full preview did not return to right 60%'
        print('PASS: Ctrl+P expands only popup preview and restores right 60%')

        keys(b'\x1b[C' * 8)
        wait_for(b'FULL_WIDTH_END')
        assert '한글'.encode() in output, 'Pan lost CJK text'
        assert re.search(rb'\x1b\[[0-9;]*31(?:;[0-9;]*)?m', output), 'Pan lost ANSI color'
        keys(b'\x1b[D' * 7)
        keys(b'\x1b[D')
        wait_for(b'LEFT_EDGE')
        assert b'FULL_WIDTH_END' not in output, 'Pan did not return to left edge'
        print('PASS: left/right pan and return with ANSI/CJK text')

        source.write_text(initial.replace('LATEST_BOTTOM', 'LATEST_REFRESHED'))
        output.clear()
        wait_for(b'LATEST_REFRESHED')
        assert b'TOP_HIDDEN' not in output
        keys(b'\x1b[1;2A' * 8)
        wait_for(b'ROW_05')
        # Manual review must survive more than two automatic refresh intervals.
        output.clear()
        drain(2.3)
        assert b'LATEST_REFRESHED' not in output, 'Refresh forced manual preview scroll back to bottom'
        keys(b'\x1b[1;2F')
        wait_for(b'LATEST_REFRESHED')
        source.write_text(initial.replace('LATEST_BOTTOM', 'AFTER_REVIEW'))
        output.clear()
        wait_for(b'AFTER_REVIEW')
        print('PASS: periodic latest refresh, manual scroll held, Shift+End resumes live updates')

        # Herdr's text-only snapshot loses image placement metadata. Compact
        # blank reserves, without inventing an image at a guessed position.
        source.write_text('BEFORE_BLANK_RESERVE\n' + '\n' * 27 + 'AFTER_BLANK_RESERVE\n')
        output.clear()
        wait_for(b'AFTER_BLANK_RESERVE')
        assert b'BEFORE_BLANK_RESERVE' in output, 'Large blank reserve still hides earlier context'
        # The static border hint was verified on initial draw; fzf does not
        # redraw an unchanged border when only preview contents refresh.
        print('PASS: long blank reserves compacted with an honest image hint; real fzf')
        source.write_text(initial.replace('LATEST_BOTTOM', 'AFTER_REVIEW'))
        output.clear()
        wait_for(b'AFTER_REVIEW')

        # w/p/r/a/q stay ordinary query characters; left/right stay cursor keys.
        keys(b'/')
        keys(b'wrap')
        wait_for(b'LEFT_EDGE')
        keys(b'\x1b[D' * 4 + b'w')
        keys(b'\x7f' + b'\x1b[C' * 4)
        keys(b'qa')
        assert not focus.exists()
        keys(b'\x7f\x7f')
        keys(b'\x1b')
        # Esc must rebind the new normal-mode shortcut too.
        keys(b'w')
        wait_for(b'FULL_WIDTH_END')
        print('PASS: new normal shortcuts do not consume search text/cursor keys; Esc restores w')

        keys(b'p')
        reads = calls.read_text().splitlines().count('pane read w1:p1 --source recent-unwrapped --lines 30 --format ansi')
        drain(2.2)
        assert calls.read_text().splitlines().count('pane read w1:p1 --source recent-unwrapped --lines 30 --format ansi') == reads
        keys(b'p')
        wait_for(b'AFTER_REVIEW')
        os.write(fd, b'\r')
        until = time.monotonic() + 5
        while time.monotonic() < until:
            done, status = os.waitpid(pid, os.WNOHANG)
            if done:
                exited = True
                assert os.waitstatus_to_exitcode(status) == 0
                break
            drain()
        assert exited and focus.read_text() == 'w1:p1', 'Enter selection changed'
        assert all(line in ('api snapshot', 'agent focus w1:p1',
                            'pane read w1:p1 --source recent-unwrapped --lines 30 --format ansi')
                   for line in calls.read_text().splitlines()), calls.read_text()
        assert set(Path(tempfile.gettempdir()).glob('herdr-agent-picker-*')) == before, 'Popup state not cleaned up'
        assert source.read_text() == initial.replace('LATEST_BOTTOM', 'AFTER_REVIEW'), 'Source snapshot was mutated'
        print('PASS: hide/show, original Enter target, source read-only calls, popup state cleanup')
    finally:
        if not exited:
            try:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
            except ProcessLookupError:
                pass
        os.close(fd)
