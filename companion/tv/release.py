"""Build and verify a local signed TV update in one command; never publishes or clears data."""
import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import io
import ipaddress
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid

import build
import install

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[1]
TOOL_KEYS = {'JAVA_HOME', 'HOMEWIRE_ANDROID_PLATFORM', 'HOMEWIRE_ANDROID_BUILD_TOOLS'}


class GateError(RuntimeError):
    """A safe gate label, never provider output or private inputs."""


def private_json(path):
    path = Path(path)
    if path.stat().st_mode & 0o077 or path.stat().st_uid != os.getuid():
        raise ValueError('Release inputs must be owned by this user and chmod 600')
    if path.stat().st_size > 16384:
        raise ValueError('Oversized release input')
    return json.loads(path.read_text())


def binding(value):
    if not isinstance(value, dict) or set(value) != {'project_id', 'environment', 'folder', 'domain'}:
        raise ValueError('Invalid Infisical binding fields')
    uuid.UUID(value['project_id'])
    if not re.fullmatch(r'[a-zA-Z0-9_-]{1,64}', value['environment']):
        raise ValueError('Invalid Infisical environment')
    if not re.fullmatch(r'/[a-zA-Z0-9_-]+(?:/[a-zA-Z0-9_-]+)*', value['folder']):
        raise ValueError('Infisical requires an explicit non-root folder')
    if not re.fullmatch(r'https://[a-zA-Z0-9.-]+(?::[0-9]+)?', value['domain']):
        raise ValueError('Invalid Infisical instance')
    return value


def configuration(path):
    value = private_json(path)
    required = {'toolchain', 'signing', 'runtime', 'serial', 'profile', 'companion', 'artifacts'}
    if set(value) - required - {'verify_connect', 'ssh_alias'} or not required <= set(value):
        raise ValueError('Invalid release configuration fields')
    if not isinstance(value['toolchain'], dict) or set(value['toolchain']) - TOOL_KEYS:
        raise ValueError('Invalid toolchain fields')
    for key in ('signing', 'runtime'):
        binding(value[key])
    if value['signing'] == value['runtime']:
        raise ValueError('Signing and runtime credentials require separate folders')
    if not isinstance(value['serial'], str) or not 1 <= len(value['serial']) <= 256 or any(c.isspace() for c in value['serial']):
        raise ValueError('Invalid explicit ADB serial')
    for key in ('profile', 'companion', 'artifacts'):
        value[key] = str((Path(path).resolve().parent / value[key]).resolve())
    for key, setting in value['toolchain'].items():
        value['toolchain'][key] = str((Path(path).resolve().parent / Path(setting).expanduser()).resolve())
    if 'verify_connect' in value:
        connect = value['verify_connect']
        if not isinstance(connect, list) or len(connect) != 2:
            raise ValueError('Invalid private verification endpoint')
        ipaddress.ip_address(connect[0])
        if type(connect[1]) is not int or not 1 <= connect[1] <= 65535:
            raise ValueError('Invalid private verification port')
    if 'ssh_alias' in value and (not re.fullmatch(r'[a-zA-Z0-9._-]{1,64}', value['ssh_alias']) or 'verify_connect' in value):
        raise ValueError('SSH routing requires a safe managed alias and no manual verification endpoint')
    install.profile(value['profile'])
    return value


