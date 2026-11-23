"""Reproducibility, exact native/source contents and extracted native launcher."""
import hashlib
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile
from rust_support import BINARY, ROOT

module = runpy.run_path(str(ROOT / 'scripts/build_package.py'))
build = module['build']
version = tomllib.loads((ROOT / 'herdr-plugin.toml').read_text())['version']
with tempfile.TemporaryDirectory(prefix='herdr-rust-package-') as directory:
    root = Path(directory)
    for source in (False, True):
        first = build(root / 'first', f'v{version}', binary=BINARY, source=source)
        second = build(root / 'second', f'v{version}', binary=BINARY, source=source)
        assert all(a.read_bytes() == b.read_bytes() for a, b in zip(first, second))
        target = 'source' if source else module['host_target']()
        name = f'herdr-agent-picker-{version}-{target}'
        names = module['COMMON'] + (module['SOURCE'] if source else ('bin/herdr-agent-picker',))
        expected = {f'{name}/{s}' for s in names}
        with tarfile.open(first[0]) as archive:
            assert set(archive.getnames()) == expected
            assert archive.getmember(f'{name}/run').mode == 0o755
            archive.extractall(root / 'unpacked', filter='data')
        with zipfile.ZipFile(first[1]) as archive:
            assert set(archive.namelist()) == expected and archive.testzip() is None
        for line in first[2].read_text().splitlines():
            checksum, filename = line.split('  ')
            assert hashlib.sha256((first[2].parent / filename).read_bytes()).hexdigest() == checksum
        extracted = root / 'unpacked' / name
        subprocess.run(['sh', '-n', str(extracted / 'run')], check=True)
        assert not (extracted / 'agent-picker.py').exists()
        if source:
            subprocess.run(['cargo', 'check', '--offline', '--locked',
                            '--manifest-path', str(extracted / 'Cargo.toml'),
                            '--target-dir', str(ROOT / 'target/package-check')], check=True)
        if not source:
            assert (extracted / 'bin/herdr-agent-picker').stat().st_mode & 0o777 == 0o755
            result = subprocess.run(['sh', str(extracted / 'run'), '--version'],
                                    env=dict(os.environ, PYTHON='/nonexistent', HERDR_AGENT_PICKER_PYTHON='/nonexistent'),
                                    text=True, capture_output=True, check=True)
            assert result.stdout.startswith('herdr-agent-picker ')
    try:
        build(root / 'bad', 'v0.0.0-wrong', binary=BINARY)
    except ValueError:
        pass
    else:
        raise AssertionError('Mismatched tag accepted')
    script = root / 'fake-native'
    script.write_text('#!/usr/bin/env python3\n')
    try:
        build(root / 'bad', binary=script)
    except ValueError:
        pass
    else:
        raise AssertionError('Python masqueraded as a native binary')
print('PASS: deterministic native/source archives, exact contents, modes, checksums, extracted Rust launcher')
