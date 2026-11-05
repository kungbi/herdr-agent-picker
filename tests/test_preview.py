"""Conversation safety, bounds and identity tests using only synthetic data."""

import json
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
import agent_preview as preview


def make_database(home):
    home.mkdir(parents=True, exist_ok=True)
    connection = sqlite3.connect(home / 'state.db')
    connection.executescript('''
        CREATE TABLE sessions (id TEXT PRIMARY KEY, profile_name TEXT, end_reason TEXT,
            parent_session_id TEXT, source TEXT, model_config TEXT);
        CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT, role TEXT,
            content TEXT, tool_name TEXT, tool_calls TEXT, active INTEGER DEFAULT 1,
            _compressed_summary INTEGER DEFAULT 0, display_kind TEXT, display_metadata TEXT);
        CREATE INDEX idx_messages_session_id ON messages(session_id);
    ''')
    for session_id in ('first', 'second', 'third'):
        connection.execute('INSERT INTO sessions(id, profile_name) VALUES (?, ?)', (session_id, 'work'))
    connection.commit()
    return connection


def add_message(connection, content, role='assistant', session='first', **extra):
    values = dict(session_id=session, role=role, content=content, **extra)
    connection.execute('INSERT INTO messages (' + ','.join(values) + ') VALUES ('
                       + ','.join('?' for _ in values) + ')', tuple(values.values()))
    connection.commit()


class PreviewTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='herdr-preview-test-')
        self.home = Path(self.temporary.name) / 'profiles' / 'work'
        self.connection = make_database(self.home)
        self.marker = self.home / 'active.json'
        self.marker.write_text(json.dumps({'session_id': 'first'}))
        self.agent: dict = {'agent': 'hermes'}

    def tearDown(self):
        self.connection.close()
        self.temporary.cleanup()

    def request(self, *args):
        if args[0:2] == ('agent', 'get'):
            return {'agent': self.agent}
        self.assertEqual(args, ('pane', 'process-info', '--pane', 'pane-one'))
        return {'process_info': {'foreground_processes': [{'pid': 123}]}}

    def paths(self):
        return patch.object(preview, 'process_session_paths', return_value={
            'HERMES_HOME': str(self.home), 'HERMES_TUI_ACTIVE_SESSION_FILE': str(self.marker),
        })

    def test_exact_session_and_no_database_write(self):
        add_message(self.connection, 'first response')
        add_message(self.connection, 'OTHER CONVERSATION', session='second')
        before = (self.home / 'state.db').read_bytes()
        with self.paths():
            result = preview.conversation_preview('pane-one', self.request)
        assert isinstance(result, str)
        self.assertIn('first response', result)
        self.assertNotIn('OTHER CONVERSATION', result)
        self.assertEqual(before, (self.home / 'state.db').read_bytes())

    def test_new_and_resume_reread_marker_over_stale_native_id(self):
        self.agent['agent_session'] = {'kind': 'id', 'value': 'first'}
        add_message(self.connection, 'old response')
        add_message(self.connection, 'new response', session='second')
        with self.paths():
            self.marker.write_text(json.dumps({'session_id': 'second'}))
            result = preview.conversation_preview('pane-one', self.request)
        assert isinstance(result, str)
        self.assertIn('new response', result)
        self.assertNotIn('old response', result)

    def test_missing_marker_never_falls_back_to_native_or_recent(self):
        self.agent['agent_session'] = {'kind': 'id', 'value': 'first'}
        add_message(self.connection, 'do not show')
        self.marker.unlink()
        with self.paths():
            result = preview.conversation_preview('pane-one', self.request)
        assert isinstance(result, str)
        self.assertIn('unavailable', result)
        self.assertNotIn('do not show', result)

    def test_native_id_with_exact_process_profile(self):
        self.agent['agent_session'] = {'kind': 'id', 'value': 'second'}
        add_message(self.connection, 'native response', session='second')
        with patch.object(preview, 'process_session_paths', return_value={'HERMES_HOME': str(self.home)}):
            result = preview.conversation_preview('pane-one', self.request)
            assert isinstance(result, str)
            self.assertIn('native response', result)

    def test_other_agent_keeps_terminal_fallback(self):
        self.agent = {'agent': 'claude'}
        self.assertIsNone(preview.conversation_preview('pane-one', self.request))

    def test_profile_mismatch_rejected(self):
        self.connection.execute("UPDATE sessions SET profile_name='other' WHERE id='first'")
        self.connection.commit()
        with self.assertRaises(ValueError):
            preview.recent_messages(self.home, 'first')

    def test_missing_database_not_created(self):
        missing = self.home / 'missing'
        with self.assertRaises(sqlite3.Error):
            preview.recent_messages(missing, 'first')
        self.assertFalse((missing / 'state.db').exists())

    def test_no_identity_does_not_use_callers_home(self):
        with patch.object(preview, 'process_session_paths', return_value={}):
            result = preview.conversation_preview('pane-one', self.request)
            assert isinstance(result, str)
            self.assertIn('unavailable', result)

    def test_bounded_newest_first_and_utf8(self):
        for number in range(preview.MESSAGE_LIMIT + 8):
            add_message(self.connection, f'record {number}')
        add_message(self.connection, '한글' * preview.CONTENT_BYTES)
        messages = preview.recent_messages(self.home, 'first')
        self.assertEqual(len(messages), preview.MESSAGE_LIMIT)
        self.assertEqual(len(messages[0]['content']), preview.CONTENT_BYTES)
        result = preview.render_messages(messages)
        self.assertIn('[Message truncated]', result)
        self.assertNotIn('\ufffd', result)
        self.assertNotIn('\nrecord 0\n', result)
        self.assertIn('\x1b[1;38;2;139;233;253mASSISTANT', result)

    def test_hidden_internal_inactive_and_reasoning_not_exposed(self):
        add_message(self.connection, 'VISIBLE', role='user')
        add_message(self.connection, 'HIDDEN_SENTINEL', role='system')
        add_message(self.connection, 'HIDDEN_SENTINEL', active=0)
        add_message(self.connection, 'HIDDEN_SENTINEL', _compressed_summary=1)
        add_message(self.connection, 'HIDDEN_SENTINEL', display_kind='hidden')
        add_message(self.connection, 'HIDDEN_SENTINEL', display_metadata='{"model_only":true}')
        self.assertNotIn('HIDDEN_SENTINEL', preview.render_messages(preview.recent_messages(self.home, 'first')))

    def test_tools_project_names_without_arguments_or_results(self):
        calls = [{'function': {'name': 'read_file', 'arguments': 'PRIVATE_ARGUMENT' * 10000}}]
        add_message(self.connection, '', tool_calls=json.dumps(calls))
        add_message(self.connection, 'PRIVATE_RESULT' * 10000, role='tool', tool_name='read_file')
        messages = preview.recent_messages(self.home, 'first')
        self.assertIsNone(messages[0]['content'])
        result = preview.render_messages(messages)
        self.assertIn('Calling: read_file', result)
        self.assertIn('read_file · result recorded', result)
        self.assertNotIn('PRIVATE_', result)

    def test_structured_text_and_attachments(self):
        content = '\0json:' + json.dumps([
            {'type': 'text', 'text': '한글 본문'},
            {'type': 'image_url', 'image_url': {'url': 'PRIVATE_IMAGE'}},
            {'type': 'reasoning', 'text': 'PRIVATE_REASONING'},
        ])
        add_message(self.connection, content, role='user')
        result = preview.render_messages(preview.recent_messages(self.home, 'first'))
        self.assertIn('한글 본문', result)
        self.assertNotIn('PRIVATE_', result)
        self.assertEqual(preview.content_text(b'\0json:[', True), '[Large structured message omitted]')
        self.assertEqual(preview.content_text(b'\0json:broken', False), '[Unreadable structured message]')

    def test_escape_controls_removed_with_content_preserved(self):
        text = '\x1b[38;2;1;2;3m한글\x1b[0m\n    code │ literal ┃   \n\x1b]52;c;PRIVATE_CLIPBOARD\x07end'
        result = preview.safe_text(text)
        self.assertEqual(result, '한글\n    code │ literal ┃\nend')
        self.assertNotIn('[38;', result)

    def test_same_row_updates_visible_without_max_id_cache(self):
        add_message(self.connection, 'before')
        self.assertIn('before', preview.render_messages(preview.recent_messages(self.home, 'first')))
        self.connection.execute("UPDATE messages SET content='after'")
        self.connection.commit()
        self.assertIn('after', preview.render_messages(preview.recent_messages(self.home, 'first')))

    def test_unambiguous_compression_continuation(self):
        self.connection.execute("UPDATE sessions SET end_reason='compression' WHERE id='first'")
        self.connection.execute("UPDATE sessions SET parent_session_id='first' WHERE id='second'")
        self.connection.execute("UPDATE sessions SET parent_session_id='first', model_config=? WHERE id='third'",
                                ('{"_branched_from":"first"}',))
        self.connection.commit()
        add_message(self.connection, 'continuation', session='second')
        add_message(self.connection, 'not continuation', session='third')
        result = preview.render_messages(preview.recent_messages(self.home, 'first'))
        self.assertIn('\ncontinuation', result)
        self.assertNotIn('not continuation', result)

    def test_ambiguous_continuation_fails_closed(self):
        self.connection.execute("UPDATE sessions SET end_reason='compression' WHERE id='first'")
        self.connection.execute("UPDATE sessions SET parent_session_id='first' WHERE id IN ('second','third')")
        self.connection.commit()
        with self.assertRaises(ValueError):
            preview.recent_messages(self.home, 'first')

    def test_missing_schema_fails_without_repair(self):
        self.connection.execute('ALTER TABLE messages DROP COLUMN display_metadata')
        self.connection.commit()
        before = (self.home / 'state.db').read_bytes()
        with self.paths():
            result = preview.conversation_preview('pane-one', self.request)
        assert isinstance(result, str)
        self.assertIn('unavailable', result)
        self.assertEqual(before, (self.home / 'state.db').read_bytes())

    def test_locked_database_does_not_block_or_repair(self):
        self.connection.execute('BEGIN EXCLUSIVE')
        with self.paths():
            result = preview.conversation_preview('pane-one', self.request)
        assert isinstance(result, str)
        self.assertIn('unavailable', result)
        self.connection.rollback()

    def test_session_id_is_bound_not_interpolated(self):
        add_message(self.connection, 'PRIVATE_CONVERSATION')
        with self.assertRaises(ValueError):
            preview.recent_messages(self.home, "first' OR 1=1 --")

    def test_malformed_tool_calls_do_not_hide_text(self):
        for calls in ('not json', '["not json"]', 'null'):
            add_message(self.connection, 'VISIBLE_REPLY', tool_calls=calls)
        result = preview.render_messages(preview.recent_messages(self.home, 'first'))
        self.assertEqual(result.count('VISIBLE_REPLY'), 3)

    def test_linux_environment_allowlist(self):
        with patch.object(preview.sys, 'platform', 'linux'), patch.object(Path, 'read_bytes', return_value=(
            b'API_KEY=PRIVATE\0HERMES_HOME=/profile\0HERMES_TUI_ACTIVE_SESSION_FILE=/active.json\0TOKEN=PRIVATE\0'
        )):
            self.assertEqual(preview.process_session_paths(123), {
                'HERMES_HOME': '/profile', 'HERMES_TUI_ACTIVE_SESSION_FILE': '/active.json',
            })

    def test_process_environment_allowlist(self):
        if sys.platform != 'darwin':
            self.skipTest('macOS ps parser')
        result = type('Result', (), {'stdout': 'node entry.js API_KEY=PRIVATE HERMES_HOME=/a path/profiles/work HERMES_TUI_ACTIVE_SESSION_FILE=/a path/active.json TOKEN=PRIVATE'})()
        with patch.object(preview.subprocess, 'run', return_value=result):
            self.assertEqual(preview.process_session_paths(123), {
                'HERMES_HOME': '/a path/profiles/work',
                'HERMES_TUI_ACTIVE_SESSION_FILE': '/a path/active.json',
            })


if __name__ == '__main__':
    unittest.main()