@contextmanager
def route(config_path, settings, env):
    """Add only our forwards to an existing managed master; cancel them on exit."""
    alias = settings.get('ssh_alias')
    if not alias:
        yield config_path, settings
        return
    profile = install.profile(settings['profile'])
    host = profile['host']
    if settings['serial'] not in (host + ':5555', '[' + host + ']:5555'):
        raise GateError('SSH routing requires the audited TV host with ADB port 5555')
    run(['ssh', '-O', 'check', alias], env, gate='Managed SSH master; restore it through FleetMan')
    reservations = [socket.socket(), socket.socket()]
    try:
        for sock in reservations:
            sock.bind(('127.0.0.1', 0))
        ports = [sock.getsockname()[1] for sock in reservations]
    finally:
        for sock in reservations:
            sock.close()
    target = '[' + host + ']' if ':' in host else host
    forwards = ['-L', f'127.0.0.1:{ports[0]}:{target}:5555',
                '-L', f'127.0.0.1:{ports[1]}:{target}:{profile["port"]}']
    temporary = None
    serial = '127.0.0.1:' + str(ports[0])
    try:
        run(['ssh', '-O', 'forward', '-o', 'ExitOnForwardFailure=yes', *forwards, alias], env,
            gate='Private TV forwarding')
        forwarded = dict(settings, serial=serial, verify_connect=['127.0.0.1', ports[1]])
        forwarded.pop('ssh_alias')
        fd, temporary = tempfile.mkstemp(prefix='tv-release-route-', suffix='.json', dir=config_path.parent)
        os.close(fd)
        install.save_json(temporary, forwarded)
        yield Path(temporary), forwarded
    finally:
        if temporary:
            Path(temporary).unlink(missing_ok=True)
        # Disconnect only this invocation's temporary serial; never stop adb/SSH.
        subprocess.run(['adb', 'disconnect', serial], capture_output=True, timeout=10)
        cleanup = subprocess.run(['ssh', '-O', 'cancel', *forwards, alias], capture_output=True, timeout=10)
        if cleanup.returncode:
            print('TV route cleanup needs attention; shared SSH master was preserved.', file=sys.stderr)


def run(command, env=None, cwd=REPO, gate='command', timeout=180):
    # Never surface child output: provider errors may contain private inputs.
    result = subprocess.run(list(map(str, command)), cwd=cwd, env=env, capture_output=True, timeout=timeout)
    if result.returncode:
        raise GateError(gate + ' failed; no device operation is retried')
    return result.stdout


def injected(profile, command, env, gate):
    return run(['infisical', 'run', '--projectId=' + profile['project_id'],
                '--env=' + profile['environment'], '--path=' + profile['folder'],
                '--domain=' + profile['domain'], '--expand=false', '--include-imports=false',
                '--silent', '--telemetry=false', '--', *map(str, command)], env, gate=gate, timeout=360)


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def snapshot(destination):
    hasher = hashlib.sha256()
    with tarfile.open(destination, 'w:gz') as archive:
        for path in sorted(ROOT.rglob('*')):
            relative = path.relative_to(ROOT)
            if any(part in ('build', '__pycache__') for part in relative.parts) or not path.is_file():
                continue
            if path.is_symlink() or path.suffix not in ('.java', '.py', '.sh', '.xml', '.md', '.json'):
                raise ValueError('Unexpected companion source file')
            data = path.read_bytes()
            hasher.update(str(relative).encode() + b'\0' + data + b'\0')
            info = archive.gettarinfo(path, arcname=str(relative))
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    destination.chmod(0o600)
    return hasher.hexdigest()


def extract(archive, target):
    total = 0
    with tarfile.open(archive, 'r:gz') as source:
        members = source.getmembers()
        if len(members) > 256:
            raise ValueError('Oversized source archive')
        for member in members:
            name = Path(member.name)
            total += member.size
            if not member.isfile() or name.is_absolute() or '..' in name.parts or total > 4 * 1024 * 1024:
                raise ValueError('Unsafe source archive')
            destination = target / name
            destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            with source.extractfile(member) as incoming, destination.open('xb') as outgoing:
                shutil.copyfileobj(incoming, outgoing)
            destination.chmod(0o600)


