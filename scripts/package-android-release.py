#!/usr/bin/env python3
"""Verify signed, per-ABI APKs before preparing public release attachments."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[1]
ABIS = ('arm64-v8a', 'x86_64')
# Public certificate metadata from the original Android release signing key.
RELEASE_CERTIFICATE = 'c46fb307beba870b610ef56995642a11b86e076126ca8173ab00aa25b13e3705'
PACKAGE = 'org.tortotech.torto'


def workspace_version():
    return re.search(r'^version = "(\d+\.\d+\.\d+)"',
                     (ROOT / 'Cargo.toml').read_text(), re.M).group(1)


def version_code(version):
    major, minor, patch = map(int, version.split('.'))
    return major * 1_000_000 + minor * 1_000 + patch


def apk_outputs(metadata, version, abis):
    if not abis or len(set(abis)) != len(abis) or not set(abis) <= set(ABIS):
        raise ValueError('Expected distinct supported Android ABIs')
    if metadata.get('applicationId') != PACKAGE or metadata.get('variantName') != 'release':
        raise ValueError('Expected the production release package')
    outputs = {}
    for item in metadata['elements']:
        filters = item.get('filters', [])
        if len(filters) == 1 and filters[0].get('filterType') == 'ABI':
            abi = filters[0]['value']
        elif not filters and len(abis) == 1:
            abi = abis[0]
        else:
            raise ValueError('Expected one ABI per APK, no universal APK')
        name = item['outputFile']
        if (abi not in abis or abi in outputs or Path(name).name != name or
                '\\' in name or not name.endswith('.apk') or
                item.get('versionName') != version or item.get('versionCode') != version_code(version)):
            raise ValueError('Unexpected APK output, ABI or version')
        outputs[abi] = name
    if set(outputs) != set(abis):
        raise ValueError('Missing release ABI')
    return outputs


def verify_badging(badging, version, abi):
    package = re.search(r"^package: name='([^']+)' versionCode='([^']+)' versionName='([^']+)'", badging, re.M)
    if not package or package.groups() != (PACKAGE, str(version_code(version)), version):
        raise ValueError('APK manifest does not match the release package/version')
    if re.search(r'^application-debuggable(?:\s|$)', badging, re.M):
        raise ValueError('A debuggable APK must not be published')
    native = re.search(r'^native-code:\s*(.*)$', badging, re.M)
    if not native or re.findall(r"'([^']+)'", native.group(1)) != [abi]:
        raise ValueError('APK manifest has an unexpected native ABI')


def verify_certificate(details, expected):
    certificates = re.findall(r'^Signer #\d+ certificate SHA-256 digest: ([a-fA-F0-9]+)$', details, re.M)
    if len(certificates) != 1 or certificates[0].lower() != expected:
        raise ValueError('APK signing certificate does not match the release key')
    if 'CN=Android Debug' in details:
        raise ValueError('A debug signing key must not be published')


def package(directory, output, sdk, version, abis, certificate):
    if not re.fullmatch(r'[a-f0-9]{64}', certificate):
        raise ValueError('Expected a SHA-256 certificate fingerprint')
    tools = sdk / 'build-tools/36.0.0'
    metadata = json.loads((directory / 'output-metadata.json').read_text())
    outputs = apk_outputs(metadata, version, abis)
    # Validate every APK before placing any file in the publication directory.
    verified = []
    for abi, name in outputs.items():
        apk = directory / name
        if not 0 < apk.stat().st_size <= 256 * 1024 * 1024:
            raise ValueError('APK exceeds the shared updater size limit')
        badging = subprocess.check_output([str(tools / 'aapt'), 'dump', 'badging', str(apk)], text=True)
        verify_badging(badging, version, abi)
        details = subprocess.check_output([str(tools / 'apksigner'), 'verify', '--verbose', '--print-certs', str(apk)], text=True)
        verify_certificate(details, certificate)
        with zipfile.ZipFile(apk) as archive:
            native = {entry.split('/')[1] for entry in archive.namelist() if entry.startswith('lib/') and entry.endswith('.so')}
            if native != {abi}:
                raise ValueError('APK native libraries do not match the requested ABI')
        subprocess.run([sys.executable, str(ROOT / 'scripts/check-android-alignment.py'), str(apk), '--abi', abi], check=True)
        verified.append((abi, apk))
    output.mkdir(parents=True, exist_ok=True)
    for abi, apk in verified:
        destination = output / ('Torto-{}-android-{}.apk'.format(version, abi))
        shutil.copyfile(apk, destination)
        digest = hashlib.sha256(destination.read_bytes()).hexdigest()
        destination.with_suffix('.apk.sha256').write_text('{}  {}\n'.format(digest, destination.name), encoding='utf-8')
        print('Verified {}'.format(destination.name))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apk-directory', type=Path, default=ROOT / 'apps/android/app/build/outputs/apk/release')
    parser.add_argument('--output', type=Path, default=ROOT / 'dist/android')
    parser.add_argument('--sdk', type=Path, default=os.environ.get('ANDROID_HOME') or os.environ.get('ANDROID_SDK_ROOT'))
    parser.add_argument('--abis', default=','.join(ABIS))
    parser.add_argument('--certificate-sha256', default=RELEASE_CERTIFICATE,
                        help='Expected signing certificate; production defaults to the existing release key')
    args = parser.parse_args()
    if args.sdk is None:
        parser.error('Set ANDROID_HOME or --sdk')
    package(args.apk_directory, args.output, args.sdk, workspace_version(), args.abis.split(','), args.certificate_sha256)


if __name__ == '__main__':
    main()
