#!/usr/bin/env python3
"""Package the same release executable used by MSI, without user data or PDBs."""
import argparse
import hashlib
from pathlib import Path
import re
import zipfile

ROOT = Path(__file__).resolve().parents[1]
README = """Torto Portable (Windows x64)

Extract the complete ZIP to a writable directory and run torto.exe.
Keep torto-portable.json beside torto.exe. Data is created beside the program.
Move/copy the complete directory to carry your books, settings and credentials.
API keys and passwords are stored as plaintext in Data/config/credentials.json.
Only one reader may open a particular Data directory at a time.
The built-in updater preserves Data and rolls back if the new program cannot start.
For a manual update, close Torto and replace program files; keep your Data folder.
Do not install an MSI over a portable directory.
"""

def package(executable: Path, output: Path, version: str) -> Path:
    if not executable.is_file() or executable.read_bytes()[:2] != b"MZ":
        raise ValueError("Expected the Windows release executable")
    if not all(part.isdigit() for part in version.split('.')) or len(version.split('.')) != 3:
        raise ValueError("Expected a stable three-part version")
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"Torto-{version}-windows-x86_64-portable.zip"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as bundle:
        bundle.write(executable, "torto.exe")
        bundle.writestr("torto-portable.json", '{"version":1}\n')
        bundle.write(ROOT / "LICENSE", "LICENSE")
        bundle.writestr("README.txt", README)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(archive.name + ".sha256").write_text(f"{digest}  {archive.name}\n", encoding="utf-8")
    return archive

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--executable", type=Path, default=ROOT / "target/release/torto.exe")
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    parser.add_argument("--version", default=re.search(r'^version\s*=\s*"([^"]+)"', (ROOT / "Cargo.toml").read_text(encoding="utf-8").split("[workspace.package]", 1)[1], re.MULTILINE).group(1))
    args = parser.parse_args()
    print(package(args.executable, args.output, args.version))
