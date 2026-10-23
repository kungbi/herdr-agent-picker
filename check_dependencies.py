"""Check installation prerequisites without connecting to a Herdr session."""

import os
import re
import shutil
import subprocess
import sys


def main():
    if sys.version_info < (3, 11):
        raise RuntimeError("Python 3.11 or newer is required")
    print(f"Python: {sys.version.split()[0]} ({sys.executable})")
    for executable, minimum in ((os.environ.get("HERDR_BIN_PATH", "herdr"), (0, 9, 1)), ("fzf", (0, 73, 1))):
        path = shutil.which(executable)
        if path is None:
            raise RuntimeError(f"Missing dependency: {executable}")
        result = subprocess.run([path, "--version"], text=True, capture_output=True, check=True)
        match = re.search(r"(\d+)\.(\d+)\.(\d+)", result.stdout)
        if match is None or tuple(map(int, match.groups())) < minimum:
            raise RuntimeError(f"{executable} requires version {'.'.join(map(str, minimum))} or newer")
        print(f"{result.stdout.strip()} ({path})")
    print("Dependencies OK")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Agent Picker: {error}", file=sys.stderr)
        sys.exit(1)
