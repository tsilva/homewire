#!/usr/bin/env python3
"""Install a fail-closed, separate-UID ZeroClaw jail on rooted Android.
Requires an unlocked phone, Magisk, native sandbox artifacts and the updated
Homewire agent endpoint. Existing ZeroClaw files remain a private recovery copy.
"""
import argparse
import hashlib
import json
import subprocess
import struct
import time
import tomllib
from pathlib import Path
import phone
import direct_boot
import lockdown
import deployment

ROOT = phone.CE_HOMEWIRE.rsplit('/',1)[0]+'/zeroclaw-sandbox'
SUPERVISOR = '/data/adb/service.d/zeroclaw-sandbox.sh'
PACKAGE = 'eu.tsilva.homewire.agent'
MARKER = phone.CE_HOMEWIRE.rsplit('/', 1)[0] + '/zeroclaw-isolated'


def probe_args(profile, network=False):
    args = [profile.agent['name'], phone.DE_HOMEWIRE+'/config/home.json' if getattr(profile, 'config_file', None) else phone.DE_HOMEWIRE+'/config/api.json', phone.ZCONFIG,
            profile.android['root_command']]
    if network:
        host, port = profile.network['lan_probe'].split(':')
        args += [str(profile.agent['mcp_port']), str(profile.network['admin_port']), host, port, profile.agent['provider_host']]
    return ' '.join(phone.q(v) for v in args)


def namespace_script(profile=None):
    profile = profile or deployment.current()
    return ('''#!/system/bin/sh
set -eu
base=__BASE__
uid=$(cat "$base/uid")
case "$uid" in *[!0-9]*|'') exit 1;; esac
if [ "${1:-}" = probe ]; then
  exec "$base/enter" "$base" "$uid" __AGENT__ /bin/probe __PROBE__
fi
if [ "${1:-}" = network ]; then
  exec "$base/enter" "$base" "$uid" __AGENT__ /bin/probe __NETWORK__
fi
exec "$base/enter" "$base" "$uid" __AGENT__ /bin/zeroclaw --config-dir /state/zero daemon --host 127.0.0.1 --port __GATEWAY__
'''.replace('__BASE__',phone.q(ROOT)).replace('__AGENT__',phone.q(profile.agent['name']))
       .replace('__PROBE__',probe_args(profile)).replace('__NETWORK__',probe_args(profile,True))
       .replace('__GATEWAY__',str(profile.agent['gateway_port'])))


def supervisor_script(profile=None):
    profile = profile or deployment.current()
    ports = [profile.agent["mcp_port"]]
    if profile.agent.get("transcription_port") is not None: ports.append(profile.agent["transcription_port"])
    dns_rules = "\n".join("iptables -A HW_AGENT -d "+address+" -p "+protocol+" --dport 53 -j ACCEPT" for address in profile.network["dns_servers"] for protocol in ("udp","tcp"))
    script = '''#!/system/bin/sh
set -eu
umask 077
base=__BASE__
exec 9>/data/adb/zeroclaw-sandbox.lock
flock -n 9 9>&9 || exit 1
echo $$ > /data/adb/zeroclaw-sandbox.pid
child=
logger=
trap '[ -z "$child" ] || kill "$child" 2>/dev/null || true; [ -z "$logger" ] || kill "$logger" 2>/dev/null || true; rm -f /data/adb/zeroclaw-sandbox.pid; exit' TERM INT EXIT
# Credentials stay in Android CE storage: boot supervision waits for unlock.
while [ ! -f "$base/ready" ]; do sleep 5; done
uid=$(cat "$base/uid")
case "$uid" in *[!0-9]*|'') exit 1;; esac
for family in iptables ip6tables; do
  $family -N HW_AGENT 2>/dev/null || true
  $family -F HW_AGENT
done
iptables -A HW_AGENT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
iptables -A HW_AGENT -d 127.0.0.1 -p tcp -m multiport --dports __PORTS__ -j ACCEPT
__DNS_RULES__
for net in 0.0.0.0/8 10.0.0.0/8 100.64.0.0/10 127.0.0.0/8 169.254.0.0/16 172.16.0.0/12 192.168.0.0/16 224.0.0.0/4 240.0.0.0/4; do
  iptables -A HW_AGENT -d "$net" -j REJECT
done
iptables -A HW_AGENT -p tcp --dport 443 -j ACCEPT
iptables -A HW_AGENT -j REJECT
ip6tables -A HW_AGENT -j REJECT
for family in iptables ip6tables; do
  $family -C OUTPUT -m owner --uid-owner "$uid" -j HW_AGENT 2>/dev/null ||
    $family -I OUTPUT 1 -m owner --uid-owner "$uid" -j HW_AGENT
done
rm -f "$base/log/pipe"
mkfifo "$base/log/pipe"
while true; do
  "$base/log-writer" "$base/log" < "$base/log/pipe" &
  logger=$!
  /system/bin/sh "$base/namespace.sh" > "$base/log/pipe" 2>&1 &
  child=$!
  wait "$child" || true
  child=
  wait "$logger" || true
  logger=
  sleep 5
done
'''

    return script.replace("__BASE__",phone.q(ROOT)).replace("__PORTS__",",".join(map(str,ports))).replace("__DNS_RULES__",dns_rules)


