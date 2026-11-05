"""Bounded, read-only Hermes conversation previews; no terminal-frame copying."""

from contextlib import closing
import json
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import time

MESSAGE_LIMIT = 24
CONTENT_BYTES = 8192
ENV_KEYS = ('HERMES_HOME', 'HERMES_TUI_ACTIVE_SESSION_FILE')
ANSI = re.compile(r'\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-_])')


def process_session_paths(pid):
    """Return only session-location metadata; never log the process environment."""
    if not isinstance(pid, int) or pid <= 0:
        return {}
    try:
        if sys.platform == 'darwin':
            result = subprocess.run(
                ['/bin/ps', '-E', '-ww', '-p', str(pid), '-o', 'command='],
                text=True, capture_output=True, check=True, timeout=0.5,
            )
            paths = {}
            for key in ENV_KEYS:
                matches = re.findall(r'(?:^|\s)' + key + r'=(.*?)(?=\s+[A-Za-z_][A-Za-z0-9_]*=|\s*$)', result.stdout)
                if matches:
                    paths[key] = matches[-1]
            return paths
        entries = Path(f'/proc/{pid}/environ').read_bytes().split(b'\0')
        return {key.decode(): value.decode(errors='replace')
                for entry in entries for key, separator, value in [entry.partition(b'=')]
                if separator and key.decode(errors='replace') in ENV_KEYS}
    except (OSError, subprocess.SubprocessError):
        return {}


def session_location(pane_id, agent, request):
    info = request('pane', 'process-info', '--pane', pane_id)['process_info']
    active, homes = set(), set()
    for process in info.get('foreground_processes', []):
        paths = process_session_paths(process.get('pid'))
        home = paths.get('HERMES_HOME')
        if not home or not Path(home).is_absolute():
            continue
        homes.add(Path(home))
        marker = paths.get('HERMES_TUI_ACTIVE_SESSION_FILE')
        if marker:
            if not Path(marker).is_absolute():
                raise ValueError('Invalid session metadata path')
            # Re-read on every refresh: /new and resume replace this exact identity.
            with Path(marker).open() as source:
                session_id = json.loads(source.read(8192)).get('session_id')
            if not isinstance(session_id, str) or not session_id:
                raise ValueError('Session is not ready')
            active.add((Path(home), session_id))
    if len(active) == 1:
        return active.pop()
    native = agent.get('agent_session') or {}
    if not active and len(homes) == 1 and native.get('kind') == 'id' and native.get('value'):
        return homes.pop(), native['value']
    # Never guess by title, most-recent session, caller profile, or another pane.
    raise ValueError('Exact Hermes session unavailable')


def compression_tip(connection, session_id, home):
    visited = set()
    for _ in range(32):
        if session_id in visited:
            raise ValueError('Cyclic session lineage')
        visited.add(session_id)
        session = connection.execute(
            'SELECT profile_name, end_reason FROM sessions WHERE id = ?', (session_id,),
        ).fetchone()
        if session is None:
            raise ValueError('Session not saved yet')
        if home.parent.name == 'profiles' and session['profile_name'] not in (None, home.name):
            raise ValueError('Profile mismatch')
        if session['end_reason'] != 'compression':
            return session_id
        # Only an unambiguous compression continuation, never branch/reset/delegate.
        children = connection.execute('''
            SELECT id FROM sessions
            WHERE parent_session_id = ? AND COALESCE(source, '') <> 'tool'
              AND json_valid(COALESCE(model_config, '{}'))
              AND json_extract(COALESCE(model_config, '{}'), '$._branched_from') IS NULL
              AND json_extract(COALESCE(model_config, '{}'), '$._delegate_from') IS NULL
              AND json_extract(COALESCE(model_config, '{}'), '$._reset_from') IS NULL
            LIMIT 2
        ''', (session_id,)).fetchall()
        if len(children) != 1:
            raise ValueError('Session continuation unavailable')
        session_id = children[0]['id']
    raise ValueError('Session lineage is too long')


