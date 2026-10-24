"""Standby availability must not introduce audio capture or an open operator API."""
from pathlib import Path
import unittest
import xml.etree.ElementTree as ET

ANDROID = '{http://schemas.android.com/apk/res/android}'
MANIFEST = Path(__file__).resolve().parents[1] / 'AndroidManifest.xml'


class PermissionTests(unittest.TestCase):
    def test_companion_cannot_request_microphone_capture(self):
        root = ET.parse(MANIFEST).getroot()
        permissions = {node.attrib[ANDROID + 'name'] for node in root.findall('uses-permission')}
        capture = {'android.permission.RECORD_AUDIO', 'android.permission.CAPTURE_AUDIO_OUTPUT',
                   'android.permission.CAPTURE_AUDIO_HOTWORD'}
        self.assertFalse(permissions & capture)

    def test_operator_provider_and_connection_service_remain_private(self):
        app = ET.parse(MANIFEST).getroot().find('application')
        provider = next(p for p in app.findall('provider')
                        if p.attrib[ANDROID + 'name'] == '.PairingProvider')
        self.assertEqual(provider.attrib[ANDROID + 'permission'], 'android.permission.DUMP')
        service = next(s for s in app.findall('service')
                       if s.attrib[ANDROID + 'name'] == '.TvService')
        self.assertEqual(service.attrib[ANDROID + 'exported'], 'false')


if __name__ == '__main__':
    unittest.main()
