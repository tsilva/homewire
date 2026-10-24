"""Calendar boundaries and root-profile validation for the offline controller."""
from datetime import datetime
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from zoneinfo import ZoneInfo
import ultrasleep


class ScheduleTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.binary = Path(cls.temp.name) / 'controller'
        subprocess.run([shutil.which('cc'), '-std=c11', '-D_POSIX_C_SOURCE=200809L', '-Wall', '-Wextra', '-Werror',
                        str(ultrasleep.SOURCE / 'controller.c'), '-o', str(cls.binary)], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def plan(self, text):
        now = datetime.fromisoformat(text).replace(tzinfo=ZoneInfo('Europe/Lisbon'))
        data = json.loads(subprocess.check_output([str(self.binary), '--plan', 'Europe/Lisbon', str(int(now.timestamp())), '1320', '420']))
        return now, data

    def test_boundaries(self):
        for text, asleep, expected in [('2026-10-24T21:59:59', False, '2026-10-24T22:00:00'),
                                       ('2026-10-24T22:00:00', True, '2026-10-25T07:00:00'),
                                       ('2026-10-25T06:59:59', True, '2026-10-25T07:00:00'),
                                       ('2026-10-25T07:00:00', False, '2026-10-25T22:00:00')]:
            with self.subTest(text=text):
                _, actual = self.plan(text)
                target = datetime.fromisoformat(expected).replace(tzinfo=ZoneInfo('Europe/Lisbon'))
                self.assertEqual(actual, {'asleep': asleep, 'next_transition': int(target.timestamp())})

    def test_dst_night_lengths(self):
        for text, hours in [('2026-10-24T22:00:00', 10), ('2026-03-28T22:00:00', 8), ('2026-10-20T22:00:00', 9)]:
            now, actual = self.plan(text)
            self.assertEqual(actual['next_transition'] - int(now.timestamp()), hours * 3600)

    def test_invalid_windows(self):
        for start, end in [('420', '1320'), ('1320', '1320'), ('oops', '420'), ('1440', '420')]:
            self.assertNotEqual(subprocess.run([str(self.binary), '--plan', 'Europe/Lisbon', '1792875600', start, end], capture_output=True).returncode, 0)

    def test_invalid_paths(self):
        for path in ['relative', '/data/../bad', '/data/path;touch', '/data/path\nother']:
            with self.assertRaises(ValueError):
                ultrasleep.absolute(path)

    def test_enable_requires_suspend_evidence(self):
        phone = ultrasleep.Phone({'ssh': 'synthetic', 'su': '/system/bin/su'})
        phone.release = lambda: '/data/adb/homewire-ultrasleep/releases/' + '0' * 20
        phone.root = lambda *args: '{"phase":"test_complete","ok":true,"suspended_seconds":0}'
        with self.assertRaises(ValueError):
            phone.start()

class TelegramTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name)
        self.binary = self.path / 'telegram'
        subprocess.run([shutil.which('cc'), '-std=c11', '-Wall', '-Wextra', '-Werror',
                        str(ultrasleep.SOURCE / 'telegram.c'), '-o', str(self.binary), *subprocess.check_output(['pkg-config', '--cflags', '--libs', 'openssl'], text=True).split()], check=True)
        self.config = self.path / 'config.toml'
        self.config.write_text('[channels.telegram.synthetic]\nenabled = true\nbot_token = "123456:SYNTHETIC_TOKEN"\n')
        self.curl = self.path / 'curl'
        self.calls = self.path / 'calls'
        self.curl.write_text('#!/usr/bin/env python3\nimport sys\nfrom pathlib import Path\ns=sys.stdin.read()\np=Path(__file__).with_name("calls")\nn=int(p.read_text()) if p.exists() else 0\np.write_text(str(n+1))\nassert sys.argv[1:]==["--disable","--config","-"]\nassert "/getUpdates" in s\nassert "sendMessage" not in s\nif "offset=-1&" in s:\n print(\'{"ok":true,"result":[{"update_id":42,"message":{"text":"synthetic"}}]}\')\nelse:\n assert "offset=43&" in s\n print(\'{"ok":true,"result":[]}\')\n')
        self.curl.chmod(0o700)

    def run_helper(self):
        return subprocess.run([str(self.binary), str(self.config), '[channels.telegram.synthetic]', str(self.curl)], capture_output=True)

    def test_confirms_last_pending_update_without_sending(self):
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls.read_text(), '2')
        self.assertEqual(result.stdout, b'')

    def test_encrypted_token_and_authentication(self):
        self.config.with_name('.secret_key').write_text(bytes(range(32)).hex())
        blob = '000102030405060708090a0bb8c93b341c219f13eecd6bbbdd4947209624fdac143a8f09fd020653e4a1d8039d9c689e4512'
        self.config.write_text(self.config.read_text().replace('123456:SYNTHETIC_TOKEN', 'enc2:' + blob))
        self.assertEqual(self.run_helper().returncode, 0)
        self.calls.unlink()
        self.config.with_name('.secret_key').write_text('ff' * 32)
        self.assertNotEqual(self.run_helper().returncode, 0)
        self.assertFalse(self.calls.exists())

    def test_quoted_toml_keys(self):
        self.config.write_text('["channels"."telegram"."synthetic"]\n"enabled" = true\n"bot_token" = "123456:SYNTHETIC_TOKEN"\n')
        self.assertEqual(self.run_helper().returncode, 0)

    def test_disabled_channel_remains_disabled(self):
        self.config.write_text(self.config.read_text().replace('enabled = true', 'enabled = false'))
        self.assertNotEqual(self.run_helper().returncode, 0)
        self.assertFalse(self.calls.exists())

    def test_invalid_credential_is_not_sent(self):
        self.config.write_text(self.config.read_text().replace('SYNTHETIC_TOKEN', 'unsafe\\"option'))
        self.assertNotEqual(self.run_helper().returncode, 0)
        self.assertFalse(self.calls.exists())

    def test_api_error_fails_closed(self):
        self.curl.write_text('#!/bin/sh\ncat >/dev/null\nprintf \'{"ok":false,"description":"synthetic"}\'\n')
        self.assertNotEqual(self.run_helper().returncode, 0)


if __name__ == '__main__':
    unittest.main()
