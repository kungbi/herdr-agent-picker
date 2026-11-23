"""Only tests that execute the Rust binary belong in this suite."""
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
# No legacy Python-runtime unit tests are invoked, including via discovery.
TESTS = ('test_runtime.py', 'test_picker.py', 'test_live_picker.py',
         'test_composer.py',
         'test_conversation_picker.py', 'test_preview_resize.py',
         'test_reminder_picker.py', 'test_package.py')
for name in TESTS:
    subprocess.run([sys.executable, str(ROOT / 'tests' / name)], check=True)
print('PASS: native Rust regression suite (no Python runtime coverage)', flush=True)
