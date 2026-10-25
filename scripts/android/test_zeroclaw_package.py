import hashlib
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import isolate
import zeroclaw_package as package


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def archive(self, entries):
        path = self.root / 'release.tar.gz'
        with tarfile.open(path, 'w:gz') as tar:
            for name, data in entries:
                info = tarfile.TarInfo(name)
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
        return path, hashlib.sha256(path.read_bytes()).hexdigest()

    def test_binary_and_dashboard_are_preserved(self):
        path, digest = self.archive([('zeroclaw', b'synthetic executable'), ('web/dist/index.html', b'<html>'),
                                     ('web/dist/assets/app.js', b'console.log(1)')])
        out = self.root / 'prepared'
        manifest = package.prepare(path, digest, out)
        self.assertEqual(len(manifest['files']), 3)
        files = package.dashboard_files(out / 'web')
        self.assertEqual({name for name, _ in files}, {'index.html', 'assets/app.js'})
        with self.assertRaises(ValueError):
            package.prepare(path, digest, out)

    def test_hash_mismatch_missing_dashboard_and_traversal_fail_before_output(self):
        for entries in [[('zeroclaw', b'elf')], [('zeroclaw', b'elf'), ('../escape', b'x')]]:
            path, digest = self.archive(entries)
            with self.assertRaises(ValueError):
                package.prepare(path, digest, self.root / 'prepared')
            self.assertFalse((self.root / 'prepared').exists())
        with self.assertRaises(ValueError):
            package.prepare(path, '0' * 64, self.root / 'prepared')

    def test_symlink_dashboard_is_refused(self):
        directory = self.root / 'web'
        directory.mkdir()
        (directory / 'index.html').symlink_to(self.root / 'outside')
        with self.assertRaises(ValueError):
            package.dashboard_files(directory)

    def test_missing_assets_prevent_any_phone_access(self):
        with patch.object(isolate.phone, 'Phone') as phone:
            with self.assertRaises(ValueError):
                isolate.install('synthetic', self.root / 'absent')
            phone.assert_not_called()

    def test_missing_native_artifacts_prevent_discovery_or_phone_access(self):
        web = self.root / 'web'
        web.mkdir()
        (web / 'index.html').write_text('synthetic dashboard')
        with patch.object(isolate.phone, 'Phone') as phone, patch.object(isolate.deployment, 'root_executable') as discovery:
            with self.assertRaises(ValueError):
                isolate.install('synthetic', self.root / 'missing-native', web)
            phone.assert_not_called()
            discovery.assert_not_called()

    def test_transfer_is_complete_and_root_read_only(self):
        files = [('index.html', b'html'), ('assets/app.js', b'javascript')]
        with patch.object(isolate.direct_boot, 'root_sh') as root, patch.object(isolate, 'root_put') as put:
            isolate.install_dashboard('synthetic', files)
            self.assertEqual(put.call_count, 2)
            self.assertTrue(all(call.args[-1] == '644' for call in put.call_args_list))
            self.assertIn('/root/bin/web/assets', root.call_args.args[1])


if __name__ == '__main__':
    unittest.main()