def checks(source, env):
    print('Checking Rust, Android deployment, companion tests and shell scripts...', flush=True)
    for command in (['cargo', 'fmt', '--check'], ['cargo', 'test', '--locked'],
                    ['cargo', 'clippy', '--locked', '--all-targets', '--', '-D', 'warnings'],
                    [sys.executable, '-m', 'unittest', 'discover', '-s', 'scripts/android', '-p', 'test_*.py'],
                    [sys.executable, '-m', 'unittest', 'discover', '-s', source / 'tests', '-p', 'test_*.py'],
                    ['sh', source / 'test.sh'],
                    ['shellcheck', source / 'build.sh', source / 'test.sh', 'scripts/android/build.sh',
                     'scripts/android/sandbox/build.sh', 'scripts/android/sandbox/build-identity.sh']):
        result = subprocess.run(list(map(str, command)), env=env, cwd=REPO, capture_output=True)
        label = ' '.join(map(str, command))
        # Synthetic checks run before credential injection; retain diagnostics locally.
        with (source.parent / 'checks.log').open('ab') as log:
            os.chmod(log.name, 0o600)
            log.write((label + '\n').encode() + result.stdout + result.stderr + b'\n')
        if result.returncode:
            raise GateError('Validation: ' + ' '.join(map(str, command[:3])) + ' failed; see checks.log')


def reboot_check(settings, receipt):
    from verify import verify
    # This command is intentionally opt-in, single-shot, and never opens the activity.
    serial = settings['serial']
    before = install.adb(serial, 'shell', 'cat', '/proc/sys/kernel/random/boot_id')
    if not re.fullmatch(r'[a-f0-9-]{36}', before):
        raise GateError('Cannot read TV boot identity; no reboot attempted')
    install.adb(serial, 'reboot')
    deadline = time.monotonic() + 180
    time.sleep(2)
    while time.monotonic() < deadline:
        try:
            boot = install.adb(serial, 'shell', 'cat', '/proc/sys/kernel/random/boot_id', timeout=5)
            if boot != before and re.fullmatch(r'[a-f0-9-]{36}', boot) and install.adb(
                    serial, 'shell', 'getprop', 'sys.boot_completed', timeout=5) == '1':
                result = verify(install.profile(settings['profile']), private_json(settings['companion']),
                                settings.get('verify_connect'), wait_seconds=30)
                receipt.update(reboot_verified=True, reboot_checks=result)
                return
        except (RuntimeError, subprocess.TimeoutExpired):
            pass
        time.sleep(2)
    raise RuntimeError('TV boot/autostart verification failed; TV was not rebooted again')


def deploy_child(config_path, directory, reboot=False):
    settings = configuration(config_path)
    receipt_path = directory / 'receipt.json'
    receipt = private_json(receipt_path)
    if receipt['phase'] != 'deploying':
        raise ValueError('Release is not awaiting deployment')
    apk = directory / 'homewire-tv.apk'
    if digest(apk) != receipt['apk_sha256']:
        raise ValueError('Release APK changed before installation')
    args = ['--serial', settings['serial'], '--apk', str(apk), '--sha256', receipt['apk_sha256'],
            '--profile', settings['profile'], '--output', settings['companion'],
            '--backup-directory', str(directory / 'previous-apk'), '--update-only',
            '--expected-version', receipt['version'], '--expected-version-code', str(receipt['version_code']),
            '--receipt', str(directory / 'installation.json')]
    if settings.get('verify_connect'):
        args += ['--verify-connect', *map(str, settings['verify_connect'])]
    result = install.main(args)
    if reboot:
        # Persist install success even if the deliberate reboot check later fails.
        reboot_check(settings, result)
        install.save_json(directory / 'installation.json', result)


def deploy(config_path, settings, directory, receipt, env, reboot):
    receipt['phase'] = 'connecting'
    install.save_json(directory / 'receipt.json', receipt)
    print('Checking the existing pairing, installing once, then verifying pinned HTTPS...', flush=True)
    with route(config_path, settings, env) as (routed_path, routed_settings):
        serial = routed_settings['serial']
        connect_adb(serial, env)
        receipt['phase'] = 'deploying'
        install.save_json(directory / 'receipt.json', receipt)
        command = [sys.executable, ROOT / 'release.py', '--config', routed_path, '--deploy-child', directory]
        if reboot:
            command.append('--verify-reboot')
        injected(settings['runtime'], command, env, 'Deployment / pinned HTTPS verification')
    receipt['phase'] = 'verified'
    receipt['installation'] = private_json(directory / 'installation.json')
    install.save_json(directory / 'receipt.json', receipt)


