"""Repair boot triggers without rebuilding or changing the installed battery policy."""
import json
from pathlib import Path
import re
import shlex
import subprocess
import time
from ultrasleep import absolute

STATE = '/data/adb/homewire-battery'
BOOT = '/data/adb/service.d/homewire-battery.sh'


def load(path):
    path = Path(path)
    if path.stat().st_mode & 0o077 or path.stat().st_size > 8192:
        raise ValueError('Private battery profile must be owner-only and at most 8192 bytes')
    value = json.loads(path.read_text())
    transport = {'ssh', 'su', 'prefix'}
    # Existing hardware/notification metadata is not used by startup repair.
    metadata = {'telegram_config', 'telegram_section', 'chat_id', 'current_path',
                'charge_counter_path', 'charging_current_sign', 'extra_controls',
                'votable_cutoffs', 'flash_charge_current_limit_ua'}
    if not isinstance(value, dict) or not transport <= value.keys() or value.keys() - transport - metadata:
        raise ValueError('Invalid battery startup profile fields')
    profile = {key: value[key] for key in transport}
    if not isinstance(profile['ssh'], str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]*', profile['ssh']):
        raise ValueError('Expected a managed SSH client alias')
    for key in ('su', 'prefix'):
        absolute(profile[key])
    return profile


def boot_script(original, release):
    # Preserve all installed integrity checks, gates and policy exactly; never
    # reconstruct them from a different source revision or hardware profile.
    if (not original.startswith('#!/system/bin/sh\n') or len(original) > 16384
            or f'cd {release}\n' not in original
            or 'sha256sum -c SHA256SUMS' not in original
            or f'exec ./guard --supervise {STATE} {release}/notify.sh\n' not in original):
        raise ValueError('Installed battery boot wrapper does not match the validated active release')
    environment = 'unset LD_PRELOAD LD_LIBRARY_PATH\nexport PATH=/system/bin:/system/xbin\n'
    if environment in original:
        return original
    first, rest = original.split('\n', 1)
    return first + '\n' + environment + rest


def startup_fallback(profile):
    # Termux:Boot runs after the first unlock, independently of Magisk's boot
    # script dispatch. Do not add a wake lock or start any other root workload.
    command = f'''set -eu
unset LD_PRELOAD LD_LIBRARY_PATH
export PATH=/system/bin:/system/xbin
r=$(cat {STATE}/active)
p=$(cat {STATE}/supervisor.pid 2>/dev/null || true)
case "$p" in
    ''|*[!0-9]*) ;;
    *) if [ "$(readlink /proc/"$p"/exe 2>/dev/null)" = "$r/guard" ]; then exit 0; fi ;;
esac
# The native supervisor lock arbitrates simultaneous Magisk/fallback launches.
if [ -f {STATE}/startup.log ] && [ "$(stat -c %s {STATE}/startup.log)" -ge 65536 ]; then
    mv -f {STATE}/startup.log {STATE}/startup.previous.log
fi
printf 'Termux boot fallback requested at %s\\n' "$(date +%s)" >> {STATE}/startup.log
nohup /system/bin/sh {BOOT} </dev/null >>{STATE}/startup.log 2>&1 &
'''
    return ('#!' + profile['prefix'] + '/bin/sh\n'
            'unset LD_PRELOAD LD_LIBRARY_PATH\n'
            '# Variables in the command expand in the root shell, not the caller.\n'
            '# shellcheck disable=SC2016\n'
            'exec ' + shlex.quote(profile['su']) + ' -c ' + shlex.quote(command) + '\n')


def repair(phone):
    """Keep the active native release/state; add an independent boot trigger."""
    release = phone.root(f'cat {STATE}/active').strip()
    if not re.fullmatch(re.escape(STATE) + '/releases/[0-9a-f]{20}', release):
        raise ValueError('Invalid active battery release')
    phone.root(f'test -f {release}/validated; cd {release}; sha256sum -c SHA256SUMS >/dev/null')
    wrapper = boot_script(phone.root(f'cat {BOOT}'), release)
    prefix = phone.profile['prefix']
    if not prefix.endswith('/usr'):
        raise ValueError('Termux fallback requires the qualified application usr prefix')
    home = prefix[:-4] + '/home'
    directory = home + '/.termux/boot'
    phone.root(f'test -d {directory}; test ! -L {directory}; '
              'cmd package path com.termux.boot | grep -q "^package:"')
    uid = phone.root(f'stat -c %u {directory}').strip()
    if (not uid.isdecimal() or int(uid) < 10000
            or phone.root(f'stat -c %u {prefix}').strip() != uid):
        raise ValueError('Termux boot directory must have an application owner')
    fallback = directory + '/homewire-battery'
    phone.root(f'test ! -L {BOOT}; test ! -L {fallback}; '
              f'if [ ! -f {STATE}/startup-previous-boot.sh ]; then '
              f'cp -p {BOOT} {STATE}/startup-previous-boot.sh; fi; '
              f'if [ -f {fallback} ] && [ ! -f {STATE}/startup-previous-fallback.sh ]; then '
              f'cp -p {fallback} {STATE}/startup-previous-fallback.sh; fi')
    phone.put(BOOT, wrapper.encode(), '700')
    phone.put(fallback, startup_fallback(phone.profile).encode(), '700')
    phone.root(f'chown {uid}:{uid} {fallback}; chmod 700 {fallback}; '
              f'/system/bin/sh -n {BOOT}; /system/bin/sh -n {fallback}')
    # Exercise only the battery launch route; do not run all Termux boot
    # scripts, change Magisk's safe mode or revive unrelated services.
    launched = subprocess.run(phone.ssh + ['/system/bin/sh ' + shlex.quote(fallback)],
                              capture_output=True, timeout=20)
    if launched.returncode:
        raise RuntimeError('Termux fallback failed; renew the existing administrative root route without broadening grants')
    time.sleep(2)
    value = json.loads(phone.root(f'cat {STATE}/status.json'))
    pid = value.get('pid')
    if (type(pid) is not int or pid < 2 or abs(time.time() - value.get('at', 0)) > 45
            or phone.root(f'readlink /proc/{pid}/exe').strip() != release + '/guard'):
        raise RuntimeError('Battery startup route did not produce a fresh verified worker')
    print('Battery startup repaired; first-unlock fallback installed. Reboot recovery remains to be verified.')
