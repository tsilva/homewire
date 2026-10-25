#!/usr/bin/env python3
"""Prepare a hash-verified ZeroClaw binary and dashboard on the host only."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import tarfile

MAX_ARCHIVE = 128 * 1024 * 1024
MAX_CONTENT = 128 * 1024 * 1024
MAX_FILES = 2048


def safe_name(name):
    path = PurePosixPath(name)
    if (path.is_absolute() or str(path) != name or any(p in ('', '.', '..') for p in path.parts)
            or not all(re.fullmatch(r'[A-Za-z0-9_.-]{1,160}', p) for p in path.parts)):
        raise ValueError('Unsafe release archive path')
    return path


def dashboard_files(directory):
    directory = Path(directory)
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError('Supply the verified package web directory before installing a new sandbox')
    files = []
    total = 0
    for path in sorted(directory.rglob('*')):
        if path.is_symlink():
            raise ValueError('Dashboard symlinks are refused')
        relative = path.relative_to(directory).as_posix()
        safe_name(relative)
        if path.is_dir():
            continue
        if not path.is_file():
            raise ValueError('Dashboard must contain regular files only')
        if path.stat().st_size > MAX_CONTENT - total:
            raise ValueError('Dashboard exceeds package limits')
        data = path.read_bytes()
        total += len(data)
        if total > MAX_CONTENT or len(files) >= MAX_FILES:
            raise ValueError('Dashboard exceeds package limits')
        files.append((relative, data))
    if not any(name == 'index.html' and data for name, data in files):
        raise ValueError('Dashboard index.html is missing or empty')
    return files


def prepare(archive, expected_sha256, output):
    archive, output = Path(archive), Path(output)
    if not re.fullmatch(r'[0-9a-f]{64}', expected_sha256):
        raise ValueError('Supply the pinned release SHA-256')
    if archive.is_symlink() or not archive.is_file() or archive.stat().st_size > MAX_ARCHIVE:
        raise ValueError('Release archive must be a bounded regular file')
    if hashlib.sha256(archive.read_bytes()).hexdigest() != expected_sha256:
        raise ValueError('Release archive hash mismatch')
    if output.exists() or output.is_symlink():
        raise ValueError('Output must be a fresh directory')
    content = {}
    seen = set()
    total = 0
    with tarfile.open(archive, 'r:gz') as tar:
        for member in tar:
            name = safe_name(member.name)
            if member.name in seen:
                raise ValueError('Duplicate release archive entry')
            seen.add(member.name)
            if len(seen) > MAX_FILES:
                raise ValueError('Release archive exceeds entry limit')
            if member.isdir():
                continue
            if not member.isfile() or member.size < 0:
                raise ValueError('Release links and special files are refused')
            total += member.size
            if total > MAX_CONTENT:
                raise ValueError('Release archive exceeds content limit')
            if name in {PurePosixPath(n) for n in ('zeroclaw', 'zerocode', 'zerorelay')}:
                target = 'bin/' + name.name
            elif name.parts[:2] == ('web', 'dist') and len(name.parts) > 2:
                target = 'web/' + '/'.join(name.parts[2:])
            else:
                raise ValueError('Unexpected release archive payload')
            content[target] = tar.extractfile(member).read()
    if 'bin/zeroclaw' not in content or not content.get('web/index.html'):
        raise ValueError('Release must include the executable and complete dashboard')
    output.mkdir(mode=0o700, parents=True)
    for name, data in content.items():
        path = output / name
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        path.write_bytes(data)
        path.chmod(0o700 if name == 'bin/zeroclaw' else 0o600)
    manifest = {'archive_sha256': expected_sha256,
                'files': {name: hashlib.sha256(data).hexdigest() for name, data in sorted(content.items())}}
    path = output / 'manifest.json'
    path.write_text(json.dumps(manifest, indent=2) + '\n')
    path.chmod(0o600)
    return manifest


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', type=Path, required=True)
    parser.add_argument('--sha256', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        result = prepare(args.archive, args.sha256, args.output)
    except (OSError, ValueError, tarfile.TarError):
        parser.exit(1, 'Package preparation failed; check the pinned archive and fresh output directory\n')
    print(f'Prepared {len(result["files"])} verified files locally; no device contacted.')
