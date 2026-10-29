"""Synthetic release tests: no signing credentials, real phone or ADB server."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / 'scripts/artifact.py'
spec = importlib.util.spec_from_file_location('betty_release_artifact', SOURCE)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class FakeAdb:
    def __init__(self, timeout=False, changed_settings=False, wrong_bytes=False):
        self.calls = []
        self.installed = False
        self.timeout = timeout
        self.changed_settings = changed_settings
        self.wrong_bytes = wrong_bytes

    def ready(self, wait):
        self.calls.append(('ready', wait))

    def version(self):
        return 1005 if self.installed else 1004

    def settings(self):
        return {'assistant': 'synthetic.assistant' if not (self.installed and self.changed_settings)
                else 'synthetic.changed', 'power_long_press': '5', 'usb_debugging': '1'}

    def pull_apk(self, destination):
        destination.write_bytes(b'wrong' if self.installed and self.wrong_bytes
                                else b'candidate' if self.installed else b'previous')
        destination.chmod(0o600)

    def command(self, *args, **kwargs):
        self.calls.append(args)
        if args[0] == 'install':
            if self.timeout:
                raise module.CheckError('Synthetic uncertain installation')
            self.installed = True
            return 'Success\n'
        return ''


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.apk = self.root / 'candidate.apk'
        self.apk.write_bytes(b'candidate')
        self.receipt = {'version_code': 1005, 'version': '0.1.5', 'sha256': module.digest(self.apk)}
        self.binding = {'schema': 1, 'serial': 'SYNTHETIC_USB', 'signer_sha256': 'a' * 64}

    def install(self, adb, actual_signer=None, launch=False):
        with patch.object(module, 'Adb', return_value=adb), \
             patch.object(module, 'signer', return_value=actual_signer or self.binding['signer_sha256']), \
             patch.object(module, 'package_info', return_value=(module.PACKAGE, 1004, '0.1.4')):
            return module.install(self.apk, self.receipt, None, self.binding, launch=launch)

    def record(self):
        return json.loads((self.root / 'installation/installation.json').read_text())

    def test_verified_update_backs_up_installs_once_and_preserves_settings(self):
        adb = FakeAdb()
        result = self.install(adb)
        self.assertEqual(result['phase'], 'verified')
        self.assertEqual(result['settings_before'], result['settings_after'])
        self.assertEqual(sum(c[0] == 'install' for c in adb.calls), 1)
        self.assertEqual((self.root / 'installation/previous.apk').read_bytes(), b'previous')
        self.assertEqual(self.record()['apk_sha256'], module.digest(self.apk))
        self.assertFalse(any(c[:3] == ('shell', 'am', 'start') for c in adb.calls))
        self.assertEqual((self.root / 'installation/installation.json').stat().st_mode & 0o077, 0)

    def test_launch_is_separate_and_explicit(self):
        adb = FakeAdb()
        self.assertTrue(self.install(adb, launch=True)['launched'])
        self.assertTrue(any(c[:3] == ('shell', 'am', 'start') for c in adb.calls))

    def test_uncertain_install_is_never_replayed(self):
        adb = FakeAdb(timeout=True)
        with self.assertRaises(module.CheckError):
            self.install(adb)
        self.assertTrue(self.record()['install_started'])
        self.assertEqual(self.record()['phase'], 'installation_requested')
        with self.assertRaises(module.CheckError):
            self.install(adb)
        self.assertEqual(sum(c[0] == 'install' for c in adb.calls), 1)

    def test_mismatched_installed_signer_stops_before_install(self):
        adb = FakeAdb()
        with self.assertRaises(module.CheckError):
            self.install(adb, actual_signer='b' * 64)
        self.assertFalse(self.record()['install_started'])
        self.assertFalse(any(c[0] == 'install' for c in adb.calls))

    def test_downgrade_or_equal_version_stops_before_evidence(self):
        self.receipt['version_code'] = 1004
        with self.assertRaises(module.CheckError):
            self.install(FakeAdb())
        self.assertFalse((self.root / 'installation').exists())

    def test_installed_hash_or_settings_mismatch_cannot_report_success(self):
        for flag in ('wrong_bytes', 'changed_settings'):
            with self.subTest(flag=flag):
                target = self.root / flag
                target.mkdir()
                self.apk = target / 'candidate.apk'
                self.apk.write_bytes(b'candidate')
                with self.assertRaises(module.CheckError):
                    self.install(FakeAdb(**{flag: True}))
                record = json.loads((target / 'installation/installation.json').read_text())
                self.assertEqual(record['phase'], 'installed')
                self.assertNotIn('installed_hash_verified', record)

    def test_preview_receipt_is_rejected_without_sdk_or_adb(self):
        path = self.apk.with_suffix('.receipt.json')
        path.write_text(json.dumps({'package': module.PACKAGE + '.preview',
                                    'signed': True, 'preview': True}))
        with self.assertRaisesRegex(module.CheckError, 'production'):
            module.verify_artifact(self.apk, None, self.binding)

    def test_tampered_apk_is_rejected_before_native_tools(self):
        self.apk.with_suffix('.receipt.json').write_text(json.dumps({
            'package': module.PACKAGE, 'signed': True, 'preview': False,
            'version_code': 1005, 'version': '0.1.5', 'sha256': '0' * 64}))
        with self.assertRaisesRegex(module.CheckError, 'APK receipt hash'):
            module.verify_artifact(self.apk, None, self.binding)

    def test_private_binding_rejects_broad_permissions_and_invalid_shape(self):
        path = self.root / 'device.json'
        path.write_text(json.dumps(self.binding))
        path.chmod(0o644)
        with self.assertRaises(module.CheckError):
            module.identity(path)
        path.chmod(0o600)
        self.assertEqual(module.identity(path), self.binding)
        path.write_text('[]')
        with self.assertRaises(module.CheckError):
            module.identity(path)

    def test_adb_requires_explicit_usb_target(self):
        with patch.object(module.shutil, 'which', return_value='synthetic-adb'):
            adb = module.Adb('SYNTHETIC_USB')
        with patch.object(adb, 'command', side_effect=['device', 'local:synthetic']):
            with self.assertRaisesRegex(module.CheckError, 'USB'):
                adb.ready(0)


if __name__ == '__main__':
    unittest.main()
