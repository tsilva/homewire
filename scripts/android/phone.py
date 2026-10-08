#!/usr/bin/env python3
"""ADB + debuggable Termux installer. Standard library only; never prints secrets."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shlex
import subprocess
import tomllib

CE_HOMEWIRE = '/data/data/com.termux/files/home/homewire'
HOMEWIRE = CE_HOMEWIRE
DE_HOMEWIRE = '/data/user_de/0/com.termux/files/homewire'
PREFIX = '/data/data/com.termux/files/usr'
ZCONFIG = '/data/data/com.termux/files/home/.zeroclaw/config.toml'
SERVICE = PREFIX + '/var/service/homewire'
FILES = ('meaco.json', 'printers.json', 'awair.json', 'lights.json', 'plugs.json',
         'samsung.json', 'digi.json', 'tv.json', 'digi-cert.pem', 'digi-key.pem',
         'router.json','brother.json','cast.json','dlna.json','alerts.json','history.json',
         'mobile.json','weather.json','shopping.json','radio.json','speech.json','api.json',
         'api-cert.pem','api-key.pem')


def q(value):
    return shlex.quote(str(value))


class Phone:
    def __init__(self, serial):
        global HOMEWIRE
        self.adb = ['adb', '-s', serial]
        self.direct_boot=False
        try:
            self.sh('id >/dev/null')
        except RuntimeError as e:
            if "couldn't stat /data/user/0/com.termux" in str(e):
                raise RuntimeError('Unlock Android first; Termux protected storage is unavailable') from None
            raise
        self.direct_boot = bool(self.sh('[ ! -f ' + q(CE_HOMEWIRE + '/direct-boot') + ' ] || echo yes').strip())
        HOMEWIRE = DE_HOMEWIRE if self.direct_boot else CE_HOMEWIRE

    def sh(self, command, data=None):
        command = ('run-as com.termux /system/bin/sh -c ' + q(
            'export PREFIX=' + q(PREFIX) + '; export PATH=' + q(PREFIX + '/bin:/system/bin') +
            '; export SVDIR=' + q(PREFIX + '/var/service') + '; ' + command))
        result = subprocess.run(self.adb + ['shell', '-T', command], input=data if data is not None else b'', capture_output=True, timeout=360)
        if result.returncode:
            # Commands operate on secrets; report only stderr, never stdout/body.
            raise RuntimeError(result.stderr.decode(errors='replace').strip() or 'Phone command failed')
        return result.stdout

    def put(self, path, data, mode='600'):
        self.sh('set -eu; umask 077; cat > ' + q(path + '.install-new') + '; chmod ' + mode + ' ' + q(path + '.install-new') + '; mv -fT ' + q(path + '.install-new') + ' ' + q(path), data)

    def request(self, method, arguments=None):
        request = {'jsonrpc': '2.0', 'id': 1, 'method': method}
        if arguments is not None:
            request['params'] = arguments
        data = (json.dumps(request) + '\n').encode()
        reply = self.sh(q(HOMEWIRE + '/current/homewire') + ' bridge ' + q(HOMEWIRE + '/run/homewire.sock'), data)
        result = json.loads(reply)
        if 'error' in result or result.get('result', {}).get('isError'):
            raise RuntimeError('MCP request failed: ' + json.dumps(result))
        return result['result']

    def restart(self):
        if self.direct_boot:
            import direct_boot
            direct_boot.restart(self.adb[2])
            import time
            for _ in range(100):
                if self.sh('[ ! -S '+q(HOMEWIRE+'/run/homewire.sock')+' ] || echo yes').strip(): return
                time.sleep(.1)
            raise RuntimeError('Direct Boot owner did not start')
        self.sh('sv -w 10 down ' + q(SERVICE))
        self.sh('rm -f ' + q(HOMEWIRE + '/run/homewire.sock') + '; sv -w 10 up ' + q(SERVICE))
        self.sh('sv -w 10 check ' + q(SERVICE))


def validate_configs(directory):
    configs = []
    for name in FILES:
        path = directory / name
        if not path.is_file():
            continue
        data = path.read_bytes()
        if name.endswith('.json'):
            value = json.loads(data)
            def check(v):
                if isinstance(v, dict):
                    if 'connect' in v:
                        raise ValueError('Deployment configs must omit test/tunnel connect overrides: ' + name)
                    for item in v.values():
                        check(item)
                elif isinstance(v, list):
                    for item in v:
                        check(item)
                elif isinstance(v, str) and ('REPLACE' in v or v.startswith('192.0.2.')):
                    raise ValueError('Unconfigured example: ' + name)
            check(value)
        configs.append((name, data))
    if not any(name.endswith('.json') for name, _ in configs):
        raise ValueError('No device configs found')
    return configs


def service_script(root=None):
    # Existing configs determine family selection; TV owns its child managers.
    script = '''#!/data/data/com.termux/files/usr/bin/sh
umask 077
exec 2>&1
root=/data/data/com.termux/files/home/homewire
set --
for pair in 'meaco:config' 'printers:printers' 'awair:awair' 'lights:lights'; do
  file=${pair%:*}; flag=${pair#*:}
  [ ! -f "$root/config/$file.json" ] || set -- "$@" "--$flag" "$root/config/$file.json"
done
if [ -f "$root/config/tv.json" ]; then
  set -- "$@" --tv "$root/config/tv.json"
else
  for file in plugs samsung digi; do
    [ ! -f "$root/config/$file.json" ] || set -- "$@" "--$file" "$root/config/$file.json"
  done
fi
for family in router brother cast dlna alerts history mobile weather shopping radio speech; do
  [ ! -f "$root/config/$family.json" ] || set -- "$@" --adapter "$family=$root/config/$family.json"
done
[ ! -f "$root/config/api.json" ] || set -- "$@" --api "$root/config/api.json"
while IFS= read -r flag; do
  [ -z "$flag" ] || set -- "$@" "$flag"
done < "$root/service.args"
exec "$root/current/homewire" "$@" daemon "$root/run/homewire.sock"
'''
    root = root or HOMEWIRE
    if root != CE_HOMEWIRE:
        script=script.replace(CE_HOMEWIRE,root).replace('#!/data/data/com.termux/files/usr/bin/sh','#!/system/bin/sh')
    return script


def service_flags(existing, allow_control=None):
    """Keep automation ownership through upgrades; change only explicit grants."""
    flags = list(dict.fromkeys(existing.decode().splitlines()))
    flags = [flag for flag in flags if flag]
    if any(not re.fullmatch(r'--[a-z-]+', flag) for flag in flags):
        raise ValueError('Invalid saved service flags')
    if allow_control is not None:
        flags = [flag for flag in flags if flag != '--allow-control']
        if allow_control:
            flags.insert(0, '--allow-control')
        elif any(flag in flags for flag in ('--schedule', '--light-schedule')):
            raise ValueError('Disable schedules before revoking control')
    return ''.join(flag + '\n' for flag in flags).encode()


def install(phone, args):
    package = Path(args.package)
    binary = (package / 'homewire').read_bytes()
    manifest = json.loads((package / 'manifest.json').read_text())
    digest = hashlib.sha256(binary).hexdigest()
    if digest != manifest['sha256'] or manifest['target'] != 'aarch64-linux-android':
        raise ValueError('Package hash/target mismatch')
    configs = validate_configs(Path(args.configs)) if args.configs else []
    release = HOMEWIRE + '/releases/' + digest[:16]
    phone.sh('umask 077; mkdir -p ' + ' '.join(q(HOMEWIRE + '/' + p) for p in ('releases', 'config', 'run', 'backups')) + '; mkdir -p ' + q(release))
    if not phone.direct_boot:
        phone.sh('umask 077; mkdir -p '+q(SERVICE)+' '+q(SERVICE+'/log')+'; touch '+q(SERVICE+'/down'))
    # Binary verification happens before changing the active version or configs.
    if phone.sh('[ ! -f ' + q(release + '/homewire') + ' ] || sha256sum ' + q(release + '/homewire')).decode().split()[:1] != [digest]:
        phone.put(release + '/homewire', binary, '700')
    phone.put(release + '/manifest.json', (package / 'manifest.json').read_bytes())
    actual = phone.sh('sha256sum ' + q(release + '/homewire')).decode().split()[0]
    if actual != digest:
        raise ValueError('Phone binary hash mismatch')
    print(phone.sh(q(release + '/homewire') + ' --version').decode().strip())
    if configs:
        phone.sh('tar -czf ' + q(HOMEWIRE + '/backups/config-') + '$(date +%s).tgz -C ' + q(HOMEWIRE) + ' config')
        if phone.direct_boot:
            import direct_boot
            direct_boot.stop(phone.adb[2])
        else:
            phone.sh('if [ -p ' + q(SERVICE + '/supervise/control') + ' ]; then sv -w 10 down ' + q(SERVICE) + '; fi')
        for name, data in configs:
            phone.put(HOMEWIRE + '/config/' + name, data)
    existing = phone.sh('if [ -f ' + q(HOMEWIRE + '/service.args') + ' ]; then cat ' + q(HOMEWIRE + '/service.args') + '; fi')
    flags = service_flags(existing, args.allow_control)
    phone.put(HOMEWIRE + '/service.args', flags)
    if phone.direct_boot:
        phone.put(HOMEWIRE+'/launch',service_script().encode(),'700')
    else:
        phone.put(SERVICE + '/run', service_script().encode(), '700')
        phone.put(SERVICE + '/log/run', ('''#!/data/data/com.termux/files/usr/bin/sh
umask 077
mkdir -p /data/data/com.termux/files/home/homewire/log
exec svlogd -tt /data/data/com.termux/files/home/homewire/log
''').encode(), '700')
    phone.sh('umask 077; mkdir -p ' + q(HOMEWIRE + '/log') + '; chmod 700 ' + q(HOMEWIRE + '/log'))
    phone.put(HOMEWIRE + '/log/config', b's65536\nn3\n')
    if not phone.direct_boot:
        phone.put(SERVICE + '/check', ('#!/data/data/com.termux/files/usr/bin/sh\n[ -S ' + q(HOMEWIRE + '/run/homewire.sock') + ' ]\n').encode(), '700')
    # Preserve the prior version and switch links atomically on the same filesystem.
    phone.sh('set -eu; cd ' + q(HOMEWIRE) + '; rm -f current.new previous.new; if [ -L current ]; then old=$(readlink current); '
             'if [ "$old" != ' + q(release) + ' ]; then ln -s "$old" previous.new; mv -fT previous.new previous; fi; fi; '
             'ln -s ' + q(release) + ' current.new; mv -fT current.new current' +
             ('' if phone.direct_boot else '; rm -f ' + q(SERVICE + '/down')))
    try:
        phone.restart()
        verify(phone)
    except Exception:
        # Restore only binary on a failed update. Device state is never rewound.
        if phone.sh('[ ! -f ' + q(HOMEWIRE + '/previous/validated') + ' ] || echo yes').strip():
            rollback(phone)
        raise
    phone.put(release + '/validated', b'MCP discovery and bounded fresh-read replies verified\n')
    print('Installed version ' + digest[:16] + '; saved service flags preserved')


def verify(phone, require_online=False):
    tools = phone.request('tools/list')['tools']
    entities = []
    offset = 0
    while True:
        discovery = phone.request('tools/call', {'name': 'entity_list', 'arguments': {'offset': offset}})['structuredContent']
        entities.extend(discovery['entities'])
        offset = discovery.get('next_offset')
        if offset is None:
            break
    print('Tools:', ', '.join(t['name'] for t in tools))
    print('Entities:', len(entities))
    # Sample one entity per physical device; adapter reads include all scalar values.
    ids = list(dict((e['device'], e['entity']) for e in entities).values())
    failures = []
    for offset in range(0, len(ids), 32):
        states = phone.request('tools/call', {'name': 'entity_status', 'arguments': {'entities': ids[offset:offset+32]}})['structuredContent']['states']
        for state in states:
            print(state['entity'], 'available' if state['available'] else 'UNAVAILABLE')
            if not state['available']:
                failures.append(state['entity'])
    if failures and require_online:
        raise RuntimeError('Unavailable devices: ' + ', '.join(failures))
    print(f'Fresh reads: {len(ids)-len(failures)}/{len(ids)} devices available; no controls sent')


def rollback(phone):
    phone.sh('set -eu; cd ' + q(HOMEWIRE) + '; [ -L previous ]; [ -f previous/validated ]; '
             'old=$(readlink previous); new=$(readlink current); ln -s "$old" current.new; mv -fT current.new current; '
             'ln -s "$new" previous.new; mv -fT previous.new previous')
    phone.restart()
    phone.request('tools/list')
    print('Previous executable restored; configs and device state preserved')


def connect(phone, agent):
    original = phone.sh('cat ' + q(ZCONFIG))
    text = original.decode()
    config = tomllib.loads(text)
    if agent not in config.get('agents', {}):
        raise ValueError('Unknown ZeroClaw agent')
    server = {'name': 'homewire', 'transport': 'stdio', 'command': HOMEWIRE + '/current/homewire',
              'args': ['bridge', HOMEWIRE + '/run/homewire.sock'], 'tool_timeout_secs': 300}
    existing = next((s for s in config.get('mcp', {}).get('servers', []) if s.get('name') == 'homewire'), None)
    migrating=False
    legacy=dict(server,command=CE_HOMEWIRE+'/current/homewire',args=['bridge',CE_HOMEWIRE+'/run/homewire.sock'])
    if existing == legacy and HOMEWIRE == DE_HOMEWIRE:
        migrating=True
        pattern=r'(?ms)^\[\[mcp\.servers\]\]\n.*?(?=^\[|\Z)'
        def move(match):
            block=match[0]
            entry=tomllib.loads(block)['mcp']['servers'][0]
            return block.replace(CE_HOMEWIRE+'/',DE_HOMEWIRE+'/') if entry==legacy else block
        text=re.sub(pattern,move,text)
    elif existing and existing != server:
        raise ValueError('Existing Homewire MCP definition differs; preserve it for manual review')
    if not existing:
        text += '\n[[mcp.servers]]\n' + ''.join(k + ' = ' + json.dumps(v, ensure_ascii=False) + '\n' for k, v in server.items())
    bundles = config.get('mcp_bundles', {})
    if 'homewire' in bundles and bundles['homewire'] != {'servers': ['homewire']}:
        raise ValueError('Existing Homewire bundle differs')
    if 'homewire' not in bundles:
        text += '\n[mcp_bundles.homewire]\nservers = ["homewire"]\n'
    pattern = r'(\[agents\.' + re.escape(agent) + r'\]\n)(.*?)(?=\n\[|\Z)'
    match = re.search(pattern, text, re.S)
    if not match:
        raise ValueError('Agent table requires a simple [agents.NAME] header')
    grants = config['agents'][agent].get('mcp_bundles', [])
    if 'homewire' not in grants:
        grants = grants + ['homewire']
    body = match[2]
    line = 'mcp_bundles = ' + json.dumps(grants)
    body = re.sub(r'^mcp_bundles\s*=.*$', line, body, flags=re.M) if re.search(r'^mcp_bundles\s*=', body, re.M) else body + '\n' + line + '\n'
    text = text[:match.start()] + match[1] + body + text[match.end():]
    updated = tomllib.loads(text)
    # Ensure only the intended server, bundle and grant changed.
    expected = tomllib.loads(original.decode())
    if not existing:
        expected.setdefault('mcp', {}).setdefault('servers', []).append(server)
    elif migrating:
        expected['mcp']['servers'][expected['mcp']['servers'].index(existing)]=server
    expected.setdefault('mcp_bundles', {})['homewire'] = {'servers': ['homewire']}
    expected['agents'][agent]['mcp_bundles'] = grants
    if updated != expected:
        raise ValueError('Unexpected configuration changes')
    if phone.sh('cat ' + q(ZCONFIG)) != original:
        raise RuntimeError('ZeroClaw config changed during preparation')
    backup = ZCONFIG + '.before-homewire'
    if text.encode() != original:
        phone.sh('umask 077; cp -p ' + q(ZCONFIG) + ' ' + q(backup))
        phone.put(ZCONFIG + '.homewire-new', text.encode())
        phone.sh('mv -f ' + q(ZCONFIG + '.homewire-new') + ' ' + q(ZCONFIG))
        phone.sh('sv restart ' + q(PREFIX + '/var/service/zeroclaw'))
        phone.sh('sv -w 10 check ' + q(PREFIX + '/var/service/zeroclaw'))
    print('Homewire MCP granted to ' + agent + '; prior config retained at ' + backup)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--serial', required=True)
    sub = parser.add_subparsers(dest='command', required=True)
    ins = sub.add_parser('install')
    ins.add_argument('--package', default='target/android-package')
    ins.add_argument('--configs', help='Private LAN config directory; never copies state or test files')
    ins.add_argument('--allow-control', action=argparse.BooleanOptionalAction, default=None)
    ver = sub.add_parser('verify')
    ver.add_argument('--require-online', action='store_true', help='Fail if any configured device is unavailable')
    sub.add_parser('rollback')
    con = sub.add_parser('connect-zeroclaw')
    con.add_argument('--agent', required=True)
    args = parser.parse_args()
    phone = Phone(args.serial)
    if args.command == 'install': install(phone, args)
    elif args.command == 'verify': verify(phone, args.require_online)
    elif args.command == 'rollback': rollback(phone)
    else: connect(phone, args.agent)


if __name__ == '__main__':
    main()
