#!/usr/bin/env python3
"""Run the ordinary bundled app with a real portable profile on APFS and exFAT.

All apps, credentials and books are inert temporary fixtures. No personal
profile, native credential store or paid model is accessed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import signal
import shutil
import subprocess
import tempfile

DATA = "torto-portable-data"
SECRET = "portable-fixture-key-not-a-real-credential"

def run(*args, **kwargs):
    result = subprocess.run([str(arg) for arg in args], stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=120, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{args[0]} failed: {result.stderr.decode(errors='replace')[-3000:]}")
    return result

def make_app(source, destination):
    if source.is_dir():
        shutil.copytree(source, destination)
    else:
        binary = destination / "Contents/MacOS/torto"
        binary.parent.mkdir(parents=True)
        shutil.copy2(source, binary)
        (destination / "Contents/Info.plist").write_bytes(plistlib.dumps({
            "CFBundleIdentifier": "io.github.l-chris.torto", "CFBundleExecutable": "torto",
            "CFBundleName": "Torto Portable Check", "CFBundlePackageType": "APPL",
            "CFBundleVersion": "0.9.0", "CFBundleShortVersionString": "0.9.0",
            "LSMinimumSystemVersion": "15.0", "NSHighResolutionCapable": True,
        }))
        run("/usr/bin/codesign", "--force", "--sign", "-", destination)
    run("/usr/bin/codesign", "--verify", "--deep", "--strict", destination)

def check(app, report, book=None):
    args = ["/usr/bin/open", "-n", "-W", "-a", app, "--args", "--smoke-test-portable", report]
    if book is not None:
        args.append(book)
    environment = {key: value for key, value in os.environ.items() if not key.startswith("REBOOK_AI_")}
    process = subprocess.Popen([str(arg) for arg in args], stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment)
    try:
        stdout, stderr = process.communicate(timeout=90)
    except subprocess.TimeoutExpired:
        # Launch Services owns the app process, rather than `open`. Kill only
        # the PID recorded by this fresh, path-matched fixture invocation.
        try:
            pid = json.loads((report / "pid.json").read_text())["pid"]
            command = run("/bin/ps", "-p", pid, "-o", "command=").stdout.decode()
            if str(app) in command and str(report) in command and "--smoke-test-portable" in command:
                os.kill(pid, signal.SIGKILL)
        except (OSError, ValueError, RuntimeError, KeyError):
            pass
        process.kill(); process.communicate()
        raise RuntimeError("Portable GUI startup timed out")
    result = subprocess.CompletedProcess(args, process.returncode, stdout, stderr)
    (report / "console.log").write_bytes(result.stdout + result.stderr)
    assert result.returncode == 0, result.stderr.decode(errors="replace")[-3000:]
    value = json.loads((report / "result.json").read_text())
    assert value["success"], value
    assert value["portable_profile"], value
    assert Path(value["data_directory"]).resolve() == (app.parent / DATA).resolve(), value

def exercise(source, parent, label):
    root = parent / "Portable books & 目录"
    root.mkdir()
    app = root / "Renamed Reader.app"
    make_app(source, app)
    data = root / DATA
    config = data / "config"
    config.mkdir(parents=True)
    credentials = config / "credentials.json"
    credentials.write_text(json.dumps({"version": 1, "entries": {
        json.dumps(["fixture", "account"], separators=(",", ":")): SECRET,
    }}), encoding="utf-8")
    # A tiny generated book exercises the real reader/renderer, with its source
    # and artifacts residing entirely inside this temporary portable directory.
    book = root / "book.fb2"
    book.write_text('''<?xml version="1.0" encoding="UTF-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info>
<book-title>Portable fixture</book-title><lang>en</lang></title-info></description>
<body><section><title><p>Reading</p></title><p>A portable book keeps its text and reading position when the folder moves.</p>
<p>中文测试。This book contains no external assets or AI requests.</p></section></body></FictionBook>''', encoding="utf-8")
    check(app, root / "first-check", book)
    assert SECRET in credentials.read_text(encoding="utf-8")
    assert (config / "webdav-sync.json").exists()
    assert (config / "portable-devices.json").exists()
    identity = (config / "webdav-sync.json").read_bytes()
    devices = (config / "portable-devices.json").read_bytes()
    library = data / "library"
    manifest = json.loads((library / "library.json").read_text())
    assert len(manifest["books"]) == 1, manifest
    stored = manifest["books"][0]
    assert stored["id"] == hashlib.sha256(book.read_bytes()).hexdigest()
    assert (library / "books" / stored["storage_name"]).read_bytes() == book.read_bytes()
    run("/usr/bin/codesign", "--verify", "--deep", "--strict", app)
    moved = parent / "Another outer name 任意名字"
    root.rename(moved)
    app = moved / app.name
    renamed = moved / "Another.app"
    app.rename(renamed)
    check(renamed, moved / "second-check", moved / DATA / "library/books" / stored["storage_name"])
    config = moved / DATA / "config"
    assert SECRET in (config / "credentials.json").read_text(encoding="utf-8")
    assert (config / "webdav-sync.json").read_bytes() == identity
    assert (config / "portable-devices.json").read_bytes() == devices
    after = json.loads((moved / DATA / "library/library.json").read_text())
    assert len(after["books"]) == 1 and after["books"][0]["id"] == stored["id"]
    assert (moved / DATA / "library/books" / stored["storage_name"]).read_bytes() == (moved / "book.fb2").read_bytes()
    assert not (moved / "second-check/profile/config/credentials.json").exists()
    run("/usr/bin/codesign", "--verify", "--deep", "--strict", renamed)
    for log in (moved / DATA / "logs").glob("*"):
        if log.is_file():
            assert SECRET not in log.read_text(errors="replace"), "Credentials leaked to a log"
    print(f"PASS {label}: real GUI reading, plaintext credentials, folder/app rename, stable sync identity and bundle signature", flush=True)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path, help="Built Torto.app or production executable")
    parser.add_argument("--skip-exfat", action="store_true", help="Run only the host filesystem check")
    parser.add_argument("--output", type=Path, help="Retain isolated fixtures and diagnostics in a new directory")
    args = parser.parse_args()
    if os.uname().sysname != "Darwin" or os.getuid() == 0:
        raise RuntimeError("Run as an ordinary macOS user")
    def execute(temporary):
        root = Path(temporary).resolve()
        source = args.app.resolve()
        exercise(source, root, "host filesystem")
        if not args.skip_exfat:
            image = root / "portable.sparseimage"
            mount = root / "mounted"
            mount.mkdir()
            run("/usr/bin/hdiutil", "create", "-size", "1g", "-fs", "ExFAT", "-type", "SPARSE", "-volname", "TortoPortableCheck", image)
            run("/usr/bin/hdiutil", "attach", image, "-nobrowse", "-mountpoint", mount)
            try:
                info = plistlib.loads(run("/usr/sbin/diskutil", "info", "-plist", mount).stdout)
                assert info["FilesystemType"].lower() == "exfat", info.get("FilesystemType")
                exercise(source, mount, "exFAT disk image")
            finally:
                run("/usr/bin/hdiutil", "detach", mount)
    # Presentation checks require an awake display. This assertion is limited
    # to this runner's lifetime and restores normal idle-sleep behavior on exit.
    keep_awake = subprocess.Popen(["/usr/bin/caffeinate", "-d", "-i", "-u", "-w", str(os.getpid())],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        if args.output:
            args.output.mkdir(parents=True, exist_ok=False)
            execute(args.output)
        else:
            with tempfile.TemporaryDirectory(prefix="torto-mac-portable-") as temporary:
                execute(temporary)
    finally:
        keep_awake.terminate()
        keep_awake.wait(timeout=5)

if __name__ == "__main__":
    main()
