#!/usr/bin/env python3
"""Exercise the production macOS helper against isolated, ad-hoc signed fixtures.

Requires the production desktop binary, sign-macos-update example, and the
TORTO_MACOS_UPDATE_SIGNING_KEY environment variable. Never accesses the library
or /Applications. Test apps are tiny executables that implement the handshake.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import stat
import subprocess
import tempfile
import time
import uuid
import zipfile
import importlib.util

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("package_update", ROOT / "scripts/package-macos-update.py")
pack = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pack)
ARCH = "arm64" if platform.machine() == "arm64" else "x86_64"


def run(*args):
    subprocess.run([str(a) for a in args], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def app(directory, version, healthy):
    target = directory / "Torto.app"
    binary = target / "Contents/MacOS/torto"
    binary.parent.mkdir(parents=True)
    (target / "Contents/Info.plist").write_bytes(plistlib.dumps({
        "CFBundleIdentifier": "io.github.l-chris.torto", "CFBundleExecutable": "torto",
        "CFBundlePackageType": "APPL", "CFBundleVersion": version,
        "CFBundleShortVersionString": version,
    }))
    source = directory / "fixture.c"
    source.write_text('''#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "--hold")) { sleep(60); return 0; }
    if (argc != 3 || strcmp(argv[1], "--torto-update-health")) return 0;
    HEALTH
}
'''.replace("HEALTH", '''char file[4096]; snprintf(file, sizeof(file), "%s/healthy", argv[2]);
    FILE *f = fopen(file, "w"); if (!f) return 2;
    fputs(strrchr(argv[2], '/') + 1, f); fclose(f); sleep(2); return 0;''' if healthy else "return 23;"))
    run("/usr/bin/clang", source, "-o", binary)
    run("/usr/bin/codesign", "--force", "--sign", "-", target)
    return target


def state(job):
    try:
        return json.loads((job / "state.json").read_text())
    except FileNotFoundError:
        return {}


def wait_for(job, process, stages, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = state(job)
        if value.get("stage") in stages:
            return value
        if process.poll() is not None:
            raise AssertionError(f"Helper exited early: {value}")
        time.sleep(.05)
    raise AssertionError(f"Timed out: {state(job)}")


def write_request(job, target, archive, signature, parent):
    version = signature["version"]
    request = {"target": str(target), "parent": parent, "token": job.name,
        "update": {"installer_path": str(archive), "release": {"version": version,
        "notes": "isolated smoke fixture", "macos_signature": signature,
        "asset": {"name": archive.name, "url": f"https://github.com/TortoTech/torto/releases/download/v{version}/{archive.name}",
                  "github_asset_id": None, "sha256": signature["sha256"], "size": signature["size"]}}}}
    file = job / "request.json"
    file.write_text(json.dumps(request))
    file.chmod(0o600)


def scenario(root, helper, signer, name, *, healthy=True, tamper=False, unsafe=False, symlink=False, bad_signature=False, wrong_version=False, downgrade=False, readonly=False, multiple=False):
    directory = root / name
    directory.mkdir()
    target = app(directory / "installed", "0.0.1", True)
    old = (target / "Contents/MacOS/torto").read_bytes()
    version = "0.0.1" if downgrade else "0.0.2"
    source = app(directory / "source", "0.0.3" if wrong_version else version, healthy)
    archive = pack.package(source, directory / "dist", version, ARCH)
    if unsafe:
        with zipfile.ZipFile(archive, "a") as zip:
            zip.writestr("Torto.app/../../outside", "must never be written")
    if symlink:
        with zipfile.ZipFile(archive, "a") as zip:
            info = zipfile.ZipInfo("Torto.app/Contents/escape")
            info.create_system = 3
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            zip.writestr(info, "../../outside")
    run(signer, archive, version)
    signature = json.loads(archive.with_suffix(".zip.sig.json").read_text())
    if bad_signature:
        signature["signature"] = "A" * 88
    if tamper:
        data = bytearray(archive.read_bytes())
        data[len(data) // 2] ^= 1
        archive.write_bytes(data)
    job = directory / str(uuid.uuid4())
    job.mkdir(mode=0o700)
    archive = Path(shutil.copy2(archive, job / archive.name))
    parent = subprocess.Popen(["/bin/sleep", "60"])
    write_request(job, target, archive, signature, parent.pid)
    if readonly:
        target.parent.chmod(0o555)
    extra = subprocess.Popen([str(target / "Contents/MacOS/torto"), "--hold"]) if multiple else None
    process = subprocess.Popen([str(helper), "--torto-update-helper", str(job)], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        prepared = wait_for(job, process, {"waiting", "failed"})
        if tamper or unsafe or symlink or bad_signature or wrong_version or downgrade or readonly or multiple:
            assert prepared["stage"] == "failed", prepared
            assert parent.poll() is None
            assert (target / "Contents/MacOS/torto").read_bytes() == old
            assert not (directory / "installed/outside").exists()
        else:
            assert prepared["stage"] == "waiting", prepared
            assert (target / "Contents/MacOS/torto").read_bytes() == old
            parent.terminate()
            parent.wait()
            final = wait_for(job, process, {"complete", "failed"})
            assert final["stage"] == ("complete" if healthy else "failed"), final
            info = plistlib.loads((target / "Contents/Info.plist").read_bytes())
            assert info["CFBundleShortVersionString"] == (version if healthy else "0.0.1")
            if not healthy:
                assert (target / "Contents/MacOS/torto").read_bytes() == old
        process.wait(timeout=10)
        assert not list(target.parent.glob(".torto-update-*"))
        print(f"PASS {name}", flush=True)
    finally:
        if readonly:
            target.parent.chmod(0o755)
        if process.poll() is None:
            process.kill()
        process.communicate(timeout=5)
        if parent.poll() is None:
            parent.terminate()
        parent.wait(timeout=5)
        if extra:
            extra.terminate()
            extra.wait(timeout=5)


def gui_health(root, helper, signer):
    directory = root / "real-gui-health"
    directory.mkdir()
    version = re.search(r'^version = "([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M)[1]
    target = app(directory / "source", version, True)
    archive = pack.package(target, directory / "dist", version, ARCH)
    run(signer, archive, version)
    signature = json.loads(archive.with_suffix(".zip.sig.json").read_text())
    job = directory / str(uuid.uuid4())
    job.mkdir(mode=0o700)
    archive = Path(shutil.copy2(archive, job / archive.name))
    write_request(job, target, archive, signature, os.getpid())
    output = directory / "smoke"
    result = subprocess.run([str(helper), "--torto-update-health", str(job), "--smoke-test", str(output)],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=90)
    assert result.returncode == 0, result.stderr.decode(errors="replace")[-4096:]
    report = json.loads((output / "result.json").read_text())
    assert report["success"], report
    assert (job / "healthy").read_text() == job.name
    assert (output / "profile").is_dir()
    print("PASS real-gui-first-frame-health (isolated profile)", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("helper", type=Path)
    parser.add_argument("signer", type=Path)
    args = parser.parse_args()
    if platform.system() != "Darwin" or os.getuid() == 0:
        raise RuntimeError("Run as an ordinary macOS user")
    with tempfile.TemporaryDirectory(prefix="torto-update-smoke-") as temp:
        root = Path(temp).resolve()
        helper = root / "TortoUpdater"
        shutil.copy2(args.helper.resolve(), helper)
        for name, options in [
            ("replace-and-confirm", {}), ("startup-failure-rollback", {"healthy": False}),
            ("tampered-archive", {"tamper": True}), ("unsafe-archive-path", {"unsafe": True}),
            ("symlink-archive", {"symlink": True}), ("invalid-publisher-signature", {"bad_signature": True}),
            ("wrong-bundle-version", {"wrong_version": True}), ("reject-downgrade", {"downgrade": True}),
            ("readonly-installation", {"readonly": True}),
            ("another-instance-running", {"multiple": True}),
        ]:
            scenario(root, helper, args.signer.resolve(), name, **options)
        gui_health(root, helper, args.signer.resolve())


if __name__ == "__main__":
    main()
