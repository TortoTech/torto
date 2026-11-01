#!/usr/bin/env python3
"""Create the bounded, symlink-free ZIP consumed by the macOS updater.

Run after stripping and codesigning. Sign its metadata with sign-macos-update.
No resource forks or __MACOSX entries are added; executable modes are preserved.
"""
import argparse
import hashlib
from pathlib import Path
import re
import stat
import zipfile


def package(app: Path, output: Path, version: str, arch: str) -> Path:
    if not re.fullmatch(r"\d+\.\d+\.\d+", version) or arch not in ("arm64", "x86_64"):
        raise ValueError("Expected a stable version and supported architecture")
    if app.name != "Torto.app" or not app.is_dir() or app.is_symlink():
        raise ValueError("Expected a real Torto.app directory")
    files = sorted(app.rglob("*"))
    if len(files) > 16384:
        raise ValueError("Too many application files")
    total = 0
    for file in files:
        mode = file.lstat().st_mode
        if not (stat.S_ISDIR(mode) or stat.S_ISREG(mode)):
            raise ValueError(f"Unsupported bundle entry: {file}")
        if file.is_file():
            total += file.stat().st_size
    if total > 512 * 1024 * 1024:
        raise ValueError("Expanded update exceeds 512 MiB")
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"Torto-{version}-macos-{arch}.zip"
    try:
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as zip:
            for file in files:
                if file.is_file():
                    zip.write(file, file.relative_to(app.parent).as_posix())
        if archive.stat().st_size > 256 * 1024 * 1024:
            raise ValueError("Compressed update exceeds 256 MiB")
        digest = hashlib.sha256()
        with archive.open("rb") as file:
            for chunk in iter(lambda: file.read(1024 * 1024), b""):
                digest.update(chunk)
        digest = digest.hexdigest()
        archive.with_suffix(".zip.sha256").write_text(f"{digest}  {archive.name}\n", encoding="utf-8")
    except Exception:
        archive.unlink(missing_ok=True)
        raise
    return archive


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("version")
    parser.add_argument("arch", choices=["arm64", "x86_64"])
    args = parser.parse_args()
    print(package(args.app, args.output, args.version, args.arch))
