#!/usr/bin/env python3
"""Move an installed owner to Android device-encrypted storage; keep AI keys in CE.
Requires Magisk root. Saves a CE backup; never resets settings or device state.
"""
import argparse
from pathlib import Path
import subprocess
import json
import shlex
import phone
import deployment
ROOT=phone.DE_HOMEWIRE
WRAPPER='/data/adb/service.d/homewire.sh'
PID='/data/adb/homewire-supervisor.pid'
def q(s):return shlex.quote(str(s))
def root_sh(serial,command,data=None):
    p=subprocess.run(['adb','-s',serial,'shell','-T',q(deployment.root_executable(serial))+' -c '+q(command)],input=data or b'',capture_output=True,timeout=60)
    if p.returncode:raise RuntimeError(p.stderr.decode(errors='replace') or 'Root command failed')
    return p.stdout

def wrapper(uid, su_path=None):
    su_path = su_path or deployment.android().get("root_command", "su")
    return f'''#!/system/bin/sh
umask 077
root={ROOT}
context=$(ls -Zd "$root")
context=${{context%% *}}
case "$context" in u:object_r:app_data_file:s0:*) ;; *) exit 1;; esac
runtime_context=u:r:magisk:${{context#u:object_r:app_data_file:}}
echo $$ > {PID}
trap 'rm -f {PID}; exit 0' TERM INT EXIT
while true; do
  touch "$root/log/current"
  chown {uid}:{uid} "$root/log/current"
  chcon "$context" "$root/log/current"
  {q(su_path)} -d -g {uid} -G 3003 -Z "$runtime_context" {uid} -s /system/bin/sh -c "exec $root/launch" 2>&1 |
  while IFS= read -r line; do
    if [ "$(stat -c %s "$root/log/current" 2>/dev/null || echo 0)" -ge 65536 ]; then
      mv -f "$root/log/current" "$root/log/previous"
      touch "$root/log/current"
      chown {uid}:{uid} "$root/log/current"
      chcon "$context" "$root/log/current"
    fi
    printf '%s\\n' "$line" >> "$root/log/current"
  done
  sleep 5
done
'''

def stop(serial):
    # Validate every identity before signaling either process. Wait for both so
    # an old supervisor's EXIT trap cannot remove the new supervisor's PID file.
    pids=[]
    for file,marker in [(PID,WRAPPER),(ROOT+'/run/homewire.pid',ROOT+'/current/homewire')]:
        pid=root_sh(serial,f'''if [ -f {q(file)} ]; then p=$(cat {q(file)}); case "$p" in *[!0-9]*|'') exit 1;; esac; if [ -s /proc/$p/cmdline ] || [ -n "$(tr '\\000' ' ' < /proc/$p/cmdline 2>/dev/null)" ]; then c=$(tr '\\000' ' ' < /proc/$p/cmdline); case "$c" in *{q(marker)}*) echo "$p";; *) exit 1;; esac; fi; fi''').decode().strip()
        if pid:
            if not pid.isdecimal():raise ValueError('Invalid supervisor PID')
            pids.append(pid)
    for pid in pids:
        root_sh(serial,'kill '+pid+' 2>/dev/null || true')
    for pid in pids:
        root_sh(serial,f'''n=0; while kill -0 {pid} 2>/dev/null; do state=$(awk '{{print $3}}' /proc/{pid}/stat); [ "$state" != Z ] || break; [ "$n" -lt 50 ] || exit 1; n=$((n+1)); sleep .1; done''')

def restart(serial):
    stop(serial)
    root_sh(serial,f'nohup /system/bin/sh {q(WRAPPER)} </dev/null >/dev/null 2>&1 &')

