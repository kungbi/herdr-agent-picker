"""Exercise preview PTY ownership through real fzf and a fake Herdr."""
import errno
import codecs
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import signal
import struct
import sys
import tempfile
import termios
import time
import unicodedata

ROOT = Path(__file__).resolve().parents[1]

from rust_support import BINARY


def alive(pid):
    try: os.kill(int(pid), 0); return True
    except ProcessLookupError: return False


class Screen:
    """Track the cells fzf paints, rather than searching old PTY output."""
    def __init__(self, rows, columns):
        self.rows, self.columns = rows, columns
        self.cells = [[' '] * columns for _ in range(rows)]
        self.frame = self.cells
        self.y = self.x = 0
        self.saved = (0, 0)
        self.pending = ''
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        while self.pending:
            if self.pending.startswith('\x1b['):
                match = re.match(r'\x1b\[([0-?]*)([ -/]*)([@-~])', self.pending)
                if not match: return
                self.pending = self.pending[match.end():]
                args, _, action = match.groups()
                if args.startswith('?'):
                    # fzf finishes each paint by restoring auto-wrap. Inspect
                    # complete frames even when a PTY read splits a repaint.
                    if args == '?7' and action == 'h':
                        self.frame = [row[:] for row in self.cells]
                    continue
                values = [int(n or 0) for n in args.split(';')]
                n = values[0] or 1
                if action == 'A': self.y -= n
                elif action == 'B': self.y += n
                elif action == 'C': self.x += n
                elif action == 'D': self.x -= n
                elif action in ('H', 'f'):
                    self.y = n - 1
                    self.x = (values[1] or 1) - 1 if len(values) > 1 else 0
                elif action == 'G': self.x = n - 1
                elif action == 'd': self.y = n - 1
                elif action == 'J':
                    if values[0] in (2, 3):
                        self.cells = [[' '] * self.columns for _ in range(self.rows)]
                    elif values[0] == 0:
                        self.cells[self.y][self.x:] = [' '] * (self.columns - self.x)
                        for y in range(self.y + 1, self.rows): self.cells[y] = [' '] * self.columns
                    else: raise AssertionError(('unsupported erase', args))
                elif action in ('K', 'X'):
                    start = 0 if action == 'K' and values[0] in (1, 2) else self.x
                    end = self.x + n if action == 'X' else self.x + 1 if values[0] == 1 else self.columns
                    self.cells[self.y][start:end] = [' '] * (min(end, self.columns) - start)
                elif action == 's': self.saved = (self.y, self.x)
                elif action == 'u': self.y, self.x = self.saved
                elif action not in ('m', 'h', 'l', 'n', 't', 'q'):
                    raise AssertionError(('unsupported CSI', args, action))
                self.y = max(0, min(self.rows - 1, self.y))
                self.x = max(0, min(self.columns - 1, self.x))
                continue
            if self.pending[0] == '\x1b':
                if len(self.pending) < 2: return
                escape = self.pending[1]
                if escape == ']':
                    match = re.search(r'\x07|\x1b\\', self.pending)
                    if not match: return
                    self.pending = self.pending[match.end():]
                    continue
                if escape == '7': self.saved = (self.y, self.x)
                elif escape == '8': self.y, self.x = self.saved
                elif escape in ('(', ')'):
                    if len(self.pending) < 3: return
                    self.pending = self.pending[3:]
                    continue
                else: raise AssertionError(('unsupported escape', escape))
                self.pending = self.pending[2:]
                continue
            char, self.pending = self.pending[0], self.pending[1:]
            if char == '\r': self.x = 0
            elif char == '\n': self.y = min(self.rows - 1, self.y + 1)
            elif char == '\b': self.x = max(0, self.x - 1)
            elif char >= ' ' and not unicodedata.combining(char):
                self.cells[self.y][self.x] = char
                self.x = min(self.columns - 1, self.x + (2 if unicodedata.east_asian_width(char) in ('W', 'F') else 1))

    def preview(self):
        return re.findall(r'ROW_\d{2}(?:_EDIT)?', '\n'.join(''.join(row[43:97]) for row in self.frame))


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
    if os.environ.get('CONTENT'):
        print(Path(os.environ['CONTENT']).read_text(), end='')
    else:
        print('\\x1b[32mSCREEN_' + args[2] + '\\x1b[0m')
else:
    log('unexpected ' + ' '.join(args))
    sys.exit(2)