def root_put(serial, path, data, mode='600'):
    q = phone.q
    direct_boot.root_sh(serial, 'set -eu; umask 077; cat > ' + q(path + '.new') +
                        '; chmod ' + mode + ' ' + q(path + '.new') +
                        '; mv -f ' + q(path + '.new') + ' ' + q(path), data)
    actual=direct_boot.root_sh(serial,'sha256sum '+q(path)).decode().split()[0]
    if actual!=hashlib.sha256(data).hexdigest():raise ValueError('Sandbox transfer hash mismatch')


def agent_uid(serial):
    row=direct_boot.root_sh(serial,'cmd package list packages --user '+str(deployment.android()['user'])+' -U '+PACKAGE).decode().strip()
    prefix='package:'+PACKAGE+' uid:'
    if not row.startswith(prefix) or not row[len(prefix):].isdecimal():
        raise ValueError('Dedicated Android identity is missing or ambiguous')
    uid=int(row[len(prefix):])
    if not 10000<=uid%100000<20000 or uid//100000!=deployment.android()['user']:raise ValueError('Unexpected Android application UID')
    return uid


def require_supported_binary(p):
    binary=phone.PREFIX+'/bin/zeroclaw'
    header=p.sh('head -c 64 '+phone.q(binary))
    if len(header)!=64 or header[:6]!=b'\x7fELF\x02\x01' or struct.unpack_from('<H',header,18)[0]!=183:
        raise ValueError('Sandbox requires a dynamic ARM64 Android ZeroClaw binary')
    offset=struct.unpack_from('<Q',header,32)[0]
    entry,count=struct.unpack_from('<HH',header,54)
    if offset>65536 or entry!=56 or not 1<=count<=64:
        raise ValueError('Unsupported ELF program headers')
    table=p.sh('dd if='+phone.q(binary)+' bs=1 skip='+str(offset)+' count='+str(entry*count)+' 2>/dev/null')
    if len(table)!=entry*count or not any(struct.unpack_from('<I',table,i*entry)[0]==3 for i in range(count)):
        raise ValueError('Static binaries ignore the required preload guard and are refused')
    if p.sh(phone.q(binary)+' --version').decode().strip()!='zeroclaw 0.8.5':
        raise ValueError('Policy is audited for ZeroClaw 0.8.5; review other versions before installation')


def preflight(p,profile):
    _, _, api, _ = phone.read_api(p)
    if int(api['listen'].rsplit(':',1)[1])!=profile.network['admin_port'] or api.get('assistant',{}).get('url')!=profile.assistant_url:
        raise ValueError('Deployment ports/agent must match the existing API/assistant configuration')
    return api


