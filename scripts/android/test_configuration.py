"""Unified migration, credential projection and scoped API updates."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import configuration
import deployment
import phone
from test_deployment import fixture


class ConfigurationTests(unittest.TestCase):
    def test_migration_preserves_state_paths_and_keeps_a_private_backup(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            meaco = {'device_id': 'synthetic', 'presets': {'day': {'mode': 'manual'}},
                     'schedule': {'timezone': 'Etc/UTC', 'day': '09:00', 'night': '22:00'}}
            configuration.write(root / 'meaco.json', meaco)
            configuration.write(root / 'lights.json', {'lights': [], 'automation': {'light': 'strip'}})
            configuration.write(root / 'shopping.json', {})
            configuration.write(root / 'router.json', {'password': '${HOMEWIRE_SECRET_UNUSED}'})
            configuration.write(root / 'deployment.json', fixture())
            state = root / 'meaco.state.json'
            state.write_bytes(b'unchanged-state')
            output = root / 'home.json'
            backup = configuration.migrate(root, output)
            value = configuration.load(output)
            self.assertEqual(value['routines']['meaco'], meaco['presets'])
            self.assertEqual(value['automations']['meaco'], meaco['schedule'])
            self.assertNotIn('presets', value['adapters']['meaco'])
            self.assertNotIn('router', value['adapters'])
            self.assertTrue((root / 'router.json').is_file())
            self.assertFalse((backup / 'router.json').exists())
            self.assertEqual(value['automations']['lights'], {'light': 'strip'})
            self.assertEqual(state.read_bytes(), b'unchanged-state')
            self.assertFalse((root / 'meaco.json').exists())
            self.assertEqual(configuration.load(backup / 'meaco.json'), meaco)
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)
            self.assertEqual(backup.stat().st_mode & 0o777, 0o700)
            with self.assertRaises(ValueError): configuration.migrate(root, output)

    def test_projection_excludes_host_profile_and_materializes_only_target_keys(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            value = {'version': 1, 'adapters': {}, 'services': {'api': {
                'dashboard_token': '${HOMEWIRE_SECRET_TOKEN}',
                'private_key': 'env:HOMEWIRE_SECRET_KEY'}}, 'deployment': fixture()}
            path = root / 'home.json'
            configuration.write(path, value)
            # A dormant legacy file cannot add adapters or leak settings.
            configuration.write(root / 'meaco.json', {'connect': '127.0.0.1:1'})
            key = '-----BEGIN PRIVATE KEY-----\nsynthetic\n-----END PRIVATE KEY-----\n'
            with patch.dict('os.environ', {'HOMEWIRE_SECRET_TOKEN': 'synthetic', 'HOMEWIRE_SECRET_KEY': key}, clear=True):
                files = dict(phone.validate_configs(path))
            self.assertEqual(set(files), {'home.json', 'api-key.pem'})
            runtime = json.loads(files['home.json'])
            self.assertNotIn('deployment', runtime)
            self.assertEqual(runtime['services']['api']['dashboard_token'], 'synthetic')
            self.assertEqual(runtime['services']['api']['private_key'], 'api-key.pem')
            self.assertEqual(configuration.load(path), value)
            self.assertFalse((root / 'api-key.pem').exists())

    def test_invalid_sections_or_orphaned_automation_fail_closed(self):
        for value in ({'version': True}, {'version': 2}, {'version': 1, 'unknown': {}},
                      {'version': 1, 'adapters': {'api': {}}},
                      {'version': 1, 'automations': {'lights': {}}},
                      {'version': 1, 'routines': {'tv': {}}}):
            with self.subTest(value=value), self.assertRaises(ValueError): configuration.validate(value)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'home.json'
            path.write_text('{"version":1}')
            with self.assertRaisesRegex(ValueError, 'chmod 600'): configuration.load(path)
            path.chmod(0o600)
            path.write_bytes(b' ' * 65537)
            with self.assertRaises(ValueError): configuration.load(path)

    def test_profile_uses_unified_source_and_api_updates_preserve_other_sections(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'home.json'
            profile = fixture()
            profile['network']['dns_servers'] = ['192.168.20.53']
            value = {'version': 1, 'deployment': profile, 'adapters': {'awair': {'host': '192.0.2.12'}},
                     'services': {'api': {'listen': '127.0.0.1:8443'}}}
            configuration.write(path, value)
            with patch.object(deployment, 'use', side_effect=lambda p: p):
                self.assertEqual(deployment.load(path).config_file, path)
            class Fake:
                def sh(self, command): return json.dumps(value).encode()
                def put(self, target, data): self.target, self.data = target, data
            p = Fake()
            target, document, api, raw = phone.read_api(p)
            api['agent'] = {'tools': ['entity_list']}
            phone.write_api(p, target, document)
            changed = json.loads(p.data)
            self.assertTrue(target.endswith('/home.json'))
            self.assertEqual(changed['adapters'], value['adapters'])
            self.assertEqual(changed['services']['api']['agent']['tools'], ['entity_list'])

    def test_import_preserves_installed_scoped_endpoint_without_changing_host_settings(self):
        installed = {'listen': '127.0.0.1:8443', 'agent': {'listen': '127.0.0.1:43002',
                     'token': 'synthetic', 'tools': ['entity_list']}}
        class Fake:
            def sh(self, command):
                return json.dumps(installed).encode() if 'then cat ' in command else b'yes'
        value = {'version': 1, 'adapters': {'awair': {'host': '192.0.2.12'}},
                 'services': {'api': {'listen': '127.0.0.1:8443'}}}
        files = phone.preserve_agent_endpoint(Fake(), [('home.json', json.dumps(value).encode())])
        merged = json.loads(files[0][1])
        self.assertEqual(merged['services']['api']['agent'], installed['agent'])
        self.assertEqual(merged['adapters'], value['adapters'])
        self.assertNotIn('agent', value['services']['api'])

    def test_launcher_uses_unified_config_and_keeps_saved_runtime_grants(self):
        script = phone.service_script()
        subprocess.run(['sh', '-n'], input=script.encode(), check=True, capture_output=True)
        self.assertIn('set -- --config "$root/config/home.json"', script)
        self.assertIn('else\nfor pair', script)
        self.assertIn('done < "$root/service.args"', script)
