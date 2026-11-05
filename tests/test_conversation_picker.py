"""Real fzf/PTY and process metadata, synthetic Hermes SQLite conversations only."""

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

from test_preview import ROOT, add_message, make_database

with tempfile.TemporaryDirectory(prefix='herdr-conversation-picker-') as directory:
    root = Path(directory)
    home = root / 'profiles' / 'work'
    connection = make_database(home)
    text = 'FIRST_RESPONSE\n' + '한글 읽기 쉬운 미리보기 ' * 6 + '\n    code │ literal ┃\nWRAP_END'
    add_message(connection, 'Fictional request', role='user')
    add_message(connection, text)
    add_message(connection, 'SECOND_RESPONSE', session='second')
    add_message(connection, 'NEW_SESSION_RESPONSE', session='third')
    processes, markers, pids = [], {}, {}
    for number, session_id in ((1, 'first'), (2, 'second')):
        marker = home / f'active-{number}.json'
        marker.write_text(json.dumps({'session_id': session_id}))
        process = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(180)'],
                                   env=dict(os.environ, HERMES_HOME=str(home), HERMES_TUI_ACTIVE_SESSION_FILE=str(marker)))
        processes.append(process)
        markers[number] = marker
        pids[f'w1:p{number}'] = process.pid
    pid_file = root / 'pids.json'
    pid_file.write_text(json.dumps(pids))
    fake = root / 'herdr'
    fake.write_text('#!' + sys.executable + '\n' + '''import json, os, sys
from pathlib import Path
args = sys.argv[1:]
with Path(os.environ['CALLS']).open('a') as log:
    log.write(' '.join(args) + '\\n')
if args == ['api', 'snapshot']:
    result = {'snapshot': {
        'agents': [dict(agent='hermes', pane_id=f'w1:p{i}', workspace_id='w1', tab_id='w1:t1',
                        agent_status='working', title=f'Fixture {i}', focused=i == 1) for i in (1, 2)],
        'workspaces': [dict(workspace_id='w1', label='Fixtures')],
        'tabs': [dict(tab_id='w1:t1', label='Test')]}}
elif args[:2] == ['agent', 'get']:
    result = {'agent': {'agent': 'hermes'}}
elif args[:3] == ['pane', 'process-info', '--pane']:
    pid = json.loads(Path(os.environ['PIDS']).read_text())[args[3]]
    result = {'process_info': {'foreground_processes': [{'pid': pid}]}}
elif args[:2] == ['agent', 'focus']:
    Path(os.environ['FOCUS']).write_text(args[2])
    result = {}
elif args[:2] == ['pane', 'read']:
    print('FORBIDDEN_TERMINAL_FRAME'.ljust(289) + '┃')
    raise SystemExit(0)
else:
    raise SystemExit(2)
print(json.dumps({'result': result}))
''')
    fake.chmod(0o700)
    config = root / 'config.toml'
    config.write_text('')
    socket = root / 'session.sock'
    socket.touch()
    try:
        for columns in (72, 100, 160):
            calls, focus = root / f'{columns}.calls', root / f'{columns}.focus'
            pid, fd = pty.fork()
            if pid == 0:
                environment = dict(os.environ, TERM='xterm-256color', COLORTERM='truecolor',
                                   HERDR_BIN_PATH=str(fake), HERDR_CONFIG_PATH=str(config),
                                   HERDR_PLUGIN_STATE_DIR=str(root / f'state-{columns}'),
                                   HERDR_SOCKET_PATH=str(socket), PIDS=str(pid_file), CALLS=str(calls), FOCUS=str(focus))
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
                assert b'FIRST_RESPONSE' in output
                assert '한글'.encode() in output
                assert re.search(rb'\x1b\[[0-9;]*38;2;139;233;253(?:;[0-9;]*)?m', output)
                assert not focus.exists()
                assert not any(line.startswith('pane read ') for line in calls.read_text().splitlines())
                if columns == 100:
                    # Same session, same row id, same agent status: still refresh.
                    output.clear()
                    connection.execute("UPDATE messages SET content='UPDATED_SAME_ROW' WHERE content=?", (text,))
                    connection.commit()
                    wait_for(b'UPDATED_SAME_ROW')
                    output.clear()
                    os.write(fd, b'j')
                    wait_for(b'SECOND_RESPONSE')
                    output.clear()
                    markers[2].write_text(json.dumps({'session_id': 'third'}))
                    wait_for(b'NEW_SESSION_RESPONSE')
                    os.write(fd, b'p')
                    drain(0.5)
                    previous_reads = sum(line.startswith(('agent get ', 'pane process-info ')) for line in calls.read_text().splitlines())
                    drain(2.1)
                    current_reads = sum(line.startswith(('agent get ', 'pane process-info ')) for line in calls.read_text().splitlines())
                    assert current_reads == previous_reads, 'Hidden preview continued reading conversations'
                    connection.execute("UPDATE messages SET content='AFTER_HIDDEN' WHERE session_id='third'")
                    connection.commit()
                    output.clear()
                    os.write(fd, b'p')
                    wait_for(b'AFTER_HIDDEN')
                    # Resize without changing the original agent terminal.
                    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 28, 72, 0, 0))
                    drain(0.3)
                    assert os.waitpid(pid, os.WNOHANG)[0] == 0
                    markers[2].write_text(json.dumps({'session_id': 'second'}))
                    connection.execute("UPDATE messages SET content=? WHERE content='UPDATED_SAME_ROW'", (text,))
                    connection.commit()
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
                assert 'FORBIDDEN_TERMINAL_FRAME' not in output.decode(errors='replace')
                print(f'PASS: semantic preview at {columns} columns; real fzf, synthetic session records')
            finally:
                if not exited:
                    try:
                        os.kill(pid, signal.SIGKILL)
                        os.waitpid(pid, 0)
                    except ProcessLookupError:
                        pass
                os.close(fd)
    finally:
        connection.close()
        for process in processes:
            process.terminate()
            process.wait(timeout=5)
