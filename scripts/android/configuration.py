"""Private unified configuration: host-side validation, projection and migration."""
import argparse
import json
import os
import re
from pathlib import Path
import shutil
import time

LIMIT = 65536
ADAPTERS = ('meaco', 'printers', 'awair', 'lights', 'plugs', 'samsung', 'digi',
            'router', 'brother', 'cast', 'dlna')
SERVICES = ('alerts', 'history', 'mobile', 'weather', 'shopping', 'radio', 'speech', 'api')


def validate(value):
    if not isinstance(value, dict) or value.get('version') != 1 or type(value['version']) is not int:
        raise ValueError('Homewire configuration requires version 1')
    if set(value) - {'version', 'adapters', 'routines', 'automations', 'services', 'deployment'}:
        raise ValueError('Unknown unified configuration section')
    for section in ('adapters', 'services', 'routines', 'automations'):
        entries = value.get(section, {})
        if not isinstance(entries, dict) or any(not isinstance(v, dict) for v in entries.values()):
            raise ValueError('Invalid unified section: ' + section)
        if any(not isinstance(k, str) or not re.fullmatch(r'[a-z][a-z0-9_]{0,31}', k) for k in entries):
            raise ValueError('Invalid configuration family name')
        # Runtime catalog validation is authoritative for compiled families.
        # New hardware/services need no second host-side registration.
        if section in ('adapters', 'services'):
            misplaced = set(entries) & set(SERVICES if section == 'adapters' else ADAPTERS)
        else:
            misplaced = set(entries) - set(('meaco', 'tv') if section == 'routines' else ('meaco', 'lights'))
        if misplaced:
            raise ValueError('Misplaced family or unknown routine/automation')
    if 'deployment' in value and not isinstance(value['deployment'], dict):
        raise ValueError('Deployment must be an object')
    adapters = value.get('adapters', {})
    for section, family, field in (('routines', 'meaco', 'presets'),
                                   ('automations', 'meaco', 'schedule'),
                                   ('automations', 'lights', 'automation')):
        if family in value.get(section, {}):
            if family not in adapters or field in adapters[family]:
                raise ValueError('Orphaned or duplicate routine/automation')
    if 'tv' in value.get('routines', {}) and not {'samsung', 'digi', 'plugs'} <= set(adapters):
        raise ValueError('TV requires Samsung, DIGI and plugs')
    return value


def load(path):
    path = Path(path)
    if path.stat().st_mode & 0o077:
        raise ValueError('Private configuration requires chmod 600')
    with path.open('rb') as source:
        raw = source.read(LIMIT + 1)
    if len(raw) > LIMIT:
        raise ValueError('Configuration exceeds 65536 bytes')
    try:
        return json.loads(raw)
    except (ValueError, UnicodeError):
        raise ValueError('Invalid private JSON configuration') from None


def write(path, value):
    path = Path(path)
    data = (json.dumps(value, indent=2) + '\n').encode()
    if len(data) > LIMIT:
        raise ValueError('Configuration exceeds 65536 bytes')
    temporary = path.with_name(path.name + '.new')
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, 'wb') as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def runtime(value):
    validate(value)
    return {key: item for key, item in value.items() if key != 'deployment'}


def api(value):
    return value['services']['api'] if 'version' in value else value


def migrate(directory, output):
    directory, output = Path(directory), Path(output)
    if output.exists():
        raise ValueError('Unified output already exists; refusing to replace it')
    # Keeping state next to the config preserves all existing adapter filenames.
    if output.parent.resolve() != directory.resolve():
        raise ValueError('Migrate in the existing config directory to preserve state and relative paths')
    value = {'version': 1, 'adapters': {}, 'routines': {}, 'automations': {}, 'services': {}}
    imported = []
    for section, families in (('adapters', ADAPTERS), ('services', SERVICES)):
        for family in families:
            path = directory / (family + '.json')
            if path.is_file():
                value[section][family] = load(path)
                imported.append(path)
    for section, family, field in (('routines', 'meaco', 'presets'),
                                   ('automations', 'meaco', 'schedule'),
                                   ('automations', 'lights', 'automation')):
        adapter = value['adapters'].get(family, {})
        if field in adapter:
            value[section][family] = adapter.pop(field)
    tv = directory / 'tv.json'
    if tv.is_file():
        routine = load(tv)
        for family in ('samsung', 'digi', 'plugs'):
            child = directory / routine.pop(family)
            canonical = directory / (family + '.json')
            if child.resolve() != canonical.resolve() or family not in value['adapters']:
                raise ValueError('TV child paths must match imported adapter configs')
        value['routines']['tv'] = routine
        imported.append(tv)
    profile = directory / 'deployment.json'
    if profile.is_file():
        import deployment
        value['deployment'] = deployment.Profile(load(profile)).value
        imported.append(profile)
    if not value['adapters'] and not value['services']:
        raise ValueError('No configurations to migrate')
    validate(value)
    backup = directory / 'backups' / ('before-unified-' + str(time.time_ns()))
    backup.mkdir(parents=True, mode=0o700)
    os.chmod(backup.parent, 0o700)
    for path in imported:
        shutil.copy2(path, backup / path.name)
    write(output, value)
    # Only the imported settings are retired; state, certificates and keys stay.
    for path in imported:
        path.unlink()
    return backup


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--configs', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    backup = migrate(args.configs, args.output)
    print('Unified private configuration created; original settings backed up at ' + str(backup))
