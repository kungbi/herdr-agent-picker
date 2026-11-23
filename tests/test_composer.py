"""Real fzf, isolated PTYs, synthetic identities and an argv-recording fake Herdr."""
import copy
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import sys
import tempfile
import termios
import time

from rust_support import BINARY

INITIAL = {
    'agents': [dict(pane_id=f'w1:p{i}', terminal_id=f'term-{i}', agent='claude',
                    name=f'writer{i}', agent_status='working', focused=i == 1,
                    workspace_id='w1', tab_id='w1:t1', title=f'Task {i}',
                    agent_session=dict(agent='claude', kind='id', source='hook', value=f'session-{i}'))
               for i in (1, 2)],
    'workspaces': [], 'tabs': [],
}

with tempfile.TemporaryDirectory(prefix='herdr-composer-') as directory:
    root = Path(directory)
    fake = root / 'herdr'
    fake.write_text('#!' + sys.executable + '\n' + r'''
import json, os, sys, time
from pathlib import Path
args = sys.argv[1:]
with Path(os.environ['CALLS']).open('a') as log:
    log.write(json.dumps(args, ensure_ascii=False) + '\n')
data = json.loads(Path(os.environ['DATA']).read_text())
if args == ['api', 'snapshot']:
    result = {'snapshot': data}
elif args[:2] == ['agent', 'get']:
    if data.get('get_error'):
        print(json.dumps({'error': {'message': 'identity lookup unavailable'}}), file=sys.stderr)
        raise SystemExit(1)
    result = {'agent': next((a for a in data['agents'] if a['pane_id'] == args[2]), None)}
elif args[:2] == ['pane', 'process-info']:
    pane = args[3]
    pid = data.get('pid', 100 + int(pane[-1]))
    result = {'process_info': dict(pane_id=pane, shell_pid=10,
        foreground_process_group_id=10 if data.get('shell') else pid,
        foreground_processes=[dict(pid=10 if data.get('shell') else pid,
                                  name='zsh' if data.get('shell') else 'claude', argv=['/bin/claude'])])}
elif args[:2] == ['agent', 'prompt']:
    assert len(args) == 5 and args[3] == '--', args
    if data.get('timeout'):
        time.sleep(5)
    if data.get('send_error'):
        print(json.dumps({'error': {'message': 'simulated partial delivery'}}), file=sys.stderr)
        raise SystemExit(1)
    result = {}
elif args[:2] == ['pane', 'read']:
    print('PREVIEW for ' + args[2], flush=True)
    raise SystemExit(0)
elif args[:2] == ['pane', 'get']:
    result = {'pane': {'terminal_id': args[2]}}
elif args[:2] == ['terminal', 'attach']:
    print('\x1b[2J', flush=True)
    while True: time.sleep(1)
else:
    raise SystemExit('FORBIDDEN command: ' + repr(args))
print(json.dumps({'result': result}))
''')
    fake.chmod(0o700)
    config = root / 'config.toml'
    config.write_text('[ui]\nagent_panel_sort = "priority"\n')
    socket = root / 'fake.sock'
    socket.touch()

    cases = ('cancel', 'empty', 'literal', 'paste', 'paste_multiline', 'paste_control',
             'paste_invalid_utf8', 'unbracketed_multiline', 'utf8_edit', 'reorder', 'gone', 'replaced',
             'session_replaced', 'terminal_replaced', 'retained_shell', 'lookup_error', 'partial_error',
             'timeout', 'search_i', 'empty_list', 'shell_on_open')
    selected = os.environ.get('HERDR_COMPOSER_TEST_CASE')
    if selected:
        assert selected in cases
        cases = (selected,)
    for case in cases:
        data = copy.deepcopy(INITIAL)
        if case == 'empty_list': data['agents'] = []
        if case == 'shell_on_open': data['shell'] = True
        path, calls = root / f'{case}.json', root / f'{case}.calls'

        def update():
            temp = path.with_suffix('.tmp')
            temp.write_text(json.dumps(data))
            temp.replace(path)

        update()
        pid, fd = pty.fork()
        if pid == 0:
            environment = dict(os.environ, TERM='xterm-256color', HERDR_BIN_PATH=str(fake),
                               HERDR_SOCKET_PATH=str(socket), HERDR_CONFIG_PATH=str(config),
                               HERDR_PLUGIN_STATE_DIR=str(root / f'{case}-state'),
                               DATA=str(path), CALLS=str(calls))
            # Do not inherit caller popup identity or a real terminal path.
            for name in ('HERDR_AGENT_PICKER_TTY_PATH', 'HERDR_PLUGIN_CONTEXT_JSON',
                         'HERDR_PLUGIN_ID', 'HERDR_PLUGIN_ENTRYPOINT_ID', 'HERDR_PANE_ID'):
                environment.pop(name, None)
            os.chdir(root)
            os.execve(str(BINARY), [str(BINARY)], environment)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 160, 0, 0))
        output = bytearray()
        reaped = False

        def drain(seconds=0.1):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([fd], [], [], min(0.05, max(0, deadline - time.monotonic())))[0]:
                    try: output.extend(os.read(fd, 65536))
                    except OSError as e:
                        if e.errno != errno.EIO: raise
                        break

        def wait(predicate, timeout=7):
            deadline = time.monotonic() + timeout
            while not predicate() and time.monotonic() < deadline: drain()
            assert predicate(), f'{case}: ' + output.decode(errors='replace')[-6000:]

        def log():
            return [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []

        def prompts():
            return [a for a in log() if a[:2] == ['agent', 'prompt']]

        def alive():
            assert os.waitpid(pid, os.WNOHANG)[0] == 0, f'{case}: picker closed'
            assert not any(a[:2] in (['agent', 'focus'], ['pane', 'focus'], ['pane', 'close']) for a in log()), log()

        def press(keys):
            os.write(fd, keys)
            drain(0.2)

        try:
            wait(lambda: (b'No running agents' if case == 'empty_list' else b'PREVIEW for w1:p1') in output)
            if case == 'search_i':
                press(b'/')
                output.clear()
                press(b'NeverMatch-i')
                assert b'NeverMatch-i' in output, 'Search did not treat i as query text'
                assert b'To w1:' not in output
                assert not any(a[:2] == ['pane', 'process-info'] for a in log())
                press(b'\x1b')
            elif case == 'empty_list':
                press(b'i')
                assert b'To ' not in output
            else:
                output.clear()
                press(b'i')
                if case == 'shell_on_open':
                    wait(lambda: b'Nothing sent' in output)
                    press(b'\x1b')
                else:
                    wait(lambda: b'To w1:p1 / pid 101 / writer1 (claude)' in output)
                    assert b'\x1b[24;1H' in output, 'Composer is not on the bottom row'
                    if case in ('gone', 'replaced', 'session_replaced', 'terminal_replaced', 'retained_shell', 'lookup_error', 'reorder'):
                        if case == 'gone': data['agents'].pop(0)
                        if case == 'replaced': data['pid'] = 999  # Same kind/session: process replacement must still fail.
                        if case == 'terminal_replaced': data['agents'][0]['terminal_id'] = 'new-terminal'
                        if case == 'session_replaced': data['agents'][0]['agent_session']['value'] = 'new-session'
                        if case == 'retained_shell': data['shell'] = True
                        if case == 'lookup_error': data['get_error'] = True
                        if case == 'reorder':
                            data['agents'].reverse()
                            data['agents'][0]['agent_status'] = 'done'
                            data['agents'][0]['focused'] = True
                            data['agents'][0]['title'] = 'Reordered second agent'
                            data['agents'][1]['focused'] = False
                        update()
                        # Exercise fzf's one-second cooked-mode restoration and preview refresh.
                        before = len([a for a in log() if a[:2] == ['pane', 'read']])
                        drain(1.4)
                        assert len([a for a in log() if a[:2] == ['pane', 'read']]) > before, 'Preview stopped during composition'
                        assert b'To w1:p2' not in output, 'Recipient shifted with the list'
                    if case == 'partial_error': data['send_error'] = True; update()
                    if case == 'timeout': data['timeout'] = True; update()
                    payload = "  --한글 'quote' \"double\" $HOME $(touch NEVER) `cmd` ; | & \\ 끝  "
                    if case == 'cancel':
                        press(payload.encode())
                        press(b'\x1b')
                    elif case == 'empty':
                        press(b'\r')
                        assert not prompts()
                        alive()
                        press(b'\x1b')
                    elif case == 'unbracketed_multiline':
                        press(b'first\rsecond\r')
                        assert not prompts(), 'Unbracketed multiline paste submitted'
                        alive()
                        press(b'\x1b')
                    elif case.startswith('paste_'):
                        pasted = {'paste_multiline': b'line1\r\nline2', 'paste_control': b'x\x1b[31m\x00\t\x03y',
                                  'paste_invalid_utf8': b'bad\xff'}[case]
                        press(b'\x1b[200~' + pasted + b'\x1b[201~')
                        press(b'\r')
                        assert not prompts(), 'Paste submitted control input'
                        alive()
                        press(b'\x1b')
                    else:
                        if case == 'paste':
                            press(b'\x1b[200~' + payload.encode() + b'\x1b[201~')
                        elif case == 'utf8_edit':
                            # Feed fragmented UTF-8 and remove a complete Korean character.
                            for byte in '한글'.encode(): press(bytes([byte]))
                            press(b'\x7f')
                            payload = '한끝'
                            press('끝'.encode())
                        else:
                            press(payload.encode())
                        press(b'\r')
                        failure = case in ('gone', 'replaced', 'session_replaced', 'terminal_replaced', 'retained_shell', 'lookup_error', 'partial_error', 'timeout')
                        if failure:
                            wait(lambda: b'Error:' in output)
                            if case in ('partial_error', 'timeout'):
                                assert b'not retried' in output
                            else: assert not prompts(), prompts()
                            press(b'\x1b')
                        else:
                            wait(lambda: len(prompts()) == 1)
                            assert prompts() == [['agent', 'prompt', 'w1:p1', '--', payload]], prompts()
                    drain(0.3)
                    alive()
                    expected_count = 1 if case in ('literal', 'paste', 'utf8_edit', 'reorder', 'partial_error', 'timeout') else 0
                    assert len(prompts()) == expected_count, prompts()
                    if case == 'reorder':
                        wait(lambda: b'Reordered second agent' in output)
                    # A fresh dialog proves return to normal picker input with no focus/close.
                    if case in ('literal', 'cancel', 'paste', 'reorder'):
                        output.clear()
                        press(b'i')
                        wait(lambda: b'To w1:p1' in output)
                        press(b'\x1b')
            alive()
            assert not (root / 'NEVER').exists(), 'Payload was evaluated by a shell'
            press(b'q')
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                done, status = os.waitpid(pid, os.WNOHANG)
                if done:
                    reaped = True
                    assert os.waitstatus_to_exitcode(status) == 0
                    break
                drain()
            assert reaped, f'{case}: q did not close picker'
            assert not any(a[:2] in (['agent', 'focus'], ['pane', 'send-text'], ['pane', 'send-keys'], ['agent', 'send-keys'], ['pane', 'close']) for a in log()), log()
            print(f'PASS: composer {case}; real fzf / fake Herdr / no focus or close', flush=True)
        finally:
            if not reaped:
                try: os.killpg(pid, signal.SIGKILL)
                except ProcessLookupError: pass
                os.waitpid(pid, 0)
            os.close(fd)