def failure(receipt, error):
    gate = receipt['phase']
    receipt['failed_gate'] = gate
    if isinstance(error, GateError):
        receipt['failure_reason'] = str(error)
    # Transport setup has no installer/control child. Manual reconnection is safe.
    receipt['phase'] = 'built' if gate == 'connecting' else 'failed'


def connect_adb(serial, env):
    network = ':' in serial
    deadline = time.monotonic() + (35 if network else 10)

    def command(arguments, maximum):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return None
        try:
            result = subprocess.run(['adb', *arguments], env=env, capture_output=True,
                                    timeout=min(maximum, remaining))
        except subprocess.TimeoutExpired:
            return None
        # adb connect may exit zero even when it failed. Its text and get-state,
        # rather than process success, establish authorization and readiness.
        output = (result.stdout + result.stderr).lower()
        if b'unauthorized' in output or b'failed to authenticate' in output:
            raise GateError('Authorize this ADB host on the TV before deployment')
        return result

    for attempt in range(3 if network else 1):
        if network:
            if attempt:
                # Drop only this failed serial. No global reconnect/kill-server,
                # installation, application start or device control is replayed.
                disconnected = command(['disconnect', serial], 3)
                if disconnected is None or disconnected.returncode:
                    break
            connected = command(['connect', serial], 12)
            # A registered connection can take a moment to become ready. A failed
            # registration must be recreated instead of only polling it offline.
            ready_until = min(deadline, time.monotonic() + 2)
            if connected is not None and b'failed to connect' in connected.stdout.lower():
                ready_until = time.monotonic()
        else:
            ready_until = deadline
        while True:
            result = command(['-s', serial, 'get-state'], 3)
            if result is not None and result.returncode == 0 and result.stdout.strip() == b'device':
                return
            if time.monotonic() >= ready_until:
                break
            time.sleep(min(0.25, max(0, ready_until - time.monotonic())))
        if time.monotonic() >= deadline:
            break
        if network and attempt < 2:
            time.sleep(min(1, max(0, deadline - time.monotonic())))
    raise GateError('TV ADB transport unavailable after bounded connection setup; no installation attempted')


def deploy_existing(args):
    settings = configuration(args.config)
    env = build.environment(settings['toolchain'])
    env['PYTHONDONTWRITEBYTECODE'] = '1'
    directory = args.deploy_existing.resolve()
    receipt = private_json(directory / 'receipt.json')
    if receipt['phase'] != 'built':
        raise ValueError('Only a built, never-deployed candidate can be deployed')
    if digest(directory / 'source.tar.gz') != receipt['source_archive_sha256'] or digest(directory / 'homewire-tv.apk') != receipt['apk_sha256']:
        raise ValueError('Saved release artifacts changed')
    identity = build.apk_identity(directory / 'homewire-tv.apk', env['HOMEWIRE_ANDROID_BUILD_TOOLS'], env)
    if any(identity[k] != receipt[k] for k in identity):
        raise ValueError('Saved release APK identity changed')
    try:
        deploy(args.config.resolve(), settings, directory, receipt, env, args.verify_reboot)
    except Exception as error:
        failure(receipt, error)
        install.save_json(directory / 'receipt.json', receipt)
        raise
    print('Deployed and verified: ' + str(directory))