def install(serial):
    p=phone.Phone(serial)
    uid=int(p.sh('id -u').strip())
    if uid<10000:raise ValueError('Expected the private Termux UID')
    source=phone.HOMEWIRE
    if source==ROOT:raise ValueError('Direct Boot is already configured; use the normal installer to update')
    context=root_sh(serial,'ls -Zd '+q(str(Path(ROOT).parents[1]))).decode().split()[0]
    if not context.startswith('u:object_r:app_data_file:'):raise ValueError('Unexpected device-encrypted package context')
    parent=str(Path(ROOT).parent)
    root_sh(serial,f'[ ! -e {q(ROOT)} ]; [ ! -e {q(WRAPPER)} ]; if [ ! -d {q(parent)} ]; then mkdir {q(parent)}; chown {uid}:{uid} {q(parent)}; chmod 700 {q(parent)}; chcon {q(context)} {q(parent)}; fi; [ "$(stat -c %u {q(parent)})" = {uid} ]')
    root_sh(serial,f'umask 077; mkdir {q(ROOT)}; cp -a {q(source)}/. {q(ROOT)}/; mkdir -p {q(ROOT)}/log; chown -R {uid}:{uid} {q(ROOT)}; chcon -hR {q(context)} {q(ROOT)}')
    # Version links created by the installer are absolute. Relocate them before
    # executing anything; following a CE link defeats before-unlock operation.
    for name in ('current','previous'):
        link=ROOT+'/'+name
        target=root_sh(serial,f'if [ -L {q(link)} ]; then readlink {q(link)}; fi').decode().strip()
        if not target:continue
        prefix=source+'/releases/'
        if not target.startswith(prefix) or '/' in target[len(prefix):]:
            raise ValueError('Unexpected executable version link')
        target=ROOT+'/releases/'+target[len(prefix):]
        root_sh(serial,f'[ -f {q(target+"/homewire")} ]; rm {q(link)}; ln -s {q(target)} {q(link)}; chown -h {uid}:{uid} {q(link)}; chcon -h {q(context)} {q(link)}')
    root_sh(serial,f'{q(deployment.root_executable(serial))} -d -g {uid} -G 3003 {uid} -s /system/bin/sh -c '+q(ROOT+'/current/homewire --version'))
    stopped=False
    try:
        p.sh('sv -w 10 down '+q(phone.SERVICE)+'; touch '+q(phone.SERVICE+'/down'))
        stopped=True
        # Copy durable state once more with the old owner stopped. The initial
        # copy was only a preflight; it must not race a final history/alert save.
        root_sh(serial,f'cp -a {q(source)}/config/. {q(ROOT)}/config/; chown -R {uid}:{uid} {q(ROOT)}/config; chcon -R {q(context)} {q(ROOT)}/config')
        for file in phone.FILES:
            if not file.endswith('.json'):continue
            old=p.sh(f'if [ -f {q(ROOT+"/config/"+file)} ]; then cat {q(ROOT+"/config/"+file)}; fi')
            if not old:continue
            value=json.loads(old)
            def paths(v):
                if isinstance(v,str):return v.replace(source+'/',ROOT+'/')
                if isinstance(v,list):return [paths(x) for x in v]
                if isinstance(v,dict):return {k:paths(x) for k,x in v.items()}
                return v
            p.put(ROOT+'/config/'+file,(json.dumps(paths(value),indent=2)+'\n').encode())
        p.put(ROOT+'/launch',phone.service_script(ROOT).encode(),'700')
        root_sh(serial,f'cat > {q(WRAPPER)}; chmod 700 {q(WRAPPER)}',wrapper(uid,deployment.root_executable(serial)).encode())
        p.put(source+'/direct-boot',b'Device owner moved to DE; CE copy is recovery only.\n')
        root_sh(serial,f'touch {q(ROOT)}/direct-boot; chown {uid}:{uid} {q(ROOT)}/direct-boot')
        restart(serial)
    except Exception:
        if stopped:
            stop(serial)
            root_sh(serial,f'rm -f {q(WRAPPER)} {q(ROOT)}/direct-boot')
            p.sh('rm -f '+q(source+'/direct-boot')+' '+q(phone.SERVICE+'/down')+'; sv up '+q(phone.SERVICE))
        raise
    print('Direct Boot owner configured. CE copy retained for recovery. Reboot validation is still required.')
if __name__=='__main__':
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--serial',required=True)
    deployment.add_argument(ap)
    ap.add_argument('action',choices=['install','restart','stop'])
    a=ap.parse_args()
    if a.deployment: deployment.load(a.deployment)
    {'install':install,'restart':restart,'stop':stop}[a.action](a.serial)
