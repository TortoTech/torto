#!/usr/bin/env python3
"""Check ELF alignment and, for mmap loading, APK ZIP offsets of all JNI libs."""
import argparse
import struct
import zipfile

p = argparse.ArgumentParser()
p.add_argument('apk')
p.add_argument('--abi', choices=('arm64-v8a', 'x86_64'))
args = p.parse_args()
with zipfile.ZipFile(args.apk) as apk, open(args.apk, 'rb') as raw:
    libraries = [i for i in apk.infolist() if i.filename.endswith('.so')]
    if not libraries:
        raise SystemExit('No native libraries found')
    abis = {i.filename.split('/')[1] for i in libraries if i.filename.startswith('lib/')}
    if len(abis) != 1 or not abis <= {'arm64-v8a', 'x86_64'}:
        raise SystemExit(f'Expected one supported ABI per APK, found {sorted(abis)}')
    abi = next(iter(abis))
    if args.abi and args.abi != abi:
        raise SystemExit(f'Expected {args.abi}, found {abi}')
    for info in libraries:
        data = apk.read(info)
        if data[:6] != b'\x7fELF\x02\x01':
            raise SystemExit(f'Unsupported ELF format: {info.filename}')
        machine = struct.unpack_from('<H', data, 18)[0]
        if machine != {'arm64-v8a': 183, 'x86_64': 62}[abi]:
            raise SystemExit(f'{info.filename}: ELF architecture does not match {abi}')
        offset = struct.unpack_from('<Q', data, 32)[0]
        size, count = struct.unpack_from('<HH', data, 54)
        loads = 0
        for index in range(count):
            header = offset + size * index
            if struct.unpack_from('<I', data, header)[0] == 1:
                loads += 1
                alignment = struct.unpack_from('<Q', data, header + 48)[0]
                if alignment < 16384:
                    raise SystemExit(f'{info.filename}: ELF LOAD alignment {alignment} < 16384')
        if not loads:
            raise SystemExit(f'{info.filename}: no ELF LOAD segments')
        raw.seek(info.header_offset + 26)
        name_len, extra_len = struct.unpack('<HH', raw.read(4))
        start = info.header_offset + 30 + name_len + extra_len
        if info.compress_type == zipfile.ZIP_DEFLATED:
            print(f'OK {info.filename}: ELF aligned for 16 KB pages; compressed, extracted at install')
        elif info.compress_type != zipfile.ZIP_STORED or start % 16384:
            raise SystemExit(f'{info.filename}: not stored uncompressed at a 16 KB ZIP boundary')
        else:
            print(f'OK {info.filename}: ELF and ZIP aligned for 16 KB pages')