def release(args):
    config_path = args.config.resolve()
    settings = configuration(config_path)
    env = build.environment(settings['toolchain'])
    env['PYTHONDONTWRITEBYTECODE'] = '1'
    if not shutil.which('infisical') or (not args.build_only and not shutil.which('adb')):
        raise ValueError('Install Infisical CLI and Android platform-tools (adb)')
    code = build.version_code(args.version)
    artifact_root = Path(settings['artifacts'])
    artifact_root.mkdir(mode=0o700, parents=True, exist_ok=True)
    if artifact_root.stat().st_mode & 0o077:
        raise ValueError('Artifact directory requires chmod 700')
    directory = artifact_root / args.version
    directory.mkdir(mode=0o700)  # Fail closed on any previously attempted version.
    receipt = {'version': args.version, 'version_code': code, 'phase': 'preparing',
               'source_commit': run(['git', 'rev-parse', 'HEAD']).decode().strip(),
               'source_dirty': bool(run(['git', 'status', '--porcelain'])),
               'recovery_from': None}
    install.save_json(directory / 'receipt.json', receipt)
    try:
        archive = directory / 'source.tar.gz'
        if args.rollback:
            previous = private_json(args.rollback / 'receipt.json')
            if previous['phase'] not in ('built', 'verified') or code <= previous['version_code'] or digest(args.rollback / 'source.tar.gz') != previous['source_archive_sha256']:
                raise ValueError('Recovery requires an intact source archive and a higher new version')
            shutil.copyfile(args.rollback / 'source.tar.gz', archive)
            receipt['recovery_from'] = str(args.rollback.resolve())
            receipt['source_tree_sha256'] = previous['source_tree_sha256']
        else:
            receipt['source_tree_sha256'] = snapshot(archive)
        receipt['source_archive_sha256'] = digest(archive)
        source = directory / 'source'
        source.mkdir(mode=0o700)
        extract(archive, source)
        checks(source, env)
        receipt['phase'] = 'building'
        install.save_json(directory / 'receipt.json', receipt)
        print('Building with the persistent signing identity...', flush=True)
        injected(settings['signing'], [sys.executable, source / 'build.py', '--version', args.version,
                                      '--output', directory / 'homewire-tv.apk'], env, 'Signed build / Infisical authentication')
        identity = build.apk_identity(directory / 'homewire-tv.apk', env['HOMEWIRE_ANDROID_BUILD_TOOLS'], env)
        if identity['version'] != args.version or identity['version_code'] != code:
            raise ValueError('Built APK version mismatch')
        receipt.update(identity, apk_sha256=digest(directory / 'homewire-tv.apk'), phase='built',
                       toolchain={'jdk': 17, 'android_platform': 35, 'build_tools': '35.0.0'})
        install.save_json(directory / 'receipt.json', receipt)
        if not args.build_only:
            deploy(config_path, settings, directory, receipt, env, args.verify_reboot)
        install.save_json(directory / 'receipt.json', receipt)
    except Exception as error:
        failure(receipt, error)
        install.save_json(directory / 'receipt.json', receipt)
        raise
    print(('Built' if args.build_only else 'Deployed and verified') + ': ' + str(directory), flush=True)
    return directory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', type=Path, default=REPO / '.local/tv-release.json')
    parser.add_argument('--version', help='New MAJOR.MINOR.PATCH; each component 0..999')
    parser.add_argument('--build-only', action='store_true', help='Signed build and tests without contacting the TV')
    parser.add_argument('--rollback', type=Path, help='Rebuild this saved release source under a higher --version')
    parser.add_argument('--deploy-existing', type=Path, help='Deploy the exact APK from a successful --build-only run')
    parser.add_argument('--verify-reboot', action='store_true', help='Deliberately reboot once and prove boot startup')
    parser.add_argument('--deploy-child', type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        if args.deploy_child:
            deploy_child(args.config, args.deploy_child, args.verify_reboot)
            return
        if (not args.version and not args.deploy_existing) or (args.build_only and args.verify_reboot):
            parser.error('--version is required; --verify-reboot requires deployment')
        if args.deploy_existing and (args.version or args.build_only or args.rollback):
            parser.error('--deploy-existing cannot be combined with --version, --build-only or --rollback')
        # A separate release invocation must never race an install on this profile.
        with args.config.with_suffix('.lock').open('a') as lock:
            os.chmod(lock.name, 0o600)
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise ValueError('Another TV release is in progress') from None
            if args.deploy_existing:
                deploy_existing(args)
            else:
                release(args)
    except Exception as error:
        # No exception values: an SDK/provider failure can include credentials.
        detail = str(error) if isinstance(error, GateError) else type(error).__name__
        print('TV release stopped (' + detail + '). No automatic retry. '
              'Inspect the private release receipt for the failed gate; renew Infisical login if authentication failed.', file=sys.stderr)
        sys.exit(1)


if __name__ == '__main__':
    main()
