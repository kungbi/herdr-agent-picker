"""Whitespace/ANSI and bounded per-popup controls without a live source pane."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
spec = importlib.util.spec_from_file_location('picker', ROOT / 'agent-picker.py')
assert spec is not None and spec.loader is not None
picker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(picker)
STYLES = re.compile(r'\x1b\[[0-9;:]*m')


class TerminalPreviewTests(unittest.TestCase):
    def render(self, source, state=None, columns=20):
        output = io.StringIO()
        with patch.object(picker.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, source)), \
                patch.dict(os.environ, FZF_PREVIEW_COLUMNS=str(columns)), contextlib.redirect_stdout(output):
            if state is None:
                picker.preview('w1:p1')
            else:
                picker.preview('w1:p1', state)
        return output.getvalue()

    def test_shared_gutter_preserves_ansi_relative_indent_and_blank_rows(self):
        source = '\x1b[31m  \x1b[1m  heading\x1b[0m\n\n      code\n    │ A │ B │\n    **literal**\n'
        result = self.render(source)
        self.assertEqual(STYLES.sub('', result), 'heading\n\n  code\n│ A │ B │\n**literal**\n')
        self.assertEqual(STYLES.findall(result), STYLES.findall(source))

    def test_gutter_is_only_shared_ascii_spaces(self):
        for source in ('no gutter\n    code\n', '  code\n\tother\n',
                       '    code\n  \tother\n', '\u3000wide space\n  code\n'):
            self.assertEqual(self.render(source), source)
        self.assertEqual(self.render('    code\n      nested\n\n'), 'code\n  nested\n\n')

    def test_frame_cleanup_then_common_gutter(self):
        source = '\x1b[32m    한글  e\u0301    │\x1b[0m\n' + ' ' * len('    ____  e    ') + '│\n'
        self.assertEqual(STYLES.sub('', self.render(source)), '한글  e\u0301\n\n')

    def test_large_blank_gap_is_compacted_without_guessing_image_content(self):
        source = 'Tool calls\n' + '\n' * 18 + 'Latest answer\n'
        result = self.render(source)
        self.assertEqual(result, 'Tool calls\n\nLatest answer\n')
        self.assertNotIn('[이미지', result)

    def test_blank_compaction_preserves_styles_and_relative_code_indent(self):
        source = '\x1b[31mheading\n\n\x1b[32m   \n\x1b[0m\n    code\n\nnext\n'
        result = self.render(source)
        self.assertEqual(STYLES.sub('', result), 'heading\n\n    code\n\nnext\n')
        self.assertEqual(STYLES.findall(result), STYLES.findall(source))

    def test_image_notice_does_not_claim_an_image_was_detected(self):
        from terminal_preview import PREVIEW_LABEL
        self.assertIn('이미지는 Enter로 원본 확인', PREVIEW_LABEL)
        with tempfile.TemporaryDirectory() as directory:
            from terminal_preview import view_action
            action = view_action(Path(directory) / 'preview.json', 'resume')
            self.assertIn(PREVIEW_LABEL, action)

    def test_pan_cjk_and_combining_keeps_styles_and_cell_boundary(self):
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory) / 'preview.json'
            state.write_text(json.dumps({'offset': 3, 'wrap': False, 'full': False}))
            source = '\x1b[31mAB한\x1b[1me\u0301글TAIL' + '.' * 40 + '\x1b[0m\n'
            result = self.render(source, state)
            # The left edge crosses 한: use one blank cell, never half a glyph.
            self.assertEqual(STYLES.sub('', result), ' e\u0301글TAIL' + '.' * 40 + '\n')
            self.assertEqual(STYLES.findall(result), STYLES.findall(source))
            state.write_text(json.dumps({'offset': 65536, 'wrap': False, 'full': False}))
            result = self.render(source, state)
            self.assertTrue(STYLES.sub('', result).strip('.\n') == '')
            self.assertGreaterEqual(len(STYLES.sub('', result).rstrip('\n')), 19)

    def test_pan_at_right_edge_can_move_left_immediately(self):
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory) / 'preview.json'
            state.write_text(json.dumps({'offset': 65536, 'wrap': False, 'full': False}))
            source = 'LEFT' + '.' * 40 + 'RIGHT\n'
            self.render(source, state, columns=20)
            # Do not accumulate invisible offset after reaching the right edge.
            self.assertLess(json.loads(state.read_text())['offset'], 40)
            subprocess.run([sys.executable, str(ROOT / 'agent-picker.py'), '--preview-control',
                            str(state), 'left'], capture_output=True, check=True, timeout=5)
            result = STYLES.sub('', self.render(source, state, columns=20)).rstrip('\n')
            self.assertEqual(len(result), 27)

    def test_controls_bound_state_and_do_not_read_source(self):
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory) / 'preview.json'
            environment = dict(os.environ, HERDR_BIN_PATH='/nonexistent/fictional-herdr')
            def control(action):
                return subprocess.run([sys.executable, str(ROOT / 'agent-picker.py'),
                                       '--preview-control', str(state), action], env=environment,
                                      text=True, capture_output=True, check=True, timeout=5).stdout
            self.assertIn('refresh-preview', control('right'))
            self.assertEqual(json.loads(state.read_text())['offset'], 8)
            self.assertIn('wrap-word', control('wrap'))
            self.assertEqual(json.loads(state.read_text())['offset'], 0)
            expanded = control('full')
            self.assertIn('down:80%', expanded)
            self.assertIn('wrap-word', expanded)
            self.assertIn('nowrap', control('wrap'))
            self.assertIn('right:60%', control('full'))
            state.write_text(json.dumps({'offset': 65536, 'wrap': False, 'full': False}))
            control('right')
            self.assertEqual(json.loads(state.read_text())['offset'], 65536)
            state.write_text('broken state')
            control('left')
            self.assertEqual(json.loads(state.read_text())['offset'], 0)
            self.assertLess(state.stat().st_size, 128)


if __name__ == '__main__':
    unittest.main()