''')
    fake.chmod(0o700)

    # Actual picker, actual fzf, fake panes only. Check current cells repeatedly
    # while the reader continues polling, including rolling snapshots and edits.
    for origin in ('p1', 'outside'):
        case = root / ('viewport-' + origin)
        case.mkdir()
        content, data, events = (case / name for name in ('content', 'snapshot', 'events'))
        data.write_text(json.dumps({
            'focused_pane_id': origin, 'panes': [{'pane_id': p} for p in ('p1', 'outside')],
            'agents': [dict(agent='claude', pane_id='p1', workspace_id='w1', tab_id='t1',
                            agent_status='working', title='Agent', focused=True)],
            'workspaces': [], 'tabs': [],
        }))
        (case / 'config').touch()
        (case / 'socket').touch()
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(('HERDR_', 'FZF_'))}
        env.update(TERM='xterm-256color', HERDR_BIN_PATH=str(fake), EVENTS=str(events),
                   SNAPSHOT=str(data), CONTENT=str(content), HERDR_PLUGIN_STATE_DIR=str(case / 'state'),
                   HERDR_SOCKET_PATH=str(case / 'socket'), HERDR_CONFIG_PATH=str(case / 'config'))

        def publish(first, last, edit=False):
            temporary = content.with_suffix('.tmp')
            temporary.write_text(''.join('\x1b[32mROW_%02d%s\x1b[0m\n' %
                                         (n, '_EDIT' if edit else '') for n in range(first, last + 1)))
            temporary.replace(content)

        publish(1, 30)
        pid, fd = pty.fork()
        if pid == 0:
            os.execve(str(BINARY), [str(BINARY), 'picker'], env)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 18, 100, 0, 0))
        screen = Screen(18, 100)

        def lines():
            return events.read_text().splitlines() if events.exists() else []

        def reads():
            return lines().count('read p1')

        def pump():
            if select.select([fd], [], [], 0.03)[0]: screen.feed(os.read(fd, 65536))

        def wait_for(check):
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                pump()
                if check(): return
            raise AssertionError((origin, screen.preview(), lines()))

        def stable(expected, refreshes=3):
            count = reads()
            deadline = time.monotonic() + 8
            end = time.monotonic() + refreshes + 0.3
            while reads() < count + refreshes or time.monotonic() < end:
                assert time.monotonic() < deadline, (origin, lines())
                pump()
                assert screen.preview() == expected, (origin, expected, screen.preview())

        try:
            wait_for(lambda: 'ROW_30' in screen.preview())
            assert 'ROW_01' not in screen.preview(), screen.preview()
            # Shift-up is fzf's default preview-up; let following pause naturally.
            os.write(fd, b'\x1b[1;2A' * 5)
            wait_for(lambda: 'ROW_30' not in screen.preview() and 'ROW_12' in screen.preview())
            end = time.monotonic() + 0.3
            while time.monotonic() < end: pump()
            scrolled = screen.preview()
            stable(scrolled)
            publish(1, 33)  # Growing output keeps the same visible cells.
            stable(scrolled)
            publish(5, 34)  # Rolling 30-line snapshots keep the same history too.
            stable(scrolled, 2)
            # Reach bottom with native preview-down, which resumes follow.
            os.write(fd, b'\x1b[1;2B' * 50)
            wait_for(lambda: 'ROW_34' in screen.preview())
            publish(6, 35)
            wait_for(lambda: 'ROW_35' in screen.preview())
            # A replacement must refresh real content, even without overlap.
            publish(1, 30, edit=True)
            wait_for(lambda: 'ROW_30_EDIT' in screen.preview())
            assert all(row.endswith('_EDIT') for row in screen.preview()), screen.preview()
            attachments = [line.split() for line in lines() if line.startswith('attach ')]
            assert len(attachments) == (0 if origin == 'p1' else 1), lines()
            print('PASS: visible viewport, idle/growing/rolling snapshots, bottom follow and replacement', origin, flush=True)
        finally:
            os.write(fd, b'q')
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                done, _ = os.waitpid(pid, os.WNOHANG)
                if done: break
                if select.select([fd], [], [], 0.03)[0]:
                    try: os.read(fd, 65536)
                    except OSError as error:
                        if error.errno != errno.EIO: raise
            else:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
            os.close(fd)
            for attachment in [line.split() for line in lines() if line.startswith('attach ')]:
                deadline = time.monotonic() + 5
                while alive(attachment[-1]) and time.monotonic() < deadline: time.sleep(0.02)
                assert not alive(attachment[-1]), lines()

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
