#!/usr/bin/env python3
"""Stage, install and inspect the independent Android battery guard over managed SSH."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shlex
import time
from ultrasleep import Phone as ManagedPhone, absolute

SOURCE = Path(__file__).with_name('battery')
STATE = '/data/adb/homewire-battery'
BOOT = '/data/adb/service.d/homewire-battery.sh'
EVENTS = ('paused', 'critical', 'control', 'recovery', 'low', 'test')


def load(path):
    path = Path(path)
    if path.stat().st_mode & 0o077 or path.stat().st_size > 8192:
        raise ValueError('Private battery profile must be owner-only and at most 8192 bytes')
    value = json.loads(path.read_text())
    required = {'ssh', 'su', 'prefix'}
    optional = {'telegram_config', 'telegram_section', 'chat_id'}
    if not isinstance(value, dict) or not required <= value.keys() or value.keys() - required - optional:
        raise ValueError('Invalid battery profile fields')
    if not isinstance(value['ssh'], str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]*', value['ssh']):
        raise ValueError('Expected a managed SSH client alias')
    for key in ('su', 'prefix'):
        absolute(value[key])
    if optional & value.keys():
        if not optional <= value.keys():
            raise ValueError('Supply all notification references together')
        absolute(value['telegram_config'])
        if not re.fullmatch(r'\[channels\.telegram\.[A-Za-z0-9_-]+\]', value['telegram_section']):
            raise ValueError('Expected one explicit existing Telegram channel')
        if not isinstance(value['chat_id'], str) or not re.fullmatch(r'-?[0-9]{1,20}', value['chat_id']):
            raise ValueError('Expected one explicit Telegram recipient')
    return value


def notifier(profile, release):
    script = ('#!/system/bin/sh\nset -eu\numask 077\n'
              f'test -f {release}/alerts-approved\n'
              'case "$1" in paused|critical|control|recovery|low|test) ;; *) exit 2;; esac\n')
    if 'telegram_config' not in profile:
        return script + 'exit 75\n'
    args = [release + '/notify', profile['telegram_config'], profile['telegram_section'],
            profile['prefix'] + '/bin/curl', release + '/chat-id']
    return script + 'exec ' + ' '.join(shlex.quote(v) for v in args) + f' {release}/messages/"$1".txt\n'


class Phone(ManagedPhone):
    def root(self, command, data=None):
        return super().root('set -eu; ' + command, data)

    def stage(self):
        p = self.profile
        files = {'battery/guard.c': (SOURCE / 'guard.c').read_bytes(),
                 'battery/notify.c': (SOURCE / 'notify.c').read_bytes(),
                 'telegram_credentials.h': (SOURCE.parent / 'telegram_credentials.h').read_bytes()}
        digest = hashlib.sha256(b''.join(name.encode() + b'\0' + data for name, data in sorted(files.items()))
                                + json.dumps(p, sort_keys=True).encode() + Path(__file__).read_bytes()).hexdigest()[:20]
        release = STATE + '/releases/' + digest
        known = self.root(f'if [ -f {release}/validated ]; then cd {release}; sha256sum -c SHA256SUMS >/dev/null; echo validated; fi').strip()
        if known == 'validated':
            self.put(STATE + '/staged', (release + '\n').encode())
            print('Reused verified battery guard:', digest)
            return
        self.root(f'umask 077; mkdir -p {release}/battery {release}/messages; chmod 700 {STATE} {STATE}/releases {release} {release}/battery {release}/messages')
        for name, data in files.items():
            self.put(release + '/' + name, data)
        # Build as the existing application UID, then install immutable root copies.
        app = p['prefix'].removesuffix('/usr')
        uid = self.root('stat -c %u ' + shlex.quote(app)).strip()
        context = self.root('ls -Zd ' + shlex.quote(app)).split()[0]
        if not uid.isdecimal() or int(uid) < 10000 or not re.fullmatch(r'u:object_r:app_data_file:s0:c[0-9]+(?:,c[0-9]+){1,3}', context):
            raise ValueError('Expected an application-owned compiler installation')
        work = p['prefix'] + '/tmp/homewire-battery-' + digest
        self.root(f'mkdir -p {work}/battery; cp {release}/battery/*.c {work}/battery/; cp {release}/telegram_credentials.h {work}/; '
                  f'chown -R {uid}:{uid} {work}; chmod 700 {work}; chcon -R {shlex.quote(context)} {work}')
        build = (f'export PATH={p["prefix"]}/bin:/system/bin; '
                 f'clang -std=c11 -O2 -Wall -Wextra -Werror {work}/battery/guard.c -o {work}/guard && '
                 f'clang -std=c11 -O2 -Wall -Wextra -Werror -I{p["prefix"]}/include {work}/battery/notify.c '
                 f'-L{p["prefix"]}/lib -Wl,-rpath,{p["prefix"]}/lib -lcrypto -o {work}/notify')
        owner = (shlex.quote(p['su']) + f' -d -g {uid} -G 3003 -Z '
                 + shlex.quote(context.replace('u:object_r:app_data_file:', 'u:r:magisk:'))
                 + f' {uid} -s /system/bin/sh -c ')
        self.root(owner + shlex.quote(build))
        self.root(f'cp {work}/guard {work}/notify {release}/; '
                  f'for n in guard notify; do [ "$(sha256sum {work}/$n | cut -d " " -f 1)" = "$(sha256sum {release}/$n | cut -d " " -f 1)" ] || exit 1; done; '
                  f'chown 0:0 {release}/guard {release}/notify; chmod 700 {release}/guard {release}/notify; rm -rf {work}')
        self.put(release + '/notify.sh', notifier(p, release).encode(), '700')
        binding = {k: p[k] for k in ('telegram_config', 'telegram_section', 'chat_id') if k in p}
        self.put(release + '/notification-binding.json', (json.dumps(binding, sort_keys=True) + '\n').encode())
        extra = ''
        if 'chat_id' in p:
            self.put(release + '/chat-id', (p['chat_id'] + '\n').encode())
            extra = ' chat-id'
        self.root(f'cd {release}; sha256sum guard notify battery/guard.c battery/notify.c telegram_credentials.h notify.sh notification-binding.json{extra} > SHA256SUMS; chmod 600 SHA256SUMS; ./guard --sample')
        self.put(STATE + '/staged', (release + '\n').encode())
        print('Staged battery guard:', digest)

    def release(self):
        release = self.root(f'cat {STATE}/staged').strip()
        if not re.fullmatch(re.escape(STATE) + '/releases/[0-9a-f]{20}', release):
            raise ValueError('Invalid staged battery release')
        return release

    def stop(self):
        self.root(f'if [ -f {STATE}/supervisor.pid ]; then p=$(cat {STATE}/supervisor.pid); '
                  'case "$p" in ""|*[!0-9]*) exit 1;; esac; '
                  f'if kill -0 "$p" 2>/dev/null; then case "$(readlink /proc/$p/exe)" in {STATE}/releases/*/guard) kill -TERM "$p";; *) exit 1;; esac; '
                  'n=0; while kill -0 "$p" 2>/dev/null; do '
                  '[ "$(awk \'{print $3}\' /proc/$p/stat)" != Z ] || break; '
                  '[ "$n" -lt 50 ] || exit 1; n=$((n+1)); sleep .1; done; fi; fi')

    def install(self):
        release = self.release()
        self.root(f'cd {release}; sha256sum -c SHA256SUMS >/dev/null; ./guard --sample >/dev/null')
        previous = self.root(f'if [ -f {STATE}/active ]; then cat {STATE}/active; fi').strip()
        if previous:
            if not re.fullmatch(re.escape(STATE) + '/releases/[0-9a-f]{20}', previous):
                raise ValueError('Invalid active battery release')
            self.root(f'test -f {previous}/validated')
        self.stop()
        wrapper = (f'#!/system/bin/sh\nset -eu\numask 077\ncd {release}\n'
                   # Fail closed even if release integrity is damaged.
                   f'if ! sha256sum -c SHA256SUMS >/dev/null 2>&1; then echo integrity > {STATE}/held; '
                   'echo 1 > /sys/class/power_supply/battery/input_suspend; exit 1; fi\n'
                   f'exec ./guard --supervise {STATE} {release}/notify.sh\n')
        if previous and previous != release:
            self.root(f'cp -p {BOOT} {STATE}/previous-boot.sh')
            self.put(STATE + '/previous-release', (previous + '\n').encode())
        elif not previous:
            # A failed experimental install is never an eligible rollback.
            self.root(f'if [ -f {STATE}/previous-boot.sh ]; then mv {STATE}/previous-boot.sh {STATE}/unvalidated-boot.sh; fi')
        self.put(BOOT, wrapper.encode(), '700')
        self.root(f'nohup /system/bin/sh {BOOT} </dev/null >/dev/null 2>&1 &')
        time.sleep(2)
        value = json.loads(self.root(f'cat {STATE}/status.json'))
        if not isinstance(value.get('pid'), int) or abs(time.time() - value.get('at', 0)) > 10:
            raise RuntimeError('Battery guard did not produce fresh status')
        if self.root(f'readlink /proc/{value["pid"]}/exe').strip() != release + '/guard':
            raise RuntimeError('Battery guard process identity did not verify')
        if self.root('cat /sys/class/power_supply/battery/input_suspend').strip() != '1':
            raise RuntimeError('Input suspend did not stay asserted after startup cutoff')
        self.put(release + '/validated', b'Fresh worker identity and settled input-suspend readback verified.\n')
        self.put(STATE + '/active', (release + '\n').encode())
        self.status()
        print('Installed; notification delivery requires separately approved message files.')

    def rollback(self):
        previous = self.root(f'cat {STATE}/previous-release').strip()
        if not re.fullmatch(re.escape(STATE) + '/releases/[0-9a-f]{20}', previous):
            raise ValueError('Invalid previous battery release')
        self.root(f'test -f {STATE}/previous-boot.sh; test -f {previous}/validated; cd {previous}; sha256sum -c SHA256SUMS >/dev/null')
        self.stop()
        self.root(f'cp -p {BOOT} {STATE}/rollback-from.sh; cp -p {STATE}/previous-boot.sh {BOOT}; '
                  f'nohup /system/bin/sh {BOOT} </dev/null >/dev/null 2>&1 &')
        self.put(STATE + '/active', (previous + '\n').encode())
        print('Previous boot wrapper restored; charging state, fault latch and alerts were preserved. Verify status.')

    def status(self):
        print(self.root(f'cat {STATE}/status.json; '
                        f'r=$(cat {STATE}/active); if [ -f "$r/alerts-approved" ]; then echo alerts=enabled; else echo alerts=disabled; fi; '
                        'printf "input_suspended="; cat /sys/class/power_supply/battery/input_suspend').strip())

    def enable_alerts(self, directory):
        if 'telegram_config' not in self.profile:
            raise ValueError('Notification references are missing')
        release = self.root(f'cat {STATE}/active').strip()
        if not re.fullmatch(re.escape(STATE) + '/releases/[0-9a-f]{20}', release):
            raise ValueError('Invalid active battery release')
        binding = {k: self.profile[k] for k in ('telegram_config', 'telegram_section', 'chat_id')}
        if json.loads(self.root(f'cat {release}/notification-binding.json')) != binding:
            raise ValueError('Installed notification recipient differs from the selected profile')
        # The caller must obtain approval for these complete texts and recipient.
        texts = {}
        for event in EVENTS:
            text = (Path(directory) / (event + '.txt')).read_text().rstrip('\n')
            if not 1 <= len(text) <= 900 or any(ord(c) < 32 or ord(c) > 126 or c in '\\"' for c in text):
                raise ValueError('Messages must be bounded, single-line plain ASCII text')
            texts[event] = (text + '\n').encode()
        self.root(f'rm -f {release}/alerts-approved')
        for event, data in texts.items():
            self.put(release + '/messages/' + event + '.txt', data)
        self.put(release + '/alerts-approved', b'Explicit recipient and complete message texts approved.\n')
        print('Approved notification texts enabled.')

    def test_alert(self):
        release = self.root(f'cat {STATE}/active').strip()
        if not re.fullmatch(re.escape(STATE) + '/releases/[0-9a-f]{20}', release):
            raise ValueError('Invalid active battery release')
        self.root(release + '/notify.sh test')
        print('Test alert accepted by Telegram.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', required=True)
    parser.add_argument('action', choices=('stage', 'install', 'status', 'stop', 'rollback', 'enable-alerts', 'test-alert'))
    parser.add_argument('--approved-messages')
    args = parser.parse_args()
    phone = Phone(load(args.profile))
    if args.action == 'enable-alerts':
        if not args.approved_messages:
            parser.error('--approved-messages is required')
        phone.enable_alerts(args.approved_messages)
    else:
        getattr(phone, args.action.replace('-', '_'))()


if __name__ == '__main__':
    main()
