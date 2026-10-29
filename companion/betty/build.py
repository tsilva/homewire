#!/usr/bin/env python3
"""Build the native Betty APK using pinned SDK tools and hash-verified TDLib inputs."""
import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tarfile
import tempfile
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[1]
LOCK = json.loads((ROOT / 'dependencies.lock.json').read_text())


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def check_native(path):
    data = path.read_bytes()
    if data[:6] != b'\x7fELF\x02\x01':
        raise ValueError('Expected a little-endian 64-bit Android library')
    offset = struct.unpack_from('<Q', data, 32)[0]
    size, count = struct.unpack_from('<HH', data, 54)
    alignments = [struct.unpack_from('<IIQQQQQQ', data, offset + index * size)[-1]
                  for index in range(count)
                  if struct.unpack_from('<I', data, offset + index * size)[0] == 1]
    if not alignments or min(alignments) < 16384:
        raise ValueError('Native library does not support 16 KiB page alignment')


def dependencies(cache):
    """The lock authenticates content; LFS pointer files are never packaged."""
    def fetch(entry):
        path = entry['path']
        destination = cache / path
        if destination.is_file() and sha256(destination.read_bytes()) == entry['sha256']:
            return
        base = ('https://media.githubusercontent.com/media/' if path.endswith('.so')
                else 'https://raw.githubusercontent.com/')
        url = base + LOCK['repository'] + '/' + LOCK['commit'] + '/' + path
        data = urllib.request.urlopen(url, timeout=180).read()
        if sha256(data) != entry['sha256'] or len(data) != entry['size']:
            raise ValueError('TDLib dependency checksum mismatch: ' + path)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
    with ThreadPoolExecutor(max_workers=4) as executor:
        list(executor.map(fetch, LOCK['files']))
    annotation = cache / 'annotation-jvm-1.9.1.jar'
    entry = LOCK['annotation']
    if not annotation.exists() or sha256(annotation.read_bytes()) != entry['sha256']:
        data = urllib.request.urlopen(entry['url'], timeout=60).read()
        if sha256(data) != entry['sha256']:
            raise ValueError('Annotation dependency checksum mismatch')
        annotation.write_bytes(data)
    ndk = LOCK['ndk']
    if any(not (cache / item['path']).exists() or sha256((cache / item['path']).read_bytes()) != item['sha256']
           for item in ndk['files']):
        archive = cache / ndk['url'].rsplit('/', 1)[1]
        if not archive.exists():
            urllib.request.urlretrieve(ndk['url'], archive)
        digest = hashlib.sha1()
        with archive.open('rb') as stream:
            for block in iter(lambda: stream.read(1048576), b''):
                digest.update(block)
        if digest.hexdigest() != ndk['sha1']:
            raise ValueError('NDK archive digest mismatch')
        with zipfile.ZipFile(archive) as source:
            for item in ndk['files']:
                data = source.read(item['member'])
                if sha256(data) != item['sha256'] or len(data) != item['size']:
                    raise ValueError('NDK runtime checksum mismatch')
                destination = cache / item['path']
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(data)
    return annotation


def run(command, **kwargs):
    subprocess.run(list(map(str, command)), check=True, **kwargs)


