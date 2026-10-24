"""Provisioning rejects wrong hardware/artifacts before installing or pairing."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

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


if __name__ == '__main__':
    unittest.main()
