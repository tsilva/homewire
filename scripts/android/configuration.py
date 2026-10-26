"""Private unified configuration: host-side validation, projection and migration."""
import argparse
import json
import os
import re
from pathlib import Path
import shutil
import time

LIMIT = 65536
from catalog import ADAPTERS, SERVICES


def validate(value):
    if not isinstance(value, dict) or value.get('version') != 1 or type(value['version']) is not int:
        raise ValueError('Homewire configuration requires version 1')
    if set(value) - {'version', 'adapters', 'routines', 'automations', 'services', 'deployment', 'groups'}:
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
            misplaced = set(entries) - set(('meaco', 'tv', 'computer', 'leave_house', 'sleep', 'wake_up') if section == 'routines' else ('meaco', 'lights'))
        if misplaced:
            raise ValueError('Misplaced family or unknown routine/automation')
    if 'deployment' in value and not isinstance(value['deployment'], dict):
        raise ValueError('Deployment must be an object')
    groups = value.get('groups', {})
    validate_groups(groups)
    adapters = value.get('adapters', {})
    for section, family, field in (('routines', 'meaco', 'presets'),
                                   ('automations', 'meaco', 'schedule'),
                                   ('automations', 'lights', 'automation')):
        if family in value.get(section, {}):
            if family not in adapters or field in adapters[family]:
                raise ValueError('Orphaned or duplicate routine/automation')
    if 'tv' in value.get('routines', {}):
        required = {'tcl', 'plugs'} if value['routines']['tv'].get('target') == 'tcl' else {'samsung', 'digi', 'plugs'}
        if not required <= set(adapters):
            raise ValueError('TV requires its configured media adapters and plugs')
    if 'computer' in value.get('routines', {}) and not {'computer', 'plugs'} <= set(adapters):
        raise ValueError('Computer routine requires computer and plugs')
    for name in ('leave_house', 'sleep'):
        if name in value.get('routines', {}):
            validate_shutdown(name, value['routines'][name], value.get('routines', {}), adapters, groups)
    if 'wake_up' in value.get('routines', {}):
        validate_startup(value['routines']['wake_up'], value['routines'])
    return value


def validate_startup(settings, routines):
    if set(settings) != {'steps', 'excluded_entities'} or any(
            not isinstance(settings[key], list) or not 1 <= len(settings[key]) <= 16
            for key in ('steps', 'excluded_entities')):
        raise ValueError('wake_up requires 1–16 explicit steps and excluded entities')
    if any(not isinstance(entity, str) or not 1 <= len(entity) <= 96
           for entity in settings['excluded_entities']):
        raise ValueError('Invalid startup exclusion')
    for step in settings['steps']:
        if not isinstance(step, dict):
            raise ValueError('Invalid startup step')
        if step.get('action') == 'power_on' and set(step) == {'action', 'entity'}:
            if not isinstance(step['entity'], str) or not 1 <= len(step['entity']) <= 96:
                raise ValueError('Invalid startup entity')
        elif step.get('action') == 'routine' and set(step) == {'action', 'name'}:
            name = step['name']
            if name not in ('tv_on', 'computer_on') or name.removesuffix('_on') not in routines:
                raise ValueError('Missing or unsupported startup child routine')
            if name == 'tv_on' and routines['tv'].get('target') != 'tcl':
                raise ValueError('Startup list requires configured TCL routine')
        else:
            raise ValueError('Only power_on and configured startup child routines are allowed')


def validate_groups(groups):
    if not isinstance(groups, dict) or len(groups) > 8:
        raise ValueError('Configure at most eight explicit device groups')
    for name, group in groups.items():
        if (not isinstance(name, str) or not re.fullmatch(r'[A-Za-z0-9_-]{1,32}', name)
                or not isinstance(group, dict) or set(group) != {'members'}
                or not isinstance(group['members'], list) or not 1 <= len(group['members']) <= 8
                or any(not isinstance(member, str) or not member.startswith('light.') or not 1 <= len(member) <= 96 for member in group['members'])
                or len(set(group['members'])) != len(group['members'])):
            raise ValueError('Device groups require 1–8 distinct explicit light entities')


def validate_shutdown(name, settings, routines, adapters, groups=None):
    allowed = {'steps', 'optional_pc', 'optional_kitchen'}
    if set(settings) - allowed or not settings.get('steps') or any(
            not isinstance(settings.get(key, []), list) for key in allowed):
        raise ValueError('Invalid explicit shutdown lists')
    total = sum(len(settings.get(key, [])) for key in allowed)
    if total > 16 or name == 'sleep' and (settings.get('optional_pc') or settings.get('optional_kitchen')):
        raise ValueError('Shutdown list exceeds 16 steps')
    for group in allowed:
        for step in settings.get(group, []):
            if not isinstance(step, dict):
                raise ValueError('Invalid shutdown step')
            action = step.get('action')
            if not isinstance(action, str):
                raise ValueError('Invalid shutdown action')
            keys = {'routine': {'action', 'name'}, 'outlet': {'action', 'entity'},
                    'printer': {'action', 'entity', 'outlet'}, 'light': {'action', 'entity'},
                    'group': {'action', 'group'}}.get(action)
            if keys is None or set(step) != keys or any(
                    not isinstance(step[key], str) or not 1 <= len(step[key]) <= 96 for key in keys):
                raise ValueError('Invalid shutdown step schema')
            if action == 'light' and not step['entity'].startswith('light.'):
                raise ValueError('Shutdown light step requires a light entity')
            if action == 'group' and (not step['group'].startswith('group.') or step['group'][6:] not in (groups or {})):
                raise ValueError('Shutdown step requires a configured device group')
            if action == 'routine':
                child = step['name']
                if child not in ('tv_off', 'computer_off') or child.removesuffix('_off') not in routines:
                    raise ValueError('Missing or unsupported shutdown routine')
                if child == 'tv_off' and routines['tv'].get('target') != 'tcl':
                    raise ValueError('Shutdown list requires verified TCL TV shutdown')
            if action in ('outlet', 'printer') and 'plugs' not in adapters:
                raise ValueError('Shutdown outlet requires plugs')
            if action == 'printer' and 'printers' not in adapters:
                raise ValueError('Shutdown printer requires IPP adapter')
            if name == 'leave_house' and (
                    (group == 'optional_pc' and step != {'action': 'routine', 'name': 'computer_off'}) or
                    (group != 'optional_pc' and step == {'action': 'routine', 'name': 'computer_off'})):
                raise ValueError('leave_house PC shutdown must be in optional_pc')


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
    computer = directory / 'computer-routine.json'
    if computer.is_file():
        value['routines']['computer'] = load(computer)
        imported.append(computer)
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
