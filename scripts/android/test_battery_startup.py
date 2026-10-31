"""Synthetic startup checks; no real phone, credentials or device controls."""
import contextlib
import io
import json
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch
import battery_startup as startup


def wrapper(release):
    return (f'#!/system/bin/sh\nset -eu\numask 077\ncd {release}\n'
            f'if ! sha256sum -c SHA256SUMS; then echo held > {startup.STATE}/held; exit 1; fi\n'
            f'exec ./guard --supervise {startup.STATE} {release}/notify.sh\n')


class FakePhone:
    profile = {'ssh': 'synthetic', 'su': '/synthetic/su', 'prefix': '/synthetic/files/usr'}
    ssh = ['synthetic-ssh']
    release = startup.STATE + '/releases/' + 'a' * 20

    def __init__(self):
        self.writes = []
        self.commands = []
        self.boot = wrapper(self.release)

    def put(self, *args):
        self.writes.append(args)

    def root(self, command):
        self.commands.append(command)
        if command == f'cat {startup.STATE}/active': return self.release
        if command == f'cat {startup.BOOT}': return self.boot
        if command.startswith('stat -c %u '): return '10123\n'
        if command.endswith('/status.json'): return json.dumps({'pid': 123, 'at': time.time()})
        if command == 'readlink /proc/123/exe': return self.release + '/guard\n'
        return ''


class StartupTests(unittest.TestCase):
    def test_profile_uses_only_transport_without_changing_hardware_metadata(self):
        profile = {**FakePhone.profile, 'extra_controls': [], 'current_path': '/synthetic/current'}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'profile.json'
            path.write_text(json.dumps(profile)); path.chmod(0o600)
            self.assertEqual(startup.load(path), FakePhone.profile)
            for key, value in [('ssh', '-bad'), ('su', '/data/../bad'), ('prefix', '/data/x;bad'),
                               ('unexpected', 'ignored')]:
                path.write_text(json.dumps({**profile, key: value}))
                with self.assertRaises(ValueError): startup.load(path)
            path.write_text(json.dumps(profile)); path.chmod(0o644)
            with self.assertRaises(ValueError): startup.load(path)

    def test_boot_wrapper_preserves_all_existing_cutoff_logic_and_is_idempotent(self):
        original = wrapper(FakePhone.release)
        hardened = startup.boot_script(original, FakePhone.release)
        self.assertTrue(hardened.endswith(original.split('\n', 1)[1]))
        self.assertIn('unset LD_PRELOAD LD_LIBRARY_PATH\nexport PATH=/system/bin:/system/xbin\n', hardened)
        self.assertEqual(startup.boot_script(hardened, FakePhone.release), hardened)
        with self.assertRaises(ValueError): startup.boot_script(original, FakePhone.release + 'b')

    def test_repair_uses_termux_identity_and_preserves_active_release_and_state(self):
        phone = FakePhone()
        with patch.object(startup.time, 'sleep'), patch.object(startup.subprocess, 'run') as run:
            run.return_value = subprocess.CompletedProcess([], 0)
            with contextlib.redirect_stdout(io.StringIO()): startup.repair(phone)
        fallback = '/synthetic/files/home/.termux/boot/homewire-battery'
        self.assertEqual([w[0] for w in phone.writes], [startup.BOOT, fallback])
        self.assertEqual(run.call_args.args[0], phone.ssh + ['/system/bin/sh ' + fallback])
        self.assertIn('sha256sum -c SHA256SUMS', '\n'.join(phone.commands))
        self.assertFalse(any('kill ' in c or 'rm ' in c for c in phone.commands))
        self.assertTrue(phone.writes[0][1].decode().endswith(phone.boot.split('\n', 1)[1]))

    def test_wrong_boot_binding_fails_before_any_writes(self):
        phone = FakePhone(); phone.boot = wrapper(phone.release + 'b')
        with self.assertRaises(ValueError): startup.repair(phone)
        self.assertEqual(phone.writes, [])

    def test_fallback_recovers_missing_guard_and_leaves_live_supervisor_alone(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state = root / 'state'; state.mkdir()
            release = state / 'releases' / ('a' * 20); release.mkdir(parents=True)
            (state / 'active').write_text(str(release) + '\n')
            (state / 'supervisor.pid').write_text('12345\n')
            proc = root / 'proc'; (proc / '12345').mkdir(parents=True)
            marker = root / 'launched'
            boot = root / 'boot.sh'
            boot.write_text('#!/bin/sh\necho launched >> ' + shlex.quote(str(marker)) + '\n')
            su = root / 'su'
            su.write_text('#!/bin/sh\n[ "$1" = -c ] || exit 1\nexec /bin/sh -c "$2"\n')
            su.chmod(0o700)
            with patch.object(startup, 'STATE', str(state)), patch.object(startup, 'BOOT', str(boot)):
                script = startup.startup_fallback({'prefix': '/synthetic/usr', 'su': str(su)})
            script = script.replace('export PATH=/system/bin:/system/xbin', 'export PATH=/usr/bin:/bin')
            script = script.replace('/proc/', str(proc) + '/').replace('/system/bin/sh', '/bin/sh')
            fallback = root / 'fallback.sh'; fallback.write_text(script)
            subprocess.run(['/bin/sh', str(fallback)], check=True)
            deadline = time.monotonic() + 3
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertEqual(marker.read_text(), 'launched\n')
            (proc / '12345' / 'exe').symlink_to(release / 'guard')
            subprocess.run(['/bin/sh', str(fallback)], check=True)
            self.assertEqual(marker.read_text(), 'launched\n')
            self.assertNotIn('wake-lock', script)
            self.assertNotIn('service-daemon', script)
            self.assertNotIn('--sqlite', script)
