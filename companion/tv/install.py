"""Hash-verified TV installation and initial pairing through ADB, without secret argv."""
import argparse
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import subprocess
import time
import tempfile

PACKAGE = 'eu.tsilva.homewire.tv'


def adb(serial, *args, data=None, timeout=30, allow_missing=False):
    result = subprocess.run(['adb', '-s', serial, *args], input=data, capture_output=True, timeout=timeout)
    if result.returncode:
        if allow_missing and result.returncode == 1 and not result.stdout.strip() and not result.stderr.strip():
            return ''
        # Provider output can contain private inputs. Do not print it.
        raise RuntimeError('TV ADB operation failed; no automatic retry')
    return result.stdout.decode(errors='replace').strip()


def profile(path):
    path = Path(path)
    if path.stat().st_mode & 0o077:
        raise ValueError('TV pairing profile requires chmod 600')
    raw = path.read_bytes()
    if len(raw) > 4096:
        raise ValueError('Oversized TV pairing profile')
    value = json.loads(raw)
    if set(value) != {'host', 'port', 'model', 'device', 'fingerprint', 'incremental', 'framework_sha256'}:
        raise ValueError('Invalid pairing profile fields')
    ipaddress.ip_address(value['host'])
    if type(value['port']) is not int or not 1024 <= value['port'] <= 65535:
        raise ValueError('Invalid TV API port')
    for name in ('model', 'device', 'fingerprint', 'incremental'):
        if not isinstance(value[name], str) or not 1 <= len(value[name]) <= 256 or any(ord(c) < 32 for c in value[name]):
            raise ValueError('Invalid TV pairing identity')
    if not re.fullmatch(r'[a-f0-9]{64}', value['framework_sha256']):
        raise ValueError('Invalid vendor framework hash')
    return value


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--serial', required=True)
    parser.add_argument('--apk', type=Path, required=True)
    parser.add_argument('--sha256', required=True)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--backup-directory', type=Path)
    parser.add_argument('--update-only', action='store_true', help='Require existing pairing and verify before updating')
    parser.add_argument('--expected-version')
    parser.add_argument('--expected-version-code', type=int)
    parser.add_argument('--verify-connect', nargs=2, metavar=('IP', 'PORT'), help='Private operator tunnel for HTTPS checks')
    parser.add_argument('--receipt', type=Path)
    args = parser.parse_args(argv)
    value = profile(args.profile)
    if hashlib.sha256(args.apk.read_bytes()).hexdigest() != args.sha256:
        raise ValueError('TV APK hash mismatch')
    token = os.environ.get('HOMEWIRE_SECRET_TV_API_TOKEN', '')
    if not re.fullmatch(r'[a-f0-9]{64}', token):
        raise ValueError('Inject HOMEWIRE_SECRET_TV_API_TOKEN from Infisical')
    from verify import verify
    connect = None
    if args.verify_connect:
        ipaddress.ip_address(args.verify_connect[0])
        connect = (args.verify_connect[0], int(args.verify_connect[1]))
        if not 1 <= connect[1] <= 65535:
            raise ValueError('Invalid HTTPS tunnel port')
    old = None
    if args.output.exists():
        if args.output.stat().st_mode & 0o077:
            raise ValueError('TV companion configuration requires chmod 600')
        old = json.loads(args.output.read_text())
        if any(old.get(k) != value[k] for k in ('fingerprint', 'incremental', 'port')):
            raise ValueError('Existing pairing differs from audited profile')
    if args.update_only and old is None:
        raise ValueError('Update requires the existing pinned companion configuration')
    props = adb(args.serial, 'shell', 'getprop ro.product.manufacturer; getprop ro.product.model; '
                'getprop ro.product.device; getprop ro.build.fingerprint; getprop ro.build.version.incremental').splitlines()
    if props != ['TCL', value['model'], value['device'], value['fingerprint'], value['incremental']]:
        raise ValueError('Live TV identity differs from pairing profile')
    digest = adb(args.serial, 'shell', 'sha256sum', '/system_ext/framework/com.tcl.tv.display.jar').split()[0]
    if digest != value['framework_sha256']:
        raise ValueError('TV vendor framework differs from audited profile')
    if args.update_only:
        verify(value, old, connect)
    previous = adb(args.serial, 'shell', 'pm', 'path', PACKAGE, allow_missing=True)
    backup = None
    if args.update_only and not previous:
        raise ValueError('Update requires an installed companion')
    if previous:
        path = previous.removeprefix('package:')
        if not path.startswith('/data/app/') or '\n' in path or args.backup_directory is None:
            raise ValueError('Existing TV companion requires a private backup directory')
        args.backup_directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        digest = adb(args.serial, 'shell', 'sha256sum', path).split()[0]
        backup = args.backup_directory / (digest + '.apk')
        if not backup.exists():
            adb(args.serial, 'pull', path, str(backup))
            backup.chmod(0o600)
        if hashlib.sha256(backup.read_bytes()).hexdigest() != digest:
            raise ValueError('Previous APK backup hash mismatch')
    if args.update_only:
        from build import apk_identity
        tools = os.environ['HOMEWIRE_ANDROID_BUILD_TOOLS']
        candidate = apk_identity(args.apk, tools)
        installed_identity = apk_identity(backup, tools)
        if candidate['signer_sha256'] != installed_identity['signer_sha256']:
            raise ValueError('APK signing identity differs from installed companion')
        if candidate['version_code'] <= installed_identity['version_code']:
            raise ValueError('Update requires a strictly higher version code')
        if candidate['version'] != args.expected_version or candidate['version_code'] != args.expected_version_code:
            raise ValueError('Candidate version differs from requested release')
    adb(args.serial, 'install', '--no-streaming', '-r', str(args.apk), timeout=45)
    installed = adb(args.serial, 'shell', 'pm', 'path', PACKAGE).removeprefix('package:')
    if adb(args.serial, 'shell', 'sha256sum', installed).split()[0] != args.sha256:
        raise ValueError('Installed TV APK hash mismatch')
    if args.expected_version is not None:
        dump = adb(args.serial, 'shell', 'dumpsys', 'package', PACKAGE)
        if not re.search(r'versionName=' + re.escape(args.expected_version) + r'(?:\s|$)', dump) or not re.search(
                r'versionCode=' + str(args.expected_version_code) + r'(?:\s|$)', dump):
            raise ValueError('Installed TV version differs from requested release')
    # TCL blocks cold provider launches. An explicit activity launch permits pairing.
    adb(args.serial, 'shell', 'am', 'start', '-W', '-n', PACKAGE + '/.MainActivity')
    uri = 'content://' + PACKAGE + '.pairing/config'
    # Initial provisioning requires the caller's existing shell DUMP permission.
    state = adb(args.serial, 'shell', 'content', 'query', '--uri', uri)
    if 'paired=0' in state:
        pairing = {k: value[k] for k in ('port', 'model', 'device', 'fingerprint', 'incremental')}
        pairing['token'] = token
        adb(args.serial, 'shell', '-T', 'content', 'write', '--uri', uri,
            data=json.dumps(pairing).encode())
        for _ in range(30):
            state = adb(args.serial, 'shell', 'content', 'query', '--uri', uri)
            if 'paired=1' in state:
                break
            time.sleep(0.2)
    match = re.search(r'paired=1, port=(\d+), certificate_sha256=([a-f0-9]{64})', state)
    if not match or int(match.group(1)) != value['port']:
        raise ValueError('TV pairing or certificate initialization failed')
    config = {'port': value['port'], 'server_cert_sha256': match.group(2),
              'token': '${HOMEWIRE_SECRET_TV_API_TOKEN}', 'fingerprint': value['fingerprint'],
              'incremental': value['incremental'], 'timeout_seconds': 8}
    if old is not None:
        if any(old.get(k) != config[k] for k in ('server_cert_sha256', 'fingerprint', 'incremental')):
            raise ValueError('Paired TV identity changed; output was not replaced')
    # This audited TCL firmware blocks background startup by default.
    adb(args.serial, 'shell', 'cmd', 'appops', 'set', PACKAGE, 'APP_AUTO_START', 'allow')
    startup = adb(args.serial, 'shell', 'cmd', 'appops', 'get', PACKAGE, 'APP_AUTO_START')
    if not startup.startswith('APP_AUTO_START: allow'):
        raise ValueError('TCL did not allow companion auto-start')
    # Recreate the initial unpaired activity so it loads the newly saved config.
    adb(args.serial, 'shell', 'am', 'start', '-S', '-W', '-n', PACKAGE + '/.MainActivity')
    checks = verify(value, config, connect)
    save_json(args.output, config)
    receipt = {'installed_sha256': args.sha256, 'previous_apk': str(backup) if backup else None,
               'version': args.expected_version, 'version_code': args.expected_version_code,
               'autostart_permission': True, 'reboot_verified': False, 'checks': checks}
    if args.receipt:
        save_json(args.receipt, receipt)
    print('TV companion installed; APK hash, pairing pin, HTTPS authentication and native status verified.')
    return receipt


def save_json(path, value):
    path = Path(path)
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=path.name + '.', dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as out:
            json.dump(value, out, indent=2)
            out.write('\n')
            out.flush()
            os.fsync(out.fileno())
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


if __name__ == '__main__':
    main()
