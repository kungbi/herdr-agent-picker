"""Build deterministic native or source Herdr plugin archives (build tooling only)."""
import argparse
import gzip
import hashlib
import io
from pathlib import Path
import platform
import re
import tarfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
COMMON = ('LICENSE', 'README.md', 'CHANGELOG.md', 'herdr-plugin.toml', 'run',
          'docs/rust-port-validation.md', 'docs/scenario-parity.md',
          'docs/parity-picker.md', 'docs/parity-preview.md', 'docs/parity-dialogs.md',
          'docs/images/agent-picker-demo.png', 'docs/images/agent-picker-demo.svg',
          'docs/images/agent-picker-preview.png', 'docs/images/agent-picker-alarm.png')
SOURCE = ('Cargo.toml', 'Cargo.lock', 'scripts/build_package.py',
          'tests/common/mod.rs', 'tests/support/fake_herdr.rs',
          'tests/runtime.rs', 'tests/picker.rs', 'tests/live_picker.rs', 'tests/interactive.rs',
          'tests/preview_resize.rs', 'tests/conversation_picker.rs',
          'tests/reminder_picker.rs', 'tests/package.rs', '.github/workflows/preview.yml',
          'src/main.rs', 'src/config.rs', 'src/util.rs', 'src/completion.rs', 'src/picker.rs',
          'src/preview.rs', 'src/reminders.rs', 'src/dialog.rs', 'src/interaction.rs', 'src/sqlite.rs')


def host_target():
    system = {'Darwin': 'apple-darwin', 'Linux': 'unknown-linux-gnu'}.get(platform.system())
    arch = {'arm64': 'aarch64', 'aarch64': 'aarch64', 'x86_64': 'x86_64', 'AMD64': 'x86_64'}.get(platform.machine())
    if not system or not arch:
        raise ValueError('Only macOS/Linux on arm64/x86_64 are packaged')
    return f'{arch}-{system}'


def build(output, tag=None, binary=None, target=None, source=False):
    version = tomllib.loads((ROOT / 'herdr-plugin.toml').read_text())['version']
    cargo_version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['package']['version']
    if not re.fullmatch(r'\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?', version) or version != cargo_version:
        raise ValueError('Manifest and Cargo versions must match')
    if tag is not None and tag != f'v{version}':
        raise ValueError(f'Tag {tag} does not match manifest version {version}')
    target = 'source' if source else (target or host_target())
    if not re.fullmatch(r'[A-Za-z0-9_-]+', target):
        raise ValueError('Invalid target')
    name = f'herdr-agent-picker-{version}-{target}'
    files = []
    for filename in sorted(COMMON + (SOURCE if source else ())):
        path = ROOT / filename
        if path.is_symlink():
            raise ValueError(f'Package file must not be a symlink: {filename}')
        files.append((f'{name}/{filename}', path.read_bytes(), 0o755 if filename == 'run' else 0o644))
    if not source:
        path = Path(binary) if binary else ROOT / 'target/release/herdr-agent-picker'
        if path.is_symlink() or not path.is_file():
            raise ValueError('Build a native release binary first: sh ./run build')
        # Do not silently ship a script or the former Python runtime as a binary.
        data = path.read_bytes()
        if data[:4] not in (b'\x7fELF', b'\xcf\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\xca\xfe\xba\xbe'):
            raise ValueError('Package binary must be a native Mach-O or ELF executable')
        files.append((f'{name}/bin/herdr-agent-picker', data, 0o755))
    files.sort()
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f'{name}.tar.gz'
    with archive.open('wb') as raw:
        with gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w') as tar:
                for filename, data, mode in files:
                    entry = tarfile.TarInfo(filename)
                    entry.size, entry.mode, entry.mtime = len(data), mode, 315532800
                    tar.addfile(entry, io.BytesIO(data))
    zipped = output / f'{name}.zip'
    with zipfile.ZipFile(zipped, 'w', compression=zipfile.ZIP_DEFLATED) as zip:
        for filename, data, mode in files:
            entry = zipfile.ZipInfo(filename, date_time=(1980, 1, 1, 0, 0, 0))
            entry.create_system, entry.external_attr = 3, (0o100000 | mode) << 16
            entry.compress_type = zipfile.ZIP_DEFLATED
            zip.writestr(entry, data)
    checksums = output / f'SHA256SUMS-{target}'
    checksums.write_text(''.join(f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n' for p in (archive, zipped)))
    return archive, zipped, checksums


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT / 'dist')
    parser.add_argument('--tag')
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--target', help='Target triple of an explicitly supplied cross-built binary')
    parser.add_argument('--source', action='store_true', help='Package Rust source and test/build tooling')
    args = parser.parse_args()
    for path in build(args.output, args.tag, args.binary, args.target, args.source):
        print(path)
