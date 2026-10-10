"""Host configuration inventory, checked against the compiled Rust catalog."""
import json
from pathlib import Path

FAMILIES = json.loads(Path(__file__).with_suffix('.json').read_text())
ADAPTERS = tuple(row['name'] for row in FAMILIES if row['section'] == 'adapters')
SERVICES = tuple(row['name'] for row in FAMILIES if row['section'] == 'services')
CERTIFICATES = ('digi-cert.pem', 'digi-key.pem', 'api-cert.pem', 'api-key.pem')
FILES = ('home.json',) + tuple(name + '.json' for name in ADAPTERS + SERVICES) + ('tv.json',) + CERTIFICATES


def legacy_arguments():
    independent = tuple(n for n in ADAPTERS + SERVICES if n not in ('meaco', 'plugs', 'samsung', 'digi', 'api'))
    return '\n'.join([
        '[ ! -f "$root/config/meaco.json" ] || set -- "$@" --config "$root/config/meaco.json"',
        'if [ -f "$root/config/tv.json" ]; then',
        '  set -- "$@" --tv "$root/config/tv.json"',
        'else',
        '  for family in plugs samsung digi; do',
        '    [ ! -f "$root/config/$family.json" ] || set -- "$@" --adapter "$family=$root/config/$family.json"',
        '  done',
        'fi',
        'for family in ' + ' '.join(independent) + '; do',
        '  [ ! -f "$root/config/$family.json" ] || set -- "$@" --adapter "$family=$root/config/$family.json"',
        'done',
        '[ ! -f "$root/config/api.json" ] || set -- "$@" --api "$root/config/api.json"'])