def install(serial, artifacts):
    p = phone.Phone(serial)
    q = phone.q
    def root(command): return direct_boot.root_sh(serial, 'set -eu; ' + command)
    # Android assigns and authorizes a dedicated network-only identity.
    root('[ ! -e '+q(ROOT)+' ]; [ ! -e '+q(SUPERVISOR)+' ]')
    require_supported_binary(p)
    profile=deployment.current()
    preflight(p,profile)
    listeners=root('ss -ltn').decode()
    if any(r.split()[3].endswith(':'+str(profile.agent['mcp_port'])) for r in listeners.splitlines()[1:] if len(r.split())>3):
        raise ValueError('Configured agent port is already in use; no service changes made')
    subprocess.run(p.adb+['install','--user',str(deployment.android()['user']),'--no-incremental','-r',str(Path(artifacts)/'identity.apk')],check=True,capture_output=True)
    uid=agent_uid(serial)
    root('dumpsys deviceidle whitelist +'+PACKAGE+'; am set-inactive --user '+str(deployment.android()['user'])+' '+PACKAGE+' false; am set-standby-bucket --user '+str(deployment.android()['user'])+' '+PACKAGE+' active')
    staged = phone.CE_HOMEWIRE.rsplit('/', 1)[0] + '/zeroclaw-policy-stage'
    token = lockdown.prepare(p, staged)
    # Take a consistent private copy; never run both ownership identities.
    p.sh('sv -w 10 down '+q(phone.PREFIX+'/var/service/zeroclaw')+'; touch '+q(phone.PREFIX+'/var/service/zeroclaw/down'))
    root('umask 077; mkdir -p '+q(ROOT)+'/root '+q(ROOT)+'/policy '+q(ROOT)+'/log '+q(ROOT)+'/state/zero '+q(ROOT)+'/tmp; chmod 755 '+q(ROOT)+'/root')
    root('cp '+q(staged)+'/config.toml '+q(staged)+'/.secret_key '+q(ROOT)+'/policy/; '
         'cp '+q(phone.PREFIX)+'/bin/zeroclaw '+q(ROOT)+'/root/zeroclaw; '
         'for dir in agents data state shared; do [ ! -d '+q(phone.ZCONFIG.rsplit('/',1)[0])+'/$dir ] || cp -a '+q(phone.ZCONFIG.rsplit('/',1)[0])+'/$dir '+q(ROOT)+'/state/zero/; done; '
         'chown -R '+str(uid)+':'+str(uid)+' '+q(ROOT)+'/state '+q(ROOT)+'/tmp; '
         'chown 0:0 '+q(ROOT)+'/state; chmod 755 '+q(ROOT)+'/state; '
         'chown '+str(uid)+':'+str(uid)+' '+q(ROOT)+'/policy/config.toml '+q(ROOT)+'/policy/.secret_key; '
         'chmod 600 '+q(ROOT)+'/policy/config.toml '+q(ROOT)+'/policy/.secret_key; '
         'mkdir -p '+q(ROOT)+'/root/bin '+q(ROOT)+'/root/system '+q(ROOT)+'/root/apex '+q(ROOT)+'/root/proc '+q(ROOT)+'/root/dev/socket '+q(ROOT)+'/root/dev/__properties__ '+q(ROOT)+'/root/state '+q(ROOT)+'/root/tmp; '
         'mv '+q(ROOT)+'/root/zeroclaw '+q(ROOT)+'/root/bin/zeroclaw; chmod 755 '+q(ROOT)+'/root/bin/zeroclaw; '
         'touch '+q(ROOT)+'/root/dev/null '+q(ROOT)+'/root/dev/random '+q(ROOT)+'/root/dev/urandom '+q(ROOT)+'/root/dev/socket/dnsproxyd '+q(ROOT)+'/root/dev/socket/fwmarkd '+q(ROOT)+'/state/zero/config.toml '+q(ROOT)+'/state/zero/.secret_key')
    root_put(serial,ROOT+'/uid',str(uid).encode())
    root_put(serial,ROOT+'/policy/deployment.json',(json.dumps(deployment.current().value,indent=2)+'\n').encode())
    root_put(serial,ROOT+'/policy/TOOLS.md',lockdown.instructions(deployment.current().tools).encode())
    workspace=ROOT+'/state/zero/agents/'+profile.agent['name']+'/workspace'
    root('mkdir -p '+q(workspace)+'; '
         'touch '+q(workspace+'/TOOLS.md')+'; '
         'chown -R '+str(uid)+':'+str(uid)+' '+q(ROOT)+'/state/zero/agents; '
         'chown '+str(uid)+':'+str(uid)+' '+q(ROOT)+'/policy/TOOLS.md')
    for name in ('enter', 'probe', 'guard.so', 'log'):
        target = ROOT + ('/enter' if name == 'enter' else '/log-writer' if name == 'log' else '/root/bin/' + name)
        root_put(serial, target, (Path(artifacts) / name).read_bytes(), '755')
    root_put(serial, ROOT + '/namespace.sh', namespace_script().encode(), '700')
    root('chown -R 0:0 '+q(ROOT)+'/root; chmod -R a+rX '+q(ROOT)+'/root')
    activate(p,token)


