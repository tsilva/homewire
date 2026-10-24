"""Synthetic native policy/I/O and notification tests; never access a real phone."""
import json
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import battery_guard


class GuardTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.directory = Path(cls.temp.name)
        cls.binary = cls.directory / 'policy'
        cls.sensors = cls.directory / 'sensors'
        cls.sensors.mkdir()
        source = cls.directory / 'policy.c'
        source.write_text('''#define main guard_program_main
#define BATTERY_SYSFS "''' + str(cls.sensors) + '''"
#include "''' + str(battery_guard.SOURCE / 'guard.c') + '''"
#undef main
int main(int argc, char **argv) {
    if (argc == 6 && !strcmp(argv[1], "cutoff")) {
        printf("%d\\n", cutoff_broken(atoi(argv[2]), atoi(argv[3]), atoi(argv[4]), argv[5]));
        return 0;
    }
    if (argc == 11) {
        struct sample s = {.valid=atoi(argv[1]), .temp=atoi(argv[2]), .percent=atoi(argv[3]), .voltage=atoi(argv[4])};
        snprintf(s.health, sizeof(s.health), "%s", argv[5]);
        struct policy p = {atoi(argv[6]), atoi(argv[7]), atoi(argv[8]), atoi(argv[9]), atoi(argv[10])};
        int allowed = decide(&p, &s);
        printf("{\\"allowed\\":%d,\\"heat\\":%d,\\"cold\\":%d,\\"ceiling\\":%d,\\"latch\\":%d,\\"good\\":%d}\\n",
            allowed, p.heat, p.cold, p.ceiling, p.latch, p.good);
        return 0;
    }
    if (argc == 3 && !strcmp(argv[1], "io")) {
        root = argv[2];
        if (!sample().valid || inhibit() || !exists("held")) return 1;
        int value;
        if (number(SWITCH, &value) || value != 1 || switch_to(1) || remove_record("held")) return 1;
        return exists("held");
    }
    return guard_program_main(argc, argv);
}
''')
        subprocess.run([shutil.which('cc'), '-std=c11', '-Wall', '-Wextra', '-Werror', str(source), '-o', str(cls.binary)], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def policy(self, temp=260, percent=40, voltage=3800000, health='Good', valid=1, heat=0, cold=0, ceiling=0, latch=0, good=2):
        return json.loads(subprocess.check_output([str(self.binary), *map(str, (valid, temp, percent, voltage, health, heat, cold, ceiling, latch, good))]))

    def test_heat_cold_and_charge_hysteresis(self):
        self.assertTrue(self.policy(temp=399)['allowed'])
        self.assertEqual(self.policy(temp=400)['heat'], 1)
        self.assertFalse(self.policy(temp=371, heat=1)['allowed'])
        self.assertTrue(self.policy(temp=370, heat=1)['allowed'])
        self.assertEqual(self.policy(temp=0)['cold'], 1)
        self.assertFalse(self.policy(temp=49, cold=1)['allowed'])
        self.assertTrue(self.policy(temp=50, cold=1)['allowed'])
        self.assertTrue(self.policy(percent=69)['allowed'])
        self.assertEqual(self.policy(percent=70)['ceiling'], 1)
        self.assertFalse(self.policy(percent=51, ceiling=1)['allowed'])
        self.assertTrue(self.policy(percent=50, ceiling=1)['allowed'])

    def test_critical_is_latched_and_unknown_never_recovers(self):
        for args in ({'temp': 500}, {'voltage': 4500000}, {'health': 'Dead'},
                     {'health': 'Over voltage'}, {'health': 'Safety timer expire'},
                     {'temp': 500, 'valid': 0}):
            with self.subTest(args=args):
                result = self.policy(**args)
                self.assertEqual(result['latch'], 1)
                self.assertFalse(result['allowed'])
        self.assertFalse(self.policy(latch=1)['allowed'])
        for health in ('Unknown', 'Overheat', 'Warm', 'Cold', 'unexpected'):
            self.assertFalse(self.policy(health=health)['allowed'])
        self.assertEqual(self.policy(valid=0)['good'], 0)

    def test_requires_three_distinct_runtime_samples_before_resume(self):
        for count in range(3):
            self.assertEqual(bool(self.policy(good=count)['allowed']), count == 2)

    def test_reboot_resets_the_switch_without_proving_a_failed_cutoff(self):
        cases = [(0, 1, 1, 'Charging', 0), (0, 1, 0, 'Charging', 0),
                 (1, 1, 1, 'Charging', 1), (1, 1, 0, 'Charging', 1),
                 (1, 1, 0, 'Not charging', 0), (1, 0, 1, 'Charging', 0)]
        for settled, held, enabled, status, expected in cases:
            with self.subTest(settled=settled, held=held, enabled=enabled, status=status):
                result = subprocess.check_output([str(self.binary), 'cutoff', str(settled), str(held), str(enabled), status])
                self.assertEqual(int(result), expected)

    def test_sensor_parsing_and_durable_control_attempt(self):
        values = {'temp': '260', 'capacity': '40', 'voltage_now': '3800000', 'present': '1',
                  'health': 'Good', 'status': 'Charging', 'input_suspend': '0'}
        for key, value in values.items():
            (self.sensors / key).write_text(value + '\n')
        with tempfile.TemporaryDirectory() as state:
            subprocess.run([str(self.binary), 'io', state], check=True)
            self.assertEqual((self.sensors / 'input_suspend').read_text(), '0\n')
        for key, bad in [('temp', ''), ('temp', '260garbage'), ('capacity', '101'),
                         ('present', '0'), ('status', 'Unknown'), ('voltage_now', '9000000')]:
            with self.subTest(key=key, bad=bad):
                (self.sensors / key).write_text(bad + '\n')
                result = subprocess.run([str(self.binary), '--sample'], capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                (self.sensors / key).write_text(values[key] + '\n')


class ProfileTests(unittest.TestCase):
    def test_install_rejects_a_cutoff_undone_by_the_charger_manager(self):
        phone = battery_guard.Phone({'ssh': 'synthetic', 'su': '/system/bin/su'})
        release = battery_guard.STATE + '/releases/' + '0' * 20
        phone.release = lambda: release
        phone.stop = lambda: None
        writes = []
        phone.put = lambda name, data, mode='600': writes.append(name)

        def root(command):
            if command == f'cat {battery_guard.STATE}/status.json':
                return json.dumps({'pid': 123, 'at': battery_guard.time.time()})
            if command == 'readlink /proc/123/exe':
                return release + '/guard\n'
            if command == 'cat /sys/class/power_supply/battery/input_suspend':
                return '0\n'  # Readback was correct initially, but the OEM reversed it.
            return ''

        phone.root = root
        with patch.object(battery_guard.time, 'sleep'), self.assertRaisesRegex(RuntimeError, 'did not stay asserted'):
            phone.install()
        self.assertNotIn(release + '/validated', writes)
        self.assertNotIn(battery_guard.STATE + '/active', writes)

    def test_invalid_profiles_and_disabled_notification_default(self):
        profile = {'ssh': 'synthetic', 'su': '/system/bin/su', 'prefix': '/data/user/0/org.example/files/usr'}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'profile.json'
            path.write_text(json.dumps(profile)); path.chmod(0o600)
            self.assertEqual(battery_guard.load(path), profile)
            for field, value in [('ssh', '-bad'), ('su', '/data/../bad'), ('prefix', '/data/x;bad'), ('chat_id', '123')]:
                path.write_text(json.dumps({**profile, field: value}))
                with self.assertRaises(ValueError): battery_guard.load(path)
            path.write_text(json.dumps(profile)); path.chmod(0o644)
            with self.assertRaises(ValueError): battery_guard.load(path)
        script = battery_guard.notifier(profile, '/synthetic/release')
        self.assertIn('test -f /synthetic/release/alerts-approved', script)
        self.assertIn('exit 75', script)

    def test_staging_a_changed_recipient_cannot_reuse_another_releases_approval(self):
        profile = {'telegram_config': '/synthetic/config.toml', 'telegram_section': '[channels.telegram.synthetic]',
                   'prefix': '/synthetic/usr', 'chat_id': '12345'}
        script = battery_guard.notifier(profile, '/synthetic/release-new')
        self.assertIn('test -f /synthetic/release-new/alerts-approved', script)
        self.assertIn('/synthetic/release-new/chat-id', script)
        self.assertNotIn('/data/adb/homewire-battery/chat-id', script)


class NotificationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name); self.binary = self.path / 'notify'
        subprocess.run([shutil.which('cc'), '-std=c11', '-Wall', '-Wextra', '-Werror',
                        str(battery_guard.SOURCE / 'notify.c'), '-o', str(self.binary),
                        *shlex.split(subprocess.check_output(['pkg-config', '--cflags', '--libs', 'openssl'], text=True))], check=True)
        self.config = self.path / 'config.toml'
        self.config.write_text('[channels.telegram.synthetic]\nenabled = true\nbot_token = "123456:SYNTHETIC_TOKEN"\n')
        (self.path / 'chat').write_text('12345\n')
        (self.path / 'message').write_text('Approved synthetic message.\n')
        self.curl = self.path / 'curl'
        self.curl.write_text('''#!/usr/bin/env python3
import sys
from pathlib import Path
data = sys.stdin.read()
assert sys.argv[1:] == ['--disable', '--config', '-']
assert 'chat_id=12345' in data and 'Approved synthetic message.' in data
assert '/sendMessage' in data
Path(__file__).with_name('sent').touch()
print('{"ok":true,"result":{"message_id":1}}')
''')
        self.curl.chmod(0o700)

    def send(self):
        return subprocess.run([str(self.binary), str(self.config), '[channels.telegram.synthetic]',
                               str(self.curl), str(self.path / 'chat'), str(self.path / 'message')], capture_output=True)

    def test_exact_text_and_recipient_pass_through_pipe_only(self):
        result = self.send()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b'')
        self.assertTrue((self.path / 'sent').exists())

    def test_disabled_channel_and_unapproved_content_fail_closed(self):
        self.config.write_text(self.config.read_text().replace('enabled = true', 'enabled = false'))
        self.assertNotEqual(self.send().returncode, 0)
        self.assertFalse((self.path / 'sent').exists())
        self.config.write_text(self.config.read_text().replace('enabled = false', 'enabled = true'))
        for file, text in [('chat', '12345&chat_id=6'), ('message', 'unsafe\nmessage'), ('message', '"injected"')]:
            (self.path / file).write_text(text)
            self.assertNotEqual(self.send().returncode, 0)
            self.assertFalse((self.path / 'sent').exists())

    def test_api_rejection_never_counts_as_delivery(self):
        self.curl.write_text('#!/bin/sh\ncat >/dev/null\nprintf \'{"ok":false,"description":"synthetic"}\'\n')
        self.assertNotEqual(self.send().returncode, 0)


if __name__ == '__main__':
    unittest.main()
