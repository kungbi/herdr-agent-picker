"""Verify the release contents, reproducibility and extracted launcher."""

import hashlib
from pathlib import Path
import runpy
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
module = runpy.run_path(str(ROOT / "scripts/build_package.py"))
build = module["build"]
version = tomllib.loads((ROOT / "herdr-plugin.toml").read_text())["version"]

with tempfile.TemporaryDirectory(prefix="herdr-package-test-") as directory:
    root = Path(directory)
    first = build(root / "first", f"v{version}")
    second = build(root / "second", f"v{version}")
    assert all(a.read_bytes() == b.read_bytes() for a, b in zip(first, second)), "Builds are not reproducible"
    expected = {f"herdr-agent-picker-{version}/{name}" for name in module["FILES"]}
    with tarfile.open(first[0]) as archive:
        assert set(archive.getnames()) == expected
        assert archive.getmember(f"herdr-agent-picker-{version}/run").mode == 0o755
        archive.extractall(root / "unpacked", filter="data")
    with zipfile.ZipFile(first[1]) as archive:
        assert set(archive.namelist()) == expected
        assert archive.testzip() is None
    for line in first[2].read_text().splitlines():
        checksum, filename = line.split("  ")
        assert hashlib.sha256((first[2].parent / filename).read_bytes()).hexdigest() == checksum
    extracted = root / "unpacked" / f"herdr-agent-picker-{version}"
    subprocess.run(["sh", "-n", str(extracted / "run")], check=True)
    subprocess.run([sys.executable, str(extracted / "tests/test_completion_times.py")], check=True)
    try:
        build(root / "bad", "v0.0.0-wrong")
    except ValueError:
        pass
    else:
        raise AssertionError("Mismatched release tag was accepted")
print("PASS: reproducible archives, exact contents, permissions, checksums and extracted completion code")
