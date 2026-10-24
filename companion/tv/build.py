"""Dependency-free, pinned-toolchain APK build; signing material stays in memory."""
import argparse
import base64
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parent
ANDROID = '{http://schemas.android.com/apk/res/android}'


def version_code(version):
    # Fixed-width components preserve semver ordering, including recovery builds.
    if not re.fullmatch(r'(0|[1-9][0-9]{0,2})\.(0|[1-9][0-9]{0,2})\.(0|[1-9][0-9]{0,2})', version):
        raise ValueError('Version must be MAJOR.MINOR.PATCH with components 0..999')
    major, minor, patch = map(int, version.split('.'))
    code = major * 1000000 + minor * 1000 + patch
    if code == 0:
        raise ValueError('Version code must be positive')
    return code


def properties(path):
    return dict(tuple(part.strip() for part in line.split('=', 1))
                for line in path.read_text().splitlines() if '=' in line and not line.startswith('#'))


def environment(settings=None):
    settings = settings or {}
    env = dict(os.environ)
    for key in ('JAVA_HOME', 'HOMEWIRE_ANDROID_PLATFORM', 'HOMEWIRE_ANDROID_BUILD_TOOLS'):
        if settings.get(key):
            env[key] = str(Path(settings[key]).expanduser().resolve())
    if not env.get('JAVA_HOME'):
        for path in ('/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home',
                     '/usr/lib/jvm/java-17-openjdk-amd64'):
            if Path(path).is_dir():
                env['JAVA_HOME'] = path
                break
    sdk = env.get('ANDROID_SDK_ROOT') or env.get('ANDROID_HOME')
    if sdk:
        env.setdefault('HOMEWIRE_ANDROID_PLATFORM', str(Path(sdk) / 'platforms/android-35'))
        env.setdefault('HOMEWIRE_ANDROID_BUILD_TOOLS', str(Path(sdk) / 'build-tools/35.0.0'))
    for key in ('JAVA_HOME', 'HOMEWIRE_ANDROID_PLATFORM', 'HOMEWIRE_ANDROID_BUILD_TOOLS'):
        if not env.get(key):
            raise ValueError('Set ' + key + ' in the environment or private release configuration')
    platform = Path(env['HOMEWIRE_ANDROID_PLATFORM'])
    tools = Path(env['HOMEWIRE_ANDROID_BUILD_TOOLS'])
    if properties(platform / 'source.properties').get('AndroidVersion.ApiLevel') != '35':
        raise ValueError('Expected Android platform 35')
    if properties(tools / 'source.properties').get('Pkg.Revision') != '35.0.0':
        raise ValueError('Expected Android build-tools 35.0.0')
    javac = Path(env['JAVA_HOME']) / 'bin/javac'
    result = subprocess.run([str(javac), '-version'], capture_output=True, text=True, check=True)
    if not re.match(r'javac 17[.\s]', result.stdout + result.stderr):
        raise ValueError('Expected JDK 17')
    for name in ('aapt2', 'd8', 'zipalign', 'apksigner'):
        if not os.access(tools / name, os.X_OK):
            raise ValueError('Missing Android build tool: ' + name)
    if not (platform / 'android.jar').is_file() or not shutil.which('zip'):
        raise ValueError('Missing android.jar or zip')
    env['PATH'] = str(Path(env['JAVA_HOME']) / 'bin') + os.pathsep + env.get('PATH', '')
    return env


def build(version=None, output=None, unsigned=False):
    env = environment()
    manifest = ET.parse(ROOT / 'AndroidManifest.xml').getroot()
    version = version or manifest.attrib[ANDROID + 'versionName']
    code = version_code(version)
    destination = Path(output or ROOT / 'build' / ('homewire-tv-unsigned.apk' if unsigned else 'homewire-tv.apk')).resolve()
    destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    if destination.exists():
        raise ValueError('Build output already exists; select a fresh --output path')
    key = None
    if not unsigned:
        key = base64.b64decode(env['HOMEWIRE_TV_SIGNING_KEY_B64'], validate=True)
        password = env['HOMEWIRE_TV_SIGNING_PASSWORD'].encode()
        if not 1000 <= len(key) <= 16384 or not 16 <= len(password) <= 256:
            raise ValueError('Invalid TV signing identity')
        env.update(HOMEWIRE_SIGNING_KEY='/dev/stdin',
                   HOMEWIRE_SIGNING_PASSWORD_SOURCE='env:HOMEWIRE_TV_SIGNING_PASSWORD')
    env.pop('HOMEWIRE_TV_SIGNING_KEY_B64', None)
    with tempfile.TemporaryDirectory(prefix='homewire-tv-') as directory:
        env.update(HOMEWIRE_TV_BUILD_DIR=directory, HOMEWIRE_TV_VERSION=version,
                   HOMEWIRE_TV_VERSION_CODE=str(code), HOMEWIRE_TV_UNSIGNED=str(int(unsigned)))
        subprocess.run(['sh', str(ROOT / 'build.sh')], env=env, input=key, check=True)
        name = 'homewire-tv-unsigned.apk' if unsigned else 'homewire-tv.apk'
        with destination.open('xb') as out:
            out.write((Path(directory) / name).read_bytes())
        destination.chmod(0o600)
    return destination


def apk_identity(path, tools, env=None):
    badging = subprocess.run([str(Path(tools) / 'aapt2'), 'dump', 'badging', str(path)],
                             check=True, capture_output=True, text=True, env=env).stdout
    match = re.search(r"package: name='([^']+)' versionCode='([0-9]+)' versionName='([^']+)'", badging)
    if not match or match[1] != 'eu.tsilva.homewire.tv':
        raise ValueError('Unexpected TV APK identity')
    certificates = subprocess.run([str(Path(tools) / 'apksigner'), 'verify', '--print-certs', str(path)],
                                  check=True, capture_output=True, text=True, env=env).stdout
    signers = re.findall(r'Signer #[0-9]+ certificate SHA-256 digest: ([a-f0-9]{64})', certificates)
    if len(signers) != 1:
        raise ValueError('Expected one verified APK signing identity')
    return {'version': match[3], 'version_code': int(match[2]), 'signer_sha256': signers[0]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--unsigned', action='store_true', help='CI only; cannot be installed')
    args = parser.parse_args()
    print(build(args.version, args.output, args.unsigned))


if __name__ == '__main__':
    main()
