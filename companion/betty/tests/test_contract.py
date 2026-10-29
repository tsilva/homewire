"""Security/build contracts independent of Android credentials or a live recipient."""
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
ANDROID = '{http://schemas.android.com/apk/res/android}'


class ContractTests(unittest.TestCase):
    def test_destination_survives_empty_cache_and_rejects_changed_identity(self):
        java = Path('/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home/bin')
        cache = ROOT.parents[1] / '.local/betty-deps'
        tdlib = cache / 'src/main/java/org/drinkless/tdlib/TdApi.java'
        annotation = cache / 'annotation-jvm-1.9.1.jar'
        if not (java / 'javac').exists() or not tdlib.exists() or not annotation.exists():
            self.skipTest('Run the pinned Betty build first to populate JDK/TDLib inputs')
        lock = json.loads((ROOT / 'dependencies.lock.json').read_text())
        pinned = next(item for item in lock['files'] if item['path'].endswith('/TdApi.java'))
        self.assertEqual(hashlib.sha256(tdlib.read_bytes()).hexdigest(), pinned['sha256'])
        self.assertEqual(hashlib.sha256(annotation.read_bytes()).hexdigest(), lock['annotation']['sha256'])
        with tempfile.TemporaryDirectory() as out:
            sources = [tdlib, ROOT / 'src/eu/tsilva/homewire/betty/Destination.java',
                       ROOT / 'tests/DestinationChecks.java']
            subprocess.run([str(java / 'javac'), '--release', '8', '-classpath', str(annotation),
                            '-d', out, *map(str, sources)], check=True, capture_output=True)
            subprocess.run([str(java / 'java'), '-cp', out, 'eu.tsilva.homewire.betty.DestinationChecks'],
                           check=True, capture_output=True)

    def test_handoff_requires_receipt_and_never_repeats_send(self):
        java = Path('/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home/bin')
        if not (java / 'javac').exists():
            self.skipTest('JDK 17 unavailable')
        with tempfile.TemporaryDirectory() as out:
            sources = [ROOT / 'src/eu/tsilva/homewire/betty/Handoff.java',
                       ROOT / 'src/eu/tsilva/homewire/betty/TelegramLink.java',
                       ROOT / 'tests/HandoffChecks.java']
            subprocess.run([str(java / 'javac'), '--release', '8', '-d', out,
                            *map(str, sources)], check=True, capture_output=True)
            subprocess.run([str(java / 'java'), '-cp', out, 'eu.tsilva.homewire.betty.HandoffChecks'],
                           check=True, capture_output=True)

    def test_password_preservation_and_safe_login_errors(self):
        java = Path('/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home/bin')
        if not (java / 'javac').exists():
            self.skipTest('JDK 17 unavailable')
        with tempfile.TemporaryDirectory() as out:
            subprocess.run([str(java / 'javac'), '--release', '8', '-d', out,
                            str(ROOT / 'src/eu/tsilva/homewire/betty/Login.java'),
                            str(ROOT / 'tests/LoginChecks.java')], check=True, capture_output=True)
            subprocess.run([str(java / 'java'), '-cp', out, 'eu.tsilva.homewire.betty.LoginChecks'],
                           check=True, capture_output=True)

    def test_only_foreground_capture_permissions(self):
        manifest = ET.parse(ROOT / 'AndroidManifest.xml').getroot()
        permissions = {p.get(ANDROID + 'name') for p in manifest.findall('uses-permission')}
        self.assertEqual(permissions, {'android.permission.INTERNET', 'android.permission.ACCESS_NETWORK_STATE',
                                       'android.permission.RECORD_AUDIO'})
        self.assertFalse(manifest.findall('.//service'))
        self.assertFalse(manifest.findall('.//receiver'))
        self.assertEqual(manifest.find('application').get(ANDROID + 'allowBackup'), 'false')
        self.assertEqual(manifest.find('application').get(ANDROID + 'usesCleartextTraffic'), 'false')
        self.assertEqual(len(manifest.findall('.//activity')), 1)
        self.assertIsNotNone(manifest.find(".//action[@" + ANDROID + "name='android.intent.action.ASSIST']"))

    def test_cloud_and_device_transfer_exclude_every_domain(self):
        rules = ET.parse(ROOT / 'res/xml/data_extraction_rules.xml').getroot()
        for mode in ('cloud-backup', 'device-transfer'):
            self.assertEqual({p.get('domain') for p in rules.find(mode)},
                             {'root', 'file', 'database', 'sharedpref', 'external'})
            self.assertTrue(all(p.get('path') == '.' for p in rules.find(mode)))

    def test_lock_has_both_native_architectures_and_content_hashes(self):
        lock = json.loads((ROOT / 'dependencies.lock.json').read_text())
        self.assertRegex(lock['commit'], r'^[a-f0-9]{40}$')
        for abi in ('arm64-v8a', 'x86_64'):
            natives = [Path(e['path']).name for e in lock['files'] if '/' + abi + '/' in e['path']]
            self.assertCountEqual(natives, ['libtdjni.so', 'libcryptox.so', 'libsslx.so'])
        for entry in lock['files']:
            self.assertRegex(entry['sha256'], r'^[a-f0-9]{64}$')
            self.assertGreater(entry['size'], 1024)
        for abi in ('arm64-v8a', 'x86_64'):
            self.assertTrue(any(entry['path'] == 'ndk/' + abi + '/libc++_shared.so'
                                for entry in lock['ndk']['files']))

    def test_state_machine_rejects_implicit_or_repeated_send(self):
        java = Path('/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home/bin')
        if not (java / 'javac').exists():
            self.skipTest('JDK 17 unavailable')
        with tempfile.TemporaryDirectory() as out:
            subprocess.run([str(java / 'javac'), '--release', '8', '-d', out,
                            str(ROOT / 'src/eu/tsilva/homewire/betty/Draft.java'),
                            str(ROOT / 'tests/DraftChecks.java')], check=True, capture_output=True)
            subprocess.run([str(java / 'java'), '-cp', out, 'eu.tsilva.homewire.betty.DraftChecks'],
                           check=True, capture_output=True)


if __name__ == '__main__':
    unittest.main()
