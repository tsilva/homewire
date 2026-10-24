"""Exercise privacy hooks with synthetic disposable Git repositories only."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parents[1] / 'privacy_guard.py'
ZERO = '0' * 40


class PrivacyGuardTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.repo = self.base / 'repo'
        self.repo.mkdir()
        self.environment = dict(os.environ, GIT_CONFIG_NOSYSTEM='1',
                                GIT_CONFIG_GLOBAL=os.devnull,
                                GIT_AUTHOR_NAME='Synthetic Test',
                                GIT_AUTHOR_EMAIL='synthetic@example.invalid',
                                GIT_COMMITTER_NAME='Synthetic Test',
                                GIT_COMMITTER_EMAIL='synthetic@example.invalid')
        for name in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE',
                     'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES'):
            self.environment.pop(name, None)
        self.git('init', '-q', '--object-format=sha1', '--initial-branch=main')
        self.file = self.repo / 'fixture.txt'
        self.file.write_text('synthetic original content\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'Synthetic original')
        self.retired = self.git('rev-parse', 'HEAD')
        self.retired_blob = self.git('rev-parse', 'HEAD:fixture.txt')
        self.file.write_text('synthetic replacement content\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'Synthetic replacement')
        self.tip = self.git('rev-parse', 'HEAD')
        self.script = self.base / 'guard.py'
        shutil.copyfile(SOURCE, self.script)
        self.manifest = self.base / 'homewire-sensitive-history.txt'
        self.manifest.write_text(self.retired + '\n' + self.retired_blob + '\n')

    def run_command(self, command, input=None, cwd=None):
        return subprocess.run(command, cwd=cwd or self.repo, env=self.environment,
                              input=input, capture_output=True, text=True)

    def git(self, *arguments, input=None):
        result = self.run_command(['git', *arguments], input=input)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def guard(self, command, input=None):
        return self.run_command([sys.executable, str(self.script), command], input=input)

    def push_input(self, tip=None, remote=ZERO):
        return f'refs/heads/main {tip or self.tip} refs/heads/main {remote}\n'

    def assert_blocked(self, result):
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('privacy guard:', result.stderr)
        self.assertNotIn('synthetic original content', result.stderr)

    def test_ancestor_and_deleted_blob_block_new_existing_and_force_pushes(self):
        for remote in (ZERO, self.retired, self.tip):
            with self.subTest(remote=remote):
                self.assert_blocked(self.guard('pre-push', self.push_input(remote=remote)))
        # The old file is gone from the tip but remains in its history.
        self.manifest.write_text(self.retired_blob + '\n')
        self.assert_blocked(self.guard('pre-push', self.push_input()))

    def test_annotated_tag_and_direct_tree_or_blob_are_checked(self):
        self.git('tag', '-a', 'synthetic', '-m', 'Synthetic tag')
        tag = self.git('rev-parse', 'refs/tags/synthetic')
        self.assert_blocked(self.guard('pre-push', self.push_input(tip=tag)))
        tree = self.git('rev-parse', self.retired + '^{tree}')
        for tip in (tree, self.retired_blob):
            with self.subTest(tip=tip):
                self.assert_blocked(self.guard('pre-push', self.push_input(tip=tip)))

    def test_merge_parent_and_copied_blob_in_unrelated_history_are_checked(self):
        tree = self.git('rev-parse', 'HEAD^{tree}')
        unrelated = self.git('commit-tree', tree, input='Synthetic clean root\n')
        merge = self.git('commit-tree', tree, '-p', unrelated, '-p', self.retired,
                         input='Synthetic merge\n')
        self.assert_blocked(self.guard('pre-push', self.push_input(tip=merge)))
        old_tree = self.git('rev-parse', self.retired + '^{tree}')
        copied = self.git('commit-tree', old_tree, input='Synthetic copied file\n')
        self.assert_blocked(self.guard('pre-push', self.push_input(tip=copied)))

    def test_safe_history_and_deletions_pass_and_all_updates_are_checked(self):
        tree = self.git('rev-parse', 'HEAD^{tree}')
        safe = self.git('commit-tree', tree, input='Synthetic clean root\n')
        self.assertEqual(self.guard('pre-push', self.push_input(tip=safe)).returncode, 0)
        deletion = f'(delete) {ZERO} refs/heads/retired {self.retired}\n'
        self.assertEqual(self.guard('pre-push', deletion).returncode, 0)
        self.assertEqual(self.guard('pre-push', '').returncode, 0)
        self.assert_blocked(self.guard('pre-push', self.push_input(tip=safe)
                                       + self.push_input()))

    def test_missing_object_invalid_input_and_shallow_history_fail_closed(self):
        self.assert_blocked(self.guard('pre-push', 'malformed\n'))
        self.assert_blocked(self.guard('pre-push', self.push_input(tip='f' * 40)))
        (self.repo / '.git/shallow').write_text(self.tip + '\n')
        self.assert_blocked(self.guard('pre-push', self.push_input()))

    def test_replace_refs_and_grafts_cannot_hide_retired_ancestors(self):
        tree = self.git('rev-parse', 'HEAD^{tree}')
        safe = self.git('commit-tree', tree, input='Synthetic clean root\n')
        self.git('replace', self.tip, safe)
        self.assert_blocked(self.guard('pre-push', self.push_input()))
        self.git('replace', '-d', self.tip)
        (self.repo / '.git/info/grafts').write_text(self.tip + '\n')
        self.assert_blocked(self.guard('pre-push', self.push_input()))

    def test_empty_corrupt_and_missing_denylist_fail_closed(self):
        for content in ('', 'not-an-object-id\n'):
            self.manifest.write_text(content)
            self.assert_blocked(self.guard('pre-push', self.push_input()))
        self.manifest.unlink()
        self.assert_blocked(self.guard('pre-push', self.push_input()))

    def test_staged_check_reads_index_and_blocks_force_added_private_paths(self):
        self.assertEqual(self.guard('staged').returncode, 0)
        self.file.write_text('synthetic original content\n')
        self.git('add', 'fixture.txt')
        self.file.write_text('synthetic replacement content\n')
        self.assert_blocked(self.guard('staged'))
        self.git('reset', '-q', 'HEAD', '--', 'fixture.txt')
        for name in ('.local/profile.json', '.access.local.toml', '.infisical.json',
                     '.env', 'nested/.env.production'):
            with self.subTest(name=name):
                path = self.repo / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('synthetic placeholder\n')
                self.git('add', '-f', '--', name)
                self.assert_blocked(self.guard('staged'))
                self.git('reset', '-q', 'HEAD', '--', name)
        (self.repo / '.env.example').write_text('SYNTHETIC=REPLACE\n')
        self.git('add', '.env.example')
        self.assertEqual(self.guard('staged').returncode, 0)

    def test_installer_preserves_foreign_hooks_and_custom_hook_paths(self):
        hook = self.repo / '.git/hooks/pre-push'
        hook.write_text('#!/bin/sh\nexit 0\n')
        self.assert_blocked(self.guard('install'))
        self.assertEqual(hook.read_text(), '#!/bin/sh\nexit 0\n')
        self.assertFalse((hook.parent / 'pre-commit').exists())
        hook.unlink()
        self.git('config', 'core.hooksPath', str(self.base / 'custom-hooks'))
        self.assert_blocked(self.guard('install'))

    def test_installed_hooks_reject_real_push_and_commit_without_tracked_script(self):
        self.assertEqual(self.guard('install').returncode, 0)
        self.assertEqual(self.guard('install').returncode, 0)
        # Installed copies survive removing the source script and its manifest.
        self.script.unlink()
        self.manifest.unlink()
        remote = self.base / 'remote.git'
        self.git('init', '-q', '--bare', '--initial-branch=main', str(remote))
        push = self.run_command(['git', 'push', str(remote), 'HEAD:refs/heads/main'])
        self.assert_blocked(push)
        remote_refs = self.run_command(['git', '--git-dir=' + str(remote),
                                        'for-each-ref', '--format=%(refname)'])
        self.assertEqual(remote_refs.stdout, '')
        safe = self.git('commit-tree', self.git('rev-parse', 'HEAD^{tree}'),
                        input='Synthetic publishable root\n')
        clean_push = self.run_command(['git', 'push', str(remote),
                                        safe + ':refs/heads/safe'])
        self.assertEqual(clean_push.returncode, 0, clean_push.stderr)
        self.file.write_text('synthetic original content\n')
        self.git('add', 'fixture.txt')
        self.assert_blocked(self.run_command(['git', 'commit', '-qm', 'Synthetic blocked']))
        self.assertEqual(self.git('rev-parse', 'HEAD'), self.tip)


if __name__ == '__main__':
    unittest.main()