def build(args):
    settings = {}
    if args.signing_config:
        path = args.signing_config.resolve()
        if path.stat().st_mode & 0o077 or path.stat().st_uid != os.getuid():
            raise ValueError('Private build configuration must be owner-only')
        settings = json.loads(path.read_text())
        if not args.injected and not args.unsigned:
            binding = settings['signing']
            spec = importlib.util.spec_from_file_location('tv_release', ROOT.parent / 'tv/release.py')
            # Import the maintained binding validator without importing its local modules globally.
            import sys
            sys.path.insert(0, str(ROOT.parent / 'tv'))
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            module.binding(binding)
            command = ['infisical', 'run', '--projectId=' + binding['project_id'],
                       '--env=' + binding['environment'], '--path=' + binding['folder'],
                       '--domain=' + binding['domain'], '--expand=false', '--include-imports=false',
                       '--silent', '--telemetry=false', '--', sys.executable, str(ROOT / 'build.py'),
                       '--injected', '--signing-config', str(path), '--output', str(args.output.resolve())]
            if args.preview:
                command.append('--preview')
            if args.checks:
                command.append('--checks')
            # Provider diagnostics may contain credentials. Surface a safe failure only.
            result = subprocess.run(command, capture_output=True)
            if result.returncode:
                output = result.stdout.decode(errors='replace') + result.stderr.decode(errors='replace')
                # Compiler errors are safe; authentication/provider output is not echoed.
                if 'error:' in output and '.java:' in output:
                    for line in output.splitlines():
                        if '.java:' in line and 'error:' in line:
                            print(line)
                    raise RuntimeError('Java compilation failed; inspect source diagnostics above')
                raise RuntimeError('Credential injection or signed build failed. If Infisical authentication expired, renew your login. Provider output was withheld.')
            print(args.output.resolve())
            return
    spec = importlib.util.spec_from_file_location('sdk_build', ROOT.parent / 'tv/build.py')
    sdk = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(sdk)
    env = sdk.environment(settings.get('toolchain'))
    platform = Path(env['HOMEWIRE_ANDROID_PLATFORM'])
    tools = Path(env['HOMEWIRE_ANDROID_BUILD_TOOLS'])
    javac = Path(env['JAVA_HOME']) / 'bin/javac'
    output = args.output.resolve()
    if output.exists():
        raise ValueError('Output already exists; choose a fresh versioned output path')
    cache = REPO / '.local/betty-deps'
    cache.mkdir(mode=0o700, parents=True, exist_ok=True)
    annotation = dependencies(cache)
    with tempfile.TemporaryDirectory(prefix='betty-build-') as directory:
        work = Path(directory)
        for folder in ('classes', 'dex', 'generated'):
            (work / folder).mkdir()
        run([tools / 'aapt2', 'compile', '--dir', ROOT / 'res', '-o', work / 'resources.zip'], env=env)
        manifest = ET.parse(ROOT / ('tests/AndroidManifest.xml' if args.checks else 'AndroidManifest.xml'))
        android = '{http://schemas.android.com/apk/res/android}'
        package = 'eu.tsilva.homewire.betty.tests' if args.checks else 'eu.tsilva.homewire.betty.preview' if args.preview else 'eu.tsilva.homewire.betty'
        manifest.getroot().set('package', package)
        if args.preview:
            manifest.getroot().find('application').set(android + 'label', 'Betty design preview')
            for element in list(manifest.getroot()):
                if element.tag == 'uses-permission' and element.get(android + 'name') == 'android.permission.INTERNET':
                    manifest.getroot().remove(element)
        manifest.write(work / 'AndroidManifest.xml', encoding='utf-8', xml_declaration=True)
        run([tools / 'aapt2', 'link', '-I', platform / 'android.jar', '--manifest', work / 'AndroidManifest.xml',
             '--java', work / 'generated', '--custom-package', 'eu.tsilva.homewire.betty',
             '-o', work / 'unsigned.apk', work / 'resources.zip'], env=env)
        flags = work / 'generated/eu/tsilva/homewire/betty/BuildFlags.java'
        flags.parent.mkdir(parents=True, exist_ok=True)
        flags.write_text('package eu.tsilva.homewire.betty; final class BuildFlags { static final boolean PREVIEW = '
                         + str(args.preview).lower() + '; }\n')
        sources = ([ROOT / 'tests/Checks.java'] if args.checks else sorted((ROOT / 'src').rglob('*.java')))
        sources += sorted((work / 'generated').rglob('*.java'))
        if not args.checks:
            sources += sorted((cache / 'src/main/java').rglob('*.java'))
        source_list = work / 'sources'
        source_list.write_text('\n'.join('"' + str(source) + '"' for source in sources))
        run([javac, '--release', '8', '-nowarn', '-classpath', str(platform / 'android.jar') + os.pathsep + str(annotation),
             '-d', work / 'classes', '@' + str(source_list)], env=env)
        run([Path(env['JAVA_HOME']) / 'bin/jar', 'cf', work / 'classes.jar', '-C', work / 'classes', '.'], env=env)
        run([tools / 'd8', '--min-api', '31', '--lib', platform / 'android.jar', '--classpath', annotation,
             '--output', work / 'dex', work / 'classes.jar'], env=env)
        with zipfile.ZipFile(work / 'unsigned.apk', 'a') as archive:
            for dex in sorted((work / 'dex').glob('*.dex')):
                archive.write(dex, dex.name, compress_type=zipfile.ZIP_DEFLATED)
            for entry in LOCK['files']:
                if entry['path'].endswith('.so') and not args.checks:
                    source = cache / entry['path']
                    check_native(source)
                    abi = next(part for part in source.parts if part in ('arm64-v8a', 'x86_64'))
                    archive.write(source, 'lib/' + abi + '/' + source.name, compress_type=zipfile.ZIP_STORED)
            if not args.checks:
                for entry in LOCK['ndk']['files']:
                    source = cache / entry['path']
                    if source.suffix == '.so':
                        check_native(source)
                        archive.write(source, 'lib/' + source.parent.name + '/' + source.name, compress_type=zipfile.ZIP_STORED)
                    else:
                        archive.write(source, 'assets/' + source.name, compress_type=zipfile.ZIP_DEFLATED)
            archive.writestr('assets/TDLIB-LICENSE.txt', (cache / 'LICENCE').read_bytes())
            archive.writestr('assets/dependencies.lock.json', (ROOT / 'dependencies.lock.json').read_bytes())
            for notice in ('NOTICE.md', 'LICENSE-APACHE-2.0.txt'):
                archive.write(ROOT / notice, 'assets/' + notice, compress_type=zipfile.ZIP_DEFLATED)
        run([tools / 'zipalign', '-P', '16', '-f', '4', work / 'unsigned.apk', work / 'aligned.apk'], env=env)
        output.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        if args.unsigned:
            shutil.copyfile(work / 'aligned.apk', output)
        else:
            key_name = 'HOMEWIRE_TV_SIGNING_KEY_B64'
            password_name = 'HOMEWIRE_TV_SIGNING_PASSWORD'
            key = base64.b64decode(env.pop(key_name), validate=True)
            if not 1000 <= len(key) <= 16384 or len(env.get(password_name, '')) < 16:
                raise ValueError('Invalid persistent signing identity')
            run([tools / 'apksigner', 'sign', '--ks', '/dev/stdin', '--ks-type', 'PKCS12',
                 '--ks-pass', 'env:' + password_name, '--out', output, work / 'aligned.apk'], input=key, env=env)
            run([tools / 'apksigner', 'verify', output], env=env)
        run([tools / 'zipalign', '-c', '-P', '16', '-v', '4', output], stdout=subprocess.DEVNULL, env=env)
        output.chmod(0o600)
    source_archive = output.with_suffix('.source.tar.gz')
    with tarfile.open(source_archive, 'w:gz') as source:
        for path in sorted(ROOT.rglob('*')):
            if path.is_file() and not any(part in ('build', '__pycache__') for part in path.relative_to(ROOT).parts):
                source.add(path, arcname='betty/' + str(path.relative_to(ROOT)), recursive=False)
    source_archive.chmod(0o600)
    receipt = {'package': package, 'version': '0.1.4', 'version_code': 1004,
               'preview': args.preview, 'signed': not args.unsigned, 'sha256': sha256(output.read_bytes()),
               'tdlib_commit': LOCK['commit'], 'abis': ['arm64-v8a', 'x86_64'],
               'source_archive_sha256': sha256(source_archive.read_bytes())}
    output.with_suffix('.receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    output.with_suffix('.receipt.json').chmod(0o600)
    print(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--signing-config', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--unsigned', action='store_true', help='Build validation only; cannot install')
    parser.add_argument('--preview', action='store_true', help='Separate offline package with synthetic chat for visual QA')
    parser.add_argument('--checks', action='store_true', help='Build the separate offline instrumentation APK')
    parser.add_argument('--injected', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.preview and args.checks:
        parser.error('--preview and --checks are mutually exclusive')
    try:
        build(args)
    except (ValueError, RuntimeError, KeyError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from None


if __name__ == '__main__':
    main()
