#!/usr/bin/env python3
"""Homewire privacy guard: local hooks, without dependencies or network access."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys


MANIFEST = 'homewire-sensitive-history.txt'
OBJECT_ID = re.compile(r'(?:[0-9a-f]{40}|[0-9a-f]{64})\Z')


class GuardError(Exception):
    pass


def git(*arguments):
    environment = dict(os.environ, GIT_NO_REPLACE_OBJECTS='1',
                       GIT_GRAFT_FILE=os.devnull, GIT_NO_LAZY_FETCH='1')
    result = subprocess.run(['git', *arguments], env=environment,
                            capture_output=True)
    if result.returncode:
        raise GuardError('Git inspection failed; publication is blocked.')
    return result.stdout


def denylist():
    try:
        lines = Path(__file__).with_name(MANIFEST).read_text().splitlines()
    except OSError as error:
        raise GuardError('Sensitive-history denylist is missing or unreadable.') from error
    entries = [line.strip() for line in lines if line.strip() and not line.startswith('#')]
    if not entries or any(not OBJECT_ID.fullmatch(entry) for entry in entries):
        raise GuardError('Sensitive-history denylist is empty or invalid.')
    return set(entries)


def reject_retired(objects, blocked):
    if set(objects) & blocked:
        raise GuardError('Retired sensitive Git objects detected. Remove the affected '
                         'history or files; do not bypass the privacy guard.')


def inspect_history(tips, blocked):
    if not tips:
        return
    reject_retired(tips, blocked)
    if git('rev-parse', '--is-shallow-repository').strip() != b'false':
        raise GuardError('A shallow repository cannot prove history is clean. '
                         'Fetch its complete sanitized history first.')
    # Inspect complete outgoing histories, including deleted files and merge parents.
    # Do not subtract remote tips: new refs, force pushes and already-published
    # ancestors must receive the same protection. Replace refs/grafts are disabled.
    environment = dict(os.environ, GIT_NO_REPLACE_OBJECTS='1',
                       GIT_GRAFT_FILE=os.devnull, GIT_NO_LAZY_FETCH='1')
    result = subprocess.run(
        ['git', 'rev-list', '--objects', '--no-object-names', '--stdin'],
        input=('\n'.join(sorted(set(tips))) + '\n').encode(),
        env=environment, capture_output=True)
    if result.returncode:
        raise GuardError('Outgoing history could not be inspected; push blocked.')
    reject_retired(result.stdout.decode().splitlines(), blocked)


def pre_push(stream, blocked):
    tips = []
    for line in stream:
        fields = line.split()
        if len(fields) != 4 or not all(OBJECT_ID.fullmatch(fields[i]) for i in (1, 3)):
            raise GuardError('Invalid pre-push input; push blocked.')
        if set(fields[1]) != {'0'}:  # A deletion sends no local history.
            tips.append(fields[1])
    inspect_history(tips, blocked)


def private_path(path):
    parts = path.lower().split('/')
    return any(part in ('.local', '.access.local.toml', '.infisical.json', '.env')
               or (part.startswith('.env.') and
                   not part.endswith(('.example', '.sample', '.template')))
               for part in parts)


def staged(blocked):
    # Read the index, never the working copy; also catches force-added ignored files.
    for entry in git('ls-files', '--stage', '-z').split(b'\0'):
        if not entry:
            continue
        metadata, path = entry.split(b'\t', 1)
        _mode, oid, stage = metadata.split()
        if stage != b'0':
            raise GuardError('Resolve index conflicts before the privacy review.')
        if private_path(os.fsdecode(path)):
            raise GuardError('A private configuration path is tracked in the index. '
                             'Unstage it and keep household configuration ignored.')
        reject_retired([oid.decode()], blocked)


def install():
    # Do not replace a user's global or custom hook setup.
    configuration = subprocess.run(['git', 'config', '--get', 'core.hooksPath'],
                                   capture_output=True)
    if configuration.returncode not in (0, 1) or configuration.stdout.strip():
        raise GuardError('A custom hooksPath is configured. Preserve it and integrate '
                         'the privacy guard into its existing hooks explicitly.')
    blocked = denylist()  # Validate everything before writing a hook.
    directory = Path(os.fsdecode(git('rev-parse', '--git-path', 'hooks')).strip()).resolve()
    source = Path(__file__).resolve()
    script = directory / 'homewire-privacy-guard.py'
    hooks = {}
    for name, command in [('pre-push', 'pre-push'), ('pre-commit', 'staged')]:
        hooks[directory / name] = (
            '#!/bin/sh\n# Homewire privacy guard\n'
            'set -eu\n'
            'exec python3 "$(dirname "$0")/homewire-privacy-guard.py" '
            + command + ' "$@"\n').encode()
    for path, content in hooks.items():
        if path.is_symlink() or (path.exists() and path.read_bytes() != content):
            raise GuardError('An existing Git hook was preserved. Integrate the '
                             'privacy guard explicitly instead of overwriting it.')
    directory.mkdir(parents=True, exist_ok=True)
    # Install a standalone copy: checking out a branch that lacks the tracked
    # guard cannot silently disable the hook. Re-run install after guard updates.
    for path, content, mode in [
            (script, source.read_bytes(), 0o755),
            (directory / MANIFEST, ('\n'.join(sorted(blocked)) + '\n').encode(), 0o644),
            *[(path, content, 0o755) for path, content in hooks.items()]]:
        temporary = path.with_name(path.name + '.tmp-' + str(os.getpid()))
        try:
            temporary.write_bytes(content)
            temporary.chmod(mode)
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)
    print('Installed Homewire pre-push and pre-commit privacy guards.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('install')
    hook = commands.add_parser('pre-push')
    hook.add_argument('remote', nargs='?')
    hook.add_argument('url', nargs='?')
    commands.add_parser('staged')
    check = commands.add_parser('check')
    check.add_argument('revisions', nargs='*', default=['HEAD'])
    arguments = parser.parse_args()
    try:
        if arguments.command == 'install':
            install()
        elif arguments.command == 'pre-push':
            pre_push(sys.stdin, denylist())
        elif arguments.command == 'staged':
            staged(denylist())
        else:
            tips = [git('rev-parse', '--verify', '--end-of-options', ref + '^{object}')
                    .decode().strip() for ref in arguments.revisions or ['HEAD']]
            inspect_history(tips, denylist())
    except (GuardError, OSError, UnicodeError, ValueError) as error:
        # Never echo Git stderr, file contents, remote URLs or private values.
        print('Homewire privacy guard: ' + (str(error) if isinstance(error, GuardError)
                                          else 'Inspection failed; operation blocked.'),
              file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
