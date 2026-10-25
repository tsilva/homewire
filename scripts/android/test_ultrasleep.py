"""Calendar boundaries and root-profile validation for the offline controller."""
from datetime import datetime
import json
from pathlib import Path
import shlex
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

    def plan(self, text, start=0):
        now = datetime.fromisoformat(text).replace(tzinfo=ZoneInfo('Europe/Berlin'))
        data = json.loads(subprocess.check_output([str(self.binary), '--plan', 'Europe/Berlin', str(int(now.timestamp())), str(start), '360']))
        return now, data

    def test_boundaries(self):
        for text, asleep, expected in [('2026-10-24T23:59:59', False, '2026-10-25T00:00:00'),
                                       ('2026-10-25T00:00:00', True, '2026-10-25T06:00:00'),
                                       ('2026-10-25T05:59:59', True, '2026-10-25T06:00:00'),
                                       ('2026-10-25T06:00:00', False, '2026-10-26T00:00:00'),
                                       ('2026-10-25T21:00:00', False, '2026-10-26T00:00:00')]:
            with self.subTest(text=text):
                _, actual = self.plan(text)
                target = datetime.fromisoformat(expected).replace(tzinfo=ZoneInfo('Europe/Berlin'))
                self.assertEqual(actual, {'asleep': asleep, 'next_transition': int(target.timestamp())})

    def test_dst_night_lengths(self):
        for text, hours in [('2026-10-25T00:00:00', 7), ('2026-03-29T00:00:00', 5), ('2026-10-20T00:00:00', 6)]:
            now, actual = self.plan(text)
            self.assertEqual(actual['next_transition'] - int(now.timestamp()), hours * 3600)

    def test_legacy_evening_window_still_supported(self):
        now, actual = self.plan('2026-10-24T21:00:00', start=1260)
        self.assertTrue(actual['asleep'])
        self.assertEqual(actual['next_transition'] - int(now.timestamp()), 10 * 3600)

    def test_invalid_windows(self):
        for start, end in [('360', '1320'), ('0', '0'), ('1320', '1320'), ('oops', '360'), ('1440', '360')]:
            self.assertNotEqual(subprocess.run([str(self.binary), '--plan', 'Europe/Berlin', '1792875600', start, end], capture_output=True).returncode, 0)

    def test_invalid_paths(self):
        for path in ['relative', '/data/../bad', '/data/path;touch', '/data/path\nother']:
            with self.assertRaises(ValueError):
                ultrasleep.absolute(path)

    def test_enable_requires_suspend_evidence(self):
        phone = ultrasleep.Phone({'ssh': 'synthetic', 'su': '/system/bin/su', 'schedule': {'timezone':'Etc/UTC','start':'21:30','end':'06:15'}})
        phone.release = lambda: '/data/adb/homewire-ultrasleep/releases/' + '0' * 20
        phone.root = lambda *args: '{"phase":"test_complete","ok":true,"suspended_seconds":0}'
        with self.assertRaises(ValueError):
            phone.start()

    def test_schedule_is_private_explicit_and_validated(self):
        self.assertEqual(ultrasleep.schedule({'timezone':'Etc/UTC','start':'21:30','end':'06:15'}),
                         ('Etc/UTC',1290,375))
        for value in (None, {}, {'timezone':'Etc/UTC','start':'09:00','end':'18:00'},
                      {'timezone':'invalid/fixture','start':'21:30','end':'06:15'},
                      {'timezone':'Etc/UTC','start':'25:00','end':'06:15'}):
            with self.subTest(value=value), self.assertRaises(ValueError):
                ultrasleep.schedule(value)
        phone=ultrasleep.Phone({'ssh':'synthetic','su':'/synthetic/su'})
        phone.root=lambda *args: self.fail('Missing schedule must fail before any remote operation')
        with self.assertRaises(ValueError):phone.start()

    def test_enable_uses_private_schedule_without_changing_it(self):
        policy={'timezone':'Etc/UTC','start':'21:30','end':'06:15'}
        phone=ultrasleep.Phone({'ssh':'synthetic','su':'/synthetic/su','schedule':dict(policy)})
        release='/data/adb/homewire-ultrasleep/releases/'+'0'*20
        phone.release=lambda:release
        commands=[];files={}
        def root(command,*args):
            commands.append(command)
            return json.dumps({'phase':'test_complete','ok':True,'suspended_seconds':240,
                               'action':release+'/action.sh'})
        phone.root=root
        phone.put=lambda path,data,mode:files.update({path:data})
        phone.start()
        self.assertEqual(phone.profile['schedule'],policy)
        self.assertIn(b'--run Etc/UTC 1290 375',files[ultrasleep.BOOT])
        self.assertIn('--run Etc/UTC 1290 375',commands[-1])

class TelegramTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name)
        self.binary = self.path / 'telegram'
        subprocess.run([shutil.which('cc'), '-std=c11', '-Wall', '-Wextra', '-Werror',
                        '-I' + str(ultrasleep.SOURCE.parent), str(ultrasleep.SOURCE / 'telegram.c'), '-o', str(self.binary), *subprocess.check_output(['pkg-config', '--cflags', '--libs', 'openssl'], text=True).split()], check=True)
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