def recent_messages(home, session_id):
    database = home / 'state.db'
    with closing(sqlite3.connect(database.resolve().as_uri() + '?mode=ro', uri=True,
                                 isolation_level=None, timeout=0.2)) as connection:
        connection.row_factory = sqlite3.Row
        deadline = time.monotonic() + 0.25
        connection.set_progress_handler(lambda: int(time.monotonic() > deadline), 1000)
        # One short snapshot prevents compression from changing IDs between reads.
        connection.execute('BEGIN')
        session_id = compression_tip(connection, session_id, home)
        rows = connection.execute('''
            SELECT role, substr(tool_name, 1, 128) AS tool_name,
                   CASE WHEN role <> 'tool' THEN substr(CAST(content AS BLOB), 1, :cap) END AS content,
                   CASE WHEN role <> 'tool' THEN length(CAST(content AS BLOB)) > :cap END AS content_cut,
                   CASE WHEN role = 'assistant' THEN
                     (SELECT json_group_array(CASE WHEN json_valid(value)
                          THEN substr(json_extract(value, '$.function.name'), 1, 128) END)
                      FROM (SELECT value FROM json_each(
                          CASE WHEN json_valid(messages.tool_calls) THEN messages.tool_calls ELSE '[]' END)
                          LIMIT 8)) ELSE '[]' END AS tool_names
            FROM messages INDEXED BY idx_messages_session_id
            WHERE session_id = :sid AND active = 1
              AND role IN ('user', 'assistant', 'tool')
              AND COALESCE(_compressed_summary, 0) = 0
              AND COALESCE(display_kind, '') <> 'hidden'
              AND COALESCE(json_extract(CASE WHEN json_valid(display_metadata)
                  THEN display_metadata ELSE '{}' END, '$.model_only'), 0) = 0
            ORDER BY id DESC LIMIT :count
        ''', {'sid': session_id, 'cap': CONTENT_BYTES, 'count': MESSAGE_LIMIT}).fetchall()
        return [dict(row) for row in rows]


def safe_text(value):
    text = ANSI.sub('', value)
    text = ''.join(character for character in text if character.isprintable() or character in '\n\t\u200d')
    return '\n'.join(line.rstrip() for line in text.expandtabs(4).splitlines())


def content_text(content, cut):
    if content is None:
        return ''
    text = content.decode('utf-8', errors='ignore') if isinstance(content, bytes) else content
    if text.startswith('\0json:'):
        if cut:
            return '[Large structured message omitted]'
        try:
            parts = json.loads(text[6:])
        except (ValueError, TypeError):
            return '[Unreadable structured message]'
        if isinstance(parts, str):
            text = parts
        else:
            if isinstance(parts, dict):
                parts = [parts]
            text = '\n'.join(part['text'] for part in parts or []
                             if isinstance(part, dict) and part.get('type') in ('text', 'input_text', 'output_text')
                             and isinstance(part.get('text'), str)) if isinstance(parts, list) else ''
            if not text:
                text = '[Non-text message]'
    return safe_text(text) + ('\n[Message truncated]' if cut else '')


def render_messages(messages):
    blocks = ['\x1b[38;2;189;147;249mSaved conversation · newest first\x1b[0m']
    colors = {'user': '80;250;123', 'assistant': '139;233;253', 'tool': '255;184;108'}
    for message in messages:
        role = message['role']
        text = content_text(message.get('content'), message.get('content_cut'))
        names = json.loads(message.get('tool_names') or '[]')
        calls = ', '.join(safe_text(name) for name in names if isinstance(name, str))
        if role == 'tool':
            text = safe_text(message.get('tool_name') or 'tool') + ' · result recorded'
        elif calls:
            text += ('\n' if text else '') + 'Calling: ' + calls
        if not text.strip():
            continue
        blocks.append(f'\x1b[1;38;2;{colors[role]}m{role.upper()}\x1b[0m\n{text}')
    if len(blocks) == 1:
        blocks.append('No saved messages yet.')
    return '\n\n'.join(blocks) + '\n'


def conversation_preview(pane_id, request):
    agent = request('agent', 'get', pane_id)['agent'] or {}
    if agent.get('agent') != 'hermes':
        return None
    try:
        home, session_id = session_location(pane_id, agent, request)
        return render_messages(recent_messages(home, session_id))
    except (OSError, ValueError, KeyError, TypeError, sqlite3.Error, subprocess.SubprocessError):
        # Do not leak paths, arguments, hidden content, or a different conversation.
        return 'Hermes conversation preview unavailable.\nEnter opens the original terminal.\n'
