"""Build reproducible Herdr plugin archives from an explicit file list."""

import argparse
import gzip
import hashlib
import io
from pathlib import Path
import re
import tarfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
FILES = (
    "LICENSE", "README.md", "CHANGELOG.md", "herdr-plugin.toml", "run",
    "agent-picker.py", "agent_completion_times.py", "check_dependencies.py",
    "scripts/build_package.py", "tests/test_completion_times.py",
    "tests/test_date_sections.py", "tests/test_picker.py", "tests/test_live_picker.py",
    "tests/test_package.py",
    "docs/images/agent-picker-demo.png", "docs/images/agent-picker-demo.svg",
)


def build(output, tag=None):
    manifest = tomllib.loads((ROOT / "herdr-plugin.toml").read_text())
    version = manifest["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise ValueError("Invalid package version")
    if tag is not None and tag != f"v{version}":
        raise ValueError(f"Tag {tag} does not match manifest version {version}")
    name = f"herdr-agent-picker-{version}"
    files = []
    for filename in sorted(FILES):
        path = ROOT / filename
        if path.is_symlink():
            raise ValueError(f"Package file must not be a symlink: {filename}")
        files.append((f"{name}/{filename}", path.read_bytes(), 0o755 if filename == "run" else 0o644))
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"{name}.tar.gz"
    with archive.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as tar:
                for filename, data, mode in files:
                    entry = tarfile.TarInfo(filename)
                    entry.size = len(data)
                    entry.mode = mode
                    entry.mtime = 315532800  # 1980-01-01, shared with ZIP.
                    tar.addfile(entry, io.BytesIO(data))
    zipped = output / f"{name}.zip"
    with zipfile.ZipFile(zipped, "w", compression=zipfile.ZIP_DEFLATED) as zip:
        for filename, data, mode in files:
            entry = zipfile.ZipInfo(filename, date_time=(1980, 1, 1, 0, 0, 0))
            entry.create_system = 3
            entry.external_attr = (0o100000 | mode) << 16
            entry.compress_type = zipfile.ZIP_DEFLATED
            zip.writestr(entry, data)
    checksums = output / "SHA256SUMS"
    checksums.write_text("".join(
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
        for path in (archive, zipped)
    ))
    return archive, zipped, checksums


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    parser.add_argument("--tag", help="Require this Git tag to match the manifest version")
    arguments = parser.parse_args()
    for result in build(arguments.output, arguments.tag):
        print(result)
