"""Exercise reminder dialogs inside the real picker without live notifications."""

from datetime import datetime, timedelta
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
    for scenario in ('preset_cancel', 'custom', 'custom_cancel', 'manager', 'escape', 'custom_escape', 'custom_retry', 'resize', 'empty_manager', 'jk', 'delayed_jk', 'delayed_arrows', 'delayed_custom'):
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
            assert predicate(), output.decode(errors='replace')[-8000:]

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
            assert b'\x1b[7m' not in output, 'White reverse-video highlight returned'
            assert b'\x1b[0;39;48;5;236m' in output
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
            elif scenario.startswith('custom') or scenario == 'delayed_custom':
                output.clear()
                os.write(fd, b'8')
                wait_for(lambda: b'Date and time (local)' in output)
                assert b'Date:' in output and b'Time:' in output
                target = (datetime.now() + timedelta(days=2)).replace(hour=9, minute=25, second=0, microsecond=0)
                if scenario == 'custom_retry':
                    output.clear()
                    os.write(fd, b'\x1b[H2000\r')
                    wait_for(lambda: b'Choose a future date and time' in output)
                if scenario in ('custom_cancel', 'custom_escape'):
                    if scenario == 'custom_escape':
                        os.write(fd, b'12')
                    output.clear()
                    os.write(fd, b'\x1b')
                    wait_for(lambda: b'Ctrl+D/U' in output)
                    assert not store.pending()
                    os.write(fd, b'\x1b')
                else:
                    if scenario == 'delayed_custom':
                        time.sleep(1.4)
                    output.clear()
                    os.write(fd, b'\x1b[H' + target.strftime('%Y').encode())
                    wait_for(lambda: f'[{target.year}]'.encode() in output)
                    os.write(fd, target.strftime('\t%m\t%d\t%H\t%M').encode())
                    wait_for(lambda: b'[25]' in output)
                    os.write(fd, b'\r')
                    wait_for(lambda: bool(store.pending()))
                    assert store.pending()[0]['due'] == target.timestamp()
                    wait_for(lambda: b'[alarm ' in output)
                    os.write(fd, b'\r')
            else:
                if scenario in ('jk', 'delayed_jk', 'delayed_arrows'):
                    # Move twice down and once up: Enter must choose ten minutes.
                    if scenario.startswith('delayed'):
                        time.sleep(1.4)
                    keys = (b'\x1b[B', b'\x1b[B', b'\x1b[A') if scenario == 'delayed_arrows' else (b'j', b'j', b'k')
                    for key, label in zip(keys, (b'[2] 10 minutes', b'[3] 15 minutes', b'[2] 10 minutes')):
                        output.clear()
                        os.write(fd, key)
                        wait_for(lambda: any(label in chunk.split(b'\x1b[0m')[0]
                                             for chunk in output.split(b'\x1b[0;39;48;5;236m')[1:]))
                    os.write(fd, b'\r')
                else:
                    os.write(fd, b'1')
                wait_for(lambda: bool(store.pending()))
                duration = 600 if scenario in ('jk', 'delayed_jk', 'delayed_arrows') else 300
                assert started + duration - 5 < store.pending()[0]['due'] < time.time() + duration + 5, store.pending()[0]
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
                    assert os.waitstatus_to_exitcode(status) == 0, output.decode(errors='replace')[-8000:]
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
            os.close(fd)
            if not exited:
                try:
                    os.killpg(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                os.waitpid(pid, 0)
