"""Exercise preview PTY ownership through real fzf and a fake Herdr."""
import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import shlex
import signal
import struct
import sys
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[1]

with tempfile.TemporaryDirectory(prefix='picker-size-') as directory:
    root = Path(directory)
    fake = root / 'herdr'
    fake.write_text('#!' + sys.executable + '\n' + '''import json, os, signal, sys, time
from pathlib import Path
args = sys.argv[1:]
def log(text):
    with Path(os.environ['EVENTS']).open('a') as out:
        out.write(text + '\\n')
if args[:2] == ['pane', 'get']:
    print(json.dumps({'result': {'pane': {'terminal_id': args[2]}}}))
elif args[:2] == ['terminal', 'attach']:
    pane = args[2]
    size = os.get_terminal_size(0)
    log('attach %s %s %s %s %s %s' % (pane, size.columns, size.lines, os.environ['FZF_PREVIEW_COLUMNS'], os.environ['FZF_PREVIEW_LINES'], os.getpid()))
    if os.environ.get('FAIL_ATTACH'): sys.exit(1)
    def finish(*unused):
        log('release ' + pane)
        sys.exit(0)
    signal.signal(signal.SIGTERM, finish)
    print('\\x1b[2J', flush=True)
    while True: time.sleep(1)
elif args[:2] == ['pane', 'read']:
    log('read ' + args[2])
    if os.environ.get('FAIL_READ'): sys.exit(1)
    print('SCREEN_' + args[2])
else: sys.exit(2)
''')
    fake.chmod(0o700)

    for scenario in ('switch-hide-close', 'interrupt', 'read-failure', 'attach-failure'):
        events = root / (scenario + '.log')
        env = dict(os.environ, TERM='xterm-256color', HERDR_BIN_PATH=str(fake), EVENTS=str(events))
        if scenario == 'read-failure':
            env['FAIL_READ'] = '1'
        if scenario == 'attach-failure':
            env['FAIL_ATTACH'] = '1'
        command = shlex.join([sys.executable, str(ROOT / 'agent-picker.py'), '--live-preview', str(root / (scenario + '.lock'))]) + ' {1}'
        args = ['fzf', '--layout=reverse', '--preview', command, '--preview-window=right:60%', '--bind=p:toggle-preview,q:abort,resize:refresh-preview']
        pid, fd = pty.fork()
        if pid == 0:
            os.execve('/bin/sh', ['sh', '-c', "printf 'p1\\np2\\n' | " + shlex.join(args)], env)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 28, 100, 0, 0))
        output = bytearray()
        def lines():
            return events.read_text().splitlines() if events.exists() else []
        def wait_for(check):
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                if select.select([fd], [], [], 0.05)[0]:
                    try: output.extend(os.read(fd, 65536))
                    except OSError as error:
                        if error.errno != errno.EIO: raise
                if check(): return
            raise AssertionError((scenario, lines(), bytes(output)[-1000:]))
        def alive(pid):
            try: os.kill(int(pid), 0); return True
            except ProcessLookupError: return False
        try:
            if scenario == 'attach-failure':
                wait_for(lambda: b'Preview unavailable' in output)
                assert not any(line.startswith('read ') for line in lines())
                os.write(fd, b'q')
                print('PASS: preview size and release', scenario, flush=True)
                continue
            wait_for(lambda: 'read p1' in lines())
            first = next(line.split() for line in lines() if line.startswith('attach '))
            assert first[2:4] == first[4:6], first
            assert lines().index('read p1') > next(i for i, line in enumerate(lines()) if line.startswith('attach p1 '))
            if scenario == 'read-failure':
                wait_for(lambda: not alive(first[-1]))
                os.write(fd, b'q')
            elif scenario == 'interrupt':
                os.write(fd, b'\x03')
                wait_for(lambda: not alive(first[-1]))
            else:
                os.write(fd, b'\x1b[B')
                wait_for(lambda: 'read p2' in lines())
                wait_for(lambda: not alive(first[-1]))
                assert lines().index('release p1') < next(i for i, line in enumerate(lines()) if line.startswith('attach p2 ')), lines()
                second = next(line.split() for line in lines() if line.startswith('attach p2 '))
                fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 28, 72, 0, 0))
                wait_for(lambda: len([line for line in lines() if line.startswith('attach p2 ')]) == 2)
                resized = [line.split() for line in lines() if line.startswith('attach p2 ')][-1]
                assert resized[2:4] == resized[4:6] and resized[2] != second[2], (second, resized)
                wait_for(lambda: not alive(second[-1]))
                os.write(fd, b'p')
                wait_for(lambda: not alive(resized[-1]))
                reads = len([line for line in lines() if line.startswith('read ')])
                time.sleep(1.3)
                assert reads == len([line for line in lines() if line.startswith('read ')])
                os.write(fd, b'p')
                wait_for(lambda: sum(line.startswith('attach p2 ') for line in lines()) == 3)
                third = [line.split() for line in lines() if line.startswith('attach p2 ')][-1]
                os.write(fd, b'q')
                wait_for(lambda: not alive(third[-1]))
            print('PASS: preview size and release', scenario, flush=True)
        finally:
            os.close(fd)
            try: os.kill(pid, signal.SIGKILL)
            except ProcessLookupError: pass
            os.waitpid(pid, 0)
