"""Provisioning rejects wrong hardware/artifacts before installing or pairing."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

spec = importlib.util.spec_from_file_location('tv_install', Path(__file__).resolve().parents[1] / 'install.py')
install = importlib.util.module_from_spec(spec)
spec.loader.exec_module(install)


class InstallationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.profile = self.root / 'profile.json'
        self.value = dict(host='192.0.2.40', port=8766, model='Fixture TV', device='fixture',
                          fingerprint='verified-build', incremental='revision', framework_sha256='a' * 64)
        self.save_profile()

    def save_profile(self):
        self.profile.write_text(json.dumps(self.value))
        self.profile.chmod(0o600)

    def test_profile_rejects_ambiguous_or_malformed_identity(self):
        self.assertEqual(install.profile(self.profile), self.value)
        for field, value in [('port', True), ('port', 80), ('host', 'example.com'),
                             ('model', 'TV\n'), ('framework_sha256', 'z' * 64), ('extra', 1)]:
            with self.subTest(field=field):
                changed = dict(self.value, **{field: value})
                self.profile.write_text(json.dumps(changed))
                with self.assertRaises((ValueError, TypeError)):
                    install.profile(self.profile)

    def test_wrong_apk_never_contacts_tv(self):
        apk = self.root / 'candidate.apk'
        apk.write_bytes(b'incorrect artifact')
        argv = ['install.py', '--serial', 'fixture', '--apk', str(apk), '--sha256', '0' * 64,
                '--profile', str(self.profile), '--output', str(self.root / 'config.json')]
        with patch.object(sys, 'argv', argv), patch.object(install, 'adb') as adb:
            with self.assertRaisesRegex(ValueError, 'hash mismatch'):
                install.main()
            adb.assert_not_called()

    def test_wrong_firmware_never_installs(self):
        import hashlib
        apk = self.root / 'candidate.apk'
        apk.write_bytes(b'fixture')
        argv = ['install.py', '--serial', 'fixture', '--apk', str(apk), '--sha256',
                hashlib.sha256(apk.read_bytes()).hexdigest(), '--profile', str(self.profile),
                '--output', str(self.root / 'config.json')]
        with patch.object(sys, 'argv', argv), patch.dict(os.environ, {'HOMEWIRE_SECRET_TV_API_TOKEN': 'a' * 64}), \
                patch.object(install, 'adb', return_value='Other\nTV\nfixture\nbuild\nrevision') as adb:
            with self.assertRaisesRegex(ValueError, 'identity differs'):
                install.main()
            self.assertEqual(adb.call_count, 1)
            self.assertNotIn('install', adb.call_args.args)

class UpdateTests(InstallationTests):
    def setUp(self):
        super().setUp()
        import install as implementation
        self.install = implementation
        self.apk = self.root / 'candidate.apk'
        self.apk.write_bytes(b'new-apk')
        self.output = self.root / 'companion.json'
        self.old = dict(port=8766, server_cert_sha256='b' * 64, fingerprint='verified-build',
                        incremental='revision', token='${HOMEWIRE_SECRET_TV_API_TOKEN}')
        self.install.save_json(self.output, self.old)
        import hashlib
        self.argv = ['--serial', 'fixture', '--apk', str(self.apk), '--sha256',
                     hashlib.sha256(self.apk.read_bytes()).hexdigest(), '--profile', str(self.profile),
                     '--output', str(self.output), '--backup-directory', str(self.root / 'backups'),
                     '--update-only', '--expected-version', '0.1.1', '--expected-version-code', '1001']
        self.pin = 'b' * 64
        self.updated = False

    def fake_adb(self, serial, *args, **kwargs):
        import hashlib
        if args == ('shell', 'getprop ro.product.manufacturer; getprop ro.product.model; '
                    'getprop ro.product.device; getprop ro.build.fingerprint; getprop ro.build.version.incremental'):
            return 'TCL\nFixture TV\nfixture\nverified-build\nrevision'
        if args == ('shell', 'sha256sum', '/system_ext/framework/com.tcl.tv.display.jar'):
            return 'a' * 64 + ' framework'
        if args[:3] == ('shell', 'pm', 'path'):
            return 'package:/data/app/fixture/base.apk'
        if args[:2] == ('shell', 'sha256sum'):
            return hashlib.sha256(b'new-apk' if self.updated else b'old-apk').hexdigest() + ' base.apk'
        if args[0] == 'pull':
            Path(args[2]).write_bytes(b'old-apk')
            return ''
        if args[0] == 'install':
            self.updated = True
            return 'Success'
        if args[:3] == ('shell', 'dumpsys', 'package'):
            return 'versionName=0.1.1\nversionCode=1001 minSdk=31'
        if args[:3] == ('shell', 'content', 'query'):
            return 'Row: 0 paired=1, port=8766, certificate_sha256=' + self.pin
        if args[:4] == ('shell', 'cmd', 'appops', 'get'):
            return 'APP_AUTO_START: allow'
        if args[:2] == ('shell', 'am') or args[:4] == ('shell', 'cmd', 'appops', 'set'):
            return ''
        raise AssertionError('Unexpected ADB operation')

    def invoke(self, identities=None, verification=None):
        import build
        import verify
        identities = identities or [dict(version='0.1.1', version_code=1001, signer_sha256='c' * 64),
                                    dict(version='0.1.0', version_code=1, signer_sha256='c' * 64)]
        with patch.object(self.install, 'adb', side_effect=self.fake_adb) as adb, \
                patch.object(build, 'apk_identity', side_effect=identities), \
                patch.object(verify, 'verify', side_effect=verification or [dict(pinned_https=True), dict(pinned_https=True)]), \
                patch.dict(os.environ, HOMEWIRE_SECRET_TV_API_TOKEN='a' * 64, HOMEWIRE_ANDROID_BUILD_TOOLS='/fixture'):
            try:
                return self.install.main(self.argv)
            finally:
                self.install_calls = sum(c.args[1] == 'install' for c in adb.call_args_list)

    def test_update_preserves_pin_backs_up_old_apk_and_verifies_both_sides(self):
        result = self.invoke()
        self.assertEqual(self.install_calls, 1)
        self.assertEqual(Path(result['previous_apk']).read_bytes(), b'old-apk')
        self.assertEqual(json.loads(self.output.read_text())['server_cert_sha256'], 'b' * 64)
        self.assertNotIn('a' * 64, self.output.read_text())
        self.assertFalse(self.output.stat().st_mode & 0o077)

    def test_signer_and_nonincreasing_versions_are_rejected_before_install(self):
        for old in (dict(version_code=1, signer_sha256='d' * 64),
                    dict(version_code=1001, signer_sha256='c' * 64)):
            with self.subTest(old=old), self.assertRaises(ValueError):
                self.invoke([dict(version='0.1.1', version_code=1001, signer_sha256='c' * 64), old])
            self.assertEqual(self.install_calls, 0)

    def test_preflight_https_failure_does_not_install(self):
        with self.assertRaises(RuntimeError):
            self.invoke(verification=[RuntimeError('offline')])
        self.assertEqual(self.install_calls, 0)

    def test_postinstall_failure_is_not_retried_and_keeps_old_pin(self):
        before = self.output.read_bytes()
        with self.assertRaises(RuntimeError):
            self.invoke(verification=[dict(pinned_https=True), RuntimeError('offline')])
        self.assertEqual(self.install_calls, 1)
        self.assertEqual(self.output.read_bytes(), before)

    def test_changed_pin_never_replaces_saved_pairing(self):
        before = self.output.read_bytes()
        self.pin = 'd' * 64
        with self.assertRaises(ValueError):
            self.invoke()
        self.assertEqual(self.output.read_bytes(), before)
        self.assertEqual(self.install_calls, 1)


if __name__ == '__main__':
    unittest.main()
