"""Exercise preview PTY ownership through real fzf and a fake Herdr."""
import errno
import fcntl
import json
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

from rust_support import BINARY


def alive(pid):
    try: os.kill(int(pid), 0); return True
    except ProcessLookupError: return False


with tempfile.TemporaryDirectory(prefix='picker-size-') as directory:
    root = Path(directory)
    fake = root / 'herdr'
    fake.write_text('#!' + sys.executable + '\n' + '''import json, os, signal, sys, time
from pathlib import Path
args = sys.argv[1:]
def log(text):
    with Path(os.environ['EVENTS']).open('a') as out:
        out.write(text + '\\n')
if args == ['api', 'snapshot']:
    snapshot = json.loads(Path(os.environ['SNAPSHOT']).read_text())
    log('snapshot ' + str(snapshot.get('focused_pane_id')))
    print(json.dumps({'result': {'snapshot': snapshot}}))
elif args[:2] == ['pane', 'get']:
    log('get ' + args[2])
    print(json.dumps({'result': {'pane': {'terminal_id': args[2]}}}))
elif args[:2] == ['terminal', 'attach']:
    assert len(args) == 3, args  # No takeover or injected terminal input.
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
    assert args[3:] == ['--source', 'recent-unwrapped', '--lines', '30', '--format', 'ansi'], args
    log('read ' + args[2])
    if os.environ.get('FAIL_READ'): sys.exit(1)
    print('\\x1b[32mSCREEN_' + args[2] + '\\x1b[0m')
else:
    log('unexpected ' + ' '.join(args))
    sys.exit(2)
''')
    fake.chmod(0o700)

    for scenario in ('switch-hide-close', 'interrupt', 'read-failure', 'attach-failure'):
        events = root / (scenario + '.log')
        env = dict(os.environ, TERM='xterm-256color', HERDR_BIN_PATH=str(fake), EVENTS=str(events))
        if scenario == 'read-failure':
            env['FAIL_READ'] = '1'
        if scenario == 'attach-failure':
            env['FAIL_ATTACH'] = '1'
        command = shlex.join([str(BINARY), '--live-preview', str(root / (scenario + '.lock'))]) + ' {1} outside'
        args = ['fzf', '--no-tty-default', '--layout=reverse', '--preview', command, '--preview-window=right:60%', '--bind=p:toggle-preview,q:abort,resize:refresh-preview']
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

    # Exercise origin capture in the actual picker, rather than injecting an
    # origin into its preview command. Both rows may report focused across tabs;
    # the authoritative session focus is the second row, never the first row.
    for scenario in ('session-origin', 'origin-not-selected', 'plugin-origin', 'context-only', 'missing-origin',
                     'conflicting-origin', 'stale-context', 'popup-only', 'origin-read-failure'):
        case = root / scenario
        case.mkdir()
        events = case / 'events'
        data = case / 'snapshot.json'
        snapshot = {
            'focused_pane_id': 'p2',
            'agents': [dict(agent='claude', pane_id=pane, workspace_id='w1', tab_id='t1',
                            agent_status='working', title='Agent ' + pane, focused=True)
                       for pane in ('p1', 'p2')],
            'panes': [{'pane_id': pane} for pane in ('p1', 'p2', 'popup')],
            'workspaces': [{'workspace_id': 'w1', 'label': 'Workspace'}],
            'tabs': [{'tab_id': 't1', 'label': 'Tab'}],
        }
        env = {key: value for key, value in os.environ.items()
               if not key.startswith('HERDR_') and not key.startswith('FZF_')}
        socket = case / 'session.sock'
        socket.touch()
        config = case / 'config.toml'
        config.write_text('')
        env.update(TERM='xterm-256color', HERDR_BIN_PATH=str(fake), EVENTS=str(events),
                   SNAPSHOT=str(data), HERDR_PLUGIN_STATE_DIR=str(case / 'state'),
                   HERDR_SOCKET_PATH=str(socket), HERDR_CONFIG_PATH=str(config),
                   HERDR_PLUGIN_ID='agent-picker', HERDR_PLUGIN_ENTRYPOINT_ID='picker',
                   HERDR_PANE_ID='popup')
        if scenario in ('plugin-origin', 'popup-only'):
            snapshot['focused_pane_id'] = 'popup'
        if scenario in ('context-only', 'missing-origin'):
            del snapshot['focused_pane_id']
        if scenario in ('plugin-origin', 'context-only', 'conflicting-origin', 'stale-context'):
            context_pane = 'p1' if scenario == 'conflicting-origin' else 'stale' if scenario == 'stale-context' else 'p2'
            env['HERDR_PLUGIN_CONTEXT_JSON'] = json.dumps({'focused_pane_id': context_pane})
        if scenario == 'origin-read-failure':
            env['FAIL_READ'] = '1'
        if scenario == 'origin-not-selected':
            snapshot['agents'][1]['focused'] = False
        data.write_text(json.dumps(snapshot))
        pid, fd = pty.fork()
        if pid == 0:
            os.execve(str(BINARY), [str(BINARY), 'picker'], env)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 28, 100, 0, 0))
        output = bytearray()
        exited = False

        def lines():
            return events.read_text().splitlines() if events.exists() else []

        def reads(pane):
            return lines().count('read ' + pane)

        def wait_for(check):
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                if select.select([fd], [], [], 0.05)[0]:
                    try: output.extend(os.read(fd, 65536))
                    except OSError as error:
                        if error.errno != errno.EIO: raise
                if check(): return
            raise AssertionError((scenario, lines(), bytes(output)[-1000:]))

        try:
            initial_attachments = 0
            if scenario == 'origin-not-selected':
                wait_for(lambda: reads('p1') >= 1)
                initial_attach = next(line.split() for line in lines() if line.startswith('attach p1 '))
                os.write(fd, b'j')
                wait_for(lambda: reads('p2') >= 1)
                wait_for(lambda: not alive(initial_attach[-1]))
                initial_attachments = 1
            wait_for(lambda: reads('p2') >= 1)
            native_start = len(lines())
            assert not any(line.startswith(('get p2', 'attach p2', 'attach popup')) for line in lines()), lines()
            if not initial_attachments:
                assert not any(line.startswith(('get ', 'attach ')) for line in lines()), lines()
            if scenario == 'origin-read-failure':
                wait_for(lambda: b'Preview unavailable' in output)
                assert not any(line.startswith(('get ', 'attach ')) for line in lines()[native_start:]), lines()
            else:
                wait_for(lambda: reads('p2') >= 2)  # Refreshes without a status change.
                wait_for(lambda: b'SCREEN_p2' in output)
                assert not any(line.startswith(('get ', 'attach ')) for line in lines()[native_start:]), lines()
                count = reads('p2')
                fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 28, 82, 0, 0))
                wait_for(lambda: reads('p2') > count)
                assert not any(line.startswith(('get ', 'attach ')) for line in lines()[native_start:]), lines()
                os.write(fd, b'p')
                # Let a read already in flight finish before testing the hidden interval.
                time.sleep(0.3)
                count = reads('p2')
                time.sleep(1.3)
                assert reads('p2') == count, lines()
                os.write(fd, b'p')
                wait_for(lambda: reads('p2') > count)
                previous_reads = reads('p1')
                os.write(fd, b'k')
                wait_for(lambda: reads('p1') > previous_reads)
                known_origin = scenario in ('session-origin', 'origin-not-selected', 'plugin-origin', 'context-only')
                if known_origin:
                    attached = [line.split() for line in lines() if line.startswith('attach p1 ')][-1]
                    assert attached[2:4] == attached[4:6], attached
                    # A later focus/context change must not replace the captured origin.
                    snapshot['focused_pane_id'] = 'p1'
                    snapshot['agents'][0]['title'] = 'Changed agent p1'
                    temporary = data.with_suffix('.tmp')
                    temporary.write_text(json.dumps(snapshot))
                    temporary.replace(data)
                    wait_for(lambda: 'snapshot p1' in lines())
                    count = reads('p1')
                    wait_for(lambda: reads('p1') > count)
                else:
                    assert not any(line.startswith(('get ', 'attach ')) for line in lines()), lines()
                count = reads('p2')
                os.write(fd, b'j')
                wait_for(lambda: reads('p2') > count)
                if known_origin:
                    wait_for(lambda: not alive(attached[-1]))
                    assert lines().index('release p1') < max(i for i, line in enumerate(lines()) if line == 'read p2'), lines()
                assert not any(line.startswith(('get p2', 'attach p2', 'attach popup')) for line in lines()), lines()
                count = reads('p2')
                wait_for(lambda: reads('p2') > count)
                if known_origin:
                    # It remains the *other* pane even after a refreshed snapshot
                    # reports it focused. Closing here must reap its attachment.
                    os.write(fd, b'k')
                    wait_for(lambda: sum(line.startswith('attach p1 ') for line in lines()) == initial_attachments + 2)
                    attached = [line.split() for line in lines() if line.startswith('attach p1 ')][-1]
            os.write(fd, b'q')
            def picker_exited():
                global exited
                done, status = os.waitpid(pid, os.WNOHANG)
                if done:
                    exited = True
                    assert os.waitstatus_to_exitcode(status) == 0, bytes(output)[-1000:]
                return exited
            wait_for(picker_exited)
            if scenario in ('session-origin', 'origin-not-selected', 'plugin-origin', 'context-only'):
                wait_for(lambda: not alive(attached[-1]))
            stopped_events = lines()
            time.sleep(1.2)
            assert lines() == stopped_events, 'Polling continued after picker close'
            assert not any(line.startswith('unexpected ') for line in lines()), lines()
            print('PASS: real picker preview', scenario, flush=True)
        finally:
            os.close(fd)
            if not exited:
                try: os.kill(pid, signal.SIGKILL)
                except ProcessLookupError: pass
                os.waitpid(pid, 0)