def activate(p,token):
    serial=p.adb[2]
    q=phone.q
    def root(command):return direct_boot.root_sh(serial,'set -eu; '+command)
    output=root('/system/bin/sh '+q(ROOT)+'/namespace.sh probe')
    print(output.decode().strip())
    if b'filesystem_and_execution_denied=true' not in output:
        raise ValueError('Native sandbox probe failed; no service cutover')
    api_path, document, api, raw = phone.read_api(p)
    if 'agent' in api: raise ValueError('An agent endpoint already exists; inspect before replacing')
    api['agent']={'listen':deployment.current().endpoint,'token':token,'tools':list(deployment.current().tools)}
    p.put(api_path+'.before-isolation',raw)
    phone.write_api(p, api_path, document)
    try:
        p.restart()
    except Exception:
        # Keep household scheduling available without re-enabling the old agent.
        p.put(api_path,raw)
        p.restart()
        raise
    # Retire the older root daemon hook, including its pre-unlock bypass.
    root('if [ -f /data/adb/service.d/zeroclaw-directboot.sh ]; then '
         'mkdir -p '+q(ROOT)+'/legacy; mv /data/adb/service.d/zeroclaw-directboot.sh '+q(ROOT)+'/legacy/zeroclaw-directboot.sh; fi')
    root_put(serial,SUPERVISOR,supervisor_script().encode(),'700')
    root('touch '+q(ROOT)+'/ready; nohup /system/bin/sh '+q(SUPERVISOR)+' </dev/null >/dev/null 2>&1 &')
    p.put(MARKER,b'scoped-http-v1\n')
    print('Separate-UID supervisor installed. Verify model/MCP access before declaring success.')


def stop(serial):
    # Check the saved identity before signaling; never kill by name alone.
    pid=direct_boot.root_sh(serial,'''set -eu
file=/data/adb/zeroclaw-sandbox.pid
[ -f "$file" ] || exit 0
p=$(cat "$file")
case "$p" in *[!0-9]*|'') exit 1;; esac
[ -d /proc/$p ] || exit 0
c=$(tr '\\000' ' ' < /proc/$p/cmdline)
case "$c" in *'/data/adb/service.d/zeroclaw-sandbox.sh'*) echo "$p";; *) exit 1;; esac
''').decode().strip()
    if pid:
        direct_boot.root_sh(serial,'kill '+pid)
        for _ in range(50):
            live=direct_boot.root_sh(serial,'[ ! -f /data/adb/zeroclaw-sandbox.pid ] || echo yes').strip()
            if not live:break
            time.sleep(.1)
        else:raise ValueError('Sandbox supervisor did not stop')
    uid=agent_uid(serial)
    rows=direct_boot.root_sh(serial,'ps -A -o UID,PID,NAME').decode().splitlines()
    if any(r.split()[0]==str(uid) and r.split()[-1]=='zeroclaw' for r in rows[1:]):
        raise ValueError('Sandbox child survived; refuse to start a second owner')


