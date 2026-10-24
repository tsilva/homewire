#!/usr/bin/env python3
"""Install and operate the root-only Android ultrasleep controller over managed SSH."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shlex
import subprocess

SOURCE = Path(__file__).with_name('ultrasleep')
STATE = '/data/adb/homewire-ultrasleep'
BOOT = '/data/adb/service.d/homewire-ultrasleep.sh'


def absolute(value):
    if not isinstance(value, str) or not re.fullmatch(r'/[A-Za-z0-9_./-]+', value) or any(p in ('.', '..') for p in value.split('/')):
        raise ValueError('Expected a normalized absolute path')
    return value


def load(path):
    path = Path(path)
    if path.stat().st_mode & 0o077:
        raise ValueError('Private ultrasleep profile must be owner-only')
    value = json.loads(path.read_text())
    if set(value) - {'telegram_config', 'telegram_section', 'resume_services_first'} != {'ssh', 'su', 'prefix', 'homewire', 'guard', 'units', 'freeze_executables'}:
        raise ValueError('Invalid ultrasleep profile fields')
    if not re.fullmatch(r'[a-zA-Z0-9_-]+', value['ssh']):
        raise ValueError('Expected a managed SSH client alias')
    for key in ('su', 'prefix', 'homewire', 'guard'):
        absolute(value[key])
    if not isinstance(value['units'], list) or not 1 <= len(value['units']) <= 16:
        raise ValueError('Expected 1–16 supervisor units')
    for unit in value['units']:
        if set(unit) != {'pidfile', 'marker', 'launcher'}:
            raise ValueError('Invalid supervisor unit')
        for entry in unit.values():
            absolute(entry)
    if not isinstance(value['freeze_executables'], list) or len(value['freeze_executables']) > 16:
        raise ValueError('Invalid frozen workload list')
    for exe in value['freeze_executables']:
        absolute(exe)
    early = value.get('resume_services_first', [])
    if not isinstance(early, list) or len(early) > 16 or any(not isinstance(n, str) or not re.fullmatch(r'[a-zA-Z0-9_-]+', n) for n in early):
        raise ValueError('Invalid early service list')
    if bool(value.get('telegram_config')) != bool(value.get('telegram_section')):
        raise ValueError('Telegram config and section must be supplied together')
    if value.get('telegram_config'):
        absolute(value['telegram_config'])
        if not re.fullmatch(r'\[channels\.telegram\.[a-zA-Z0-9_-]+\]', value['telegram_section']):
            raise ValueError('Invalid explicit Telegram bot section')
    return value


class Phone:
    def __init__(self, profile):
        self.profile = profile
        self.ssh = ['ssh', '-o', 'ControlMaster=no', '-o', 'ProxyCommand=/usr/bin/false',
                    '-o', 'BatchMode=yes', '-o', 'PubkeyAuthentication=no',
                    '-o', 'PasswordAuthentication=no', '-o', 'KbdInteractiveAuthentication=no', profile['ssh']]

    def root(self, command, data=None):
        command = 'unset LD_PRELOAD LD_LIBRARY_PATH; export PATH=/system/bin:/system/xbin; ' + command
        result = subprocess.run(self.ssh + [self.profile['su'] + ' -c ' + shlex.quote(command)],
                                input=data or b'', capture_output=True, timeout=110)
        if result.returncode:
            raise RuntimeError('Remote ultrasleep operation failed: ' + result.stderr.decode(errors='replace')[:500])
        return result.stdout.decode()

    def put(self, path, data, mode='600'):
        digest = hashlib.sha256(data).hexdigest()
        q = shlex.quote(path)
        self.root(f'umask 077; cat > {q}.new; chmod {mode} {q}.new; '
                  f'[ "$(sha256sum {q}.new | cut -d " " -f 1)" = {digest} ] && mv -f {q}.new {q}', data)

    def stage(self):
        p = self.profile
        self.root(f'if [ -f {STATE}/controller.pid ]; then p=$(cat {STATE}/controller.pid); '
                  f'case "$(readlink /proc/$p/exe 2>/dev/null)" in {STATE}/releases/*/controller) exit 1;; esac; fi')
        uid = self.root('stat -c %u ' + shlex.quote(p['homewire'])).strip()
        if not uid.isdecimal() or int(uid) < 10000:
            raise ValueError('Expected an application-owned Homewire installation')
        context = self.root('ls -Zd ' + shlex.quote(p['homewire'])).split()[0]
        if not re.fullmatch(r'u:object_r:app_data_file:s0:c[0-9]+(?:,c[0-9]+){1,3}', context):
            raise ValueError('Unexpected application SELinux context')
        runtime_context = context.replace('u:object_r:app_data_file:', 'u:r:magisk:')
        config = {'STATE': STATE, 'SU': p['su'], 'PREFIX': p['prefix'], 'HOMEWIRE': p['homewire'],
                  'GUARD': p['guard'], 'APP_UID': uid, 'APP_USER': '0', 'APP_CONTEXT': runtime_context,
                  'TELEGRAM_CONFIG': p.get('telegram_config', ''), 'TELEGRAM_SECTION': p.get('telegram_section', '')}
        files = {name: (SOURCE / name).read_bytes() for name in ('controller.c', 'telegram.c', 'action.sh', 'supervisor.sh')}
        files['telegram_credentials.h'] = (SOURCE.parent / 'telegram_credentials.h').read_bytes()
        files['config.sh'] = ''.join(k + '=' + shlex.quote(v) + '\n' for k, v in config.items()).encode()
        files['units'] = ''.join('|'.join(u[k] for k in ('pidfile', 'marker', 'launcher')) + '\n' for u in p['units']).encode()
        files['early-services'] = ''.join(n + '\n' for n in p.get('resume_services_first', [])).encode()
        files['freeze-executables'] = ''.join(e + '\n' for e in p['freeze_executables']).encode()
        version = hashlib.sha256(b''.join(files.values())).hexdigest()[:20]
        release = STATE + '/releases/' + version
        self.root(f'umask 077; mkdir -p {release}; chmod 700 {STATE} {STATE}/releases {release}; '
                  f'[ ! -e {STATE}/transaction ]; test -x {shlex.quote(p["guard"])}; {shlex.quote(p["guard"])} check >/dev/null')
        for name, data in files.items():
            self.put(release + '/' + name, data)
        # Compile in a private temporary application directory; install a root copy.
        work = p['prefix'] + '/tmp/homewire-ultrasleep-' + version
        owner = (shlex.quote(p['su']) + f' -d -g {uid} -G 3003 -Z ' + shlex.quote(runtime_context)
                 + f' {uid} -s /system/bin/sh -c ')
        build = (f'export PATH={p["prefix"]}/bin:/system/bin; export PREFIX={p["prefix"]}; '
                 f'clang -std=c11 -O2 -Wall -Wextra -Werror {work}/controller.c -o {work}/controller && '
                 f'clang -std=c11 -O2 -Wall -Wextra -Werror -I{p["prefix"]}/include {work}/telegram.c -L{p["prefix"]}/lib -Wl,-rpath,{p["prefix"]}/lib -lcrypto -o {work}/telegram')
        self.root(f'mkdir -p {work}; chmod 700 {work}; cp {release}/controller.c {release}/telegram.c {release}/telegram_credentials.h {work}/; '
                  f'chown -R {uid}:{uid} {work}; chcon -R {shlex.quote(context)} {work}; '
                  + owner + shlex.quote(build) + f' && cp {work}/controller {release}/controller && '
                  f'cp {work}/telegram {release}/telegram && '
                  f'[ "$(sha256sum {work}/controller | cut -d " " -f 1)" = "$(sha256sum {release}/controller | cut -d " " -f 1)" ] && '
                  f'[ "$(sha256sum {work}/telegram | cut -d " " -f 1)" = "$(sha256sum {release}/telegram | cut -d " " -f 1)" ] && chown 0:0 {release}/controller {release}/telegram && chmod 700 {release}/controller {release}/telegram && rm -rf {work}')
        self.root(f'cd {release} && sha256sum controller telegram controller.c telegram.c telegram_credentials.h action.sh supervisor.sh config.sh units freeze-executables early-services > SHA256SUMS && chmod 600 SHA256SUMS')
        self.root(f'{release}/controller --probe {STATE} {release}/action.sh 1')
        if p.get('telegram_config'):
            self.root(f'{release}/telegram ' + shlex.quote(p['telegram_config']) + ' ' + shlex.quote(p['telegram_section']) + ' ' + shlex.quote(p['prefix'] + '/bin/curl') + ' --check-network')
        self.put(STATE + '/staged', (release + '\n').encode())
        print('Staged and wake-alarm probe passed:', version)

    def release(self):
        value = self.root(f'cat {STATE}/staged').strip()
        if not re.fullmatch(re.escape(STATE) + r'/releases/[0-9a-f]{20}', value):
            raise ValueError('Invalid staged release')
        return value

    def start(self, test=False):
        release = self.release()
        self.root(f'cd {release} && sha256sum -c SHA256SUMS >/dev/null')
        if test:
            args = '--test 300'
        else:
            status = json.loads(self.root(f'cat {STATE}/status.json'))
            if (status.get('phase') != 'test_complete' or not status.get('ok')
                    or status.get('suspended_seconds', 0) < 180 or status.get('action') != release + '/action.sh'):
                raise ValueError('A successful five-minute test with actual suspend is required before enabling')
            args = '--run Europe/Lisbon 1320 420'
            wrapper = f'#!/system/bin/sh\ncd {release} || exit 1\nsha256sum -c SHA256SUMS >/dev/null || exit 1\nexec /system/bin/sh {release}/supervisor.sh {release} {STATE} {args}\n'
            self.root(f'[ ! -e {BOOT} ] || cp -p {BOOT} {STATE}/previous-boot.sh')
            self.put(BOOT, wrapper.encode(), '700')
        mode, rest = args.split(' ', 1)
        self.root(f'umask 077; nohup /system/bin/sh {release}/supervisor.sh {release} {STATE} {mode} {rest} '
                  f'</dev/null >{STATE}/controller.log 2>&1 &')
        print('Five-minute offline test started.' if test else 'Nightly schedule enabled: 22:00–07:00 Europe/Lisbon.')

    def status(self):
        # No process command lines or application configurations are exposed.
        for name in ('status.json', 'entry-proof', 'restore-proof', 'telegram-resume'):
            print(self.root(f'if [ -f {STATE}/{name} ]; then cat {STATE}/{name}; fi').strip())
        print(self.root(f'if [ -d {STATE}/transaction ]; then echo recovery_journal=present; else echo recovery_journal=clear; fi').strip())

    def recover(self):
        self.root('/system/bin/sh ' + self.release() + '/action.sh leave >/dev/null 2>&1')
        print('Saved state restored; verify status and services.')

    def disable(self):
        self.root(f'rm -f {BOOT}; p=$(cat {STATE}/controller.pid); case "$p" in *[!0-9]*|"") exit 1;; esac; '
                  f'case "$(readlink /proc/$p/exe 2>/dev/null)" in {STATE}/releases/*/controller) kill -TERM "$p";; esac')
        print('Boot schedule disabled; controller requested to restore saved state.')

    def wake(self):
        self.root(f'p=$(cat {STATE}/controller.pid); case "$p" in *[!0-9]*|"") exit 1;; esac; '
                  f'[ "$(readlink /proc/$p/exe)" = "{self.release()}/controller" ] && kill -USR1 "$p"')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', required=True)
    parser.add_argument('action', choices=('stage', 'test', 'enable', 'status', 'wake', 'disable', 'recover'))
    args = parser.parse_args()
    phone = Phone(load(args.profile))
    if args.action == 'test':
        phone.start(True)
    elif args.action == 'enable':
        phone.start()
    else:
        getattr(phone, args.action)()


if __name__ == '__main__':
    main()
