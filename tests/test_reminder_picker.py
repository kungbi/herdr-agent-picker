"""Exercise reminder dialogs inside the real picker without live notifications."""

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
from agent_reminders import ReminderStore

with tempfile.TemporaryDirectory(prefix='herdr-reminder-picker-') as directory:
    root = Path(directory)
    fake = root / 'herdr'
    fake.write_text('#!' + sys.executable + '\n' + '''import json, os, sys
from pathlib import Path
if sys.argv[1:] == ['api', 'snapshot']:
    result = {'snapshot': {
        'agents': [dict(agent='demo', pane_id='demo:p1', workspace_id='demo', tab_id='demo:t1',
                        agent_status='working', title='Fictional demo task', focused=True)],
        'workspaces': [dict(workspace_id='demo', label='Demo')],
        'tabs': [dict(tab_id='demo:t1', label='Example')]}}
elif sys.argv[1:3] == ['agent', 'focus']:
    Path(os.environ['FOCUS']).write_text(sys.argv[3])
    result = {}
else:
    raise SystemExit('Unexpected command; tests must not notify')
print(json.dumps({'result': result}))
''')
    fake.chmod(0o700)
    socket = root / 'session.sock'
    socket.touch()
    config = root / 'config.toml'
    config.write_text('')
    for scenario in ('preset_cancel', 'custom', 'custom_cancel', 'manager', 'escape', 'custom_escape', 'custom_retry', 'resize', 'empty_manager'):
        state = root / scenario
        store = ReminderStore(state)
        focus = root / f'{scenario}.focus'
        pid, fd = pty.fork()
        if pid == 0:
            environment = os.environ.copy()
            environment.update(TERM='xterm-256color', HERDR_BIN_PATH=str(fake), FOCUS=str(focus),
                               HERDR_PLUGIN_STATE_DIR=str(state), HERDR_CONFIG_PATH=str(config),
                               HERDR_SOCKET_PATH=str(socket))
            os.execve(sys.executable, [sys.executable, str(ROOT / 'agent-picker.py')], environment)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 26, 130, 0, 0))
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
            deadline = time.monotonic() + 8
            while not predicate() and time.monotonic() < deadline:
                drain()
            assert predicate(), output.decode(errors='replace')

        try:
            wait_for(lambda: b'Fictional demo task' in output)
            output.clear()
            if scenario == 'empty_manager':
                os.write(fd, b'r')
                wait_for(lambda: b'No scheduled alarms.' in output)
                os.write(fd, b'\x1b')
                time.sleep(0.15)
                output.clear()
            os.write(fd, b'a')
            wait_for(lambda: b'Custom time' in output)
            assert b'Tomorrow 09:00' in output
            assert b'\x1b[2J' not in output and b'\x1b[?1049' not in output, 'Dialog cleared the background'
            positions = re.findall(rb'\x1b\[(\d+);(\d+)H', output)
            assert positions and all(3 < int(row) < 24 and int(column) == 33 for row, column in positions), positions
            if scenario == 'resize':
                output.clear()
                fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 20, 100, 0, 0))
                wait_for(lambda: b'\x1b[4;18H' in output)
                os.write(fd, b'\x1b[B\x1b[A')
            if scenario == 'escape':
                os.write(fd, b'\x1b')
                time.sleep(0.15)
                assert not store.pending()
                os.write(fd, b'\r')
            started = time.time()
            if scenario == 'escape':
                pass
            elif scenario.startswith('custom'):
                output.clear()
                os.write(fd, b'8')
                wait_for(lambda: b'When (blank to cancel)' in output)
                if scenario == 'custom_retry':
                    output.clear()
                    os.write(fd, b'not-a-time\r')
                    wait_for(lambda: b'Use 20m' in output)
                os.write(fd, b'\x1b' if scenario == 'custom_escape' else b'\n' if scenario == 'custom_cancel' else '20분 후\n'.encode())
                if scenario in ('custom_cancel', 'custom_escape'):
                    time.sleep(0.15)
                    assert not store.pending()
                    os.write(fd, b'\x1b')
                else:
                    wait_for(lambda: bool(store.pending()))
                    assert started + 1195 < store.pending()[0]['due'] < time.time() + 1205
                    wait_for(lambda: b'[alarm ' in output)
                    os.write(fd, b'\r')
            else:
                os.write(fd, b'1')
                wait_for(lambda: bool(store.pending()))
                assert started + 295 < store.pending()[0]['due'] < time.time() + 305
                wait_for(lambda: b'[alarm ' in output)
                assert not focus.exists(), 'Setting an alarm focused the agent'
                output.clear()
                os.write(fd, b'r' if scenario == 'manager' else b'a')
                marker = b'Scheduled alarms' if scenario == 'manager' else b'Cancel current alarm'
                wait_for(lambda: marker in output)
                os.write(fd, b'\r' if scenario == 'manager' else b'9')
                wait_for(lambda: not store.pending())
                time.sleep(0.2)
                os.write(fd, b'\r')
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                done, status = os.waitpid(pid, os.WNOHANG)
                if done:
                    exited = True
                    assert os.waitstatus_to_exitcode(status) == 0, output.decode(errors='replace')
                    break
                drain()
            assert exited
            assert focus.exists() == (scenario not in ('custom_cancel', 'custom_escape'))
            if focus.exists():
                assert focus.read_text() == 'demo:p1'
            print(f'PASS: alarm {scenario}; real fzf, fictional agents', flush=True)
        finally:
            for item in store.pending():
                store.cancel(item['id'])
            if not exited:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
            os.close(fd)