def restart(serial):
    phone.Phone(serial)  # Requires CE unlock before operator restarts.
    stop(serial)
    direct_boot.root_sh(serial,'nohup /system/bin/sh '+phone.q(SUPERVISOR)+' </dev/null >/dev/null 2>&1 &')


def update(serial,artifacts):
    p=phone.Phone(serial)
    profile=deployment.current()
    preflight(p,profile)
    api_path, document, api, _ = phone.read_api(p)
    if api.get('agent',{}).get('listen')!=profile.endpoint:
        raise ValueError('Change API agent listen through the operator before updating its deployment profile')
    cfg=tomllib.loads(direct_boot.root_sh(serial,'cat '+phone.q(ROOT+'/policy/config.toml')).decode())
    for patch in lockdown.patches(cfg,api['agent']['token'],profile): cfg[patch['path'][1:]]=patch['value']
    require_supported_binary(p)
    stage=phone.CE_HOMEWIRE.rsplit('/',1)[0]+'/zeroclaw-update-'+str(time.time_ns())
    key=direct_boot.root_sh(serial,'cat '+phone.q(ROOT+'/policy/.secret_key'))
    try:
        cfg=lockdown.validate_prepared(p,stage,cfg,key)
    finally:
        p.sh('rm -f '+phone.q(stage+'/config.toml')+' '+phone.q(stage+'/.secret_key')+'; rmdir '+phone.q(stage)+' 2>/dev/null || true')
    prepared=lockdown.to_toml(cfg)
    stop(serial)
    uid=agent_uid(serial)
    workspace=ROOT+'/state/zero/agents/'+profile.agent['name']+'/workspace'
    direct_boot.root_sh(serial,'mkdir -p '+phone.q(workspace)+'; touch '+phone.q(workspace+'/TOOLS.md')+'; chown -R '+str(uid)+':'+str(uid)+' '+phone.q(ROOT+'/state/zero/agents'))
    root_put(serial,ROOT+'/policy/deployment.json',(json.dumps(profile.value,indent=2)+'\n').encode())
    root_put(serial,ROOT+'/policy/config.toml',prepared.encode())
    direct_boot.root_sh(serial,'chown '+str(agent_uid(serial))+':'+str(agent_uid(serial))+' '+phone.q(ROOT+'/policy/config.toml'))
    api['agent']['tools']=list(profile.tools)
    phone.write_api(p, api_path, document)
    p.restart()
    root_put(serial,ROOT+'/policy/TOOLS.md',lockdown.instructions(deployment.current().tools).encode())
    direct_boot.root_sh(serial,'chown '+str(agent_uid(serial))+':'+str(agent_uid(serial))+' '+ROOT+'/policy/TOOLS.md')
    for name in ('enter','probe','guard.so','log'):
        path=ROOT+('/enter' if name=='enter' else '/log-writer' if name=='log' else '/root/bin/'+name)
        direct_boot.root_sh(serial,'cp -p '+phone.q(path)+' '+phone.q(path+'.previous'))
        root_put(serial,path,(Path(artifacts)/name).read_bytes(),'755')
    root_put(serial,ROOT+'/namespace.sh',namespace_script().encode(),'700')
    root_put(serial,SUPERVISOR,supervisor_script().encode(),'700')
    output=direct_boot.root_sh(serial,'/system/bin/sh '+phone.q(ROOT)+'/namespace.sh probe')
    if b'filesystem_and_execution_denied=true' not in output:raise ValueError('Sandbox probe failed; service stays stopped')
    restart(serial)


