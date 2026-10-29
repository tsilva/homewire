#!/usr/bin/env python3
"""Verify a Betty production artifact or perform one state-preserving USB update."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import tempfile
import xml.etree.ElementTree as ET

REPO = Path(__file__).resolve().parents[4]
PACKAGE = 'eu.tsilva.homewire.betty'
ANDROID = '{http://schemas.android.com/apk/res/android}'


class CheckError(RuntimeError):
    """A safe, stage-specific diagnostic without provider/device output."""


def require(condition, message):
    if not condition:
        raise CheckError(message)


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1048576), b''):
            value.update(block)
    return value.hexdigest()


def private_json(path):
    require(path.is_file() and not path.is_symlink(), 'Private binding must be a regular file')
    stat = path.stat()
    require(stat.st_uid == os.getuid() and not stat.st_mode & 0o077,
            'Private binding must be owned by this user and owner-only')
    value = json.loads(path.read_text())
    require(isinstance(value, dict), 'Private binding must be a JSON object')
    return value


def run(command, label, env=None, timeout=30):
    try:
        result = subprocess.run(list(map(str, command)), capture_output=True, text=True,
                                env=env, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        raise CheckError(label + ' failed or timed out; no automatic retry was made') from None
    require(result.returncode == 0, label + ' failed; diagnostic output withheld')
    return result.stdout


def tools(config):
    binding = private_json(config).get('toolchain', {})
    require(isinstance(binding, dict), 'Toolchain binding must be a JSON object')
    require(binding.get('JAVA_HOME') and binding.get('HOMEWIRE_ANDROID_BUILD_TOOLS'),
            'Missing toolchain binding')
    env = os.environ.copy()
    env['JAVA_HOME'] = str(Path(binding['JAVA_HOME']).expanduser().resolve())
    folder = Path(binding['HOMEWIRE_ANDROID_BUILD_TOOLS']).expanduser().resolve()
    require(all(os.access(folder / tool, os.X_OK) for tool in ('aapt2', 'apksigner', 'zipalign')),
            'Configured Android verification tools are unavailable')
    return folder, env


def identity(config):
    binding = private_json(config)
    require(binding.get('schema') == 1, 'Unsupported device binding schema')
    require(isinstance(binding.get('serial'), str) and binding['serial'].strip(),
            'An explicit device serial is required')
    require(isinstance(binding.get('signer_sha256'), str)
            and re.fullmatch(r'[a-f0-9]{64}', binding['signer_sha256']),
            'A pinned signing certificate SHA-256 is required')
    return binding


def signer(apk, sdk):
    folder, env = sdk
    output = run([folder / 'apksigner', 'verify', '--print-certs', apk],
                 'APK signature verification', env)
    values = re.findall(r'Signer #1 certificate SHA-256 digest: ([a-f0-9]{64})', output)
    require(len(values) == 1, 'Expected one identifiable APK signer')
    require(not re.search(r'Signer #2 certificate SHA-256 digest:', output),
            'Multiple APK signers require explicit review')
    return values[0]


def package_info(apk, sdk):
    folder, env = sdk
    output = run([folder / 'aapt2', 'dump', 'badging', apk], 'APK package inspection', env)
    match = re.search(r"^package: name='([^']+)' versionCode='([0-9]+)' versionName='([^']*)'", output, re.M)
    require(match is not None, 'APK package/version metadata is unavailable')
    return match.group(1), int(match.group(2)), match.group(3)


def verify_artifact(apk, sdk, binding, source_root=None):
    receipt = json.loads(apk.with_suffix('.receipt.json').read_text())
    require(isinstance(receipt, dict), 'APK receipt must be a JSON object')
    require(receipt.get('package') == PACKAGE and receipt.get('signed') is True
            and receipt.get('preview') is False, 'Only signed Betty production artifacts are accepted')
    require(type(receipt.get('version_code')) is int and receipt['version_code'] > 0
            and isinstance(receipt.get('version'), str) and receipt['version'],
            'Receipt version must be a positive code and a nonempty name')
    require(digest(apk) == receipt.get('sha256'), 'APK receipt hash mismatch')
    source = apk.with_suffix('.source.tar.gz')
    require(digest(source) == receipt.get('source_archive_sha256'), 'Source archive receipt hash mismatch')
    require(signer(apk, sdk) == binding['signer_sha256'], 'APK signing identity does not match the pinned identity')
    expected = (PACKAGE, receipt.get('version_code'), receipt.get('version'))
    require(package_info(apk, sdk) == expected, 'APK package/version differs from its receipt')
    folder, env = sdk
    run([folder / 'zipalign', '-c', '-P', '16', '4', apk], '16 KiB APK alignment verification', env)
    names = set()
    manifest = None
    with tarfile.open(source) as archive:
        for member in archive.getmembers():
            path = PurePosixPath(member.name)
            require(member.isfile() and path.parts and path.parts[0] == 'betty'
                    and not path.is_absolute() and '..' not in path.parts,
                    'Unexpected entry in frozen source archive')
            require(member.name not in names, 'Duplicate frozen source entry')
            names.add(member.name)
            data = archive.extractfile(member).read()
            if member.name == 'betty/AndroidManifest.xml':
                manifest = ET.fromstring(data)
            if source_root:
                current = source_root.joinpath(*path.parts[1:])
                require(current.is_file() and not current.is_symlink() and current.read_bytes() == data,
                        'Current source differs from the frozen archive')
    require(manifest is not None, 'Frozen source manifest is missing')
    require((manifest.get('package'), int(manifest.get(ANDROID + 'versionCode', '0')),
             manifest.get(ANDROID + 'versionName')) == expected,
            'Frozen source version differs from the APK')
    if source_root:
        current_names = {'betty/' + p.relative_to(source_root).as_posix()
                         for p in source_root.rglob('*') if p.is_file()
                         and not any(x in ('build', '__pycache__') for x in p.relative_to(source_root).parts)}
        require(current_names == names, 'Current source file set differs from the frozen archive')
    return receipt


class Adb:
    def __init__(self, serial):
        self.serial = serial
        self.binary = shutil.which('adb')
        require(self.binary is not None, 'ADB is unavailable')

    def command(self, *args, timeout=30):
        return run([self.binary, '-s', self.serial, *args], 'Targeted ADB command', timeout=timeout)

    def ready(self, wait_seconds):
        if wait_seconds:
            self.command('wait-for-device', timeout=wait_seconds)
        require(self.command('get-state').strip() == 'device', 'Bound device is not authorized and connected')
        require(self.command('get-devpath').strip().startswith('usb:'), 'The bound target must be connected over USB')

    def version(self):
        text = self.command('shell', 'dumpsys', 'package', PACKAGE)
        match = re.search(r'versionCode=(\d+)', text)
        require(match is not None, 'Installed Betty version is unavailable; this is an update-only helper')
        return int(match.group(1))

    def settings(self):
        return {'assistant': self.command('shell', 'cmd', 'role', 'get-role-holders', '--user', '0',
                                          'android.app.role.ASSISTANT').strip(),
                'power_long_press': self.command('shell', 'settings', 'get', 'global', 'power_button_long_press').strip(),
                'usb_debugging': self.command('shell', 'settings', 'get', 'global', 'adb_enabled').strip()}

    def pull_apk(self, destination):
        paths = [line[8:] for line in self.command('shell', 'pm', 'path', PACKAGE).splitlines()
                 if line.startswith('package:') and line.endswith('/base.apk')]
        require(len(paths) == 1, 'Expected one installed Betty base APK')
        fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(fd)
        self.command('pull', paths[0], str(destination), timeout=60)


def save(path, record):
    with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        try:
            json.dump(record, stream, indent=2)
            stream.write('\n')
            stream.flush()
            os.fsync(stream.fileno())
        except BaseException:
            temporary.unlink(missing_ok=True)
            raise
    os.replace(temporary, path)
    path.chmod(0o600)


def install(apk, receipt, sdk, binding, wait_seconds=0, launch=False):
    adb = Adb(binding['serial'])
    adb.ready(wait_seconds)
    before_version = adb.version()
    require(receipt['version_code'] > before_version, 'Candidate version must be newer than the installed version')
    before_settings = adb.settings()
    # Reserving an exclusive directory stops a second attempt using the same candidate.
    evidence = apk.parent / 'installation'
    require(not evidence.exists(), 'Installation evidence already exists; inspect it before any recovery attempt')
    evidence.mkdir(mode=0o700)
    record = {'phase': 'preparing', 'package': PACKAGE, 'version_before': before_version,
              'version_target': receipt['version_code'], 'apk_sha256': receipt['sha256'],
              'settings_before': before_settings, 'install_started': False}
    path = evidence / 'installation.json'
    save(path, record)
    try:
        backup = evidence / 'previous.apk'
        adb.pull_apk(backup)
        require(signer(backup, sdk) == binding['signer_sha256'], 'Installed signing identity differs from the pinned identity')
        old_package, old_version, _ = package_info(backup, sdk)
        require(old_package == PACKAGE and old_version == before_version, 'Backup package/version verification failed')
        record['previous_apk_sha256'] = digest(backup)
        record.update(phase='installation_requested', install_started=True)
        save(path, record)
        output = adb.command('install', '-r', str(apk), timeout=120)
        require(any(line.strip() == 'Success' for line in output.splitlines()), 'Installation result is uncertain; do not repeat it')
        record['phase'] = 'installed'
        save(path, record)
        require(adb.version() == receipt['version_code'], 'Installed version did not match the candidate')
        installed = evidence / 'installed.apk'
        adb.pull_apk(installed)
        require(digest(installed) == receipt['sha256'], 'Installed APK hash does not match the candidate')
        after_settings = adb.settings()
        require(after_settings == before_settings, 'Assistant/power/debugging settings changed during the update')
        record.update(phase='verified', installed_hash_verified=True, settings_after=after_settings)
        save(path, record)
        if launch:
            adb.command('shell', 'am', 'start', '-n', PACKAGE + '/' + PACKAGE + '.MainActivity')
            record['launched'] = True
            save(path, record)
    except Exception as error:
        record['failure_type'] = type(error).__name__
        save(path, record)
        raise CheckError('Update stopped. Inspect installation/installation.json; no installation was replayed') from None
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('verify', 'install'))
    parser.add_argument('apk', type=Path)
    parser.add_argument('--toolchain-config', type=Path, default=REPO / '.local/tv-release.json')
    parser.add_argument('--device-config', type=Path, default=REPO / '.local/betty-device.json')
    parser.add_argument('--source-root', type=Path)
    parser.add_argument('--wait-seconds', type=int, default=0, choices=range(46), metavar='0..45')
    parser.add_argument('--launch', action='store_true', help='Open Betty after verification; recording can start')
    args = parser.parse_args()
    try:
        require(args.mode == 'install' or (not args.wait_seconds and not args.launch),
                'Wait/launch options apply only to an authorized installation')
        apk = args.apk.resolve()
        binding = identity(args.device_config)
        sdk = tools(args.toolchain_config)
        receipt = verify_artifact(apk, sdk, binding, args.source_root)
        if args.mode == 'install':
            result = install(apk, receipt, sdk, binding, args.wait_seconds, args.launch)
            print(json.dumps({'phase': result['phase'], 'version': receipt['version'],
                              'installed_hash_verified': result['installed_hash_verified']}))
        else:
            print(json.dumps({'phase': 'artifact_verified', 'version': receipt['version'],
                              'sha256': receipt['sha256'], 'signer_verified': True}))
    except CheckError as error:
        raise SystemExit(str(error)) from None
    except (ValueError, OSError, KeyError, tarfile.TarError, ET.ParseError):
        raise SystemExit('Verification/update stopped. Inspect bindings, artifact hashes and any private installation receipt; no automatic retry was made.') from None


if __name__ == '__main__':
    main()
