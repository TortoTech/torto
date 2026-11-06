#!/usr/bin/env python3
"""Attach the signed Android split set to an existing published stable release."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

REPOSITORY = 'TortoTech/torto'


def publication_plan(release, tag, directory):
    if (not re.fullmatch(r'v\d+\.\d+\.\d+', tag) or release.get('tag_name') != tag or
            release.get('name') != tag or release.get('draft') is not False or
            release.get('prerelease') is not False):
        raise ValueError('Expected an existing published stable release named exactly after its tag')
    expected = ['Torto-{}-android-{}.apk'.format(tag[1:], abi) for abi in ('arm64-v8a', 'x86_64')]
    names = expected + [name + '.sha256' for name in expected]
    if {file.name for file in directory.iterdir()} != set(names):
        raise ValueError('Expected exactly two signed APKs and their checksums')
    for name in expected:
        digest = hashlib.sha256((directory / name).read_bytes()).hexdigest()
        if (directory / (name + '.sha256')).read_text() != '{}  {}\n'.format(digest, name):
            raise ValueError('APK checksum mismatch: ' + name)
    pending = []
    # Detect every conflict before uploading anything. Never silently replace
    # an installer that clients or the Gitee mirror may already have cached.
    for name in names:
        file = directory / name
        digest = 'sha256:' + hashlib.sha256(file.read_bytes()).hexdigest()
        matches = [asset for asset in release['assets'] if asset['name'] == name]
        if not matches:
            pending.append(file)
        elif (len(matches) != 1 or matches[0].get('digest') != digest or
              matches[0].get('size') != file.stat().st_size):
            raise ValueError('Existing release attachment differs; publish a new version: ' + name)
    # Checksums first; the Gitee mirror waits for the entire set in either case.
    return sorted(pending, key=lambda file: (not file.name.endswith('.sha256'), file.name))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('tag')
    parser.add_argument('--directory', type=Path, default=Path('dist/android'))
    args = parser.parse_args()
    if not re.fullmatch(r'v\d+\.\d+\.\d+', args.tag):
        parser.error('Expected a stable vMAJOR.MINOR.PATCH tag')
    release = json.loads(subprocess.check_output(['gh', 'api',
        'repos/{}/releases/tags/{}'.format(REPOSITORY, args.tag)], text=True))
    pending = publication_plan(release, args.tag, args.directory)
    for file in pending:
        subprocess.run(['gh', 'release', 'upload', args.tag, str(file), '--repo', REPOSITORY], check=True)
    print('Android release attachments verified; uploaded {} missing files'.format(len(pending)))


if __name__ == '__main__':
    main()
