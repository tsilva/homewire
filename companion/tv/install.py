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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--serial', required=True)
    parser.add_argument('--apk', type=Path, required=True)
    parser.add_argument('--sha256', required=True)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--backup-directory', type=Path)
    args = parser.parse_args()
    value = profile(args.profile)
    if hashlib.sha256(args.apk.read_bytes()).hexdigest() != args.sha256:
        raise ValueError('TV APK hash mismatch')
    token = os.environ.get('HOMEWIRE_SECRET_TV_API_TOKEN', '')
    if not re.fullmatch(r'[a-f0-9]{64}', token):
        raise ValueError('Inject HOMEWIRE_SECRET_TV_API_TOKEN from Infisical')
    props = adb(args.serial, 'shell', 'getprop ro.product.manufacturer; getprop ro.product.model; '
                'getprop ro.product.device; getprop ro.build.fingerprint; getprop ro.build.version.incremental').splitlines()
    if props != ['TCL', value['model'], value['device'], value['fingerprint'], value['incremental']]:
        raise ValueError('Live TV identity differs from pairing profile')
    digest = adb(args.serial, 'shell', 'sha256sum', '/system_ext/framework/com.tcl.tv.display.jar').split()[0]
    if digest != value['framework_sha256']:
        raise ValueError('TV vendor framework differs from audited profile')
    previous = adb(args.serial, 'shell', 'pm', 'path', PACKAGE, allow_missing=True)
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
    adb(args.serial, 'install', '--no-streaming', '-r', str(args.apk), timeout=45)
    installed = adb(args.serial, 'shell', 'pm', 'path', PACKAGE).removeprefix('package:')
    if adb(args.serial, 'shell', 'sha256sum', installed).split()[0] != args.sha256:
        raise ValueError('Installed TV APK hash mismatch')
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
    if args.output.exists():
        old = json.loads(args.output.read_text())
        if any(old.get(k) != config[k] for k in ('server_cert_sha256', 'fingerprint', 'incremental')):
            raise ValueError('Paired TV identity changed; output was not replaced')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.fchmod(fd, 0o600)
    with os.fdopen(fd, 'w') as out:
        json.dump(config, out, indent=2)
        out.write('\n')
    # This audited TCL firmware blocks background startup by default.
    adb(args.serial, 'shell', 'cmd', 'appops', 'set', PACKAGE, 'APP_AUTO_START', 'allow')
    startup = adb(args.serial, 'shell', 'cmd', 'appops', 'get', PACKAGE, 'APP_AUTO_START')
    if not startup.startswith('APP_AUTO_START: allow'):
        raise ValueError('TCL did not allow companion auto-start')
    # Recreate the initial unpaired activity so it loads the newly saved config.
    adb(args.serial, 'shell', 'am', 'start', '-S', '-W', '-n', PACKAGE + '/.MainActivity')
    print('TV companion installed and paired; credential-reference config saved. Verify pinned HTTPS before use.')


if __name__ == '__main__':
    main()
