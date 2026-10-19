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

    def test_tcl_key_projection_and_certificate_stay_private(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            value = {'version': 1, 'adapters': {'tcl': {
                'host': '127.0.0.1', 'model': 'Smart TV',
                'client_cert': 'tcl-cert.pem',
                'client_key': 'env:HOMEWIRE_SECRET_TCL_KEY'}}}
            path = root / 'home.json'
            configuration.write(path, value)
            cert = root / 'tcl-cert.pem'
            cert.write_text('synthetic certificate')
            cert.chmod(0o600)
            key = '-----BEGIN PRIVATE KEY-----\nsynthetic\n-----END PRIVATE KEY-----\n'
            with patch.dict('os.environ', {'HOMEWIRE_SECRET_TCL_KEY': key}, clear=True):
                files = dict(phone.validate_configs(path))
            self.assertEqual(set(files), {'home.json', 'tcl-cert.pem', 'tcl-key.pem'})
            self.assertEqual(json.loads(files['home.json'])['adapters']['tcl']['client_key'], 'tcl-key.pem')
            self.assertEqual(files['tcl-key.pem'], key.encode())
            self.assertFalse((root / 'tcl-key.pem').exists())

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
        self.assertIn('done < "$root/service.args"', script)

    def test_launcher_selects_unified_or_legacy_inputs_without_duplicate_tv_children(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            configs = root / 'config'
            configs.mkdir()
            (root / 'current').mkdir()
            executable = root / 'current' / 'homewire'
            executable.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
            executable.chmod(0o700)
            (root / 'service.args').write_text('--allow-control\n--watch-printers\n')
            for name in ('meaco', 'printers', 'lights', 'wol', 'computer', 'samsung', 'digi', 'plugs'):
                (configs / (name + '.json')).write_text('{}')
            script = phone.service_script(str(root))
            def launch():
                return subprocess.run(['sh'], input=script, text=True, check=True,
                                      capture_output=True).stdout.splitlines()
            args = launch()
            for name in ('printers', 'lights', 'wol', 'computer', 'samsung', 'digi', 'plugs'):
                self.assertIn(name + '=' + str(configs / (name + '.json')), args)
            self.assertIn('--allow-control', args)
            self.assertIn('--watch-printers', args)
            self.assertEqual(args[-2:], ['daemon', str(root / 'run' / 'homewire.sock')])
            (configs / 'tv.json').write_text('{}')
            args = launch()
            self.assertIn('--tv', args)
            for name in ('samsung', 'digi', 'plugs'):
                self.assertNotIn(name + '=' + str(configs / (name + '.json')), args)
            (configs / 'home.json').write_text('{}')
            self.assertEqual(launch(), ['--config', str(configs / 'home.json'),
                                       '--allow-control', '--watch-printers',
                                       'daemon', str(root / 'run' / 'homewire.sock')])

    def test_migration_includes_wake_targets_and_computer_routine(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            inputs = {'wol': {'devices': [{'name': 'workstation'}]},
                      'computer': {'name': 'workstation'}, 'plugs': {'plugs': []},
                      'computer-routine': {'computer': 'workstation', 'plug': 'outlet'}}
            for name, value in inputs.items():
                configuration.write(root / (name + '.json'), value)
            backup = configuration.migrate(root, root / 'home.json')
            value = configuration.load(root / 'home.json')
            for name in ('wol', 'computer', 'plugs'):
                self.assertEqual(value['adapters'][name], inputs[name])
            self.assertEqual(value['routines']['computer'], inputs['computer-routine'])
            self.assertEqual({p.stem for p in backup.iterdir()}, set(inputs))
            self.assertFalse(any((root / (name + '.json')).exists() for name in inputs))