def verify(serial):
    p=phone.Phone(serial);uid=agent_uid(serial)
    saved=json.loads(direct_boot.root_sh(serial,'cat '+phone.q(ROOT+'/policy/deployment.json')))
    if saved!=deployment.current().value: raise ValueError('Run update to apply the changed deployment profile before verification')
    output=direct_boot.root_sh(serial,'/system/bin/sh '+phone.q(ROOT)+'/namespace.sh network')
    print(output.decode().strip())
    profile=deployment.current()
    api=preflight(p,profile)
    if api.get('agent',{}).get('listen')!=profile.endpoint or api['agent'].get('tools')!=list(profile.tools):
        raise ValueError('Homewire agent scope does not match deployment profile')
    cfg=tomllib.loads(direct_boot.root_sh(serial,'cat '+phone.q(ROOT+'/policy/config.toml')).decode())
    if cfg['mcp']['servers'][0]['url']!=profile.mcp_url:
        raise ValueError('MCP endpoint does not match deployment profile')
    expected={'homewire__'+n for n in deployment.current().tools}
    risk=cfg['risk_profiles']['homewire']
    if risk['level']!='supervised' or risk['allowed_commands'] or set(risk['allowed_tools'])!=expected|{'time'}:
        raise ValueError('Unexpected agent permissions')
    if [s['name'] for s in cfg['mcp']['servers']]!=['homewire']:
        raise ValueError('Unexpected MCP servers')
    for c in cfg.get('channels',{}).get('telegram',{}).values():
        if not {'homewire__'+n for n in lockdown.WRITE}.issubset(c.get('excluded_tools',[])):
            raise ValueError('Telegram control exclusion is missing')
    if cfg['agents'][deployment.current().agent['name']]['workspace']['unrestricted_filesystem'] or cfg['scheduler']['enabled']:
        raise ValueError('Unrestricted workspace or scheduler enabled')
    if not p.sh('[ ! -f '+phone.q(MARKER)+' ] || echo yes').strip():
        raise ValueError('Legacy grant guard is missing')
    direct_boot.root_sh(serial,'test ! -f /data/adb/service.d/zeroclaw-directboot.sh')
    # Inspect the actual daemon, not just the separately launched probe.
    rows=direct_boot.root_sh(serial,'ps -A -o UID,PID,NAME').decode().splitlines()
    pids=[r.split()[1] for r in rows[1:] if r.split()[0]==str(uid) and r.split()[-1]=='zeroclaw']
    if len(pids)!=1:raise ValueError('Expected one isolated ZeroClaw daemon')
    pid=pids[0]
    status=direct_boot.root_sh(serial,'cat /proc/'+pid+'/status').decode()
    fields={r.split(':',1)[0]:r.split(':',1)[1].strip() for r in status.splitlines() if ':' in r}
    if fields.get('Seccomp')!='2' or fields.get('CapEff')!='0000000000000000' or fields.get('CapBnd')!='0000000000000000':
        raise ValueError('The active daemon is missing its kernel confinement')
    if b'guard.so' not in direct_boot.root_sh(serial,'cat /proc/'+pid+'/maps'):
        raise ValueError('The active daemon did not load the obligatory guard')
    print('Policy verified: Homewire only; Telegram read-only; legacy root boot bypass disabled.')


if __name__ == '__main__':
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--serial',required=True)
    deployment.add_argument(ap,required=True)
    ap.add_argument('--artifacts',default='target/android-sandbox')
    ap.add_argument('command',choices=['install','update','verify','restart'],nargs='?',default='install')
    a=ap.parse_args()
    profile=deployment.load(a.deployment)
    profile.android.setdefault('root_command',deployment.root_executable(a.serial))
    ROOT=profile.paths()['sandbox']
    MARKER=phone.CE_HOMEWIRE.rsplit('/',1)[0]+'/zeroclaw-isolated'
    if a.command in ('install','update'):
        globals()[a.command](a.serial,a.artifacts)
    else:
        globals()[a.command](a.serial)
