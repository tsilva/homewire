"""Release safety gates, credential isolation and recoverable source/artifact receipts."""
import argparse
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build
import install
import release
import verify


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.profile = dict(host='192.0.2.40', port=8766, model='Fixture TV', device='fixture',
                            fingerprint='verified-build', incremental='revision', framework_sha256='a' * 64)
        install.save_json(self.root / 'profile.json', self.profile)
        install.save_json(self.root / 'companion.json', dict(port=8766, server_cert_sha256='b' * 64,
                          fingerprint='verified-build', incremental='revision', token='${HOMEWIRE_SECRET_TV_API_TOKEN}'))
        binding = dict(project_id='00000000-0000-4000-8000-000000000001', environment='prod',
                       folder='/tv/signing', domain='https://example.com')
        self.settings = dict(toolchain={}, signing=binding, runtime=dict(binding, folder='/tv/runtime'),
                             serial='fixture', profile='profile.json', companion='companion.json', artifacts='artifacts')
        self.config = self.root / 'release.json'
        install.save_json(self.config, self.settings)

    def args(self, **changes):
        return argparse.Namespace(**dict(dict(config=self.config, version='0.1.1', build_only=True,
                                              rollback=None, verify_reboot=False), **changes))

    def test_versions_order_correctly_and_reject_ambiguous_input(self):
        self.assertLess(build.version_code('0.1.999'), build.version_code('0.2.0'))
        self.assertLess(build.version_code('0.999.999'), build.version_code('1.0.0'))
        for value in ('0.0.0', '01.1.0', '1.0', '1.0.1000', '1.0.0-beta', '1.0.0\n'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                build.version_code(value)

    def test_invalid_or_public_credential_bindings_fail_closed(self):
        self.config.chmod(0o644)
        with self.assertRaises(ValueError):
            release.configuration(self.config)
        for changed in (dict(self.settings['signing'], folder='/'),
                        dict(self.settings['signing'], provider='fallback'),
                        dict(self.settings['signing'], domain='http://example.com')):
            install.save_json(self.config, dict(self.settings, signing=changed))
            with self.assertRaises(ValueError):
                release.configuration(self.config)

    def test_native_injection_is_explicit_and_output_is_not_exposed_on_failure(self):
        with patch.object(release.subprocess, 'run') as run:
            run.return_value.returncode = 1
            run.return_value.stdout = b'secret value'
            with self.assertRaisesRegex(RuntimeError, '^Signing failed') as result:
                release.injected(self.settings['signing'], ['python3', 'build.py'], {}, 'Signing')
            self.assertNotIn('secret value', str(result.exception))
            argv = run.call_args.args[0]
            for flag in ('--expand=false', '--include-imports=false', '--silent', '--telemetry=false',
                         '--path=/tv/signing', '--domain=https://example.com', '--env=prod'):
                self.assertIn(flag, argv)

    def test_signing_key_is_piped_and_never_in_child_environment_or_arguments(self):
        key = b'k' * 1200
        env = {'HOMEWIRE_TV_SIGNING_KEY_B64': base64.b64encode(key).decode(),
               'HOMEWIRE_TV_SIGNING_PASSWORD': 'p' * 32}
        def compile_apk(argv, **kwargs):
            self.assertEqual(kwargs['input'], key)
            self.assertNotIn('HOMEWIRE_TV_SIGNING_KEY_B64', kwargs['env'])
            self.assertNotIn(key.decode(), ' '.join(argv))
            self.assertEqual(kwargs['env']['HOMEWIRE_SIGNING_KEY'], '/dev/stdin')
            (Path(kwargs['env']['HOMEWIRE_TV_BUILD_DIR']) / 'homewire-tv.apk').write_bytes(b'apk')
        with patch.object(build, 'environment', return_value=env), patch.object(build.subprocess, 'run', side_effect=compile_apk):
            path = build.build('0.1.1', self.root / 'signed.apk')
            self.assertEqual(path.read_bytes(), b'apk')
            with self.assertRaises(ValueError):
                build.build('0.1.1', path)

    def test_source_extraction_rejects_traversal_and_symlinks(self):
        for name, kind in (('../outside', tarfile.REGTYPE), ('link', tarfile.SYMTYPE)):
            archive = self.root / 'bad.tar.gz'
            with tarfile.open(archive, 'w:gz') as out:
                member = tarfile.TarInfo(name)
                member.type = kind
                out.addfile(member, io.BytesIO())
            with self.assertRaises(ValueError):
                release.extract(archive, self.root / 'source')

    def signed_build(self, binding, command, env, gate):
        self.assertEqual(binding['folder'], '/tv/signing')
        Path(command[command.index('--output') + 1]).write_bytes(b'signed fixture')

    def test_build_only_archives_source_and_never_calls_adb_or_runtime_injection(self):
        with patch.object(release.shutil, 'which', return_value='/fixture/tool'), \
                patch.object(build, 'environment', return_value={'HOMEWIRE_ANDROID_BUILD_TOOLS': '/fixture/tools'}), patch.object(build, 'apk_identity', return_value={
                'version': '0.1.1', 'version_code': 1001, 'signer_sha256': 'c' * 64}), \
                patch.object(release, 'checks'), patch.object(release, 'run', return_value=b'commit'), \
                patch.object(release, 'injected', side_effect=self.signed_build) as injected, \
                patch.object(install, 'adb') as adb:
            directory = release.release(self.args())
            receipt = release.private_json(directory / 'receipt.json')
            self.assertEqual(receipt['phase'], 'built')
            self.assertEqual(receipt['source_archive_sha256'], release.digest(directory / 'source.tar.gz'))
            self.assertEqual(receipt['apk_sha256'], hashlib.sha256(b'signed fixture').hexdigest())
            self.assertTrue((directory / 'source/src/eu/tsilva/homewire/tv/ApiServer.java').exists())
            self.assertEqual(injected.call_count, 1)
            adb.assert_not_called()
            with self.assertRaises(FileExistsError):
                release.release(self.args())

    def test_failed_build_persists_gate_and_never_attempts_install(self):
        with patch.object(release.shutil, 'which', return_value='/fixture/tool'), \
                patch.object(build, 'environment', return_value={'HOMEWIRE_ANDROID_BUILD_TOOLS': '/fixture/tools'}), patch.object(release, 'checks'), \
                patch.object(release, 'run', return_value=b'commit'), \
                patch.object(release, 'injected', side_effect=RuntimeError('signing failed')) as injected:
            with self.assertRaises(RuntimeError):
                release.release(self.args(build_only=False))
            receipt = release.private_json(self.root / 'artifacts/0.1.1/receipt.json')
            self.assertEqual(receipt['phase'], 'failed')
            self.assertEqual(receipt['failed_gate'], 'building')
            self.assertEqual(injected.call_count, 1)

    def test_certificate_mismatch_sends_no_credentials(self):
        with patch.object(verify.socket, 'create_connection'), patch.object(verify.ssl, 'SSLContext') as context:
            tls = context.return_value.wrap_socket.return_value.__enter__.return_value
            tls.getpeercert.return_value = b'unexpected certificate'
            with self.assertRaises(verify.IdentityError):
                verify.request('192.0.2.40', 8766, '0' * 64, 'secret')
            tls.sendall.assert_not_called()

    def test_wrong_api_identity_and_failed_auth_rejection_fail_acceptance(self):
        good = dict(protocol_version=1, manufacturer='TCL', backlight_level=50,
                    **{k: self.profile[k] for k in ('model', 'device', 'fingerprint', 'incremental')})
        config = {'server_cert_sha256': 'b' * 64}
        with patch.dict(os.environ, HOMEWIRE_SECRET_TV_API_TOKEN='a' * 64):
            with patch.object(verify, 'request', return_value=(200, dict(good, device='other'))):
                with self.assertRaises(verify.IdentityError):
                    verify.verify(self.profile, config)
            with patch.object(verify, 'request', side_effect=[(200, good), (200, good)]):
                with self.assertRaisesRegex(ValueError, 'rejection'):
                    verify.verify(self.profile, config)
            with patch.object(verify, 'request', side_effect=[(200, good), (401, {}), (401, {})]):
                self.assertTrue(verify.verify(self.profile, config)['pinned_https'])

    def test_temporary_route_uses_existing_master_and_cancels_only_its_forwards(self):
        settings = release.configuration(self.config)
        settings.update(ssh_alias='fixture-phone', serial=self.profile['host'] + ':5555')
        with patch.object(release, 'run') as run, patch.object(release.subprocess, 'run') as cleanup:
            cleanup.return_value.returncode = 0
            with release.route(self.config, settings, {}) as (path, forwarded):
                self.assertTrue(path.exists())
                self.assertTrue(forwarded['serial'].startswith('127.0.0.1:'))
                self.assertEqual(forwarded['verify_connect'][0], '127.0.0.1')
                self.assertNotIn('ssh_alias', forwarded)
            self.assertFalse(path.exists())
            self.assertEqual(run.call_args_list[0].args[0], ['ssh', '-O', 'check', 'fixture-phone'])
            self.assertEqual(run.call_args_list[1].args[0][:3], ['ssh', '-O', 'forward'])
            self.assertEqual(cleanup.call_args_list[-1].args[0][:3], ['ssh', '-O', 'cancel'])

    def test_changed_saved_candidate_never_reaches_deployment(self):
        directory = self.root / 'candidate'
        directory.mkdir()
        (directory / 'source.tar.gz').write_bytes(b'source')
        (directory / 'homewire-tv.apk').write_bytes(b'changed')
        install.save_json(directory / 'receipt.json', dict(phase='built', source_archive_sha256=release.digest(directory / 'source.tar.gz'),
                                                         apk_sha256='0' * 64))
        args = self.args(deploy_existing=directory)
        with patch.object(build, 'environment', return_value={}), patch.object(release, 'deploy') as deploy:
            with self.assertRaises(ValueError):
                release.deploy_existing(args)
            deploy.assert_not_called()

    def test_recovery_build_uses_saved_source_and_requires_intact_archive(self):
        with patch.object(release.shutil, 'which', return_value='/fixture/tool'), \
                patch.object(build, 'environment', return_value={'HOMEWIRE_ANDROID_BUILD_TOOLS': '/fixture/tools'}), \
                patch.object(build, 'apk_identity', side_effect=[
                    dict(version='0.1.1', version_code=1001, signer_sha256='c' * 64),
                    dict(version='0.1.2', version_code=1002, signer_sha256='c' * 64)]), \
                patch.object(release, 'checks'), patch.object(release, 'run', return_value=b'commit'), \
                patch.object(release, 'injected', side_effect=self.signed_build):
            original = release.release(self.args())
            recovered = release.release(self.args(version='0.1.2', rollback=original))
            a = release.private_json(original / 'receipt.json')
            b = release.private_json(recovered / 'receipt.json')
            self.assertEqual(a['source_archive_sha256'], b['source_archive_sha256'])
            self.assertEqual(b['recovery_from'], str(original))
            (original / 'source.tar.gz').write_bytes(b'tampered')
            with self.assertRaises(ValueError):
                release.release(self.args(version='0.1.3', rollback=original))

    def test_reboot_failure_never_reboots_twice_or_opens_activity(self):
        settings = release.configuration(self.config)
        receipt = {}
        with patch.object(install, 'adb', side_effect=['00000000-0000-4000-8000-000000000001', '', RuntimeError('offline')]) as adb, \
                patch.object(release.time, 'sleep'), \
                patch.object(release.time, 'monotonic', side_effect=[0, 1, 181]):
            with self.assertRaises(RuntimeError):
                release.reboot_check(settings, receipt)
            self.assertEqual(sum(call.args[1] == 'reboot' for call in adb.call_args_list), 1)
            self.assertFalse(any('start' in call.args for call in adb.call_args_list))

    def test_apk_identity_uses_the_selected_jdk_environment(self):
        from types import SimpleNamespace
        responses = [SimpleNamespace(stdout="package: name='eu.tsilva.homewire.tv' versionCode='1001' versionName='0.1.1'"),
                     SimpleNamespace(stdout='Signer #1 certificate SHA-256 digest: ' + 'a' * 64)]
        env = {'JAVA_HOME': '/fixture/jdk17'}
        with patch.object(build.subprocess, 'run', side_effect=responses) as run:
            self.assertEqual(build.apk_identity(self.root / 'apk', '/fixture/tools', env)['version_code'], 1001)
            self.assertTrue(all(call.kwargs['env'] is env for call in run.call_args_list))

    def test_tcp_adb_connects_once_and_waits_only_for_readiness(self):
        from types import SimpleNamespace
        responses = [SimpleNamespace(returncode=1, stdout=b'', stderr=b'offline'),
                     SimpleNamespace(returncode=0, stdout=b'device\n', stderr=b'')]
        with patch.object(release, 'run') as connect, \
                patch.object(release.subprocess, 'run', side_effect=responses) as state, \
                patch.object(release.time, 'sleep'):
            release.connect_adb('127.0.0.1:12345', {})
            self.assertEqual(connect.call_count, 1)
            self.assertEqual(state.call_count, 2)


if __name__ == '__main__':
    unittest.main()
