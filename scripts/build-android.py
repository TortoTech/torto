#!/usr/bin/env python3
"""Build matching native libraries and UniFFI bindings from this checkout."""
import argparse
import os
from pathlib import Path
import subprocess
import shutil

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser()
p.add_argument('--abis', default='arm64-v8a')
p.add_argument('--release', action='store_true')
args = p.parse_args()
abis = args.abis.split(',')
if not abis or len(set(abis)) != len(abis) or any(abi not in ('arm64-v8a', 'x86_64') for abi in abis):
    p.error('Unsupported ABI')
env = os.environ.copy()
if args.release:
    env['CARGO_PROFILE_RELEASE_DEBUG'] = '1'
    env['CARGO_PROFILE_RELEASE_STRIP'] = 'none'
sdk = env.get('ANDROID_HOME') or env.get('ANDROID_SDK_ROOT')
local = ROOT / 'apps/android/local.properties'
if not sdk and local.exists():
    sdk = next((line.partition('=')[2].strip() for line in local.read_text().splitlines() if line.startswith('sdk.dir=')), None)
if not sdk:
    p.error('Set ANDROID_HOME or apps/android/local.properties sdk.dir')
env['ANDROID_NDK_HOME'] = str(Path(sdk) / 'ndk/28.2.13676358')
env['ANDROID_HOME'] = sdk
out = ROOT / 'apps/android/app/build/generated/rust' / ('release' if args.release else 'debug')
# Remove obsolete ABI/binding outputs; Gradle treats this directory as one unit.
if out.exists():
    shutil.rmtree(out)
out.mkdir(parents=True)
# Android 16 KB ELF alignment applies to Rust libraries as well as C/C++.
for triple in ('AARCH64_LINUX_ANDROID', 'X86_64_LINUX_ANDROID', 'ARMV7_LINUX_ANDROIDEABI'):
    # APS2 packed relocations work from API 23 (our minimum is 26). RELR needs
    # newer Android and must not be substituted here. Safe ICF preserves
    # address-significant functions while folding equivalent linker sections.
    env[f'CARGO_TARGET_{triple}_RUSTFLAGS'] = (
        '-C link-arg=-Wl,-z,max-page-size=16384 '
        '-C link-arg=-Wl,--pack-dyn-relocs=android '
        '-C link-arg=-Wl,--icf=safe'
    )
def run(*argv):
    subprocess.run(argv, cwd=ROOT, env=env, check=True)
cmd = ['cargo', 'ndk', '--platform', '26', '-o', str(out / 'jniLibs')]
for abi in abis:
    cmd.extend(['-t', abi])
cmd += ['build', '--locked', '-p', 'torto-android-bridge', '--lib']
if args.release:
    cmd.append('--release')
run(*cmd)
# Use the host binary only as the generator. The metadata is read from the exact
# Android library shipped in this build, so stale binding files cannot survive.
run('cargo', 'run', '--locked', '-p', 'torto-android-bridge', '--features', 'bindgen', '--bin', 'torto-bindgen', '--',
    'generate', '--library', str(out / 'jniLibs' / abis[0] / 'libtorto_android.so'),
    '--language', 'kotlin', '--no-format', '--out-dir', str(out / 'kotlin'))

if args.release:
    toolchains = Path(env['ANDROID_NDK_HOME']) / 'toolchains/llvm/prebuilt'
    host = next(path for path in toolchains.iterdir() if path.is_dir())
    for abi in abis:
        library = out / 'jniLibs' / abi / 'libtorto_android.so'
        symbols = out / 'symbols' / abi / 'libtorto_android.so.dbg'
        symbols.parent.mkdir(parents=True, exist_ok=True)
        run(str(host / 'bin/llvm-objcopy'), '--only-keep-debug', str(library), str(symbols))
        run(str(host / 'bin/llvm-strip'), '--strip-unneeded', str(library))