class ServiceTransactionTests(unittest.TestCase):
    """Run the real shell transaction with synthetic radios and supervisors."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name)
        self.state = self.path / 'state'
        self.prefix = self.path / 'prefix'
        self.state.mkdir()
        (self.state / 'snapshot.new').mkdir()
        (self.prefix / 'var/service').mkdir(parents=True)
        self.services = {'admin': 'run', 'worker': 'run', 'disabled': 'down'}
        for name, state in self.services.items():
            (self.prefix / 'var/service' / name).mkdir()
            (self.prefix / ('mock-' + name)).write_text(state + '\n')
        self.guard = self.path / 'guard'
        self.guard.write_text('#!/bin/sh\nexit 0\n')
        self.guard.chmod(0o700)
        config = {'STATE': self.state, 'PREFIX': self.prefix, 'HOMEWIRE': self.path / 'homewire',
                  'GUARD': self.guard, 'SU': '/synthetic/su', 'APP_UID': '12345',
                  'APP_USER': '0', 'APP_CONTEXT': 'synthetic', 'TELEGRAM_CONFIG': ''}
        (self.path / 'config.sh').write_text(''.join(f'{k}={shlex.quote(str(v))}\n' for k, v in config.items()))
        for name in ('units', 'freeze-executables', 'early-services'):
            (self.path / name).write_text('')
        (self.state / 'session').write_text('test\n')
        # Source the production functions, replacing only the Android boundaries.
        self.library = (ultrasleep.SOURCE / 'action.sh').read_text().rsplit('case "${1:-}" in', 1)[0]
        self.mocks = r'''
export PATH=/usr/bin:/bin
termux() {
    mock_name=${1##*/}; mock_name=${mock_name%\'}
    mock_state=$(cat "$PREFIX/mock-$mock_name")
    printf '%s\n' "$1" >> "$STATE/calls"
    case "$1" in
        'sv status '*)
            case "$mock_state" in
                error) return 1;;
                unknown) echo 'warning: unavailable'; return 0;;
            esac
            printf '%s: %s/var/service/%s: synthetic\n' "$mock_state" "$PREFIX" "$mock_name";;
        'sv -w 15 '*)
            [ ! -e "$STATE/no-change" ] || return 0
            case "$1" in *' up '*) echo run;; *) echo down;; esac > "$PREFIX/mock-$mock_name";;
        *) return 1;;
    esac
}
setting() {
    case "$1" in wifi_on) echo 1;; *) echo 0;; esac
}
dumpsys() {
    case "$*" in 'deviceidle get force') echo false;; nfc) echo mState=off;; esac
}
restore_setting() { :; }
cmd() { echo cmd >> "$STATE/radios"; }
svc() { echo svc >> "$STATE/radios"; }
am() { echo am >> "$STATE/radios"; }
input() { echo input >> "$STATE/radios"; }
sync() { :; }
'''

    def run_shell(self, body):
        runner = self.path / 'runner'
        runner.write_text(self.library + self.mocks + '\n' + body + '\n')
        return subprocess.run(['/bin/sh', str(runner)], capture_output=True, text=True)

    def journal(self):
        txn = self.state / 'transaction'
        txn.mkdir()
        for name in ('complete', 'frozen', 'units'):
            (txn / name).write_text('')
        for name, value in {'services': 'admin\nworker\n', 'session': 'test\n', 'wakelock': '0\n',
                            'airplane_mode_on': '0\n', 'wifi_on': '1\n', 'bluetooth_on': '0\n'}.items():
            (txn / name).write_text(value)
        return txn

    def test_failed_or_unknown_snapshot_aborts_before_parking(self):
        for state in ('error', 'unknown'):
            with self.subTest(state=state):
                (self.prefix / 'mock-worker').write_text(state)
                result = self.run_shell('enter')
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.state / 'transaction').exists())
                self.assertFalse((self.state / 'radios').exists())
                self.assertNotIn(' -w ', (self.state / 'calls').read_text())

    def test_snapshot_keeps_disabled_services_out_of_restore_list(self):
        result = self.run_shell('snapshot_services')
        self.assertEqual(result.returncode, 0, result.stderr)
        saved = self.state / 'snapshot.new'
        self.assertEqual((saved / 'services').read_text(), 'admin\nworker\n')
        self.assertIn('disabled|down\n', (saved / 'service-states').read_text())

    def test_successful_control_without_changed_state_is_rejected(self):
        (self.state / 'no-change').touch()
        result = self.run_shell('set_service down admin')
        self.assertNotEqual(result.returncode, 0)

    def test_failed_resume_keeps_journal_and_does_not_claim_success(self):
        txn = self.journal()
        (self.prefix / 'mock-admin').write_text('down')
        (self.state / 'no-change').touch()
        result = self.run_shell('leave')
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(txn.exists())
        self.assertFalse((self.state / 'restore-proof').exists())

    def test_successful_resume_retains_one_completed_journal(self):
        txn = self.journal()
        old = self.state / 'last-transaction'
        old.mkdir()
        (old / 'old').touch()
        (self.prefix / 'mock-admin').write_text('down')
        result = self.run_shell('leave')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(txn.exists())
        self.assertTrue((old / 'services').exists())
        self.assertFalse((old / 'old').exists())
        self.assertEqual((self.prefix / 'mock-disabled').read_text().strip(), 'down')
        self.assertTrue((self.state / 'restore-proof').exists())

    def test_failed_root_launcher_is_not_accepted_as_restored(self):
        txn = self.journal()
        (txn / 'units').write_text(f'999999|{self.path}/missing.pid|/synthetic/unit|/synthetic/launcher\n')
        result = self.run_shell('identity() { return 1; }\nnohup() { :; }\nsleep() { :; }\nleave')
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(txn.exists())
        self.assertFalse((self.state / 'restore-proof').exists())


if __name__ == '__main__':
    unittest.main()
